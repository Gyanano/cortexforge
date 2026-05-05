<p align="center">
  <a href="#cortexforge">English</a> &nbsp;|&nbsp;
  <a href="#cortexforge-1">中文</a>
</p>

---

# CortexForge

> N-level recursive agent orchestration for MCU embedded development.

CortexForge is a **platform-agnostic**, file-bus-based agent orchestration environment.
It lets Claude spawn and manage a tree of specialized agent processes — each focused on
a single hardware module (HAL, BSP, driver, middleware, application) — that coordinate
via a pull-based dependency resolution engine.

**Not a Claude Code wrapper.** CortexForge uses Claude Agent SDK + subprocess + a TOML
file state bus. Claude Code is the developer IDE, not the runtime.

## Quick Start

```bash
# 1. Install
git clone https://github.com/Gyanano/cortexforge.git
cd cortexforge
cargo build --release

# 2. Create a project
cargo run -- forge init

# 3. Flesh out with Claude
export ANTHROPIC_API_KEY=sk-...
claude -p "$(cat .forge/FLESH_OUT_PROMPT.md)" --dangerously-skip-permissions

# 4. Validate & run
cargo run -- forge validate
cargo run -- forge run
```

## Commands

| Command | Description |
|---------|-------------|
| `forge init` | Interactive project wizard — select MCU, toolchain, layers, describe modules |
| `forge validate` | Validate forge.toml + all node.toml files |
| `forge run` | Start the orchestrator daemon (10-pass dep resolution + heartbeat monitoring) |
| `forge status` | Tree view of all node states with Unicode status icons |
| `forge node list/show` | Inspect declared nodes |
| `forge log` | Read the NDJSON event bus (`--follow`, `--node`, `--event`, `--since`) |
| `forge kill` | Kill a node or subtree |

## Architecture

```
forge_root/
  forge.toml                   # Global config (§4.6)
  .forge/
    eventbus.log               # NDJSON event log (Orchestrator writes)
    escalated.toml             # Cross-layer dependency routing
  modules/
    firmware/                  # L1 — Domain Agent
      node.toml                # Node definition (§4.1)
      verify.sh                # Verification gate (§8)
      CLAUDE.md                # Module methodology
      .forge/
        state.toml             # State machine + heartbeat (§4.2)
        inbox/                 # Message queue (directory as queue)
      shared/
        needs.toml             # "What I need" — pull-based dependency discovery
        provides.toml          # "What I provide" — key → value + seq version
        resolved.toml          # Orchestrator writes matched values
        tasks.toml             # Orchestrator writes pending tasks
      submodules/
        hal-clock/             # L2 — Module Agent
        bsp-uart/              # L2 — Module Agent
```

### Core mechanisms

| Mechanism | Description |
|-----------|-------------|
| **N-level recursive tree** | Same `spawn_child()` function at every level; `max_depth` in forge.toml |
| **8-state FSM** | `idle → assigned → planning → implementing ↔ blocked → verifying → delivered / dead` |
| **Pull-based deps** | Modules declare needs during development; Orchestrator matches providers, resolves |
| **10-pass dep engine** | Collect → build graph → cycle check ×2 → match → spawn → resolve → propagate → value change → cross-layer |
| **Heartbeat + TTL** | Per-node heartbeat files; stuck detection via SHA256 progress hash; dead branch propagation |
| **verify.sh gate** | Each module self-verifies; parent only trusts `state="delivered"` |
| **TOML file bus** | All inter-node communication via `.forge/` directory protocol — no RPC, no message broker |

## Crate Structure

| Crate | Purpose | Tests |
|-------|---------|-------|
| `forge-core` | Types, protocols, state machine, spawn, heartbeat, dependency engine, event bus, permissions | 82 |
| `forge-cli` | `forge` command-line interface | — |
| `forge-sdk` | Node runtime: watchdog, verify gate, prompt builder | 7 |
| `mvp_tests` | End-to-end MVP criteria validation | 9 |

**Total: 98 tests** | CI: [![CI](https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml/badge.svg)](https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml)

## MVP Status (§13)

- ✅ 3-layer spawn
- ✅ Recursive symmetry (L0→L1 same code as L1→L2)
- ✅ Heartbeat timeout kills branch, siblings unaffected
- ✅ Dependency discovery & resolution
- ✅ Cycle detection (DFS)
- ✅ Dead branch propagation
- ✅ Crash recovery (PID table rebuild)
- ✅ Event bus node lifecycle reconstruction

## Documentation

- [`docs/01-architecture.md`](docs/01-architecture.md) — Authoritative architecture document (1605 lines, ~30 review rounds, 118 tracked fix-points)
- [`docs/02-implementation-status.md`](docs/02-implementation-status.md) — Implementation status, crate structure, key API reference
- [`CLAUDE.md`](CLAUDE.md) — Project-level long-term memory (loaded by Claude Code)

## License

MIT

---

<p align="center">
  <a href="#cortexforge">↑ English</a> &nbsp;|&nbsp;
  <a href="#cortexforge-1">↑ 中文</a>
</p>

---

# CortexForge

> MCU 嵌入式开发的 N 级递归 Agent 编排环境。

