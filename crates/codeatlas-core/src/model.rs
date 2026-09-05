use serde::{Deserialize, Serialize};

/// Serializable, non-secret settings for an OpenAI-compatible endpoint.
///
/// Credentials are deliberately absent. API keys must be supplied to model
/// clients through a separate runtime secret mechanism.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub endpoint: String,
    pub model: String,
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
