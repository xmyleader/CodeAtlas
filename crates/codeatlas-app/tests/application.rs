#![allow(clippy::expect_used, clippy::too_many_lines)]

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use codeatlas_agent::{
    AssistantOutput, AssistantToolCall, ModelClient, ModelError, ModelPricing, ModelRequest,
    ModelResponse, ModelRole, SessionState, SessionStore, StructuredAnswer, StructuredClaim,
    StructuredDiagram, StructuredDiagramDecision, StructuredDiagramEdge, StructuredDiagramNode,
};
use codeatlas_app::{ApplicationChannels, ApplicationConfig, ApplicationRunner, spawn_application};
use codeatlas_core::{
    AgentAnswer, AppCommand, AppEvent, ClaimKind, DiagramDecision, DiagramId, DiagramKind,
    EvidenceId, ExplanationAudience, ExplanationDepth, ExplanationProfile, ModelBudget,
    ProgressPhase, RepositoryId, RepositoryPath, RequestId, SessionId, SessionTaskStatus,
    SuggestedAction, TokenUsage, WorkflowEvent,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Notify;

const EVENT_TIMEOUT: Duration = Duration::from_secs(20);
const FIRST_QUESTION: &str = "Where is the Rust entry function?";
const FOLLOW_UP_QUESTION: &str = "Explain it again using our prior context.";
const GROUNDED_TEXT: &str = "rust_entry is defined in src/lib.rs.";

#[derive(Default)]
struct AdaptiveModel {
    requests: Mutex<Vec<ModelRequest>>,
    tool_call_sequence: AtomicUsize,
}

impl AdaptiveModel {
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl ModelClient for AdaptiveModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());

        if let Some(evidence_id) = tool_evidence_id(&request)? {
            return Ok(ModelResponse {
                output: AssistantOutput::FinalAnswer {
                    answer: StructuredAnswer {
                        text: GROUNDED_TEXT.to_owned(),
                        claims: vec![StructuredClaim {
                            kind: ClaimKind::Fact,
                            text: GROUNDED_TEXT.to_owned(),
                            evidence_ids: vec![evidence_id],
                        }],
                        call_paths: Vec::new(),
                        diagram: StructuredDiagramDecision::NotNeeded {
                            reason: "A short source lookup does not benefit from a diagram."
                                .to_owned(),
                        },
                    },
                },
                usage: Some(token_usage(7, 3)),
                finish_reason: Some("stop".to_owned()),
            });
        }

        let sequence = self.tool_call_sequence.fetch_add(1, Ordering::Relaxed);
        Ok(ModelResponse {
            output: AssistantOutput::ToolCalls {
                content: None,
                calls: vec![AssistantToolCall {
                    id: format!("offline-find-symbol-{sequence}"),
                    name: "find_symbol".to_owned(),
                    arguments: json!({
                        "query": "rust_entry",
                        "case_sensitive": true,
                        "limit": 5
                    }),
                }],
            },
            usage: Some(token_usage(5, 2)),
            finish_reason: Some("tool_calls".to_owned()),
        })
    }
}

fn tool_evidence_id(request: &ModelRequest) -> Result<Option<EvidenceId>, ModelError> {
    let Some(tool_message) = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ModelRole::Tool)
    else {
        return Ok(None);
    };
    let content = tool_message
        .content
        .as_deref()
        .ok_or_else(|| ModelError::InvalidResponse {
            message: "tool message did not contain an envelope".to_owned(),
        })?;
    let envelope: Value =
        serde_json::from_str(content).map_err(|error| ModelError::InvalidResponse {
            message: format!("tool message was not a repository envelope: {error}"),
        })?;
    let evidence_id = envelope
        .get("evidence")
        .and_then(Value::as_array)
        .and_then(|evidence| evidence.first())
        .and_then(|evidence| evidence.get("id"))
        .cloned()
        .ok_or_else(|| ModelError::InvalidResponse {
            message: "find_symbol returned no evidence".to_owned(),
        })?;
    serde_json::from_value(evidence_id)
        .map(Some)
        .map_err(|error| ModelError::InvalidResponse {
            message: format!("find_symbol returned an invalid evidence ID: {error}"),
        })
}

struct DiagramModel {
    title: String,
    tool_call_sequence: AtomicUsize,
}

impl DiagramModel {
    fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            tool_call_sequence: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelClient for DiagramModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        if let Some(evidence_id) = tool_evidence_id(&request)? {
            return Ok(ModelResponse {
                output: AssistantOutput::FinalAnswer {
                    answer: StructuredAnswer {
                        text: GROUNDED_TEXT.to_owned(),
                        claims: vec![StructuredClaim {
                            kind: ClaimKind::Fact,
                            text: GROUNDED_TEXT.to_owned(),
                            evidence_ids: vec![evidence_id],
                        }],
                        call_paths: Vec::new(),
                        diagram: StructuredDiagramDecision::Needed {
                            reason: "The call relationship benefits from a diagram.".to_owned(),
                            diagram: StructuredDiagram {
                                kind: DiagramKind::Flow,
                                title: self.title.clone(),
                                nodes: vec![
                                    StructuredDiagramNode {
                                        id: "../../model-entry".to_owned(),
                                        label: "rust_entry".to_owned(),
                                        claim_indices: vec![0],
                                    },
                                    StructuredDiagramNode {
                                        id: "model-helper".to_owned(),
                                        label: "rust_helper".to_owned(),
                                        claim_indices: vec![0],
                                    },
                                ],
                                edges: vec![StructuredDiagramEdge {
                                    source: "../../model-entry".to_owned(),
                                    target: "model-helper".to_owned(),
                                    label: "calls".to_owned(),
                                    claim_indices: vec![0],
                                }],
                            },
                        },
                    },
                },
                usage: Some(token_usage(7, 3)),
                finish_reason: Some("stop".to_owned()),
            });
        }

        let sequence = self.tool_call_sequence.fetch_add(1, Ordering::Relaxed);
        Ok(ModelResponse {
            output: AssistantOutput::ToolCalls {
                content: None,
                calls: vec![AssistantToolCall {
                    id: format!("diagram-find-symbol-{sequence}"),
                    name: "find_symbol".to_owned(),
                    arguments: json!({
                        "query": "rust_entry",
                        "case_sensitive": true,
                        "limit": 5
                    }),
                }],
            },
            usage: Some(token_usage(5, 2)),
            finish_reason: Some("tool_calls".to_owned()),
        })
    }
}

#[derive(Default)]
struct RejectingModel {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelClient for RejectingModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Err(ModelError::InvalidRequest {
            message: "the model must not be called for rejected commands".to_owned(),
        })
    }
}

struct GatedModel {
    inner: AdaptiveModel,
    calls: AtomicUsize,
    first_call_started: Mutex<Option<Sender<()>>>,
    release_first_call: Notify,
}

impl GatedModel {
    fn new(first_call_started: Sender<()>) -> Self {
        Self {
            inner: AdaptiveModel::default(),
            calls: AtomicUsize::new(0),
            first_call_started: Mutex::new(Some(first_call_started)),
            release_first_call: Notify::new(),
        }
    }
}

#[async_trait]
impl ModelClient for GatedModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            if let Some(started) = self
                .first_call_started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = started.send(());
            }
            self.release_first_call.notified().await;
        }
        self.inner.complete(request).await
    }
}

struct LockDelayTimeoutModel {
    lock_holder_started: Sender<()>,
    timed_call_started: Sender<()>,
    lock_hold: Duration,
}

#[async_trait]
impl ModelClient for LockDelayTimeoutModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let question = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == ModelRole::User)
            .and_then(|message| message.content.as_deref());
        if question == Some(FIRST_QUESTION) {
            let _ = self.lock_holder_started.send(());
            tokio::time::sleep(self.lock_hold).await;
            return Ok(ModelResponse {
                output: AssistantOutput::FinalAnswer {
                    answer: StructuredAnswer {
                        text: "The first request completed.".to_owned(),
                        claims: vec![StructuredClaim {
                            kind: ClaimKind::Inference,
                            text: "The first request completed.".to_owned(),
                            evidence_ids: Vec::new(),
                        }],
                        call_paths: Vec::new(),
                        diagram: StructuredDiagramDecision::NotNeeded {
                            reason: "No structural explanation is needed.".to_owned(),
                        },
                    },
                },
                usage: Some(token_usage(1, 1)),
                finish_reason: Some("stop".to_owned()),
            });
        }

        let _ = self.timed_call_started.send(());
        std::future::pending::<Result<ModelResponse, ModelError>>().await
    }
}

fn token_usage(input_tokens: u64, output_tokens: u64) -> TokenUsage {
    TokenUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens: 0,
        total_tokens: input_tokens + output_tokens,
    }
}

struct RunningApplication {
    commands: Option<Sender<AppCommand>>,
    events: Receiver<AppEvent>,
    runner: Option<ApplicationRunner>,
}

impl RunningApplication {
    fn spawn(config: ApplicationConfig, model: Arc<dyn ModelClient>) -> Self {
        let (channels, runner) =
            spawn_application(config, model).expect("application should start for the test");
        let ApplicationChannels { commands, events } = channels;
        Self {
            commands: Some(commands),
            events,
            runner: Some(runner),
        }
    }

    fn send(&self, command: AppCommand) {
        self.commands
            .as_ref()
            .expect("application command sender should be open")
            .send(command)
            .expect("application should accept the command");
    }

