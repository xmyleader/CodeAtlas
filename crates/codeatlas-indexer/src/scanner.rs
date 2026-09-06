use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use codeatlas_core::{Language, RepositoryPath};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{DiagnosticSeverity, DiagnosticStage, FileDiagnostic};

const FINGERPRINT_DOMAIN: &[u8] = b"codeatlas-scan-fingerprint-v1";
const READ_CHUNK_BYTES: usize = 64 * 1024;

pub type CancellationCheck<'a> = dyn Fn() -> bool + Send + Sync + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanConfig {
    pub max_file_size: u64,
    pub max_total_bytes: u64,
    pub max_files: usize,
    pub max_entries: usize,
    pub max_depth: usize,
    pub binary_probe_bytes: usize,
    pub excluded_directories: BTreeSet<String>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            max_file_size: 2 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_files: 100_000,
            max_entries: 250_000,
            max_depth: 64,
            binary_probe_bytes: 8 * 1024,
            excluded_directories: [".git", "target", "node_modules", ".venv"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: RepositoryPath,
    pub language: Language,
    pub source: String,
    modified: Option<(u64, u32)>,
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    pub skipped_files: u64,
    pub diagnostics: Vec<FileDiagnostic>,
    pub fingerprint: String,
}

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("failed to resolve repository root {path}: {source}")]
    ResolveRoot { path: PathBuf, source: io::Error },
    #[error("repository root is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("repository scan was cancelled")]
    Cancelled,
}

#[derive(Debug)]
pub struct RepositoryScanner {
    config: ScanConfig,
}

impl RepositoryScanner {
    #[must_use]
    pub const fn new(config: ScanConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub const fn config(&self) -> &ScanConfig {
        &self.config
    }

    /// Scans one local repository without following symbolic links.
    ///
    /// # Errors
    ///
    /// Returns [`ScanError`] when the root cannot be resolved or is not a directory.
    #[allow(clippy::too_many_lines)]
    pub fn scan(&self, root: impl AsRef<Path>) -> Result<ScanResult, ScanError> {
        self.scan_cancellable(root, None)
    }

    /// Scans one local repository with cooperative cancellation between
    /// traversal, file-read, and fingerprinting work units.
    ///
    /// # Errors
    ///
    /// Returns [`ScanError`] when the root is invalid or cancellation is requested.
    #[allow(clippy::too_many_lines)]
    pub fn scan_cancellable(
        &self,
        root: impl AsRef<Path>,
        cancellation: Option<&CancellationCheck<'_>>,
    ) -> Result<ScanResult, ScanError> {
        ensure_active(cancellation)?;
        let requested_root = root.as_ref();
        let root = fs::canonicalize(requested_root).map_err(|source| ScanError::ResolveRoot {
            path: requested_root.to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(ScanError::NotDirectory(root));
        }

        let excluded_directories = self.config.excluded_directories.clone();
        let mut builder = WalkBuilder::new(&root);
        builder
            .follow_links(false)
            .hidden(false)
            .parents(false)
            .git_global(false)
            .require_git(false)
            .max_depth(Some(self.config.max_depth))
            .sort_by_file_path(Path::cmp)
            .filter_entry(move |entry| {
                if entry.depth() == 0 || !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    return true;
                }
                entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| !excluded_directories.contains(name))
            });

        let mut files = Vec::new();
        let mut diagnostics = Vec::new();
        let mut skipped_files = 0_u64;
        let mut total_bytes = 0_u64;
        let mut file_limit_reported = false;

        for (visited_entries, entry) in builder.build().enumerate() {
            ensure_active(cancellation)?;
            if visited_entries >= self.config.max_entries {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(FileDiagnostic::new(
                    None,
                    DiagnosticStage::Scan,
                    DiagnosticSeverity::Warning,
                    "entry_limit_reached",
                    format!(
                        "repository traversal entry limit is {}",
                        self.config.max_entries
                    ),
                ));
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    skipped_files = skipped_files.saturating_add(1);
                    diagnostics.push(walk_diagnostic(&root, &error));
                    continue;
                }
            };
            if entry.depth() == 0 {
                continue;
            }

            let repository_path = match to_repository_path(&root, entry.path()) {
                Ok(path) => path,
                Err(message) => {
                    skipped_files = skipped_files.saturating_add(1);
                    diagnostics.push(FileDiagnostic::new(
                        None,
                        DiagnosticStage::Scan,
                        DiagnosticSeverity::Warning,
                        "invalid_repository_path",
                        message,
                    ));
                    continue;
                }
            };

            let Some(file_type) = entry.file_type() else {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(scan_warning(
                    repository_path,
                    "unknown_file_type",
                    "file type could not be determined",
                ));
                continue;
            };
            if file_type.is_symlink() {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(scan_warning(
                    repository_path,
                    "symbolic_link",
                    "symbolic links are not followed",
                ));
                continue;
            }
            if file_type.is_dir() {
                continue;
            }
            if !file_type.is_file() {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(scan_warning(
                    repository_path,
                    "special_file",
                    "non-regular files are not read",
                ));
                continue;
            }

            let Some(language) = language_for_path(&repository_path) else {
                skipped_files = skipped_files.saturating_add(1);
                continue;
            };
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) => {
                    skipped_files = skipped_files.saturating_add(1);
                    diagnostics.push(scan_warning(
                        repository_path,
                        "metadata_error",
                        error.to_string(),
                    ));
                    continue;
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(scan_warning(
                    repository_path,
                    "file_changed_during_scan",
                    "entry stopped being a regular file during scanning",
                ));
                continue;
            }
            if metadata.len() > self.config.max_file_size {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(scan_warning(
                    repository_path,
                    "file_too_large",
                    format!(
                        "file is {} bytes; limit is {} bytes",
                        metadata.len(),
                        self.config.max_file_size
                    ),
                ));
                continue;
            }
            if files.len() >= self.config.max_files {
                skipped_files = skipped_files.saturating_add(1);
                if !file_limit_reported {
                    diagnostics.push(scan_warning(
                        repository_path,
                        "file_limit_reached",
                        format!("repository file limit is {}", self.config.max_files),
                    ));
                    file_limit_reported = true;
                }
                continue;
            }

