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
- **文件状态总线**：每节点本地 `state.toml` + `inbox/outbox/` 目录即队列，项目级 `eventbus.log`
- **心跳 + TTL + 坏枝检测**：防"一根烂枝拖死全树"
- **`verify.sh` 自验闸门**：节点自己负责验证，父只认 `state="delivered"`

**vs 早期 Teams 方案的关键差异**：

| 维度 | 早期方案（已废弃） | 当前方案 |
|------|-------------------|---------|
| 拓扑 | 两级 Teams（Lead + Teammates） | N 级递归树（`max_depth` 可配） |
| 运行时 | Claude Code 内部 Teams 功能 | Claude Agent SDK + subprocess |
| 层级限制 | Claude Code 硬编码 2 级 | 用户自定义 `forge.toml.max_depth` |
| 通信 | `.coordination/inbox.toml` 单文件 | `.forge/inbox/` 目录即队列（并发安全） |
| 通知 | `TeammateIdle` / `SendMessage` hooks | 父扫子 `state.toml` + 事件总线 |
| 验证 | 项目级 `TaskCompleted` hook 调 `verify.sh` | 节点自验，父只认 `state="delivered"` |
| 隔离 | Claude Code `PreToolUse` 路径白名单 | SDK 级 `realpath` + cwd 锚点 |

---

## 1. 原始脑暴评估（为什么这么改）

> 输入：`/Users/gyanano/Documents/ObsidianBrain/0-Inbox/MCU嵌入式Claude Code开发环境设计【Draft】.md`

### 1.1 脑暴 10 条原文

| # | 原文摘要 | 判定 | 落地方式 |
|---|---------|------|---------|
| B1 | 项目按子模块拆分，可分层；每个文件夹一个 Agent | **采纳** | N 级树形拓扑，每节点 cwd = 对应目录 |
| B2 | 根 Agent 负责整体编译、版本管理、子模块协调与监督 | **采纳** | Orchestrator (L0) 角色 |
| B3 | 分层文件夹的 Agent 负责本层职责说明、子模块开发监督 | **采纳** | Domain Agent (L1) 角色 |
| B4 | 子模块 Agent 负责该模块的设计、开发、管理、维护 | **采纳** | Module Agent (L2+) 角色 |
| B5 | 子模块只能访问本目录；通过省 token 文件沟通 | **采纳** | SDK `realpath` 隔离 + `.forge/inbox/outbox` |
| B6 | 每文件夹有自己的 CLAUDE.md 记录长期方法论 | **采纳** | 每节点 cwd 下 `CLAUDE.md` 自动加载 |
| B7 | 父级只读对话文件做高价值交换 | **采纳** | 父只扫子 `state.toml`，不读子源码 |
| B8 | 根目录省 token 配置文件汇总所有子模块状态 | **采纳** | 项目级 `eventbus.log` + 各节点 `state.toml` |
| B9 | 每子 Agent 自己的 hooks + 验收流程 | **采纳** | 每节点 `verify.sh` + SDK per-invocation hooks |
| B10 | 树状管理：任务发散，交付物收敛 | **采纳** | 状态机 `implementing → verifying → delivered` |

**结论**：原始脑暴的 10 条**全部保留**，核心思想完整映射到 SDK 树形架构。

### 1.2 早期评估中已废弃的建议

早期版本（基于 Claude Code Teams 功能）曾建议：
- ❌ "压扁为两级拓扑" → 现在是 N 级，由 `forge.toml.max_depth` 控制
- ❌ "用 `TeamCreate` / `SendMessage` / `TeammateIdle`" → 现在用 subprocess + 文件状态总线
- ❌ "项目级 `TaskCompleted` hook 做闸门" → 现在节点自验，父只认 `state="delivered"`
- ❌ "`.claude/agents/*.md` 承载 per-agent 记忆" → 现在每节点 cwd 下 `CLAUDE.md` + `node.toml`

这些建议当时受限于"必须在 Claude Code 内部运行"的前提。用户明确指出 CortexForge 的目标是**下一代 IDE 级编排环境**，不是 Claude Code 套壳，因此绕过了这些限制。

