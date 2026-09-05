use thiserror::Error;

use crate::{
    ConfidenceError, EvidenceValidationError, IdParseError, RepositoryPathError, SchemaError,
    SourceSpanError, ToolError,
};

/// Convenience error umbrella for consumers that do not need typed failures.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Id(#[from] IdParseError),
    #[error(transparent)]
    RepositoryPath(#[from] RepositoryPathError),
    #[error(transparent)]
    SourceSpan(#[from] SourceSpanError),
    #[error(transparent)]
    Confidence(#[from] ConfidenceError),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error(transparent)]
    Evidence(#[from] EvidenceValidationError),
    #[error(transparent)]
    Tool(#[from] ToolError),
}
