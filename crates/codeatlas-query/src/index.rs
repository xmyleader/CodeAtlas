use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use codeatlas_core::{
    CallEdge, EntryPoint, EntryPointKind, Evidence, EvidenceId, FileId, ImportItem, Language,
    Module, ModuleId, ReferenceEdge, RepositoryModel, RepositoryPath, SourcePosition, SourceSpan,
    Symbol, SymbolId, TargetResolution,
};

use crate::{
    CallEdgeView, DefinitionView, EntryPointView, FileContent, FileSummary, FindReferencesQuery,
    FindReferencesResult, FindSymbolQuery, FindSymbolResult, GetModuleQuery, GetModuleResult,
    GetRepositoryOverviewQuery, GetSymbolQuery, GetSymbolResult, LanguageCount, ListFilesQuery,
    ListFilesResult, ModuleSummary, QueryError, QueryResponse, ReadFileQuery, ReferenceEdgeView,
    RepositoryCounts, RepositoryOverview, ResolutionMethod, ResolvedTargetView, SearchCodeQuery,
    SearchCodeResult, SearchMatch, SkippedFile, SymbolSummary, TraceCallQuery, TraceCallResult,
    TraceDirection, TraceEdge, TraceNode,
};

const DEFAULT_RESULT_LIMIT: usize = 50;
const MAX_RESULT_LIMIT: usize = 500;
const DEFAULT_READ_LINES: u32 = 200;
const MAX_READ_LINES: u32 = 500;
const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUERY_BYTES: usize = 256;
const MAX_EXCERPT_BYTES: usize = 2 * 1024;
const MAX_SEARCH_LINE_BYTES: usize = 512;
const MAX_SKIPPED_FILES: usize = 100;
const RELATED_EDGE_LIMIT: usize = 100;
const OVERVIEW_ITEM_LIMIT: usize = 100;
const DEFAULT_REPOSITORY_OVERVIEW_LIMIT: usize = 12;
const MAX_REPOSITORY_OVERVIEW_LIMIT: usize = 50;
const MAX_REPOSITORY_OVERVIEW_EXCERPT_BYTES: usize = 512;
const DEFAULT_TRACE_DEPTH: u32 = 4;
const MAX_TRACE_DEPTH: u32 = 20;
const DEFAULT_TRACE_NODES: usize = 100;
const MAX_TRACE_NODES: usize = 1_000;
const MAX_TRACE_EDGES: usize = 4_000;

type FilesById = BTreeMap<FileId, usize>;
type FilesByPath = BTreeMap<RepositoryPath, FileId>;

/// A deterministic, read-only in-memory index over one repository model.
#[derive(Debug)]
pub struct RepositoryIndex {
    root: PathBuf,
    model: RepositoryModel,
    files_by_id: BTreeMap<FileId, usize>,
    files_by_path: BTreeMap<RepositoryPath, FileId>,
    modules_by_id: BTreeMap<ModuleId, usize>,
    modules_by_qualified_name: BTreeMap<String, Vec<ModuleId>>,
    modules_by_file: BTreeMap<FileId, Vec<ModuleId>>,
    child_modules: BTreeMap<ModuleId, Vec<ModuleId>>,
    symbols_by_id: BTreeMap<SymbolId, usize>,
    symbols_by_name: BTreeMap<String, Vec<SymbolId>>,
    symbols_by_qualified_name: BTreeMap<String, Vec<SymbolId>>,
    symbols_by_module: BTreeMap<ModuleId, Vec<SymbolId>>,
    references: Vec<ReferenceEdgeView>,
    references_by_target: BTreeMap<SymbolId, Vec<usize>>,
    calls: Vec<CallEdgeView>,
    callers_by_target: BTreeMap<SymbolId, Vec<usize>>,
    callees_by_caller: BTreeMap<SymbolId, Vec<usize>>,
    entry_points: Vec<EntryPointView>,
    entry_points_by_symbol: BTreeMap<SymbolId, Vec<usize>>,
}

impl RepositoryIndex {
    /// Builds an index and canonicalizes its source root.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] if the root is unusable or the model contains
    /// duplicate/dangling identities.
    pub fn new(root: impl AsRef<Path>, mut model: RepositoryModel) -> Result<Self, QueryError> {
        let root = canonical_repository_root(root.as_ref())?;
        model
            .validate_schema()
            .map_err(|error| QueryError::invalid_model(error.to_string()))?;
        sort_model(&mut model);

        let maps = BaseMaps::build(&model)?;
        validate_model_relations(&model, &maps)?;
        let references = build_reference_views(&model, &maps);
        let calls = build_call_views(&model, &maps);
        let entry_points = build_entry_point_views(&model, &maps);
        let relationships = Relationships::build(&references, &calls, &entry_points);

        Ok(Self {
            root,
            model,
            files_by_id: maps.files_by_id,
            files_by_path: maps.files_by_path,
            modules_by_id: maps.modules_by_id,
            modules_by_qualified_name: maps.modules_by_qualified_name,
            modules_by_file: maps.modules_by_file,
            child_modules: maps.child_modules,
            symbols_by_id: maps.symbols_by_id,
            symbols_by_name: maps.symbols_by_name,
            symbols_by_qualified_name: maps.symbols_by_qualified_name,
            symbols_by_module: maps.symbols_by_module,
            references,
            references_by_target: relationships.references_by_target,
            calls,
            callers_by_target: relationships.callers_by_target,
            callees_by_caller: relationships.callees_by_caller,
            entry_points,
            entry_points_by_symbol: relationships.entry_points_by_symbol,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn model(&self) -> &RepositoryModel {
        &self.model
    }

    #[must_use]
    pub fn resolved_references(&self) -> &[ReferenceEdgeView] {
        &self.references
    }

    #[must_use]
    pub fn resolved_calls(&self) -> &[CallEdgeView] {
        &self.calls
    }

    #[must_use]
    pub fn file(&self, file_id: FileId) -> Option<&codeatlas_core::FileInfo> {
        self.files_by_id
            .get(&file_id)
            .map(|index| &self.model.files[*index])
    }

    #[must_use]
    pub fn file_at_path(&self, path: &RepositoryPath) -> Option<&codeatlas_core::FileInfo> {
        self.files_by_path
            .get(path)
            .and_then(|file_id| self.file(*file_id))
    }

    pub fn modules_in_file(&self, file_id: FileId) -> impl Iterator<Item = &Module> + use<'_> {
        self.modules_by_file
            .get(&file_id)
            .into_iter()
            .flatten()
            .filter_map(|module_id| self.module_by_id(*module_id))
    }

    pub fn symbols_named(&self, name: &str) -> impl Iterator<Item = &Symbol> + use<'_> {
        self.symbols_by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|symbol_id| self.symbol_by_id(*symbol_id))
    }

