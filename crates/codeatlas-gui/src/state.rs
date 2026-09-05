use std::collections::BTreeMap;

use codeatlas_core::{
    AgentAnswer, AppCommand, AppError, AppEvent, Claim, ClaimId, DiagramId, Evidence, EvidenceId,
    ModelUsage, Progress, ProgressPhase, RepositoryId, RepositoryMap, RepositoryPath, RequestId,
    SessionContext, SessionId, SessionSummary, TargetResolution, ToolCallId, WorkflowEvent,
};

const MAX_PROGRESS_ITEMS_PER_REQUEST: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspacePanel {
    Repository,
    Conversation,
    Evidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Index,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkStatus {
    Idle,
    Indexing,
    Thinking,
    Cancelling,
    Indexed,
    AnswerReady,
    Cancelled,
    Error,
}

impl WorkStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Ready to index",
            Self::Indexing => "Indexing repository",
            Self::Thinking => "Researching answer",
            Self::Cancelling => "Cancelling",
            Self::Indexed => "Repository ready",
            Self::AnswerReady => "Answer ready",
            Self::Cancelled => "Request cancelled",
            Self::Error => "Needs attention",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryView {
    pub path: String,
    pub id: RepositoryId,
    pub file_count: u64,
    pub symbol_count: u64,
    pub map: RepositoryMap,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnView {
    pub request_id: RequestId,
    pub question: String,
    pub answer_text: String,
    pub answer: Option<AgentAnswer>,
    pub status: TurnStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityStatus {
    Running,
    Complete,
    Cancelled,
    Failed,
    Information,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityItem {
    pub request_id: RequestId,
    pub call_id: Option<ToolCallId>,
    pub title: String,
    pub detail: Option<String>,
    pub status: ActivityStatus,
    progress_phase: Option<ProgressPhase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    Excerpt,
    Loading,
    Loaded,
    Failed,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceView {
    pub evidence_id: EvidenceId,
    pub path: RepositoryPath,
    pub start_line: u32,
    pub end_line: u32,
    pub content: String,
    pub status: SourceStatus,
    pub message: Option<String>,
    pending_request: Option<RequestId>,
    repository_id: Option<RepositoryId>,
}

#[derive(Debug, Clone)]
struct ActiveRequest {
    id: RequestId,
    kind: RequestKind,
    cancel_id: Option<RequestId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestOutcome {
    Completed,
    Cancelled,
    Failed,
}

/// Pure presentation state and reducer for the native GUI.
#[derive(Debug, Clone)]
pub struct GuiState {
    namespace: String,
    next_request: u64,
    next_session: u64,
    pub repository_input: String,
    pub question_input: String,
    pub repository: Option<RepositoryView>,
    pub status: WorkStatus,
    pub panel: WorkspacePanel,
    pub turns: Vec<TurnView>,
    pub selected_task: Option<RequestId>,
    pub selected_claim: Option<ClaimId>,
    pub selected_evidence: Option<EvidenceId>,
    pub source: Option<SourceView>,
    pub activity: Vec<ActivityItem>,
    pub activity_open: bool,
    pub history_open: bool,
    pub history_loading: bool,
    pub sessions: Vec<SessionSummary>,
    pub session_id: SessionId,
    pub session_repository_id: Option<RepositoryId>,
    pub error: Option<UiError>,
    pub notices: Vec<UiError>,
    diagram_messages: BTreeMap<DiagramId, String>,
    active: Option<ActiveRequest>,
    outcomes: BTreeMap<RequestId, RequestOutcome>,
    pending_index_path: Option<(RequestId, String)>,
    pending_history: Option<RequestId>,
    pending_session: Option<RequestId>,
    pending_diagrams: BTreeMap<RequestId, DiagramId>,
    usage: BTreeMap<RequestId, ModelUsage>,
}

impl GuiState {
    #[must_use]
    pub fn new(namespace: impl Into<String>) -> Self {
        let namespace = namespace.into();
        let session_id = SessionId::from_stable_parts(&["gui", &namespace, "unindexed"]);
        Self {
            namespace,
            next_request: 0,
            next_session: 0,
            repository_input: String::new(),
            question_input: String::new(),
            repository: None,
            status: WorkStatus::Idle,
            panel: WorkspacePanel::Conversation,
            turns: Vec::new(),
            selected_task: None,
            selected_claim: None,
            selected_evidence: None,
            source: None,
            activity: Vec::new(),
            activity_open: false,
            history_open: false,
            history_loading: false,
            sessions: Vec::new(),
            session_id,
            session_repository_id: None,
            error: None,
            notices: Vec::new(),
            diagram_messages: BTreeMap::new(),
            active: None,
            outcomes: BTreeMap::new(),
            pending_index_path: None,
            pending_history: None,
            pending_session: None,
            pending_diagrams: BTreeMap::new(),
            usage: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }

    #[must_use]
    pub fn can_ask(&self) -> bool {
        self.repository.is_some() && !self.is_read_only() && self.active.is_none()
    }

    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.session_repository_id.is_some()
            && self.repository.as_ref().map(|repository| repository.id)
                != self.session_repository_id
    }

    #[must_use]
    pub fn read_only_message(&self) -> Option<&'static str> {
        self.is_read_only().then_some(
            "This saved session belongs to another repository. It is available for reading, but cannot be continued or used to load local source until that repository is indexed.",
        )
    }

    #[must_use]
    pub fn selected_turn(&self) -> Option<&TurnView> {
        self.selected_task
            .and_then(|request_id| self.turns.iter().find(|turn| turn.request_id == request_id))
            .or_else(|| self.turns.last())
    }

    #[must_use]
    pub fn selected_answer(&self) -> Option<&AgentAnswer> {
        self.selected_turn().and_then(|turn| turn.answer.as_ref())
    }

    /// Usage for the selected request, including cumulative usage received
    /// before that request has produced its final answer.
    #[must_use]
    pub fn selected_usage(&self) -> Option<&ModelUsage> {
        let turn = self.selected_turn()?;
        self.usage.get(&turn.request_id).or_else(|| {
            turn.answer
                .as_ref()
                .and_then(|answer| answer.usage.as_ref())
        })
    }

    #[must_use]
    pub fn visible_claims(&self) -> &[Claim] {
        self.selected_answer()
            .map_or(&[], |answer| answer.claims.as_slice())
    }

    #[must_use]
    pub fn visible_evidence(&self) -> Vec<&Evidence> {
        let Some(answer) = self.selected_answer() else {
            return Vec::new();
        };
        let selected = self.selected_claim;
        answer
            .evidence
            .iter()
            .filter(|evidence| {
                selected.is_none_or(|claim_id| {
                    answer.claims.iter().any(|claim| {
                        claim.id == claim_id && claim.evidence_ids.contains(&evidence.id)
                    })
                })
            })
            .collect()
    }

    #[must_use]
    pub fn index_repository(&mut self) -> Option<AppCommand> {
        if self.active.is_some() {
            self.local_error(
                "busy",
                "Cancel the current work before indexing another repository.",
            );
            return None;
        }
        let path = self.repository_input.trim().to_owned();
        if path.is_empty() {
            self.local_error(
                "repository_path_required",
                "Choose a repository directory first.",
            );
            return None;
        }
        let request_id = self.next_request_id("index");
        self.active = Some(ActiveRequest {
            id: request_id,
            kind: RequestKind::Index,
            cancel_id: None,
        });
        self.pending_index_path = Some((request_id, path.clone()));
        self.status = WorkStatus::Indexing;
        self.error = None;
        Some(AppCommand::Index {
            request_id,
            repository_root: path,
        })
    }

    #[must_use]
    pub fn ask(&mut self) -> Option<AppCommand> {
        if self.active.is_some() {
            self.local_error(
                "busy",
                "Cancel the current work before asking another question.",
            );
            return None;
        }
        let Some(repository_id) = self.repository.as_ref().map(|repository| repository.id) else {
            self.local_error(
                "repository_not_indexed",
                "Index a repository before asking a question.",
            );
            return None;
        };
        if self.is_read_only() {
            self.local_error(
                "session_repository_mismatch",
                "This session is read-only because it belongs to another repository.",
            );
            return None;
        }
        let question = self.question_input.trim().to_owned();
        if question.is_empty() {
            self.local_error("question_required", "Enter a question about the codebase.");
            return None;
        }
        let request_id = self.next_request_id("ask");
        self.turns.push(TurnView {
            request_id,
            question: question.clone(),
            answer_text: String::new(),
            answer: None,
            status: TurnStatus::Running,
        });
        self.selected_task = Some(request_id);
        self.selected_claim = None;
        self.selected_evidence = None;
        self.source = None;
        self.question_input.clear();
        self.active = Some(ActiveRequest {
            id: request_id,
            kind: RequestKind::Ask,
            cancel_id: None,
        });
        self.status = WorkStatus::Thinking;
        self.error = None;
        Some(AppCommand::Ask {
            request_id,
            session_id: self.session_id,
            repository_id,
            question,
        })
    }

    #[must_use]
    pub fn cancel(&mut self) -> Option<AppCommand> {
        let target_request_id = self.active.as_ref()?.id;
        if self.active.as_ref()?.cancel_id.is_some() {
            return None;
        }
        let request_id = self.next_request_id("cancel");
        if let Some(active) = self.active.as_mut() {
            active.cancel_id = Some(request_id);
        }
        self.status = WorkStatus::Cancelling;
        Some(AppCommand::Cancel {
            request_id,
            target_request_id,
        })
    }

    #[must_use]
    pub fn list_sessions(&mut self) -> AppCommand {
        let request_id = self.next_request_id("sessions");
        self.pending_history = Some(request_id);
        self.history_open = true;
        self.history_loading = true;
        AppCommand::ListSessions {
            request_id,
            repository_id: None,
        }
    }

    #[must_use]
    pub fn load_session(&mut self, session_id: SessionId) -> Option<AppCommand> {
        if self.active.is_some() {
            self.local_error(
                "busy",
                "Cancel the current work before loading a saved session.",
            );
            return None;
        }
        let request_id = self.next_request_id("load-session");
        self.pending_session = Some(request_id);
        self.history_loading = true;
        Some(AppCommand::LoadSession {
            request_id,
            session_id,
        })
    }

    pub fn new_session(&mut self) {
        if self.active.is_some() {
            self.local_error(
                "busy",
                "Cancel the current work before starting a new session.",
            );
            return;
        }
        let Some(repository_id) = self.repository.as_ref().map(|repository| repository.id) else {
            self.local_error(
                "repository_not_indexed",
                "Index a repository before starting a session.",
            );
            return;
        };
        self.clear_session();
        self.session_id = self.fresh_session_id(repository_id);
        self.session_repository_id = Some(repository_id);
        self.status = WorkStatus::Indexed;
        self.history_open = false;
        self.error = None;
    }

    pub fn select_task(&mut self, request_id: RequestId) {
        if self.turns.iter().any(|turn| turn.request_id == request_id) {
            self.selected_task = Some(request_id);
            self.selected_claim = None;
            self.selected_evidence = None;
            self.source = None;
        }
    }

    pub fn select_claim(&mut self, claim_id: ClaimId) {
        self.selected_claim = (self.selected_claim != Some(claim_id)).then_some(claim_id);
        self.selected_evidence = None;
        self.source = None;
    }

    #[must_use]
    pub fn load_evidence(&mut self, evidence_id: EvidenceId) -> Option<AppCommand> {
        let evidence = self
            .selected_answer()?
            .evidence
            .iter()
            .find(|evidence| evidence.id == evidence_id)?
            .clone();
        let (start_line, end_line) = evidence_line_range(&evidence);
        self.selected_evidence = Some(evidence.id);
        let excerpt = evidence.excerpt.unwrap_or_default();
        let Some(repository_id) = self.repository.as_ref().map(|repository| repository.id) else {
            self.source = Some(SourceView {
                evidence_id,
                path: evidence.path,
                start_line,
                end_line,
                content: excerpt,
                status: SourceStatus::ReadOnly,
                message: Some(
                    "Index this session's repository to load source from disk.".to_owned(),
                ),
                pending_request: None,
                repository_id: None,
            });
            return None;
        };
        if self.session_repository_id != Some(repository_id) {
            self.source = Some(SourceView {
                evidence_id,
                path: evidence.path,
                start_line,
                end_line,
                content: excerpt,
                status: SourceStatus::ReadOnly,
                message: Some(
                    "Source loading is disabled for a cross-repository session.".to_owned(),
                ),
                pending_request: None,
                repository_id: Some(repository_id),
            });
            return None;
        }
        let request_id = self.next_request_id("source");
        self.source = Some(SourceView {
            evidence_id,
            path: evidence.path.clone(),
            start_line,
            end_line,
            content: excerpt,
            status: SourceStatus::Loading,
            message: None,
            pending_request: Some(request_id),
            repository_id: Some(repository_id),
        });
        Some(AppCommand::LoadSource {
            request_id,
            repository_id,
            path: evidence.path,
            start_line,
            end_line: Some(end_line),
        })
    }

    #[must_use]
    pub fn open_diagram(&mut self, diagram_id: DiagramId) -> AppCommand {
        let request_id = self.next_request_id("diagram");
        self.pending_diagrams.insert(request_id, diagram_id);
        self.diagram_messages.insert(
            diagram_id,
            "Opening diagram in the default viewer...".to_owned(),
        );
        AppCommand::OpenDiagram {
            request_id,
            diagram_id,
        }
    }

    #[must_use]
    pub fn diagram_message(&self, diagram_id: DiagramId) -> Option<&str> {
        self.diagram_messages.get(&diagram_id).map(String::as_str)
    }

    pub fn local_error(&mut self, code: &str, message: &str) {
        self.error = Some(UiError {
            code: code.to_owned(),
            message: message.to_owned(),
            retryable: false,
        });
    }

    /// Reduces one backend event without performing I/O.
    pub fn reduce(&mut self, event: AppEvent) {
        match event {
            AppEvent::Progress {
                request_id,
                progress,
            } => self.reduce_progress(request_id, progress),
            AppEvent::ToolCallStarted { request_id, call } => {
                if self.is_active(request_id) {
                    self.activity.push(ActivityItem {
                        request_id,
                        call_id: Some(call.id),
                        title: human_tool_name(&call.name),
                        detail: Some("Read-only repository query".to_owned()),
                        status: ActivityStatus::Running,
                        progress_phase: None,
                    });
                }
            }
            AppEvent::ToolCallCompleted { request_id, output } => {
                if self.is_active(request_id) {
                    self.complete_tool(request_id, output.call_id, output.is_error);
                }
            }
            // Candidate evidence is exploratory. Only AnswerCompleted carries verified final evidence.
            AppEvent::EvidenceAdded { .. } => {}
            AppEvent::AnswerDelta { request_id, delta } => {
                if self.active_is(request_id, RequestKind::Ask) {
                    if let Some(turn) = self.turn_mut(request_id) {
                        turn.answer_text.push_str(&delta);
                    }
                }
            }
            AppEvent::AnswerCompleted { request_id, answer } => {
                self.reduce_answer_completed(request_id, answer);
            }
            AppEvent::UsageUpdated { request_id, usage } => {
                self.reduce_usage_updated(request_id, usage);
            }
            AppEvent::IndexCompleted {
                request_id,
                repository_id,
                file_count,
                symbol_count,
                repository_map,
            } => self.reduce_index_completed(
                request_id,
                repository_id,
                file_count,
                symbol_count,
                repository_map,
            ),
            AppEvent::SourceLoaded {
                request_id,
                repository_id,
                path,
                start_line,
                end_line,
                content,
            } => self.reduce_source_loaded(
                request_id,
                repository_id,
                &path,
                start_line,
                end_line,
                content,
            ),
            AppEvent::Cancelled { request_id } => self.reduce_cancelled(request_id),
            AppEvent::SessionsListed {
                request_id,
                sessions,
            } => {
                if self.pending_history == Some(request_id) {
                    self.pending_history = None;
                    self.sessions = sessions;
                    self.history_loading = false;
                }
            }
            AppEvent::SessionLoaded {
                request_id,
                session,
            } => self.reduce_session_loaded(request_id, session),
            AppEvent::DiagramOpened {
                request_id,
                diagram_id,
            } => {
                if self.pending_diagrams.get(&request_id) == Some(&diagram_id) {
                    self.pending_diagrams.remove(&request_id);
                    self.diagram_messages.insert(
                        diagram_id,
                        "Diagram opened in the default viewer.".to_owned(),
                    );
                }
            }
            AppEvent::Error { request_id, error } => self.reduce_error(request_id, error),
        }
    }

    fn reduce_progress(&mut self, request_id: RequestId, progress: Progress) {
        if !self.is_active(request_id) {
            return;
        }
        let coalesce_file_progress = self.active_is(request_id, RequestKind::Index);
        self.record_progress(request_id, progress, coalesce_file_progress);
    }

    fn record_progress(
        &mut self,
        request_id: RequestId,
        progress: Progress,
        coalesce_file_progress: bool,
    ) {
        let detail = match (progress.completed, progress.total) {
            (Some(completed), Some(total)) => Some(format!("{completed} of {total}")),
            _ => None,
        };
        let merge_phase = coalesce_file_progress
            && matches!(
                progress.phase,
                ProgressPhase::Parsing | ProgressPhase::Indexing
            );
        if merge_phase {
            if let Some(item) = self.activity.iter_mut().rev().find(|item| {
                item.request_id == request_id && item.progress_phase == Some(progress.phase)
            }) {
                item.title = progress.message;
                item.detail = detail;
                return;
            }
        }

        let progress_count = self
            .activity
            .iter()
            .filter(|item| item.request_id == request_id && item.progress_phase.is_some())
            .count();
        if progress_count >= MAX_PROGRESS_ITEMS_PER_REQUEST {
            if let Some(index) = self
                .activity
                .iter()
                .position(|item| item.request_id == request_id && item.progress_phase.is_some())
            {
                self.activity.remove(index);
            }
        }
        self.activity.push(ActivityItem {
            request_id,
            call_id: None,
            title: progress.message,
            detail,
            status: ActivityStatus::Information,
            progress_phase: Some(progress.phase),
        });
    }

    fn complete_tool(&mut self, request_id: RequestId, call_id: ToolCallId, failed: bool) {
        if let Some(item) = self
            .activity
            .iter_mut()
            .rev()
            .find(|item| item.request_id == request_id && item.call_id == Some(call_id))
        {
            item.status = if failed {
                ActivityStatus::Failed
            } else {
                ActivityStatus::Complete
            };
            item.detail = Some(
                if failed {
                    "Repository query reported an error"
                } else {
                    "Repository query completed"
                }
                .to_owned(),
            );
        } else {
            self.activity.push(ActivityItem {
                request_id,
                call_id: Some(call_id),
                title: "Repository query".to_owned(),
                detail: Some("Completed without a matching start event".to_owned()),
                status: if failed {
                    ActivityStatus::Failed
                } else {
                    ActivityStatus::Complete
                },
                progress_phase: None,
            });
        }
    }

    fn reduce_answer_completed(&mut self, request_id: RequestId, mut answer: AgentAnswer) {
        if !self.active_is(request_id, RequestKind::Ask) || self.outcomes.contains_key(&request_id)
        {
            return;
        }
        let stored_usage = self.usage.get(&request_id).cloned();
        answer.usage = match (answer.usage.take(), stored_usage) {
            (Some(answer_usage), Some(stored_usage))
                if stored_usage.tokens.total_tokens > answer_usage.tokens.total_tokens =>
            {
                Some(stored_usage)
            }
            (Some(answer_usage), _) => Some(answer_usage),
            (None, stored_usage) => stored_usage,
        };
        if let Some(usage) = answer.usage.clone() {
            self.usage.insert(request_id, usage);
        }
        if let Some(turn) = self.turn_mut(request_id) {
            turn.answer_text.clone_from(&answer.text);
            turn.answer = Some(answer);
            turn.status = TurnStatus::Completed;
        }
        self.finish_running_activity(request_id, ActivityStatus::Complete);
        self.finish_active(request_id, RequestOutcome::Completed);
        self.status = WorkStatus::AnswerReady;
        self.selected_task = Some(request_id);
        self.selected_claim = None;
        self.selected_evidence = None;
        self.source = None;
    }

    fn reduce_usage_updated(&mut self, request_id: RequestId, usage: ModelUsage) {
        let accepted = self.is_active(request_id)
            || self
                .turns
                .iter()
                .any(|turn| turn.request_id == request_id && turn.answer.is_some());
        if !accepted {
            return;
        }
        let usage = self
            .usage
            .get(&request_id)
            .map_or(usage.clone(), |current| {
                if current.tokens.total_tokens > usage.tokens.total_tokens {
                    current.clone()
                } else {
                    usage
                }
            });
        if let Some(answer) = self
            .turn_mut(request_id)
            .and_then(|turn| turn.answer.as_mut())
        {
            answer.usage = Some(usage.clone());
        }
        self.usage.insert(request_id, usage);
    }

    fn reduce_index_completed(
        &mut self,
        request_id: RequestId,
        repository_id: RepositoryId,
        file_count: u64,
        symbol_count: u64,
        map: RepositoryMap,
    ) {
        if !self.active_is(request_id, RequestKind::Index)
            || self.pending_index_path.as_ref().map(|pending| pending.0) != Some(request_id)
        {
            return;
        }
        let path = self
            .pending_index_path
            .take()
            .map_or_else(String::new, |pending| pending.1);
        self.repository = Some(RepositoryView {
            path,
            id: repository_id,
            file_count,
            symbol_count,
            map,
        });
        self.clear_session();
        self.session_id = self.fresh_session_id(repository_id);
        self.session_repository_id = Some(repository_id);
        self.finish_running_activity(request_id, ActivityStatus::Complete);
        self.finish_active(request_id, RequestOutcome::Completed);
        self.status = WorkStatus::Indexed;
        self.panel = WorkspacePanel::Conversation;
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "arguments mirror the SourceLoaded contract for strict stale-event validation"
    )]
    fn reduce_source_loaded(
        &mut self,
        request_id: RequestId,
        repository_id: RepositoryId,
        path: &RepositoryPath,
        start_line: u32,
        end_line: u32,
        content: String,
    ) {
        let Some(source) = self.source.as_mut() else {
            return;
        };
        if source.pending_request != Some(request_id)
            || source.repository_id != Some(repository_id)
            || source.path != *path
            || source.start_line != start_line
        {
            return;
        }
        source.end_line = end_line;
        source.content = content;
        source.status = SourceStatus::Loaded;
        source.message = None;
        source.pending_request = None;
    }

    fn reduce_cancelled(&mut self, request_id: RequestId) {
        let target_request_id = self
            .active
            .as_ref()
            .filter(|active| active.cancel_id == Some(request_id))
            .map_or(request_id, |active| active.id);
        if !self.is_active(target_request_id) || self.outcomes.contains_key(&target_request_id) {
            return;
        }
        if request_id != target_request_id {
            self.outcomes.insert(request_id, RequestOutcome::Cancelled);
        }
        self.set_turn_status(target_request_id, TurnStatus::Cancelled);
        self.finish_running_activity(target_request_id, ActivityStatus::Cancelled);
        self.finish_active(target_request_id, RequestOutcome::Cancelled);
        self.status = WorkStatus::Cancelled;
    }

    fn reduce_session_loaded(&mut self, request_id: RequestId, session: SessionContext) {
        if self.pending_session != Some(request_id) {
            return;
        }
        self.pending_session = None;
        self.clear_session();
        self.session_id = session.session_id;
        self.session_repository_id = Some(session.repository_id);
        for task in session.tasks {
            let task_request = task.request_id;
            for event in &task.workflow {
                self.restore_activity(task_request, event);
            }
            self.finish_running_activity(task_request, ActivityStatus::Complete);
            if let Some(usage) = task.answer.usage.clone() {
                self.usage.insert(task_request, usage);
            }
            self.turns.push(TurnView {
                request_id: task_request,
                question: task.question,
                answer_text: task.answer.text.clone(),
                answer: Some(task.answer),
                status: TurnStatus::Completed,
            });
            self.outcomes
                .insert(task_request, RequestOutcome::Completed);
        }
        self.selected_task = self.turns.last().map(|turn| turn.request_id);
        self.status = if self.turns.is_empty() {
            WorkStatus::Indexed
        } else {
            WorkStatus::AnswerReady
        };
        self.history_loading = false;
        self.history_open = false;
        self.error = None;
    }

    fn restore_activity(&mut self, request_id: RequestId, event: &WorkflowEvent) {
        match event {
            WorkflowEvent::Progress(progress) => {
                self.record_progress(request_id, progress.clone(), true);
            }
            WorkflowEvent::ToolCallStarted(call) => self.activity.push(ActivityItem {
                request_id,
                call_id: Some(call.id),
                title: human_tool_name(&call.name),
                detail: Some("Read-only repository query".to_owned()),
                status: ActivityStatus::Running,
                progress_phase: None,
            }),
            WorkflowEvent::ToolCallCompleted {
                call_id, is_error, ..
            } => self.complete_tool(request_id, *call_id, *is_error),
        }
    }

    fn reduce_error(&mut self, request_id: Option<RequestId>, error: AppError) {
        if let Some(event_request_id) = request_id {
            let source_matches = self
                .source
                .as_ref()
                .is_some_and(|source| source.pending_request == Some(event_request_id));
            if source_matches {
                if let Some(source) = self.source.as_mut() {
                    source.status = SourceStatus::Failed;
                    source.message = Some(error.message);
                    source.pending_request = None;
                }
                return;
            }
            if let Some(diagram_id) = self.pending_diagrams.remove(&event_request_id) {
                let notice = present_error(error);
                self.diagram_messages.insert(
                    diagram_id,
                    format!("Could not open diagram: {}", notice.message),
                );
                self.error = Some(notice);
                return;
            }
        }
        let history_failed = request_id.is_some_and(|id| self.pending_history == Some(id));
        let session_failed = request_id.is_some_and(|id| self.pending_session == Some(id));
        if history_failed {
            self.pending_history = None;
            self.history_loading = false;
        }
        if session_failed {
            self.pending_session = None;
            self.history_loading = false;
        }

        let notice = present_error(error);
        let non_fatal = matches!(
            notice.code.as_str(),
            "diagram_generation_failed" | "session_update_failed" | "session_save_failed"
        );
        if non_fatal
            && request_id.is_some_and(|id| {
                self.is_active(id) || self.turns.iter().any(|turn| turn.request_id == id)
            })
        {
            self.notices.push(notice);
            return;
        }

        let Some(request_id) = request_id else {
            self.error = Some(notice);
            if let Some(active_request_id) = self.active.as_ref().map(|active| active.id) {
                self.set_turn_status(active_request_id, TurnStatus::Failed);
                self.finish_running_activity(active_request_id, ActivityStatus::Failed);
            }
            self.active = None;
            self.status = WorkStatus::Error;
            return;
        };
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.cancel_id == Some(request_id))
        {
            if let Some(active) = self.active.as_mut() {
                active.cancel_id = None;
                self.status = match active.kind {
                    RequestKind::Index => WorkStatus::Indexing,
                    RequestKind::Ask => WorkStatus::Thinking,
                };
            }
            self.error = Some(notice);
            return;
        }
        if !self.is_active(request_id) && !history_failed && !session_failed {
            return;
        }
        if self.is_active(request_id) {
            self.set_turn_status(request_id, TurnStatus::Failed);
            self.finish_running_activity(request_id, ActivityStatus::Failed);
            self.finish_active(request_id, RequestOutcome::Failed);
            self.status = WorkStatus::Error;
        }
        self.error = Some(notice);
    }

    fn clear_session(&mut self) {
        self.question_input.clear();
        self.turns.clear();
        self.selected_task = None;
        self.selected_claim = None;
        self.selected_evidence = None;
        self.source = None;
        self.activity.clear();
        self.outcomes.clear();
        self.usage.clear();
        self.notices.clear();
        self.diagram_messages.clear();
    }

    fn is_active(&self, request_id: RequestId) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.id == request_id)
    }

    fn active_is(&self, request_id: RequestId, kind: RequestKind) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.id == request_id && active.kind == kind)
    }

    fn finish_active(&mut self, request_id: RequestId, outcome: RequestOutcome) {
        self.outcomes.insert(request_id, outcome);
        if self.is_active(request_id) {
            self.active = None;
        }
    }

    fn set_turn_status(&mut self, request_id: RequestId, status: TurnStatus) {
        if let Some(turn) = self.turn_mut(request_id) {
            if turn.status == TurnStatus::Running {
                turn.status = status;
            }
        }
    }

    fn finish_running_activity(&mut self, request_id: RequestId, status: ActivityStatus) {
        let detail = match status {
            ActivityStatus::Complete => "Request completed",
            ActivityStatus::Cancelled => "Stopped because the request was cancelled",
            ActivityStatus::Failed => "Stopped because the request failed",
            ActivityStatus::Running | ActivityStatus::Information => return,
        };
        for item in self
            .activity
            .iter_mut()
            .filter(|item| item.request_id == request_id && item.status == ActivityStatus::Running)
        {
            item.status = status;
            item.detail = Some(detail.to_owned());
        }
    }

    fn turn_mut(&mut self, request_id: RequestId) -> Option<&mut TurnView> {
        self.turns
            .iter_mut()
            .find(|turn| turn.request_id == request_id)
    }

    fn next_request_id(&mut self, operation: &str) -> RequestId {
        let sequence = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        RequestId::from_stable_parts(&["gui", &self.namespace, operation, &sequence.to_string()])
    }

    fn fresh_session_id(&mut self, repository_id: RepositoryId) -> SessionId {
        let sequence = self.next_session;
        self.next_session = self.next_session.saturating_add(1);
        SessionId::from_stable_parts(&[
            "gui",
            &self.namespace,
            &repository_id.to_string(),
            &sequence.to_string(),
        ])
    }
}