CortexForge 是一个**平台无关**、基于文件总线的 Agent 编排环境。
它让 Claude 能够 spawn 并管理一棵由专用 Agent 进程组成的树——每个进程聚焦于
单个硬件模块（HAL、BSP、驱动、中间件、应用）——通过 pull-based 依赖解析引擎协同工作。

**不是 Claude Code 套壳。** CortexForge 使用 Claude Agent SDK + subprocess + TOML
文件状态总线。Claude Code 是开发者 IDE，**不是**运行时。

## 快速开始

```bash
# 1. 安装
git clone https://github.com/Gyanano/cortexforge.git
cd cortexforge
cargo build --release

# 2. 创建项目
cargo run -- forge init

# 3. 用 Claude 完善骨架文件
export ANTHROPIC_API_KEY=sk-...
claude -p "$(cat .forge/FLESH_OUT_PROMPT.md)" --dangerously-skip-permissions

# 4. 校验并运行
cargo run -- forge validate
cargo run -- forge run
```

## 命令一览

| 命令 | 说明 |
|------|------|
| `forge init` | 交互式项目向导——选择 MCU、工具链、分层、描述模块 |
| `forge validate` | 校验 forge.toml + 所有 node.toml 文件 |
| `forge run` | 启动 Orchestrator 守护进程（10-Pass 依赖解析 + 心跳监控） |
| `forge status` | 树形展示所有节点状态（Unicode 状态图标） |
| `forge node list/show` | 查看已声明的节点 |
| `forge log` | 读取 NDJSON 事件总线（`--follow`, `--node`, `--event`, `--since`） |
| `forge kill` | 终止节点或子树 |

## 架构总览

```
forge_root/
  forge.toml                   # 全局配置 (§4.6)
  .forge/
    eventbus.log               # NDJSON 事件日志（Orchestrator 单写）
    escalated.toml             # 跨层依赖路由表
  modules/
    firmware/                  # L1 — Domain Agent
      node.toml                # 节点定义 (§4.1)
      verify.sh                # 验证闸门 (§8)
      CLAUDE.md                # 模块方法论
      .forge/
        state.toml             # 状态机 + 心跳 (§4.2)
        inbox/                 # 消息队列（目录即队列）
      shared/
        needs.toml             # "我需要什么"——pull-based 依赖发现
        provides.toml          # "我能提供什么"——key → 值 + seq 版本
        resolved.toml          # Orchestrator 写入的已匹配值
        tasks.toml             # Orchestrator 写入的待处理任务
      submodules/
        hal-clock/             # L2 — Module Agent
        bsp-uart/              # L2 — Module Agent
```

### 核心机制

| 机制 | 说明 |
|------|------|
| **N 级递归树** | 每层使用同一 `spawn_child()` 函数；深度由 `forge.toml.max_depth` 约束 |
| **8 态状态机** | `idle → assigned → planning → implementing ↔ blocked → verifying → delivered / dead` |
| **Pull-based 依赖** | 模块开发过程中动态声明需求；Orchestrator 匹配 provider，自动解析 |
| **10-Pass 依赖引擎** | 收集 → 建图 → 两次环检测 → 匹配 → spawn → 解析 → 传播 → 值变更 → 跨层 |
| **心跳 + TTL** | 每节点心跳文件；SHA256 进度 hash 卡死检测；坏枝传播 |
| **verify.sh 闸门** | 每个模块自验；父只认 `state="delivered"` |
| **TOML 文件总线** | 所有跨节点通信走 `.forge/` 目录协议——无 RPC、无消息队列 |

## Crate 结构

| Crate | 用途 | 测试数 |
|-------|------|--------|
| `forge-core` | 类型、协议、状态机、Spawn、心跳、依赖引擎、事件总线、权限 | 82 |
| `forge-cli` | `forge` 命令行工具 | — |
| `forge-sdk` | 节点运行时：watchdog、verify 闸门、Prompt 构建 | 7 |
| `mvp_tests` | 端到端 MVP 标准验证 | 9 |

**总计 98 项测试** | CI: [![CI](https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml/badge.svg)](https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml)

## MVP 验收结果 (§13)

- ✅ 3 层 spawn 跑通
- ✅ 递归对称（L0→L1 与 L1→L2 走同一段代码）
- ✅ 心跳超时杀枝，兄弟节点不受影响
- ✅ 依赖发现与解决
- ✅ 循环依赖检测（DFS）
- ✅ 坏枝传播
- ✅ 崩溃恢复（PID 表重建）
- ✅ 事件总线可重建任意节点完整生命周期

## 文档

- [`docs/01-architecture.md`](docs/01-architecture.md) — 唯一权威架构文档（1605 行，~30 轮评审，118 个修复点）
- [`docs/02-implementation-status.md`](docs/02-implementation-status.md) — 实施状态报告、Crate 结构、关键 API 参考
- [`CLAUDE.md`](CLAUDE.md) — 项目级长期记忆（Claude Code 自动加载）

## 许可证

MIT

---

<p align="center">
  <a href="#cortexforge">↑ English</a> &nbsp;|&nbsp;
  <a href="#cortexforge-1">↑ 中文</a>
</p>

---

## Star History

<p align="center">
  <a href="https://www.star-history.com/#Gyanano/cortexforge&Date">
    <img src="https://api.star-history.com/svg?repos=Gyanano/cortexforge&type=Date" alt="Star History Chart" width="600" />
  </a>
</p>