    pub fn symbols_with_qualified_name(
        &self,
        qualified_name: &str,
    ) -> impl Iterator<Item = &Symbol> + use<'_> {
        self.symbols_by_qualified_name
            .get(qualified_name)
            .into_iter()
            .flatten()
            .filter_map(|symbol_id| self.symbol_by_id(*symbol_id))
    }

    #[must_use]
    pub fn definition(&self, symbol_id: SymbolId) -> Option<DefinitionView> {
        self.symbol_by_id(symbol_id)
            .map(|symbol| self.definition_view(symbol))
    }

    #[must_use]
    pub fn entry_points(&self) -> &[EntryPointView] {
        &self.entry_points
    }

    pub fn references_to(
        &self,
        symbol_id: SymbolId,
    ) -> impl Iterator<Item = &ReferenceEdgeView> + use<'_> {
        self.references_by_target
            .get(&symbol_id)
            .into_iter()
            .flatten()
            .map(|index| &self.references[*index])
    }

    pub fn callers_of(&self, symbol_id: SymbolId) -> impl Iterator<Item = &CallEdgeView> + use<'_> {
        self.callers_by_target
            .get(&symbol_id)
            .into_iter()
            .flatten()
            .map(|index| &self.calls[*index])
    }

    /// Includes unresolved outgoing edges so callers can inspect failed
    /// conservative resolutions.
    pub fn callees_of(&self, symbol_id: SymbolId) -> impl Iterator<Item = &CallEdgeView> + use<'_> {
        self.callees_by_caller
            .get(&symbol_id)
            .into_iter()
            .flatten()
            .map(|index| &self.calls[*index])
    }

    /// Lists indexed files without walking the filesystem.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidQuery`] for an invalid limit.
    pub fn list_files(
        &self,
        query: &ListFilesQuery,
    ) -> Result<QueryResponse<ListFilesResult>, QueryError> {
        let limit = result_limit(query.limit)?;
        let mut files = self
            .model
            .files
            .iter()
            .filter(|file| {
                query
                    .prefix
                    .as_ref()
                    .is_none_or(|prefix| path_has_prefix(&file.path, prefix))
            })
            .map(file_summary)
            .collect::<Vec<_>>();
        let total = files.len();
        files.truncate(limit);
        Ok(QueryResponse::new(
            ListFilesResult {
                files,
                total,
                truncated: total > limit,
            },
            Vec::new(),
        ))
    }

    /// Reads a bounded line range from an indexed, in-root text file.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid ranges, unknown files, unsafe paths,
    /// binary files, oversized files, or I/O failures.
    pub fn read_file(
        &self,
        query: &ReadFileQuery,
    ) -> Result<QueryResponse<FileContent>, QueryError> {
        let file_id = self.indexed_file_id(&query.path)?;
        let source = self.read_source(&query.path)?;
        let selection = select_file_range(&source, query, FileSelectionMode::Bounded)?;
        let evidence = self.make_evidence(file_id, selection.span, None, Some(&selection.content));
        let data = FileContent {
            path: query.path.clone(),
            start_line: query.start_line.unwrap_or(1),
            end_line: selection.end_line,
            content: selection.content,
            truncated: selection.truncated,
        };
        Ok(QueryResponse::new(data, vec![evidence]))
    }

    /// Reads the complete requested line range for a local source viewer.
    ///
    /// Unlike [`Self::read_file`], this does not apply model-payload line or
    /// byte caps. Repository membership, path containment, file type, UTF-8,
    /// and indexed source-size checks remain enforced.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid ranges, unknown files, unsafe paths,
    /// binary files, oversized files, or I/O failures.
    pub fn read_source_range(&self, query: &ReadFileQuery) -> Result<FileContent, QueryError> {
        let source = self.read_source(&query.path)?;
        let selection = select_file_range(&source, query, FileSelectionMode::Complete)?;
        Ok(FileContent {
            path: query.path.clone(),
            start_line: query.start_line.unwrap_or(1),
            end_line: selection.end_line,
            content: selection.content,
            truncated: false,
        })
    }

    /// Finds symbols by simple or qualified name.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidQuery`] for an empty/oversized query or an
    /// invalid limit.
    pub fn find_symbol(
        &self,
        query: &FindSymbolQuery,
    ) -> Result<QueryResponse<FindSymbolResult>, QueryError> {
        validate_search_term(&query.query)?;
        let limit = result_limit(query.limit)?;
        let case_sensitive = query.case_sensitive.unwrap_or(false);
        let mut matches = self
            .model
            .symbols
            .iter()
            .filter_map(|symbol| {
                symbol_match_rank(symbol, &query.query, case_sensitive).map(|rank| (rank, symbol))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_rank, left), (right_rank, right)| {
            left_rank
                .cmp(right_rank)
                .then_with(|| compare_symbols(left, right, &self.files_by_id, &self.model))
        });
        let total = matches.len();
        let symbols = matches
            .into_iter()
            .take(limit)
            .map(|(_, symbol)| self.symbol_summary(symbol))
            .collect::<Vec<_>>();
        let evidence = symbols
            .iter()
            .map(|symbol| self.make_evidence(symbol.file_id, symbol.span, Some(symbol.id), None))
            .collect();
        Ok(QueryResponse::new(
            FindSymbolResult {
                symbols,
                total,
                truncated: total > limit,
            },
            normalize_evidence(evidence),
        ))
    }

    /// Returns references whose explicit or conservative target is a symbol.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an unknown symbol or invalid limit.
    pub fn find_references(
        &self,
        query: &FindReferencesQuery,
    ) -> Result<QueryResponse<FindReferencesResult>, QueryError> {
        let symbol = self.require_symbol(query.symbol_id)?;
        let limit = result_limit(query.limit)?;
        let indexes = self
            .references_by_target
            .get(&query.symbol_id)
            .map_or(&[][..], Vec::as_slice);
        let total = indexes.len();
        let references = indexes
            .iter()
            .take(limit)
            .map(|index| self.references[*index].clone())
            .collect::<Vec<_>>();
        let mut evidence = vec![self.evidence_for_symbol(symbol)];
        evidence.extend(
            references
                .iter()
                .map(|reference| self.evidence_for_reference(reference)),
        );
        Ok(QueryResponse::new(
            FindReferencesResult {
                symbol: self.symbol_summary(symbol),
                references,
                total,
                truncated: total > limit,
            },
            normalize_evidence(evidence),
        ))
    }

    /// Returns a symbol definition and bounded structural relationships.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::NotFound`] for an unknown symbol.
    pub fn get_symbol(
        &self,
        query: &GetSymbolQuery,
    ) -> Result<QueryResponse<GetSymbolResult>, QueryError> {
        let symbol = self.require_symbol(query.symbol_id)?;
        let references = self.related_references(query.symbol_id, RELATED_EDGE_LIMIT);
        let incoming_calls =
            self.related_calls(&self.callers_by_target, query.symbol_id, RELATED_EDGE_LIMIT);
        let outgoing_calls =
            self.related_calls(&self.callees_by_caller, query.symbol_id, RELATED_EDGE_LIMIT);
        let entry_points = self.symbol_entry_points(query.symbol_id);
        let data = GetSymbolResult {
            symbol: self.symbol_summary(symbol),
            definition: self.definition_view(symbol),
            references_total: Self::relationship_count(&self.references_by_target, query.symbol_id),
            callers_total: Self::relationship_count(&self.callers_by_target, query.symbol_id),
            callees_total: Self::relationship_count(&self.callees_by_caller, query.symbol_id),
            related_edges_truncated: self.has_truncated_symbol_relationship(query.symbol_id),
            references,
            callers: incoming_calls,
            callees: outgoing_calls,
            entry_points,
        };
        let evidence = self.evidence_for_symbol_detail(&data);
        Ok(QueryResponse::new(data, evidence))
    }

    /// Returns one uniquely selected module and its direct contents.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an invalid selector/limit, unknown module, or
    /// ambiguous qualified name.
    pub fn get_module(
        &self,
        query: &GetModuleQuery,
    ) -> Result<QueryResponse<GetModuleResult>, QueryError> {
        let module_id = self.select_module(query)?;
        let module = self.require_module(module_id)?;
        let limit = result_limit(query.limit)?;
        let symbol_ids = self
            .symbols_by_module
            .get(&module_id)
            .map_or(&[][..], Vec::as_slice);
        let child_ids = self
            .child_modules
            .get(&module_id)
            .map_or(&[][..], Vec::as_slice);
        let symbols = symbol_ids
            .iter()
            .take(limit)
            .filter_map(|id| self.symbol_by_id(*id))
            .map(|symbol| self.symbol_summary(symbol))
            .collect::<Vec<_>>();
        let child_modules = child_ids
            .iter()
            .take(OVERVIEW_ITEM_LIMIT)
            .filter_map(|id| self.module_by_id(*id))
            .map(|child| self.module_summary(child))
            .collect::<Vec<_>>();
        let data = GetModuleResult {
            module: self.module_summary(module),
            parent: module
                .parent_id
                .and_then(|id| self.module_by_id(id))
                .map(|parent| self.module_summary(parent)),
            child_modules,
            child_modules_truncated: child_ids.len() > OVERVIEW_ITEM_LIMIT,
            symbols,
            symbol_total: symbol_ids.len(),
            symbols_truncated: symbol_ids.len() > limit,
        };
        let evidence = self.evidence_for_module_detail(&data);
        Ok(QueryResponse::new(data, evidence))
    }

    /// Performs bounded literal source search over indexed files only.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for invalid input or an indexed path that escapes
    /// the canonical root.
    pub fn search_code(
        &self,
        query: &SearchCodeQuery,
    ) -> Result<QueryResponse<SearchCodeResult>, QueryError> {
        validate_search_term(&query.query)?;
        let limit = result_limit(query.limit)?;
        let case_sensitive = query.case_sensitive.unwrap_or(true);
        let mut state = SearchState::new(limit);

        for file in self.model.files.iter().filter(|file| {
            query
                .path_prefix
                .as_ref()
                .is_none_or(|prefix| path_has_prefix(&file.path, prefix))
        }) {
            let source = match self.read_source(&file.path) {
                Ok(source) => source,
                Err(error @ (QueryError::PathEscape { .. } | QueryError::NotAFile { .. })) => {
                    return Err(error);
                }
                Err(error) => {
                    state.skip(file.path.clone(), error.to_string());
                    continue;
                }
            };
            self.search_file(
                file.id,
                &file.path,
                &source,
                query,
                case_sensitive,
                &mut state,
            )?;
            if state.truncated {
                break;
            }
        }

        Ok(QueryResponse::new(
            SearchCodeResult {
                matches: state.matches,
                truncated: state.truncated,
                skipped_file_count: state.skipped_file_count,
                skipped_files: state.skipped_files,
            },
            normalize_evidence(state.evidence),
        ))
    }

    /// Traverses callers or callees with explicit depth, node, and internal
    /// edge bounds.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`] for an unknown root or out-of-range bounds.
    pub fn trace_call(
        &self,
        query: &TraceCallQuery,
    ) -> Result<QueryResponse<TraceCallResult>, QueryError> {
        let max_depth = trace_depth(query.max_depth)?;
        let max_nodes = trace_nodes(query.max_nodes)?;
        let root = self.require_symbol(query.symbol_id)?;
        let root_evidence = self.evidence_for_symbol(root);
        let mut traversal =
            TraceTraversal::new(self.symbol_summary(root), query.direction, max_nodes);
        traversal.evidence.push(root_evidence);

        while let Some((symbol_id, depth)) = traversal.queue.pop_front() {
            let edge_indexes = self.trace_edge_indexes(query.direction, symbol_id);
            if depth >= max_depth {
                traversal.truncated_by_depth |= !edge_indexes.is_empty();
                continue;
            }
            if !self.expand_trace_node(depth, edge_indexes, &mut traversal) {
                break;
            }
        }

        let evidence = normalize_evidence(std::mem::take(&mut traversal.evidence));
        Ok(QueryResponse::new(traversal.finish(), evidence))
    }

    /// Summarizes indexed structure, languages, modules, and entry points.
    ///
    /// # Errors
    ///
    /// This query currently has no fallible model-dependent operation.
    pub fn get_repository_overview(
        &self,
        query: &GetRepositoryOverviewQuery,
    ) -> Result<QueryResponse<RepositoryOverview>, QueryError> {
        let limit = repository_overview_limit(query.limit)?;
        let include_tests = query.include_tests.unwrap_or(false);
        let modules = self
            .model
            .modules
            .iter()
            .take(limit)
            .map(|module| self.module_summary(module))
            .collect::<Vec<_>>();
        let entry_points = self
            .entry_points
            .iter()
            .filter(|entry| include_tests || !is_test_entry_point(entry))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let evidence = entry_points
            .iter()
            .map(|entry| {
                let mut evidence =
                    self.make_evidence(entry.file_id, entry.span, entry.symbol_id, None);
                evidence.excerpt = evidence
                    .excerpt
                    .as_deref()
                    .map(|excerpt| bounded_text(excerpt, MAX_REPOSITORY_OVERVIEW_EXCERPT_BYTES));
                evidence
            })
            .collect();
        let data = RepositoryOverview {
            repository_id: self.model.repository_id,
            name: self.model.name.clone(),
            counts: self.repository_counts(),
            languages: language_counts(&self.model),
            modules,
            modules_truncated: self.model.modules.len() > limit,
            entry_points,
            entry_points_truncated: self.entry_points.len() > limit
                || (!include_tests && self.entry_points.iter().any(is_test_entry_point)),
        };
        Ok(QueryResponse::new(data, normalize_evidence(evidence)))
    }
}

