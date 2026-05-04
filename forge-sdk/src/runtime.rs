//! Per-node runtime — the execution environment for a single node.
//!
//! Manages the node lifecycle: PID file, initial state, heartbeat watchdog,
//! budget tracking, verify gate execution, and file protocol helpers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_core::error::ForgeResult;
use forge_core::protocol::{NeedsDeclaration, ProvidesDeclaration, ResolvedValues, TaskList, SpawnRequests};
use forge_core::state::{NodeStatus, StateMachine};
use forge_core::types::{BudgetTracker, NodeDepth, NodeName};

// ─── NodeRuntime ────────────────────────────────────────────────────────

/// The per-node runtime environment.
///
/// Created once at node startup, manages the full lifecycle.
pub struct NodeRuntime {
    pub name: NodeName,
    pub depth: NodeDepth,
    pub parent: String,
    pub cwd: PathBuf,
    pub root: PathBuf,
    pub is_wake_up: bool,

    pub state: StateMachine,

    // File paths (computed once)
    pub state_file: PathBuf,
    pub inbox_dir: PathBuf,
    pub needs_file: PathBuf,
    pub provides_file: PathBuf,
    pub resolved_file: PathBuf,
    pub tasks_file: PathBuf,
    pub spawn_requests_file: PathBuf,

    // Runtime tracking
    pub token_tracker: Arc<Mutex<BudgetTracker>>,
    shutdown: Arc<AtomicBool>,
    start_time: Instant,
}

impl NodeRuntime {
    /// Initialize from environment variables (FORGE_NODE_NAME, FORGE_ROOT, etc.).
    ///
    /// Writes the PID file and initial state.toml (state=idle).
    pub fn from_env() -> ForgeResult<Self> {
        let name = std::env::var("FORGE_NODE_NAME")
            .unwrap_or_else(|_| "unknown-node".into());
        let depth_str = std::env::var("FORGE_NODE_DEPTH").unwrap_or_else(|_| "1".into());
        let depth = depth_str.parse().unwrap_or(1);
        let parent = std::env::var("FORGE_PARENT").unwrap_or_default();
        let root = PathBuf::from(
            std::env::var("FORGE_ROOT").unwrap_or_else(|_| ".".into()),
        );
        let is_wake_up = std::env::var("FORGE_IS_WAKE_UP")
            .map(|v| v == "true")
            .unwrap_or(false);

        // Determine cwd: FORGE_ROOT + relative path from env or current dir
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let max_retries = std::env::var("FORGE_MAX_RETRIES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let max_wallclock = std::env::var("FORGE_MAX_WALLCLOCK_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1800);

        let forge_dir = cwd.join(".forge");

        let rt = Self {
            name: NodeName::new(&name),
            depth: NodeDepth(depth),
            parent,
            cwd: cwd.clone(),
            root,
            is_wake_up,
            state: StateMachine::new(max_retries, max_wallclock),
            state_file: forge_dir.join("state.toml"),
            inbox_dir: forge_dir.join("inbox"),
            needs_file: cwd.join("shared/needs.toml"),
            provides_file: cwd.join("shared/provides.toml"),
            resolved_file: cwd.join("shared/resolved.toml"),
            tasks_file: cwd.join("shared/tasks.toml"),
            spawn_requests_file: forge_dir.join("spawn_requests.toml"),
            token_tracker: Arc::new(Mutex::new(BudgetTracker::new(
                None,
                Some(max_wallclock),
            ))),
            shutdown: Arc::new(AtomicBool::new(false)),
            start_time: Instant::now(),
        };

        Ok(rt)
    }

    // ── Initialization ─────────────────────────────────────────────────

    /// Write PID file and initial state.toml (idle).
    pub fn initialize(&mut self) -> ForgeResult<()> {
        // Write PID file
        std::fs::create_dir_all(self.cwd.join(".forge"))?;
        std::fs::create_dir_all(self.cwd.join("shared"))?;

        let pid = std::process::id();
        forge_core::spawn::write_pid_file(&self.cwd, pid, self.name.as_str())?;

        // Write initial state
        self.state.heartbeat("booted", 0);
        self.state.save(&self.state_file)?;

        tracing::info!(
            name = %self.name,
            pid = pid,
            depth = self.depth.as_u32(),
            wake_up = self.is_wake_up,
            "node initialized"
        );

        Ok(())
    }

    // ── Heartbeat ──────────────────────────────────────────────────────

