use std::{
    error::Error,
    num::{NonZeroU32, NonZeroU64},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

#[cfg(target_os = "linux")]
use std::{
    fs,
    os::unix::process::CommandExt as _,
    process::{Command, Stdio},
    thread,
};

use clap::Parser;
use codeatlas_agent::{OpenAiChatClient, RuntimeSecretHeaders};
use codeatlas_app::{
    ApplicationConfig,
    credentials::{CredentialsStore, resolve_api_key},
    default_data_directory, spawn_application,
};
use codeatlas_core::ModelConfig;

#[derive(Debug, Parser)]
#[command(
    name = "codeatlas-gui",
    version,
    about = "Explore unfamiliar codebases in a native evidence-backed workbench"
)]
struct Cli {
    /// Repository to prefill and index when the GUI starts.
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
    #[cfg(target_os = "linux")]
    prepare_wsl_ibus()?;

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
    let initial_repository = cli
        .repository
        .map(|path| path.to_string_lossy().into_owned());

    let gui_result = codeatlas_gui::run_gui(channels.commands, channels.events, initial_repository);
    let worker_result = runner.join();
    gui_result?;
    worker_result?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn prepare_wsl_ibus() -> Result<(), Box<dyn Error>> {
    const REEXEC_MARKER: &str = "CODEATLAS_WSL_IBUS_READY";

    if std::env::var_os(REEXEC_MARKER).is_some()
        || std::env::var_os("XMODIFIERS").is_some_and(|value| !value.is_empty())
        || !is_wsl()
        || !command_succeeds("ibus-daemon", &["--version"])
    {
        return Ok(());
    }

    if !command_succeeds("ibus", &["address"]) {
        let started = Command::new("ibus-daemon")
            .args(["--daemonize", "--xim"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !started {
            return Ok(());
        }
        for _ in 0..20 {
            if command_succeeds("ibus", &["address"]) {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    if !command_succeeds("ibus", &["address"]) {
        return Ok(());
    }

    let executable = std::env::current_exe()?;
    let error = Command::new(executable)
        .args(std::env::args_os().skip(1))
        .env(REEXEC_MARKER, "1")
        .env("XMODIFIERS", "@im=ibus")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .exec();
    Err(error.into())
}

#[cfg(target_os = "linux")]
fn command_succeeds(program: &str, arguments: &[&str]) -> bool {
    Command::new(program)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn is_wsl() -> bool {
    std::env::var_os("WSL_INTEROP").is_some()
        || std::env::var_os("WSL_DISTRO_NAME").is_some()
        || fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_runtime_options_match_the_tui_defaults() {
        let cli = Cli::try_parse_from(["codeatlas-gui", "."]).expect("default options");

        assert_eq!(cli.model_timeout_seconds.get(), 180);
        assert_eq!(cli.agent_timeout_seconds.get(), 1_200);
        assert_eq!(cli.model_max_retries, 2);
        assert_eq!(cli.gateway_max_retries, 5);
        assert_eq!(cli.repository, Some(PathBuf::from(".")));
    }
}
