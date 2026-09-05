# CodeAtlas：面向陌生代码库理解的专用 Agent

> **Slogan:** Turn codebases into mental models.  
> **Alternative:** Let code speak human.

## 1. 项目核心想法

当前最热门的一类 Agent 是 Coding Agent：

```text
Natural Language
      ↓
    Code
```

本项目采取逆向思路：

```text
Code
  ↓
Human-understandable Explanation
```

但项目不应被定义成一个简单的“代码翻译器”。更准确的定位是：

> **一个面向陌生代码库理解（codebase onboarding）的专用 Agent。它自动探索代码库、建立结构化程序模型，并根据用户问题生成可追溯到源码证据的自然语言解释。**

Coding Agent 的目标是降低“写代码”的门槛，而本 Agent 的目标是降低“理解代码”的门槛。

随着 AI 生成代码的速度越来越快，一个新的瓶颈会越来越明显：

```text
AI produces code faster
        ↓
Codebases become larger and change faster
        ↓
Human understanding becomes the bottleneck
```

本项目希望解决的正是这个 bottleneck。

---

## 2. 项目理念

Jensen Huang 曾提出类似观点：

> The programming language of the future is human.

也常被概括为：

> The best programming language in the AI era is English.

如果 AI 时代自然语言逐渐成为人类向计算机表达意图的主要接口，那么反过来，计算机也应该能把程序重新解释成人类可以快速理解的语言。

可以把 AI 理解成一种“双向编译器”：

```text
Human Intent
    │
    ▼
Natural Language
    │
    ▼
    AI
    │
    ▼
   Code
```

同时：

```text
   Code
    │
    ▼
    AI
    │
    ▼
Human Mental Model
```

因此，本项目不是简单做：

> code → text

而是：

> code → structured understanding → human mental model

自然语言只是最终的人机接口，而不是系统内部的核心表示。

---

## 3. 产品定位

### 3.1 一句话定义

> **CodeAtlas 是一个面向陌生代码库理解的跨语言、结构化、可验证 Agent。**

三个关键词：

- **Cross-language**
- **Structured**
- **Verifiable**

### 3.2 目标用户

主要面向：

- 第一次进入陌生项目的开发者；
- 阅读大型开源项目的学生；
- 新加入工程团队的开发者；
- 需要快速理解某条执行路径、模块关系或数据流的人；
- 希望学习一个真实 codebase，而不是只获得局部代码解释的人。

### 3.3 典型问题

用户可以直接问：

```text
这个项目整体是干什么的？

主要模块有哪些？

程序入口在哪里？

一次 HTTP 请求是怎么走的？

这个 Scheduler 是如何工作的？

某个函数最终会调用到哪里？

这个数据结构在哪些模块中被使用？

为什么这里用了 channel？

把这一条调用链继续展开。

我刚接触 Rust，请用初学者能理解的方式解释这个模块。
```

---

## 4. 为什么不直接使用 Claude Code / Codex？

这是项目答辩时最关键的问题之一。

### 4.1 不应回答“Claude Code 做不到”

Claude Code、Codex 等通用 Coding Agent 已经具有很强的代码理解能力。

因此本项目的价值不在于：

> “Claude Code 完全不能解释代码。”

而在于：

> **Claude Code 是通用 Coding Agent，代码理解只是它完成软件工程任务的一种中间能力；CodeAtlas 则把建立人类对代码库的 mental model 本身作为最终目标，并对整个理解过程进行结构化和产品化。**

### 4.2 核心差异

| Claude Code / Codex | CodeAtlas |
|---|---|
| General-purpose coding agent | Codebase-understanding specialist |
| 目标是完成 coding task | 目标是建立 human mental model |
| 按需临时读取上下文 | 预先或增量建立 codebase index / IR |
| 模型临时决定怎么查 | 固定的 analysis + verification workflow |
| 回答可能大量依赖 LLM inference | 关键结论绑定 source evidence |
| Coding-oriented UI | Architecture / Trace / Explain / Evidence UI |
| 主要关注任务完成 | 主要关注长期、渐进式理解 |
| 多语言主要依赖模型能力 | 多语言 parser → language-neutral IR |

### 4.3 推荐答辩口径

如果老师问：

> 为什么不直接问 Claude Code？

可以回答：

