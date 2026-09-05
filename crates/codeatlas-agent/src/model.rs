use std::{collections::VecDeque, sync::Mutex};

use async_trait::async_trait;
use codeatlas_core::{
    CallPathStep, ClaimKind, DiagramKind, EvidenceId, TokenUsage, ToolDefinition,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// A model-neutral chat role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    System,
    User,
    Assistant,
    Tool,
}

/// One provider-independent chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AssistantToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ModelMessage {
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(ModelRole::System, content)
    }

    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(ModelRole::User, content)
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::plain(ModelRole::Assistant, content)
    }

    #[must_use]
    pub fn assistant_tool_calls(
        content: Option<String>,
        tool_calls: Vec<AssistantToolCall>,
    ) -> Self {
        Self {
            role: ModelRole::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            name: None,
        }
    }

    #[must_use]
    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: ModelRole::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
        }
    }

    fn plain(role: ModelRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }
}

/// A model-originated function call. Its ID is provider-owned and opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// A factual or qualified claim before runtime-generated stable IDs are added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredClaim {
    pub kind: ClaimKind,
    pub text: String,
    #[serde(default)]
    pub evidence_ids: Vec<EvidenceId>,
}

/// A call path before the runtime assigns its stable path ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredCallPath {
    pub label: Option<String>,
    #[serde(default)]
    pub steps: Vec<CallPathStep>,
    pub complete: bool,
}

/// The model's explicit semantic decision about whether a diagram adds value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum StructuredDiagramDecision {
    NotNeeded {
        reason: String,
    },
    Needed {
        reason: String,
        diagram: StructuredDiagram,
    },
}

/// A model-authored diagram whose claim references are answer-local indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredDiagram {
    pub kind: DiagramKind,
    pub title: String,
    pub nodes: Vec<StructuredDiagramNode>,
    pub edges: Vec<StructuredDiagramEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredDiagramNode {
    pub id: String,
    pub label: String,
    pub claim_indices: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredDiagramEdge {
    pub source: String,
    pub target: String,
    pub label: String,
    pub claim_indices: Vec<usize>,
}

/// The final JSON answer shape required from a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredAnswer {
    pub text: String,
    #[serde(default)]
    pub claims: Vec<StructuredClaim>,
    #[serde(default)]
    pub call_paths: Vec<StructuredCallPath>,
    pub diagram: StructuredDiagramDecision,
}

/// A single assistant turn, either tool dispatch or a final structured answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantOutput {
    ToolCalls {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        calls: Vec<AssistantToolCall>,
    },
    FinalAnswer {
        answer: StructuredAnswer,
    },
    /// Provider returned prose instead of the structured final-answer contract.
    UnstructuredText {
        content: String,
    },
}

/// Provider-independent input for one non-streaming model turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolDefinition>,
}

/// Provider-independent result for one non-streaming model turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub output: AssistantOutput,
    /// Provider-reported token usage. `None` means the provider omitted usage;
    /// `Some(TokenUsage::default())` means it explicitly reported zero tokens.
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// Diagnosable failures at the model transport/protocol boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelError {
    #[error("invalid model request: {message}")]
    InvalidRequest { message: String },
    #[error("model transport failed: {message}")]
    Transport { message: String },
    #[error("model request timed out after {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },
    #[error("model endpoint returned HTTP {status}: {message}")]
    Http {
        status: u16,
        message: String,
        retryable: bool,
    },
    #[error("invalid model response: {message}")]
    InvalidResponse { message: String },
    #[error("mock model script is exhausted")]
    MockExhausted,
}

impl ModelError {
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::Timeout { .. } | Self::InvalidResponse { .. } => true,
            Self::Http { retryable, .. } => *retryable,
            Self::InvalidRequest { .. } | Self::MockExhausted => false,
        }
    }

    #[must_use]
    pub fn is_context_overflow(&self) -> bool {
        matches!(
            self,
            Self::Http {
                status: 400 | 413,
                retryable: true,
                ..
            }
        )
    }

    #[must_use]
    pub const fn is_gateway_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Http {
                status: 502..=504,
                ..
            }
        )
    }
}

/// Object-safe asynchronous model boundary used by [`crate::AgentRuntime`].
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Returns the configured model context window when it is known.
    fn context_window_tokens(&self) -> Option<u32> {
        None
    }

    /// Completes one model turn.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] for request, transport, timeout, HTTP, or response
    /// protocol failures.
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError>;
}

/// Fully offline FIFO model implementation for deterministic tests and demos.
#[derive(Default)]
pub struct MockModelClient {
    script: Mutex<VecDeque<Result<ModelResponse, ModelError>>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl MockModelClient {
    #[must_use]
    pub fn new(script: impl IntoIterator<Item = Result<ModelResponse, ModelError>>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn push_response(&self, response: ModelResponse) {
        self.script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Ok(response));
    }

    pub fn push_error(&self, error: ModelError) {
        self.script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(Err(error));
    }

    #[must_use]
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl std::fmt::Debug for MockModelClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MockModelClient")
            .field("remaining", &self.remaining())
            .field("request_count", &self.requests().len())
            .finish()
    }
}

#[async_trait]
impl ModelClient for MockModelClient {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        self.script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or(Err(ModelError::MockExhausted))
    }
}
