use std::sync::mpsc;

use codeatlas_core::{
    AgentAnswer, AnswerId, AppCommand, AppError, AppEvent, CallPath, CallPathId, CallPathStep,
    Claim, ClaimId, ClaimKind, Cost, Diagram, DiagramArtifact, DiagramDecision, DiagramEdge,
    DiagramId, DiagramKind, DiagramNode, EntryPointKind, Evidence, EvidenceId, FileId, Language,
    ModelUsage, Progress, ProgressPhase, RepositoryEntryPoint, RepositoryId, RepositoryLanguage,
    RepositoryMap, RepositoryModule, RepositoryPath, RequestId, SessionContext, SessionId,
    SessionSummary, SessionTask, SessionTaskSummary, SourceSpan, SymbolId, TargetResolution,
    TokenUsage, ToolCall, ToolCallId, ToolOutput, WorkflowEvent,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use serde_json::json;

use crate::{
    Activity, ApplicationPort, ChannelApplicationPort, ConversationEntry, EvidenceViewer,
    InputMode, LayoutMode, Panel, ToolTraceStatus, TuiApp, UiPreferences, terminal::encode_base64,
};

fn id_request(label: &str) -> RequestId {
    RequestId::from_stable_parts(&["test", label])
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn type_text(app: &mut TuiApp, text: &str) {
    for character in text.chars() {
        assert!(app.handle_key(key(KeyCode::Char(character))).is_none());
    }
}

fn sample_evidence(label: &str, path: &str, start: u32) -> Evidence {
    Evidence {
        id: EvidenceId::from_stable_parts(&[label]),
        file_id: FileId::from_stable_parts(&[path]),
        path: RepositoryPath::new(path).expect("valid test repository path"),
        span: SourceSpan::new(start, 0, start + 2, 1).expect("valid test source span"),
        symbol_id: Some(SymbolId::from_stable_parts(&[label, "symbol"])),
        excerpt: Some("fn route() {\n    service.call();\n}".to_owned()),
    }
}

fn add_evidence(app: &mut TuiApp, evidence: Evidence) {
    let key = evidence.id.to_string();
    app.reduce(AppEvent::AnswerCompleted {
        request_id: RequestId::from_stable_parts(&["test", "evidence", &key]),
        answer: AgentAnswer {
            id: AnswerId::from_stable_parts(&["test", "evidence-answer", &key]),
            text: "Evidence fixture".to_owned(),
            claims: vec![Claim {
                id: ClaimId::from_stable_parts(&["test", "evidence-claim", &key]),
                kind: ClaimKind::Fact,
                text: "Fixture claim".to_owned(),
                evidence_ids: vec![evidence.id],
            }],
            evidence: vec![evidence],
            call_paths: Vec::new(),
            diagram: DiagramDecision::NotNeeded {
                reason: "A fixture does not need a diagram.".to_owned(),
            },
            usage: None,
        },
    });
}

fn sample_answer(evidence: &Evidence) -> AgentAnswer {
    AgentAnswer {
        id: AnswerId::from_stable_parts(&["answer"]),
        text: "The route delegates to the service.".to_owned(),
        claims: vec![
            Claim {
                id: ClaimId::from_stable_parts(&["fact"]),
                kind: ClaimKind::Fact,
                text: "route calls service.call".to_owned(),
                evidence_ids: vec![evidence.id],
            },
            Claim {
                id: ClaimId::from_stable_parts(&["inference"]),
                kind: ClaimKind::Inference,
                text: "the split likely isolates routing".to_owned(),
                evidence_ids: vec![evidence.id],
            },
            Claim {
                id: ClaimId::from_stable_parts(&["unknown"]),
                kind: ClaimKind::Unknown,
                text: "the original design intent is unknown".to_owned(),
                evidence_ids: Vec::new(),
            },
        ],
        evidence: vec![evidence.clone()],
        call_paths: vec![CallPath {
            id: CallPathId::from_stable_parts(&["route-path"]),
            label: Some("request route".to_owned()),
            steps: vec![CallPathStep {
                target: TargetResolution::Resolved(
                    evidence
                        .symbol_id
                        .expect("sample evidence always has a symbol"),
                ),
                call_edge_id: None,
                evidence_ids: vec![evidence.id],
            }],
            complete: true,
        }],
        diagram: DiagramDecision::NotNeeded {
            reason: "The answer is direct enough without a diagram.".to_owned(),
        },
        usage: Some(ModelUsage {
            tokens: TokenUsage {
                input_tokens: 800,
                output_tokens: 200,
                cached_input_tokens: 100,
                total_tokens: 1_000,
            },
            cost: Some(Cost {
                currency: "USD".to_owned(),
                amount: 0.0125,
                estimated: true,
            }),
        }),
    }
}

fn sample_needed_diagram(
    kind: DiagramKind,
    evidence_id: EvidenceId,
    artifact_path: Option<&str>,
) -> DiagramDecision {
    DiagramDecision::Needed {
        reason: "The request crosses multiple components.".to_owned(),
        diagram: Diagram {
            kind,
            title: "Request routing".to_owned(),
            nodes: vec![
                DiagramNode {
                    id: "router".to_owned(),
                    label: "Router".to_owned(),
                    claim_ids: Vec::new(),
                    evidence_ids: vec![evidence_id],
                },
                DiagramNode {
                    id: "service".to_owned(),
                    label: "Service".to_owned(),
                    claim_ids: Vec::new(),
                    evidence_ids: vec![evidence_id],
                },
                DiagramNode {
                    id: "store".to_owned(),
                    label: "Store".to_owned(),
                    claim_ids: Vec::new(),
                    evidence_ids: Vec::new(),
                },
            ],
            edges: vec![
                DiagramEdge {
                    source: "router".to_owned(),
                    target: "service".to_owned(),
                    label: "delegates".to_owned(),
                    claim_ids: Vec::new(),
                    evidence_ids: vec![evidence_id],
                },
                DiagramEdge {
                    source: "service".to_owned(),
                    target: "store".to_owned(),
                    label: "reads".to_owned(),
                    claim_ids: Vec::new(),
                    evidence_ids: Vec::new(),
                },
            ],
            artifact: artifact_path.map(|path| DiagramArtifact {
                id: DiagramId::from_stable_parts(&["diagram-artifact"]),
                path: path.to_owned(),
                media_type: "image/svg+xml".to_owned(),
                byte_size: 1_024,
            }),
        },
    }
}

fn rendered_buffer(app: &mut TuiApp, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal is valid");
    terminal
        .draw(|frame| app.render(frame))
        .expect("render to TestBackend succeeds");
    terminal.backend().buffer().clone()
}

fn rendered_text(app: &mut TuiApp, width: u16, height: u16) -> String {
    let buffer = rendered_buffer(app, width, height);
    let mut text = String::new();
    for y in 0..height {
        for x in 0..width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn assert_source_is_highlighted(buffer: &Buffer, width: u16, height: u16, source: &str) {
    const MUTED: Color = Color::Rgb(116, 128, 141);
    const FALLBACK: Color = Color::Rgb(180, 210, 202);

    let source_width =
        u16::try_from(source.chars().count()).expect("test source fits terminal width");
    for y in 0..height {
        let source_x = (0..=width.saturating_sub(source_width)).find(|start| {
            (*start..start.saturating_add(source_width))
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                == source
        });
        let Some(source_x) = source_x else {
            continue;
        };
        assert_eq!(buffer[(source_x.saturating_sub(2), y)].symbol(), "|");
        assert_eq!(buffer[(source_x.saturating_sub(2), y)].fg, MUTED);

        let mut colors = Vec::new();
        for x in source_x..source_x.saturating_add(source_width) {
            let cell = &buffer[(x, y)];
            if !cell.symbol().trim().is_empty() && !colors.contains(&cell.fg) {
                colors.push(cell.fg);
            }
        }
        assert!(colors.len() >= 2, "expected multiple syntax token colors");
        assert!(
            colors.iter().any(|color| *color != FALLBACK),
            "expected syntax colors instead of only the fallback color"
        );
        return;
    }

    panic!("source row {source:?} was not rendered");
}

fn complete_index(app: &mut TuiApp, path: &str) -> RepositoryId {
    app.set_repository_path(path);
    let command = app
        .submit_repository()
        .expect("valid path creates an index command");
    let request_id = match command {
        AppCommand::Index { request_id, .. } => request_id,
        AppCommand::Ask { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected index command")
        }
    };
    let repository_id = RepositoryId::from_stable_parts(&[path]);
    app.reduce(AppEvent::IndexCompleted {
        request_id,
        repository_id,
        file_count: 183,
        symbol_count: 1_241,
        repository_map: RepositoryMap::default(),
    });
    repository_id
}

fn sample_repository_map() -> RepositoryMap {
    RepositoryMap {
        name: "CodeAtlas".to_owned(),
        module_count: 18,
        call_count: 247,
        unresolved_call_count: 9,
        languages: vec![
            RepositoryLanguage {
                language: Language::Rust,
                file_count: 58,
            },
            RepositoryLanguage {
                language: Language::Python,
                file_count: 2,
            },
        ],
        modules: vec![RepositoryModule {
            name: "app".to_owned(),
            path: RepositoryPath::new("crates/codeatlas-tui/src/app.rs")
                .expect("valid module path"),
        }],
        modules_truncated: true,
        entry_points: vec![RepositoryEntryPoint {
            kind: EntryPointKind::Executable,
            label: "codeatlas".to_owned(),
            path: RepositoryPath::new("crates/codeatlas-app/src/main.rs")
                .expect("valid entry path"),
            line: 82,
        }],
        entry_points_truncated: false,
    }
}

#[test]
fn wide_layout_renders_three_code_reading_panels() {
    let mut app = TuiApp::new("wide-render");
    assert_eq!(app.layout_mode(160, 40), LayoutMode::Wide);

    let text = rendered_text(&mut app, 160, 40);

    assert!(text.contains("Repository / Code Map"));
    assert!(text.contains("Conversation"));
    assert!(text.contains("Source Evidence"));
    assert!(text.contains("files 0 | symbols 0 | tokens 0"));
}

#[test]
fn indexed_repository_renders_languages_modules_and_entry_points() {
    let mut app = TuiApp::new("repository-map");
    app.set_repository_path("/workspace/codeatlas");
    let AppCommand::Index { request_id, .. } = app.submit_repository().expect("index command")
    else {
        panic!("expected index command");
    };
    app.reduce(AppEvent::IndexCompleted {
        request_id,
        repository_id: RepositoryId::from_stable_parts(&["repository-map"]),
        file_count: 60,
        symbol_count: 1_824,
        repository_map: sample_repository_map(),
    });

    let text = rendered_text(&mut app, 100, 32);
    assert!(text.contains("CODE MAP"));
    assert!(text.contains("Rust 58 / Python 2"));
    assert!(text.contains("18 modules / 247 calls"));
    assert!(text.contains("238 linked / 9 unresolved"));
    assert!(text.contains("app  codeatlas-tui/src/app.rs"));
    assert!(text.contains("... more modules"));
    assert!(text.contains("codeatlas"));
    assert!(text.contains("codeatlas-app/src/main.rs:82"));
}

#[test]
fn tool_payloads_are_hidden_until_the_user_requests_details() {
    let mut app = TuiApp::new("tool-payload-toggle");
    let request_id = id_request("tool-payload-toggle");
    let call_id = ToolCallId::from_stable_parts(&["tool-payload-toggle"]);
    app.reduce(AppEvent::ToolCallStarted {
        request_id,
        call: ToolCall {
            id: call_id,
            name: "read_file".to_owned(),
            arguments: json!({"path": "SECRET_ARGUMENT_MARKER"}),
        },
    });
    app.reduce(AppEvent::ToolCallCompleted {
        request_id,
        output: ToolOutput {
            call_id,
            result: json!({"content": "SECRET_OUTPUT_MARKER"}),
            is_error: false,
        },
    });
    let _ = app.handle_key(key(KeyCode::Esc));

    let collapsed = rendered_text(&mut app, 100, 32);
    assert!(collapsed.contains("[t] show arguments and output"));
    assert!(!collapsed.contains("SECRET_ARGUMENT_MARKER"));
    assert!(!collapsed.contains("SECRET_OUTPUT_MARKER"));

    let _ = app.handle_key(key(KeyCode::Char('t')));
    let expanded = rendered_text(&mut app, 100, 32);
    assert!(expanded.contains("SECRET_ARGUMENT_MARKER"));
    assert!(expanded.contains("SECRET_OUTPUT_MARKER"));
}

#[test]
fn eighty_by_twenty_four_uses_tabs_and_switches_single_panel() {
    let mut app = TuiApp::new("narrow-render");
    assert_eq!(app.layout_mode(80, 24), LayoutMode::Tabbed);

    let repository = rendered_text(&mut app, 80, 24);
    assert!(repository.contains("Repository / Code Map"));
    assert!(repository.contains("No runtime progress yet"));
    assert!(repository.contains("T0 I0 O0 C0 | Cost:-"));

    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('2')));
    let conversation = rendered_text(&mut app, 80, 24);
    assert_eq!(app.focused_panel(), Panel::Conversation);
    assert!(conversation.contains("Ask how the codebase works"));
}

#[test]
fn compact_and_minimum_sizes_render_without_panicking() {
    for (width, height) in [(1, 1), (10, 3), (23, 7), (24, 8), (40, 10)] {
        let mut app = TuiApp::new(format!("size-{width}-{height}"));
        let text = rendered_text(&mut app, width, height);
        assert!(!text.is_empty());
    }
}

#[test]
fn minimum_standard_layout_shows_local_errors_and_x_opens_details_during_input() {
    let mut app = TuiApp::new("minimum-error-status");
    assert!(app.submit_repository().is_none());
    assert!(!app.error_details_visible());

    let text = rendered_text(&mut app, 24, 8);
    assert!(text.contains("ERROR"));
    let _ = app.handle_key(key(KeyCode::Char('x')));
    assert!(app.error_details_visible());
}

#[test]
fn long_wrapped_conversation_scrolls_to_its_last_line_and_back_home() {
    const WORD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const WORD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const WORD_C: &str = "cccccccccccccccccccccccccccccccc";

    let mut app = TuiApp::new("wrapped-conversation-scroll");
    let evidence = sample_evidence("wrapped-conversation", "src/wrapped.rs", 1);
    let mut answer = sample_answer(&evidence);
    let mut answer_lines = vec!["CONVERSATION_TOP_MARKER".to_owned()];
    answer_lines.extend((0..60).map(|line| format!("row {line:02} {WORD_A} {WORD_B} {WORD_C}")));
    answer.text = answer_lines.join("\n");
    answer.claims.clear();
    answer.evidence.clear();
    answer.call_paths.clear();
    answer.diagram = DiagramDecision::NotNeeded {
        reason: "CONVERSATION_FINAL_MARKER".to_owned(),
    };
    app.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("wrapped-conversation-scroll"),
        answer,
    });
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('2')));

    let _ = app.handle_key(key(KeyCode::Home));
    let first = rendered_text(&mut app, 64, 18);
    assert!(first.contains("CONVERSATION_TOP_MARKER"));
    assert!(!first.contains("CONVERSATION_FINAL_MARKER"));

    let _ = app.handle_key(key(KeyCode::Down));
    let _ = rendered_text(&mut app, 64, 18);
    assert_eq!(app.conversation_scroll(), 1);
    let _ = app.handle_key(key(KeyCode::Up));
    let _ = rendered_text(&mut app, 64, 18);
    assert_eq!(app.conversation_scroll(), 0);
    let _ = app.handle_key(key(KeyCode::Char('j')));
    let _ = rendered_text(&mut app, 64, 18);
    assert_eq!(app.conversation_scroll(), 1);
    let _ = app.handle_key(key(KeyCode::Char('k')));
    let _ = rendered_text(&mut app, 64, 18);
    assert_eq!(app.conversation_scroll(), 0);

    for _ in 0..100 {
        let _ = app.handle_key(key(KeyCode::PageDown));
    }
    let last = rendered_text(&mut app, 64, 18);
    assert!(last.contains("CONVERSATION_FINAL_MARKER"));
    let end_scroll = app.conversation_scroll();
    assert!(end_scroll > 0);

    let _ = app.handle_key(key(KeyCode::PageUp));
    let _ = rendered_text(&mut app, 64, 18);
    assert!(app.conversation_scroll() < end_scroll);
    let _ = app.handle_key(key(KeyCode::End));
    let end = rendered_text(&mut app, 64, 18);
    assert!(end.contains("CONVERSATION_FINAL_MARKER"));

    let _ = app.handle_key(key(KeyCode::Home));
    let home = rendered_text(&mut app, 64, 18);
    assert!(home.contains("CONVERSATION_TOP_MARKER"));
    assert_eq!(app.conversation_scroll(), 0);
}

