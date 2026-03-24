//! Agent Dispatcher — consumes messages from `bus_queue.jsonl` and spawns
//! Claude CLI sub-processes for target agents.
//!
//! The dispatcher polls the queue file every 5 seconds. When it finds an
//! `agent_message`, it calls the Claude CLI on behalf of the target agent
//! and appends the result back to the queue as an `agent_response`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::claude_runner::call_claude_for_agent;
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_container::sandbox;

/// Message envelope stored in `bus_queue.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BusMessage {
    /// "agent_message" | "agent_response"
    #[serde(rename = "type")]
    msg_type: String,
    message_id: String,
    agent_id: String,
    payload: String,
    timestamp: String,
    /// Present only on responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<String>,
    /// Present only on responses — the original message_id being answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    in_reply_to: Option<String>,
}

/// Starts the agent dispatcher as a background task.
///
/// Polls `bus_queue.jsonl` every 5 seconds for unprocessed `agent_message`
/// entries and dispatches them to the Claude CLI.
pub fn start_agent_dispatcher(
    home_dir: PathBuf,
    registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!("Agent dispatcher started");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if let Err(e) = poll_and_dispatch(&home_dir, &registry).await {
                warn!("Dispatcher poll error: {e}");
            }
        }
    })
}

/// Read the queue, extract pending `agent_message` entries, process them,
/// then rewrite the queue without those entries (and append responses).
async fn poll_and_dispatch(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
) -> Result<(), String> {
    let queue_path = home_dir.join("bus_queue.jsonl");

    // Read all lines
    let content = match tokio::fs::read_to_string(&queue_path).await {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return Ok(()), // no file or empty — nothing to do
    };

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Ok(());
    }

    let mut to_dispatch: Vec<BusMessage> = Vec::new();
    let mut remaining_lines: Vec<String> = Vec::new();

    for line in &lines {
        match serde_json::from_str::<BusMessage>(line) {
            Ok(msg) if msg.msg_type == "agent_message" => {
                to_dispatch.push(msg);
            }
            _ => {
                // Keep non-message lines (responses, unknown types)
                remaining_lines.push(line.to_string());
            }
        }
    }

    if to_dispatch.is_empty() {
        return Ok(());
    }

    info!(count = to_dispatch.len(), "Dispatching agent messages");

    // Rewrite the queue without the messages we're about to process.
    // This prevents double-processing on next poll.
    let new_content = if remaining_lines.is_empty() {
        String::new()
    } else {
        let mut s = remaining_lines.join("\n");
        s.push('\n');
        s
    };
    tokio::fs::write(&queue_path, &new_content)
        .await
        .map_err(|e| format!("Failed to rewrite bus_queue: {e}"))?;

    // Process each message concurrently (up to 4 at a time)
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let home = home_dir.to_path_buf();
    let reg = registry.clone();
    let queue = queue_path.clone();

    let mut handles = Vec::new();
    for msg in to_dispatch {
        let permit = semaphore.clone().acquire_owned().await.map_err(|e| e.to_string())?;
        let home = home.clone();
        let reg = reg.clone();
        let queue = queue.clone();

        handles.push(tokio::spawn(async move {
            let result = dispatch_to_agent(&home, &reg, &msg.agent_id, &msg.payload).await;

            let response_text = match &result {
                Ok(text) => text.clone(),
                Err(e) => format!("Error: {e}"),
            };

            info!(
                message_id = %msg.message_id,
                agent = %msg.agent_id,
                ok = result.is_ok(),
                "Agent dispatch completed"
            );

            // Append response to the queue
            let response_entry = BusMessage {
                msg_type: "agent_response".to_string(),
                message_id: uuid::Uuid::new_v4().to_string(),
                agent_id: msg.agent_id.clone(),
                payload: response_text,
                timestamp: Utc::now().to_rfc3339(),
                response: None,
                in_reply_to: Some(msg.message_id.clone()),
            };

            if let Ok(json) = serde_json::to_string(&response_entry) {
                let _ = append_line(&queue, &json).await;
            }

            drop(permit);
        }));
    }

    // Wait for all dispatches to complete
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

/// Dispatch a task to an agent — using sandbox if enabled, otherwise direct call.
async fn dispatch_to_agent(
    home_dir: &std::path::Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
) -> Result<String, String> {
    // Check if agent has sandbox enabled
    let use_sandbox = {
        let reg = registry.read().await;
        let agent = if agent_id == "default" {
            reg.main_agent()
        } else {
            reg.get(agent_id)
        };
        agent.is_some_and(|a| a.config.container.sandbox_enabled)
    };

    if use_sandbox && sandbox::is_sandbox_available().await {
        info!(agent = agent_id, "Dispatching via sandbox");
        dispatch_sandboxed(home_dir, registry, agent_id, prompt).await
    } else {
        call_claude_for_agent(home_dir, registry, agent_id, prompt).await
    }
}

/// Execute a task inside a sandboxed Docker container.
async fn dispatch_sandboxed(
    home_dir: &std::path::Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
) -> Result<String, String> {
    let reg = registry.read().await;
    let agent = if agent_id == "default" {
        reg.main_agent()
    } else {
        reg.get(agent_id)
    };
    let agent = agent.ok_or_else(|| format!("Agent '{agent_id}' not found"))?;

    let agent_dir = agent.dir.clone();
    let model = agent.config.model.preferred.clone();
    let network = agent.config.container.network_access;
    let timeout_ms = agent.config.container.timeout_ms;

    // Build system prompt
    let mut parts = Vec::new();
    if let Some(soul) = &agent.soul {
        parts.push(format!("# Soul\n{soul}"));
    }
    if let Some(identity) = &agent.identity {
        parts.push(format!("# Identity\n{identity}"));
    }
    let system_prompt = parts.join("\n\n---\n\n");
    drop(reg);

    // Get API key
    let api_key = crate::claude_runner::get_api_key_from_home(home_dir).await;
    if api_key.is_empty() {
        return Err("No API key configured for sandbox".to_string());
    }

    let timeout = std::time::Duration::from_millis(timeout_ms);
    let result = sandbox::run_sandboxed(
        &agent_dir,
        prompt,
        &model,
        &system_prompt,
        &api_key,
        timeout,
        network,
    )
    .await?;

    if result.timed_out {
        return Err("Sandbox execution timed out".to_string());
    }

    if result.exit_code != 0 {
        return Err(format!(
            "Sandbox exit code {}: {}",
            result.exit_code,
            result.stderr.chars().take(200).collect::<String>()
        ));
    }

    let text = result.stdout.trim().to_string();
    if text.is_empty() {
        Ok("(empty response from sandbox)".to_string())
    } else {
        Ok(text)
    }
}

/// Append a single line to a file (atomic-ish via OpenOptions::append).
async fn append_line(path: &Path, line: &str) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;

    let mut buf = line.to_string();
    buf.push('\n');
    file.write_all(buf.as_bytes())
        .await
        .map_err(|e| format!("Failed to write to {}: {e}", path.display()))?;

    Ok(())
}
