//! Error types for `CortexForge`.

use crate::types::NodeName;

/// Unified error type for all `CortexForge` operations.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("spawn failed for node '{node}': {reason}")]
    Spawn { node: NodeName, reason: String },

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("invalid state transition for node '{node}': {from} -> {to}")]
    StateInvalid { node: NodeName, from: String, to: String },

    #[error("permission denied: {0}")]
    Permission(String),

    #[error("dependency cycle detected involving nodes: {nodes:?}")]
    DependencyCycle { nodes: Vec<String> },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("telemetry error: {0}")]
    Telemetry(String),

    #[error("{0}")]
    Other(String),
}

impl ForgeError {
    #[must_use]
    pub fn telemetry(msg: impl Into<String>) -> Self {
        Self::Telemetry(msg.into())
    }
}

/// Alias for Result with `ForgeError`.
pub type ForgeResult<T> = Result<T, ForgeError>;
