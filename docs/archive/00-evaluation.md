# 脑暴评估报告：MCU 嵌入式 Claude Code 多 Agent 编排环境

> 输入：`/Users/gyanano/Documents/ObsidianBrain/0-Inbox/MCU嵌入式Claude Code开发环境设计【Draft】.md`
> 输出：本文 + 配套的 [`01-architecture.md`](./01-architecture.md)
> 立场：本文只做「合不合理 / 合不合 Claude Code 现实能力」的评估，落地方案见架构文档。

---

## 1. 输入回顾（脑暴 10 条原文，编号供后文引用）

| # | 原文摘要 |
|---|---------|
| B1 | 项目按子模块拆分，可分层；每个文件夹一个 Agent。 |
| B2 | 根 Agent 负责整体编译、版本管理、子模块协调与监督；复杂时可拆为「版本管理」+「任务监督」。 |
| B3 | 分层文件夹的 Agent 负责本层职责说明、子模块开发监督、任务需求划分。 |
| B4 | 子模块 Agent 负责该模块的设计、开发、管理、维护。 |
| B5 | 子模块只能访问本目录；与上级沟通靠子目录里一个**省 token 格式**的对话文件。 |
| B6 | 每个文件夹 / Agent 都有自己的 `Agent.md` 或 `claude.md`，记录长期方法论与注意事项。 |
| B7 | 父级文件夹能读子模块目录，但为节省上下文，只读「对话文件」做高价值信息交换。 |
| B8 | 父级根目录维护一个省 token 配置文件，存所有子模块的当前任务、进度、下一步交付物催促。 |
| B9 | 每个子 Agent 等同一个 Claude Code 工作区，有自己的 hooks、测试环境、验收流程；通过 hook 自动验收，只有合格交付物才能上交；**但用户不确定怎么把完成事件通知给父级 Agent**。 |
| B10 | 整体是树状管理：任务发散，交付物收敛。 |

---

## 2. Claude Code 现实能力速查（评估依据）

| 关注点 | 现实情况 | 对脑暴的影响 |
|--------|---------|-------------|
| **Subagent 递归** | `Agent` 工具产生的子 Agent **不能再 spawn 子 Agent**。 | 「根 → 层 → 子模块」的多级嵌套**不能用 subagent 自动编排**。 |
| **Teams** | 启用 `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` 后可用：`SendMessage`、共享 TaskList、`TeammateIdle` / `TaskCompleted` hook、`TeamCreate`。每个 teammate 是独立 Claude Code 实例。 | 是 B1/B2/B3/B4 的**唯一原生承载**。但 teammate 也不能再嵌套子 team，所以最多 2 级。 |
| **Hooks** | `PreToolUse` / `PostToolUse` / `Stop` / `SubagentStop` / `TaskCompleted` / `TeammateIdle` / `UserPromptSubmit` 等。matcher 按工具名或 agent 名匹配，**不按路径**。退出码 2 表示阻断 + 把 stderr 反馈给模型。 | B9 的「自动验收 + 阻断不合格交付」可以做，但**路径分流要在 hook 脚本里自己写**。 |
| **CLAUDE.md** | 从 cwd 向上递归加载所有 `CLAUDE.md` / `CLAUDE.local.md`；agent 读取子目录文件时按需载入子目录的 `CLAUDE.md`。**没有「per-agent 私有 CLAUDE.md」**——只有 per-folder。 | B6 的语义需要修正：「每 Agent 一份 CLAUDE.md」≈「每文件夹一份 CLAUDE.md」+ `.claude/agents/<name>.md` frontmatter。 |
| **权限隔离** | `.claude/settings.json` 的 `permissions` 是**会话级**路径白名单，subagent 切换时不会自动收窄。 | B5 的「子模块只能访问本目录」**不是 Claude Code 默认行为**，必须靠 `PreToolUse` hook 或 git worktree 强制。 |
| **Worktree** | `EnterWorktree` / subagent frontmatter `isolation: worktree` 给独立 git checkout。改动若没有则自动清理。 | 给「真隔离子工作区」一条干净路径，但嵌入式常需共享 HAL/链接脚本/构建缓存，要权衡。 |
| **`.claude/agents/*.md`** | 自定义 subagent 定义；可设 `tools` 白名单、`memory: project|user|local`、`isolation`、`model`、自带 system prompt。**插件版 agent 不允许 hooks/mcpServers**，本地 agent 可以。 | 是 B6「per-Agent 长期记忆 + 角色画像」的实际承载。 |
| **MCP / 插件** | 可以挂自建 MCP server 暴露任意工具；用户已启用 `claude-hud`、`codex` 插件。 | 编译 / 烧录 / HIL 等专有工具链最适合走 MCP，而不是塞进每个 Agent 的 Bash 权限。 |
| **父子完成通知** | Teams 模式下：teammate 完成任务后会自动 idle 并触发 `TeammateIdle` hook；状态机可走 TaskList；主动汇报走 `SendMessage`。 | **B9 的未知点已有原生答案**——不需要自己造文件 watcher。 |

