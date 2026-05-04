//! Configuration types for forge.toml and node.toml.

use serde::{Deserialize, Serialize};

/// Root configuration (forge.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeConfig {
    pub forge: ForgeSection,
    #[serde(default)]
    pub budget: BudgetSection,
    #[serde(default)]
    pub paths: PathsSection,
}

/// Minimal struct to hold a forge root path and provide sub-path builders.
pub struct ForgePaths {
    root: std::path::PathBuf,
}

impl ForgePaths {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    #[must_use]
    pub fn state_file(&self, node_cwd: &str) -> std::path::PathBuf {
        self.root.join(node_cwd).join(".forge/state.toml")
    }

    #[must_use]
    pub fn inbox_dir(&self, node_cwd: &str) -> std::path::PathBuf {
        self.root.join(node_cwd).join(".forge/inbox")
    }

    #[must_use]
    pub fn shared_dir(&self, node_cwd: &str) -> std::path::PathBuf {
        self.root.join(node_cwd).join("shared")
    }

    #[must_use]
    pub fn pid_file(&self, node_cwd: &str) -> std::path::PathBuf {
        self.root.join(node_cwd).join(".forge/pid")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeSection {
    pub schema_version: u32,
    pub max_depth: u32,
    #[serde(default = "default_max_total_nodes")]
    pub max_total_nodes: u32,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_sec: u32,
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_sec: u32,
    #[serde(default = "default_max_retries")]
    pub default_max_retries: u32,
    #[serde(default = "default_stuck_threshold")]
    pub stuck_threshold_heartbeats: u32,
    #[serde(default = "default_scan_interval")]
    pub scan_interval_sec: u32,
    #[serde(default = "default_spawn_timeout")]
    pub spawn_timeout_sec: u32,
}

const fn default_max_total_nodes() -> u32 {
    64
}
const fn default_heartbeat_interval() -> u32 {
    15
}
const fn default_heartbeat_timeout() -> u32 {
    60
}
const fn default_max_retries() -> u32 {
    3
}
const fn default_stuck_threshold() -> u32 {
    4
}
const fn default_scan_interval() -> u32 {
    5
}
const fn default_spawn_timeout() -> u32 {
    30
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetSection {
    #[serde(default)]
    pub global: GlobalBudget,
    #[serde(default)]
    pub per_layer: Vec<LayerBudgetEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerBudgetEntry {
    pub layer: u32,
    #[serde(default)]
    pub tokens: Option<u64>,
    #[serde(default)]
    pub wallclock_sec: Option<u64>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalBudget {
    #[serde(default)]
    pub max_tokens_total: Option<u64>,
    #[serde(default)]
    pub max_wallclock_total_sec: Option<u64>,
}

impl Default for GlobalBudget {
    fn default() -> Self {
        Self { max_tokens_total: Some(5_000_000), max_wallclock_total_sec: Some(14400) }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathsSection {
    #[serde(default = "default_event_bus")]
    pub event_bus: String,
    #[serde(default = "default_escalated")]
    pub escalated: String,
}

fn default_event_bus() -> String {
    ".forge/eventbus.log".into()
}
fn default_escalated() -> String {
    ".forge/escalated.toml".into()
}
