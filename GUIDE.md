# CodeAtlas 使用指南

CodeAtlas 是只读的代码库理解 Agent。它索引 Rust、Python 和 Python stub（`.rs`、`.py`、`.pyi`），调用只读查询工具探索代码，并给出带源码证据的渐进式回答。

完整的环境要求、构建命令、架构、课程 R1-R6 实现状态与提交清单位于 [`README.md`](README.md)。使用前请阅读 [`PRIVACY.md`](PRIVACY.md)：问题、解释 profile、会话回答以及 Agent 按需读取的源码和工具结果会发送给所配置的模型 Provider；该文档也给出了本地历史和凭据的完整删除方法。

## 1. 启动

### 课程平台配置

可以从 [`.env.example`](.env.example) 创建本地配置，但 CodeAtlas 不会自动加载 `.env`；需要先在当前 shell 中导出变量，或执行 `set -a; . ./.env; set +a`。不要提交填入真实 Key 的 `.env`。

首次使用时隐藏输入并保存 API Key：

```bash
cd CodeAtlas
cargo run -p codeatlas-app --bin codeatlas -- --store-api-key
```

Key 默认保存在 `$XDG_CONFIG_HOME/codeatlas/credentials.json`；未设置 `XDG_CONFIG_HOME` 时使用 `~/.config/codeatlas/credentials.json`。这是权限为 `0600` 的本地明文凭据文件，不会写入 repository、索引或会话。

以后启动时不需要再次导出 Key：

```bash
cd CodeAtlas

export CODEATLAS_ENDPOINT="https://lab.cs.tsinghua.edu.cn/ai-platform/api/v1/chat/completions"
export CODEATLAS_MODEL="glm-5"

cargo run -p codeatlas-app --bin codeatlas -- .
```

原生 GUI 使用同一套 endpoint、model、超时、重试、data directory 和安全凭据读取逻辑。启动并自动索引当前目录：

```bash
cd CodeAtlas
cargo run -p codeatlas-app --bin codeatlas-gui -- .
```

也可以不传路径进入欢迎页，在路径输入框中填写 repository，或使用 `Choose folder` 打开系统目录选择器。首版 GUI 支持 Rust 与 Python；它不依赖 WebKit。

注意：

- `CODEATLAS_ENDPOINT` 必须是完整的 Chat Completions URL，不是 API base URL。
- 保存时只输入原始 Key，不要添加 `Bearer `；客户端会自动生成 Authorization header。
- `CODEATLAS_API_KEY` 环境变量仍可临时覆盖本地凭据，且不会改写文件。
- 删除本地凭据：`cargo run -p codeatlas-app --bin codeatlas -- --delete-stored-api-key`。
- 命令末尾的 `.` 表示索引当前目录。也可以改成其他 repository 路径。

其他 OpenAI-compatible Provider 使用同样的三个变量，只需替换 endpoint、model 和 Key。模型必须支持 Chat Completions tool calling。

## 2. 基本流程

1. 启动时传入 repository 路径，或在 TUI 中按 `i` 输入路径。
2. 等待状态变为 `INDEXED`。
3. 按 `a`，输入问题并按 `Enter`。
4. 左栏显示探索阶段和工具调用，中栏显示回答，右栏只显示最终回答实际引用的源码证据。
5. 用 `Tab` 切换面板，用 `j`/`k` 选择或滚动内容。

GUI 中等待顶部状态变为 `Repository ready`，然后在 Conversation 底部提问。宽窗口显示左侧 Repository overview、中央 Conversation 和右侧 Claims & Evidence；窄窗口自动切换为三个内容标签。两侧栏可拖动，但会为中央回答保留可读宽度。点击 Claim 可筛选其 Evidence，点击 Evidence 会从本地索引加载对应源码。底部 `ACTIVITY` 默认折叠，只显示可理解的探索步骤，不显示 raw tool payload。提问框直接使用平台原生文本输入和 IME，可自然混合输入中文与英文；按 `Enter` 提问，按 `Shift+Enter` 换行，输入法正在组词或确认候选时不会误提交。WSLg 目前不会把 Windows IME 转发给 Linux 窗口，因此在 WSLg 中需要安装 Linux IME（如 IBus 或 Fcitx5）。检测到 IBus 时，GUI 会自动启动 daemon 并通过 X11/XIM 接入；Ubuntu 可安装 `ibus`、`ibus-libpinyin` 和 `libxkbcommon-x11-0`，使用 `Ctrl+Space` 切换中英文。