            let modified = modified_parts(metadata.modified().ok());
            let bytes = match read_bounded(entry.path(), self.config.max_file_size, cancellation) {
                Ok(bytes) => bytes,
                Err(ReadBoundedError::TooLarge) => {
                    skipped_files = skipped_files.saturating_add(1);
                    diagnostics.push(scan_warning(
                        repository_path,
                        "file_too_large",
                        "file grew beyond the configured limit while being read",
                    ));
                    continue;
                }
                Err(ReadBoundedError::Io(error)) => {
                    skipped_files = skipped_files.saturating_add(1);
                    diagnostics.push(scan_warning(
                        repository_path,
                        "read_error",
                        error.to_string(),
                    ));
                    continue;
                }
                Err(ReadBoundedError::Cancelled) => return Err(ScanError::Cancelled),
            };
            let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if total_bytes
                .checked_add(byte_count)
                .is_none_or(|total| total > self.config.max_total_bytes)
            {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(scan_warning(
                    repository_path,
                    "total_size_limit_reached",
                    format!(
                        "repository source byte limit is {}",
                        self.config.max_total_bytes
                    ),
                ));
                continue;
            }
            let probe_len = bytes.len().min(self.config.binary_probe_bytes);
            if bytes[..probe_len].contains(&0) {
                skipped_files = skipped_files.saturating_add(1);
                diagnostics.push(scan_warning(
                    repository_path,
                    "binary_file",
                    "NUL byte detected in source probe",
                ));
                continue;
            }
            let source = match String::from_utf8(bytes) {
                Ok(source) => source,
                Err(error) => {
                    skipped_files = skipped_files.saturating_add(1);
                    diagnostics.push(scan_warning(
                        repository_path,
                        "binary_file",
                        format!("source is not valid UTF-8: {error}"),
                    ));
                    continue;
                }
            };

            total_bytes += byte_count;
            files.push(ScannedFile {
                path: repository_path,
                language,
                source,
                modified,
            });
        }

        files.sort_by(|left, right| left.path.cmp(&right.path));
        diagnostics.sort_by(diagnostic_order);
        ensure_active(cancellation)?;
        let fingerprint = fingerprint(&self.config, &files, cancellation)?;
        Ok(ScanResult {
            files,
            skipped_files,
            diagnostics,
            fingerprint,
        })
    }
}

impl Default for RepositoryScanner {
    fn default() -> Self {
        Self::new(ScanConfig::default())
    }
}

#[derive(Debug)]
enum ReadBoundedError {
    TooLarge,
    Io(io::Error),
    Cancelled,
}

fn read_bounded(
    path: &Path,
    max_bytes: u64,
    cancellation: Option<&CancellationCheck<'_>>,
) -> Result<Vec<u8>, ReadBoundedError> {
    let mut file = File::open(path).map_err(ReadBoundedError::Io)?;
    let mut bytes = Vec::new();
    let mut chunk = vec![0_u8; READ_CHUNK_BYTES];
    loop {
        if cancellation.is_some_and(|check| check()) {
            return Err(ReadBoundedError::Cancelled);
        }
        let remaining = max_bytes
            .saturating_add(1)
            .saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if remaining == 0 {
            break;
        }
        let limit = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(chunk.len());
        let read = file
            .read(&mut chunk[..limit])
            .map_err(ReadBoundedError::Io)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(ReadBoundedError::TooLarge);
    }
    Ok(bytes)
}

