use std::{fmt, time::Duration};

use async_trait::async_trait;
use codeatlas_core::{ModelConfig, TokenUsage, ToolDefinition};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::model::{
    AssistantOutput, AssistantToolCall, ModelClient, ModelError, ModelMessage, ModelRequest,
    ModelResponse, ModelRole, StructuredAnswer,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DIAGNOSTIC_BYTES: usize = 8 * 1024;

/// An owned runtime secret that never exposes its value through `Debug`.
///
/// This type intentionally does not implement `Serialize`, `Display`, or
/// `AsRef<str>`.
pub struct SecretString(String);

impl SecretString {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn into_inner(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// Secret HTTP headers injected only into live model requests.
///
/// Values are private, non-serializable, marked sensitive for `reqwest`, and
/// replaced in endpoint diagnostics if a server echoes one back.
#[derive(Default)]
pub struct RuntimeSecretHeaders {
    headers: HeaderMap,
    redactions: Vec<String>,
}

impl RuntimeSecretHeaders {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds an `Authorization: Bearer ...` secret header set.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiClientBuildError`] when the key cannot be represented as
    /// an HTTP header value.
    pub fn bearer(api_key: SecretString) -> Result<Self, OpenAiClientBuildError> {
        let raw_key = api_key.into_inner();
        let value = format!("Bearer {raw_key}");
        let mut headers = Self::new();
        headers.insert_raw("authorization", value, Some(raw_key))?;
        Ok(headers)
    }

    /// Adds a provider-specific secret header such as `api-key`.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiClientBuildError`] for an invalid header name or value.
    pub fn insert(
        &mut self,
        name: &str,
        value: SecretString,
    ) -> Result<(), OpenAiClientBuildError> {
        let raw = value.into_inner();
        self.insert_raw(name, raw.clone(), Some(raw))
    }

    fn insert_raw(
        &mut self,
        name: &str,
        value: String,
        additional_redaction: Option<String>,
    ) -> Result<(), OpenAiClientBuildError> {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            OpenAiClientBuildError::InvalidHeaderName {
                message: error.to_string(),
            }
        })?;
        let mut header = HeaderValue::from_str(&value).map_err(|_| {
            OpenAiClientBuildError::InvalidHeaderValue {
                name: name.to_string(),
            }
        })?;
        header.set_sensitive(true);
        self.headers.insert(name, header);
        self.redactions.push(value);
        if let Some(value) = additional_redaction {
            self.redactions.push(value);
        }
        Ok(())
    }

    fn redact(&self, message: &str) -> String {
        let mut redacted = message.to_owned();
        let mut values: Vec<&str> = self
            .redactions
            .iter()
            .map(String::as_str)
            .filter(|value| !value.is_empty())
            .collect();
        values.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        for value in values {
            redacted = redacted.replace(value, "[REDACTED]");
        }
        redacted
    }
}

