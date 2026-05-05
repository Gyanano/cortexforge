# CortexForge — 项目级长期记忆

> 这是 Claude Code 自动加载的项目说明，**只放长期稳定的项目级要点**。
> 临时任务进 TaskList；用户偏好/反馈进 `~/.claude/projects/.../memory/`；不要往这里塞。

## 项目定位
- **CortexForge**：MCU 嵌入式项目的通用 Agent 编排环境。
- **平台无关**：不绑定 STM32 / ESP32 / NXP / GD32 等具体厂商。具体工具链假设落在每个模块自己
  的 `<module>/CLAUDE.md` 里。
- **运行时**：Claude Agent SDK + subprocess + 文件状态总线；Claude Code 是开发者 IDE，**不是**运行时。
- **当前阶段**：MVP 核心实现完成（96 tests），可本地构建运行。

## 顶层拓扑（强约束）
- **N 级递归树**：根 Orchestrator (L0) + Domain Agents (L1) + Module Agents (L2+)。
- **深度由 `forge.toml.max_depth` 约束**，每层用同一套 `spawn_child()` 函数（递归对称）。
- **节点不能自己 spawn 子进程**。Domain Agent 通过 `.forge/spawn_requests.toml` 请求 Orchestrator spawn（保证预算/depth/总数检查
  走单点）。
- 嵌入式分层（HAL / BSP / MW / APP / DRV / TEST 等）通过 `node.toml` 的 `role` 字段表达，
  **不**对应额外机制差异。

## `.forge/` 文件总线（标准协议）
每个节点目录下有 `.forge/`；项目根目录也有一份。所有文件用 **TOML**。

| 文件 | 写入方 | 读取方 | 作用 |
|------|--------|--------|------|
| `<n>/.forge/state.toml` | 仅节点自己 | 父节点只读 | 状态机 + 进度 + 心跳 + verify 结果 |
| `<n>/.forge/inbox/*.toml` | Orchestrator / 父 / 兄弟（目录即队列） | 节点自己 | 任务 / 反馈 / kill / value_changed |
| `<n>/.forge/outbox/*.toml` | 节点自己 | Orchestrator 路由 | 汇报 / 回复 / 依赖上报 |
| `forge.toml` | 人写 | Orchestrator + 所有节点 | 全局配置（depth / budget / heartbeat / model） |
| `.forge/eventbus.log` | 仅 Orchestrator（追加写） | Orchestrator / 人 | 项目级唯一事件总线（NDJSON） |

字段 schema 详见 [`docs/01-architecture.md` §4](./docs/01-architecture.md)。

## 状态机（8 态）
`idle → assigned → planning → implementing ↔ blocked → verifying → delivered / dead`

- `blocked` 用于等待外部条件满足（Module: 依赖值；Domain: 子节点完成）；verify 失败回 `implementing` 修代码，达到 `max_retries` 直接 `dead`。
- 每节点本地 `state.toml`，**写入者唯一（自己）**。
- 父对子唯一的写操作：把子标为 `dead`（枯枝清理）。
- 详见 [`docs/01-architecture.md` §3](./docs/01-architecture.md)。

## 心跳 + 坏枝检测（强约束）
- 每节点每 `heartbeat_interval_sec` 秒刷新 `state.toml.last_heartbeat`。
- 父扫子心跳超时 → 标 `dead` → 递归杀子树 → 写 eventbus。
- 进度签名 hash：连续 `stuck_threshold_heartbeats` 次心跳 summary 不变 → `suspected_stuck` 事件。
- 坏枝传播：父决定可降级完成 vs 升级阻塞（按需向上传播，不一刀切）。

## 交付物闸门（强约束）
- 每个模块**必须**在自己根目录提供可执行的 `verify.sh`（或等价 `verify.py` / `verify.mk`）。
- 退出码：`0` 通过；非 `0` 失败。
- 节点自己负责跑 `verify.sh`，通过后写 `state="delivered"`；父只认这个状态。
- 产物落到 `.forge/deliverables/`，附 `artifacts.toml` hash 防 TOCTOU。

## 节点纪律
- **不读不写兄弟节点目录**。共享代码（HAL、公共 inc 等）通过父节点协调或 inbox 请求。
- **不直接 spawn 子进程**。需要新 children 时，写 `spawn_requests.toml` 请求 Orchestrator spawn。
- **Domain Agent 不直接写子节点 tasks.toml**。依赖匹配和 tasks.toml 写入由 Orchestrator 统一负责。
- 每次有意义的进展后**覆盖式**更新 `state.toml`。
- 交付前自己跑一次 `verify.sh`。
- 把模块自己的工具链假设（编译命令、芯片型号、烧录器）写进 `<module>/CLAUDE.md`，不要污染本
  文件。

## 权限模型
- 文件读写：SDK PreToolUse hook，`realpath(target).startswith(cwd)`，否则 deny。
- Bash：`node.toml` 里 `[bash_allowlist]` 显式列；hook 校验命令前缀。
- 网络：默认 deny。
- Spawn：节点本身**没有** subprocess 权限。

## 构建与测试
```bash
cargo build              # 构建 workspace（forge-core + forge-cli + forge-sdk）
cargo test               # 全量测试（96 tests）
cargo run -p forge-cli -- init   # 初始化项目
cargo run -p forge-cli -- validate  # 校验配置
```

## Crate 结构
| Crate | 职责 |
|-------|------|
| `forge-core` | 核心类型、文件协议、状态机、Spawn、心跳、权限、依赖解析引擎、事件总线 |
| `forge-cli` | `forge` 命令行工具（init/validate/run/status/kill/log/node） |
| `forge-sdk` | 节点运行时（NodeRuntime、心跳 watchdog、verify 闸门、Prompt 构建） |

## 文档导航
- [`docs/01-architecture.md`](./docs/01-architecture.md) — 唯一权威架构文档（SDK 树形版，含评估、
  协议、状态机、闸门、权限、真坑清单、MVP 标准）。
- [`docs/02-implementation-status.md`](./docs/02-implementation-status.md) — 实施状态报告
  （14 阶段完成情况、Crate 结构、关键 API 清单、MVP 验收结果、待完成项）。
- [`docs/archive/`](./docs/archive/) — 早期 Teams 版评估报告与可行性深挖（历史参考）。
- 原始脑暴（外部）：`/Users/gyanano/Documents/ObsidianBrain/0-Inbox/MCU嵌入式Claude Code开
  发环境设计【Draft】.md`。

## 文档迭代约定
- 架构性变更**先**改 `docs/01-architecture.md`，再回写本文件的对应小节摘要。
- 本文件总长保持在 ~80 行内；超过就把详情下沉到 `docs/`，本文件留指针。

## 不应写进本文件
- 当前任务、PR 列表、会话进度（→ TaskList）；
- 用户个人偏好与反馈（→ memory 系统）；
- 单次 bug 调试经过（→ 提交信息 / `<module>/CLAUDE.md`）；
- 厂商工具链细节（→ `<module>/CLAUDE.md`）。
