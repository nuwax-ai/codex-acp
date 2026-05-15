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

/// Runtime model/provider overrides for embedded callers.
///
/// The standalone CLI still reads the same values from environment variables.
/// Embedded callers can pass this struct instead, avoiding process-wide env
/// mutation before starting the ACP stdio loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexRuntimeOverrides {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub provider_id: Option<String>,
    pub provider_name: Option<String>,
    pub model_context_window: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct RuntimeOverrideValues {
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    provider_id: Option<String>,
    provider_name: Option<String>,
    model_context_window: Option<String>,
}

impl From<CodexRuntimeOverrides> for RuntimeOverrideValues {
    fn from(overrides: CodexRuntimeOverrides) -> Self {
        Self {
            model: overrides.model,
            base_url: overrides.base_url,
            api_key: overrides.api_key,
            provider_id: overrides.provider_id,
            provider_name: overrides.provider_name,
            model_context_window: overrides
                .model_context_window
                .map(|value| value.to_string()),
        }
    }
}

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

fn non_empty_env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn read_runtime_overrides_from_env() -> RuntimeOverrideValues {
    RuntimeOverrideValues {
        model: non_empty_env_var(ENV_CODEX_MODEL),
        base_url: non_empty_env_var(ENV_CODEX_BASE_URL),
        api_key: non_empty_env_var(ENV_CODEX_API_KEY),
        provider_id: non_empty_env_var(ENV_CODEX_PROVIDER_ID),
        provider_name: non_empty_env_var(ENV_CODEX_PROVIDER_NAME),
        model_context_window: non_empty_env_var(ENV_CODEX_MODEL_CONTEXT_WINDOW),
    }
}

/// Apply runtime overrides to the loaded configuration.
///
/// This enables per-process configuration of the LLM model, allowing multiple
/// agents with different providers to run on the same system. The overrides can
/// come from environment variables or an embedding application.
fn apply_runtime_override_values(mut config: Config, overrides: RuntimeOverrideValues) -> Config {
    if let Some(model) = overrides
        .model
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        config.model = Some(model.to_string());
    }

    let provider_id = overrides
        .provider_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| config.model_provider_id.clone());

    let provider_name = overrides
        .provider_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let api_key = overrides
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let base_url = overrides
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);

    let custom_provider_configured = base_url.is_some() || api_key.is_some();
    match resolve_model_context_window_override(
        overrides.model_context_window.as_deref(),
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
        let env_key = if api_key.is_some() {
            None
        } else {
            Some(ENV_CODEX_API_KEY.to_string())
        };
        let provider_info = ModelProviderInfo {
            name: provider_name.unwrap_or_else(|| provider_id.clone()),
            base_url,
            // When an API key is provided by an embedding caller, store it in
            // the provider config so Codex does not need to read process env.
            env_key,
            env_key_instructions: None,
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
            // No OpenAI login required; API key is provided by env_key or an
            // embedded bearer token.
            requires_openai_auth: false,
            // Disable WebSocket transport; most domestic models only support HTTP
            supports_websockets: false,
        };

        config.model_provider_id = provider_id.clone();
        config.model_provider = provider_info.clone();
        config.model_providers.insert(provider_id, provider_info);
    }

    if config.model.is_some() || config.model_provider.base_url.is_some() {
        tracing::info!(
            model = %config.model.as_deref().unwrap_or("<unset>"),
            base_url = %config.model_provider.base_url.as_deref().unwrap_or("<unset>"),
            provider_id = %config.model_provider_id,
            provider_name = %config.model_provider.name,
            model_context_window = ?config.model_context_window,
            "applied runtime overrides"
        );
    }

    config
}

fn apply_runtime_overrides(config: Config, overrides: CodexRuntimeOverrides) -> Config {
    apply_runtime_override_values(config, overrides.into())
}

/// Apply environment variable overrides to the loaded configuration.
fn apply_env_overrides(config: Config) -> Config {
    apply_runtime_override_values(config, read_runtime_overrides_from_env())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    async fn base_test_config() -> Config {
        let codex_home =
            std::env::temp_dir().join(format!("nuwax-codex-acp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&codex_home).expect("create test codex home");
        let config =
            Config::load_default_with_cli_overrides_for_codex_home(codex_home.clone(), vec![])
                .await
                .expect("load base test config");
        std::fs::remove_dir_all(codex_home).expect("remove test codex home");
        config
    }

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

    #[tokio::test]
    async fn runtime_overrides_configure_custom_provider_without_env_key() {
        let config = apply_runtime_overrides(
            base_test_config().await,
            CodexRuntimeOverrides {
                model: Some("glm-5".to_string()),
                base_url: Some("http://127.0.0.1:12345/v1".to_string()),
                api_key: Some("real-key".to_string()),
                provider_id: Some("glm".to_string()),
                provider_name: Some("GLM".to_string()),
                model_context_window: Some(200_000),
            },
        );

        assert_eq!(config.model.as_deref(), Some("glm-5"));
        assert_eq!(config.model_provider_id, "glm");
        assert_eq!(config.model_provider.name, "GLM");
        assert_eq!(
            config.model_provider.base_url.as_deref(),
            Some("http://127.0.0.1:12345/v1")
        );
        assert_eq!(config.model_provider.env_key, None);
        assert_eq!(
            config.model_provider.experimental_bearer_token.as_deref(),
            Some("real-key")
        );
        assert_eq!(config.model_context_window, Some(200_000));
        assert!(config.model_providers.contains_key("glm"));
    }

    #[tokio::test]
    async fn runtime_overrides_without_api_key_keep_env_key_fallback() {
        let config = apply_runtime_overrides(
            base_test_config().await,
            CodexRuntimeOverrides {
                base_url: Some("http://127.0.0.1:12345/v1".to_string()),
                provider_id: Some("glm".to_string()),
                ..CodexRuntimeOverrides::default()
            },
        );

        assert_eq!(
            config.model_provider.env_key.as_deref(),
            Some(ENV_CODEX_API_KEY)
        );
        assert_eq!(config.model_provider.experimental_bearer_token, None);
    }
}