impl fmt::Debug for RuntimeSecretHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.keys().map(HeaderName::as_str).collect();
        formatter
            .debug_struct("RuntimeSecretHeaders")
            .field("header_names", &names)
            .field("values", &"[REDACTED]")
            .field("redactions", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum OpenAiClientBuildError {
    #[error("invalid model endpoint {endpoint:?}: {message}")]
    InvalidEndpoint { endpoint: String, message: String },
    #[error("model name must not be empty")]
    EmptyModel,
    #[error("model temperature must be finite")]
    NonFiniteTemperature,
    #[error("request timeout must be greater than zero")]
    ZeroTimeout,
    #[error("invalid secret header name: {message}")]
    InvalidHeaderName { message: String },
    #[error("invalid value for secret header {name}")]
    InvalidHeaderValue { name: String },
    #[error("failed to build HTTP client: {message}")]
    HttpClient { message: String },
}

/// Non-streaming OpenAI-compatible Chat Completions client.
///
/// [`ModelConfig::endpoint`] is treated as the complete request URL rather than
/// a base URL to which a path is appended.
pub struct OpenAiChatClient {
    config: ModelConfig,
    endpoint: reqwest::Url,
    timeout: Duration,
    secrets: RuntimeSecretHeaders,
    http: Client,
}

impl OpenAiChatClient {
    /// Creates a client with a 60-second HTTP timeout.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiClientBuildError`] for an invalid endpoint, empty model,
    /// or HTTP client construction failure.
    pub fn new(
        config: ModelConfig,
        secrets: RuntimeSecretHeaders,
    ) -> Result<Self, OpenAiClientBuildError> {
        Self::with_timeout(config, secrets, DEFAULT_TIMEOUT)
    }

    /// Creates a client with an explicit per-request HTTP timeout.
    ///
    /// # Errors
    ///
    /// Returns [`OpenAiClientBuildError`] for invalid configuration.
    pub fn with_timeout(
        config: ModelConfig,
        secrets: RuntimeSecretHeaders,
        timeout: Duration,
    ) -> Result<Self, OpenAiClientBuildError> {
        if config.model.trim().is_empty() {
            return Err(OpenAiClientBuildError::EmptyModel);
        }
        if config.temperature.is_some_and(|value| !value.is_finite()) {
            return Err(OpenAiClientBuildError::NonFiniteTemperature);
        }
        if timeout.is_zero() {
            return Err(OpenAiClientBuildError::ZeroTimeout);
        }
        let endpoint = reqwest::Url::parse(&config.endpoint).map_err(|error| {
            OpenAiClientBuildError::InvalidEndpoint {
                endpoint: config.endpoint.clone(),
                message: error.to_string(),
            }
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(OpenAiClientBuildError::InvalidEndpoint {
                endpoint: config.endpoint.clone(),
                message: "only http and https endpoints are supported".to_owned(),
            });
        }
        if endpoint.host_str().is_none() {
            return Err(OpenAiClientBuildError::InvalidEndpoint {
                endpoint: config.endpoint.clone(),
                message: "HTTP endpoint must include a host".to_owned(),
            });
        }
        let http =
            Client::builder()
                .build()
                .map_err(|error| OpenAiClientBuildError::HttpClient {
                    message: secrets.redact(&error.to_string()),
                })?;
        Ok(Self {
            config,
            endpoint,
            timeout,
            secrets,
            http,
        })
    }

    #[must_use]
    pub fn config(&self) -> &ModelConfig {
        &self.config
    }
}

impl fmt::Debug for OpenAiChatClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiChatClient")
            .field("config", &self.config)
            .field("timeout", &self.timeout)
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelClient for OpenAiChatClient {
    fn context_window_tokens(&self) -> Option<u32> {
        self.config.context_window_tokens
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let payload = ChatRequest::from_model_request(&self.config, request)?;
        let response = self
            .http
            .post(self.endpoint.clone())
            .headers(self.secrets.headers.clone())
            .timeout(self.timeout)
            .json(&payload)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    ModelError::Timeout {
                        timeout_ms: duration_millis(self.timeout),
                    }
                } else {
                    ModelError::Transport {
                        message: self.secrets.redact(&error.to_string()),
                    }
                }
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|error| {
            if error.is_timeout() {
                ModelError::Timeout {
                    timeout_ms: duration_millis(self.timeout),
                }
            } else {
                ModelError::Transport {
                    message: self.secrets.redact(&error.to_string()),
                }
            }
        })?;
        if !status.is_success() {
            let diagnostic = self.secrets.redact(&body);
            return Err(ModelError::Http {
                status: status.as_u16(),
                message: summarize_error_body(&diagnostic),
                retryable: status.as_u16() == 408
                    || status.as_u16() == 429
                    || status.is_server_error()
                    || is_context_overflow_error(status.as_u16(), &body),
            });
        }

        let parsed: ChatResponse =
            serde_json::from_str(&body).map_err(|error| ModelError::InvalidResponse {
                message: format!(
                    "response body is not valid Chat Completions JSON: {error}; body={}",
                    truncate_diagnostic(&self.secrets.redact(&body))
                ),
            })?;
        parsed.into_model_response()
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn truncate_diagnostic(value: &str) -> String {
    if value.len() <= MAX_DIAGNOSTIC_BYTES {
        return value.to_owned();
    }
    let mut boundary = MAX_DIAGNOSTIC_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}...[truncated]", &value[..boundary])
}

fn summarize_error_body(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with('<') {
        if let Some(title) = html_element_text(trimmed, "title") {
            return title;
        }
        if let Some(heading) = html_element_text(trimmed, "h1") {
            return heading;
        }
        return "upstream returned an HTML error page".to_owned();
    }
    truncate_diagnostic(trimmed)
}

fn is_context_overflow_error(status: u16, body: &str) -> bool {
    if !matches!(status, 400 | 413) {
        return false;
    }

    let message = body.to_ascii_lowercase();
    [
        "context_length_exceeded",
        "context_window_exceeded",
        "context_length_error",
        "context_window_error",
        "prompt_too_long",
        "input_too_long",
        "maximum context length",
        "maximum context window",
        "context length exceeded",
        "context window exceeded",
        "exceeds the context length",
        "exceeds the context window",
        "too many tokens",
        "token limit exceeded",
        "token count exceeds the maximum",
        "maximum number of tokens allowed",
        "prompt is too long",
        "input is too long",
        "reduce the length of the messages",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn html_element_text(value: &str, element: &str) -> Option<String> {
    let opening = format!("<{element}>");
    let closing = format!("</{element}>");
    let start = value.find(&opening)? + opening.len();
    let end = value[start..].find(&closing)? + start;
    let text = value[start..end].trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
}

impl ChatRequest {
    fn from_model_request(config: &ModelConfig, request: ModelRequest) -> Result<Self, ModelError> {
        let messages = request
            .messages
            .into_iter()
            .map(ChatMessage::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        if messages.is_empty() {
            return Err(ModelError::InvalidRequest {
                message: "at least one message is required".to_owned(),
            });
        }
        let tools: Vec<_> = request.tools.into_iter().map(ChatTool::from).collect();
        let tool_choice = (!tools.is_empty()).then_some("auto");
        Ok(Self {
            model: config.model.clone(),
            messages,
            tools,
            tool_choice,
            temperature: config.temperature,
            max_tokens: config.max_output_tokens,
            stream: false,
        })
    }
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ChatToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl TryFrom<ModelMessage> for ChatMessage {
    type Error = ModelError;

    fn try_from(message: ModelMessage) -> Result<Self, Self::Error> {
        match message.role {
            ModelRole::System | ModelRole::User => {
                if message.content.is_none()
                    || !message.tool_calls.is_empty()
                    || message.tool_call_id.is_some()
                {
                    return Err(invalid_message(message.role));
                }
            }
            ModelRole::Assistant => {
                if message.content.is_none() && message.tool_calls.is_empty() {
                    return Err(invalid_message(message.role));
                }
                if message.tool_call_id.is_some() || message.name.is_some() {
                    return Err(invalid_message(message.role));
                }
            }
            ModelRole::Tool => {
                if message.content.is_none()
                    || message.tool_call_id.is_none()
                    || !message.tool_calls.is_empty()
                {
                    return Err(invalid_message(message.role));
                }
            }
        }
        let role = match message.role {
            ModelRole::System => "system",
            ModelRole::User => "user",
            ModelRole::Assistant => "assistant",
            ModelRole::Tool => "tool",
        };
        let tool_calls = message
            .tool_calls
            .into_iter()
            .map(ChatToolCall::from)
            .collect();
        Ok(Self {
            role,
            content: message.content,
            tool_calls,
            tool_call_id: message.tool_call_id,
            name: message.name,
        })
    }
}

fn invalid_message(role: ModelRole) -> ModelError {
    ModelError::InvalidRequest {
        message: format!("invalid field combination for {role:?} message"),
    }
}

#[derive(Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatFunctionCall,
}

impl From<AssistantToolCall> for ChatToolCall {
    fn from(call: AssistantToolCall) -> Self {
        Self {
            id: call.id,
            kind: "function",
            function: ChatFunctionCall {
                name: call.name,
                arguments: serde_json::to_string(&call.arguments)
                    .expect("serializing serde_json::Value cannot fail"),
            },
        }
    }
}

#[derive(Serialize)]
struct ChatFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ChatTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatToolDefinition,
}

impl From<ToolDefinition> for ChatTool {
    fn from(definition: ToolDefinition) -> Self {
        Self {
            kind: "function",
            function: ChatToolDefinition {
                name: definition.name,
                description: definition.description,
                parameters: definition.input_schema,
            },
        }
    }
}

#[derive(Serialize)]
struct ChatToolDefinition {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

impl ChatResponse {
    fn into_model_response(self) -> Result<ModelResponse, ModelError> {
        let choice =
            self.choices
                .into_iter()
                .next()
                .ok_or_else(|| ModelError::InvalidResponse {
                    message: "response contains no choices".to_owned(),
                })?;
        let finish_reason = choice.finish_reason;
        let output = choice
            .message
            .into_output(finish_reason.as_deref() == Some("length"))?;
        let usage = self.usage.map(TokenUsage::try_from).transpose()?;
        Ok(ModelResponse {
            output,
            usage,
            finish_reason,
        })
    }
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ChatResponseToolCall>,
}

impl ChatResponseMessage {
    fn into_output(self, length_limited: bool) -> Result<AssistantOutput, ModelError> {
        if !self.tool_calls.is_empty() {
            let calls = self
                .tool_calls
                .into_iter()
                .map(AssistantToolCall::try_from)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    if length_limited {
                        ModelError::InvalidResponse {
                            message: format!(
                                "finish_reason=length indicates truncated tool call arguments: {error}"
                            ),
                        }
                    } else {
                        error
                    }
                })?;
            return Ok(AssistantOutput::ToolCalls {
                content: self.content,
                calls,
            });
        }
        let content = self.content.ok_or_else(|| ModelError::InvalidResponse {
            message: if length_limited {
                "finish_reason=length indicates truncated assistant content".to_owned()
            } else {
                "assistant message has neither content nor tool calls".to_owned()
            },
        })?;
        if content.trim().is_empty() {
            return Err(ModelError::InvalidResponse {
                message: if length_limited {
                    "finish_reason=length indicates truncated assistant content".to_owned()
                } else {
                    "assistant returned empty content without tool calls".to_owned()
                },
            });
        }
        if let Some(answer) = parse_structured_answer(&content) {
            return Ok(AssistantOutput::FinalAnswer { answer });
        }
        if length_limited {
            return Err(ModelError::InvalidResponse {
                message: "finish_reason=length indicates truncated assistant content that is not a complete structured answer".to_owned(),
            });
        }
        Ok(AssistantOutput::UnstructuredText { content })
    }
}

fn parse_structured_answer(content: &str) -> Option<StructuredAnswer> {
    let content = content.trim().trim_start_matches('\u{feff}').trim();
    serde_json::from_str(content).ok().or_else(|| {
        json_object_candidates(content).find_map(|candidate| serde_json::from_str(candidate).ok())
    })
}

fn json_object_candidates(value: &str) -> impl Iterator<Item = &str> {
    let mut ranges = Vec::new();
    let mut start = None;
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, character) in value.char_indices() {
        if start.is_none() {
            if character == '{' {
                start = Some(index);
                depth = 1;
            }
            continue;
        }

        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => in_string = true,
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let begin = start.take().expect("JSON candidate has a start");
                    ranges.push(begin..index + character.len_utf8());
                }
            }
            _ => {}
        }
    }

    ranges.into_iter().map(|range| &value[range])
}