fn evidence_line_range(evidence: &Evidence) -> (u32, u32) {
    let start = evidence.span.start().line();
    let end_position = evidence.span.end();
    let end = if end_position.column() == 0 && end_position.line() > start {
        end_position.line() - 1
    } else {
        end_position.line()
    };
    (start, end)
}

fn present_error(error: AppError) -> UiError {
    let authentication_failed = error.code == "model_error"
        && (error.message.contains("HTTP 401")
            || error.message.contains("invalid_api_key")
            || error.message.contains("Incorrect API key"));
    UiError {
        code: error.code,
        message: if authentication_failed {
            "Authentication failed (HTTP 401). The API key was rejected by the configured model provider. Make sure CODEATLAS_ENDPOINT, CODEATLAS_MODEL, and the API key belong to the same provider, then restart CodeAtlas."
                .to_owned()
        } else {
            error.message
        },
        retryable: error.retryable,
    }
}

pub(crate) fn call_target(target: &TargetResolution<codeatlas_core::SymbolId>) -> String {
    match target {
        TargetResolution::Resolved(symbol) => format!("symbol {}", short_id(symbol)),
        TargetResolution::Unresolved(target) => format!("unresolved: {}", target.name),
    }
}

pub(crate) fn short_id(id: &impl ToString) -> String {
    id.to_string().chars().take(8).collect()
}

