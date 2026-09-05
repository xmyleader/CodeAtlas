use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use codeatlas_core::{
    AgentAnswer, AppCommand, AppError, AppEvent, CallPath, Claim, ClaimKind, Cost, DiagramArtifact,
    DiagramDecision, DiagramId, Evidence, EvidenceId, ModelUsage, Progress, ProgressPhase,
    RepositoryId, RepositoryMap, RepositoryPath, RequestId, SessionContext, SessionId,
    SessionSummary, SymbolId, TokenUsage, ToolCallId, WorkflowEvent,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};

const MIN_STANDARD_WIDTH: u16 = 24;
const MIN_STANDARD_HEIGHT: u16 = 8;
const MIN_WIDE_WIDTH: u16 = 100;
const MIN_WIDE_HEIGHT: u16 = 12;
static TUI_INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    Repository,
    Conversation,
    Evidence,
}

impl Panel {
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Repository => 0,
            Self::Conversation => 1,
            Self::Evidence => 2,
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Repository => Self::Conversation,
            Self::Conversation => Self::Evidence,
            Self::Evidence => Self::Repository,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Repository => Self::Evidence,
            Self::Conversation => Self::Repository,
            Self::Evidence => Self::Conversation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Navigation,
    RepositoryPath,
    Question,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Wide,
    Tabbed,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "phase", rename_all = "snake_case")]
