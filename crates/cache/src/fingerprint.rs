//! Content fingerprinting for tasks.

use anyhow::Result;
use blake3::Hasher;
use std::path::Path;
use walkdir::WalkDir;

/// Compute a blake3 fingerprint for a task.
///
/// Hashes:
///   1. The command string (bytes)
///   2. All source files in `pkg_dir` (*.ts *.js *.tsx *.jsx *.mts *.cts *.rs *.go *.py *.json),
///      excluding `node_modules/`, `target/`, `dist/`, `.git/` directories,
///      sorted by path for determinism.
///
/// Returns the hex-encoded 32-byte blake3 hash.
pub fn fingerprint_task(command: &str, pkg_dir: &Path) -> Result<String> {
    let mut hasher = Hasher::new();

    // 1. Hash the command string
    hasher.update(command.as_bytes());

    // 2. Collect and sort source files (deterministic order)
    let mut files: Vec<std::path::PathBuf> = WalkDir::new(pkg_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Prune excluded directories — don't descend into them
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !matches!(name.as_ref(), "node_modules" | "target" | "dist" | ".git")
            } else {
                true
            }
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let ext = e.path().extension().and_then(|s| s.to_str()).unwrap_or("");
            matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "rs" | "go" | "py" | "json"
            )
        })
        .map(|e| e.into_path())
        .collect();

    files.sort();

    // 3. Hash each source file's path and contents
    for file in &files {
        // Include the path so that renames (same content, different name) invalidate the cache.
        hasher.update(file.as_os_str().as_encoded_bytes());
        let contents = std::fs::read(file).unwrap_or_default(); // missing file → treat as empty (tolerant)
        hasher.update(&contents);
    }

    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn same_command_same_empty_dir_same_hash() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("echo hello", dir.path()).unwrap();
        let h2 = fingerprint_task("echo hello", dir.path()).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_command_different_hash() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("echo hello", dir.path()).unwrap();
        let h2 = fingerprint_task("echo world", dir.path()).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn adding_source_file_changes_hash() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("build", dir.path()).unwrap();
        fs::write(dir.path().join("index.ts"), b"export const x = 1;").unwrap();
        let h2 = fingerprint_task("build", dir.path()).unwrap();
        assert_ne!(h1, h2, "adding a source file should change the fingerprint");
    }

    #[test]
    fn node_modules_excluded() {
        let dir = tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("dep.ts"), b"// should be ignored").unwrap();
        let h1 = fingerprint_task("build", dir.path()).unwrap();
        // Modifying node_modules should NOT change the hash
        fs::write(nm.join("dep.ts"), b"// changed").unwrap();
        let h2 = fingerprint_task("build", dir.path()).unwrap();
        assert_eq!(h1, h2, "node_modules changes should not affect fingerprint");
    }

    #[test]
    fn non_source_file_excluded() {
        let dir = tempdir().unwrap();
        let h1 = fingerprint_task("build", dir.path()).unwrap();
        fs::write(dir.path().join("README.md"), b"# readme").unwrap();
        let h2 = fingerprint_task("build", dir.path()).unwrap();
        assert_eq!(h1, h2, ".md files should not affect fingerprint");
    }

    #[test]
    fn hash_is_64_hex_chars() {
        let dir = tempdir().unwrap();
        let h = fingerprint_task("build", dir.path()).unwrap();
        assert_eq!(h.len(), 64, "blake3 hex output is 64 chars");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn nonexistent_dir_still_hashes_command() {
        // A package with no directory on disk (e.g. in-memory test package)
        // should still return a fingerprint based on the command alone.
        let h = fingerprint_task("echo test", Path::new("/tmp/nonexistent-rage-test-xyz")).unwrap();
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn renaming_file_changes_hash() {
        // Renaming a file (same content, different path) must invalidate the fingerprint.
        // Without path-in-hash, a rename would produce a false cache hit.
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("utils.ts"), b"export const x = 1;").unwrap();
        let h1 = fingerprint_task("build", dir.path()).unwrap();

        // Rename: identical content, different filename
        fs::rename(dir.path().join("utils.ts"), dir.path().join("helpers.ts")).unwrap();
        let h2 = fingerprint_task("build", dir.path()).unwrap();

        assert_ne!(
            h1, h2,
            "renaming a source file must change the fingerprint even when content is identical"
        );
    }
}