顶部 `History` 可加载保存的会话，`New session` 在当前 repository 开始新对话。属于其他 repository 的历史会话会明确显示为只读。GUI 不会在索引后自动恢复会话；要继续旧会话，必须先索引该会话对应的 repository，再通过 `History` 手动加载。有 SVG artifact 的回答可在 Diagram decision 中点击 `Open diagram` 调用系统默认查看器。

### Guided Progressive Onboarding

UI-neutral core、Agent、session persistence 与 application command 层已经实现 `ExplanationProfile` 和 `SuggestedAction` API。每个 profile 由 audience 与 depth 各一个闭合枚举值组成：

| 维度 | 可选值 | 说明 |
|---|---|---|
| Audience | `Beginner`、`Developer`、`Expert` | Beginner 先讲目的、定义高级术语并提供具体例子；Developer 假设具备一般编程能力但不了解当前 repository；Expert 聚焦不变量、权衡、边界情况和实现约束。 |
| Depth | `Auto`、`Overview`、`Architecture`、`Workflow`、`Code`、`Detail` | Auto 根据问题选择最小充分范围；其余档位依次覆盖全局定位、组件边界、运行流程、代码机制与最深的局部细节。 |

默认值是 `Developer` + `Auto`。profile 只由受信任枚举生成当前 task 的 system control message，不会降低 Evidence 要求。每个终态 task 都把实际 profile 写入 schema 3 session 和 history summary；缺少该字段的旧记录恢复为默认值。

`SuggestedAction` 是闭合且不可执行任意文本的类型，包含 `DeepenClaim`、`ContinueCallPath`、`ExplainEvidence`、`ShowSource` 和 `ChangeDepth`。当前 runtime 在构造最终且通过验证的回答时，确定性地产生最多 4 个不重复 action：首个 Fact（没有 Fact 时为首个 Claim）、未完成的 Call Path（没有时为首个路径）、首个最终保留的 Evidence，以及下一个适用 depth。`ExplainEvidence` 已进入 application contract，但当前自动生成器不会主动选择它。action label 由 runtime 固定，所有被引用的 ID 必须属于该回答；application 也会拒绝执行未成功保存的回答或该回答未曾提供的 action。

需要继续解释的 action 会在同一个 repository session 中转换为新的 `Ask`，复用已有可观察上下文和原 audience；`ChangeDepth` 只替换 depth。`ShowSource` 会直接转换为本地 `LoadSource`，不调用模型，也不增加 Provider Token 或费用。

TUI 在导航状态下按 `g` 打开 profile 设置，用上下键或 `j`/`k` 切换字段、左右键或 `h`/`l` 修改值，按 `Enter` 应用、`Esc` 取消；按 `A` 打开所选回答的建议动作菜单，再用 `j`/`k` 和 `Enter` 执行。GUI 在提问框旁提供紧凑的 Audience/Depth 选择器，并在所选回答下显示最多 4 个动作按钮。加载或切换历史 task 时，两种界面都会恢复该 task 保存的 profile。

### 演示脚本：Overview -> Workflow -> Code/Evidence

在 TUI/GUI 中保持同一 session，先选择 `Beginner` + `Overview`，再依次输入以下问题，并在后两步切换为 `Workflow` 和 `Code`：

```text
1. Overview
这个项目主要做什么？请用概览方式列出核心能力和少量关键模块，并给出源码证据。
```

```text
2. Workflow
在同一会话中，追踪从用户提问到证据校验完成的运行流程；按顺序说明关键分支，并区分事实、推断和未知。
```

```text
3. Code / Evidence
继续在同一会话中，下钻到实现这条流程的关键文件、类型和函数，并解释每条源码 Evidence 如何支持结论。
```

最后执行回答提供的 `ShowSource`，在现有 Evidence 查看器中检查引用源码；这一步只读取本地索引，不产生模型请求。

## 3. 界面