fn human_tool_name(name: &str) -> String {
    match name {
        "list_files" => "Listing repository files".to_owned(),
        "read_file" => "Reading source".to_owned(),
        "find_symbol" => "Finding symbols".to_owned(),
        "find_references" => "Finding references".to_owned(),
        "get_symbol" => "Inspecting a symbol".to_owned(),
        "get_module" => "Inspecting a module".to_owned(),
        "search_code" => "Searching source".to_owned(),
        "trace_call" => "Tracing a call path".to_owned(),
        "get_repository_overview" => "Mapping repository".to_owned(),
        other => {
            let mut words = other.replace('_', " ");
            if let Some(first) = words.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            words
        }
    }
}

#[cfg(test)]
mod tests {
    use codeatlas_core::{AnswerId, ClaimKind, DiagramDecision, FileId, SourceSpan, TokenUsage};

    use super::*;

    fn repository_id(name: &str) -> RepositoryId {
        RepositoryId::from_stable_parts(&[name])
    }

    fn indexed(state: &mut GuiState, name: &str) -> RepositoryId {
        state.repository_input = name.to_owned();
        let command = state.index_repository().expect("index command");
        let AppCommand::Index { request_id, .. } = command else {
            panic!("expected index command");
        };
        let id = repository_id(name);
        state.reduce(AppEvent::IndexCompleted {
            request_id,
            repository_id: id,
            file_count: 2,
            symbol_count: 3,
            repository_map: RepositoryMap {
                name: name.to_owned(),
                ..RepositoryMap::default()
            },
        });
        id
    }

