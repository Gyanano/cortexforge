//! Spawn protocol — child process lifecycle management (§5, §6.2, §15.3).
//!
//! Implements the recursive `spawn_child()` function used uniformly at every tree level.
//! Orchestrator is the sole process with spawn authority.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::ForgeConfig;
use crate::error::{ForgeError, ForgeResult};
use crate::protocol::NodeDefinition;
use crate::types::{NodeDepth, NodeName};

// ─── Process handle ──────────────────────────────────────────────────────

/// A handle to a spawned child process.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ChildHandle {
    pub name: NodeName,
    pub pid: u32,
    pub cwd: PathBuf,
    pub depth: NodeDepth,
    pub is_wake_up: bool,
    spawned_at: Instant,
    child: Option<Child>,
}

impl ChildHandle {
    /// Check if the process is still alive.
    ///
    /// Uses `try_wait()` on the child handle; falls back to OS-level probe
    /// via kill(pid, 0) for crash-recovery scenarios where the handle is lost.
    pub fn is_alive(&mut self) -> bool {
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(None) => return true,  // still running
                Ok(Some(_)) => return false, // exited
                Err(_) => return false,
            }
        }
        // Fallback: probe via OS
        os_probe_pid(self.pid)
    }

    /// Send SIGKILL to the process.
    pub fn kill(&mut self) -> ForgeResult<()> {
        if let Some(ref mut child) = self.child {
            child.kill()?;
        } else {
            os_kill_pid(self.pid)?;
        }
        Ok(())
    }

    /// Wait for the process to exit with a timeout.
    pub fn wait_timeout(&mut self, timeout: Duration) -> ForgeResult<bool> {
        if let Some(ref mut child) = self.child {
            let start = Instant::now();
            while start.elapsed() < timeout {
                match child.try_wait()? {
                    Some(status) => {
                        tracing::info!(
                            pid = self.pid,
                            name = %self.name,
                            exit = ?status,
                            "child process exited"
                        );
                        return Ok(true);
                    }
                    None => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            Ok(false) // timeout
        } else {
            Ok(!os_probe_pid(self.pid))
        }
    }

    /// Detach the child handle (for daemon/background mode).
    pub fn detach(&mut self) {
        self.child.take();
    }
}

// ─── Process manager ────────────────────────────────────────────────────

/// Tracks all running child processes in memory.
///
/// Lost on Orchestrator restart; rebuilt from `.forge/pid` files (§5.4).
#[derive(Debug, Default)]
pub struct ProcessManager {
    children: HashMap<String, ChildHandle>,
}

impl ProcessManager {
    #[must_use] 
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a newly spawned child.
    pub fn register(&mut self, handle: ChildHandle) {
        tracing::info!(
            name = %handle.name,
            pid = handle.pid,
            depth = handle.depth.as_u32(),
            "registered child process"
        );
        self.children.insert(handle.name.to_string(), handle);
    }

    /// Remove a child from tracking (after confirmed exit).
    pub fn remove(&mut self, name: &str) -> Option<ChildHandle> {
        let removed = self.children.remove(name);
        if removed.is_some() {
            tracing::info!(name = %name, "removed child process from tracking");
        }
        removed
    }

    /// Get a mutable reference to a child.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut ChildHandle> {
        self.children.get_mut(name)
    }

    /// Check if a named child is alive.
    pub fn is_alive(&mut self, name: &str) -> bool {
        self.children
            .get_mut(name)
            .is_some_and(ChildHandle::is_alive)
    }

    /// Kill a child by name.
    pub fn kill_child(&mut self, name: &str) -> ForgeResult<()> {
        if let Some(handle) = self.children.get_mut(name) {
            handle.kill()?;
        }
        Ok(())
    }

    /// Count of tracked children.
    #[must_use] 
    pub fn active_count(&self) -> usize {
        self.children.len()
    }

    /// Get all registered child names.
    #[must_use] 
    pub fn names(&self) -> Vec<&str> {
        self.children.keys().map(std::string::String::as_str).collect()
    }

    /// Check alive status for all children, removing dead ones.
    /// Returns names of children that were found dead.
    pub fn reap_dead(&mut self) -> Vec<String> {
        let mut dead = Vec::new();
        self.children.retain(|name, handle| {
            if handle.is_alive() {
                true
            } else {
                dead.push(name.clone());
                false
            }
        });
        for name in &dead {
            tracing::info!(name = %name, "reaped dead child process");
        }
        dead
    }
}

