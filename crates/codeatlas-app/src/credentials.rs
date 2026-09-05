use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use codeatlas_agent::SecretString;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_CREDENTIALS_BYTES: u64 = 64 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredentials {
    api_key: String,
}

impl Drop for StoredCredentials {
    fn drop(&mut self) {
        self.api_key.clear();
    }
}

#[derive(Debug, Clone)]
/// Private on-disk API credential storage shared by presentation binaries.
pub struct CredentialsStore {
    path: PathBuf,
}

impl CredentialsStore {
    /// Locates the `CodeAtlas` credentials file using XDG conventions.
    ///
    /// # Errors
    ///
    /// Returns an error when no absolute configuration directory is available.
    pub fn discover() -> Result<Self, CredentialsError> {
        Ok(Self {
            path: credentials_path(
                std::env::var_os("XDG_CONFIG_HOME"),
                std::env::var_os("HOME"),
            )?,
        })
    }

    #[cfg(test)]
    fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    /// Returns the credentials file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads and validates a stored API key without exposing it as a plain string.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, permissions, I/O, or malformed credentials.
    pub fn load_api_key(&self) -> Result<Option<SecretString>, CredentialsError> {
        let Some(credentials) = self.load()? else {
            return Ok(None);
        };
        let mut credentials = credentials;
        Ok(Some(SecretString::new(std::mem::take(
            &mut credentials.api_key,
        ))))
    }

    /// Atomically stores an API key in a private file.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty key, unsafe path, or failed filesystem operation.
    pub fn store_api_key(&self, api_key: &str) -> Result<(), CredentialsError> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(CredentialsError::EmptyApiKey);
        }
        let parent = self.path.parent().ok_or(CredentialsError::InvalidPath)?;
        prepare_private_directory(parent)?;
        reject_unsafe_target(&self.path)?;

        let mut bytes = serde_json::to_vec_pretty(&StoredCredentials {
            api_key: api_key.to_owned(),
        })
        .map_err(CredentialsError::Serialize)?;
        let temporary = write_temporary(parent, &bytes);
        bytes.fill(0);
        let temporary = temporary?;
        let result = (|| {
            reject_unsafe_target(&self.path)?;
            fs::rename(&temporary, &self.path).map_err(|source| CredentialsError::Io {
                operation: "commit credentials",
                source,
            })?;
            restrict_file_permissions(&self.path)?;
            sync_directory(parent)?;
            Ok(())
        })();
        let _ = fs::remove_file(temporary);
        result
    }

    /// Deletes the private credentials file if it exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the target is unsafe or cannot be removed safely.
    pub fn delete(&self) -> Result<bool, CredentialsError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(CredentialsError::Io {
                    operation: "inspect credentials",
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CredentialsError::UnsafePath);
        }
        fs::remove_file(&self.path).map_err(|source| CredentialsError::Io {
            operation: "delete credentials",
            source,
        })?;
        if let Some(parent) = self.path.parent() {
            sync_directory(parent)?;
        }
        Ok(true)
    }

    fn load(&self) -> Result<Option<StoredCredentials>, CredentialsError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CredentialsError::Io {
                    operation: "inspect credentials",
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CredentialsError::UnsafePath);
        }
        validate_file_permissions(&metadata)?;
        if metadata.len() > MAX_CREDENTIALS_BYTES {
            return Err(CredentialsError::FileTooLarge);
        }
        let mut bytes = fs::read(&self.path).map_err(|source| CredentialsError::Io {
            operation: "read credentials",
            source,
        })?;
        let credentials = serde_json::from_slice(&bytes);
        bytes.fill(0);
        let credentials: StoredCredentials = credentials.map_err(CredentialsError::Deserialize)?;
        if credentials.api_key.trim().is_empty() {
            return Err(CredentialsError::EmptyApiKey);
        }
        Ok(Some(credentials))
    }
}

/// Resolves the runtime API key from the environment or private storage.
///
/// # Errors
///
/// Returns an error when no environment key is set and the fallback credentials
/// location cannot be discovered or read safely.
pub fn resolve_api_key() -> Result<Option<SecretString>, CredentialsError> {
    resolve_api_key_with(
        std::env::var("CODEATLAS_API_KEY").ok(),
        CredentialsStore::discover,
    )
}

fn resolve_api_key_with<F>(
    environment_api_key: Option<String>,
    discover: F,
) -> Result<Option<SecretString>, CredentialsError>
where
    F: FnOnce() -> Result<CredentialsStore, CredentialsError>,
{
    match environment_api_key.filter(|value| !value.is_empty()) {
        Some(api_key) => Ok(Some(SecretString::new(api_key))),
        None => discover()?.load_api_key(),
    }
}

fn credentials_path(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, CredentialsError> {
    let root = match xdg_config_home.filter(|value| !value.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(
            home.filter(|value| !value.is_empty())
                .ok_or(CredentialsError::MissingConfigDirectory)?,
        )
        .join(".config"),
    };
    if !root.is_absolute() {
        return Err(CredentialsError::ConfigDirectoryNotAbsolute);
    }
    Ok(root.join("codeatlas").join("credentials.json"))
}

fn prepare_private_directory(path: &Path) -> Result<(), CredentialsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(CredentialsError::UnsafePath);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_dir_all(path)?;
        }
        Err(source) => {
            return Err(CredentialsError::Io {
                operation: "inspect credentials directory",
                source,
            });
        }
    }
    restrict_directory_permissions(path)
}

