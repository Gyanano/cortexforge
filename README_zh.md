<p align="center">
  <a href="https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml"><img src="https://github.com/Gyanano/cortexforge/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/Gyanano/cortexforge/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="License" /></a>
  <a href="https://github.com/Gyanano/cortexforge"><img src="https://img.shields.io/github/stars/Gyanano/cortexforge?style=social" alt="Stars" /></a>
  &nbsp;<a href="README.md">English</a>
</p>

# CortexForge

> 面向 MCU 嵌入式开发的 N 级递归 Agent 编排环境。

CortexForge 是一个**平台无关**、基于文件总线的 Agent 编排环境。它让 Claude 能够 spawn 并管理一棵由专用 Agent 进程组成的树——每个进程聚焦于单个硬件模块——通过 Pull-based 依赖解析引擎协同工作。

**不是 Claude Code 套壳。** CortexForge 使用 Claude Agent SDK + subprocess + TOML 文件状态总线。Claude Code 是开发者 IDE，**不是**运行时。

## 快速开始

```bash
git clone https://github.com/Gyanano/cortexforge.git && cd cortexforge
cargo build --release

# 交互式项目向导 —— 选择 MCU、工具链、分层、描述模块
cargo run -- init

# 使用 LLM 完善骨架文件
# 方案 A：DeepSeek API（推荐，成本最低 ~¥0.07/项目）
export DEEPSEEK_API_KEY=sk-...
curl -s https://api.deepseek.com/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $DEEPSEEK_API_KEY" \
  -d "$(jq -n --arg prompt "$(cat .forge/FLESH_OUT_PROMPT.md)" \
    '{model:"deepseek-chat",messages:[{role:"user",content:$prompt}]}')"

# 方案 B：Claude CLI（已安装并通过 claude login 登录）
claude -p "$(cat .forge/FLESH_OUT_PROMPT.md)" --dangerously-skip-permissions

# 校验配置并启动 Orchestrator
cargo run -- validate
cargo run -- run
```

## 架构概览

```
forge_root/
  forge.toml                     # 全局配置（max_depth、budget、heartbeat）
  .forge/
    eventbus.log                 # NDJSON 事件日志 —— 唯一写入者：Orchestrator
    escalated.toml               # 跨层依赖路由表
  modules/
    firmware/                    # L1 Domain Agent —— 管理子模块
      node.toml                  # 静态定义：名称、角色、provides、children
      verify.sh                  # 自验闸门（退出 0 = 通过）
      CLAUDE.md                  # 模块方法论 & 工具链假设
      .forge/state.toml          # 动态状态：8 态状态机、心跳、进度
      .forge/inbox/              # 消息队列 —— 目录即队列，并发安全
      shared/needs.toml          # "我需要什么" —— 开发过程中动态发现
      shared/provides.toml       # "我能提供什么" —— key → 值 + seq 版本号
      shared/resolved.toml       # Orchestrator 写入已匹配的依赖值
      shared/tasks.toml          # Orchestrator 写入待处理的 provider 任务
      submodules/
        hal-clock/               # L2 Module Agent —— 提供 APB1_CLK、APB2_CLK
        bsp-uart/                # L2 Module Agent —— 提供 UART_TX_PIN
```

### 核心机制

| 机制 | 说明 |
|------|------|
| **N 级递归树** | 每层使用同一 `spawn_child()` 函数；深度由 `forge.toml.max_depth` 约束 |
| **8 态状态机** | `idle → assigned → planning → implementing ↔ blocked → verifying → delivered / dead` |
| **Pull-based 依赖** | 模块开发中动态声明需求；Orchestrator 自动匹配 provider、解析值 |
| **10-Pass 依赖引擎** | 收集 → 建图 → 两次环检测 → 匹配 → spawn → 解析 → 传播 → 值变更 → 跨层上报 |
| **心跳 + TTL** | 每节点心跳文件；SHA256 进度 hash 卡死检测；坏枝传播 |
| **verify.sh 闸门** | 每模块自验，父只认 `state="delivered"` |
| **TOML 文件总线** | 全部跨节点通信走 `.forge/` 目录协议 —— 零 RPC、零消息队列 |
| **权限与隔离** | 每节点 `realpath` 文件边界；Bash 白名单；网络控制；Spawn 权限收束 |