// ─── Spawn function (§5.1) ────────────────────────────────────────────────

/// Result of a spawn attempt.
#[derive(Debug)]
pub struct SpawnResult {
    pub pid: u32,
    pub name: String,
    pub depth: NodeDepth,
}

/// Spawn a child node (§5.1).
///
/// This is the **single** `spawn_child` function used for all tree levels
/// (L0→L1 and L1→L2 go through the same code — recursive symmetry).
///
/// Returns `None` if the spawn was refused or failed.
pub fn spawn_child(
    config: &ForgeConfig,
    proc_mgr: &mut ProcessManager,
    parent_depth: NodeDepth,
    child_def: &NodeDefinition,
    node_toml_path: &Path,
    is_wake_up: bool,
) -> ForgeResult<Option<SpawnResult>> {
    let child_name = NodeName::new(&child_def.node.name);
    let child_depth = parent_depth.child_depth();

    // ── Step 1: Pre-checks ──
    if !pre_checks_pass(config, proc_mgr, child_depth, child_def)? {
        return Ok(None);
    }

    // ── Step 2: Prepare environment ──
    let root = node_toml_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."));

    let env = build_env(&child_name, child_depth, &child_def.node.parent, root, is_wake_up);

    // ── Step 3: Build prompt ──
    let prompt = if is_wake_up {
        build_wake_prompt(&child_def.node.name)
    } else {
        build_first_prompt(&child_def.node.name, &child_def.node.role.to_string(),
            child_depth.as_u32(), &child_def.node.parent, &child_def.node.cwd,
            config.forge.heartbeat_interval_sec, child_def.budget.max_tokens,
            child_def.budget.max_wallclock_sec)
    };

    // ── Step 4: Launch process ──
    let cwd = root.join(&child_def.node.cwd);
    fs::create_dir_all(cwd.join(".forge"))?;
    fs::create_dir_all(cwd.join("shared"))?;

    let stdout_file = fs::File::create(cwd.join(".forge/stdout.log"))?;
    let stderr_file = fs::File::create(cwd.join(".forge/stderr.log"))?;

    // For MVP: use a mock command instead of real `claude -p`
    // Real invocation would be:
    //   claude -p "<prompt>" --dangerously-skip-permissions --output-format json
    let mut cmd = if cfg!(test) || std::env::var("FORGE_MOCK_CLAUDE").is_ok() {
        // Mock: spawn a simple shell that simulates node behavior
        let mut c = Command::new("sh");
        c.args(["-c", &format!(
            "echo '{}' > {}/.forge/pid && echo 'pid=$$' && sleep 3600",
            std::process::id(),
            child_def.node.cwd
        )]);
        c
    } else {
        let mut c = Command::new("claude");
        c.args([
            "-p", &prompt,
            "--dangerously-skip-permissions",
            "--output-format", "json",
        ]);
        c
    };

    cmd.current_dir(&cwd)
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

    let child = cmd.spawn().map_err(|e| ForgeError::Spawn {
        node: child_name.clone(),
        reason: format!("failed to spawn: {e}"),
    })?;

    let pid = child.id();
    tracing::info!(
        name = %child_name,
        pid = pid,
        depth = child_depth.as_u32(),
        wake_up = is_wake_up,
        "spawned child process"
    );

    // ── Step 5: Register and wait for state file ──
    let handle = ChildHandle {
        name: child_name.clone(),
        pid,
        cwd: cwd.clone(),
        depth: child_depth,
        is_wake_up,
        spawned_at: Instant::now(),
        child: Some(child),
    };

    proc_mgr.register(handle);

    let state_file = cwd.join(".forge/state.toml");
    let timeout = Duration::from_secs(u64::from(config.forge.spawn_timeout_sec));

    if !wait_for_state_file(&state_file, timeout) {
        proc_mgr.kill_child(&child_def.node.name)?;
        proc_mgr.remove(&child_def.node.name);
        return Err(ForgeError::Spawn {
            node: child_name,
            reason: "state_file_timeout: node did not write state.toml in time".into(),
        });
    }

    Ok(Some(SpawnResult {
        pid,
        name: child_def.node.name.clone(),
        depth: child_depth,
    }))
}

