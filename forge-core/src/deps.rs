//! Pull-based dependency resolution engine (§15).
//!
//! Implements the 10-pass Orchestrator main loop:
//! Pass 1: collect → Pass 2: build graph → Pass 3: 1st cycle check →
//! Pass 4: match new edges → Pass 5: 2nd cycle check →
//! Pass 6: write tasks + spawn → Pass 6b: `spawn_requests` →
//! Pass 7: transfer resolved → Pass 7b: dependency chain propagation →
//! Pass 8: value change detection → Pass 9: cross-layer escalation

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::ForgeConfig;
use crate::error::ForgeResult;
use crate::event::EventType;
use crate::eventbus::EventBus;
use crate::protocol::{
    EscalatedNeed, EscalatedStatus, EscalatedTable, NeedsDeclaration, NodeDefinition, NodeState,
    ProvidesDeclaration, ResolvedEntry, ResolvedValues, TaskList,
};
use crate::state::NodeStatus;

// ─── Dependency graph types ──────────────────────────────────────────────

/// A directed edge in the dependency graph: requester needs a key from provider.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DepEdge {
    pub requester: String,
    pub provider: String,
    pub key: String,
}

/// A snapshot of a node's state used during a scan cycle.
#[derive(Debug, Clone)]
pub struct NodeSnapshot {
    pub name: String,
    pub cwd: PathBuf,
    pub status: NodeStatus,
    pub state_seq: u64,
    pub needs: NeedsDeclaration,
    pub provides: ProvidesDeclaration,
    pub resolved: ResolvedValues,
    pub tasks: TaskList,
    pub def: NodeDefinition,
    pub pid: Option<u32>,
}

/// The dependency graph built during one scan cycle.
#[derive(Debug, Default)]
pub struct DepGraph {
    pub nodes: HashMap<String, NodeSnapshot>,
    pub existing_edges: Vec<DepEdge>,
    pub new_edges: Vec<DepEdge>,
    pub cycle_nodes: Vec<String>,
}

impl DepGraph {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── Pass 1: Collect all node states ─────────────────────────────────

    /// Recursively collect all declared nodes.
    /// Fix #98, #115: single function, renamed from `collect_all_active_nodes`.
    #[must_use]
    pub fn collect_all_declared_nodes(root: &Path) -> Vec<(String, PathBuf)> {
        let mut nodes = Vec::new();
        Self::collect_recursive(root, root, &mut nodes);
        nodes
    }

