use codeatlas_core::{
    CallEdgeId, Confidence, EntryPointId, EntryPointKind, Evidence, FileId, Language, ModuleId,
    ReferenceEdgeId, ReferenceKind, RepositoryPath, SourceSpan, SymbolId, SymbolKind,
    TargetResolution,
};
use serde::{Deserialize, Serialize};

/// A typed query result. This is also the JSON envelope returned by every tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryResponse<T> {
    pub data: T,
    pub evidence: Vec<Evidence>,
}

impl<T> QueryResponse<T> {
    pub(crate) const fn new(data: T, evidence: Vec<Evidence>) -> Self {
        Self { data, evidence }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileSummary {
    pub id: FileId,
    pub path: RepositoryPath,
    pub language: Language,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleSummary {
    pub id: ModuleId,
    pub name: String,
    pub qualified_name: String,
    pub file_id: FileId,
    pub path: RepositoryPath,
    pub span: Option<SourceSpan>,
    pub parent_id: Option<ModuleId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolSummary {
    pub id: SymbolId,
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub file_id: FileId,
    pub path: RepositoryPath,
    pub module_id: Option<ModuleId>,
    pub span: SourceSpan,
    pub parent_id: Option<SymbolId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefinitionView {
    pub symbol_id: SymbolId,
    pub file_id: FileId,
    pub path: RepositoryPath,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionMethod {
    Model,
    ExactQualifiedName,
    SameModule,
    SameFile,
    UniqueSimpleName,
    Unresolved,
}

/// The original model target alongside the conservative, non-mutating resolution view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedTargetView {
    pub original: TargetResolution<SymbolId>,
    pub resolved: TargetResolution<SymbolId>,
    pub method: ResolutionMethod,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReferenceEdgeView {
    pub id: ReferenceEdgeId,
    pub source_file_id: FileId,
    pub source_path: RepositoryPath,
    pub source_symbol_id: Option<SymbolId>,
    pub target: ResolvedTargetView,
    pub kind: ReferenceKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallEdgeView {
    pub id: CallEdgeId,
    pub source_file_id: FileId,
    pub source_path: RepositoryPath,
    pub caller_id: Option<SymbolId>,
    pub target: ResolvedTargetView,
    pub span: SourceSpan,
    pub confidence: Option<Confidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryPointView {
    pub id: EntryPointId,
    pub kind: EntryPointKind,
    pub label: String,
    pub file_id: FileId,
    pub path: RepositoryPath,
    pub symbol_id: Option<SymbolId>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListFilesQuery {
    pub prefix: Option<RepositoryPath>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListFilesResult {
    pub files: Vec<FileSummary>,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileQuery {
    pub path: RepositoryPath,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileContent {
    pub path: RepositoryPath,
    pub start_line: u32,
    pub end_line: u32,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindSymbolQuery {
    pub query: String,
    pub case_sensitive: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindSymbolResult {
    pub symbols: Vec<SymbolSummary>,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindReferencesQuery {
    pub symbol_id: SymbolId,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FindReferencesResult {
    pub symbol: SymbolSummary,
    pub references: Vec<ReferenceEdgeView>,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSymbolQuery {
    pub symbol_id: SymbolId,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GetSymbolResult {
    pub symbol: SymbolSummary,
    pub definition: DefinitionView,
    pub references: Vec<ReferenceEdgeView>,
    pub references_total: usize,
    pub callers: Vec<CallEdgeView>,
    pub callers_total: usize,
    pub callees: Vec<CallEdgeView>,
    pub callees_total: usize,
    pub entry_points: Vec<EntryPointView>,
    pub related_edges_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetModuleQuery {
    pub module_id: Option<ModuleId>,
    pub qualified_name: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GetModuleResult {
    pub module: ModuleSummary,
    pub parent: Option<ModuleSummary>,
    pub child_modules: Vec<ModuleSummary>,
    pub child_modules_truncated: bool,
    pub symbols: Vec<SymbolSummary>,
    pub symbol_total: usize,
    pub symbols_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCodeQuery {
    pub query: String,
    pub path_prefix: Option<RepositoryPath>,
    pub case_sensitive: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchMatch {
    pub file_id: FileId,
    pub path: RepositoryPath,
    pub span: SourceSpan,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedFile {
    pub path: RepositoryPath,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchCodeResult {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    pub skipped_file_count: usize,
    pub skipped_files: Vec<SkippedFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceDirection {
    Callers,
    Callees,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceCallQuery {
    pub symbol_id: SymbolId,
    pub direction: TraceDirection,
    pub max_depth: Option<u32>,
    pub max_nodes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TraceNode {
    pub symbol: SymbolSummary,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceEdge {
    pub depth: u32,
    pub call: CallEdgeView,
    pub next_symbol_id: Option<SymbolId>,
    pub revisited: bool,
    pub omitted_by_node_limit: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceCallResult {
    pub root: SymbolSummary,
    pub direction: TraceDirection,
    pub nodes: Vec<TraceNode>,
    pub edges: Vec<TraceEdge>,
    pub truncated_by_depth: bool,
    pub truncated_by_node_limit: bool,
    pub truncated_by_edge_limit: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetRepositoryOverviewQuery {
    /// Maximum modules and entry points returned in the compact overview.
    pub limit: Option<usize>,
    /// Includes test and benchmark entry points when explicitly requested.
    pub include_tests: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryCounts {
    pub files: usize,
    pub modules: usize,
    pub symbols: usize,
    pub imports: usize,
    pub references: usize,
    pub calls: usize,
    pub entry_points: usize,
    pub unresolved_references: usize,
    pub unresolved_calls: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanguageCount {
    pub language: Language,
    pub files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryOverview {
    pub repository_id: codeatlas_core::RepositoryId,
    pub name: String,
    pub counts: RepositoryCounts,
    pub languages: Vec<LanguageCount>,
    pub modules: Vec<ModuleSummary>,
    pub modules_truncated: bool,
    pub entry_points: Vec<EntryPointView>,
    pub entry_points_truncated: bool,
}
