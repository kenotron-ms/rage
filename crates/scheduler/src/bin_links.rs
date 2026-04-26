//! Create `node_modules/.bin/` symlinks from each package's `bin` field in `package.json`.
//!
//! Called after a CAS restore to avoid running the package manager just for
//! bin-link creation. This replaces the old workaround of re-running `yarn install`
//! when `node_modules/.bin/` was absent.

use std::path::Path;

/// Create `node_modules/.bin/` symlinks from each package's `bin` field.
///
/// Walks `workspace_root/node_modules/`, reads every `package.json`, and
/// creates symlinks in `.bin/` for each declared binary.
///
/// Returns the number of bin entries successfully created.
pub fn create_bin_links(workspace_root: &Path) -> std::io::Result<usize> {
    let nm = workspace_root.join("node_modules");
    let bin_dir = nm.join(".bin");
    std::fs::create_dir_all(&bin_dir)?;

    let mut count = 0;

    for entry in std::fs::read_dir(&nm)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden dirs like .bin, .cache, .pnpm, etc.
        if name_str.starts_with('.') {
            continue;
        }

        if name_str.starts_with('@') {
            // Scoped package: @scope/pkg
            let scope_dir = entry.path();
            if !scope_dir.is_dir() {
                continue;
            }
            for inner in std::fs::read_dir(&scope_dir)? {
                let inner = inner?;
                let full_name = format!("{}/{}", name_str, inner.file_name().to_string_lossy());
                count +=
                    create_bin_links_for_package(&inner.path(), &full_name, &bin_dir)?;
            }
        } else {
            count += create_bin_links_for_package(&entry.path(), &name_str, &bin_dir)?;
        }
    }

    Ok(count)
}

fn create_bin_links_for_package(
    pkg_dir: &Path,
    pkg_name: &str,
    bin_dir: &Path,
) -> std::io::Result<usize> {
    if !pkg_dir.is_dir() {
        return Ok(0);
    }

    let pkg_json_path = pkg_dir.join("package.json");
    let text = match std::fs::read_to_string(&pkg_json_path) {
        Ok(t) => t,
        Err(_) => return Ok(0),
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(0),
    };

    let bin_field = match v.get("bin") {
        Some(b) => b,
        None => return Ok(0),
    };

    let mut count = 0;

    match bin_field {
        serde_json::Value::String(rel_path) => {
            // Single binary: name is the package name (last segment for scoped)
            let bin_name = pkg_name.split('/').last().unwrap_or(pkg_name);
            if create_one_bin_link(bin_dir, bin_name, pkg_dir, rel_path)? {
                count += 1;
            }
        }
        serde_json::Value::Object(map) => {
            for (bin_name, bin_path) in map {
                if let Some(rel_path) = bin_path.as_str() {
                    if create_one_bin_link(bin_dir, bin_name, pkg_dir, rel_path)? {
                        count += 1;
                    }
                }
            }
        }
        _ => {}
    }

    Ok(count)
}

