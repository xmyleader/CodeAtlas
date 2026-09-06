use std::path::Path;

use codeatlas_core::{ParseInput, Progress, ProgressPhase, RepositoryModel};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CachedIndex, CancellationCheck, DiagnosticSeverity, DiagnosticStage, FileDiagnostic,
    IndexAssembler, IndexStoreError, JsonIndexStore, ParserRegistry, RepositoryScanner,
    RepositorySpec, ScanConfig, ScanError, assembler::sort_model, scanner::diagnostic_order,
};

const INDEX_FINGERPRINT_DOMAIN: &[u8] = b"codeatlas-index-fingerprint-v1";

pub type ProgressCallback<'a> = dyn Fn(Progress) + Send + Sync + 'a;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCounts {
    pub scanned_files: u64,
    pub parsed_files: u64,
    pub skipped_files: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexReport {
    pub model: RepositoryModel,
    pub counts: IndexCounts,
    pub diagnostics: Vec<FileDiagnostic>,
    pub cache_hit: bool,
}

#[derive(Debug)]
pub struct Indexer {
    scanner: RepositoryScanner,
    parsers: ParserRegistry,
    store: Option<JsonIndexStore>,
    cache_revision: String,
}

impl Indexer {
    #[must_use]
    pub fn new(scan_config: ScanConfig, parsers: ParserRegistry) -> Self {
        Self {
            scanner: RepositoryScanner::new(scan_config),
            parsers,
            store: None,
            cache_revision: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    #[must_use]
    pub fn with_store(mut self, store: JsonIndexStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Adds a caller-controlled cache revision for parser or configuration changes.
    #[must_use]
    pub fn with_cache_revision(mut self, revision: impl Into<String>) -> Self {
        self.cache_revision = revision.into();
        self
    }

    #[must_use]
    pub const fn scanner(&self) -> &RepositoryScanner {
        &self.scanner
    }

    #[must_use]
    pub const fn parsers(&self) -> &ParserRegistry {
        &self.parsers
    }

    /// Scans, parses, and assembles a repository. File-level failures become
    /// diagnostics and do not abort unrelated files.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] only when the repository root itself cannot be scanned.
    #[allow(clippy::too_many_lines)]
    pub fn index(
        &self,
        root: impl AsRef<Path>,
        repository: RepositorySpec,
        progress: Option<&ProgressCallback<'_>>,
    ) -> Result<IndexReport, IndexError> {
        self.index_cancellable(root, repository, progress, None)
    }

    /// Indexes a repository with cooperative cancellation at scan, parse,
    /// assembly, and cache persistence boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`IndexError`] when scanning fails or cancellation is requested.
    #[allow(clippy::too_many_lines)]
    pub fn index_cancellable(
        &self,
        root: impl AsRef<Path>,
        repository: RepositorySpec,
        progress: Option<&ProgressCallback<'_>>,
        cancellation: Option<&CancellationCheck<'_>>,
    ) -> Result<IndexReport, IndexError> {
        ensure_active(cancellation)?;
        emit_progress(
            progress,
            ProgressPhase::Scanning,
            "scanning repository",
            Some(0),
            None,
        );
        let scan =
            self.scanner
                .scan_cancellable(root, cancellation)
                .map_err(|error| match error {
                    ScanError::Cancelled => IndexError::Cancelled,
                    error => IndexError::Scan(error),
                })?;
        let scanned_files = u64::try_from(scan.files.len()).unwrap_or(u64::MAX);
        emit_progress(
            progress,
            ProgressPhase::Scanning,
            "repository scan complete",
            Some(scanned_files),
            Some(scanned_files),
        );

        let fingerprint = self.index_fingerprint(&scan.fingerprint);
        ensure_active(cancellation)?;
        let repository_id = repository.repository_id();
        let repository_name = repository.name.clone();
        let cache_key = repository_id.to_string();
        let mut diagnostics = scan.diagnostics;
        if let Some(store) = &self.store {
            ensure_active(cancellation)?;
            match store.read_cancellable(&cache_key, cancellation) {
                Ok(Some(cached))
                    if cached.fingerprint == fingerprint
                        && cached.model.repository_id == repository_id
                        && cached.model.name == repository_name =>
                {
                    diagnostics.extend(cached.diagnostics);
                    diagnostics.sort_by(diagnostic_order);
                    ensure_active(cancellation)?;
                    emit_progress(
                        progress,
                        ProgressPhase::Indexing,
                        "loaded unchanged repository index from cache",
                        Some(scanned_files),
                        Some(scanned_files),
                    );
                    return Ok(IndexReport {
                        model: cached.model,
                        counts: IndexCounts {
                            scanned_files,
                            parsed_files: cached.parsed_files,
                            skipped_files: scan.skipped_files.saturating_add(cached.skipped_files),
                        },
                        diagnostics,
                        cache_hit: true,
                    });
                }
                Ok(_) => {}
                Err(IndexStoreError::Cancelled) => return Err(IndexError::Cancelled),
                Err(error) => diagnostics.push(FileDiagnostic::new(
                    None,
                    DiagnosticStage::Cache,
                    DiagnosticSeverity::Warning,
                    "cache_read_error",
                    error.to_string(),
                )),
            }
        }

        let total_files = scanned_files;
        let mut parsed_files = Vec::new();
        let mut parse_skipped_files = 0_u64;
        let mut parsed_count = 0_u64;
        for (index, file) in scan.files.iter().enumerate() {
            ensure_active(cancellation)?;
            emit_progress(
                progress,
                ProgressPhase::Parsing,
                format!("parsing {}", file.path),
                Some(u64::try_from(index).unwrap_or(u64::MAX)),
                Some(total_files),
            );
            match self.parsers.parse(
                &file.language,
                ParseInput {
                    path: &file.path,
                    source: &file.source,
                },
            ) {
                Ok(parsed) => {
                    parsed_count = parsed_count.saturating_add(1);
                    parsed_files.push(parsed);
                }
                Err(error) => {
                    parse_skipped_files = parse_skipped_files.saturating_add(1);
                    diagnostics.push(FileDiagnostic::new(
                        Some(error.path().clone()),
                        DiagnosticStage::Parse,
                        DiagnosticSeverity::Error,
                        error.code(),
                        error.to_string(),
                    ));
                }
            }
            ensure_active(cancellation)?;
        }
        emit_progress(
            progress,
            ProgressPhase::Parsing,
            "repository parsing complete",
            Some(total_files),
            Some(total_files),
        );

        let mut model = RepositoryModel::empty(repository_id, repository_name);
        let assembler = IndexAssembler::new(repository);
        let parsed_total = u64::try_from(parsed_files.len()).unwrap_or(u64::MAX);
        for (index, parsed_file) in parsed_files.into_iter().enumerate() {
            ensure_active(cancellation)?;
            emit_progress(
                progress,
                ProgressPhase::Indexing,
                format!("assembling {}", parsed_file.path),
                Some(u64::try_from(index).unwrap_or(u64::MAX)),
                Some(parsed_total),
            );
            let path = parsed_file.path.clone();
            match assembler.assemble([parsed_file]) {
                Ok(partial) => merge_model(&mut model, partial),
                Err(error) => {
                    parse_skipped_files = parse_skipped_files.saturating_add(1);
                    diagnostics.push(FileDiagnostic::new(
                        Some(path),
                        DiagnosticStage::Assemble,
                        DiagnosticSeverity::Error,
                        "assembly_error",
                        error.to_string(),
                    ));
                }
            }
            ensure_active(cancellation)?;
        }
        ensure_active(cancellation)?;
        sort_model(&mut model);
        diagnostics.sort_by(diagnostic_order);
        ensure_active(cancellation)?;

        let counts = IndexCounts {
            scanned_files,
            parsed_files: parsed_count,
            skipped_files: scan.skipped_files.saturating_add(parse_skipped_files),
        };
        let mut report = IndexReport {
            model,
            counts,
            diagnostics,
            cache_hit: false,
        };

        if let Some(store) = &self.store {
            ensure_active(cancellation)?;
            let cached_diagnostics = report
                .diagnostics
                .iter()
                .filter(|diagnostic| {
                    matches!(
                        diagnostic.stage,
                        DiagnosticStage::Parse | DiagnosticStage::Assemble
                    )
                })
                .cloned()
                .collect();
            let cached = CachedIndex {
                fingerprint,
                model: report.model.clone(),
                parsed_files: parsed_count,
                skipped_files: parse_skipped_files,
                diagnostics: cached_diagnostics,
            };
            if let Err(error) = store.write_cancellable(&cache_key, &cached, cancellation) {
                if matches!(error, IndexStoreError::Cancelled) {
                    return Err(IndexError::Cancelled);
                }
                report.diagnostics.push(FileDiagnostic::new(
                    None,
                    DiagnosticStage::Cache,
                    DiagnosticSeverity::Warning,
                    "cache_write_error",
                    error.to_string(),
                ));
                report.diagnostics.sort_by(diagnostic_order);
            }
        }

        ensure_active(cancellation)?;
        emit_progress(
            progress,
            ProgressPhase::Indexing,
            "repository index complete",
            Some(parsed_total),
            Some(parsed_total),
        );
        Ok(report)
    }

    fn index_fingerprint(&self, scan_fingerprint: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(INDEX_FINGERPRINT_DOMAIN);
        update_component(&mut hasher, scan_fingerprint);
        update_component(&mut hasher, &self.cache_revision);
        for (language, revision) in self.parsers.revisions() {
            update_component(&mut hasher, &language);
            update_component(&mut hasher, &revision);
        }
        hex_digest(&hasher.finalize())
    }
}

impl Default for Indexer {
    fn default() -> Self {
        Self::new(ScanConfig::default(), ParserRegistry::default())
    }
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error("repository indexing was cancelled")]
    Cancelled,
}

fn ensure_active(cancellation: Option<&CancellationCheck<'_>>) -> Result<(), IndexError> {
    if cancellation.is_some_and(|check| check()) {
        Err(IndexError::Cancelled)
    } else {
        Ok(())
    }
}

fn emit_progress(
    callback: Option<&ProgressCallback<'_>>,
    phase: ProgressPhase,
    message: impl Into<String>,
    completed: Option<u64>,
    total: Option<u64>,
) {
    if let Some(callback) = callback {
        callback(Progress {
            phase,
            message: message.into(),
            completed,
            total,
        });
    }
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

fn update_component(hasher: &mut Sha256, component: &str) {
    hasher.update(
        u64::try_from(component.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(component.as_bytes());
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