#[test]
fn answer_completion_preserves_manual_scroll_and_tail_following_can_resume() {
    const WORD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const WORD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let mut app = TuiApp::new("manual-conversation-scroll");
    let request_id = id_request("manual-conversation-scroll");
    let mut answer_lines = vec!["MANUAL_SCROLL_TOP_MARKER".to_owned()];
    answer_lines.extend((0..60).map(|line| format!("row {line:02} {WORD_A} {WORD_B}")));
    let answer_text = answer_lines.join("\n");
    app.reduce(AppEvent::AnswerDelta {
        request_id,
        delta: answer_text.clone(),
    });
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('2')));
    let _ = app.handle_key(key(KeyCode::Home));

    let top = rendered_text(&mut app, 64, 18);
    assert!(top.contains("MANUAL_SCROLL_TOP_MARKER"));
    assert!(!app.conversation_follows_tail());

    app.reduce(AppEvent::AnswerCompleted {
        request_id,
        answer: AgentAnswer {
            id: AnswerId::from_stable_parts(&["manual-conversation-scroll"]),
            text: answer_text,
            claims: Vec::new(),
            evidence: Vec::new(),
            call_paths: Vec::new(),
            diagram: DiagramDecision::NotNeeded {
                reason: "COMPLETED_TAIL_MARKER".to_owned(),
            },
            usage: None,
        },
    });

    assert!(!app.conversation_follows_tail());
    let held = rendered_text(&mut app, 64, 18);
    assert!(held.contains("MANUAL_SCROLL_TOP_MARKER"));
    assert!(!held.contains("COMPLETED_TAIL_MARKER"));
    assert_eq!(app.conversation_scroll(), 0);

    let _ = app.handle_key(key(KeyCode::End));
    let end = rendered_text(&mut app, 64, 18);
    assert!(end.contains("COMPLETED_TAIL_MARKER"));
    assert!(app.conversation_follows_tail());

    let _ = app.handle_key(key(KeyCode::Down));
    let _ = app.handle_key(key(KeyCode::PageDown));
    assert!(app.conversation_follows_tail());
    app.reduce(AppEvent::AnswerDelta {
        request_id: id_request("resumed-conversation-tail"),
        delta: "NEW_STREAM_TAIL_MARKER".to_owned(),
    });
    let resumed = rendered_text(&mut app, 64, 18);
    assert!(resumed.contains("NEW_STREAM_TAIL_MARKER"));
}

#[test]
fn repository_scroll_reaches_call_paths_with_tools_present() {
    const WORD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const WORD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const WORD_C: &str = "cccccccccccccccccccccccccccccccc";

    let mut app = TuiApp::new("repository-pane-scroll");
    let answer_request = id_request("repository-call-path-answer");
    for index in 0..36 {
        let sequence = index.to_string();
        let call_id = ToolCallId::from_stable_parts(&["repository-pane-tool", &sequence]);
        app.reduce(AppEvent::ToolCallStarted {
            request_id: answer_request,
            call: ToolCall {
                id: call_id,
                name: format!("tool-{index:02} {WORD_A} {WORD_B} {WORD_C}"),
                arguments: json!({"index": index}),
            },
        });
        app.reduce(AppEvent::ToolCallCompleted {
            request_id: answer_request,
            output: ToolOutput {
                call_id,
                result: json!({"status": "complete"}),
                is_error: false,
            },
        });
    }

    let evidence = sample_evidence("repository-call-path", "src/call_path.rs", 20);
    let mut answer = sample_answer(&evidence);
    answer.claims.clear();
    answer.call_paths = (0..5)
        .map(|index| {
            let sequence = index.to_string();
            CallPath {
                id: CallPathId::from_stable_parts(&["repository-call-path", &sequence]),
                label: Some(format!("CALL_PATH_MARKER_{index}")),
                steps: (0..3)
                    .map(|_| CallPathStep {
                        target: TargetResolution::Resolved(
                            evidence.symbol_id.expect("sample evidence has a symbol"),
                        ),
                        call_edge_id: None,
                        evidence_ids: vec![evidence.id],
                    })
                    .collect(),
                complete: true,
            }
        })
        .collect();
    app.reduce(AppEvent::AnswerCompleted {
        request_id: answer_request,
        answer,
    });
    let _ = app.handle_key(key(KeyCode::Esc));

    assert_eq!(app.selected_tool_index(), Some(0));
    let _ = app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.selected_tool_index(), Some(1));
    let _ = app.handle_key(key(KeyCode::Char('p')));
    assert_eq!(app.selected_tool_index(), Some(0));

    let _ = app.handle_key(key(KeyCode::Home));
    let first = rendered_text(&mut app, 70, 18);
    assert!(first.contains("ROOT"));
    assert!(!first.contains("CALL_PATH_MARKER_4"));

    for _ in 0..500 {
        let _ = app.handle_key(key(KeyCode::Char('j')));
    }
    let last = rendered_text(&mut app, 70, 18);
    assert!(last.contains("CALL_PATH_MARKER_4"));
    assert!(app.repository_scroll() > 0);

    let mut visited = last;
    loop {
        let previous_scroll = app.repository_scroll();
        let _ = app.handle_key(key(KeyCode::PageUp));
        visited.push_str(&rendered_text(&mut app, 70, 18));
        if app.repository_scroll() == previous_scroll {
            break;
        }
    }
    assert!(visited.contains("CALL PATHS"));
    assert!(visited.contains("src/call_path.rs:20"));
    for index in 0..5 {
        assert!(
            visited.contains(&format!("CALL_PATH_MARKER_{index}")),
            "call path {index} should be reachable"
        );
    }
    let home = rendered_text(&mut app, 70, 18);
    assert!(home.contains("ROOT"));
    assert_eq!(app.repository_scroll(), 0);
}

