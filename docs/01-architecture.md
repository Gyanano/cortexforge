# CortexForge 架构设计（SDK 树形版）

> 本文是 CortexForge 的**唯一权威架构文档**，合并了早期评估报告与可行性深挖的结论。
> 范围：平台无关（不绑定 STM32 / ESP32 / NXP / GD32 等具体厂商），仅约定**编排层**。
> 立场：Claude Agent SDK + subprocess + 文件状态总线；Claude Code 是开发者 IDE，**不是**运行时。

---

## 0. 总览

```
                     ┌───────────────────────────────┐
                     │   Orchestrator (L0, 长驻)      │
                     │   forge.toml / eventbus.log   │
                     └──────────┬────────────────────┘
                                │ subprocess (spawn_child)
               ┌────────────────┼────────────────┐
               ▼                                 ▼
      ┌────────────────┐                 ┌────────────────┐
      │ Domain (L1)    │                 │ Domain (L1)    │
      │ firmware/      │                 │ tools/         │
      └───────┬────────┘                 └────────────────┘
              │ subprocess (同一套 spawn_child)
     ┌────────┼────────┐
     ▼                 ▼
┌──────────┐    ┌──────────┐
│ Module   │    │ Module   │
│ hal-clock│    │ bsp-uart │
│ (L2)     │    │ (L2)     │
└──────────┘    └──────────┘
```

**核心机制**：
- **N 级递归树**：通过 `forge.toml` 的 `max_depth` 约束层级，每层用同一套 `spawn_child()` 函数
- **文件状态总线**：每节点本地 `state.toml` + `inbox/` 目录即队列 + `shared/` 依赖协议，项目级 `eventbus.log`
- **Pull-based 依赖发现**：模块在开发过程中动态声明依赖（`needs.toml`），Orchestrator 匹配 provider 并路由
- **心跳 + TTL + 坏枝检测**：防"一根烂枝拖死全树"
- **`verify.sh` 自验闸门**：节点自己负责验证，父只认 `state="delivered"`

**vs 早期 Teams 方案的关键差异**：

| 维度 | 早期方案（已废弃） | 当前方案 |
|------|-------------------|---------|
| 拓扑 | 两级 Teams（Lead + Teammates） | N 级递归树（`max_depth` 可配） |
| 运行时 | Claude Code 内部 Teams 功能 | Claude Agent SDK + subprocess |
| 通信 | `.coordination/inbox.toml` 单文件 | `.forge/inbox/` 目录即队列（并发安全） |
| 依赖 | 静态 `depends_on` 声明 | 动态 pull-based（`needs.toml` + `provides.toml`） |
| 验证 | 项目级 `TaskCompleted` hook 调 `verify.sh` | 节点自验，父只认 `state="delivered"` |
| 隔离 | Claude Code `PreToolUse` 路径白名单 | SDK 级 `realpath` + cwd 锚点 |

---

## 1. 原始脑暴评估（为什么这么改）

> 输入：`/Users/gyanano/Documents/ObsidianBrain/0-Inbox/MCU嵌入式Claude Code开发环境设计【Draft】.md`

### 1.1 脑暴 10 条原文

| # | 原文摘要 | 判定 | 落地方式 |
|---|---------|------|---------|
| B1 | 项目按子模块拆分，可分层；每个文件夹一个 Agent | **采纳** | N 级树形拓扑，每节点 cwd = 对应目录 |
| B2 | 根 Agent 负责整体编译、版本管理、子模块协调与监督 | **采纳** | Orchestrator (L0) 角色（纯代码守护进程） |
| B3 | 分层文件夹的 Agent 负责本层职责说明、子模块开发监督 | **采纳** | Domain Agent (L1) 角色 |
| B4 | 子模块 Agent 负责该模块的设计、开发、管理、维护 | **采纳** | Module Agent (L2+) 角色 |
| B5 | 子模块只能访问本目录；通过省 token 文件沟通 | **采纳** | SDK `realpath` 隔离 + `.forge/inbox/` + `shared/` |
| B6 | 每文件夹有自己的 CLAUDE.md 记录长期方法论 | **采纳** | 每节点 cwd 下 `CLAUDE.md` 自动加载 |
| B7 | 父级只读对话文件做高价值交换 | **采纳** | 父只扫子 `state.toml`，不读子源码 |
| B8 | 根目录省 token 配置文件汇总所有子模块状态 | **采纳** | 项目级 `eventbus.log` + 各节点 `state.toml` |
| B9 | 每子 Agent 自己的 hooks + 验收流程 | **采纳** | 每节点 `verify.sh` + SDK per-invocation hooks |
| B10 | 树状管理：任务发散，交付物收敛 | **采纳** | 状态机 + pull-based 依赖发现 + 集成顺序 |

**结论**：原始脑暴的 10 条**全部保留**，核心思想完整映射到 SDK 树形架构。

### 1.2 早期评估中已废弃的建议

- ❌ "压扁为两级拓扑" → 现在是 N 级，由 `forge.toml.max_depth` 控制
- ❌ "用 `TeamCreate` / `SendMessage` / `TeammateIdle`" → 现在用 subprocess + 文件状态总线
- ❌ "静态 `depends_on` 声明" → 现在是动态 pull-based 依赖发现
- ❌ "`.claude/agents/*.md` 承载 per-agent 记忆" → 现在每节点 cwd 下 `CLAUDE.md` + `node.toml`

---

## 2. 拓扑

### 2.1 递归树结构

```
forge_root/                        # L0 — Orchestrator（长驻，纯代码守护进程）
  forge.toml                       #   全局配置
  .forge/
    eventbus.log                   #   项目级唯一事件总线（NDJSON 追加）
    escalated.toml                 #   跨层上报的依赖路由表（持久化）
    # 注：pids 表为 Orchestrator 内存索引，不持久化；重启时从各节点 .forge/pid 重建
  modules/
    firmware/                      # L1 — Domain Agent
      node.toml                    #   节点定义
      verify.sh                    #   集成验证
      .forge/                      #   节点本地状态
        state.toml
        spawn_requests.toml        #   Domain Agent 请求 spawn 子节点
        inbox/
      shared/                      #   依赖协议公共目录
        needs.toml                 #     "我需要什么"
        resolved.toml              #     "别人给我了什么"
        provides.toml              #     "我能提供什么"
        tasks.toml                 #     "Orchestrator 给我的任务"
      submodules/
        hal-clock/                 # L2 — Module Agent
          node.toml
          verify.sh
          .forge/
            state.toml
            inbox/
          shared/
            needs.toml
            resolved.toml
            provides.toml
            tasks.toml
        bsp-uart/                  # L2
          ...
```

**关键设计原则**：
- 节点不知道自己是"第几层"，只知道自己是否有 children（看 `node.toml` 里有没有声明 `children` 列表）。这样**机制可以无差别复用到任意深度**。
- Orchestrator (L0) 是**纯代码守护进程**，不调 LLM。智能决策由 L1/L2 节点完成。

### 2.2 角色定义

每个节点都是一个独立 Claude 进程（`claude -p` 子进程）。Orchestrator 是纯代码 Rust 程序。

| 层级 | 角色 | 运行方式 | 关键职责 |
|------|------|----------|----------|
| L0 | Orchestrator | **纯代码**（Rust，长驻） | 加载 `forge.toml`，spawn 节点，周期巡检 `shared/`，依赖匹配与路由，心跳监控，坏枝处理，消息路由 |
| L1 | Domain Agent | `claude -p`（任务期间） | 把 domain 级任务拆解，管理子模块依赖协调，聚合 L2 的状态/交付物 |
| L2 | Module Agent | `claude -p`（任务期间） | 实现该模块，发现依赖时写 `needs.toml`，跑 `verify.sh` |
| L≥3 | Sub-module Agent | `claude -p`（任务期间） | 同 L2 机制，直到 `max_depth` |

### 2.3 每节点的读写边界

| 维度 | Orchestrator (L0) | Domain (L1) | Module (L2+) |
|------|-------------------|-------------|--------------|
| **读** | 所有节点 `state.toml`、`shared/`、`eventbus.log` | 本域全部文件 + 子节点 `shared/` 和 `.forge/state.toml` | 本模块全部文件 |
| **写** | 所有节点 `shared/resolved.toml`、`shared/tasks.toml`、`eventbus.log` | 本域文件 + `.forge/spawn_requests.toml`（请求 Orchestrator spawn） | 本模块文件 + 自己的 `shared/needs.toml`、`shared/provides.toml` |
| **不读** | 不读模块源码（除非 inbox attach） | 兄弟域目录 | 兄弟模块目录 |
| **spawn** | 可以（唯一有 subprocess 权限的角色） | 通过 `.forge/spawn_requests.toml` 请求 Orchestrator spawn | 同 L1 |

### 2.4 记忆载体

| 载体 | 作用 | 谁写 |
|------|------|------|
| `<node>/CLAUDE.md` | 模块长期方法论、工具链假设、注意事项 | 节点自己 |
| `<node>/node.toml` | 节点静态定义（角色、children、provides 声明、budget、model） | 人写 |
| `<node>/.forge/state.toml` | 节点动态状态（状态机、进度、心跳、verify 结果） | 节点自己 |
| `<node>/shared/needs.toml` | 运行时发现的依赖请求 | 节点自己 |
| `<node>/shared/provides.toml` | 运行时产出的接口值（带 seq 版本） | 节点自己 |
| `<node>/shared/resolved.toml` | Orchestrator 写入的已解决依赖值 | Orchestrator |
| `<node>/shared/tasks.toml` | Orchestrator 写入的待处理依赖任务 | Orchestrator |
| `forge.toml` | 全局配置 | 人写 |
| `eventbus.log` | 全项目事件追加（不可变） | 仅 Orchestrator（从 state.toml 变化和自身动作推导事件，统一写入） |

---

## 3. 状态机

每个节点本地有且只有一份 `state.toml`，代表自己当前状态。**写入者唯一（自己）**；父节点只读，不写。

```
        ┌──────┐  parent assigns task   ┌──────────┐
        │ idle ├──────────────────────► │ assigned │
        └──┬───┘                        └────┬─────┘
           │ TTL expired                     │ self-plan
           │                                 ▼
           │                            ┌──────────┐
           │                            │ planning │
           │                            └────┬─────┘
           │                                 │ plan ready
           │                                 ▼
           │                            ┌──────────────┐    发现依赖    ┌─────────┐
           │                            │ implementing ├──────────────►│ blocked │
           │                            └──┬───────┬───┘               └──┬──┬──┘
           │                  ┌───────────┘       │         deps resolved / │  │
           │     self-done +  │         verify     │       children delivered│  │
           │   all deps resolved        fail       │                       │  │
           │                  ▼          (retry)┌──────────┐◄──────────────┘  │
           │                            ┌──────►│ verifying│                  │
           │                            │       └──┬───────┘                  │
           │                            │  verify  │  verify fail +           │
           │                            │  pass    │  max_retries             │
           │                            │          ▼                          │
           │  ┌──────┐ wallclock        │  ┌──────────┐  ┌──────┐            │
           │  │ dead │◄─ timeout ───────┼──│delivered │  │ dead │◄───────────┘
           │  └──────┘ (implementing    │  └──────────┘  └──────┘  wallclock
           │           / blocked)       │                    timeout (blocked)
           └────────────────────────────┘
```

### 3.1 状态语义

