use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;

const ID_DOMAIN: &[u8] = b"codeatlas-stable-id-v1";
const ID_BYTES: usize = 16;
const ID_HEX_LEN: usize = ID_BYTES * 2;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct IdValue([u8; ID_BYTES]);

impl IdValue {
    fn derive(namespace: &str, parts: &[&str]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(ID_DOMAIN);
        update_component(&mut hasher, namespace);
        for part in parts {
            update_component(&mut hasher, part);
        }

        let digest = hasher.finalize();
        let mut bytes = [0_u8; ID_BYTES];
        bytes.copy_from_slice(&digest[..ID_BYTES]);
        Self(bytes)
    }

    fn parse(value: &str) -> Result<Self, IdParseError> {
        if value.len() != ID_HEX_LEN {
            return Err(IdParseError::InvalidLength {
                expected: ID_HEX_LEN,
                actual: value.len(),
            });
        }

        let mut bytes = [0_u8; ID_BYTES];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = decode_hex(pair[0]).ok_or(IdParseError::InvalidCharacter {
                index: index * 2,
                character: char::from(pair[0]),
            })?;
            let low = decode_hex(pair[1]).ok_or(IdParseError::InvalidCharacter {
                index: index * 2 + 1,
                character: char::from(pair[1]),
            })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

fn update_component(hasher: &mut Sha256, value: &str) {
    let length = u64::try_from(value.len()).expect("stable ID component exceeds u64::MAX bytes");
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
}

const fn decode_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for IdValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Error returned when a serialized stable ID is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdParseError {
    #[error("stable ID must contain {expected} hexadecimal characters, got {actual}")]
    InvalidLength { expected: usize, actual: usize },
    #[error("invalid hexadecimal character {character:?} at stable ID offset {index}")]
    InvalidCharacter { index: usize, character: char },
}

macro_rules! define_id {
    ($name:ident, $namespace:literal) => {
        #[doc = concat!("A deterministic, typed ", stringify!($name), ".")]
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(IdValue);

        impl $name {
            /// Derives an ID from ordered canonical identity components.
            ///
            /// Components are length-delimited and hashed with the public
            /// `codeatlas-stable-id-v1` SHA-256 scheme. Index assembly owns
            /// global ID generation; parser adapters should use local parsed IDs.
            #[must_use]
            pub fn from_stable_parts(parts: &[&str]) -> Self {
                Self(IdValue::derive($namespace, parts))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.to_string())
                    .finish()
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                IdValue::parse(value).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(de::Error::custom)
            }
        }
    };
}

define_id!(RepositoryId, "repository");
define_id!(FileId, "file");
define_id!(ModuleId, "module");
define_id!(SymbolId, "symbol");
define_id!(ImportEdgeId, "import-edge");
define_id!(ReferenceEdgeId, "reference-edge");
define_id!(CallEdgeId, "call-edge");
define_id!(EntryPointId, "entry-point");
define_id!(EvidenceId, "evidence");
define_id!(ClaimId, "claim");
define_id!(DiagramId, "diagram");
define_id!(CallPathId, "call-path");
define_id!(ToolCallId, "tool-call");
define_id!(RequestId, "request");
define_id!(SessionId, "session");
define_id!(AnswerId, "answer");
