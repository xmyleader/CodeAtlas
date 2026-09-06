#![allow(
    clippy::too_many_lines,
    reason = "the data-heavy showcase builders are clearer when each diagram stays together"
)]

use std::{
    env,
    error::Error,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};

use codeatlas_core::{
    AgentAnswer, AnswerId, CallEdgeId, CallPath, CallPathId, CallPathStep, Claim, ClaimId,
    ClaimKind, Diagram, DiagramDecision, DiagramEdge, DiagramKind, DiagramNode, Evidence,
    EvidenceId, FileId, ModelUsage, RepositoryId, RepositoryPath, SourceSpan, SymbolId,
    TargetResolution, TokenUsage,
};
use codeatlas_diagram::render_svg;

const ARCHITECTURE_QUESTION: &str = "CodeAtlas 的核心 crate 如何分工并组合？";
const ASK_FLOW_QUESTION: &str = "从 TUI 提交问题到 Evidence-grounded 回答显示，经过哪些阶段？";
const EVIDENCE_RELATIONSHIP_QUESTION: &str =
    "AgentAnswer、Fact Claim、Evidence、CallPath 和 Diagram 如何关联并被验证？";

struct Showcase {
    slug: &'static str,
    question: &'static str,
    answer: AgentAnswer,
}

impl Showcase {
    fn svg_filename(&self) -> String {
        format!("{}.svg", self.slug)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let output_directory = env::args_os()
        .nth(1)
        .map_or_else(default_output_directory, PathBuf::from);
    ensure_output_directory(&output_directory)?;

    let showcases = build_showcases();
    let mut rendered = Vec::with_capacity(showcases.len());
    for showcase in &showcases {
        showcase.answer.validate_evidence()?;
        let (_, diagram) = required_diagram(&showcase.answer)?;
        rendered.push((showcase.svg_filename(), render_svg(diagram)?));
    }

    for (filename, svg) in &rendered {
        write_regular_file(&output_directory.join(filename), svg)?;
    }
    let readme = render_readme(&showcases)?;
    write_regular_file(&output_directory.join("README.md"), readme.as_bytes())?;

    println!(
        "Generated CodeAtlas diagram showcase in {}",
        output_directory.display()
    );
    Ok(())
}

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codeatlas-diagram must be inside the workspace crates directory")
}

fn default_output_directory() -> PathBuf {
    workspace_root().join("diagram-showcase")
}

fn ensure_output_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "showcase output is not a regular directory: {}",
                    path.display()
                ),
            ));
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "showcase output is not a regular directory: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn write_regular_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("showcase target is not a regular file: {}", path.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::write(path, contents)
}

fn build_showcases() -> Vec<Showcase> {
    vec![
        architecture_showcase(),
        ask_flow_showcase(),
        evidence_relationship_showcase(),
    ]
}

