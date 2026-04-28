//! Extract installed-package references from a sandbox pathset.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsetPackageRef {
    /// Package name. Scoped packages keep their `@scope/name` form.
    pub name: String,
    pub version: String,
    /// Absolute path to the package's directory in the workspace.
    pub package_root: PathBuf,
}

/// Parse pnpm virtual-store paths from a pathset.
pub fn extract_pnpm_packages(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
) -> Vec<PathsetPackageRef> {
    // pnpm virtual-store path:
    //   .pnpm/<encoded_name>@<version>[_<peer>]/node_modules/<dir_name>/...
    //   - Scoped: @scope/pkg → encoded as @scope+pkg
    //   - Peer suffix: name@ver_peerinfo or name@ver+peerinfo
    let re = regex::Regex::new(
        r"\.pnpm/(?P<encoded_name>(?:@[^/+]+\+)?[^@/]+)@(?P<version>[^/_+]+)(?:[_+][^/]+)?/node_modules/(?P<dir_name>(?:@[^/]+/)?[^/]+)/",
    )
    .expect("static regex");

    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out: Vec<PathsetPackageRef> = Vec::new();

    for p in pathset_reads {
        let s = p.to_string_lossy();
        let Some(caps) = re.captures(&s) else {
            continue;
        };
        let dir_name = caps.name("dir_name").unwrap().as_str().to_string();
        let version = caps.name("version").unwrap().as_str().to_string();

        if !seen.insert((dir_name.clone(), version.clone())) {
            continue;
        }

        let encoded = caps.name("encoded_name").unwrap().as_str();
        let pnpm_dir = format!("{encoded}@{version}");
        let package_root = workspace_root
            .join("node_modules")
            .join(".pnpm")
            .join(&pnpm_dir)
            .join("node_modules")
            .join(&dir_name);

        out.push(PathsetPackageRef {
            name: dir_name,
            version,
            package_root,
        });
    }
    out
}

/// Extract package refs for flat node_modules layouts (yarn classic, npm).
pub fn extract_flat_packages(
    pathset_reads: &[PathBuf],
    workspace_root: &Path,
    lockfile: &Path,
) -> Vec<PathsetPackageRef> {
    let nm_prefix = workspace_root.join("node_modules");

    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for p in pathset_reads {
        let Ok(rel) = p.strip_prefix(&nm_prefix) else {
            continue;
        };
        let s = rel.to_string_lossy();
        if s.starts_with(".pnpm/") || s.starts_with(".bin/") || s.starts_with(".cache/") {
            continue;
        }
        let mut comps = rel.components();
        let first = match comps.next() {
            Some(c) => c.as_os_str().to_string_lossy().to_string(),
            None => continue,
        };
        let name = if first.starts_with('@') {
            let Some(second) = comps.next() else { continue };
            format!("{}/{}", first, second.as_os_str().to_string_lossy())
        } else {
            first
        };
        names.insert(name);
    }

    if names.is_empty() {
        return Vec::new();
    }

    let lock_text = std::fs::read_to_string(lockfile).unwrap_or_default();
    let is_yarn = lockfile
        .file_name()
        .and_then(|s| s.to_str())
        .map(|n| n.ends_with("yarn.lock"))
        .unwrap_or(false);

    let mut out = Vec::new();
    for name in names {
        let version = if is_yarn {
            lookup_yarn_classic_version(&lock_text, &name)
        } else {
            lookup_npm_lock_version(&lock_text, &name)
        };
        if let Some(v) = version {
            out.push(PathsetPackageRef {
                name: name.clone(),
                version: v,
                package_root: nm_prefix.join(&name),
            });
        }
    }
    out
}