> Claude Code 当然可以回答代码问题，但它是一个通用 Coding Agent，代码理解只是它完成编程任务的一个中间能力。我的项目把 codebase understanding 本身作为目标，专门构建跨语言的代码结构索引和调用关系，用 Agent 自动决定应该继续读取哪些代码，并要求关键解释都能追溯到源码证据。  
>   
> 我想优化的不是“LLM 能不能解释代码”，而是“能不能稳定、低成本、可验证地帮助一个人建立陌生代码库的 mental model”。

如果老师继续追问：

> Claude Code 也能 grep、找 reference、给行号。

则回答：

> 单项能力并不是新的。区别在于我把这些能力固化成专门的 workflow 和数据结构，而不是依赖模型每次临时决定怎么做。例如 repository 会先被索引成统一的 symbol graph，后续问题可以复用；解释必须经过 evidence verification；输出固定组织成 architecture、workflow、trace 和 source evidence。这些是产品层面的稳定约束，而不是一句 prompt。

核心表述：

> **不是“Claude 做不到”，而是“把通用 Agent 临时完成的代码理解过程产品化、结构化、可验证化”。**

---

## 5. 与 Archify 的关系

参考项目：

- `https://github.com/tt-a1i/archify`

Archify 更接近：

> Agent Skill + visualization backend

它重点解决：

```text
Code / System Description
          ↓
Architecture IR
          ↓
Architecture / Workflow / Sequence / Data-flow Diagram
```

而本项目的核心是：

```text
Repository
    ↓
Explore
    ↓
Read
    ↓
Trace
    ↓
Build Model
    ↓
Verify
    ↓
Explain
```

因此 Archify 与 CodeAtlas 的关系可以理解为：

```text
                CodeAtlas
                   │
        ┌──────────┼──────────┐
        ▼          ▼          ▼
   Explanation   Trace      Diagram
                              ↑
                        Archify-like skill
```

也就是说：

> Archify 可以被视为一个“视觉表达能力”，而 CodeAtlas 本身是负责自主探索和理解代码库的 Agent。

因此 Archify 更适合作为 related work，而不是直接竞争对手。

---

## 6. Rust 与跨语言支持

课程要求核心逻辑使用 Rust 实现，但 CodeAtlas 面对的 codebase 不应该局限于 Rust。

需要明确区分：

```text
Implementation Language ≠ Target Language
```

Rust 是：

> Agent runtime、索引系统、分析 pipeline、工具调用框架的实现语言。

被分析的代码可以是：

- Rust
- Python
- C
- C++
- Java
- JavaScript / TypeScript
- Go
- 其他语言

总体架构：

```text
             CodeAtlas Core
                (Rust)
                   │
        ┌──────────┼──────────┐
        ▼          ▼          ▼
      Rust       Python       C/C++
     Parser      Parser       Parser
        │          │          │
        └──────────┼──────────┘
                   ▼
          Language-neutral IR
                   │
                   ▼
                Agent
```

推荐技术路线：

> 使用 tree-sitter 或类似 parser，对不同语言进行语法解析，然后统一转换成 language-neutral Codebase IR。

---

## 7. 核心：Codebase IR

系统不应该简单实现成：

```text
walkdir
  ↓
读取所有代码
  ↓
拼到 prompt
  ↓
LLM
  ↓
总结
```

否则本质上只是 LLM wrapper。

正确方向应该是：

```text
Repository
    │
    ├── Files
    ├── Modules / Packages
    ├── Symbols
    ├── Functions
    ├── Structs / Classes
    ├── Imports
    ├── References
    ├── Call Relations
    └── Entry Points
             │
             ▼
        Codebase IR
```

可能的数据结构：

```rust
struct Symbol {
    id: SymbolId,
    name: String,
    kind: SymbolKind,
    file: PathBuf,
    start_line: usize,
    end_line: usize,
}

struct CallEdge {
    caller: SymbolId,
    callee: SymbolId,
}

struct FileInfo {
    path: PathBuf,
    language: Language,
}

struct RepositoryModel {
    files: Vec<FileInfo>,
    symbols: Vec<Symbol>,
    calls: Vec<CallEdge>,
}
```

后续可以进一步扩展：

```rust
struct ImportEdge { ... }

struct ReferenceEdge { ... }

struct ModuleNode { ... }

struct Evidence {
    file: PathBuf,
    start_line: usize,
    end_line: usize,
    symbol: Option<SymbolId>,
}
```

核心思想：

> **不要让 LLM 直接承担“理解整个 repository”的全部工作。先由程序构建结构化世界模型，再让 LLM 在这个模型上进行查询、规划和解释。**

---

## 8. Agent Loop

