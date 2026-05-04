//! Deliverables and verification gate (§8).
//!
//! Manages artifact publication, TOCTOU protection, and integration ordering.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ForgeResult;

// ─── Artifacts manifest (§8.3) ──────────────────────────────────────────

/// Contents of `deliverables/vX.Y.Z/artifacts.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactsManifest {
    pub version: String,
    pub schema_version: u32,
    pub verify_result: String,
    pub state_sequence: u64,
    pub files: Vec<ArtifactEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactEntry {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

impl ArtifactsManifest {
    /// Create a manifest from a list of file paths under the deliverables directory.
    pub fn from_dir(
        version: &str,
        verify_result: &str,
        state_sequence: u64,
        deliverables_dir: &Path,
    ) -> ForgeResult<Self> {
        let mut files = Vec::new();

        if deliverables_dir.exists() {
            for entry in walk_dir(deliverables_dir)? {
                if entry == "artifacts.toml" || entry == "CHANGELOG.md" {
                    continue;
                }
                let full_path = deliverables_dir.join(&entry);
                let content = std::fs::read(&full_path)?;
                let mut hasher = Sha256::new();
                hasher.update(&content);
                let hash = format!("{:x}", hasher.finalize());
                let size = content.len() as u64;

                files.push(ArtifactEntry { path: entry, sha256: hash, size_bytes: size });
            }
        }

        Ok(Self {
            version: version.to_string(),
            schema_version: 1,
            verify_result: verify_result.to_string(),
            state_sequence,
            files,
        })
    }

    /// Save to `artifacts.toml`.
    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize artifacts: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }

    /// Load from `artifacts.toml`.
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let manifest: Self = toml::from_str(&content).map_err(|e| {
            crate::error::ForgeError::Config(format!("invalid artifacts.toml: {e}"))
        })?;
        Ok(manifest)
    }

    /// Verify that all files in the manifest still exist and have matching hashes.
    pub fn verify_integrity(&self, base_dir: &Path) -> ForgeResult<bool> {
        for entry in &self.files {
            let full_path = base_dir.join(&entry.path);
            if !full_path.exists() {
                return Ok(false);
            }
            let content = std::fs::read(&full_path)?;
            let mut hasher = Sha256::new();
            hasher.update(&content);
            let actual_hash = format!("{:x}", hasher.finalize());
            if actual_hash != entry.sha256 {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// Recursively collect relative file paths under a directory.
fn walk_dir(dir: &Path) -> ForgeResult<Vec<String>> {
    let mut files = Vec::new();
    walk_recursive(dir, dir, &mut files)?;
    Ok(files)
}

fn walk_recursive(base: &Path, dir: &Path, files: &mut Vec<String>) -> ForgeResult<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_recursive(base, &path, files)?;
        } else {
            let rel = path.strip_prefix(base).unwrap_or(&path);
            files.push(rel.to_string_lossy().to_string());
        }
    }
    Ok(())
}

// ─── Versioned deliverables directory (§8.3) ────────────────────────────