fn reject_unsafe_target(path: &Path) -> Result<(), CredentialsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(CredentialsError::UnsafePath)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CredentialsError::Io {
            operation: "inspect credentials target",
            source,
        }),
    }
}

fn write_temporary(directory: &Path, bytes: &[u8]) -> Result<PathBuf, CredentialsError> {
    for _ in 0..128 {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            ".credentials.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(CredentialsError::Io {
                    operation: "create temporary credentials",
                    source,
                });
            }
        };
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(CredentialsError::Io {
                operation: "write temporary credentials",
                source,
            });
        }
        return Ok(path);
    }
    Err(CredentialsError::TemporaryNameExhausted)
}

#[cfg(unix)]
fn create_private_dir_all(path: &Path) -> Result<(), CredentialsError> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).map_err(|source| CredentialsError::Io {
        operation: "create credentials directory",
        source,
    })
}

#[cfg(not(unix))]
fn create_private_dir_all(path: &Path) -> Result<(), CredentialsError> {
    fs::create_dir_all(path).map_err(|source| CredentialsError::Io {
        operation: "create credentials directory",
        source,
    })
}

#[cfg(unix)]
fn validate_file_permissions(metadata: &fs::Metadata) -> Result<(), CredentialsError> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = metadata.permissions().mode() & 0o777;
    if mode.trailing_zeros() >= 6 {
        Ok(())
    } else {
        Err(CredentialsError::InsecurePermissions { mode })
    }
}

#[cfg(not(unix))]
fn validate_file_permissions(_metadata: &fs::Metadata) -> Result<(), CredentialsError> {
    Ok(())
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> Result<(), CredentialsError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        CredentialsError::Io {
            operation: "restrict credentials directory permissions",
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) -> Result<(), CredentialsError> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<(), CredentialsError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        CredentialsError::Io {
            operation: "restrict credentials file permissions",
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> Result<(), CredentialsError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), CredentialsError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| CredentialsError::Io {
            operation: "sync credentials directory",
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), CredentialsError> {
    Ok(())
}

#[derive(Debug, Error)]
/// Failures while locating, validating, reading, or writing credentials.
pub enum CredentialsError {
    #[error("neither XDG_CONFIG_HOME nor HOME provides a credentials directory")]
    MissingConfigDirectory,
    #[error("the credentials directory must be an absolute path")]
    ConfigDirectoryNotAbsolute,
    #[error("the credentials path is invalid")]
    InvalidPath,
    #[error("the credentials path is not a regular, non-symlink file")]
    UnsafePath,
    #[error("the stored API key is empty")]
    EmptyApiKey,
    #[error("the credentials file exceeds the 64 KiB safety limit")]
    FileTooLarge,
    #[cfg(unix)]
    #[error("credentials file permissions are {mode:03o}; run chmod 600 on the file")]
    InsecurePermissions { mode: u32 },
    #[error("failed to serialize credentials")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to parse credentials JSON")]
    Deserialize(#[source] serde_json::Error),
    #[error("could not allocate a temporary credentials filename")]
    TemporaryNameExhausted,
    #[error("credentials operation failed during {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn path_prefers_xdg_and_falls_back_to_home() {
        assert_eq!(
            credentials_path(
                Some(OsString::from("/config")),
                Some(OsString::from("/home"))
            )
            .expect("absolute XDG path"),
            PathBuf::from("/config/codeatlas/credentials.json")
        );
        assert_eq!(
            credentials_path(None, Some(OsString::from("/home/user"))).expect("absolute home path"),
            PathBuf::from("/home/user/.config/codeatlas/credentials.json")
        );
        assert!(matches!(
            credentials_path(Some(OsString::from("relative")), None),
            Err(CredentialsError::ConfigDirectoryNotAbsolute)
        ));
    }

    #[test]
    fn credentials_round_trip_delete_and_redact_debug_output() {
        let temporary = tempdir().expect("temporary directory");
        let store = CredentialsStore::new(temporary.path().join("config/credentials.json"));
        store
            .store_api_key("local-secret-key")
            .expect("store credentials");
        let secret = store
            .load_api_key()
            .expect("load credentials")
            .expect("stored key");
        assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");
        assert!(!format!("{secret:?}").contains("local-secret-key"));
        assert!(store.delete().expect("delete credentials"));
        assert!(!store.delete().expect("already deleted"));
    }

    #[cfg(unix)]
    #[test]
    fn credentials_are_private_and_insecure_files_are_rejected() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("codeatlas/credentials.json");
        let store = CredentialsStore::new(&path);
        store.store_api_key("private-key").expect("store key");
        assert_eq!(
            fs::metadata(&path)
                .expect("credentials metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("make file insecure");
        assert!(matches!(
            store.load_api_key(),
            Err(CredentialsError::InsecurePermissions { mode: 0o644 })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_credentials_are_rejected() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let target = temporary.path().join("target.json");
        fs::write(&target, r#"{"api_key":"secret"}"#).expect("write target");
        let path = temporary.path().join("credentials.json");
        symlink(&target, &path).expect("create symlink");
        let store = CredentialsStore::new(path);

        assert!(matches!(
            store.load_api_key(),
            Err(CredentialsError::UnsafePath)
        ));
    }

    #[test]
    fn environment_key_does_not_require_credentials_discovery() {
        let secret = resolve_api_key_with(Some("environment-key".to_owned()), || {
            panic!("environment credentials must bypass discovery")
        })
        .expect("environment key has priority")
        .expect("environment key");
        assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");
    }
}