### Repository / Code Map

顶部显示语言构成、结构统计、核心模块与入口点，帮助用户先建立代码库全局认知；下方显示 Agent 当前阶段、只读工具调用和调用路径。工具参数和输出默认收起，选中工具后按 `t` 显示或隐藏详情。工具状态：

| 标记 | 含义 |
|---|---|
| `>` | 正在运行 |
| `+` | 成功 |
| `!` | 失败 |

### Conversation

显示问题、主回答和结构化结论。长中文、英文和代码标识符会在中央栏内换行；滚动到底部可查看完整的 Call Paths 与 Diagram decision。常见 Markdown 会转换成原生样式，包括标题、粗体、斜体、行内代码、链接、列表、引用和代码块；复杂 HTML 或浏览器专属排版不会渲染。选中请求的累计 Token Usage 固定显示在 Conversation 标题下方，运行中和完成后都包含 input、output、cached、total token，并在可用时显示成本。Provider 未返回 usage 时不会伪造零值。

结论标签：

| 标签 | 含义 |
|---|---|
| `F1`、`F2`... | 有源码 Evidence 支持的事实 |
| `I1`、`I2`... | 基于代码结构的推断 |
| `U1`、`U2`... | 当前证据无法确认 |
| `TRACE` | 回答包含调用路径 |

#### Diagram

Agent 必须为每个回答明确判断配图是否有解释价值。用户直接要求绘图时必须选择 `needed`；其他问题不会根据文件数、代码行数、仓库大小或回答长度机械触发，而是在图能明显澄清架构边界、非平凡流程或实体关系时选择：

| 类型 | 适用内容 |
|---|---|
| `ARCHITECTURE` | 组件职责、边界、依赖与组合关系 |
| `FLOW` | 请求生命周期、数据流、分支或状态交接 |
| `RELATIONSHIP` | Claim、Evidence、Symbol 等实体关系 |

每个图节点和图边都必须引用正文中的 `FACT` Claim。模型只提交节点、边和 answer-local Claim 索引，运行时从 Claim 自动推导元素 Evidence，避免同一绑定被重复填写。结构或绑定无效时，CodeAtlas 会返回具体错误并要求模型严格修复一次；重复失败会拒绝回答，不再把失败伪装成 `not needed`。SVG 由本地 Rust 渲染器确定性生成，不执行模型提供的 SVG、脚本、Graphviz 或外部绘图命令。

TUI 显示图类型、生成理由、节点/边数量、Evidence 编号和简短 Diagram ID，不直接渲染图片。选中对应任务后按 `o` 会使用系统默认查看器打开 SVG；在 WSL 中会自动把 Linux 路径转换为 Windows UNC 路径并调用 `explorer.exe`。按 `y` 可复制原始路径作为备用。打开前 application 会确认 artifact 来自已保存会话、位于受管目录、不是符号链接，且 ID、媒体类型、文件大小和确定性重新渲染的内容均匹配。回答正文中的 Markdown 表格、ASCII/Unicode 字符画都只是模型文本，不是本地 Diagram renderer 的输出。SVG 默认保存在：

```text
$CODEATLAS_DATA_DIR/diagrams/v1/<answer-id>/<diagram-id>.svg
```

离线示例位于 `diagram-showcase/README.md`。重新生成：

```bash
cargo run -p codeatlas-diagram --example codeatlas_showcase -- ./diagram-showcase
```

### Source Evidence

探索工具可能发现很多候选 Evidence，但最终面板和回答对象只保留被 Claim、CallPath 或 Diagram 实际引用的证据。未引用的探索结果不会堆积在右栏；schema 3 的完整 Tool Trace 仍会保存 raw 工具结果。运行时不会设置“最多显示 N 条”的数量上限，真正被引用的证据不会因列表长度而被静默删除。

每条证据显示 `E1`、`E2` 编号、repository-relative path、行号、Symbol，以及引用它的 Claim 标签：`F1` 表示第一个事实，`I1` 表示第一个推断，`U1` 表示第一个未知项。选中证据后，详情区同时显示相关 Claim 文本和源码 excerpt。详情区和全屏查看器都会按文件类型使用终端原生语法高亮；支持 Rust、Python、C/C++、Java、JavaScript、TypeScript、Go、JSON、TOML、YAML、Shell、HTML、CSS 和 SQL，无法识别的类型回退为普通文本。事实引用不存在或证据冲突时，回答会被验证层拒绝。