fn create_one_bin_link(
    bin_dir: &Path,
    bin_name: &str,
    pkg_dir: &Path,
    rel_path: &str,
) -> std::io::Result<bool> {
    let target_abs = pkg_dir.join(rel_path);
    if !target_abs.exists() {
        return Ok(false);
    }

    let link_path = bin_dir.join(bin_name);

    // Remove existing link if present (idempotent)
    let _ = std::fs::remove_file(&link_path);

    // Target: relative path from bin_dir to pkg_dir/rel_path
    // e.g.: ../typescript/bin/tsc
    let rel_target = pathdiff::diff_paths(&target_abs, bin_dir)
        .unwrap_or_else(|| target_abs.clone());

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&rel_target, &link_path)?;
        // Ensure target is executable
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&target_abs) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(&target_abs, perms);
        }
    }
    #[cfg(not(unix))]
    {
        // Windows: copy instead of symlink (requires admin for symlinks)
        std::fs::copy(&target_abs, &link_path)?;
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_bin_symlinks_for_package_with_object_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();

        // Create node_modules/typescript with bin: {"tsc": "./bin/tsc"}
        let ts_dir = ws.join("node_modules/typescript");
        std::fs::create_dir_all(ts_dir.join("bin")).unwrap();
        std::fs::write(ts_dir.join("bin/tsc"), b"#!/usr/bin/env node\n").unwrap();
        std::fs::write(
            ts_dir.join("package.json"),
            r#"{"name":"typescript","version":"5.0.0","bin":{"tsc":"./bin/tsc"}}"#,
        )
        .unwrap();

        let count = create_bin_links(ws).unwrap();

        assert_eq!(count, 1);
        assert!(ws.join("node_modules/.bin/tsc").exists());
        // Verify it's a symlink pointing toward typescript/bin/tsc
        let target = std::fs::read_link(ws.join("node_modules/.bin/tsc")).unwrap();
        assert!(
            target.to_string_lossy().contains("tsc"),
            "symlink target should contain 'tsc', got: {}",
            target.display()
        );
    }

    #[test]
    fn skips_hidden_dirs_like_dot_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        std::fs::create_dir_all(ws.join("node_modules/.bin")).unwrap();
        let count = create_bin_links(ws).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn handles_scoped_packages_without_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let pkg_dir = ws.join("node_modules/@types/node");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        // @types/node has no bin field — should produce 0
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"@types/node","version":"20.0.0"}"#,
        )
        .unwrap();
        let count = create_bin_links(ws).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn handles_string_bin_field() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let pkg_dir = ws.join("node_modules/semver");
        std::fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        std::fs::write(pkg_dir.join("bin/semver.js"), b"#!/usr/bin/env node\n").unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"semver","version":"7.0.0","bin":"./bin/semver.js"}"#,
        )
        .unwrap();

        let count = create_bin_links(ws).unwrap();
        assert_eq!(count, 1);
        assert!(ws.join("node_modules/.bin/semver").exists());
    }

    #[test]
    fn skips_missing_bin_target() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let pkg_dir = ws.join("node_modules/broken-pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        // bin field points to a non-existent file
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"broken-pkg","version":"1.0.0","bin":"./bin/does-not-exist"}"#,
        )
        .unwrap();

        let count = create_bin_links(ws).unwrap();
        assert_eq!(count, 0, "should skip bins whose target file is missing");
    }

    #[test]
    fn idempotent_on_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let pkg_dir = ws.join("node_modules/chalk");
        std::fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        std::fs::write(pkg_dir.join("bin/chalk"), b"#!/usr/bin/env node\n").unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"chalk","version":"5.0.0","bin":{"chalk":"./bin/chalk"}}"#,
        )
        .unwrap();

        let count1 = create_bin_links(ws).unwrap();
        assert_eq!(count1, 1);

        // Second call must not fail (remove_file + re-symlink is idempotent)
        let count2 = create_bin_links(ws).unwrap();
        assert_eq!(count2, 1);
        assert!(ws.join("node_modules/.bin/chalk").exists());
    }

    #[test]
    fn handles_scoped_package_with_bin() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let pkg_dir = ws.join("node_modules/@angular/cli");
        std::fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        std::fs::write(pkg_dir.join("bin/ng"), b"#!/usr/bin/env node\n").unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"@angular/cli","version":"16.0.0","bin":{"ng":"./bin/ng"}}"#,
        )
        .unwrap();

        let count = create_bin_links(ws).unwrap();
        assert_eq!(count, 1);
        assert!(ws.join("node_modules/.bin/ng").exists());
    }

    #[test]
    fn multiple_bins_in_one_package() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let pkg_dir = ws.join("node_modules/multi-bin");
        std::fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        std::fs::write(pkg_dir.join("bin/foo"), b"#!/usr/bin/env node\n").unwrap();
        std::fs::write(pkg_dir.join("bin/bar"), b"#!/usr/bin/env node\n").unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"multi-bin","version":"1.0.0","bin":{"foo":"./bin/foo","bar":"./bin/bar"}}"#,
        )
        .unwrap();

        let count = create_bin_links(ws).unwrap();
        assert_eq!(count, 2);
        assert!(ws.join("node_modules/.bin/foo").exists());
        assert!(ws.join("node_modules/.bin/bar").exists());
    }
}
