//! Stable, language-neutral contracts shared by `CodeAtlas` components.
//!
//! Parser implementations, model clients, storage backends, and user
//! interfaces intentionally live outside this crate.

mod app;
mod error;
mod evidence;
mod explanation;
mod id;
mod ir;
mod model;
mod parser;
mod path;
mod schema;
mod session;
mod source;
mod tool;

pub use app::{
    AppCommand, AppError, AppEvent, Progress, ProgressPhase, RepositoryEntryPoint,
    RepositoryLanguage, RepositoryMap, RepositoryModule,
};
pub use error::CoreError;
pub use evidence::{
    AgentAnswer, CallPath, CallPathStep, Claim, ClaimKind, Diagram, DiagramArtifact,
    DiagramDecision, DiagramEdge, DiagramKind, DiagramNode, Evidence, EvidenceValidationError,
    SuggestedAction,
};
pub use explanation::{ExplanationAudience, ExplanationDepth, ExplanationProfile};
pub use id::{
    AnswerId, CallEdgeId, CallPathId, ClaimId, DiagramId, EntryPointId, EvidenceId, FileId,
    IdParseError, ImportEdgeId, ModelCallId, ModuleId, ReferenceEdgeId, RepositoryId, RequestId,
    SessionId, SymbolId, ToolCallId,
};
pub use ir::{
    CallEdge, Confidence, ConfidenceError, EntryPoint, EntryPointKind, FileInfo, ImportEdge,
    ImportItem, Language, Module, ReferenceEdge, ReferenceKind, RepositoryModel, Symbol,
    SymbolKind, TargetResolution, UnresolvedTarget,
};
pub use model::{
    BudgetStopReason, Cost, ModelBudget, ModelBudgetStatus, ModelCallOutcome, ModelCallRecord,
    ModelConfig, ModelUsage, MonetaryBudget, TokenUsage,
};
pub use parser::{
    ParseInput, ParsedCall, ParsedEntryPoint, ParsedFile, ParsedImport, ParsedModule,
    ParsedReference, ParsedSymbol, ParsedSymbolId, ParsedTarget, ParserAdapter, ParserError,
};
pub use path::{RepositoryPath, RepositoryPathError};
pub use schema::{SCHEMA_VERSION, SchemaError, SchemaVersion};
pub use session::{
    SessionContext, SessionSummary, SessionTask, SessionTaskStatus, SessionTaskSummary,
    WorkflowEvent,
};
pub use source::{SourcePosition, SourceSpan, SourceSpanError};
pub use tool::{ToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput};