| 状态 | 含义 | 谁可触发进入 |
|------|------|-------------|
| `idle` | 节点已启动，等任务 | 自身（boot） |
| `assigned` | 收到 inbox 任务，尚未开始 | 自身（读到 inbox 新消息） |
| `planning` | 在分解任务 / 决定要不要 spawn 子节点 | 自身 |
| `implementing` | 在干活。发现依赖时→写 `needs.toml`→立即转 `blocked` | 自身 |
| `blocked` | 等待外部条件满足（Module: 依赖值；Domain: 子节点完成） | 自身（依赖发现 / 子节点未完成） |
| `verifying` | 在跑 `verify.sh` | 自身 |
| `delivered` | 交付通过，产物在 `deliverables/` | 自身（verify pass 后） |
| `dead` | 不可恢复（超时无心跳 / 反复失败 / 被父杀 / 循环依赖） | 自身**或**父节点 |

### 3.2 状态转换规则（完整）

**idle → assigned**（收到任务）：
- 触发条件：节点从 inbox 读到新任务消息
- 动作：写 `state="assigned"`

**idle → dead**（TTL 超时）：
- 触发条件：节点启动后在 `heartbeat_timeout_sec` 内未收到任何任务
- 动作：写 `state="dead"`，exit

**assigned → planning**（开始规划）：
- 触发条件：节点开始分解任务
- 动作：写 `state="planning"`

**planning → implementing**（计划完成）：
- 触发条件：任务分解完成，决定好是否 spawn 子节点
- 动作：写 `state="implementing"`，开始开发

**implementing → verifying**（代码完成）：
- 触发条件：节点自认为代码完成，所有已知依赖已解决
- 动作：写 `state="verifying"`，执行 `./verify.sh`

**implementing → blocked**（依赖发现）：
- 触发条件：模块在开发过程中发现需要其他模块提供的接口
- 动作：先写 `shared/needs.toml`（key + desc + requester 路径），**再**写 `state="blocked"`
- 顺序不可颠倒：先 needs 后 state，确保 Orchestrator 读到 blocked 时 needs 已就绪

**blocked → implementing**（依赖解决 — Module Agent）：
- 触发条件：`needs.toml` 为空（无需依赖）OR `shared/resolved.toml` 中出现 `needs.toml` 里所有 key 的值
- 动作：读取 resolved 值，检查 inbox 里的 `value_changed` 通知，写 `state="implementing"`

**blocked → implementing**（子节点全部 delivered + 依赖解决 — Domain Agent）：
- 触发条件：`children_view` 中所有子节点状态为 `delivered` **且** `needs.toml` 中所有 key 都已出现在 `shared/resolved.toml` 中
- 动作：聚合子节点交付物，写 `state="implementing"`

**verifying → implementing**（verify 失败，重试未耗尽）：
- 触发条件：`verify.sh` 返回非 0，`retry_count < max_retries`
- 动作：`retry_count++`，写 `state="implementing"`，回去修代码

**verifying → dead**（verify 失败达到上限）：
- 触发条件：`retry_count >= max_retries`
- 动作：写 `state="dead"`，exit
- 注："implementing 阶段无进展"由 §6.3 的 `stuck_threshold_heartbeats` 覆盖，verifying 阶段不额外检查

**implementing → dead**（墙钟超时）：
- 触发条件：`max_wallclock_sec` 耗尽（由 SDK wrapper 在 tool call 回调时检查，或由 Orchestrator 侧 watchdog 通过 pid + 墙钟比对强制终止）
- 动作：写 `state="dead"`，exit
- 详见 §6.5

**blocked → dead**（墙钟超时）：
- 触发条件：在 `blocked` 状态下 `max_wallclock_sec` 耗尽（依赖永远无法满足）
- 动作：写 `state="dead"`，exit
- 详见 §6.5

### 3.3 父子写权限规则

- **唯一的"父能写子"操作**：把子的状态强制改为 `dead`（枯枝清理）。除此之外，父对子的 `state.toml` 是只读。
- **Orchestrator 可写**：任何节点的 `shared/resolved.toml` 和 `shared/tasks.toml`（这是依赖协议的一部分）。
- **节点不能写兄弟的 state.toml**。
- **节点不能直接 spawn 子进程**。Domain Agent 通过 `.forge/spawn_requests.toml` 请求 Orchestrator spawn（§15.3 Pass 6b）；其他节点无 spawn 权限。

---

## 4. 文件协议

### 4.1 `<node>/node.toml` — 节点定义（静态，人写）

```toml
[node]
name      = "module-bsp-uart"
role      = "module"                # orchestrator / domain / module / submodule
cwd       = "modules/firmware/submodules/bsp-uart"
parent    = "domain-firmware"       # 根节点为空字符串
depth     = 2                       # 由父在 spawn 时校验

[children]
declared = ["module-bsp-uart-tx", "module-bsp-uart-rx"]
spawn_strategy = "lazy"             # eager | lazy

[provides]
# 声明本模块能提供的接口（用于依赖匹配和环检测）
# 即使值还没产出，声明也是必须的——Orchestrator 靠这个建图
declared = ["APB1_CLK", "APB2_CLK", "UART_TX_PIN"]

[budget]
max_tokens         = 200_000
max_wallclock_sec  = 1800
max_retries        = 3                  # 覆盖 forge.toml 的 default_max_retries
max_subprocess     = 4

[runtime]
model     = "claude-sonnet-4-6"     # 越深的节点用越便宜的模型
```

### 4.2 `<node>/.forge/state.toml` — 节点动态状态（节点本人写）

```toml
schema_version = 1

[state]
current        = "implementing"
entered_at     = 2026-04-30T12:00:00+08:00
last_heartbeat = 2026-04-30T12:01:30+08:00
sequence       = 42                 # 单调递增，父读到老 sequence 就忽略

[progress]
percent_self_estimate = 60
summary               = "在写 uart_tx 的 DMA 路径"
current_task_id       = "T-firmware-uart-001"

[children_view]
[[children_view.child]]
name           = "module-bsp-uart-tx"
state          = "verifying"
last_seen_at   = 2026-04-30T12:01:25+08:00

[verify]
last_run_at   = 2026-04-30T11:55:00+08:00
last_result   = "fail"
fail_summary  = "test_uart_tx_overrun timeout"
retry_count   = 1

[budget_used]
tokens_used        = 87_400
wallclock_sec_used = 320
```

### 4.3 `<node>/.forge/inbox/` — 消息目录（目录即队列）

**为什么用目录而不是单文件**：并发安全。父或兄弟想给我发消息时，直接写一个**唯一文件名**进我的 `inbox/`，不需要文件锁。处理完移到 `inbox/processed/`（或删掉）。

文件名格式：`<unix_ts>-<from>-<msg_uuid>.toml`

```toml
schema_version = 1
id          = "msg-9f3a2b"
from        = "domain-firmware"
to          = "module-bsp-uart"
created_at  = 2026-04-30T12:02:00+08:00
kind        = "task" | "review" | "ack" | "kill" | "info" | "value_changed"
ref_task_id = "T-firmware-uart-001"
priority    = "P1"

[body]
title = "实现 uart_tx DMA 路径"
text  = "多行 markdown。"
attachments = ["src/uart.c#L80-L120"]
```

**特殊消息**：
- `kind="kill"`：父发出后，子必须在 ≤5s 内进入 `dead` 状态；否则父 SIGKILL。
  ```toml
  [body]
  reason    = "heartbeat_timeout"    # 或 "verify_failed", "cycle_dependency", "manual"
  grace_sec = 5
  ```
- `kind="value_changed"`：Orchestrator 通知模块某个依赖值已变更。
  ```toml
  [body]
  key     = "APB1_CLK"
  old_seq = 2
  new_seq = 3
  ```
  模块在进入 `implementing` 状态后**必须检查** inbox 里的 `value_changed` 消息，重新验证代码。

### 4.4 `<node>/shared/` — 依赖协议目录

每个需要参与依赖协作的模块都有 `shared/` 目录。

#### `shared/needs.toml` — 模块声明"我需要什么"（模块自己写）

```toml
# 模块在 implementing 状态下发现依赖时写入
# 写完后立即 state="blocked"
[needs.APB1_CLK]
desc = "APB1 总线时钟频率，用于计算 UART 波特率分频器"
requester = "modules/firmware/submodules/bsp-uart"    # 完整路径，用于跨层路由

[needs.UART_TX_PIN]
desc = "UART 发送引脚号"
requester = "modules/firmware/submodules/bsp-uart"
```

#### `shared/provides.toml` — 模块声明"我能提供什么"（模块自己写）

```toml
# 模块提供接口值时写入，带 seq 版本号
# node.toml 里的 provides.declared 是静态声明（用于环检测）
# 这里的 provides.toml 是实际值（用于传递）
[provides.APB1_CLK]
value = "42000000"
desc  = "APB1 总线时钟频率"
seq   = 2                  # 单调递增，值变更时 +1

[provides.APB2_CLK]
value = "84000000"
desc  = "APB2 总线时钟频率"
seq   = 1
```

#### `shared/resolved.toml` — Orchestrator 写入"别人给你的值"

```toml
# Orchestrator 写入，覆盖式（整个文件重写为当前所有 resolved 值）
# 模块从 blocked 回到 implementing 时读取
# 模块不清理此文件（Orchestrator 会在值变更时覆盖更新）
[resolved.APB1_CLK]
value = "42000000"
from  = "module-hal-clock"
seq   = 2
```

#### `shared/tasks.toml` — Orchestrator 写入"你被要求提供什么"

```toml
# Orchestrator 写入，当其他模块需要本模块提供的接口时
# 模块处理后在 provides.toml 里填值
[[task]]
key    = "APB1_CLK"
desc   = "APB1 总线时钟频率"
from   = "module-bsp-uart"
status = "pending"         # pending | done
```

#### `.forge/spawn_requests.toml` — Domain Agent 请求 spawn 子节点

```toml
# Domain Agent 在规划阶段写入，请求 Orchestrator spawn 子节点
# Orchestrator Pass 6b 读取并处理，处理后清空该文件
# name 必须在 node.toml children.declared 中声明

[[request]]
name = "module-bsp-uart-tx"
cwd  = "modules/firmware/submodules/bsp-uart-tx"

[[request]]
name = "module-bsp-uart-rx"
cwd  = "modules/firmware/submodules/bsp-uart-rx"
```

### 4.5 `forge_root/.forge/eventbus.log` — 项目级唯一事件总线

**追加写，从不修改**。Orchestrator 是 eventbus.log 的**唯一写入者**——它从各节点 state.toml / needs.toml / provides.toml 的变化以及自身动作（spawn / 依赖匹配 / 坏枝检测）中推导事件，统一追加一行 NDJSON。节点本身不直接写 eventbus.log（受 cwd 隔离限制）：

