use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use codeatlas_core::{
    AgentAnswer, Cost, ModelUsage, RepositoryId, RequestId, SessionId, TokenUsage, WorkflowEvent,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SESSION_SCHEMA_VERSION: u32 = 2;
const LEGACY_SESSION_SCHEMA_VERSION: u32 = 1;
const TOKENS_PER_MILLION: f64 = 1_000_000.0;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    User,
    Assistant,
}

/// Lightweight history passed back into future model turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: ConversationRole,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub request_id: RequestId,
    pub question: String,
    pub answer: AgentAnswer,
    #[serde(default)]
    pub workflow: Vec<WorkflowEvent>,
}

/// Serializable, UI-neutral state for one repository conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub repository_id: RepositoryId,
    pub turns: Vec<ConversationTurn>,
    pub usage: ModelUsage,
    #[serde(default)]
    pub created_at_unix_ms: u64,
    #[serde(default)]
    pub updated_at_unix_ms: u64,
}

impl SessionState {
    #[must_use]
    pub fn new(session_id: SessionId, repository_id: RepositoryId) -> Self {
        let now = current_unix_ms();
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id,
            repository_id,
            turns: Vec::new(),
            usage: ModelUsage {
                tokens: TokenUsage::default(),
                cost: None,
            },
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        }
    }

    /// Adds a completed exchange after re-validating its evidence contract.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for invalid evidence, a duplicate request ID,
    /// or incompatible cost currencies.
    pub fn append_turn(
        &mut self,
        request_id: RequestId,
        question: impl Into<String>,
        answer: AgentAnswer,
    ) -> Result<(), SessionError> {
        self.append_turn_with_workflow(request_id, question, answer, Vec::new())
    }

    /// Adds a completed exchange and its user-visible execution workflow.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for invalid evidence, a duplicate request ID,
    /// or incompatible cost currencies.
    pub fn append_turn_with_workflow(
        &mut self,
        request_id: RequestId,
        question: impl Into<String>,
        answer: AgentAnswer,
        workflow: Vec<WorkflowEvent>,
    ) -> Result<(), SessionError> {
        answer
            .validate_evidence()
            .map_err(|error| SessionError::InvalidAnswer {
                message: error.to_string(),
            })?;
        if self.turns.iter().any(|turn| turn.request_id == request_id) {
            return Err(SessionError::DuplicateRequest { request_id });
        }
        let usage = merge_usage(&self.usage, answer.usage.as_ref())?;
        self.turns.push(ConversationTurn {
            request_id,
            question: question.into(),
            answer,
            workflow,
        });
        self.usage = usage;
        self.updated_at_unix_ms = current_unix_ms();
        Ok(())
    }

    #[must_use]
    pub fn conversation_history(&self) -> Vec<ConversationMessage> {
        let mut history = Vec::with_capacity(self.turns.len().saturating_mul(2));
        for turn in &self.turns {
            history.push(ConversationMessage {
                role: ConversationRole::User,
                content: turn.question.clone(),
            });
            history.push(ConversationMessage {
                role: ConversationRole::Assistant,
                content: turn.answer.text.clone(),
            });
        }
        history
    }

    /// Recalculates the aggregate estimated cost with explicit prices.
    ///
    /// # Errors
    ///
    /// Returns [`PricingError`] for invalid pricing.
    pub fn reprice(&mut self, pricing: &ModelPricing) -> Result<(), PricingError> {
        pricing.validate()?;
        let mut amount = 0.0;
        let mut has_usage = false;
        for turn in &mut self.turns {
            if let Some(usage) = &mut turn.answer.usage {
                has_usage = true;
                let cost = pricing.estimate(&usage.tokens)?;
                amount += cost.amount;
                usage.cost = Some(cost);
            }
        }
        self.usage.cost = has_usage.then(|| Cost {
            currency: pricing.currency.clone(),
            amount,
            estimated: true,
        });
        Ok(())
    }
}

fn merge_usage(
    current: &ModelUsage,
    additional: Option<&ModelUsage>,
) -> Result<ModelUsage, SessionError> {
    let Some(additional) = additional else {
        return Ok(current.clone());
    };
    let tokens = add_token_usage(current.tokens, additional.tokens);
    let current_has_tokens = current.tokens.total_tokens > 0;
    let additional_has_tokens = additional.tokens.total_tokens > 0;
    let cost = match (&current.cost, &additional.cost) {
        (Some(left), Some(right)) => {
            if left.currency != right.currency {
                return Err(SessionError::CurrencyMismatch {
                    expected: left.currency.clone(),
                    actual: right.currency.clone(),
                });
            }
            Some(Cost {
                currency: left.currency.clone(),
                amount: left.amount + right.amount,
                estimated: left.estimated || right.estimated,
            })
        }
        (None, Some(cost)) if !current_has_tokens => Some(cost.clone()),
        (Some(cost), None) if !additional_has_tokens => Some(cost.clone()),
        _ => None,
    };
    Ok(ModelUsage { tokens, cost })
}