#[test]
fn evidence_focus_enter_and_v_open_viewer_while_ask_keys_still_ask() {
    let mut app = TuiApp::new("evidence-open-keys");
    let evidence = sample_evidence("open", "src/open.rs", 10);
    add_evidence(&mut app, evidence.clone());
    let _ = app.handle_key(key(KeyCode::Esc));

    let _ = app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.input_mode(), InputMode::Question);
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));
    assert_eq!(app.focused_panel(), Panel::Evidence);

    assert!(app.handle_key(key(KeyCode::Enter)).is_none());
    assert_eq!(app.input_mode(), InputMode::Navigation);
    let viewer = app
        .evidence_viewer()
        .expect("Enter opens selected evidence");
    assert_eq!(viewer.evidence_id(), evidence.id);
    assert_eq!(
        viewer.content(),
        evidence.excerpt.as_deref().unwrap_or_default()
    );
    assert!(!viewer.loading());

    assert!(app.handle_key(key(KeyCode::Char('v'))).is_none());
    assert!(app.evidence_viewer().is_none());
    let split = rendered_text(&mut app, 80, 24);
    assert!(split.contains("Source Evidence | 1 cited"));
    assert!(split.contains("selected excerpt - Enter/v view"));
    assert!(split.contains("Enter/v view | o/y diagram | a/? ask"));
    assert!(app.handle_key(key(KeyCode::Char('v'))).is_none());
    assert!(app.evidence_viewer().is_some());
    let _ = app.handle_key(key(KeyCode::Esc));
    assert!(app.evidence_viewer().is_none());

    let _ = app.handle_key(key(KeyCode::Char('?')));
    assert_eq!(app.input_mode(), InputMode::Question);
}

#[test]
fn opening_evidence_loads_its_inclusive_line_range_without_becoming_active() {
    let mut app = TuiApp::new("source-command");
    let repository_id = complete_index(&mut app, "/repo");
    let mut evidence = sample_evidence("exclusive-end", "src/range.rs", 7);
    evidence.span = SourceSpan::new(7, 4, 12, 0).expect("valid half-open span");
    evidence.excerpt = Some("line 7\nline 8\nline 9\nline 10\nline 11".to_owned());
    add_evidence(&mut app, evidence.clone());
    let activity = app.activity();
    let _ = app.handle_key(key(KeyCode::Char('e')));

    let command = app
        .handle_key(key(KeyCode::Enter))
        .expect("indexed evidence requests its source");
    match command {
        AppCommand::LoadSource {
            repository_id: command_repository,
            path,
            start_line,
            end_line,
            ..
        } => {
            assert_eq!(command_repository, repository_id);
            assert_eq!(path, evidence.path);
            assert_eq!(start_line, 7);
            assert_eq!(end_line, Some(11));
        }
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    }
    let viewer = app.evidence_viewer().expect("viewer remains open");
    assert_eq!(viewer.start_line(), 7);
    assert_eq!(viewer.end_line(), 11);
    assert_eq!(
        viewer.content(),
        evidence.excerpt.as_deref().unwrap_or_default()
    );
    assert!(viewer.loading());
    assert!(!viewer.source_loaded());
    assert_eq!(app.activity(), activity);
    assert_eq!(app.active_request_id(), None);

    let text = rendered_text(&mut app, 100, 24);
    assert!(text.contains("Source Evidence E1/1"));
    assert!(text.contains("src/range.rs:7-11"));
    assert!(text.contains("loading requested 7-11"));
}

#[test]
fn source_reducer_ignores_stale_results_and_replaces_excerpt_with_matching_load() {
    let mut app = TuiApp::new("source-reducer");
    let repository_id = complete_index(&mut app, "/repo");
    let evidence = sample_evidence("source-reducer", "src/source.rs", 100);
    let excerpt = evidence.excerpt.clone().expect("sample excerpt");
    add_evidence(&mut app, evidence.clone());
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let command = app
        .handle_key(key(KeyCode::Char('v')))
        .expect("source command");
    let (request_id, path) = match command {
        AppCommand::LoadSource {
            request_id, path, ..
        } => (request_id, path),
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    };

    app.reduce(AppEvent::SourceLoaded {
        request_id: id_request("stale-source"),
        repository_id,
        path: path.clone(),
        start_line: 100,
        end_line: 102,
        content: "stale request".to_owned(),
    });
    app.reduce(AppEvent::SourceLoaded {
        request_id,
        repository_id,
        path: RepositoryPath::new("src/wrong.rs").expect("valid path"),
        start_line: 100,
        end_line: 102,
        content: "wrong path".to_owned(),
    });
    let waiting = app.evidence_viewer().expect("viewer remains open");
    assert_eq!(waiting.content(), excerpt);
    assert!(waiting.loading());

    let loaded = "loaded 100\nloaded 101\nloaded 102";
    app.reduce(AppEvent::SourceLoaded {
        request_id,
        repository_id,
        path: path.clone(),
        start_line: 100,
        end_line: 102,
        content: loaded.to_owned(),
    });
    let viewer = app
        .evidence_viewer()
        .expect("matching result stays visible");
    assert_eq!(viewer.content(), loaded);
    assert_eq!(viewer.content_start_line(), 100);
    assert_eq!(viewer.content_end_line(), 102);
    assert!(viewer.source_loaded());
    assert!(!viewer.loading());

    app.reduce(AppEvent::SourceLoaded {
        request_id,
        repository_id,
        path,
        start_line: 100,
        end_line: 100,
        content: "late duplicate".to_owned(),
    });
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::content),
        Some(loaded)
    );
    let text = rendered_text(&mut app, 100, 20);
    assert!(text.contains("loaded showing 100-102"));
}

#[test]
fn source_load_errors_remain_inline_and_keep_the_evidence_excerpt() {
    let mut app = TuiApp::new("source-inline-error");
    let _repository_id = complete_index(&mut app, "/repo");
    let evidence = sample_evidence("source-inline-error", "src/large.rs", 1);
    let excerpt = evidence.excerpt.clone().expect("sample excerpt");
    add_evidence(&mut app, evidence);
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let command = app
        .handle_key(key(KeyCode::Enter))
        .expect("source load command");
    let request_id = match command {
        AppCommand::LoadSource { request_id, .. } => request_id,
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    };

    app.reduce(AppEvent::Error {
        request_id: Some(request_id),
        error: AppError {
            code: "source_load_failed".to_owned(),
            message: "source changed while loading".to_owned(),
            retryable: false,
        },
    });

    assert!(app.error().is_none());
    let viewer = app.evidence_viewer().expect("viewer remains available");
    assert_eq!(viewer.content(), excerpt);
    assert_eq!(viewer.source_error(), Some("source changed while loading"));
    let text = rendered_text(&mut app, 100, 20);
    assert!(text.contains("source load failed"));
    assert!(text.contains("excerpt remains"));

    let _ = app.handle_key(key(KeyCode::Esc));
    let stale_command = app
        .handle_key(key(KeyCode::Enter))
        .expect("reopened viewer issues another source load");
    let stale_request_id = match stale_command {
        AppCommand::LoadSource { request_id, .. } => request_id,
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    };
    let _ = app.handle_key(key(KeyCode::Esc));
    app.reduce(AppEvent::Error {
        request_id: Some(stale_request_id),
        error: AppError {
            code: "source_load_failed".to_owned(),
            message: "late source error".to_owned(),
            retryable: false,
        },
    });
    assert!(app.error().is_none());
    assert!(app.evidence_viewer().is_none());
}

#[test]
fn evidence_viewer_scrolls_to_source_ends_and_pages_by_eight() {
    let mut app = TuiApp::new("source-scroll");
    let mut evidence = sample_evidence("scroll", "src/long.rs", 1);
    evidence.span = SourceSpan::new(1, 0, 40, 1).expect("valid source span");
    evidence.excerpt = Some(
        (1..=40)
            .map(|line| format!("source line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    add_evidence(&mut app, evidence);
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let _ = app.handle_key(key(KeyCode::Enter));

    let _ = app.handle_key(key(KeyCode::PageDown));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::vertical_scroll),
        Some(8)
    );
    let _ = app.handle_key(key(KeyCode::Char('j')));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::vertical_scroll),
        Some(9)
    );
    let _ = app.handle_key(key(KeyCode::Home));
    let first = rendered_text(&mut app, 64, 16);
    assert!(first.contains("1 | source line 01"));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::vertical_scroll),
        Some(0)
    );

    let _ = app.handle_key(key(KeyCode::End));
    let last = rendered_text(&mut app, 64, 16);
    assert!(last.contains("40 | source line 40"));
    assert!(
        app.evidence_viewer()
            .is_some_and(|viewer| viewer.vertical_scroll() > 0)
    );
    let _ = app.handle_key(key(KeyCode::PageUp));
    let after_page_up = app
        .evidence_viewer()
        .expect("viewer remains open")
        .vertical_scroll();
    assert!(after_page_up < u16::MAX);
}

#[test]
fn evidence_viewer_shows_numbered_claim_context() {
    let mut app = TuiApp::new("source-claim-context");
    add_evidence(
        &mut app,
        sample_evidence("source-claim-context", "src/context.rs", 30),
    );
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let _ = app.handle_key(key(KeyCode::Enter));

    let text = rendered_text(&mut app, 100, 24);
    assert!(text.contains("Source Evidence E1/1"));
    assert!(text.contains("CLAIMS"));
    assert!(text.contains("[F1] Fixture claim"));
    assert!(text.contains("src/context.rs:30-32"));
}

