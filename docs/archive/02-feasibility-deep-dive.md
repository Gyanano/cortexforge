# 可行性深挖:3 层递归 MVP 设计

> 目标:**用一个具体的 3 层递归场景**,把 N 级树形 Agent 编排器的所有关键机制走一遍,
> 暴露真问题,过滤伪需求。**本文不写实现代码**——只出设计、协议、流程、伪代码。
>
> 上游:[`00-evaluation.md`](./00-evaluation.md)、[`01-architecture.md`](./01-architecture.md)
> (v1 Teams 版,本文路线确认后会 supersede 01)。
> 立场:Claude Agent SDK + subprocess + 文件状态总线;Claude Code 是开发者 IDE,**不**是
> 这个项目的运行时。

---

## 1. MVP 场景设定(为什么是这 3 层)

为了让递归是"真递归"而不是"硬凑两层",MVP 选这个目录形态:

```
forge_root/                        # Layer 0  (Orchestrator,长驻)
  forge.toml                       #   全局配置
  .forge/
    eventbus.log                   #   全项目唯一事件总线
    pids.toml                      #   活跃节点 PID 表
  modules/
    firmware/                      # Layer 1  (Domain Agent,短驻)
      node.toml                    #   节点定义
      .forge/                      #   节点本地状态目录
        state.toml
        inbox/   outbox/
      submodules/
        hal-clock/                 # Layer 2  (Module Agent,实际干活)
          node.toml
          verify.sh
          .forge/
            state.toml
            inbox/   outbox/
        bsp-uart/                  # Layer 2
          ...
    tools/                         # Layer 1
      submodules/
        flasher/                   # Layer 2
          ...
```

**为什么是真递归**:Layer 0 spawn Layer 1 用同一套机制;Layer 1 spawn Layer 2 也用同一套机
制;两层 spawn 调用是**完全对称**的代码路径——这就证明了机制可以**继续往下递归**(只要根
配置 `max_depth` 允许)。

3 层是最小可证明递归的层数。2 层无法证明(因为 2 层等于"父 + 一组 leaves",看不出递归)。

---

## 2. 节点角色(每层做什么)

每个节点都是一个独立 Claude 进程(SDK 程序化调用)。角色按"是否还有 children"区分,**机制
完全相同**,只是 prompt / 权限不同。

| 层级 | 角色 | 是否长驻 | 关键职责 |
|------|------|----------|----------|
| L0 | Orchestrator | **长驻**(用户主进程) | 加载 `forge.toml`,spawn L1 节点,刷 eventbus,坏枝处理 |
| L1 | Domain Agent | 任务期间驻留 | 把 domain 级任务拆给 submodules,spawn L2 节点,聚合 L2 的状态/交付物 |
| L2 | Module Agent | 任务期间驻留 | 实现该模块,跑 `verify.sh`,在交付前不允许进入 `delivered` |
| L≥3 | Sub-module Agent | 同 L2 模式 | 同 L2 机制,直到 `max_depth` |

**关键设计原则**:节点不知道自己是"第几层",只知道自己是否有 children(看 `node.toml` 里
有没有声明 `children` 列表)。这样**机制可以无差别复用到任意深度**。

---

## 3. 状态机(canonical states)

每个节点本地有且只有一份 `state.toml`,代表自己当前状态。**写入者唯一(自己)**;
父节点只读,不写。

```
        ┌──────┐  parent assigns task   ┌──────────┐
        │ idle ├──────────────────────► │ assigned │
        └──┬───┘                        └────┬─────┘
           │ TTL expired (no                 │ self-plan
           │ heartbeat from owner)           ▼
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
           │                                          │ N retries
           │                                          ▼
           │                                     ┌──────┐
           └────────────────────────────────────►│ dead │
                                                 └──────┘
```

**状态语义**(全部小写下划线):