聚焦 Evidence 后按 `Enter` 或 `v` 打开全屏源码查看器。查看器会从本地索引加载完整证据行段，不受模型工具的 500 行或 64 KiB payload 限制，不调用模型，也不增加 Token。建议动作中的 `ShowSource` 复用同一条本地 `LoadSource` 边界并打开该查看器。若本地文件在索引后发生变化或无法读取，查看器会保留已有 excerpt 并在面板内提示，不再用全屏错误中断操作。

### Task History 与续聊

有效回答会写入内存、尝试原子保存为 JSON，并通过 `AnswerCompleted` 发布给 UI。保存失败不会撤销可见回答，但该回答的建议动作不会被授权执行：

```text
$CODEATLAS_DATA_DIR/sessions/<session-id>.json
```

当前文件使用 session schema 3；已有 schema 1 和 2 文件会在读取时安全迁移为已完成任务，并在下一次保存时写回新格式。损坏的单个 JSON 会被报告并跳过，不阻止其他有效会话加载。

重新索引 repository 不会自动恢复历史上下文。需要续聊时，先索引目标 repository，再手动打开 Task History：TUI 在导航状态下按 `h`，GUI 点击顶部 `History`。选择会话后再加载；TUI 使用 `j`/`k` 选择并按 `Enter`，正在输入路径或问题时先按 `Esc` 返回导航状态。

恢复内容包括任务终态（completed、failed、cancelled、budget exceeded）、每个 task 的 `ExplanationProfile`、问题、可选回答或终止错误、时间戳、Claim、Evidence、Call Path、Diagram、`SuggestedAction`、Token/Cost、每次模型调用及重试、Budget、Progress 和完整 Tool Trace。按 `[` / `]` 切换任务，Repository 面板显示所选任务的探索流程、模型 ledger 和预算。

其他 repository 的会话可以只读查看；只有先索引与会话匹配的 repository，再通过 History 手动加载该会话，才能继续提问或从本地加载完整源码。

schema 3 历史用于完整本地审计与续聊：保存每个 task 的 profile、传入 `ModelClient` 的 provider-neutral 请求、解析归一化后的响应/assistant 输出、重试错误、Progress、Usage 与 Budget 事件。`ToolCallCompleted` 保存工具执行器返回的完整 raw JSON（包括未引用候选源码），不做源码字段脱敏或 1000 字符截断；另存的 system-free 续聊消息是送入模型对话的版本，其中 Evidence 去重和发送窗口整理可能使 tool message 比 raw ToolOutput 更紧凑。这里不保存 HTTP wire bytes、响应中未解析的 Provider 私有字段或认证 header，也不会创建或保存模型私有思维链。请把 sessions 目录视为敏感源码副本。

终端较窄时界面自动切换为单面板 Tabs；终端过小时只显示精简状态。

## 4. 按键

### 导航和输入

| 按键 | 功能 |
|---|---|
| `i` | 输入或更换 repository 路径 |
| `a` 或 `?` | 输入问题 |
| `g` | 打开 Explanation Profile 设置 |
| `A` | 打开所选回答的 Suggested Actions 菜单 |
| `h` | 打开或刷新 Task History；历史页中再次按下可关闭 |
| `[` / `]` | 切换上一个 / 下一个任务及其工作流 |
| `n` / `p` | Repository 面板中选择下一个 / 上一个工具调用 |
| `t` | Repository 面板中显示 / 隐藏所选工具的参数和输出 |
| `o` | 用系统默认查看器打开当前任务的 SVG Diagram |
| `y` | 复制当前任务的 SVG 路径；全屏查看器中保留各自的复制功能 |
| `Enter` | Evidence 聚焦时查看源码；其他面板输入问题 |
| `Tab` / `Shift-Tab` | 切换面板 |
| `1` / `2` / `3` | 聚焦 Repository / Conversation / Evidence |
| `j` / `k` 或上下键 | 选择或逐行滚动 |
| `PgUp` / `PgDn` | 滚动 8 行 |
| `Home` / `End` | 跳到开头或结尾 |
| `c` | 取消当前请求 |
| `Esc` | 关闭输入、取消活动请求或清除错误 |
| `x` | 重新打开最近的错误详情 |
| `q` | 退出 |
| `Ctrl-C` | 取消活动请求；无活动请求时退出 |

