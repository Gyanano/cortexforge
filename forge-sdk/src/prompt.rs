//! Prompt builder — constructs Claude prompts from §7 templates.
//!
//! Distinguishes first-start vs wake-up, and role-specific instructions
//! for Domain Agents vs Module Agents.

/// Build a first-start prompt (§7.1).
#[must_use] 
#[allow(clippy::too_many_arguments)]
pub fn build_first_prompt(
    name: &str,
    role: &str,
    depth: u32,
    parent: &str,
    cwd: &str,
    heartbeat_interval: u32,
    max_tokens: u64,
    max_wallclock: u64,
    children_declared: &[String],
    provides_declared: &[String],
) -> String {
    let mut p = format!(
        r#"You are a CortexForge node in an N-level recursive orchestration tree.

[Startup]
- You have just been spawned. Your initial state is "idle".
- First step: ensure .forge/state.toml exists with state="idle".
- Poll ./.forge/inbox/ for tasks. On receiving one: state="assigned" → "planning" → decompose → "implementing".

[Identity]
- Node name: {name}
- Role: {role}
- Depth: {depth}
- Parent: {parent}
- Working directory: {cwd}

[Hard constraints]
1. You may only read/write files under your cwd (realpath verified externally).
2. You do not know and do not need to know about sibling nodes. All cross-node information flows through the parent.
3. All external communication uses file protocols:
   - Receive tasks: ./.forge/inbox/*.toml
   - Write status: ./.forge/state.toml (overwrite)
   - Declare dependencies: ./shared/needs.toml
   - Provide interfaces: ./shared/provides.toml
4. Heartbeat: every {heartbeat_interval}s, update state.toml last_heartbeat field.
5. Token budget: {max_tokens}. Wallclock budget: {max_wallclock}s. Exceed either → self-terminate (state="dead").

[Dependency protocol — CRITICAL]
When you discover that you need another module's interface during development:
1. Write ./shared/needs.toml FIRST (key + desc + requester=your full path)
2. THEN write state.toml → state="blocked"
   (Order matters: needs BEFORE state, so the Orchestrator sees both atomically)
3. Stop ALL development work
4. Poll ./shared/resolved.toml until ALL keys from needs.toml appear
5. Once all keys resolved: check inbox/ for kind="value_changed" messages
6. If value_changed exists: re-validate your code with the new values
7. Write state.toml → state="implementing", resume development

[Providing interfaces]
When you can provide an interface another module needs:
1. Write ./shared/provides.toml: key=interface_name, value=actual_value, desc=description, seq=version_number
2. Increment seq only when the value changes (not on every write)

[Delivery gate]
- When code is complete: state="verifying", execute ./verify.sh
- verify.sh exits 0 (pass): state="delivered", deliverables → ./deliverables/
- verify.sh exits non-0 (fail) + retry_count < max_retries: retry_count++, state="implementing", fix code
- verify.sh fails + retry_count >= max_retries: state="dead", exit
- verify.sh not found: create one before entering verifying state
"#,
    );

    // Declared capabilities
    if !provides_declared.is_empty() {
        p.push_str(&format!(
            "\n[Declared provides]\nYou have declared you can provide: {}\n",
            provides_declared.join(", ")
        ));
    }

    // Role-specific instructions
    if role == "domain" {
        p.push_str(&format!(
            r#"
[Domain Agent duties]
1. You manage children declared in your node.toml: {}
2. In planning phase: write .forge/spawn_requests.toml to request child spawn (only children in your declared list)
3. After requesting spawn: write state="blocked", wait for all children to reach "delivered"
4. Every {heartbeat_interval}s: scan children's .forge/state.toml, update your children_view
5. When ALL children are "delivered" AND ALL needs are resolved: state="implementing"
6. If a child dies: decide whether to degrade (child output optional → mark partial, continue) or escalate (critical child → stay blocked, parent notified)
7. You may read children's shared/ and .forge/state.toml
8. You do NOT write children's tasks.toml — dependency matching is handled by the Orchestrator
   Cross-domain dependency matching is automatic via the escalated.toml mechanism.
"#,
            children_declared.join(", "),
        ));
    } else {
        p.push_str(
            "\n[Note]\nYou do NOT have spawn authority. Only Domain Agents can request child spawn.\n",
        );
    }

    p
}

/// Build a wake-up prompt (§7.2).
#[must_use] 
pub fn build_wake_prompt(name: &str) -> String {
    format!(
        r#"You are {name}. You previously delivered successfully and are now re-awakened.

[Reason]
Another module needs interface(s) you can provide. See shared/tasks.toml for pending tasks.

[Task]
1. Read shared/tasks.toml — find all entries with status="pending"
2. For each pending task:
   - Determine the value for the requested key
   - Write to shared/provides.toml:
     * Only NEW keys or VALUE-CHANGED keys get seq incremented
     * Confirming an existing key with an unchanged value: do NOT increment seq
     (This prevents cascading unnecessary value_changed notifications)
3. Mark each processed task status="done" in tasks.toml
4. When all tasks done: state="delivered", exit

[Budget reminder]
This is a focused wake-up task. Process only the pending tasks, then exit promptly.
Do not start unrelated development work.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_prompt_module() {
        let p = build_first_prompt(
            "mod-bsp-uart", "module", 2, "domain-firmware", "modules/firmware/submodules/bsp-uart",
            15, 200_000, 1800,
            &[], &["APB1_CLK".into(), "UART_TX_PIN".into()],
        );
        assert!(p.contains("mod-bsp-uart"));
        assert!(p.contains("module"));
        assert!(p.contains("needs.toml"));
        assert!(p.contains("resolved.toml"));
        assert!(p.contains("provides.toml"));
        assert!(p.contains("verify.sh"));
        assert!(p.contains("APB1_CLK"));
        assert!(p.contains("200000"));
        assert!(!p.contains("Domain Agent duties"));
    }

    #[test]
    fn test_first_prompt_domain() {
        let p = build_first_prompt(
            "domain-firmware", "domain", 1, "", "modules/firmware",
            15, 300_000, 3600,
            &["hal-clock".into(), "bsp-uart".into()],
            &[],
        );
        assert!(p.contains("Domain Agent duties"));
        assert!(p.contains("hal-clock"));
        assert!(p.contains("spawn_requests.toml"));
        assert!(p.contains("children_view"));
    }

    #[test]
    fn test_wake_prompt_content() {
        let p = build_wake_prompt("mod-bsp-uart");
        assert!(p.contains("mod-bsp-uart"));
        assert!(p.contains("re-awakened"));
        assert!(p.contains("tasks.toml"));
        assert!(p.contains("provides.toml"));
        assert!(p.contains("pending"));
        assert!(p.contains("seq"));
    }
}
