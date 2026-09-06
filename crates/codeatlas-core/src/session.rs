use serde::{Deserialize, Serialize};

use crate::{
    AgentAnswer, AppError, BudgetStopReason, ExplanationProfile, ModelBudgetStatus, ModelCallId,
    ModelCallRecord, ModelUsage, Progress, RepositoryId, RequestId, SessionId, ToolCall,
    ToolCallId,
};
use serde_json::Value;

/// Terminal state of one persisted agent task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTaskStatus {
    #[default]
    Completed,
    Failed,
    Cancelled,
    BudgetExceeded,
}

/// One normalized, provider-neutral operation in a persisted task trajectory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkflowEvent {
    Progress(Progress),
    /// One observable model message. Trusted system controls are recorded here
    /// but may be omitted from continuation; tool content may be compacted.
    Message(Value),
    ModelRequest {
        call_id: ModelCallId,
        sequence: u64,
        model: String,
        request: Value,
    },
    ModelResponse {
        call_id: ModelCallId,
        sequence: u64,
        response: Value,
    },
    ModelError {
        call_id: ModelCallId,
        sequence: u64,
        message: String,
    },
    ToolCallStarted(ToolCall),
    ToolCallCompleted {
        call_id: ToolCallId,
        output: String,
        is_error: bool,
    },
    ModelCall(ModelCallRecord),
    Usage(ModelUsage),
    BudgetUpdated(ModelBudgetStatus),
    BudgetExceeded {
        status: ModelBudgetStatus,
        reason: BudgetStopReason,
    },
}

/// Lightweight task metadata used by history lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTaskSummary {
    pub request_id: RequestId,
    pub question: String,
    #[serde(default)]
    pub profile: ExplanationProfile,
    #[serde(default)]
    pub status: SessionTaskStatus,
}

/// Lightweight metadata for one persisted session JSON document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub repository_id: RepositoryId,
    pub tasks: Vec<SessionTaskSummary>,
    #[serde(default)]
    pub created_at_unix_ms: u64,
    #[serde(default)]
    pub updated_at_unix_ms: u64,
    pub json_path: String,
}

/// A terminal task restored from a persisted session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTask {
    pub request_id: RequestId,
    pub question: String,
    #[serde(default)]
    pub profile: ExplanationProfile,
    #[serde(default)]
    pub status: SessionTaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<AgentAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<AppError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_stop_reason: Option<BudgetStopReason>,
    #[serde(default)]
    pub started_at_unix_ms: u64,
    #[serde(default)]
    pub finished_at_unix_ms: u64,
    /// Complete provider-neutral model/tool trace plus progress, usage, and budgets.
    #[serde(default, alias = "workflow")]
    pub trajectory: Vec<WorkflowEvent>,
    #[serde(default)]
    pub model_calls: Vec<ModelCallRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ModelUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<ModelBudgetStatus>,
}

/// Complete, presentation-neutral context for a persisted session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContext {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub repository_id: RepositoryId,
    pub tasks: Vec<SessionTask>,
    pub usage: ModelUsage,
    #[serde(default)]
    pub created_at_unix_ms: u64,
    #[serde(default)]
    pub updated_at_unix_ms: u64,
    pub json_path: String,
}