fn lookup_npm_lock_version(lock_text: &str, name: &str) -> Option<String> {
    let key = format!("\"node_modules/{name}\"");
    let idx = lock_text.find(&key)?;
    let tail = &lock_text[idx..];
    let window_end = tail.len().min(4096);
    let window = &tail[..window_end];
    let v_idx = window.find("\"version\"")?;
    let after = &window[v_idx..];
    let colon = after.find(':')?;
    let after_colon = &after[colon + 1..];
    let start = after_colon.find('"')? + 1;
    let rest = &after_colon[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn lookup_yarn_classic_version(lock_text: &str, name: &str) -> Option<String> {
    let needles = [format!("\n{name}@"), format!("\n\"{name}@")];
    let head = needles
        .iter()
        .filter_map(|n| lock_text.find(n.as_str()))
        .min()?;
    let after = &lock_text[head..];
    let v_idx = after.find("version ")?;
    let rest = &after[v_idx + "version ".len()..];
    let q = rest.find('"')? + 1;
    let body = &rest[q..];
    let end = body.find('"')?;
    Some(body[..end].to_string())
}

/// Extract package refs from flat node_modules layout (yarn/npm).
///
/// Scans `workspace_root/node_modules` directly — no lockfile required and no
/// dependency on what the sandbox observed.  This is intentional: for flat
/// layouts (yarn berry node-modules linker, yarn classic, npm) the build
/// sandbox typically only records the invocation of the tool binary itself
/// (e.g. `node_modules/.bin/yarn`), never the deep file reads performed by
/// the TypeScript compiler inside the subprocess.  Relying on pathset_reads
/// would therefore always yield zero packages.
///
/// The `_pathset_reads` parameter is retained for API compatibility but is
/// not used for package discovery.
///
/// Filtering rules (same as before):
/// - dot-prefixed top-level dirs (`.bin`, `.cache`, `.pnpm`, …) are skipped
/// - entries whose canonicalized path escapes `node_modules` (workspace
///   package symlinks) are skipped
/// - entries without a readable `package.json` are skipped
/// - entries with a `workspaces` key in `package.json` are skipped
pub fn extract_flat_from_node_modules(
    _pathset_reads: &[PathBuf],
    workspace_root: &Path,
) -> Vec<PathsetPackageRef> {
    let nm_root = workspace_root.join("node_modules");
    let canonical_nm = match std::fs::canonicalize(&nm_root) {
        Ok(c) => c,
        Err(_) => return Vec::new(), // node_modules doesn't exist yet
    };

    let mut out: Vec<PathsetPackageRef> = Vec::new();

    let top_entries = match std::fs::read_dir(&nm_root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    for entry in top_entries.flatten() {
        let name_os = entry.file_name();
        let name_str = name_os.to_string_lossy();

        // Skip dot-prefixed entries (.bin, .cache, .pnpm, .yarn, …)
        if name_str.starts_with('.') {
            continue;
        }

        if name_str.starts_with('@') {
            // Scoped namespace: descend one more level to find the actual packages.
            let scope = name_str.to_string();
            let scope_dir = nm_root.join(&scope);
            if let Ok(sub_entries) = std::fs::read_dir(&scope_dir) {
                for sub in sub_entries.flatten() {
                    let pkg_name = format!("{}/{}", scope, sub.file_name().to_string_lossy());
                    if let Some(r) = read_flat_pkg(&nm_root, &canonical_nm, &pkg_name) {
                        out.push(r);
                    }
                }
            }
        } else {
            let pkg_name = name_str.to_string();
            if let Some(r) = read_flat_pkg(&nm_root, &canonical_nm, &pkg_name) {
                out.push(r);
            }
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Read a single package entry from a flat `node_modules` layout.
///
/// Returns `None` when the entry should be skipped (symlink to workspace
/// source, missing/invalid `package.json`, workspace root package, …).
fn read_flat_pkg(nm_root: &Path, canonical_nm: &Path, name: &str) -> Option<PathsetPackageRef> {
    let package_root = nm_root.join(name);

    // Skip if this entry resolves outside node_modules — it is a symlink
    // pointing at a workspace package's source directory, not a real dep.
    if let Ok(canonical_pkg) = std::fs::canonicalize(&package_root) {
        if !canonical_pkg.starts_with(canonical_nm) {
            return None;
        }
    }

    // Read version from the package's own package.json.
    let pkg_json_path = package_root.join("package.json");
    let text = std::fs::read_to_string(&pkg_json_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;

    // Workspace root packages carry a "workspaces" field — skip them.
    if value.get("workspaces").is_some() {
        return None;
    }

    let version = value
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())?;

    Some(PathsetPackageRef {
        name: name.to_string(),
        version,
        package_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pnpm_resolved_path_extracts_name_and_version() {
        let reads = vec![
            PathBuf::from("/workspace/node_modules/.pnpm/ms@2.1.3/node_modules/ms/index.js"),
            PathBuf::from("/workspace/node_modules/.pnpm/typescript@5.3.2/node_modules/typescript/lib/typescript.js"),
            PathBuf::from("/workspace/node_modules/ms/index.js"),
            PathBuf::from("/workspace/src/main.ts"),
        ];
        let ws = Path::new("/workspace");
        let refs = extract_pnpm_packages(&reads, ws);
        assert_eq!(refs.len(), 2, "expected ms + typescript, got {refs:?}");
        assert!(refs.iter().any(|r| r.name == "ms" && r.version == "2.1.3"));
        assert!(refs
            .iter()
            .any(|r| r.name == "typescript" && r.version == "5.3.2"));

        let ms = refs.iter().find(|r| r.name == "ms").unwrap();
        assert_eq!(
            ms.package_root,
            PathBuf::from("/workspace/node_modules/.pnpm/ms@2.1.3/node_modules/ms")
        );
    }

    #[test]
    fn pnpm_scoped_package_extracted_with_full_name() {
        let reads = vec![PathBuf::from(
            "/ws/node_modules/.pnpm/@types+node@20.1.0/node_modules/@types/node/index.d.ts",
        )];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert_eq!(refs.len(), 1);
        let r = &refs[0];
        assert_eq!(r.name, "@types/node");
        assert_eq!(r.version, "20.1.0");
        assert_eq!(
            r.package_root,
            PathBuf::from("/ws/node_modules/.pnpm/@types+node@20.1.0/node_modules/@types/node")
        );
    }

    #[test]
    fn pnpm_peer_dep_suffix_is_stripped_from_version() {
        let reads = vec![PathBuf::from(
            "/ws/node_modules/.pnpm/react-dom@18.2.0_react@18.2.0/node_modules/react-dom/index.js",
        )];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "react-dom");
        assert_eq!(refs[0].version, "18.2.0");
    }

    #[test]
    fn pnpm_workspace_symlink_paths_excluded() {
        let reads = vec![
            PathBuf::from("/ws/packages/my-lib/index.ts"),
            PathBuf::from("/ws/node_modules/@scope/my-lib/index.ts"),
        ];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert!(
            refs.is_empty(),
            "workspace packages must NOT be captured, got {refs:?}"
        );
    }

    #[test]
    fn pnpm_deduplicates_repeated_reads() {
        let reads = vec![
            PathBuf::from("/ws/node_modules/.pnpm/ms@2.1.3/node_modules/ms/index.js"),
            PathBuf::from("/ws/node_modules/.pnpm/ms@2.1.3/node_modules/ms/package.json"),
            PathBuf::from("/ws/node_modules/.pnpm/ms@2.1.3/node_modules/ms/lib/util.js"),
        ];
        let refs = extract_pnpm_packages(&reads, Path::new("/ws"));
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn flat_extracts_from_npm_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(
            ws.join("package-lock.json"),
            r#"{
              "lockfileVersion": 3,
              "packages": {
                "node_modules/ms": { "version": "2.1.3" },
                "node_modules/lodash": { "version": "4.17.21" }
              }
            }"#,
        )
        .unwrap();

        let reads = vec![
            ws.join("node_modules/ms/index.js"),
            ws.join("node_modules/ms/package.json"),
            ws.join("node_modules/lodash/lodash.js"),
            ws.join("node_modules/.pnpm/ignored@1.0.0/node_modules/ignored/x.js"),
            ws.join("packages/local-pkg/index.ts"),
        ];
        let refs = extract_flat_packages(&reads, ws, &ws.join("package-lock.json"));
        assert_eq!(refs.len(), 2, "got {refs:?}");
        assert!(refs.iter().any(|r| r.name == "ms" && r.version == "2.1.3"));
        assert!(refs
            .iter()
            .any(|r| r.name == "lodash" && r.version == "4.17.21"));
    }

    #[test]
    fn flat_extracts_from_yarn_classic_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(
            ws.join("yarn.lock"),
            "\nms@^2.1.3:\n  version \"2.1.3\"\n  resolved \"https://...\"\n",
        )
        .unwrap();
        let reads = vec![ws.join("node_modules/ms/index.js")];
        let refs = extract_flat_packages(&reads, ws, &ws.join("yarn.lock"));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "ms");
        assert_eq!(refs[0].version, "2.1.3");
    }

    #[test]
    fn flat_skips_packages_not_in_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("package-lock.json"), r#"{"packages":{}}"#).unwrap();
        let reads = vec![ws.join("node_modules/ghost/index.js")];
        let refs = extract_flat_packages(&reads, ws, &ws.join("package-lock.json"));
        assert!(refs.is_empty());
    }

    #[test]
    fn flat_node_modules_extracts_version_from_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();

        let ms_dir = ws.join("node_modules/ms");
        std::fs::create_dir_all(&ms_dir).unwrap();
        std::fs::write(
            ms_dir.join("package.json"),
            r#"{"name":"ms","version":"2.1.3"}"#,
        )
        .unwrap();
        std::fs::write(ms_dir.join("index.js"), b"// ms").unwrap();

        let reads = vec![ms_dir.join("index.js")];
        let refs = extract_flat_from_node_modules(&reads, ws);

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "ms");
        assert_eq!(refs[0].version, "2.1.3");
        assert_eq!(refs[0].package_root, ms_dir);
    }

    #[test]
    fn flat_node_modules_handles_scoped_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();

        let types_node = ws.join("node_modules/@types/node");
        std::fs::create_dir_all(&types_node).unwrap();
        std::fs::write(
            types_node.join("package.json"),
            r#"{"name":"@types/node","version":"20.1.0"}"#,
        )
        .unwrap();

        let reads = vec![types_node.join("index.d.ts")];
        let refs = extract_flat_from_node_modules(&reads, ws);

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "@types/node");
        assert_eq!(refs[0].version, "20.1.0");
    }

    #[test]
    fn flat_node_modules_skips_if_no_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        // No package.json — should skip gracefully
        let reads = vec![ws.join("node_modules/ghost/index.js")];
        let refs = extract_flat_from_node_modules(&reads, ws);
        assert!(refs.is_empty());
    }
}