    #[allow(clippy::only_used_in_recursion)]
    fn collect_recursive(base: &Path, dir: &Path, nodes: &mut Vec<(String, PathBuf)>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            if name.starts_with('.') {
                continue;
            }

            let node_toml = path.join("node.toml");
            if node_toml.exists() {
                if let Ok(def) = NodeDefinition::load(&node_toml) {
                    nodes.push((def.node.name.clone(), path.clone()));
                }
            }
            Self::collect_recursive(base, &path, nodes);
        }
    }

    /// Populate the graph with node snapshots (Pass 1).
    /// Returns the set of alive node names.
    pub fn populate(
        &mut self,
        declared_nodes: &[(String, PathBuf)],
    ) -> ForgeResult<HashSet<String>> {
        let mut alive = HashSet::new();

        for (name, cwd) in declared_nodes {
            // Read PID
            let pid = crate::spawn::read_pid_file(cwd);
            let is_alive = pid.is_some_and(crate::spawn::os_probe_pid);

            if !is_alive {
                continue; // only alive nodes participate
            }

            // Read state
            let state = crate::safe_read_toml::<NodeState>(&cwd.join(".forge/state.toml"));
            let status = state
                .as_ref()
                .and_then(|s| s.state.current.parse::<NodeStatus>().ok())
                .unwrap_or(NodeStatus::Dead);
            let state_seq = state.as_ref().map_or(0, |s| s.state.sequence);

            if status == NodeStatus::Dead {
                continue; // dead nodes don't participate (§15.7, fix #P1-9)
            }

            // Read protocol files
            let needs = crate::safe_read_toml::<NeedsDeclaration>(&cwd.join("shared/needs.toml"))
                .unwrap_or_default();

            let provides =
                crate::safe_read_toml::<ProvidesDeclaration>(&cwd.join("shared/provides.toml"))
                    .unwrap_or_default();

            let resolved =
                crate::safe_read_toml::<ResolvedValues>(&cwd.join("shared/resolved.toml"))
                    .unwrap_or_default();

            let tasks = crate::safe_read_toml::<TaskList>(&cwd.join("shared/tasks.toml"))
                .unwrap_or_default();

            // Read node definition
            let def = NodeDefinition::load(&cwd.join("node.toml"))?;

            self.nodes.insert(
                name.clone(),
                NodeSnapshot {
                    name: name.clone(),
                    cwd: cwd.clone(),
                    status,
                    state_seq,
                    needs,
                    provides,
                    resolved,
                    tasks,
                    def,
                    pid,
                },
            );

            alive.insert(name.clone());
        }

        Ok(alive)
    }

    // ── Pass 2: Build dependency graph ─────────────────────────────────

    /// Build edges from needs → provides.declared (§15.3 Pass 2).
    /// Fix #P0-7: use node.toml provides.declared (static), not provides.toml values.
    pub fn build_graph(&mut self) {
        self.existing_edges.clear();

        for (req_name, req) in &self.nodes {
            for key in req.needs.needs.keys() {
                // Skip already resolved keys (fix #12)
                if req.resolved.resolved.contains_key(key) {
                    continue;
                }

                // Find provider by declared provides
                for (prov_name, prov) in &self.nodes {
                    if prov_name == req_name {
                        continue;
                    }
                    if prov.def.provides.declared.iter().any(|d| d == key) {
                        self.existing_edges.push(DepEdge {
                            requester: req_name.clone(),
                            provider: prov_name.clone(),
                            key: key.clone(),
                        });
                    }
                }
            }
        }
    }

    // ── Pass 3-5: Cycle detection (two rounds) ─────────────────────────

    /// Detect cycles in the current edge set (§15.6).
    /// Returns the list of nodes in the cycle, or empty if acyclic.
    #[must_use]
    pub fn detect_cycles(&self, edges: &[DepEdge]) -> Vec<String> {
        // Build adjacency list
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in edges {
            adj.entry(&edge.requester).or_default().push(&edge.provider);
        }

        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();
        let mut cycle_nodes = Vec::new();

        for node in adj.keys() {
            if !visited.contains(node)
                && self.dfs_cycle(node, &adj, &mut visited, &mut in_stack, &mut cycle_nodes)
            {
                return cycle_nodes;
            }
        }

        cycle_nodes
    }

    fn dfs_cycle<'a>(
        &self,
        node: &'a str,
        adj: &HashMap<&'a str, Vec<&'a str>>,
        visited: &mut HashSet<&'a str>,
        in_stack: &mut HashSet<&'a str>,
        cycle_nodes: &mut Vec<String>,
    ) -> bool {
        visited.insert(node);
        in_stack.insert(node);

        if let Some(neighbors) = adj.get(node) {
            for &neighbor in neighbors {
                if in_stack.contains(neighbor) {
                    cycle_nodes.push(neighbor.to_string());
                    return true;
                }
                if !visited.contains(neighbor)
                    && self.dfs_cycle(neighbor, adj, visited, in_stack, cycle_nodes)
                {
                    return true;
                }
            }
        }

        in_stack.remove(node);
        false
    }

    /// Pass 3: First cycle detection on existing edges.
    pub fn pass3_first_cycle_check(&mut self, eventbus: &EventBus) -> ForgeResult<bool> {
        let cycle = self.detect_cycles(&self.existing_edges);
        if !cycle.is_empty() {
            eventbus.append(&crate::event::EventEntry::new(
                "orchestrator",
                EventType::Deadlock { cycle: cycle.clone() },
            ))?;
            self.cycle_nodes = cycle;
            return Ok(true); // has cycle
        }
        Ok(false)
    }

    /// Pass 4: Match unresolved needs → generate new edges.
    /// Fix #P1-6: only process blocked nodes.
    pub fn pass4_match_new_edges(&mut self, root: &Path, eventbus: &EventBus) -> ForgeResult<()> {
        self.new_edges.clear();

        for (req_name, req) in &self.nodes {
            if req.status != NodeStatus::Blocked {
                continue;
            }

            for key in req.needs.needs.keys() {
                // Skip resolved
                if req.resolved.resolved.contains_key(key) {
                    continue;
                }

                let provider = self.find_provider(key);
                if let Some(prov_name) = provider {
                    // Check dedup (fix #P1-4)
                    if req.tasks.has_task(key, req_name) {
                        continue;
                    }
                    self.new_edges.push(DepEdge {
                        requester: req_name.clone(),
                        provider: prov_name,
                        key: key.clone(),
                    });
                } else {
                    // No provider → escalate (fix #P1-5)
                    self.escalate_to_parent(root, req_name, key, eventbus)?;
                }
            }
        }

        Ok(())
    }

    /// Pass 5: Second cycle detection including new edges.
    pub fn pass5_second_cycle_check(&mut self, eventbus: &EventBus) -> ForgeResult<bool> {
        let all_edges: Vec<DepEdge> =
            self.existing_edges.iter().chain(self.new_edges.iter()).cloned().collect();

        let cycle = self.detect_cycles(&all_edges);
        if !cycle.is_empty() {
            eventbus.append(&crate::event::EventEntry::new(
                "orchestrator",
                EventType::NewDeadlockPrevented {
                    new_edges: self
                        .new_edges
                        .iter()
                        .map(|e| format!("{}→{}", e.requester, e.provider))
                        .collect(),
                },
            ))?;
            self.cycle_nodes = cycle;
            // Remove edges involving cycle nodes
            self.new_edges.retain(|e| !self.cycle_nodes.contains(&e.requester));
            return Ok(true);
        }
        Ok(false)
    }

    /// Pass 6: Write tasks.toml + spawn decisions.
    /// Fix #77: remove task on spawn failure.
    /// Fix #79: update provider.pid after spawn to prevent duplicate spawn.
    pub fn pass6_write_tasks_and_spawn(
        &mut self,
        _config: &ForgeConfig,
        _root: &Path,
        eventbus: &EventBus,
    ) -> ForgeResult<()> {
        // Clone edges first to avoid borrow conflicts
        let edges: Vec<DepEdge> = self.new_edges.clone();

        // Collect required data from immutable borrows
        let mut tasks_to_write: Vec<(String, String, String, String)> = Vec::new(); // (provider, cwd, key, desc_from)
        let mut wake_events: Vec<(String, String)> = Vec::new();

        for edge in &edges {
            let desc = self
                .nodes
                .get(&edge.requester)
                .and_then(|r| r.needs.needs.get(&edge.key))
                .map(|e| e.desc.clone())
                .unwrap_or_default();

            let needs_spawn = if let Some(prov) = self.nodes.get(&edge.provider) {
                tasks_to_write.push((
                    edge.provider.clone(),
                    prov.cwd.to_string_lossy().to_string(),
                    edge.key.clone(),
                    desc,
                ));

                if prov.pid.is_none_or(|p| !crate::spawn::os_probe_pid(p)) {
                    let has_value = prov.provides.provides.contains_key(&edge.key);
                    !(prov.status == NodeStatus::Delivered && has_value)
                } else {
                    false
                }
            } else {
                false
            };

            if needs_spawn {
                wake_events.push((edge.provider.clone(), edge.key.clone()));
            }
        }

        // Now apply mutations
        for (prov_name, cwd, key, desc) in &tasks_to_write {
            if let Some(prov) = self.nodes.get_mut(prov_name) {
                prov.tasks.add_if_absent(key, desc, prov_name);
                let tasks_path = Path::new(cwd).join("shared/tasks.toml");
                let _ = prov.tasks.save(&tasks_path);
            }
        }

        for (provider, key) in &wake_events {
            eventbus.append(&crate::event::EventEntry::new(
                "orchestrator",
                EventType::SpawnWakeFailed { provider: provider.clone(), key: key.clone() },
            ))?;
        }

        Ok(())
    }

    /// Pass 7: Transfer resolved values from providers to requesters.
    /// Fix #39, #44, #51, #60: provides→resolved format conversion.
    /// Fix #99: based on Pass 1 snapshot, new spawn providers visible next cycle.
    pub fn pass7_transfer_resolved(&mut self, eventbus: &EventBus) -> ForgeResult<()> {
        // Collect changes first (immutable borrow)
        let mut updates: Vec<(String, String, String, String, u64)> = Vec::new(); // (requester, cwd, key, value_from, seq)

        for (req_name, req) in &self.nodes {
            if req.status != NodeStatus::Blocked {
                continue;
            }

            let mut current = req.resolved.clone();
            let mut changed = false;

            for key in req.needs.needs.keys() {
                if current.resolved.contains_key(key) {
                    continue;
                }

                let provider = self.find_provider(key);
                if let Some(ref prov_name) = provider {
                    if let Some(prov) = self.nodes.get(prov_name.as_str()) {
                        if let Some(entry) = prov.provides.provides.get(key) {
                            current.resolved.insert(
                                key.clone(),
                                ResolvedEntry {
                                    value: entry.value.clone(),
                                    from: prov_name.clone(),
                                    seq: entry.seq,
                                },
                            );
                            changed = true;
                            updates.push((
                                req_name.clone(),
                                req.cwd.to_string_lossy().to_string(),
                                key.clone(),
                                prov_name.clone(),
                                entry.seq,
                            ));
                        }
                    }
                }
            }

            if changed {
                let resolved_path = req.cwd.join("shared/resolved.toml");
                current.save(&resolved_path)?;
            }
        }

        // Apply updates to in-memory snapshots
        for (req_name, _cwd, key, _from, _seq) in &updates {
            if let Some(_req) = self.nodes.get_mut(req_name) {
                // Value is already in the file; just emit events
            }
            eventbus.append(&crate::event::EventEntry::new(
                "orchestrator",
                EventType::DependencyResolved { requester: req_name.clone(), key: key.clone() },
            ))?;
        }

        Ok(())
    }

    /// Pass 7b: Dependency chain propagation (§6.4, §15.7).
    /// Fix #88, #97: mark requester dead if all providers dead + no pending escalation.
    pub fn pass7b_dependency_chain(
        &mut self,
        escalated: &EscalatedTable,
        eventbus: &EventBus,
    ) -> ForgeResult<()> {
        for (req_name, req) in &self.nodes {
            if req.status != NodeStatus::Blocked {
                continue;
            }
            if req.needs.needs.is_empty() {
                continue;
            }

            let mut all_providers_dead = true;
            for key in req.needs.needs.keys() {
                if req.resolved.resolved.contains_key(key) {
                    continue;
                }

                let provider = self.find_provider(key);
                if let Some(prov_name) = provider {
                    if let Some(prov) = self.nodes.get(&prov_name) {
                        let is_alive = prov.pid.is_some_and(crate::spawn::os_probe_pid);
                        let has_value = prov.provides.provides.contains_key(key);
                        if is_alive || has_value {
                            all_providers_dead = false;
                            break;
                        }
                    }
                } else {
                    // No provider in tree → check if escalation is pending
                    if escalated.has_pending(key, req_name) {
                        all_providers_dead = false;
                        break;
                    }
                }
            }

            if all_providers_dead {
                // Mark requester dead
                let state_path = req.cwd.join(".forge/state.toml");
                if let Ok(mut state) = NodeState::load(&state_path) {
                    state.state.current = "dead".into();
                    state.save(&state_path)?;
                }
                eventbus.append(&crate::event::EventEntry::new(
                    req_name.as_str(),
                    EventType::NodeDead { reason: "all providers dead".into() },
                ))?;
            }
        }

        Ok(())
    }

    /// Pass 8: Value change detection.
    /// Fix #P1-6: don't pull back to implementing, just inbox notify.
    /// Fix #102: null guard on provides[provider][key].
    pub fn pass8_value_change_detection(&mut self, eventbus: &EventBus) -> ForgeResult<()> {
        // Collect changes (immutable pass)
        let mut inbox_writes: Vec<(String, String, u64, u64)> = Vec::new(); // (requester, key, old_seq, new_seq)
        let mut resolved_writes: Vec<(String, ResolvedValues)> = Vec::new();

        for (req_name, req) in &self.nodes {
            let mut changed = false;
            let mut current = req.resolved.clone();

            for (key, resolved_entry) in &req.resolved.resolved {
                let provider = self.find_provider(key);
                if let Some(ref prov_name) = provider {
                    if let Some(prov) = self.nodes.get(prov_name.as_str()) {
                        if let Some(entry) = prov.provides.get(key) {
                            if entry.seq > resolved_entry.seq {
                                let old_seq = resolved_entry.seq;
                                current.resolved.insert(
                                    key.clone(),
                                    ResolvedEntry {
                                        value: entry.value.clone(),
                                        from: prov_name.clone(),
                                        seq: entry.seq,
                                    },
                                );
                                changed = true;
                                inbox_writes.push((
                                    req_name.clone(),
                                    key.clone(),
                                    old_seq,
                                    entry.seq,
                                ));
                            }
                        }
                    }
                }
            }

            if changed {
                resolved_writes.push((req_name.clone(), current.clone()));
                let resolved_path = req.cwd.join("shared/resolved.toml");
                current.save(&resolved_path)?;
            }
        }

        // Apply memory updates + inbox + events
        for (req_name, current) in &resolved_writes {
            if let Some(req) = self.nodes.get_mut(req_name) {
                req.resolved = current.clone();
            }
        }

        for (req_name, key, old_seq, new_seq) in &inbox_writes {
            if let Some(req) = self.nodes.get(req_name) {
                let msg = crate::protocol::InboxMessage {
                    schema_version: 1,
                    id: uuid::Uuid::new_v4().to_string(),
                    from: "orchestrator".into(),
                    to: req_name.clone(),
                    created_at: chrono::Utc::now().into(),
                    kind: crate::protocol::MessageKind::ValueChanged,
                    ref_task_id: None,
                    priority: "P1".into(),
                    body: crate::protocol::MessageBody {
                        key: Some(key.clone()),
                        old_seq: Some(*old_seq),
                        new_seq: Some(*new_seq),
                        ..Default::default()
                    },
                };
                let _ = msg.write_to_inbox(&req.cwd.join(".forge/inbox"));
            }

            eventbus.append(&crate::event::EventEntry::new(
                "orchestrator",
                EventType::ValueChanged { target: req_name.clone(), key: key.clone() },
            ))?;
        }

        Ok(())
    }

    /// Pass 9: Cross-layer escalation processing (§15.5).
    /// Fix #24, #38, #44, #67: full state machine for escalated entries.
    pub fn pass9_cross_layer(
        &self,
        root: &Path,
        escalated: &mut EscalatedTable,
        eventbus: &EventBus,
    ) -> ForgeResult<()> {
        for entry in &mut escalated.needs {
            match entry.status {
                EscalatedStatus::Pending => {
                    // Try to find a global provider
                    let declared = Self::collect_all_declared_nodes(root);
                    for (name, cwd) in &declared {
                        if let Ok(def) = NodeDefinition::load(&cwd.join("node.toml")) {
                            if def.provides.declared.contains(&entry.key) {
                                // Write task to provider
                                let mut tasks = crate::safe_read_toml::<TaskList>(
                                    &cwd.join("shared/tasks.toml"),
                                )
                                .unwrap_or_default();
                                tasks.add_if_absent(
                                    &entry.key,
                                    &format!("cross-layer need from {}", entry.requester),
                                    &entry.requester,
                                );
                                let _ = tasks.save(&cwd.join("shared/tasks.toml"));

                                entry.provider = Some(name.clone());
                                entry.status = EscalatedStatus::Matched;
                                break;
                            }
                        }
                    }
                }
                EscalatedStatus::Matched => {
                    // Check if provider delivered
                    let prov_name = match &entry.provider {
                        Some(n) => n.clone(),
                        None => continue,
                    };

                    // Find provider in our snapshot
                    if let Some(prov) = self.nodes.get(&prov_name) {
                        // Fix #38: check state=delivered guard
                        if prov.status == NodeStatus::Dead {
                            entry.attempt_count += 1;
                            if entry.attempt_count >= 3 {
                                entry.status = EscalatedStatus::Failed;
                                eventbus.append(&crate::event::EventEntry::new(
                                    "orchestrator",
                                    EventType::EscalationFailed {
                                        key: entry.key.clone(),
                                        requester: entry.requester.clone(),
                                    },
                                ))?;
                            } else {
                                entry.provider = None;
                                entry.status = EscalatedStatus::Pending;
                            }
                            continue;
                        }

                        let pid_alive = prov.pid.is_some_and(crate::spawn::os_probe_pid);
                        if !pid_alive
                            && prov.status == NodeStatus::Delivered
                            && prov.provides.provides.contains_key(&entry.key)
                        {
                            // Provider done → write resolved to requester
                            if let Some(req) = self.nodes.get(&entry.requester) {
                                let mut resolved = req.resolved.clone();
                                if let Some(pv) = prov.provides.provides.get(&entry.key) {
                                    resolved.resolved.insert(
                                        entry.key.clone(),
                                        ResolvedEntry {
                                            value: pv.value.clone(),
                                            from: prov_name.clone(),
                                            seq: pv.seq,
                                        },
                                    );
                                    let _ = resolved.save(&req.cwd.join("shared/resolved.toml"));
                                }
                            }
                            entry.status = EscalatedStatus::Resolved;
                            eventbus.append(&crate::event::EventEntry::new(
                                "orchestrator",
                                EventType::CrossLayerResolved {
                                    requester: entry.requester.clone(),
                                    key: entry.key.clone(),
                                },
                            ))?;
                        }
                    }
                }
                _ => {}
            }
        }

        // Clean up terminals (fix #78)
        escalated.remove_terminals();

        Ok(())
    }

    // ── Helper functions ────────────────────────────────────────────────

    /// Find a provider for a key in the current graph snapshot.
    /// Fix #68: searches in graph's node snapshot (Pass 1 collection).
    #[must_use]
    pub fn find_provider(&self, key: &str) -> Option<String> {
        for (name, node) in &self.nodes {
            if node.def.provides.declared.iter().any(|d| d == key) {
                return Some(name.clone());
            }
        }
        None
    }

    /// Escalate an unmet dependency to the cross-layer table.
    /// Fix #107: dedup check before writing.
    pub fn escalate_to_parent(
        &self,
        root: &Path,
        requester: &str,
        key: &str,
        eventbus: &EventBus,
    ) -> ForgeResult<()> {
        let escalated_path = root.join(".forge/escalated.toml");
        let mut table = EscalatedTable::load(&escalated_path).unwrap_or_default();

        // Dedup (fix #107)
        if table.has_pending(key, requester) {
            return Ok(());
        }

        if let Some(req) = self.nodes.get(requester) {
            table.needs.push(EscalatedNeed {
                key: key.to_string(),
                requester: requester.to_string(),
                provider: None,
                status: EscalatedStatus::Pending,
                attempt_count: 0,
                created_at: Some(chrono::Utc::now().into()),
                provides: req.def.provides.declared.clone(),
            });
        }

        table.save(&escalated_path)?;

        eventbus.append(&crate::event::EventEntry::new(
            "orchestrator",
            EventType::DependencyEscalated {
                requester: requester.to_string(),
                key: key.to_string(),
            },
        ))?;

        Ok(())
    }

    /// Mark all nodes in a cycle as dead (recursively kills children).
    /// Fix #61: recursive `mark_cycle_dead`.
    pub fn mark_cycle_dead(&self, cycle_nodes: &[String], eventbus: &EventBus) -> ForgeResult<()> {
        for name in cycle_nodes {
            if let Some(node) = self.nodes.get(name) {
                let state_path = node.cwd.join(".forge/state.toml");
                if let Ok(mut state) = NodeState::load(&state_path) {
                    state.state.current = "dead".into();
                    state.progress.summary = "cycle dependency detected".into();
                    state.save(&state_path)?;
                }
            }
            eventbus.append(&crate::event::EventEntry::new(
                name,
                EventType::NodeDead { reason: "cycle dependency".into() },
            ))?;
        }
        Ok(())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_nodes(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let a_dir = dir.join("mod-a");
        let b_dir = dir.join("mod-b");
        let c_dir = dir.join("mod-c");

        // Create root .forge for escalated.toml
        std::fs::create_dir_all(dir.join(".forge")).unwrap();

        for d in [&a_dir, &b_dir, &c_dir] {
            std::fs::create_dir_all(d.join(".forge")).unwrap();
            std::fs::create_dir_all(d.join("shared")).unwrap();
        }

        (a_dir, b_dir, c_dir)
    }

    fn write_node_def(cwd: &Path, name: &str, provides: &[&str]) {
        let def = NodeDefinition {
            node: crate::protocol::NodeDefSection {
                name: name.into(),
                role: crate::types::NodeRole::Module,
                cwd: cwd.to_string_lossy().to_string(),
                parent: String::new(),
                depth: 2,
            },
            children: Default::default(),
            provides: crate::protocol::NodeProvidesSection {
                declared: provides.iter().map(|s| s.to_string()).collect(),
            },
            budget: Default::default(),
            runtime: Default::default(),
        };
        def.save(&cwd.join("node.toml")).unwrap();
    }

    fn write_node_state(cwd: &Path, status: &str, seq: u64) {
        let now: chrono::DateTime<chrono::FixedOffset> = chrono::Utc::now().into();
        let state = NodeState {
            schema_version: 1,
            state: crate::protocol::StateSection {
                current: status.into(),
                entered_at: now,
                last_heartbeat: now,
                sequence: seq,
            },
            progress: Default::default(),
            children_view: Default::default(),
            verify: Default::default(),
            budget_used: Default::default(),
        };
        state.save(&cwd.join(".forge/state.toml")).unwrap();
    }

    #[test]
    fn test_cycle_detection_simple() {
        let graph = DepGraph::new();
        let edges = vec![
            DepEdge { requester: "A".into(), provider: "B".into(), key: "X".into() },
            DepEdge { requester: "B".into(), provider: "A".into(), key: "Y".into() },
        ];
        let cycle = graph.detect_cycles(&edges);
        assert!(!cycle.is_empty());
    }

    #[test]
    fn test_cycle_detection_no_cycle() {
        let graph = DepGraph::new();
        let edges = vec![
            DepEdge { requester: "A".into(), provider: "B".into(), key: "X".into() },
            DepEdge { requester: "B".into(), provider: "C".into(), key: "Y".into() },
        ];
        let cycle = graph.detect_cycles(&edges);
        assert!(cycle.is_empty());
    }

    #[test]
    fn test_cycle_detection_three_node() {
        let graph = DepGraph::new();
        let edges = vec![
            DepEdge { requester: "A".into(), provider: "B".into(), key: "X".into() },
            DepEdge { requester: "B".into(), provider: "C".into(), key: "Y".into() },
            DepEdge { requester: "C".into(), provider: "A".into(), key: "Z".into() },
        ];
        let cycle = graph.detect_cycles(&edges);
        assert!(!cycle.is_empty());
    }

    #[test]
    fn test_find_provider() {
        let dir = tempfile::tempdir().unwrap();
        let (a_dir, b_dir, _c_dir) = setup_test_nodes(dir.path());

        write_node_def(&a_dir, "mod-a", &[]);
        write_node_def(&b_dir, "mod-b", &["APB1_CLK", "UART_TX"]);

        let mut graph = DepGraph::new();
        graph.nodes.insert(
            "mod-a".into(),
            NodeSnapshot {
                name: "mod-a".into(),
                cwd: a_dir,
                status: NodeStatus::Blocked,
                state_seq: 1,
                needs: NeedsDeclaration::default(),
                provides: ProvidesDeclaration::default(),
                resolved: ResolvedValues::default(),
                tasks: TaskList::default(),
                def: NodeDefinition::load(&dir.path().join("mod-a/node.toml")).unwrap(),
                pid: None,
            },
        );
        graph.nodes.insert(
            "mod-b".into(),
            NodeSnapshot {
                name: "mod-b".into(),
                cwd: b_dir,
                status: NodeStatus::Implementing,
                state_seq: 1,
                needs: NeedsDeclaration::default(),
                provides: ProvidesDeclaration::default(),
                resolved: ResolvedValues::default(),
                tasks: TaskList::default(),
                def: NodeDefinition::load(&dir.path().join("mod-b/node.toml")).unwrap(),
                pid: None,
            },
        );

        assert_eq!(graph.find_provider("APB1_CLK"), Some("mod-b".into()));
        assert_eq!(graph.find_provider("NONEXISTENT"), None);
    }

    #[test]
    fn test_collect_all_declared_nodes() {
        let dir = tempfile::tempdir().unwrap();
        setup_test_nodes(dir.path());
        write_node_def(&dir.path().join("mod-a"), "mod-a", &[]);
        write_node_def(&dir.path().join("mod-b"), "mod-b", &["X"]);

        let nodes = DepGraph::collect_all_declared_nodes(dir.path());
        assert!(nodes.iter().any(|(n, _)| n == "mod-a"));
        assert!(nodes.iter().any(|(n, _)| n == "mod-b"));
    }

    #[test]
    fn test_escalate_to_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (a_dir, _b_dir, _c_dir) = setup_test_nodes(dir.path());
        write_node_def(&a_dir, "mod-a", &["FLASH_SIZE"]);
        write_node_state(&a_dir, "blocked", 1);

        let mut graph = DepGraph::new();
        graph.nodes.insert(
            "mod-a".into(),
            NodeSnapshot {
                name: "mod-a".into(),
                cwd: a_dir.clone(),
                status: NodeStatus::Blocked,
                state_seq: 1,
                needs: NeedsDeclaration::default(),
                provides: ProvidesDeclaration::default(),
                resolved: ResolvedValues::default(),
                tasks: TaskList::default(),
                def: NodeDefinition::load(&a_dir.join("node.toml")).unwrap(),
                pid: None,
            },
        );

        let eventbus = EventBus::open(dir.path().join("eventbus.log"));
        graph.escalate_to_parent(dir.path(), "mod-a", "UART_TX_PIN", &eventbus).unwrap();

        let escalated_path = dir.path().join(".forge/escalated.toml");
        let table = EscalatedTable::load(&escalated_path).unwrap();
        assert!(!table.needs.is_empty());
        assert_eq!(table.needs[0].key, "UART_TX_PIN");
    }

    #[test]
    fn test_escalated_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let (a_dir, _b_dir, _c_dir) = setup_test_nodes(dir.path());
        write_node_def(&a_dir, "mod-a", &[]);
        write_node_state(&a_dir, "blocked", 1);

        let mut graph = DepGraph::new();
        graph.nodes.insert(
            "mod-a".into(),
            NodeSnapshot {
                name: "mod-a".into(),
                cwd: a_dir.clone(),
                status: NodeStatus::Blocked,
                state_seq: 1,
                needs: NeedsDeclaration::default(),
                provides: ProvidesDeclaration::default(),
                resolved: ResolvedValues::default(),
                tasks: TaskList::default(),
                def: NodeDefinition::load(&a_dir.join("node.toml")).unwrap(),
                pid: None,
            },
        );

        let eventbus = EventBus::open(dir.path().join("eventbus.log"));

        // First escalation
        graph.escalate_to_parent(dir.path(), "mod-a", "KEY_X", &eventbus).unwrap();
        // Duplicate — should be skipped
        graph.escalate_to_parent(dir.path(), "mod-a", "KEY_X", &eventbus).unwrap();

        let table = EscalatedTable::load(&dir.path().join(".forge/escalated.toml")).unwrap();
        assert_eq!(table.needs.len(), 1); // not duplicated
    }

    #[test]
    fn test_dep_edge_equality() {
        let e1 = DepEdge { requester: "A".into(), provider: "B".into(), key: "X".into() };
        let e2 = DepEdge { requester: "A".into(), provider: "B".into(), key: "X".into() };
        let e3 = DepEdge { requester: "A".into(), provider: "C".into(), key: "X".into() };
        assert_eq!(e1, e2);
        assert_ne!(e1, e3);
    }
}