```jsonl
# ── 节点生命周期 ──
{"ts":"2026-04-30T12:00:00+08:00","node":"module-bsp-uart","event":"state","from":"assigned","to":"implementing","seq":41,"depth":2}
{"ts":"2026-04-30T12:00:05+08:00","node":"domain-firmware","event":"spawn","child":"module-bsp-uart","pid":48211,"depth":1}
{"ts":"2026-04-30T12:00:10+08:00","node":"module-bsp-uart","event":"dependency_discovered","key":"APB1_CLK","from":"implementing","to":"blocked"}
# ── 依赖匹配与解决 ──
{"ts":"2026-04-30T12:00:15+08:00","node":"orchestrator","event":"dependency_matched","requester":"module-bsp-uart","provider":"module-hal-clock","key":"APB1_CLK"}
{"ts":"2026-04-30T12:00:20+08:00","node":"orchestrator","event":"dependency_resolved","requester":"module-bsp-uart","key":"APB1_CLK"}
{"ts":"2026-04-30T12:00:25+08:00","node":"orchestrator","event":"value_changed","target":"module-bsp-uart","key":"APB1_CLK"}
# ── 环检测 ──
{"ts":"2026-04-30T12:00:30+08:00","node":"orchestrator","event":"deadlock","cycle":["mod-a","mod-b","mod-a"]}
{"ts":"2026-04-30T12:00:35+08:00","node":"orchestrator","event":"new_deadlock_prevented","new_edges":["mod-c→mod-d"]}
# ── 坏枝与清理 ──
{"ts":"2026-04-30T12:01:35+08:00","node":"orchestrator","event":"heartbeat_miss","subject":"module-bsp-uart","missed_for_sec":35,"action":"warn"}
{"ts":"2026-04-30T12:02:10+08:00","node":"orchestrator","event":"branch_dead","root_of_dead_branch":"module-bsp-uart","reason":"heartbeat_timeout"}
{"ts":"2026-04-30T12:02:15+08:00","node":"orchestrator","event":"node_dead","node":"module-bsp-uart","reason":"dependency_chain_propagation"}
{"ts":"2026-04-30T12:02:20+08:00","node":"orchestrator","event":"orphan_detected","node":"module-bsp-uart-rx","pid":48212}
# ── 跨层与 spawn ──
{"ts":"2026-04-30T12:03:00+08:00","node":"orchestrator","event":"cross_layer_resolved","requester":"tools/flasher","key":"UART_TX_PIN"}
{"ts":"2026-04-30T12:03:05+08:00","node":"orchestrator","event":"spawn_wake_failed","provider":"module-hal-clock","key":"APB1_CLK"}
{"ts":"2026-04-30T12:03:10+08:00","node":"orchestrator","event":"spawn_refused","reason":"max_depth"}
{"ts":"2026-04-30T12:03:15+08:00","node":"orchestrator","event":"dependency_escalated","requester":"tools/flasher","key":"UART_TX_PIN"}
{"ts":"2026-04-30T12:03:20+08:00","node":"orchestrator","event":"escalation_failed","key":"FLASH_SIZE","requester":"tools/flasher"}
{"ts":"2026-04-30T12:03:25+08:00","node":"orchestrator","event":"spawn_failed","child":"module-bsp-uart","reason":"state_file_timeout"}
{"ts":"2026-04-30T12:03:30+08:00","node":"domain-firmware","event":"suspected_stuck","subject":"module-bsp-uart-tx","unchanged_heartbeats":4}
```

### 4.6 `forge_root/forge.toml` — 根配置（全项目唯一）

```toml
[forge]
schema_version         = 1
max_depth              = 4
max_total_nodes        = 64
heartbeat_interval_sec = 15
heartbeat_timeout_sec  = 60
default_max_retries    = 3
stuck_threshold_heartbeats = 4        # 连续 N 次心跳 summary 不变 → suspected_stuck
scan_interval_sec      = 5            # Orchestrator 巡检 shared/ 的间隔
spawn_timeout_sec      = 30           # 等子进程写出初始 state.toml 的超时

[budget.global]
max_tokens_total        = 5_000_000
max_wallclock_total_sec = 14400

[budget.per_layer]
# layer 0 = Orchestrator（纯代码，不消耗 token，不需要 model）
1 = { tokens = 300_000, wallclock_sec = 3600, model = "claude-sonnet-4-6" }
2 = { tokens = 200_000, wallclock_sec = 1800, model = "claude-sonnet-4-6" }
3 = { tokens = 100_000, wallclock_sec = 900,  model = "claude-haiku-4-5" }

[paths]
event_bus   = ".forge/eventbus.log"
escalated   = ".forge/escalated.toml"
# pids 是 Orchestrator 内存索引，不持久化；PID 数据来自各节点 .forge/pid 文件（§5.3, §5.4）
```

### 4.7 `forge_root/.forge/escalated.toml` — 跨层依赖路由表（持久化）

```toml
# Orchestrator 启动时读取恢复；跨层依赖上报时写入
[[need]]
key           = "UART_TX_PIN"
requester     = "modules/tools/submodules/flasher"
provider      = "modules/firmware/submodules/bsp-uart"
status        = "pending"                # pending | matched | resolved | failed
attempt_count = 0                        # matched→pending 重试次数，>=3 标 failed
created_at    = "2026-04-30T10:00:00+08:00"
```

---

## 5. Spawn 协议

### 5.1 标准 spawn（首次启动）

**单一 spawn 函数，所有层共用**。L0→L1 与 L1→L2 走同一段代码——这就是"递归对称"。

```
function spawn_child(parent_node, child_def, is_wake_up=false):
    # ── 1. 预检查 ──
    if parent_node.depth + 1 > forge.max_depth:
        emit_event("spawn_refused", reason="max_depth"); return None
    if active_node_count() + 1 > forge.max_total_nodes:
        emit_event("spawn_refused", reason="max_total_nodes"); return None
    # 预算检查：remaining_budget(child_def) 检查 global max_tokens_total 和 per_layer tokens，
    # 取较严格限制，确保不超项目级和层级级 token 预算
    if remaining_budget(child_def, parent_node.depth + 1) < child_def.budget.max_tokens:
        emit_event("spawn_refused", reason="budget"); return None

    # ── 2. 准备子进程环境 ──
    env = {
        "FORGE_NODE_NAME":   child_def.name,
        "FORGE_NODE_DEPTH":  str(parent_node.depth + 1),
        "FORGE_PARENT":      parent_node.name,
        "FORGE_ROOT":        forge.root_path,
        "FORGE_IS_WAKE_UP":  str(is_wake_up),    # 区分首次启动 vs 唤醒
        "CLAUDE_API_KEY":    inherited_from_orchestrator,
    }

    # ── 3. 起子进程 ──
    prompt = build_prompt(child_def, is_wake_up)   # 见 §7
    pid = subprocess.spawn(
        cmd    = ["claude", "-p", prompt, "--dangerously-skip-permissions", "--output-format", "json"],
        cwd    = child_def.cwd,
        env    = env,
        stdout = "<cwd>/.forge/stdout.log",
        stderr = "<cwd>/.forge/stderr.log",
    )

    # ── 4. 注册 PID，写事件 ──
    pids_table.add(child_def.name, pid)
    emit_event("spawn", child=child_def.name, pid=pid, depth=parent_node.depth+1, wake_up=is_wake_up)

    # ── 5. 等子的初始 state.toml 写出 ──
    if not wait_for_state_file(child_def.cwd, timeout_sec=forge.spawn_timeout_sec):
        emit_event("spawn_failed", child=child_def.name, reason="state_file_timeout")
        kill(pid, SIGKILL)      # 子进程可能还活着（初始化慢），杀之防孤儿
        pids_table.remove(child_def.name)
        return None
    return pid
```

### 5.2 依赖驱动的唤醒

当 Orchestrator 发现一个模块 `blocked` 在等待依赖，且 provider 进程已退出时，决定是否唤醒 provider：

```
# 完整逻辑见 §15.3 Pass 6/Pass 7 和 §15.9。摘要如下：
if not is_alive(provider.pid):
    if provider.state == "delivered" and provides[provider].has(key):
        # provider 已交付且有该 key 的值。无论值是否变更，都不 spawn。
        # 值传递由 Pass 7 负责（首次解析写入 resolved.toml）。
        # 值变更由 Pass 8 负责（seq 比对 → 更新 resolved + inbox 通知）。
        pass    # 详见 §15.9
    else:
        # provider 未交付或无该 key 的值 → 需要唤醒处理新任务
        spawn_child(orchestrator, provider, is_wake_up=true)
        # provider 被唤醒后读 tasks.toml 处理新任务
```

### 5.3 PID 文件 + 进程探活

每个 `claude -p` 子进程在启动时写 PID 文件到自己的 `.forge/` 目录：

```bash
# 节点启动时写入（由 SDK 初始化代码或 UserPromptSubmit hook 触发）
# 注：PID 文件由子进程自己写入，spawn 后可能有延迟（claude -p 初始化需要几秒）。
# Orchestrator 通过 wait_for_state_file (§5.1) 等待子进程就绪，
# 期间 pids_table 用 subprocess.spawn 返回的 pid（OS 级，立即可用）。
echo "$$" > .forge/pid
echo "$(date -Iseconds)" > .forge/started_at
echo "$FORGE_NODE_NAME" > .forge/node_name
```

Orchestrator 用 `kill(pid, 0)` 探活（不发信号，只检查进程是否存在）。PID 文件包含三元组（PID + 启动时间 + 节点名）防复用误判。

> **注意**：`pids.toml`（§4.6）是 Orchestrator 内存中的索引表（从各节点 `.forge/pid` 汇聚），**不持久化**。
> Orchestrator 重启时通过扫描所有节点 `.forge/pid` 文件重建。

### 5.4 Orchestrator 启动时 PID 重建

```
# Orchestrator 启动（或 crash 恢复）时
pids_table = {}
for node_dir in collect_all_declared_nodes():    # 递归扫描所有含 node.toml 的目录
    pid_file = node_dir / ".forge/pid"
    if not pid_file.exists(): continue
    pid = int(read(pid_file).strip())
    if is_alive(pid):
        pids_table.add(node_dir.node_name, pid)
    else:
        # 进程已死，标 dead
        write(node_dir / ".forge/state.toml", {state: "dead"})
        emit_event("orphan_detected", node=node_dir.node_name, pid=pid)
```

---

## 6. 心跳 + TTL + 坏枝检测

### 6.1 心跳协议

- 每节点每 `forge.heartbeat_interval_sec` 秒**必须**更新自己 `state.toml` 的 `last_heartbeat` 字段。
- 写法：原子写（写到 `state.toml.tmp` 然后 `rename`）。
- 节点的 SDK 主循环里加一个 watchdog 协程，即使模型在长 inference 也定期写心跳。

### 6.2 父的扫描循环

```
每 heartbeat_interval_sec 秒:
    for child in children:
        pid = read_file(child.cwd / ".forge/pid")
        alive = is_alive(pid)
        state = read_state_toml(child)

        match (alive, state.current):
            (true,  "delivered")  → 等进程自行退出 (grace period 30s)；超时则 SIGKILL
            (true,  "dead")       → 进程应该已退出，给 5s 然后 SIGKILL
            (true,  _)            → 正常运行中，检查心跳
            (false, "delivered")  → ✅ 终态确认，回收
            (false, "blocked")    → 区分角色 + 交由 §15.3 Pass 7b 做最终依赖判定：
                Module Agent  → 若 Pass 7b 已确认 all_providers_dead 则回收；否则保留（依赖仍可能通过 escalation 解决）
                Domain Agent  → ⚠️ 标记 dead，但不回收子节点（子节点变为孤儿，由 Orchestrator 接管监控）
            (false, "dead")       → ✅ 终态确认，回收
            (false, _)            → ⚠️ 崩溃！标 dead，写 eventbus
```

### 6.3 进度 vs 活着（关键！）

**心跳只证明"还活着"，不证明"还在进展"**。叠加**进度签名**机制：

