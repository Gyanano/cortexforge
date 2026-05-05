//! File protocol types — all TOML schema structs from §4.
//!
//! Each subsection corresponds to its architecture-document section:
//! - §4.1 node.toml — `NodeDefinition`
//! - §4.2 state.toml — `NodeState`
//! - §4.3 inbox/*.toml — `InboxMessage`
//! - §4.4 shared/ — `NeedsDeclaration`, `ProvidesDeclaration`, `ResolvedValues`, `TaskList`
//! - §4.5 eventbus.log — see `crate::event`
//! - §4.6 forge.toml — see `crate::config`
//! - §4.7 escalated.toml — `EscalatedTable`

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, ForgeResult};
use crate::types::NodeRole;

// ─── §4.1 node.toml — NodeDefinition ───────────────────────────────────────

/// Static node definition (§4.1). Written by humans, read by the Orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDefinition {
    pub node: NodeDefSection,
    #[serde(default)]
    pub children: ChildrenSection,
    #[serde(default)]
    pub provides: NodeProvidesSection,
    #[serde(default)]
    pub budget: NodeBudgetSection,
    #[serde(default)]
    pub runtime: NodeRuntimeSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeDefSection {
    pub name: String,
    pub role: NodeRole,
    pub cwd: String,
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub depth: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChildrenSection {
    #[serde(default)]
    pub declared: Vec<String>,
    #[serde(default)]
    pub spawn_strategy: SpawnStrategy,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnStrategy {
    #[default]
    Lazy,
    Eager,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeProvidesSection {
    #[serde(default)]
    pub declared: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeBudgetSection {
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    #[serde(default = "default_max_wallclock")]
    pub max_wallclock_sec: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_max_subprocess")]
    pub max_subprocess: u32,
}

const fn default_max_tokens() -> u64 {
    200_000
}
const fn default_max_wallclock() -> u64 {
    1800
}
const fn default_max_retries() -> u32 {
    3
}
const fn default_max_subprocess() -> u32 {
    4
}

impl Default for NodeBudgetSection {
    fn default() -> Self {
        Self {
            max_tokens: default_max_tokens(),
            max_wallclock_sec: default_max_wallclock(),
            max_retries: default_max_retries(),
            max_subprocess: default_max_subprocess(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeRuntimeSection {
    #[serde(default)]
    pub model: Option<String>,
}

impl NodeDefinition {
    /// Load a node definition from a `node.toml` file.
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let def: Self = toml::from_str(&content)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid node.toml: {e}")))?;
        def.validate()?;
        Ok(def)
    }

    /// Write this definition to a `node.toml` file.
    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize node.toml: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }

    /// Validate the node definition for correctness.
    pub fn validate(&self) -> ForgeResult<()> {
        if self.node.name.is_empty() {
            return Err(crate::error::ForgeError::Config("node.name is empty".into()));
        }
        if self.node.cwd.is_empty() {
            return Err(crate::error::ForgeError::Config("node.cwd is empty".into()));
        }
        // Check for duplicate children
        let mut seen = BTreeMap::new();
        for child in &self.children.declared {
            if seen.contains_key(child) {
                return Err(crate::error::ForgeError::Config(format!(
                    "duplicate child '{}' in node '{}'",
                    child, self.node.name
                )));
            }
            seen.insert(child, true);
        }
        if self.budget.max_tokens == 0 {
            return Err(crate::error::ForgeError::Config("budget.max_tokens must be > 0".into()));
        }
        Ok(())
    }
}

// ─── §4.2 state.toml — NodeState ──────────────────────────────────────────

/// Dynamic per-node state (§4.2). Written exclusively by the node itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeState {
    pub schema_version: u32,

    pub state: StateSection,
    #[serde(default)]
    pub progress: ProgressSection,
    #[serde(default)]
    pub children_view: ChildrenViewSection,
    #[serde(default)]
    pub verify: VerifySection,
    #[serde(default)]
    pub budget_used: BudgetUsedSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSection {
    pub current: String,
    pub entered_at: DateTime<FixedOffset>,
    pub last_heartbeat: DateTime<FixedOffset>,
    #[serde(default)]
    pub sequence: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgressSection {
    #[serde(default)]
    pub percent_self_estimate: u32,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub current_task_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChildrenViewSection {
    #[serde(default)]
    pub child: Vec<ChildViewEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChildViewEntry {
    pub name: String,
    pub state: String,
    pub last_seen_at: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifySection {
    #[serde(default)]
    pub last_run_at: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub last_result: Option<String>,
    #[serde(default)]
    pub fail_summary: Option<String>,
    #[serde(default)]
    pub retry_count: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetUsedSection {
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub wallclock_sec_used: u64,
}

impl NodeState {
    /// Load state from `.forge/state.toml`.
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let state: Self = toml::from_str(&content)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid state.toml: {e}")))?;
        if state.schema_version != 1 {
            return Err(crate::error::ForgeError::Config(format!(
                "unsupported state.toml schema_version {}",
                state.schema_version
            )));
        }
        Ok(state)
    }

    /// Atomically write state to `.forge/state.toml`.
    ///
    /// Automatically increments `sequence` and updates `last_heartbeat`.
    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize state: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }
}

// ─── §4.3 inbox/*.toml — InboxMessage ─────────────────────────────────────

/// A message delivered to a node's inbox directory (§4.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxMessage {
    pub schema_version: u32,
    pub id: String,
    pub from: String,
    pub to: String,
    pub created_at: DateTime<FixedOffset>,
    pub kind: MessageKind,
    #[serde(default)]
    pub ref_task_id: Option<String>,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub body: MessageBody,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Task,
    Review,
    Ack,
    Kill,
    Info,
    ValueChanged,
}

impl std::fmt::Display for MessageKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Task => write!(f, "task"),
            Self::Review => write!(f, "review"),
            Self::Ack => write!(f, "ack"),
            Self::Kill => write!(f, "kill"),
            Self::Info => write!(f, "info"),
            Self::ValueChanged => write!(f, "value_changed"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageBody {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    // kill-specific
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub grace_sec: Option<u32>,
    // value_changed-specific
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub old_seq: Option<u64>,
    #[serde(default)]
    pub new_seq: Option<u64>,
}

impl InboxMessage {
    /// Parse a message from its inbox file.
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let msg: Self = toml::from_str(&content)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid inbox message: {e}")))?;
        Ok(msg)
    }

    /// Write this message as a new inbox file under `inbox_dir`.
    ///
    /// Filename: `<unix_ts>-<from>-<msg_uuid>.toml`
    pub fn write_to_inbox(&self, inbox_dir: &Path) -> ForgeResult<()> {
        std::fs::create_dir_all(inbox_dir)?;
        let ts = chrono::Utc::now().timestamp();
        let filename = format!("{}-{}-{}.toml", ts, self.from, self.id);
        let path = inbox_dir.join(filename);
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize message: {e}")))?;
        crate::atomic::atomic_write(&path, &content)?;
        Ok(())
    }

    /// Move a processed message to `inbox/processed/`, creating the dir if needed.
    pub fn move_to_processed(path: &Path, inbox_dir: &Path) -> ForgeResult<()> {
        let processed_dir = inbox_dir.join("processed");
        std::fs::create_dir_all(&processed_dir)?;
        let filename = path
            .file_name()
            .ok_or_else(|| crate::error::ForgeError::Other("invalid inbox file path".into()))?;
        std::fs::rename(path, processed_dir.join(filename))?;
        Ok(())
    }

    /// List all inbox messages sorted by filename (chronological).
    pub fn list_all(inbox_dir: &Path) -> ForgeResult<Vec<std::path::PathBuf>> {
        if !inbox_dir.exists() {
            return Ok(vec![]);
        }
        let mut files: Vec<_> = std::fs::read_dir(inbox_dir)?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
            .map(|e| e.path())
            .collect();
        files.sort();
        Ok(files)
    }
}

// ─── §4.4 shared/ — Dependency protocol files ─────────────────────────────

/// `shared/needs.toml` — Module declares what it needs (§4.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NeedsDeclaration {
    #[serde(default, flatten)]
    pub needs: BTreeMap<String, NeedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedEntry {
    pub desc: String,
    pub requester: String, // full relative path from forge root
}

impl NeedsDeclaration {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let nd = toml::from_str(&content)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid needs.toml: {e}")))?;
        Ok(nd)
    }

    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize needs: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }
}

/// `shared/provides.toml` — Module declares what it can provide (§4.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidesDeclaration {
    #[serde(default, flatten)]
    pub provides: BTreeMap<String, ProvideEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvideEntry {
    pub value: String,
    #[serde(default)]
    pub desc: String,
    pub seq: u64,
}

impl ProvidesDeclaration {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let pd = toml::from_str(&content)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid provides.toml: {e}")))?;
        Ok(pd)
    }

    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize provides: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }

    /// Check if a specific key exists in the provides declaration.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.provides.contains_key(key)
    }

    /// Get the entry for a key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ProvideEntry> {
        self.provides.get(key)
    }
}

/// `shared/resolved.toml` — Orchestrator writes resolved dependency values (§4.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolvedValues {
    #[serde(default, flatten)]
    pub resolved: BTreeMap<String, ResolvedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedEntry {
    pub value: String,
    pub from: String,
    pub seq: u64,
}

impl ResolvedValues {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let rv = toml::from_str(&content)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid resolved.toml: {e}")))?;
        Ok(rv)
    }

    /// Read-merge-write: merge new entries without losing existing keys.
    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize resolved: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }

    /// Check if all needed keys are present.
    #[must_use]
    pub fn has_all(&self, keys: &[String]) -> bool {
        keys.iter().all(|k| self.resolved.contains_key(k))
    }

    /// Check if a specific key exists.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.resolved.contains_key(key)
    }
}

