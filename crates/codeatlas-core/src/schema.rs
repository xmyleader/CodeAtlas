use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }

    /// Ensures this is the schema understood by this crate version.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] for a different schema version.
    pub const fn ensure_current(self) -> Result<(), SchemaError> {
        if self.0 == SCHEMA_VERSION.0 {
            Ok(())
        } else {
            Err(SchemaError::Unsupported {
                expected: SCHEMA_VERSION,
                actual: self,
            })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SchemaError {
    #[error("unsupported schema version {actual:?}; expected {expected:?}")]
    Unsupported {
        expected: SchemaVersion,
        actual: SchemaVersion,
    },
}
