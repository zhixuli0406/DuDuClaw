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

use crate::claude_runner::{call_claude_for_agent_with_type};
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
    // Mutex protects the read-modify-write cycle on bus_queue.jsonl
    let dispatch_lock = Arc::new(tokio::sync::Mutex::new(()));
    tokio::spawn(async move {
        info!("Agent dispatcher started");
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let _guard = dispatch_lock.lock().await;
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
                if !duduclaw_core::is_valid_agent_id(&msg.agent_id) {
                    warn!("Invalid agent_id in bus queue, skipping");
                    continue;
                }
                // Enforce payload size limit (BE-H6) — drop, do NOT keep in queue
                if msg.payload.len() > 100_000 {
                    warn!(id = %msg.message_id, len = msg.payload.len(), "Dropping oversized message (removed from queue)");
                } else {
                    to_dispatch.push(msg);
                }
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

    // ── Request Coalescing ──────────────────────────────────────
    // Merge consecutive messages for the same agent into a single prompt.
    // This reduces API calls and amortizes the system prompt overhead.
    // Max: 5 messages per coalesced group, 2-second window is implicit
    // (we batch within a single poll cycle which is 5 seconds).
    let to_dispatch = coalesce_messages(to_dispatch);

    info!(count = to_dispatch.len(), "Dispatching agent messages (after coalescing)");

    // Rewrite the queue without the messages we're about to process.
    // Uses write→rename for atomicity — prevents data loss if process crashes mid-write.
    let new_content = if remaining_lines.is_empty() {
        String::new()
    } else {
        let mut s = remaining_lines.join("\n");
        s.push('\n');
        s
    };
    let tmp_path = queue_path.with_extension("jsonl.tmp");
    tokio::fs::write(&tmp_path, &new_content)
        .await
        .map_err(|e| format!("Failed to write temp bus_queue: {e}"))?;
    tokio::fs::rename(&tmp_path, &queue_path)
        .await
        .map_err(|e| format!("Failed to rename temp bus_queue: {e}"))?;

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
        if let Err(e) = handle.await {
            warn!("Dispatch task panicked: {e}");
        }
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
        call_claude_for_agent_with_type(
            home_dir, registry, agent_id, prompt,
            crate::cost_telemetry::RequestType::Dispatch,
        ).await
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

/// Maximum number of messages to coalesce into a single prompt per agent.
const MAX_COALESCE: usize = 5;

/// Merge consecutive bus messages for the same agent into a single message.
///
/// This amortizes the system prompt overhead across multiple user messages,
/// reducing total API calls. Messages are grouped by `agent_id`; within each
/// group, payloads are joined with a separator. The first message's
/// `message_id` is reused as the coalesced message's ID.
fn coalesce_messages(messages: Vec<BusMessage>) -> Vec<BusMessage> {
    use std::collections::HashMap;

    // Group by agent_id, preserving insertion order via Vec
    let mut groups: HashMap<String, Vec<BusMessage>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for msg in messages {
        let key = msg.agent_id.clone();
        let entry = groups.entry(key.clone()).or_default();
        if entry.is_empty() {
            order.push(key);
        }
        entry.push(msg);
    }

    let mut result = Vec::new();
    for agent_id in order {
        let group = groups.remove(&agent_id).unwrap_or_default();
        if group.len() <= 1 {
            result.extend(group);
            continue;
        }

        // Split into chunks of MAX_COALESCE
        for chunk in group.chunks(MAX_COALESCE) {
            if chunk.len() == 1 {
                result.push(chunk[0].clone());
                continue;
            }

            let coalesced_payload = chunk
                .iter()
                .enumerate()
                .map(|(i, m)| format!("[Message {}] {}", i + 1, m.payload))
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            let mut coalesced_payload = coalesced_payload;
            if coalesced_payload.len() > 200_000 {
                warn!(size = coalesced_payload.len(), "Coalesced payload too large, truncating");
                coalesced_payload.truncate(200_000);
            }

            let first = &chunk[0];
            info!(
                agent = %first.agent_id,
                merged = chunk.len(),
                "Coalesced messages into single prompt"
            );

            result.push(BusMessage {
                msg_type: "agent_message".to_string(),
                message_id: first.message_id.clone(),
                agent_id: first.agent_id.clone(),
                payload: coalesced_payload,
                timestamp: first.timestamp.clone(),
                response: None,
                in_reply_to: None,
            });
        }
    }

    result
}

/// Append a single line to a file with advisory file lock (MCP-M4).
///
/// Uses `spawn_blocking` + `flock(LOCK_EX)` on Unix for safe concurrent writes.
pub async fn append_line(path: &Path, line: &str) -> Result<(), String> {
    let path = path.to_path_buf();
    let line = line.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: fd comes from a valid, open File handle obtained above.
            // flock is async-signal-safe and the fd remains valid for the duration of this call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "flock failed on {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
        }

        writeln!(file, "{line}")
            .map_err(|e| format!("Failed to write to {}: {e}", path.display()))?;
        Ok(())
    })
    .await
    .map_err(|e| format!("spawn_blocking: {e}"))?
}
