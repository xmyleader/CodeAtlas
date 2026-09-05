//! Application composition and background command dispatch for `CodeAtlas`.

pub mod credentials;
mod diagram_artifact;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
};

use codeatlas_agent::{
    AgentRequest, AgentRuntime, CancellationToken, EventSink, ModelClient, RuntimeConfig,
    SessionError, SessionState, SessionStore,
};
use codeatlas_core::{
    AgentAnswer, AppCommand, AppError, AppEvent, DiagramDecision, DiagramId, EntryPointKind,
    Progress, RepositoryEntryPoint, RepositoryId, RepositoryLanguage, RepositoryMap,
    RepositoryModule, RepositoryPath, RequestId, SessionContext, SessionId, SessionSummary,
    SessionTask, SessionTaskSummary, WorkflowEvent,
};
use codeatlas_indexer::{
    IndexReport, Indexer, JsonIndexStore, ParserRegistry, RepositorySpec, ScanConfig,
};
use codeatlas_lang_python::PythonParser;
use codeatlas_lang_rust::RustParser;
use codeatlas_query::{
    GetRepositoryOverviewQuery, ReadFileQuery, RepositoryIndex, RepositoryTools,
};
use serde_json::Value;
use thiserror::Error;
use tokio::{runtime, sync::mpsc as tokio_mpsc, task::JoinSet, time::Instant};

/// Non-secret settings for the long-running application service.
#[derive(Debug, Clone)]
pub struct ApplicationConfig {
    pub data_directory: PathBuf,
    pub scan: ScanConfig,
    pub agent: RuntimeConfig,
}

impl ApplicationConfig {
    #[must_use]
    pub fn new(data_directory: impl Into<PathBuf>) -> Self {
        Self {
            data_directory: data_directory.into(),
            scan: ScanConfig::default(),
            agent: RuntimeConfig::default(),
        }
    }
}

/// Standard-library channels consumed by any presentation layer.
#[derive(Debug)]
pub struct ApplicationChannels {
    pub commands: mpsc::Sender<AppCommand>,
    pub events: mpsc::Receiver<AppEvent>,
}

/// Owns the command bridge and Tokio worker threads.
#[derive(Debug)]
pub struct ApplicationRunner {
    bridge: thread::JoinHandle<()>,
    worker: thread::JoinHandle<Result<(), ApplicationWorkerError>>,
}

impl ApplicationRunner {
    /// Waits for a clean shutdown after all command senders have been dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if either application thread panicked or the Tokio
    /// worker could not run.
    pub fn join(self) -> Result<(), ApplicationJoinError> {
        self.bridge
            .join()
            .map_err(|_| ApplicationJoinError::BridgePanicked)?;
        self.worker
            .join()
            .map_err(|_| ApplicationJoinError::WorkerPanicked)??;
        Ok(())
    }
}

/// Starts the UI-neutral application service.
///
/// # Errors
///
/// Returns an error when persisted sessions are invalid or worker threads
/// cannot be created.
pub fn spawn_application(
    config: ApplicationConfig,
    model: Arc<dyn ModelClient>,
) -> Result<(ApplicationChannels, ApplicationRunner), ApplicationBuildError> {
    let session_store = SessionStore::new(config.data_directory.join("sessions"));
    let mut sessions = HashMap::new();
    for session in session_store.load_all()? {
        let session_id = session.session_id;
        if sessions.insert(session_id, session).is_some() {
            return Err(ApplicationBuildError::DuplicateSession { session_id });
        }
    }

    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let (async_command_sender, async_command_receiver) = tokio_mpsc::unbounded_channel();

    let bridge = thread::Builder::new()
        .name("codeatlas-command-bridge".to_owned())
        .spawn(move || {
            while let Ok(command) = command_receiver.recv() {
                if async_command_sender.send(command).is_err() {
                    break;
                }
            }
        })
        .map_err(ApplicationBuildError::ThreadSpawn)?;

    let state = Arc::new(SharedState {
        config,
        model,
        events: event_sender,
        session_store,
        repositories: Mutex::new(HashMap::new()),
        sessions: Mutex::new(sessions),
        session_locks: Mutex::new(HashMap::new()),
        cancellations: Mutex::new(HashMap::new()),
    });
    let worker_state = Arc::clone(&state);
    let worker = match thread::Builder::new()
        .name("codeatlas-runtime".to_owned())
        .spawn(move || {
            let runtime = runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("codeatlas-worker")
                .build()
                .map_err(ApplicationWorkerError::Runtime)?;
            runtime.block_on(command_loop(worker_state, async_command_receiver));
            Ok(())
        }) {
        Ok(worker) => worker,
        Err(error) => {
            drop(command_sender);
            let _ = bridge.join();
            return Err(ApplicationBuildError::ThreadSpawn(error));
        }
    };

    Ok((
        ApplicationChannels {
            commands: command_sender,
            events: event_receiver,
        },
        ApplicationRunner { bridge, worker },
    ))
}

