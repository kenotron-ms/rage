//! Lockfile parsers for JavaScript package managers.
//!
//! Produces `Vec<plugin::LockfilePackage>` from yarn berry, yarn classic, pnpm, and npm lockfiles.
//! The returned list contains only external packages — workspace packages (no integrity hash) are excluded.

use plugin::LockfilePackage;
use std::path::{Path, PathBuf};

/// Parse a yarn berry (v8+) lockfile.
///
/// Format:
/// ```text
/// __metadata:
///   version: 8
///   cacheKey: 10c0
///
/// "ms@npm:2.1.3":
///   version: 2.1.3
///   resolution: "ms@npm:2.1.3"
///   checksum: 10c0/sha512hex
///   languageName: node
///   linkType: hard
/// ```
///
/// Workspace packages (languageName: unknown or linkType: soft) are excluded.
pub fn parse_yarn_berry_lockfile(content: &str) -> Vec<LockfilePackage> {
    let mut packages = Vec::new();

    // Split by blank lines to get entry blocks
    let blocks: Vec<&str> = content.split("\n\n").collect();

    for block in blocks {
        let block = block.trim();
        if block.is_empty() || block.starts_with("__metadata") {
            continue;
        }

        let mut pkg_name: Option<String> = None;
        let mut version: Option<String> = None;
        let mut checksum: Option<String> = None;
        let mut is_workspace = false;

        for line in block.lines() {
            let line = line.trim();

            // First line: "pkg@npm:version": or "@scope/pkg@npm:version, other@npm:version":
            if line.starts_with('"') && line.ends_with(':') && pkg_name.is_none() {
                // Extract name from the first specifier only
                // "@actions/cache@npm:3.3.0": → "@actions/cache"
                let inner = line.trim_start_matches('"').trim_end_matches(':');
                // May have multiple comma-separated specifiers, take first
                let first_spec = inner.split(',').next().unwrap_or("").trim().trim_matches('"');
                // Extract package name (everything before the last @)
                if let Some(at_pos) = first_spec.rfind("@npm:") {
                    let name = &first_spec[..at_pos];
                    pkg_name = Some(name.to_string());
                } else if let Some(at_pos) = first_spec.rfind("@workspace:") {
                    let _ = at_pos; // workspace package, will be marked below
                    is_workspace = true;
                }
            } else if let Some(v) = line.strip_prefix("version: ") {
                version = Some(v.trim().to_string());
            } else if let Some(cs) = line.strip_prefix("checksum: ") {
                checksum = Some(cs.trim().to_string());
            } else if line == "languageName: unknown" || line == "linkType: soft" {
                is_workspace = true;
            }
        }

        if is_workspace || pkg_name.is_none() || version.is_none() || checksum.is_none() {
            continue;
        }

        packages.push(LockfilePackage {
            name: pkg_name.unwrap(),
            version: version.unwrap(),
            integrity: checksum.unwrap(),
            tarball_url: None,
        });
    }

    // Deduplicate by (name, version)
    packages.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    packages.dedup_by(|a, b| a.name == b.name && a.version == b.version);

    packages
}

