use serde::{Deserialize, Serialize};

/// Serializable, non-secret settings for an OpenAI-compatible endpoint.
///
/// Credentials are deliberately absent. API keys must be supplied to model
/// clients through a separate runtime secret mechanism.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub endpoint: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub context_window_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub currency: String,
    pub amount: f64,
    pub estimated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub tokens: TokenUsage,
    pub cost: Option<Cost>,
}

/// The terminal outcome of one actual model request attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelCallOutcome {
    Succeeded,
    Failed { message: String, retryable: bool },
    Cancelled,
    TimedOut,
}

/// A presentation-neutral ledger entry for one request attempt, including retries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCallRecord {
    pub id: crate::ModelCallId,
    pub sequence: u64,
    pub model: String,
    pub outcome: ModelCallOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonetaryBudget {
    pub currency: String,
    pub amount: f64,
}

/// Limits applied independently to each agent task.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost: Option<MonetaryBudget>,
}

impl ModelBudget {
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.max_total_tokens.is_some() || self.max_cost.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelBudgetStatus {
    pub budget: ModelBudget,
    pub usage: ModelUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BudgetStopReason {
    TotalTokensReached { used: u64, limit: u64 },
    CostReached { used: f64, limit: MonetaryBudget },
    UsageUnavailable,
}