- 节点每写心跳时，顺带刷 `progress.summary` 与 `progress.percent_self_estimate`。
- 父记录子 `progress.summary` 的滚动 hash；若 `forge.stuck_threshold_heartbeats` 个连续心跳间 hash 完全没变，记为 `event="suspected_stuck"`，人（或 Orchestrator）介入决定是否杀。

### 6.4 坏枝传播规则

- 子被标 `dead` → 父在自己 `children_view` 标记 → 父决定：
  - **可降级完成**（子的输出可选）：父自己继续，把分支结果标 `partial`；
  - **不可降级**：父也进 `blocked`，向上汇报；爷爷再决定。
- **依赖链传播**：如果 provider dead → Orchestrator 检查所有依赖它的 requester → 如果无其他 provider → requester 也标 dead。

### 6.5 自杀闸门

- 节点本人若 `verify` 连续失败达到 `max_retries`，自己写 `state="dead"` 然后 exit；
- 在 `implementing` 状态下若 `max_wallclock_sec` 耗尽，自己写 `state="dead"` 然后 exit；
- 在 `blocked` 状态下若 `max_wallclock_sec` 耗尽（依赖永远无法满足），自己写 `state="dead"` 然后 exit。
- **实现方式**：SDK wrapper 在每次 tool call 回调时检查墙钟（推荐）；或由 Orchestrator 侧 watchdog 通过 pid + 墙钟比对强制终止。节点 prompt 里声明预算作为 LLM 自律兜底（不可靠）。

---

## 7. Agent Prompt 模板

通用启动器在调 SDK 前组装 prompt。**区分首次启动和唤醒**：

### 7.1 首次启动 prompt

```
你是 CortexForge 编排树中的一个节点。

[启动]
- 你刚被 spawn，当前状态应为 idle。
- 第一步：写 state.toml → state="idle"，然后轮询 ./.forge/inbox/ 等待任务。
- 收到任务后：state="assigned" → state="planning" → 分解任务 → state="implementing"。

[身份]
- 节点名: {FORGE_NODE_NAME}
- 角色:   {role}
- 深度:   {FORGE_NODE_DEPTH}
- 父节点: {FORGE_PARENT}
- 工作目录: {cwd}

[硬约束]
1. 你只能读写自己 cwd 之下的文件（realpath 后）。
2. 你不知道、也不需要知道兄弟节点的存在。所有跨节点信息走父节点。
3. 与外界的全部沟通走文件:
   - 收任务: ./.forge/inbox/*.toml
   - 写状态: ./.forge/state.toml（覆盖式）
   - 声明依赖: ./shared/needs.toml
   - 提供接口: ./shared/provides.toml
4. 每 {heartbeat_interval_sec}s 必须刷一次 state.toml 的 last_heartbeat。
5. 你的 token 预算 {max_tokens}，墙钟预算 {max_wallclock_sec}s，超即自我终止。

[依赖协议 — 关键]
当你在开发过程中发现需要其他模块提供的接口时：
1. 写 shared/needs.toml: 每项 key=接口名, desc=你需要什么的描述, requester=你的完整路径
2. **立即**写 state.toml → state="blocked", summary="等待 <key list>"
3. 停止所有开发工作
4. 轮询 shared/resolved.toml，等待 needs.toml 里**所有** key 都出现在 resolved.toml 中
5. 所有 key 都有值后：读取值，检查 inbox 里是否有 kind="value_changed" 的消息
6. 如果有 value_changed：用新值重新验证你的代码
7. 写 state.toml → state="implementing"，继续开发

[提供接口]
当你能提供其他模块需要的接口时：
1. 写 shared/provides.toml: key=接口名, value=实际值, desc=描述, seq=版本号
2. 每次值变更时 seq+1

[交付]
- 代码完成后：写 state.toml → state="verifying"，然后执行 ./verify.sh。
- verify 通过（退出码 0）：写 state="delivered"，产物落到 ./deliverables/。
- verify 失败（退出码非 0）+ retry_count < max_retries：retry_count++，写 state="implementing"，回去修代码。
- verify 失败 + retry_count >= max_retries：写 state="dead"，exit。

[决策权]
{IF role == "domain"}
- 你可以在 children.declared 范围内请求 spawn 子节点：在规划阶段写 .forge/spawn_requests.toml（Orchestrator 通过 §15.3 Pass 6b 读取并处理）。
- 你不可以创造未在 node.toml 声明的 children。
{ENDIF}
{IF role != "domain"}
- 你不可以 spawn 子节点（只有 Domain Agent 有此权限）。
{ENDIF}

{IF role == "domain"}
[Domain Agent 专属职责]
1. 你负责管理 children 列表中的子节点生命周期。
2. 周期扫描子节点 .forge/state.toml，聚合状态到自己的 .forge/state.toml → children_view。
3. 在规划阶段，写 .forge/spawn_requests.toml 请求 Orchestrator spawn 子节点（仅限 node.toml children.declared 中声明的）。
4. 请求 spawn 后，写 state.toml → state="blocked"，等待所有子节点 delivered。
5. 子节点 dead 时，你决定：
   - 可降级（子的输出可选）→ 标记 partial，自己继续
   - 不可降级 → 自己也进 blocked，向上汇报
6. 当 children_view 中所有子节点状态为 delivered **且** needs.toml 中所有 key 都已出现在 shared/resolved.toml 中时，写 state.toml → state="implementing"。
   - 在 blocked 状态下你仍需周期扫描 children_view（每 heartbeat_interval_sec 秒），检测子节点状态变化。
   - "继续开发"对你而言 = 聚合子节点交付物准备 verify，不涉及写业务代码。
   - 所有子节点 delivered 后，你才能进入 verifying。
7. 你可以读子节点的 shared/ 目录（needs.toml / provides.toml）和 .forge/state.toml。
8. 你不直接写子节点的 shared/tasks.toml——依赖匹配和 tasks.toml 写入由 Orchestrator 统一负责（§15.3 Pass 6）。跨域依赖匹配由 Orchestrator 自动通过 escalated.toml 机制处理（§15.5）。
{ENDIF}
```

### 7.2 唤醒 prompt

```
你是 {name}。你之前已完成交付，现在被重新唤醒。

[原因]
有其他模块需要你提供的接口（见 shared/tasks.toml）。

[任务]
1. 读取 shared/tasks.toml 中 status="pending" 的任务
2. 处理任务，在 shared/provides.toml 里填值。只有**新 key**或**值变更**的 key 才 seq 递增；仅确认已有 key 且值未变则不递增 seq。
3. 把 tasks.toml 里对应条目的 status 改为 "done"
4. 完成后 state="delivered"，退出
```

---

## 8. 验证与交付（verify.sh 闸门）

### 8.1 模块约定

每个模块**必须**在自己根目录提供一个可执行的 `verify.sh`（也可以是 `verify.py` / `verify.mk`，由模块自己在 `<m>/CLAUDE.md` 里声明）。

- 入口：`./verify.sh`（cwd = 模块根）
- 退出码：`0` = 通过；非 `0` = 失败
- stdout / stderr：失败原因
- 建议包含：编译通过、单元测试、静态检查、内存/flash 占用阈值、公共接口兼容性

### 8.2 交付流程

1. 节点在 `verifying` 状态下执行 `./verify.sh`
2. 通过：写 `state.toml` → `state="delivered"`，产物落到 `./deliverables/`
3. 失败 + `retry_count < max_retries`：`retry_count++`，写 `state="implementing"`，回去修代码
4. 失败 + `retry_count >= max_retries`：写 `state="dead"`，exit
5. 父扫到 `state="delivered"` 才认账，附 `artifacts.toml` 的 hash 防 TOCTOU

### 8.3 deliverables/ 目录

```
<node>/.forge/deliverables/
  v0.3.1/
    CHANGELOG.md
    artifacts.toml          # 文件列表 + hash + verify 结果引用
    (其它产物)
```

---

## 9. 权限模型（per-node 真隔离）

| 维度 | 实现 |
|------|------|
| 文件读 | SDK PreToolUse hook：`realpath(target).startswith(cwd)`，否则 deny |
| 文件写 | 同上 |
| Bash | 节点 `node.toml` 里 `[bash_allowlist]` 显式列；hook 校验命令前缀 |
| 网络 | 默认 deny；特定节点在 node.toml 里显式开 |
| Spawn 子进程 | 节点本身**没有** subprocess 权限。Domain Agent 通过 `.forge/spawn_requests.toml` 请求 Orchestrator spawn（§4.4, §15.3 Pass 6b） |

**MVP 阶段安全边界**：靠 prompt 纪律（模块自行遵守 cwd 约束）。后续版本可加 OS 级沙箱（chroot / bubblewrap / Docker）。`forge_node.rs` 的 spawn 函数里预留 `sandbox: Option<SandboxConfig>` 接口。

---

## 10. 可选增强（按需开启）

### 10.1 MCP server 封装工具链

- 将编译（`make`、`west build`、`idf.py`）、烧录（`openocd`、`esptool`、`pyocd`）、HIL 测试封装为 MCP tools
- 每个节点通过 MCP 调用，权限统一在 MCP server 层管控

### 10.2 模块层级集成顺序

- Orchestrator 在 release 阶段按 `node.toml` 的 `role` 字段拓扑排序：HAL → BSP → MW → APP
- 任一层没全 `delivered` 就不进入下一层集成

### 10.3 事件总线可视化

- 从 `eventbus.log` 生成 Mermaid / Graphviz 依赖图
- 集成到 `claude-hud` 状态栏或自建 dashboard

---

## 11. 显式取舍（不做什么）

| 不做 | 原因 |
|------|------|
| ❌ 不让节点自己 spawn 子进程 | 绕过预算/depth/总数检查，宽度爆炸 |
| ❌ 不让父主动读子源码 | 上下文爆炸；改用 inbox attach 机制按需取片段 |
| ❌ 不把临时任务/进度写进 CLAUDE.md | 那些属于 `state.toml` / `inbox/` |
| ❌ 不强制所有模块用 worktree | 嵌入式共享代码多 |
| ❌ 不把厂商工具链假设写入本架构 | 平台无关；具体绑定写在 `<m>/CLAUDE.md` |
| ❌ 不在 MVP 阶段做 OS 级沙箱 | 先靠 prompt 约束，后续加 chroot/Docker |
| ❌ 不要求模块预先声明所有依赖 | Pull-based 动态发现更贴近实际开发流程 |
| ❌ 不允许多个 provider 声明同一个 key | 简化依赖匹配（无歧义消解），通过 key 命名约定区分（如 `UART1_TX_PIN` vs `UART2_TX_PIN`） |

---

## 12. 真坑清单（必须设计应对）

