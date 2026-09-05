use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::{
    CallEdgeId, EntryPointId, FileId, ImportEdgeId, ModuleId, ReferenceEdgeId, RepositoryId,
    RepositoryPath, SCHEMA_VERSION, SchemaError, SchemaVersion, SourceSpan, SymbolId,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    Python,
    C,
    Cpp,
    Java,
    JavaScript,
    TypeScript,
    Go,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Enum,
    Interface,
    Trait,
    Module,
    Namespace,
    Constant,
    Static,
    Variable,
    Field,
    Parameter,
    TypeAlias,
    Macro,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: FileId,
    pub path: RepositoryPath,
    pub language: Language,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub id: ModuleId,
    pub name: String,
    pub qualified_name: String,
    pub file_id: FileId,
    pub span: Option<SourceSpan>,
    pub parent_id: Option<ModuleId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub file_id: FileId,
    pub module_id: Option<ModuleId>,
    pub span: SourceSpan,
    pub parent_id: Option<SymbolId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedTarget {
    pub name: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "target", rename_all = "snake_case")]
pub enum TargetResolution<T> {
    Resolved(T),
    Unresolved(UnresolvedTarget),
}

impl<T> TargetResolution<T> {
    #[must_use]
    pub const fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportItem {
    Module(ModuleId),
    Symbol(SymbolId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportEdge {
    pub id: ImportEdgeId,
    pub source_file_id: FileId,
    pub source_module_id: Option<ModuleId>,
    pub target: TargetResolution<ImportItem>,
    pub span: SourceSpan,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Read,
    Write,
    Type,
    Inheritance,
    Implementation,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceEdge {
    pub id: ReferenceEdgeId,
    pub source_file_id: FileId,
    pub source_symbol_id: Option<SymbolId>,
    pub target: TargetResolution<SymbolId>,
    pub kind: ReferenceKind,
    pub span: SourceSpan,
}

/// Optional parser or resolver confidence constrained to the inclusive range 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Confidence(f32);

impl Confidence {
    /// Creates a confidence value.
    ///
    /// # Errors
    ///
    /// Returns [`ConfidenceError`] unless `value` is finite and in `0.0..=1.0`.
    pub fn new(value: f32) -> Result<Self, ConfidenceError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ConfidenceError(value))
        }
    }

    #[must_use]
    pub const fn value(self) -> f32 {
        self.0
    }
}

impl Serialize for Confidence {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f32(self.0)
    }
}

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f32::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Error)]
#[error("confidence must be finite and between 0 and 1, got {0}")]
pub struct ConfidenceError(pub f32);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallEdge {
    pub id: CallEdgeId,
    pub source_file_id: FileId,
    pub caller_id: Option<SymbolId>,
    pub target: TargetResolution<SymbolId>,
    pub span: SourceSpan,
    pub confidence: Option<Confidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPointKind {
    Executable,
    Library,
    Test,
    Benchmark,
    WebRoute,
    BackgroundTask,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPoint {
    pub id: EntryPointId,
    pub kind: EntryPointKind,
    pub label: String,
    pub file_id: FileId,
    pub symbol_id: Option<SymbolId>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepositoryModel {
    pub schema_version: SchemaVersion,
    pub repository_id: RepositoryId,
    pub name: String,
    pub files: Vec<FileInfo>,
    pub modules: Vec<Module>,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<ImportEdge>,
    pub references: Vec<ReferenceEdge>,
    pub calls: Vec<CallEdge>,
    pub entry_points: Vec<EntryPoint>,
}

impl RepositoryModel {
    #[must_use]
    pub fn empty(repository_id: RepositoryId, name: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            repository_id,
            name: name.into(),
            files: Vec::new(),
            modules: Vec::new(),
            symbols: Vec::new(),
            imports: Vec::new(),
            references: Vec::new(),
            calls: Vec::new(),
            entry_points: Vec::new(),
        }
    }

    /// Checks whether the model uses this crate's current persistence schema.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] when the model schema is unsupported.
    pub const fn validate_schema(&self) -> Result<(), SchemaError> {
        self.schema_version.ensure_current()
    }
}
