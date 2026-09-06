use std::{
    error::Error,
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use clap::Parser;
use codeatlas_agent::{ModelPricing, OpenAiChatClient, RuntimeSecretHeaders};
use codeatlas_app::{
    ApplicationConfig,
    credentials::{CredentialsStore, resolve_api_key},
    default_data_directory, spawn_application,
};
use codeatlas_core::{ModelBudget, ModelConfig, MonetaryBudget};
use codeatlas_tui::{ApplicationPort, ChannelApplicationPort, TuiApp, run_tui};

#[derive(Debug, Parser)]
#[command(
    name = "codeatlas",
    version,
    about = "Turn unfamiliar codebases into evidence-backed mental models"
)]
struct Cli {
    /// Repository to prefill and index when the TUI starts.
    repository: Option<PathBuf>,

    /// Complete OpenAI-compatible Chat Completions URL.
    #[arg(
        long,
        env = "CODEATLAS_ENDPOINT",
        default_value = "https://api.openai.com/v1/chat/completions"
    )]
    endpoint: String,

    /// OpenAI-compatible model name.
    #[arg(long, env = "CODEATLAS_MODEL", default_value = "gpt-4.1-mini")]
    model: String,

    /// Optional provider reasoning mode (for example, enabled or disabled).
    #[arg(long, env = "CODEATLAS_REASONING_MODE")]
    reasoning_mode: Option<String>,

    /// Optional provider reasoning effort (for example, low, medium, or high).
    #[arg(long, env = "CODEATLAS_REASONING_EFFORT")]
    reasoning_effort: Option<String>,

    /// Sampling temperature sent to the model.
    #[arg(long, env = "CODEATLAS_TEMPERATURE")]
    temperature: Option<f32>,

    /// Maximum output tokens requested from the model.
    #[arg(long, env = "CODEATLAS_MAX_OUTPUT_TOKENS")]
    max_output_tokens: Option<NonZeroU32>,

    /// Model context window used to keep requests within provider limits.
    #[arg(long, env = "CODEATLAS_CONTEXT_WINDOW_TOKENS")]
    context_window_tokens: Option<NonZeroU32>,

    /// Timeout for each model HTTP request, in seconds.
    #[arg(long, env = "CODEATLAS_MODEL_TIMEOUT_SECONDS", default_value = "180")]
    model_timeout_seconds: NonZeroU64,

    /// Overall timeout for one multi-step Agent question, in seconds.
    #[arg(long, env = "CODEATLAS_AGENT_TIMEOUT_SECONDS", default_value = "1200")]
    agent_timeout_seconds: NonZeroU64,

    /// Retries for transport, timeout, 429, and retryable non-gateway failures.
    #[arg(long, env = "CODEATLAS_MODEL_MAX_RETRIES", default_value_t = 2)]
    model_max_retries: u8,

    /// Retries for immediately rejected HTTP 502, 503, and 504 responses.
    #[arg(long, env = "CODEATLAS_GATEWAY_MAX_RETRIES", default_value_t = 5)]
    gateway_max_retries: u8,

    /// Currency label used by explicit model pricing and monetary budgets.
    #[arg(long, env = "CODEATLAS_PRICING_CURRENCY", default_value = "USD")]
    pricing_currency: String,

    /// Uncached input price per million tokens.
    #[arg(long, env = "CODEATLAS_INPUT_PRICE_PER_MILLION", value_parser = parse_non_negative_f64, requires = "output_price_per_million")]
    input_price_per_million: Option<f64>,

    /// Cached input price per million tokens; regular input pricing is the fallback.
    #[arg(long, env = "CODEATLAS_CACHED_INPUT_PRICE_PER_MILLION", value_parser = parse_non_negative_f64, requires = "input_price_per_million")]
    cached_input_price_per_million: Option<f64>,

    /// Output price per million tokens.
    #[arg(long, env = "CODEATLAS_OUTPUT_PRICE_PER_MILLION", value_parser = parse_non_negative_f64, requires = "input_price_per_million")]
    output_price_per_million: Option<f64>,

    /// Hard total-token limit for each agent task.
    #[arg(long, env = "CODEATLAS_AGENT_MAX_TOTAL_TOKENS")]
    agent_max_total_tokens: Option<NonZeroU64>,

    /// Hard estimated-cost limit for each agent task.
    #[arg(long, env = "CODEATLAS_AGENT_MAX_COST", value_parser = parse_positive_f64, requires_all = ["input_price_per_million", "output_price_per_million"])]
    agent_max_cost: Option<f64>,

    /// External cache and session directory.
    #[arg(long, env = "CODEATLAS_DATA_DIR")]
    data_directory: Option<PathBuf>,

    /// Prompt for an API key and store it in the private XDG credentials file.
    #[arg(long, conflicts_with = "delete_stored_api_key")]
    store_api_key: bool,

    /// Delete the API key stored in the XDG credentials file.
    #[arg(long, conflicts_with = "store_api_key")]
    delete_stored_api_key: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if cli.store_api_key {
        let credentials = CredentialsStore::discover()?;
        let mut api_key = rpassword::prompt_password("CodeAtlas API key: ")?;
        let result = credentials.store_api_key(&api_key);
        api_key.clear();
        result?;
        println!("Stored API key in {}", credentials.path().display());
        return Ok(());
    }
    if cli.delete_stored_api_key {
        let credentials = CredentialsStore::discover()?;
        if credentials.delete()? {
            println!(
                "Deleted stored API key from {}",
                credentials.path().display()
            );
        } else {
            println!("No stored API key at {}", credentials.path().display());
        }
        return Ok(());
    }

    let model_config = ModelConfig {
        endpoint: cli.endpoint,
        model: cli.model,
        reasoning_mode: cli.reasoning_mode,
        reasoning_effort: cli.reasoning_effort,
        temperature: cli.temperature,
        max_output_tokens: cli.max_output_tokens.map(NonZeroU32::get),
        context_window_tokens: cli.context_window_tokens.map(NonZeroU32::get),
    };
    let secrets = resolve_api_key()?.map_or_else(
        || Ok(RuntimeSecretHeaders::new()),
        RuntimeSecretHeaders::bearer,
    )?;
    let model = Arc::new(OpenAiChatClient::with_timeout(
        model_config,
        secrets,
        Duration::from_secs(cli.model_timeout_seconds.get()),
    )?);
    let mut config =
        ApplicationConfig::new(cli.data_directory.unwrap_or_else(default_data_directory));
    config.agent.timeout = Duration::from_secs(cli.agent_timeout_seconds.get());
    config.agent.max_model_retries = cli.model_max_retries;
    config.agent.max_gateway_retries = cli.gateway_max_retries;
    config.agent.pricing = cli
        .input_price_per_million
        .map(|input_per_million| ModelPricing {
            currency: cli.pricing_currency.clone(),
            input_per_million,
            cached_input_per_million: cli.cached_input_price_per_million,
            output_per_million: cli
                .output_price_per_million
                .expect("clap requires output pricing with input pricing"),
        });
    config.agent.budget = ModelBudget {
        max_total_tokens: cli.agent_max_total_tokens.map(NonZeroU64::get),
        max_cost: cli.agent_max_cost.map(|amount| MonetaryBudget {
            currency: cli.pricing_currency,
            amount,
        }),
    };
    let (channels, runner) = spawn_application(config, model)?;
    let mut port = ChannelApplicationPort::new(channels.commands, channels.events);

    let seed = cli.repository.as_ref().map_or_else(
        || "interactive".to_owned(),
        |path| path.to_string_lossy().into_owned(),
    );
    let mut app = TuiApp::new(seed);
    if let Some(repository) = cli.repository {
        app.set_repository_path(repository.to_string_lossy());
        if let Some(command) = app.submit_repository() {
            port.send_command(command)?;
        }
    }

    let tui_result = run_tui(&mut app, &mut port);
    drop(port);
    let worker_result = runner.join();
    tui_result?;
    worker_result?;
    Ok(())
}