#[must_use]
pub const fn add_token_usage(left: TokenUsage, right: TokenUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: left.input_tokens.saturating_add(right.input_tokens),
        output_tokens: left.output_tokens.saturating_add(right.output_tokens),
        cached_input_tokens: left
            .cached_input_tokens
            .saturating_add(right.cached_input_tokens),
        total_tokens: left.total_tokens.saturating_add(right.total_tokens),
    }
}

/// Per-million-token prices used only when explicitly supplied by a caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub currency: String,
    pub input_per_million: f64,
    pub cached_input_per_million: Option<f64>,
    pub output_per_million: f64,
}

impl ModelPricing {
    /// Validates all price fields.
    ///
    /// # Errors
    ///
    /// Returns [`PricingError`] for an empty currency or a negative/non-finite
    /// price.
    pub fn validate(&self) -> Result<(), PricingError> {
        if self.currency.trim().is_empty() {
            return Err(PricingError::EmptyCurrency);
        }
        validate_rate("input_per_million", self.input_per_million)?;
        validate_rate("output_per_million", self.output_per_million)?;
        if let Some(rate) = self.cached_input_per_million {
            validate_rate("cached_input_per_million", rate)?;
        }
        Ok(())
    }

    /// Estimates a cost from token usage.
    ///
    /// Cached tokens are a subset of input tokens and are not charged twice.
    /// If no cached-input price is supplied, the regular input price is used.
    ///
    /// # Errors
    ///
    /// Returns [`PricingError`] when this configuration is invalid.
    #[allow(clippy::cast_precision_loss)]
    pub fn estimate(&self, usage: &TokenUsage) -> Result<Cost, PricingError> {
        self.validate()?;
        let cached = usage.cached_input_tokens.min(usage.input_tokens);
        let uncached = usage.input_tokens.saturating_sub(cached);
        let cached_rate = self
            .cached_input_per_million
            .unwrap_or(self.input_per_million);
        let amount = ((uncached as f64) * self.input_per_million
            + (cached as f64) * cached_rate
            + (usage.output_tokens as f64) * self.output_per_million)
            / TOKENS_PER_MILLION;
        Ok(Cost {
            currency: self.currency.clone(),
            amount,
            estimated: true,
        })
    }
}

fn validate_rate(field: &'static str, value: f64) -> Result<(), PricingError> {
    if !value.is_finite() || value < 0.0 {
        Err(PricingError::InvalidRate { field, value })
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum PricingError {
    #[error("pricing currency must not be empty")]
    EmptyCurrency,
    #[error("pricing field {field} must be finite and non-negative, got {value}")]
    InvalidRate { field: &'static str, value: f64 },
}

/// Filesystem-backed session persistence rooted at a caller-selected directory.
#[derive(Debug, Clone)]
pub struct SessionStore {
    directory: PathBuf,
}

impl SessionStore {
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

    #[must_use]
    pub fn session_path(&self, session_id: SessionId) -> PathBuf {
        self.directory.join(format!("{session_id}.json"))
    }

    /// Persists a session using write-sync-rename in the target directory.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for schema, serialization, or filesystem errors.
    pub fn save(&self, state: &SessionState) -> Result<(), SessionError> {
        validate_state(state)?;
        self.prepare_directory()?;
        let bytes = serde_json::to_vec_pretty(state).map_err(SessionError::Serialize)?;
        let target = self.session_path(state.session_id);
        reject_unsafe_session_file(&target, true)?;
        let temporary = loop {
            let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let candidate = self.directory.join(format!(
                ".{}.{}.{}.tmp",
                state.session_id,
                std::process::id(),
                counter
            ));
            match write_new_file(&candidate, &bytes) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    let _ = fs::remove_file(&candidate);
                    return Err(SessionError::Io {
                        operation: "write",
                        path: candidate,
                        source,
                    });
                }
            }
        };
        if let Err(source) = fs::rename(&temporary, &target) {
            let _ = fs::remove_file(&temporary);
            return Err(SessionError::Io {
                operation: "rename",
                path: target,
                source,
            });
        }
        sync_directory(&self.directory).map_err(|source| self.io_error("sync", source))?;
        Ok(())
    }

    /// Restores one session and validates its schema and requested ID.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for missing/corrupt files or incompatible data.
    pub fn load(&self, session_id: SessionId) -> Result<SessionState, SessionError> {
        self.validate_directory()?;
        let path = self.session_path(session_id);
        let state = Self::load_path(&path)?;
        if state.session_id != session_id {
            return Err(SessionError::SessionIdMismatch {
                expected: session_id,
                actual: state.session_id,
            });
        }
        Ok(state)
    }

    /// Restores every `.json` session in deterministic filename order.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] on the first unreadable or invalid document.
    pub fn load_all(&self) -> Result<Vec<SessionState>, SessionError> {
        match fs::symlink_metadata(&self.directory) {
            Ok(_) => self.validate_directory()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(self.io_error("inspect", source)),
        }
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(self.io_error("read", source)),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| self.io_error("read", source))?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                paths.push(path);
            }
        }
        paths.sort();
        paths.iter().map(|path| Self::load_path(path)).collect()
    }

    fn load_path(path: &Path) -> Result<SessionState, SessionError> {
        reject_unsafe_session_file(path, false)?;
        let bytes = fs::read(path).map_err(|source| SessionError::Io {
            operation: "read",
            path: path.to_owned(),
            source,
        })?;
        let mut state: SessionState =
            serde_json::from_slice(&bytes).map_err(|source| SessionError::Deserialize {
                path: path.to_owned(),
                source,
            })?;
        match state.schema_version {
            LEGACY_SESSION_SCHEMA_VERSION => state.schema_version = SESSION_SCHEMA_VERSION,
            SESSION_SCHEMA_VERSION => {}
            actual => {
                return Err(SessionError::UnsupportedSchema {
                    expected: SESSION_SCHEMA_VERSION,
                    actual,
                });
            }
        }
        normalize_timestamps(&mut state);
        validate_state(&state)?;
        Ok(state)
    }

    fn io_error(&self, operation: &'static str, source: io::Error) -> SessionError {
        SessionError::Io {
            operation,
            path: self.directory.clone(),
            source,
        }
    }

    fn prepare_directory(&self) -> Result<(), SessionError> {
        match fs::symlink_metadata(&self.directory) {
            Ok(_) => self.validate_directory()?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.directory)
                    .map_err(|source| self.io_error("create", source))?;
                self.validate_directory()?;
            }
            Err(source) => return Err(self.io_error("inspect", source)),
        }
        restrict_directory_permissions(&self.directory);
        Ok(())
    }

    fn validate_directory(&self) -> Result<(), SessionError> {
        let metadata = fs::symlink_metadata(&self.directory)
            .map_err(|source| self.io_error("inspect", source))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SessionError::UnsafeStoragePath {
                path: self.directory.clone(),
            });
        }
        Ok(())
    }
}

