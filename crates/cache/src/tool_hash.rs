//! Hash a tool binary by path. Used as part of the weak fingerprint so a
//! tsc / cargo / rustc upgrade invalidates caches.

use std::path::Path;

/// Hash the bytes of the binary at `path`.
///
/// Returns:
///   - `Some(hex)` when the file exists and is readable.
///   - `None` when the file is missing or unreadable. Callers should fall back
///     to hashing only the path string.
pub fn hash_tool_binary(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let h = blake3::hash(&bytes);
    Some(h.to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn hashes_file_contents() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("tool");
        std::fs::write(&p, b"#!/bin/sh\necho hi\n").unwrap();
        let h = hash_tool_binary(&p).unwrap();
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn missing_returns_none() {
        assert!(hash_tool_binary(Path::new("/nope/nope/nope")).is_none());
    }

    #[test]
    fn different_content_different_hash() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"a").unwrap();
        let b = dir.path().join("b");
        std::fs::write(&b, b"b").unwrap();
        assert_ne!(hash_tool_binary(&a).unwrap(), hash_tool_binary(&b).unwrap());
    }
}