// ─── Pre-checks (§5.1 step 1) ──────────────────────────────────────────

fn pre_checks_pass(
    config: &ForgeConfig,
    proc_mgr: &ProcessManager,
    child_depth: NodeDepth,
    child_def: &NodeDefinition,
) -> ForgeResult<bool> {
    // Depth check
    if child_depth.as_u32() > config.forge.max_depth {
        tracing::warn!(
            child = %child_def.node.name,
            depth = child_depth.as_u32(),
            max = config.forge.max_depth,
            "spawn refused: max_depth exceeded"
        );
        return Ok(false);
    }

    // Total nodes check
    if proc_mgr.active_count() as u32 + 1 > config.forge.max_total_nodes {
        tracing::warn!(
            child = %child_def.node.name,
            current = proc_mgr.active_count(),
            max = config.forge.max_total_nodes,
            "spawn refused: max_total_nodes exceeded"
        );
        return Ok(false);
    }

    // Budget check (simplified — real impl uses remaining_budget from budget.rs)
    let layer_budget = config
        .budget
        .per_layer
        .iter()
        .find(|lb| lb.layer == child_depth.as_u32());

    if let Some(lb) = layer_budget {
        if let Some(max_tokens) = lb.tokens {
            if child_def.budget.max_tokens > max_tokens {
                tracing::warn!(
                    child = %child_def.node.name,
                    requested = child_def.budget.max_tokens,
                    layer_max = max_tokens,
                    "spawn refused: per-layer budget exceeded"
                );
                return Ok(false);
            }
        }
    }

    Ok(true)
}

// ─── Environment setup (§5.1 step 2) ───────────────────────────────────

fn build_env(
    name: &NodeName,
    depth: NodeDepth,
    parent: &str,
    root: &Path,
    is_wake_up: bool,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("FORGE_NODE_NAME".into(), name.to_string());
    env.insert("FORGE_NODE_DEPTH".into(), depth.as_u32().to_string());
    env.insert("FORGE_PARENT".into(), parent.to_string());
    env.insert("FORGE_ROOT".into(), root.to_string_lossy().to_string());
    env.insert("FORGE_IS_WAKE_UP".into(), is_wake_up.to_string());

    // Inherit API key from orchestrator environment
    if let Ok(key) = std::env::var("CLAUDE_API_KEY") {
        env.insert("CLAUDE_API_KEY".into(), key);
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        env.insert("ANTHROPIC_API_KEY".into(), key);
    }

    env
}

