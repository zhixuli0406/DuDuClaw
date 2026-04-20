//! ACP server — generates protocol discovery cards (`.well-known` endpoints)
//! and runs a stdio JSON-RPC 2.0 server for A2A protocol support.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

use super::handlers::{A2ATaskManager, handle_prompt_with_agent};

/// Skill descriptor within an Agent Card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// Capabilities advertised by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub streaming: bool,
    pub multi_turn: bool,
    pub tool_use: bool,
}

/// A2A-compatible Agent Card returned at `/.well-known/agent.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub version: String,
    pub capabilities: AgentCapabilities,
    pub skills: Vec<AgentSkill>,
}

/// Minimal ACP server that can generate discovery metadata.
pub struct AcpServer;

impl AcpServer {
    /// Generate an A2A-compatible Agent Card.
    pub fn generate_agent_card(name: &str, description: &str, url: &str) -> AgentCard {
        AgentCard {
            name: name.to_string(),
            description: description.to_string(),
            url: url.to_string(),
            version: duduclaw_gateway::updater::current_version().to_string(),
            capabilities: AgentCapabilities {
                streaming: true,
                multi_turn: true,
                tool_use: true,
            },
            skills: vec![
                AgentSkill {
                    name: "chat".to_string(),
                    description: "Multi-turn conversation".to_string(),
                    tags: vec!["conversation".to_string()],
                },
                AgentSkill {
                    name: "channel_messaging".to_string(),
                    description: "Telegram/LINE/Discord messaging".to_string(),
                    tags: vec!["messaging".to_string()],
                },
                AgentSkill {
                    name: "memory".to_string(),
                    description: "Search and store memories".to_string(),
                    tags: vec!["memory".to_string()],
                },
            ],
        }
    }
}

// ── JSON-RPC helpers ────────────────────────────────────────

fn jsonrpc_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

async fn write_response(
    stdout: &mut tokio::io::Stdout,
    response: &Value,
) -> Result<()> {
    let mut output = serde_json::to_string(response)
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to serialize response: {e}")))?;
    output.push('\n');
    stdout.write_all(output.as_bytes()).await.map_err(|e| {
        DuDuClawError::Gateway(format!("Failed to write to stdout: {e}"))
    })?;
    stdout.flush().await.map_err(|e| {
        DuDuClawError::Gateway(format!("Failed to flush stdout: {e}"))
    })?;
    Ok(())
}

// ── Method handlers ─────────────────────────────────────────

/// Handle `agent/discover` — returns the agent card.
pub(crate) fn handle_agent_discover(id: &Value) -> Value {
    let card = AcpServer::generate_agent_card(
        "DuDuClaw Agent",
        "Multi-Runtime AI Agent Platform with channel routing, memory, and self-evolution",
        "stdio://duduclaw-acp",
    );
    jsonrpc_response(id, serde_json::to_value(card).unwrap_or(Value::Null))
}

/// Handle `tasks/send` — creates a task, runs the prompt, returns result.
pub(crate) fn handle_tasks_send(id: &Value, params: &Value, task_manager: &mut A2ATaskManager) -> Value {
    let message = match params.get("message").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return jsonrpc_error(id, -32602, "Missing required parameter: message");
        }
    };

    let context_id = params
        .get("context_id")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let model = params.get("model").and_then(|v| v.as_str());

    // Create a task in the manager
    let task = task_manager.create_task(context_id, message);
    let task_id = task.id.clone();

    // Run the prompt through the agent handler
    let updates = handle_prompt_with_agent(&task_id, message, model);

    // Extract the final response from updates
    let final_message = updates
        .iter()
        .rev()
        .find_map(|u| match u {
            super::types::SessionUpdate::Complete { final_message, .. } => {
                Some(final_message.clone())
            }
            super::types::SessionUpdate::Error { message, .. } => Some(message.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "No response generated".to_string());

    // Complete the task with the result
    task_manager.complete_task(&task_id, final_message.clone());

    // Build A2A-compatible response
    let task_snapshot = task_manager.get_task(&task_id);
    let task_json = match task_snapshot {
        Some(t) => serde_json::to_value(t).unwrap_or(Value::Null),
        None => Value::Null,
    };

    jsonrpc_response(
        id,
        serde_json::json!({
            "task": task_json,
            "artifacts": [{
                "type": "text",
                "content": final_message,
            }],
        }),
    )
}

/// Handle `tasks/get` — retrieves a task by ID.
pub(crate) fn handle_tasks_get(id: &Value, params: &Value, task_manager: &A2ATaskManager) -> Value {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(tid) => tid,
        None => {
            return jsonrpc_error(id, -32602, "Missing required parameter: task_id");
        }
    };

    match task_manager.get_task(task_id) {
        Some(task) => {
            let task_json = serde_json::to_value(task).unwrap_or(Value::Null);
            jsonrpc_response(id, serde_json::json!({ "task": task_json }))
        }
        None => jsonrpc_error(id, -32001, &format!("Task not found: {task_id}")),
    }
}

/// Handle `tasks/cancel` — cancels a task by ID.
fn handle_tasks_cancel(id: &Value, params: &Value, task_manager: &mut A2ATaskManager) -> Value {
    let task_id = match params.get("task_id").and_then(|v| v.as_str()) {
        Some(tid) => tid,
        None => {
            return jsonrpc_error(id, -32602, "Missing required parameter: task_id");
        }
    };

    if task_manager.cancel_task(task_id) {
        let task_json = task_manager
            .get_task(task_id)
            .and_then(|t| serde_json::to_value(t).ok())
            .unwrap_or(Value::Null);
        jsonrpc_response(id, serde_json::json!({ "task": task_json }))
    } else {
        jsonrpc_error(id, -32001, &format!("Task not found: {task_id}"))
    }
}

// ── Main server loop ────────────────────────────────────────

/// Run the ACP server, reading JSON-RPC 2.0 from stdin and writing responses to stdout.
///
/// Supported methods:
/// - `agent/discover` — returns the A2A agent card
/// - `tasks/send` — creates and executes a task, returns result with artifacts
/// - `tasks/get` — retrieves task status by ID
/// - `tasks/cancel` — cancels a task by ID
pub async fn run_acp_server(_home_dir: &Path) -> Result<()> {
    info!("Starting DuDuClaw ACP server (A2A protocol over stdio)");

    let mut task_manager = A2ATaskManager::new();

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await.map_err(|e| {
            DuDuClawError::Gateway(format!("Failed to read from stdin: {e}"))
        })?;

        if bytes_read == 0 {
            // EOF — client disconnected
            info!("ACP server: stdin closed, shutting down");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                warn!("ACP server: invalid JSON: {e}");
                let err = jsonrpc_error(&Value::Null, -32700, "Parse error");
                write_response(&mut stdout, &err).await?;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = match method {
            "agent/discover" => handle_agent_discover(&id),
            "tasks/send" => handle_tasks_send(&id, &params, &mut task_manager),
            "tasks/get" => handle_tasks_get(&id, &params, &task_manager),
            "tasks/cancel" => handle_tasks_cancel(&id, &params, &mut task_manager),
            _ => jsonrpc_error(&id, -32601, &format!("Method not found: {method}")),
        };

        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}