    fn receive_until(
        &self,
        description: &str,
        mut terminal: impl FnMut(&AppEvent) -> bool,
    ) -> Vec<AppEvent> {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        let mut events = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for {description}; events: {events:?}"
            );
            let event = match self.events.recv_timeout(remaining) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("timed out waiting for {description}; events: {events:?}")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("event channel disconnected while waiting for {description}")
                }
            };
            let is_terminal = terminal(&event);
            events.push(event);
            if is_terminal {
                return events;
            }
        }
    }

    fn stop(&mut self) -> Result<(), String> {
        drop(self.commands.take());
        if let Some(runner) = self.runner.take() {
            runner.join().map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn shutdown(mut self) {
        self.stop().expect("application should shut down cleanly");
    }
}

impl Drop for RunningApplication {
    fn drop(&mut self) {
        let _shutdown_result = self.stop();
    }
}

fn fixture(root: &Path) {
    let rust = root.join("src/lib.rs");
    let python = root.join("python/service.py");
    fs::create_dir_all(rust.parent().expect("Rust source should have a parent"))
        .expect("Rust source directory should be created");
    fs::create_dir_all(python.parent().expect("Python source should have a parent"))
        .expect("Python source directory should be created");
    fs::write(
        rust,
        "pub fn rust_entry() {\n    rust_helper();\n}\n\nfn rust_helper() {}\n",
    )
    .expect("Rust fixture should be written");
    fs::write(
        python,
        "def python_entry():\n    return python_helper()\n\ndef python_helper():\n    return 42\n",
    )
    .expect("Python fixture should be written");
}

fn config(data_directory: PathBuf) -> ApplicationConfig {
    let mut config = ApplicationConfig::new(data_directory);
    config.agent.timeout = Duration::from_secs(10);
    config
}

fn load_session_eventually(store: &SessionStore, session_id: SessionId) -> SessionState {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        match store.load(session_id) {
            Ok(session) => return session,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("session {session_id} was not persisted in time: {error}"),
        }
    }
}

fn index_repository(
    application: &RunningApplication,
    repository_root: &Path,
    label: &str,
) -> (RepositoryId, Vec<AppEvent>) {
    let request_id = RequestId::from_stable_parts(&[label, "index"]);
    application.send(AppCommand::Index {
        request_id,
        repository_root: repository_root.to_string_lossy().into_owned(),
    });
    let events = application.receive_until("IndexCompleted", |event| {
        matches!(
            event,
            AppEvent::IndexCompleted {
                request_id: event_request_id,
                ..
            } | AppEvent::Cancelled {
                request_id: event_request_id,
            } if *event_request_id == request_id
        ) || matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                ..
            } if *event_request_id == request_id
        )
    });
    assert_no_request_error(&events, request_id);
    let repository_id = events
        .iter()
        .find_map(|event| match event {
            AppEvent::IndexCompleted {
                request_id: event_request_id,
                repository_id,
                ..
            } if *event_request_id == request_id => Some(*repository_id),
            _ => None,
        })
        .expect("index should complete successfully");
    (repository_id, events)
}

fn ask(
    application: &RunningApplication,
    request_id: RequestId,
    session_id: SessionId,
    repository_id: RepositoryId,
    question: &str,
) -> (AgentAnswer, Vec<AppEvent>) {
    let events = ask_until_terminal(application, request_id, session_id, repository_id, question);
    assert_no_request_error(&events, request_id);
    let answer = events
        .iter()
        .find_map(|event| match event {
            AppEvent::AnswerCompleted {
                request_id: event_request_id,
                answer,
            } if *event_request_id == request_id => Some(answer.clone()),
            _ => None,
        })
        .expect("ask should complete successfully");
    (answer, events)
}

fn ask_until_terminal(
    application: &RunningApplication,
    request_id: RequestId,
    session_id: SessionId,
    repository_id: RepositoryId,
    question: &str,
) -> Vec<AppEvent> {
    application.send(AppCommand::Ask {
        request_id,
        session_id,
        repository_id,
        question: question.to_owned(),
        profile: ExplanationProfile::default(),
    });
    application.receive_until("AnswerCompleted", |event| {
        matches!(
            event,
            AppEvent::AnswerCompleted {
                request_id: event_request_id,
                ..
            } | AppEvent::Cancelled {
                request_id: event_request_id,
            } if *event_request_id == request_id
        ) || matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == request_id
                && !matches!(
                    error.code.as_str(),
                    "diagram_generation_failed"
                        | "session_update_failed"
                        | "session_save_failed"
                )
        )
    })
}

fn assert_no_request_error(events: &[AppEvent], request_id: RequestId) {
    if let Some(event) = events.iter().find(|event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                ..
            } if *event_request_id == request_id
        ) || matches!(
            event,
            AppEvent::Cancelled {
                request_id: event_request_id,
            } if *event_request_id == request_id
        )
    }) {
        panic!("request {request_id} failed: {event:?}");
    }
}

fn event_position(events: &[AppEvent], predicate: impl Fn(&AppEvent) -> bool) -> usize {
    events
        .iter()
        .position(predicate)
        .expect("expected event should be present")
}

fn assert_no_automatic_session_events(events: &[AppEvent], index_request_id: RequestId) {
    assert!(events.iter().all(|event| !matches!(
        event,
        AppEvent::SessionsListed { request_id, .. }
            | AppEvent::SessionLoaded { request_id, .. }
            if *request_id == index_request_id
    )));
}

#[test]
fn opening_an_unknown_diagram_id_is_rejected_without_launching_a_viewer() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let model = Arc::new(RejectingModel::default());
    let application = RunningApplication::spawn(
        ApplicationConfig::new(temporary.path().join("data")),
        model.clone(),
    );
    let request_id = RequestId::from_stable_parts(&["diagram", "unknown-open-request"]);
    let diagram_id = DiagramId::from_stable_parts(&["diagram", "unknown-artifact"]);

    application.send(AppCommand::OpenDiagram {
        request_id,
        diagram_id,
    });
    let events = application.receive_until("diagram open rejection", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == request_id
                && error.code == "diagram_artifact_not_found"
        )
    });

    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == request_id
            && error.message.contains(&diagram_id.to_string())
            && !error.retryable
    )));
    assert_eq!(model.calls.load(Ordering::Relaxed), 0);
    application.shutdown();
}

#[test]
fn corrupt_session_is_reported_without_preventing_startup() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let data_directory = temporary.path().join("data");
    let sessions = data_directory.join("sessions");
    fs::create_dir_all(&sessions).expect("session directory should be created");
    fs::write(sessions.join("corrupt.json"), b"{not json")
        .expect("corrupt session fixture should be written");

    let model: Arc<dyn ModelClient> = Arc::new(RejectingModel::default());
    let application = RunningApplication::spawn(config(data_directory), model);
    let events = application.receive_until("corrupt session diagnostic", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: None,
                error,
            } if error.code == "corrupt_session_skipped"
        )
    });

    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Error { error, .. }
            if error.code == "corrupt_session_skipped"
                && error.message.contains("corrupt.json")
    )));
    application.shutdown();
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn answer_completed_can_immediately_open_in_memory_diagram() {
    const CHILD_MARKER: &str = "CODEATLAS_OPEN_DIAGRAM_TEST_CHILD";
    const CAPTURE_PATH: &str = "CODEATLAS_OPEN_DIAGRAM_TEST_CAPTURE";

    if std::env::var_os(CHILD_MARKER).is_none() {
        let temporary = TempDir::new().expect("temporary launcher directory should be created");
        let launcher_directory = temporary.path().join("bin");
        let capture = temporary.path().join("viewer.log");
        fs::create_dir(&launcher_directory).expect("launcher directory should be created");
        write_executable(
            &launcher_directory.join("wslpath"),
            "#!/bin/sh\nprintf 'wslpath:%s\\n' \"$2\" >> \"$CODEATLAS_OPEN_DIAGRAM_TEST_CAPTURE\"\nprintf 'C:\\\\fake\\\\diagram.svg\\n'\n",
        );
        write_executable(
            &launcher_directory.join("explorer.exe"),
            "#!/bin/sh\nprintf 'explorer:%s\\n' \"$1\" >> \"$CODEATLAS_OPEN_DIAGRAM_TEST_CAPTURE\"\n",
        );
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path = launcher_directory.into_os_string();
        path.push(":");
        path.push(inherited_path);
        let output = Command::new(std::env::current_exe().expect("test executable path"))
            .args([
                "--exact",
                "answer_completed_can_immediately_open_in_memory_diagram",
                "--nocapture",
            ])
            .env(CHILD_MARKER, "1")
            .env(CAPTURE_PATH, &capture)
            .env("PATH", path)
            .env_remove("WSL_INTEROP")
            .env_remove("WSL_DISTRO_NAME")
            .output()
            .expect("isolated application test should run");
        assert!(
            output.status.success(),
            "isolated application test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new("Immediate diagram"));
    let application = RunningApplication::spawn(config(data_directory.clone()), model);
    let (repository_id, _) = index_repository(&application, &repository_root, "immediate-open");
    fs::write(data_directory.join("sessions"), b"not a directory")
        .expect("session persistence should be forced to fail");
    let ask_request_id = RequestId::from_stable_parts(&["immediate-open", "ask"]);
    let session_id = SessionId::from_stable_parts(&["immediate-open", "session"]);
    let events = ask_until_terminal(
        &application,
        ask_request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    let artifact = events
        .iter()
        .find_map(|event| match event {
            AppEvent::AnswerCompleted { answer, .. } => match &answer.diagram {
                DiagramDecision::Needed { diagram, .. } => diagram.artifact.clone(),
                DiagramDecision::NotNeeded { .. } => None,
            },
            _ => None,
        })
        .expect("completed answer should include an artifact");

    let open_request_id = RequestId::from_stable_parts(&["immediate-open", "open"]);
    application.send(AppCommand::OpenDiagram {
        request_id: open_request_id,
        diagram_id: artifact.id,
    });
    let opened = application.receive_until("immediate DiagramOpened", |event| {
        matches!(
            event,
            AppEvent::DiagramOpened { request_id, diagram_id }
                if *request_id == open_request_id && *diagram_id == artifact.id
        ) || matches!(
            event,
            AppEvent::Error { request_id: Some(request_id), .. }
                if *request_id == open_request_id
        )
    });
    assert!(opened.iter().any(|event| matches!(
        event,
        AppEvent::DiagramOpened { request_id, diagram_id }
            if *request_id == open_request_id && *diagram_id == artifact.id
    )));
    assert!(data_directory.join("sessions").is_file());

    fs::write(
        &artifact.path,
        vec![b'x'; usize::try_from(artifact.byte_size).expect("artifact size should fit usize")],
    )
    .expect("artifact should be replaced with same-sized invalid content");
    let rejected_request_id = RequestId::from_stable_parts(&["immediate-open", "tampered"]);
    application.send(AppCommand::OpenDiagram {
        request_id: rejected_request_id,
        diagram_id: artifact.id,
    });
    let rejected = application.receive_until("tampered diagram rejection", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(request_id),
                error,
            } if *request_id == rejected_request_id && error.code == "diagram_open_failed"
        )
    });
    assert!(rejected.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(request_id),
            error,
        } if *request_id == rejected_request_id
            && error.code == "diagram_open_failed"
            && error.message.contains("content no longer matches")
            && !error.retryable
    )));

    let capture = fs::read_to_string(
        std::env::var_os(CAPTURE_PATH).expect("viewer capture path should be configured"),
    )
    .expect("viewer invocation should be captured");
    assert_eq!(
        capture
            .lines()
            .filter(|line| line.starts_with("wslpath:"))
            .count(),
        1
    );
    assert_eq!(
        capture
            .lines()
            .filter(|line| line.starts_with("explorer:"))
            .count(),
        1
    );
    assert!(capture.contains(&artifact.path));
    assert!(capture.contains(r"explorer:C:\fake\diagram.svg"));
    application.shutdown();
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("fake viewer command should be written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("fake viewer command should be executable");
}