fn architecture_showcase() -> Showcase {
    let evidence = vec![
        source_evidence(
            "crates/codeatlas-core/src/lib.rs",
            1,
            4,
            "//! Parser implementations, model clients, storage backends, and user",
            None,
        ),
        source_evidence(
            "crates/codeatlas-indexer/src/lib.rs",
            1,
            16,
            "//! Safe repository scanning and language-neutral index assembly.",
            None,
        ),
        source_evidence(
            "crates/codeatlas-indexer/src/indexer.rs",
            148,
            217,
            "match self.parsers.parse(",
            Some(("method", "codeatlas_indexer::Indexer::index")),
        ),
        source_evidence(
            "crates/codeatlas-lang-rust/src/lib.rs",
            24,
            70,
            "impl ParserAdapter for RustParser {",
            Some(("method", "codeatlas_lang_rust::RustParser::parse")),
        ),
        source_evidence(
            "crates/codeatlas-lang-python/src/lib.rs",
            35,
            85,
            "impl ParserAdapter for PythonParser {",
            Some(("method", "codeatlas_lang_python::PythonParser::parse")),
        ),
        source_evidence(
            "crates/codeatlas-query/src/index.rs",
            49,
            114,
            "/// A deterministic, read-only in-memory index over one repository model.",
            Some(("struct", "codeatlas_query::RepositoryIndex")),
        ),
        source_evidence(
            "crates/codeatlas-query/src/tools.rs",
            22,
            99,
            "impl ToolExecutor for RepositoryTools {",
            Some(("struct", "codeatlas_query::RepositoryTools")),
        ),
        source_evidence(
            "crates/codeatlas-agent/src/lib.rs",
            1,
            20,
            "//! UI-independent model, agent runtime, evidence, and session services.",
            None,
        ),
        source_evidence(
            "crates/codeatlas-agent/src/runtime.rs",
            8,
            13,
            "AgentAnswer, AnswerId, AppError, AppEvent, CallPath, CallPathId, Claim, ClaimId, ClaimKind,",
            None,
        ),
        source_evidence(
            "crates/codeatlas-agent/src/runtime.rs",
            173,
            180,
            "/// Question-driven, read-only tool loop with timeout, cancellation, and evidence gates.",
            Some(("struct", "codeatlas_agent::AgentRuntime")),
        ),
        source_evidence(
            "crates/codeatlas-app/Cargo.toml",
            12,
            25,
            "codeatlas-agent = { path = \"../codeatlas-agent\" }",
            None,
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            1,
            2,
            "//! Application composition and background command dispatch for `CodeAtlas`.",
            None,
        ),
        source_evidence(
            "crates/codeatlas-tui/src/lib.rs",
            1,
            5,
            "//! execute tools, call models, or retain credentials.",
            None,
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            397,
            447,
            "parsers.register(Arc::new(RustParser::new()));",
            Some(("function", "codeatlas_app::index_repository")),
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            435,
            450,
            "let index = RepositoryIndex::new(root, report.model).map_err(|error| error.to_string())?;",
            Some(("function", "codeatlas_app::build_repository_context")),
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            466,
            522,
            "let runtime = match AgentRuntime::new(",
            Some(("function", "codeatlas_app::handle_ask")),
        ),
        source_evidence(
            "crates/codeatlas-app/src/main.rs",
            116,
            132,
            "let mut port = ChannelApplicationPort::new(channels.commands, channels.events);",
            Some(("function", "codeatlas_app::main")),
        ),
        source_evidence(
            "crates/codeatlas-tui/src/port.rs",
            31,
            59,
            "pub struct ChannelApplicationPort {",
            Some(("struct", "codeatlas_tui::ChannelApplicationPort")),
        ),
    ];

    let claims = vec![
        fact(
            "architecture",
            "core-role",
            "codeatlas-core 提供跨组件共享的语言无关契约；解析器、模型客户端、存储后端和 UI 位于该 crate 之外。",
            &[evidence[0].id],
        ),
        fact(
            "architecture",
            "indexer-role",
            "codeatlas-indexer 消费 core 的 ParseInput 与 RepositoryModel 契约，负责仓库扫描、解析分派和语言无关索引组装。",
            &[evidence[1].id, evidence[2].id],
        ),
        fact(
            "architecture",
            "parser-role",
            "Rust 与 Python language crates 都实现 core::ParserAdapter，并把 tree-sitter 解析结果转换为 ParsedFile。",
            &[evidence[3].id, evidence[4].id],
        ),
        fact(
            "architecture",
            "query-role",
            "codeatlas-query 在 RepositoryModel 上构造确定性的只读 RepositoryIndex，并通过 RepositoryTools 实现 ToolExecutor。",
            &[evidence[5].id, evidence[6].id],
        ),
        fact(
            "architecture",
            "agent-role",
            "codeatlas-agent 使用 core 的 answer、event 与 tool 契约，提供 UI 无关的只读工具循环、证据门禁和会话服务。",
            &[evidence[7].id, evidence[8].id, evidence[9].id],
        ),
        fact(
            "architecture",
            "app-role",
            "codeatlas-app 直接组合 agent、core、diagram、indexer、Rust/Python parsers、query 与 tui，并承担后台命令分发。",
            &[evidence[10].id, evidence[11].id],
        ),
        fact(
            "architecture",
            "tui-role",
            "codeatlas-tui 只接收 AppEvent 并发出 AppCommand；它不检查仓库、不执行工具，也不调用模型。",
            &[evidence[12].id],
        ),
        fact(
            "architecture",
            "register-parsers",
            "应用组合层把 RustParser 与 PythonParser 注册到 ParserRegistry，再交给 Indexer。",
            &[evidence[13].id],
        ),
        fact(
            "architecture",
            "model-to-query",
            "索引完成后，应用用 IndexReport.model 构造 RepositoryIndex，并封装为 RepositoryTools。",
            &[evidence[14].id],
        ),
        fact(
            "architecture",
            "tools-to-agent",
            "处理 Ask 时，应用把 RepositoryTools 及其 definitions 作为 AgentRuntime 的 executor 和工具清单。",
            &[evidence[15].id],
        ),
        fact(
            "architecture",
            "app-to-tui",
            "二进制把 application channels 适配为 ChannelApplicationPort，再以同一个 TuiApp 运行终端循环。",
            &[evidence[16].id, evidence[17].id],
        ),
    ];
    let text = markdown_answer(
        &[
            ("职责", &[0, 1, 2, 3, 4, 5, 6]),
            ("依赖与组合", &[7, 8, 9, 10]),
        ],
        &claims,
        &evidence,
    );

    let diagram = Diagram {
        kind: DiagramKind::Architecture,
        title: "CodeAtlas crate architecture".to_owned(),
        nodes: vec![
            node("core", "codeatlas-core\ncontracts + neutral IR", &claims[0]),
            node(
                "indexer",
                "codeatlas-indexer\nscan + parse + assemble",
                &claims[1],
            ),
            node(
                "parsers",
                "language parsers\nRust + Python adapters",
                &claims[2],
            ),
            node(
                "query",
                "codeatlas-query\nread-only index + tools",
                &claims[3],
            ),
            node(
                "agent",
                "codeatlas-agent\ntool loop + evidence gate",
                &claims[4],
            ),
            node("app", "codeatlas-app\ncomposition + dispatch", &claims[5]),
            node("tui", "codeatlas-tui\ncommands/events UI", &claims[6]),
        ],
        edges: vec![
            edge(
                "core",
                "indexer",
                "ParseInput + RepositoryModel",
                &claims[1],
            ),
            edge("core", "parsers", "ParserAdapter contract", &claims[2]),
            edge("core", "query", "model + tool contracts", &claims[3]),
            edge(
                "core",
                "agent",
                "answer + event + tool contracts",
                &claims[4],
            ),
            edge("core", "tui", "AppCommand + AppEvent", &claims[6]),
            edge("parsers", "indexer", "registered adapters", &claims[7]),
            edge("indexer", "query", "RepositoryModel", &claims[8]),
            edge("query", "agent", "RepositoryTools executor", &claims[9]),
            edge("indexer", "app", "composed indexing service", &claims[5]),
            edge("agent", "app", "runtime run by app", &claims[9]),
            edge("app", "tui", "channel application port", &claims[10]),
        ],
        artifact: None,
    };

    Showcase {
        slug: "architecture",
        question: ARCHITECTURE_QUESTION,
        answer: AgentAnswer {
            id: AnswerId::from_stable_parts(&["codeatlas-showcase", "architecture"]),
            text,
            claims,
            evidence,
            call_paths: Vec::new(),
            diagram: DiagramDecision::Needed {
                reason:
                    "多个 crate 的职责、共享契约和组合依赖同时存在，architecture 图能减少边界歧义。"
                        .to_owned(),
                diagram,
            },
            suggested_actions: Vec::new(),
            usage: Some(offline_usage()),
        },
    }
}