| 状态 | 含义 | 谁可触发进入 |
|------|------|-------------|
| `idle` | 节点已启动,等任务 | 自身(boot) |
| `assigned` | 收到 inbox 任务,尚未开始 | 自身(读到 inbox 新消息) |
| `planning` | 在分解任务 / 决定要不要 spawn 子节点 | 自身 |
| `implementing` | 在干活,可能在等子节点 | 自身 |
| `verifying` | 在跑 `verify.sh` | 自身 |
| `delivered` | 交付通过,产物在 `deliverables/` | 自身(verify pass 后) |
| `blocked` | 验证失败 / 缺依赖 / 等子节点修复 | 自身 |
| `dead` | 不可恢复(超时无心跳 / 反复失败 / 被父杀) | 自身 **或** 父节点 |

**唯一的"父能写子"操作**:把子的状态强制改为 `dead`(枯枝清理)。除此之外,父对子的
`state.toml` 是只读。

---

## 4. 文件协议(节点本地三件套 + 项目级一件套)

### 4.1 `<node>/node.toml` — 节点定义(静态,人写)

```toml
[node]
name      = "module-bsp-uart"
role      = "module"                # orchestrator / domain / module / submodule(语义层,机制相同)
cwd       = "modules/firmware/submodules/bsp-uart"
parent    = "domain-firmware"       # 根节点为空字符串
depth     = 2                       # 由父在 spawn 时校验,不要手维护

[children]
# 没有就留空。有就显式列出,父在 boot 时按这张表 spawn。
declared = ["module-bsp-uart-tx", "module-bsp-uart-rx"]   # 例:再下一层
spawn_strategy = "lazy"             # eager | lazy(子任务到达时再 spawn)

[budget]
max_tokens         = 200_000        # 单节点单生命周期 token 上限
max_wallclock_sec  = 1800           # 单节点单生命周期墙钟上限
max_subprocess     = 4              # 同时活跃的子进程上限

[runtime]
model     = "claude-sonnet-4-6"     # 越深的节点用越便宜的模型
sdk       = "python"                # python | typescript | bash-headless
```

### 4.2 `<node>/.forge/state.toml` — 节点动态状态(节点本人写)

```toml
schema_version = 1                  # 协议版本,父子双方都验

[state]
current        = "implementing"
entered_at     = 2026-04-26T12:00:00+08:00
last_heartbeat = 2026-04-26T12:01:30+08:00     # 必须每 heartbeat_interval 刷新
sequence       = 42                              # 单调递增,父读到老 sequence 就忽略

[progress]
percent_self_estimate = 60
summary               = "在写 uart_tx 的 DMA 路径"
current_task_id       = "T-firmware-uart-001"

[children_view]                     # 节点对自己 children 的当前认知(从子的 state 摘出)
[[children_view.child]]
name           = "module-bsp-uart-tx"
state          = "verifying"
last_seen_at   = 2026-04-26T12:01:25+08:00

[verify]
last_run_at   = 2026-04-26T11:55:00+08:00
last_result   = "fail"
fail_summary  = "test_uart_tx_overrun timeout"
retry_count   = 1                   # 连续 fail 次数,达到 max_retries → blocked → dead

[budget_used]
tokens_used        = 87_400
wallclock_sec_used = 320
```

### 4.3 `<node>/.forge/inbox/` 和 `outbox/` — 消息目录(目录即队列)

**为什么用目录而不是单文件**:并发安全。父或兄弟想给我发消息时,直接写一个**唯一文件名**
进我的 `inbox/`,不需要文件锁。我自己处理完就把它移到 `inbox/processed/`(或删掉)。

文件名格式:`<unix_ts>-<from>-<msg_uuid>.toml`

单条消息内容:

```toml
schema_version = 1
id          = "msg-9f3a2b"
from        = "domain-firmware"     # 节点名,不是 PID
to          = "module-bsp-uart"
created_at  = 2026-04-26T12:02:00+08:00
kind        = "task" | "review" | "ack" | "kill" | "info" | "child_state"
ref_task_id = "T-firmware-uart-001"
priority    = "P1"

[body]
title = "实现 uart_tx DMA 路径"
text  = """
多行 markdown。
"""
attachments = ["src/uart.c#L80-L120"]
```