#[test]
fn mixed_repository_runs_offline_agent_and_restores_session_history() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model = Arc::new(AdaptiveModel::default());
    let application_model: Arc<dyn ModelClient> = model.clone();
    let first_application =
        RunningApplication::spawn(config(data_directory.clone()), application_model);

    let (repository_id, index_events) =
        index_repository(&first_application, &repository_root, "first");
    let (file_count, symbol_count, repository_map) = index_events
        .iter()
        .find_map(|event| match event {
            AppEvent::IndexCompleted {
                file_count,
                symbol_count,
                repository_map,
                ..
            } => Some((*file_count, *symbol_count, repository_map)),
            _ => None,
        })
        .expect("IndexCompleted should carry counts");
    assert_eq!(file_count, 2);
    assert_eq!(symbol_count, 4);
    assert_eq!(repository_map.name, "repository");
    assert_eq!(repository_map.languages.len(), 2);
    assert!(repository_map.module_count > 0);
    for phase in [
        ProgressPhase::Scanning,
        ProgressPhase::Parsing,
        ProgressPhase::Indexing,
    ] {
        assert!(index_events.iter().any(|event| matches!(
            event,
            AppEvent::Progress { progress, .. } if progress.phase == phase
        )));
    }

    let initial_list_request_id =
        RequestId::from_stable_parts(&["offline-e2e", "initial-list-sessions"]);
    first_application.send(AppCommand::ListSessions {
        request_id: initial_list_request_id,
        repository_id: Some(repository_id),
    });
    let initially_listed = first_application.receive_until("initial SessionsListed", |event| {
        matches!(
            event,
            AppEvent::SessionsListed { request_id, .. } if *request_id == initial_list_request_id
        )
    });
    let first_index_request_id = RequestId::from_stable_parts(&["first", "index"]);
    assert_no_automatic_session_events(&initially_listed, first_index_request_id);
    assert!(initially_listed.iter().any(|event| matches!(
        event,
        AppEvent::SessionsListed {
            request_id,
            sessions,
        } if *request_id == initial_list_request_id && sessions.is_empty()
    )));

    let session_id = SessionId::from_stable_parts(&["offline-e2e-session"]);
    let first_request_id = RequestId::from_stable_parts(&["offline-e2e", "first-question"]);
    let (first_answer, ask_events) = ask(
        &first_application,
        first_request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    first_answer
        .validate_evidence()
        .expect("application answer should be evidence-grounded");
    assert_eq!(first_answer.text, GROUNDED_TEXT);
    assert_eq!(first_answer.claims.len(), 1);
    assert_eq!(first_answer.claims[0].kind, ClaimKind::Fact);
    assert_eq!(first_answer.evidence.len(), 1);
    assert_eq!(
        first_answer.claims[0].evidence_ids,
        vec![first_answer.evidence[0].id]
    );

    let tool_started = event_position(&ask_events, |event| {
        matches!(event, AppEvent::ToolCallStarted { .. })
    });
    let tool_completed = event_position(&ask_events, |event| {
        matches!(event, AppEvent::ToolCallCompleted { .. })
    });
    let evidence_added = event_position(&ask_events, |event| {
        matches!(event, AppEvent::EvidenceAdded { .. })
    });
    let answer_completed = event_position(&ask_events, |event| {
        matches!(event, AppEvent::AnswerCompleted { .. })
    });
    assert!(tool_started < tool_completed);
    assert!(tool_completed < evidence_added);
    assert!(evidence_added < answer_completed);
    let usage_updates = ask_events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            AppEvent::UsageUpdated { usage, .. } => Some((index, usage.tokens)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        usage_updates
            .iter()
            .map(|(_, usage)| usage.total_tokens)
            .collect::<Vec<_>>(),
        vec![7, 17]
    );
    assert!(
        usage_updates
            .iter()
            .all(|(index, _)| *index < answer_completed)
    );
    assert!(ask_events.iter().any(|event| matches!(
        event,
        AppEvent::EvidenceAdded { evidence, .. } if evidence.id == first_answer.evidence[0].id
    )));

    first_application.shutdown();

    let store = SessionStore::new(data_directory.join("sessions"));
    let persisted = store
        .load(session_id)
        .expect("completed question should be persisted");
    assert_eq!(persisted.repository_id, repository_id);
    assert_eq!(persisted.turns.len(), 1);
    assert_eq!(persisted.turns[0].request_id, first_request_id);
    assert_eq!(persisted.turns[0].question, FIRST_QUESTION);
    assert_eq!(persisted.turns[0].status, SessionTaskStatus::Completed);
    assert_eq!(persisted.turns[0].answer.as_ref(), Some(&first_answer));
    assert_eq!(persisted.turns[0].model_calls.len(), 2);
    assert_eq!(
        persisted.turns[0]
            .trajectory
            .iter()
            .filter(|event| matches!(event, WorkflowEvent::ModelRequest { .. }))
            .count(),
        2
    );
    assert_eq!(
        persisted.turns[0]
            .trajectory
            .iter()
            .filter(|event| matches!(event, WorkflowEvent::ModelResponse { .. }))
            .count(),
        2
    );
    assert!(
        persisted.turns[0]
            .trajectory
            .iter()
            .any(|event| matches!(event, WorkflowEvent::Progress(_)))
    );
    assert!(persisted.turns[0].trajectory.iter().any(|event| matches!(
        event,
        WorkflowEvent::ToolCallStarted(call) if call.name == "find_symbol"
    )));
    let workflow_output = persisted.turns[0]
        .trajectory
        .iter()
        .find_map(|event| match event {
            WorkflowEvent::ToolCallCompleted {
                output,
                is_error: false,
                ..
            } => Some(output),
            _ => None,
        })
        .expect("tool completion summary should be persisted");
    assert!(workflow_output.contains("pub fn rust_entry"));
    assert!(!workflow_output.contains("[source text omitted]"));
    let continued_tool_output = persisted.turns[0]
        .continuation
        .iter()
        .find(|message| message.role == ModelRole::Tool)
        .and_then(|message| message.content.as_deref())
        .expect("full tool message should be retained for continuation");
    assert!(continued_tool_output.contains("pub fn rust_entry"));

    let request_offset = model.requests().len();
    assert_eq!(request_offset, 2);
    let restarted_model: Arc<dyn ModelClient> = model.clone();
    let restarted = RunningApplication::spawn(config(data_directory.clone()), restarted_model);
    let (reloaded_repository_id, _) = index_repository(&restarted, &repository_root, "restart");
    assert_eq!(reloaded_repository_id, repository_id);
    let restart_index_request = RequestId::from_stable_parts(&["restart", "index"]);

    let list_request_id = RequestId::from_stable_parts(&["offline-e2e", "list-sessions"]);
    restarted.send(AppCommand::ListSessions {
        request_id: list_request_id,
        repository_id: Some(repository_id),
    });
    let listed = restarted.receive_until("SessionsListed", |event| {
        matches!(
            event,
            AppEvent::SessionsListed { request_id, .. } if *request_id == list_request_id
        )
    });
    let summaries = listed
        .iter()
        .find_map(|event| match event {
            AppEvent::SessionsListed {
                request_id,
                sessions,
            } if *request_id == list_request_id => Some(sessions),
            _ => None,
        })
        .expect("session list should be returned");
    assert_no_automatic_session_events(&listed, restart_index_request);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].session_id, session_id);
    assert_eq!(
        summaries[0].created_at_unix_ms,
        persisted.created_at_unix_ms
    );
    assert_eq!(
        summaries[0].updated_at_unix_ms,
        persisted.updated_at_unix_ms
    );
    assert_eq!(summaries[0].tasks[0].question, FIRST_QUESTION);
    assert!(
        Path::new(&summaries[0].json_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    );

    let load_request_id = RequestId::from_stable_parts(&["offline-e2e", "load-session"]);
    restarted.send(AppCommand::LoadSession {
        request_id: load_request_id,
        session_id,
    });
    let loaded = restarted.receive_until("SessionLoaded", |event| {
        matches!(
            event,
            AppEvent::SessionLoaded { request_id, .. } if *request_id == load_request_id
        )
    });
    let context = loaded
        .iter()
        .find_map(|event| match event {
            AppEvent::SessionLoaded {
                request_id,
                session,
            } if *request_id == load_request_id => Some(session),
            _ => None,
        })
        .expect("complete session context should be returned");
    assert_eq!(context.tasks.len(), 1);
    assert_eq!(context.created_at_unix_ms, persisted.created_at_unix_ms);
    assert_eq!(context.updated_at_unix_ms, persisted.updated_at_unix_ms);
    assert_eq!(context.tasks[0].answer.as_ref(), Some(&first_answer));
    assert!(!context.tasks[0].trajectory.is_empty());
    assert_eq!(model.requests().len(), request_offset);

    let duplicate_events = ask_until_terminal(
        &restarted,
        first_request_id,
        session_id,
        repository_id,
        "This request ID must not run twice.",
    );
    assert!(duplicate_events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == first_request_id && error.code == "request_id_conflict"
    )));
    assert_eq!(model.requests().len(), request_offset);

    let follow_up_request_id = RequestId::from_stable_parts(&["offline-e2e", "follow-up-question"]);
    let (follow_up_answer, _) = ask(
        &restarted,
        follow_up_request_id,
        session_id,
        repository_id,
        FOLLOW_UP_QUESTION,
    );
    follow_up_answer
        .validate_evidence()
        .expect("follow-up answer should be evidence-grounded");

    let requests = model.requests();
    assert_eq!(requests.len(), request_offset + 1);
    let follow_up_initial_request = &requests[request_offset];
    assert_eq!(
        follow_up_initial_request
            .messages
            .iter()
            .filter(|message| message.role == ModelRole::System)
            .count(),
        2
    );
    let conversational_messages = follow_up_initial_request
        .messages
        .iter()
        .filter(|message| matches!(message.role, ModelRole::User | ModelRole::Assistant))
        .map(|message| (message.role, message.content.as_deref()))
        .collect::<Vec<_>>();
    let tagged_first_answer = format!(
        "[CodeAtlas persisted answer {}]\n{GROUNDED_TEXT}",
        first_answer.id
    );
    assert_eq!(
        conversational_messages,
        vec![
            (ModelRole::User, Some(FIRST_QUESTION)),
            (ModelRole::Assistant, None),
            (ModelRole::Assistant, Some(tagged_first_answer.as_str())),
            (ModelRole::User, Some(FOLLOW_UP_QUESTION)),
        ]
    );
    assert!(
        follow_up_initial_request
            .messages
            .iter()
            .any(|message| message.role == ModelRole::Tool)
    );

    restarted.shutdown();
    let persisted = store
        .load(session_id)
        .expect("follow-up question should be persisted");
    assert_eq!(persisted.turns.len(), 2);
    assert_eq!(persisted.turns[1].request_id, follow_up_request_id);
    assert_eq!(persisted.turns[1].question, FOLLOW_UP_QUESTION);
}