输入时可使用方向键、`Home`、`End`、`Backspace`、`Delete`；`Ctrl-U` 清空输入。

### Evidence 全屏查看器

| 按键 | 功能 |
|---|---|
| `j` / `k` 或上下键 | 逐行滚动源码 |
| `PgUp` / `PgDn` | 滚动 8 行 |
| `Home` / `End` | 跳到源码首尾 |
| `n` / `p` | 下一条 / 上一条 Evidence |
| `w` | 切换自动换行 |
| `h` / `l` 或左右键 | 不换行时水平滚动 |
| `y` | 复制当前完整已加载源码 |
| `Esc` 或 `v` | 返回分栏界面 |

### 错误详情

请求失败时会自动打开完整错误详情：

| 按键 | 功能 |
|---|---|
| `j` / `k`、`PgUp` / `PgDn` | 滚动错误 |
| `Home` / `End` | 跳到错误开头或结尾 |
| `y` | 通过 OSC 52 请求复制完整错误 |
| `Esc` 或 `x` | 关闭详情 |
| `q` | 退出并在普通终端再次打印完整错误 |

如果终端禁用了 OSC 52，按 `q` 后从普通终端选择并复制错误。

## 5. 可选配置

| 环境变量 | 默认值 | 用途 |
|---|---:|---|
| `CODEATLAS_REASONING_MODE` | 未设置 | 原样发送给 Provider 的可选 reasoning mode；仅在 Provider 支持时设置 |
| `CODEATLAS_REASONING_EFFORT` | 未设置 | 原样发送给 Provider 的可选 reasoning effort；仅在 Provider 支持时设置 |
| `CODEATLAS_MODEL_TIMEOUT_SECONDS` | `180` | 单次模型 HTTP 请求超时 |
| `CODEATLAS_AGENT_TIMEOUT_SECONDS` | `1200` | 一次完整多轮问答的总超时；包含探索、重试和答案修复 |
| `CODEATLAS_MODEL_MAX_RETRIES` | `2` | transport、timeout、HTTP 429 及其他临时模型错误的重试次数 |
| `CODEATLAS_GATEWAY_MAX_RETRIES` | `5` | HTTP 502、503、504 快速拒绝的专用重试次数 |
| `CODEATLAS_MAX_OUTPUT_TOKENS` | Provider 默认 | 最大输出 Token |
| `CODEATLAS_CONTEXT_WINDOW_TOKENS` | 未设置 | 模型上下文窗口；设置后会在发送前主动整理过长历史和旧工具结果 |
| `CODEATLAS_TEMPERATURE` | Provider 默认 | 采样温度 |
| `CODEATLAS_PRICING_CURRENCY` | `USD` | 显式价格和金额预算的币种标签 |
| `CODEATLAS_INPUT_PRICE_PER_MILLION` | 未设置 | 每百万 uncached input Token 价格；必须与 output 价格同时设置 |
| `CODEATLAS_CACHED_INPUT_PRICE_PER_MILLION` | input 价格 | 可选的每百万 cached input Token 价格 |
| `CODEATLAS_OUTPUT_PRICE_PER_MILLION` | 未设置 | 每百万 output Token 价格；必须与 input 价格同时设置 |
| `CODEATLAS_AGENT_MAX_TOTAL_TOKENS` | 未设置 | 每个问答的累计 reported Token 上限 |
| `CODEATLAS_AGENT_MAX_COST` | 未设置 | 每个问答的估算金额上限；必须先配置 input/output 价格 |
| `CODEATLAS_DATA_DIR` | `$XDG_DATA_HOME/codeatlas`、`~/.local/share/codeatlas`，或无 HOME 时系统临时目录中的 `codeatlas-<pid>` | 外部缓存和会话目录 |