/// Create a versioned deliverables directory.
///
/// Layout: `<node>/.forge/deliverables/v<version>/`
pub fn create_deliverables_dir(cwd: &Path, version: &str) -> ForgeResult<PathBuf> {
    let dir = cwd.join(".forge/deliverables").join(format!("v{version}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Write a CHANGELOG.md in the deliverables directory.
pub fn write_changelog(deliverables_dir: &Path, entries: &[&str]) -> ForgeResult<()> {
    let changelog_path = deliverables_dir.join("CHANGELOG.md");
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let mut content = format!(
        "# Changelog — v{}\n\nGenerated: {now}\n\n",
        deliverables_dir.file_name().unwrap_or_default().to_string_lossy()
    );
    for entry in entries {
        content.push_str(&format!("- {entry}\n"));
    }
    std::fs::write(&changelog_path, content)?;
    Ok(())
}

// ─── TOCTOU protection (§8.2, §12) ──────────────────────────────────────

/// Check for TOCTOU mismatch between artifacts and current state.
///
/// Returns true if the artifacts match the expected state sequence.
#[must_use]
pub const fn check_toc_tou(manifest: &ArtifactsManifest, current_state_seq: u64) -> bool {
    manifest.state_sequence == current_state_seq
}

// ─── Integration order (§10.2) ──────────────────────────────────────────

/// Role ordering for integration: HAL → BSP → MW → APP (plus extensions).
const ROLE_ORDER: &[&str] = &["hal", "bsp", "mw", "app", "drv", "test", "tools"];

/// Get the integration priority for a role. Lower = earlier in integration.
#[must_use]
pub fn integration_priority(role: &str) -> usize {
    ROLE_ORDER.iter().position(|r| *r == role).unwrap_or(usize::MAX)
}

/// Topologically sort nodes by role for integration ordering.
#[must_use]
pub fn sort_by_integration_order(nodes: &[(String, String)], // (name, role)
) -> Vec<String> {
    let mut with_prio: Vec<_> =
        nodes.iter().map(|(name, role)| (integration_priority(role), name.clone())).collect();
    with_prio.sort_by_key(|(prio, _)| *prio);
    with_prio.into_iter().map(|(_, name)| name).collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artifacts_manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts_dir = dir.path().join("deliverables/v0.1.0");
        std::fs::create_dir_all(&artifacts_dir).unwrap();

        // Create some dummy artifact files
        std::fs::write(artifacts_dir.join("libhal.a"), b"binary content").unwrap();
        std::fs::write(artifacts_dir.join("hal.h"), b"header content").unwrap();

        let manifest = ArtifactsManifest::from_dir("0.1.0", "pass", 42, &artifacts_dir).unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert_eq!(manifest.state_sequence, 42);
        assert_eq!(manifest.verify_result, "pass");

        // Save and reload
        let manifest_path = artifacts_dir.join("artifacts.toml");
        manifest.save(&manifest_path).unwrap();
        let loaded = ArtifactsManifest::load(&manifest_path).unwrap();
        assert_eq!(loaded.files.len(), 2);

        // Verify integrity
        assert!(loaded.verify_integrity(&artifacts_dir).unwrap());
    }

    #[test]
    fn test_artifacts_integrity_tampered() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts_dir = dir.path().join("deliverables/v0.1.0");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::write(artifacts_dir.join("secret.bin"), b"original").unwrap();

        let manifest = ArtifactsManifest::from_dir("0.1.0", "pass", 1, &artifacts_dir).unwrap();
        assert!(manifest.verify_integrity(&artifacts_dir).unwrap());

        // Tamper
        std::fs::write(artifacts_dir.join("secret.bin"), b"tampered!").unwrap();
        assert!(!manifest.verify_integrity(&artifacts_dir).unwrap());
    }

    #[test]
    fn test_toc_tou_check() {
        let manifest = ArtifactsManifest {
            version: "0.1.0".into(),
            schema_version: 1,
            verify_result: "pass".into(),
            state_sequence: 42,
            files: vec![],
        };
        assert!(check_toc_tou(&manifest, 42));
        assert!(!check_toc_tou(&manifest, 43));
    }

    #[test]
    fn test_integration_order() {
        assert!(integration_priority("hal") < integration_priority("bsp"));
        assert!(integration_priority("bsp") < integration_priority("mw"));
        assert!(integration_priority("mw") < integration_priority("app"));
    }

    #[test]
    fn test_sort_by_integration_order() {
        let nodes = vec![
            ("app-main".into(), "app".into()),
            ("bsp-uart".into(), "bsp".into()),
            ("hal-clock".into(), "hal".into()),
            ("mw-usb".into(), "mw".into()),
        ];
        let ordered = sort_by_integration_order(&nodes);
        assert_eq!(ordered[0], "hal-clock");
        assert_eq!(ordered[1], "bsp-uart");
        assert_eq!(ordered[2], "mw-usb");
        assert_eq!(ordered[3], "app-main");
    }
}