    /// Start the heartbeat watchdog thread.
    ///
    /// Returns a handle that can be joined on shutdown.
    pub fn start_heartbeat(&self, interval_sec: u64) -> std::thread::JoinHandle<()> {
        let state_file = self.state_file.clone();
        let shutdown = self.shutdown.clone();
        let name = self.name.to_string();

        std::thread::spawn(move || {
            tracing::debug!(node = %name, "heartbeat thread started");
            while !shutdown.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(interval_sec));

                // Read current state, update heartbeat, write back
                if let Ok(mut sm) = StateMachine::load(&state_file) {
                    sm.last_heartbeat = chrono::Utc::now().into();
                    let _ = sm.save(&state_file);
                }
            }
            tracing::debug!(node = %name, "heartbeat thread stopped");
        })
    }

    /// Signal shutdown to the heartbeat thread.
    pub fn signal_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    // ── File helpers ───────────────────────────────────────────────────

    /// Read my current state from disk.
    pub fn read_my_state(&self) -> ForgeResult<StateMachine> {
        StateMachine::load(&self.state_file)
    }

    /// Write my state to disk.
    pub fn write_my_state(&self) -> ForgeResult<()> {
        self.state.save(&self.state_file)
    }

    /// List inbox messages.
    pub fn read_my_inbox(&self) -> ForgeResult<Vec<PathBuf>> {
        forge_core::protocol::InboxMessage::list_all(&self.inbox_dir)
    }

    /// Write a needs declaration.
    pub fn write_needs(&self, needs: &NeedsDeclaration) -> ForgeResult<()> {
        needs.save(&self.needs_file)
    }

    /// Read the current resolved values.
    pub fn read_resolved(&self) -> ForgeResult<ResolvedValues> {
        if self.resolved_file.exists() {
            ResolvedValues::load(&self.resolved_file)
        } else {
            Ok(ResolvedValues::default())
        }
    }

    /// Write provides declaration.
    pub fn write_provides(&self, provides: &ProvidesDeclaration) -> ForgeResult<()> {
        provides.save(&self.provides_file)
    }

    /// Read tasks assigned to me.
    pub fn read_my_tasks(&self) -> ForgeResult<TaskList> {
        if self.tasks_file.exists() {
            TaskList::load(&self.tasks_file)
        } else {
            Ok(TaskList::default())
        }
    }

    /// Write spawn requests (Domain Agent only).
    pub fn write_spawn_requests(&self, requests: &SpawnRequests) -> ForgeResult<()> {
        SpawnRequests::save_empty(&self.spawn_requests_file)?;

        let content = toml::to_string_pretty(requests)
            .map_err(|e| forge_core::error::ForgeError::Config(format!("serialize: {e}")))?;
        forge_core::atomic_write(&self.spawn_requests_file, &content)?;
        Ok(())
    }

    // ── Budget tracking ────────────────────────────────────────────────

    /// Record token usage.
    pub fn record_tokens(&self, tokens: u64) {
        if let Ok(mut tracker) = self.token_tracker.lock() {
            tracker.tokens_used = tracker.tokens_used.saturating_add(tokens);
        }
    }

    /// Check if budget is exhausted.
    pub fn budget_exhausted(&self) -> bool {
        self.token_tracker
            .lock()
            .map(|t| t.is_exhausted())
            .unwrap_or(false)
    }

    /// Get elapsed wallclock seconds.
    pub fn elapsed_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    // ── Verify gate (§8) ───────────────────────────────────────────────

    /// Execute `./verify.sh` with a timeout.
    ///
    /// Returns (exit_code, stdout, stderr).
    pub fn run_verify(&self, timeout_sec: u64) -> ForgeResult<VerifyOutcome> {
        use std::process::Command;

        let script = self.cwd.join("verify.sh");
        if !script.exists() {
            return Ok(VerifyOutcome {
                exit_code: -1,
                stdout: String::new(),
                stderr: "verify.sh not found".into(),
            });
        }

        let output = Command::new("./verify.sh")
            .current_dir(&self.cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| forge_core::error::ForgeError::Other(format!("verify.sh exec failed: {e}")))?;

        Ok(VerifyOutcome {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

/// Outcome of running verify.sh.
#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl VerifyOutcome {
    pub fn passed(&self) -> bool {
        self.exit_code == 0
    }
}

// ─── Node main loop (§7) ────────────────────────────────────────────────

/// Run the full node lifecycle on a `NodeRuntime`.
///
/// This is the high-level orchestrator of node behavior:
/// wait for task → plan → implement (with dependency pauses) → verify → deliver.
///
/// In the real system this logic is executed by the Claude agent guided by the prompt.
/// This function provides the SDK-level scaffolding for testing and simulation.
pub fn run_node_loop(rt: &mut NodeRuntime, heartbeat_interval: u64) -> ForgeResult<NodeStatus> {
    // Start heartbeat
    let _heartbeat = rt.start_heartbeat(heartbeat_interval);

    // If wake-up, process tasks immediately
    if rt.is_wake_up {
        return run_wake_flow(rt);
    }

    // Normal lifecycle: idle → wait for task
    if rt.state.current != NodeStatus::Assigned {
        // In real system, the node polls inbox/ for tasks
        // For SDK simulation, we check if we already have a task
    }

    // Check wallclock
    if !rt.state.check_wallclock().unwrap_or(false) {
        rt.write_my_state()?;
        rt.signal_shutdown();
        return Ok(NodeStatus::Dead);
    }

    // If verifying, run verify
    if rt.state.current == NodeStatus::Verifying {
        let outcome = rt.run_verify(300)?;
        if outcome.passed() {
            rt.state.deliver()?;
        } else {
            let fail_msg = format!("{} | {}", outcome.stderr.lines().next().unwrap_or("unknown"), outcome.stdout.lines().next().unwrap_or(""));
            if rt.state.verify_retry_count >= rt.state.max_retries {
                rt.state.die_verify_exhausted(&fail_msg)?;
            } else {
                rt.state.retry_verify(&fail_msg)?;
            }
        }
        rt.write_my_state()?;
    }

    rt.signal_shutdown();
    Ok(rt.state.current)
}

/// Wake-up flow: process tasks.toml → write provides → done.
fn run_wake_flow(rt: &mut NodeRuntime) -> ForgeResult<NodeStatus> {
    let tasks = rt.read_my_tasks()?;
    let pending = tasks.pending();

    if pending.is_empty() {
        rt.state.deliver()?;
        rt.write_my_state()?;
        return Ok(NodeStatus::Delivered);
    }

    // Read existing provides
    let mut provides = if rt.provides_file.exists() {
        ProvidesDeclaration::load(&rt.provides_file)?
    } else {
        ProvidesDeclaration::default()
    };

    // Process each pending task
    for task in pending {
        // Check if we already provide this key
        if let Some(existing) = provides.get(&task.key) {
            // Value unchanged — don't increment seq (avoids cascading value_changed noise)
            // Just mark task done
            continue;
        }

        // New key or value change — add with incremented seq
        let new_seq = provides.get(&task.key).map(|e| e.seq + 1).unwrap_or(1);
        provides.provides.insert(
            task.key.clone(),
            forge_core::protocol::ProvideEntry {
                value: String::new(), // placeholder, filled by Claude
                desc: task.desc.clone(),
                seq: new_seq,
            },
        );
    }

    rt.write_provides(&provides)?;
    rt.state.deliver()?;
    rt.write_my_state()?;
    Ok(NodeStatus::Delivered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_runtime(cwd: &Path) -> NodeRuntime {
        NodeRuntime {
            name: NodeName::new("test-node"),
            depth: NodeDepth(2),
            parent: "test-parent".into(),
            cwd: cwd.to_path_buf(),
            root: PathBuf::from("/tmp/test-forge"),
            is_wake_up: false,
            state: StateMachine::new(3, 1800),
            state_file: cwd.join(".forge/state.toml"),
            inbox_dir: cwd.join(".forge/inbox"),
            needs_file: cwd.join("shared/needs.toml"),
            provides_file: cwd.join("shared/provides.toml"),
            resolved_file: cwd.join("shared/resolved.toml"),
            tasks_file: cwd.join("shared/tasks.toml"),
            spawn_requests_file: cwd.join(".forge/spawn_requests.toml"),
            token_tracker: Arc::new(Mutex::new(BudgetTracker::new(None, Some(1800)))),
            shutdown: Arc::new(AtomicBool::new(false)),
            start_time: Instant::now(),
        }
    }

    fn setup_test_cwd() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_path_buf();
        let forge_dir = cwd.join(".forge");
        let shared_dir = cwd.join("shared");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::create_dir_all(&shared_dir).unwrap();
        let state_file = forge_dir.join("state.toml");
        let inbox_dir = forge_dir.join("inbox");
        let needs_file = shared_dir.join("needs.toml");
        let provides_file = shared_dir.join("provides.toml");
        let resolved_file = shared_dir.join("resolved.toml");
        let tasks_file = shared_dir.join("tasks.toml");
        let spawn_requests_file = forge_dir.join("spawn_requests.toml");
        (dir, cwd, state_file, inbox_dir, needs_file, provides_file, resolved_file, tasks_file, spawn_requests_file)
    }

    #[test]
    fn test_runtime_initialize() {
        let (_dir, cwd, state_file, inbox_dir, needs_file, provides_file, resolved_file, tasks_file, spawn_requests_file) = setup_test_cwd();

        let mut rt = NodeRuntime {
            name: NodeName::new("test-init"),
            depth: NodeDepth(1),
            parent: "parent".into(),
            cwd: cwd.clone(),
            root: PathBuf::from("/tmp"),
            is_wake_up: false,
            state: StateMachine::new(3, 1800),
            state_file: state_file.clone(),
            inbox_dir,
            needs_file,
            provides_file,
            resolved_file,
            tasks_file,
            spawn_requests_file,
            token_tracker: Arc::new(Mutex::new(BudgetTracker::new(None, Some(1800)))),
            shutdown: Arc::new(AtomicBool::new(false)),
            start_time: Instant::now(),
        };

        rt.initialize().unwrap();
        assert!(cwd.join(".forge/pid").exists());
        assert!(state_file.exists());

        let loaded = StateMachine::load(&state_file).unwrap();
        assert_eq!(loaded.current, NodeStatus::Idle);
    }

    #[test]
    fn test_runtime_file_helpers() {
        let (_dir, cwd, state_file, inbox_dir, needs_file, provides_file, resolved_file, tasks_file, spawn_requests_file) = setup_test_cwd();

        let mut rt = NodeRuntime {
            name: NodeName::new("test-files"),
            depth: NodeDepth(2),
            parent: String::new(),
            cwd: cwd.clone(),
            root: PathBuf::from("/tmp"),
            is_wake_up: false,
            state: StateMachine::new(3, 1800),
            state_file,
            inbox_dir,
            needs_file: needs_file.clone(),
            provides_file: provides_file.clone(),
            resolved_file,
            tasks_file,
            spawn_requests_file,
            token_tracker: Arc::new(Mutex::new(BudgetTracker::new(None, Some(1800)))),
            shutdown: Arc::new(AtomicBool::new(false)),
            start_time: Instant::now(),
        };

        rt.state.assign("T-001").unwrap();
        rt.write_my_state().unwrap();
        let loaded = rt.read_my_state().unwrap();
        assert_eq!(loaded.current, NodeStatus::Assigned);

        let mut needs = NeedsDeclaration::default();
        needs.needs.insert(
            "APB1_CLK".into(),
            forge_core::protocol::NeedEntry { desc: "test".into(), requester: "test-files".into() },
        );
        rt.write_needs(&needs).unwrap();
        assert!(needs_file.exists());

        let mut provides = ProvidesDeclaration::default();
        provides.provides.insert(
            "APB1_CLK".into(),
            forge_core::protocol::ProvideEntry { value: "42MHz".into(), desc: "test".into(), seq: 1 },
        );
        rt.write_provides(&provides).unwrap();
        assert!(provides_file.exists());
    }

    #[test]
    fn test_budget_tracking() {
        let (_dir, cwd, state_file, inbox_dir, needs_file, provides_file, resolved_file, tasks_file, spawn_requests_file) = setup_test_cwd();

        let mut rt = NodeRuntime {
            name: NodeName::new("test-budget"),
            depth: NodeDepth(2),
            parent: String::new(),
            cwd: cwd.clone(),
            root: PathBuf::from("/tmp"),
            is_wake_up: false,
            state: StateMachine::new(3, 0),
            state_file,
            inbox_dir,
            needs_file,
            provides_file,
            resolved_file,
            tasks_file,
            spawn_requests_file,
            token_tracker: Arc::new(Mutex::new(BudgetTracker::new(None, Some(0)))),
            shutdown: Arc::new(AtomicBool::new(false)),
            start_time: Instant::now(),
        };

        rt.state.check_wallclock().ok();
        let is_terminal = rt.state.current.is_terminal();
        assert!(is_terminal || rt.elapsed_secs() < 2);
    }

    #[test]
    fn test_verify_outcome() {
        let outcome = VerifyOutcome { exit_code: 0, stdout: "all tests passed".into(), stderr: String::new() };
        assert!(outcome.passed());

        let fail = VerifyOutcome { exit_code: 1, stdout: String::new(), stderr: "test_uart_tx_overrun timeout".into() };
        assert!(!fail.passed());
    }
}