struct BaseMaps {
    files_by_id: BTreeMap<FileId, usize>,
    files_by_path: BTreeMap<RepositoryPath, FileId>,
    modules_by_id: BTreeMap<ModuleId, usize>,
    modules_by_qualified_name: BTreeMap<String, Vec<ModuleId>>,
    modules_by_file: BTreeMap<FileId, Vec<ModuleId>>,
    child_modules: BTreeMap<ModuleId, Vec<ModuleId>>,
    symbols_by_id: BTreeMap<SymbolId, usize>,
    symbols_by_name: BTreeMap<String, Vec<SymbolId>>,
    symbols_by_qualified_name: BTreeMap<String, Vec<SymbolId>>,
    symbols_by_module: BTreeMap<ModuleId, Vec<SymbolId>>,
}

impl BaseMaps {
    fn build(model: &RepositoryModel) -> Result<Self, QueryError> {
        let (files_by_id, files_by_path) = index_files(model)?;
        let mut maps = Self {
            files_by_id,
            files_by_path,
            modules_by_id: BTreeMap::new(),
            modules_by_qualified_name: BTreeMap::new(),
            modules_by_file: BTreeMap::new(),
            child_modules: BTreeMap::new(),
            symbols_by_id: BTreeMap::new(),
            symbols_by_name: BTreeMap::new(),
            symbols_by_qualified_name: BTreeMap::new(),
            symbols_by_module: BTreeMap::new(),
        };
        maps.index_modules(model)?;
        maps.index_symbols(model)?;
        Ok(maps)
    }

    fn index_modules(&mut self, model: &RepositoryModel) -> Result<(), QueryError> {
        for (index, module) in model.modules.iter().enumerate() {
            if self.modules_by_id.insert(module.id, index).is_some() {
                return Err(QueryError::invalid_model(format!(
                    "duplicate module ID {}",
                    module.id
                )));
            }
            self.modules_by_qualified_name
                .entry(module.qualified_name.clone())
                .or_default()
                .push(module.id);
            self.modules_by_file
                .entry(module.file_id)
                .or_default()
                .push(module.id);
            if let Some(parent_id) = module.parent_id {
                self.child_modules
                    .entry(parent_id)
                    .or_default()
                    .push(module.id);
            }
        }
        Ok(())
    }

    fn index_symbols(&mut self, model: &RepositoryModel) -> Result<(), QueryError> {
        for (index, symbol) in model.symbols.iter().enumerate() {
            if self.symbols_by_id.insert(symbol.id, index).is_some() {
                return Err(QueryError::invalid_model(format!(
                    "duplicate symbol ID {}",
                    symbol.id
                )));
            }
            self.symbols_by_name
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.id);
            self.symbols_by_qualified_name
                .entry(symbol.qualified_name.clone())
                .or_default()
                .push(symbol.id);
            if let Some(module_id) = symbol.module_id {
                self.symbols_by_module
                    .entry(module_id)
                    .or_default()
                    .push(symbol.id);
            }
        }
        Ok(())
    }
}

struct Relationships {
    references_by_target: BTreeMap<SymbolId, Vec<usize>>,
    callers_by_target: BTreeMap<SymbolId, Vec<usize>>,
    callees_by_caller: BTreeMap<SymbolId, Vec<usize>>,
    entry_points_by_symbol: BTreeMap<SymbolId, Vec<usize>>,
}

impl Relationships {
    fn build(
        references: &[ReferenceEdgeView],
        calls: &[CallEdgeView],
        entry_points: &[EntryPointView],
    ) -> Self {
        let mut relationships = Self {
            references_by_target: BTreeMap::new(),
            callers_by_target: BTreeMap::new(),
            callees_by_caller: BTreeMap::new(),
            entry_points_by_symbol: BTreeMap::new(),
        };
        for (index, reference) in references.iter().enumerate() {
            if let TargetResolution::Resolved(target) = reference.target.resolved {
                relationships
                    .references_by_target
                    .entry(target)
                    .or_default()
                    .push(index);
            }
        }
        for (index, call) in calls.iter().enumerate() {
            if let Some(caller) = call.caller_id {
                relationships
                    .callees_by_caller
                    .entry(caller)
                    .or_default()
                    .push(index);
            }
            if let TargetResolution::Resolved(target) = call.target.resolved {
                relationships
                    .callers_by_target
                    .entry(target)
                    .or_default()
                    .push(index);
            }
        }
        for (index, entry) in entry_points.iter().enumerate() {
            if let Some(symbol_id) = entry.symbol_id {
                relationships
                    .entry_points_by_symbol
                    .entry(symbol_id)
                    .or_default()
                    .push(index);
            }
        }
        relationships
    }
}