    fn answer(text: &str) -> AgentAnswer {
        AgentAnswer {
            id: AnswerId::from_stable_parts(&[text]),
            text: text.to_owned(),
            claims: Vec::new(),
            evidence: Vec::new(),
            call_paths: Vec::new(),
            diagram: DiagramDecision::NotNeeded {
                reason: "A diagram would not add clarity.".to_owned(),
            },
            usage: None,
        }
    }

    #[test]
    fn provider_authentication_errors_are_actionable_and_do_not_repeat_key_fragments() {
        let error = present_error(AppError {
            code: "model_error".to_owned(),
            message: "model endpoint returned HTTP 401: Incorrect API key provided: sk-secret"
                .to_owned(),
            retryable: false,
        });

        assert!(error.message.contains("CODEATLAS_ENDPOINT"));
        assert!(error.message.contains("CODEATLAS_MODEL"));
        assert!(!error.message.contains("sk-secret"));
    }

    #[test]
    fn answer_delta_and_non_fatal_error_do_not_block_final_answer() {
        let mut state = GuiState::new("test");
        indexed(&mut state, "repo");
        state.question_input = "What happens?".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        state.reduce(AppEvent::AnswerDelta {
            request_id,
            delta: "Partial".to_owned(),
        });
        state.reduce(AppEvent::Error {
            request_id: Some(request_id),
            error: AppError {
                code: "diagram_generation_failed".to_owned(),
                message: "SVG unavailable".to_owned(),
                retryable: false,
            },
        });
        state.reduce(AppEvent::AnswerCompleted {
            request_id,
            answer: answer("Final"),
        });
        state.reduce(AppEvent::Error {
            request_id: Some(request_id),
            error: AppError {
                code: "session_save_failed".to_owned(),
                message: "Answer remains available in memory".to_owned(),
                retryable: true,
            },
        });

        assert_eq!(state.turns[0].answer_text, "Final");
        assert!(state.turns[0].answer.is_some());
        assert_eq!(state.turns[0].status, TurnStatus::Completed);
        assert_eq!(state.notices.len(), 2);
        assert_eq!(state.status, WorkStatus::AnswerReady);
    }