## CLI 命令

| 命令 | 说明 |
|------|------|
| `forge init` | 交互式向导：选 MCU、工具链、分层、写模块描述 → 生成骨架项目 |
| `forge validate` | 校验 `forge.toml` + 所有 `node.toml` 的语法与语义 |
| `forge run` | 启动 Orchestrator 守护进程：10-Pass 依赖解析 + 心跳监控循环 |
| `forge status` | 树形展示节点状态（Unicode 图标 `○ ◕ ✅ ❌`）+ `--json` |
| `forge node list` | 列出所有已声明节点 |
| `forge node show <name>` | 查看节点详情：角色、深度、子节点、provides、运行时状态 |
| `forge log` | 读取事件总线：`--node`、`--event`、`--since`、`--follow` |
| `forge kill <node>` | 终止节点或子树：`--force`（跳过宽限期）、`--cascade`（递归） |

## Crate 结构

| Crate | 代码行数 | 测试数 | 用途 |
|-------|---------|--------|------|
| `forge-core` | ~7000 | 82 | 类型、10 种 TOML 协议、状态机、Spawn、心跳、10-Pass 依赖引擎、事件总线、权限、交付物 |
| `forge-cli` | ~800 | — | `forge` 命令行：7 个子命令 + 交互式 init 向导 |
| `forge-sdk` | ~700 | 7 | 节点运行时：watchdog 线程、verify 闸门、Prompt 构建 |
| `mvp_tests` | ~480 | 9 | 端到端 MVP 验收标准验证 |
| **合计** | **~9000** | **98** | |

## MVP 验收结果 (§13)

| # | 标准 | 测试函数 |
|---|------|---------|
| 1 | 3 层 spawn 跑通 | `mvp1_three_layer_spawn` |
| 2 | 递归对称（L0→L1 与 L1→L2 同一代码） | `mvp2_recursive_symmetry` |
| 3 | 心跳超时杀枝，兄弟不受影响 | `mvp3_heartbeat_timeout_detection` |
| 4 | 依赖发现与解析 | `mvp4_dependency_discovery_and_resolution` |
| 5 | 循环依赖检测（DFS） | `mvp5_cycle_detection` |
| 6 | 坏枝传播 | `mvp6_dead_branch_propagation` |
| 7 | 崩溃恢复（PID 表重建） | `mvp7_crash_recovery_pid_rebuild` |
| 8 | 事件总线可重建完整生命周期 | `mvp8_event_bus_reconstruction` |

## 文档导航

| 文档 | 说明 |
|------|------|
| [`docs/01-architecture.md`](docs/01-architecture.md) | 唯一权威架构文档——1605 行，~30 轮评审，118 个追踪修复点 |
| [`docs/02-implementation-status.md`](docs/02-implementation-status.md) | 实施状态报告、完整 Crate 结构、关键 API 参考 |
| [`CLAUDE.md`](CLAUDE.md) | 项目级长期记忆（Claude Code 自动加载） |

## 运行要求

- **Rust** 1.85+（edition 2024）
- **Claude CLI**——必须安装 `claude` 命令并通过 `claude login` 登录。Orchestrator 会 spawn `claude -p` 子进程作为 Agent 运行时，无需额外 API Key，认证由 Claude CLI 本地会话完成。
- **LLM API Key（可选）**——仅 `forge init` 后的一次性骨架完善步骤需要。推荐 DeepSeek API Key（约 ¥0.07/项目），任何兼容 OpenAI 接口的 API 均可。
- **macOS 或 Linux**——进程管理使用 POSIX 信号

## 许可证

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
