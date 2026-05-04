//! Permission and isolation model (§9).
//!
//! Enforces per-node file boundaries (realpath anchored to cwd),
//! Bash command allowlists, network control, and spawn authority.
//! Sandbox integration point is reserved for post-MVP hardening.

use std::path::{Path, PathBuf};

use crate::error::{ForgeError, ForgeResult};

// ─── File isolation (§9) ────────────────────────────────────────────────

/// Check if a file path is within the node's allowed cwd.
///
/// Uses realpath (canonicalize) to resolve symlinks and prevent
/// path-traversal attacks (e.g. `../../sibling/secret`).
pub fn check_file_access(cwd: &Path, target: &Path) -> ForgeResult<()> {
    // If target doesn't exist yet (write), canonicalize the parent
    let resolved = if target.exists() {
        target.canonicalize()
    } else if let Some(parent) = target.parent() {
        if parent.exists() {
            let resolved_parent = parent.canonicalize()?;
            Ok(resolved_parent.join(target.file_name().unwrap_or_default()))
        } else {
            // Parent doesn't exist — can't resolve. Allow for now (file creation).
            return Ok(());
        }
    } else {
        return Ok(()); // No parent path, allow
    }?;

    let resolved_cwd = cwd.canonicalize()?;

    if !resolved.starts_with(&resolved_cwd) {
        return Err(ForgeError::Permission(format!(
            "access denied: {} is outside cwd {}",
            target.display(),
            cwd.display()
        )));
    }
    Ok(())
}

// ─── Bash allowlist (§9) ────────────────────────────────────────────────

/// Check if a bash command is in the node's allowlist.
///
/// Returns the command prefix (first word) for matching.
pub fn check_bash_command(
    allowlist: &[String],
    command: &str,
) -> ForgeResult<()> {
    if allowlist.is_empty() {
        return Err(ForgeError::Permission(
            "bash execution denied: no allowlist configured".into(),
        ));
    }

    let cmd_prefix = command.split_whitespace().next().unwrap_or("");

    if allowlist.iter().any(|allowed| cmd_prefix.starts_with(allowed.as_str())) {
        Ok(())
    } else {
        Err(ForgeError::Permission(format!(
            "bash command '{cmd_prefix}' not in allowlist: {allowlist:?}"
        )))
    }
}

// ─── Network control (§9) ───────────────────────────────────────────────

/// Check if network access is allowed for this node.
pub fn check_network_access(network_allowed: bool) -> ForgeResult<()> {
    if network_allowed {
        Ok(())
    } else {
        Err(ForgeError::Permission(
            "network access denied: not enabled in node.toml".into(),
        ))
    }
}

// ─── Spawn authority (§9) ───────────────────────────────────────────────

/// Check if a node has spawn authority.
///
/// Only Domain Agents can request child spawn, and only through
/// `spawn_requests.toml` — never directly. Orchestrator is the sole
/// process with actual subprocess spawn capability.
pub fn check_spawn_authority(role: &str) -> ForgeResult<()> {
    if role == "domain" || role == "orchestrator" {
        Ok(())
    } else {
        Err(ForgeError::Permission(format!(
            "spawn authority denied: role '{role}' cannot spawn children"
        )))
    }
}

// ─── Sandbox interface (reserved for post-MVP) ──────────────────────────

/// Sandbox configuration — reserved interface for §9.
///
/// Post-MVP hardening will support chroot / bubblewrap / Docker backends.
#[derive(Debug, Clone)]
#[derive(Default)]
pub enum SandboxConfig {
    /// No sandbox (MVP default).
    #[default]
    None,
    /// chroot-based isolation (future).
    Chroot { root: PathBuf },
    /// bubblewrap-based isolation (future).
    Bubblewrap { profile: String },
    /// Docker-based isolation (future).
    Docker { image: String },
}


impl SandboxConfig {
    #[must_use] 
    pub const fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_access_allowed_within_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let file = cwd.join("allowed.txt");
        std::fs::write(&file, "ok").unwrap();
        assert!(check_file_access(cwd, &file).is_ok());
    }

    #[test]
    fn test_file_access_denied_outside_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let file = outside.path().join("secret.txt");
        std::fs::write(&file, "secret").unwrap();
        assert!(check_file_access(cwd, &file).is_err());
    }

    #[test]
    fn test_file_access_nonexistent_parent_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let file = cwd.join("new-file.txt");
        // Doesn't exist yet — write operation
        assert!(check_file_access(cwd, &file).is_ok());
    }

    #[test]
    fn test_bash_allowlist_empty_deny() {
        let allowlist: Vec<String> = vec![];
        assert!(check_bash_command(&allowlist, "make build").is_err());
    }

    #[test]
    fn test_bash_allowlist_match() {
        let allowlist = vec!["make".into(), "west".into(), "cmake".into()];
        assert!(check_bash_command(&allowlist, "make -j4").is_ok());
        assert!(check_bash_command(&allowlist, "west build").is_ok());
        assert!(check_bash_command(&allowlist, "cmake -S . -B build").is_ok());
    }

    #[test]
    fn test_bash_allowlist_deny() {
        let allowlist = vec!["make".into()];
        assert!(check_bash_command(&allowlist, "rm -rf /").is_err());
        assert!(check_bash_command(&allowlist, "curl evil.com").is_err());
    }

    #[test]
    fn test_network_default_deny() {
        assert!(check_network_access(false).is_err());
        assert!(check_network_access(true).is_ok());
    }

    #[test]
    fn test_spawn_authority() {
        assert!(check_spawn_authority("domain").is_ok());
        assert!(check_spawn_authority("orchestrator").is_ok());
        assert!(check_spawn_authority("module").is_err());
        assert!(check_spawn_authority("submodule").is_err());
    }

    #[test]
    fn test_sandbox_default() {
        let s = SandboxConfig::default();
        assert!(!s.is_enabled());
    }
}
