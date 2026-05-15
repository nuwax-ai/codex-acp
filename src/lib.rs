//! Codex ACP - An Agent Client Protocol implementation for Codex.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use agent_client_protocol::ByteStreams;
use codex_core::config::{Config, ConfigOverrides};
use codex_utils_cli::CliConfigOverrides;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing_subscriber::EnvFilter;

mod codex_agent;
mod runtime_overrides;
mod thread;

pub use runtime_overrides::CodexRuntimeOverrides;
use runtime_overrides::{apply_env_overrides, apply_runtime_overrides};

async fn load_config(
    codex_linux_sandbox_exe: Option<PathBuf>,
    cli_config_overrides: CliConfigOverrides,
) -> std::io::Result<Config> {
    // Parse CLI overrides and load configuration
    let cli_kv_overrides = cli_config_overrides.parse_overrides().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("error parsing -c overrides: {e}"),
        )
    })?;

    let config_overrides = ConfigOverrides {
        codex_linux_sandbox_exe,
        ..ConfigOverrides::default()
    };

    Config::load_with_cli_overrides_and_harness_overrides(cli_kv_overrides, config_overrides)
        .await
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("error loading config: {e}"),
            )
        })
}

fn init_tracing() {
    // Install a simple subscriber so `tracing` output is visible.
    // Users can control the log level with `RUST_LOG`.
    drop(
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(EnvFilter::from_default_env())
            .try_init(),
    );
}

/// Run the Codex ACP agent.
///
/// This sets up an ACP agent that communicates over stdio, bridging
/// the ACP protocol with the existing codex-rs infrastructure.
///
/// # Errors
///
/// If unable to parse the config or start the program.
pub async fn run_main(
    codex_linux_sandbox_exe: Option<PathBuf>,
    cli_config_overrides: CliConfigOverrides,
) -> std::io::Result<()> {
    init_tracing();
    let config = load_config(codex_linux_sandbox_exe.clone(), cli_config_overrides).await?;

    // Apply environment variable overrides (CODEX_BASE_URL, CODEX_MODEL, etc.)
    let config = apply_env_overrides(config);
    run_main_with_config(codex_linux_sandbox_exe, config).await
}

/// Run the Codex ACP agent with structured runtime overrides.
///
/// This is intended for embedders that need per-agent model/provider settings
/// without mutating process-wide environment variables.
pub async fn run_main_with_runtime_overrides(
    codex_linux_sandbox_exe: Option<PathBuf>,
    cli_config_overrides: CliConfigOverrides,
    runtime_overrides: CodexRuntimeOverrides,
) -> std::io::Result<()> {
    init_tracing();
    let config = load_config(codex_linux_sandbox_exe.clone(), cli_config_overrides).await?;
    let config = apply_runtime_overrides(config, runtime_overrides);
    run_main_with_config(codex_linux_sandbox_exe, config).await
}

/// Run the Codex ACP agent with a fully prepared Codex configuration.
pub async fn run_main_with_config(
    codex_linux_sandbox_exe: Option<PathBuf>,
    config: Config,
) -> std::io::Result<()> {
    // Apply residency requirement so the HTTP client sends the
    // x-openai-internal-codex-residency header on all requests.
    codex_login::default_client::set_default_client_residency_requirement(
        config.enforce_residency.value(),
    );

    let agent = Arc::new(codex_agent::CodexAgent::new(config, codex_linux_sandbox_exe).await?);

    let stdin = tokio::io::stdin().compat();
    let stdout = tokio::io::stdout().compat_write();

    agent
        .serve(ByteStreams::new(stdout, stdin))
        .await
        .map_err(|e| std::io::Error::other(format!("ACP error: {e}")))?;

    Ok(())
}

// Re-export the MCP server types for compatibility
pub use codex_mcp_server::{
    CodexToolCallParam, CodexToolCallReplyParam, ExecApprovalElicitRequestParams,
    ExecApprovalResponse, PatchApprovalElicitRequestParams, PatchApprovalResponse,
};
