use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::ToolCallId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub call_id: ToolCallId,
    pub result: Value,
    pub is_error: bool,
}

/// Asynchronous, object-safe execution boundary used by the agent runtime.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Executes one structured tool call.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] for dispatch, argument, cancellation, or runtime
    /// failures. A successfully executed tool may still return an application
    /// error to the model through [`ToolOutput::is_error`].
    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolError {
    #[error("unknown tool: {name}")]
    NotFound { name: String },
    #[error("invalid arguments for tool {name}: {message}")]
    InvalidArguments { name: String, message: String },
    #[error("tool {name} failed: {message}")]
    Execution { name: String, message: String },
    #[error("tool call was cancelled")]
    Cancelled,
}