    #[test]
    fn usage_received_before_answer_is_merged_into_completed_turn() {
        let mut state = GuiState::new("usage-before-answer");
        indexed(&mut state, "repo");
        state.question_input = "How expensive was this?".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        let usage = ModelUsage {
            tokens: TokenUsage {
                input_tokens: 1_200,
                output_tokens: 300,
                cached_input_tokens: 250,
                total_tokens: 1_500,
            },
            cost: None,
        };

        state.reduce(AppEvent::UsageUpdated {
            request_id,
            usage: usage.clone(),
        });

        assert!(state.selected_answer().is_none());
        assert_eq!(state.selected_usage(), Some(&usage));
        assert_eq!(state.status, WorkStatus::Thinking);

        state.reduce(AppEvent::AnswerCompleted {
            request_id,
            answer: answer("Final answer"),
        });

        assert_eq!(
            state.turns[0]
                .answer
                .as_ref()
                .and_then(|answer| answer.usage.as_ref()),
            Some(&usage)
        );
        assert_eq!(state.selected_usage(), Some(&usage));
    }

    #[test]
    fn usage_received_after_answer_updates_completed_turn() {
        let mut state = GuiState::new("usage-after-answer");
        indexed(&mut state, "repo");
        state.question_input = "How expensive was this?".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        state.reduce(AppEvent::AnswerCompleted {
            request_id,
            answer: answer("Final answer"),
        });
        let usage = ModelUsage {
            tokens: TokenUsage {
                input_tokens: 800,
                output_tokens: 200,
                cached_input_tokens: 100,
                total_tokens: 1_000,
            },
            cost: None,
        };

        state.reduce(AppEvent::UsageUpdated {
            request_id,
            usage: usage.clone(),
        });

        assert_eq!(
            state.turns[0]
                .answer
                .as_ref()
                .and_then(|answer| answer.usage.as_ref()),
            Some(&usage)
        );
        assert_eq!(state.selected_usage(), Some(&usage));
    }

