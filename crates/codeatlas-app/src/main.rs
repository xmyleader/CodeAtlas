use std::{
    error::Error,
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use clap::Parser;
use codeatlas_agent::{OpenAiChatClient, RuntimeSecretHeaders};
use codeatlas_app::{
    ApplicationConfig,
    credentials::{CredentialsStore, resolve_api_key},
    default_data_directory, spawn_application,
};
use codeatlas_core::ModelConfig;
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

    /// Sampling temperature sent to the model.
    #[arg(long, env = "CODEATLAS_TEMPERATURE")]
    temperature: Option<f32>,

    /// Maximum output tokens requested from the model.
    #[arg(long, env = "CODEATLAS_MAX_OUTPUT_TOKENS")]
    max_output_tokens: Option<u32>,

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
        temperature: cli.temperature,
        max_output_tokens: cli.max_output_tokens,
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
        assert_eq!(cli.model_max_retries, 3);
        assert_eq!(cli.gateway_max_retries, 7);
        assert_eq!(cli.repository, Some(PathBuf::from(".")));
    }

    #[test]
    fn rejects_zero_runtime_limits() {
        assert!(Cli::try_parse_from(["codeatlas", "--model-timeout-seconds", "0"]).is_err());
        assert!(Cli::try_parse_from(["codeatlas", "--agent-timeout-seconds", "0"]).is_err());
        assert!(Cli::try_parse_from(["codeatlas", "--context-window-tokens", "0"]).is_err());
    }

    #[test]
    fn cumulative_token_limit_is_not_a_cli_option() {
        assert!(Cli::try_parse_from(["codeatlas", "--agent-max-total-tokens", "250000"]).is_err());
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