async fn command_loop(
    state: Arc<SharedState>,
    mut commands: tokio_mpsc::UnboundedReceiver<AppCommand>,
) {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                dispatch_command(&state, command, &mut tasks);
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = completed {
                    state.emit_error(None, "task_panicked", error.to_string(), false);
                }
            }
        }
    }

    state.cancel_all();
    while let Some(completed) = tasks.join_next().await {
        if let Err(error) = completed {
            state.emit_error(None, "task_panicked", error.to_string(), false);
        }
    }
}

fn dispatch_command(state: &Arc<SharedState>, command: AppCommand, tasks: &mut JoinSet<()>) {
    match command {
        AppCommand::Index {
            request_id,
            repository_root,
        } => {
            let Some(cancellation) = state.register_request(request_id) else {
                return;
            };
            let task_state = Arc::clone(state);
            tasks.spawn(async move {
                handle_index(
                    Arc::clone(&task_state),
                    request_id,
                    repository_root,
                    cancellation,
                )
                .await;
                task_state.finish_request(request_id);
            });
        }
        AppCommand::Ask {
            request_id,
            session_id,
            repository_id,
            question,
        } => {
            let Some(cancellation) = state.register_request(request_id) else {
                return;
            };
            let task_state = Arc::clone(state);
            tasks.spawn(async move {
                handle_ask(
                    Arc::clone(&task_state),
                    request_id,
                    session_id,
                    repository_id,
                    question,
                    cancellation,
                )
                .await;
                task_state.finish_request(request_id);
            });
        }
        AppCommand::LoadSource {
            request_id,
            repository_id,
            path,
            start_line,
            end_line,
        } => {
            let task_state = Arc::clone(state);
            tasks.spawn(async move {
                handle_load_source(
                    task_state,
                    request_id,
                    repository_id,
                    path,
                    start_line,
                    end_line,
                )
                .await;
            });
        }
        AppCommand::Cancel {
            request_id,
            target_request_id,
        } => {
            if !state.cancel(target_request_id) {
                state.emit_error(
                    Some(request_id),
                    "request_not_running",
                    format!("request {target_request_id} is not running"),
                    false,
                );
            }
        }
        AppCommand::ListSessions {
            request_id,
            repository_id,
        } => emit_session_list(state, request_id, repository_id),
        AppCommand::LoadSession {
            request_id,
            session_id,
        } => emit_session(state, request_id, session_id),
        AppCommand::OpenDiagram {
            request_id,
            diagram_id,
        } => {
            let task_state = Arc::clone(state);
            tasks.spawn(async move {
                handle_open_diagram(task_state, request_id, diagram_id).await;
            });
        }
    }
}

async fn handle_open_diagram(
    state: Arc<SharedState>,
    request_id: RequestId,
    diagram_id: DiagramId,
) {
    let diagram = lock(&state.sessions)
        .values()
        .flat_map(|session| &session.turns)
        .find_map(|turn| match &turn.answer.diagram {
            DiagramDecision::Needed { diagram, .. } => diagram
                .artifact
                .as_ref()
                .filter(|artifact| artifact.id == diagram_id)
                .map(|_| diagram.clone()),
            DiagramDecision::NotNeeded { .. } => None,
        });
    let Some(diagram) = diagram else {
        state.emit_error(
            Some(request_id),
            "diagram_artifact_not_found",
            format!("diagram artifact {diagram_id} is not present in a saved session"),
            false,
        );
        return;
    };
    let data_directory = state.config.data_directory.clone();
    let opened = tokio::task::spawn_blocking(move || {
        diagram_artifact::open_in_default_viewer(&data_directory, &diagram)
    })
    .await;
    match opened {
        Ok(Ok(())) => state.emit(AppEvent::DiagramOpened {
            request_id,
            diagram_id,
        }),
        Ok(Err(error)) => state.emit_error(
            Some(request_id),
            "diagram_open_failed",
            error.to_string(),
            false,
        ),
        Err(error) => state.emit_error(
            Some(request_id),
            "diagram_open_failed",
            format!("diagram viewer task failed: {error}"),
            false,
        ),
    }
}