fn canonical_repository_root(root: &Path) -> Result<PathBuf, QueryError> {
    let display = root.display().to_string();
    let canonical = fs::canonicalize(root).map_err(|error| QueryError::InvalidRoot {
        path: display.clone(),
        message: error.to_string(),
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| QueryError::InvalidRoot {
        path: display.clone(),
        message: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(QueryError::InvalidRoot {
            path: display,
            message: "root is not a directory".to_owned(),
        });
    }
    Ok(canonical)
}

fn index_files(model: &RepositoryModel) -> Result<(FilesById, FilesByPath), QueryError> {
    let mut by_id = BTreeMap::new();
    let mut by_path = BTreeMap::new();
    for (index, file) in model.files.iter().enumerate() {
        if by_id.insert(file.id, index).is_some() {
            return Err(QueryError::invalid_model(format!(
                "duplicate file ID {}",
                file.id
            )));
        }
        if by_path.insert(file.path.clone(), file.id).is_some() {
            return Err(QueryError::invalid_model(format!(
                "duplicate repository path {}",
                file.path
            )));
        }
    }
    Ok((by_id, by_path))
}

fn validate_model_relations(model: &RepositoryModel, maps: &BaseMaps) -> Result<(), QueryError> {
    validate_modules(model, maps)?;
    validate_symbols(model, maps)?;
    validate_imports(model, maps)?;
    validate_references(model, maps)?;
    validate_calls(model, maps)?;
    validate_entry_points(model, maps)
}

fn validate_modules(model: &RepositoryModel, maps: &BaseMaps) -> Result<(), QueryError> {
    for module in &model.modules {
        require_known_file(module.file_id, "module", module.id, maps)?;
        if let Some(parent) = module.parent_id {
            if !maps.modules_by_id.contains_key(&parent) {
                return Err(QueryError::invalid_model(format!(
                    "module {} has unknown parent {}",
                    module.id, parent
                )));
            }
        }
    }
    Ok(())
}

fn validate_symbols(model: &RepositoryModel, maps: &BaseMaps) -> Result<(), QueryError> {
    for symbol in &model.symbols {
        require_known_file(symbol.file_id, "symbol", symbol.id, maps)?;
        if let Some(module_id) = symbol.module_id {
            if !maps.modules_by_id.contains_key(&module_id) {
                return Err(QueryError::invalid_model(format!(
                    "symbol {} has unknown module {}",
                    symbol.id, module_id
                )));
            }
        }
        if let Some(parent_id) = symbol.parent_id {
            if !maps.symbols_by_id.contains_key(&parent_id) {
                return Err(QueryError::invalid_model(format!(
                    "symbol {} has unknown parent {}",
                    symbol.id, parent_id
                )));
            }
        }
    }
    Ok(())
}

fn validate_imports(model: &RepositoryModel, maps: &BaseMaps) -> Result<(), QueryError> {
    let mut ids = BTreeSet::new();
    for import in &model.imports {
        require_unique_id(&mut ids, import.id, "import edge")?;
        require_known_file(import.source_file_id, "import edge", import.id, maps)?;
        if let Some(module_id) = import.source_module_id {
            if !maps.modules_by_id.contains_key(&module_id) {
                return Err(QueryError::invalid_model(format!(
                    "import edge {} has unknown source module {}",
                    import.id, module_id
                )));
            }
        }
        match &import.target {
            TargetResolution::Resolved(ImportItem::Module(id))
                if !maps.modules_by_id.contains_key(id) =>
            {
                return Err(QueryError::invalid_model(format!(
                    "import edge {} targets unknown module {}",
                    import.id, id
                )));
            }
            TargetResolution::Resolved(ImportItem::Symbol(id))
                if !maps.symbols_by_id.contains_key(id) =>
            {
                return Err(QueryError::invalid_model(format!(
                    "import edge {} targets unknown symbol {}",
                    import.id, id
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_references(model: &RepositoryModel, maps: &BaseMaps) -> Result<(), QueryError> {
    let mut ids = BTreeSet::new();
    for reference in &model.references {
        require_unique_id(&mut ids, reference.id, "reference edge")?;
        require_known_file(
            reference.source_file_id,
            "reference edge",
            reference.id,
            maps,
        )?;
        validate_optional_symbol(reference.source_symbol_id, reference.id, "reference", maps)?;
        validate_resolved_symbol(&reference.target, reference.id, "reference", maps)?;
    }
    Ok(())
}

fn validate_calls(model: &RepositoryModel, maps: &BaseMaps) -> Result<(), QueryError> {
    let mut ids = BTreeSet::new();
    for call in &model.calls {
        require_unique_id(&mut ids, call.id, "call edge")?;
        require_known_file(call.source_file_id, "call edge", call.id, maps)?;
        validate_optional_symbol(call.caller_id, call.id, "call", maps)?;
        validate_resolved_symbol(&call.target, call.id, "call", maps)?;
    }
    Ok(())
}

fn validate_entry_points(model: &RepositoryModel, maps: &BaseMaps) -> Result<(), QueryError> {
    let mut ids = BTreeSet::new();
    for entry in &model.entry_points {
        require_unique_id(&mut ids, entry.id, "entry point")?;
        require_known_file(entry.file_id, "entry point", entry.id, maps)?;
        validate_optional_symbol(entry.symbol_id, entry.id, "entry point", maps)?;
    }
    Ok(())
}

fn require_unique_id<T: Copy + Ord + std::fmt::Display>(
    ids: &mut BTreeSet<T>,
    id: T,
    entity: &str,
) -> Result<(), QueryError> {
    if ids.insert(id) {
        Ok(())
    } else {
        Err(QueryError::invalid_model(format!(
            "duplicate {entity} ID {id}"
        )))
    }
}

fn require_known_file<T: std::fmt::Display, U: std::fmt::Display>(
    file_id: FileId,
    entity: T,
    id: U,
    maps: &BaseMaps,
) -> Result<(), QueryError> {
    if maps.files_by_id.contains_key(&file_id) {
        Ok(())
    } else {
        Err(QueryError::invalid_model(format!(
            "{entity} {id} has unknown file {file_id}"
        )))
    }
}

fn validate_optional_symbol<T: std::fmt::Display>(
    symbol_id: Option<SymbolId>,
    edge_id: T,
    entity: &str,
    maps: &BaseMaps,
) -> Result<(), QueryError> {
    if let Some(symbol_id) = symbol_id {
        if !maps.symbols_by_id.contains_key(&symbol_id) {
            return Err(QueryError::invalid_model(format!(
                "{entity} {edge_id} has unknown source symbol {symbol_id}"
            )));
        }
    }
    Ok(())
}

fn validate_resolved_symbol<T: std::fmt::Display>(
    target: &TargetResolution<SymbolId>,
    edge_id: T,
    entity: &str,
    maps: &BaseMaps,
) -> Result<(), QueryError> {
    if let TargetResolution::Resolved(symbol_id) = target {
        if !maps.symbols_by_id.contains_key(symbol_id) {
            return Err(QueryError::invalid_model(format!(
                "{entity} {edge_id} targets unknown symbol {symbol_id}"
            )));
        }
    }
    Ok(())
}

fn build_reference_views(model: &RepositoryModel, maps: &BaseMaps) -> Vec<ReferenceEdgeView> {
    model
        .references
        .iter()
        .map(|reference| ReferenceEdgeView {
            id: reference.id,
            source_file_id: reference.source_file_id,
            source_path: file_path(model, maps, reference.source_file_id),
            source_symbol_id: reference.source_symbol_id,
            target: resolve_target(
                &reference.target,
                reference.source_file_id,
                reference.source_symbol_id,
                model,
                maps,
            ),
            kind: reference.kind.clone(),
            span: reference.span,
        })
        .collect()
}

fn build_call_views(model: &RepositoryModel, maps: &BaseMaps) -> Vec<CallEdgeView> {
    model
        .calls
        .iter()
        .map(|call| CallEdgeView {
            id: call.id,
            source_file_id: call.source_file_id,
            source_path: file_path(model, maps, call.source_file_id),
            caller_id: call.caller_id,
            target: resolve_target(
                &call.target,
                call.source_file_id,
                call.caller_id,
                model,
                maps,
            ),
            span: call.span,
            confidence: call.confidence,
        })
        .collect()
}

fn build_entry_point_views(model: &RepositoryModel, maps: &BaseMaps) -> Vec<EntryPointView> {
    model
        .entry_points
        .iter()
        .map(|entry| EntryPointView {
            id: entry.id,
            kind: entry.kind.clone(),
            label: entry.label.clone(),
            file_id: entry.file_id,
            path: file_path(model, maps, entry.file_id),
            symbol_id: entry.symbol_id,
            span: entry.span,
        })
        .collect()
}

fn resolve_target(
    original: &TargetResolution<SymbolId>,
    source_file_id: FileId,
    source_symbol_id: Option<SymbolId>,
    model: &RepositoryModel,
    maps: &BaseMaps,
) -> ResolvedTargetView {
    let TargetResolution::Unresolved(unresolved) = original else {
        return ResolvedTargetView {
            original: original.clone(),
            resolved: original.clone(),
            method: ResolutionMethod::Model,
        };
    };

    if let Some(candidates) = maps.symbols_by_qualified_name.get(&unresolved.name) {
        return unique_resolution(original, candidates, ResolutionMethod::ExactQualifiedName);
    }

    let simple_name = target_simple_name(&unresolved.name);
    let Some(candidates) = maps.symbols_by_name.get(simple_name) else {
        return unresolved_view(original);
    };
    let source_module = context_module(source_file_id, source_symbol_id, model, maps);
    let same_module = candidates.iter().copied().filter(|candidate| {
        source_module.is_some_and(|module_id| {
            symbol_for_id(model, maps, *candidate).module_id == Some(module_id)
        })
    });
    let same_file = candidates
        .iter()
        .copied()
        .filter(|candidate| symbol_for_id(model, maps, *candidate).file_id == source_file_id);
    let contextual = same_module.chain(same_file).collect::<BTreeSet<_>>();
    match contextual.len() {
        0 => unique_resolution(original, candidates, ResolutionMethod::UniqueSimpleName),
        1 => {
            let Some(candidate) = contextual.first().copied() else {
                return unresolved_view(original);
            };
            let method = if source_module.is_some_and(|module_id| {
                symbol_for_id(model, maps, candidate).module_id == Some(module_id)
            }) {
                ResolutionMethod::SameModule
            } else {
                ResolutionMethod::SameFile
            };
            resolved_view(original, candidate, method)
        }
        _ => unresolved_view(original),
    }
}

fn resolved_view(
    original: &TargetResolution<SymbolId>,
    candidate: SymbolId,
    method: ResolutionMethod,
) -> ResolvedTargetView {
    ResolvedTargetView {
        original: original.clone(),
        resolved: TargetResolution::Resolved(candidate),
        method,
    }
}

fn unique_resolution(
    original: &TargetResolution<SymbolId>,
    candidates: &[SymbolId],
    method: ResolutionMethod,
) -> ResolvedTargetView {
    if let [candidate] = candidates {
        resolved_view(original, *candidate, method)
    } else {
        unresolved_view(original)
    }
}

fn unresolved_view(original: &TargetResolution<SymbolId>) -> ResolvedTargetView {
    ResolvedTargetView {
        original: original.clone(),
        resolved: original.clone(),
        method: ResolutionMethod::Unresolved,
    }
}

fn context_module(
    source_file_id: FileId,
    source_symbol_id: Option<SymbolId>,
    model: &RepositoryModel,
    maps: &BaseMaps,
) -> Option<ModuleId> {
    source_symbol_id
        .map(|id| symbol_for_id(model, maps, id))
        .and_then(|symbol| symbol.module_id)
        .or_else(|| {
            maps.modules_by_file
                .get(&source_file_id)
                .and_then(|modules| {
                    modules
                        .as_slice()
                        .first()
                        .copied()
                        .filter(|_| modules.len() == 1)
                })
        })
}

fn target_simple_name(name: &str) -> &str {
    name.rsplit([':', '.', '/', '#'])
        .find(|component| !component.is_empty())
        .unwrap_or(name)
}

fn file_path(model: &RepositoryModel, maps: &BaseMaps, file_id: FileId) -> RepositoryPath {
    model.files[maps.files_by_id[&file_id]].path.clone()
}

fn symbol_for_id<'a>(
    model: &'a RepositoryModel,
    maps: &BaseMaps,
    symbol_id: SymbolId,
) -> &'a Symbol {
    &model.symbols[maps.symbols_by_id[&symbol_id]]
}

fn sort_model(model: &mut RepositoryModel) {
    model
        .files
        .sort_by(|left, right| left.path.cmp(&right.path).then(left.id.cmp(&right.id)));
    model.modules.sort_by(compare_modules);
    model.symbols.sort_by(|left, right| {
        left.qualified_name
            .cmp(&right.qualified_name)
            .then(left.file_id.cmp(&right.file_id))
            .then_with(|| compare_spans(left.span, right.span))
            .then(left.id.cmp(&right.id))
    });
    model.imports.sort_by(|left, right| {
        left.source_file_id
            .cmp(&right.source_file_id)
            .then_with(|| compare_spans(left.span, right.span))
            .then(left.id.cmp(&right.id))
    });
    model.references.sort_by(compare_reference_edges);
    model.calls.sort_by(compare_call_edges);
    model.entry_points.sort_by(compare_entry_points);
}

fn compare_modules(left: &Module, right: &Module) -> Ordering {
    left.qualified_name
        .cmp(&right.qualified_name)
        .then(left.file_id.cmp(&right.file_id))
        .then_with(|| compare_optional_spans(left.span, right.span))
        .then(left.id.cmp(&right.id))
}

fn compare_reference_edges(left: &ReferenceEdge, right: &ReferenceEdge) -> Ordering {
    left.source_file_id
        .cmp(&right.source_file_id)
        .then_with(|| compare_spans(left.span, right.span))
        .then(left.id.cmp(&right.id))
}

fn compare_call_edges(left: &CallEdge, right: &CallEdge) -> Ordering {
    left.source_file_id
        .cmp(&right.source_file_id)
        .then_with(|| compare_spans(left.span, right.span))
        .then(left.id.cmp(&right.id))
}

fn compare_entry_points(left: &EntryPoint, right: &EntryPoint) -> Ordering {
    left.file_id
        .cmp(&right.file_id)
        .then_with(|| compare_spans(left.span, right.span))
        .then(left.label.cmp(&right.label))
        .then(left.id.cmp(&right.id))
}

fn compare_optional_spans(left: Option<SourceSpan>, right: Option<SourceSpan>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare_spans(left, right),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_spans(left: SourceSpan, right: SourceSpan) -> Ordering {
    left.start()
        .cmp(&right.start())
        .then(left.end().cmp(&right.end()))
}

fn file_summary(file: &codeatlas_core::FileInfo) -> FileSummary {
    FileSummary {
        id: file.id,
        path: file.path.clone(),
        language: file.language.clone(),
    }
}

fn result_limit(value: Option<usize>) -> Result<usize, QueryError> {
    bounded_usize(
        value.unwrap_or(DEFAULT_RESULT_LIMIT),
        1,
        MAX_RESULT_LIMIT,
        "limit",
    )
}

fn repository_overview_limit(value: Option<usize>) -> Result<usize, QueryError> {
    bounded_usize(
        value.unwrap_or(DEFAULT_REPOSITORY_OVERVIEW_LIMIT),
        1,
        MAX_REPOSITORY_OVERVIEW_LIMIT,
        "limit",
    )
}

fn is_test_entry_point(entry: &EntryPointView) -> bool {
    matches!(entry.kind, EntryPointKind::Test | EntryPointKind::Benchmark)
}

fn trace_depth(value: Option<u32>) -> Result<u32, QueryError> {
    let value = value.unwrap_or(DEFAULT_TRACE_DEPTH);
    if value <= MAX_TRACE_DEPTH {
        Ok(value)
    } else {
        Err(QueryError::invalid_query(format!(
            "max_depth must be at most {MAX_TRACE_DEPTH}, got {value}"
        )))
    }
}

fn trace_nodes(value: Option<usize>) -> Result<usize, QueryError> {
    bounded_usize(
        value.unwrap_or(DEFAULT_TRACE_NODES),
        1,
        MAX_TRACE_NODES,
        "max_nodes",
    )
}

fn bounded_usize(
    value: usize,
    minimum: usize,
    maximum: usize,
    field: &str,
) -> Result<usize, QueryError> {
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(QueryError::invalid_query(format!(
            "{field} must be between {minimum} and {maximum}, got {value}"
        )))
    }
}

fn validate_search_term(query: &str) -> Result<(), QueryError> {
    if query.is_empty() {
        Err(QueryError::invalid_query("query must not be empty"))
    } else if query.len() > MAX_QUERY_BYTES {
        Err(QueryError::invalid_query(format!(
            "query must not exceed {MAX_QUERY_BYTES} UTF-8 bytes"
        )))
    } else {
        Ok(())
    }
}

fn path_has_prefix(path: &RepositoryPath, prefix: &RepositoryPath) -> bool {
    path == prefix
        || path
            .as_str()
            .strip_prefix(prefix.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn symbol_match_rank(symbol: &Symbol, query: &str, case_sensitive: bool) -> Option<u8> {
    let qualified = comparable(&symbol.qualified_name, case_sensitive);
    let name = comparable(&symbol.name, case_sensitive);
    let query = comparable(query, case_sensitive);
    if qualified == query {
        Some(0)
    } else if name == query {
        Some(1)
    } else if qualified.contains(&query) {
        Some(2)
    } else if name.contains(&query) {
        Some(3)
    } else {
        None
    }
}

fn comparable(value: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        value.to_owned()
    } else {
        value.to_lowercase()
    }
}

fn compare_symbols(
    left: &Symbol,
    right: &Symbol,
    files_by_id: &BTreeMap<FileId, usize>,
    model: &RepositoryModel,
) -> Ordering {
    left.qualified_name
        .cmp(&right.qualified_name)
        .then_with(|| {
            model.files[files_by_id[&left.file_id]]
                .path
                .cmp(&model.files[files_by_id[&right.file_id]].path)
        })
        .then_with(|| compare_spans(left.span, right.span))
        .then(left.id.cmp(&right.id))
}

struct FileSelection {
    content: String,
    end_line: u32,
    span: SourceSpan,
    truncated: bool,
}

#[derive(Clone, Copy)]
enum FileSelectionMode {
    Bounded,
    Complete,
}

fn select_file_range(
    source: &str,
    query: &ReadFileQuery,
    mode: FileSelectionMode,
) -> Result<FileSelection, QueryError> {
    let start_line = query.start_line.unwrap_or(1);
    if start_line == 0 {
        return Err(QueryError::invalid_query("start_line must be at least 1"));
    }
    if let Some(end_line) = query.end_line {
        if end_line < start_line {
            return Err(QueryError::invalid_query(format!(
                "end_line {end_line} is before start_line {start_line}"
            )));
        }
    }

    let starts = line_starts(source);
    let total_lines = u32::try_from(starts.len())
        .map_err(|_| QueryError::invalid_query("source has too many lines"))?;
    if start_line > total_lines {
        return Err(QueryError::invalid_query(format!(
            "start_line {start_line} is past end of file at line {total_lines}"
        )));
    }
    let requested_end = query.end_line.unwrap_or_else(|| match mode {
        FileSelectionMode::Bounded => start_line.saturating_add(DEFAULT_READ_LINES - 1),
        FileSelectionMode::Complete => total_lines,
    });
    let requested_end_in_file = requested_end.min(total_lines);
    let end_line = match mode {
        FileSelectionMode::Bounded => {
            requested_end_in_file.min(start_line.saturating_add(MAX_READ_LINES.saturating_sub(1)))
        }
        FileSelectionMode::Complete => requested_end_in_file,
    };
    let start_offset = starts[to_usize(start_line - 1)];
    let end_index = to_usize(end_line);
    let range_end = starts.get(end_index).copied().unwrap_or(source.len());
    let capped_end = match mode {
        FileSelectionMode::Bounded => {
            floor_char_boundary(source, (start_offset + MAX_READ_BYTES).min(range_end))
        }
        FileSelectionMode::Complete => range_end,
    };
    let content = source[start_offset..capped_end].to_owned();
    let end_position = position_for_offset(&starts, capped_end)?;
    let returned_end_line = if end_position.column() == 0 && end_position.line() > start_line {
        end_position.line() - 1
    } else {
        end_position.line()
    };
    let span = SourceSpan::from_positions(
        SourcePosition::new(start_line, 0)
            .map_err(|error| QueryError::invalid_query(error.to_string()))?,
        end_position,
    )
    .map_err(|error| QueryError::invalid_query(error.to_string()))?;
    Ok(FileSelection {
        content,
        end_line: returned_end_line,
        span,
        truncated: matches!(mode, FileSelectionMode::Bounded)
            && (end_line < requested_end_in_file
                || capped_end < range_end
                || (query.end_line.is_none() && end_line < total_lines)),
    })
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        source
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
    );
    starts
}

fn position_for_offset(starts: &[usize], offset: usize) -> Result<SourcePosition, QueryError> {
    let index = starts
        .partition_point(|start| *start <= offset)
        .saturating_sub(1);
    let line = u32::try_from(index + 1)
        .map_err(|_| QueryError::invalid_query("source has too many lines"))?;
    let column = u32::try_from(offset - starts[index])
        .map_err(|_| QueryError::invalid_query("source line is too long"))?;
    SourcePosition::new(line, column).map_err(|error| QueryError::invalid_query(error.to_string()))
}

const fn to_usize(value: u32) -> usize {
    value as usize
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

struct SearchState {
    limit: usize,
    matches: Vec<SearchMatch>,
    evidence: Vec<Evidence>,
    truncated: bool,
    skipped_file_count: usize,
    skipped_files: Vec<SkippedFile>,
}

impl SearchState {
    const fn new(limit: usize) -> Self {
        Self {
            limit,
            matches: Vec::new(),
            evidence: Vec::new(),
            truncated: false,
            skipped_file_count: 0,
            skipped_files: Vec::new(),
        }
    }

    fn skip(&mut self, path: RepositoryPath, reason: String) {
        self.skipped_file_count += 1;
        if self.skipped_files.len() < MAX_SKIPPED_FILES {
            self.skipped_files.push(SkippedFile { path, reason });
        }
    }
}

struct TraceTraversal {
    root: SymbolSummary,
    direction: TraceDirection,
    max_nodes: usize,
    edge_limit: usize,
    queue: VecDeque<(SymbolId, u32)>,
    visited: BTreeMap<SymbolId, u32>,
    nodes: Vec<TraceNode>,
    edges: Vec<TraceEdge>,
    evidence: Vec<Evidence>,
    truncated_by_depth: bool,
    truncated_by_node_limit: bool,
    truncated_by_edge_limit: bool,
}

impl TraceTraversal {
    fn new(root: SymbolSummary, direction: TraceDirection, max_nodes: usize) -> Self {
        let mut queue = VecDeque::new();
        queue.push_back((root.id, 0));
        let mut visited = BTreeMap::new();
        visited.insert(root.id, 0);
        let nodes = vec![TraceNode {
            symbol: root.clone(),
            depth: 0,
        }];
        Self {
            root,
            direction,
            max_nodes,
            edge_limit: max_nodes.saturating_mul(8).clamp(8, MAX_TRACE_EDGES),
            queue,
            visited,
            nodes,
            edges: Vec::new(),
            evidence: Vec::new(),
            truncated_by_depth: false,
            truncated_by_node_limit: false,
            truncated_by_edge_limit: false,
        }
    }

    fn finish(self) -> TraceCallResult {
        TraceCallResult {
            root: self.root,
            direction: self.direction,
            nodes: self.nodes,
            edges: self.edges,
            truncated_by_depth: self.truncated_by_depth,
            truncated_by_node_limit: self.truncated_by_node_limit,
            truncated_by_edge_limit: self.truncated_by_edge_limit,
        }
    }
}

impl RepositoryIndex {
    fn indexed_file_id(&self, path: &RepositoryPath) -> Result<FileId, QueryError> {
        self.files_by_path
            .get(path)
            .copied()
            .ok_or_else(|| QueryError::not_found("indexed file", path.to_string()))
    }

    fn read_source(&self, path: &RepositoryPath) -> Result<String, QueryError> {
        self.indexed_file_id(path)?;
        let joined = self.root.join(path.as_str());
        let canonical = fs::canonicalize(&joined).map_err(|error| QueryError::FileRead {
            path: path.to_string(),
            message: error.to_string(),
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(QueryError::PathEscape {
                path: path.to_string(),
            });
        }
        let metadata = fs::metadata(&canonical).map_err(|error| QueryError::FileRead {
            path: path.to_string(),
            message: error.to_string(),
        })?;
        if !metadata.is_file() {
            return Err(QueryError::NotAFile {
                path: path.to_string(),
            });
        }
        if metadata.len() > MAX_SOURCE_BYTES as u64 {
            return Err(QueryError::FileTooLarge {
                path: path.to_string(),
                limit_bytes: MAX_SOURCE_BYTES,
            });
        }
        let file = File::open(&canonical).map_err(|error| QueryError::FileRead {
            path: path.to_string(),
            message: error.to_string(),
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_SOURCE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| QueryError::FileRead {
                path: path.to_string(),
                message: error.to_string(),
            })?;
        if bytes.len() > MAX_SOURCE_BYTES {
            return Err(QueryError::FileTooLarge {
                path: path.to_string(),
                limit_bytes: MAX_SOURCE_BYTES,
            });
        }
        if bytes.contains(&0) {
            return Err(QueryError::BinaryFile {
                path: path.to_string(),
            });
        }
        String::from_utf8(bytes).map_err(|_| QueryError::BinaryFile {
            path: path.to_string(),
        })
    }

    fn require_symbol(&self, symbol_id: SymbolId) -> Result<&Symbol, QueryError> {
        self.symbol_by_id(symbol_id)
            .ok_or_else(|| QueryError::not_found("symbol", symbol_id.to_string()))
    }

    #[must_use]
    pub fn symbol_by_id(&self, symbol_id: SymbolId) -> Option<&Symbol> {
        self.symbols_by_id
            .get(&symbol_id)
            .map(|index| &self.model.symbols[*index])
    }

    fn require_module(&self, module_id: ModuleId) -> Result<&Module, QueryError> {
        self.module_by_id(module_id)
            .ok_or_else(|| QueryError::not_found("module", module_id.to_string()))
    }

    #[must_use]
    pub fn module_by_id(&self, module_id: ModuleId) -> Option<&Module> {
        self.modules_by_id
            .get(&module_id)
            .map(|index| &self.model.modules[*index])
    }

    fn symbol_summary(&self, symbol: &Symbol) -> SymbolSummary {
        SymbolSummary {
            id: symbol.id,
            name: symbol.name.clone(),
            qualified_name: symbol.qualified_name.clone(),
            kind: symbol.kind.clone(),
            file_id: symbol.file_id,
            path: self.model.files[self.files_by_id[&symbol.file_id]]
                .path
                .clone(),
            module_id: symbol.module_id,
            span: symbol.span,
            parent_id: symbol.parent_id,
        }
    }

    fn module_summary(&self, module: &Module) -> ModuleSummary {
        ModuleSummary {
            id: module.id,
            name: module.name.clone(),
            qualified_name: module.qualified_name.clone(),
            file_id: module.file_id,
            path: self.model.files[self.files_by_id[&module.file_id]]
                .path
                .clone(),
            span: module.span,
            parent_id: module.parent_id,
        }
    }

    fn definition_view(&self, symbol: &Symbol) -> DefinitionView {
        DefinitionView {
            symbol_id: symbol.id,
            file_id: symbol.file_id,
            path: self.model.files[self.files_by_id[&symbol.file_id]]
                .path
                .clone(),
            span: symbol.span,
        }
    }

    fn related_references(&self, symbol_id: SymbolId, limit: usize) -> Vec<ReferenceEdgeView> {
        self.references_by_target
            .get(&symbol_id)
            .into_iter()
            .flatten()
            .take(limit)
            .map(|index| self.references[*index].clone())
            .collect()
    }

    fn related_calls(
        &self,
        relationships: &BTreeMap<SymbolId, Vec<usize>>,
        symbol_id: SymbolId,
        limit: usize,
    ) -> Vec<CallEdgeView> {
        relationships
            .get(&symbol_id)
            .into_iter()
            .flatten()
            .take(limit)
            .map(|index| self.calls[*index].clone())
            .collect()
    }

    fn symbol_entry_points(&self, symbol_id: SymbolId) -> Vec<EntryPointView> {
        self.entry_points_by_symbol
            .get(&symbol_id)
            .into_iter()
            .flatten()
            .take(OVERVIEW_ITEM_LIMIT)
            .map(|index| self.entry_points[*index].clone())
            .collect()
    }

    fn relationship_count(
        relationships: &BTreeMap<SymbolId, Vec<usize>>,
        symbol_id: SymbolId,
    ) -> usize {
        relationships.get(&symbol_id).map_or(0, Vec::len)
    }

    fn has_truncated_symbol_relationship(&self, symbol_id: SymbolId) -> bool {
        Self::relationship_count(&self.references_by_target, symbol_id) > RELATED_EDGE_LIMIT
            || Self::relationship_count(&self.callers_by_target, symbol_id) > RELATED_EDGE_LIMIT
            || Self::relationship_count(&self.callees_by_caller, symbol_id) > RELATED_EDGE_LIMIT
            || self
                .entry_points_by_symbol
                .get(&symbol_id)
                .is_some_and(|entries| entries.len() > OVERVIEW_ITEM_LIMIT)
    }

    fn select_module(&self, query: &GetModuleQuery) -> Result<ModuleId, QueryError> {
        match (query.module_id, query.qualified_name.as_deref()) {
            (Some(module_id), None) => Ok(module_id),
            (None, Some("")) => Err(QueryError::invalid_query(
                "qualified_name must not be empty",
            )),
            (None, Some(name)) if name.len() > MAX_QUERY_BYTES => Err(QueryError::invalid_query(
                format!("qualified_name must not exceed {MAX_QUERY_BYTES} UTF-8 bytes"),
            )),
            (None, Some(name)) => match self.modules_by_qualified_name.get(name) {
                Some(ids) if ids.len() == 1 => Ok(ids[0]),
                Some(ids) => Err(QueryError::invalid_query(format!(
                    "qualified_name {name:?} is ambiguous across {} modules",
                    ids.len()
                ))),
                None => Err(QueryError::not_found("module", name)),
            },
            (Some(_), Some(_)) | (None, None) => Err(QueryError::invalid_query(
                "provide exactly one of module_id or qualified_name",
            )),
        }
    }

    fn repository_counts(&self) -> RepositoryCounts {
        RepositoryCounts {
            files: self.model.files.len(),
            modules: self.model.modules.len(),
            symbols: self.model.symbols.len(),
            imports: self.model.imports.len(),
            references: self.references.len(),
            calls: self.calls.len(),
            entry_points: self.entry_points.len(),
            unresolved_references: self
                .references
                .iter()
                .filter(|edge| !edge.target.resolved.is_resolved())
                .count(),
            unresolved_calls: self
                .calls
                .iter()
                .filter(|edge| !edge.target.resolved.is_resolved())
                .count(),
        }
    }
}

fn language_counts(model: &RepositoryModel) -> Vec<LanguageCount> {
    let mut counts: BTreeMap<String, (Language, usize)> = BTreeMap::new();
    for file in &model.files {
        let key = language_key(&file.language);
        let entry = counts.entry(key).or_insert((file.language.clone(), 0));
        entry.1 += 1;
    }
    counts
        .into_values()
        .map(|(language, files)| LanguageCount { language, files })
        .collect()
}

fn language_key(language: &Language) -> String {
    match language {
        Language::Rust => "rust".to_owned(),
        Language::Python => "python".to_owned(),
        Language::C => "c".to_owned(),
        Language::Cpp => "cpp".to_owned(),
        Language::Java => "java".to_owned(),
        Language::JavaScript => "javascript".to_owned(),
        Language::TypeScript => "typescript".to_owned(),
        Language::Go => "go".to_owned(),
        Language::Other(name) => format!("other:{name}"),
    }
}

impl RepositoryIndex {
    fn make_evidence(
        &self,
        file_id: FileId,
        span: SourceSpan,
        symbol_id: Option<SymbolId>,
        excerpt: Option<&str>,
    ) -> Evidence {
        let path = self.model.files[self.files_by_id[&file_id]].path.clone();
        let excerpt = excerpt
            .map(|value| bounded_text(value, MAX_EXCERPT_BYTES))
            .or_else(|| self.excerpt_for_span(&path, span));
        let repository_key = self.model.repository_id.to_string();
        let span_key = format!(
            "{}:{}-{}:{}",
            span.start().line(),
            span.start().column(),
            span.end().line(),
            span.end().column()
        );
        let symbol_key = symbol_id.map_or_else(String::new, |id| id.to_string());
        let id = EvidenceId::from_stable_parts(&[
            &repository_key,
            path.as_str(),
            &span_key,
            &symbol_key,
        ]);
        Evidence {
            id,
            file_id,
            path,
            span,
            symbol_id,
            excerpt,
        }
    }

    fn excerpt_for_span(&self, path: &RepositoryPath, span: SourceSpan) -> Option<String> {
        let source = self.read_source(path).ok()?;
        let starts = line_starts(&source);
        let start = offset_for_position(&source, &starts, span.start())?;
        let end = offset_for_position(&source, &starts, span.end())?;
        (end >= start).then(|| bounded_text(&source[start..end], MAX_EXCERPT_BYTES))
    }

    fn evidence_for_symbol(&self, symbol: &Symbol) -> Evidence {
        self.make_evidence(symbol.file_id, symbol.span, Some(symbol.id), None)
    }

    fn evidence_for_reference(&self, reference: &ReferenceEdgeView) -> Evidence {
        let symbol_id = reference
            .source_symbol_id
            .or(match reference.target.resolved {
                TargetResolution::Resolved(target) => Some(target),
                TargetResolution::Unresolved(_) => None,
            });
        self.make_evidence(reference.source_file_id, reference.span, symbol_id, None)
    }

    fn evidence_for_call(&self, call: &CallEdgeView) -> Evidence {
        self.make_evidence(call.source_file_id, call.span, call.caller_id, None)
    }

    fn evidence_for_symbol_detail(&self, data: &GetSymbolResult) -> Vec<Evidence> {
        let mut evidence = vec![self.make_evidence(
            data.definition.file_id,
            data.definition.span,
            Some(data.definition.symbol_id),
            None,
        )];
        evidence.extend(
            data.references
                .iter()
                .map(|reference| self.evidence_for_reference(reference)),
        );
        evidence.extend(data.callers.iter().map(|call| self.evidence_for_call(call)));
        evidence.extend(data.callees.iter().map(|call| self.evidence_for_call(call)));
        evidence.extend(
            data.entry_points
                .iter()
                .map(|entry| self.make_evidence(entry.file_id, entry.span, entry.symbol_id, None)),
        );
        normalize_evidence(evidence)
    }

    fn evidence_for_module_detail(&self, data: &GetModuleResult) -> Vec<Evidence> {
        let mut evidence = Vec::new();
        if let Some(span) = data.module.span {
            evidence.push(self.make_evidence(data.module.file_id, span, None, None));
        }
        evidence.extend(
            data.symbols.iter().map(|symbol| {
                self.make_evidence(symbol.file_id, symbol.span, Some(symbol.id), None)
            }),
        );
        normalize_evidence(evidence)
    }
}

fn offset_for_position(source: &str, starts: &[usize], position: SourcePosition) -> Option<usize> {
    let line_index = usize::try_from(position.line() - 1).ok()?;
    let line_start = *starts.get(line_index)?;
    let column = usize::try_from(position.column()).ok()?;
    let offset = line_start.checked_add(column)?;
    let line_end = starts.get(line_index + 1).copied().unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let content_end = line_start + line.len();
    if offset <= content_end && source.is_char_boundary(offset) {
        Some(offset)
    } else {
        None
    }
}

fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let content_limit = limit.saturating_sub(3);
    let end = floor_char_boundary(value, content_limit);
    format!("{}...", &value[..end])
}

fn normalize_evidence(mut evidence: Vec<Evidence>) -> Vec<Evidence> {
    evidence.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| compare_spans(left.span, right.span))
            .then(left.symbol_id.cmp(&right.symbol_id))
            .then(left.id.cmp(&right.id))
    });
    let mut seen = BTreeSet::new();
    evidence.retain(|item| seen.insert(item.id));
    evidence
}

impl RepositoryIndex {
    fn search_file(
        &self,
        file_id: FileId,
        path: &RepositoryPath,
        source: &str,
        query: &SearchCodeQuery,
        case_sensitive: bool,
        state: &mut SearchState,
    ) -> Result<(), QueryError> {
        for (line_index, raw_line) in source.split_inclusive('\n').enumerate() {
            let line = raw_line.strip_suffix('\n').unwrap_or(raw_line);
            let line = line.strip_suffix('\r').unwrap_or(line);
            let offsets =
                literal_match_offsets(line, &query.query, case_sensitive, state.limit + 1);
            for offset in offsets {
                if state.matches.len() >= state.limit {
                    state.truncated = true;
                    return Ok(());
                }
                let line_number = u32::try_from(line_index + 1)
                    .map_err(|_| QueryError::invalid_query("source has too many lines"))?;
                let start_column = u32::try_from(offset)
                    .map_err(|_| QueryError::invalid_query("source line is too long"))?;
                let end_column = u32::try_from(offset + query.query.len())
                    .map_err(|_| QueryError::invalid_query("source line is too long"))?;
                let span = SourceSpan::new(line_number, start_column, line_number, end_column)
                    .map_err(|error| QueryError::invalid_query(error.to_string()))?;
                let excerpt = excerpt_around(line, offset, query.query.len());
                state.matches.push(SearchMatch {
                    file_id,
                    path: path.clone(),
                    span,
                    line: excerpt.clone(),
                });
                state
                    .evidence
                    .push(self.make_evidence(file_id, span, None, Some(&excerpt)));
            }
        }
        Ok(())
    }
}

fn literal_match_offsets(
    line: &str,
    query: &str,
    case_sensitive: bool,
    limit: usize,
) -> Vec<usize> {
    if case_sensitive {
        line.match_indices(query)
            .take(limit)
            .map(|(offset, _)| offset)
            .collect()
    } else {
        line.to_ascii_lowercase()
            .match_indices(&query.to_ascii_lowercase())
            .take(limit)
            .map(|(offset, _)| offset)
            .collect()
    }
}

fn excerpt_around(line: &str, match_start: usize, match_len: usize) -> String {
    if line.len() <= MAX_SEARCH_LINE_BYTES {
        return line.to_owned();
    }
    let half = MAX_SEARCH_LINE_BYTES / 2;
    let desired_start = match_start.saturating_sub(half);
    let start = ceil_char_boundary(line, desired_start);
    let minimum_end = match_start.saturating_add(match_len).min(line.len());
    let desired_end = start
        .saturating_add(MAX_SEARCH_LINE_BYTES.saturating_sub(6))
        .max(minimum_end)
        .min(line.len());
    let end = floor_char_boundary(line, desired_end);
    let prefix = if start > 0 { "..." } else { "" };
    let suffix = if end < line.len() { "..." } else { "" };
    format!("{prefix}{}{suffix}", &line[start..end])
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    while !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

impl RepositoryIndex {
    fn trace_edge_indexes(&self, direction: TraceDirection, symbol_id: SymbolId) -> &[usize] {
        let relationships = match direction {
            TraceDirection::Callers => &self.callers_by_target,
            TraceDirection::Callees => &self.callees_by_caller,
        };
        relationships.get(&symbol_id).map_or(&[][..], Vec::as_slice)
    }

    fn expand_trace_node(
        &self,
        depth: u32,
        edge_indexes: &[usize],
        traversal: &mut TraceTraversal,
    ) -> bool {
        for edge_index in edge_indexes {
            if traversal.edges.len() >= traversal.edge_limit {
                traversal.truncated_by_edge_limit = true;
                return false;
            }
            let call = self.calls[*edge_index].clone();
            let next = trace_next_symbol(traversal.direction, &call);
            let revisited = next.is_some_and(|id| traversal.visited.contains_key(&id));
            let mut omitted = false;
            if let Some(next_id) = next {
                if !revisited {
                    if traversal.nodes.len() >= traversal.max_nodes {
                        traversal.truncated_by_node_limit = true;
                        omitted = true;
                    } else if let Some(symbol) = self.symbol_by_id(next_id) {
                        let next_depth = depth + 1;
                        traversal.visited.insert(next_id, next_depth);
                        traversal.queue.push_back((next_id, next_depth));
                        traversal.nodes.push(TraceNode {
                            symbol: self.symbol_summary(symbol),
                            depth: next_depth,
                        });
                    }
                }
            }
            traversal.evidence.push(self.evidence_for_call(&call));
            traversal.edges.push(TraceEdge {
                depth: depth + 1,
                call,
                next_symbol_id: next,
                revisited,
                omitted_by_node_limit: omitted,
            });
        }
        true
    }
}

fn trace_next_symbol(direction: TraceDirection, call: &CallEdgeView) -> Option<SymbolId> {
    match direction {
        TraceDirection::Callers => call.caller_id,
        TraceDirection::Callees => match call.target.resolved {
            TargetResolution::Resolved(target) => Some(target),
            TargetResolution::Unresolved(_) => None,
        },
    }
}