fn current_unix_ms() -> u64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    u64::try_from(milliseconds).unwrap_or(u64::MAX)
}

fn normalize_timestamps(state: &mut SessionState) {
    match (state.created_at_unix_ms, state.updated_at_unix_ms) {
        (0, 0) => {}
        (0, updated) => state.created_at_unix_ms = updated,
        (created, 0) => state.updated_at_unix_ms = created,
        (created, updated) if updated < created => state.updated_at_unix_ms = created,
        _ => {}
    }
}

fn reject_unsafe_session_file(path: &Path, allow_missing: bool) -> Result<(), SessionError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(SessionError::UnsafeStoragePath {
                path: path.to_owned(),
            })
        }
        Ok(_) => Ok(()),
        Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SessionError::Io {
            operation: "inspect",
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) {}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn validate_state(state: &SessionState) -> Result<(), SessionError> {
    if state.schema_version != SESSION_SCHEMA_VERSION {
        return Err(SessionError::UnsupportedSchema {
            expected: SESSION_SCHEMA_VERSION,
            actual: state.schema_version,
        });
    }
    let mut requests = HashSet::with_capacity(state.turns.len());
    let mut recalculated = ModelUsage {
        tokens: TokenUsage::default(),
        cost: None,
    };
    for turn in &state.turns {
        if !requests.insert(turn.request_id) {
            return Err(SessionError::DuplicateRequest {
                request_id: turn.request_id,
            });
        }
        turn.answer
            .validate_evidence()
            .map_err(|error| SessionError::InvalidAnswer {
                message: error.to_string(),
            })?;
        recalculated = merge_usage(&recalculated, turn.answer.usage.as_ref())?;
    }
    if recalculated != state.usage {
        return Err(SessionError::UsageMismatch);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("unsupported session schema {actual}; expected {expected}")]
    UnsupportedSchema { expected: u32, actual: u32 },
    #[error("session document ID {actual} does not match requested ID {expected}")]
    SessionIdMismatch {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("request {request_id} already exists in the session")]
    DuplicateRequest { request_id: RequestId },
    #[error("session contains an invalid answer: {message}")]
    InvalidAnswer { message: String },
    #[error("session usage does not equal the aggregate turn usage")]
    UsageMismatch,
    #[error("cannot aggregate costs in {actual}; expected {expected}")]
    CurrencyMismatch { expected: String, actual: String },
    #[error("unsafe session storage path: {path}")]
    UnsafeStoragePath { path: PathBuf },
    #[error("failed to serialize session: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to deserialize session at {path}: {source}")]
    Deserialize {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to {operation} session data at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