async fn handle_index(
    state: Arc<SharedState>,
    request_id: RequestId,
    repository_root: String,
    cancellation: CancellationToken,
) {
    let blocking_state = Arc::clone(&state);
    let blocking_cancellation = cancellation.clone();
    let result = tokio::task::spawn_blocking(move || {
        index_repository(
            &blocking_state,
            request_id,
            &repository_root,
            &blocking_cancellation,
        )
    })
    .await;

    if cancellation.is_cancelled() {
        state.emit(AppEvent::Cancelled { request_id });
        return;
    }
    let indexed = match result {
        Ok(Ok(indexed)) => indexed,
        Ok(Err(error)) => {
            state.emit_error(Some(request_id), "index_failed", error, true);
            return;
        }
        Err(error) => {
            state.emit_error(
                Some(request_id),
                "index_task_failed",
                error.to_string(),
                false,
            );
            return;
        }
    };

    let repository_id = indexed.repository_id;
    let file_count = indexed.file_count;
    let symbol_count = indexed.symbol_count;
    let repository_map = indexed.repository_map.clone();
    lock(&state.repositories).insert(repository_id, Arc::new(indexed.context));
    state.emit(AppEvent::IndexCompleted {
        request_id,
        repository_id,
        file_count,
        symbol_count,
        repository_map,
    });
}

fn index_repository(
    state: &SharedState,
    request_id: RequestId,
    requested_root: &str,
    cancellation: &CancellationToken,
) -> Result<IndexedRepository, String> {
    let root = std::fs::canonicalize(requested_root)
        .map_err(|error| format!("cannot resolve repository root {requested_root:?}: {error}"))?;
    if !root.is_dir() {
        return Err(format!(
            "repository root is not a directory: {}",
            root.display()
        ));
    }
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("repository")
        .to_owned();
    let stable_key = root.to_string_lossy().into_owned();

    let mut parsers = ParserRegistry::new();
    parsers.register(Arc::new(RustParser::new()));
    parsers.register(Arc::new(PythonParser::new()));
    let indexer = Indexer::new(state.config.scan.clone(), parsers)
        .with_store(JsonIndexStore::new(
            state.config.data_directory.join("indexes"),
        ))
        .with_cache_revision("rust-python-mvp-v1");
    let events = state.events.clone();
    let progress = |progress: Progress| {
        if !cancellation.is_cancelled() {
            let _ = events.send(AppEvent::Progress {
                request_id,
                progress,
            });
        }
    };
    let report = indexer
        .index(
            &root,
            RepositorySpec::new(name, stable_key),
            Some(&progress),
        )
        .map_err(|error| error.to_string())?;
    if cancellation.is_cancelled() {
        return Err("index request was cancelled".to_owned());
    }
    build_repository_context(&root, report)
}