### 1.3 脑暴中的"伪需求"（不需要专门机制）

| 伪需求 | 为什么不是问题 |
|--------|---------------|
| "省 token 的对话文件格式" | TOML 每条消息 < 1 KB。真正影响 token 的是节点数 × 每节点上下文长度。 |
| "每个 Agent 自己的 hooks" | SDK 调用时配置 per-invocation hooks 即可，本来就是分治的。 |
| "Agent 自带测试环境" | 每节点 cwd 下一份 `verify.sh` 就够了。 |
| "怎么通知父级"（脑暴原文的未知点） | 父扫子 `state.toml` 即可；事件总线兜底全局观测。 |

---

## 2. 拓扑

### 2.1 递归树结构

```
forge_root/                        # L0 — Orchestrator（长驻，用户主进程）
  forge.toml                       #   全局配置（max_depth / budget / heartbeat）
  .forge/
    eventbus.log                   #   项目级唯一事件总线（NDJSON 追加）
    pids.toml                      #   活跃节点 PID 表
  modules/
    firmware/                      # L1 — Domain Agent
      node.toml                    #   节点定义
      .forge/                      #   节点本地状态
        state.toml
        inbox/   outbox/
      submodules/
        hal-clock/                 # L2 — Module Agent
          node.toml
          verify.sh
          .forge/
            state.toml
            inbox/   outbox/
        bsp-uart/                  # L2
          ...
    tools/                         # L1 — Domain Agent
      submodules/
        flasher/                   # L2 — Module Agent
          ...
```

**关键设计原则**：节点不知道自己是"第几层"，只知道自己是否有 children（看 `node.toml` 里有没有声明 `children` 列表）。这样**机制可以无差别复用到任意深度**。

### 2.2 角色定义

每个节点都是一个独立 Claude 进程（SDK 程序化调用）。角色按"是否还有 children"区分，**机制完全相同**，只是 prompt / 权限不同。

| 层级 | 角色 | 是否长驻 | 关键职责 |
|------|------|----------|----------|
| L0 | Orchestrator | **长驻**（用户主进程） | 加载 `forge.toml`，spawn L1 节点，刷 eventbus，坏枝处理，消息路由 |
| L1 | Domain Agent | 任务期间驻留 | 把 domain 级任务拆给 submodules，spawn L2 节点，聚合 L2 的状态/交付物 |
| L2 | Module Agent | 任务期间驻留 | 实现该模块，跑 `verify.sh`，交付前不允许进入 `delivered` |
| L≥3 | Sub-module Agent | 同 L2 模式 | 同 L2 机制，直到 `max_depth` |

### 2.3 每节点的读写边界

| 维度 | Orchestrator (L0) | Domain (L1) | Module (L2+) |
|------|-------------------|-------------|--------------|
| **读** | `forge.toml`、所有节点 `state.toml`、`eventbus.log` | 本域全部文件 + 子节点 `state.toml` | 本模块全部文件 |
| **写** | `eventbus.log`、`pids.toml`、各节点 `inbox/` | 本域文件 + 子节点 `inbox/` | 本模块文件 + 自己 `state.toml` |
| **不读** | 模块源码（除非 inbox attach） | 兄弟域目录 | 兄弟模块目录 |
| **spawn** | 可以（唯一有 subprocess 权限的角色） | 通过 outbox 请求 Orchestrator spawn | 同 L1 |

### 2.4 记忆载体

