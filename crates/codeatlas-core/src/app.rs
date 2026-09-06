use serde::{Deserialize, Serialize};

use crate::{
    AgentAnswer, AnswerId, BudgetStopReason, DiagramId, EntryPointKind, Evidence,
    ExplanationProfile, Language, ModelBudgetStatus, ModelCallRecord, ModelUsage, RepositoryId,
    RepositoryPath, RequestId, SessionContext, SessionId, SessionSummary, SuggestedAction,
    ToolCall, ToolOutput, WorkflowEvent,
};

/// Commands accepted by a UI-independent `CodeAtlas` application runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AppCommand {
    Index {
        request_id: RequestId,
        /// Host path used only to locate the repository; it is not stored in the IR.
        repository_root: String,
    },
    Ask {
        request_id: RequestId,
        session_id: SessionId,
        repository_id: RepositoryId,
        question: String,
        #[serde(default)]
        profile: ExplanationProfile,
    },
    /// Executes only an action previously offered on a persisted answer.
    RunSuggestedAction {
        request_id: RequestId,
        session_id: SessionId,
        repository_id: RepositoryId,
        answer_id: AnswerId,
        action: SuggestedAction,
    },
    LoadSource {
        request_id: RequestId,
        repository_id: RepositoryId,
        path: RepositoryPath,
        start_line: u32,
        end_line: Option<u32>,
    },
    Cancel {
        request_id: RequestId,
        target_request_id: RequestId,
    },
    ListSessions {
        request_id: RequestId,
        repository_id: Option<RepositoryId>,
    },
    LoadSession {
        request_id: RequestId,
        session_id: SessionId,
    },
    OpenDiagram {
        request_id: RequestId,
        diagram_id: DiagramId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressPhase {
    Scanning,
    Parsing,
    Indexing,
    Searching,
    Tracing,
    Reading,
    Verifying,
    Explaining,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub phase: ProgressPhase,
    pub message: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// A bounded, presentation-neutral summary used to establish a repository mental model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryMap {
    pub name: String,
    pub module_count: u64,
    pub call_count: u64,
    pub unresolved_call_count: u64,
    pub languages: Vec<RepositoryLanguage>,
    pub modules: Vec<RepositoryModule>,
    pub modules_truncated: bool,
    pub entry_points: Vec<RepositoryEntryPoint>,
    pub entry_points_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryLanguage {
    pub language: Language,
    pub file_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryModule {
    pub name: String,
    pub path: RepositoryPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryEntryPoint {
    pub kind: EntryPointKind,
    pub label: String,
    pub path: RepositoryPath,
    pub line: u32,
}

/// Events emitted by the runtime for any presentation layer.
#[allow(
    clippy::large_enum_variant,
    reason = "events intentionally own their serializable contract payloads"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AppEvent {
    Progress {
        request_id: RequestId,
        progress: Progress,
    },
    ToolCallStarted {
        request_id: RequestId,
        call: ToolCall,
    },
    ToolCallCompleted {
        request_id: RequestId,
        output: ToolOutput,
    },
    EvidenceAdded {
        request_id: RequestId,
        evidence: Evidence,
    },
    AnswerDelta {
        request_id: RequestId,
        delta: String,
    },
    AnswerCompleted {
        request_id: RequestId,
        answer: AgentAnswer,
    },
    UsageUpdated {
        request_id: RequestId,
        usage: ModelUsage,
    },
    ModelCallRecorded {
        request_id: RequestId,
        record: ModelCallRecord,
    },
    BudgetUpdated {
        request_id: RequestId,
        status: ModelBudgetStatus,
    },
    BudgetExceeded {
        request_id: RequestId,
        status: ModelBudgetStatus,
        reason: BudgetStopReason,
    },
    TaskTraceRecorded {
        request_id: RequestId,
        event: WorkflowEvent,
    },
    IndexCompleted {
        request_id: RequestId,
        repository_id: RepositoryId,
        file_count: u64,
        symbol_count: u64,
        repository_map: RepositoryMap,
    },
    SourceLoaded {
        request_id: RequestId,
        repository_id: RepositoryId,
        path: RepositoryPath,
        start_line: u32,
        end_line: u32,
        content: String,
    },
    Cancelled {
        request_id: RequestId,
    },
    SessionsListed {
        request_id: RequestId,
        sessions: Vec<SessionSummary>,
    },
    SessionLoaded {
        request_id: RequestId,
        session: SessionContext,
    },
    DiagramOpened {
        request_id: RequestId,
        diagram_id: DiagramId,
    },
    Error {
        request_id: Option<RequestId>,
        error: AppError,
    },
}