| 真坑 | 应对 |
|------|------|
| **成本爆炸** | `forge.toml` 的 `max_total_nodes` + `budget.per_layer` + 越深用越便宜的模型 |
| **递归宽度滥用** | spawn 走 Orchestrator 单点检查 |
| **心跳但卡死** | §6.3 进度签名 hash 比对 |
| **状态文件并发** | 所有共享文件原子写（tmp+rename）+ 容错读（解析失败跳过本轮） |
| **崩溃恢复** | per-node `.forge/pid` 文件持久化；Orchestrator 启动时扫描重建 pids 索引（§5.4）；`escalated.toml` 持久化跨层路由 |
| **schema 演进** | 每个 toml 带 `schema_version`；启动时校验兼容性 |
| **坏枝阻塞兄弟** | §6.4 按需传播；父决定可降级 vs 升级阻塞 |
| **debugging 黑盒** | §4.5 项目级 eventbus，grep/jq 可重建任意节点时间线 |
| **token 预算硬切** | SDK wrapper 包装 token 计数；达到 budget 就 abort |
| **节点绕过 spawn** | 节点没 subprocess 权限；spawn 必须走 Orchestrator |
| **verify TOCTOU** | artifacts.toml hash + state.toml seq |
| **模型升级 prompt 漂移** | node.toml 锁 model ID；升级时按 layer 灰度 |
| **循环依赖死锁** | Orchestrator 建图 + 环检测（§15.3） |
| **值覆盖（过期值）** | provides.toml per-key seq 版本号 + 值变更检测（§15.4） |
| **跨层依赖路由** | needs.toml 带 requester 完整路径 + escalated.toml 持久化（§15.5） |

### 待 MVP 实跑验证的未知项

- 节点 boot 启动延迟叠加（每层 `claude -p` 初始化 + first-token）
- 跨节点 token 重复（同一段代码被多层读进上下文）
- `claude -p` subprocess 的 API 重连稳定性
- 事件总线高并发下的写竞争（NDJSON O_APPEND 跨平台差异）。注：P0 #1 修复后 eventbus 由 Orchestrator 单写，此项风险大幅降低
- 唤醒冷启动的 token 重复成本（provider 被唤醒时 prompt 无上下文，需重新读取源码以理解如何提供被请求的接口值）

---

## 13. MVP 最小成功标准

以下 8 条全部通过即可证明本设计**可行**：

1. **3 层 spawn 跑通**：L0 起 1 个 L1，L1 起 2 个 L2，所有节点 state.toml 正常生命周期
2. **递归对称**：L0→L1 和 L1→L2 走同一段 `spawn_child` 代码，无 layer-specific 分支
3. **心跳超时杀枝**：人为让一个 L2 节点 sleep 超过 timeout，父正确标 dead，事件总线有完整记录，兄弟不受影响
4. **依赖发现与解决**：模块 A 写 needs.toml → Orchestrator 匹配到 provider B → 写 task → B 提供值 → resolved.toml 写入 → A 继续开发
5. **循环依赖检测**：A needs X from B, B needs Y from A → Orchestrator 检测到环 → 标 dead
6. **坏枝传播**：L2 verify 连续失败到 max_retries，L1 收敛到 partial 或 blocked 的决策正确
7. **崩溃恢复**：杀掉 Orchestrator，重启，state 能从文件正确恢复
8. **事件总线可重建**：从 eventbus.log 单文件能 grep 出任意节点的完整生命周期

---

## 14. 与原始脑暴对照表

| 脑暴 | 本架构落点 |
|------|-----------|
| B1 树状分层 | §2 拓扑（N 级，`max_depth` 可配） |
| B2 根 Agent 多职责 | §2.2 Orchestrator (L0)（纯代码） |
| B3 分层 Agent | §2.2 Domain Agent (L1) |
| B4 子模块 Agent | §2.2 Module Agent (L2+) |
| B5 子模块只能本目录 + 文件沟通 | §4 文件协议 + §9 权限模型 + §15 依赖协议 |
| B6 每 Agent 自己的 CLAUDE.md | §2.4 记忆载体（per-folder CLAUDE.md） |
| B7 父级只读对话文件 | §2.3「不读子源码」纪律 + §4.2 state.toml |
| B8 根目录省 token 配置 | §4.6 forge.toml + §4.5 eventbus.log |
| B9 自带 hook + 验收 + 通知父级 | §8 verify.sh + §6 心跳/扫描 |
| B10 任务发散 → 交付收敛 | §3 状态机 + §15 pull-based 依赖 + §10.2 集成顺序 |

---

## 15. Pull-Based 依赖解析协议（完整规范）

> 本节是依赖机制的权威规范，包含三十轮架构评审发现的全部 118 个修复点。
> 前面章节中的依赖相关内容是摘要，以本节为准。

### 15.1 机制总览

```
模块在 implementing 时发现依赖
    ↓
写 shared/needs.toml (key + desc + requester 路径)
    ↓
立即写 state.toml → state="blocked"
    ↓
轮询 shared/resolved.toml

Orchestrator 周期巡检（每 scan_interval_sec 秒）:
    ↓
读所有 alive 节点的 needs.toml + provides.toml + state + node.toml.provides
    ↓
建依赖图（用 node.toml 的 provides 声明，不用 provides.toml 的值）
    ↓
环检测（第一次：现有状态）
    ↓
匹配 unresolved needs → 生成新边
    ↓
环检测（第二次：包含新边）→ 有环则回滚，标 dead
    ↓
写 tasks.toml 到 provider + spawn 如果需要
    ↓
provider 完成 → provides.toml 有值 → 写 resolved.toml 到 requester
    ↓
依赖链传播：provider dead 且无值 + 无 pending escalated → requester 标 dead（§6.4）
    ↓
值变更检测：provider.seq > resolved.seq → 重写 resolved + 写 inbox 通知
    ↓
跨层：全树无 provider → 写 escalated.toml，由 Pass 9 处理（§15.5）
```

### 15.2 模块侧：依赖发现流程

**触发**：模块在 implementing 状态下，通过读代码 / 文档 / 开发推理发现需要其他模块提供的接口。

**动作序列**（顺序不可颠倒）：

```
1. 写 shared/needs.toml:
   [needs.APB1_CLK]
   desc = "APB1 总线时钟频率，用于计算 BRR"
   requester = "modules/firmware/submodules/bsp-uart"    # 完整相对路径

2. 写 state.toml → state="blocked", summary="等待 APB1_CLK"

3. 停止所有开发工作

4. 轮询 shared/resolved.toml（每 scan_interval_sec 秒读一次）:
   # 等待 needs.toml 里所有 key 都出现在 resolved.toml 中
   while not all(key in resolved for key in needs):
       sleep(scan_interval_sec)
   
5. 所有 key 都有值后:
   - 检查 inbox/ 里是否有 kind="value_changed" 的消息（针对任一 key）
   - 如果有：对比 resolved.toml 里的 seq，用最新值
   - 写 state.toml → state="implementing"
   - 继续开发
```

**修复点 #P0-1**：写 needs 和写 state 的顺序——先 needs 后 state。确保 Orchestrator 读到 blocked 时 needs 已就绪。

**修复点 #P0-9**：resolved.toml **不清空**。Orchestrator 覆盖式写入。模块从 blocked 回到 implementing 时不清空 resolved（已解决的依赖值需要保留供后续使用）。

### 15.3 Orchestrator 侧：依赖匹配与环检测

**主循环（每 `scan_interval_sec` 秒执行一次）**：