/// Parse a yarn classic (v1) lockfile.
///
/// Format:
/// ```text
/// ms@2.1.3, ms@^2.1.1:
///   version "2.1.3"
///   resolved "https://registry.npmjs.org/ms/-/ms-2.1.3.tgz#sha1=abc123"
///   integrity sha512-XXX
/// ```
pub fn parse_yarn_classic_lockfile(content: &str) -> Vec<LockfilePackage> {
    let mut packages = Vec::new();
    let blocks: Vec<&str> = content.split("\n\n").collect();

    for block in blocks {
        let block = block.trim();
        if block.is_empty() || block.starts_with('#') {
            continue;
        }

        let mut pkg_name: Option<String> = None;
        let mut version: Option<String> = None;
        let mut integrity: Option<String> = None;
        let mut resolved: Option<String> = None;

        for (i, line) in block.lines().enumerate() {
            let trimmed = line.trim();

            if i == 0 {
                // First line: `ms@2.1.3, ms@^2.1.1:` or `"ms@^2.1.1":`
                let line_clean = trimmed.trim_end_matches(':').trim_matches('"');
                let first_spec = line_clean.split(',').next().unwrap_or("").trim().trim_matches('"');
                // Find last `@` (scoped packages have `@` in name too)
                if let Some(at_pos) = first_spec.rfind('@') {
                    pkg_name = Some(first_spec[..at_pos].to_string());
                }
                continue;
            }

            if let Some(v) = trimmed.strip_prefix("version ") {
                version = Some(v.trim().trim_matches('"').to_string());
            } else if let Some(r) = trimmed.strip_prefix("resolved ") {
                resolved = Some(r.trim().trim_matches('"').to_string());
            } else if let Some(cs) = trimmed.strip_prefix("integrity ") {
                integrity = Some(cs.trim().to_string());
            }
        }

        if pkg_name.is_none() || version.is_none() {
            continue;
        }

        // Skip workspace packages (no resolved URL)
        if resolved.is_none() && integrity.is_none() {
            continue;
        }

        packages.push(LockfilePackage {
            name: pkg_name.unwrap(),
            version: version.unwrap(),
            integrity: integrity.unwrap_or_default(),
            tarball_url: resolved,
        });
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    packages.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    packages
}

/// Parse a pnpm lockfile (YAML format).
///
/// Format (v6/v9):
/// ```yaml
/// packages:
///   /ms@2.1.3:
///     resolution: {integrity: sha512-XXX, tarball: https://...}
///     dev: false
/// ```
pub fn parse_pnpm_lockfile(content: &str) -> Vec<LockfilePackage> {
    let mut packages = Vec::new();
    let mut in_packages = false;
    let mut current_name: Option<String> = None;
    let mut current_version: Option<String> = None;
    let mut current_integrity: Option<String> = None;

    for line in content.lines() {
        if line.starts_with("packages:") {
            in_packages = true;
            continue;
        }
        if !in_packages {
            continue;
        }
        // New top-level section
        if !line.starts_with(' ') && !line.starts_with('\t') && line.ends_with(':') {
            if !line.starts_with("packages:") {
                in_packages = false;
            }
            continue;
        }

        let trimmed = line.trim();

        // Entry key: /ms@2.1.3: or /ms/2.1.3: (pnpm v5/v6)
        if trimmed.ends_with(':') && !trimmed.starts_with("resolution:") && line.starts_with("  /") {
            // Push previous
            if let (Some(n), Some(v)) = (current_name.take(), current_version.take()) {
                let int = current_integrity.take().unwrap_or_default();
                if !int.is_empty() {
                    packages.push(LockfilePackage {
                        name: n,
                        version: v,
                        integrity: int,
                        tarball_url: None,
                    });
                }
            }
            // Parse new: /ms@2.1.3: or /@types/node@20.0.0:
            let spec = trimmed.trim_end_matches(':').trim_start_matches('/');
            if let Some(at_pos) = spec.rfind('@') {
                current_name = Some(spec[..at_pos].to_string());
                current_version = Some(spec[at_pos + 1..].to_string());
            }
        } else if trimmed.starts_with("integrity:") {
            let val = trimmed["integrity:".len()..].trim();
            current_integrity = Some(val.to_string());
        } else if trimmed.contains("integrity:") && trimmed.starts_with("resolution:") {
            // resolution: {integrity: sha512-XXX}
            if let Some(start) = trimmed.find("integrity: ") {
                let rest = &trimmed[start + "integrity: ".len()..];
                let end = rest.find(|c: char| c == ',' || c == '}').unwrap_or(rest.len());
                current_integrity = Some(rest[..end].trim().to_string());
            }
        }
    }

    // Push last
    if let (Some(n), Some(v)) = (current_name, current_version) {
        let int = current_integrity.unwrap_or_default();
        if !int.is_empty() {
            packages.push(LockfilePackage {
                name: n,
                version: v,
                integrity: int,
                tarball_url: None,
            });
        }
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    packages.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    packages
}

/// Parse an npm lockfile (package-lock.json v2+).
pub fn parse_npm_lockfile(content: &str) -> Vec<LockfilePackage> {
    let Ok(v): Result<serde_json::Value, _> = serde_json::from_str(content) else {
        return Vec::new();
    };

    let mut packages = Vec::new();

    if let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) {
        for (key, pkg) in pkgs {
            // key is "node_modules/name" or "node_modules/@scope/name"
            let name = key.strip_prefix("node_modules/").unwrap_or(key.as_str());
            if name.is_empty() {
                continue;
            }

            // Skip workspace packages (no resolved field or has link: true)
            if pkg.get("link").and_then(|l| l.as_bool()).unwrap_or(false) {
                continue;
            }

            let integrity = pkg.get("integrity")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();

            if integrity.is_empty() {
                continue;
            }

            let version = pkg.get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let tarball_url = pkg.get("resolved")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());

            packages.push(LockfilePackage {
                name: name.to_string(),
                version,
                integrity,
                tarball_url,
            });
        }
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    packages
}

/// Compute the CAS key for a package, given its integrity string.
///
/// CAS key = Blake3(integrity.as_bytes()) — deterministic across machines.
pub fn compute_cas_key(integrity: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(integrity.as_bytes());
    *h.finalize().as_bytes()
}

/// Find the yarn berry zip file in `cache_dir` for the given package.
///
/// Yarn berry cache naming: `{sanitized-name}-npm-{version}-{hash_fragment}-{cacheKey}.zip`
/// where `sanitized-name` replaces `/` with `-` and keeps `@`.
///
/// Since we can't easily reconstruct yarn's locator hash, we scan for files
/// matching: `{sanitized-name}-npm-{version}-*.zip`
pub fn find_yarn_berry_zip(cache_dir: &Path, name: &str, version: &str) -> Option<PathBuf> {
    // Sanitize: @actions/cache → @actions-cache
    let sanitized = name.replace('/', "-");
    let prefix = format!("{}-npm-{}-", sanitized, version);

    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy();
            if fname_str.starts_with(&prefix) && fname_str.ends_with(".zip") {
                return Some(entry.path());
            }
        }
    }
    None
}

