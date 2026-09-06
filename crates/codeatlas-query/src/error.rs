use thiserror::Error;

/// A diagnostic failure produced while building or querying a repository index.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QueryError {
    #[error("repository query was cancelled")]
    Cancelled,
    #[error("invalid repository model: {message}")]
    InvalidModel { message: String },
    #[error("invalid query: {message}")]
    InvalidQuery { message: String },
    #[error("{entity} not found: {key}")]
    NotFound { entity: &'static str, key: String },
    #[error("cannot use repository root {path}: {message}")]
    InvalidRoot { path: String, message: String },
    #[error("cannot read indexed file {path}: {message}")]
    FileRead { path: String, message: String },
    #[error("indexed path escapes the repository root after canonicalization: {path}")]
    PathEscape { path: String },
    #[error("indexed path is not a regular file: {path}")]
    NotAFile { path: String },
    #[error("binary indexed file cannot be read: {path}")]
    BinaryFile { path: String },
    #[error("indexed file exceeds the {limit_bytes}-byte query limit: {path}")]
    FileTooLarge { path: String, limit_bytes: usize },
}

impl QueryError {
    pub(crate) fn invalid_model(message: impl Into<String>) -> Self {
        Self::InvalidModel {
            message: message.into(),
        }
    }

    pub(crate) fn invalid_query(message: impl Into<String>) -> Self {
        Self::InvalidQuery {
            message: message.into(),
        }
    }

    pub(crate) fn not_found(entity: &'static str, key: impl Into<String>) -> Self {
        Self::NotFound {
            entity,
            key: key.into(),
        }
    }
}