#[test]
fn suggested_actions_load_source_without_model_and_follow_up_in_the_same_profiled_session() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model = Arc::new(AdaptiveModel::default());
    let application_model: Arc<dyn ModelClient> = model.clone();
    let application = RunningApplication::spawn(config(data_directory.clone()), application_model);
    let (repository_id, _) = index_repository(&application, &repository_root, "actions");
    let session_id = SessionId::from_stable_parts(&["actions", "session"]);
    let initial_request_id = RequestId::from_stable_parts(&["actions", "initial"]);
    let profile =
        ExplanationProfile::new(ExplanationAudience::Beginner, ExplanationDepth::Overview);
    application.send(AppCommand::Ask {
        request_id: initial_request_id,
        session_id,
        repository_id,
        question: FIRST_QUESTION.to_owned(),
        profile,
    });
    let initial_events = application.receive_until("initial profiled answer", |event| {
        matches!(
            event,
            AppEvent::AnswerCompleted { request_id, .. } if *request_id == initial_request_id
        )
    });
    let answer = initial_events
        .iter()
        .find_map(|event| match event {
            AppEvent::AnswerCompleted { answer, .. } => Some(answer.clone()),
            _ => None,
        })
        .expect("initial answer should complete");
    let show_source = answer
        .suggested_actions
        .iter()
        .copied()
        .find(|action| matches!(action, SuggestedAction::ShowSource { .. }))
        .expect("grounded answer should offer source");
    let model_calls_before_source = model.requests().len();
    let source_request_id = RequestId::from_stable_parts(&["actions", "source"]);
    application.send(AppCommand::RunSuggestedAction {
        request_id: source_request_id,
        session_id,
        repository_id,
        answer_id: answer.id,
        action: show_source,
    });
    application.receive_until("suggested source", |event| {
        matches!(
            event,
            AppEvent::SourceLoaded { request_id, .. } if *request_id == source_request_id
        )
    });
    assert_eq!(model.requests().len(), model_calls_before_source);

    let invalid_request_id = RequestId::from_stable_parts(&["actions", "invalid"]);
    application.send(AppCommand::RunSuggestedAction {
        request_id: invalid_request_id,
        session_id,
        repository_id,
        answer_id: answer.id,
        action: SuggestedAction::ShowSource {
            evidence_id: EvidenceId::from_stable_parts(&["actions", "unknown-evidence"]),
        },
    });
    application.receive_until("invalid suggested action", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(request_id),
                error,
            } if *request_id == invalid_request_id && error.code == "invalid_suggested_action"
        )
    });
    assert_eq!(model.requests().len(), model_calls_before_source);

    let intervening_request_id = RequestId::from_stable_parts(&["actions", "intervening"]);
    let intervening_profile =
        ExplanationProfile::new(ExplanationAudience::Expert, ExplanationDepth::Detail);
    application.send(AppCommand::Ask {
        request_id: intervening_request_id,
        session_id,
        repository_id,
        question: "Explain the entry point one more time.".to_owned(),
        profile: intervening_profile,
    });
    application.receive_until("intervening answer", |event| {
        matches!(
            event,
            AppEvent::AnswerCompleted { request_id, .. } if *request_id == intervening_request_id
        )
    });

    let change_depth = answer
        .suggested_actions
        .iter()
        .copied()
        .find(|action| {
            matches!(
                action,
                SuggestedAction::ChangeDepth {
                    depth: ExplanationDepth::Architecture
                }
            )
        })
        .expect("overview answer should offer architecture depth");
    let follow_up_request_id = RequestId::from_stable_parts(&["actions", "follow-up"]);
    application.send(AppCommand::RunSuggestedAction {
        request_id: follow_up_request_id,
        session_id,
        repository_id,
        answer_id: answer.id,
        action: change_depth,
    });
    application.receive_until("suggested follow-up", |event| {
        matches!(
            event,
            AppEvent::AnswerCompleted { request_id, .. } if *request_id == follow_up_request_id
        )
    });
    let expected_follow_up_profile = ExplanationProfile::new(
        ExplanationAudience::Beginner,
        ExplanationDepth::Architecture,
    );
    let expected_control_message = expected_follow_up_profile.control_message();
    assert_eq!(
        model
            .requests()
            .last()
            .expect("follow-up should call model")
            .messages
            .iter()
            .rev()
            .find(|message| message.role == ModelRole::System)
            .and_then(|message| message.content.as_deref()),
        Some(expected_control_message.as_str())
    );
    let requests = model.requests();
    let follow_up_user_message = requests
        .last()
        .expect("follow-up should call model")
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ModelRole::User)
        .and_then(|message| message.content.as_deref())
        .expect("follow-up should include a user message");
    assert!(follow_up_user_message.contains(&answer.id.to_string()));
    assert!(follow_up_user_message.contains(GROUNDED_TEXT));
    let tagged_target = format!("[CodeAtlas persisted answer {}]", answer.id);
    assert!(
        requests
            .last()
            .expect("follow-up should call model")
            .messages
            .iter()
            .any(|message| message
                .content
                .as_deref()
                .is_some_and(|content| content.starts_with(&tagged_target)))
    );

    application.shutdown();
    let restored = SessionStore::new(data_directory.join("sessions"))
        .load(session_id)
        .expect("action session should persist");
    assert_eq!(restored.turns.len(), 3);
    assert_eq!(restored.turns[0].profile, profile);
    assert_eq!(restored.turns[1].profile, intervening_profile);
    assert_eq!(restored.turns[2].profile, expected_follow_up_profile);
    assert_eq!(restored.turns[2].request_id, follow_up_request_id);
}

