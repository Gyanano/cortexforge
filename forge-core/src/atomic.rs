//! Atomic file I/O utilities (§15.8).
//!
//! `atomic_write`: tmp + rename for crash-safe writes.
//! `safe_read_toml`: tolerant read, returns None on any failure.

use std::fs;
use std::path::Path;

/// Atomically write content to a file using tmp + rename.
///
/// On most filesystems, rename is atomic within the same filesystem.
pub fn atomic_write(path: &Path, content: &str) -> Result<(), std::io::Error> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Tolerantly read and parse a TOML file. Returns `None` on any error.
#[must_use] 
pub fn safe_read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let s = fs::read_to_string(path).ok()?;
    toml::from_str(&s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    

    #[test]
    fn test_atomic_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");

        atomic_write(&path, "[state]\ncurrent = \"idle\"\n").unwrap();
        assert!(path.exists());
        // tmp file should be gone
        assert!(!dir.path().join("test.tmp").exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("idle"));
    }

    #[test]
    fn test_safe_read_toml_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        fs::write(&path, "this is not valid toml {{{").unwrap();
        let result: Option<toml::Value> = safe_read_toml(&path);
        assert!(result.is_none());
    }

    #[test]
    fn test_safe_read_toml_missing() {
        let result: Option<toml::Value> = safe_read_toml(Path::new("/nonexistent/file.toml"));
        assert!(result.is_none());
    }

    #[test]
    fn test_atomic_write_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overwrite.toml");

        atomic_write(&path, "v1\n").unwrap();
        atomic_write(&path, "v2\n").unwrap();
        // Should contain v2, not v1
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "v2\n");
        // No tmp residue
        assert!(!dir.path().join("overwrite.tmp").exists());
    }
}