/// `shared/tasks.toml` — Orchestrator writes pending dependency tasks (§4.4).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskList {
    #[serde(default, rename = "task")]
    pub tasks: Vec<TaskEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEntry {
    pub key: String,
    pub desc: String,
    pub from: String,
    #[serde(default = "default_task_status")]
    pub status: TaskStatus,
}

const fn default_task_status() -> TaskStatus {
    TaskStatus::Pending
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Done,
}

impl TaskList {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let tl = toml::from_str(&content)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid tasks.toml: {e}")))?;
        Ok(tl)
    }

    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize tasks: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }

    /// Check if a task already exists (dedup by key + from).
    #[must_use]
    pub fn has_task(&self, key: &str, from: &str) -> bool {
        self.tasks.iter().any(|t| t.key == key && t.from == from)
    }

    /// Add a task if not already present. Returns true if added.
    pub fn add_if_absent(&mut self, key: &str, desc: &str, from: &str) -> bool {
        if self.has_task(key, from) {
            return false;
        }
        self.tasks.push(TaskEntry {
            key: key.to_string(),
            desc: desc.to_string(),
            from: from.to_string(),
            status: TaskStatus::Pending,
        });
        true
    }

    /// Get all pending tasks.
    #[must_use]
    pub fn pending(&self) -> Vec<&TaskEntry> {
        self.tasks.iter().filter(|t| t.status == TaskStatus::Pending).collect()
    }
}