#[test]
fn explicit_session_history_is_recent_first_and_repository_scoped() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let first_repository_root = temporary.path().join("first-repository");
    let second_repository_root = temporary.path().join("second-repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&first_repository_root).expect("first repository should be created");
    fs::create_dir(&second_repository_root).expect("second repository should be created");
    fixture(&first_repository_root);
    fixture(&second_repository_root);

    let application = RunningApplication::spawn(
        config(data_directory.clone()),
        Arc::new(AdaptiveModel::default()),
    );
    let (first_repository_id, _) =
        index_repository(&application, &first_repository_root, "history-first");
    let (second_repository_id, _) =
        index_repository(&application, &second_repository_root, "history-second");
    let older_session_id = SessionId::from_stable_parts(&["history", "older"]);
    let recent_session_id = SessionId::from_stable_parts(&["history", "recent"]);
    let other_repository_session_id =
        SessionId::from_stable_parts(&["history", "other-repository"]);
    for (label, session_id, repository_id) in [
        ("older", older_session_id, first_repository_id),
        ("recent", recent_session_id, first_repository_id),
        (
            "other-repository",
            other_repository_session_id,
            second_repository_id,
        ),
    ] {
        ask(
            &application,
            RequestId::from_stable_parts(&["history", label, "ask"]),
            session_id,
            repository_id,
            FIRST_QUESTION,
        );
    }
    application.shutdown();

    let store = SessionStore::new(data_directory.join("sessions"));
    let mut older = store
        .load(older_session_id)
        .expect("older session should load");
    older.created_at_unix_ms = 100;
    older.updated_at_unix_ms = 200;
    store.save(&older).expect("older session should save");
    let mut recent = store
        .load(recent_session_id)
        .expect("recent session should load");
    recent.created_at_unix_ms = 150;
    recent.updated_at_unix_ms = 300;
    store.save(&recent).expect("recent session should save");
    let mut other = store
        .load(other_repository_session_id)
        .expect("other repository session should load");
    other.created_at_unix_ms = 175;
    other.updated_at_unix_ms = 300;
    store
        .save(&other)
        .expect("other repository session should save");

    let model = Arc::new(RejectingModel::default());
    let restarted = RunningApplication::spawn(config(data_directory), model.clone());
    let (_, _) = index_repository(&restarted, &first_repository_root, "history-restart-first");
    let first_index_request_id = RequestId::from_stable_parts(&["history-restart-first", "index"]);
    let first_list_request_id = RequestId::from_stable_parts(&["history", "list-first-repository"]);
    restarted.send(AppCommand::ListSessions {
        request_id: first_list_request_id,
        repository_id: Some(first_repository_id),
    });
    let first_list_events = restarted.receive_until("first repository session list", |event| {
        matches!(
            event,
            AppEvent::SessionsListed { request_id, .. } if *request_id == first_list_request_id
        )
    });
    assert_no_automatic_session_events(&first_list_events, first_index_request_id);
    let first_summaries = first_list_events
        .iter()
        .find_map(|event| match event {
            AppEvent::SessionsListed {
                request_id,
                sessions,
            } if *request_id == first_list_request_id => Some(sessions),
            _ => None,
        })
        .expect("first repository session list should be returned");
    assert_eq!(first_summaries.len(), 2);
    assert_eq!(first_summaries[0].session_id, recent_session_id);
    assert_eq!(first_summaries[0].created_at_unix_ms, 150);
    assert_eq!(first_summaries[0].updated_at_unix_ms, 300);
    assert_eq!(first_summaries[1].session_id, older_session_id);
    assert_eq!(first_summaries[1].created_at_unix_ms, 100);
    assert_eq!(first_summaries[1].updated_at_unix_ms, 200);
    assert!(
        first_summaries
            .iter()
            .all(|summary| summary.repository_id == first_repository_id)
    );

    let (_, _) = index_repository(
        &restarted,
        &second_repository_root,
        "history-restart-second",
    );
    let second_index_request_id =
        RequestId::from_stable_parts(&["history-restart-second", "index"]);
    let second_list_request_id =
        RequestId::from_stable_parts(&["history", "list-second-repository"]);
    restarted.send(AppCommand::ListSessions {
        request_id: second_list_request_id,
        repository_id: Some(second_repository_id),
    });
    let second_list_events = restarted.receive_until("second repository session list", |event| {
        matches!(
            event,
            AppEvent::SessionsListed { request_id, .. } if *request_id == second_list_request_id
        )
    });
    assert_no_automatic_session_events(&second_list_events, second_index_request_id);
    assert!(second_list_events.iter().any(|event| matches!(
        event,
        AppEvent::SessionsListed {
            request_id,
            sessions,
        } if *request_id == second_list_request_id
            && sessions.len() == 1
            && sessions[0].session_id == other_repository_session_id
            && sessions[0].repository_id == second_repository_id
    )));

    let all_list_request_id = RequestId::from_stable_parts(&["history", "list-all"]);
    restarted.send(AppCommand::ListSessions {
        request_id: all_list_request_id,
        repository_id: None,
    });
    let all_list_events = restarted.receive_until("all repository session list", |event| {
        matches!(
            event,
            AppEvent::SessionsListed { request_id, .. } if *request_id == all_list_request_id
        )
    });
    let all_summaries = all_list_events
        .iter()
        .find_map(|event| match event {
            AppEvent::SessionsListed {
                request_id,
                sessions,
            } if *request_id == all_list_request_id => Some(sessions),
            _ => None,
        })
        .expect("unfiltered session list should be returned");
    let mut equally_recent = [recent_session_id, other_repository_session_id];
    equally_recent.sort();
    assert_eq!(
        all_summaries
            .iter()
            .map(|summary| summary.session_id)
            .collect::<Vec<_>>(),
        vec![equally_recent[0], equally_recent[1], older_session_id]
    );

    let load_request_id = RequestId::from_stable_parts(&["history", "load-recent"]);
    restarted.send(AppCommand::LoadSession {
        request_id: load_request_id,
        session_id: recent_session_id,
    });
    let load_events = restarted.receive_until("explicit recent session load", |event| {
        matches!(
            event,
            AppEvent::SessionLoaded { request_id, .. } if *request_id == load_request_id
        )
    });
    assert!(load_events.iter().any(|event| matches!(
        event,
        AppEvent::SessionLoaded {
            request_id,
            session,
        } if *request_id == load_request_id
            && session.session_id == recent_session_id
            && session.repository_id == first_repository_id
            && session.created_at_unix_ms == 150
            && session.updated_at_unix_ms == 300
    )));

    let mismatch_request_id = RequestId::from_stable_parts(&["history", "repository-mismatch"]);
    let mismatch_events = ask_until_terminal(
        &restarted,
        mismatch_request_id,
        recent_session_id,
        second_repository_id,
        "This session must remain owned by its original repository.",
    );
    assert!(mismatch_events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(request_id),
            error,
        } if *request_id == mismatch_request_id
            && error.code == "session_mismatch"
            && error.message.contains(&first_repository_id.to_string())
            && error.message.contains(&second_repository_id.to_string())
            && !error.retryable
    )));
    assert_eq!(model.calls.load(Ordering::Relaxed), 0);
    restarted.shutdown();
}

#[test]
fn rejected_ask_and_cancel_emit_stable_application_errors() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let model = Arc::new(RejectingModel::default());
    let application_model: Arc<dyn ModelClient> = model.clone();
    let data_directory = temporary.path().join("data");
    let application = RunningApplication::spawn(config(data_directory.clone()), application_model);

    let ask_request_id = RequestId::from_stable_parts(&["errors", "ask"]);
    application.send(AppCommand::Ask {
        request_id: ask_request_id,
        session_id: SessionId::from_stable_parts(&["errors", "session"]),
        repository_id: RepositoryId::from_stable_parts(&["errors", "repository"]),
        question: "What is indexed?".to_owned(),
        profile: ExplanationProfile::default(),
    });
    let ask_events = application.receive_until("repository_not_indexed", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == ask_request_id && error.code == "repository_not_indexed"
        )
    });
    assert!(ask_events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == ask_request_id
            && error.code == "repository_not_indexed"
            && !error.retryable
    )));

    let cancel_request_id = RequestId::from_stable_parts(&["errors", "cancel"]);
    application.send(AppCommand::Cancel {
        request_id: cancel_request_id,
        target_request_id: RequestId::from_stable_parts(&["errors", "missing-request"]),
    });
    let cancel_events = application.receive_until("request_not_running", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == cancel_request_id && error.code == "request_not_running"
        )
    });
    assert!(cancel_events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == cancel_request_id
            && error.code == "request_not_running"
            && !error.retryable
    )));

    application.shutdown();
    assert_eq!(model.calls.load(Ordering::Relaxed), 0);
    let failed = SessionStore::new(data_directory.join("sessions"))
        .load(SessionId::from_stable_parts(&["errors", "session"]))
        .expect("rejected ask should be persisted");
    assert_eq!(failed.turns[0].status, SessionTaskStatus::Failed);
    assert_eq!(
        failed.turns[0]
            .terminal_error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("repository_not_indexed")
    );
}

#[test]
fn cancelling_an_ask_waiting_for_its_session_lock_is_prompt() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let (first_call_started, first_call_started_receiver) = std::sync::mpsc::channel();
    let model = Arc::new(GatedModel::new(first_call_started));
    let application_model: Arc<dyn ModelClient> = model.clone();
    let data_directory = temporary.path().join("data");
    let application = RunningApplication::spawn(config(data_directory.clone()), application_model);
    let (repository_id, _) = index_repository(&application, &repository_root, "lock-cancel");
    let session_id = SessionId::from_stable_parts(&["lock-cancel", "session"]);
    let first_request_id = RequestId::from_stable_parts(&["lock-cancel", "first"]);
    let waiting_request_id = RequestId::from_stable_parts(&["lock-cancel", "waiting"]);

    application.send(AppCommand::Ask {
        request_id: first_request_id,
        session_id,
        repository_id,
        question: FIRST_QUESTION.to_owned(),
        profile: ExplanationProfile::default(),
    });
    first_call_started_receiver
        .recv_timeout(EVENT_TIMEOUT)
        .expect("first ask should reach the model while holding the session lock");
    application.send(AppCommand::Ask {
        request_id: waiting_request_id,
        session_id,
        repository_id,
        question: FOLLOW_UP_QUESTION.to_owned(),
        profile: ExplanationProfile::default(),
    });
    application.send(AppCommand::Cancel {
        request_id: RequestId::from_stable_parts(&["lock-cancel", "cancel"]),
        target_request_id: waiting_request_id,
    });

    let cancelled = application.receive_until("waiting ask cancellation", |event| {
        matches!(
            event,
            AppEvent::Cancelled { request_id } if *request_id == waiting_request_id
        )
    });
    assert!(cancelled.iter().all(|event| !matches!(
        event,
        AppEvent::AnswerCompleted { request_id, .. } if *request_id == waiting_request_id
    )));
    assert_eq!(model.calls.load(Ordering::Relaxed), 1);

    model.release_first_call.notify_one();
    let completed = application.receive_until("first ask completion", |event| {
        matches!(
            event,
            AppEvent::AnswerCompleted { request_id, .. } if *request_id == first_request_id
        )
    });
    assert_eq!(
        completed
            .iter()
            .filter(|event| matches!(
                event,
                AppEvent::AnswerCompleted { request_id, .. } if *request_id == first_request_id
            ))
            .count(),
        1
    );
    application.shutdown();
    let persisted = SessionStore::new(data_directory.join("sessions"))
        .load(session_id)
        .expect("completed and cancelled tasks should persist");
    assert!(persisted.turns.iter().any(|turn| {
        turn.request_id == waiting_request_id && turn.status == SessionTaskStatus::Cancelled
    }));
}

#[test]
fn cancelling_an_active_model_call_records_its_terminal_ledger_before_cancellation() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);
    let (call_started, call_started_receiver) = std::sync::mpsc::channel();
    let model = Arc::new(GatedModel::new(call_started));
    let application = RunningApplication::spawn(config(data_directory.clone()), model);
    let (repository_id, _) = index_repository(&application, &repository_root, "active-cancel");
    let session_id = SessionId::from_stable_parts(&["active-cancel", "session"]);
    let request_id = RequestId::from_stable_parts(&["active-cancel", "ask"]);
    application.send(AppCommand::Ask {
        request_id,
        session_id,
        repository_id,
        question: FIRST_QUESTION.to_owned(),
        profile: ExplanationProfile::default(),
    });
    call_started_receiver
        .recv_timeout(EVENT_TIMEOUT)
        .expect("ask should reach the model");
    application.send(AppCommand::Cancel {
        request_id: RequestId::from_stable_parts(&["active-cancel", "cancel"]),
        target_request_id: request_id,
    });
    let events = application.receive_until("active ask cancellation", |event| {
        matches!(event, AppEvent::Cancelled { request_id: event_request } if *event_request == request_id)
    });
    let ledger = event_position(&events, |event| {
        matches!(
            event,
            AppEvent::ModelCallRecorded {
                request_id: event_request,
                record,
            } if *event_request == request_id
                && record.outcome == codeatlas_core::ModelCallOutcome::Cancelled
        )
    });
    let cancelled = event_position(
        &events,
        |event| matches!(event, AppEvent::Cancelled { request_id: event_request } if *event_request == request_id),
    );
    assert!(ledger < cancelled);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AppEvent::Cancelled { request_id: event_request } if *event_request == request_id))
            .count(),
        1
    );

    application.shutdown();
    let persisted = SessionStore::new(data_directory.join("sessions"))
        .load(session_id)
        .expect("cancelled task should be persisted");
    assert_eq!(persisted.turns[0].status, SessionTaskStatus::Cancelled);
    assert!(matches!(
        persisted.turns[0].model_calls[0].outcome,
        codeatlas_core::ModelCallOutcome::Cancelled
    ));
}