| 载体 | 作用 | 谁写 |
|------|------|------|
| `<node>/CLAUDE.md` | 模块长期方法论、工具链假设、注意事项 | 节点自己 |
| `<node>/node.toml` | 节点静态定义（角色、children、budget、model） | 人写 |
| `<node>/.forge/state.toml` | 节点动态状态（状态机、进度、心跳、verify 结果） | 节点自己 |
| `forge.toml` | 全局配置（depth / budget / heartbeat / model per layer） | 人写 |
| `eventbus.log` | 全项目事件追加（不可变） | 所有节点 |

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
           │                            ┌──────────────┐
           │                            │ implementing │◄────┐
           │                            └──────┬───────┘     │ children
           │                                   │ self-done   │ all delivered
           │                                   ▼             │
           │                            ┌──────────┐         │
           │                            │ verifying├─────────┘
           │                            └──┬───┬───┘
           │                  verify pass  │   │  verify fail
           │                               ▼   ▼
           │                       ┌──────────┐  ┌─────────┐
           │                       │delivered │  │ blocked │
           │                       └──────────┘  └────┬────┘
           │                                          │ max_retries
           │                                          ▼
           │                                     ┌──────┐
           └────────────────────────────────────►│ dead │
                                                 └──────┘
```

### 3.1 状态语义

| 状态 | 含义 | 谁可触发进入 |
|------|------|-------------|
| `idle` | 节点已启动，等任务 | 自身（boot） |
| `assigned` | 收到 inbox 任务，尚未开始 | 自身（读到 inbox 新消息） |
| `planning` | 在分解任务 / 决定要不要 spawn 子节点 | 自身 |
| `implementing` | 在干活，可能在等子节点 | 自身 |
| `verifying` | 在跑 `verify.sh` | 自身 |
| `delivered` | 交付通过，产物在 `deliverables/` | 自身（verify pass 后） |
| `blocked` | 验证失败 / 缺依赖 / 等子节点修复 | 自身 |
| `dead` | 不可恢复（超时无心跳 / 反复失败 / 被父杀） | 自身**或**父节点 |

### 3.2 父子写权限规则

- **唯一的"父能写子"操作**：把子的状态强制改为 `dead`（枯枝清理）。除此之外，父对子的 `state.toml` 是只读。
- **节点不能写兄弟的 state.toml**。
- **节点不能直接 spawn 子进程**，必须通过 outbox 请求 Orchestrator spawn。

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

[budget]
max_tokens         = 200_000
max_wallclock_sec  = 1800
max_subprocess     = 4

[runtime]
model     = "claude-sonnet-4-6"     # 越深的节点用越便宜的模型
sdk       = "python"                # python | typescript | bash-headless
```

### 4.2 `<node>/.forge/state.toml` — 节点动态状态（节点本人写）

```toml
schema_version = 1

[state]
current        = "implementing"
entered_at     = 2026-04-26T12:00:00+08:00
last_heartbeat = 2026-04-26T12:01:30+08:00
sequence       = 42                 # 单调递增，父读到老 sequence 就忽略

[progress]
percent_self_estimate = 60
summary               = "在写 uart_tx 的 DMA 路径"
current_task_id       = "T-firmware-uart-001"

[children_view]
[[children_view.child]]
name           = "module-bsp-uart-tx"
state          = "verifying"
last_seen_at   = 2026-04-26T12:01:25+08:00

[verify]
last_run_at   = 2026-04-26T11:55:00+08:00
last_result   = "fail"
fail_summary  = "test_uart_tx_overrun timeout"
retry_count   = 1

[budget_used]
tokens_used        = 87_400
wallclock_sec_used = 320
```

### 4.3 `<node>/.forge/inbox/` 和 `outbox/` — 消息目录（目录即队列）

**为什么用目录而不是单文件**：并发安全。父或兄弟想给我发消息时，直接写一个**唯一文件名**进我的 `inbox/`，不需要文件锁。处理完移到 `inbox/processed/`（或删掉）。

文件名格式：`<unix_ts>-<from>-<msg_uuid>.toml`

```toml
schema_version = 1
id          = "msg-9f3a2b"
from        = "domain-firmware"
to          = "module-bsp-uart"
created_at  = 2026-04-26T12:02:00+08:00
kind        = "task" | "review" | "ack" | "kill" | "info" | "child_state"
ref_task_id = "T-firmware-uart-001"
priority    = "P1"

[body]
title = "实现 uart_tx DMA 路径"
text  = "多行 markdown。"
attachments = ["src/uart.c#L80-L120"]
```