#[test]
fn evidence_viewer_n_and_p_switch_selection_and_issue_fresh_loads() {
    let mut app = TuiApp::new("source-switch");
    let repository_id = complete_index(&mut app, "/repo");
    let first = sample_evidence("switch-first", "src/first.rs", 10);
    let second = sample_evidence("switch-second", "src/second.rs", 20);
    add_evidence(&mut app, first.clone());
    add_evidence(&mut app, second.clone());
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let first_command = app
        .handle_key(key(KeyCode::Enter))
        .expect("first source load");
    let first_request = match first_command {
        AppCommand::LoadSource {
            request_id, path, ..
        } => {
            assert_eq!(path, first.path);
            request_id
        }
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    };

    let second_command = app
        .handle_key(key(KeyCode::Char('n')))
        .expect("next evidence loads source");
    let second_request = match second_command {
        AppCommand::LoadSource {
            request_id, path, ..
        } => {
            assert_eq!(path, second.path);
            request_id
        }
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    };
    assert_ne!(first_request, second_request);
    assert_eq!(app.selected_evidence().map(|item| item.id), Some(second.id));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::content),
        second.excerpt.as_deref()
    );

    app.reduce(AppEvent::SourceLoaded {
        request_id: first_request,
        repository_id,
        path: first.path.clone(),
        start_line: 10,
        end_line: 12,
        content: "stale first source".to_owned(),
    });
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::content),
        second.excerpt.as_deref()
    );

    let previous = app
        .handle_key(key(KeyCode::Char('p')))
        .expect("previous evidence loads source");
    match previous {
        AppCommand::LoadSource {
            request_id, path, ..
        } => {
            assert_ne!(request_id, first_request);
            assert_ne!(request_id, second_request);
            assert_eq!(path, first.path);
        }
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    }
    assert_eq!(app.selected_evidence().map(|item| item.id), Some(first.id));
    assert!(app.handle_key(key(KeyCode::Char('p'))).is_none());
}

#[test]
fn evidence_viewer_toggles_wrap_and_only_scrolls_horizontally_when_unwrapped() {
    let mut app = TuiApp::new("source-wrap");
    let mut evidence = sample_evidence("wrap", "src/wide.rs", 3);
    evidence.span = SourceSpan::new(3, 0, 3, 80).expect("valid source span");
    evidence.excerpt = Some("abcdefghijklmnopqrstuvwxyz0123456789".repeat(3));
    add_evidence(&mut app, evidence);
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let _ = app.handle_key(key(KeyCode::Enter));
    assert!(app.evidence_viewer().is_some_and(EvidenceViewer::wrap));

    let _ = app.handle_key(key(KeyCode::Char('l')));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::horizontal_scroll),
        Some(0)
    );
    let _ = app.handle_key(key(KeyCode::Char('w')));
    assert!(app.evidence_viewer().is_some_and(|viewer| !viewer.wrap()));
    let _ = app.handle_key(key(KeyCode::Char('l')));
    let _ = app.handle_key(key(KeyCode::Right));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::horizontal_scroll),
        Some(2)
    );
    let _ = app.handle_key(key(KeyCode::Left));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::horizontal_scroll),
        Some(1)
    );
    let text = rendered_text(&mut app, 40, 10);
    assert!(text.contains("wrap:off col:2"));

    let _ = app.handle_key(key(KeyCode::Char('w')));
    assert!(app.evidence_viewer().is_some_and(EvidenceViewer::wrap));
    assert_eq!(
        app.evidence_viewer().map(EvidenceViewer::horizontal_scroll),
        Some(0)
    );
}

#[test]
fn evidence_viewer_copy_requests_exact_source_and_escape_closes_it() {
    let mut app = TuiApp::new("source-copy");
    let repository_id = complete_index(&mut app, "/repo");
    let evidence = sample_evidence("copy", "src/copy.rs", 50);
    add_evidence(&mut app, evidence.clone());
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let command = app
        .handle_key(key(KeyCode::Enter))
        .expect("source load command");
    let request_id = match command {
        AppCommand::LoadSource { request_id, .. } => request_id,
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected source load command")
        }
    };
    let source = "fn complete() {\n    println!(\"all source\");\n}";
    app.reduce(AppEvent::SourceLoaded {
        request_id,
        repository_id,
        path: evidence.path,
        start_line: 50,
        end_line: 52,
        content: source.to_owned(),
    });

    let _ = app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(app.take_clipboard_request().as_deref(), Some(source));
    assert_eq!(
        app.clipboard_status(),
        Some("Copying source to clipboard...")
    );
    let _ = app.handle_key(key(KeyCode::Esc));
    assert!(app.evidence_viewer().is_none());
    assert_eq!(app.input_mode(), InputMode::Navigation);
}

#[test]
fn evidence_viewer_renders_at_tiny_sizes_and_error_details_take_priority() {
    let mut app = TuiApp::new("source-small-error");
    add_evidence(&mut app, sample_evidence("small-error", "src/small.rs", 1));
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let _ = app.handle_key(key(KeyCode::Enter));
    for (width, height) in [(1, 1), (2, 2), (10, 3), (23, 7), (24, 8), (40, 10)] {
        assert!(!rendered_text(&mut app, width, height).is_empty());
    }

    app.reduce(AppEvent::Error {
        request_id: None,
        error: AppError {
            code: "viewer_error".to_owned(),
            message: "Error details must cover evidence".to_owned(),
            retryable: false,
        },
    });
    let error = rendered_text(&mut app, 80, 20);
    assert!(error.contains("Error Details"));
    assert!(!error.contains("Source Evidence E1/1"));
    let _ = app.handle_key(key(KeyCode::Esc));
    let viewer = rendered_text(&mut app, 80, 20);
    assert!(viewer.contains("Source Evidence E1/1"));
}

#[test]
fn keyboard_state_machine_indexes_asks_selects_cancels_and_quits() {
    let mut app = TuiApp::new("keyboard");
    assert_eq!(app.input_mode(), InputMode::RepositoryPath);
    type_text(&mut app, "/workspace/project");
    let index = app
        .handle_key(key(KeyCode::Enter))
        .expect("enter submits index");
    let index_request = match index {
        AppCommand::Index {
            request_id,
            repository_root,
        } => {
            assert_eq!(repository_root, "/workspace/project");
            request_id
        }
        AppCommand::Ask { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected index command")
        }
    };
    assert_eq!(app.activity(), Activity::IndexQueued);

    let repository_id = RepositoryId::from_stable_parts(&["keyboard-repository"]);
    app.reduce(AppEvent::IndexCompleted {
        request_id: index_request,
        repository_id,
        file_count: 10,
        symbol_count: 42,
        repository_map: RepositoryMap::default(),
    });
    assert_eq!(app.repository().repository_id, Some(repository_id));

    let _ = app.handle_key(key(KeyCode::Char('a')));
    assert_eq!(app.input_mode(), InputMode::Question);
    type_text(&mut app, "How does routing work?");
    let ask = app
        .handle_key(key(KeyCode::Enter))
        .expect("enter submits question");
    let ask_request = match ask {
        AppCommand::Ask {
            request_id,
            session_id,
            repository_id: command_repository,
            question,
        } => {
            assert_eq!(session_id, app.session_id());
            assert_eq!(command_repository, repository_id);
            assert_eq!(question, "How does routing work?");
            request_id
        }
        AppCommand::Index { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected ask command")
        }
    };

    let first = sample_evidence("first", "src/router.rs", 10);
    let second = sample_evidence("second", "src/service.rs", 20);
    app.reduce(AppEvent::EvidenceAdded {
        request_id: ask_request,
        evidence: first,
    });
    app.reduce(AppEvent::EvidenceAdded {
        request_id: ask_request,
        evidence: second,
    });
    assert!(app.evidence().is_empty());

    let cancel = app
        .handle_key(key(KeyCode::Char('c')))
        .expect("cancel emits a command");
    let cancel_request = match cancel {
        AppCommand::Cancel {
            request_id,
            target_request_id,
        } => {
            assert_eq!(target_request_id, ask_request);
            request_id
        }
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected cancel command")
        }
    };
    app.reduce(AppEvent::Cancelled {
        request_id: cancel_request,
    });
    assert_eq!(app.activity(), Activity::Cancelled);
    assert_eq!(app.active_request_id(), None);

    let _ = app.handle_key(key(KeyCode::Char('q')));
    assert!(app.is_quit_requested());
}

#[test]
fn generated_request_and_repository_session_ids_are_fresh() {
    let mut first = TuiApp::new("stable-session");
    let mut restarted = TuiApp::new("stable-session");
    first.set_repository_path("/repo");
    restarted.set_repository_path("/repo");

    let first_command = first.submit_repository().expect("index command");
    let restarted_command = restarted.submit_repository().expect("index command");
    assert_ne!(first_command, restarted_command);
    assert_ne!(first.session_id(), restarted.session_id());

    let first_request = match first_command {
        AppCommand::Index { request_id, .. } => request_id,
        AppCommand::Ask { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected index command")
        }
    };
    let AppCommand::Index {
        request_id: restarted_request,
        ..
    } = restarted_command
    else {
        panic!("expected index command");
    };
    first.reduce(AppEvent::IndexCompleted {
        request_id: first_request,
        repository_id: RepositoryId::from_stable_parts(&["stable-repo"]),
        file_count: 1,
        symbol_count: 2,
        repository_map: RepositoryMap::default(),
    });
    let first_index_session = first.session_id();
    restarted.reduce(AppEvent::IndexCompleted {
        request_id: restarted_request,
        repository_id: RepositoryId::from_stable_parts(&["stable-repo"]),
        file_count: 1,
        symbol_count: 2,
        repository_map: RepositoryMap::default(),
    });
    assert_ne!(first_index_session, restarted.session_id());

    first.set_repository_path("/repo");
    let AppCommand::Index {
        request_id: repeated_request,
        ..
    } = first.submit_repository().expect("repeated index command")
    else {
        panic!("expected index command");
    };
    first.reduce(AppEvent::IndexCompleted {
        request_id: repeated_request,
        repository_id: RepositoryId::from_stable_parts(&["stable-repo"]),
        file_count: 1,
        symbol_count: 2,
        repository_map: RepositoryMap::default(),
    });
    assert_ne!(first.session_id(), first_index_session);
    assert!(first.conversation().is_empty());

    first.begin_question_input();
    first.set_question("question");
    let ask = first.submit_question().expect("ask command");
    let ask_request = match ask {
        AppCommand::Ask { request_id, .. } => request_id,
        AppCommand::Index { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected ask command")
        }
    };
    assert_ne!(first_request, ask_request);

    let different = TuiApp::new("different-session");
    assert_ne!(first.session_id(), different.session_id());
}