fn build_repository_context(root: &Path, report: IndexReport) -> Result<IndexedRepository, String> {
    let repository_id = report.model.repository_id;
    let file_count = u64::try_from(report.model.files.len()).unwrap_or(u64::MAX);
    let symbol_count = u64::try_from(report.model.symbols.len()).unwrap_or(u64::MAX);
    let diagnostic_count = report.diagnostics.len();
    let index = RepositoryIndex::new(root, report.model).map_err(|error| error.to_string())?;
    let overview = index
        .get_repository_overview(&GetRepositoryOverviewQuery {
            limit: Some(50),
            include_tests: Some(false),
        })
        .map_err(|error| error.to_string())?
        .data;
    let mut seen_workspace_crates = HashSet::new();
    let modules = overview
        .modules
        .into_iter()
        .filter(|module| {
            module
                .path
                .as_str()
                .strip_prefix("crates/")
                .and_then(|path| path.split('/').next())
                .is_none_or(|name| seen_workspace_crates.insert(name.to_owned()))
        })
        .take(6)
        .map(|module| {
            let workspace_crate = module
                .path
                .as_str()
                .strip_prefix("crates/")
                .and_then(|path| path.split('/').next())
                .map(str::to_owned);
            RepositoryModule {
                name: workspace_crate.unwrap_or(module.name),
                path: module.path,
            }
        })
        .collect::<Vec<_>>();
    let modules_truncated = modules.len() < overview.counts.modules;
    let mut overview_entry_points = overview.entry_points;
    overview_entry_points.sort_by_key(|entry| entry_point_priority(&entry.kind));
    let entry_points = overview_entry_points
        .into_iter()
        .filter(|entry| {
            !entry
                .path
                .as_str()
                .split('/')
                .any(|part| matches!(part, "test" | "tests" | "fixtures" | "benches"))
        })
        .take(6)
        .map(|entry| RepositoryEntryPoint {
            kind: entry.kind,
            label: entry.label,
            path: entry.path,
            line: entry.span.start().line(),
        })
        .collect::<Vec<_>>();
    let entry_points_truncated = entry_points.len() < overview.counts.entry_points;
    let repository_map = RepositoryMap {
        name: overview.name,
        module_count: usize_to_u64(overview.counts.modules),
        call_count: usize_to_u64(overview.counts.calls),
        unresolved_call_count: usize_to_u64(overview.counts.unresolved_calls),
        languages: overview
            .languages
            .into_iter()
            .map(|language| RepositoryLanguage {
                language: language.language,
                file_count: usize_to_u64(language.files),
            })
            .collect(),
        modules,
        modules_truncated,
        entry_points,
        entry_points_truncated,
    };
    Ok(IndexedRepository {
        repository_id,
        file_count,
        symbol_count,
        repository_map,
        context: RepositoryContext {
            tools: RepositoryTools::new(index),
            diagnostic_count,
        },
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the ask lifecycle is kept together so its cancellation and single-answer semantics remain visible"
)]
async fn handle_ask(
    state: Arc<SharedState>,
    request_id: RequestId,
    session_id: SessionId,
    repository_id: RepositoryId,
    question: String,
    cancellation: CancellationToken,
) {
    let timeout = state.config.agent.timeout;
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        state.emit_error(
            Some(request_id),
            "invalid_deadline",
            "agent request deadline cannot be represented",
            false,
        );
        return;
    };
    let Some(repository) = lock(&state.repositories).get(&repository_id).cloned() else {
        state.emit_error(
            Some(request_id),
            "repository_not_indexed",
            format!("repository {repository_id} is not indexed in this process"),
            false,
        );
        return;
    };

    let executor = Arc::new(repository.tools.clone());
    let runtime = match AgentRuntime::new(
        Arc::clone(&state.model),
        executor,
        RepositoryTools::definitions(),
        state.config.agent.clone(),
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            state.emit_error(
                Some(request_id),
                "runtime_config_invalid",
                error.to_string(),
                false,
            );
            return;
        }
    };

    let session_lock = state.session_lock(session_id);
    let _session_guard = match await_ask_stage(session_lock.lock(), &cancellation, deadline).await {
        AskStageOutcome::Completed(guard) => guard,
        AskStageOutcome::Cancelled => return state.emit(AppEvent::Cancelled { request_id }),
        AskStageOutcome::TimedOut => {
            return emit_ask_timeout(&state, request_id, timeout, "waiting for the session lock");
        }
    };
    let Some(history) = load_session_history(&state, request_id, session_id, repository_id) else {
        return;
    };

    let sink = RecordingEventSink {
        request_id,
        events: state.events.clone(),
        workflow: Mutex::new(Vec::new()),
    };
    emit_repository_diagnostics(&sink, request_id, repository.diagnostic_count);

    let request = AgentRequest::new(request_id, session_id, repository_id, question.clone())
        .with_history(history);
    let mut answer = match await_ask_stage(
        runtime.run(request, cancellation.clone(), &sink),
        &cancellation,
        deadline,
    )
    .await
    {
        AskStageOutcome::Completed(Ok(answer)) => answer,
        AskStageOutcome::Completed(Err(_)) => return,
        AskStageOutcome::Cancelled => return state.emit(AppEvent::Cancelled { request_id }),
        AskStageOutcome::TimedOut => {
            return emit_ask_timeout(&state, request_id, timeout, "running the agent");
        }
    };

    match await_ask_stage(
        attach_diagram_artifact(
            state.config.data_directory.clone(),
            repository.tools.index().root().to_owned(),
            answer.clone(),
        ),
        &cancellation,
        deadline,
    )
    .await
    {
        AskStageOutcome::Completed(Ok(answer_with_artifact)) => answer = answer_with_artifact,
        AskStageOutcome::Completed(Err(message)) => {
            emit_diagram_generation_error(&state, request_id, &message);
        }
        AskStageOutcome::Cancelled => return state.emit(AppEvent::Cancelled { request_id }),
        AskStageOutcome::TimedOut => emit_diagram_generation_error(
            &state,
            request_id,
            &format!("diagram generation did not finish before the overall timeout of {timeout:?}"),
        ),
    }

    if cancellation.is_cancelled() {
        state.emit(AppEvent::Cancelled { request_id });
        return;
    }

    let mut updated = lock(&state.sessions)
        .get(&session_id)
        .cloned()
        .unwrap_or_else(|| SessionState::new(session_id, repository_id));
    if let Err(error) =
        updated.append_turn_with_workflow(request_id, question, answer.clone(), sink.workflow())
    {
        state.emit_error(
            Some(request_id),
            "session_update_failed",
            format!("answer was generated but was not added to the session or persisted: {error}"),
            false,
        );
        state.emit(AppEvent::AnswerCompleted { request_id, answer });
        return;
    }

    lock(&state.sessions).insert(session_id, updated.clone());
    state.emit(AppEvent::AnswerCompleted { request_id, answer });

    if Instant::now() >= deadline {
        emit_session_save_error(
            &state,
            request_id,
            format!(
                "answer remains available in memory but was not persisted because the overall timeout of {timeout:?} expired"
            ),
        );
        return;
    }

    let store = state.session_store.clone();
    let to_save = updated.clone();
    let save = tokio::task::spawn_blocking(move || store.save(&to_save));
    tokio::pin!(save);
    let save_result = tokio::select! {
        result = &mut save => Some(result),
        () = tokio::time::sleep_until(deadline) => None,
    };
    match save_result {
        Some(Ok(Ok(()))) => {}
        Some(Ok(Err(error))) => emit_session_save_error(
            &state,
            request_id,
            format!("answer remains available in memory but was not persisted: {error}"),
        ),
        Some(Err(error)) => emit_session_save_error(
            &state,
            request_id,
            format!(
                "answer remains available in memory but was not persisted because the session save task failed: {error}"
            ),
        ),
        None => emit_session_save_error(
            &state,
            request_id,
            format!(
                "answer remains available in memory, but persistence did not finish before the overall timeout of {timeout:?}"
            ),
        ),
    }
}