#[test]
fn session_lock_wait_reduces_runtime_timeout_and_persists_timed_out_ledger() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);
    let (lock_holder_started, lock_holder_started_receiver) = std::sync::mpsc::channel();
    let (timed_call_started, timed_call_started_receiver) = std::sync::mpsc::channel();
    let model = Arc::new(LockDelayTimeoutModel {
        lock_holder_started,
        timed_call_started,
        lock_hold: Duration::from_millis(500),
    });
    let mut application_config = config(data_directory.clone());
    application_config.agent.timeout = Duration::from_millis(800);
    let application = RunningApplication::spawn(application_config, model);
    let (repository_id, _) = index_repository(&application, &repository_root, "active-timeout");
    let session_id = SessionId::from_stable_parts(&["active-timeout", "session"]);
    let lock_holder_request = RequestId::from_stable_parts(&["active-timeout", "lock-holder"]);
    let request_id = RequestId::from_stable_parts(&["active-timeout", "ask"]);
    application.send(AppCommand::Ask {
        request_id: lock_holder_request,
        session_id,
        repository_id,
        question: FIRST_QUESTION.to_owned(),
        profile: ExplanationProfile::default(),
    });
    lock_holder_started_receiver
        .recv_timeout(EVENT_TIMEOUT)
        .expect("first ask should hold the session lock");
    let ask_started = Instant::now();
    application.send(AppCommand::Ask {
        request_id,
        session_id,
        repository_id,
        question: FOLLOW_UP_QUESTION.to_owned(),
        profile: ExplanationProfile::default(),
    });
    timed_call_started_receiver
        .recv_timeout(EVENT_TIMEOUT)
        .expect("waiting ask should reach the model after the lock is released");
    let model_started = Instant::now();
    assert!(ask_started.elapsed() >= Duration::from_millis(400));
    let events = application.receive_until("active model timeout", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request),
                error,
            } if *event_request == request_id && error.code == "runtime_timeout"
        )
    });
    let ledger = event_position(&events, |event| {
        matches!(
            event,
            AppEvent::ModelCallRecorded {
                request_id: event_request,
                record,
            } if *event_request == request_id
                && record.outcome == codeatlas_core::ModelCallOutcome::TimedOut
        )
    });
    let timeout = event_position(&events, |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request),
                error,
            } if *event_request == request_id && error.code == "runtime_timeout"
        )
    });
    assert!(ledger < timeout);
    assert!(model_started.elapsed() < Duration::from_millis(600));
    assert!(ask_started.elapsed() < Duration::from_millis(1_100));

    application.shutdown();
    let persisted = SessionStore::new(data_directory.join("sessions"))
        .load(session_id)
        .expect("timed-out task should be persisted");
    assert_eq!(persisted.turns.len(), 2);
    assert_eq!(persisted.turns[0].request_id, lock_holder_request);
    assert_eq!(persisted.turns[1].request_id, request_id);
    assert_eq!(persisted.turns[1].status, SessionTaskStatus::Failed);
    assert!(matches!(
        persisted.turns[1].model_calls[0].outcome,
        codeatlas_core::ModelCallOutcome::TimedOut
    ));
}

#[test]
fn token_budget_exceeded_task_persists_ledger_usage_and_reason() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);
    let mut application_config = config(data_directory.clone());
    application_config.agent.budget = ModelBudget {
        max_total_tokens: Some(1),
        max_cost: None,
    };
    let application =
        RunningApplication::spawn(application_config, Arc::new(AdaptiveModel::default()));
    let (repository_id, _) = index_repository(&application, &repository_root, "budget-history");
    let session_id = SessionId::from_stable_parts(&["budget-history", "session"]);
    let request_id = RequestId::from_stable_parts(&["budget-history", "ask"]);
    let events = ask_until_terminal(
        &application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::BudgetExceeded { request_id: event_request, .. }
            if *event_request == request_id
    )));
    application.shutdown();

    let persisted = SessionStore::new(data_directory.join("sessions"))
        .load(session_id)
        .expect("budget stop should persist");
    let task = &persisted.turns[0];
    assert_eq!(task.status, SessionTaskStatus::BudgetExceeded);
    assert!(task.answer.is_none());
    assert!(task.budget_stop_reason.is_some());
    assert_eq!(task.model_calls.len(), 1);
    assert_eq!(
        task.usage.as_ref().map(|usage| usage.tokens.total_tokens),
        Some(7)
    );
    assert_eq!(persisted.usage.tokens.total_tokens, 7);
}

#[test]
fn model_failure_persists_request_error_and_failed_call_record() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);
    let application = RunningApplication::spawn(
        config(data_directory.clone()),
        Arc::new(RejectingModel::default()),
    );
    let (repository_id, _) = index_repository(&application, &repository_root, "model-failure");
    let session_id = SessionId::from_stable_parts(&["model-failure", "session"]);
    let request_id = RequestId::from_stable_parts(&["model-failure", "ask"]);
    let events = ask_until_terminal(
        &application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request),
            error,
        } if *event_request == request_id && error.code == "model_error"
    )));
    application.shutdown();

    let persisted = SessionStore::new(data_directory.join("sessions"))
        .load(session_id)
        .expect("failed model task should persist");
    let task = &persisted.turns[0];
    assert_eq!(task.status, SessionTaskStatus::Failed);
    assert_eq!(task.model_calls.len(), 1);
    assert!(matches!(
        task.model_calls[0].outcome,
        codeatlas_core::ModelCallOutcome::Failed {
            retryable: false,
            ..
        }
    ));
    assert!(
        task.trajectory
            .iter()
            .any(|event| matches!(event, WorkflowEvent::ModelRequest { .. }))
    );
    assert!(
        task.trajectory
            .iter()
            .any(|event| matches!(event, WorkflowEvent::ModelError { .. }))
    );
    assert!(task.answer.is_none());
    assert!(task.terminal_error.is_some());
}

#[test]
fn mixed_currency_session_preserves_both_tasks_ledgers_and_transcripts() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);
    let pricing = |currency: &str| ModelPricing {
        currency: currency.to_owned(),
        input_per_million: 1.0,
        cached_input_per_million: None,
        output_per_million: 1.0,
    };
    let session_id = SessionId::from_stable_parts(&["currency-mismatch", "session"]);

    let mut first_config = config(data_directory.clone());
    first_config.agent.pricing = Some(pricing("EUR"));
    let first = RunningApplication::spawn(first_config, Arc::new(AdaptiveModel::default()));
    let (repository_id, _) = index_repository(&first, &repository_root, "currency-mismatch-first");
    let first_request_id = RequestId::from_stable_parts(&["currency-mismatch", "first"]);
    ask(
        &first,
        first_request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    first.shutdown();

    let mut second_config = config(data_directory.clone());
    second_config.agent.pricing = Some(pricing("USD"));
    let second = RunningApplication::spawn(second_config, Arc::new(AdaptiveModel::default()));
    let (reloaded_repository_id, _) =
        index_repository(&second, &repository_root, "currency-mismatch-second");
    assert_eq!(reloaded_repository_id, repository_id);
    let request_id = RequestId::from_stable_parts(&["currency-mismatch", "second"]);
    let (_, events) = ask(
        &second,
        request_id,
        session_id,
        repository_id,
        FOLLOW_UP_QUESTION,
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                AppEvent::AnswerCompleted {
                    request_id: event_request,
                    ..
                } if *event_request == request_id
            ))
            .count(),
        1
    );
    assert!(events.iter().all(|event| !matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request),
            ..
        } if *event_request == request_id
    )));

    second.shutdown();
    let persisted = SessionStore::new(data_directory.join("sessions"))
        .load(session_id)
        .expect("both mixed-currency tasks should persist");
    assert_eq!(persisted.turns.len(), 2);
    assert_eq!(persisted.turns[0].request_id, first_request_id);
    assert_eq!(persisted.turns[1].request_id, request_id);
    assert_eq!(persisted.usage.tokens.total_tokens, 27);
    assert!(persisted.usage.cost.is_none());
    assert_eq!(
        persisted.turns[0]
            .usage
            .as_ref()
            .and_then(|usage| usage.cost.as_ref())
            .map(|cost| cost.currency.as_str()),
        Some("EUR")
    );
    assert_eq!(
        persisted.turns[1]
            .usage
            .as_ref()
            .and_then(|usage| usage.cost.as_ref())
            .map(|cost| cost.currency.as_str()),
        Some("USD")
    );
    assert_eq!(persisted.turns[0].model_calls.len(), 2);
    assert_eq!(persisted.turns[1].model_calls.len(), 1);
    assert!(persisted.turns.iter().all(|turn| {
        !turn.continuation.is_empty() && turn.model_calls.iter().all(|record| record.cost.is_some())
    }));
}