// ─── §4.4 spawn_requests.toml —────────────────────────────────────────────

/// Domain Agent spawn requests (§4.4, §15.3 Pass 6b).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpawnRequests {
    #[serde(default, rename = "request")]
    pub requests: Vec<SpawnRequestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequestEntry {
    pub name: String,
    pub cwd: String,
}

impl SpawnRequests {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let sr = toml::from_str(&content).map_err(|e| {
            crate::error::ForgeError::Config(format!("invalid spawn_requests.toml: {e}"))
        })?;
        Ok(sr)
    }

    /// Save and then clear (Orchestrator processes then empties the file).
    pub fn save_empty(path: &Path) -> ForgeResult<()> {
        let content = "\n"; // minimal valid toml
        crate::atomic::atomic_write(path, content)?;
        Ok(())
    }
}

// ─── §4.7 escalated.toml — Cross-layer routing table ──────────────────────

/// Escalated dependency table (§4.7). Persisted, survives Orchestrator restarts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EscalatedTable {
    #[serde(default, rename = "need")]
    pub needs: Vec<EscalatedNeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalatedNeed {
    pub key: String,
    pub requester: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default = "default_escalated_status")]
    pub status: EscalatedStatus,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default)]
    pub created_at: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub provides: Vec<String>,
}

