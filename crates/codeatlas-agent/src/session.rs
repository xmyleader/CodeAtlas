use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use codeatlas_core::{
    AgentAnswer, AnswerId, AppError, BudgetStopReason, Cost, ExplanationProfile, ModelBudgetStatus,
    ModelCallRecord, ModelUsage, RepositoryId, RequestId, SessionId, SessionTaskStatus, TokenUsage,
    WorkflowEvent,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ModelMessage, ModelRole};

pub const SESSION_SCHEMA_VERSION: u32 = 3;
const LEGACY_SESSION_SCHEMA_VERSIONS: [u32; 2] = [1, 2];
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
    #[serde(default)]
    pub profile: ExplanationProfile,
    pub status: SessionTaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<AgentAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_error: Option<AppError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_stop_reason: Option<BudgetStopReason>,
    #[serde(default)]
    pub started_at_unix_ms: u64,
    #[serde(default)]
    pub finished_at_unix_ms: u64,
    #[serde(default)]
    pub trajectory: Vec<WorkflowEvent>,
    #[serde(default)]
    pub model_calls: Vec<ModelCallRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ModelUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<ModelBudgetStatus>,
    /// Complete prior observable messages without a system instruction.
    #[serde(default)]
    pub continuation: Vec<ModelMessage>,
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

#[derive(Debug, Deserialize)]
struct LegacyConversationTurn {
    request_id: RequestId,
    question: String,
    #[serde(default)]
    profile: ExplanationProfile,
    answer: AgentAnswer,
    #[serde(default)]
    workflow: Vec<WorkflowEvent>,
}

#[derive(Debug, Deserialize)]
struct LegacySessionState {
    session_id: SessionId,
    repository_id: RepositoryId,
    turns: Vec<LegacyConversationTurn>,
    usage: ModelUsage,
    #[serde(default)]
    created_at_unix_ms: u64,
    #[serde(default)]
    updated_at_unix_ms: u64,
}

