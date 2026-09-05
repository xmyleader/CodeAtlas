//! Deterministic, read-only structural queries over a `CodeAtlas` repository model.

mod error;
mod index;
mod tools;
mod types;

pub use error::QueryError;
pub use index::RepositoryIndex;
pub use tools::{RepositoryTools, TOOL_NAMES, definitions};
pub use types::{
    CallEdgeView, DefinitionView, EntryPointView, FileContent, FileSummary, FindReferencesQuery,
    FindReferencesResult, FindSymbolQuery, FindSymbolResult, GetModuleQuery, GetModuleResult,
    GetRepositoryOverviewQuery, GetSymbolQuery, GetSymbolResult, LanguageCount, ListFilesQuery,
    ListFilesResult, ModuleSummary, QueryResponse, ReadFileQuery, ReferenceEdgeView,
    RepositoryCounts, RepositoryOverview, ResolutionMethod, ResolvedTargetView, SearchCodeQuery,
    SearchCodeResult, SearchMatch, SkippedFile, SymbolSummary, TraceCallQuery, TraceCallResult,
    TraceDirection, TraceEdge, TraceNode,
};