#[test]
#[allow(clippy::too_many_lines)]
fn history_load_restores_complete_context_and_scopes_workflow_by_task() {
    let mut app = TuiApp::new("history-restore");
    let repository_id = complete_index(&mut app, "/repo");
    let list_command = app
        .handle_key(key(KeyCode::Char('h')))
        .expect("history requests saved sessions");
    let list_request = match list_command {
        AppCommand::ListSessions {
            request_id,
            repository_id: listed_repository_id,
        } => {
            assert_eq!(listed_repository_id, Some(repository_id));
            request_id
        }
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => panic!("expected session list command"),
    };

    let session_id = SessionId::from_stable_parts(&["history-restore-session"]);
    let first_request = id_request("history-first-task");
    let second_request = id_request("history-second-task");
    let json_path = "/data/sessions/history-restore.json";
    app.reduce(AppEvent::SessionsListed {
        request_id: list_request,
        sessions: vec![SessionSummary {
            session_id,
            repository_id,
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_123_000,
            tasks: vec![
                SessionTaskSummary {
                    request_id: first_request,
                    question: "FIRST_HISTORY_QUESTION".to_owned(),
                },
                SessionTaskSummary {
                    request_id: second_request,
                    question: "SECOND_HISTORY_QUESTION".to_owned(),
                },
            ],
            json_path: json_path.to_owned(),
        }],
    });
    let history = rendered_text(&mut app, 100, 24);
    assert!(history.contains("Session History"));
    assert!(history.contains("FIRST_HISTORY_QUESTION"));
    assert!(!history.contains("SECOND_HISTORY_QUESTION"));
    assert!(history.contains("2 tasks"));
    assert!(history.contains("updated unix:1700000123s"));
    assert!(history.contains(json_path));

    let load_command = app
        .handle_key(key(KeyCode::Enter))
        .expect("selected history task loads its session");
    let load_request = match load_command {
        AppCommand::LoadSession {
            request_id,
            session_id: selected_session,
        } => {
            assert_eq!(selected_session, session_id);
            request_id
        }
        AppCommand::Index { .. }
        | AppCommand::Ask { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::OpenDiagram { .. } => panic!("expected session load command"),
    };

    let first_evidence = sample_evidence("history-first", "src/first_history.rs", 10);
    let second_evidence = sample_evidence("history-second", "src/second_history.rs", 20);
    let mut first_answer = sample_answer(&first_evidence);
    first_answer.text = "FIRST_HISTORY_ANSWER".to_owned();
    let mut second_answer = sample_answer(&second_evidence);
    second_answer.text = "SECOND_HISTORY_ANSWER".to_owned();
    let first_tool = ToolCallId::from_stable_parts(&["history-first-tool"]);
    let second_tool = ToolCallId::from_stable_parts(&["history-second-tool"]);
    let workflow = |phase, message: &str, call_id, name: &str| {
        vec![
            WorkflowEvent::Progress(Progress {
                phase,
                message: message.to_owned(),
                completed: None,
                total: None,
            }),
            WorkflowEvent::ToolCallStarted(ToolCall {
                id: call_id,
                name: name.to_owned(),
                arguments: json!({"query": name}),
            }),
            WorkflowEvent::ToolCallCompleted {
                call_id,
                output: format!("{name}_OUTPUT"),
                is_error: false,
            },
        ]
    };
    app.reduce(AppEvent::SessionLoaded {
        request_id: load_request,
        session: SessionContext {
            schema_version: 1,
            session_id,
            repository_id,
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_123_000,
            tasks: vec![
                SessionTask {
                    request_id: first_request,
                    question: "FIRST_HISTORY_QUESTION".to_owned(),
                    answer: first_answer,
                    workflow: workflow(
                        ProgressPhase::Searching,
                        "FIRST_HISTORY_PROGRESS",
                        first_tool,
                        "FIRST_HISTORY_TOOL",
                    ),
                },
                SessionTask {
                    request_id: second_request,
                    question: "SECOND_HISTORY_QUESTION".to_owned(),
                    answer: second_answer,
                    workflow: workflow(
                        ProgressPhase::Tracing,
                        "SECOND_HISTORY_PROGRESS",
                        second_tool,
                        "SECOND_HISTORY_TOOL",
                    ),
                },
            ],
            usage: ModelUsage {
                tokens: TokenUsage {
                    input_tokens: 1_600,
                    output_tokens: 400,
                    cached_input_tokens: 200,
                    total_tokens: 2_000,
                },
                cost: Some(Cost {
                    currency: "USD".to_owned(),
                    amount: 0.025,
                    estimated: true,
                }),
            },
            json_path: json_path.to_owned(),
        },
    });

    assert!(app.history_view().is_none());
    assert_eq!(app.session_id(), session_id);
    assert_eq!(app.session_repository_id(), Some(repository_id));
    assert_eq!(app.session_json_path(), Some(json_path));
    assert_eq!(app.selected_task(), Some(second_request));
    assert_eq!(app.conversation().len(), 4);
    assert_eq!(app.evidence().len(), 2);
    assert_eq!(app.tools().len(), 2);
    assert_eq!(app.progress_trace().len(), 2);
    assert_eq!(app.total_token_usage().total_tokens, 2_000);

    let second_task = rendered_text(&mut app, 80, 30);
    assert!(second_task.contains("SECOND_HISTORY_PROGRESS"));
    assert!(second_task.contains("SECOND_HISTORY_TOOL"));
    assert!(!second_task.contains("FIRST_HISTORY_TOOL"));

    let _ = app.handle_key(key(KeyCode::Char('[')));
    let first_task = rendered_text(&mut app, 80, 30);
    assert_eq!(app.selected_task(), Some(first_request));
    assert!(first_task.contains("FIRST_HISTORY_PROGRESS"));
    assert!(first_task.contains("FIRST_HISTORY_TOOL"));
    assert!(!first_task.contains("SECOND_HISTORY_TOOL"));

    app.begin_question_input();
    app.set_question("Continue from the restored context");
    let follow_up = app
        .submit_question()
        .expect("restored session can continue");
    match follow_up {
        AppCommand::Ask {
            request_id,
            session_id: command_session,
            repository_id: command_repository,
            ..
        } => {
            assert_eq!(command_session, session_id);
            assert_eq!(command_repository, repository_id);
            assert_ne!(request_id, first_request);
            assert_ne!(request_id, second_request);
        }
        AppCommand::Index { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => panic!("expected ask command"),
    }
}

#[test]
fn failed_history_load_closes_the_modal_and_ignores_its_late_response() {
    let mut app = TuiApp::new("stale-history-load");
    let repository_id = complete_index(&mut app, "/repo");
    let original_session = app.session_id();
    let AppCommand::ListSessions {
        request_id: list_request,
        ..
    } = app
        .handle_key(key(KeyCode::Char('h')))
        .expect("history list command")
    else {
        panic!("expected session list command");
    };
    let historical_session = SessionId::from_stable_parts(&["late-history-session"]);
    let historical_task = id_request("late-history-task");
    app.reduce(AppEvent::SessionsListed {
        request_id: list_request,
        sessions: vec![SessionSummary {
            session_id: historical_session,
            repository_id,
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_001_000,
            tasks: vec![SessionTaskSummary {
                request_id: historical_task,
                question: "Late history task".to_owned(),
            }],
            json_path: "/data/sessions/late.json".to_owned(),
        }],
    });
    let AppCommand::LoadSession {
        request_id: load_request,
        ..
    } = app
        .handle_key(key(KeyCode::Enter))
        .expect("history load command")
    else {
        panic!("expected session load command");
    };

    app.reduce(AppEvent::Error {
        request_id: Some(load_request),
        error: AppError {
            code: "session_not_found".to_owned(),
            message: "the selected session was removed".to_owned(),
            retryable: false,
        },
    });
    assert!(app.history_view().is_none());
    assert!(app.error_details_visible());
    let _ = app.handle_key(key(KeyCode::Esc));

    let evidence = sample_evidence("late-history", "src/late.rs", 1);
    app.reduce(AppEvent::SessionLoaded {
        request_id: load_request,
        session: SessionContext {
            schema_version: 2,
            session_id: historical_session,
            repository_id,
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_001_000,
            tasks: vec![SessionTask {
                request_id: historical_task,
                question: "Late history task".to_owned(),
                answer: sample_answer(&evidence),
                workflow: Vec::new(),
            }],
            usage: ModelUsage {
                tokens: TokenUsage::default(),
                cost: None,
            },
            json_path: "/data/sessions/late.json".to_owned(),
        },
    });
    assert_eq!(app.session_id(), original_session);
    assert!(app.conversation().is_empty());
}

#[test]
fn index_completion_ignores_unsolicited_history_restoration_and_allows_ask() {
    let mut app = TuiApp::new("ambiguous-history");
    app.set_repository_path("/repo");
    let AppCommand::Index {
        request_id: index_request,
        ..
    } = app.submit_repository().expect("index command")
    else {
        panic!("expected index command");
    };
    let repository_id = RepositoryId::from_stable_parts(&["ambiguous-history-repository"]);
    app.reduce(AppEvent::IndexCompleted {
        request_id: index_request,
        repository_id,
        file_count: 1,
        symbol_count: 2,
        repository_map: RepositoryMap::default(),
    });
    let summary = |label: &str| SessionSummary {
        session_id: SessionId::from_stable_parts(&["ambiguous-history", label]),
        repository_id,
        created_at_unix_ms: 1_700_000_000_000,
        updated_at_unix_ms: 1_700_000_001_000,
        tasks: vec![SessionTaskSummary {
            request_id: RequestId::from_stable_parts(&["ambiguous-history-task", label]),
            question: format!("History task {label}"),
        }],
        json_path: format!("/data/sessions/{label}.json"),
    };
    app.reduce(AppEvent::SessionsListed {
        request_id: index_request,
        sessions: vec![summary("first"), summary("second")],
    });

    let fresh_session = app.session_id();
    app.reduce(AppEvent::SessionLoaded {
        request_id: index_request,
        session: SessionContext {
            schema_version: 2,
            session_id: SessionId::from_stable_parts(&["unsolicited-session"]),
            repository_id,
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_001_000,
            tasks: Vec::new(),
            usage: ModelUsage {
                tokens: TokenUsage::default(),
                cost: None,
            },
            json_path: "/data/sessions/unsolicited.json".to_owned(),
        },
    });

    assert!(app.history_view().is_none());
    assert_eq!(app.session_id(), fresh_session);
    app.begin_question_input();
    app.set_question("Start in the fresh session");
    let AppCommand::Ask { session_id, .. } = app.submit_question().expect("ask is allowed") else {
        panic!("expected ask command");
    };
    assert_eq!(session_id, fresh_session);
}

#[test]
fn uppercase_n_starts_an_empty_session_while_lowercase_n_navigates_tools() {
    let mut app = TuiApp::new("new-session-key");
    let repository_id = complete_index(&mut app, "/repo");
    let old_session = app.session_id();
    let request_id = id_request("new-session-context");
    let evidence = sample_evidence("new-session", "src/new_session.rs", 1);
    app.reduce(AppEvent::AnswerCompleted {
        request_id,
        answer: sample_answer(&evidence),
    });
    for label in ["first", "second"] {
        app.reduce(AppEvent::ToolCallStarted {
            request_id,
            call: ToolCall {
                id: ToolCallId::from_stable_parts(&["new-session-tool", label]),
                name: label.to_owned(),
                arguments: json!({}),
            },
        });
    }

    assert_eq!(app.selected_tool_index(), Some(0));
    assert!(app.handle_key(key(KeyCode::Char('n'))).is_none());
    assert_eq!(app.selected_tool_index(), Some(1));
    assert_eq!(app.session_id(), old_session);

    assert!(app.handle_key(key(KeyCode::Char('N'))).is_none());
    let navigation_session = app.session_id();
    assert_ne!(navigation_session, old_session);
    assert_eq!(app.session_repository_id(), Some(repository_id));
    assert!(app.conversation().is_empty());
    assert!(app.evidence().is_empty());
    assert!(app.tools().is_empty());
    assert_eq!(app.session_json_path(), None);

    assert!(matches!(
        app.handle_key(key(KeyCode::Char('h'))),
        Some(AppCommand::ListSessions { .. })
    ));
    assert!(app.handle_key(key(KeyCode::Char('N'))).is_none());
    let new_session = app.session_id();
    assert_ne!(new_session, navigation_session);
    assert!(app.history_view().is_none());

    app.begin_question_input();
    app.set_question("Ask in the new session");
    let AppCommand::Ask { session_id, .. } = app.submit_question().expect("new session can ask")
    else {
        panic!("expected ask command");
    };
    assert_eq!(session_id, new_session);

    assert!(app.handle_key(key(KeyCode::Char('N'))).is_none());
    assert_eq!(app.session_id(), new_session);
    assert_eq!(app.error().map(|error| error.code.as_str()), Some("busy"));
}

#[test]
fn new_session_key_requires_an_indexed_repository() {
    let mut app = TuiApp::new("new-session-before-index");
    let _ = app.handle_key(key(KeyCode::Esc));

    assert!(app.handle_key(key(KeyCode::Char('N'))).is_none());
    assert_eq!(
        app.error().map(|error| error.code.as_str()),
        Some("repository_not_indexed")
    );
}

#[test]
fn loaded_session_for_another_repository_remains_read_only() {
    let mut app = TuiApp::new("history-repository-safety");
    let indexed_repository = complete_index(&mut app, "/repo");
    let other_repository = RepositoryId::from_stable_parts(&["other-repository"]);
    let other_session = SessionId::from_stable_parts(&["other-session"]);
    let AppCommand::ListSessions {
        request_id: list_request,
        repository_id,
    } = app
        .handle_key(key(KeyCode::Char('h')))
        .expect("history list command")
    else {
        panic!("expected list sessions command");
    };
    assert_eq!(repository_id, Some(indexed_repository));
    app.reduce(AppEvent::SessionsListed {
        request_id: list_request,
        sessions: vec![SessionSummary {
            session_id: other_session,
            repository_id: other_repository,
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_001_000,
            tasks: Vec::new(),
            json_path: "/data/sessions/other.json".to_owned(),
        }],
    });
    let AppCommand::LoadSession {
        request_id: load_request,
        ..
    } = app
        .handle_key(key(KeyCode::Enter))
        .expect("explicit load command")
    else {
        panic!("expected load session command");
    };
    app.reduce(AppEvent::SessionLoaded {
        request_id: load_request,
        session: SessionContext {
            schema_version: 2,
            session_id: other_session,
            repository_id: other_repository,
            created_at_unix_ms: 1_700_000_000_000,
            updated_at_unix_ms: 1_700_000_001_000,
            tasks: Vec::new(),
            usage: ModelUsage {
                tokens: TokenUsage::default(),
                cost: None,
            },
            json_path: "/data/sessions/other.json".to_owned(),
        },
    });

    app.begin_question_input();
    app.set_question("This must not run against the wrong repository");
    assert!(app.submit_question().is_none());
    assert_eq!(
        app.error().map(|error| error.code.as_str()),
        Some("session_repository_mismatch")
    );
}

#[test]
fn cancellation_event_may_identify_the_target_request() {
    let mut app = TuiApp::new("target-cancellation");
    complete_index(&mut app, "/repo");
    app.begin_question_input();
    app.set_question("trace this request");
    let ask_request = match app.submit_question().expect("ask command") {
        AppCommand::Ask { request_id, .. } => request_id,
        AppCommand::Index { .. }
        | AppCommand::LoadSource { .. }
        | AppCommand::Cancel { .. }
        | AppCommand::ListSessions { .. }
        | AppCommand::LoadSession { .. }
        | AppCommand::OpenDiagram { .. } => {
            panic!("expected ask command")
        }
    };
    assert!(app.cancel_active().is_some());

    app.reduce(AppEvent::Cancelled {
        request_id: ask_request,
    });

    assert_eq!(app.active_request_id(), None);
    assert_eq!(app.activity(), Activity::Cancelled);
}

#[test]
fn reducer_handles_duplicate_progress_and_out_of_order_tools() {
    let mut app = TuiApp::new("progress-tool-reducer");
    let progress_request = id_request("progress");
    let progress = Progress {
        phase: ProgressPhase::Tracing,
        message: "Following route into service".to_owned(),
        completed: Some(2),
        total: Some(5),
    };
    app.reduce(AppEvent::Progress {
        request_id: progress_request,
        progress: progress.clone(),
    });
    app.reduce(AppEvent::Progress {
        request_id: progress_request,
        progress,
    });
    assert_eq!(app.progress_trace().len(), 1);
    assert_eq!(app.activity(), Activity::Running(ProgressPhase::Tracing));

    let tool_id = ToolCallId::from_stable_parts(&["out-of-order-tool"]);
    app.reduce(AppEvent::ToolCallCompleted {
        request_id: progress_request,
        output: ToolOutput {
            call_id: tool_id,
            result: json!({"matches": 3}),
            is_error: false,
        },
    });
    app.reduce(AppEvent::ToolCallStarted {
        request_id: progress_request,
        call: ToolCall {
            id: tool_id,
            name: "find_symbol".to_owned(),
            arguments: json!({"name": "route"}),
        },
    });
    app.reduce(AppEvent::ToolCallCompleted {
        request_id: progress_request,
        output: ToolOutput {
            call_id: tool_id,
            result: json!({"matches": 3}),
            is_error: false,
        },
    });
    assert_eq!(app.tools().len(), 1);
    assert_eq!(app.tools()[0].name.as_deref(), Some("find_symbol"));
    assert_eq!(app.tools()[0].status, ToolTraceStatus::Completed);
}

#[test]
fn reducer_handles_evidence_streaming_answers_and_usage() {
    let mut app = TuiApp::new("answer-reducer");
    let answer_request = id_request("answer");
    let evidence = sample_evidence("answer-evidence", "src/router.rs", 10);
    app.reduce(AppEvent::EvidenceAdded {
        request_id: answer_request,
        evidence: evidence.clone(),
    });
    app.reduce(AppEvent::EvidenceAdded {
        request_id: answer_request,
        evidence: evidence.clone(),
    });
    assert!(app.evidence().is_empty());

    app.reduce(AppEvent::AnswerDelta {
        request_id: answer_request,
        delta: "partial ".to_owned(),
    });
    app.reduce(AppEvent::AnswerDelta {
        request_id: answer_request,
        delta: "partial ".to_owned(),
    });
    let streaming = app.conversation().iter().find_map(|entry| match entry {
        ConversationEntry::Answer(answer) => Some(answer),
        ConversationEntry::Question { .. } => None,
    });
    assert_eq!(streaming.and_then(|answer| answer.diagram.as_ref()), None);
    let answer = sample_answer(&evidence);
    let expected_diagram = answer.diagram.clone();
    app.reduce(AppEvent::AnswerCompleted {
        request_id: answer_request,
        answer: answer.clone(),
    });
    app.reduce(AppEvent::AnswerCompleted {
        request_id: answer_request,
        answer,
    });
    app.reduce(AppEvent::AnswerDelta {
        request_id: answer_request,
        delta: "stale".to_owned(),
    });
    let completed = app.conversation().iter().find_map(|entry| match entry {
        ConversationEntry::Answer(answer) => Some(answer),
        ConversationEntry::Question { .. } => None,
    });
    assert_eq!(
        completed.map(|answer| answer.text.as_str()),
        Some("The route delegates to the service.")
    );
    assert_eq!(
        completed.and_then(|answer| answer.diagram.as_ref()),
        Some(&expected_diagram)
    );
    assert_eq!(app.evidence().len(), 1);

    app.reduce(AppEvent::UsageUpdated {
        request_id: answer_request,
        usage: ModelUsage {
            tokens: TokenUsage {
                input_tokens: 900,
                output_tokens: 250,
                cached_input_tokens: 150,
                total_tokens: 1_150,
            },
            cost: Some(Cost {
                currency: "USD".to_owned(),
                amount: 0.015,
                estimated: true,
            }),
        },
    });
    assert_eq!(app.total_token_usage().total_tokens, 1_150);
    assert_eq!(app.total_cost().map(|cost| cost.amount), Some(0.015));
}

#[test]
fn evidence_panel_shows_completed_citations_with_claim_context() {
    let mut app = TuiApp::new("cited-evidence-panel");
    let request_id = id_request("cited-evidence-panel");
    let cited = sample_evidence("cited", "src/cited.rs", 10);
    let explored = sample_evidence("explored", "src/explored.rs", 20);
    app.reduce(AppEvent::EvidenceAdded {
        request_id,
        evidence: explored,
    });
    assert!(app.evidence().is_empty());

    app.reduce(AppEvent::AnswerCompleted {
        request_id,
        answer: sample_answer(&cited),
    });
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let text = rendered_text(&mut app, 100, 28);

    assert!(text.contains("1 cited"));
    assert!(text.contains("[F1,I1] E1"));
    assert!(text.contains("src/cited.rs:10-12"));
    assert!(text.contains("[F1] route calls service.call"));
    assert!(!text.contains("src/explored.rs"));
}

#[test]
fn evidence_detail_and_fullscreen_viewer_apply_syntax_highlighting() {
    let mut app = TuiApp::new("highlighted-evidence");
    add_evidence(
        &mut app,
        sample_evidence("highlighted-evidence", "src/highlighted.rs", 10),
    );
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));

    let detail = rendered_buffer(&mut app, 100, 28);
    assert_source_is_highlighted(&detail, 100, 28, "fn route() {");

    assert!(app.handle_key(key(KeyCode::Enter)).is_none());
    let viewer = rendered_buffer(&mut app, 100, 28);
    assert_source_is_highlighted(&viewer, 100, 28, "fn route() {");
}