enum AskStageOutcome<T> {
    Completed(T),
    Cancelled,
    TimedOut,
}

async fn await_ask_stage<F>(
    future: F,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> AskStageOutcome<F::Output>
where
    F: Future,
{
    tokio::pin!(future);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => AskStageOutcome::Cancelled,
        result = &mut future => AskStageOutcome::Completed(result),
        () = tokio::time::sleep_until(deadline) => AskStageOutcome::TimedOut,
    }
}

fn emit_ask_timeout(
    state: &SharedState,
    request_id: RequestId,
    timeout: std::time::Duration,
    operation: &str,
) {
    state.emit_error(
        Some(request_id),
        "runtime_timeout",
        format!("agent request exceeded the overall timeout of {timeout:?} while {operation}"),
        true,
    );
}

fn load_session_history(
    state: &SharedState,
    request_id: RequestId,
    session_id: SessionId,
    repository_id: RepositoryId,
) -> Option<Vec<codeatlas_agent::ConversationMessage>> {
    if state.session_contains_request(session_id, request_id) {
        state.emit_error(
            Some(request_id),
            "request_id_conflict",
            format!("request {request_id} already exists in session {session_id}"),
            false,
        );
        return None;
    }
    match state.session_history(session_id, repository_id) {
        Ok(history) => Some(history),
        Err(message) => {
            state.emit_error(Some(request_id), "session_mismatch", message, false);
            None
        }
    }
}