#[derive(Deserialize)]
struct ChatResponseToolCall {
    id: String,
    function: ChatResponseFunctionCall,
}

impl TryFrom<ChatResponseToolCall> for AssistantToolCall {
    type Error = ModelError;

    fn try_from(call: ChatResponseToolCall) -> Result<Self, Self::Error> {
        if call.id.is_empty() {
            return Err(ModelError::InvalidResponse {
                message: "tool call ID must not be empty".to_owned(),
            });
        }
        if call.function.name.is_empty() {
            return Err(ModelError::InvalidResponse {
                message: format!("tool call {} has an empty function name", call.id),
            });
        }
        let arguments = serde_json::from_str(&call.function.arguments).map_err(|error| {
            ModelError::InvalidResponse {
                message: format!("tool call {} has invalid JSON arguments: {error}", call.id),
            }
        })?;
        Ok(Self {
            id: call.id,
            name: call.function.name,
            arguments,
        })
    }
}

#[derive(Deserialize)]
struct ChatResponseFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptTokenDetails>,
}

impl TryFrom<ChatUsage> for TokenUsage {
    type Error = ModelError;

    fn try_from(usage: ChatUsage) -> Result<Self, Self::Error> {
        let calculated_total = usage.prompt_tokens.saturating_add(usage.completion_tokens);
        let cached_input_tokens = usage
            .prompt_tokens_details
            .map_or(0, |details| details.cached_tokens);
        if cached_input_tokens > usage.prompt_tokens {
            return Err(ModelError::InvalidResponse {
                message: format!(
                    "cached prompt tokens {cached_input_tokens} exceed prompt tokens {}",
                    usage.prompt_tokens
                ),
            });
        }
        let total_tokens = usage.total_tokens.unwrap_or(calculated_total);
        if total_tokens < calculated_total {
            return Err(ModelError::InvalidResponse {
                message: format!(
                    "total tokens {total_tokens} are less than prompt plus completion tokens {calculated_total}"
                ),
            });
        }
        Ok(Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cached_input_tokens,
            total_tokens,
        })
    }
}

#[derive(Deserialize)]
struct PromptTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
}