#[test]
fn conversation_and_evidence_share_fact_numbers_across_answers() {
    let mut app = TuiApp::new("multi-answer-claim-numbers");
    let first_evidence = sample_evidence("multi-first", "src/first_fact.rs", 10);
    let second_evidence = sample_evidence("multi-second", "src/second_fact.rs", 20);

    let mut first_answer = sample_answer(&first_evidence);
    first_answer.text = "First completed answer.".to_owned();
    first_answer.claims = vec![Claim {
        id: ClaimId::from_stable_parts(&["multi-first-fact"]),
        kind: ClaimKind::Fact,
        text: "FIRST_FACT_MARKER".to_owned(),
        evidence_ids: vec![first_evidence.id],
    }];
    first_answer.call_paths.clear();
    app.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("multi-first-answer"),
        answer: first_answer,
    });

    let mut second_answer = sample_answer(&second_evidence);
    second_answer.text = "Second completed answer.".to_owned();
    second_answer.claims = vec![Claim {
        id: ClaimId::from_stable_parts(&["multi-second-fact"]),
        kind: ClaimKind::Fact,
        text: "SECOND_FACT_MARKER".to_owned(),
        evidence_ids: vec![second_evidence.id],
    }];
    second_answer.call_paths.clear();
    app.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("multi-second-answer"),
        answer: second_answer,
    });

    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('2')));
    let conversation = rendered_text(&mut app, 100, 32);
    assert!(conversation.contains("[F1] [E1] FIRST_FACT_MARKER"));
    assert!(conversation.contains("[F2] [E2] SECOND_FACT_MARKER"));

    let _ = app.handle_key(key(KeyCode::Char('e')));
    let first_evidence_panel = rendered_text(&mut app, 100, 32);
    assert!(first_evidence_panel.contains("[F1] E1"));
    assert!(first_evidence_panel.contains("[F2] E2"));
    assert!(first_evidence_panel.contains("[F1] FIRST_FACT_MARKER"));

    let _ = app.handle_key(key(KeyCode::Char('j')));
    let second_evidence_panel = rendered_text(&mut app, 100, 32);
    assert!(second_evidence_panel.contains("[F2] SECOND_FACT_MARKER"));

    assert!(app.handle_key(key(KeyCode::Enter)).is_none());
    let second_evidence_viewer = rendered_text(&mut app, 100, 32);
    assert!(second_evidence_viewer.contains("Source Evidence E2/2"));
    assert!(second_evidence_viewer.contains("[F2] SECOND_FACT_MARKER"));
    assert!(!second_evidence_viewer.contains("[F1] FIRST_FACT_MARKER"));
}