Agent 不是简单的一次 Prompt → Response。

它需要根据用户问题主动探索代码库。

例如用户问：

> 这个项目是怎么处理 HTTP request 的？

理想执行过程：

```text
User Question
      │
      ▼
Intent Analysis
      │
      ▼
Identify target:
HTTP request lifecycle
      │
      ▼
Search possible entry points
      │
      ▼
Read router registration
      │
      ▼
Trace handler
      │
      ▼
Trace service
      │
      ▼
Trace database / downstream call
      │
      ▼
Verify call chain
      │
      ▼
Generate explanation
```

更完整的 Agent 架构：

```text
                 User Question
                       │
                       ▼
                Intent Analyzer
                       │
                       ▼
              Repository Index
            ┌──────────┴─────────┐
            ▼                    ▼
       Symbol Search         Semantic Search
            │                    │
            └──────────┬─────────┘
                       ▼
                 Code Explorer
                       │
                 tool calls
                       │
          ┌────────────┼────────────┐
          ▼            ▼            ▼
       read_file   find_symbol   trace_call
          │            │            │
          └────────────┼────────────┘
                       ▼
                Evidence Store
                       │
                       ▼
                 LLM Reasoning
                       │
               enough evidence?
                 /           \
               no             yes
               │               │
               └── explore ─────┘
                               │
                               ▼
                     Human Explanation
```

关键问题应该由 Agent 自己决定：

```text
Which file should I inspect next?

Which symbol should I trace?

What evidence do I currently have?

Is the evidence sufficient to support the answer?

Which part is fact and which part is inference?

Should I continue exploring?
```

---

## 9. Evidence-grounded Explanation

这是 CodeAtlas 最重要的专用能力之一。

LLM 在代码解释中的一个主要问题是：

> 容易把推测写成事实。

因此系统应该规定：

> **Every important explanation must have evidence.**

每个关键结论都尽量绑定：

```text
file
symbol
line range
call path / reference path
```

例如：

```text
Claim:
The HTTP request eventually reaches UserService::query.

Evidence:
src/api.rs:43-58
src/service.rs:21-37

Trace:
router
  ↓
handle_request
  ↓
UserService::query
```

同时把信息分为三类：

```text
✓ Code Fact

△ Reasonable Inference

? Unknown / Insufficient Evidence
```

例如：

```text
✓ Fact
handle_request() 调用了 UserService::query()。

△ Inference
这一层 service abstraction 可能是为了隔离 HTTP 层和数据访问层。

? Unknown
仅从代码无法确认作者最初为什么采用该架构。
```

目标：

> 不让 LLM 把“解释得像真的”误当成“有代码依据”。

---

## 10. Progressive Disclosure：渐进式解释

大型 codebase 最大的问题之一是信息量过大。

所以解释不应该默认一次性展开所有细节。

可以设计不同层级：

```text
Level 0 — Overview
一句话说明项目做什么

Level 1 — Architecture
核心模块和关系

Level 2 — Workflow
一个具体行为如何流经系统

Level 3 — Code
具体函数、类型和数据结构

Level 4 — Detail
逐行或局部实现解释
```

用户可以不断：

```text
展开这里

继续往下

这个函数具体做什么？

把 Scheduler 这一块展开

从 beginner 角度解释

从 systems developer 角度解释
```

核心理念：

> **先建立整体 mental model，再根据用户兴趣不断下钻。**

---

## 11. Adaptive Explanation

Agent 可以根据用户背景调整解释方式。

例如：

```text
beginner
developer
expert
```

同一个 Rust 结构：

```rust
Box<Node>
```

可以解释成：

### Beginner

> `Box<Node>` 表示这个 Node 被存储在堆上，而当前结构只保存一个指向它的拥有关系。

### Expert

> `Box` provides unique ownership and introduces indirection, allowing recursive data structures to have a statically known outer size.

这种自适应解释可以成为 codebase learning 的重要差异化功能。

---

## 12. Learning Mode

CodeAtlas 不仅回答问题，还可以成为：

> **Interactive Codebase Tutor**

在理解一个 repo 后，可以生成：

- 推荐阅读的 5 个核心文件；
- 推荐阅读顺序；
- 关键 module；
- entry point；
- 核心 data structure；
- 关键 workflow；
- 学习路线；
- review questions；
- quiz。

例如：

```text
如果你只有 20 分钟理解这个项目：

1. src/main.rs
2. src/router.rs
3. src/service.rs
4. src/model.rs
5. src/storage.rs
```