**特殊消息 `kind="kill"`**:父发出后,子必须在 ≤5s 内进入 `dead` 状态;否则父把子 PID
SIGKILL。

### 4.4 `forge_root/.forge/eventbus.log` — 项目级唯一事件总线

**追加写,从不修改**。每个节点状态切换 / spawn / 心跳超时 / 坏枝标记都写一行 NDJSON
(每行一个 JSON 对象,grep / jq 友好):

```jsonl
{"ts":"2026-04-26T12:00:00+08:00","node":"module-bsp-uart","event":"state","from":"assigned","to":"implementing","seq":41,"depth":2}
{"ts":"2026-04-26T12:00:05+08:00","node":"domain-firmware","event":"spawn","child":"module-bsp-uart","pid":48211,"depth":1}
{"ts":"2026-04-26T12:01:35+08:00","node":"orchestrator","event":"heartbeat_miss","subject":"module-bsp-uart","missed_for_sec":35,"action":"warn"}
{"ts":"2026-04-26T12:02:10+08:00","node":"orchestrator","event":"branch_dead","root_of_dead_branch":"module-bsp-uart","reason":"heartbeat_timeout"}
```

**为什么必须有事件总线**:N 层深处出错时,scattered 日志根本调试不了。事件总线是
**全局可观测性的命脉**——所有节点都打这一个文件,加 correlation id 串联,可以重建任意节点
的完整时间线。

### 4.5 `forge_root/forge.toml` — 根配置(全项目唯一)

```toml
[forge]
schema_version    = 1
max_depth         = 4               # 物理硬上限,任何 spawn 超过即拒绝
max_total_nodes   = 64              # 全项目同时活跃节点上限(挡住宽度爆炸)
heartbeat_interval_sec = 15         # 子心跳频率
heartbeat_timeout_sec  = 60         # 父等多久没心跳就标 dead
default_max_retries    = 3          # 单节点 verify 连续失败几次进 blocked

[budget.global]
max_tokens_total          = 5_000_000
max_wallclock_total_sec   = 14400

[budget.per_layer]
# 越深的节点配越紧的预算,防止递归深度滥用
0 = { tokens = 500_000, wallclock_sec = 7200, model = "claude-opus-4-7" }
1 = { tokens = 300_000, wallclock_sec = 3600, model = "claude-sonnet-4-6" }
2 = { tokens = 200_000, wallclock_sec = 1800, model = "claude-sonnet-4-6" }
3 = { tokens = 100_000, wallclock_sec = 900,  model = "claude-haiku-4-5" }

[paths]
event_bus = ".forge/eventbus.log"
pids      = ".forge/pids.toml"
```

---

## 5. Spawn 协议(父怎么起子)

**单一 spawn 函数,所有层共用**。伪代码(语言无关):

```
function spawn_child(parent_node, child_def):
    # ── 1. 预检查(全部失败即拒绝 spawn) ──
    if parent_node.depth + 1 > forge.max_depth:
        emit_event("spawn_refused", reason="max_depth")
        return None
    if active_node_count() + 1 > forge.max_total_nodes:
        emit_event("spawn_refused", reason="max_total_nodes")
        return None
    if remaining_budget() < child_def.budget.estimated:
        emit_event("spawn_refused", reason="budget")
        return None

    # ── 2. 准备子进程环境 ──
    env = {
        "FORGE_NODE_NAME":  child_def.name,
        "FORGE_NODE_DEPTH": str(parent_node.depth + 1),
        "FORGE_PARENT":     parent_node.name,
        "FORGE_ROOT":       forge.root_path,
        "CLAUDE_API_KEY":   inherited_from_orchestrator,
    }
    cwd = child_def.cwd        # 物理隔离:子的 cwd 是它自己的目录

    # ── 3. 起子进程(SDK 调用,不是 Claude Code) ──
    pid = subprocess.spawn(
        cmd  = ["python", "forge_node.py"],   # 或 typescript/bash 实现
        cwd  = cwd,
        env  = env,
        stdout = "<cwd>/.forge/stdout.log",
        stderr = "<cwd>/.forge/stderr.log",
    )

    # ── 4. 注册 PID,写事件 ──
    pids_table.add(child_def.name, pid)
    emit_event("spawn", child=child_def.name, pid=pid, depth=parent_node.depth+1)

    # ── 5. 等子的初始 state.toml 写出(boot 完成证明) ──
    wait_for_state_file(child_def.cwd, timeout_sec=10)
    return pid
```