#[test]
fn reducer_handles_duplicate_index_cancel_and_error_events() {
    let mut app = TuiApp::new("terminal-event-reducer");
    let index_request = id_request("late-index");
    let repository_id = RepositoryId::from_stable_parts(&["reducer-repository"]);
    app.reduce(AppEvent::IndexCompleted {
        request_id: index_request,
        repository_id,
        file_count: 7,
        symbol_count: 31,
        repository_map: RepositoryMap::default(),
    });
    app.reduce(AppEvent::IndexCompleted {
        request_id: index_request,
        repository_id,
        file_count: 7,
        symbol_count: 31,
        repository_map: RepositoryMap::default(),
    });
    assert_eq!(app.repository().repository_id, Some(repository_id));

    let cancelled = id_request("cancelled");
    app.reduce(AppEvent::Cancelled {
        request_id: cancelled,
    });
    app.reduce(AppEvent::Cancelled {
        request_id: cancelled,
    });
    app.reduce(AppEvent::Error {
        request_id: None,
        error: AppError {
            code: "runtime_unavailable".to_owned(),
            message: "Runtime disconnected".to_owned(),
            retryable: true,
        },
    });
    app.reduce(AppEvent::Error {
        request_id: None,
        error: AppError {
            code: "runtime_unavailable".to_owned(),
            message: "Runtime disconnected".to_owned(),
            retryable: true,
        },
    });
    assert_eq!(
        app.error().map(|error| error.code.as_str()),
        Some("runtime_unavailable")
    );
}

#[test]
fn non_fatal_postprocessing_error_does_not_hide_completed_answer() {
    let mut app = TuiApp::new("postprocessing-warning");
    let request_id = id_request("postprocessing-warning");
    let evidence = sample_evidence("postprocessing-warning", "src/lib.rs", 1);

    app.reduce(AppEvent::Error {
        request_id: Some(request_id),
        error: AppError {
            code: "session_save_failed".to_owned(),
            message: "answer remains available but was not persisted".to_owned(),
            retryable: true,
        },
    });
    app.reduce(AppEvent::AnswerCompleted {
        request_id,
        answer: sample_answer(&evidence),
    });

    assert_eq!(app.activity(), Activity::AnswerReady);
    assert!(app.conversation().iter().any(|entry| matches!(
        entry,
        ConversationEntry::Answer(answer)
            if answer.request_id == request_id && answer.complete && !answer.text.is_empty()
    )));
}

#[test]
fn long_errors_open_a_scrollable_and_copyable_detail_view() {
    let mut app = TuiApp::new("long-error");
    let _ = app.handle_key(key(KeyCode::Esc));
    let message = (0..40)
        .map(|line| format!("diagnostic line {line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.reduce(AppEvent::Error {
        request_id: Some(id_request("long-error-request")),
        error: AppError {
            code: "model_error".to_owned(),
            message: message.clone(),
            retryable: true,
        },
    });

    assert!(app.error_details_visible());
    let first_page = rendered_text(&mut app, 100, 18);
    assert!(first_page.contains("Error Details"));
    assert!(first_page.contains("diagnostic line 00"));
    assert!(first_page.contains("y copy full error"));

    let _ = app.handle_key(key(KeyCode::End));
    let last_page = rendered_text(&mut app, 100, 18);
    assert!(last_page.contains("diagnostic line 39"));

    let _ = app.handle_key(key(KeyCode::Char('y')));
    let copied = app
        .take_clipboard_request()
        .expect("copy key should expose the complete error report");
    assert!(copied.contains("code: model_error"));
    assert!(copied.contains("diagnostic line 00"));
    assert!(copied.contains("diagnostic line 39"));

    let _ = app.handle_key(key(KeyCode::Esc));
    assert!(!app.error_details_visible());
    let _ = app.handle_key(key(KeyCode::Char('x')));
    assert!(app.error_details_visible());
}

#[test]
fn osc52_payload_uses_standard_base64() {
    assert_eq!(encode_base64(b""), "");
    assert_eq!(encode_base64(b"f"), "Zg==");
    assert_eq!(encode_base64(b"fo"), "Zm8=");
    assert_eq!(encode_base64(b"foo"), "Zm9v");
    assert_eq!(encode_base64("错误".as_bytes()), "6ZSZ6K+v");
}

#[test]
fn facts_references_trace_and_source_excerpt_are_visible() {
    let mut app = TuiApp::new("grounded-render");
    let request_id = id_request("grounded-answer");
    let evidence = sample_evidence("grounded", "src/router.rs", 10);
    app.reduce(AppEvent::AnswerCompleted {
        request_id,
        answer: sample_answer(&evidence),
    });

    let text = rendered_text(&mut app, 160, 40);
    assert!(text.contains("[F1] [E1] route calls service.call"));
    assert!(text.contains("[I1] [E1]"));
    assert!(text.contains("[U1] [no evidence]"));
    assert!(text.contains("src/router.rs:10-12"));
    assert!(text.contains("symbol #"));
    assert!(text.contains("service.call();"));
    assert!(text.contains("CALL PATHS"));
}

#[test]
fn conversation_renders_all_needed_diagram_kinds_and_artifact_states() {
    for (kind, label, artifact_path) in [
        (
            DiagramKind::Architecture,
            "ARCHITECTURE",
            Some("artifacts/request-architecture.svg"),
        ),
        (DiagramKind::Flow, "FLOW", None),
        (
            DiagramKind::Relationship,
            "RELATIONSHIP",
            Some("artifacts/request-relationship.svg"),
        ),
    ] {
        let mut app = TuiApp::new(format!("needed-{label}"));
        let evidence = sample_evidence(label, "src/router.rs", 10);
        let mut answer = sample_answer(&evidence);
        answer.diagram = sample_needed_diagram(kind, evidence.id, artifact_path);
        app.reduce(AppEvent::AnswerCompleted {
            request_id: id_request(label),
            answer,
        });

        let text = rendered_text(&mut app, 160, 40);
        assert!(text.contains(&format!("[DIAGRAM: {label}] Request routing")));
        assert!(text.contains("WHY The request crosses multiple components."));
        assert!(text.contains("3 nodes / 2 edges"));
        assert!(text.contains("grounded by [E1]"));
        if let Some(path) = artifact_path {
            let artifact_id = DiagramId::from_stable_parts(&["diagram-artifact"]);
            let short_id: String = artifact_id.to_string().chars().take(8).collect();
            assert!(text.contains(&format!("SVG #{short_id} ready")));
            assert!(text.contains("[o] open  [y] copy path"));
            assert!(!text.contains(path));
        } else {
            assert!(text.contains("SVG unavailable"));
        }
    }
}