const fn default_escalated_status() -> EscalatedStatus {
    EscalatedStatus::Pending
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalatedStatus {
    Pending,
    Matched,
    Resolved,
    Failed,
}

impl EscalatedTable {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        let et = toml::from_str(&content).map_err(|e| {
            crate::error::ForgeError::Config(format!("invalid escalated.toml: {e}"))
        })?;
        Ok(et)
    }

    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| crate::error::ForgeError::Config(format!("serialize escalated: {e}")))?;
        crate::atomic::atomic_write(path, &content)?;
        Ok(())
    }

    /// Check if there's already a non-terminal entry for the same requester+key.
    #[must_use]
    pub fn has_pending(&self, key: &str, requester: &str) -> bool {
        self.needs.iter().any(|e| {
            e.key == key
                && e.requester == requester
                && e.status != EscalatedStatus::Resolved
                && e.status != EscalatedStatus::Failed
        })
    }

    /// Clean up terminal entries.
    pub fn remove_terminals(&mut self) {
        self.needs.retain(|e| {
            e.status != EscalatedStatus::Resolved && e.status != EscalatedStatus::Failed
        });
    }
}

// ─── §4.8 .forge/telemetry/telemetry_declaration.toml ────────────────────

/// Declares what telemetry data a node produces at runtime.
/// Written during init or by the agent, read by the Orchestrator's telemetry scan pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryDeclaration {
    #[serde(default)]
    pub streams: Vec<TelemetryStream>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryStream {
    /// Unique stream name (e.g. "uart_tx_status", "adc_current")
    pub name: String,
    /// References a channel in forge.toml [feedback].channels
    pub channel: String,
    /// Data format of this stream
    pub format: TelemetryFormat,
    /// Expected output rate (informational)
    #[serde(default)]
    pub rate_hz: Option<f64>,
    /// Human-readable description
    #[serde(default)]
    pub desc: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryFormat {
    /// "KEY: VALUE\n" — one key-value pair per line
    KeyValue,
    /// "1234,5678,90\n" — CSV row
    Csv,
    /// NDJSON — one JSON object per line
    Json,
    /// Raw hex dump "DEADBEEF\n"
    Hex,
}

impl TelemetryDeclaration {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content)
            .map_err(|e| ForgeError::Config(format!("invalid telemetry declaration: {e}")))
    }

    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| ForgeError::Config(format!("serialize telemetry declaration: {e}")))?;
        crate::atomic_write(path, &content)?;
        Ok(())
    }
}

// ─── §4.9 .forge/telemetry/expectations.toml ────────────────────────────

/// Expected output patterns for rule-based anomaly detection.
/// Written during init or updated by agents, read by the AnomalyDetector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelemetryExpectation {
    #[serde(default)]
    pub expect: Vec<ExpectationRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectationRule {
    /// Which telemetry stream this rule applies to
    pub stream: String,
    /// Type of rule
    pub rule_type: ExpectationRuleType,
    /// Rule-specific parameters
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Severity when this rule is violated
    #[serde(default = "default_anomaly_severity")]
    pub severity: AnomalySeverity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectationRuleType {
    /// Numeric value must be within [min, max]
    Range { min: f64, max: f64 },
    /// Value must equal this string exactly
    Equals { value: String },
    /// Value must contain this substring
    Contains { substring: String },
    /// Value must match this regex pattern
    Matches { pattern: String },
    /// Value must increase monotonically (counter/accumulator)
    MonotonicIncreasing,
    /// Value must appear at least every N seconds (liveness heartbeat)
    Heartbeat { max_gap_sec: f64 },
    /// Value must NOT contain any of these error substrings
    NoError { error_substrings: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalySeverity {
    /// Log only, no action
    Info,
    /// Log and notify
    Warning,
    /// Trigger auto-fix cycle (if auto_fix_enabled)
    Critical,
}

impl AnomalySeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

const fn default_anomaly_severity() -> AnomalySeverity {
    AnomalySeverity::Warning
}

impl TelemetryExpectation {
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content)
            .map_err(|e| ForgeError::Config(format!("invalid telemetry expectations: {e}")))
    }

    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| ForgeError::Config(format!("serialize telemetry expectations: {e}")))?;
        crate::atomic_write(path, &content)?;
        Ok(())
    }
}

// ─── §4.10 .forge/telemetry/<ts>-<stream>.toml ──────────────────────────