---

## 3. 逐条评估

每条给出「保留 / 调整 / 废弃」标签 + 简评 + 对应的原生机制锚点。

### B1 项目按文件夹分 Agent，可分层 — 调整
- 直觉合理，但 Claude Code 的 subagent **不可递归**、teammate **不能再开 team**。
- 推荐压扁为**两级拓扑**：根 Lead + 模块 Teammates。「分层」（如 HAL / BSP / MW / APP）建议作为
  **模块命名前缀**或**模块内分组**，而不是真的多一层 Agent。
- 锚点：Teams + `.claude/agents/*.md`。

### B2 根 Agent 负责整体编译 / 版本 / 协调 — 保留
- 完全契合 Lead Agent 的角色定位。
- 「复杂时拆为版本管理 + 任务监督」可以通过给 Lead 加**专精 subagent**实现（一次性 Task 调用，
  非常驻 teammate），不必再造一个常驻 Agent。
- 锚点：Lead session + on-demand `Agent` 调用。

### B3 分层 Agent 管理本层 — 调整
- 同 B1，去掉「分层 Agent」这一中间层级。需要"层级监督"的话，由 Lead 在 prompt / CLAUDE.md
  里按模块前缀分组，或在 TaskList 里用 `metadata.layer` 字段聚合视图。
- 锚点：TaskList + 命名约定。

### B4 子模块 Agent 设计/开发/维护本模块 — 保留
- 这就是 Module Teammate 的标准职责，1:1 映射。
- 锚点：`TeamCreate` + `.claude/agents/module-*.md`。

### B5 子模块只能访问本目录 + 通过省 token 文件沟通 — 保留（但需要主动加固）
- "只能访问本目录"不是 Claude Code 默认；要靠下面三选一：
  1. `.claude/agents/module-*.md` 的 `tools` 白名单 + `PreToolUse` hook 校验路径在
     `<module>/` 之内；
  2. 模块用 `isolation: worktree`；
  3. 信任 + 文档约束 + Lead 的 review。
- 「省 token 文件」**强烈建议保留**，但格式要有 schema。本架构选择 **TOML**（人类可读、解析容
  错好、Anthropic 文档生态也常用）。详见 [`01-architecture.md` §3](./01-architecture.md)。
- 锚点：`PreToolUse` hook + `./.coordination/inbox.toml`。

### B6 每文件夹 / Agent 自己的 CLAUDE.md — 保留但语义修正
- **CLAUDE.md 是 per-folder，不是 per-agent**。Claude Code 自动从 cwd 向上加载，并在 agent 读
  到子目录文件时按需加载子目录的 CLAUDE.md。
- 真正想要「per-agent 长期记忆」要双管齐下：
  - 文件夹层面：`<module>/CLAUDE.md`（任何在该目录下工作的 Agent 都会看到）；
  - Agent 层面：`.claude/agents/module-foo.md` 里的 system prompt + `memory: project` 自动记
    忆区。
- 锚点：CLAUDE.md hierarchy + subagent frontmatter `memory:`。

### B7 父级只读子模块的对话文件做高价值交换 — 保留
- 思路非常正确：避免父 Agent 把整个子目录拉进上下文。
- 实施上对应：Lead 只读 `<module>/.coordination/status.toml` 和 `inbox.toml`，**不主动 Read
  模块源代码**，除非 teammate 在 `inbox.toml` 里主动 attach 了片段或路径请求 review。
- 锚点：协议规范 + Lead 的 prompt 纪律（写进根 CLAUDE.md）。

### B8 根目录一份省 token 配置文件汇总所有子模块状态 — 保留
- 强需求。但**不需要 Lead 自己手维护**——可以让 hook 在 teammate 每次 `TaskCompleted` /
  `TeammateIdle` 时把对应模块 `status.toml` 摘要回写到根目录的 `.coordination/registry.toml`。
- 锚点：`TaskCompleted` hook + 根目录 `registry.toml`。