#[test]
fn session_save_failure_still_publishes_answer_and_keeps_in_memory_history() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let application = RunningApplication::spawn(
        config(data_directory.clone()),
        Arc::new(AdaptiveModel::default()),
    );
    let (repository_id, _) = index_repository(&application, &repository_root, "save-failure");
    fs::write(data_directory.join("sessions"), b"not a directory")
        .expect("unsafe session path fixture should be written");
    let session_id = SessionId::from_stable_parts(&["save-failure", "session"]);
    let request_id = RequestId::from_stable_parts(&["save-failure", "ask"]);

    let mut events = ask_until_terminal(
        &application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    events.extend(application.receive_until("session_save_failed", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == request_id && error.code == "session_save_failed"
        )
    }));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                AppEvent::AnswerCompleted {
                    request_id: event_request_id,
                    ..
                } if *event_request_id == request_id
            ))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == request_id
            && error.code == "session_save_failed"
            && error.message.contains("remains available in memory")
            && error.message.contains("was not persisted")
            && error.retryable
    )));

    let answer = events
        .iter()
        .find_map(|event| match event {
            AppEvent::AnswerCompleted { answer, .. } => Some(answer),
            _ => None,
        })
        .expect("failed persistence should still publish the answer");
    let action = answer
        .suggested_actions
        .first()
        .copied()
        .expect("grounded answer should offer an action");
    let action_request_id = RequestId::from_stable_parts(&["save-failure", "action"]);
    application.send(AppCommand::RunSuggestedAction {
        request_id: action_request_id,
        session_id,
        repository_id,
        answer_id: answer.id,
        action,
    });
    application.receive_until("unpersisted action rejection", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == action_request_id
                && error.code == "invalid_suggested_action"
                && error.message.contains("was not persisted")
        )
    });

    let load_request_id = RequestId::from_stable_parts(&["save-failure", "load"]);
    application.send(AppCommand::LoadSession {
        request_id: load_request_id,
        session_id,
    });
    let loaded = application.receive_until("in-memory failed-save session", |event| {
        matches!(
            event,
            AppEvent::SessionLoaded { request_id, .. } if *request_id == load_request_id
        )
    });
    assert!(loaded.iter().any(|event| matches!(
        event,
        AppEvent::SessionLoaded {
            request_id,
            session,
        } if *request_id == load_request_id
            && session.tasks.len() == 1
            && session.tasks[0].question == FIRST_QUESTION
    )));
    assert!(data_directory.join("sessions").is_file());
    application.shutdown();
}

#[test]
fn indexed_source_range_loads_without_model_or_usage() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model = Arc::new(RejectingModel::default());
    let application_model: Arc<dyn ModelClient> = model.clone();
    let application = RunningApplication::spawn(config(data_directory.clone()), application_model);
    let (repository_id, _) = index_repository(&application, &repository_root, "source-load");
    let request_id = RequestId::from_stable_parts(&["source-load", "range"]);

    application.send(AppCommand::LoadSource {
        request_id,
        repository_id,
        path: RepositoryPath::new("src/lib.rs").expect("canonical source path"),
        start_line: 2,
        end_line: Some(3),
    });
    let events = application.receive_until("SourceLoaded", |event| {
        matches!(
            event,
            AppEvent::SourceLoaded {
                request_id: event_request_id,
                ..
            } if *event_request_id == request_id
        ) || matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                ..
            } if *event_request_id == request_id
        )
    });
    assert_no_request_error(&events, request_id);
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::SourceLoaded {
            request_id: event_request_id,
            repository_id: event_repository_id,
            path,
            start_line: 2,
            end_line: 3,
            content,
        } if *event_request_id == request_id
            && *event_repository_id == repository_id
            && path.as_str() == "src/lib.rs"
            && content == "    rust_helper();\n}\n"
    )));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AppEvent::UsageUpdated { .. }))
    );

    application.shutdown();
    assert_eq!(model.calls.load(Ordering::Relaxed), 0);
    assert!(!data_directory.join("sessions").exists());
}

#[test]
fn indexed_source_viewer_loads_ranges_beyond_model_payload_caps() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);
    let mut long_source = String::new();
    let padding = "x".repeat(96);
    for line in 1..=700 {
        writeln!(long_source, "// source line {line:04} {padding}")
            .expect("writing to a String should succeed");
    }
    assert!(long_source.len() > 64 * 1024);
    fs::write(repository_root.join("src/long.rs"), &long_source)
        .expect("long source fixture should be written");

    let model = Arc::new(RejectingModel::default());
    let application_model: Arc<dyn ModelClient> = model.clone();
    let application =
        RunningApplication::spawn(config(temporary.path().join("data")), application_model);
    let (repository_id, _) = index_repository(&application, &repository_root, "long-source-load");
    let request_id = RequestId::from_stable_parts(&["long-source-load", "range"]);

    application.send(AppCommand::LoadSource {
        request_id,
        repository_id,
        path: RepositoryPath::new("src/long.rs").expect("canonical source path"),
        start_line: 1,
        end_line: Some(700),
    });
    let events = application.receive_until("SourceLoaded", |event| {
        matches!(
            event,
            AppEvent::SourceLoaded {
                request_id: event_request_id,
                ..
            } if *event_request_id == request_id
        ) || matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                ..
            } if *event_request_id == request_id
        )
    });
    assert_no_request_error(&events, request_id);
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::SourceLoaded {
            request_id: event_request_id,
            start_line: 1,
            end_line: 700,
            content,
            ..
        } if *event_request_id == request_id && content == &long_source
    )));

    application.shutdown();
    assert_eq!(model.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn source_load_rejects_unindexed_repository_and_invalid_range() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model = Arc::new(RejectingModel::default());
    let application_model: Arc<dyn ModelClient> = model.clone();
    let application =
        RunningApplication::spawn(config(temporary.path().join("data")), application_model);
    let source_path = RepositoryPath::new("src/lib.rs").expect("canonical source path");
    let unindexed_request_id = RequestId::from_stable_parts(&["source-load", "unindexed"]);

    application.send(AppCommand::LoadSource {
        request_id: unindexed_request_id,
        repository_id: RepositoryId::from_stable_parts(&["source-load", "missing"]),
        path: source_path.clone(),
        start_line: 1,
        end_line: Some(1),
    });
    let unindexed_events = application.receive_until("repository_not_indexed", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == unindexed_request_id
                && error.code == "repository_not_indexed"
        )
    });
    assert!(unindexed_events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == unindexed_request_id
            && error.code == "repository_not_indexed"
            && !error.retryable
    )));

    let (repository_id, _) = index_repository(&application, &repository_root, "source-error");
    let invalid_request_id = RequestId::from_stable_parts(&["source-load", "invalid-range"]);
    application.send(AppCommand::LoadSource {
        request_id: invalid_request_id,
        repository_id,
        path: source_path,
        start_line: 3,
        end_line: Some(2),
    });
    let invalid_events = application.receive_until("source_load_failed", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == invalid_request_id && error.code == "source_load_failed"
        )
    });
    assert!(invalid_events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == invalid_request_id
            && error.code == "source_load_failed"
            && error.message == "invalid query: end_line 2 is before start_line 3"
            && !error.retryable
    )));
    assert!(
        unindexed_events
            .iter()
            .chain(&invalid_events)
            .all(|event| !matches!(event, AppEvent::UsageUpdated { .. }))
    );

    application.shutdown();
    assert_eq!(model.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn needed_diagram_writes_canonical_svg_and_persists_artifact() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new("Runtime flow"));
    let application = RunningApplication::spawn(config(data_directory.clone()), model);
    let (repository_id, _) = index_repository(&application, &repository_root, "diagram-needed");
    let session_id = SessionId::from_stable_parts(&["diagram-needed", "session"]);
    let request_id = RequestId::from_stable_parts(&["diagram-needed", "request"]);
    let (answer, _) = ask(
        &application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );

    let DiagramDecision::Needed { diagram, .. } = &answer.diagram else {
        panic!("answer should require a diagram");
    };
    let artifact = diagram
        .artifact
        .as_ref()
        .expect("needed diagram should contain an artifact");
    let mut identity_diagram = diagram.clone();
    identity_diagram.artifact = None;
    let canonical_json =
        serde_json::to_string(&identity_diagram).expect("diagram identity should serialize");
    let answer_id = answer.id.to_string();
    let expected_id =
        DiagramId::from_stable_parts(&[answer_id.as_str(), "svg-v1", canonical_json.as_str()]);
    assert_eq!(artifact.id, expected_id);
    assert_eq!(artifact.media_type, "image/svg+xml");

    let canonical_data =
        fs::canonicalize(&data_directory).expect("data directory should be canonicalizable");
    let expected_path = canonical_data
        .join("diagrams/v1")
        .join(answer.id.to_string())
        .join(format!("{}.svg", artifact.id));
    let canonical_artifact =
        fs::canonicalize(&expected_path).expect("artifact should exist at its trusted path");
    assert_eq!(Path::new(&artifact.path), canonical_artifact);
    assert!(canonical_artifact.starts_with(canonical_data.join("diagrams/v1")));

    let bytes = fs::read(&canonical_artifact).expect("SVG artifact should be readable");
    assert_eq!(
        artifact.byte_size,
        u64::try_from(bytes.len()).expect("SVG byte size should fit in u64")
    );
    let svg = String::from_utf8(bytes).expect("rendered SVG should be UTF-8");
    assert!(svg.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg "));
    assert!(svg.contains("Runtime flow"));
    assert!(svg.contains("rust_entry"));
    assert!(svg.contains("rust_helper"));

    let persisted = load_session_eventually(
        &SessionStore::new(data_directory.join("sessions")),
        session_id,
    );
    assert_eq!(persisted.turns.len(), 1);
    assert_eq!(persisted.turns[0].answer.as_ref(), Some(&answer));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for directory in [
            canonical_data.join("diagrams"),
            canonical_data.join("diagrams/v1"),
            canonical_data
                .join("diagrams/v1")
                .join(answer.id.to_string()),
        ] {
            let mode = fs::metadata(directory)
                .expect("diagram directory should have metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let mode = fs::metadata(&canonical_artifact)
            .expect("artifact should have metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    application.shutdown();
}

#[test]
fn not_needed_answer_does_not_create_diagram_directory() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model: Arc<dyn ModelClient> = Arc::new(AdaptiveModel::default());
    let application = RunningApplication::spawn(config(data_directory.clone()), model);
    let (repository_id, _) = index_repository(&application, &repository_root, "diagram-not-needed");
    let (answer, _) = ask(
        &application,
        RequestId::from_stable_parts(&["diagram-not-needed", "request"]),
        SessionId::from_stable_parts(&["diagram-not-needed", "session"]),
        repository_id,
        FIRST_QUESTION,
    );

    assert!(matches!(answer.diagram, DiagramDecision::NotNeeded { .. }));
    assert!(!data_directory.join("diagrams").exists());
    application.shutdown();
}

#[test]
fn malicious_diagram_title_is_escaped_in_stored_svg() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let title = "</title><script>leaked_source_body()</script>&";
    let model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new(title));
    let application = RunningApplication::spawn(config(data_directory), model);
    let (repository_id, _) = index_repository(&application, &repository_root, "diagram-escaping");
    let (answer, _) = ask(
        &application,
        RequestId::from_stable_parts(&["diagram-escaping", "request"]),
        SessionId::from_stable_parts(&["diagram-escaping", "session"]),
        repository_id,
        FIRST_QUESTION,
    );
    let DiagramDecision::Needed { diagram, .. } = &answer.diagram else {
        panic!("answer should require a diagram");
    };
    let artifact = diagram
        .artifact
        .as_ref()
        .expect("needed diagram should contain an artifact");
    let svg = fs::read_to_string(&artifact.path).expect("stored SVG should be UTF-8");

    assert!(!svg.contains("<script>"));
    assert!(svg.contains("&lt;/title&gt;&lt;script&gt;leaked_source_body()&lt;/script&gt;&amp;"));
    application.shutdown();
}

