//! Codex ACP - An Agent Client Protocol implementation for Codex.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use agent_client_protocol::ByteStreams;
use codex_core::config::{Config, ConfigOverrides};
use codex_model_provider_info::ModelProviderInfo;
use codex_utils_cli::CliConfigOverrides;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing_subscriber::EnvFilter;

mod codex_agent;
mod thread;

/// Environment variable names for agent configuration.
/// These allow configuring different LLM providers per process.
const ENV_CODEX_MODEL: &str = "CODEX_MODEL";
const ENV_CODEX_BASE_URL: &str = "CODEX_BASE_URL";
const ENV_CODEX_API_KEY: &str = "CODEX_API_KEY";
const ENV_CODEX_PROVIDER_ID: &str = "CODEX_PROVIDER_ID";
const ENV_CODEX_PROVIDER_NAME: &str = "CODEX_PROVIDER_NAME";
const ENV_CODEX_MODEL_CONTEXT_WINDOW: &str = "CODEX_MODEL_CONTEXT_WINDOW";
const DEFAULT_CUSTOM_MODEL_CONTEXT_WINDOW: i64 = 200_000;

#[derive(Debug, PartialEq, Eq)]
enum ModelContextWindowResolution {
    Explicit(i64),
    Default(i64),
    Keep,
    Invalid(String),
}

fn resolve_model_context_window_override(
    raw_value: Option<&str>,
    custom_provider_configured: bool,
    current_context_window: Option<i64>,
) -> ModelContextWindowResolution {
    if let Some(raw_value) = raw_value {
        let trimmed = raw_value.trim();
        return match trimmed.parse::<i64>() {
            Ok(value) if value > 0 => ModelContextWindowResolution::Explicit(value),
            _ => ModelContextWindowResolution::Invalid(trimmed.to_string()),
        };
    }

    if custom_provider_configured && current_context_window.is_none() {
        ModelContextWindowResolution::Default(DEFAULT_CUSTOM_MODEL_CONTEXT_WINDOW)
    } else {
        ModelContextWindowResolution::Keep
    }
}