fn emit_repository_diagnostics(events: &impl EventSink, request_id: RequestId, count: usize) {
    if count > 0 {
        events.emit(AppEvent::Progress {
            request_id,
            progress: Progress {
                phase: codeatlas_core::ProgressPhase::Searching,
                message: format!("using index with {count} non-fatal diagnostics"),
                completed: None,
                total: None,
            },
        });
    }
}

fn emit_diagram_generation_error(state: &SharedState, request_id: RequestId, message: &str) {
    state.emit_error(
        Some(request_id),
        "diagram_generation_failed",
        format!("{message}; answer will be delivered without a diagram artifact"),
        false,
    );
}

fn emit_session_save_error(state: &SharedState, request_id: RequestId, message: String) {
    state.emit_error(Some(request_id), "session_save_failed", message, true);
}

async fn attach_diagram_artifact(
    data_directory: PathBuf,
    repository_root: PathBuf,
    mut answer: AgentAnswer,
) -> Result<AgentAnswer, String> {
    if !matches!(
        answer.diagram,
        codeatlas_core::DiagramDecision::Needed { .. }
    ) {
        return Ok(answer);
    }
    match tokio::task::spawn_blocking(move || {
        diagram_artifact::validate_storage_root(&data_directory, &repository_root)?;
        diagram_artifact::render_and_store(&data_directory, &repository_root, &mut answer)?;
        Ok::<_, diagram_artifact::DiagramGenerationError>(answer)
    })
    .await
    {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err("diagram generation task failed".to_owned()),
    }
}

async fn handle_load_source(
    state: Arc<SharedState>,
    request_id: RequestId,
    repository_id: RepositoryId,
    path: RepositoryPath,
    start_line: u32,
    end_line: Option<u32>,
) {
    let Some(repository) = lock(&state.repositories).get(&repository_id).cloned() else {
        state.emit_error(
            Some(request_id),
            "repository_not_indexed",
            format!("repository {repository_id} is not indexed in this process"),
            false,
        );
        return;
    };

    let result = tokio::task::spawn_blocking(move || {
        repository.tools.index().read_source_range(&ReadFileQuery {
            path,
            start_line: Some(start_line),
            end_line,
        })
    })
    .await;
    let source = match result {
        Ok(Ok(source)) => source,
        Ok(Err(error)) => {
            state.emit_error(
                Some(request_id),
                "source_load_failed",
                error.to_string(),
                false,
            );
            return;
        }
        Err(error) => {
            state.emit_error(
                Some(request_id),
                "source_load_failed",
                error.to_string(),
                false,
            );
            return;
        }
    };

    state.emit(AppEvent::SourceLoaded {
        request_id,
        repository_id,
        path: source.path,
        start_line: source.start_line,
        end_line: source.end_line,
        content: source.content,
    });
}

struct SharedState {
    config: ApplicationConfig,
    model: Arc<dyn ModelClient>,
    events: mpsc::Sender<AppEvent>,
    session_store: SessionStore,
    repositories: Mutex<HashMap<RepositoryId, Arc<RepositoryContext>>>,
    sessions: Mutex<HashMap<SessionId, SessionState>>,
    session_locks: Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
    cancellations: Mutex<HashMap<RequestId, CancellationToken>>,
}

impl SharedState {
    fn emit(&self, event: AppEvent) {
        let _ = self.events.send(event);
    }

    fn emit_error(
        &self,
        request_id: Option<RequestId>,
        code: &str,
        message: impl Into<String>,
        retryable: bool,
    ) {
        self.emit(AppEvent::Error {
            request_id,
            error: AppError {
                code: code.to_owned(),
                message: message.into(),
                retryable,
            },
        });
    }

    fn register_request(&self, request_id: RequestId) -> Option<CancellationToken> {
        let mut requests = lock(&self.cancellations);
        if requests.contains_key(&request_id) {
            drop(requests);
            self.emit_error(
                Some(request_id),
                "duplicate_request",
                format!("request {request_id} is already running"),
                false,
            );
            return None;
        }
        let cancellation = CancellationToken::new();
        requests.insert(request_id, cancellation.clone());
        Some(cancellation)
    }

    fn finish_request(&self, request_id: RequestId) {
        lock(&self.cancellations).remove(&request_id);
    }

