//! Heartbeat and health monitoring (§6).
//!
//! Tracks per-node liveness via heartbeat files, detects stuck progress,
//! propagates dead branches, and enforces wallclock suicide gates.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use sha2::{Digest, Sha256};

use crate::config::ForgeConfig;
use crate::error::{ForgeError, ForgeResult};
use crate::protocol::NodeState;
use crate::state::NodeStatus;

// ─── Heartbeat record ───────────────────────────────────────────────────

/// Per-node heartbeat tracking state.
#[derive(Debug, Clone)]
pub struct HeartbeatRecord {
    pub node_name: String,
    pub cwd: PathBuf,
    pub last_heartbeat: DateTime<FixedOffset>,
    pub last_sequence: u64,
    pub last_summary: String,
    pub last_summary_hash: String,
    pub consecutive_unchanged: u32,
    pub current_status: NodeStatus,
}

impl HeartbeatRecord {
    fn new(node_name: &str, cwd: &Path) -> Self {
        Self {
            node_name: node_name.to_string(),
            cwd: cwd.to_path_buf(),
            last_heartbeat: Utc::now().into(),
            last_sequence: 0,
            last_summary: String::new(),
            last_summary_hash: String::new(),
            consecutive_unchanged: 0,
            current_status: NodeStatus::Idle,
        }
    }