/// Apply environment variable overrides to the loaded configuration.
/// This enables per-process configuration of the LLM model,
/// allowing multiple agents with different providers to run on the same system.
fn apply_env_overrides(mut config: Config) -> Config {
    if let Ok(model) = std::env::var(ENV_CODEX_MODEL) {
        let model = model.trim();
        if !model.is_empty() {
            config.model = Some(model.to_string());
        }
    }

    let provider_id = std::env::var(ENV_CODEX_PROVIDER_ID)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| config.model_provider_id.clone());

    let provider_name = std::env::var(ENV_CODEX_PROVIDER_NAME)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string());

    let api_key = std::env::var(ENV_CODEX_API_KEY)
        .ok()
        .filter(|v| !v.trim().is_empty());

    let base_url = std::env::var(ENV_CODEX_BASE_URL).ok().and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let model_context_window = std::env::var(ENV_CODEX_MODEL_CONTEXT_WINDOW).ok();
    let custom_provider_configured = base_url.is_some() || api_key.is_some();

    match resolve_model_context_window_override(
        model_context_window.as_deref(),
        custom_provider_configured,
        config.model_context_window,
    ) {
        ModelContextWindowResolution::Explicit(value)
        | ModelContextWindowResolution::Default(value) => {
            config.model_context_window = Some(value);
        }
        ModelContextWindowResolution::Keep => {}
        ModelContextWindowResolution::Invalid(value) => {
            tracing::warn!(
                env_var = ENV_CODEX_MODEL_CONTEXT_WINDOW,
                value = %value,
                "ignoring invalid model context window override; expected a positive integer"
            );
        }
    }

    if custom_provider_configured {
        let provider_info = ModelProviderInfo {
            name: provider_name.unwrap_or_else(|| provider_id.clone()),
            base_url,
            // env_key stores the env var name that codex will read at runtime
            env_key: Some(ENV_CODEX_API_KEY.to_string()),
            env_key_instructions: None,
            // Use bearer token for domestic models (codex reads env_key at runtime,
            // but bearer token allows embedding the key directly)
            experimental_bearer_token: api_key,
            auth: None,
            aws: None,
            // Use Responses API wire protocol (codex only supports this)
            wire_api: Default::default(),
            query_params: None,
            // Include version header for debugging/analytics
            http_headers: Some(
                [("version".to_string(), env!("CARGO_PKG_VERSION").to_string())]
                    .into_iter()
                    .collect(),
            ),
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            websocket_connect_timeout_ms: None,
            // No OpenAI login required; API key is provided via CODEX_API_KEY env var
            requires_openai_auth: false,
            // Disable WebSocket transport; most domestic models only support HTTP
            supports_websockets: false,
        };

        config.model_provider_id = provider_id.clone();
        config.model_provider = provider_info.clone();
        config.model_providers.insert(provider_id, provider_info);
    } else if let Ok(base_url) = std::env::var(ENV_CODEX_BASE_URL) {
        // Legacy path: only base_url was set, no api_key → just update existing provider
        let base_url = base_url.trim();
        if !base_url.is_empty() {
            let base_url = base_url.to_string();
            config.model_provider.base_url = Some(base_url.clone());
            if let Some(provider) = config.model_providers.get_mut(&config.model_provider_id) {
                provider.base_url = Some(base_url);
            }
        }
    }

    if config.model.is_some() || config.model_provider.base_url.is_some() {
        tracing::info!(
            model = %config.model.as_deref().unwrap_or("<unset>"),
            base_url = %config.model_provider.base_url.as_deref().unwrap_or("<unset>"),
            provider_id = %config.model_provider_id,
            provider_name = %config.model_provider.name,
            model_context_window = ?config.model_context_window,
            "applied environment variable overrides"
        );
    }

    config
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
    // Install a simple subscriber so `tracing` output is visible.
    // Users can control the log level with `RUST_LOG`.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Parse CLI overrides and load configuration
    let cli_kv_overrides = cli_config_overrides.parse_overrides().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("error parsing -c overrides: {e}"),
        )
    })?;

    let config_overrides = ConfigOverrides {
        codex_linux_sandbox_exe: codex_linux_sandbox_exe.clone(),
        ..ConfigOverrides::default()
    };

    let config =
        Config::load_with_cli_overrides_and_harness_overrides(cli_kv_overrides, config_overrides)
            .await
            .map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("error loading config: {e}"),
                )
            })?;

    // Apply environment variable overrides (CODEX_BASE_URL, CODEX_MODEL, etc.)
    let config = apply_env_overrides(config);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_context_window_uses_explicit_env_value() {
        assert_eq!(
            resolve_model_context_window_override(Some("200000"), true, None),
            ModelContextWindowResolution::Explicit(200_000)
        );
    }

    #[test]
    fn model_context_window_defaults_for_custom_provider_when_unset() {
        assert_eq!(
            resolve_model_context_window_override(None, true, None),
            ModelContextWindowResolution::Default(DEFAULT_CUSTOM_MODEL_CONTEXT_WINDOW)
        );
    }

    #[test]
    fn model_context_window_keeps_existing_config_when_env_unset() {
        assert_eq!(
            resolve_model_context_window_override(None, true, Some(128_000)),
            ModelContextWindowResolution::Keep
        );
    }

    #[test]
    fn model_context_window_invalid_values_do_not_override() {
        assert_eq!(
            resolve_model_context_window_override(Some("abc"), true, Some(128_000)),
            ModelContextWindowResolution::Invalid("abc".to_string())
        );
        assert_eq!(
            resolve_model_context_window_override(Some("0"), true, None),
            ModelContextWindowResolution::Invalid("0".to_string())
        );
        assert_eq!(
            resolve_model_context_window_override(Some("-1"), true, None),
            ModelContextWindowResolution::Invalid("-1".to_string())
        );
    }

    #[test]
    fn model_context_window_is_not_defaulted_for_builtin_provider() {
        assert_eq!(
            resolve_model_context_window_override(None, false, None),
            ModelContextWindowResolution::Keep
        );
    }
}
