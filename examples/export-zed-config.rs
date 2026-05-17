//! Export nuwax-codex-acp agent configurations from Zed's settings.json
//! into a project-local `config.json` for integration tests.
//!
//! Reads `/Users/soddy/.config/zed/settings.json`, extracts `agent_servers`
//! entries whose `command` is `nuwax-codex-acp`, and writes a simplified
//! `config.json` to the project root.
//!
//! # Usage
//!
//! ```bash
//! cargo run --example export-zed-config
//! # Or specify custom paths:
//! cargo run --example export-zed-config -- \
//!   --zed-config /path/to/settings.json \
//!   --output /path/to/config.json
//! ```

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn default_zed_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zed")
        .join("settings.json")
}

#[derive(Parser)]
#[command(name = "export-zed-config")]
#[command(about = "Extract nuwax-codex-acp configs from Zed settings.json")]
struct Cli {
    /// Path to Zed settings.json (defaults to ~/.config/zed/settings.json)
    #[arg(long)]
    zed_config: Option<PathBuf>,

    /// Output path for config.json
    #[arg(long, default_value = "config.json")]
    output: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProviderConfig {
    model: String,
    base_url: String,
    api_key: String,
    #[serde(default)]
    context_window: Option<u32>,
    #[serde(default = "default_wire_api")]
    wire_api: String,
}

fn default_wire_api() -> String {
    "chat".to_string()
}

#[derive(Debug, Deserialize, Serialize)]
struct TestConfig {
    providers: BTreeMap<String, ProviderConfig>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let zed_config_path = cli.zed_config.unwrap_or_else(default_zed_config_path);
    eprintln!("Reading Zed config from: {:?}", zed_config_path);

    let zed_json: Value = {
        let content = std::fs::read_to_string(&zed_config_path)?;
        let cleaned = strip_json_comments(&content);
        serde_json::from_str(&cleaned)?
    };

    let agent_servers = zed_json
        .get("agent_servers")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "No 'agent_servers' key found in Zed settings".to_string())?;

    let mut providers: BTreeMap<String, ProviderConfig> = BTreeMap::new();

    for (name, server) in agent_servers {
        // Only extract nuwax-codex-acp entries
        let command = server.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if command != "nuwax-codex-acp" {
            continue;
        }

        let env = match server.get("env").and_then(|v| v.as_object()) {
            Some(e) => e,
            None => {
                eprintln!("Skipping {name}: no 'env' section");
                continue;
            }
        };

        let model = env
            .get("CODEX_MODEL")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let base_url = env
            .get("CODEX_BASE_URL")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let api_key = env
            .get("CODEX_API_KEY")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let context_window = env
            .get("CODEX_MODEL_CONTEXT_WINDOW")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u32>().ok());
        let wire_api = env
            .get("CODEX_WIRE_API")
            .and_then(|v| v.as_str())
            .unwrap_or("chat")
            .to_string();

        if model.is_empty() || base_url.is_empty() || api_key.is_empty() {
            eprintln!("Skipping {name}: missing required fields (model/base_url/api_key)");
            continue;
        }

        // Use a short provider key derived from the agent name
        let provider_key = name
            .strip_prefix("nuwax-codex-acp-")
            .unwrap_or(name)
            .to_string();

        eprintln!(
            "Extracted provider '{provider_key}': model={model}, base_url={base_url}, wire_api={wire_api}"
        );

        providers.insert(
            provider_key,
            ProviderConfig {
                model,
                base_url,
                api_key,
                context_window,
                wire_api,
            },
        );
    }

    if providers.is_empty() {
        return Err("No nuwax-codex-acp agent servers found in Zed settings".into());
    }

    let config = TestConfig { providers };
    let json = serde_json::to_string_pretty(&config)?;
    std::fs::write(&cli.output, json)?;

    eprintln!(
        "Wrote {} provider(s) to {}",
        config.providers.len(),
        cli.output.display()
    );

    Ok(())
}

/// Strip JavaScript-style comments (// and /* */) and trailing commas from JSON text.
/// Respects JSON string boundaries so `//` inside URLs is not treated as a comment.
fn strip_json_comments(json: &str) -> String {
    let mut result = String::with_capacity(json.len());
    let mut chars = json.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            result.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                result.push(ch);
            }
            '/' => match chars.peek() {
                Some('/') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '*' && chars.peek() == Some(&'/') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => result.push(ch),
            },
            ',' => {
                let remaining: String = chars.clone().collect();
                let next_meaningful = remaining.chars().find(|c| !c.is_whitespace());
                if next_meaningful == Some(']') || next_meaningful == Some('}') {
                    continue;
                }
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }

    result
}
