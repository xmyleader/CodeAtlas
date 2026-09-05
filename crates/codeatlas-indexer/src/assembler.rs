use std::collections::{HashMap, HashSet};

use codeatlas_core::{
    CallEdge, CallEdgeId, EntryPoint, EntryPointId, EntryPointKind, FileId, FileInfo, ImportEdge,
    ImportEdgeId, Module, ModuleId, ParsedFile, ParsedSymbolId, ParsedTarget, ReferenceEdge,
    ReferenceEdgeId, ReferenceKind, RepositoryId, RepositoryModel, RepositoryPath, SourceSpan,
    Symbol, SymbolId, SymbolKind, TargetResolution, UnresolvedTarget,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySpec {
    pub name: String,
    pub stable_key: String,
}

impl RepositorySpec {
    #[must_use]
    pub fn new(name: impl Into<String>, stable_key: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            stable_key: stable_key.into(),
        }
    }

    #[must_use]
    pub fn repository_id(&self) -> RepositoryId {
        RepositoryId::from_stable_parts(&[&self.stable_key])
    }
}

#[derive(Debug, Clone)]
pub struct IndexAssembler {
    repository: RepositorySpec,
}

impl IndexAssembler {
    #[must_use]
    pub const fn new(repository: RepositorySpec) -> Self {
        Self { repository }
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositorySpec {
        &self.repository
    }

    /// Converts parser-local records into a deterministic repository model.
    ///
    /// # Errors
    ///
    /// Returns [`AssemblyError`] when files are duplicated or any parser-local
    /// relationship points at a symbol that was not declared in the same file.
    pub fn assemble(
        &self,
        parsed_files: impl IntoIterator<Item = ParsedFile>,
    ) -> Result<RepositoryModel, AssemblyError> {
        let mut parsed_files = parsed_files.into_iter().collect::<Vec<_>>();
        parsed_files.sort_by(|left, right| left.path.cmp(&right.path));

        let mut seen_paths = HashSet::new();
        let mut model = RepositoryModel::empty(
            self.repository.repository_id(),
            self.repository.name.clone(),
        );
        for parsed_file in parsed_files {
            if !seen_paths.insert(parsed_file.path.clone()) {
                return Err(AssemblyError::DuplicateFile {
                    path: parsed_file.path,
                });
            }
            merge_model(&mut model, self.assemble_file(&parsed_file)?);
        }
        sort_model(&mut model);
        Ok(model)
    }

    #[allow(clippy::too_many_lines)]
    fn assemble_file(&self, parsed: &ParsedFile) -> Result<RepositoryModel, AssemblyError> {
        let repository_id = self.repository.repository_id();
        let repository_id_key = repository_id.to_string();
        let path_key = parsed.path.as_str();
        let file_id = FileId::from_stable_parts(&[&repository_id_key, path_key]);
        let mut model = RepositoryModel::empty(repository_id, self.repository.name.clone());
        model.files.push(FileInfo {
            id: file_id,
            path: parsed.path.clone(),
            language: parsed.language.clone(),
        });

        let module_id = parsed.module.as_ref().map(|module| {
            ModuleId::from_stable_parts(&[&repository_id_key, path_key, &module.qualified_name])
        });
        if let (Some(parsed_module), Some(module_id)) = (&parsed.module, module_id) {
            model.modules.push(Module {
                id: module_id,
                name: parsed_module.name.clone(),
                qualified_name: parsed_module.qualified_name.clone(),
                file_id,
                span: parsed_module.span,
                parent_id: None,
            });
        }

        let mut local_to_global = HashMap::new();
        let mut canonical_counts = HashMap::<String, usize>::new();
        for symbol in &parsed.symbols {
            *canonical_counts
                .entry(symbol_canonical_key(symbol))
                .or_default() += 1;
        }

        let mut symbol_indices = (0..parsed.symbols.len()).collect::<Vec<_>>();
        symbol_indices.sort_by(|left, right| {
            let left = &parsed.symbols[*left];
            let right = &parsed.symbols[*right];
            (
                symbol_canonical_key(left),
                span_key(left.span),
                left.local_id,
            )
                .cmp(&(
                    symbol_canonical_key(right),
                    span_key(right.span),
                    right.local_id,
                ))
        });

        let mut canonical_occurrences = HashMap::<String, usize>::new();
        for index in symbol_indices {
            let symbol = &parsed.symbols[index];
            let canonical_key = symbol_canonical_key(symbol);
            let occurrence = canonical_occurrences
                .entry(canonical_key.clone())
                .or_default();
            let kind_key = symbol_kind_key(&symbol.kind);
            let symbol_id = if canonical_counts[&canonical_key] == 1 {
                SymbolId::from_stable_parts(&[
                    &repository_id_key,
                    path_key,
                    &kind_key,
                    &symbol.qualified_name,
                ])
            } else {
                let occurrence_key = occurrence.to_string();
                SymbolId::from_stable_parts(&[
                    &repository_id_key,
                    path_key,
                    &kind_key,
                    &symbol.qualified_name,
                    &occurrence_key,
                ])
            };
            *occurrence += 1;
            if local_to_global.insert(symbol.local_id, symbol_id).is_some() {
                return Err(AssemblyError::DuplicateLocalSymbolId {
                    path: parsed.path.clone(),
                    local_id: symbol.local_id,
                });
            }
        }

        for symbol in &parsed.symbols {
            let id = local_to_global[&symbol.local_id];
            let parent_id = resolve_optional_local(
                symbol.parent_id,
                &local_to_global,
                &parsed.path,
                "symbol parent",
            )?;
            model.symbols.push(Symbol {
                id,
                name: symbol.name.clone(),
                qualified_name: symbol.qualified_name.clone(),
                kind: symbol.kind.clone(),
                file_id,
                module_id,
                span: symbol.span,
                parent_id,
            });
        }

        for import in &parsed.imports {
            let span = span_key(import.span);
            let alias = import.alias.as_deref().unwrap_or("");
            let id = ImportEdgeId::from_stable_parts(&[
                &repository_id_key,
                path_key,
                &import.target,
                alias,
                &span,
            ]);
            model.imports.push(ImportEdge {
                id,
                source_file_id: file_id,
                source_module_id: module_id,
                target: TargetResolution::Unresolved(unresolved(&import.target)),
                span: import.span,
                alias: import.alias.clone(),
            });
        }

        for reference in &parsed.references {
            let source_symbol_id = resolve_optional_local(
                reference.source_id,
                &local_to_global,
                &parsed.path,
                "reference source",
            )?;
            let target = resolve_target(
                &reference.target,
                &local_to_global,
                &parsed.path,
                "reference target",
            )?;
            let source_key = optional_symbol_key(source_symbol_id);
            let target_key = target_key(&target);
            let kind_key = reference_kind_key(&reference.kind);
            let span = span_key(reference.span);
            let id = ReferenceEdgeId::from_stable_parts(&[
                &repository_id_key,
                path_key,
                &source_key,
                &target_key,
                &kind_key,
                &span,
            ]);
            model.references.push(ReferenceEdge {
                id,
                source_file_id: file_id,
                source_symbol_id,
                target,
                kind: reference.kind.clone(),
                span: reference.span,
            });
        }

        for call in &parsed.calls {
            let caller_id = resolve_optional_local(
                call.caller_id,
                &local_to_global,
                &parsed.path,
                "call caller",
            )?;
            let target =
                resolve_target(&call.target, &local_to_global, &parsed.path, "call target")?;
            let caller_key = optional_symbol_key(caller_id);
            let target_key = target_key(&target);
            let span = span_key(call.span);
            let id = CallEdgeId::from_stable_parts(&[
                &repository_id_key,
                path_key,
                &caller_key,
                &target_key,
                &span,
            ]);
            model.calls.push(CallEdge {
                id,
                source_file_id: file_id,
                caller_id,
                target,
                span: call.span,
                confidence: call.confidence,
            });
        }

        for entry_point in &parsed.entry_points {
            let symbol_id = resolve_optional_local(
                entry_point.symbol_id,
                &local_to_global,
                &parsed.path,
                "entry point symbol",
            )?;
            let kind_key = entry_point_kind_key(&entry_point.kind);
            let symbol_key = optional_symbol_key(symbol_id);
            let span = span_key(entry_point.span);
            let id = EntryPointId::from_stable_parts(&[
                &repository_id_key,
                path_key,
                &kind_key,
                &entry_point.label,
                &symbol_key,
                &span,
            ]);
            model.entry_points.push(EntryPoint {
                id,
                kind: entry_point.kind.clone(),
                label: entry_point.label.clone(),
                file_id,
                symbol_id,
                span: entry_point.span,
            });
        }

        sort_model(&mut model);
        Ok(model)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AssemblyError {
    #[error("parsed file appears more than once: {path}")]
    DuplicateFile { path: RepositoryPath },
    #[error("{path} declares local symbol ID {local_id:?} more than once")]
    DuplicateLocalSymbolId {
        path: RepositoryPath,
        local_id: ParsedSymbolId,
    },
    #[error("{path} has dangling local symbol ID {local_id:?} in {relation}")]
    DanglingLocalSymbolId {
        path: RepositoryPath,
        local_id: ParsedSymbolId,
        relation: &'static str,
    },
}

impl AssemblyError {
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        match self {
            Self::DuplicateFile { path }
            | Self::DuplicateLocalSymbolId { path, .. }
            | Self::DanglingLocalSymbolId { path, .. } => path,
        }
    }
}

fn resolve_optional_local(
    local_id: Option<ParsedSymbolId>,
    symbols: &HashMap<ParsedSymbolId, SymbolId>,
    path: &RepositoryPath,
    relation: &'static str,
) -> Result<Option<SymbolId>, AssemblyError> {
    local_id
        .map(|local_id| resolve_local(local_id, symbols, path, relation))
        .transpose()
}

fn resolve_target(
    target: &ParsedTarget,
    symbols: &HashMap<ParsedSymbolId, SymbolId>,
    path: &RepositoryPath,
    relation: &'static str,
) -> Result<TargetResolution<SymbolId>, AssemblyError> {
    match target {
        ParsedTarget::Local(local_id) => {
            resolve_local(*local_id, symbols, path, relation).map(TargetResolution::Resolved)
        }
        ParsedTarget::Unresolved(name) => Ok(TargetResolution::Unresolved(unresolved(name))),
    }
}

fn resolve_local(
    local_id: ParsedSymbolId,
    symbols: &HashMap<ParsedSymbolId, SymbolId>,
    path: &RepositoryPath,
    relation: &'static str,
) -> Result<SymbolId, AssemblyError> {
    symbols
        .get(&local_id)
        .copied()
        .ok_or_else(|| AssemblyError::DanglingLocalSymbolId {
            path: path.clone(),
            local_id,
            relation,
        })
}

fn unresolved(name: &str) -> UnresolvedTarget {
    UnresolvedTarget {
        name: name.to_owned(),
        reason: Some("cross-file and external resolution is not performed".to_owned()),
    }
}

fn symbol_canonical_key(symbol: &codeatlas_core::ParsedSymbol) -> String {
    format!(
        "{}\u{0}{}",
        symbol_kind_key(&symbol.kind),
        symbol.qualified_name
    )
}

fn symbol_kind_key(kind: &SymbolKind) -> String {
    match kind {
        SymbolKind::Function => "function".to_owned(),
        SymbolKind::Method => "method".to_owned(),
        SymbolKind::Struct => "struct".to_owned(),
        SymbolKind::Class => "class".to_owned(),
        SymbolKind::Enum => "enum".to_owned(),
        SymbolKind::Interface => "interface".to_owned(),
        SymbolKind::Trait => "trait".to_owned(),
        SymbolKind::Module => "module".to_owned(),
        SymbolKind::Namespace => "namespace".to_owned(),
        SymbolKind::Constant => "constant".to_owned(),
        SymbolKind::Static => "static".to_owned(),
        SymbolKind::Variable => "variable".to_owned(),
        SymbolKind::Field => "field".to_owned(),
        SymbolKind::Parameter => "parameter".to_owned(),
        SymbolKind::TypeAlias => "type_alias".to_owned(),
        SymbolKind::Macro => "macro".to_owned(),
        SymbolKind::Other(value) => format!("other:{value}"),
    }
}

fn reference_kind_key(kind: &ReferenceKind) -> String {
    match kind {
        ReferenceKind::Read => "read".to_owned(),
        ReferenceKind::Write => "write".to_owned(),
        ReferenceKind::Type => "type".to_owned(),
        ReferenceKind::Inheritance => "inheritance".to_owned(),
        ReferenceKind::Implementation => "implementation".to_owned(),
        ReferenceKind::Other(value) => format!("other:{value}"),
    }
}

fn entry_point_kind_key(kind: &EntryPointKind) -> String {
    match kind {
        EntryPointKind::Executable => "executable".to_owned(),
        EntryPointKind::Library => "library".to_owned(),
        EntryPointKind::Test => "test".to_owned(),
        EntryPointKind::Benchmark => "benchmark".to_owned(),
        EntryPointKind::WebRoute => "web_route".to_owned(),
        EntryPointKind::BackgroundTask => "background_task".to_owned(),
        EntryPointKind::Other(value) => format!("other:{value}"),
    }
}

fn optional_symbol_key(symbol_id: Option<SymbolId>) -> String {
    symbol_id.map_or_else(|| "none".to_owned(), |id| id.to_string())
}

fn target_key(target: &TargetResolution<SymbolId>) -> String {
    match target {
        TargetResolution::Resolved(id) => format!("resolved:{id}"),
        TargetResolution::Unresolved(target) => format!("unresolved:{}", target.name),
    }
}

fn span_key(span: SourceSpan) -> String {
    format!(
        "{}:{}-{}:{}",
        span.start().line(),
        span.start().column(),
        span.end().line(),
        span.end().column()
    )
}

fn merge_model(target: &mut RepositoryModel, mut source: RepositoryModel) {
    target.files.append(&mut source.files);
    target.modules.append(&mut source.modules);
    target.symbols.append(&mut source.symbols);
    target.imports.append(&mut source.imports);
    target.references.append(&mut source.references);
    target.calls.append(&mut source.calls);
    target.entry_points.append(&mut source.entry_points);
}

pub(crate) fn sort_model(model: &mut RepositoryModel) {
    model
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    model.modules.sort_by_key(|module| module.id);
    model.symbols.sort_by_key(|symbol| symbol.id);
    model.imports.sort_by_key(|import| import.id);
    model.references.sort_by_key(|reference| reference.id);
    model.calls.sort_by_key(|call| call.id);
    model.entry_points.sort_by_key(|entry_point| entry_point.id);
}