    /// Hash the summary string for stuck detection.
    fn hash_summary(summary: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(summary.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

// ─── Heartbeat monitor ──────────────────────────────────────────────────

/// Orchestrator-side heartbeat monitor (§6.2).
///
/// Scans child `state.toml` files, detects timeouts, stuck progress,
/// and determines actions per the (alive, state) match table.
pub struct HeartbeatMonitor {
    records: HashMap<String, HeartbeatRecord>,
    stuck_threshold: u32,
    heartbeat_timeout: Duration,
}

impl HeartbeatMonitor {
    #[must_use] 
    pub fn new(config: &ForgeConfig) -> Self {
        Self {
            records: HashMap::new(),
            stuck_threshold: config.forge.stuck_threshold_heartbeats,
            heartbeat_timeout: Duration::from_secs(u64::from(config.forge.heartbeat_timeout_sec)),
        }
    }

    /// Register a node for heartbeat tracking.
    pub fn register(&mut self, node_name: &str, cwd: &Path) {
        self.records
            .entry(node_name.to_string())
            .or_insert_with(|| HeartbeatRecord::new(node_name, cwd));
    }

    /// Remove a node from tracking.
    pub fn remove(&mut self, node_name: &str) {
        self.records.remove(node_name);
    }

    /// Scan one node's state.toml and update tracking.
    ///
    /// Returns a `ScanResult` indicating what action to take.
    pub fn scan_node(
        &mut self,
        node_name: &str,
        is_alive: bool,
    ) -> ForgeResult<ScanResult> {
        let record = self
            .records
            .get_mut(node_name)
            .ok_or_else(|| ForgeError::Other(format!("unknown node: {node_name}")))?;

        let state_path = record.cwd.join(".forge/state.toml");

        // Read state if file exists
        let state = crate::safe_read_toml::<NodeState>(&state_path);

        let current_status = state
            .as_ref()
            .map_or(NodeStatus::Dead, |s| s.state.current.parse::<NodeStatus>().unwrap_or(NodeStatus::Dead));

        record.current_status = current_status;

        // Determine action per §6.2 match table
        let action = match (is_alive, current_status) {
            (true, NodeStatus::Delivered) => {
                // Process still alive but delivered — give grace period then SIGKILL
                ScanAction::TerminateAfterGrace {
                    reason: "delivered but still alive".into(),
                    grace: Duration::from_secs(30),
                }
            }
            (true, NodeStatus::Dead) => {
                // Process alive but claims dead — force kill
                ScanAction::ForceKill {
                    reason: "state=dead but process alive".into(),
                }
            }
            (true, _) => {
                // Normal running — check heartbeat freshness
                if let Some(ref s) = state {
                    record.last_heartbeat = s.state.last_heartbeat;
                    record.last_sequence = s.state.sequence;

                    let elapsed = Utc::now().signed_duration_since(s.state.last_heartbeat);
                    let elapsed_sec = elapsed.num_seconds().max(0) as u64;

                    if elapsed_sec > self.heartbeat_timeout.as_secs() {
                        ScanAction::HeartbeatTimeout {
                            missed_for_sec: elapsed_sec,
                        }
                    } else {
                        // Check for stuck progress
                        let new_hash = HeartbeatRecord::hash_summary(&s.progress.summary);
                        if new_hash == record.last_summary_hash
                            && !s.progress.summary.is_empty()
                            && current_status != NodeStatus::Blocked
                        {
                            record.consecutive_unchanged += 1;
                        } else {
                            record.consecutive_unchanged = 0;
                        }
                        record.last_summary = s.progress.summary.clone();
                        record.last_summary_hash = new_hash;

                        if record.consecutive_unchanged >= self.stuck_threshold {
                            ScanAction::SuspectedStuck {
                                unchanged_heartbeats: record.consecutive_unchanged,
                            }
                        } else {
                            ScanAction::Healthy
                        }
                    }
                } else {
                    // No state file yet, but process is alive — likely still booting
                    ScanAction::Healthy
                }
            }
            (false, NodeStatus::Delivered) => {
                ScanAction::Reap {
                    reason: "delivered and exited".into(),
                }
            }
            (false, NodeStatus::Blocked) => {
                // Defer to Pass 7b for final dependency determination (§6.2 fix #113)
                ScanAction::DeferToDependencyCheck
            }
            (false, NodeStatus::Dead) => {
                ScanAction::Reap {
                    reason: "dead and exited".into(),
                }
            }
            (false, _) => {
                // Process died unexpectedly
                ScanAction::Crashed {
                    reason: format!("unexpected exit while in state {current_status}"),
                }
            }
        };

        Ok(ScanResult {
            node_name: node_name.to_string(),
            current_status,
            action,
        })
    }

    /// Get the heartbeat record for a node.
    #[must_use] 
    pub fn get(&self, node_name: &str) -> Option<&HeartbeatRecord> {
        self.records.get(node_name)
    }

    /// Return names of all tracked nodes.
    #[must_use] 
    pub fn tracked_nodes(&self) -> Vec<&str> {
        self.records.keys().map(std::string::String::as_str).collect()
    }
}

// ─── Scan types ─────────────────────────────────────────────────────────

/// Result of scanning a single node.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub node_name: String,
    pub current_status: NodeStatus,
    pub action: ScanAction,
}

/// Action to take based on the (alive, state) match (§6.2).
#[derive(Debug, Clone)]
pub enum ScanAction {
    /// Node is healthy, no action needed.
    Healthy,

    /// Node missed its heartbeat deadline.
    HeartbeatTimeout { missed_for_sec: u64 },

    /// Process is alive but should have exited (delivered/dead).
    TerminateAfterGrace { reason: String, grace: Duration },

    /// Force SIGKILL immediately.
    ForceKill { reason: String },

    /// Node exited cleanly, remove from tracking.
    Reap { reason: String },

    /// Node crashed unexpectedly.
    Crashed { reason: String },

    /// Progress summary hasn't changed for too long (§6.3).
    SuspectedStuck { unchanged_heartbeats: u32 },

    /// Defer to Pass 7b dependency resolution (§6.2 fix #113).
    DeferToDependencyCheck,
}

// ─── Dead branch propagation (§6.4) ─────────────────────────────────────

/// Determine the propagation action when a child is marked dead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropagationDecision {
    /// Parent can continue with partial results (child's output was optional).
    DegradeToPartial,
    /// Parent must block and escalate upward.
    EscalateBlocked,
    /// No action needed (no children affected).
    None,
}

/// Decide how a parent should react to a dead child.
///
/// §6.4: If the child's output is optional → `DegradeToPartial`.
/// Otherwise → `EscalateBlocked`.
#[must_use] 
pub const fn decide_propagation(
    child_is_optional: bool,
    parent_has_other_providers: bool,
) -> PropagationDecision {
    if child_is_optional || parent_has_other_providers {
        PropagationDecision::DegradeToPartial
    } else {
        PropagationDecision::EscalateBlocked
    }
}

/// Check if a dependency chain should propagate death.
///
/// Returns true if the requester should also be marked dead.
#[must_use] 
pub const fn should_propagate_death(
    provider_dead: bool,
    provider_has_value: bool,
    has_other_providers: bool,
    has_pending_escalation: bool,
) -> bool {
    // Per §6.4 and §15.7: propagate if provider is dead AND
    // it has no value AND no alternative provider exists AND no pending escalation
    provider_dead && !provider_has_value && !has_other_providers && !has_pending_escalation
}

// ─── Suicide gate checks (§6.5) ─────────────────────────────────────────

/// Check if a node should self-terminate based on verify retries.
#[must_use] 
pub const fn check_verify_exhausted(retry_count: u32, max_retries: u32) -> bool {
    retry_count >= max_retries
}

/// Check if wallclock budget is exhausted.
#[must_use] 
pub fn check_wallclock_exhausted(
    started_at: DateTime<FixedOffset>,
    max_wallclock_sec: u64,
) -> bool {
    let now: DateTime<FixedOffset> = Utc::now().into();
    let elapsed = (now - started_at).num_seconds() as u64;
    elapsed >= max_wallclock_sec
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        ProgressSection, StateSection,
    };