/// A single telemetry record captured from MCU runtime.
/// Each record is stored as its own TOML file for concurrency safety.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryRecord {
    pub ts: chrono::DateTime<chrono::FixedOffset>,
    /// Which telemetry stream produced this
    pub stream: String,
    /// Which node owns this record
    pub source: String,
    /// Which physical channel carried the data
    pub channel: String,
    /// Original raw data line
    pub raw: String,
    /// Parsed key-value pairs
    #[serde(default)]
    pub parsed: BTreeMap<String, String>,
}

impl TelemetryRecord {
    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let content = toml::to_string_pretty(self)
            .map_err(|e| ForgeError::Config(format!("serialize telemetry record: {e}")))?;
        crate::atomic_write(path, &content)?;
        Ok(())
    }

    pub fn load(path: &Path) -> ForgeResult<Self> {
        let content = std::fs::read_to_string(path)?;
        toml::from_str(&content)
            .map_err(|e| ForgeError::Config(format!("invalid telemetry record: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NodeRole;

    fn test_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // ── node.toml ────────────────────────────────────────────────────

    #[test]
    fn test_node_definition_roundtrip() {
        let dir = test_dir();
        let path = dir.path().join("node.toml");

        let def = NodeDefinition {
            node: NodeDefSection {
                name: "module-bsp-uart".into(),
                role: NodeRole::Module,
                cwd: "modules/firmware/submodules/bsp-uart".into(),
                parent: "domain-firmware".into(),
                depth: 2,
            },
            children: ChildrenSection {
                declared: vec!["bsp-uart-tx".into(), "bsp-uart-rx".into()],
                spawn_strategy: SpawnStrategy::Lazy,
            },
            provides: NodeProvidesSection {
                declared: vec!["APB1_CLK".into(), "UART_TX_PIN".into()],
            },
            budget: NodeBudgetSection::default(),
            runtime: NodeRuntimeSection { model: Some("claude-sonnet-4-6".into()) },
        };

        def.save(&path).unwrap();
        let loaded = NodeDefinition::load(&path).unwrap();
        assert_eq!(loaded.node.name, "module-bsp-uart");
        assert_eq!(loaded.node.role, NodeRole::Module);
        assert_eq!(loaded.children.declared.len(), 2);
        assert_eq!(loaded.provides.declared.len(), 2);
    }

    #[test]
    fn test_node_definition_validate_empty_name() {
        let dir = test_dir();
        let path = dir.path().join("node.toml");
        let def = NodeDefinition {
            node: NodeDefSection {
                name: "".into(),
                role: NodeRole::Module,
                cwd: "m".into(),
                parent: "".into(),
                depth: 0,
            },
            children: Default::default(),
            provides: Default::default(),
            budget: Default::default(),
            runtime: Default::default(),
        };
        def.save(&path).unwrap();
        let result = NodeDefinition::load(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_node_definition_duplicate_children() {
        let def = NodeDefinition {
            node: NodeDefSection {
                name: "test".into(),
                role: NodeRole::Domain,
                cwd: "test".into(),
                parent: "".into(),
                depth: 1,
            },
            children: ChildrenSection {
                declared: vec!["dup".into(), "dup".into()],
                spawn_strategy: SpawnStrategy::Lazy,
            },
            provides: Default::default(),
            budget: Default::default(),
            runtime: Default::default(),
        };
        let result = def.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }

    // ── state.toml ───────────────────────────────────────────────────

    #[test]
    fn test_state_roundtrip() {
        let dir = test_dir();
        let path = dir.path().join("state.toml");

        let now: DateTime<FixedOffset> = chrono::Utc::now().into();
        let state = NodeState {
            schema_version: 1,
            state: StateSection {
                current: "implementing".into(),
                entered_at: now,
                last_heartbeat: now,
                sequence: 42,
            },
            progress: ProgressSection {
                percent_self_estimate: 60,
                summary: "writing DMA path".into(),
                current_task_id: "T-uart-001".into(),
            },
            children_view: Default::default(),
            verify: VerifySection { retry_count: 1, ..Default::default() },
            budget_used: BudgetUsedSection { tokens_used: 87400, wallclock_sec_used: 320 },
        };

        state.save(&path).unwrap();
        let loaded = NodeState::load(&path).unwrap();
        assert_eq!(loaded.state.current, "implementing");
        assert_eq!(loaded.state.sequence, 42);
        assert_eq!(loaded.progress.summary, "writing DMA path");
        assert_eq!(loaded.budget_used.tokens_used, 87400);
    }

    #[test]
    fn test_state_reject_wrong_schema() {
        let dir = test_dir();
        let path = dir.path().join("state.toml");
        std::fs::write(&path, "schema_version = 999\n\n[state]\ncurrent = \"idle\"\nentered_at = \"2026-01-01T00:00:00+08:00\"\nlast_heartbeat = \"2026-01-01T00:00:00+08:00\"\nsequence = 0\n").unwrap();
        let result = NodeState::load(&path);
        assert!(result.is_err());
    }

    // ── inbox messages ───────────────────────────────────────────────

    #[test]
    fn test_inbox_message_write_and_read() {
        let dir = test_dir();
        let inbox_dir = dir.path().join("inbox");

        let msg = InboxMessage {
            schema_version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            from: "domain-firmware".into(),
            to: "module-bsp-uart".into(),
            created_at: chrono::Utc::now().into(),
            kind: MessageKind::Task,
            ref_task_id: Some("T-uart-001".into()),
            priority: "P1".into(),
            body: MessageBody {
                title: "Implement UART TX".into(),
                text: "Implement the UART TX DMA path".into(),
                ..Default::default()
            },
        };

        msg.write_to_inbox(&inbox_dir).unwrap();
        let files = InboxMessage::list_all(&inbox_dir).unwrap();
        assert_eq!(files.len(), 1);

        let loaded = InboxMessage::load(&files[0]).unwrap();
        assert_eq!(loaded.kind, MessageKind::Task);
        assert_eq!(loaded.from, "domain-firmware");
        assert_eq!(loaded.body.title, "Implement UART TX");

        // Test move to processed
        InboxMessage::move_to_processed(&files[0], &inbox_dir).unwrap();
        assert!(!files[0].exists());
        let processed = inbox_dir.join("processed");
        assert!(processed.join(files[0].file_name().unwrap()).exists());
    }

    #[test]
    fn test_kill_message() {
        let dir = test_dir();
        let inbox_dir = dir.path().join("inbox");
        let msg = InboxMessage {
            schema_version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            from: "domain-firmware".into(),
            to: "module-bsp-uart".into(),
            created_at: chrono::Utc::now().into(),
            kind: MessageKind::Kill,
            ref_task_id: None,
            priority: "P0".into(),
            body: MessageBody {
                reason: Some("heartbeat_timeout".into()),
                grace_sec: Some(5),
                ..Default::default()
            },
        };
        msg.write_to_inbox(&inbox_dir).unwrap();
        let files = InboxMessage::list_all(&inbox_dir).unwrap();
        let loaded = InboxMessage::load(&files[0]).unwrap();
        assert_eq!(loaded.kind, MessageKind::Kill);
        assert_eq!(loaded.body.reason.as_deref(), Some("heartbeat_timeout"));
        assert_eq!(loaded.body.grace_sec, Some(5));
    }

    // ── shared/ dependency files ─────────────────────────────────────

    #[test]
    fn test_needs_declaration_roundtrip() {
        let dir = test_dir();
        let path = dir.path().join("needs.toml");

        let mut needs = NeedsDeclaration::default();
        needs.needs.insert(
            "APB1_CLK".into(),
            NeedEntry {
                desc: "APB1 clock for baud rate".into(),
                requester: "modules/firmware/submodules/bsp-uart".into(),
            },
        );
        needs.needs.insert(
            "UART_TX_PIN".into(),
            NeedEntry {
                desc: "UART TX pin number".into(),
                requester: "modules/firmware/submodules/bsp-uart".into(),
            },
        );

        needs.save(&path).unwrap();
        let loaded = NeedsDeclaration::load(&path).unwrap();
        assert_eq!(loaded.needs.len(), 2);
        assert!(loaded.needs.contains_key("APB1_CLK"));
        assert_eq!(loaded.needs["APB1_CLK"].requester, "modules/firmware/submodules/bsp-uart");
    }

    #[test]
    fn test_provides_declaration_roundtrip() {
        let dir = test_dir();
        let path = dir.path().join("provides.toml");

        let mut provides = ProvidesDeclaration::default();
        provides.provides.insert(
            "APB1_CLK".into(),
            ProvideEntry { value: "42000000".into(), desc: "APB1 bus clock".into(), seq: 2 },
        );

        provides.save(&path).unwrap();
        let loaded = ProvidesDeclaration::load(&path).unwrap();
        assert!(loaded.has("APB1_CLK"));
        let entry = loaded.get("APB1_CLK").unwrap();
        assert_eq!(entry.value, "42000000");
        assert_eq!(entry.seq, 2);
    }

    #[test]
    fn test_resolved_values_merge() {
        let dir = test_dir();
        let path = dir.path().join("resolved.toml");

        // First write: one key
        let mut resolved = ResolvedValues::default();
        resolved.resolved.insert(
            "APB1_CLK".into(),
            ResolvedEntry { value: "42000000".into(), from: "hal-clock".into(), seq: 1 },
        );
        resolved.save(&path).unwrap();

        // Second write: add another key without losing the first
        let mut loaded = ResolvedValues::load(&path).unwrap();
        loaded.resolved.insert(
            "APB2_CLK".into(),
            ResolvedEntry { value: "84000000".into(), from: "hal-clock".into(), seq: 1 },
        );
        loaded.save(&path).unwrap();

        // Verify both keys present
        let final_ = ResolvedValues::load(&path).unwrap();
        assert!(final_.has("APB1_CLK"));
        assert!(final_.has("APB2_CLK"));
        assert!(final_.has_all(&["APB1_CLK".into(), "APB2_CLK".into()]));
    }

    #[test]
    fn test_task_list_dedup() {
        let mut tasks = TaskList::default();
        assert!(tasks.add_if_absent("APB1_CLK", "desc", "from-a"));
        assert!(!tasks.add_if_absent("APB1_CLK", "desc", "from-a")); // duplicate
        assert!(tasks.add_if_absent("APB1_CLK", "desc", "from-b")); // different from
        assert_eq!(tasks.tasks.len(), 2);

        let pending = tasks.pending();
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn test_spawn_requests_roundtrip() {
        let dir = test_dir();
        let path = dir.path().join("spawn_requests.toml");

        let sr = SpawnRequests {
            requests: vec![
                SpawnRequestEntry {
                    name: "bsp-uart-tx".into(),
                    cwd: "modules/firmware/submodules/bsp-uart-tx".into(),
                },
                SpawnRequestEntry {
                    name: "bsp-uart-rx".into(),
                    cwd: "modules/firmware/submodules/bsp-uart-rx".into(),
                },
            ],
        };

        // Save as toml
        let content = toml::to_string_pretty(&sr).unwrap();
        std::fs::write(&path, &content).unwrap();

        let loaded = SpawnRequests::load(&path).unwrap();
        assert_eq!(loaded.requests.len(), 2);

        // Test clear
        SpawnRequests::save_empty(&path).unwrap();
        let cleared = SpawnRequests::load(&path).unwrap();
        assert!(cleared.requests.is_empty());
    }

    #[test]
    fn test_escalated_table_dedup() {
        let dir = test_dir();
        let path = dir.path().join("escalated.toml");

        let table = EscalatedTable {
            needs: vec![EscalatedNeed {
                key: "UART_TX_PIN".into(),
                requester: "tools/flasher".into(),
                provider: None,
                status: EscalatedStatus::Pending,
                attempt_count: 0,
                created_at: Some(chrono::Utc::now().into()),
                provides: vec!["FLASH_SIZE".into()],
            }],
        };

        table.save(&path).unwrap();
        let loaded = EscalatedTable::load(&path).unwrap();
        assert_eq!(loaded.needs.len(), 1);
        assert!(loaded.has_pending("UART_TX_PIN", "tools/flasher"));
        assert!(!loaded.has_pending("NONEXISTENT", "tools/flasher"));

        // Test remove terminals
        let mut t2 = EscalatedTable {
            needs: vec![
                EscalatedNeed {
                    key: "K1".into(),
                    requester: "R1".into(),
                    provider: None,
                    status: EscalatedStatus::Resolved,
                    attempt_count: 0,
                    created_at: None,
                    provides: vec![],
                },
                EscalatedNeed {
                    key: "K2".into(),
                    requester: "R2".into(),
                    provider: None,
                    status: EscalatedStatus::Pending,
                    attempt_count: 0,
                    created_at: None,
                    provides: vec![],
                },
            ],
        };
        t2.remove_terminals();
        assert_eq!(t2.needs.len(), 1);
        assert_eq!(t2.needs[0].key, "K2");
    }
}
