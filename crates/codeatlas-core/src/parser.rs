use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Confidence, EntryPointKind, Language, ReferenceKind, RepositoryPath, SourceSpan, SymbolKind,
};

/// Borrowed source passed to a synchronous parser adapter.
#[derive(Debug, Clone, Copy)]
pub struct ParseInput<'a> {
    pub path: &'a RepositoryPath,
    pub source: &'a str,
}

/// File-local identity assigned by a parser to connect its parsed records.
///
/// It is not a persistent repository ID. The index assembly layer maps these
/// values to stable global IDs using repository paths and canonical symbol keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParsedSymbolId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "target", rename_all = "snake_case")]
pub enum ParsedTarget {
    Local(ParsedSymbolId),
    Unresolved(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedModule {
    pub name: String,
    pub qualified_name: String,
    pub span: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedSymbol {
    pub local_id: ParsedSymbolId,
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub span: SourceSpan,
    pub parent_id: Option<ParsedSymbolId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedImport {
    pub target: String,
    pub span: SourceSpan,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedReference {
    pub source_id: Option<ParsedSymbolId>,
    pub target: ParsedTarget,
    pub kind: ReferenceKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedCall {
    pub caller_id: Option<ParsedSymbolId>,
    pub target: ParsedTarget,
    pub span: SourceSpan,
    pub confidence: Option<Confidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedEntryPoint {
    pub kind: EntryPointKind,
    pub label: String,
    pub symbol_id: Option<ParsedSymbolId>,
    pub span: SourceSpan,
}

/// Language-neutral output for one source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedFile {
    pub path: RepositoryPath,
    pub language: Language,
    pub module: Option<ParsedModule>,
    pub symbols: Vec<ParsedSymbol>,
    pub imports: Vec<ParsedImport>,
    pub references: Vec<ParsedReference>,
    pub calls: Vec<ParsedCall>,
    pub entry_points: Vec<ParsedEntryPoint>,
}

impl ParsedFile {
    #[must_use]
    pub fn empty(path: RepositoryPath, language: Language) -> Self {
        Self {
            path,
            language,
            module: None,
            symbols: Vec::new(),
            imports: Vec::new(),
            references: Vec::new(),
            calls: Vec::new(),
            entry_points: Vec::new(),
        }
    }
}

/// Synchronous boundary implemented by language-specific parser crates.
///
/// Implementations parse one file and must preserve unresolved names in
/// [`ParsedTarget::Unresolved`]. They do not assign global IDs or build a
/// complete [`crate::RepositoryModel`].
pub trait ParserAdapter: Send + Sync {
    #[must_use]
    fn language(&self) -> Language;

    /// Parses one source file into language-neutral records.
    ///
    /// # Errors
    ///
    /// Returns [`ParserError`] when the adapter cannot produce a parsed file.
    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedFile, ParserError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("failed to parse {path}: {message}")]
pub struct ParserError {
    pub path: RepositoryPath,
    pub message: String,
}