/// Extract a yarn berry zip file into `target_dir/`.
///
/// Yarn berry zip files contain entries like:
/// - `node_modules/`
/// - `node_modules/{name}/`
/// - `node_modules/{name}/package.json`
/// - ...
///
/// We extract them relative to `target_dir`, so the full package lands at
/// `target_dir/node_modules/{name}/`.
pub fn extract_yarn_zip_to_workspace(zip_bytes: &[u8], target_dir: &Path) -> Result<(), anyhow::Error> {
    use std::io::Cursor;

    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = target_dir.join(file.name());

        if file.name().ends_with('/') {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut outfile = std::fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const YARN_BERRY_FIXTURE: &str = r#"__metadata:
  version: 8
  cacheKey: 10c0

"ms@npm:2.1.3":
  version: 2.1.3
  resolution: "ms@npm:2.1.3"
  checksum: 10c0/abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890
  languageName: node
  linkType: hard

"@types/node@npm:20.0.0":
  version: 20.0.0
  resolution: "@types/node@npm:20.0.0"
  checksum: 10c0/fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210
  languageName: node
  linkType: hard

"workspace-a@workspace:packages/a":
  version: 0.0.0-use.local
  resolution: "workspace-a@workspace:packages/a"
  languageName: unknown
  linkType: soft
"#;

    #[test]
    fn parse_yarn_berry_extracts_external_packages() {
        let packages = parse_yarn_berry_lockfile(YARN_BERRY_FIXTURE);
        assert_eq!(packages.len(), 2, "workspace package must be excluded");

        let ms = packages.iter().find(|p| p.name == "ms").unwrap();
        assert_eq!(ms.version, "2.1.3");
        assert!(ms.integrity.starts_with("10c0/"), "integrity must include cache prefix: {:?}", ms.integrity);

        let types = packages.iter().find(|p| p.name == "@types/node").unwrap();
        assert_eq!(types.version, "20.0.0");
    }

    #[test]
    fn parse_yarn_berry_skips_workspace_packages() {
        let packages = parse_yarn_berry_lockfile(YARN_BERRY_FIXTURE);
        assert!(!packages.iter().any(|p| p.name.contains("workspace-a")));
    }

    #[test]
    fn compute_cas_key_is_deterministic() {
        let key1 = compute_cas_key("10c0/abcdef123");
        let key2 = compute_cas_key("10c0/abcdef123");
        assert_eq!(key1, key2);

        let key3 = compute_cas_key("10c0/different");
        assert_ne!(key1, key3);
    }

    #[test]
    fn find_yarn_berry_zip_finds_by_name_version() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path();

        // Create fake zip files
        std::fs::write(cache.join("ms-npm-2.1.3-abc123def456-10c0.zip"), b"fakecontent").unwrap();
        std::fs::write(cache.join("typescript-npm-5.4.2-xyz789abc123-10c0.zip"), b"fakecontent2").unwrap();

        let found = find_yarn_berry_zip(cache, "ms", "2.1.3").unwrap();
        assert!(found.file_name().unwrap().to_string_lossy().contains("ms-npm-2.1.3"));

        let found2 = find_yarn_berry_zip(cache, "typescript", "5.4.2").unwrap();
        assert!(found2.file_name().unwrap().to_string_lossy().contains("typescript-npm-5.4.2"));

        // Scoped package
        std::fs::write(cache.join("@types-node-npm-20.0.0-111aaa222bbb-10c0.zip"), b"scoped").unwrap();
        let found3 = find_yarn_berry_zip(cache, "@types/node", "20.0.0").unwrap();
        assert!(found3.file_name().unwrap().to_string_lossy().contains("@types-node-npm-20.0.0"));
    }

    #[test]
    fn parse_npm_lockfile_extracts_packages() {
        let lock_json = r#"{
            "lockfileVersion": 2,
            "packages": {
                "node_modules/ms": {
                    "version": "2.1.3",
                    "resolved": "https://registry.npmjs.org/ms/-/ms-2.1.3.tgz",
                    "integrity": "sha512-abc123=="
                },
                "node_modules/@types/node": {
                    "version": "20.0.0",
                    "resolved": "https://registry.npmjs.org/@types/node/-/node-20.0.0.tgz",
                    "integrity": "sha512-def456=="
                },
                "": {
                    "name": "my-workspace",
                    "version": "1.0.0"
                }
            }
        }"#;

        let packages = parse_npm_lockfile(lock_json);
        assert_eq!(packages.len(), 2);
        let ms = packages.iter().find(|p| p.name == "ms").unwrap();
        assert_eq!(ms.integrity, "sha512-abc123==");
    }
}
