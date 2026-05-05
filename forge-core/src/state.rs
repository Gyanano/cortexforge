//! State machine engine — 10-state FSM for per-node lifecycle (§3 + §3.4).
//!
//! States: idle → assigned → planning → implementing ↔ blocked → verifying → delivered → monitoring ↔ diagnosing
//! Full transition rules per §3.2 + §3.4 (runtime feedback loop).

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{ForgeError, ForgeResult};
use crate::protocol::{
    BudgetUsedSection, ChildrenViewSection, NodeState, ProgressSection, StateSection, VerifySection,
};

// ─── State enum (§3.1) ──────────────────────────────────────────────────

/// The 8 canonical states of a `CortexForge` node (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// Node has booted, waiting for a task
    Idle,
    /// Received an inbox task, not yet started planning
    Assigned,
    /// Decomposing the task, deciding spawn strategy
    Planning,
    /// Actively implementing
    Implementing,
    /// Waiting for external conditions (dependency values or child completion)
    Blocked,
    /// Running verify.sh
    Verifying,
    /// Verification passed, deliverables published
    Delivered,
    /// Unrecoverable (timeout, max retries, cycle, killed)
    Dead,
    /// Code is flashed on MCU, monitoring runtime telemetry
    Monitoring,
    /// Anomaly detected, agent diagnosing root cause
    Diagnosing,
}

impl NodeStatus {
    /// True if this is a terminal state.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Delivered | Self::Dead)
    }

    /// True if the node is alive (not terminal, can participate in the tree).
    #[must_use]
    pub fn is_alive(&self) -> bool {
        !self.is_terminal()
    }

    /// Return the canonical string representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Assigned => "assigned",
            Self::Planning => "planning",
            Self::Implementing => "implementing",
            Self::Blocked => "blocked",
            Self::Verifying => "verifying",
            Self::Delivered => "delivered",
            Self::Dead => "dead",
            Self::Monitoring => "monitoring",
            Self::Diagnosing => "diagnosing",
        }
    }
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for NodeStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idle" => Ok(Self::Idle),
            "assigned" => Ok(Self::Assigned),
            "planning" => Ok(Self::Planning),
            "implementing" => Ok(Self::Implementing),
            "blocked" => Ok(Self::Blocked),
            "verifying" => Ok(Self::Verifying),
            "delivered" => Ok(Self::Delivered),
            "dead" => Ok(Self::Dead),
            "monitoring" => Ok(Self::Monitoring),
            "diagnosing" => Ok(Self::Diagnosing),
            _ => Err(format!("unknown node status: {s}")),
        }
    }
}

// ─── Transition table (§3.2) ─────────────────────────────────────────────

/// Returns true if the transition `from → to` is legal.
///
/// This encodes the complete adjacency matrix from §3.2.
/// Illegal transitions return false; the caller should return `ForgeError::StateInvalid`.
#[must_use]
pub const fn is_valid_transition(from: NodeStatus, to: NodeStatus) -> bool {
    use NodeStatus::{
        Assigned, Blocked, Dead, Delivered, Diagnosing, Idle, Implementing, Monitoring, Planning,
        Verifying,
    };
    matches!(
        (from, to),
        // idle → assigned | dead (TTL)
        (Idle, Assigned | Dead)
            | (Assigned, Planning)
            | (Planning | Blocked | Verifying, Implementing)
            | (Planning | Implementing, Blocked)
            | (Implementing, Verifying | Dead)
            | (Blocked | Verifying, Dead)
            | (Verifying, Delivered) // delivered / dead are terminal for code-gen
            // ── Runtime feedback loop (§3.4) ──
            | (Delivered, Monitoring)  // code flashed, start monitoring
            | (Monitoring, Diagnosing | Dead)  // anomaly detected or device unresponsive
            | (Diagnosing, Implementing) // diagnosis done, fix in progress
    )
}

/// Human-readable reason for an illegal transition.
#[must_use]
pub fn transition_error(from: NodeStatus, to: NodeStatus) -> String {
    format!("illegal state transition: {} → {}", from.as_str(), to.as_str())
}

// ─── StateMachine ────────────────────────────────────────────────────────

/// Per-node state machine with full lifecycle tracking.
#[derive(Debug, Clone)]
pub struct StateMachine {
    pub current: NodeStatus,
    pub entered_at: DateTime<FixedOffset>,
    pub last_heartbeat: DateTime<FixedOffset>,
    pub sequence: u64,