    #[test]
    fn delayed_partial_usage_does_not_replace_completed_cumulative_usage() {
        let mut state = GuiState::new("delayed-partial-usage");
        indexed(&mut state, "repo");
        state.question_input = "How expensive was this?".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        let cumulative = ModelUsage {
            tokens: TokenUsage {
                input_tokens: 1_200,
                output_tokens: 300,
                cached_input_tokens: 250,
                total_tokens: 1_500,
            },
            cost: None,
        };
        let mut completed = answer("Final answer");
        completed.usage = Some(cumulative.clone());
        state.reduce(AppEvent::AnswerCompleted {
            request_id,
            answer: completed,
        });

        state.reduce(AppEvent::UsageUpdated {
            request_id,
            usage: ModelUsage {
                tokens: TokenUsage {
                    input_tokens: 800,
                    output_tokens: 200,
                    cached_input_tokens: 100,
                    total_tokens: 1_000,
                },
                cost: None,
            },
        });

        assert_eq!(state.selected_usage(), Some(&cumulative));
        assert_eq!(
            state
                .selected_answer()
                .and_then(|answer| answer.usage.as_ref()),
            Some(&cumulative)
        );
    }

    #[test]
    fn restored_session_exposes_selected_task_usage() {
        let mut state = GuiState::new("restored-usage");
        let session_id = SessionId::from_stable_parts(&["restored-session"]);
        let AppCommand::LoadSession { request_id, .. } = state
            .load_session(session_id)
            .expect("load session command")
        else {
            panic!("expected load session");
        };
        let task_request_id = RequestId::from_stable_parts(&["restored-task"]);
        let usage = ModelUsage {
            tokens: TokenUsage {
                input_tokens: 600,
                output_tokens: 150,
                cached_input_tokens: 25,
                total_tokens: 750,
            },
            cost: None,
        };
        let mut restored_answer = answer("Restored answer");
        restored_answer.usage = Some(usage.clone());

        state.reduce(AppEvent::SessionLoaded {
            request_id,
            session: SessionContext {
                schema_version: 2,
                session_id,
                repository_id: repository_id("restored-repo"),
                tasks: vec![codeatlas_core::SessionTask {
                    request_id: task_request_id,
                    question: "Restored question".to_owned(),
                    answer: restored_answer,
                    workflow: Vec::new(),
                }],
                usage: usage.clone(),
                created_at_unix_ms: 0,
                updated_at_unix_ms: 0,
                json_path: "session.json".to_owned(),
            },
        });

        assert_eq!(state.selected_task, Some(task_request_id));
        assert_eq!(state.selected_usage(), Some(&usage));
    }

