//! UI-independent model, agent runtime, evidence, and session services.

mod model;
mod openai;
mod runtime;
mod session;

pub use model::{
    AssistantOutput, AssistantToolCall, MockModelClient, ModelClient, ModelError, ModelMessage,
    ModelRequest, ModelResponse, ModelRole, StructuredAnswer, StructuredCallPath, StructuredClaim,
    StructuredDiagram, StructuredDiagramDecision, StructuredDiagramEdge, StructuredDiagramNode,
};
pub use openai::{OpenAiChatClient, OpenAiClientBuildError, RuntimeSecretHeaders, SecretString};
pub use runtime::{
    AgentRequest, AgentRuntime, ChannelEventSink, EventSink, NoopEventSink, RepositoryToolEnvelope,
    RuntimeConfig, RuntimeConfigError, RuntimeError, SUBMIT_ANSWER_TOOL_NAME, SYSTEM_PROMPT,
};
pub use session::{
    ConversationMessage, ConversationRole, ConversationTurn, ModelPricing, PricingError,
    SESSION_SCHEMA_VERSION, SessionError, SessionState, SessionStore, add_token_usage,
};
pub use tokio_util::sync::CancellationToken;