    fn cancel(&self, request_id: RequestId) -> bool {
        let cancellation = lock(&self.cancellations).get(&request_id).cloned();
        cancellation.is_some_and(|cancellation| {
            cancellation.cancel();
            true
        })
    }

    fn cancel_all(&self) {
        for cancellation in lock(&self.cancellations).values() {
            cancellation.cancel();
        }
    }

    fn session_lock(&self, session_id: SessionId) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.session_locks)
            .entry(session_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn session_history(
        &self,
        session_id: SessionId,
        repository_id: RepositoryId,
    ) -> Result<Vec<codeatlas_agent::ConversationMessage>, String> {
        let sessions = lock(&self.sessions);
        let Some(session) = sessions.get(&session_id) else {
            return Ok(Vec::new());
        };
        if session.repository_id != repository_id {
            return Err(format!(
                "session {session_id} belongs to repository {}, not {repository_id}",
                session.repository_id
            ));
        }
        Ok(session.conversation_history())
    }

    fn session_contains_request(&self, session_id: SessionId, request_id: RequestId) -> bool {
        lock(&self.sessions)
            .get(&session_id)
            .is_some_and(|session| {
                session
                    .turns
                    .iter()
                    .any(|turn| turn.request_id == request_id)
            })
    }

    fn session_contexts(&self, repository_id: Option<RepositoryId>) -> Vec<SessionState> {
        let mut sessions = lock(&self.sessions)
            .values()
            .filter(|session| repository_id.is_none_or(|id| session.repository_id == id))
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            right
                .updated_at_unix_ms
                .cmp(&left.updated_at_unix_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        sessions
    }
}

impl std::fmt::Debug for SharedState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedState")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct RepositoryContext {
    tools: RepositoryTools,
    diagnostic_count: usize,
}

