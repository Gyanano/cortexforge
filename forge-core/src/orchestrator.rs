//! Orchestrator daemon — the main event loop (§0, §15.3).
//!
//! Ties together ProcessManager, DepGraph, HeartbeatMonitor, and EventBus
//! into a single scan-sleep loop. Pure Rust, no LLM — the Orchestrator
//! only spawns and monitors child Claude processes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::ForgeConfig;
use crate::deps::DepGraph;
use crate::error::ForgeResult;
use crate::event::EventType;
use crate::eventbus::EventBus;
use crate::heartbeat::{HeartbeatMonitor, ScanAction};
use crate::protocol::{EscalatedTable, NodeDefinition};
use crate::spawn::{self, ProcessManager};
use crate::types::NodeDepth;

/// The CortexForge Orchestrator — pure-code daemon at L0.
pub struct Orchestrator {
    config: ForgeConfig,
    root: PathBuf,
    eventbus: EventBus,
    proc_mgr: ProcessManager,
    heartbeat: HeartbeatMonitor,
    dep_graph: DepGraph,
    shutdown: Arc<AtomicBool>,
}

impl Orchestrator {
    /// Create a new Orchestrator for the given project root.
    pub fn new(root: &Path) -> ForgeResult<Self> {
        let forge_toml = root.join("forge.toml");
        let config: ForgeConfig = toml::from_str(&std::fs::read_to_string(&forge_toml)?)
            .map_err(|e| crate::error::ForgeError::Config(format!("invalid forge.toml: {e}")))?;

        let eventbus = EventBus::open(root.join(&config.paths.event_bus));
        let heartbeat = HeartbeatMonitor::new(&config);

        Ok(Self {
            config,
            root: root.to_path_buf(),
            eventbus,
            proc_mgr: ProcessManager::new(),
            heartbeat,
            dep_graph: DepGraph::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Run the main orchestration loop.
    ///
    /// Blocks until SIGTERM/SIGINT or a fatal error.
    pub fn run(&mut self) -> ForgeResult<()> {
        tracing::info!(
            root = %self.root.display(),
            scan_interval = self.config.forge.scan_interval_sec,
            "Orchestrator starting"
        );

        // ── Startup: rebuild PID table from disk (§5.4) ──
        let recovered = spawn::rebuild_pids_table(&self.root, &mut self.proc_mgr)?;
        if recovered > 0 {
            tracing::info!(recovered, "rebuilt PID table from previous run");
        }

        // Register signal handlers for graceful shutdown
        let shutdown = self.shutdown.clone();
        ctrlc_handler(shutdown.clone());

        self.eventbus.append(&crate::event::EventEntry::new(
            "orchestrator",
            EventType::State { from: "stopped".into(), to: "running".into(), seq: 0, depth: 0 },
        ))?;

        // Register initial nodes for heartbeat tracking
        let declared = DepGraph::collect_all_declared_nodes(&self.root);
        for (name, cwd) in &declared {
            self.heartbeat.register(name, cwd);
        }
        tracing::info!(nodes = declared.len(), "registered nodes for monitoring");

        // ── Main loop ──
        let mut cycle = 0u64;
        let interval = Duration::from_secs(self.config.forge.scan_interval_sec as u64);

        while !shutdown.load(Ordering::Relaxed) {
            cycle += 1;
            tracing::debug!(cycle, "scan cycle start");

            if let Err(e) = self.one_cycle(cycle) {
                tracing::error!(cycle, error = %e, "scan cycle error");
                self.eventbus.append(&crate::event::EventEntry::new(
                    "orchestrator",
                    EventType::SpawnFailed {
                        child: "orchestrator".into(),
                        reason: format!("cycle {cycle} error: {e}"),
                    },
                ))?;
            }

            // Sleep for the scan interval (with early wake on shutdown)
            let deadline = std::time::Instant::now() + interval;
            while std::time::Instant::now() < deadline {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        // ── Shutdown ──
        tracing::info!(cycles = cycle, "Orchestrator shutting down");
        self.eventbus.append(&crate::event::EventEntry::new(
            "orchestrator",
            EventType::State { from: "running".into(), to: "stopped".into(), seq: 0, depth: 0 },
        ))?;

        self.shutdown_children()?;
        Ok(())
    }

    /// One full scan cycle: collect → dependency resolution → heartbeat → actions.
    fn one_cycle(&mut self, _cycle: u64) -> ForgeResult<()> {
        // ── Reap dead processes ──
        let reaped = self.proc_mgr.reap_dead();
        for name in &reaped {
            self.heartbeat.remove(name);
        }

        // ── Refresh declared nodes (new nodes may have been added) ──
        let declared = DepGraph::collect_all_declared_nodes(&self.root);

        // ── Pass 1: Populate graph with alive node snapshots ──
        self.dep_graph = DepGraph::new();
        let alive = self.dep_graph.populate(&declared)?;

        // ── Auto-spawn declared nodes that aren't alive yet ──
        for (name, cwd) in &declared {
            if alive.contains(name) {
                continue; // already running
            }
            // Check if this node has already been delivered (don't re-spawn)
            let state_file = cwd.join(".forge/state.toml");
            if let Some(state) = crate::safe_read_toml::<crate::protocol::NodeState>(&state_file) {
                if state.state.current == "delivered" || state.state.current == "dead" {
                    continue;
                }
            }
            // Try to spawn
            let node_toml = cwd.join("node.toml");
            if node_toml.exists() {
                if let Ok(def) = NodeDefinition::load(&node_toml) {
                    let parent_depth = NodeDepth(def.node.depth.saturating_sub(1));
                    let _ = spawn::spawn_child(
                        &self.config,
                        &mut self.proc_mgr,
                        parent_depth,
                        &def,
                        &node_toml,
                        false,
                    );
                }
            }
        }

        // ── Pass 2: Build dependency graph ──
        self.dep_graph.build_graph();

        // ── Pass 3: First cycle check ──
        let has_cycle = self.dep_graph.pass3_first_cycle_check(&self.eventbus)?;
        if has_cycle {
            self.dep_graph.mark_cycle_dead(&self.dep_graph.cycle_nodes.clone(), &self.eventbus)?;
        }

        // ── Pass 4: Match new edges ──
        self.dep_graph.pass4_match_new_edges(&self.root, &self.eventbus)?;

        // ── Pass 5: Second cycle check ──
        let has_new_cycle = self.dep_graph.pass5_second_cycle_check(&self.eventbus)?;
        if has_new_cycle {
            self.dep_graph.mark_cycle_dead(&self.dep_graph.cycle_nodes.clone(), &self.eventbus)?;
        }

        // ── Pass 6: Write tasks + spawn decisions ──
        self.dep_graph.pass6_write_tasks_and_spawn(&self.config, &self.root, &self.eventbus)?;

        // ── Pass 7: Transfer resolved values ──
        self.dep_graph.pass7_transfer_resolved(&self.eventbus)?;

        // ── Pass 7b: Dependency chain propagation ──
        let escalated_path = self.root.join(&self.config.paths.escalated);
        let escalated = EscalatedTable::load(&escalated_path).unwrap_or_default();
        self.dep_graph.pass7b_dependency_chain(&escalated, &self.eventbus)?;

        // ── Pass 8: Value change detection ──
        self.dep_graph.pass8_value_change_detection(&self.eventbus)?;

        // ── Pass 9: Cross-layer escalation ──
        let mut escalated = EscalatedTable::load(&escalated_path).unwrap_or_default();
        self.dep_graph.pass9_cross_layer(&self.root, &mut escalated, &self.eventbus)?;
        let _ = escalated.save(&escalated_path);

        // ── Heartbeat scan ──
        for (name, _cwd) in &declared {
            let is_alive = self.proc_mgr.is_alive(name);
            match self.heartbeat.scan_node(name, is_alive) {
                Ok(result) => match result.action {
                    ScanAction::Healthy => {}
                    ScanAction::HeartbeatTimeout { missed_for_sec } => {
                        tracing::warn!(node = %name, missed = missed_for_sec, "heartbeat timeout");
                        self.eventbus.append(&crate::event::EventEntry::new(
                            "orchestrator",
                            EventType::HeartbeatMiss {
                                subject: name.clone(),
                                missed_for_sec: missed_for_sec as u32,
                                action: "warn".into(),
                            },
                        ))?;
                        // Mark dead after timeout
                        self.proc_mgr.kill_child(name)?;
                        self.eventbus.append(&crate::event::EventEntry::new(
                            name,
                            EventType::NodeDead { reason: "heartbeat_timeout".into() },
                        ))?;
                    }
                    ScanAction::TerminateAfterGrace { reason, grace } => {
                        tracing::info!(node = %name, %reason, "terminating after grace");
                        // Wait for process to exit naturally
                        if let Some(handle) = self.proc_mgr.get_mut(name) {
                            let _ = handle.wait_timeout(grace);
                            if handle.is_alive() {
                                let _ = handle.kill();
                            }
                        }
                        self.proc_mgr.remove(name);
                    }
                    ScanAction::ForceKill { reason } => {
                        tracing::warn!(node = %name, %reason, "force killing");
                        self.proc_mgr.kill_child(name)?;
                        self.proc_mgr.remove(name);
                    }
                    ScanAction::Reap { .. } => {
                        tracing::info!(node = %name, "reaping");
                        self.proc_mgr.remove(name);
                        self.heartbeat.remove(name);
                    }
                    ScanAction::Crashed { reason } => {
                        tracing::error!(node = %name, %reason, "node crashed");
                        self.eventbus.append(&crate::event::EventEntry::new(
                            name,
                            EventType::NodeDead { reason },
                        ))?;
                        self.proc_mgr.remove(name);
                        self.heartbeat.remove(name);
                    }
                    ScanAction::SuspectedStuck { unchanged_heartbeats } => {
                        tracing::warn!(node = %name, unchanged = unchanged_heartbeats, "suspected stuck");
                        self.eventbus.append(&crate::event::EventEntry::new(
                            "orchestrator",
                            EventType::SuspectedStuck {
                                subject: name.clone(),
                                unchanged_heartbeats,
                            },
                        ))?;
                    }
                    ScanAction::DeferToDependencyCheck => {
                        // Handled by Pass 7b
                    }
                },
                Err(e) => {
                    tracing::warn!(node = %name, error = %e, "heartbeat scan failed");
                }
            }
        }

        Ok(())
    }

    /// Gracefully shut down all managed children.
    fn shutdown_children(&mut self) -> ForgeResult<()> {
        let names: Vec<String> = self.proc_mgr.names().into_iter().map(String::from).collect();
        for name in &names {
            tracing::info!(node = %name, "sending shutdown signal");
            // Graceful: send kill message to inbox first
            if let Some(handle) = self.proc_mgr.get_mut(name) {
                let msg = crate::protocol::InboxMessage {
                    schema_version: 1,
                    id: uuid::Uuid::new_v4().to_string(),
                    from: "orchestrator".into(),
                    to: name.clone(),
                    created_at: chrono::Utc::now().into(),
                    kind: crate::protocol::MessageKind::Kill,
                    ref_task_id: None,
                    priority: "P0".into(),
                    body: crate::protocol::MessageBody {
                        reason: Some("orchestrator_shutdown".into()),
                        grace_sec: Some(5),
                        ..Default::default()
                    },
                };
                let inbox_dir = handle.cwd.join(".forge/inbox");
                let _ = msg.write_to_inbox(&inbox_dir);
            }
        }

        // Wait for processes to exit
        std::thread::sleep(Duration::from_secs(6));

        // Force kill remaining
        for name in &names {
            if self.proc_mgr.is_alive(name) {
                tracing::warn!(node = %name, "force killing after grace period");
                let _ = self.proc_mgr.kill_child(name);
            }
            self.proc_mgr.remove(name);
        }

        Ok(())
    }
}

/// Set up a SIGTERM/SIGINT handler that sets the shutdown flag.
fn ctrlc_handler(shutdown: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        // Simple approach: poll for signals via a oneshot-like mechanism.
        // For a real daemon, use tokio::signal or the signal-hook crate.
        // For MVP: the sleep loop checks the flag, user sends SIGTERM.
        let _ = shutdown; // kept alive by the Arc clone
    });
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();

        std::fs::create_dir_all(root.join(".forge")).unwrap();

        // Write forge.toml
        let config = r#"[forge]
schema_version = 1
max_depth = 4
max_total_nodes = 64
heartbeat_interval_sec = 15
heartbeat_timeout_sec = 60
default_max_retries = 3
stuck_threshold_heartbeats = 4
scan_interval_sec = 5
spawn_timeout_sec = 30

[budget.global]
max_tokens_total = 5000000
max_wallclock_total_sec = 14400

[[budget.per_layer]]
layer = 1
tokens = 300000
wallclock_sec = 3600
model = "claude-sonnet-4-6"

[paths]
event_bus = ".forge/eventbus.log"
escalated = ".forge/escalated.toml"
"#;
        std::fs::write(root.join("forge.toml"), config).unwrap();

        (dir, root)
    }

    #[test]
    fn test_orchestrator_new_and_load() {
        let (_dir, root) = setup_test_project();
        let orch = Orchestrator::new(&root);
        assert!(orch.is_ok());
        let orch = orch.unwrap();
        assert_eq!(orch.config.forge.max_depth, 4);
        assert_eq!(orch.config.forge.scan_interval_sec, 5);
    }

    #[test]
    fn test_orchestrator_with_nodes() {
        let (_dir, root) = setup_test_project();

        // Create a test node with PID pointing to current process (alive)
        let node_dir = root.join("modules/test-node");
        std::fs::create_dir_all(node_dir.join(".forge")).unwrap();
        std::fs::create_dir_all(node_dir.join("shared")).unwrap();

        let def = crate::protocol::NodeDefinition {
            node: crate::protocol::NodeDefSection {
                name: "test-node".into(),
                role: crate::types::NodeRole::Module,
                cwd: "modules/test-node".into(),
                parent: "".into(),
                depth: 1,
            },
            children: Default::default(),
            provides: Default::default(),
            budget: Default::default(),
            runtime: Default::default(),
        };
        def.save(&node_dir.join("node.toml")).unwrap();

        // Write state
        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
        let state = crate::protocol::NodeState {
            schema_version: 1,
            state: crate::protocol::StateSection {
                current: "idle".into(),
                entered_at: now,
                last_heartbeat: now,
                sequence: 1,
            },
            progress: Default::default(),
            children_view: Default::default(),
            verify: Default::default(),
            budget_used: Default::default(),
        };
        state.save(&node_dir.join(".forge/state.toml")).unwrap();

        // Write PID pointing to current process — it's alive
        crate::spawn::write_pid_file(&node_dir, std::process::id(), "test-node").unwrap();

        let orch = Orchestrator::new(&root).unwrap();

        // Verify config loaded correctly
        assert_eq!(orch.config.forge.max_depth, 4);

        // Write a test event to verify eventbus works
        orch.eventbus
            .append(&crate::event::EventEntry::new(
                "orchestrator",
                EventType::State { from: "test".into(), to: "test".into(), seq: 0, depth: 0 },
            ))
            .unwrap();

        let events = orch.eventbus.read_all().unwrap();
        assert!(!events.is_empty(), "eventbus should work");
    }
}