fn ask_flow_showcase() -> Showcase {
    let evidence = vec![
        source_evidence(
            "crates/codeatlas-tui/src/app.rs",
            980,
            1030,
            "Some(AppCommand::Ask {",
            Some(("method", "codeatlas_tui::TuiApp::submit_question")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/app.rs",
            8,
            42,
            "pub enum AppCommand {",
            Some(("enum", "codeatlas_core::AppCommand")),
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            184,
            227,
            "handle_ask(",
            Some(("function", "codeatlas_app::dispatch_command")),
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            466,
            630,
            "runtime.run(request, cancellation.clone(), &sink),",
            Some(("function", "codeatlas_app::handle_ask")),
        ),
        source_evidence(
            "crates/codeatlas-query/src/tools.rs",
            10,
            99,
            "impl ToolExecutor for RepositoryTools {",
            Some(("struct", "codeatlas_query::RepositoryTools")),
        ),
        source_evidence(
            "crates/codeatlas-agent/src/runtime.rs",
            510,
            630,
            "match evidence.insert(item.clone()) {",
            Some(("method", "codeatlas_agent::AgentRuntime::run_inner")),
        ),
        source_evidence(
            "crates/codeatlas-agent/src/runtime.rs",
            1420,
            1490,
            "answer.validate_evidence()?;",
            Some(("function", "codeatlas_agent::build_answer")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            153,
            191,
            "pub fn validate_evidence(&self) -> Result<(), EvidenceValidationError> {",
            Some(("method", "codeatlas_core::AgentAnswer::validate_evidence")),
        ),
        source_evidence(
            "crates/codeatlas-agent/src/session.rs",
            83,
            113,
            "/// Adds a completed exchange and its user-visible execution workflow.",
            Some((
                "method",
                "codeatlas_agent::SessionState::append_turn_with_workflow",
            )),
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            568,
            630,
            "let save = tokio::task::spawn_blocking(move || store.save(&to_save));",
            Some(("function", "codeatlas_app::handle_ask")),
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            960,
            1010,
            "// The application publishes one final answer after optional diagram processing.",
            Some(("method", "codeatlas_app::RecordingEventSink::emit")),
        ),
        source_evidence(
            "crates/codeatlas-app/src/lib.rs",
            568,
            590,
            "state.emit(AppEvent::AnswerCompleted { request_id, answer });",
            Some(("function", "codeatlas_app::handle_ask")),
        ),
        source_evidence(
            "crates/codeatlas-tui/src/terminal.rs",
            136,
            147,
            "app.reduce(event);",
            Some(("function", "codeatlas_tui::drain_application_events")),
        ),
        source_evidence(
            "crates/codeatlas-tui/src/app.rs",
            1085,
            1152,
            "AppEvent::AnswerCompleted { request_id, answer } => {",
            Some(("method", "codeatlas_tui::TuiApp::reduce")),
        ),
        source_evidence(
            "crates/codeatlas-tui/src/app.rs",
            1900,
            1921,
            "view.complete = true;",
            Some(("method", "codeatlas_tui::TuiApp::reduce_answer_completed")),
        ),
    ];

    let claims = vec![
        fact(
            "ask-flow",
            "submit",
            "TuiApp::submit_question 校验索引与输入状态、记录问题和 active request，然后返回 AppCommand::Ask。",
            &[evidence[0].id],
        ),
        fact(
            "ask-flow",
            "command-contract",
            "AppCommand::Ask 携带 request_id、session_id、repository_id 和 question。",
            &[evidence[1].id],
        ),
        fact(
            "ask-flow",
            "dispatch",
            "应用命令分发器为 Ask 注册取消令牌，并在异步任务中调用 handle_ask。",
            &[evidence[2].id],
        ),
        fact(
            "ask-flow",
            "handle-ask",
            "handle_ask 取得仓库与会话历史，用 RepositoryTools 构造 AgentRuntime，并运行 AgentRequest。",
            &[evidence[3].id],
        ),
        fact(
            "ask-flow",
            "repository-tools",
            "RepositoryTools 暴露九个仓库查询名称，并通过 core::ToolExecutor 执行只读查询。",
            &[evidence[4].id],
        ),
        fact(
            "ask-flow",
            "tool-loop",
            "AgentRuntime 执行已注册工具，解析成功的 RepositoryToolEnvelope，去重收集 Evidence，并发出 EvidenceAdded。",
            &[evidence[5].id],
        ),
        fact(
            "ask-flow",
            "validation",
            "finalize_answer 构造 AgentAnswer，并在返回前调用 answer.validate_evidence() 进行证据门禁。",
            &[evidence[6].id, evidence[7].id],
        ),
        fact(
            "ask-flow",
            "session-revalidation",
            "SessionState::append_turn_with_workflow 会再次验证 AgentAnswer 的 evidence contract，并保存可审计工作流。",
            &[evidence[8].id],
        ),
        fact(
            "ask-flow",
            "session-save",
            "应用把更新后的 SessionState 写回内存并发送最终 AnswerCompleted，磁盘保存失败不会撤销答案。",
            &[evidence[9].id, evidence[11].id],
        ),
        fact(
            "ask-flow",
            "completion-order",
            "RecordingEventSink 会记录并转发工作流、过滤 AgentRuntime 自身的 AnswerCompleted，因此最终完成事件由应用统一发布。",
            &[evidence[10].id, evidence[11].id],
        ),
        fact(
            "ask-flow",
            "tui-reduce",
            "终端循环非阻塞地排空事件并调用 TuiApp::reduce；AnswerCompleted 被归约为包含 text、claims、call_paths、diagram 和 evidence 的 complete AnswerView。",
            &[evidence[12].id, evidence[13].id, evidence[14].id],
        ),
    ];
    let text = markdown_answer(
        &[("阶段", &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10])],
        &claims,
        &evidence,
    );

    let diagram = Diagram {
        kind: DiagramKind::Flow,
        title: "TUI question to grounded answer".to_owned(),
        nodes: vec![
            node("submit", "TuiApp\nsubmit_question", &claims[0]),
            node("command", "AppCommand::Ask", &claims[1]),
            node("handle", "app dispatch\nhandle_ask", &claims[3]),
            node("runtime", "AgentRuntime\nmodel/tool loop", &claims[3]),
            node("tools", "RepositoryTools\nread-only queries", &claims[4]),
            node(
                "validate",
                "Evidence validation\nAgentAnswer gate",
                &claims[6],
            ),
            node("session", "append_turn\nSessionStore::save", &claims[8]),
            node("completed", "AppEvent\nAnswerCompleted", &claims[9]),
            node("reduce", "TUI reduce\ncomplete AnswerView", &claims[10]),
        ],
        edges: vec![
            edge("submit", "command", "build command", &claims[0]),
            edge("command", "handle", "dispatch async task", &claims[2]),
            edge("handle", "runtime", "run AgentRequest", &claims[3]),
            edge("runtime", "tools", "execute registered tool", &claims[5]),
            edge("tools", "runtime", "data + Evidence envelope", &claims[5]),
            edge("runtime", "validate", "finalize answer", &claims[6]),
            edge("validate", "session", "append revalidates", &claims[7]),
            edge("session", "completed", "save before emit", &claims[8]),
            edge("completed", "reduce", "application event", &claims[10]),
        ],
        artifact: None,
    };

    Showcase {
        slug: "ask-flow",
        question: ASK_FLOW_QUESTION,
        answer: AgentAnswer {
            id: AnswerId::from_stable_parts(&["codeatlas-showcase", "ask-flow"]),
            text,
            claims,
            evidence,
            call_paths: Vec::new(),
            diagram: DiagramDecision::Needed {
                reason: "请求跨越命令边界、工具循环、验证、持久化和事件归约，flow 图能明确阶段与返回环。"
                    .to_owned(),
                diagram,
            },
            suggested_actions: Vec::new(),
            usage: Some(offline_usage()),
        },
    }
}

fn evidence_relationship_showcase() -> Showcase {
    let evidence = vec![
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            16,
            24,
            "pub struct Evidence {",
            Some(("struct", "codeatlas_core::Evidence")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            141,
            151,
            "pub struct AgentAnswer {",
            Some(("struct", "codeatlas_core::AgentAnswer")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            34,
            65,
            "pub struct Claim {",
            Some(("struct", "codeatlas_core::Claim")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            153,
            191,
            "pub fn validate_evidence(&self) -> Result<(), EvidenceValidationError> {",
            Some(("method", "codeatlas_core::AgentAnswer::validate_evidence")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            126,
            139,
            "pub struct CallPath {",
            Some(("struct", "codeatlas_core::CallPath")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            99,
            124,
            "pub struct Diagram {",
            Some(("struct", "codeatlas_core::Diagram")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            210,
            220,
            "fn validate_diagram_decision(",
            Some(("function", "codeatlas_core::validate_diagram_decision")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            232,
            294,
            "fn validate_diagram(",
            Some(("function", "codeatlas_core::validate_diagram")),
        ),
        source_evidence(
            "crates/codeatlas-core/src/evidence.rs",
            297,
            353,
            "fn validate_diagram_bindings(",
            Some(("function", "codeatlas_core::validate_diagram_bindings")),
        ),
    ];

    let claims = vec![
        fact(
            "evidence-relationship",
            "evidence-record",
            "Evidence 是带稳定 ID 的源码记录，包含 file_id、RepositoryPath、SourceSpan、可选 symbol_id 与 excerpt。",
            &[evidence[0].id],
        ),
        fact(
            "evidence-relationship",
            "answer-aggregate",
            "AgentAnswer 聚合 Markdown text、claims、evidence、call_paths、diagram decision 与可选 usage。",
            &[evidence[1].id],
        ),
        fact(
            "evidence-relationship",
            "fact-support",
            "Fact Claim 必须携带 evidence_ids，且 AgentAnswer 校验会拒绝不在 answer.evidence 中的引用。",
            &[evidence[2].id, evidence[3].id],
        ),
        fact(
            "evidence-relationship",
            "call-path-support",
            "CallPath 的每个 step 可以引用 Evidence；AgentAnswer 校验会拒绝未知的 call-path evidence ID。",
            &[evidence[4].id, evidence[3].id],
        ),
        fact(
            "evidence-relationship",
            "diagram-shape",
            "Diagram 的每个 node 和 edge 都分别携带 claim_ids 与 evidence_ids。",
            &[evidence[5].id],
        ),
        fact(
            "evidence-relationship",
            "diagram-binding",
            "Diagram 绑定校验要求每个元素引用至少一个 Fact Claim 和 Evidence；每条 Evidence 必须属于至少一个已绑定 Claim，每个 Claim 也必须由至少一条元素 Evidence 覆盖。",
            &[evidence[8].id],
        ),
        fact(
            "evidence-relationship",
            "validation-walk",
            "AgentAnswer::validate_evidence 先建立已知 Evidence，校验 Claims 与 CallPaths，再进入 DiagramDecision、Diagram 和元素绑定校验。",
            &[
                evidence[3].id,
                evidence[6].id,
                evidence[7].id,
                evidence[8].id,
            ],
        ),
    ];
    let text = markdown_answer(
        &[("实体关系", &[0, 1, 2, 3, 4, 5]), ("验证", &[6])],
        &claims,
        &evidence,
    );

    let diagram = Diagram {
        kind: DiagramKind::Relationship,
        title: "Grounded answer relationships".to_owned(),
        nodes: vec![
            node("answer", "AgentAnswer", &claims[1]),
            node("claim", "Fact Claim", &claims[2]),
            node("evidence", "Evidence\npath + span + excerpt", &claims[0]),
            node("call-path", "CallPath\nsteps", &claims[3]),
            node("diagram", "Diagram\nnodes + edges", &claims[4]),
            node(
                "validation",
                "validate_evidence\nvalidation gate",
                &claims[6],
            ),
        ],
        edges: vec![
            edge("answer", "claim", "contains", &claims[1]),
            edge("answer", "evidence", "carries", &claims[1]),
            edge("answer", "call-path", "contains", &claims[1]),
            edge("answer", "diagram", "DiagramDecision", &claims[1]),
            edge("claim", "evidence", "cites evidence_ids", &claims[2]),
            edge("call-path", "evidence", "step evidence_ids", &claims[3]),
            edge("diagram", "claim", "binds Fact claim_ids", &claims[5]),
            edge(
                "diagram",
                "evidence",
                "binds linked evidence_ids",
                &claims[5],
            ),
            edge("answer", "validation", "validate_evidence()", &claims[6]),
            edge("validation", "claim", "checks Fact support", &claims[6]),
            edge(
                "validation",
                "call-path",
                "checks known Evidence",
                &claims[6],
            ),
            edge("validation", "diagram", "checks all bindings", &claims[6]),
        ],
        artifact: None,
    };

    let call_paths = vec![CallPath {
        id: CallPathId::from_stable_parts(&[
            "codeatlas-showcase",
            "evidence-relationship",
            "validation-path",
        ]),
        label: Some("AgentAnswer evidence-validation calls".to_owned()),
        steps: vec![
            validation_step(
                "method",
                "codeatlas_core::AgentAnswer::validate_evidence",
                None,
                &[evidence[3].id],
            ),
            validation_step(
                "method",
                "codeatlas_core::Claim::validate_evidence",
                Some("answer-to-claim-validation"),
                &[evidence[2].id],
            ),
            validation_step(
                "function",
                "codeatlas_core::validate_diagram_decision",
                Some("answer-to-diagram-decision"),
                &[evidence[6].id],
            ),
            validation_step(
                "function",
                "codeatlas_core::validate_diagram",
                Some("decision-to-diagram-validation"),
                &[evidence[7].id],
            ),
            validation_step(
                "function",
                "codeatlas_core::validate_diagram_bindings",
                Some("diagram-to-binding-validation"),
                &[evidence[8].id],
            ),
        ],
        complete: true,
    }];

    Showcase {
        slug: "evidence-relationship",
        question: EVIDENCE_RELATIONSHIP_QUESTION,
        answer: AgentAnswer {
            id: AnswerId::from_stable_parts(&["codeatlas-showcase", "evidence-relationship"]),
            text,
            claims,
            evidence,
            call_paths,
            diagram: DiagramDecision::Needed {
                reason:
                    "多个实体通过 ID 引用和验证约束相连，relationship 图能显示所有权、引用和门禁。"
                        .to_owned(),
                diagram,
            },
            suggested_actions: Vec::new(),
            usage: Some(offline_usage()),
        },
    }
}

fn source_evidence(
    path: &str,
    start_line: u32,
    end_line: u32,
    excerpt: &str,
    symbol: Option<(&str, &str)>,
) -> Evidence {
    let repository_id = RepositoryId::from_stable_parts(&["codeatlas-showcase-repository-v1"]);
    let repository_key = repository_id.to_string();
    let span = SourceSpan::new(start_line, 0, end_line.saturating_add(1), 0)
        .expect("showcase source ranges are valid and one-based");
    let symbol_id = symbol.map(|(kind, qualified_name)| {
        SymbolId::from_stable_parts(&[&repository_key, path, kind, qualified_name])
    });
    let symbol_key = symbol_id.map_or_else(String::new, |id| id.to_string());
    let span_key = format!("{start_line}:0-{}:0", end_line.saturating_add(1));

    Evidence {
        id: EvidenceId::from_stable_parts(&[&repository_key, path, &span_key, &symbol_key]),
        file_id: FileId::from_stable_parts(&[&repository_key, path]),
        path: RepositoryPath::new(path).expect("showcase paths are repository-relative"),
        span,
        symbol_id,
        excerpt: Some(excerpt.to_owned()),
    }
}

fn fact(showcase: &str, key: &str, text: &str, evidence_ids: &[EvidenceId]) -> Claim {
    Claim {
        id: ClaimId::from_stable_parts(&["codeatlas-showcase", showcase, key]),
        kind: ClaimKind::Fact,
        text: text.to_owned(),
        evidence_ids: evidence_ids.to_vec(),
    }
}

fn node(id: &str, label: &str, claim: &Claim) -> DiagramNode {
    DiagramNode {
        id: id.to_owned(),
        label: label.to_owned(),
        claim_ids: vec![claim.id],
        evidence_ids: claim.evidence_ids.clone(),
    }
}

fn edge(source: &str, target: &str, label: &str, claim: &Claim) -> DiagramEdge {
    DiagramEdge {
        source: source.to_owned(),
        target: target.to_owned(),
        label: label.to_owned(),
        claim_ids: vec![claim.id],
        evidence_ids: claim.evidence_ids.clone(),
    }
}

fn validation_step(
    kind: &str,
    qualified_name: &str,
    incoming_edge: Option<&str>,
    evidence_ids: &[EvidenceId],
) -> CallPathStep {
    let repository_id = RepositoryId::from_stable_parts(&["codeatlas-showcase-repository-v1"]);
    let repository_key = repository_id.to_string();
    CallPathStep {
        target: TargetResolution::Resolved(SymbolId::from_stable_parts(&[
            &repository_key,
            "crates/codeatlas-core/src/evidence.rs",
            kind,
            qualified_name,
        ])),
        call_edge_id: incoming_edge.map(|edge| {
            CallEdgeId::from_stable_parts(&["codeatlas-showcase", "evidence-relationship", edge])
        }),
        evidence_ids: evidence_ids.to_vec(),
    }
}

fn offline_usage() -> ModelUsage {
    ModelUsage {
        tokens: TokenUsage::default(),
        cost: None,
    }
}

fn markdown_answer(
    sections: &[(&str, &[usize])],
    claims: &[Claim],
    evidence: &[Evidence],
) -> String {
    let mut answer = String::new();
    for (section_index, (heading, claim_indices)) in sections.iter().enumerate() {
        if section_index > 0 {
            answer.push('\n');
        }
        writeln!(answer, "**{heading}**\n").expect("writing Markdown to a String cannot fail");
        for claim_index in *claim_indices {
            let claim = &claims[*claim_index];
            write!(answer, "- {}", claim.text).expect("writing Markdown to a String cannot fail");
            for evidence_id in &claim.evidence_ids {
                let number = evidence
                    .iter()
                    .position(|item| item.id == *evidence_id)
                    .map(|index| index + 1)
                    .expect("showcase claims only cite carried evidence");
                write!(answer, " [E{number}]").expect("writing Markdown to a String cannot fail");
            }
            answer.push('\n');
        }
    }
    answer
}

fn required_diagram(answer: &AgentAnswer) -> io::Result<(&str, &Diagram)> {
    match &answer.diagram {
        DiagramDecision::Needed { reason, diagram } => Ok((reason, diagram)),
        DiagramDecision::NotNeeded { .. } => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "showcase answer unexpectedly omitted its diagram",
        )),
    }
}

fn render_readme(showcases: &[Showcase]) -> io::Result<String> {
    let mut readme = String::from(
        "# CodeAtlas Diagram Showcase\n\n此目录由 `codeatlas_showcase` example 离线、确定性生成；不调用模型、外部绘图工具或应用 artifact store。\n\n",
    );
    for (index, showcase) in showcases.iter().enumerate() {
        let (reason, diagram) = required_diagram(&showcase.answer)?;
        writeln!(readme, "## {}. `{}`\n", index + 1, showcase.slug)
            .expect("writing README to a String cannot fail");
        writeln!(readme, "### 问题\n\n> {}\n", showcase.question)
            .expect("writing README to a String cannot fail");
        writeln!(readme, "### 回答\n\n{}", showcase.answer.text)
            .expect("writing README to a String cannot fail");
        writeln!(
            readme,
            "### 图\n\n![{}]({})\n\n- 图类型：`{}`\n- 选择理由：{}\n",
            diagram.title,
            showcase.svg_filename(),
            kind_name(diagram.kind),
            reason,
        )
        .expect("writing README to a String cannot fail");
        readme.push_str("### Evidence\n\n");
        for (evidence_index, item) in showcase.answer.evidence.iter().enumerate() {
            let excerpt = item
                .excerpt
                .as_deref()
                .unwrap_or("(no excerpt)")
                .replace('\n', " ");
            writeln!(
                readme,
                "- **E{}** `{}` - `` {} ``",
                evidence_index + 1,
                evidence_location(item),
                excerpt,
            )
            .expect("writing README to a String cannot fail");
        }
        readme.push('\n');
    }
    readme.push_str(
        "## 自动验证\n\n每个 `AgentAnswer` 都在 `render_svg()` 之前执行 `validate_evidence()`；任一 Fact、CallPath 或 Diagram 绑定无效都会令 example 以错误退出。三个 SVG 仅由 `codeatlas_diagram::render_svg` 生成。稳定 ID、固定输入顺序和无时间戳输出保证相同源码下的结果可重复。\n",
    );
    Ok(readme)
}

fn evidence_location(evidence: &Evidence) -> String {
    let start = evidence.span.start().line();
    let end = evidence_display_end_line(evidence);
    if start == end {
        format!("{}:{start}", evidence.path)
    } else {
        format!("{}:{start}-{end}", evidence.path)
    }
}

fn evidence_display_end_line(evidence: &Evidence) -> u32 {
    let start = evidence.span.start().line();
    let end = evidence.span.end();
    if end.column() == 0 && end.line() > start {
        end.line() - 1
    } else {
        end.line()
    }
}

const fn kind_name(kind: DiagramKind) -> &'static str {
    match kind {
        DiagramKind::Architecture => "architecture",
        DiagramKind::Flow => "flow",
        DiagramKind::Relationship => "relationship",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn showcases_validate_bind_render_and_match_current_sources() {
        let showcases = build_showcases();
        assert_eq!(showcases.len(), 3);

        for showcase in &showcases {
            showcase
                .answer
                .validate_evidence()
                .expect("showcase answer should pass evidence validation");
            let (_, diagram) = required_diagram(&showcase.answer).expect("diagram is required");
            assert_question_kind(showcase, diagram.kind);

            let mut linked_claims = HashSet::new();
            for node in &diagram.nodes {
                assert_element_binding(
                    &showcase.answer,
                    &node.claim_ids,
                    &node.evidence_ids,
                    &mut linked_claims,
                );
            }
            for edge in &diagram.edges {
                assert_element_binding(
                    &showcase.answer,
                    &edge.claim_ids,
                    &edge.evidence_ids,
                    &mut linked_claims,
                );
            }
            assert_eq!(linked_claims.len(), showcase.answer.claims.len());

            let svg = String::from_utf8(render_svg(diagram).expect("SVG should render"))
                .expect("SVG should be UTF-8");
            assert!(svg.contains(&format!("data-kind=\"{}\"", kind_name(diagram.kind))));
            assert!(svg.contains(&format!(
                "<title id=\"diagram-title-0\">{}</title>",
                diagram.title
            )));
            assert_svg_bindings(&svg, diagram);
            assert_current_source_evidence(&showcase.answer.evidence);
        }

        let readme = render_readme(&showcases).expect("README should render");
        for showcase in &showcases {
            assert!(readme.contains(showcase.question));
            assert!(readme.contains(&format!("]({})", showcase.svg_filename())));
        }
        assert!(readme.contains("validate_evidence()"));
        assert!(readme.contains("render_svg"));
    }

    fn assert_question_kind(showcase: &Showcase, actual_kind: DiagramKind) {
        let (question, expected_kind) = match showcase.slug {
            "architecture" => (ARCHITECTURE_QUESTION, DiagramKind::Architecture),
            "ask-flow" => (ASK_FLOW_QUESTION, DiagramKind::Flow),
            "evidence-relationship" => (EVIDENCE_RELATIONSHIP_QUESTION, DiagramKind::Relationship),
            slug => panic!("unexpected showcase slug: {slug}"),
        };
        assert_eq!(showcase.question, question);
        assert_eq!(actual_kind, expected_kind);
    }

    fn assert_element_binding(
        answer: &AgentAnswer,
        claim_ids: &[ClaimId],
        evidence_ids: &[EvidenceId],
        linked_claims: &mut HashSet<ClaimId>,
    ) {
        assert_eq!(claim_ids.len(), 1);
        let claim = answer
            .claims
            .iter()
            .find(|claim| claim.id == claim_ids[0])
            .expect("diagram claim should exist in answer");
        assert_eq!(claim.kind, ClaimKind::Fact);
        assert_eq!(evidence_ids, claim.evidence_ids);
        assert!(answer.text.contains(&claim.text));
        linked_claims.insert(claim.id);

        for evidence_id in evidence_ids {
            let index = answer
                .evidence
                .iter()
                .position(|evidence| evidence.id == *evidence_id)
                .expect("diagram evidence should exist in answer");
            assert!(answer.text.contains(&format!("[E{}]", index + 1)));
        }
    }

    fn assert_svg_bindings(svg: &str, diagram: &Diagram) {
        for (claim_ids, evidence_ids) in diagram
            .nodes
            .iter()
            .map(|node| (&node.claim_ids, &node.evidence_ids))
            .chain(
                diagram
                    .edges
                    .iter()
                    .map(|edge| (&edge.claim_ids, &edge.evidence_ids)),
            )
        {
            let claims = claim_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            let evidence = evidence_ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(svg.contains(&format!(
                "data-claim-ids=\"{claims}\" data-evidence-ids=\"{evidence}\""
            )));
        }
    }

    fn assert_current_source_evidence(evidence: &[Evidence]) {
        for item in evidence {
            let path = workspace_root().join(item.path.as_str());
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            let start = usize::try_from(item.span.start().line())
                .expect("line should fit usize")
                .saturating_sub(1);
            let end =
                usize::try_from(evidence_display_end_line(item)).expect("line should fit usize");
            let selected = source
                .lines()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect::<Vec<_>>()
                .join("\n");
            let excerpt = item
                .excerpt
                .as_deref()
                .expect("showcase evidence has excerpt");
            assert!(
                selected.contains(excerpt),
                "{} does not contain excerpt {:?}",
                evidence_location(item),
                excerpt,
            );
        }
    }
}