pub enum Activity {
    Idle,
    IndexQueued,
    AskQueued,
    Running(ProgressPhase),
    Cancelling,
    Indexed,
    AnswerReady,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Index,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPreferences {
    /// Width at which all three information-dense panels become visible.
    pub wide_layout_min_width: u16,
    /// Includes structured tool arguments and outputs in the trace view.
    pub show_tool_payloads: bool,
    /// Wraps source excerpts instead of clipping long source lines.
    pub wrap_evidence: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            wide_layout_min_width: 120,
            show_tool_payloads: false,
            wrap_evidence: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryView {
    pub path: String,
    pub repository_id: Option<RepositoryId>,
    pub file_count: u64,
    pub symbol_count: u64,
    pub repository_map: RepositoryMap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "preserve the public ConversationEntry payload shape"
)]
pub enum ConversationEntry {
    Question { request_id: RequestId, text: String },
    Answer(AnswerView),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerView {
    pub request_id: RequestId,
    pub text: String,
    pub claims: Vec<Claim>,
    pub call_paths: Vec<CallPath>,
    pub diagram: Option<DiagramDecision>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceClaimContext {
    pub label: String,
    pub kind: ClaimKind,
    pub text: String,
}

#[derive(Debug, Default)]
pub(crate) struct ClaimNumbers {
    fact: usize,
    inference: usize,
    unknown: usize,
}

impl ClaimNumbers {
    pub(crate) fn next_label(&mut self, kind: ClaimKind) -> String {
        let (prefix, number) = match kind {
            ClaimKind::Fact => ('F', &mut self.fact),
            ClaimKind::Inference => ('I', &mut self.inference),
            ClaimKind::Unknown => ('U', &mut self.unknown),
        };
        *number = number.saturating_add(1);
        format!("{prefix}{number}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTraceStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTrace {
    pub request_id: RequestId,
    pub call_id: ToolCallId,
    pub name: Option<String>,
    pub arguments: Option<String>,
    pub output: Option<String>,
    pub status: ToolTraceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressTrace {
    pub request_id: RequestId,
    pub progress: Progress,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryView {
    sessions: Vec<SessionSummary>,
    selected_session: Option<usize>,
    loading: bool,
    visible: bool,
    scroll: u16,
}

impl HistoryView {
    #[must_use]
    pub fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }

    #[must_use]
    pub const fn selected_session(&self) -> Option<usize> {
        self.selected_session
    }

    #[must_use]
    pub const fn loading(&self) -> bool {
        self.loading
    }

    #[must_use]
    pub const fn scroll(&self) -> u16 {
        self.scroll
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingSourceLoad {
    request_id: RequestId,
    repository_id: RepositoryId,
    path: RepositoryPath,
    start_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceLoadState {
    Excerpt,
    Loading(PendingSourceLoad),
    Loaded,
    Failed { message: String },
}

/// Presentation state for the full-screen source evidence viewer.
///
/// Source content intentionally stays out of [`TuiSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceViewer {
    evidence_id: EvidenceId,
    evidence_index: usize,
    path: RepositoryPath,
    start_line: u32,
    end_line: u32,
    symbol_id: Option<SymbolId>,
    content: String,
    content_start_line: u32,
    content_end_line: u32,
    source_state: SourceLoadState,
    wrap: bool,
    vertical_scroll: u16,
    horizontal_scroll: u16,
}

impl EvidenceViewer {
    #[must_use]
    pub const fn evidence_id(&self) -> EvidenceId {
        self.evidence_id
    }

    #[must_use]
    pub const fn evidence_index(&self) -> usize {
        self.evidence_index
    }

    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    #[must_use]
    pub const fn start_line(&self) -> u32 {
        self.start_line
    }

    #[must_use]
    pub const fn end_line(&self) -> u32 {
        self.end_line
    }

    #[must_use]
    pub const fn symbol_id(&self) -> Option<SymbolId> {
        self.symbol_id
    }

    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    #[must_use]
    pub const fn content_start_line(&self) -> u32 {
        self.content_start_line
    }

    #[must_use]
    pub const fn content_end_line(&self) -> u32 {
        self.content_end_line
    }

    #[must_use]
    pub const fn source_loaded(&self) -> bool {
        matches!(&self.source_state, SourceLoadState::Loaded)
    }

    #[must_use]
    pub const fn loading(&self) -> bool {
        matches!(&self.source_state, SourceLoadState::Loading(_))
    }

    #[must_use]
    pub fn source_error(&self) -> Option<&str> {
        match &self.source_state {
            SourceLoadState::Failed { message } => Some(message),
            SourceLoadState::Excerpt | SourceLoadState::Loading(_) | SourceLoadState::Loaded => {
                None
            }
        }
    }

    #[must_use]
    pub const fn wrap(&self) -> bool {
        self.wrap
    }

    #[must_use]
    pub const fn vertical_scroll(&self) -> u16 {
        self.vertical_scroll
    }

    #[must_use]
    pub const fn horizontal_scroll(&self) -> u16 {
        self.horizontal_scroll
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiError {
    pub request_id: Option<RequestId>,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TuiSnapshot {
    pub session_id: SessionId,
    pub repository: RepositoryView,
    pub activity: Activity,
    pub focused_panel: Panel,
    pub input_mode: InputMode,
    pub active_request_id: Option<RequestId>,
    pub active_request_kind: Option<RequestKind>,
    pub token_usage: TokenUsage,
    pub cost: Option<Cost>,
    pub conversation_entries: u64,
    pub evidence_items: u64,
    pub error: Option<UiError>,
}

#[derive(Debug, Clone)]
struct ActiveRequest {
    request_id: RequestId,
    kind: RequestKind,
    cancelling: bool,
    cancel_request_id: Option<RequestId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InputBuffer {
    value: String,
    cursor: usize,
}

impl InputBuffer {
    fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
    }

    fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    fn insert(&mut self, character: char) {
        self.value.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    fn backspace(&mut self) {
        let Some(previous) = self.value[..self.cursor].char_indices().next_back() else {
            return;
        };
        self.value.drain(previous.0..self.cursor);
        self.cursor = previous.0;
    }

    fn delete(&mut self) {
        let Some(character) = self.value[self.cursor..].chars().next() else {
            return;
        };
        self.value
            .drain(self.cursor..self.cursor + character.len_utf8());
    }

    fn move_left(&mut self) {
        if let Some((index, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.cursor = index;
        }
    }

    fn move_right(&mut self) {
        if let Some(character) = self.value[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn visible_with_cursor(&self, width: usize) -> String {
        if width == 0 {
            return String::new();
        }
        if width == 1 {
            return "|".to_owned();
        }

        let mut characters: Vec<char> = self.value.chars().collect();
        let cursor = self.value[..self.cursor].chars().count();
        characters.insert(cursor, '|');
        if characters.len() <= width {
            return characters.into_iter().collect();
        }

        let mut start = cursor.saturating_sub(width / 2);
        start = start.min(characters.len().saturating_sub(width));
        let end = start + width;
        let mut visible = characters[start..end].to_vec();
        if start > 0 {
            visible[0] = '<';
        }
        if end < characters.len() {
            visible[width - 1] = '>';
        }
        visible.into_iter().collect()
    }
}

/// Pure TUI state machine and renderer input.
///
/// The seed is not a credential. Each process launch uses a separate namespace
/// for transient request IDs and fresh repository sessions.
#[derive(Debug, Clone)]
pub struct TuiApp {
    id_seed: String,
    request_namespace: String,
    session_id: SessionId,
    session_repository_id: Option<RepositoryId>,
    session_json_path: Option<String>,
    next_request_sequence: u64,
    next_session_sequence: u64,
    preferences: UiPreferences,
    focused_panel: Panel,
    input_mode: InputMode,
    repository_input: InputBuffer,
    question_input: InputBuffer,
    repository: RepositoryView,
    activity: Activity,
    active_request: Option<ActiveRequest>,
    request_outcomes: BTreeMap<RequestId, RequestOutcome>,
    cancel_targets: BTreeMap<RequestId, RequestId>,
    pending_index_paths: BTreeMap<RequestId, String>,
    latest_index_request: Option<RequestId>,
    conversation: Vec<ConversationEntry>,
    evidence: Vec<Evidence>,
    selected_evidence: Option<usize>,
    evidence_viewer: Option<EvidenceViewer>,
    pending_source_requests: BTreeSet<RequestId>,
    tools: Vec<ToolTrace>,
    selected_tool: Option<usize>,
    progress_trace: Vec<ProgressTrace>,
    last_progress: Option<Progress>,
    selected_task: Option<RequestId>,
    history: HistoryView,
    pending_history_request: Option<RequestId>,
    pending_session_request: Option<RequestId>,
    pending_diagram_opens: BTreeMap<RequestId, (RequestId, DiagramId)>,
    diagram_open_status: BTreeMap<RequestId, String>,
    usage_by_request: BTreeMap<RequestId, ModelUsage>,
    repository_scroll: u16,
    conversation_scroll: u16,
    conversation_follow_tail: bool,
    evidence_scroll: u16,
    error: Option<UiError>,
    error_details_visible: bool,
    error_scroll: u16,
    clipboard_request: Option<String>,
    clipboard_status: Option<String>,
    quit_requested: bool,
}

impl TuiApp {
    #[must_use]
    pub fn new(id_seed: impl Into<String>) -> Self {
        Self::with_preferences(id_seed, UiPreferences::default())
    }

    #[must_use]
    pub fn with_preferences(id_seed: impl Into<String>, preferences: UiPreferences) -> Self {
        let id_seed = id_seed.into();
        let request_namespace = fresh_request_namespace();
        let session_id = SessionId::from_stable_parts(&[
            "tui",
            id_seed.as_str(),
            request_namespace.as_str(),
            "unindexed-session",
        ]);
        Self::with_session_and_request_namespace(
            id_seed,
            session_id,
            request_namespace,
            preferences,
        )
    }

    #[must_use]
    pub fn with_session(
        id_seed: impl Into<String>,
        session_id: SessionId,
        preferences: UiPreferences,
    ) -> Self {
        Self::with_session_and_request_namespace(
            id_seed,
            session_id,
            fresh_request_namespace(),
            preferences,
        )
    }

    #[must_use]
    pub fn with_session_and_request_namespace(
        id_seed: impl Into<String>,
        session_id: SessionId,
        request_namespace: impl Into<String>,
        preferences: UiPreferences,
    ) -> Self {
        Self {
            id_seed: id_seed.into(),
            request_namespace: request_namespace.into(),
            session_id,
            session_repository_id: None,
            session_json_path: None,
            next_request_sequence: 0,
            next_session_sequence: 0,
            preferences,
            focused_panel: Panel::Repository,
            input_mode: InputMode::RepositoryPath,
            repository_input: InputBuffer::default(),
            question_input: InputBuffer::default(),
            repository: RepositoryView::default(),
            activity: Activity::Idle,
            active_request: None,
            request_outcomes: BTreeMap::new(),
            cancel_targets: BTreeMap::new(),
            pending_index_paths: BTreeMap::new(),
            latest_index_request: None,
            conversation: Vec::new(),
            evidence: Vec::new(),
            selected_evidence: None,
            evidence_viewer: None,
            pending_source_requests: BTreeSet::new(),
            tools: Vec::new(),
            selected_tool: None,
            progress_trace: Vec::new(),
            last_progress: None,
            selected_task: None,
            history: HistoryView::default(),
            pending_history_request: None,
            pending_session_request: None,
            pending_diagram_opens: BTreeMap::new(),
            diagram_open_status: BTreeMap::new(),
            usage_by_request: BTreeMap::new(),
            repository_scroll: 0,
            conversation_scroll: 0,
            conversation_follow_tail: true,
            evidence_scroll: 0,
            error: None,
            error_details_visible: false,
            error_scroll: 0,
            clipboard_request: None,
            clipboard_status: None,
            quit_requested: false,
        }
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub const fn session_repository_id(&self) -> Option<RepositoryId> {
        self.session_repository_id
    }

    #[must_use]
    pub fn session_json_path(&self) -> Option<&str> {
        self.session_json_path.as_deref()
    }

    #[must_use]
    pub const fn selected_task(&self) -> Option<RequestId> {
        self.selected_task
    }

    #[must_use]
    pub const fn history_view(&self) -> Option<&HistoryView> {
        if self.history.visible {
            Some(&self.history)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn preferences(&self) -> &UiPreferences {
        &self.preferences
    }

    pub const fn preferences_mut(&mut self) -> &mut UiPreferences {
        &mut self.preferences
    }

    #[must_use]
    pub const fn focused_panel(&self) -> Panel {
        self.focused_panel
    }

    #[must_use]
    pub const fn input_mode(&self) -> InputMode {
        self.input_mode
    }

    #[must_use]
    pub const fn activity(&self) -> Activity {
        self.activity
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryView {
        &self.repository
    }

    #[must_use]
    pub fn conversation(&self) -> &[ConversationEntry] {
        &self.conversation
    }

    #[must_use]
    pub fn evidence(&self) -> &[Evidence] {
        &self.evidence
    }

    pub(crate) fn evidence_claim_contexts(
        &self,
        evidence_id: EvidenceId,
    ) -> Vec<EvidenceClaimContext> {
        let mut numbers = ClaimNumbers::default();
        let mut contexts = Vec::new();
        for answer in self.conversation.iter().filter_map(|entry| match entry {
            ConversationEntry::Answer(answer) if answer.complete => Some(answer),
            ConversationEntry::Question { .. } | ConversationEntry::Answer(_) => None,
        }) {
            for claim in &answer.claims {
                let label = numbers.next_label(claim.kind);
                if claim.evidence_ids.contains(&evidence_id) {
                    contexts.push(EvidenceClaimContext {
                        label,
                        kind: claim.kind,
                        text: claim.text.clone(),
                    });
                }
            }
        }
        contexts
    }

    #[must_use]
    pub fn tools(&self) -> &[ToolTrace] {
        &self.tools
    }

    #[must_use]
    pub fn progress_trace(&self) -> &[ProgressTrace] {
        &self.progress_trace
    }

    #[must_use]
    pub const fn last_progress(&self) -> Option<&Progress> {
        self.last_progress.as_ref()
    }

    #[must_use]
    pub const fn error(&self) -> Option<&UiError> {
        self.error.as_ref()
    }

    #[must_use]
    pub const fn error_details_visible(&self) -> bool {
        self.error_details_visible
    }

    #[must_use]
    pub const fn error_scroll(&self) -> u16 {
        self.error_scroll
    }

    #[must_use]
    pub fn error_report(&self) -> Option<String> {
        let error = self.error.as_ref()?;
        let request = error
            .request_id
            .map_or_else(|| "none".to_owned(), |request_id| request_id.to_string());
        Some(format!(
            "CodeAtlas error\ncode: {}\nrequest_id: {request}\nretryable: {}\n\n{}",
            error.code, error.retryable, error.message
        ))
    }

    #[must_use]
    pub fn clipboard_status(&self) -> Option<&str> {
        self.clipboard_status.as_deref()
    }

    #[must_use]
    pub(crate) fn diagram_open_status(&self, request_id: RequestId) -> Option<&str> {
        self.diagram_open_status
            .get(&request_id)
            .map(String::as_str)
    }

    #[must_use]
    pub const fn is_quit_requested(&self) -> bool {
        self.quit_requested
    }

    #[must_use]
    pub fn active_request_id(&self) -> Option<RequestId> {
        self.active_request.as_ref().map(|active| active.request_id)
    }

    #[must_use]
    pub fn selected_evidence(&self) -> Option<&Evidence> {
        self.selected_evidence
            .and_then(|index| self.evidence.get(index))
    }

    #[must_use]
    pub const fn evidence_viewer(&self) -> Option<&EvidenceViewer> {
        self.evidence_viewer.as_ref()
    }

    #[must_use]
    pub fn total_token_usage(&self) -> TokenUsage {
        self.usage_by_request
            .values()
            .fold(TokenUsage::default(), |mut total, usage| {
                total.input_tokens = total.input_tokens.saturating_add(usage.tokens.input_tokens);
                total.output_tokens = total
                    .output_tokens
                    .saturating_add(usage.tokens.output_tokens);
                total.cached_input_tokens = total
                    .cached_input_tokens
                    .saturating_add(usage.tokens.cached_input_tokens);
                total.total_tokens = total.total_tokens.saturating_add(usage.tokens.total_tokens);
                total
            })
    }

    #[must_use]
    pub fn total_cost(&self) -> Option<Cost> {
        let mut currency: Option<&str> = None;
        let mut amount = 0.0;
        let mut estimated = false;
        let mut found = false;
        for cost in self
            .usage_by_request
            .values()
            .filter_map(|usage| usage.cost.as_ref())
        {
            if currency.is_some_and(|known| known != cost.currency) {
                return None;
            }
            currency = Some(cost.currency.as_str());
            amount += cost.amount;
            estimated |= cost.estimated;
            found = true;
        }
        found.then(|| Cost {
            currency: currency.unwrap_or_default().to_owned(),
            amount,
            estimated,
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> TuiSnapshot {
        TuiSnapshot {
            session_id: self.session_id,
            repository: self.repository.clone(),
            activity: self.activity,
            focused_panel: self.focused_panel,
            input_mode: self.input_mode,
            active_request_id: self.active_request_id(),
            active_request_kind: self.active_request.as_ref().map(|active| active.kind),
            token_usage: self.total_token_usage(),
            cost: self.total_cost(),
            conversation_entries: u64::try_from(self.conversation.len()).unwrap_or(u64::MAX),
            evidence_items: u64::try_from(self.evidence.len()).unwrap_or(u64::MAX),
            error: self.error.clone(),
        }
    }

    #[must_use]
    pub fn layout_mode(&self, width: u16, height: u16) -> LayoutMode {
        if width < MIN_STANDARD_WIDTH || height < MIN_STANDARD_HEIGHT {
            LayoutMode::Compact
        } else if width >= self.preferences.wide_layout_min_width.max(MIN_WIDE_WIDTH)
            && height >= MIN_WIDE_HEIGHT
        {
            LayoutMode::Wide
        } else {
            LayoutMode::Tabbed
        }
    }

    pub fn set_repository_path(&mut self, path: impl Into<String>) {
        self.repository_input.set(path);
    }

    pub fn set_question(&mut self, question: impl Into<String>) {
        self.question_input.set(question);
    }

    pub fn begin_repository_input(&mut self) {
        self.clear_error();
        self.input_mode = InputMode::RepositoryPath;
    }

    pub fn begin_question_input(&mut self) {
        self.clear_error();
        self.input_mode = InputMode::Question;
        self.focused_panel = Panel::Conversation;
    }

    pub fn clear_error(&mut self) {
        self.error = None;
        self.error_details_visible = false;
        self.error_scroll = 0;
        self.clipboard_request = None;
        self.clipboard_status = None;
        if self.activity == Activity::Error {
            self.activity = self.active_request.as_ref().map_or_else(
                || {
                    if self.repository.repository_id.is_some() {
                        Activity::Indexed
                    } else {
                        Activity::Idle
                    }
                },
                |active| {
                    if active.cancelling {
                        Activity::Cancelling
                    } else {
                        match active.kind {
                            RequestKind::Index => Activity::IndexQueued,
                            RequestKind::Ask => Activity::AskQueued,
                        }
                    }
                },
            );
        }
    }

    /// Applies one keyboard event and returns at most one core command.
    #[must_use]
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            return self.cancel_or_quit();
        }
        if self.error_details_visible {
            return self.handle_error_details_key(key);
        }
        if matches!(key.code, KeyCode::Char('x')) && self.error.is_some() {
            self.error_details_visible = true;
            self.error_scroll = 0;
            return None;
        }
        if self.evidence_viewer.is_some() {
            return self.handle_evidence_viewer_key(key);
        }
        if self.history.visible {
            return self.handle_history_key(key);
        }

        if self.input_mode == InputMode::Navigation {
            self.handle_navigation_key(key)
        } else {
            self.handle_input_key(key)
        }
    }

    #[must_use]
    pub fn submit_repository(&mut self) -> Option<AppCommand> {
        if self.active_request.is_some() {
            self.set_local_error(
                "busy",
                "Cancel the active request before indexing another repository",
            );
            return None;
        }
        let path = self.repository_input.value().trim().to_owned();
        if path.is_empty() {
            self.set_local_error(
                "repository_path_required",
                "Enter a repository path to index",
            );
            return None;
        }

        let request_id = self.next_request_id("index");
        self.pending_index_paths.insert(request_id, path.clone());
        self.latest_index_request = Some(request_id);
        self.active_request = Some(ActiveRequest {
            request_id,
            kind: RequestKind::Index,
            cancelling: false,
            cancel_request_id: None,
        });
        self.activity = Activity::IndexQueued;
        self.last_progress = None;
        self.input_mode = InputMode::Navigation;
        self.error = None;
        Some(AppCommand::Index {
            request_id,
            repository_root: path,
        })
    }

    #[must_use]
    pub fn submit_question(&mut self) -> Option<AppCommand> {
        if self.active_request.is_some() {
            self.set_local_error(
                "busy",
                "Cancel the active request before asking another question",
            );
            return None;
        }
        let Some(repository_id) = self.repository.repository_id else {
            self.set_local_error(
                "repository_not_indexed",
                "Index a repository before asking a question",
            );
            return None;
        };
        if self.session_repository_id != Some(repository_id) {
            self.set_local_error(
                "session_repository_mismatch",
                "Load a history session for this repository, or index it again to start a new session",
            );
            return None;
        }
        let question = self.question_input.value().trim().to_owned();
        if question.is_empty() {
            self.set_local_error("question_required", "Enter a codebase question");
            return None;
        }

        let request_id = self.next_request_id("ask");
        self.conversation.push(ConversationEntry::Question {
            request_id,
            text: question.clone(),
        });
        self.active_request = Some(ActiveRequest {
            request_id,
            kind: RequestKind::Ask,
            cancelling: false,
            cancel_request_id: None,
        });
        self.activity = Activity::AskQueued;
        self.last_progress = None;
        self.selected_task = Some(request_id);
        self.selected_tool = None;
        self.repository_scroll = 0;
        self.input_mode = InputMode::Navigation;
        self.focused_panel = Panel::Conversation;
        self.question_input.clear();
        self.conversation_follow_tail = true;
        self.error = None;
        Some(AppCommand::Ask {
            request_id,
            session_id: self.session_id,
            repository_id,
            question,
        })
    }

    #[must_use]
    pub fn cancel_active(&mut self) -> Option<AppCommand> {
        let Some(active) = self.active_request.as_ref() else {
            self.set_local_error("nothing_to_cancel", "There is no active request");
            return None;
        };
        if active.cancelling {
            return None;
        }
        let target_request_id = active.request_id;
        let request_id = self.next_request_id("cancel");
        if let Some(active) = self.active_request.as_mut() {
            active.cancelling = true;
            active.cancel_request_id = Some(request_id);
        }
        self.cancel_targets.insert(request_id, target_request_id);
        self.activity = Activity::Cancelling;
        self.error = None;
        Some(AppCommand::Cancel {
            request_id,
            target_request_id,
        })
    }

    #[must_use]
    fn open_selected_diagram(&mut self) -> Option<AppCommand> {
        let Some((task_request_id, artifact)) = self.selected_diagram_artifact() else {
            self.set_local_error(
                "diagram_unavailable",
                "The selected task has no generated SVG diagram",
            );
            return None;
        };
        let request_id = self.next_request_id("open-diagram");
        self.pending_diagram_opens
            .insert(request_id, (task_request_id, artifact.id));
        self.diagram_open_status.insert(
            task_request_id,
            "Opening in the default viewer...".to_owned(),
        );
        self.clear_error();
        Some(AppCommand::OpenDiagram {
            request_id,
            diagram_id: artifact.id,
        })
    }

    fn request_diagram_path_copy(&mut self) {
        let Some((_, artifact)) = self.selected_diagram_artifact() else {
            self.set_local_error(
                "diagram_unavailable",
                "The selected task has no generated SVG diagram",
            );
            return;
        };
        self.clipboard_request = Some(artifact.path);
        self.clipboard_status = Some("Copying SVG path to clipboard...".to_owned());
    }

    /// Reduces one backend event into presentation state.
    pub fn reduce(&mut self, event: AppEvent) {
        match event {
            AppEvent::Progress {
                request_id,
                progress,
            } => self.reduce_progress(request_id, progress),
            AppEvent::ToolCallStarted { request_id, call } => {
                self.reduce_tool_started(
                    request_id,
                    call.id,
                    call.name,
                    call.arguments.to_string(),
                );
            }
            AppEvent::ToolCallCompleted { request_id, output } => self.reduce_tool_completed(
                request_id,
                output.call_id,
                output.result.to_string(),
                output.is_error,
            ),
            AppEvent::EvidenceAdded { .. } => {}
            AppEvent::AnswerDelta { request_id, delta } => {
                self.reduce_answer_delta(request_id, &delta);
            }
            AppEvent::AnswerCompleted { request_id, answer } => {
                self.reduce_answer_completed(request_id, answer);
            }
            AppEvent::UsageUpdated { request_id, usage } => {
                self.reduce_usage(request_id, usage);
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
            } => self.reduce_sessions_listed(request_id, sessions),
            AppEvent::SessionLoaded {
                request_id,
                session,
            } => self.reduce_session_loaded(request_id, session),
            AppEvent::DiagramOpened {
                request_id,
                diagram_id,
            } => self.reduce_diagram_opened(request_id, diagram_id),
            AppEvent::Error { request_id, error } => self.reduce_error(request_id, error),
        }
    }

    fn handle_navigation_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
        match key.code {
            KeyCode::Char('q') => self.quit_requested = true,
            KeyCode::Char('x') if self.error.is_some() => {
                self.error_details_visible = true;
                self.error_scroll = 0;
            }
            KeyCode::Char('i') => self.begin_repository_input(),
            KeyCode::Char('h') => return Some(self.open_history()),
            KeyCode::Char('N') => self.start_new_session(),
            KeyCode::Char('o') => return self.open_selected_diagram(),
            KeyCode::Char('y') => self.request_diagram_path_copy(),
            KeyCode::Char('[') => self.select_adjacent_task(false),
            KeyCode::Char(']') => self.select_adjacent_task(true),
            KeyCode::Enter | KeyCode::Char('v') if self.focused_panel == Panel::Evidence => {
                return self.open_selected_evidence();
            }
            KeyCode::Char('a' | '?') | KeyCode::Enter => self.begin_question_input(),
            KeyCode::Char('c') | KeyCode::Esc => return self.cancel_active_if_present(),
            KeyCode::Tab | KeyCode::Right => self.focused_panel = self.focused_panel.next(),
            KeyCode::BackTab | KeyCode::Left => {
                self.focused_panel = self.focused_panel.previous();
            }
            KeyCode::Char('1' | 'r') => self.focused_panel = Panel::Repository,
            KeyCode::Char('2') => self.focused_panel = Panel::Conversation,
            KeyCode::Char('3' | 'e') => self.focused_panel = Panel::Evidence,
            KeyCode::Char('p') if self.focused_panel == Panel::Repository => {
                self.select_previous_tool();
            }
            KeyCode::Char('n') if self.focused_panel == Panel::Repository => {
                self.select_next_tool();
            }
            KeyCode::Char('t') if self.focused_panel == Panel::Repository => {
                self.preferences.show_tool_payloads = !self.preferences.show_tool_payloads;
                self.repository_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.navigate_up(),
            KeyCode::Down | KeyCode::Char('j') => self.navigate_down(),
            KeyCode::PageUp => self.page_up(),
            KeyCode::PageDown => self.page_down(),
            KeyCode::Home => self.scroll_home(),
            KeyCode::End => self.scroll_end(),
            _ => {}
        }
        None
    }

    fn handle_history_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
        match key.code {
            KeyCode::Char('q') => self.quit_requested = true,
            KeyCode::Esc | KeyCode::Char('h') => self.close_history(),
            KeyCode::Char('N') => self.start_new_session(),
            KeyCode::Up | KeyCode::Char('k') => self.select_history_session(false, 1),
            KeyCode::Down | KeyCode::Char('j') => self.select_history_session(true, 1),
            KeyCode::PageUp => self.select_history_session(false, 8),
            KeyCode::PageDown => self.select_history_session(true, 8),
            KeyCode::Home => self.select_history_boundary(false),
            KeyCode::End => self.select_history_boundary(true),
            KeyCode::Enter => return self.load_selected_history_session(),
            _ => {}
        }
        None
    }

    fn handle_evidence_viewer_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
        match key.code {
            KeyCode::Char('q') => self.quit_requested = true,
            KeyCode::Esc | KeyCode::Char('v') => self.close_evidence_viewer(),
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(viewer) = self.evidence_viewer.as_mut() {
                    viewer.vertical_scroll = viewer.vertical_scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(viewer) = self.evidence_viewer.as_mut() {
                    viewer.vertical_scroll = viewer.vertical_scroll.saturating_add(1);
                }
            }
            KeyCode::PageUp => {
                if let Some(viewer) = self.evidence_viewer.as_mut() {
                    viewer.vertical_scroll = viewer.vertical_scroll.saturating_sub(8);
                }
            }
            KeyCode::PageDown => {
                if let Some(viewer) = self.evidence_viewer.as_mut() {
                    viewer.vertical_scroll = viewer.vertical_scroll.saturating_add(8);
                }
            }
            KeyCode::Home => {
                if let Some(viewer) = self.evidence_viewer.as_mut() {
                    viewer.vertical_scroll = 0;
                }
            }
            KeyCode::End => {
                if let Some(viewer) = self.evidence_viewer.as_mut() {
                    viewer.vertical_scroll = u16::MAX;
                }
            }
            KeyCode::Char('n') => return self.switch_evidence(true),
            KeyCode::Char('p') => return self.switch_evidence(false),
            KeyCode::Char('w') => {
                if let Some(viewer) = self.evidence_viewer.as_mut() {
                    viewer.wrap = !viewer.wrap;
                    viewer.horizontal_scroll = 0;
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some(viewer) = self.evidence_viewer.as_mut().filter(|viewer| !viewer.wrap) {
                    viewer.horizontal_scroll = viewer.horizontal_scroll.saturating_sub(1);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(viewer) = self.evidence_viewer.as_mut().filter(|viewer| !viewer.wrap) {
                    viewer.horizontal_scroll = viewer.horizontal_scroll.saturating_add(1);
                }
            }
            KeyCode::Char('y') => self.request_source_copy(),
            _ => {}
        }
        None
    }

    fn handle_error_details_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
        match key.code {
            KeyCode::Char('q') => self.quit_requested = true,
            KeyCode::Esc | KeyCode::Char('x') => self.error_details_visible = false,
            KeyCode::Char('y') => {
                self.clipboard_request = self.error_report();
                self.clipboard_status = Some("Copying full error to clipboard...".to_owned());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.error_scroll = self.error_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.error_scroll = self.error_scroll.saturating_add(1);
            }
            KeyCode::PageUp => self.error_scroll = self.error_scroll.saturating_sub(8),
            KeyCode::PageDown => self.error_scroll = self.error_scroll.saturating_add(8),
            KeyCode::Home => self.error_scroll = 0,
            KeyCode::End => self.error_scroll = u16::MAX,
            _ => {}
        }
        None
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Option<AppCommand> {
        match key.code {
            KeyCode::Enter => {
                return match self.input_mode {
                    InputMode::RepositoryPath => self.submit_repository(),
                    InputMode::Question => self.submit_question(),
                    InputMode::Navigation => None,
                };
            }
            KeyCode::Esc => {
                self.input_mode = InputMode::Navigation;
                self.clear_error();
            }
            KeyCode::Backspace => self.active_input_mut().backspace(),
            KeyCode::Delete => self.active_input_mut().delete(),
            KeyCode::Left => self.active_input_mut().move_left(),
            KeyCode::Right => self.active_input_mut().move_right(),
            KeyCode::Home => self.active_input_mut().cursor = 0,
            KeyCode::End => {
                let input = self.active_input_mut();
                input.cursor = input.value.len();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.active_input_mut().clear();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.active_input_mut().insert(character);
                self.clear_error();
            }
            _ => {}
        }
        None
    }

    fn active_input_mut(&mut self) -> &mut InputBuffer {
        match self.input_mode {
            InputMode::RepositoryPath => &mut self.repository_input,
            InputMode::Question | InputMode::Navigation => &mut self.question_input,
        }
    }

    fn open_selected_evidence(&mut self) -> Option<AppCommand> {
        let index = self.selected_evidence?;
        let evidence = self.evidence.get(index)?.clone();
        let (start_line, end_line) = evidence_line_range(&evidence);
        let content = evidence.excerpt.unwrap_or_default();
        let content_end_line = content_end_line(start_line, &content);
        let mut viewer = EvidenceViewer {
            evidence_id: evidence.id,
            evidence_index: index,
            path: evidence.path.clone(),
            start_line,
            end_line,
            symbol_id: evidence.symbol_id,
            content,
            content_start_line: start_line,
            content_end_line,
            source_state: SourceLoadState::Excerpt,
            wrap: self.preferences.wrap_evidence,
            vertical_scroll: 0,
            horizontal_scroll: 0,
        };
        self.clipboard_request = None;
        self.clipboard_status = None;

        let command = self
            .repository
            .repository_id
            .filter(|repository_id| self.session_repository_id == Some(*repository_id))
            .map(|repository_id| {
                let request_id = self.next_request_id("source");
                viewer.source_state = SourceLoadState::Loading(PendingSourceLoad {
                    request_id,
                    repository_id,
                    path: evidence.path.clone(),
                    start_line,
                });
                self.pending_source_requests.insert(request_id);
                AppCommand::LoadSource {
                    request_id,
                    repository_id,
                    path: evidence.path,
                    start_line,
                    end_line: Some(end_line),
                }
            });
        self.evidence_viewer = Some(viewer);
        command
    }

    fn switch_evidence(&mut self, next: bool) -> Option<AppCommand> {
        let last = self.evidence.len().checked_sub(1)?;
        let current = self.selected_evidence.unwrap_or(0).min(last);
        let selected = if next {
            current.saturating_add(1).min(last)
        } else {
            current.saturating_sub(1)
        };
        if selected == current {
            return None;
        }
        self.selected_evidence = Some(selected);
        self.evidence_scroll = 0;
        self.open_selected_evidence()
    }

    fn close_evidence_viewer(&mut self) {
        self.evidence_viewer = None;
        self.clipboard_request = None;
        self.clipboard_status = None;
    }

    fn request_source_copy(&mut self) {
        let Some(content) = self
            .evidence_viewer
            .as_ref()
            .map(|viewer| viewer.content.clone())
            .filter(|content| !content.is_empty())
        else {
            self.clipboard_status = Some("No source content to copy.".to_owned());
            return;
        };
        self.clipboard_request = Some(content);
        self.clipboard_status = Some("Copying source to clipboard...".to_owned());
    }

    fn open_history(&mut self) -> AppCommand {
        let request_id = self.next_request_id("list-sessions");
        self.history.visible = true;
        self.history.loading = true;
        self.history.scroll = 0;
        self.pending_history_request = Some(request_id);
        AppCommand::ListSessions {
            request_id,
            repository_id: self.repository.repository_id,
        }
    }

    fn select_history_session(&mut self, next: bool, amount: usize) {
        let count = self.history.sessions.len();
        if count == 0 {
            self.history.selected_session = None;
            return;
        }
        let current = self.history.selected_session.unwrap_or(0).min(count - 1);
        self.history.selected_session = Some(if next {
            current.saturating_add(amount).min(count - 1)
        } else {
            current.saturating_sub(amount)
        });
    }

    fn select_history_boundary(&mut self, end: bool) {
        let count = self.history.sessions.len();
        self.history.selected_session = (count > 0).then_some(if end { count - 1 } else { 0 });
    }

    fn load_selected_history_session(&mut self) -> Option<AppCommand> {
        if self.pending_session_request.is_some() {
            return None;
        }
        if self.active_request.is_some() {
            self.close_history();
            self.set_local_error(
                "busy",
                "Cancel the active request before loading a historical session",
            );
            return None;
        }
        let selected = self.history.selected_session?;
        let session_id = self.history.sessions.get(selected)?.session_id;
        let request_id = self.next_request_id("load-session");
        self.pending_session_request = Some(request_id);
        self.history.loading = true;
        Some(AppCommand::LoadSession {
            request_id,
            session_id,
        })
    }

    fn close_history(&mut self) {
        self.history.visible = false;
        self.history.loading = false;
        self.pending_history_request = None;
        self.pending_session_request = None;
    }

    fn start_new_session(&mut self) {
        if self.active_request.is_some() {
            self.close_history();
            self.set_local_error(
                "busy",
                "Cancel the active request before starting a new session",
            );
            return;
        }
        let Some(repository_id) = self.repository.repository_id else {
            self.close_history();
            self.set_local_error(
                "repository_not_indexed",
                "Index a repository before starting a new session",
            );
            return;
        };

        self.close_history();
        self.clear_session_context();
        self.session_id = self.fresh_session_id(repository_id);
        self.session_repository_id = Some(repository_id);
        self.activity = Activity::Indexed;
        self.input_mode = InputMode::Navigation;
        self.focused_panel = Panel::Conversation;
        self.clear_error();
    }

    fn cancel_or_quit(&mut self) -> Option<AppCommand> {
        if self.active_request.is_some() {
            self.cancel_active()
        } else {
            self.quit_requested = true;
            None
        }
    }

    fn cancel_active_if_present(&mut self) -> Option<AppCommand> {
        if self.active_request.is_some() {
            self.cancel_active()
        } else {
            self.clear_error();
            None
        }
    }

    fn navigate_up(&mut self) {
        match self.focused_panel {
            Panel::Repository => {
                self.repository_scroll = self.repository_scroll.saturating_sub(1);
            }
            Panel::Conversation => {
                self.conversation_follow_tail = false;
                self.conversation_scroll = self.conversation_scroll.saturating_sub(1);
            }
            Panel::Evidence => self.select_previous_evidence(),
        }
    }

    fn navigate_down(&mut self) {
        match self.focused_panel {
            Panel::Repository => {
                self.repository_scroll = self.repository_scroll.saturating_add(1);
            }
            Panel::Conversation => {
                if !self.conversation_follow_tail {
                    self.conversation_scroll = self.conversation_scroll.saturating_add(1);
                }
            }
            Panel::Evidence => self.select_next_evidence(),
        }
    }

    fn page_up(&mut self) {
        match self.focused_panel {
            Panel::Repository => {
                self.repository_scroll = self.repository_scroll.saturating_sub(8);
            }
            Panel::Conversation => {
                self.conversation_follow_tail = false;
                self.conversation_scroll = self.conversation_scroll.saturating_sub(8);
            }
            Panel::Evidence => self.evidence_scroll = self.evidence_scroll.saturating_sub(8),
        }
    }

    fn page_down(&mut self) {
        match self.focused_panel {
            Panel::Repository => {
                self.repository_scroll = self.repository_scroll.saturating_add(8);
            }
            Panel::Conversation => {
                if !self.conversation_follow_tail {
                    self.conversation_scroll = self.conversation_scroll.saturating_add(8);
                }
            }
            Panel::Evidence => self.evidence_scroll = self.evidence_scroll.saturating_add(8),
        }
    }

    fn scroll_home(&mut self) {
        match self.focused_panel {
            Panel::Repository => self.repository_scroll = 0,
            Panel::Conversation => {
                self.conversation_follow_tail = false;
                self.conversation_scroll = 0;
            }
            Panel::Evidence => self.evidence_scroll = 0,
        }
    }

    fn scroll_end(&mut self) {
        match self.focused_panel {
            Panel::Repository => self.repository_scroll = u16::MAX,
            Panel::Conversation => self.conversation_follow_tail = true,
            Panel::Evidence => self.evidence_scroll = u16::MAX,
        }
    }

    fn select_previous_evidence(&mut self) {
        if self.evidence.is_empty() {
            self.selected_evidence = None;
        } else {
            self.selected_evidence = Some(self.selected_evidence.unwrap_or(0).saturating_sub(1));
            self.evidence_scroll = 0;
        }
    }

    fn select_next_evidence(&mut self) {
        if self.evidence.is_empty() {
            self.selected_evidence = None;
        } else {
            let last = self.evidence.len() - 1;
            self.selected_evidence = Some(
                self.selected_evidence
                    .unwrap_or(0)
                    .saturating_add(1)
                    .min(last),
            );
            self.evidence_scroll = 0;
        }
    }

    fn select_previous_tool(&mut self) {
        let candidates = self.task_tool_indices();
        if candidates.is_empty() {
            self.selected_tool = None;
        } else {
            let current = self
                .selected_tool
                .and_then(|selected| candidates.iter().position(|index| *index == selected))
                .unwrap_or(0);
            self.selected_tool = Some(candidates[current.saturating_sub(1)]);
        }
    }

    fn select_next_tool(&mut self) {
        let candidates = self.task_tool_indices();
        if candidates.is_empty() {
            self.selected_tool = None;
        } else {
            let current = self
                .selected_tool
                .and_then(|selected| candidates.iter().position(|index| *index == selected))
                .unwrap_or(0);
            self.selected_tool =
                Some(candidates[current.saturating_add(1).min(candidates.len() - 1)]);
        }
    }

    fn task_tool_indices(&self) -> Vec<usize> {
        self.tools
            .iter()
            .enumerate()
            .filter_map(|(index, tool)| {
                self.selected_task
                    .is_none_or(|request_id| tool.request_id == request_id)
                    .then_some(index)
            })
            .collect()
    }

    fn select_adjacent_task(&mut self, next: bool) {
        let tasks = self.task_request_ids();
        let Some(last) = tasks.len().checked_sub(1) else {
            self.selected_task = None;
            return;
        };
        let current = self
            .selected_task
            .and_then(|selected| tasks.iter().position(|request_id| *request_id == selected))
            .unwrap_or(last);
        let selected = if next {
            current.saturating_add(1).min(last)
        } else {
            current.saturating_sub(1)
        };
        self.select_task_with_evidence(tasks[selected]);
    }

    fn select_task(&mut self, request_id: RequestId) {
        self.selected_task = Some(request_id);
        self.selected_tool = self
            .tools
            .iter()
            .position(|tool| tool.request_id == request_id);
        self.last_progress = self
            .progress_trace
            .iter()
            .rev()
            .find(|entry| entry.request_id == request_id)
            .map(|entry| entry.progress.clone());
        self.repository_scroll = 0;
    }

    fn select_task_with_evidence(&mut self, request_id: RequestId) {
        self.select_task(request_id);
        let evidence_id = self.conversation.iter().find_map(|entry| match entry {
            ConversationEntry::Answer(answer) if answer.request_id == request_id => {
                first_answer_evidence(answer)
            }
            ConversationEntry::Question { .. } | ConversationEntry::Answer(_) => None,
        });
        self.selected_evidence = evidence_id.and_then(|evidence_id| {
            self.evidence
                .iter()
                .position(|evidence| evidence.id == evidence_id)
        });
        if self.selected_evidence.is_some() {
            self.evidence_scroll = 0;
        }
    }

    fn task_request_ids(&self) -> Vec<RequestId> {
        let mut tasks = Vec::new();
        for entry in &self.conversation {
            let request_id = match entry {
                ConversationEntry::Question { request_id, .. } => *request_id,
                ConversationEntry::Answer(answer) => answer.request_id,
            };
            if !tasks.contains(&request_id) {
                tasks.push(request_id);
            }
        }
        tasks
    }

    fn next_request_id(&mut self, purpose: &str) -> RequestId {
        let sequence = self.next_request_sequence.to_string();
        self.next_request_sequence = self.next_request_sequence.saturating_add(1);
        RequestId::from_stable_parts(&[
            "tui",
            self.id_seed.as_str(),
            self.request_namespace.as_str(),
            purpose,
            sequence.as_str(),
        ])
    }

    fn fresh_session_id(&mut self, repository_id: RepositoryId) -> SessionId {
        let sequence = self.next_session_sequence.to_string();
        self.next_session_sequence = self.next_session_sequence.saturating_add(1);
        let repository_id = repository_id.to_string();
        SessionId::from_stable_parts(&[
            "tui",
            self.id_seed.as_str(),
            self.request_namespace.as_str(),
            "repository-session",
            repository_id.as_str(),
            sequence.as_str(),
        ])
    }

    fn set_local_error(&mut self, code: &str, message: &str) {
        self.error = Some(UiError {
            request_id: None,
            code: code.to_owned(),
            message: message.to_owned(),
            retryable: true,
        });
        self.error_details_visible = message.chars().count() > 120;
        self.error_scroll = 0;
        self.clipboard_status = None;
        self.activity = Activity::Error;
    }

    fn reduce_progress(&mut self, request_id: RequestId, progress: Progress) {
        if self.request_outcomes.contains_key(&request_id) {
            return;
        }
        let duplicate = self
            .progress_trace
            .iter()
            .any(|entry| entry.request_id == request_id && entry.progress == progress);
        if !duplicate {
            self.progress_trace.push(ProgressTrace {
                request_id,
                progress: progress.clone(),
            });
        }
        if self
            .active_request
            .as_ref()
            .is_none_or(|active| active.request_id == request_id)
        {
            self.activity = Activity::Running(progress.phase);
            self.last_progress = Some(progress);
        }
    }

    fn reduce_tool_started(
        &mut self,
        request_id: RequestId,
        call_id: ToolCallId,
        name: String,
        arguments: String,
    ) {
        if let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.request_id == request_id && tool.call_id == call_id)
        {
            tool.name = Some(name);
            tool.arguments = Some(arguments);
            if tool.output.is_none() {
                tool.status = ToolTraceStatus::Running;
            }
            return;
        }
        self.tools.push(ToolTrace {
            request_id,
            call_id,
            name: Some(name),
            arguments: Some(arguments),
            output: None,
            status: ToolTraceStatus::Running,
        });
        let has_selected_tool = self
            .selected_tool
            .and_then(|index| self.tools.get(index))
            .is_some_and(|tool| tool.request_id == request_id);
        if !has_selected_tool
            && (self.selected_task == Some(request_id) || self.selected_task.is_none())
        {
            self.selected_tool = Some(self.tools.len() - 1);
        }
    }

    fn reduce_tool_completed(
        &mut self,
        request_id: RequestId,
        call_id: ToolCallId,
        output: String,
        is_error: bool,
    ) {
        let status = if is_error {
            ToolTraceStatus::Failed
        } else {
            ToolTraceStatus::Completed
        };
        if let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.request_id == request_id && tool.call_id == call_id)
        {
            tool.output = Some(output);
            tool.status = status;
            return;
        }
        self.tools.push(ToolTrace {
            request_id,
            call_id,
            name: None,
            arguments: None,
            output: Some(output),
            status,
        });
        let has_selected_tool = self
            .selected_tool
            .and_then(|index| self.tools.get(index))
            .is_some_and(|tool| tool.request_id == request_id);
        if !has_selected_tool
            && (self.selected_task == Some(request_id) || self.selected_task.is_none())
        {
            self.selected_tool = Some(self.tools.len() - 1);
        }
    }

    fn upsert_evidence(&mut self, evidence: Evidence) {
        if let Some(existing) = self
            .evidence
            .iter_mut()
            .find(|existing| existing.id == evidence.id)
        {
            *existing = evidence;
        } else {
            self.evidence.push(evidence);
            if self.selected_evidence.is_none() {
                self.selected_evidence = Some(0);
            }
        }
    }

    fn reduce_answer_delta(&mut self, request_id: RequestId, delta: &str) {
        if self.request_outcomes.contains_key(&request_id) {
            return;
        }
        let answer = self.answer_mut_or_insert(request_id);
        if !answer.complete {
            answer.text.push_str(delta);
        }
        self.selected_task = Some(request_id);
    }

    fn reduce_answer_completed(&mut self, request_id: RequestId, answer: AgentAnswer) {
        if self.request_outcomes.contains_key(&request_id) {
            return;
        }
        for evidence in answer.evidence {
            self.upsert_evidence(evidence);
        }
        if let Some(usage) = answer.usage {
            self.reduce_usage(request_id, usage);
        }
        let view = self.answer_mut_or_insert(request_id);
        view.text = answer.text;
        view.claims = answer.claims;
        view.call_paths = answer.call_paths;
        view.diagram = Some(answer.diagram);
        view.complete = true;
        let was_active = self.finish_request(request_id, RequestOutcome::Completed);
        if was_active || self.active_request.is_none() {
            self.activity = Activity::AnswerReady;
        }
        self.select_task(request_id);
    }

    fn answer_mut_or_insert(&mut self, request_id: RequestId) -> &mut AnswerView {
        let existing = self.conversation.iter().position(|entry| {
            matches!(entry, ConversationEntry::Answer(answer) if answer.request_id == request_id)
        });
        let index = existing.unwrap_or_else(|| {
            self.conversation
                .push(ConversationEntry::Answer(AnswerView {
                    request_id,
                    text: String::new(),
                    claims: Vec::new(),
                    call_paths: Vec::new(),
                    diagram: None,
                    complete: false,
                }));
            self.conversation.len() - 1
        });
        match &mut self.conversation[index] {
            ConversationEntry::Answer(answer) => answer,
            ConversationEntry::Question { .. } => {
                unreachable!("answer index must contain an answer")
            }
        }
    }

    fn reduce_usage(&mut self, request_id: RequestId, usage: ModelUsage) {
        match self.usage_by_request.entry(request_id) {
            Entry::Vacant(entry) => {
                entry.insert(usage);
            }
            Entry::Occupied(mut entry) => merge_usage(entry.get_mut(), usage),
        }
    }

    fn reduce_index_completed(
        &mut self,
        request_id: RequestId,
        repository_id: RepositoryId,
        file_count: u64,
        symbol_count: u64,
        repository_map: RepositoryMap,
    ) {
        if self.request_outcomes.contains_key(&request_id) {
            return;
        }
        let is_latest = self
            .latest_index_request
            .is_none_or(|latest| latest == request_id);
        if is_latest {
            self.close_history();
            self.clear_session_context();
            self.session_id = self.fresh_session_id(repository_id);
            self.session_repository_id = Some(repository_id);
            if let Some(path) = self.pending_index_paths.remove(&request_id) {
                self.repository.path = path;
            }
            self.repository.repository_id = Some(repository_id);
            self.repository.file_count = file_count;
            self.repository.symbol_count = symbol_count;
            self.repository.repository_map = repository_map;
        }
        let was_active = self.finish_request(request_id, RequestOutcome::Completed);
        if is_latest && (was_active || self.active_request.is_none()) {
            self.activity = Activity::Indexed;
        }
    }

    fn reduce_sessions_listed(&mut self, request_id: RequestId, sessions: Vec<SessionSummary>) {
        if self.pending_history_request != Some(request_id) {
            return;
        }
        self.pending_history_request = None;
        let current_session = self.session_id;
        self.history.sessions = sessions;
        self.history.loading = false;
        self.history.scroll = 0;
        self.history.selected_session = self
            .history
            .sessions
            .iter()
            .position(|session| session.session_id == current_session)
            .or((!self.history.sessions.is_empty()).then_some(0));
        self.history.visible = true;
    }

    fn reduce_session_loaded(&mut self, request_id: RequestId, session: SessionContext) {
        if self.pending_session_request != Some(request_id) {
            return;
        }
        self.pending_session_request = None;
        self.clear_session_context();
        self.session_id = session.session_id;
        self.session_repository_id = Some(session.repository_id);
        self.session_json_path = Some(session.json_path);

        for task in session.tasks {
            let request_id = task.request_id;
            self.conversation.push(ConversationEntry::Question {
                request_id,
                text: task.question,
            });
            for event in task.workflow {
                match event {
                    WorkflowEvent::Progress(progress) => {
                        self.reduce_progress(request_id, progress);
                    }
                    WorkflowEvent::ToolCallStarted(call) => self.reduce_tool_started(
                        request_id,
                        call.id,
                        call.name,
                        call.arguments.to_string(),
                    ),
                    WorkflowEvent::ToolCallCompleted {
                        call_id,
                        output,
                        is_error,
                    } => self.reduce_tool_completed(request_id, call_id, output, is_error),
                }
            }

            let AgentAnswer {
                id: _,
                text,
                claims,
                evidence,
                call_paths,
                diagram,
                usage,
            } = task.answer;
            for evidence in evidence {
                self.upsert_evidence(evidence);
            }
            if let Some(usage) = usage {
                self.usage_by_request.insert(request_id, usage);
            }
            self.conversation
                .push(ConversationEntry::Answer(AnswerView {
                    request_id,
                    text,
                    claims,
                    call_paths,
                    diagram: Some(diagram),
                    complete: true,
                }));
            self.request_outcomes
                .insert(request_id, RequestOutcome::Completed);
        }

        let tasks = self.task_request_ids();
        if let Some(request_id) = tasks.last().copied() {
            self.select_task_with_evidence(request_id);
        }
        self.activity = if self.conversation.is_empty() {
            Activity::Indexed
        } else {
            Activity::AnswerReady
        };
        self.history.loading = false;
        self.history.visible = false;
        self.conversation_follow_tail = true;
        self.clear_error();
    }

    fn clear_session_context(&mut self) {
        self.session_json_path = None;
        self.question_input.clear();
        self.conversation.clear();
        self.evidence.clear();
        self.selected_evidence = None;
        self.evidence_viewer = None;
        self.pending_source_requests.clear();
        self.tools.clear();
        self.selected_tool = None;
        self.progress_trace.clear();
        self.last_progress = None;
        self.selected_task = None;
        self.usage_by_request.clear();
        self.request_outcomes.clear();
        self.cancel_targets.clear();
        self.pending_diagram_opens.clear();
        self.diagram_open_status.clear();
        self.repository_scroll = 0;
        self.conversation_scroll = 0;
        self.conversation_follow_tail = true;
        self.evidence_scroll = 0;
        self.clipboard_request = None;
        self.clipboard_status = None;
    }

    fn reduce_source_loaded(
        &mut self,
        request_id: RequestId,
        repository_id: RepositoryId,
        path: &RepositoryPath,
        start_line: u32,
        end_line: u32,
        content: String,
    ) {
        self.pending_source_requests.remove(&request_id);
        let Some(viewer) = self.evidence_viewer.as_mut() else {
            return;
        };
        let SourceLoadState::Loading(pending) = &viewer.source_state else {
            return;
        };
        if pending.request_id != request_id
            || pending.repository_id != repository_id
            || pending.path != *path
            || pending.start_line != start_line
        {
            return;
        }

        viewer.content = content;
        viewer.content_start_line = start_line;
        viewer.content_end_line = end_line;
        viewer.source_state = SourceLoadState::Loaded;
        viewer.vertical_scroll = 0;
        viewer.horizontal_scroll = 0;
        self.clipboard_request = None;
        self.clipboard_status = None;
    }

    fn reduce_cancelled(&mut self, request_id: RequestId) {
        let target = self
            .cancel_targets
            .remove(&request_id)
            .unwrap_or(request_id);
        if self.request_outcomes.contains_key(&target) {
            return;
        }

        let related_cancel_ids: Vec<RequestId> = self
            .cancel_targets
            .iter()
            .filter_map(|(cancel_id, mapped_target)| {
                (*mapped_target == target).then_some(*cancel_id)
            })
            .collect();
        for cancel_id in related_cancel_ids {
            self.cancel_targets.remove(&cancel_id);
            self.request_outcomes
                .entry(cancel_id)
                .or_insert(RequestOutcome::Cancelled);
        }
        if request_id != target {
            self.request_outcomes
                .entry(request_id)
                .or_insert(RequestOutcome::Cancelled);
        }
        let was_active = self.finish_request(target, RequestOutcome::Cancelled);
        if was_active || self.active_request.is_none() {
            self.activity = Activity::Cancelled;
        }
    }

    fn reduce_diagram_opened(&mut self, request_id: RequestId, diagram_id: DiagramId) {
        let Some((task_request_id, expected_diagram_id)) =
            self.pending_diagram_opens.get(&request_id).copied()
        else {
            return;
        };
        if diagram_id != expected_diagram_id {
            return;
        }
        self.pending_diagram_opens.remove(&request_id);
        self.diagram_open_status
            .insert(task_request_id, "Opened in the default viewer.".to_owned());
    }

    fn reduce_error(&mut self, request_id: Option<RequestId>, error: AppError) {
        if let Some((task_request_id, _)) =
            request_id.and_then(|request_id| self.pending_diagram_opens.remove(&request_id))
        {
            self.diagram_open_status.insert(
                task_request_id,
                format!("Could not open SVG: {}", error.message),
            );
            return;
        }
        if let Some(request_id) = request_id
            && self.pending_source_requests.remove(&request_id)
        {
            let is_source_request = self.evidence_viewer.as_ref().is_some_and(|viewer| {
                matches!(
                    &viewer.source_state,
                    SourceLoadState::Loading(pending) if pending.request_id == request_id
                )
            });
            if is_source_request && let Some(viewer) = self.evidence_viewer.as_mut() {
                viewer.source_state = SourceLoadState::Failed {
                    message: error.message,
                };
            }
            self.clipboard_status = None;
            return;
        }

        if request_id.is_some_and(|request_id| {
            self.pending_history_request == Some(request_id)
                || self.pending_session_request == Some(request_id)
        }) {
            self.pending_history_request = None;
            self.pending_session_request = None;
            self.history.loading = false;
            self.history.visible = false;
        }

        let non_fatal_answer_degradation = matches!(
            error.code.as_str(),
            "diagram_generation_failed" | "session_update_failed" | "session_save_failed"
        );
        self.error = Some(UiError {
            request_id,
            code: error.code,
            message: error.message,
            retryable: error.retryable,
        });
        self.error_details_visible = true;
        self.error_scroll = 0;
        self.clipboard_status = None;

        // These failures happen after a valid model answer exists. Keep the
        // request active so the following AnswerCompleted event is accepted.
        if non_fatal_answer_degradation {
            return;
        }

        let Some(request_id) = request_id else {
            self.active_request = None;
            self.activity = Activity::Error;
            return;
        };
        let cancel_failed = self.active_request.as_ref().is_some_and(|active| {
            active.cancel_request_id == Some(request_id) && active.request_id != request_id
        });
        if cancel_failed {
            self.cancel_targets.remove(&request_id);
            self.request_outcomes
                .entry(request_id)
                .or_insert(RequestOutcome::Failed);
            if let Some(active) = self.active_request.as_mut() {
                active.cancelling = false;
                active.cancel_request_id = None;
            }
        } else {
            self.finish_request(request_id, RequestOutcome::Failed);
        }
        self.activity = Activity::Error;
    }

    fn finish_request(&mut self, request_id: RequestId, outcome: RequestOutcome) -> bool {
        match self.request_outcomes.entry(request_id) {
            Entry::Occupied(_) => return false,
            Entry::Vacant(entry) => {
                entry.insert(outcome);
            }
        }
        self.pending_index_paths.remove(&request_id);
        let is_active = self
            .active_request
            .as_ref()
            .is_some_and(|active| active.request_id == request_id);
        if is_active {
            self.active_request = None;
        }
        is_active
    }

    pub(crate) const fn repository_input(&self) -> &InputBuffer {
        &self.repository_input
    }

    pub(crate) const fn question_input(&self) -> &InputBuffer {
        &self.question_input
    }

    pub(crate) const fn repository_scroll(&self) -> u16 {
        self.repository_scroll
    }

    pub(crate) fn set_repository_scroll(&mut self, scroll: u16) {
        self.repository_scroll = scroll;
    }

    pub(crate) const fn conversation_scroll(&self) -> u16 {
        self.conversation_scroll
    }

    pub(crate) fn set_conversation_scroll(&mut self, scroll: u16) {
        self.conversation_scroll = scroll;
    }

    pub(crate) fn set_conversation_follow_tail(&mut self, follow_tail: bool) {
        self.conversation_follow_tail = follow_tail;
    }

    pub(crate) const fn conversation_follows_tail(&self) -> bool {
        self.conversation_follow_tail
    }

    pub(crate) const fn evidence_scroll(&self) -> u16 {
        self.evidence_scroll
    }

    pub(crate) fn set_evidence_scroll(&mut self, scroll: u16) {
        self.evidence_scroll = scroll;
    }

    pub(crate) fn set_evidence_viewer_scroll(&mut self, vertical: u16, horizontal: u16) {
        if let Some(viewer) = self.evidence_viewer.as_mut() {
            viewer.vertical_scroll = vertical;
            viewer.horizontal_scroll = horizontal;
        }
    }

    pub(crate) fn set_error_scroll(&mut self, scroll: u16) {
        self.error_scroll = scroll;
    }

    pub(crate) fn take_clipboard_request(&mut self) -> Option<String> {
        self.clipboard_request.take()
    }

    pub(crate) fn set_clipboard_status(&mut self, status: impl Into<String>) {
        self.clipboard_status = Some(status.into());
    }

    pub(crate) const fn selected_evidence_index(&self) -> Option<usize> {
        self.selected_evidence
    }

    pub(crate) const fn selected_tool_index(&self) -> Option<usize> {
        self.selected_tool
    }

    pub(crate) fn selected_progress(&self) -> impl Iterator<Item = &ProgressTrace> {
        let selected_task = self.selected_task;
        self.progress_trace.iter().filter(move |entry| {
            selected_task.is_none_or(|request_id| entry.request_id == request_id)
        })
    }

    pub(crate) fn selected_tools(&self) -> impl Iterator<Item = (usize, &ToolTrace)> {
        let selected_task = self.selected_task;
        self.tools.iter().enumerate().filter(move |(_, tool)| {
            selected_task.is_none_or(|request_id| tool.request_id == request_id)
        })
    }

    pub(crate) fn selected_answers(&self) -> impl Iterator<Item = &AnswerView> {
        let selected_task = self.selected_task;
        self.conversation
            .iter()
            .filter_map(move |entry| match entry {
                ConversationEntry::Answer(answer)
                    if selected_task.is_none_or(|request_id| answer.request_id == request_id) =>
                {
                    Some(answer)
                }
                ConversationEntry::Question { .. } | ConversationEntry::Answer(_) => None,
            })
    }

    fn selected_diagram_artifact(&self) -> Option<(RequestId, DiagramArtifact)> {
        let selected_task = self.selected_task?;
        self.conversation.iter().find_map(|entry| match entry {
            ConversationEntry::Answer(answer) if answer.request_id == selected_task => {
                let DiagramDecision::Needed { diagram, .. } = answer.diagram.as_ref()? else {
                    return None;
                };
                diagram
                    .artifact
                    .clone()
                    .map(|artifact| (selected_task, artifact))
            }
            ConversationEntry::Question { .. } | ConversationEntry::Answer(_) => None,
        })
    }

    pub(crate) fn selected_task_position(&self) -> Option<(usize, usize)> {
        let tasks = self.task_request_ids();
        let selected = self.selected_task?;
        tasks
            .iter()
            .position(|request_id| *request_id == selected)
            .map(|index| (index + 1, tasks.len()))
    }

    pub(crate) fn set_history_scroll(&mut self, scroll: u16) {
        self.history.scroll = scroll;
    }
}

fn fresh_request_namespace() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TUI_INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp}-{sequence}", process::id())
}

fn first_answer_evidence(answer: &AnswerView) -> Option<EvidenceId> {
    if let Some(evidence_id) = answer
        .claims
        .iter()
        .flat_map(|claim| &claim.evidence_ids)
        .next()
    {
        return Some(*evidence_id);
    }
    if let Some(evidence_id) = answer
        .call_paths
        .iter()
        .flat_map(|path| &path.steps)
        .flat_map(|step| &step.evidence_ids)
        .next()
    {
        return Some(*evidence_id);
    }
    let Some(DiagramDecision::Needed { diagram, .. }) = answer.diagram.as_ref() else {
        return None;
    };
    diagram
        .nodes
        .iter()
        .flat_map(|node| &node.evidence_ids)
        .chain(diagram.edges.iter().flat_map(|edge| &edge.evidence_ids))
        .next()
        .copied()
}

fn merge_usage(existing: &mut ModelUsage, incoming: ModelUsage) {
    existing.tokens.input_tokens = existing
        .tokens
        .input_tokens
        .max(incoming.tokens.input_tokens);
    existing.tokens.output_tokens = existing
        .tokens
        .output_tokens
        .max(incoming.tokens.output_tokens);
    existing.tokens.cached_input_tokens = existing
        .tokens
        .cached_input_tokens
        .max(incoming.tokens.cached_input_tokens);
    existing.tokens.total_tokens = existing
        .tokens
        .total_tokens
        .max(incoming.tokens.total_tokens);

    match (&mut existing.cost, incoming.cost) {
        (slot @ None, cost) => *slot = cost,
        (Some(current), Some(next))
            if current.currency != next.currency
                || (next.amount.is_finite()
                    && (!current.amount.is_finite() || next.amount >= current.amount)) =>
        {
            *current = next;
        }
        (Some(_), None | Some(_)) => {}
    }
}

#[must_use]
pub(crate) fn evidence_number(evidence: &[Evidence], id: EvidenceId) -> Option<usize> {
    evidence
        .iter()
        .position(|candidate| candidate.id == id)
        .map(|index| index + 1)
}

#[must_use]
pub(crate) fn evidence_line_range(evidence: &Evidence) -> (u32, u32) {
    let start = evidence.span.start().line();
    let end_position = evidence.span.end();
    let end = if end_position.column() == 0 && end_position.line() > start {
        end_position.line() - 1
    } else {
        end_position.line()
    };
    (start, end)
}

fn content_end_line(start_line: u32, content: &str) -> u32 {
    let offset = u32::try_from(content.lines().count().saturating_sub(1)).unwrap_or(u32::MAX);
    start_line.saturating_add(offset)
}
