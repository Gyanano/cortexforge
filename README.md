<p align="center">
  <a href="https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml"><img src="https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/Gyanano/cortexforge/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="License" /></a>
  <a href="https://github.com/Gyanano/cortexforge"><img src="https://img.shields.io/github/stars/Gyanano/cortexforge?style=social" alt="Stars" /></a>
  &nbsp;<a href="README_zh.md">中文</a>
</p>

# CortexForge

> N-level recursive agent orchestration for MCU embedded development.

CortexForge is a **platform-agnostic**, file-bus-based agent orchestration environment. It lets Claude spawn and manage a tree of specialized agent processes — each focused on a single hardware module — that coordinate through a pull-based dependency resolution engine.

**Not a Claude Code wrapper.** CortexForge uses Claude Agent SDK + subprocess + a TOML file state bus. Claude Code is the developer IDE, not the runtime.

## Quick Start

```bash
git clone https://github.com/Gyanano/cortexforge.git && cd cortexforge
cargo build --release

# Interactive project wizard — select MCU, toolchain, layers, describe modules
cargo run -- init

# Have an LLM flesh out the skeleton files
# Option A: DeepSeek API (cheapest, ~$0.01 per project)
export DEEPSEEK_API_KEY=sk-...
curl -s https://api.deepseek.com/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $DEEPSEEK_API_KEY" \
  -d "$(jq -n --arg prompt "$(cat .forge/FLESH_OUT_PROMPT.md)" \
    '{model:"deepseek-chat",messages:[{role:"user",content:$prompt}]}')"

# Option B: Claude CLI (if installed and logged in via `claude login`)
claude -p "$(cat .forge/FLESH_OUT_PROMPT.md)" --dangerously-skip-permissions

# Validate & run the orchestrator
cargo run -- validate
cargo run -- run
```

## Architecture

```
forge_root/
  forge.toml                     # Global config (max_depth, budget, heartbeat)
  .forge/
    eventbus.log                 # NDJSON event log — sole writer: Orchestrator
    escalated.toml               # Cross-layer dependency routing table
  modules/
    firmware/                    # L1 Domain Agent — manages sub-modules
      node.toml                  # Static definition: name, role, provides, children
      verify.sh                  # Self-verification gate (exit 0 = pass)
      CLAUDE.md                  # Module methodology & toolchain assumptions
      .forge/state.toml          # Dynamic state: 8-state FSM, heartbeat, progress
      .forge/inbox/              # Message queue — directory as queue, concurrent-safe
      shared/needs.toml          # "What I need" — discovered during development
      shared/provides.toml       # "What I provide" — key → value + seq version
      shared/resolved.toml       # Orchestrator writes matched dependency values
      shared/tasks.toml          # Orchestrator writes pending provider tasks
      submodules/
        hal-clock/               # L2 Module Agent — provides APB1_CLK, APB2_CLK
        bsp-uart/                # L2 Module Agent — provides UART_TX_PIN
```

### Core Mechanisms

| Mechanism | Description |
|-----------|-------------|
| **N-level recursive tree** | Same `spawn_child()` at every level; depth constrained by `forge.toml.max_depth` |
| **8-state FSM** | `idle → assigned → planning → implementing ↔ blocked → verifying → delivered / dead` |
| **Pull-based dependencies** | Modules declare needs at dev time; Orchestrator matches providers, resolves values |
| **10-pass dependency engine** | Collect → build graph → cycle check×2 → match → spawn → resolve → propagate → value change → cross-layer |
| **Heartbeat + TTL** | Per-node heartbeat files; SHA256 progress-hash stuck detection; dead branch propagation |
| **verify.sh gate** | Every module self-verifies; parent only trusts `state="delivered"` |
| **TOML file bus** | All cross-node communication via `.forge/` directory protocol — zero RPC, zero brokers |
| **Permissions & isolation** | Per-node `realpath` file boundary; Bash allowlist; network control; spawn authority restricted |

## CLI Commands

| Command | Description |
|---------|-------------|
| `forge init` | Interactive wizard: MCU, toolchain, layers, module descriptions → skeleton project |
| `forge validate` | Validate `forge.toml` + all `node.toml` files for syntax & semantics |
| `forge run` | Start the orchestrator daemon: 10-pass dep resolution + heartbeat monitoring loop |
| `forge status` | Tree view of node states with Unicode icons (`○ ◕ ✅ ❌`) + `--json` |
| `forge node list` | List all declared nodes |
| `forge node show <name>` | Show node details: role, depth, children, provides, runtime state |
| `forge log` | Read event bus: `--node`, `--event`, `--since`, `--follow` |
| `forge kill <node>` | Kill a node or subtree: `--force` (skip grace), `--cascade` (recursive) |

## Crate Structure

| Crate | Lines | Tests | Purpose |
|-------|-------|-------|---------|
| `forge-core` | ~7000 | 82 | Types, 10 TOML protocols, FSM, spawn, heartbeat, 10-pass dep engine, event bus, permissions, deliverables |
| `forge-cli` | ~800 | — | `forge` CLI with 7 subcommands + interactive init wizard |
| `forge-sdk` | ~700 | 7 | Node runtime: watchdog thread, verify gate, prompt builder |
| `mvp_tests` | ~480 | 9 | End-to-end MVP criteria validation |
| **Total** | **~9000** | **98** | |

## MVP Verification (§13)

| # | Criterion | Test |
|---|-----------|------|
| 1 | 3-layer spawn works | `mvp1_three_layer_spawn` |
| 2 | Recursive symmetry (L0→L1 same code as L1→L2) | `mvp2_recursive_symmetry` |
| 3 | Heartbeat timeout kills branch, siblings unaffected | `mvp3_heartbeat_timeout_detection` |
| 4 | Dependency discovery & resolution | `mvp4_dependency_discovery_and_resolution` |
| 5 | Cycle detection (DFS) | `mvp5_cycle_detection` |
| 6 | Dead branch propagation | `mvp6_dead_branch_propagation` |
| 7 | Crash recovery (PID table rebuild) | `mvp7_crash_recovery_pid_rebuild` |
| 8 | Event bus lifecycle reconstruction | `mvp8_event_bus_reconstruction` |

## Documentation

| Document | Description |
|----------|-------------|
| [`docs/01-architecture.md`](docs/01-architecture.md) | Authoritative architecture — 1605 lines, ~30 review rounds, 118 tracked fix-points |
| [`docs/02-implementation-status.md`](docs/02-implementation-status.md) | Implementation status, full crate structure, key API reference |
| [`CLAUDE.md`](CLAUDE.md) | Project-level long-term memory (auto-loaded by Claude Code) |

## Requirements

- **Rust** 1.85+ (edition 2024)
- **Claude CLI** — the `claude` command must be installed and logged in (`claude login`). The orchestrator spawns `claude -p` child processes as agent runtimes. No API key needed — authentication is handled by Claude CLI's local session.
- **LLM API key (optional)** — only needed for the one-time flesh-out step during `forge init`. DeepSeek API key is recommended for cost (~$0.01/project). Any OpenAI-compatible API works.
- **macOS or Linux** — process management uses POSIX signals

## License

MIT

---

## Star History

<p align="center">
  <a href="https://www.star-history.com/#Gyanano/cortexforge&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=Gyanano/cortexforge&type=Date&theme=dark" />
      <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=Gyanano/cortexforge&type=Date" />
      <img src="https://api.star-history.com/svg?repos=Gyanano/cortexforge&type=Date" alt="Star History Chart" width="600" />
    </picture>
  </a>
</p>