然后：

```text
先理解 main.rs 的启动流程。
理解后再进入 router。
随后沿一个具体 request 追踪到 service。
```

这可以显著强化“人类友好的代码库翻译官”这一定位。

---

## 13. 用户呈现形式

推荐总体形式：

> **对话为主，结构化视图为辅。**

不是生成一次性的长篇报告，而是让用户感觉自己在和“一个非常熟悉该代码库的人”交流。

### 13.1 四种核心输出卡片

#### 1. Explain Card

回答：

> “这段代码 / 这个模块在做什么？”

#### 2. Map Card

回答：

> “整个系统由什么组成？”

#### 3. Trace Card

回答：

> “某个行为是怎么发生的？”

#### 4. Evidence Card

回答：

> “你为什么这么说？”

---

## 14. 推荐 UI

可以采用三栏式布局：

```text
┌─────────────────────────────────────────────────────────────┐
│ CodeAtlas — repository: xxx                                │
├────────────────┬──────────────────────────┬─────────────────┤
│                │                          │                 │
│   Code Map     │      Conversation        │ Source Evidence │
│                │                          │                 │
│ main.rs        │ Q: 一次请求怎么执行？    │ src/main.rs:32  │
│   ↓            │                          │ src/api.rs:48   │
│ Router         │ 请求首先进入 Router...   │ service.rs:21   │
│   ↓            │                          │                 │
│ Handler        │ [展开 Handler]           │ 具体源码         │
│   ↓            │ [查看完整调用链]         │                 │
│ Service        │                          │                 │
│                │                          │                 │
├────────────────┴──────────────────────────┴─────────────────┤
│ ✓ Indexed 183 files  ✓ 1241 symbols   Token: 12.4k        │
└─────────────────────────────────────────────────────────────┘
```

### 左栏

展示：

- repository tree；
- module graph；
- architecture；
- 当前 trace；
- 当前选择的 symbol。

### 中栏

自然语言对话：

- 用户问题；
- Agent 回答；
- 深入按钮；
- 回退；
- 继续追踪；
- 调整解释层次。

### 右栏

Evidence：

- source file；
- line range；
- symbol；
- 原始代码；
- 当前 claim 对应的证据。

---

## 15. Agent 实时进度

课程要求实时展示 Agent 执行过程。

CodeAtlas 很适合展示：

```text
✓ Indexed 183 files
✓ Parsed 1,241 symbols
✓ Found 17 possible entry points
→ Tracing HTTP request lifecycle
→ Reading src/server.rs
→ Following call to handle_request()
→ Reading src/service.rs
→ Verifying call chain
→ Generating explanation
```

不要只显示：

```text
Thinking...
```

用户应该能够看到：

> Agent 正在主动进行 repository exploration。

---

## 16. 用户交互循环

理想体验：

```text
用户：
这个项目怎么处理请求？

Agent：
先给高层结论
      ↓
给 workflow
      ↓
给 source evidence

用户：
Handler 里面具体发生了什么？

Agent：
沿当前 trace 继续深入
      ↓
读取相关代码
      ↓
更新 mental model
      ↓
重新组织解释
      ↓
继续给 evidence
```

这可以被理解为：

> **一个可无限下钻的代码解释器。**

---

## 17. UX 目标

对于一个完全陌生的 repository：

> **5 分钟建立全局认知，20 分钟理解一条关键 workflow，需要细节时随时下钻到源码。**

这可以作为整个产品体验设计的核心目标。

---

## 18. 课程要求映射

课程 Agent 大作业包含若干基本要求，本项目可以自然映射。

### Rust 核心逻辑

Rust 用于实现：

- repository scanner；
- parser adapter；
- Codebase IR；
- symbol index；
- dependency graph；
- call graph；
- Agent runtime；
- tool registry；
- session persistence；
- model client；
- token accounting。

### UI

可选：

- TUI；
- Web UI；
- Desktop GUI。

建议 MVP 可以优先：

> Rust backend + Web UI

或者：

> Rust TUI

### 模型配置

支持：

- OpenAI-compatible API；
- configurable base URL；
- API key；
- model name；
- temperature；
- context configuration。

### 实时进度

展示：

```text
Scanning
Parsing
Indexing
Searching
Tracing
Reading
Verifying
Explaining
```

### 历史记录

每个 repo 可对应一个 workspace / session：

```text
repo
 ├── repository index
 ├── conversation history
 ├── explored symbols
 ├── saved traces
 └── understanding state
```

