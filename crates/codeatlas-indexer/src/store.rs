use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use codeatlas_core::RepositoryModel;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{CancellationCheck, FileDiagnostic};

const STORE_SCHEMA_VERSION: u32 = 1;
const CACHE_KEY_DOMAIN: &[u8] = b"codeatlas-json-index-store-v1";
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedIndex {
    pub fingerprint: String,
    pub model: RepositoryModel,
    pub parsed_files: u64,
    pub skipped_files: u64,
    pub diagnostics: Vec<FileDiagnostic>,
}

#[derive(Debug, Clone)]
pub struct JsonIndexStore {
    directory: PathBuf,
}

impl JsonIndexStore {
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Reads and validates one cache record.
    ///
    /// # Errors
    ///
    /// Returns [`IndexStoreError`] for I/O, JSON, store schema, or core model
    /// schema failures. A missing entry returns `Ok(None)`.
    pub fn read(&self, cache_key: &str) -> Result<Option<CachedIndex>, IndexStoreError> {
        self.read_cancellable(cache_key, None)
    }

    /// Reads one cache record with cooperative cancellation during JSON input.
    ///
    /// # Errors
    ///
    /// Returns [`IndexStoreError`] for cancellation, unsafe paths, invalid
    /// cache data, or filesystem failures.
    pub fn read_cancellable(
        &self,
        cache_key: &str,
        cancellation: Option<&CancellationCheck<'_>>,
    ) -> Result<Option<CachedIndex>, IndexStoreError> {
        ensure_active(cancellation)?;
        self.validate_directory(true)?;
        let path = self.entry_path(cache_key);
        reject_unsafe_cache_file(&path, true)?;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(IndexStoreError::Io { path, source }),
        };
        let reader = CancellableReader::new(BufReader::new(file), cancellation);
        let envelope: CacheEnvelope = serde_json::from_reader(reader).map_err(|source| {
            if cancellation.is_some_and(|check| check()) {
                return IndexStoreError::Cancelled;
            }
            IndexStoreError::Json {
                path: path.clone(),
                source,
            }
        })?;
        if envelope.schema_version != STORE_SCHEMA_VERSION {
            return Err(IndexStoreError::UnsupportedSchema {
                expected: STORE_SCHEMA_VERSION,
                actual: envelope.schema_version,
            });
        }
        envelope.index.model.validate_schema()?;
        ensure_active(cancellation)?;
        Ok(Some(envelope.index))
    }

    /// Atomically writes one validated JSON cache record.
    ///
    /// # Errors
    ///
    /// Returns [`IndexStoreError`] when validation, directory creation,
    /// serialization, syncing, or atomic replacement fails.
    pub fn write(&self, cache_key: &str, index: &CachedIndex) -> Result<(), IndexStoreError> {
        self.write_cancellable(cache_key, index, None)
    }

    /// Atomically writes a cache record, leaving the prior record untouched if
    /// cancellation is observed before replacement.
    ///
    /// # Errors
    ///
    /// Returns [`IndexStoreError`] for cancellation, unsafe paths, invalid
    /// model data, serialization, or filesystem failures.
    pub fn write_cancellable(
        &self,
        cache_key: &str,
        index: &CachedIndex,
        cancellation: Option<&CancellationCheck<'_>>,
    ) -> Result<(), IndexStoreError> {
        ensure_active(cancellation)?;
        index.model.validate_schema()?;
        ensure_active(cancellation)?;
        self.prepare_directory()?;

        let final_path = self.entry_path(cache_key);
        reject_unsafe_cache_file(&final_path, true)?;
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_path = self
            .directory
            .join(format!(".codeatlas-{}-{sequence}.tmp", std::process::id()));
        let file =
            open_private_new_file(&temporary_path).map_err(|source| IndexStoreError::Io {
                path: temporary_path.clone(),
                source,
            })?;

        let write_result = write_envelope(file, index, cancellation).and_then(|()| {
            ensure_active(cancellation)?;
            fs::rename(&temporary_path, &final_path).map_err(|source| IndexStoreError::Io {
                path: final_path.clone(),
                source,
            })?;
            restrict_file_permissions(&final_path);
            sync_directory(&self.directory).map_err(|source| IndexStoreError::Io {
                path: self.directory.clone(),
                source,
            })
        });
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result
    }

    fn prepare_directory(&self) -> Result<(), IndexStoreError> {
        match fs::symlink_metadata(&self.directory) {
            Ok(_) => self.validate_directory(false)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.directory).map_err(|source| IndexStoreError::Io {
                    path: self.directory.clone(),
                    source,
                })?;
                self.validate_directory(false)?;
            }
            Err(source) => {
                return Err(IndexStoreError::Io {
                    path: self.directory.clone(),
                    source,
                });
            }
        }
        restrict_directory_permissions(&self.directory);
        Ok(())
    }

    fn validate_directory(&self, allow_missing: bool) -> Result<(), IndexStoreError> {
        match fs::symlink_metadata(&self.directory) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                Err(IndexStoreError::UnsafeStoragePath {
                    path: self.directory.clone(),
                })
            }
            Ok(_) => Ok(()),
            Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(IndexStoreError::Io {
                path: self.directory.clone(),
                source,
            }),
        }
    }

    fn entry_path(&self, cache_key: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(CACHE_KEY_DOMAIN);
        hasher.update(
            u64::try_from(cache_key.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(cache_key.as_bytes());
        self.directory
            .join(format!("{}.json", hex_digest(&hasher.finalize())))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEnvelope {
    schema_version: u32,
    index: CachedIndex,
}

#[derive(Serialize)]
struct CacheEnvelopeRef<'a> {
    schema_version: u32,
    index: &'a CachedIndex,
}

fn write_envelope(
    file: File,
    index: &CachedIndex,
    cancellation: Option<&CancellationCheck<'_>>,
) -> Result<(), IndexStoreError> {
    let path_description = "temporary cache file".to_owned();
    let mut writer = CancellableWriter::new(BufWriter::new(file), cancellation);
    serde_json::to_writer(
        &mut writer,
        &CacheEnvelopeRef {
            schema_version: STORE_SCHEMA_VERSION,
            index,
        },
    )
    .map_err(|source| {
        if cancellation.is_some_and(|check| check()) {
            return IndexStoreError::Cancelled;
        }
        IndexStoreError::JsonDescription {
            path: path_description.clone(),
            source,
        }
    })?;
    writer
        .write_all(b"\n")
        .map_err(|source| map_write_error(path_description.clone(), source, cancellation))?;
    writer
        .flush()
        .map_err(|source| map_write_error(path_description.clone(), source, cancellation))?;
    ensure_active(cancellation)?;
    writer
        .inner
        .get_ref()
        .sync_all()
        .map_err(|source| IndexStoreError::IoDescription {
            path: path_description,
            source,
        })
}

fn map_write_error(
    path: String,
    source: io::Error,
    cancellation: Option<&CancellationCheck<'_>>,
) -> IndexStoreError {
    if cancellation.is_some_and(|check| check()) {
        IndexStoreError::Cancelled
    } else {
        IndexStoreError::IoDescription { path, source }
    }
}

#[derive(Debug, Error)]
pub enum IndexStoreError {
    #[error("cache operation was cancelled")]
    Cancelled,
    #[error("unsafe cache storage path: {path}")]
    UnsafeStoragePath { path: PathBuf },
    #[error("cache I/O error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("cache I/O error at {path}: {source}")]
    IoDescription { path: String, source: io::Error },
    #[error("invalid cache JSON at {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid cache JSON at {path}: {source}")]
    JsonDescription {
        path: String,
        source: serde_json::Error,
    },
    #[error("unsupported index store schema {actual}; expected {expected}")]
    UnsupportedSchema { expected: u32, actual: u32 },
    #[error(transparent)]
    CoreSchema(#[from] codeatlas_core::SchemaError),
}

fn reject_unsafe_cache_file(path: &Path, allow_missing: bool) -> Result<(), IndexStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(IndexStoreError::UnsafeStoragePath {
                path: path.to_owned(),
            })
        }
        Ok(_) => Ok(()),
        Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(IndexStoreError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn open_private_new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn ensure_active(cancellation: Option<&CancellationCheck<'_>>) -> Result<(), IndexStoreError> {
    if cancellation.is_some_and(|check| check()) {
        Err(IndexStoreError::Cancelled)
    } else {
        Ok(())
    }
}

struct CancellableReader<'a, R> {
    inner: R,
    cancellation: Option<&'a CancellationCheck<'a>>,
}

impl<'a, R> CancellableReader<'a, R> {
    const fn new(inner: R, cancellation: Option<&'a CancellationCheck<'a>>) -> Self {
        Self {
            inner,
            cancellation,
        }
    }
}

impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.is_some_and(|check| check()) {
            return Err(io::Error::other("cancelled"));
        }
        self.inner.read(buffer)
    }
}

struct CancellableWriter<'a, W> {
    inner: W,
    cancellation: Option<&'a CancellationCheck<'a>>,
}

impl<'a, W> CancellableWriter<'a, W> {
    const fn new(inner: W, cancellation: Option<&'a CancellationCheck<'a>>) -> Self {
        Self {
            inner,
            cancellation,
        }
    }
}

impl<W: Write> Write for CancellableWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.cancellation.is_some_and(|check| check()) {
            return Err(io::Error::other("cancelled"));
        }
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.cancellation.is_some_and(|check| check()) {
            return Err(io::Error::other("cancelled"));
        }
        self.inner.flush()
    }
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) {}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) {}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