```
# ── Pass 1: 收集（递归所有活跃节点，不仅是直接子节点） ──
all_nodes = collect_all_declared_nodes()    # 递归遍历整棵树
for node in all_nodes:
    pid = safe_read(node/.forge/pid)
    if not pid or not is_alive(int(pid)): continue
    state[node]      = safe_read(node/.forge/state.toml)
    state_seq[node]  = (state[node] or {}).sequence or 0    # 记录 seq，防读到旧版本 state
    needs[node]      = safe_read(node/shared/needs.toml)
    provides[node]   = safe_read(node/shared/provides.toml)
    tasks[node]      = safe_read(node/shared/tasks.toml)
    node_def[node]   = safe_read(node/node.toml)

# 过滤：仅保留 alive 节点，后续 Pass 安全访问 state/node_def/needs/provides
all_nodes = [node for node in all_nodes if node in state]

# ── Pass 2: 建图（用 node.toml 的 provides 声明） ──
edges = []
for (requester, need_entries) in needs:
    for key in need_entries.keys():
        # 跳过已解决的（resolved.toml 可能不存在，or {} 兜底）
        if key in (safe_read(requester/shared/resolved.toml) or {}): continue
        
        # 用 node.toml 的 provides.declared 匹配（不用 provides.toml 的值）
        for provider in all_nodes:
            if provider == requester: continue
            if key in node_def[provider].provides.declared:
                edges.push((requester → provider, key))

# ── Pass 3: 第一次环检测（现有状态） ──
(has_cycle, cycle_nodes) = has_cycle(edges)
if has_cycle:
    mark_cycle_dead(cycle_nodes)
    emit_event("deadlock", cycle=cycle_nodes)
    # 从后续 Pass 的 needs 中移除有环节点，不阻塞其他独立依赖组
    for node in cycle_nodes:
        needs.pop(node, None)

# mark_cycle_dead 定义:
# function mark_cycle_dead(nodes):
#     for node in nodes:
#         write(node/.forge/state.toml, {state: "dead", summary: "cycle_dependency"})
#         emit_event("node_dead", node=node.name, reason="cycle")
#     for node in nodes:
#         for child in node.children:
#             mark_cycle_dead([child])

# find_provider 定义: 在当前扫描轮次的 all_nodes 中查找 provider
# 搜索范围：Pass 1 收集的 all_nodes（本轮开始时的活跃节点快照）
# function find_provider(key, node_def, all_nodes):
#     for node in all_nodes:
#         if key in node_def[node].provides.declared:
#             return node
#     return None

# find_global_provider 定义: 在所有活跃节点中查找 provider（含本轮新 spawn）
# 搜索范围：collect_all_declared_nodes() 重新递归收集（包含 Pass 6 新 spawn 的节点）
# function find_global_provider(key, active_nodes):
#     for node in active_nodes:
#         if key in read(node/node.toml).provides.declared:
#             return node
#     return None

# find_node 定义: 按名称在给定 all_nodes 中查找节点对象
# 注：接受 all_nodes 参数（非自行收集），保证返回对象与 Pass 1 dict key 一致
# function find_node(name, all_nodes):
#     for node in all_nodes:
#         if node.name == name:
#             return node
#     return None

# collect_all_declared_nodes 定义: 递归收集所有目录中有 node.toml 的节点
# 返回的是所有已声明的节点对象（含 dead），调用方自行过滤 alive
# function collect_all_declared_nodes():
#     nodes = []
#     for dir in recursive_walk(forge_root):
#         if (dir / "node.toml").exists():
#             node = parse_node_toml(dir / "node.toml")
#             node.pid_file = dir / ".forge/pid"    # 预置 PID 文件路径
#             nodes.append(node)
#     return nodes

# ── Pass 4: 匹配 unresolved needs → 生成新边 ──
new_edges = []
for (requester, need_entries) in needs:
    if state[requester].current != "blocked": continue
    for key in need_entries.keys():
        if key in (safe_read(requester/shared/resolved.toml) or {}): continue

        provider = find_provider(key, node_def, all_nodes)
        if not provider:
            # 全树无 provider → 写 escalated.toml 交由 Pass 9 处理
            escalate_to_parent(requester, key, need_entries[key])
            continue

# escalate_to_parent 定义（§15.5）:
# function escalate_to_parent(requester, key, need_entry):
#     # 去重：检查是否已有同 requester+key 的非终态条目
#     existing = safe_read(forge_root/.forge/escalated.toml) or []
#     for e in existing:
#         if e.key == key and e.requester == requester.name and e.status not in ("resolved", "failed"):
#             return    # 已存在 pending/matched 条目，跳过
#     append(forge_root/.forge/escalated.toml, {
#         key:        key,
#         requester:  requester.name,
#         provides:   node_def[requester].provides.declared,
#         status:     "pending",
#         created_at: now()
#     })
#     emit_event("dependency_escalated", requester=requester.name, key=key)
        
        # 检查是否已在 tasks.toml 里（去重）
        if (tasks[provider] or {}).has(key=key, from=requester.name): continue
        
        new_edges.push((requester → provider, key))

# ── Pass 5: 第二次环检测（包含新边） ──
all_edges = edges + new_edges
(has_cycle2, cycle_nodes2) = has_cycle(all_edges)
if has_cycle2:
    # 回滚：不写新 tasks，从 new_edges 中移除有环边
    emit_event("new_deadlock_prevented", new_edges=new_edges)
    mark_cycle_dead(cycle_nodes2)
    new_edges = [e for e in new_edges if e[0] not in cycle_nodes2]

# ── Pass 6: 写 tasks.toml + spawn 决策 ──
for (requester → provider, key) in new_edges:
    desc = needs[requester][key].desc    # 从 Pass 1 收集的 needs 字典取 desc
    append(provider/shared/tasks.toml, {
        key: key,
        desc: desc,
        from: requester.name,
        status: "pending"
    })

    if not is_alive(provider.pid):
        # provider 已退出。若已 delivered 且有该 key 的值，不 spawn（值由 Pass 7 直接从 provides.toml 复制）
        if state[provider].current == "delivered" and (provides[provider] or {}).has(key):
            pass    # 详见 §15.9：值由 Pass 7 复制，无需 spawn
        else:
            result = spawn_child(orchestrator, provider, is_wake_up=true)
            if result:
                provider.pid = result    # 更新快照中的 pid，防止同一 provider 被重复 spawn
            else:
                # spawn 失败，移除 task 条目，等下一轮重试
                remove_task(provider/shared/tasks.toml, key=key, from=requester.name)
                emit_event("spawn_wake_failed", provider=provider.name, key=key)

# ── Pass 6b: 处理 Domain Agent 的 spawn_requests ──
for node in all_nodes:
    if node_def[node].role != "domain": continue
    requests = safe_read(node/.forge/spawn_requests.toml) or {}
    for req in (requests.request or []):
        # 校验：child 必须在 node.toml children.declared 中
        if req.name not in node_def[node].children.declared:
            emit_event("spawn_refused", reason="not_declared", child=req.name, parent=node.name)
            continue
        # 从 cwd 读取 child 的 node.toml
        child_node_toml = safe_read(forge_root / req.cwd / "node.toml")
        if not child_node_toml:
            emit_event("spawn_refused", reason="node_toml_not_found", child=req.name, cwd=req.cwd)
            continue
        # 检查 child 是否已 alive（通过 PID 文件）
        child_pid = safe_read(forge_root / req.cwd / ".forge/pid")
        if child_pid and is_alive(int(child_pid)): continue
        result = spawn_child(orchestrator, child_node_toml, is_wake_up=false)
        if not result:
            emit_event("spawn_failed", child=req.name, parent=node.name)
    # 处理完毕，清空 spawn_requests
    if requests.request:
        write(node/.forge/spawn_requests.toml, {})
    # 注：spawn 被拒/失败时，应写 inbox 通知给请求方（Domain Agent），kind="info"，
    # body.reason 写明原因（max_depth / not_declared / node_toml_not_found / budget 等）。
    # 避免 Domain Agent 在 blocked 状态下无限等待不存在的子节点。

# ── Pass 7: 传递 resolved 值（统一负责所有 resolved 写入） ──
# 注：Pass 7 基于 Pass 1 收集的 all_nodes 快照。本轮 Pass 6 新 spawn 的 provider 将在
# 下一轮巡检被 Pass 1 收集并处理（一个 scan_interval_sec 延迟）。新 spawn provider 通常
# 需要更长时间才能写出 provides.toml，故此延迟不造成功能问题。
for node in all_nodes:
    if state[node].current != "blocked": continue
    current = safe_read(node/shared/resolved.toml) or {}
    changed = false
    for key in (needs[node] or {}).keys():
        if key in current: continue
        provider = find_provider(key, node_def, all_nodes)
        if provider and (provides[provider] or {}).has(key):
            # provider 已有值 → 合并到 current
            current[key] = {
                value: provides[provider][key].value,
                from:  provider.name,
                seq:   provides[provider][key].seq
            }
            changed = true
            emit_event("dependency_resolved", requester=node.name, key=key)
        elif not provider:
            # 全树无 provider → 跨层上报（已在 Pass 4 处理）
    if changed:
        write(node/shared/resolved.toml, current)   # read-merge-write，不丢已有 key

# ── Pass 7b: 依赖链传播（§6.4, §15.7） ──
escalated = safe_read(forge_root/.forge/escalated.toml) or []
for node in all_nodes:
    if state[node].current != "blocked": continue
    all_providers_dead = true
    for key in (needs[node] or {}).keys():
        if key in (safe_read(node/shared/resolved.toml) or {}): continue
        provider = find_provider(key, node_def, all_nodes)
        if provider:
            if is_alive(provider.pid) or (provides[provider] or {}).has(key):
                all_providers_dead = false    # 有活 provider 或有值的死 provider
                break
        else:
            # 全树无 provider → 检查是否有 pending 的 escalated 条目（仍有可能解决）
            if any(e.key == key and e.requester == node.name and e.status == "pending"
                   for e in escalated):
                all_providers_dead = false    # 有 pending escalated，等待匹配
                break
    if all_providers_dead and needs[node] and needs[node].keys():
        write(node/.forge/state.toml, {state: "dead", summary: "all_providers_dead"})
        emit_event("node_dead", node=node.name, reason="dependency_chain_propagation")

# ── Pass 8: 值变更检测 ──
for node in all_nodes:
    resolved = safe_read(node/shared/resolved.toml) or {}
    changed = false
    for key in resolved.keys():
        provider = find_provider(key, node_def, all_nodes)
        if provider and provides[provider] and provides[provider][key] and provides[provider][key].seq > resolved[key].seq:
            old_seq = resolved[key].seq           # 保存旧 seq，供 inbox 通知使用
            # 值变了，合并更新 resolved
            resolved[key] = {
                value: provides[provider][key].value,
                from:  provider.name,
                seq:   provides[provider][key].seq
            }
            changed = true
            # 写 inbox 通知（不直接拉回 implementing）
            msg_id = gen_uuid()
            write(node/.forge/inbox/ + now_ts() + "-orchestrator-" + msg_id + ".toml", {
                schema_version: 1,
                id: msg_id,
                from: "orchestrator",
                to: node.name,
                kind: "value_changed",
                body: {
                    key: key,
                    old_seq: old_seq,
                    new_seq: provides[provider][key].seq
                }
            })
            emit_event("value_changed", target=node.name, key=key)
    if changed:
        write(node/shared/resolved.toml, resolved)   # read-merge-write

# ── Pass 9: 跨层上报处理 ──
escalated = safe_read(forge_root/.forge/escalated.toml)
for entry in escalated:
    if entry.status == "pending":
        # 检查所有活跃节点的 provides 声明（递归收集）
        provider = find_global_provider(entry.key, collect_all_declared_nodes())
        if provider:
            write(provider/shared/tasks.toml, ...)
            entry.provider = provider.name    # 记录 provider，供 matched→resolved 使用
            entry.status = "matched"

    elif entry.status == "matched":
        # 注：本轮 Pass 6 新 spawn 的 provider 不在过滤后的 all_nodes 中（§15.3 Pass 1 过滤），
        # 将在下一轮巡检被 Pass 1 收集后由本段处理（一个 scan_interval_sec 延迟）。
        provider = find_node(entry.provider, all_nodes)
        # 检查 provider 是否已死（需要重置或放弃）
        if provider and state[provider] and state[provider].current == "dead":
            entry.attempt_count = (entry.attempt_count or 0) + 1
            if entry.attempt_count >= 3:
                entry.status = "failed"
                emit_event("escalation_failed", key=entry.key, requester=entry.requester)
            else:
                entry.provider = None         # 清空旧 provider，等下一轮重新匹配
                entry.status = "pending"
            continue
        # 检查 provider 是否已完成：进程已退出（终态确认）+ state=delivered + provides.toml 有值
        if provider
           and state[provider]
           and not is_alive(provider.pid)       # 进程已退出，provides.toml 完整
           and state[provider].current == "delivered"
           and (provides[provider] or {}).has(entry.key):
            # provider 已交付 → 读 requester 当前 resolved，merge 后写入，不丢已有 key
            requester = find_node(entry.requester, all_nodes)
            if requester:
                current = safe_read(requester/shared/resolved.toml) or {}
                current[entry.key] = {
                    value: provides[provider][entry.key].value,
                    from:  entry.provider,
                    seq:   provides[provider][entry.key].seq
                }
                write(requester/shared/resolved.toml, current)  # read-merge-write
                entry.status = "resolved"
                emit_event("cross_layer_resolved", requester=entry.requester, key=entry.key)

    elif entry.status == "resolved":
        # 跨层值变更检测：provider 值变了 → 更新 requester 的 resolved + inbox 通知
        provider = find_node(entry.provider, all_nodes)
        requester = find_node(entry.requester, all_nodes)
        if provider and requester and provides[provider]:
            current = safe_read(requester/shared/resolved.toml) or {}
            if (provides[provider] or {}).has(entry.key)
               and current.has(entry.key)
               and provides[provider][entry.key].seq > current[entry.key].seq:
                old_seq = current[entry.key].seq
                # 合并更新 resolved
                current[entry.key] = {
                    value: provides[provider][entry.key].value,
                    from:  entry.provider,
                    seq:   provides[provider][entry.key].seq
                }
                write(requester/shared/resolved.toml, current)  # read-merge-write
                msg_id = gen_uuid()
                write(requester/.forge/inbox/ + now_ts() + "-orchestrator-" + msg_id + ".toml", {
                    schema_version: 1,
                    id: msg_id,
                    from: "orchestrator",
                    to: requester.name,
                    kind: "value_changed",
                    body: {
                        key: entry.key,
                        old_seq: old_seq,
                        new_seq: provides[provider][entry.key].seq
                    }
                })

# ── Pass 9 收尾: 清理已终态的 escalated 条目 ──
escalated = [e for e in escalated if e.status not in ("resolved", "failed")]
write(forge_root/.forge/escalated.toml, escalated)
```

**修复点 #P0-10**：主循环两次环检测——第一次用现有状态，第二次包含本轮新边。确保写入 tasks.toml 前确认无环。

**修复点 #P0-7**：建图用 `node.toml` 的 `provides.declared`（静态声明），不用 `provides.toml`（动态值）。声明在 spawn 前就知道，值可能还没填。

**修复点 #P1-4**：写 task 前检查 `tasks.toml` 是否已有同 key + 同 from 的条目，去重。

**修复点 #4-P2-1**：Pass 6 不再做 seq 比对的直接复制（首次解析时 resolved 中无 key 会崩溃）。resolved 值传递统一由 Pass 7 负责。

**修复点 #4-P2-4**：Orchestrator 扫描范围是**所有活跃节点**（递归收集），不只是直接子节点。依赖匹配、环检测、值变更检测都基于全树。

### 15.4 值变更与级联失效

**场景**：hal-clock 重配时钟，APB1_CLK 从 42MHz 变成 84MHz。