**特殊消息 `kind="kill"`**：父发出后，子必须在 ≤5s 内进入 `dead` 状态；否则父把子 PID SIGKILL。

### 4.4 `forge_root/.forge/eventbus.log` — 项目级唯一事件总线

**追加写，从不修改**。每个节点状态切换 / spawn / 心跳超时 / 坏枝标记都写一行 NDJSON：

```jsonl
{"ts":"2026-04-26T12:00:00+08:00","node":"module-bsp-uart","event":"state","from":"assigned","to":"implementing","seq":41,"depth":2}
{"ts":"2026-04-26T12:00:05+08:00","node":"domain-firmware","event":"spawn","child":"module-bsp-uart","pid":48211,"depth":1}
{"ts":"2026-04-26T12:01:35+08:00","node":"orchestrator","event":"heartbeat_miss","subject":"module-bsp-uart","missed_for_sec":35,"action":"warn"}
{"ts":"2026-04-26T12:02:10+08:00","node":"orchestrator","event":"branch_dead","root_of_dead_branch":"module-bsp-uart","reason":"heartbeat_timeout"}
```

事件总线是**全局可观测性的命脉**——所有节点都打这一个文件，grep / jq 可重建任意节点的完整时间线。

### 4.5 `forge_root/forge.toml` — 根配置（全项目唯一）

```toml
[forge]
schema_version         = 1
max_depth              = 4            # 物理硬上限
max_total_nodes        = 64           # 全项目同时活跃节点上限
heartbeat_interval_sec = 15
heartbeat_timeout_sec  = 60
default_max_retries    = 3

[budget.global]
max_tokens_total        = 5_000_000
max_wallclock_total_sec = 14400

[budget.per_layer]
0 = { tokens = 500_000, wallclock_sec = 7200, model = "claude-opus-4-7" }
1 = { tokens = 300_000, wallclock_sec = 3600, model = "claude-sonnet-4-6" }
2 = { tokens = 200_000, wallclock_sec = 1800, model = "claude-sonnet-4-6" }
3 = { tokens = 100_000, wallclock_sec = 900,  model = "claude-haiku-4-5" }

[paths]
event_bus = ".forge/eventbus.log"
pids      = ".forge/pids.toml"
```

---

## 5. Spawn 协议

**单一 spawn 函数，所有层共用**。L0→L1 与 L1→L2 走同一段代码——这就是"递归对称"。

```
function spawn_child(parent_node, child_def):
    # ── 1. 预检查 ──
    if parent_node.depth + 1 > forge.max_depth:
        emit_event("spawn_refused", reason="max_depth"); return None
    if active_node_count() + 1 > forge.max_total_nodes:
        emit_event("spawn_refused", reason="max_total_nodes"); return None
    if remaining_budget() < child_def.budget.estimated:
        emit_event("spawn_refused", reason="budget"); return None

    # ── 2. 准备子进程环境 ──
    env = {
        "FORGE_NODE_NAME":  child_def.name,
        "FORGE_NODE_DEPTH": str(parent_node.depth + 1),
        "FORGE_PARENT":     parent_node.name,
        "FORGE_ROOT":       forge.root_path,
        "CLAUDE_API_KEY":   inherited_from_orchestrator,
    }

    # ── 3. 起子进程（SDK 调用） ──
    pid = subprocess.spawn(
        cmd    = ["python", "forge_node.py"],
        cwd    = child_def.cwd,
        env    = env,
        stdout = "<cwd>/.forge/stdout.log",
        stderr = "<cwd>/.forge/stderr.log",
    )

    # ── 4. 注册 PID，写事件 ──
    pids_table.add(child_def.name, pid)
    emit_event("spawn", child=child_def.name, pid=pid, depth=parent_node.depth+1)

    # ── 5. 等子的初始 state.toml 写出 ──
    wait_for_state_file(child_def.cwd, timeout_sec=10)
    return pid
```