Reasoning 字段是 Provider-specific 的透传设置，CodeAtlas 不解析或展示模型私有思维链。价格完全由用户提供，不是 Provider 账单；per-call ledger、重试结果与预算状态同时显示并写入 schema 3 history。Token/金额预算在每次 Provider 返回 usage 后检查，因此超出量最多可达到最后一次调用的用量；Provider 不返回 usage 时会停止任务并记录预算不可执行。

慢速模型示例：

```bash
export CODEATLAS_MODEL_TIMEOUT_SECONDS="240"
export CODEATLAS_AGENT_TIMEOUT_SECONDS="1800"
export CODEATLAS_MODEL_MAX_RETRIES="2"
export CODEATLAS_GATEWAY_MAX_RETRIES="5"
export CODEATLAS_CONTEXT_WINDOW_TOKENS="128000"
```

## 6. Token 效率

CodeAtlas 在 schema 3 会话的 `ToolCallCompleted` 中完整保留 raw ToolOutput 和候选 Evidence，并保存 provider-neutral 模型/工具轨迹；Evidence 面板仍只展示结构化回答实际引用的 Evidence。raw 工具结果与续聊/发送消息是两个边界：后者可做 Evidence 去重或按窗口整理，但不会改写前者。发送规则如下：

- 保留 `false`、`null`、完整性标记和工具错误详情，不通过删除有语义字段节省 Token；
- data 已包含 Evidence excerpt 时不重复发送同一份源码；
- 同一问答中完全重复的 Evidence 只再次发送稳定 ID；若后续出现更完整的 excerpt，则重新发送增强后的 Evidence；
- 不再固定截断旧工具字符串，也不同时保存内容相同的 fresh/retained 工具副本；
- assistant tool call 与 tool response 始终成对保留，不破坏 OpenAI 协议；
- 设置 `CODEATLAS_CONTEXT_WINDOW_TOKENS` 后，仅在请求确实接近上下文窗口时先移除最旧会话轮次，再把旧工具结果改成保留 Evidence 身份和结果形状的结构化摘要；最新工具结果优先完整保留；
- Provider 明确返回 context overflow 时会进一步整理上下文并重试；普通 5xx、429、transport timeout 重试不会修改工具上下文；
- 不使用 LLM 摘要替换源码，不牺牲 Evidence 校验换取 Token 节省。

默认情况下 CodeAtlas 不设置探索轮数、工具调用次数或累计 Token/金额上限；探索持续到提交有效答案、用户取消或总 deadline 到达。可以用 `CODEATLAS_AGENT_MAX_TOTAL_TOKENS` 设置每个问答的累计 reported Token 上限，或在提供显式价格后用 `CODEATLAS_AGENT_MAX_COST` 设置估算金额上限，但仍没有工具调用次数硬上限。Provider 自身的上下文窗口、账户额度和单次响应限制仍然有效；`CODEATLAS_MAX_OUTPUT_TOKENS` 只有显式设置时才会限制单次输出。

## 7. 排障

### HTTP 401/403

服务端没有接受认证。重新运行 `--store-api-key` 更新本地 Key，或确认当前 shell 中覆盖它的 `CODEATLAS_API_KEY` 正确，然后重新启动进程。

如果错误正文提到 `platform.openai.com`，通常表示仍在使用默认 OpenAI endpoint，而不是课程平台 URL。Key 必须是完整原始值，不要添加 `Bearer `。

如果手工编辑凭据文件，Unix 权限必须为 `0600`：

```bash
chmod 600 ~/.config/codeatlas/credentials.json
```

### HTTP 502/503/504

这是 Provider 或网关暂时不可用，不是本地索引错误。502、503、504 往往会被网关立即返回，因此单次 model timeout 不会让请求至少等待指定时长。CodeAtlas 默认对这三种状态执行 5 次专用重试，使用约 2、4、8、16、30 秒的指数退避；连同首次请求共尝试 6 次，为短暂网关抖动保留约一分钟恢复窗口。重试不会改变已经返回的工具上下文，进度区会显示 HTTP 状态、当前重试次数和下次等待时间。