    #[test]
    fn exploratory_evidence_is_not_presented_as_verified() {
        let mut state = GuiState::new("test");
        indexed(&mut state, "repo");
        state.question_input = "Question".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        let evidence = Evidence {
            id: EvidenceId::from_stable_parts(&["candidate"]),
            file_id: FileId::from_stable_parts(&["file"]),
            path: RepositoryPath::new("src/lib.rs").expect("path"),
            span: SourceSpan::new(1, 0, 2, 0).expect("span"),
            symbol_id: None,
            excerpt: Some("candidate".to_owned()),
        };
        state.reduce(AppEvent::EvidenceAdded {
            request_id,
            evidence,
        });

        assert!(state.visible_evidence().is_empty());
    }

    #[test]
    fn cancelled_ask_finishes_turn_and_running_activity() {
        let mut state = GuiState::new("cancelled");
        indexed(&mut state, "repo");
        state.question_input = "Question".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        state.activity.push(ActivityItem {
            request_id,
            call_id: Some(ToolCallId::from_stable_parts(&["running"])),
            title: "Reading source".to_owned(),
            detail: None,
            status: ActivityStatus::Running,
            progress_phase: None,
        });

        state.reduce(AppEvent::Cancelled { request_id });

        assert_eq!(state.turns[0].status, TurnStatus::Cancelled);
        assert_eq!(state.activity[0].status, ActivityStatus::Cancelled);
        assert_eq!(state.status, WorkStatus::Cancelled);
        assert!(!state.is_busy());
    }

    #[test]
    fn fatal_ask_error_finishes_turn_and_running_activity() {
        let mut state = GuiState::new("failed");
        indexed(&mut state, "repo");
        state.question_input = "Question".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        state.activity.push(ActivityItem {
            request_id,
            call_id: Some(ToolCallId::from_stable_parts(&["running"])),
            title: "Tracing a call path".to_owned(),
            detail: None,
            status: ActivityStatus::Running,
            progress_phase: None,
        });

        state.reduce(AppEvent::Error {
            request_id: Some(request_id),
            error: AppError {
                code: "model_failed".to_owned(),
                message: "The model request failed".to_owned(),
                retryable: true,
            },
        });

        assert_eq!(state.turns[0].status, TurnStatus::Failed);
        assert_eq!(state.activity[0].status, ActivityStatus::Failed);
        assert_eq!(state.status, WorkStatus::Error);
        assert!(!state.is_busy());
    }

    #[test]
    fn thousands_of_index_progress_events_remain_bounded() {
        let mut state = GuiState::new("progress");
        state.repository_input = "repo".to_owned();
        let AppCommand::Index { request_id, .. } = state.index_repository().expect("index command")
        else {
            panic!("expected index");
        };

        for step in 0..5_000 {
            state.reduce(AppEvent::Progress {
                request_id,
                progress: Progress {
                    phase: if step % 2 == 0 {
                        ProgressPhase::Parsing
                    } else {
                        ProgressPhase::Indexing
                    },
                    message: format!("processing file {step}"),
                    completed: Some(step),
                    total: Some(5_000),
                },
            });
        }

        assert_eq!(state.activity.len(), 2);
        assert!(state.activity.iter().all(|item| {
            matches!(
                item.progress_phase,
                Some(ProgressPhase::Parsing | ProgressPhase::Indexing)
            )
        }));
    }

    #[test]
    fn index_attempt_keeps_current_activity_until_successful_switch() {
        let mut state = GuiState::new("switch");
        indexed(&mut state, "current");
        let retained_request = RequestId::from_stable_parts(&["retained"]);
        state.activity.push(ActivityItem {
            request_id: retained_request,
            call_id: None,
            title: "Existing session activity".to_owned(),
            detail: None,
            status: ActivityStatus::Complete,
            progress_phase: None,
        });
        state.question_input = "unfinished draft".to_owned();
        state.repository_input = "other".to_owned();
        let AppCommand::Index { request_id, .. } = state.index_repository().expect("index command")
        else {
            panic!("expected index");
        };
        assert!(
            state
                .activity
                .iter()
                .any(|item| item.request_id == retained_request)
        );
        state.reduce(AppEvent::Cancelled { request_id });
        assert!(
            state
                .activity
                .iter()
                .any(|item| item.request_id == retained_request)
        );
        assert_eq!(state.question_input, "unfinished draft");

        let AppCommand::Index { request_id, .. } =
            state.index_repository().expect("second index command")
        else {
            panic!("expected index");
        };
        state.reduce(AppEvent::Error {
            request_id: Some(request_id),
            error: AppError {
                code: "index_failed".to_owned(),
                message: "Indexing failed".to_owned(),
                retryable: true,
            },
        });
        assert!(
            state
                .activity
                .iter()
                .any(|item| item.request_id == retained_request)
        );
        assert_eq!(state.question_input, "unfinished draft");

        let AppCommand::Index { request_id, .. } =
            state.index_repository().expect("third index command")
        else {
            panic!("expected index");
        };
        state.reduce(AppEvent::IndexCompleted {
            request_id,
            repository_id: repository_id("other"),
            file_count: 1,
            symbol_count: 1,
            repository_map: RepositoryMap {
                name: "other".to_owned(),
                ..RepositoryMap::default()
            },
        });
        assert!(state.activity.is_empty());
        assert!(state.question_input.is_empty());
    }