// ─── Prompt builders (§7) ───────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_first_prompt(
    name: &str,
    role: &str,
    depth: u32,
    parent: &str,
    cwd: &str,
    heartbeat_interval: u32,
    max_tokens: u64,
    max_wallclock: u64,
) -> String {
    let mut p = format!(
        r#"You are a CortexForge node in an orchestration tree.

[Identity]
- Node: {name}
- Role: {role}
- Depth: {depth}
- Parent: {parent}
- Working directory: {cwd}

[Hard constraints]
1. Read/write only files under your cwd (realpath verified).
2. All cross-node communication goes through file protocols.
3. Receive tasks: ./.forge/inbox/*.toml
4. Write state: ./.forge/state.toml (overwrite)
5. Declare dependencies: ./shared/needs.toml
6. Provide interfaces: ./shared/provides.toml
7. Heartbeat every {heartbeat_interval}s via state.toml last_heartbeat.
8. Token budget: {max_tokens}, wallclock budget: {max_wallclock}s. Exceed → self-terminate.

[Dependency protocol — CRITICAL]
When you discover you need another module's interface:
1. Write shared/needs.toml FIRST (key + desc + requester path)
2. THEN write state.toml → state="blocked"
3. Stop all work
4. Poll shared/resolved.toml until ALL needed keys appear
5. Check inbox/ for kind="value_changed" messages
6. If value_changed: re-validate your code with new values
7. Write state.toml → state="implementing", resume work

[Delivery]
- When code is complete: state="verifying", run ./verify.sh
- verify passes (exit 0): state="delivered", deliverables → ./deliverables/
- verify fails + retry < max: retry_count++, state="implementing", fix
- verify fails + retry >= max: state="dead", exit
"#
    );

    if role == "domain" {
        p.push_str(&format!(
            r#"
[Domain Agent duties]
1. Manage children: {}
2. Request spawn: write .forge/spawn_requests.toml
3. Scan children .forge/state.toml every {heartbeat_interval}s
4. Aggregate child states into your children_view
5. When all children delivered + all needs resolved: state="implementing"
6. If a child dies: decide degradable (mark partial) vs escalate blocked
"#,
            "(from node.toml children.declared)"
        ));
    } else {
        p.push_str("\n[Note]\nYou do NOT have spawn authority. Only Domain Agents can request child spawn.\n");
    }

    p
}

fn build_wake_prompt(name: &str) -> String {
    format!(
        r#"You are {name}. You were previously delivered and are now re-awakened.

[Reason]
Another module needs interfaces you provide. See shared/tasks.toml.

[Task]
1. Read shared/tasks.toml for status="pending" tasks
2. Process each task: fill values in shared/provides.toml
   - Only NEW keys or VALUE-CHANGED keys get seq+1
   - Confirming existing key with same value: do NOT increment seq
3. Mark each task status="done" in tasks.toml
4. When done: state="delivered", exit
"#
    )
}

// ─── PID file management (§5.3) ─────────────────────────────────────────

/// Write the PID file for a node (§5.3).
///
/// Called by the node itself at startup (via SDK initialization).
pub fn write_pid_file(cwd: &Path, pid: u32, node_name: &str) -> ForgeResult<()> {
    let forge_dir = cwd.join(".forge");
    fs::create_dir_all(&forge_dir)?;

    let pid_path = forge_dir.join("pid");
    let started_at_path = forge_dir.join("started_at");
    let node_name_path = forge_dir.join("node_name");

    fs::write(&pid_path, pid.to_string())?;
    fs::write(&started_at_path, chrono::Utc::now().to_rfc3339())?;
    fs::write(&node_name_path, node_name)?;

    Ok(())
}

/// Read the PID from a node's .forge/pid file.
#[must_use] 
pub fn read_pid_file(cwd: &Path) -> Option<u32> {
    let pid_path = cwd.join(".forge/pid");
    fs::read_to_string(&pid_path)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Read all PID info from a node directory.
#[must_use] 
pub fn read_pid_info(cwd: &Path) -> Option<(u32, String, String)> {
    let pid = read_pid_file(cwd)?;
    let node_name = fs::read_to_string(cwd.join(".forge/node_name"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let started_at = fs::read_to_string(cwd.join(".forge/started_at"))
        .unwrap_or_default()
        .trim()
        .to_string();
    Some((pid, node_name, started_at))
}

/// Wait for a state.toml file to appear within a timeout.
fn wait_for_state_file(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

// ─── OS-level process utilities ─────────────────────────────────────────

/// Probe whether a PID is alive using `kill(pid, 0)` (via nix safe wrapper).
///
/// Returns true if the process exists (signal delivery possible).
#[cfg(unix)]
#[must_use] 
pub fn os_probe_pid(pid: u32) -> bool {
    
    use nix::unistd::Pid;
    nix::sys::signal::kill(Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(not(unix))]
pub fn os_probe_pid(_pid: u32) -> bool {
    false
}

/// Send SIGKILL to a PID (via nix safe wrapper).
#[cfg(unix)]
fn os_kill_pid(pid: u32) -> ForgeResult<()> {
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;
    nix::sys::signal::kill(Pid::from_raw(pid as i32), Signal::SIGKILL).map_err(|e| {
        ForgeError::Spawn {
            node: NodeName::new(format!("pid-{pid}")),
            reason: format!("kill failed: {e}"),
        }
    })
}

#[cfg(not(unix))]
fn os_kill_pid(_pid: u32) -> ForgeResult<()> {
    Err(ForgeError::Other("process kill not supported on this platform".into()))
}

// ─── Orchestrator PID rebuild (§5.4) ────────────────────────────────────

/// Rebuild the process manager index from `.forge/pid` files on disk.
///
/// Called at Orchestrator startup or after crash recovery (§5.4).
pub fn rebuild_pids_table(
    root: &Path,
    proc_mgr: &mut ProcessManager,
) -> ForgeResult<usize> {
    let mut count = 0;
    rebuild_recursive(root, root, proc_mgr, &mut count)?;
    tracing::info!(recovered = count, "rebuilt PID table from disk");
    Ok(count)
}

fn rebuild_recursive(
    root: &Path,
    dir: &Path,
    proc_mgr: &mut ProcessManager,
    count: &mut usize,
) -> ForgeResult<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Skip hidden dirs (except for forge root itself)
        let dir_name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        if dir_name.starts_with('.') {
            continue;
        }

        // Check for .forge/pid
        let forge_dir = path.join(".forge");
        if forge_dir.exists() {
            if let Some((pid, node_name, _started_at)) = read_pid_info(&path) {
                if os_probe_pid(pid) {
                    let handle = ChildHandle {
                        name: NodeName::new(&node_name),
                        pid,
                        cwd: path.clone(),
                        depth: estimate_depth(root, &path),
                        is_wake_up: false,
                        spawned_at: Instant::now(),
                        child: None, // No child handle after restart
                    };
                    proc_mgr.register(handle);
                    *count += 1;
                } else {
                    tracing::info!(
                        pid = pid,
                        node = %node_name,
                        "orphan detected: PID not alive, marking dead"
                    );
                }
            }
        }

        rebuild_recursive(root, &path, proc_mgr, count)?;
    }
    Ok(())
}

/// Estimate node depth from path relative to root.
fn estimate_depth(root: &Path, node_dir: &Path) -> NodeDepth {
    let rel = node_dir.strip_prefix(root).unwrap_or(node_dir);
    let depth = rel.components().count() as u32;
    NodeDepth(depth)
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ForgeSection, BudgetSection, PathsSection};
    use crate::protocol::{
        NodeDefSection, NodeBudgetSection,
    };
    use crate::types::NodeRole;

    fn test_config() -> ForgeConfig {
        ForgeConfig {
            forge: ForgeSection {
                schema_version: 1,
                max_depth: 4,
                max_total_nodes: 64,
                heartbeat_interval_sec: 15,
                heartbeat_timeout_sec: 60,
                default_max_retries: 3,
                stuck_threshold_heartbeats: 4,
                scan_interval_sec: 5,
                spawn_timeout_sec: 5,
            },
            budget: BudgetSection::default(),
            paths: PathsSection::default(),
        }
    }

    fn test_node_def(name: &str, cwd: &str) -> NodeDefinition {
        NodeDefinition {
            node: NodeDefSection {
                name: name.into(),
                role: NodeRole::Module,
                cwd: cwd.into(),
                parent: "test-parent".into(),
                depth: 2,
            },
            children: Default::default(),
            provides: Default::default(),
            budget: NodeBudgetSection::default(),
            runtime: Default::default(),
        }
    }

    #[test]
    fn test_pre_checks_depth_exceeded() {
        let config = ForgeConfig {
            forge: ForgeSection { max_depth: 2, ..test_config().forge },
            ..test_config()
        };
        let pm = ProcessManager::new();
        let def = test_node_def("deep-node", "modules/deep");
        // parent depth 2 → child depth 3 > max_depth 2
        let result = pre_checks_pass(&config, &pm, NodeDepth(3), &def).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_pre_checks_pass() {
        let config = test_config();
        let pm = ProcessManager::new();
        let def = test_node_def("ok-node", "modules/ok");
        let result = pre_checks_pass(&config, &pm, NodeDepth(2), &def).unwrap();
        assert!(result);
    }

    #[test]
    fn test_pid_file_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        write_pid_file(dir.path(), 4242, "test-node").unwrap();
        let pid = read_pid_file(dir.path()).unwrap();
        assert_eq!(pid, 4242);
        let (pid2, name, _) = read_pid_info(dir.path()).unwrap();
        assert_eq!(pid2, 4242);
        assert_eq!(name, "test-node");
    }

    #[test]
    fn test_os_probe_nonexistent_pid() {
        // PID 99999 is unlikely to exist
        assert!(!os_probe_pid(99999));
    }

    #[test]
    fn test_process_manager_register_and_remove() {
        let mut pm = ProcessManager::new();
        let dir = tempfile::tempdir().unwrap();
        write_pid_file(dir.path(), std::process::id(), "test").unwrap();

        let handle = ChildHandle {
            name: NodeName::new("test"),
            pid: std::process::id(),
            cwd: dir.path().to_path_buf(),
            depth: NodeDepth(2),
            is_wake_up: false,
            spawned_at: Instant::now(),
            child: None,
        };
        pm.register(handle);
        assert_eq!(pm.active_count(), 1);

        let removed = pm.remove("test");
        assert!(removed.is_some());
        assert_eq!(pm.active_count(), 0);
    }

    #[test]
    fn test_spawn_refused_max_depth() {
        let config = ForgeConfig {
            forge: ForgeSection { max_depth: 1, ..test_config().forge },
            ..test_config()
        };
        let mut pm = ProcessManager::new();
        let def = test_node_def("too-deep", "m");

        // Create a temporary node.toml for the function
        let tmp = tempfile::tempdir().unwrap();
        let node_toml = tmp.path().join("node.toml");
        def.save(&node_toml).unwrap();

        let result = spawn_child(&config, &mut pm, NodeDepth(1), &def, &node_toml, false).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_build_env() {
        let env = build_env(
            &NodeName::new("test-node"),
            NodeDepth(2),
            "parent",
            Path::new("/forge/root"),
            false,
        );
        assert_eq!(env.get("FORGE_NODE_NAME").unwrap(), "test-node");
        assert_eq!(env.get("FORGE_NODE_DEPTH").unwrap(), "2");
        assert_eq!(env.get("FORGE_PARENT").unwrap(), "parent");
        assert_eq!(env.get("FORGE_IS_WAKE_UP").unwrap(), "false");
    }

    #[test]
    fn test_prompt_contains_keywords() {
        let p = build_first_prompt("test", "module", 2, "parent", "cwd", 15, 100_000, 1800);
        assert!(p.contains("test"));
        assert!(p.contains("module"));
        assert!(p.contains("needs.toml"));
        assert!(p.contains("resolved.toml"));
        assert!(p.contains("verify.sh"));

        // Domain prompt should include domain-specific text
        let dp = build_first_prompt("dom", "domain", 1, "", "dom", 15, 100_000, 1800);
        assert!(dp.contains("Domain Agent duties"));
        assert!(dp.contains("spawn_requests.toml"));
    }

    #[test]
    fn test_wake_prompt() {
        let p = build_wake_prompt("mod-bsp-uart");
        assert!(p.contains("mod-bsp-uart"));
        assert!(p.contains("re-awakened"));
        assert!(p.contains("tasks.toml"));
        assert!(p.contains("provides.toml"));
    }
}