#[derive(Debug)]
struct IndexedRepository {
    repository_id: RepositoryId,
    file_count: u64,
    symbol_count: u64,
    repository_map: RepositoryMap,
    context: RepositoryContext,
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

const fn entry_point_priority(kind: &EntryPointKind) -> u8 {
    match kind {
        EntryPointKind::Executable => 0,
        EntryPointKind::WebRoute => 1,
        EntryPointKind::BackgroundTask => 2,
        EntryPointKind::Library => 3,
        EntryPointKind::Other(_) => 4,
        EntryPointKind::Test | EntryPointKind::Benchmark => 5,
    }
}

const MAX_WORKFLOW_OUTPUT_CHARS: usize = 1_000;

struct RecordingEventSink {
    request_id: RequestId,
    events: mpsc::Sender<AppEvent>,
    workflow: Mutex<Vec<WorkflowEvent>>,
}

impl RecordingEventSink {
    fn workflow(&self) -> Vec<WorkflowEvent> {
        lock(&self.workflow).clone()
    }
}

impl EventSink for RecordingEventSink {
    fn emit(&self, event: AppEvent) {
        let workflow = match &event {
            AppEvent::Progress {
                request_id,
                progress,
            } if *request_id == self.request_id => Some(WorkflowEvent::Progress(progress.clone())),
            AppEvent::ToolCallStarted { request_id, call } if *request_id == self.request_id => {
                Some(WorkflowEvent::ToolCallStarted(call.clone()))
            }
            AppEvent::ToolCallCompleted { request_id, output }
                if *request_id == self.request_id =>
            {
                Some(WorkflowEvent::ToolCallCompleted {
                    call_id: output.call_id,
                    output: workflow_output_summary(&output.result),
                    is_error: output.is_error,
                })
            }
            AppEvent::EvidenceAdded { .. }
            | AppEvent::AnswerDelta { .. }
            | AppEvent::AnswerCompleted { .. }
            | AppEvent::UsageUpdated { .. }
            | AppEvent::IndexCompleted { .. }
            | AppEvent::SourceLoaded { .. }
            | AppEvent::Cancelled { .. }
            | AppEvent::SessionsListed { .. }
            | AppEvent::SessionLoaded { .. }
            | AppEvent::DiagramOpened { .. }
            | AppEvent::Error { .. }
            | AppEvent::Progress { .. }
            | AppEvent::ToolCallStarted { .. }
            | AppEvent::ToolCallCompleted { .. } => None,
        };
        if let Some(workflow) = workflow {
            lock(&self.workflow).push(workflow);
        }
        // The application publishes one final answer after optional diagram processing.
        if !matches!(event, AppEvent::AnswerCompleted { .. }) {
            let _ = self.events.send(event);
        }
    }
}

fn bounded_workflow_output(value: &str) -> String {
    let length = value.chars().count();
    if length <= MAX_WORKFLOW_OUTPUT_CHARS {
        return value.to_owned();
    }
    let preview = value
        .chars()
        .take(MAX_WORKFLOW_OUTPUT_CHARS)
        .collect::<String>();
    format!(
        "{preview}... [{} chars omitted]",
        length.saturating_sub(MAX_WORKFLOW_OUTPUT_CHARS)
    )
}

fn workflow_output_summary(value: &Value) -> String {
    bounded_workflow_output(&redact_workflow_source(value).to_string())
}

fn redact_workflow_source(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if matches!(key.as_str(), "content" | "excerpt" | "line")
                        && value.is_string()
                    {
                        Value::String("[source text omitted]".to_owned())
                    } else {
                        redact_workflow_source(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_workflow_source).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

fn emit_session_list(
    state: &SharedState,
    request_id: RequestId,
    repository_id: Option<RepositoryId>,
) {
    let sessions = state
        .session_contexts(repository_id)
        .iter()
        .map(|session| session_summary(&state.session_store, session))
        .collect();
    state.emit(AppEvent::SessionsListed {
        request_id,
        sessions,
    });
}

fn emit_session(state: &SharedState, request_id: RequestId, session_id: SessionId) {
    let session = lock(&state.sessions).get(&session_id).cloned();
    let Some(session) = session else {
        state.emit_error(
            Some(request_id),
            "session_not_found",
            format!("session {session_id} does not exist"),
            false,
        );
        return;
    };
    state.emit(AppEvent::SessionLoaded {
        request_id,
        session: session_context(&state.session_store, &session),
    });
}

fn session_summary(store: &SessionStore, session: &SessionState) -> SessionSummary {
    SessionSummary {
        session_id: session.session_id,
        repository_id: session.repository_id,
        created_at_unix_ms: session.created_at_unix_ms,
        updated_at_unix_ms: session.updated_at_unix_ms,
        tasks: session
            .turns
            .iter()
            .map(|turn| SessionTaskSummary {
                request_id: turn.request_id,
                question: turn.question.clone(),
            })
            .collect(),
        json_path: store.session_path(session.session_id).display().to_string(),
    }
}

fn session_context(store: &SessionStore, session: &SessionState) -> SessionContext {
    SessionContext {
        schema_version: session.schema_version,
        session_id: session.session_id,
        repository_id: session.repository_id,
        created_at_unix_ms: session.created_at_unix_ms,
        updated_at_unix_ms: session.updated_at_unix_ms,
        tasks: session
            .turns
            .iter()
            .map(|turn| SessionTask {
                request_id: turn.request_id,
                question: turn.question.clone(),
                answer: turn.answer.clone(),
                workflow: turn.workflow.clone(),
            })
            .collect(),
        usage: session.usage.clone(),
        json_path: store.session_path(session.session_id).display().to_string(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Error)]
pub enum ApplicationBuildError {
    #[error("failed to load persisted sessions: {0}")]
    Sessions(#[from] SessionError),
    #[error("persisted session {session_id} occurs more than once")]
    DuplicateSession { session_id: SessionId },
    #[error("failed to start application thread: {0}")]
    ThreadSpawn(#[source] io::Error),
}

#[derive(Debug, Error)]
pub enum ApplicationWorkerError {
    #[error("failed to create Tokio runtime: {0}")]
    Runtime(#[source] io::Error),
}

#[derive(Debug, Error)]
pub enum ApplicationJoinError {
    #[error("command bridge thread panicked")]
    BridgePanicked,
    #[error("application worker thread panicked")]
    WorkerPanicked,
    #[error(transparent)]
    Worker(#[from] ApplicationWorkerError),
}

#[must_use]
pub fn default_data_directory() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(path).join("codeatlas");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Path::new(&home).join(".local/share/codeatlas");
    }
    PathBuf::from(".codeatlas")
}