    fn write_test_state(dir: &Path, current: &str, summary: &str, seq: u64) {
        let now: DateTime<FixedOffset> = Utc::now().into();
        let state = NodeState {
            schema_version: 1,
            state: StateSection {
                current: current.into(),
                entered_at: now,
                last_heartbeat: now,
                sequence: seq,
            },
            progress: ProgressSection {
                percent_self_estimate: 50,
                summary: summary.into(),
                current_task_id: "T-001".into(),
            },
            children_view: Default::default(),
            verify: Default::default(),
            budget_used: Default::default(),
        };
        state.save(&dir.join(".forge/state.toml")).unwrap();
    }

    #[test]
    fn test_monitor_healthy_node() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        write_test_state(dir.path(), "implementing", "working on DMA", 1);

        let mut monitor = HeartbeatMonitor::new(&test_config());
        monitor.register("node-a", dir.path());

        let result = monitor.scan_node("node-a", true).unwrap();
        assert_eq!(result.current_status, NodeStatus::Implementing);
        assert!(matches!(result.action, ScanAction::Healthy));
    }

    #[test]
    fn test_monitor_heartbeat_timeout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        // Write a state with an old heartbeat
        let old_time: DateTime<FixedOffset> =
            Utc::now().checked_sub_days(chrono::Days::new(1)).unwrap().into();
        let state = NodeState {
            schema_version: 1,
            state: StateSection {
                current: "implementing".into(),
                entered_at: old_time,
                last_heartbeat: old_time,
                sequence: 1,
            },
            progress: ProgressSection {
                summary: "old".into(),
                ..Default::default()
            },
            children_view: Default::default(),
            verify: Default::default(),
            budget_used: Default::default(),
        };
        state.save(&dir.path().join(".forge/state.toml")).unwrap();

        let mut monitor = HeartbeatMonitor::new(&test_config());
        monitor.register("node-b", dir.path());

        let result = monitor.scan_node("node-b", true).unwrap();
        assert!(matches!(result.action, ScanAction::HeartbeatTimeout { .. }));
    }

    #[test]
    fn test_monitor_delivered_and_alive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        write_test_state(dir.path(), "delivered", "done", 5);

        let mut monitor = HeartbeatMonitor::new(&test_config());
        monitor.register("node-c", dir.path());

        let result = monitor.scan_node("node-c", true).unwrap();
        assert!(matches!(result.action, ScanAction::TerminateAfterGrace { .. }));
    }

    #[test]
    fn test_monitor_dead_and_exited() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        write_test_state(dir.path(), "dead", "killed", 3);

        let mut monitor = HeartbeatMonitor::new(&test_config());
        monitor.register("node-d", dir.path());

        let result = monitor.scan_node("node-d", false).unwrap();
        assert!(matches!(result.action, ScanAction::Reap { .. }));
    }

    #[test]
    fn test_monitor_crashed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        write_test_state(dir.path(), "implementing", "mid-work", 7);

        let mut monitor = HeartbeatMonitor::new(&test_config());
        monitor.register("node-e", dir.path());

        let result = monitor.scan_node("node-e", false).unwrap();
        assert!(matches!(result.action, ScanAction::Crashed { .. }));
    }

    #[test]
    fn test_stuck_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        let mut config = test_config();
        config.forge.stuck_threshold_heartbeats = 2;

        let mut monitor = HeartbeatMonitor::new(&config);
        monitor.register("node-f", dir.path());

        // Scan 1: initial, no baseline yet → Healthy
        write_test_state(dir.path(), "implementing", "same summary", 1);
        let r1 = monitor.scan_node("node-f", true).unwrap();
        assert!(matches!(r1.action, ScanAction::Healthy));

        // Scan 2: same summary → 1st consecutive (below threshold 2) → Healthy
        write_test_state(dir.path(), "implementing", "same summary", 2);
        let r2 = monitor.scan_node("node-f", true).unwrap();
        assert!(matches!(r2.action, ScanAction::Healthy));

        // Scan 3: same summary → 2nd consecutive (reaches threshold) → SuspectedStuck
        write_test_state(dir.path(), "implementing", "same summary", 3);
        let r3 = monitor.scan_node("node-f", true).unwrap();
        assert!(
            matches!(r3.action, ScanAction::SuspectedStuck { .. }),
            "expected SuspectedStuck, got {:?}", r3.action
        );

        // Scan 4: different summary → resets counter → Healthy
        write_test_state(dir.path(), "implementing", "new summary", 4);
        let r4 = monitor.scan_node("node-f", true).unwrap();
        assert!(matches!(r4.action, ScanAction::Healthy));
    }

    #[test]
    fn test_stuck_not_triggered_for_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();

        let mut config = test_config();
        config.forge.stuck_threshold_heartbeats = 1;

        let mut monitor = HeartbeatMonitor::new(&config);
        monitor.register("node-g", dir.path());

        // Blocked nodes can legitimately stay unchanged — don't flag as stuck
        write_test_state(dir.path(), "blocked", "waiting for APB1_CLK", 1);
        let r1 = monitor.scan_node("node-g", true).unwrap();
        assert!(matches!(r1.action, ScanAction::Healthy));

        write_test_state(dir.path(), "blocked", "waiting for APB1_CLK", 2);
        let r2 = monitor.scan_node("node-g", true).unwrap();
        assert!(matches!(r2.action, ScanAction::Healthy));
    }

    #[test]
    fn test_propagation_decision() {
        // Optional child → degrade
        assert_eq!(
            decide_propagation(true, false),
            PropagationDecision::DegradeToPartial
        );
        // Has other providers → degrade
        assert_eq!(
            decide_propagation(false, true),
            PropagationDecision::DegradeToPartial
        );
        // Critical child, no alternative → escalate
        assert_eq!(
            decide_propagation(false, false),
            PropagationDecision::EscalateBlocked
        );
    }

    #[test]
    fn test_dependency_death_propagation() {
        // provider dead, has value → don't propagate
        assert!(!should_propagate_death(true, true, false, false));
        // provider dead, no value, no alternative, no escalation → propagate
        assert!(should_propagate_death(true, false, false, false));
        // provider dead, but has alternative → don't propagate
        assert!(!should_propagate_death(true, false, true, false));
        // provider dead, but pending escalation → don't propagate
        assert!(!should_propagate_death(true, false, false, true));
        // provider alive → don't propagate
        assert!(!should_propagate_death(false, false, false, false));
    }

    #[test]
    fn test_suicide_checks() {
        // Verify retries exhausted
        assert!(check_verify_exhausted(3, 3));
        assert!(!check_verify_exhausted(2, 3));

        // Wallclock exhausted (0 sec = instantly)
        let now: DateTime<FixedOffset> = Utc::now().into();
        assert!(check_wallclock_exhausted(now, 0));
    }

    #[test]
    fn test_monitor_deferred_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        write_test_state(dir.path(), "blocked", "waiting", 1);

        let mut monitor = HeartbeatMonitor::new(&test_config());
        monitor.register("node-h", dir.path());

        let result = monitor.scan_node("node-h", false).unwrap();
        assert!(matches!(result.action, ScanAction::DeferToDependencyCheck));
    }

    fn test_config() -> ForgeConfig {
        ForgeConfig {
            forge: crate::config::ForgeSection {
                schema_version: 1,
                max_depth: 4,
                max_total_nodes: 64,
                heartbeat_interval_sec: 15,
                heartbeat_timeout_sec: 60,
                default_max_retries: 3,
                stuck_threshold_heartbeats: 4,
                scan_interval_sec: 5,
                spawn_timeout_sec: 30,
            },
            budget: Default::default(),
            paths: Default::default(),
        }
    }
}