fn to_repository_path(root: &Path, path: &Path) -> Result<RepositoryPath, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|error| format!("path escaped repository root: {error}"))?;
    let mut normalized = String::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err("path contains a non-normal component".to_owned());
        };
        let component = component
            .to_str()
            .ok_or_else(|| "path is not valid UTF-8".to_owned())?;
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    RepositoryPath::new(normalized).map_err(|error| error.to_string())
}

#[must_use]
pub fn language_for_path(path: &RepositoryPath) -> Option<Language> {
    let extension = Path::new(path.as_str()).extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("rs") {
        Some(Language::Rust)
    } else if extension.eq_ignore_ascii_case("py") || extension.eq_ignore_ascii_case("pyi") {
        Some(Language::Python)
    } else {
        None
    }
}

fn modified_parts(modified: Option<SystemTime>) -> Option<(u64, u32)> {
    let duration = modified?.duration_since(UNIX_EPOCH).ok()?;
    Some((duration.as_secs(), duration.subsec_nanos()))
}

fn fingerprint(
    config: &ScanConfig,
    files: &[ScannedFile],
    cancellation: Option<&CancellationCheck<'_>>,
) -> Result<String, ScanError> {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hash_u64(&mut hasher, config.max_file_size);
    hash_u64(&mut hasher, config.max_total_bytes);
    hash_u64(
        &mut hasher,
        u64::try_from(config.max_files).unwrap_or(u64::MAX),
    );
    hash_u64(
        &mut hasher,
        u64::try_from(config.max_entries).unwrap_or(u64::MAX),
    );
    hash_u64(
        &mut hasher,
        u64::try_from(config.max_depth).unwrap_or(u64::MAX),
    );
    hash_u64(
        &mut hasher,
        u64::try_from(config.binary_probe_bytes).unwrap_or(u64::MAX),
    );
    for directory in &config.excluded_directories {
        ensure_active(cancellation)?;
        hash_bytes(&mut hasher, directory.as_bytes());
    }
    for file in files {
        ensure_active(cancellation)?;
        hash_bytes(&mut hasher, file.path.as_str().as_bytes());
        hash_bytes(&mut hasher, language_key(&file.language).as_bytes());
        match file.modified {
            Some((seconds, nanos)) => {
                hasher.update([1]);
                hash_u64(&mut hasher, seconds);
                hash_u64(&mut hasher, u64::from(nanos));
            }
            None => hasher.update([0]),
        }
        hash_bytes(&mut hasher, file.source.as_bytes());
    }
    Ok(hex_digest(hasher.finalize().as_slice()))
}

fn ensure_active(cancellation: Option<&CancellationCheck<'_>>) -> Result<(), ScanError> {
    if cancellation.is_some_and(|check| check()) {
        Err(ScanError::Cancelled)
    } else {
        Ok(())
    }
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
        Language::Other(value) => format!("other:{value}"),
    }
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hash_u64(hasher, u64::try_from(value.len()).unwrap_or(u64::MAX));
    hasher.update(value);
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn scan_warning(
    path: RepositoryPath,
    code: impl Into<String>,
    message: impl Into<String>,
) -> FileDiagnostic {
    FileDiagnostic::new(
        Some(path),
        DiagnosticStage::Scan,
        DiagnosticSeverity::Warning,
        code,
        message,
    )
}

fn walk_diagnostic(root: &Path, error: &ignore::Error) -> FileDiagnostic {
    let path = walk_error_path(error).and_then(|path| to_repository_path(root, path).ok());
    let message = error.io_error().map_or_else(
        || "repository traversal or ignore-rule error".to_owned(),
        |error| format!("repository traversal I/O error: {:?}", error.kind()),
    );
    FileDiagnostic::new(
        path,
        DiagnosticStage::Scan,
        DiagnosticSeverity::Warning,
        "walk_error",
        message,
    )
}

fn walk_error_path(error: &ignore::Error) -> Option<&Path> {
    match error {
        ignore::Error::Partial(errors) => errors.iter().find_map(walk_error_path),
        ignore::Error::WithLineNumber { err, .. } | ignore::Error::WithDepth { err, .. } => {
            walk_error_path(err)
        }
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::Loop { child, .. } => Some(child),
        ignore::Error::Io(_)
        | ignore::Error::Glob { .. }
        | ignore::Error::UnrecognizedFileType(_)
        | ignore::Error::InvalidDefinition => None,
    }
}

pub(crate) fn diagnostic_order(
    left: &FileDiagnostic,
    right: &FileDiagnostic,
) -> std::cmp::Ordering {
    (
        &left.path,
        left.stage,
        left.severity,
        &left.code,
        &left.message,
    )
        .cmp(&(
            &right.path,
            right.stage,
            right.severity,
            &right.code,
            &right.message,
        ))
}