**关键点**：
- 子是 `forge_node.py` 这种**通用启动器**（读 `node.toml`，按角色装 prompt，起 SDK 主循环），不是每节点一份脚本。
- 子进程的 cwd = 子的目录，天然提供 per-node 权限锚点。
- **节点本身没有 subprocess 权限**；spawn 必须走 Orchestrator（通过 outbox 请求），保证所有 spawn 走同一个预算/depth/总数检查。

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
        if now - child.last_heartbeat > forge.heartbeat_timeout_sec:
            child.state := "dead"   (父 override)
            emit_event("heartbeat_miss" → "branch_dead")
            kill_subtree(child)     (递归杀死该子的所有 descendants)
```

### 6.3 进度 vs 活着（关键！）

**心跳只证明"还活着"，不证明"还在进展"**。叠加**进度签名**机制：

- 节点每写心跳时，顺带刷 `progress.summary` 与 `progress.percent_self_estimate`。
- 父记录子 `progress.summary` 的滚动 hash；若 N 个连续心跳间 hash 完全没变，记为 `event="suspected_stuck"`，人（或 Orchestrator）介入决定是否杀。

### 6.4 坏枝传播规则

- 子被标 `dead` → 父在自己 `children_view` 标记 → 父决定：
  - **可降级完成**（子的输出可选）：父自己继续，把分支结果标 `partial`；
  - **不可降级**：父也进 `blocked`，向上汇报；爷爷再决定。
- 这种"按需向上传播"避免一个无关紧要的子节点拖死整个项目。

### 6.5 自杀闸门

- 节点本人若 `verify` 连续失败达到 `max_retries`，自己进入 `blocked`；
- 在 `blocked` 状态下若 `max_wallclock_sec` 也耗尽，自己写 `state="dead"` 然后 exit。

---

## 7. Agent Prompt 模板

通用启动器 `forge_node.py` 在调 SDK 前组装：

```
你是 CortexForge 编排树中的一个节点。

[身份]
- 节点名: {FORGE_NODE_NAME}
- 角色:   {role}    (orchestrator/domain/module/submodule)
- 深度:   {FORGE_NODE_DEPTH}
- 父节点: {FORGE_PARENT}
- 工作目录: {cwd}