### B9 每子 Agent 自己的 hooks + 验收流程 + 不知道怎么通知父级 — 解锁
- 「自带验收流程」→ 模块根目录 `verify.sh`（或 `verify.mk` / `verify.py`，由模块自己定）。
- 「自动调用 + 不合格阻断」→ `TaskCompleted` hook，校验失败 `exit 2` 把错误反馈回 teammate。
- 「怎么通知父级」→ **这是用户脑暴里最大的未知点，Teams 模式已经原生解决**：
  - teammate 任务完成会自动进入 idle 状态 → 触发 Lead 的 `TeammateIdle` hook；
  - teammate 也可以主动 `SendMessage(to="lead", ...)` 汇报；
  - TaskList 状态变更对 Lead 可见。
- 锚点：`TaskCompleted` + `TeammateIdle` + `SendMessage`。

### B10 树状管理：任务发散 → 交付收敛 — 保留
- 心智模型是对的，但**收敛方式要明确**：不是 Lead 主动 poll 各子目录，而是各 teammate 在交付
  前自己跑 `verify.sh`，hook 通过后写 `status.toml` 标记 `delivered`，Lead 读 registry 决定
  是否打包总集成。
- 锚点：`registry.toml` 状态机 + Lead 的集成阶段。

---

## 4. 关键风险与未决问题

### 风险 R1：Teams 是 experimental
- `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` 是实验特性，API 形态可能变化。
- 缓解：把所有「Teams 专用」逻辑（`SendMessage`、`TeammateIdle` hook）封装在 hook 脚本和
  `.claude/agents/*.md` 里，业务侧只依赖文件协议（`.coordination/*.toml`），即使将来 Teams 改
  名/重构，文件协议依然可读，可以 fallback 到「人工或别的 orchestrator」。

### 风险 R2：teammate 数量与 token 成本
- 每个 teammate 是独立 Claude Code 实例，独立上下文。模块多了之后**总 token 成本接近线性增
  长**。
- 缓解：默认按模块**懒启动 teammate**，长期 idle 的让它退出；Lead 通过文件协议而不是常驻进程
  跟踪状态。

### 风险 R3：嵌入式构建产物的跨模块依赖
- HAL / BSP / 链接脚本 / 共用库一旦放进 worktree 隔离，模块之间会互相看不见。
- 缓解：架构默认**共享工作树**（不强制 worktree），仅对「实验性重写 / 高破坏性变更」按需开
  `isolation: worktree`。共享 HAL 之类的代码作为单独 module，由 Lead 协调谁先改。

### 风险 R4：路径白名单 hook 的可靠性
- `PreToolUse` 路径校验是字符串处理，容易绕过（`../`、符号链接、绝对路径）。
- 缓解：白名单解析必须先 `realpath` 再前缀匹配；并把 hook 自身放在版本控制里 review。如果对
  安全性要求极高，改用 worktree + 文件系统权限。

### 未决问题 Q1（需要用户决策）
- 是否将 `verify.sh` 的失败也作为 Lead 的「待办」自动登记？
  - 选项 A：失败仅 `exit 2` 回 teammate，由 teammate 自循环修复。
  - 选项 B：连续失败 N 次后自动通过 `SendMessage` 升级给 Lead。
- 默认推荐 A；B 等真的出现"卡死"再加。

### 未决问题 Q2（需要用户决策）
- 是否引入 MCP server 封装编译/烧录/HIL 工具链？
  - 利：toolchain 升级 / 切换不影响 agent 定义；权限可在 MCP 层统一管控。
  - 弊：多一层维护成本，调试更绕。
- 默认推荐**先用 Bash 直接调**，在工具链稳定后再抽 MCP。

### 未决问题 Q3（需要用户决策）
- 模块"分层"在大型项目里仍然有现实需要（HAL/BSP/MW/APP）。是否要让 Lead 在
  `registry.toml` 里给每个模块打 `layer` 字段，并按层做集成顺序？
- 默认推荐**是**，仅作为 metadata，不引入新一级 Agent。

---

## 5. 评估结论

- 用户脑暴的**思路方向是对的**：把"文件夹—Agent—长期记忆—验收闸门—状态汇总"这条主线串起
  来了，且对 token 经济性有清晰直觉。
- 主要 gap 在于**对 Claude Code 现实约束的认知**：subagent 不可递归、权限不是 per-agent、
  CLAUDE.md 是文件夹作用域、Teams 是 2 级拓扑。
- 修订后的设计**保留全部核心思想**，只是把"无限深度树"压成"两级 + 文件协议"，并把"每 Agent 自
  带验收 + 自动通知父级"映射到 Teams 原生的 `TaskCompleted` / `TeammateIdle` / `SendMessage`。
- 落地建议：先按 [`01-architecture.md`](./01-architecture.md) 做一个**单模块**最小闭环（Lead +
  1 个 module teammate + `verify.sh` + `.coordination/`），验证通过后再扩到 N 个模块。
