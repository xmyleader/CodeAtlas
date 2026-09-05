use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use codeatlas_core::RepositoryModel;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::FileDiagnostic;

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
        let path = self.entry_path(cache_key);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(IndexStoreError::Io { path, source }),
        };
        let envelope: CacheEnvelope =
            serde_json::from_reader(BufReader::new(file)).map_err(|source| {
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
        Ok(Some(envelope.index))
    }

    /// Atomically writes one validated JSON cache record.
    ///
    /// # Errors
    ///
    /// Returns [`IndexStoreError`] when validation, directory creation,
    /// serialization, syncing, or atomic replacement fails.
    pub fn write(&self, cache_key: &str, index: &CachedIndex) -> Result<(), IndexStoreError> {
        index.model.validate_schema()?;
        fs::create_dir_all(&self.directory).map_err(|source| IndexStoreError::Io {
            path: self.directory.clone(),
            source,
        })?;

        let final_path = self.entry_path(cache_key);
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_path = self
            .directory
            .join(format!(".codeatlas-{}-{sequence}.tmp", std::process::id()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .map_err(|source| IndexStoreError::Io {
                path: temporary_path.clone(),
                source,
            })?;

        let write_result = write_envelope(file, index).and_then(|()| {
            fs::rename(&temporary_path, &final_path).map_err(|source| IndexStoreError::Io {
                path: final_path.clone(),
                source,
            })
        });
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        write_result
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

fn write_envelope(file: File, index: &CachedIndex) -> Result<(), IndexStoreError> {
    let path_description = "temporary cache file".to_owned();
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(
        &mut writer,
        &CacheEnvelopeRef {
            schema_version: STORE_SCHEMA_VERSION,
            index,
        },
    )
    .map_err(|source| IndexStoreError::JsonDescription {
        path: path_description.clone(),
        source,
    })?;
    writer
        .write_all(b"\n")
        .map_err(|source| IndexStoreError::IoDescription {
            path: path_description.clone(),
            source,
        })?;
    writer
        .flush()
        .map_err(|source| IndexStoreError::IoDescription {
            path: path_description.clone(),
            source,
        })?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|source| IndexStoreError::IoDescription {
            path: path_description,
            source,
        })
}

#[derive(Debug, Error)]
pub enum IndexStoreError {
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

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
