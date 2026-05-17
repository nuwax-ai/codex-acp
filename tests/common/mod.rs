//! Shared utilities for provider integration tests.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default = "default_wire_api")]
    pub wire_api: String,
}

fn default_wire_api() -> String {
    "chat".to_string()
}

#[derive(Debug, Deserialize)]
pub struct TestConfig {
    pub providers: BTreeMap<String, ProviderConfig>,
}

/// Load config.json from the project root.
///
/// Panics if the file is missing or invalid — run
/// `cargo run --release --example export-zed-config` first.
pub fn load_config() -> TestConfig {
    let config_path = concat!(env!("CARGO_MANIFEST_DIR"), "/config.json");
    let content = std::fs::read_to_string(config_path).unwrap_or_else(|_| {
        panic!(
            "config.json not found at {config_path}. \
             Run 'cargo run --release --example export-zed-config' first."
        )
    });
    serde_json::from_str(&content).expect("Failed to parse config.json")
}

/// Build the agent command string for a given provider.
/// Returns an `AcpAgent`-compatible command with environment variables.
pub fn agent_command(provider: &ProviderConfig) -> String {
    let mut cmd = format!(
        "CODEX_MODEL={model} CODEX_BASE_URL={base_url} CODEX_API_KEY={api_key} CODEX_WIRE_API={wire_api}",
        model = provider.model,
        base_url = provider.base_url,
        api_key = provider.api_key,
        wire_api = provider.wire_api,
    );
    if let Some(cw) = provider.context_window {
        cmd.push_str(&format!(" CODEX_MODEL_CONTEXT_WINDOW={cw}"));
    }
    // Use the cargo-built binary in tests
    cmd.push(' ');
    cmd.push_str(env!("CARGO_BIN_EXE_nuwax-codex-acp"));
    cmd
}