**关键点**:
- **单一函数被 L0、L1、L2 复用**——这就是"递归机制"。L0 spawn L1 与 L1 spawn L2 是同一段代码。
- 子进程的 cwd = 子的目录。这给后续 **per-node 权限隔离** 提供了天然 anchor:每个子的 SDK
  hook 只允许 read/write 在自己 cwd 之下(realpath + 前缀匹配)。
- 子是 `forge_node.py` 这种**通用启动器**(读 `node.toml`,按角色装 prompt,起 SDK 主循环),
  不是每节点一份脚本。

---

## 6. 心跳 + TTL + 坏枝传播(防"一根烂枝拖死全树")

### 6.1 心跳协议
- 每节点每 `forge.heartbeat_interval_sec` 秒**必须**更新自己 `state.toml` 的
  `last_heartbeat` 字段(原子写:写到 `state.toml.tmp` 然后 `rename`)。
- 节点的 SDK 主循环里加一个 watchdog 协程,即使模型在长 inference 也定期写心跳。

### 6.2 父的扫描循环
- 父每 `heartbeat_interval_sec` 秒扫描自己所有 children 的 `state.toml`:
  ```
  if now - child.last_heartbeat > forge.heartbeat_timeout_sec:
      child.state := "dead"   (父 override)
      emit_event("heartbeat_miss" → "branch_dead")
      kill_subtree(child)     (递归杀死该子的所有 descendants)
  ```

### 6.3 进度 vs 活着(关键!)
**心跳只证明"还活着",不证明"还在进展"**。所以叠加一个**进度签名**机制:

- 节点每写心跳时,要顺带刷 `progress.summary` 与 `progress.percent_self_estimate`。
- 父记录子 `progress.summary` 的滚动 hash;若 N 个连续心跳间 hash 完全没变,记为
  `event="suspected_stuck"`,人(或 Orchestrator)介入决定是否杀。

这一步用户脑暴里没提,但**没有它,光有心跳会被无限循环的 agent 骗**。

### 6.4 坏枝传播规则
- 子被标 `dead` → 父在自己 `children_view` 标记 → 父决定:
  - **可降级完成**(子的输出可选):父自己继续,把分支结果标 `partial`;
  - **不可降级**:父也进 `blocked`,向上汇报;爷爷再决定。
- 这种"按需向上传播"避免一个无关紧要的子节点拖死整个项目。

### 6.5 自杀闸门(节点自己也能宣告 dead)
- 节点本人若 `verify` 连续失败达到 `max_retries`,自己进入 `blocked`;
- 在 `blocked` 状态下若 `max_wallclock_sec` 也耗尽,自己写 `state="dead"` 然后 exit。
- 这避免父被骗:子明明知道自己废了,却假装还在试。

---

## 7. 每节点 Agent prompt 模板(注入身份与边界)

通用启动器 `forge_node.py` 在调 SDK 前组装:

```
你是 CortexForge 编排树中的一个节点。

[身份]
- 节点名: {FORGE_NODE_NAME}
- 角色:   {role from node.toml}    (orchestrator/domain/module/submodule)
- 深度:   {FORGE_NODE_DEPTH}
- 父节点: {FORGE_PARENT}
- 工作目录: {cwd}

[硬约束]
1. 你只能读写自己 cwd 之下的文件。任何超出 cwd 的路径(realpath 后)立即拒绝。
2. 你不知道、也不需要知道兄弟节点的存在。所有跨节点信息走父节点。
3. 与外界的全部沟通走文件:
   - 收任务: ./.forge/inbox/*.toml
   - 写状态: ./.forge/state.toml(覆盖式;每次有意义进展刷新)
   - 发消息: ./.forge/outbox/*.toml(由 orchestrator 路由)
4. 每 {heartbeat_interval_sec}s 必须刷一次 state.toml 的 last_heartbeat。
5. 你的 token 预算 {max_tokens},墙钟预算 {max_wallclock_sec}s,超即自我终止。

[决策权]
- 你可以在 children.declared 范围内 spawn 子节点(通过写 outbox 给 orchestrator)。
- 你不可以创造未在 node.toml 声明的 children;需要新 children 时,向父发 kind="info"
  的消息请求扩列。

[交付]
- 完成后跑 ./verify.sh;退出码 0 = 通过;非 0 = 失败,写回 inbox 并自动重试至 max_retries。
- verify 通过后 state="delivered",产物落到 ./deliverables/。
```

**注意**:节点**不直接 spawn 子进程**,而是写 outbox 让 orchestrator 路由——这是为了保证
**所有 spawn 走同一个预算/depth/总数检查**,避免节点自作主张爆配额。

---

## 8. 权限模型(per-node 真隔离)

每个节点的 SDK invocation 配置:

| 维度 | 实现 |
|------|------|
| 文件读 | SDK PreToolUse hook:`realpath(target).startswith(cwd)`,否则 deny |
| 文件写 | 同上 |
| Bash | 节点 `node.toml` 里 `[bash_allowlist]` 显式列;hook 校验命令前缀 |
| 网络 | 默认 deny;特定节点(如 `module-fetcher`)在 node.toml 里显式开 |
| Spawn 子进程 | 节点本身**没有** subprocess 权限;唯一例外是写 outbox 请求 orchestrator spawn |

**为什么不用 worktree 做强隔离**:嵌入式项目共享 HAL/链接脚本/公共 inc 的需求高频,worktree
反而带来同步噩梦。Hook 校验 + cwd 锚点已经够强;真要硬隔离的极少数节点单独配
`isolation: worktree`。

---

## 9. 验证与交付(verify.sh 闸门)

不变:每节点根目录提供 `verify.sh`,退出码 0/2 同 v1 设计。

**新增**:
- 节点完成 verify 后**主动**把摘要写进自己 `state.toml` 的 `[verify]` 段,父扫到
  `state="delivered"` 才认账(不再依赖项目级 hook 来调 verify——节点自己负责)。
- Orchestrator 在 emit `state="delivered"` 事件时附 `artifacts.toml` 的 hash,父节点据此
  确认拿到的是 **verify 通过那一刻的产物**(防 TOCTOU)。

---

## 10. 真坑 vs 伪需求

### 10.1 伪需求(脑暴里强调过但不是真问题)

| 伪需求 | 为什么不是问题 |
|--------|---------------|
| "省 token 的对话文件格式" | TOML / JSON 都行。每条消息一般 < 1 KB,不影响整体 token 经济性。**真正影响 token 的是节点数 × 每节点上下文长度,不是消息格式。** |
| "每个 Agent 自己的 CLAUDE.md" | 节点 cwd 下放一份 CLAUDE.md 即可,Claude 自动加载;不需要专门机制。 |
| "每个 Agent 自己的 hooks" | SDK 调用时配置 per-invocation hooks 即可,本来就是分治的。 |
| "Agent 自带测试环境" | 每节点 cwd 下一份 `verify.sh` 就够了,不需要"环境"抽象。 |
| "怎么通知父级"(脑暴 v1 的未知点) | 父扫子的 `state.toml` 即可;不需要 push;事件总线兜底全局观测。 |

### 10.2 真坑(必须设计应对)