#[test]
fn selected_diagram_opens_with_one_key_and_copies_its_path() {
    let mut app = TuiApp::new("diagram-open");
    let request_id = id_request("diagram-open-answer");
    let evidence = sample_evidence("diagram-open", "src/router.rs", 10);
    let path = "/tmp/codeatlas/diagrams/v1/answer/diagram.svg";
    let mut answer = sample_answer(&evidence);
    answer.diagram = sample_needed_diagram(DiagramKind::Architecture, evidence.id, Some(path));
    let DiagramDecision::Needed { diagram, .. } = &answer.diagram else {
        panic!("sample diagram should be needed");
    };
    let diagram_id = diagram.artifact.as_ref().expect("sample artifact").id;
    app.reduce(AppEvent::AnswerCompleted { request_id, answer });
    let _ = app.handle_key(key(KeyCode::Esc));

    let command = app
        .handle_key(key(KeyCode::Char('o')))
        .expect("o should request the selected diagram");
    let open_request_id = match command {
        AppCommand::OpenDiagram {
            request_id,
            diagram_id: opened_diagram_id,
        } => {
            assert_eq!(opened_diagram_id, diagram_id);
            request_id
        }
        _ => panic!("expected diagram open command"),
    };
    let opening = rendered_text(&mut app, 160, 48);
    assert!(opening.contains("Opening in the default viewer..."));
    assert!(!opening.contains(path));

    app.reduce(AppEvent::DiagramOpened {
        request_id: open_request_id,
        diagram_id,
    });
    let opened = rendered_text(&mut app, 160, 48);
    assert!(opened.contains("Opened in the default viewer."));

    assert!(app.handle_key(key(KeyCode::Char('y'))).is_none());
    assert_eq!(app.take_clipboard_request().as_deref(), Some(path));
    assert_eq!(
        app.clipboard_status(),
        Some("Copying SVG path to clipboard...")
    );

    let failed_command = app
        .handle_key(key(KeyCode::Char('o')))
        .expect("a diagram can be opened again");
    let AppCommand::OpenDiagram {
        request_id: failed_request_id,
        ..
    } = failed_command
    else {
        panic!("expected diagram open command");
    };
    app.reduce(AppEvent::Error {
        request_id: Some(failed_request_id),
        error: AppError {
            code: "diagram_open_failed".to_owned(),
            message: "no default viewer is installed".to_owned(),
            retryable: false,
        },
    });
    assert!(app.error().is_none());
    let failed = rendered_text(&mut app, 160, 48);
    assert!(failed.contains("Could not open SVG: no default viewer is installed"));
}

#[test]
fn diagram_evidence_references_are_deduplicated_and_bounded() {
    let mut app = TuiApp::new("diagram-evidence-bound");
    let evidence: Vec<_> = (1..=10)
        .map(|number| {
            sample_evidence(
                &format!("diagram-evidence-{number}"),
                &format!("src/evidence-{number}.rs"),
                number,
            )
        })
        .collect();
    let mut answer = sample_answer(&evidence[0]);
    answer.evidence.clone_from(&evidence);
    answer.diagram = sample_needed_diagram(DiagramKind::Architecture, evidence[0].id, None);
    let DiagramDecision::Needed { diagram, .. } = &mut answer.diagram else {
        panic!("sample needed diagram must be needed");
    };
    diagram.nodes[0].evidence_ids = vec![
        evidence[0].id,
        evidence[1].id,
        evidence[0].id,
        evidence[2].id,
        evidence[3].id,
    ];
    diagram.nodes[1].evidence_ids = vec![evidence[4].id, evidence[5].id];
    diagram.nodes[2].evidence_ids = vec![evidence[6].id, evidence[7].id];
    diagram.edges[0].evidence_ids = vec![evidence[7].id, evidence[8].id];
    diagram.edges[1].evidence_ids = vec![evidence[9].id];
    app.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("diagram-evidence-bound"),
        answer,
    });

    let text = rendered_text(&mut app, 160, 40);
    assert!(text.contains("grounded by [E1,E2,E3,E4,E5,E6,E7,E8]"));
    assert!(!text.contains("grounded by [E1,E2,E3,E4,E5,E6,E7,E8,E9"));
}

#[test]
fn not_needed_diagram_is_a_single_muted_summary_line() {
    let mut app = TuiApp::new("diagram-not-needed");
    let evidence = sample_evidence("not-needed", "src/direct.rs", 1);
    let mut answer = sample_answer(&evidence);
    answer.claims.clear();
    answer.call_paths.clear();
    answer.diagram = DiagramDecision::NotNeeded {
        reason: "A direct lookup fully answers the question.".to_owned(),
    };
    app.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("diagram-not-needed"),
        answer,
    });

    let text = rendered_text(&mut app, 160, 40);
    assert!(text.contains("[DIAGRAM] not needed: A direct lookup fully answers the question."));
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("[DIAGRAM]"))
            .count(),
        1
    );
    assert!(!text.contains("SVG unavailable"));
}

#[test]
fn diagram_control_sequences_render_as_plain_text_only() {
    let mut app = TuiApp::new("diagram-control-text");
    let evidence = sample_evidence("control", "src/control.rs", 1);
    let mut answer = sample_answer(&evidence);
    answer.claims.clear();
    answer.call_paths.clear();
    answer.diagram = sample_needed_diagram(
        DiagramKind::Flow,
        evidence.id,
        Some("out/\u{1b}]8;;x\u{7}diagram.svg\u{1b}]8;;\u{7}"),
    );
    let DiagramDecision::Needed { reason, diagram } = &mut answer.diagram else {
        panic!("sample needed diagram must be needed");
    };
    *reason = "why\u{1b}]52;c;payload\u{7}".to_owned();
    diagram.title = "\u{1b}]8;;x\u{7}title\u{1b}]8;;\u{7}".to_owned();
    app.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("diagram-control-text"),
        answer,
    });

    let text = rendered_text(&mut app, 220, 50);
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{7}'));
    assert!(text.contains(r"\u{1b}]8;;x\u{7}title"));
    assert!(text.contains(r"why\u{1b}]52;c;payload\u{7}"));
    assert!(!text.contains("out/"));
}

#[test]
fn diagram_summary_wraps_without_panicking_in_all_layout_modes() {
    let evidence = sample_evidence("layout", "src/layout.rs", 1);
    let mut answer = sample_answer(&evidence);
    answer.text = "Summary.".to_owned();
    answer.claims.clear();
    answer.call_paths.clear();
    answer.diagram = sample_needed_diagram(DiagramKind::Relationship, evidence.id, None);
    let DiagramDecision::Needed { reason, diagram } = &mut answer.diagram else {
        panic!("sample needed diagram must be needed");
    };
    *reason = "This relationship spans the router, service, and persistence boundary.".to_owned();
    diagram.title = "A relationship summary that wraps on a narrow terminal".to_owned();

    let mut base = TuiApp::new("diagram-layouts");
    base.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("diagram-layouts"),
        answer,
    });
    let _ = base.handle_key(key(KeyCode::Esc));
    let _ = base.handle_key(key(KeyCode::Char('2')));

    for (width, height, mode) in [
        (160, 40, LayoutMode::Wide),
        (48, 18, LayoutMode::Tabbed),
        (23, 7, LayoutMode::Compact),
        (1, 1, LayoutMode::Compact),
    ] {
        let mut app = base.clone();
        assert_eq!(app.layout_mode(width, height), mode);
        assert!(!rendered_text(&mut app, width, height).is_empty());
    }
}

#[test]
fn conversation_renders_common_markdown_without_source_markers() {
    let mut app = TuiApp::new("markdown-render");
    let evidence = sample_evidence("markdown", "src/lib.rs", 1);
    let mut answer = sample_answer(&evidence);
    answer.text = concat!(
        "## Architecture for C#\n\n",
        "**CodeAtlas** uses `Rust` and *structured evidence*.\n\n",
        "The codeatlas_core crate owns shared contracts.\n\n",
        "- indexes source files\n",
        "- [x] links claims to evidence\n\n",
        "> Read-only analysis\n\n",
        "| Crate | Role |\n",
        "|---|---|\n",
        "| core | contracts |\n\n",
        "```rust\n",
        "fn main() {}\n",
        "```"
    )
    .to_owned();
    answer.claims.clear();
    answer.evidence.clear();
    answer.call_paths.clear();
    answer.usage = None;
    app.reduce(AppEvent::AnswerCompleted {
        request_id: id_request("markdown-answer"),
        answer,
    });

    let text = rendered_text(&mut app, 160, 40);
    assert!(text.contains("Architecture for C#"));
    assert!(text.contains("CodeAtlas uses Rust and structured evidence."));
    assert!(text.contains("codeatlas_core"));
    assert!(text.contains("- indexes source files"));
    assert!(text.contains("- [x] links claims to evidence"));
    assert!(text.contains("| Read-only analysis"));
    assert!(text.contains("Crate | Role"));
    assert!(text.contains("core | contracts"));
    assert!(text.contains("[rust]"));
    assert!(text.contains("fn main() {}"));
    assert!(!text.contains("## Architecture"));
    assert!(!text.contains("**CodeAtlas**"));
    assert!(!text.contains("`Rust`"));
    assert!(!text.contains("|---|---|"));
    assert!(!text.contains("```"));
}

#[test]
fn rendering_is_terminal_independent_and_repeatable() {
    let mut app = TuiApp::new("repeatable");
    complete_index(&mut app, "/repo");
    let mut first = app.clone();
    let mut second = app.clone();

    let first_render = rendered_text(&mut first, 100, 30);
    let second_render = rendered_text(&mut second, 100, 30);

    assert_eq!(first_render, second_render);
}

#[test]
fn channel_adapter_only_transports_core_commands_and_events() {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let mut port = ChannelApplicationPort::new(command_sender, event_receiver);
    let command = AppCommand::Index {
        request_id: id_request("channel-command"),
        repository_root: "/repo".to_owned(),
    };
    port.send_command(command.clone())
        .expect("open command channel");
    assert_eq!(command_receiver.recv().expect("command available"), command);

    let event = AppEvent::Cancelled {
        request_id: id_request("channel-event"),
    };
    event_sender
        .send(event.clone())
        .expect("open event channel");
    assert_eq!(
        port.try_recv_event().expect("open event channel"),
        Some(event)
    );
    assert_eq!(port.try_recv_event().expect("empty event channel"), None);
}

#[test]
fn serializable_preferences_and_snapshot_have_no_secret_field() {
    let preferences = UiPreferences::default();
    let serialized = serde_json::to_string(&preferences).expect("preferences serialize");
    assert!(!serialized.contains("api_key"));
    assert!(!serialized.contains("secret"));
    let round_trip: UiPreferences =
        serde_json::from_str(&serialized).expect("preferences deserialize");
    assert_eq!(round_trip, preferences);

    let mut app = TuiApp::new("snapshot");
    let mut evidence = sample_evidence("snapshot", "src/snapshot.rs", 1);
    evidence.excerpt = Some("private source text must stay out of snapshots".to_owned());
    add_evidence(&mut app, evidence);
    let _ = app.handle_key(key(KeyCode::Esc));
    let _ = app.handle_key(key(KeyCode::Char('e')));
    let _ = app.handle_key(key(KeyCode::Enter));
    let snapshot = serde_json::to_string(&app.snapshot()).expect("snapshot serializes");
    assert!(!snapshot.contains("api_key"));
    assert!(!snapshot.contains("secret"));
    assert!(!snapshot.contains("private source text"));
}