fn parse_non_negative_f64(value: &str) -> Result<f64, String> {
    let parsed = value.parse::<f64>().map_err(|error| error.to_string())?;
    if parsed.is_finite() && parsed >= 0.0 {
        Ok(parsed)
    } else {
        Err("value must be finite and non-negative".to_owned())
    }
}

fn parse_positive_f64(value: &str) -> Result<f64, String> {
    let parsed = parse_non_negative_f64(value)?;
    if parsed > 0.0 {
        Ok(parsed)
    } else {
        Err("value must be greater than zero".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_runtime_options_allow_slow_multi_turn_models() {
        let cli = Cli::try_parse_from(["codeatlas", "."]).expect("default options should parse");

        assert_eq!(cli.model_timeout_seconds.get(), 180);
        assert_eq!(cli.agent_timeout_seconds.get(), 1_200);
        assert_eq!(cli.model_max_retries, 2);
        assert_eq!(cli.gateway_max_retries, 5);
    }

    #[test]
    fn parses_explicit_runtime_options() {
        let cli = Cli::try_parse_from([
            "codeatlas",
            "--model-timeout-seconds",
            "240",
            "--agent-timeout-seconds",
            "900",
            "--context-window-tokens",
            "128000",
            "--max-output-tokens",
            "4096",
            "--model-max-retries",
            "3",
            "--gateway-max-retries",
            "7",
            ".",
        ])
        .expect("valid timeout options");

        assert_eq!(cli.model_timeout_seconds.get(), 240);
        assert_eq!(cli.agent_timeout_seconds.get(), 900);
        assert_eq!(
            cli.context_window_tokens.map(NonZeroU32::get),
            Some(128_000)
        );
        assert_eq!(cli.max_output_tokens.map(NonZeroU32::get), Some(4_096));
        assert_eq!(cli.model_max_retries, 3);
        assert_eq!(cli.gateway_max_retries, 7);
        assert_eq!(cli.repository, Some(PathBuf::from(".")));
    }

    #[test]
    fn rejects_zero_runtime_limits() {
        assert!(Cli::try_parse_from(["codeatlas", "--model-timeout-seconds", "0"]).is_err());
        assert!(Cli::try_parse_from(["codeatlas", "--agent-timeout-seconds", "0"]).is_err());
        assert!(Cli::try_parse_from(["codeatlas", "--context-window-tokens", "0"]).is_err());
        assert!(Cli::try_parse_from(["codeatlas", "--max-output-tokens", "0"]).is_err());
    }

    #[test]
    fn parses_pricing_reasoning_and_task_budgets() {
        let cli = Cli::try_parse_from([
            "codeatlas",
            "--reasoning-mode",
            "enabled",
            "--reasoning-effort",
            "high",
            "--pricing-currency",
            "EUR",
            "--input-price-per-million",
            "1.25",
            "--cached-input-price-per-million",
            "0.25",
            "--output-price-per-million",
            "5",
            "--agent-max-total-tokens",
            "250000",
            "--agent-max-cost",
            "2.5",
        ])
        .expect("cost controls should parse");

        assert_eq!(cli.reasoning_mode.as_deref(), Some("enabled"));
        assert_eq!(cli.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(cli.pricing_currency, "EUR");
        assert_eq!(cli.input_price_per_million, Some(1.25));
        assert_eq!(cli.cached_input_price_per_million, Some(0.25));
        assert_eq!(cli.output_price_per_million, Some(5.0));
        assert_eq!(
            cli.agent_max_total_tokens.map(NonZeroU64::get),
            Some(250_000)
        );
        assert_eq!(cli.agent_max_cost, Some(2.5));
    }

    #[test]
    fn credential_management_flags_parse_and_conflict() {
        let store =
            Cli::try_parse_from(["codeatlas", "--store-api-key"]).expect("store flag should parse");
        assert!(store.store_api_key);
        assert!(!store.delete_stored_api_key);

        let delete = Cli::try_parse_from(["codeatlas", "--delete-stored-api-key"])
            .expect("delete flag should parse");
        assert!(delete.delete_stored_api_key);
        assert!(!delete.store_api_key);

        assert!(
            Cli::try_parse_from(["codeatlas", "--store-api-key", "--delete-stored-api-key"])
                .is_err()
        );
    }
}
