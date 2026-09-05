use codeatlas_core::RepositoryPath;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    Scan,
    Parse,
    Assemble,
    Cache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

/// A repository-relative diagnostic. `path` is absent only when an error
/// cannot safely be associated with a valid UTF-8 repository path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiagnostic {
    pub path: Option<RepositoryPath>,
    pub stage: DiagnosticStage,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
}

impl FileDiagnostic {
    #[must_use]
    pub fn new(
        path: Option<RepositoryPath>,
        stage: DiagnosticStage,
        severity: DiagnosticSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path,
            stage,
            severity,
            code: code.into(),
            message: message.into(),
        }
    }
}