最终错误会显示已经执行的总尝试次数。此时的 `retryable: true` 表示用户稍后可以重新尝试，不表示后台仍在运行。持续失败时应检查 endpoint 和平台状态、换可用模型，或通过 `CODEATLAS_GATEWAY_MAX_RETRIES` / `--gateway-max-retries` 调整专用重试次数。所有重试仍受 `CODEATLAS_AGENT_TIMEOUT_SECONDS` 限制，不会无限等待；提高本地 model timeout 也不能延长网关自身的 timeout。

### `model request timed out`

单次模型调用超过本地限制。提高 `CODEATLAS_MODEL_TIMEOUT_SECONDS`；如果整个 Agent 过程超时，再提高 `CODEATLAS_AGENT_TIMEOUT_SECONDS`。

### `runtime_timeout`

整次 Agent 问答的总 deadline 已耗尽。错误会显示配置的总时长以及超时时正在等待模型、执行工具还是等待重试；该预算包含所有探索轮、模型重试 backoff 和结构化答案修复。慢速模型或多次 504 可能使多个仍低于单次 model timeout 的调用累计超过总预算，此时提高 `CODEATLAS_AGENT_TIMEOUT_SECONDS`，不必同时提高单次 model timeout。失败请求已经停止，`retryable: true` 只表示可以重新提问。

### `unstructured_final_answer` 或 `invalid model response`

模型没有按 tool-calling/结构化回答协议提交结果。CodeAtlas 会自动尝试一次格式修复；Provider 返回的临时 malformed/截断响应也会按模型重试配置重试。仍失败时应确认模型支持 OpenAI-compatible tools。

运行中的 `submit_answer was invalid (...)` 是自动修复进度，不是最终错误；括号内会显示缺失字段、类型或枚举值等有界诊断。修复成功后会继续进入 Evidence 校验并正常生成回答。

### `conflicting_evidence`

同一个 Evidence ID 必须始终指向相同的 file、path、span 和 symbol。不同工具为同一位置返回长度或上下文不同的 excerpt 时，CodeAtlas 会确定性保留信息更完整的一份，不再中断回答。只有身份字段真正冲突时才会报错；这通常表示 repository 在索引后发生结构性变化，应重新索引。

### `evidence_validation_failed`

模型提交的 Fact、CallPath 或 Diagram 绑定无效，或 Fact 没有证据。CodeAtlas 会把具体验证错误和本轮可用 Evidence ID 返回给模型并严格修复一次；Diagram 元素的 Evidence 会由其 `FACT` Claim 自动推导。若修复后仍无效，则拒绝回答，不会静默删除 CallPath、把失败图标记成 `not needed`、降低 Claim 等级或伪造 Evidence。持续出现时可重试问题，或改用更可靠地遵守 tool-calling 约束的模型。

格式修复和 Evidence/Diagram 验证修复分别计数，前一种错误不会占用后一种的修复机会。空白答案也会进入验证修复，不会作为成功的空回答发布。

### `repository_not_indexed`

等待状态变为 `INDEXED` 后再提问。仓库未变化时，重新索引会复用缓存。

### `diagram_generation_failed`

已验证的结构化图在本地生成或存储 SVG 时失败，例如 SVG 超过安全限制或 diagram data directory 不安全。该错误不会再吞掉已经生成的文本答案；回答会保留结构化图但没有 artifact。模型结构/绑定错误会在 Agent 阶段获得一次修复机会，不会到达 renderer。为保证只读边界，`CODEATLAS_DATA_DIR` 不能位于正在分析的 repository 内，也不能通过符号链接逃逸。

## 8. 数据与边界

- API Key 从环境变量或私有 XDG credentials 文件读取，不写入索引、会话或普通应用配置。
- 缓存和会话保存在目标 repository 外，不修改被分析代码。
- SVG 只写入外部 data directory；文件名只使用稳定 ID，模型文本经过 XML 转义。
- 查询工具只能读取已索引内容，没有 shell、写文件或自动修复能力。
- 调用图是保守的静态近似；动态派发、反射、宏生成和运行时注册可能无法完整解析。
- 当前模型请求为非流式 Chat Completions；TUI 在完成后显示完整回答和中间工具事件。有效答案会在 session 保存尝试完成后发布；磁盘保存失败只显示可恢复诊断，不会撤销回答，但会禁用该未落盘回答的建议动作。
