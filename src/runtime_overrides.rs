use codex_core::config::Config;
use codex_features::Feature;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::config_types::Personality;

/// Environment variable names for agent configuration.
/// These allow configuring different LLM providers per process.
const ENV_CODEX_MODEL: &str = "CODEX_MODEL";
const ENV_CODEX_BASE_URL: &str = "CODEX_BASE_URL";
const ENV_CODEX_API_KEY: &str = "CODEX_API_KEY";
const ENV_CODEX_PROVIDER_ID: &str = "CODEX_PROVIDER_ID";
const ENV_CODEX_PROVIDER_NAME: &str = "CODEX_PROVIDER_NAME";
const ENV_CODEX_MODEL_CONTEXT_WINDOW: &str = "CODEX_MODEL_CONTEXT_WINDOW";
const ENV_CODEX_PERSONALITY_ENABLED: &str = "CODEX_PERSONALITY_ENABLED";
const ENV_CODEX_WIRE_API: &str = "CODEX_WIRE_API";
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
    pub personality_enabled: Option<bool>,
    pub wire_api: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct RuntimeOverrideValues {
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    provider_id: Option<String>,
    provider_name: Option<String>,
    model_context_window: Option<String>,
    personality_enabled: Option<String>,
    wire_api: Option<String>,
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
            personality_enabled: overrides.personality_enabled.map(|value| value.to_string()),
            wire_api: overrides.wire_api,
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

#[derive(Debug, PartialEq, Eq)]
enum PersonalityResolution {
    Explicit(bool),
    DefaultDisabledForCustomProvider,
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

fn parse_bool_override(raw_value: &str) -> Option<bool> {
    match raw_value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn resolve_personality_override(
    raw_value: Option<&str>,
    custom_provider_configured: bool,
) -> PersonalityResolution {
    if let Some(raw_value) = raw_value {
        let trimmed = raw_value.trim();
        return parse_bool_override(trimmed)
            .map(PersonalityResolution::Explicit)
            .unwrap_or_else(|| PersonalityResolution::Invalid(trimmed.to_string()));
    }

    if custom_provider_configured {
        PersonalityResolution::DefaultDisabledForCustomProvider
    } else {
        PersonalityResolution::Keep
    }
}

fn parse_wire_api(raw_value: Option<&str>) -> Option<codex_model_provider_info::WireApi> {
    match raw_value?.trim().to_ascii_lowercase().as_str() {
        "responses" => Some(codex_model_provider_info::WireApi::Responses),
        "chat" => Some(codex_model_provider_info::WireApi::Chat),
        other => {
            tracing::warn!(
                value = %other,
                "ignoring invalid wire_api; expected 'responses' or 'chat'"
            );
            None
        }
    }
}

fn set_personality_enabled(config: &mut Config, enabled: bool) {
    let result = if enabled {
        config.features.enable(Feature::Personality)
    } else {
        config.features.disable(Feature::Personality)
    };

    if let Err(err) = result {
        tracing::warn!(
            enabled,
            error = %err,
            "failed to apply personality feature override"
        );
        return;
    }

    if enabled {
        config.personality.get_or_insert(Personality::Pragmatic);
    } else {
        config.personality = None;
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
        personality_enabled: non_empty_env_var(ENV_CODEX_PERSONALITY_ENABLED),
        wire_api: non_empty_env_var(ENV_CODEX_WIRE_API),
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

    match resolve_personality_override(
        overrides.personality_enabled.as_deref(),
        custom_provider_configured,
    ) {
        PersonalityResolution::Explicit(enabled) => {
            set_personality_enabled(&mut config, enabled);
        }
        PersonalityResolution::DefaultDisabledForCustomProvider => {
            set_personality_enabled(&mut config, false);
        }
        PersonalityResolution::Keep => {}
        PersonalityResolution::Invalid(value) => {
            tracing::warn!(
                env_var = ENV_CODEX_PERSONALITY_ENABLED,
                value = %value,
                "ignoring invalid personality override; expected true/false, 1/0, yes/no, or on/off"
            );
        }
    }

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
            wire_api: overrides
                .wire_api
                .as_deref()
                .and_then(|v| parse_wire_api(Some(v)))
                .unwrap_or_default(),
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
            personality_enabled = config.features.enabled(Feature::Personality),
            "applied runtime overrides"
        );
    }

    config
}

pub(crate) fn apply_runtime_overrides(config: Config, overrides: CodexRuntimeOverrides) -> Config {
    apply_runtime_override_values(config, overrides.into())
}

/// Apply environment variable overrides to the loaded configuration.
pub(crate) fn apply_env_overrides(config: Config) -> Config {
    apply_runtime_override_values(config, read_runtime_overrides_from_env())
}

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

    #[test]
    fn personality_override_parses_common_bool_values() {
        assert_eq!(
            resolve_personality_override(Some("true"), true),
            PersonalityResolution::Explicit(true)
        );
        assert_eq!(
            resolve_personality_override(Some("1"), true),
            PersonalityResolution::Explicit(true)
        );
        assert_eq!(
            resolve_personality_override(Some("yes"), true),
            PersonalityResolution::Explicit(true)
        );
        assert_eq!(
            resolve_personality_override(Some("on"), true),
            PersonalityResolution::Explicit(true)
        );
        assert_eq!(
            resolve_personality_override(Some("false"), true),
            PersonalityResolution::Explicit(false)
        );
        assert_eq!(
            resolve_personality_override(Some("0"), true),
            PersonalityResolution::Explicit(false)
        );
        assert_eq!(
            resolve_personality_override(Some("no"), true),
            PersonalityResolution::Explicit(false)
        );
        assert_eq!(
            resolve_personality_override(Some("off"), true),
            PersonalityResolution::Explicit(false)
        );
    }

    #[test]
    fn personality_defaults_to_disabled_for_custom_provider() {
        assert_eq!(
            resolve_personality_override(None, true),
            PersonalityResolution::DefaultDisabledForCustomProvider
        );
    }

    #[test]
    fn personality_keeps_builtin_provider_default() {
        assert_eq!(
            resolve_personality_override(None, false),
            PersonalityResolution::Keep
        );
    }

    #[test]
    fn personality_invalid_values_do_not_override() {
        assert_eq!(
            resolve_personality_override(Some("maybe"), true),
            PersonalityResolution::Invalid("maybe".to_string())
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
                personality_enabled: None,
                ..CodexRuntimeOverrides::default()
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
        assert!(!config.features.enabled(Feature::Personality));
        assert_eq!(config.personality, None);
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
        assert!(!config.features.enabled(Feature::Personality));
        assert_eq!(config.personality, None);
    }

    #[tokio::test]
    async fn runtime_overrides_keep_personality_default_for_builtin_provider() {
        let base_config = base_test_config().await;
        let base_personality_enabled = base_config.features.enabled(Feature::Personality);
        let base_personality = base_config.personality;

        let config = apply_runtime_overrides(base_config, CodexRuntimeOverrides::default());

        assert_eq!(
            config.features.enabled(Feature::Personality),
            base_personality_enabled
        );
        assert_eq!(config.personality, base_personality);
    }

    #[tokio::test]
    async fn runtime_overrides_can_enable_personality_for_custom_provider() {
        let config = apply_runtime_overrides(
            base_test_config().await,
            CodexRuntimeOverrides {
                base_url: Some("http://127.0.0.1:12345/v1".to_string()),
                api_key: Some("real-key".to_string()),
                provider_id: Some("glm".to_string()),
                personality_enabled: Some(true),
                ..CodexRuntimeOverrides::default()
            },
        );

        assert!(config.features.enabled(Feature::Personality));
        assert_eq!(config.personality, Some(Personality::Pragmatic));
    }

    #[tokio::test]
    async fn runtime_overrides_can_disable_personality_for_builtin_provider() {
        let config = apply_runtime_overrides(
            base_test_config().await,
            CodexRuntimeOverrides {
                personality_enabled: Some(false),
                ..CodexRuntimeOverrides::default()
            },
        );

        assert!(!config.features.enabled(Feature::Personality));
        assert_eq!(config.personality, None);
    }

    #[tokio::test]
    async fn runtime_overrides_ignore_invalid_personality_value() {
        let base_config = base_test_config().await;
        let base_personality_enabled = base_config.features.enabled(Feature::Personality);
        let base_personality = base_config.personality;

        let config = apply_runtime_override_values(
            base_config,
            RuntimeOverrideValues {
                personality_enabled: Some("maybe".to_string()),
                ..RuntimeOverrideValues::default()
            },
        );

        assert_eq!(
            config.features.enabled(Feature::Personality),
            base_personality_enabled
        );
        assert_eq!(config.personality, base_personality);
    }

    #[test]
    fn wire_api_parses_chat() {
        use codex_model_provider_info::WireApi;
        assert_eq!(parse_wire_api(Some("chat")), Some(WireApi::Chat));
    }

    #[test]
    fn wire_api_parses_responses() {
        use codex_model_provider_info::WireApi;
        assert_eq!(parse_wire_api(Some("responses")), Some(WireApi::Responses));
    }

    #[test]
    fn wire_api_defaults_to_none_when_unset() {
        assert_eq!(parse_wire_api(None), None);
    }

    #[test]
    fn wire_api_ignores_invalid_values() {
        assert_eq!(parse_wire_api(Some("invalid")), None);
        assert_eq!(parse_wire_api(Some("")), None);
        assert_eq!(parse_wire_api(Some("CHATGPT")), None);
    }

    #[tokio::test]
    async fn runtime_overrides_sets_wire_api_to_chat() {
        let config = apply_runtime_overrides(
            base_test_config().await,
            CodexRuntimeOverrides {
                base_url: Some("http://127.0.0.1:12345/v1".to_string()),
                api_key: Some("real-key".to_string()),
                provider_id: Some("glm".to_string()),
                wire_api: Some("chat".to_string()),
                ..CodexRuntimeOverrides::default()
            },
        );
        use codex_model_provider_info::WireApi;
        assert_eq!(config.model_provider.wire_api, WireApi::Chat);
    }

    #[tokio::test]
    async fn runtime_overrides_defaults_wire_api_to_responses() {
        let config = apply_runtime_overrides(
            base_test_config().await,
            CodexRuntimeOverrides {
                base_url: Some("http://127.0.0.1:12345/v1".to_string()),
                api_key: Some("real-key".to_string()),
                provider_id: Some("glm".to_string()),
                ..CodexRuntimeOverrides::default()
            },
        );
        use codex_model_provider_info::WireApi;
        assert_eq!(config.model_provider.wire_api, WireApi::Responses);
    }
}