```
T1  hal-clock 写 provides.toml: APB1_CLK = 84000000, seq = 3 (原来是 2)
T2  Orchestrator Pass 8 检测到: provider.seq(3) > resolved.seq(2)
T3  Orchestrator 写 bsp-uart/resolved.toml: APB1_CLK = 84000000, seq = 3
T4  Orchestrator 写 bsp-uart/inbox: kind="value_changed", key=APB1_CLK, old_seq=2, new_seq=3
T5  bsp-uart 当前状态可能是:
    (a) implementing → 读 inbox → 发现 value_changed → 重新读 resolved → 用新值重新验证代码
    (b) blocked（等另一个依赖）→ inbox 里有通知 → 等解 blocked 后检查
```

**修复点 #P1-6**：值变更时**不直接拉回 implementing**（因为模块可能还在等别的依赖）。改为写 inbox 通知。模块在进入 implementing 后检查 inbox。

**修复点 #P0-4**：`provides.toml` 每个 key 带 `seq` 版本号。值变更时 seq+1。Orchestrator 通过 seq 比对检测变更。

### 15.5 跨层依赖上报

**场景**：tools/flasher 需要 UART_TX_PIN，但本层没有 provider。

```
T1  flasher 写 needs.toml: UART_TX_PIN = {desc="...", requester="modules/tools/submodules/flasher"}
T2  flasher state="blocked"
T3  根 Orchestrator Pass 4: find_provider("UART_TX_PIN") → 全树无 provider
T4  根 Orchestrator 调 escalate_to_parent(): 写 .forge/escalated.toml:
      key="UART_TX_PIN", requester="modules/tools/submodules/flasher", status="pending"
T5  根 Orchestrator Pass 9: 扫描所有活跃节点的 provides 声明
    → firmware/bsp-uart 的 node.toml 有 provides.declared = ["UART_TX_PIN"]
T6  根 Orchestrator 写 task 到 bsp-uart, spawn 如果需要; entry.status = "matched"
T7  bsp-uart 提供 UART_TX_PIN = "PA9"
T8  根 Orchestrator Pass 9 matched→resolved: 写 resolved.toml 到 tools/flasher:
    路径 = forge_root / "modules/tools/submodules/flasher/shared/resolved.toml"
T9  flasher 读到 resolved → 继续开发
```

**修复点 #P1-5**：needs.toml 里带 `requester` 完整路径。根 Orchestrator 用这个路径拼接 resolved.toml 的写入位置。

**修复点 #P2-4**：`escalated.toml` 持久化。Orchestrator crash 后重启能恢复跨层路由状态。

**修复点 #P2-2**：跨层上报时附带 requester 的 provides 声明，支持跨层环检测。

**修复点 #4-P1-3**：跨层值变更检测。Pass 9 对 `escalated.toml` 中 `status="resolved"` 的条目做 seq 比对，发现 provider 值变更时写 resolved + inbox 通知到 requester。解决"跨层依赖的值变更无法传播"问题。

### 15.6 循环依赖检测

**算法**：DFS 检测有向图中的环。O(V+E)，V=全树活跃节点数，E=依赖边数。

```
function has_cycle(edges) -> (bool, list[cycle_nodes]):
    graph = adjacency_list(edges)
    visited = set()
    in_stack = set()
    cycle_nodes = []
    
    function dfs(node):
        visited.add(node)
        in_stack.add(node)
        for neighbor in graph[node]:
            if neighbor in in_stack:
                cycle_nodes.append(neighbor)    // 记录环上的节点
                return true
            if neighbor not in visited:
                if dfs(neighbor): return true
        in_stack.remove(node)
        return false
    
    for node in graph:
        if node not in visited:
            if dfs(node): return (true, cycle_nodes)
    return (false, [])
```

**修复点 #P0-2**：Orchestrator 在匹配依赖前做环检测。有环则标 dead，不写 task。

**修复点 #P0-7**：建图用 `node.toml` 的 `provides.declared`，不用 `provides.toml` 的值。声明是静态的，值可能还没填。

**修复点 #P0-10**：两次环检测——第一次用现有边，第二次包含本轮新边。新边在写 tasks.toml 前检测。

**渐进式环的限制**：如果 A needs X from B, B needs Y from A，但 B 还没声明 needs.toml（还没发现自己需要 Y），环暂时检测不到。随着更多模块声明 needs，环会逐步被发现。每 10 轮额外跑一次全局环检测兜底。

### 15.7 死模块清理

**修复点 #P1-9**：Orchestrator 只处理 alive 模块的 needs。

```
for node in all_nodes:
    if not is_alive(node.pid): continue    # 跳过死模块
    needs = read(node/shared/needs.toml)
    ...
```

**依赖链传播**：如果 provider dead → 检查所有依赖它的 requester → 如果无其他 provider → requester 也标 dead。

### 15.8 文件并发安全

**修复点 #P0-3**：所有共享文件原子写 + 容错读。

```rust
// 原子写
fn atomic_write(path: &Path, content: &str) {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).unwrap();
    fs::rename(&tmp, path).unwrap();  // rename 在同一文件系统上是原子的
}

// 容错读
fn safe_read_toml(path: &Path) -> Option<Document> {
    match fs::read_to_string(path) {
        Ok(s) => s.parse().ok(),  // 解析失败返回 None，不 panic
        Err(_) => None,
    }
}
```

### 15.9 已 delivered provider 的优化

**修复点 #P1-8**：如果 provider 已 delivered 且有该 key，直接从 provides.toml 复制到 requester 的 resolved.toml（首次解析）或跳过（值已存在），不 spawn。值变更由 Pass 8 seq 比对处理。

```
if provider.state == "delivered" and provides[provider].has(key):
    if not resolved[requester].has(key):
        // 首次解析：写 resolved，转换格式
        write(resolved.toml, {
            key: {
                value: provides[provider][key].value,
                from:  provider.name,
                seq:   provides[provider][key].seq
            }
        })
    else:
        // key 已存在：无需操作（值变更由 Pass 8 seq 比对处理）
        pass
else:
    // provider 未交付或无该 key → 唤醒处理新任务
    spawn_child(orchestrator, provider, is_wake_up=true)
```

### 15.10 修复点完整清单

