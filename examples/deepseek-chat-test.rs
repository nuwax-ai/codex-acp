//! Rapid integration test for validating DeepSeek (and other Chinese LLM) Chat API
//! support through the ACP agent protocol.
//!
//! This example spawns `nuwax-codex-acp` as a subprocess, connects to it via the
//! ACP Client SDK, sends a test prompt, and prints the streaming response.
//!
//! # Usage
//!
//! ```bash
//! # Test DeepSeek with Chat API
//! DEEPSEEK_API_KEY=sk-xxx cargo run --example deepseek-chat-test -- \
//!   --model deepseek-chat \
//!   --base-url https://api.deepseek.com/v1 \
//!   --wire-api chat \
//!   "What is 2+2?"
//!
//! # Or set all config via environment variables
//! export DEEPSEEK_API_KEY=sk-xxx
//! export DEEPSEEK_MODEL=deepseek-chat
//! export DEEPSEEK_BASE_URL=https://api.deepseek.com/v1
//! cargo run --example deepseek-chat-test -- "Explain Rust's ownership in one sentence."
//! ```

use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, InitializeRequest, NewSessionRequest, PromptRequest,
    ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::{Agent, ConnectionTo, on_receive_notification, on_receive_request};
use agent_client_protocol_tokio::AcpAgent;
use clap::Parser;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
#[command(name = "deepseek-chat-test")]
#[command(about = "Integration test for Chat API support via ACP", long_about = None)]
struct Cli {
    /// DeepSeek model name (env: DEEPSEEK_MODEL)
    #[arg(long, env = "DEEPSEEK_MODEL", default_value = "deepseek-chat")]
    model: String,

    /// DeepSeek API base URL (env: DEEPSEEK_BASE_URL)
    #[arg(
        long,
        env = "DEEPSEEK_BASE_URL",
        default_value = "https://api.deepseek.com/v1"
    )]
    base_url: String,

    /// DeepSeek API key (env: DEEPSEEK_API_KEY)
    #[arg(long, env = "DEEPSEEK_API_KEY")]
    api_key: Option<String>,

    /// Wire API to use: "chat" or "responses" (env: CODEX_WIRE_API)
    #[arg(long, env = "CODEX_WIRE_API", default_value = "chat")]
    wire_api: String,

    /// Path to nuwax-codex-acp binary (defaults to searching PATH)
    #[arg(long, default_value = "nuwax-codex-acp")]
    agent_bin: String,

    /// Working directory for the agent session
    #[arg(long, default_value = ".")]
    cwd: PathBuf,

    /// The prompt to send
    prompt: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let api_key = cli.api_key.ok_or_else(|| {
        "DEEPSEEK_API_KEY not set. Pass --api-key or set DEEPSEEK_API_KEY environment variable."
            .to_string()
    })?;

    // Resolve cwd before moving into closures
    let cwd = if cli.cwd.is_relative() {
        std::env::current_dir()?.join(&cli.cwd)
    } else {
        cli.cwd.clone()
    };
    let cwd = std::path::absolute(&cwd)?;

    let model = cli.model.clone();
    let wire_api = cli.wire_api.clone();
    let prompt = cli.prompt.clone();
    let agent_bin = cli.agent_bin.clone();

    // Build the agent command with environment variables for Codex config
    let agent_cmd = format!(
        "CODEX_MODEL={model} CODEX_BASE_URL={base_url} CODEX_API_KEY={api_key} CODEX_WIRE_API={wire_api} {agent_bin}",
        model = model,
        base_url = cli.base_url,
        api_key = api_key,
        wire_api = wire_api,
        agent_bin = agent_bin,
    );

    eprintln!("Spawning agent: {agent_bin} (model={model}, wire_api={wire_api})");

    let agent = AcpAgent::from_str(&agent_cmd)?.with_debug(|line, direction| {
        eprintln!("[ACP {:?}] {line}", direction);
    });

    // Track the stop reason for post-hoc validation
    let stop_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stop_reason_clone = stop_reason.clone();

    agent_client_protocol::Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                match notification.update {
                    SessionUpdate::AgentMessageChunk(ContentChunk {
                        content: ContentBlock::Text(text),
                        ..
                    }) => {
                        print!("{}", text.text);
                    }
                    SessionUpdate::AgentThoughtChunk(ContentChunk {
                        content: ContentBlock::Text(text),
                        ..
                    }) => {
                        eprintln!("\n[Thought] {}", text.text);
                    }
                    SessionUpdate::ToolCall(tool_call) => {
                        eprintln!(
                            "\n[Tool Call] {} (id={})",
                            tool_call.title, tool_call.tool_call_id
                        );
                    }
                    _ => {}
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                // YOLO mode for testing: auto-approve all permission requests
                eprintln!(
                    "Auto-approving permission for tool_call_id={}, title={:?}",
                    request.tool_call.tool_call_id, request.tool_call.fields.title
                );
                let option_id = request.options.first().map(|opt| opt.option_id.clone());
                if let Some(id) = option_id {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
                    ))
                } else {
                    eprintln!("No options in permission request, cancelling");
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ))
                }
            },
            on_receive_request!(),
        )
        .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
            eprintln!("Initializing agent...");
            let init_response = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            eprintln!("Agent initialized: {:?}", init_response.agent_info);

            eprintln!("Creating session (cwd: {})...", cwd.display());
            let new_session_response = connection
                .send_request(NewSessionRequest::new(cwd))
                .block_task()
                .await?;

            let session_id = new_session_response.session_id;
            eprintln!("Session created: {session_id}");

            eprintln!("Sending prompt: \"{prompt}\"");
            let prompt_response = connection
                .send_request(PromptRequest::new(
                    session_id,
                    vec![ContentBlock::Text(TextContent::new(prompt))],
                ))
                .block_task()
                .await?;

            println!(); // newline after streaming text
            eprintln!("Stop reason: {:?}", prompt_response.stop_reason);
            *stop_reason_clone.lock().unwrap() = Some(format!("{:?}", prompt_response.stop_reason));

            Ok(())
        })
        .await?;

    let reason = stop_reason.lock().unwrap();
    match reason.as_deref() {
        Some("EndTurn") => {
            eprintln!("SUCCESS: Agent completed normally.");
            Ok(())
        }
        Some(other) => {
            eprintln!("WARNING: Unexpected stop reason: {other}");
            Ok(())
        }
        None => {
            eprintln!("ERROR: No stop reason recorded.");
            Err("Agent did not produce a stop reason".into())
        }
    }
}