#[test]
fn data_directory_inside_repository_rejects_index_before_writing() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = repository_root.join(".codeatlas-data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let model = Arc::new(RejectingModel::default());
    let application = RunningApplication::spawn(config(data_directory.clone()), model.clone());
    let request_id = RequestId::from_stable_parts(&["data-in-repo", "index"]);
    application.send(AppCommand::Index {
        request_id,
        repository_root: repository_root.to_string_lossy().into_owned(),
    });
    let events = application.receive_until("unsafe data directory rejection", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == request_id && error.code == "index_failed"
        )
    });
    let error = events
        .iter()
        .find_map(|event| match event {
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == request_id => Some(error),
            _ => None,
        })
        .expect("index should emit an error");

    assert_eq!(error.code, "index_failed");
    assert!(
        error
            .message
            .contains("must be outside analyzed repository")
    );
    assert!(!data_directory.exists());
    assert_eq!(model.calls.load(Ordering::Relaxed), 0);
    application.shutdown();
}

#[cfg(unix)]
#[test]
fn data_directory_symlink_into_repository_rejects_index() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let linked_data = temporary.path().join("linked-data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);
    symlink(&repository_root, &linked_data).expect("data directory symlink");

    let model = Arc::new(RejectingModel::default());
    let application = RunningApplication::spawn(config(linked_data), model);
    let request_id = RequestId::from_stable_parts(&["linked-data-in-repo", "index"]);
    application.send(AppCommand::Index {
        request_id,
        repository_root: repository_root.to_string_lossy().into_owned(),
    });
    let events = application.receive_until("symlinked data directory rejection", |event| {
        matches!(
            event,
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == request_id && error.code == "index_failed"
        )
    });
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Error { error, .. } if error.message.contains("must be outside analyzed repository")
    )));
    assert!(!repository_root.join("indexes").exists());
    assert!(!repository_root.join("sessions").exists());
    assert!(!repository_root.join("diagrams").exists());
    application.shutdown();
}

#[cfg(unix)]
#[test]
fn diagram_storage_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    let escaped_directory = temporary.path().join("escaped");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fs::create_dir(&data_directory).expect("data directory should be created");
    fs::create_dir(&escaped_directory).expect("escape directory should be created");
    fixture(&repository_root);
    symlink(&escaped_directory, data_directory.join("diagrams"))
        .expect("diagram symlink should be created");

    let model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new("Unsafe diagram"));
    let application = RunningApplication::spawn(config(data_directory.clone()), model);
    let (repository_id, _) = index_repository(&application, &repository_root, "diagram-symlink");
    let request_id = RequestId::from_stable_parts(&["diagram-symlink", "request"]);
    let events = ask_until_terminal(
        &application,
        request_id,
        SessionId::from_stable_parts(&["diagram-symlink", "session"]),
        repository_id,
        FIRST_QUESTION,
    );

    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Error {
            request_id: Some(event_request_id),
            error,
        } if *event_request_id == request_id
            && error.code == "diagram_generation_failed"
            && !error.retryable
    )));
    let answer = events
        .iter()
        .find_map(|event| match event {
            AppEvent::AnswerCompleted {
                request_id: event_request_id,
                answer,
            } if *event_request_id == request_id => Some(answer),
            _ => None,
        })
        .expect("valid answer should survive diagram storage rejection");
    let DiagramDecision::Needed { diagram, .. } = &answer.diagram else {
        panic!("degraded answer should preserve the diagram decision");
    };
    assert!(diagram.artifact.is_none());
    assert!(
        fs::read_dir(&escaped_directory)
            .expect("escape directory should remain readable")
            .next()
            .is_none()
    );
    let persisted = load_session_eventually(
        &SessionStore::new(data_directory.join("sessions")),
        SessionId::from_stable_parts(&["diagram-symlink", "session"]),
    );
    assert_eq!(persisted.turns[0].answer.as_ref(), Some(answer));
    application.shutdown();
}

#[test]
fn identical_answer_and_diagram_reuse_the_same_artifact() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let session_id = SessionId::from_stable_parts(&["diagram-reuse", "session"]);
    let request_id = RequestId::from_stable_parts(&["diagram-reuse", "request"]);
    let first_model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new("Reusable diagram"));
    let first_application = RunningApplication::spawn(config(data_directory.clone()), first_model);
    let (repository_id, _) =
        index_repository(&first_application, &repository_root, "diagram-reuse-first");
    let (first_answer, _) = ask(
        &first_application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    let DiagramDecision::Needed {
        diagram: first_diagram,
        ..
    } = &first_answer.diagram
    else {
        panic!("first answer should require a diagram");
    };
    let first_artifact = first_diagram
        .artifact
        .clone()
        .expect("first answer should contain an artifact");
    let first_bytes = fs::read(&first_artifact.path).expect("first artifact should be readable");
    #[cfg(unix)]
    let first_inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&first_artifact.path)
            .expect("first artifact should have metadata")
            .ino()
    };
    first_application.shutdown();

    let session_store = SessionStore::new(data_directory.join("sessions"));
    fs::remove_file(session_store.session_path(session_id))
        .expect("persisted session should be removed before replay");
    let second_model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new("Reusable diagram"));
    let second_application =
        RunningApplication::spawn(config(data_directory.clone()), second_model);
    let (reloaded_repository_id, _) = index_repository(
        &second_application,
        &repository_root,
        "diagram-reuse-second",
    );
    assert_eq!(reloaded_repository_id, repository_id);
    let (second_answer, _) = ask(
        &second_application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    let DiagramDecision::Needed {
        diagram: second_diagram,
        ..
    } = &second_answer.diagram
    else {
        panic!("replayed answer should require a diagram");
    };
    let second_artifact = second_diagram
        .artifact
        .as_ref()
        .expect("replayed answer should contain an artifact");

    assert_eq!(second_answer.id, first_answer.id);
    assert_eq!(second_artifact, &first_artifact);
    assert_eq!(
        fs::read(&second_artifact.path).expect("reused artifact should be readable"),
        first_bytes
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let second_inode = fs::metadata(&second_artifact.path)
            .expect("reused artifact should have metadata")
            .ino();
        assert_eq!(second_inode, first_inode);
    }
    second_application.shutdown();
}

#[test]
fn conflicting_existing_artifact_degrades_without_overwrite() {
    let temporary = TempDir::new().expect("temporary workspace should be created");
    let repository_root = temporary.path().join("repository");
    let data_directory = temporary.path().join("data");
    fs::create_dir(&repository_root).expect("fixture repository should be created");
    fixture(&repository_root);

    let session_id = SessionId::from_stable_parts(&["diagram-conflict", "session"]);
    let request_id = RequestId::from_stable_parts(&["diagram-conflict", "request"]);
    let first_model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new("Conflict diagram"));
    let first_application = RunningApplication::spawn(config(data_directory.clone()), first_model);
    let (repository_id, _) = index_repository(
        &first_application,
        &repository_root,
        "diagram-conflict-first",
    );
    let (first_answer, _) = ask(
        &first_application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    let DiagramDecision::Needed { diagram, .. } = &first_answer.diagram else {
        panic!("first answer should require a diagram");
    };
    let artifact_path = diagram
        .artifact
        .as_ref()
        .expect("first answer should contain an artifact")
        .path
        .clone();
    first_application.shutdown();

    let session_store = SessionStore::new(data_directory.join("sessions"));
    fs::remove_file(session_store.session_path(session_id))
        .expect("persisted session should be removed before replay");
    fs::write(&artifact_path, b"conflicting artifact bytes")
        .expect("artifact should be replaced with conflicting test bytes");

    let second_model: Arc<dyn ModelClient> = Arc::new(DiagramModel::new("Conflict diagram"));
    let second_application =
        RunningApplication::spawn(config(data_directory.clone()), second_model);
    let (reloaded_repository_id, _) = index_repository(
        &second_application,
        &repository_root,
        "diagram-conflict-second",
    );
    assert_eq!(reloaded_repository_id, repository_id);
    let events = ask_until_terminal(
        &second_application,
        request_id,
        session_id,
        repository_id,
        FIRST_QUESTION,
    );
    let error = events
        .iter()
        .find_map(|event| match event {
            AppEvent::Error {
                request_id: Some(event_request_id),
                error,
            } if *event_request_id == request_id => Some(error),
            _ => None,
        })
        .expect("conflicting artifact should emit an error");

    assert_eq!(error.code, "diagram_generation_failed");
    assert!(
        error
            .message
            .starts_with("diagram artifact conflicts with existing content")
    );
    assert!(!error.retryable);
    let answer = events
        .iter()
        .find_map(|event| match event {
            AppEvent::AnswerCompleted {
                request_id: event_request_id,
                answer,
            } if *event_request_id == request_id => Some(answer),
            _ => None,
        })
        .expect("valid answer should survive an artifact conflict");
    let DiagramDecision::Needed { diagram, .. } = &answer.diagram else {
        panic!("degraded answer should preserve the diagram decision");
    };
    assert!(diagram.artifact.is_none());
    assert_eq!(
        fs::read(&artifact_path).expect("conflicting artifact should remain readable"),
        b"conflicting artifact bytes"
    );
    let persisted = load_session_eventually(&session_store, session_id);
    assert_eq!(persisted.turns[0].answer.as_ref(), Some(answer));
    second_application.shutdown();
}