| # | 来源 | 等级 | 修复点 | 落地位置 |
|---|------|------|--------|---------|
| 1 | Pass 1 | P0 | 模块写 needs.toml 后立即 state="blocked" | §15.2, prompt §7 |
| 2 | Pass 1 | P0 | 循环依赖：Orchestrator 建图 + 环检测 | §15.6 |
| 3 | Pass 1 | P0 | 文件并发：原子写（tmp+rename）+ 容错读 | §15.8 |
| 4 | Pass 1 | P0 | 值覆盖：provides.toml per-key seq 版本号 | §4.4, §15.4 |
| 5 | Pass 1 | P1 | 跨层路由：needs.toml 带 requester 完整路径 | §4.4, §15.5 |
| 6 | Pass 1 | P1 | 多 provider 歧义：全树同 key 只允许一个 provider | §15.3 Pass 4 |
| 7 | Pass 1 | P1 | 唤醒 prompt 区分首次启动 vs 唤醒 | §5.2, §7 |
| 8 | Pass 1 | P1 | 值变级联：upstream seq 变化 → 写 inbox 通知 | §15.4 |
| 9 | Pass 1 | P1 | dead 模块 needs 残留：只处理 alive 模块 | §15.7 |
| 10 | Pass 1 | P2 | 重启后 resolved 残留：resolved.toml 不清空，覆盖式 | §15.2 |
| 11 | Pass 1 | P2 | 读端竞争：容错读 + 跳过本轮 | §15.8 |
| 12 | Pass 2 | 低 | needs.toml 残留（已 resolved 但没清理）：匹配时先检查 resolved | §15.3 Pass 4 |
| 13 | Pass 2 | 中 | 跨层上报格式：附带 requester 路径 + provides 列表 | §15.5 |
| 14 | Pass 2 | 中 | 渐进式环检测：每 10 轮全树全局检测兜底 | §15.6 |
| 15 | Pass 2 | 低 | 重复分发：写 task 前检查是否已存在 | §15.3 Pass 6 |
| 16 | Pass 2 | 中 | 值变更通知被 blocked 吞：写 inbox 通知 | §15.4 |
| 17 | Pass 2 | 低 | 跨层写回权限：Orchestrator 是 OS 进程，无限制 | §15.5 |
| 18 | Pass 3 | 低 | 写 needs 和 state 的顺序：先 needs 后 state | §15.2 |
| 19 | Pass 3 | P1 | escalated_needs 持久化：`.forge/escalated.toml` | §4.7, §15.5 |
| 20 | Pass 3 | P0 | 环检测用 provides 声明不用值 | §15.3 Pass 2, §15.6 |
| 21 | Pass 3 | P0 | 主循环两次环检测（写 task 前二次检测） | §15.3 Pass 5 |
| - | Pass 3 | 优化 | 已 delivered provider 值没变则直接复制不 spawn | §15.9 |
| - | Pass 3 | 优化 | resolved.toml 不清空，覆盖式写入 | §15.2 |
| 22 | Pass 4 | P1 | verify 失败回 implementing（非 blocked）；blocked 只保留"等依赖"语义 | §3, §8.2 |
| 23 | Pass 4 | P1 | Domain Agent prompt 增加角色差异化指令 | §7 |
| 24 | Pass 4 | P1 | 跨层值变更检测：Pass 9 对 escalated resolved 条目做 seq 比对 | §15.3, §15.5 |
| 25 | Pass 4 | P2 | Pass 6 seq 比对移除，resolved 传递统一由 Pass 7 负责 | §15.3 |
| 26 | Pass 4 | P2 | PID 文件机制统一：per-node .forge/pid + Orchestrator 内存索引 | §5.3 |
| 27 | Pass 4 | P2 | Orchestrator 递归扫描所有活跃节点（非仅直接子节点） | §15.3 Pass 1 |
| 28 | Pass 5 | P1 | implementing → verifying 转换规则补全 | §3.2 |
| 29 | Pass 5 | P2 | §15.1 总览"子模块"改为"节点" | §15.1 |
| 30 | Pass 5 | P2 | §2.1 目录树移除 pids.toml（内存索引不持久化） | §2.1 |
| 31 | Pass 5 | P2 | §15.7 死模块清理改为 all_nodes 递归 | §15.7 |
| 32 | Pass 5 | P2 | Domain Agent 可写 tasks.toml（修复与 §2.3 矛盾） | §7 |
| 33 | Pass 6 | P1 | Pass 7/8 改为 all_nodes 递归（修复 children 未定义 + L2 失效） | §15.3 |
| 34 | Pass 6 | P1 | escalated.toml 增加 matched→resolved 状态流转 | §15.3 Pass 9 |
| 35 | Pass 6 | P1 | Pass 6 need_entries 作用域修复（从 needs 字典取 desc） | §15.3 |
| 36 | Pass 6 | P2 | planning → implementing 转换规则补全 | §3.2 |
| 37 | Pass 6 | P2 | Domain Agent blocked→implementing 行为定义 | §7 |
| 38 | Pass 7 | P1 | Pass 9 matched 分段增加 state=delivered 守卫 | §15.3 |
| 39 | Pass 7 | P1 | resolved.toml 写入格式转换（provides→resolved schema） | §5.2, §15.3 |
| 40 | Pass 7 | P2 | safe_read resolved.toml 加 or {} None 兜底 | §15.3 |
| 41 | Pass 7 | P2 | §3.2 补全 idle→assigned, idle→dead, assigned→planning | §3.2 |
| 42 | Pass 7 | P2 | 首次启动 prompt 增加初始状态指令 | §7 |
| 43 | Pass 7 | P2 | Pass 3/5 continue 改为只移除有环节点，不阻塞其他依赖组 | §15.3 |
| 44 | Pass 8 | P1 | Pass 9 value_changed resolved 格式转换（与 #39 同类） | §15.3 |
| 45 | Pass 8 | P1 | Domain Agent 不直接写 tasks.toml，由 Orchestrator 统一负责 | §7, §15.3 |
| 46 | Pass 8 | P2 | §3.2 补 Domain Agent blocked→implementing 转换规则 | §3.2 |
| 47 | Pass 8 | P2 | §3.1 blocked 语义扩展（含子节点等待） | §3.1 |
| 48 | Pass 8 | P2 | §4.6 移除 layer 0 budget（Orchestrator 不用 LLM） | §4.6 |
| 49 | Pass 8 | P2 | Pass 9 matched 分段加 provider 存活检查 | §15.3 |
| 50 | Pass 8 | P2 | §5.3 补 Orchestrator PID 重建机制描述 | §5.4 |
| 51 | Pass 9 | P1 | §15.9 provides→resolved 格式转换遗漏（#39 第五处同类） | §15.9 |
| 52 | Pass 9 | P1 | §15.5 移除"tools Orchestrator"，改为根 Orchestrator 统一处理 | §15.5 |
| 53 | Pass 9 | P2 | §5.1 spawn timeout 后应返回 None 而非 pid | §5.1 |
| 54 | Pass 9 | P2 | §6.2 delivered+alive 超时后 SIGKILL | §6.2 |
| 55 | Pass 9 | P2 | §6.3 stuck_threshold_heartbeats 配置项 | §4.6, §6.3 |
| 56 | Pass 9 | P2 | §15.5 补 escalate_to_parent() 函数定义 | §15.3 |
| 57 | Pass 9 | P2 | §6.2 blocked+dead 对 Domain Agent 需区分处理 | §6.2 |
| 58 | Pass 9 | P2 | §5.3 PID 文件写入时序注释 | §5.3 |
| 59 | Pass 9 | P2 | §6.5 墙钟检查实现方式说明 | §6.5 |
| 60 | Pass 10 | P1 | Pass 8 provides→resolved 格式转换遗漏（#39 第六处同类） | §15.3 |
| 61 | Pass 10 | P2 | mark_cycle_dead() 函数定义补充 | §15.3 |
| 62 | Pass 10 | P2 | has_cycle() 返回值增加 cycle_nodes 列表 | §15.3, §15.6 |
| 63 | Pass 10 | P2 | Pass 1 is_alive guard 处理 pid 文件不存在 | §15.3 |
| 64 | Pass 10 | P2 | §4.3 kill 消息 body 结构定义 | §4.3 |
| 65 | Pass 11 | P2 | §3.2 verifying→dead 移除"连续 N 轮"，统一用 max_retries | §3.2 |
| 66 | Pass 11 | P2 | §3 状态图 blocked→implementing 标签补全 | §3 |
| 67 | Pass 11 | P2 | Pass 9 matched 增加 dead provider 超时重置 | §15.3 |
| 68 | Pass 11 | P2 | find_provider / find_global_provider 定义明确化 | §15.3 |
| 69 | Pass 12 | P2 | §12 pids.toml 持久化描述修正为 .forge/pid 文件 | §12 |
| 70 | Pass 12 | P2 | §15.1"上报父层"改为 escalated.toml | §15.1 |
| 71 | Pass 12 | P2 | §15.2 轮询间隔 N 改为 scan_interval_sec | §15.2 |
| 72 | Pass 12 | P2 | §4.6 paths.pids 移除，加注释说明 | §4.6 |
| 73 | Pass 13 | P1 | §2.3 Domain 写权限移除 tasks.toml，与 §7 对齐 | §2.3 |
| 74 | Pass 13 | P2 | Pass 6 tasks.toml 读取加 or {} 兜底 | §15.3 |
| 75 | Pass 13 | P2 | §3.2 blocked→implementing 增加 needs.toml 为空边界 | §3.2 |
| 76 | Pass 14 | P1 | Pass 7 provides.toml 不存在时 None guard | §15.3 |
| 77 | Pass 14 | P2 | Pass 6 spawn 失败时移除 task 条目 | §15.3 |
| 78 | Pass 14 | P2 | Pass 9 escalated.toml 终态条目清理 | §15.3 |
| 79 | Pass 15 | P1 | Pass 6 spawn 后更新 provider.pid 防止重复 spawn | §15.3 |
| 80 | Pass 15 | P1 | Pass 9 pending→matched 写 entry.provider；matched→pending 清空 | §15.3 |
| 81 | Pass 16 | P1 | Domain Agent prompt 补充"请求 spawn 后写 state=blocked" | §7 |
| 82 | Pass 16 | P2 | §3.2 补 implementing→dead、blocked→dead 墙钟超时转换规则；状态图增加对应边 | §3 |
| 83 | Pass 17 | P1 | §15.9 delivered 优化条件逻辑修正（key 已存在且 seq 相同时冗余写入；seq 不同时误触发 spawn） | §15.9 |
| 84 | Pass 18 | P1 | Domain Agent prompt blocked→implementing 缺少显式写 state 指令（仅描述性说明） | §7 |
| 85 | Pass 19 | P1 | Module Agent prompt 缺少 implementing→verifying 和 verifying→implementing（重试）指令 | §7 |
| 86 | Pass 19 | P2 | Domain Agent prompt blocked 状态下需显式说明周期扫描 children_view | §7 |
| 87 | Pass 20 | P1 | node.toml [budget] 缺少 max_retries 字段（伪代码引用但无定义） | §4.1 |
| 88 | Pass 21 | P0 | §6.4/§15.7 依赖链传播未在 §15.3 主循环实现（dead provider 无值时 requester 永远 blocked） | §15.3 Pass 7b |
| 89 | Pass 22 | P2 | §15.1 机制总览流程图缺少 Pass 7b（依赖链传播）步骤 | §15.1 |
| 90 | Pass 23 | P2 | §4.5 eventbus.log 示例只覆盖 6 种 event type，补充至 15 种 | §4.5 |
| 91 | Pass 24 | P0 | Domain Agent spawn 请求机制断裂：Orchestrator 不读 outbox，增加 spawn_requests.toml + Pass 6b | §2.1, §2.3, §4.4, §7, §15.3 |
| 92 | Pass 25 | P1 | spawn_requests.toml 缺少 cwd 字段，Orchestrator 无法定位子节点目录 | §4.4, §15.3 Pass 6b |
| 93 | Pass 26 | P1 | §15.3 state.toml 路径缺少 .forge/ 前缀（Pass 1 / mark_cycle_dead / Pass 7b）+ escalated.toml 路径缺少 forge_root 前缀（Pass 7b / Pass 9 / Pass 9 收尾） | §15.3 |
| 94 | Pass 26 | P1 | §15.3 Pass 6 无条件 spawn dead provider，与 §5.2/§15.9 的 delivered 优化不一致 | §15.3 Pass 6 |
| 95 | Pass 26 | P1 | Domain Agent blocked→implementing 条件只检查 children_view，缺少 resolved.toml 检查（Domain Agent 可能同时等子节点+等依赖值） | §7 |
| 96 | Pass 26 | P1 | §7 L751 将 state.toml 错误归入 shared/ 目录（实际在 .forge/ 下） | §7 |
| 97 | Pass 26 | P1 | Pass 7b needs[node].keys() 缺少 null 守卫（needs[node] 可能为 None） | §15.3 Pass 7b |
| 98 | Pass 26 | P2 | collect_all_declared_nodes() 函数被 4 处引用但未定义 | §15.3 |
| 99 | Pass 26 | P2 | Pass 7 基于 Pass 1 旧快照，本轮 Pass 6 新 spawn 的 provider 滞后一个周期可见 | §15.3 Pass 7 |
| 100 | Pass 26 | P2 | spawn 被拒/失败未通过 inbox 通知请求方 Domain Agent，导致其无限等待 | §15.3 Pass 6b |
| 101 | Pass 27 | P1 | §3.2 Domain Agent blocked→implementing 转换规则未同步更新 resolved.toml 检查（与 §7 L747 修复不一致） | §3.2 |
| 102 | Pass 27 | P1 | Pass 8 `provides[provider][key]` 缺少 null 守卫（key 可能已被移除） | §15.3 Pass 8 |
| 103 | Pass 27 | P1 | `find_node` 自行收集 all_nodes（新建对象），与 Pass 1 dict key 不一致；改为接受 all_nodes 参数 | §15.3 |
| 104 | Pass 27 | P2 | §2.3 Domain Agent 读边界仅写"子节点 shared/"，遗漏 .forge/state.toml | §2.3 |
| 105 | Pass 27 | P2 | 唤醒 provider 无条件 seq 递增 → 级联 value_changed 扰民 | §7.2 |
| 106 | Pass 28 | P1 | outbox/ 目录无消费者：节点写的消息被静默丢弃；改为移除 outbox 引用，所有通信走专用文件通道 | §2.1, §4.3, §7.1 |
| 107 | Pass 28 | P1 | escalated.toml 重复条目：同 requester+key 被多次 escalate；escalate_to_parent 增加去重检查 | §15.3 |
| 108 | Pass 28 | P2 | state.toml `sequence` 字段未被 §15.3 使用；Pass 1 增加 state_seq 记录 | §15.3 Pass 1 |
| 109 | Pass 28 | P2 | eventbus.log 示例遗漏 dependency_escalated / escalation_failed / spawn_failed / suspected_stuck | §4.5 |
| 110 | Pass 28 | P2 | §2.1 目录树 L1 Domain Agent 缺少 verify.sh | §2.1 |
| 111 | Pass 28 | P2 | §5.1 spawn 超时硬编码 10s；提取为 forge.toml spawn_timeout_sec 配置项 | §4.6, §5.1 |
| 112 | Pass 29 | P1 | `all_nodes` 含 dead 节点但 Pass 2/4/6b/7/7b/8 无守卫访问 alive-only dict → 崩溃；Pass 1 后过滤 all_nodes 为仅 alive | §15.3 |
| 113 | Pass 29 | P2 | §6.2 父扫描 `(false,blocked)` 过早回收；改为交由 Pass 7b 做最终判定 | §6.2 |
| 114 | Pass 29 | P2 | `spawn_child` 引用不存在的 `child_def.budget.estimated` 字段；改为 `max_tokens` + `remaining_budget(child_def)` | §5.1 |
| 115 | Pass 29 | P2 | `collect_all_active_nodes()` 函数名误导（返回含 dead）；重命名为 `collect_all_declared_nodes` | §15.3 |
| 116 | Pass 30 | P2 | `remaining_budget(child_def)` 未定义检查范围；增加注释说明检查 global + per_layer 预算 | §5.1 |
| 117 | Pass 30 | P2 | Pass 9 matched→resolved 对新 spawn provider 存在同一周期延迟，未加注释说明 | §15.3 Pass 9 |
| 118 | Pass 30 | P2 | `scan_all_node_dirs` 与 `collect_all_declared_nodes` 功能重叠但命名不同；统一为后者 | §5.4 |
