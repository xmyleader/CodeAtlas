use serde::{Deserialize, Serialize};

use crate::{
    AgentAnswer, ModelUsage, Progress, RepositoryId, RequestId, SessionId, ToolCall, ToolCallId,
};

/// One user-visible operation in an agent task's persisted workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkflowEvent {
    Progress(Progress),
    ToolCallStarted(ToolCall),
    ToolCallCompleted {
        call_id: ToolCallId,
        output: String,
        is_error: bool,
    },
}

/// Lightweight task metadata used by history lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTaskSummary {
    pub request_id: RequestId,
    pub question: String,
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

/// A completed task restored from a persisted session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTask {
    pub request_id: RequestId,
    pub question: String,
    pub answer: AgentAnswer,
    pub workflow: Vec<WorkflowEvent>,
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
