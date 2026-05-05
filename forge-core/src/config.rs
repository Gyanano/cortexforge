//! Configuration types for forge.toml and node.toml.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Root configuration (forge.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeConfig {
    pub forge: ForgeSection,
    #[serde(default)]
    pub product: ProductSection,
    #[serde(default)]
    pub feedback: FeedbackSection,
    #[serde(default)]
    pub budget: BudgetSection,
    #[serde(default)]
    pub paths: PathsSection,
    #[serde(default)]
    pub llm: LlmSection,
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

    #[must_use]
    pub fn telemetry_dir(&self, node_cwd: &str) -> std::path::PathBuf {
        self.root.join(node_cwd).join(".forge/telemetry")
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

// ── Product section (§10) ─────────────────────────────────────────────────

/// High-level product description — what the user is building.
/// This drives AI-based module tree design during `forge init`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProductSection {
    /// Short product name (e.g. "Smart LED Strip Controller")
    #[serde(default)]
    pub name: String,
    /// Free-text product description
    #[serde(default)]
    pub description: String,
    /// What the product should accomplish
    #[serde(default)]
    pub goal: String,
    /// Hard constraints (e.g. "must fit in 64KB flash", "< 50mA sleep current")
    #[serde(default)]
    pub constraints: Vec<String>,
}

// ── Feedback section (§10) ────────────────────────────────────────────────

/// Runtime execution feedback configuration.
/// Defines how CortexForge monitors MCU program execution at runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedbackSection {
    /// Available monitoring channels on the target hardware
    #[serde(default)]
    pub channels: Vec<FeedbackChannelConfig>,
    /// Directory for telemetry data files (default: ".forge/telemetry")
    #[serde(default = "default_telemetry_dir")]
    pub telemetry_dir: String,
    /// Anomaly detection configuration
    #[serde(default)]
    pub anomaly_detection: AnomalyDetectionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackChannelConfig {
    /// Unique name for this channel (e.g. "debug-uart", "i2c-sensors")
    pub name: String,
    /// Physical interface type
    #[serde(rename = "type")]
    pub channel_type: ChannelType,
    /// Pin assignments (e.g. ["PA9", "PA10"] for UART TX/RX)
    #[serde(default)]
    pub pins: Vec<String>,
    /// Additional parameters (baud_rate, address, etc.)
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelType {
    Uart,
    I2c,
    Spi,
    Swo,
    Rtt,
    Gpio,
    Adc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyDetectionConfig {
    /// How many samples to buffer per stream for pattern matching
    #[serde(default = "default_anomaly_window")]
    pub window_samples: u32,
    /// Deviation threshold (0.0-1.0) for numeric range rules
    #[serde(default = "default_anomaly_threshold")]
    pub deviation_threshold: f64,
    /// Whether to auto-trigger the diagnose→fix→rebuild→reflash cycle
    #[serde(default)]
    pub auto_fix_enabled: bool,
}

impl Default for AnomalyDetectionConfig {
    fn default() -> Self {
        Self { window_samples: 20, deviation_threshold: 0.15, auto_fix_enabled: false }
    }
}

const fn default_anomaly_window() -> u32 {
    20
}
const fn default_anomaly_threshold() -> f64 {
    0.15
}
fn default_telemetry_dir() -> String {
    ".forge/telemetry".into()
}

/// LLM configuration section — API keys stored per-project (not in environment).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmSection {
    /// DeepSeek API key for the flesh-out step during `forge init`.
    /// Stored per-project so different projects can use different keys.
    /// If absent, falls back to DEEPSEEK_API_KEY env var (with a warning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deepseek_api_key: Option<String>,
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