[硬约束]
1. 你只能读写自己 cwd 之下的文件（realpath 后）。
2. 你不知道、也不需要知道兄弟节点的存在。所有跨节点信息走父节点。
3. 与外界的全部沟通走文件:
   - 收任务: ./.forge/inbox/*.toml
   - 写状态: ./.forge/state.toml（覆盖式）
   - 发消息: ./.forge/outbox/*.toml（由 Orchestrator 路由）
4. 每 {heartbeat_interval_sec}s 必须刷一次 state.toml 的 last_heartbeat。
5. 你的 token 预算 {max_tokens}，墙钟预算 {max_wallclock_sec}s，超即自我终止。

[决策权]
- 你可以在 children.declared 范围内请求 spawn 子节点（通过写 outbox 给 Orchestrator）。
- 你不可以创造未在 node.toml 声明的 children。

[交付]
- 完成后跑 ./verify.sh；退出码 0 = 通过；非 0 = 失败。
- verify 通过后 state="delivered"，产物落到 ./deliverables/。
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
3. 失败：写 `state.toml` → `retry_count++`，若达到 `max_retries` → `state="blocked"`
4. 父扫到 `state="delivered"` 才认账，附 `artifacts.toml` 的 hash 防 TOCTOU

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
| Spawn 子进程 | 节点本身**没有** subprocess 权限；唯一例外是写 outbox 请求 Orchestrator spawn |

**为什么不用 worktree 做强隔离**：嵌入式项目共享 HAL / 链接脚本 / 公共 inc 的需求高频，worktree 反而带来同步噩梦。Hook 校验 + cwd 锚点已经够强；真要硬隔离的极少数节点单独配 `isolation: worktree`。

---

## 10. 可选增强（按需开启）

### 10.1 MCP server 封装工具链

- 将编译（`make`、`west build`、`idf.py`）、烧录（`openocd`、`esptool`、`pyocd`）、HIL 测试封装为 MCP tools
- 每个节点通过 MCP 调用，权限统一在 MCP server 层管控
- 更换厂商 / 工具链版本只改 MCP server，不动 agent 定义

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

---

## 12. 真坑清单（必须设计应对）

| 真坑 | 应对 |
|------|------|
| **成本爆炸** | `forge.toml` 的 `max_total_nodes` + `budget.per_layer` + 越深用越便宜的模型 |
| **递归宽度滥用** | spawn 走 Orchestrator 单点检查 |
| **心跳但卡死** | §6.3 进度签名 hash 比对 |
| **状态文件并发** | inbox/outbox 用目录即队列；state.toml 用 atomic rename |
| **崩溃恢复** | `pids.toml` 持久化；Orchestrator 启动时扫 state.toml 恢复 |
| **schema 演进** | 每个 toml 带 `schema_version`；启动时校验兼容性 |
| **坏枝阻塞兄弟** | §6.4 按需传播；父决定可降级 vs 升级阻塞 |
| **debugging 黑盒** | §4.4 项目级 eventbus，grep/jq 可重建任意节点时间线 |
| **token 预算硬切** | SDK wrapper 包装 token 计数；达到 budget 就 abort |
| **节点绕过 spawn** | 节点没 subprocess 权限；spawn 必须走 Orchestrator |
| **verify TOCTOU** | artifacts.toml hash + state.toml seq |
| **模型升级 prompt 漂移** | node.toml 锁 model ID；升级时按 layer 灰度 |

### 待 MVP 实跑验证的未知项

- 节点 boot 启动延迟叠加（每层 SDK 初始化 + first-token）
- 跨节点 token 重复（同一段代码被多层读进上下文）
- SDK subprocess 里 API 重连稳定性
- 事件总线高并发下的写竞争（NDJSON O_APPEND 跨平台差异）

---

## 13. MVP 最小成功标准

以下 8 条全部通过即可证明本设计**可行**：

1. **3 层 spawn 跑通**：L0 起 1 个 L1，L1 起 2 个 L2，所有节点 state.toml 正常生命周期
2. **递归对称**：L0→L1 和 L1→L2 走同一段 `spawn_child` 代码，无 layer-specific 分支
3. **心跳超时杀枝**：人为让一个 L2 节点 sleep 超过 timeout，父正确标 dead，事件总线有完整记录，兄弟不受影响
4. **坏枝传播**：L2 verify 连续失败到 max_retries，L1 收敛到 partial 或 blocked 的决策正确
5. **预算切断**：L2 token budget 设很小，该节点正确 abort，父正常处理
6. **崩溃恢复**：杀掉 Orchestrator，重启，state 能从文件正确恢复
7. **事件总线可重建**：从 eventbus.log 单文件能 grep 出任意节点的完整生命周期
8. **总成本可控**：跑一个真实小任务，total token 不超过 `forge.budget.global.max_tokens_total`

---

## 14. 与原始脑暴对照表

| 脑暴 | 本架构落点 |
|------|-----------|
| B1 树状分层 | §2 拓扑（N 级，`max_depth` 可配） |
| B2 根 Agent 多职责 | §2.2 Orchestrator (L0) |
| B3 分层 Agent | §2.2 Domain Agent (L1) |
| B4 子模块 Agent | §2.2 Module Agent (L2+) |
| B5 子模块只能本目录 + 文件沟通 | §4 文件协议 + §9 权限模型 |
| B6 每 Agent 自己的 CLAUDE.md | §2.4 记忆载体（per-folder CLAUDE.md） |
| B7 父级只读对话文件 | §2.3「不读子源码」纪律 + §4.2 state.toml |
| B8 根目录省 token 配置 | §4.5 forge.toml + §4.4 eventbus.log |
| B9 自带 hook + 验收 + 通知父级 | §8 verify.sh + §6 心跳/扫描 |
| B10 任务发散 → 交付收敛 | §3 状态机 + §10.2 集成顺序 |
