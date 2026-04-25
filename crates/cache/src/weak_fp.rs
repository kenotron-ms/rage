//! Weak fingerprint: hashes command, tool binary, input globs, and env vars.
//! Used as the first phase of the two-phase cache lookup (design doc §5).

use globset::{Glob, GlobSetBuilder};
use std::path::Path;
use walkdir::WalkDir;

use crate::tool_hash::hash_tool_binary;

/// All inputs that vary the weak fingerprint.
pub struct WeakFpInputs<'a> {
    pub command: &'a str,
    pub tool_path: &'a Path,
    pub package_path: &'a Path,
    pub declared_input_globs: &'a [String],
    pub tracked_env: &'a [(String, String)],
}

/// Resolve `globs` relative to `pkg_dir`, skipping common generated/vcs dirs.
/// Returns (relative_path, blake3_hex_of_contents) pairs, sorted and deduped.
fn resolve_globs(pkg_dir: &Path, globs: &[String]) -> Vec<(std::path::PathBuf, String)> {
    if globs.is_empty() {
        return Vec::new();
    }

    let mut builder = GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = Glob::new(g) {
            builder.add(glob);
        }
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", ".git"];

    let mut files = Vec::new();

    for entry in WalkDir::new(pkg_dir)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.contains(&name.as_ref())
            } else {
                true
            }
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(pkg_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if set.is_match(rel) {
            let content = match std::fs::read(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let hash = blake3::hash(&content).to_hex().to_string();
            files.push((rel.to_path_buf(), hash));
        }
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.dedup_by(|a, b| a.0 == b.0);
    files
}

/// Compute the weak fingerprint as a 64-char blake3 hex string.
///
/// WF = blake3(
///   "command:" || command || "\n",
///   "tool_path:" || tool_path_bytes || "\n",
///   "tool_hash:" || tool_hash_or_missing || "\n",
///   "pkg_path:" || package_path_bytes || "\n",
///   sorted("input:" || rel_path || ":" || content_hash || "\n" for each matched file),
///   sorted("env:" || key || "=" || value || "\n" for each env pair),
/// )
pub fn compute_weak_fingerprint(inputs: &WeakFpInputs) -> String {
    let mut hasher = blake3::Hasher::new();

    // 1. command
    hasher.update(b"command:");
    hasher.update(inputs.command.as_bytes());
    hasher.update(b"\n");

    // 2. tool path and content hash
    hasher.update(b"tool_path:");
    hasher.update(inputs.tool_path.to_string_lossy().as_bytes());
    hasher.update(b"\n");

    let tool_hash = hash_tool_binary(inputs.tool_path).unwrap_or_else(|| "<missing>".to_string());
    hasher.update(b"tool_hash:");
    hasher.update(tool_hash.as_bytes());
    hasher.update(b"\n");

    // 3. package path
    hasher.update(b"pkg_path:");
    hasher.update(inputs.package_path.to_string_lossy().as_bytes());
    hasher.update(b"\n");

    // 4. resolved input globs (sorted, deduped)
    let resolved = resolve_globs(inputs.package_path, inputs.declared_input_globs);
    for (rel_path, content_hash) in &resolved {
        hasher.update(b"input:");
        hasher.update(rel_path.to_string_lossy().as_bytes());
        hasher.update(b":");
        hasher.update(content_hash.as_bytes());
        hasher.update(b"\n");
    }

    // 5. sorted env vars
    let mut env_sorted: Vec<&(String, String)> = inputs.tracked_env.iter().collect();
    env_sorted.sort_by_key(|(k, _)| k.as_str());
    for (key, value) in &env_sorted {
        hasher.update(b"env:");
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }

    // 6. finalize
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Helper: write a fake tool binary and return its path.
    fn make_tool(dir: &std::path::Path, content: &[u8]) -> PathBuf {
        let p = dir.join("tool");
        std::fs::write(&p, content).unwrap();
        p
    }

    /// Helper: build a minimal WeakFpInputs with no globs / no env.
    fn base_inputs<'a>(
        command: &'a str,
        tool_path: &'a Path,
        package_path: &'a Path,
    ) -> WeakFpInputs<'a> {
        WeakFpInputs {
            command,
            tool_path,
            package_path,
            declared_input_globs: &[],
            tracked_env: &[],
        }
    }

    // ── test 1 ────────────────────────────────────────────────────────────────
    #[test]
    fn deterministic_for_same_inputs() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path(), b"tool-v1");
        let inputs = base_inputs("tsc --build", &tool, dir.path());

        let h1 = compute_weak_fingerprint(&inputs);
        let h2 = compute_weak_fingerprint(&inputs);

        assert_eq!(h1, h2, "same inputs must produce the same hash");
        assert_eq!(h1.len(), 64, "blake3 hex output is 64 chars");
    }

    // ── test 2 ────────────────────────────────────────────────────────────────
    #[test]
    fn command_changes_invalidate() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path(), b"tool-v1");

        let h1 = compute_weak_fingerprint(&base_inputs("tsc --build", &tool, dir.path()));
        let h2 = compute_weak_fingerprint(&base_inputs("tsc --watch", &tool, dir.path()));

        assert_ne!(h1, h2, "different commands must produce different hashes");
    }

    // ── test 3 ────────────────────────────────────────────────────────────────
    #[test]
    fn tool_content_changes_invalidate() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path(), b"tsc-v4");
        let h1 = compute_weak_fingerprint(&base_inputs("tsc", &tool, dir.path()));

        // overwrite with new binary content
        std::fs::write(&tool, b"tsc-v5").unwrap();
        let h2 = compute_weak_fingerprint(&base_inputs("tsc", &tool, dir.path()));

        assert_ne!(h1, h2, "updated tool binary must change the fingerprint");
    }

    // ── test 4 ────────────────────────────────────────────────────────────────
    #[test]
    fn declared_input_content_invalidates() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path(), b"tool");

        // create src/index.ts
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let src = dir.path().join("src/index.ts");
        std::fs::write(&src, b"export const a = 1;").unwrap();

        let globs = vec!["src/**/*.ts".to_string()];
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: dir.path(),
            declared_input_globs: &globs,
            tracked_env: &[],
        };

        let h1 = compute_weak_fingerprint(&inputs);

        // change file content
        std::fs::write(&src, b"export const a = 2;").unwrap();
        let h2 = compute_weak_fingerprint(&inputs);

        assert_ne!(
            h1, h2,
            "changed input file content must change the fingerprint"
        );
    }

    // ── test 5 ────────────────────────────────────────────────────────────────
    #[test]
    fn unrelated_file_does_not_invalidate() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path(), b"tool");

        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/index.ts"), b"const x = 1;").unwrap();

        let globs = vec!["src/**/*.ts".to_string()];
        let inputs = WeakFpInputs {
            command: "tsc",
            tool_path: &tool,
            package_path: dir.path(),
            declared_input_globs: &globs,
            tracked_env: &[],
        };

        let h1 = compute_weak_fingerprint(&inputs);

        // create an unrelated README.md — not matched by src/**/*.ts
        std::fs::write(dir.path().join("README.md"), b"# docs").unwrap();
        let h2 = compute_weak_fingerprint(&inputs);

        assert_eq!(
            h1, h2,
            "a file outside the glob pattern must not affect the fingerprint"
        );
    }

    // ── test 6 ────────────────────────────────────────────────────────────────
    #[test]
    fn tracked_env_changes_invalidate() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path(), b"tool");

        let env1 = vec![("CI".to_string(), "1".to_string())];
        let env2 = vec![("CI".to_string(), "0".to_string())];

        let h1 = compute_weak_fingerprint(&WeakFpInputs {
            command: "build",
            tool_path: &tool,
            package_path: dir.path(),
            declared_input_globs: &[],
            tracked_env: &env1,
        });
        let h2 = compute_weak_fingerprint(&WeakFpInputs {
            command: "build",
            tool_path: &tool,
            package_path: dir.path(),
            declared_input_globs: &[],
            tracked_env: &env2,
        });

        assert_ne!(
            h1, h2,
            "different env var values must change the fingerprint"
        );
    }

    // ── test 7 ────────────────────────────────────────────────────────────────
    #[test]
    fn env_order_does_not_matter() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path(), b"tool");

        let env_a = vec![
            ("CI".to_string(), "true".to_string()),
            ("NODE_ENV".to_string(), "production".to_string()),
        ];
        let env_b = vec![
            ("NODE_ENV".to_string(), "production".to_string()),
            ("CI".to_string(), "true".to_string()),
        ];

        let h1 = compute_weak_fingerprint(&WeakFpInputs {
            command: "build",
            tool_path: &tool,
            package_path: dir.path(),
            declared_input_globs: &[],
            tracked_env: &env_a,
        });
        let h2 = compute_weak_fingerprint(&WeakFpInputs {
            command: "build",
            tool_path: &tool,
            package_path: dir.path(),
            declared_input_globs: &[],
            tracked_env: &env_b,
        });

        assert_eq!(h1, h2, "env var order must not affect the fingerprint");
    }
}