| 真坑 | 应对 |
|------|------|
| **成本爆炸** | 根 `forge.toml` 的 `max_total_nodes` + `budget.per_layer` token 上限 + 越深用越便宜的模型 |
| **递归宽度滥用** | spawn 走 orchestrator 单点检查;节点不能绕过 |
| **心跳但卡死** | §6.3 进度签名 hash 比对;suspected_stuck 事件 + 人工介入 |
| **状态文件并发** | inbox/outbox 用**目录即队列**(每消息一个文件);state.toml 用 atomic rename 写 |
| **崩溃恢复** | `pids.toml` 持久化;orchestrator 启动时扫所有 state.toml `last_heartbeat`,死的标 dead,活的尝试 reattach |
| **schema 演进** | 每个 toml 必须带 `schema_version`;orchestrator 启动时校验全树版本兼容,不兼容拒启动并提示迁移 |
| **坏枝阻塞兄弟** | §6.4 按需传播;父决定可降级完成 vs 升级阻塞,而不是一刀切 |
| **debugging 黑盒** | §4.4 项目级单一 eventbus,所有事件 NDJSON 追加,grep/jq 可重建任意节点时间线 |
| **token 预算硬切** | SDK wrapper 包装 token 计数;接近 budget 时主动 emit warning,达到 budget 就 abort 当前节点 |
| **节点自起 spawn 绕过预算** | §7 节点本身没 subprocess 权限;spawn 必须走 orchestrator |
| **verify TOCTOU** | §9 artifacts.toml hash + state.toml seq |
| **Claude 模型升级造成 prompt 漂移** | node.toml 锁 model ID;升级时按 layer 灰度 |

### 10.3 我现在还无法判断是真坑还是伪需求(需要 MVP 实跑才知道)

- **节点 boot 启动延迟叠加**:每层 spawn 子要等 SDK 初始化 + 模型 first-token,3 层串行可能
  几十秒。是否需要预热池?待 MVP 测。
- **跨节点 token 重复**:同一段代码可能被 L1、L2 都读进上下文。是否需要"共享 read cache"
  (节点把读到的内容摘要回 outbox,父复用而非重读)?待 MVP 测。
- **SDK 在 subprocess 里反复重连 API 是否稳定**:网络抖动放大 N 倍。
- **事件总线在高并发节点下的写竞争**:NDJSON 追加在多进程下需要 O_APPEND 原子保证,跨平台
  差异如何?

---

## 11. MVP 实跑前的最小成功标准(下一阶段验证用)

如果未来真的写一个 MVP,以下最小集合通过即可证明本设计**可行**:

1. **3 层 spawn 跑通**:L0 起 1 个 L1,L1 起 2 个 L2,所有节点 state.toml 正常生命周期。
2. **递归对称**:L0→L1 和 L1→L2 走同一段 `spawn_child` 代码,无 layer-specific 分支。
3. **心跳超时杀枝**:人为让一个 L2 节点 sleep 超过 timeout,父正确标 dead,事件总线有完整
   记录,兄弟 L2 不受影响。
4. **坏枝传播**:让一个 L2 节点 verify 连续失败到 max_retries,L1 收敛到 partial 或 blocked
   的决策正确。
5. **预算切断**:人为把一个 L2 节点 token budget 设很小,该节点正确 abort,父正常处理。
6. **崩溃恢复**:杀掉 orchestrator,重启,state 能从文件正确恢复。
7. **事件总线可重建**:从 eventbus.log 单文件能 grep 出任意节点的完整生命周期。
8. **总成本可控**:跑一个真实小任务,total token 不超过 `forge.budget.global.max_tokens_total`。

**不通过的话需要回头改设计**——所以这步是真验证,不是仪式。

---

## 12. 与 v1 文档的关系

- 本文 **不**取代 `01-architecture.md`,而是为它的 v2 重写提供**论证基础**。
- 本文若被采纳,后续动作:
  1. 标记 `01-architecture.md` 为 v1(Teams 快速版,fallback 用途);
  2. 写 `01-architecture-v2.md`(SDK 树形版),把本文的设计正式化、补图、补落地步骤;
  3. 更新 `CLAUDE.md` 顶层拓扑约定到 v2;
  4. 旧 `00-evaluation.md` 保持不动(历史快照)。
- 本文不假设具体语言/SDK 版本——只描述协议与机制。