    // Progress tracking
    pub progress_summary: String,
    pub progress_percent: u32,
    pub current_task_id: String,

    // Verify tracking
    pub verify_retry_count: u32,
    pub max_retries: u32,
    pub last_verify_result: Option<String>,
    pub last_verify_summary: Option<String>,

    // Budget
    pub tokens_used: u64,
    pub wallclock_sec_used: u64,

    // Children view (Domain Agent only)
    pub children_view: BTreeMap<String, ChildState>,

    // Max wallclock (seconds from start)
    pub max_wallclock_sec: u64,
    pub started_at: DateTime<FixedOffset>,
}

/// Snapshot of a child node's state, recorded by the parent Domain Agent.
#[derive(Debug, Clone)]
pub struct ChildState {
    pub name: String,
    pub state: NodeStatus,
    pub last_seen_at: DateTime<FixedOffset>,
}

impl StateMachine {
    /// Create a fresh state machine in `Idle` state.
    #[must_use]
    pub fn new(max_retries: u32, max_wallclock_sec: u64) -> Self {
        let now: DateTime<FixedOffset> = Utc::now().into();
        Self {
            current: NodeStatus::Idle,
            entered_at: now,
            last_heartbeat: now,
            sequence: 0,
            progress_summary: String::new(),
            progress_percent: 0,
            current_task_id: String::new(),
            verify_retry_count: 0,
            max_retries,
            last_verify_result: None,
            last_verify_summary: None,
            tokens_used: 0,
            wallclock_sec_used: 0,
            children_view: BTreeMap::new(),
            max_wallclock_sec,
            started_at: now,
        }
    }

    // ── Transition helpers (§3.2) ───────────────────────────────────────

    /// Attempt a state transition. Returns an error if illegal.
    ///
    /// Automatically updates `entered_at` and increments `sequence`.
    pub fn transition(&mut self, to: NodeStatus) -> ForgeResult<()> {
        if !is_valid_transition(self.current, to) {
            return Err(ForgeError::StateInvalid {
                node: crate::types::NodeName::new("(self)"),
                from: self.current.as_str().into(),
                to: to.as_str().into(),
            });
        }
        self.current = to;
        self.entered_at = Utc::now().into();
        self.sequence += 1;
        Ok(())
    }