### Token / Cost

统计：

- prompt tokens；
- completion tokens；
- total tokens；
- estimated cost；
- cached calls；
- reused repository index。

---

## 19. 最重要的场景特化

至少应实现两个，这里推荐优先做前三个。

### A. Structural Codebase Index

使用 parser / static analysis 建立：

```text
AST
symbols
definitions
references
module graph
call graph
```

目标：

> 避免每个问题都重新让 LLM 扫整个 repo。

---

### B. Evidence-grounded Explanation

任何重要解释尽量绑定：

```text
file + symbol + line range
```

并区分：

```text
Fact / Inference / Unknown
```

目标：

> 降低 hallucination，提高可验证性。

---

### C. Question-driven Exploration

Agent 根据用户目标决定：

```text
read which file?
trace which symbol?
inspect which dependency?
continue or stop?
```

目标：

> 真正形成 Agent loop，而不是“把所有代码丢给 LLM”。

---

### D. Adaptive Explanation

根据用户水平调整解释：

```text
Beginner
Developer
Expert
```

目标：

> 优化 learning / onboarding，而不仅仅是代码搜索。

---

## 20. MVP 建议

第一版不要追求支持所有语言和所有高级静态分析。

推荐：

### 支持语言

先实现：

```text
Rust
Python
C/C++
```

如果时间紧：

```text
Rust + Python
```

但系统架构必须是 language-agnostic。

### 第一阶段 Codebase IR

优先支持：

```text
File
Module
Symbol
Function
Struct / Class
Import
Definition
Reference
```

Call graph 可以先：

> 静态近似 + LLM 辅助

而不是一开始追求完整的 interprocedural analysis。

### 第一阶段 Agent Tools

```text
list_files
read_file
find_symbol
find_references
get_symbol
get_module
search_code
trace_call
get_repository_overview
```

### 第一阶段 UI

至少做到：

```text
Conversation
+
Trace
+
Evidence
+
Progress
```

Architecture visualization 可以作为后续增强。

---

## 21. 非目标

为了控制项目复杂度，第一版不应该做：

- 完整 IDE；
- 完整 language server；
- 编译器级语义分析；
- 自动修改代码；
- 自动修 bug；
- 通用 Coding Agent；
- 所有语言的完整 call graph；
- 完整架构图编辑器。

CodeAtlas 的核心目标始终是：

> **理解，而不是修改。**

---

## 22. 项目判断标准

如果某个功能不能明显帮助用户：

```text
更快建立 mental model
```

那么它可能不是 MVP 核心功能。

优先级应该始终是：

```text
Repository Understanding
        ↓
Traceability
        ↓
Progressive Exploration
        ↓
Visualization
        ↓
Other Features
```

---

## 23. 最终项目叙事

可以这样概括整个项目：

> AI 正在极大提高代码的生成速度，但人的代码阅读速度没有同比提高。未来大型代码库的瓶颈可能不再只是“怎么写”，而是“怎么理解”。  
>   
> CodeAtlas 是一个面向陌生代码库理解的专用 Agent。它首先通过跨语言 parser 和静态分析建立结构化 Codebase IR，然后根据用户问题主动探索相关文件、symbol 和调用关系，在证据充分后生成自然语言解释。所有关键结论尽可能绑定源码证据，并允许用户从系统架构一路渐进式下钻到具体实现。  
>   
> 它不是另一个 Coding Agent，而是一个帮助人类重新获得代码理解能力的 Agent。

一句最简版本：

> **Coding agents help humans write code. CodeAtlas helps humans understand code.**

---

## 24. Codex CLI 开发时的原则

后续使用 Codex CLI 开发时，应始终遵循：

1. 不要把系统退化成“读取 repo + 拼 prompt + LLM 总结”。
2. Rust 核心应真正承担 repository analysis、index、IR、tool execution 和 Agent runtime。
3. 所有语言统一转换到 language-neutral IR。
4. Agent 应主动选择下一步工具，而不是一次性完成回答。
5. 优先保证 source evidence 可追踪。
6. 优先实现结构化理解，再做漂亮可视化。
7. MVP 聚焦“陌生 codebase onboarding”。
8. 任何功能都应回答一个问题：它是否帮助用户更快建立正确的 mental model？
9. 不与 Claude Code 正面竞争“谁更聪明”，而强调专用 workflow 的稳定性、可验证性和低重复成本。
10. 产品最终输出必须以“人类理解”为核心，而不是以“代码分析结果展示”为核心。