impl LegacySessionState {
    fn migrate(self) -> SessionState {
        SessionState {
            schema_version: SESSION_SCHEMA_VERSION,
            session_id: self.session_id,
            repository_id: self.repository_id,
            turns: self
                .turns
                .into_iter()
                .map(|turn| {
                    let usage = turn.answer.usage.clone();
                    ConversationTurn {
                        request_id: turn.request_id,
                        continuation: vec![
                            ModelMessage::user(turn.question.clone()),
                            ModelMessage::assistant(turn.answer.text.clone()),
                        ],
                        question: turn.question,
                        profile: turn.profile,
                        status: SessionTaskStatus::Completed,
                        answer: Some(turn.answer),
                        terminal_error: None,
                        budget_stop_reason: None,
                        started_at_unix_ms: self.created_at_unix_ms,
                        finished_at_unix_ms: self.updated_at_unix_ms,
                        trajectory: turn.workflow,
                        model_calls: Vec::new(),
                        usage,
                        budget: None,
                    }
                })
                .collect(),
            usage: self.usage,
            created_at_unix_ms: self.created_at_unix_ms,
            updated_at_unix_ms: self.updated_at_unix_ms,
        }
    }
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
    /// Returns [`SessionError`] for invalid evidence or a duplicate request ID.
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
    /// Returns [`SessionError`] for invalid evidence or a duplicate request ID.
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
        if self.turns.iter().any(|turn| {
            turn.answer
                .as_ref()
                .is_some_and(|existing| existing.id == answer.id)
        }) {
            return Err(SessionError::DuplicateAnswer {
                answer_id: answer.id,
            });
        }
        let question = question.into();
        let task_usage = answer.usage.clone();
        let now = current_unix_ms();
        self.turns.push(ConversationTurn {
            request_id,
            continuation: vec![
                ModelMessage::user(question.clone()),
                ModelMessage::assistant(answer.text.clone()),
            ],
            question,
            profile: ExplanationProfile::default(),
            status: SessionTaskStatus::Completed,
            answer: Some(answer),
            terminal_error: None,
            budget_stop_reason: None,
            started_at_unix_ms: now,
            finished_at_unix_ms: now,
            trajectory: workflow,
            model_calls: Vec::new(),
            usage: task_usage,
            budget: None,
        });
        self.usage = aggregate_turn_usage(&self.turns);
        self.updated_at_unix_ms = current_unix_ms();
        Ok(())
    }

    #[must_use]
    pub fn conversation_history(&self) -> Vec<ModelMessage> {
        let mut history = Vec::new();
        for turn in &self.turns {
            if turn.status == SessionTaskStatus::Completed {
                let mut continuation = turn.continuation.clone();
                if let Some(answer) = &turn.answer
                    && let Some(message) = continuation.iter_mut().rev().find(|message| {
                        message.role == ModelRole::Assistant && message.content.is_some()
                    })
                    && let Some(content) = message.content.take()
                {
                    message.content = Some(format!(
                        "[CodeAtlas persisted answer {}]\n{content}",
                        answer.id
                    ));
                }
                history.extend(continuation);
            }
        }
        history
    }

    /// Appends any terminal task, including failures and cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] for invalid data or duplicate request IDs.
    pub fn append_task(&mut self, turn: ConversationTurn) -> Result<(), SessionError> {
        validate_turn(&turn)?;
        if self
            .turns
            .iter()
            .any(|existing| existing.request_id == turn.request_id)
        {
            return Err(SessionError::DuplicateRequest {
                request_id: turn.request_id,
            });
        }
        if let Some(answer) = &turn.answer
            && self.turns.iter().any(|existing| {
                existing
                    .answer
                    .as_ref()
                    .is_some_and(|existing| existing.id == answer.id)
            })
        {
            return Err(SessionError::DuplicateAnswer {
                answer_id: answer.id,
            });
        }
        self.turns.push(turn);
        self.usage = aggregate_turn_usage(&self.turns);
        self.updated_at_unix_ms = current_unix_ms();
        Ok(())
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
            if turn.usage.is_none() {
                turn.usage = turn.answer.as_ref().and_then(|answer| answer.usage.clone());
            }
            if let Some(usage) = &mut turn.usage {
                has_usage = true;
                let cost = pricing.estimate(&usage.tokens)?;
                amount += cost.amount;
                usage.cost = Some(cost.clone());
                if let Some(answer_usage) = turn
                    .answer
                    .as_mut()
                    .and_then(|answer| answer.usage.as_mut())
                {
                    answer_usage.cost = Some(cost);
                }
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

fn aggregate_turn_usage(turns: &[ConversationTurn]) -> ModelUsage {
    let mut tokens = TokenUsage::default();
    let mut cost = None::<Cost>;
    let mut cost_unavailable = false;
    for usage in turns.iter().filter_map(turn_usage) {
        tokens = add_token_usage(tokens, usage.tokens);
        match (&mut cost, &usage.cost) {
            (_, None) if usage.tokens.total_tokens > 0 => {
                cost = None;
                cost_unavailable = true;
            }
            (Some(total), Some(additional)) if total.currency == additional.currency => {
                total.amount += additional.amount;
                total.estimated |= additional.estimated;
            }
            (None, Some(additional)) if !cost_unavailable => cost = Some(additional.clone()),
            (Some(_), Some(_)) => {
                cost = None;
                cost_unavailable = true;
            }
            _ => {}
        }
    }
    ModelUsage { tokens, cost }
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
        let paths = self.session_paths()?;
        paths.iter().map(|path| Self::load_path(path)).collect()
    }

    /// Restores every valid session while reporting corrupt individual files.
    /// Directory-level storage failures are still returned because enumeration
    /// itself cannot be trusted in that case.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the storage directory cannot be safely read.
    pub fn load_all_lossy(&self) -> Result<(Vec<SessionState>, Vec<String>), SessionError> {
        let paths = self.session_paths()?;
        let mut sessions = Vec::with_capacity(paths.len());
        let mut diagnostics = Vec::new();
        for path in paths {
            match Self::load_path(&path) {
                Ok(session) => sessions.push(session),
                Err(error) => diagnostics.push(error.to_string()),
            }
        }
        Ok((sessions, diagnostics))
    }

    fn session_paths(&self) -> Result<Vec<PathBuf>, SessionError> {
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
        Ok(paths)
    }

    fn load_path(path: &Path) -> Result<SessionState, SessionError> {
        reject_unsafe_session_file(path, false)?;
        let bytes = fs::read(path).map_err(|source| SessionError::Io {
            operation: "read",
            path: path.to_owned(),
            source,
        })?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|source| SessionError::Deserialize {
                path: path.to_owned(),
                source,
            })?;
        let schema_version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .unwrap_or_default();
        let mut state = match schema_version {
            actual if LEGACY_SESSION_SCHEMA_VERSIONS.contains(&actual) => {
                serde_json::from_value::<LegacySessionState>(value)
                    .map(LegacySessionState::migrate)
                    .map_err(|source| SessionError::Deserialize {
                        path: path.to_owned(),
                        source,
                    })?
            }
            SESSION_SCHEMA_VERSION => {
                serde_json::from_value(value).map_err(|source| SessionError::Deserialize {
                    path: path.to_owned(),
                    source,
                })?
            }
            actual => {
                return Err(SessionError::UnsupportedSchema {
                    expected: SESSION_SCHEMA_VERSION,
                    actual,
                });
            }
        };
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
    let mut answers = HashSet::with_capacity(state.turns.len());
    for turn in &state.turns {
        if !requests.insert(turn.request_id) {
            return Err(SessionError::DuplicateRequest {
                request_id: turn.request_id,
            });
        }
        validate_turn(turn)?;
        if let Some(answer) = &turn.answer
            && !answers.insert(answer.id)
        {
            return Err(SessionError::DuplicateAnswer {
                answer_id: answer.id,
            });
        }
    }
    let recalculated = aggregate_turn_usage(&state.turns);
    if recalculated != state.usage {
        return Err(SessionError::UsageMismatch);
    }
    Ok(())
}

fn turn_usage(turn: &ConversationTurn) -> Option<&ModelUsage> {
    turn.usage.as_ref().or_else(|| {
        turn.answer
            .as_ref()
            .and_then(|answer| answer.usage.as_ref())
    })
}

fn validate_turn(turn: &ConversationTurn) -> Result<(), SessionError> {
    match turn.status {
        SessionTaskStatus::Completed => {
            let answer = turn.answer.as_ref().ok_or(SessionError::MissingAnswer)?;
            answer
                .validate_evidence()
                .map_err(|error| SessionError::InvalidAnswer {
                    message: error.to_string(),
                })?;
        }
        SessionTaskStatus::Failed
        | SessionTaskStatus::Cancelled
        | SessionTaskStatus::BudgetExceeded => {
            if turn.answer.is_some() {
                return Err(SessionError::UnexpectedAnswer);
            }
        }
    }
    if turn.status == SessionTaskStatus::BudgetExceeded && turn.budget_stop_reason.is_none() {
        return Err(SessionError::MissingBudgetReason);
    }
    if turn.finished_at_unix_ms != 0
        && turn.started_at_unix_ms != 0
        && turn.finished_at_unix_ms < turn.started_at_unix_ms
    {
        return Err(SessionError::InvalidTaskTimestamps);
    }
    let trajectory_calls = turn
        .trajectory
        .iter()
        .filter_map(|event| match event {
            WorkflowEvent::ModelCall(record) => Some(record),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !trajectory_calls.is_empty()
        && (trajectory_calls.len() != turn.model_calls.len()
            || trajectory_calls
                .iter()
                .zip(&turn.model_calls)
                .any(|(left, right)| *left != right))
    {
        return Err(SessionError::ModelLedgerMismatch);
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
    #[error("answer {answer_id} already exists in the session")]
    DuplicateAnswer { answer_id: AnswerId },
    #[error("session contains an invalid answer: {message}")]
    InvalidAnswer { message: String },
    #[error("a completed session task is missing its answer")]
    MissingAnswer,
    #[error("a non-completed session task unexpectedly contains an answer")]
    UnexpectedAnswer,
    #[error("a budget-exceeded session task is missing its stop reason")]
    MissingBudgetReason,
    #[error("session task completion precedes its start timestamp")]
    InvalidTaskTimestamps,
    #[error("session task model-call ledger does not match its trajectory")]
    ModelLedgerMismatch,
    #[error("session usage does not equal the aggregate turn usage")]
    UsageMismatch,
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
