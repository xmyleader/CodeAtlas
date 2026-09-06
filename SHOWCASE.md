# 【Agent 公开展示】CodeAtlas：把陌生代码库变成可验证的 Mental Model

项目地址：<https://github.com/xmyleader/CodeAtlas>

## 一句话介绍

CodeAtlas 是一个使用 Rust 实现的只读代码库理解 Agent。它会先把 Rust、Python 和 Python Stub 项目转换为可复用的语言无关结构索引，再由模型按问题自主调用受限工具探索代码，最后生成能够追溯到源码 Evidence 的渐进式解释。

## 痛点

AI 让代码生成越来越快，但人理解陌生代码库的速度没有同步提高。第一次接触一个项目时，开发者通常需要反复搜索入口、模块、引用和调用关系，还要判断模型给出的解释究竟来自源码，还是听起来合理的推测。

Claude Code、Codex 等通用 Agent 当然能够临时搜索和解释代码，但代码理解只是它们完成通用工程任务的中间能力。CodeAtlas 将“帮助人建立正确的代码库 mental model”作为最终目标，把索引、探索、验证、渐进式解释和历史审计固化为稳定工作流。

## 场景定制

CodeAtlas 不只是给通用 Agent 换一个界面，目前实现了以下专门设计：

1. **可复用的跨语言 Codebase IR**：使用 tree-sitter 解析 `.rs`、`.py`、`.pyi`，统一表示文件、模块、Symbol、Import、Reference、Call 和 Entry Point。内容指纹缓存避免每次提问都重新扫描整个仓库。
2. **Evidence-grounded 回答门禁**：模型只能提交结构化回答；Fact、Call Path 和 Diagram 都必须绑定当前探索得到的稳定 Evidence ID。运行时会拒绝无证据 Fact、悬空引用和无效图，而不是把模型推测伪装成源码事实。
3. **问题驱动的只读 Agent Loop**：模型可在九个边界明确的查询工具中自主选择下一步，包括查文件、读源码、找 Symbol/Reference、搜索代码和追踪调用。工具不能执行 Shell、写文件或修改被分析仓库。
4. **渐进式 Onboarding**：用户可选择 `Beginner`、`Developer`、`Expert`，以及 `Overview`、`Architecture`、`Workflow`、`Code`、`Detail` 等解释深度。每个回答最多给出四个由 Rust 确定性生成的强类型后续动作，用于继续调用链、深入 Claim、切换层次或本地查看源码。
5. **可审计历史和安全图示**：Session Schema 3 保存任务终态、可观察模型消息、完整工具轨迹、Evidence、Token/费用账本和预算状态。模型只提交有证据绑定的图数据，本地 Rust Renderer 确定性生成 SVG，不执行模型提供的脚本或绘图代码。

## 系统架构

```text
Repository (.rs/.py/.pyi)
        |
        v
Scanner -> Tree-sitter adapters -> Language-neutral IR -> Cache
                                                   |
Question -> Agent runtime -> 9 bounded read-only tools
                 ^                     |
                 |                     v
                 +------ Tool result + Evidence
                 |
                 v
       Structured answer validation
                 |
                 +----> Typed suggested actions
                 +----> Deterministic local SVG
                 |
                 v
          Application event bus
              /          \
         Ratatui TUI    egui GUI
```

工作区按职责拆分为 Core Contracts、Rust/Python Parser、Indexer、Query Tools、Agent Runtime、Diagram Renderer、Application、TUI 和 GUI。详细架构与三张离线生成的示例图见 [`diagram-showcase`](https://github.com/xmyleader/CodeAtlas/tree/main/diagram-showcase)。

## 作业固定要求

- **R1 Rust**：索引、IR、查询、Agent 编排、模型客户端、持久化和 UI 主控均由 Rust 实现。
- **R2 UI**：同时提供 Ratatui TUI 和 egui 原生 GUI。
- **R3 配置**：支持自定义 Endpoint、API Key、模型、上下文长度、思考模式、价格、超时和重试。
- **R4 进度与打断**：索引、搜索、追踪、读取、验证和模型等待均有实时事件，并支持协作式取消。
- **R5 历史**：可以查看、保存和加载完整 Session 与 Agent 工作轨迹。
- **R6 Token/价格**：记录每次模型尝试的输入/输出 Token、可选成本和预算状态，达到预算时自动停止。

## 快速运行

需要 Rust 1.85+ 和支持 OpenAI Chat Completions Tool Calling 的模型。完整配置见项目 `README.md` 和 `.env.example`。

```bash
export CODEATLAS_ENDPOINT="https://your-provider/v1/chat/completions"
export CODEATLAS_MODEL="your-tool-capable-model"
export CODEATLAS_API_KEY="your-api-key"

# TUI
cargo run -p codeatlas-app --bin codeatlas -- .

# GUI
cargo run -p codeatlas-app --bin codeatlas-gui -- .
```

TUI 中按 `a` 提问、`g` 设置解释层次、`A` 运行建议动作、`h` 查看历史、`c` 取消。GUI 在提问框旁提供 Audience/Depth 选择器，并在回答下显示建议动作按钮。

## 推荐试用流程

请保持同一个 Session，尝试从全局认知逐步下钻：

1. 选择 `Beginner + Overview`：`这个项目主要做什么？请列出核心能力和少量关键模块，并给出源码证据。`
2. 切换到 `Workflow`：`追踪从用户提问到证据校验完成的运行流程，区分事实、推断和未知。`
3. 切换到 `Code`：`下钻到这条流程的关键文件、类型和函数，并解释 Evidence 如何支持结论。`
4. 执行回答提供的 `ShowSource`，确认它直接打开本地源码且不产生模型调用。

也欢迎测试历史恢复、取消长任务，或者直接要求生成带源码依据的 Architecture/Flow/Relationship Diagram。

## 希望获得的反馈

1. 第一次打开陌生仓库时，Overview 是否能帮助你快速定位入口和关键模块？
2. Fact / Inference / Unknown 与 Evidence 的绑定是否足够清楚、可信？
3. 从 Overview 逐步下钻到 Workflow 和 Code 是否自然？建议动作是否符合下一步阅读需求？
4. 哪些 Rust/Python 项目结构或调用关系没有被正确识别？
5. TUI 或 GUI 中有哪些信息过多、难找或操作不直观？

说明：问题、模型按需读取的源码和会话上下文会发送给你配置的模型 Provider；请勿在未经批准的机密仓库上试用。项目只读，不会修改被分析代码。
