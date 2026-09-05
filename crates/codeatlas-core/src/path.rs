use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// A normalized path relative to a repository root.
///
/// Persisted repository paths always use `/`, contain no `.` or `..`
/// components, and never have a leading or trailing separator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    /// Validates and constructs a normalized repository-relative path.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryPathError`] when `value` is empty, absolute, or is
    /// not already in the canonical repository-relative form.
    pub fn new(value: impl Into<String>) -> Result<Self, RepositoryPathError> {
        let value = value.into();
        validate(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate(value: &str) -> Result<(), RepositoryPathError> {
    if value.is_empty() {
        return Err(RepositoryPathError::Empty);
    }
    if value.starts_with('/') || has_windows_prefix(value) {
        return Err(RepositoryPathError::Absolute(value.to_owned()));
    }
    if value.contains('\\') {
        return Err(RepositoryPathError::Backslash(value.to_owned()));
    }
    if value.contains('\0') {
        return Err(RepositoryPathError::NulByte);
    }

    for component in value.split('/') {
        match component {
            "" => return Err(RepositoryPathError::EmptyComponent(value.to_owned())),
            "." => return Err(RepositoryPathError::CurrentDirectory(value.to_owned())),
            ".." => return Err(RepositoryPathError::ParentDirectory(value.to_owned())),
            _ => {}
        }
    }
    Ok(())
}

fn has_windows_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

impl fmt::Display for RepositoryPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for RepositoryPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for RepositoryPath {
    type Err = RepositoryPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for RepositoryPath {
    type Error = RepositoryPathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for RepositoryPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RepositoryPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepositoryPathError {
    #[error("repository path must not be empty")]
    Empty,
    #[error("repository path must be relative: {0}")]
    Absolute(String),
    #[error("repository path must use forward slashes: {0}")]
    Backslash(String),
    #[error("repository path must not contain a NUL byte")]
    NulByte,
    #[error("repository path contains an empty component: {0}")]
    EmptyComponent(String),
    #[error("repository path contains a current-directory component: {0}")]
    CurrentDirectory(String),
    #[error("repository path contains a parent-directory component: {0}")]
    ParentDirectory(String),
}