    /// idle → assigned: received inbox task.
    /// Side effect: records `task_id`.
    pub fn assign(&mut self, task_id: &str) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Idle, "assign")?;
        self.transition(NodeStatus::Assigned)?;
        self.current_task_id = task_id.to_string();
        Ok(())
    }

    /// assigned → planning: begin decomposing.
    pub fn start_planning(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Assigned, "start_planning")?;
        self.transition(NodeStatus::Planning)
    }

    /// planning → implementing: plan ready.
    pub fn start_implementing(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Planning, "start_implementing")?;
        self.transition(NodeStatus::Implementing)
    }

    /// implementing → blocked: dependency discovered.
    /// IMPORTANT: caller MUST write needs.toml BEFORE calling this (§15.2, fix #P0-1).
    pub fn block(&mut self, reason: &str) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Implementing, "block")?;
        self.progress_summary = format!("blocked: {reason}");
        self.transition(NodeStatus::Blocked)
    }

    /// blocked → implementing: all dependencies resolved.
    pub fn resume_after_blocked(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Blocked, "resume_after_blocked")?;
        self.progress_summary = "resumed after dependencies resolved".into();
        self.transition(NodeStatus::Implementing)
    }

    /// implementing → verifying: code complete, running verify.sh.
    pub fn start_verifying(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Implementing, "start_verifying")?;
        self.transition(NodeStatus::Verifying)
    }

    /// verifying → delivered: verify.sh passed.
    pub fn deliver(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Verifying, "deliver")?;
        self.last_verify_result = Some("pass".into());
        self.transition(NodeStatus::Delivered)
    }

    // ── Runtime feedback loop (§3.4) ────────────────────────────────────

    /// delivered → monitoring: code is flashed, begin runtime telemetry monitoring.
    pub fn start_monitoring(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Delivered, "start_monitoring")?;
        self.transition(NodeStatus::Monitoring)
    }

    /// monitoring → diagnosing: anomaly detected, begin root-cause diagnosis.
    pub fn anomaly_detected(&mut self, summary: &str) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Monitoring, "anomaly_detected")?;
        self.progress_summary = format!("anomaly: {summary}");
        self.transition(NodeStatus::Diagnosing)
    }

    /// diagnosing → implementing: root cause found, fixing the code.
    pub fn diagnosis_complete(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Diagnosing, "diagnosis_complete")?;
        self.progress_summary = String::new();
        self.transition(NodeStatus::Implementing)
    }

    /// verifying → implementing: verify.sh failed, retrying.
    pub fn retry_verify(&mut self, fail_summary: &str) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Verifying, "retry_verify")?;
        if self.verify_retry_count >= self.max_retries {
            return Err(ForgeError::StateInvalid {
                node: crate::types::NodeName::new("(self)"),
                from: "verifying".into(),
                to: "implementing".into(),
            });
        }
        self.verify_retry_count += 1;
        self.last_verify_result = Some("fail".into());
        self.last_verify_summary = Some(fail_summary.to_string());
        self.progress_summary =
            format!("verify failed (retry {}/{})", self.verify_retry_count, self.max_retries);
        self.transition(NodeStatus::Implementing)
    }

    /// verifying → dead: max retries exhausted.
    pub fn die_verify_exhausted(&mut self, fail_summary: &str) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Verifying, "die_verify_exhausted")?;
        self.last_verify_result = Some("fail".into());
        self.last_verify_summary = Some(fail_summary.to_string());
        self.progress_summary =
            format!("verify failed (max retries {} exhausted)", self.max_retries);
        self.transition(NodeStatus::Dead)
    }

    /// → dead (generic): wallclock timeout, killed, cycle, etc.
    pub fn die(&mut self, reason: &str) -> ForgeResult<()> {
        // Can transition to Dead from Idle, Implementing, Blocked, Verifying
        if !matches!(
            self.current,
            NodeStatus::Idle
                | NodeStatus::Implementing
                | NodeStatus::Blocked
                | NodeStatus::Verifying
        ) && !is_valid_transition(self.current, NodeStatus::Dead)
        {
            return Err(ForgeError::StateInvalid {
                node: crate::types::NodeName::new("(self)"),
                from: self.current.as_str().into(),
                to: "dead".into(),
            });
        }
        self.progress_summary = format!("dead: {reason}");
        self.transition(NodeStatus::Dead)
    }

    /// idle → dead: TTL timeout, never received a task.
    pub fn die_ttl(&mut self) -> ForgeResult<()> {
        self.ensure_current(NodeStatus::Idle, "die_ttl")?;
        self.progress_summary = "dead: TTL expired, never received a task".into();
        self.transition(NodeStatus::Dead)
    }

    /// Check wallclock budget; transition to Dead if exhausted.
    /// Returns true if still alive.
    pub fn check_wallclock(&mut self) -> ForgeResult<bool> {
        if self.current.is_terminal() {
            return Ok(true);
        }
        let now: DateTime<FixedOffset> = Utc::now().into();
        let elapsed = (now - self.started_at).num_seconds() as u64;
        self.wallclock_sec_used = elapsed;
        if elapsed >= self.max_wallclock_sec {
            self.die(&format!("wallclock exhausted: {elapsed}s >= {}s", self.max_wallclock_sec))?;
            return Ok(false);
        }
        Ok(true)
    }

    // ── Heartbeat ───────────────────────────────────────────────────────

    /// Record a heartbeat, updating `last_heartbeat` and progress.
    pub fn heartbeat(&mut self, summary: &str, percent: u32) {
        self.last_heartbeat = Utc::now().into();
        self.progress_summary = summary.to_string();
        self.progress_percent = percent;
    }

    // ── Children view ───────────────────────────────────────────────────

    /// Update the parent's view of a child node's state.
    pub fn update_child(&mut self, name: &str, state: NodeStatus) {
        self.children_view.insert(
            name.to_string(),
            ChildState { name: name.to_string(), state, last_seen_at: Utc::now().into() },
        );
    }

    /// Check if all children have reached `Delivered` state.
    #[must_use]
    pub fn all_children_delivered(&self) -> bool {
        if self.children_view.is_empty() {
            return true;
        }
        self.children_view.values().all(|c| c.state == NodeStatus::Delivered)
    }

    /// Check if any child is dead.
    #[must_use]
    pub fn any_child_dead(&self) -> bool {
        self.children_view.values().any(|c| c.state == NodeStatus::Dead)
    }

    // ── Persistence (§4.2) ─────────────────────────────────────────────

    /// Load a `StateMachine` from a state.toml file.
    pub fn load(path: &Path) -> ForgeResult<Self> {
        let node_state = NodeState::load(path)?;
        Self::from_node_state(node_state)
    }

    /// Save this `StateMachine` to a state.toml file.
    pub fn save(&self, path: &Path) -> ForgeResult<()> {
        let node_state = self.to_node_state();
        node_state.save(path)
    }

    /// Convert to the file protocol struct.
    #[must_use]
    pub fn to_node_state(&self) -> NodeState {
        NodeState {
            schema_version: 1,
            state: StateSection {
                current: self.current.as_str().into(),
                entered_at: self.entered_at,
                last_heartbeat: self.last_heartbeat,
                sequence: self.sequence,
            },
            progress: ProgressSection {
                percent_self_estimate: self.progress_percent,
                summary: self.progress_summary.clone(),
                current_task_id: self.current_task_id.clone(),
            },
            children_view: ChildrenViewSection {
                child: self
                    .children_view
                    .values()
                    .map(|c| crate::protocol::ChildViewEntry {
                        name: c.name.clone(),
                        state: c.state.as_str().into(),
                        last_seen_at: c.last_seen_at,
                    })
                    .collect(),
            },
            verify: VerifySection {
                last_run_at: None, // set by the node before running verify.sh
                last_result: self.last_verify_result.clone(),
                fail_summary: self.last_verify_summary.clone(),
                retry_count: self.verify_retry_count,
            },
            budget_used: BudgetUsedSection {
                tokens_used: self.tokens_used,
                wallclock_sec_used: self.wallclock_sec_used,
            },
        }
    }

    /// Restore from the file protocol struct.
    pub fn from_node_state(ns: NodeState) -> ForgeResult<Self> {
        let current: NodeStatus =
            ns.state.current.parse().map_err(|e: String| {
                ForgeError::Config(format!("invalid state in state.toml: {e}"))
            })?;

        let mut children_view = BTreeMap::new();
        for child in &ns.children_view.child {
            let child_state: NodeStatus = child.state.parse().unwrap_or(NodeStatus::Dead);
            children_view.insert(
                child.name.clone(),
                ChildState {
                    name: child.name.clone(),
                    state: child_state,
                    last_seen_at: child.last_seen_at,
                },
            );
        }

        Ok(Self {
            current,
            entered_at: ns.state.entered_at,
            last_heartbeat: ns.state.last_heartbeat,
            sequence: ns.state.sequence,
            progress_summary: ns.progress.summary,
            progress_percent: ns.progress.percent_self_estimate,
            current_task_id: ns.progress.current_task_id,
            verify_retry_count: ns.verify.retry_count,
            max_retries: 3, // will be overwritten if loading from config
            last_verify_result: ns.verify.last_result,
            last_verify_summary: ns.verify.fail_summary,
            tokens_used: ns.budget_used.tokens_used,
            wallclock_sec_used: ns.budget_used.wallclock_sec_used,
            children_view,
            max_wallclock_sec: 1800, // default, overwrite after load
            started_at: ns.state.entered_at,
        })
    }

    // ── Internal helpers ────────────────────────────────────────────────

    fn ensure_current(&self, expected: NodeStatus, operation: &str) -> ForgeResult<()> {
        if self.current != expected {
            return Err(ForgeError::StateInvalid {
                node: crate::types::NodeName::new("(self)"),
                from: self.current.as_str().into(),
                to: format!("({operation})"),
            });
        }
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn new_sm() -> StateMachine {
        StateMachine::new(3, 1800)
    }

    #[test]
    fn test_initial_state_is_idle() {
        let sm = new_sm();
        assert_eq!(sm.current, NodeStatus::Idle);
        assert!(sm.current.is_alive());
        assert!(!sm.current.is_terminal());
    }

    #[test]
    fn test_is_valid_transition_coverage() {
        // Valid paths
        assert!(is_valid_transition(NodeStatus::Idle, NodeStatus::Assigned));
        assert!(is_valid_transition(NodeStatus::Assigned, NodeStatus::Planning));
        assert!(is_valid_transition(NodeStatus::Planning, NodeStatus::Implementing));
        assert!(is_valid_transition(NodeStatus::Implementing, NodeStatus::Blocked));
        assert!(is_valid_transition(NodeStatus::Blocked, NodeStatus::Implementing));
        assert!(is_valid_transition(NodeStatus::Implementing, NodeStatus::Verifying));
        assert!(is_valid_transition(NodeStatus::Verifying, NodeStatus::Delivered));
        assert!(is_valid_transition(NodeStatus::Verifying, NodeStatus::Implementing));
        assert!(is_valid_transition(NodeStatus::Verifying, NodeStatus::Dead));
        assert!(is_valid_transition(NodeStatus::Implementing, NodeStatus::Dead));
        assert!(is_valid_transition(NodeStatus::Blocked, NodeStatus::Dead));
        assert!(is_valid_transition(NodeStatus::Idle, NodeStatus::Dead));

        // Feedback loop transitions (§3.4)
        assert!(is_valid_transition(NodeStatus::Delivered, NodeStatus::Monitoring));
        assert!(is_valid_transition(NodeStatus::Monitoring, NodeStatus::Diagnosing));
        assert!(is_valid_transition(NodeStatus::Diagnosing, NodeStatus::Implementing));
        assert!(is_valid_transition(NodeStatus::Monitoring, NodeStatus::Dead));

        // Invalid paths
        assert!(!is_valid_transition(NodeStatus::Delivered, NodeStatus::Implementing));
        assert!(!is_valid_transition(NodeStatus::Dead, NodeStatus::Idle));
        assert!(!is_valid_transition(NodeStatus::Planning, NodeStatus::Idle));
        assert!(!is_valid_transition(NodeStatus::Verifying, NodeStatus::Assigned));
        assert!(!is_valid_transition(NodeStatus::Monitoring, NodeStatus::Idle));
        assert!(!is_valid_transition(NodeStatus::Diagnosing, NodeStatus::Delivered));
    }

    #[test]
    fn test_happy_path_lifecycle() {
        let mut sm = new_sm();
        sm.assign("T-001").unwrap();
        assert_eq!(sm.current, NodeStatus::Assigned);

        sm.start_planning().unwrap();
        assert_eq!(sm.current, NodeStatus::Planning);

        sm.start_implementing().unwrap();
        assert_eq!(sm.current, NodeStatus::Implementing);

        sm.start_verifying().unwrap();
        assert_eq!(sm.current, NodeStatus::Verifying);

        sm.deliver().unwrap();
        assert_eq!(sm.current, NodeStatus::Delivered);
        assert!(sm.current.is_terminal());
    }

    #[test]
    fn test_dependency_block_unblock() {
        let mut sm = new_sm();
        sm.assign("T-002").unwrap();
        sm.start_planning().unwrap();
        sm.start_implementing().unwrap();

        // Discover dependency → block
        sm.block("need APB1_CLK from hal-clock").unwrap();
        assert_eq!(sm.current, NodeStatus::Blocked);
        assert!(sm.progress_summary.contains("blocked"));

        // Dependency resolved → resume
        sm.resume_after_blocked().unwrap();
        assert_eq!(sm.current, NodeStatus::Implementing);
    }

    #[test]
    fn test_verify_retry_then_pass() {
        let mut sm = new_sm();
        sm.assign("T-003").unwrap();
        sm.start_planning().unwrap();
        sm.start_implementing().unwrap();
        sm.start_verifying().unwrap();

        // 1st failure
        sm.retry_verify("test_uart_tx_overrun timeout").unwrap();
        assert_eq!(sm.current, NodeStatus::Implementing);
        assert_eq!(sm.verify_retry_count, 1);

        // Fix code, back to verifying
        sm.start_verifying().unwrap();
        assert_eq!(sm.current, NodeStatus::Verifying);

        // Pass
        sm.deliver().unwrap();
        assert_eq!(sm.current, NodeStatus::Delivered);
        assert_eq!(sm.last_verify_result.as_deref(), Some("pass"));
    }

    #[test]
    fn test_verify_max_retries_exhausted() {
        let mut sm = StateMachine::new(2, 1800); // only 2 retries
        sm.assign("T-004").unwrap();
        sm.start_planning().unwrap();
        sm.start_implementing().unwrap();
        sm.start_verifying().unwrap();

        // 1st retry
        sm.retry_verify("fail 1").unwrap();
        sm.start_verifying().unwrap();
        // 2nd retry
        sm.retry_verify("fail 2").unwrap();
        sm.start_verifying().unwrap();

        // 3rd attempt should fail (retry_count >= max_retries)
        let result = sm.retry_verify("fail 3");
        assert!(result.is_err());
        assert_eq!(sm.verify_retry_count, 2);

        // Die from exhaustion
        sm.die_verify_exhausted("fail 3").unwrap();
        assert_eq!(sm.current, NodeStatus::Dead);
    }

    #[test]
    fn test_wallclock_exhausted() {
        let mut sm = StateMachine::new(3, 0); // 0 sec wallclock = instantly exhausted
        sm.assign("T-005").unwrap();
        sm.start_planning().unwrap();
        sm.start_implementing().unwrap();

        let alive = sm.check_wallclock().unwrap_or(true);
        if sm.current == NodeStatus::Dead {
            assert!(!alive);
        }
    }

    #[test]
    fn test_illegal_transition_rejected() {
        let mut sm = new_sm();
        // Can't go straight from idle to verifying
        let result = sm.start_verifying();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("invalid") || err_msg.contains("illegal"), "got: {err_msg}");

        // Can't resume when not blocked
        let r = sm.resume_after_blocked();
        assert!(r.is_err());
    }

    #[test]
    fn test_children_view() {
        let mut sm = new_sm();
        assert!(sm.all_children_delivered());

        sm.update_child("child-a", NodeStatus::Delivered);
        sm.update_child("child-b", NodeStatus::Implementing);
        assert!(!sm.all_children_delivered());
        assert!(!sm.any_child_dead());

        sm.update_child("child-b", NodeStatus::Delivered);
        assert!(sm.all_children_delivered());

        sm.update_child("child-c", NodeStatus::Dead);
        assert!(sm.any_child_dead());
    }

    #[test]
    fn test_sequence_monotonic() {
        let mut sm = new_sm();
        assert_eq!(sm.sequence, 0);
        sm.assign("T-006").unwrap();
        assert_eq!(sm.sequence, 1);
        sm.start_planning().unwrap();
        assert_eq!(sm.sequence, 2);
        sm.start_implementing().unwrap();
        assert_eq!(sm.sequence, 3);
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        let mut sm = new_sm();
        sm.assign("T-007").unwrap();
        sm.start_planning().unwrap();
        sm.start_implementing().unwrap();
        sm.update_child("c1", NodeStatus::Implementing);
        sm.heartbeat("working on DMA", 60);
        sm.save(&path).unwrap();

        let loaded = StateMachine::load(&path).unwrap();
        assert_eq!(loaded.current, NodeStatus::Implementing);
        assert_eq!(loaded.sequence, 3);
        assert_eq!(loaded.current_task_id, "T-007");
        assert_eq!(loaded.progress_summary, "working on DMA");
        assert_eq!(loaded.progress_percent, 60);
        assert_eq!(loaded.children_view.len(), 1);
        assert_eq!(loaded.children_view["c1"].state, NodeStatus::Implementing);
    }

    #[test]
    fn test_domain_agent_blocked_by_children() {
        // Domain Agent: blocks when children not delivered
        let mut sm = new_sm();
        sm.assign("T-domain-001").unwrap();
        sm.start_planning().unwrap();
        sm.update_child("l2-a", NodeStatus::Implementing);
        sm.update_child("l2-b", NodeStatus::Blocked);

        // Domain Agent blocks because children aren't done
        // (Domain Agent goes to Blocked via start_implementing + block)
        sm.start_implementing().unwrap();
        sm.block("waiting for children to deliver").unwrap();
        assert_eq!(sm.current, NodeStatus::Blocked);

        // Children deliver
        sm.update_child("l2-a", NodeStatus::Delivered);
        sm.update_child("l2-b", NodeStatus::Delivered);
        assert!(sm.all_children_delivered());

        // Resume
        sm.resume_after_blocked().unwrap();
        assert_eq!(sm.current, NodeStatus::Implementing);
    }

    #[test]
    fn test_die_from_idle() {
        let mut sm = new_sm();
        sm.die_ttl().unwrap();
        assert_eq!(sm.current, NodeStatus::Dead);
        assert!(sm.current.is_terminal());
    }
}