    #[test]
    fn stale_answer_and_source_events_are_ignored() {
        let mut state = GuiState::new("test");
        let repository_id = indexed(&mut state, "repo");
        state.question_input = "Question".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        let stale_request = RequestId::from_stable_parts(&["stale"]);
        state.reduce(AppEvent::AnswerCompleted {
            request_id: stale_request,
            answer: answer("stale"),
        });
        assert!(state.turns[0].answer.is_none());

        state.reduce(AppEvent::AnswerCompleted {
            request_id,
            answer: answer("current"),
        });
        assert_eq!(state.turns[0].answer_text, "current");

        state.source = Some(SourceView {
            evidence_id: EvidenceId::from_stable_parts(&["evidence"]),
            path: RepositoryPath::new("src/lib.rs").expect("path"),
            start_line: 1,
            end_line: 2,
            content: "excerpt".to_owned(),
            status: SourceStatus::Loading,
            message: None,
            pending_request: Some(request_id),
            repository_id: Some(repository_id),
        });
        state.reduce(AppEvent::SourceLoaded {
            request_id: stale_request,
            repository_id,
            path: RepositoryPath::new("src/lib.rs").expect("path"),
            start_line: 1,
            end_line: 2,
            content: "stale".to_owned(),
        });
        assert_eq!(state.source.as_ref().expect("source").content, "excerpt");
    }

    #[test]
    fn loaded_session_for_another_repository_is_read_only() {
        let mut state = GuiState::new("test");
        indexed(&mut state, "current");
        let command = state.list_sessions();
        let AppCommand::ListSessions { request_id, .. } = command else {
            panic!("expected list");
        };
        state.reduce(AppEvent::SessionsListed {
            request_id,
            sessions: Vec::new(),
        });
        let other_session = SessionId::from_stable_parts(&["other-session"]);
        let AppCommand::LoadSession { request_id, .. } = state
            .load_session(other_session)
            .expect("load session command")
        else {
            panic!("expected load");
        };
        state.reduce(AppEvent::SessionLoaded {
            request_id,
            session: SessionContext {
                schema_version: 2,
                session_id: other_session,
                repository_id: repository_id("other"),
                tasks: Vec::new(),
                usage: ModelUsage {
                    tokens: TokenUsage::default(),
                    cost: None,
                },
                created_at_unix_ms: 0,
                updated_at_unix_ms: 0,
                json_path: "session.json".to_owned(),
            },
        });

        assert!(state.is_read_only());
        state.question_input = "Can I continue?".to_owned();
        assert!(state.ask().is_none());
        assert_eq!(
            state.error.as_ref().expect("human-readable error").code,
            "session_repository_mismatch"
        );
    }

    #[test]
    fn cancel_source_and_diagram_commands_preserve_core_contracts() {
        let mut state = GuiState::new("test");
        let repository_id = indexed(&mut state, "repo");
        state.question_input = "Question".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("ask command") else {
            panic!("expected ask");
        };
        let AppCommand::Cancel {
            request_id: cancel_request_id,
            target_request_id,
        } = state.cancel().expect("cancel command")
        else {
            panic!("expected cancel");
        };
        assert_eq!(target_request_id, request_id);
        // Presentation implementations accept either the cancellation command
        // ID or the target ID so transports can acknowledge either form.
        state.reduce(AppEvent::Cancelled {
            request_id: cancel_request_id,
        });
        assert!(!state.is_busy());

        state.question_input = "Evidence?".to_owned();
        let AppCommand::Ask { request_id, .. } = state.ask().expect("second ask") else {
            panic!("expected ask");
        };
        let evidence = Evidence {
            id: EvidenceId::from_stable_parts(&["final"]),
            file_id: FileId::from_stable_parts(&["file"]),
            path: RepositoryPath::new("src/lib.rs").expect("path"),
            span: SourceSpan::new(4, 0, 7, 0).expect("span"),
            symbol_id: None,
            excerpt: Some("fn example() {}".to_owned()),
        };
        let claim_id = ClaimId::from_stable_parts(&["claim"]);
        let mut final_answer = answer("Supported answer");
        final_answer.claims.push(Claim {
            id: claim_id,
            kind: ClaimKind::Fact,
            text: "The example exists.".to_owned(),
            evidence_ids: vec![evidence.id],
        });
        final_answer.evidence.push(evidence.clone());
        state.reduce(AppEvent::AnswerCompleted {
            request_id,
            answer: final_answer,
        });
        state.select_claim(claim_id);
        assert_eq!(state.visible_evidence().len(), 1);
        let AppCommand::LoadSource {
            request_id: source_request,
            repository_id: source_repository,
            path,
            start_line,
            end_line,
        } = state.load_evidence(evidence.id).expect("source command")
        else {
            panic!("expected source command");
        };
        assert_eq!(source_repository, repository_id);
        assert_eq!(path.as_str(), "src/lib.rs");
        assert_eq!((start_line, end_line), (4, Some(6)));
        state.reduce(AppEvent::SourceLoaded {
            request_id: source_request,
            repository_id,
            path,
            start_line,
            end_line: 6,
            content: "loaded source".to_owned(),
        });
        assert_eq!(
            state.source.as_ref().expect("loaded source").status,
            SourceStatus::Loaded
        );

        let diagram_id = DiagramId::from_stable_parts(&["diagram"]);
        let other_diagram_id = DiagramId::from_stable_parts(&["other-diagram"]);
        let AppCommand::OpenDiagram {
            request_id: diagram_request,
            diagram_id: command_diagram,
        } = state.open_diagram(diagram_id)
        else {
            panic!("expected diagram command");
        };
        assert_eq!(command_diagram, diagram_id);
        assert_eq!(state.diagram_message(other_diagram_id), None);
        state.reduce(AppEvent::DiagramOpened {
            request_id: diagram_request,
            diagram_id,
        });
        assert_eq!(
            state.diagram_message(diagram_id),
            Some("Diagram opened in the default viewer.")
        );
    }

    #[test]
    fn diagram_open_failure_is_visible_inline_and_in_the_error_banner() {
        let mut state = GuiState::new("diagram-error");
        let diagram_id = DiagramId::from_stable_parts(&["failed-diagram"]);
        let AppCommand::OpenDiagram { request_id, .. } = state.open_diagram(diagram_id) else {
            panic!("expected diagram command");
        };

        state.reduce(AppEvent::Error {
            request_id: Some(request_id),
            error: AppError {
                code: "diagram_open_failed".to_owned(),
                message: "No compatible viewer was found".to_owned(),
                retryable: false,
            },
        });

        assert_eq!(
            state.diagram_message(diagram_id),
            Some("Could not open diagram: No compatible viewer was found")
        );
        assert_eq!(
            state.error,
            Some(UiError {
                code: "diagram_open_failed".to_owned(),
                message: "No compatible viewer was found".to_owned(),
                retryable: false,
            })
        );
        assert_ne!(state.status, WorkStatus::Error);
    }
}
