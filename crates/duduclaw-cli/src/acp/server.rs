//! ACP server — generates protocol discovery cards (`.well-known` endpoints)
//! and runs a stdio JSON-RPC 2.0 server for A2A protocol support.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

use super::handlers::{A2ATaskManager, handle_prompt_with_agent};
use super::message_send::BusTaskIndex;

// The `.well-known` discovery surface below is the card-serving API for an HTTP
// front door. The live route currently lives in `duduclaw-gateway` (which serves
// its own inline card and cannot depend on this crate — dependency runs the other
// way), so within this crate these are exercised only by tests until that route is
// switched over to `resolve_well_known_card`. `allow(dead_code)` documents that
// intent without leaving a build warning.

/// A2A v1.0 well-known Agent Card path.
#[allow(dead_code)]
pub const WELL_KNOWN_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
/// Legacy (pre-v1.0) well-known path — retained as a back-compat alias so
/// clients pinned to the old discovery URL keep working.
#[allow(dead_code)]
pub const WELL_KNOWN_AGENT_CARD_PATH_LEGACY: &str = "/.well-known/agent.json";

/// The A2A protocol version this card conforms to.
pub const A2A_PROTOCOL_VERSION: &str = "1.0";

/// Skill descriptor within an A2A v1.0 Agent Card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    /// Stable machine identifier for the skill (A2A v1.0 required field).
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Example prompts that exercise this skill.
    pub examples: Vec<String>,
}

/// Capabilities advertised by the agent (A2A v1.0 schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Long-running tasks can stream progress over the same connection.
    pub streaming: bool,
    /// Server-initiated push notifications (not yet supported → false).
    pub push_notifications: bool,
    /// Task state transitions are retained and queryable (A2ATaskManager).
    pub state_transition_history: bool,
}

/// Provider identity block (A2A v1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProvider {
    pub organization: String,
    pub url: String,
}

/// The `x-duduclaw` capability-negotiation extension (ADR-002). Carried inside
/// the card's `extensions` object so A2A clients can opt into DuDuClaw's
/// header-based capability negotiation without breaking the base v1.0 schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XDuduclawExtension {
    /// Which ADR defines this extension.
    pub adr: String,
    /// HTTP API compatibility version (`x-duduclaw-version`; ADR-002 §4.3).
    pub version: String,
    /// Enabled capabilities in `<name>/<major>` header form (ADR-002 §4.1).
    pub capabilities: String,
    /// Request/response header used for negotiation (ADR-002 §1).
    pub negotiation_header: String,
}

/// The card's `extensions` object (A2A v1.0 allows vendor extensions here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCardExtensions {
    #[serde(rename = "x-duduclaw")]
    pub x_duduclaw: XDuduclawExtension,
}

/// A2A **v1.0** Agent Card served at [`WELL_KNOWN_AGENT_CARD_PATH`] (and, for
/// back-compat, [`WELL_KNOWN_AGENT_CARD_PATH_LEGACY`]).
///
/// Signed Agent Cards (JWS over the card) are a future addition — noted here so
/// the field set can grow without a breaking change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    /// A2A protocol version this card conforms to (e.g. `"1.0"`).
    pub protocol_version: String,
    pub name: String,
    pub description: String,
    /// The agent's service endpoint (stdio/HTTP URL).
    pub url: String,
    /// The agent implementation's own version (DuDuClaw release).
    pub version: String,
    pub capabilities: AgentCapabilities,
    /// Accepted input media types.
    pub default_input_modes: Vec<String>,
    /// Produced output media types.
    pub default_output_modes: Vec<String>,
    pub skills: Vec<AgentSkill>,
    pub provider: AgentProvider,
    /// Vendor extensions — carries the ADR-002 `x-duduclaw` negotiation block.
    pub extensions: AgentCardExtensions,
}

/// Minimal ACP server that can generate discovery metadata.
pub struct AcpServer;

impl AcpServer {
    /// Generate an A2A **v1.0** Agent Card.
    ///
    /// `skills` are static reasonable defaults describing DuDuClaw's core
    /// surface; a future pass can populate them from the resolved agent's
    /// actual capabilities/skills registry.
    pub fn generate_agent_card(name: &str, description: &str, url: &str) -> AgentCard {
        AgentCard {
            protocol_version: A2A_PROTOCOL_VERSION.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            url: url.to_string(),
            version: duduclaw_gateway::updater::current_version().to_string(),
            capabilities: AgentCapabilities {
                // Honest capabilities: `message/stream` and push notifications
                // are answered with A2A UnsupportedOperationError (-32004), so
                // the card must not advertise them.
                streaming: false,
                push_notifications: false,
                state_transition_history: true,
            },
            default_input_modes: vec!["text/plain".to_string()],
            default_output_modes: vec!["text/plain".to_string()],
            skills: vec![
                AgentSkill {
                    id: "chat".to_string(),
                    name: "chat".to_string(),
                    description: "Multi-turn conversation".to_string(),
                    tags: vec!["conversation".to_string()],
                    examples: vec![
                        "Summarize this thread".to_string(),
                        "What did we decide yesterday?".to_string(),
                    ],
                },
                AgentSkill {
                    id: "channel_messaging".to_string(),
                    name: "channel_messaging".to_string(),
                    description: "Telegram/LINE/Discord messaging".to_string(),
                    tags: vec!["messaging".to_string()],
                    examples: vec!["Post the release notes to Discord".to_string()],
                },
                AgentSkill {
                    id: "memory".to_string(),
                    name: "memory".to_string(),
                    description: "Search and store memories".to_string(),
                    tags: vec!["memory".to_string()],
                    examples: vec!["What do you know about project X?".to_string()],
                },
            ],
            provider: AgentProvider {
                organization: "DuDuClaw".to_string(),
                url: "https://duduclaw.ai".to_string(),
            },
            extensions: AgentCardExtensions {
                x_duduclaw: XDuduclawExtension {
                    adr: "ADR-002".to_string(),
                    version: crate::mcp_headers::API_VERSION.to_string(),
                    capabilities: crate::mcp_headers::build_capabilities_header(),
                    negotiation_header: "x-duduclaw-capabilities".to_string(),
                },
            },
        }
    }

    /// The default DuDuClaw Agent Card (identity used by `agent/discover` and
    /// the `.well-known` endpoints).
    pub fn default_agent_card() -> AgentCard {
        Self::generate_agent_card(
            "DuDuClaw Agent",
            "Multi-Runtime AI Agent Platform with channel routing, memory, and self-evolution",
            "stdio://duduclaw-acp",
        )
    }
}

/// Resolve a `.well-known` request path to the default Agent Card.
///
/// Both the A2A v1.0 path ([`WELL_KNOWN_AGENT_CARD_PATH`]) and the legacy alias
/// ([`WELL_KNOWN_AGENT_CARD_PATH_LEGACY`]) resolve to the same card; any other
/// path returns `None` so an HTTP layer can respond `404`.
#[allow(dead_code)]
pub fn resolve_well_known_card(path: &str) -> Option<AgentCard> {
    if path == WELL_KNOWN_AGENT_CARD_PATH || path == WELL_KNOWN_AGENT_CARD_PATH_LEGACY {
        Some(AcpServer::default_agent_card())
    } else {
        None
    }
}

// ── JSON-RPC helpers ────────────────────────────────────────

pub(crate) fn jsonrpc_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

pub(crate) fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

async fn write_response(stdout: &mut tokio::io::Stdout, response: &Value) -> Result<()> {
    let mut output = serde_json::to_string(response)
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to serialize response: {e}")))?;
    output.push('\n');
    stdout
        .write_all(output.as_bytes())
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to write to stdout: {e}")))?;
    stdout
        .flush()
        .await
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to flush stdout: {e}")))?;
    Ok(())
}

// ── Method handlers ─────────────────────────────────────────

/// Handle `agent/discover` — returns the A2A v1.0 agent card.
pub(crate) fn handle_agent_discover(id: &Value) -> Value {
    let card = AcpServer::default_agent_card();
    jsonrpc_response(id, serde_json::to_value(card).unwrap_or(Value::Null))
}

/// Handle methods that are A2A-spec'd but deliberately unsupported —
/// `message/stream`, `tasks/resubscribe`, and push-notification config. The
/// agent card advertises `streaming: false` / `pushNotifications: false`, and
/// these return the spec-shaped `UnsupportedOperationError` (`-32004`) so
/// clients get a machine-parseable answer instead of a bare method-not-found.
pub(crate) fn handle_unsupported_operation(id: &Value, method: &str) -> Value {
    jsonrpc_error(
        id,
        -32004,
        &format!(
            "{method} is not supported (see agent card capabilities); use message/send + tasks/get"
        ),
    )
}

/// Handle `tasks/send` — creates a task, runs the prompt, returns result.
pub(crate) async fn handle_tasks_send(
    id: &Value,
    params: &Value,
    task_manager: &mut A2ATaskManager,
    home_dir: &std::path::Path,
) -> Value {
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

    // RM-137 H3: under `none`, only the default (Main-role) agent — checked
    // before anything runs. Other policies keep today's behaviour.
    if duduclaw_core::delegation_policy_from_home(home_dir) == duduclaw_core::DelegationPolicy::Disabled {
        let target = match super::handlers::resolve_send_target(home_dir, context_id).await {
            Ok(t) => t,
            Err((code, msg)) => return jsonrpc_error(id, code, &msg),
        };
        if let Err((code, msg)) =
            super::handlers::check_acp_target_under_none(home_dir, &target).await
        {
            return jsonrpc_error(id, code, &msg);
        }
    }

    // Create a task in the manager
    let task = task_manager.create_task(context_id, message);
    let task_id = task.id.clone();

    // Run the prompt through the agent handler. `context_id` is the A2A target
    // agent (RFC-25 Phase 3); "default" resolves to the main agent.
    let updates = handle_prompt_with_agent(home_dir, context_id, &task_id, message, model).await;

    // Extract the final response from updates.
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

    // Mark the task Completed or Failed based on whether execution errored, so
    // a remote A2A peer can tell success from failure (RFC-25 Phase 3 audit fix).
    let errored = updates
        .iter()
        .any(|u| matches!(u, super::types::SessionUpdate::Error { .. }));
    if errored {
        task_manager.fail_task(&task_id, final_message.clone());
    } else {
        task_manager.complete_task(&task_id, final_message.clone());
    }

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
///
/// Two task populations are served:
/// - **`tasks/send` tasks** (in-memory `A2ATaskManager`) — legacy
///   `{ "task": … }` envelope, unchanged.
/// - **`message/send` bus tasks** ([`BusTaskIndex`]) — spec-shaped A2A `Task`
///   with a best-effort state probed from `bus_queue.jsonl` (mapping table
///   documented in [`super::message_send`]).
///
/// Accepts the A2A v1.0 param name `id` as well as the legacy `task_id`.
pub(crate) async fn handle_tasks_get(
    id: &Value,
    params: &Value,
    task_manager: &A2ATaskManager,
    bus_index: &BusTaskIndex,
    home_dir: &std::path::Path,
) -> Value {
    let task_id = match params
        .get("task_id")
        .or_else(|| params.get("id"))
        .and_then(|v| v.as_str())
    {
        Some(tid) => tid,
        None => {
            return jsonrpc_error(id, -32602, "Missing required parameter: task_id (or id)");
        }
    };

    if let Some(task) = task_manager.get_task(task_id) {
        let task_json = serde_json::to_value(task).unwrap_or(Value::Null);
        return jsonrpc_response(id, serde_json::json!({ "task": task_json }));
    }

    if let Some(record) = bus_index.get(task_id) {
        return super::message_send::handle_bus_task_get(id, task_id, record, home_dir).await;
    }

    jsonrpc_error(id, -32001, &format!("Task not found: {task_id}"))
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

// ── Startup refusal (RM-137 H3) ─────────────────────────────

/// `duduclaw acp server` on the Bash lane: refused for AI employees (and
/// unverified callers) like every other operator-only command. Messages the
/// server enqueues are stamped `a2a-client`, which `[acp] trusted = true`
/// treats as a system sender, so an employee that could start its own server
/// would get past `[delegation] policy = "none"`.
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] = &[
    duduclaw_core::OperatorCommand { name: "duduclaw acp server", path: &["acp", "server"] },
    duduclaw_core::OperatorCommand {
        name: "duduclaw acp server",
        path: &["maintenance", "acp", "server"],
    },
];

/// The employee-identity variables that refuse `duduclaw acp server` on
/// their own (also members of `ai_session_guard::AI_SESSION_ENV_VARS`; listed
/// here so the refusal does not depend on that list keeping them).
pub(crate) const ACP_SERVER_IDENTITY_ENV_VARS: &[&str] = &[
    duduclaw_core::ENV_AGENT_ID,
    duduclaw_core::ENV_AGENT_TOKEN,
    duduclaw_core::ENV_HOOK_TOKEN,
];

/// Why `duduclaw acp server` must not start here, if it must not:
/// `session_markers` are the AI-session markers present
/// (`ai_session_guard::command_markers`), `identity_present` the
/// [`ACP_SERVER_IDENTITY_ENV_VARS`] present (even empty).
pub(crate) fn acp_server_start_refusal(
    session_markers: &[&str],
    identity_present: &[&str],
) -> Option<String> {
    if session_markers.is_empty() && identity_present.is_empty() {
        return None;
    }
    let mut found: Vec<&str> = session_markers.to_vec();
    for k in identity_present {
        if !found.contains(k) {
            found.push(k);
        }
    }
    Some(format!(
        "duduclaw acp server 只能由本機操作人員啟動,不能在 AI 員工的工作階段中啟動\
         (偵測到環境變數:{})。ACP 伺服器送出的工作以 a2a-client 身分排入佇列,\
         在 [acp] trusted = true 時等同系統寄件者。\
         / `duduclaw acp server` cannot be started from an AI employee session \
         (found: {}); start it from an operator terminal.",
        found.join(", "),
        found.join(", ")
    ))
}

fn identity_env_present() -> Vec<&'static str> {
    #[cfg(not(test))]
    {
        ACP_SERVER_IDENTITY_ENV_VARS
            .iter()
            .copied()
            .filter(|k| std::env::var_os(k).is_some())
            .collect()
    }
    #[cfg(test)]
    {
        // Same isolated source as `command_markers()` in tests.
        crate::ai_session_guard::command_markers()
            .into_iter()
            .filter(|k| ACP_SERVER_IDENTITY_ENV_VARS.contains(k))
            .collect()
    }
}

// ── Main server loop ────────────────────────────────────────

/// Run the ACP server, reading JSON-RPC 2.0 from stdin and writing responses to stdout.
///
/// Supported methods:
/// - `agent/discover` — returns the A2A v1.0 agent card
/// - `message/send` — A2A v1.0 primary submission RPC; enqueues a bus task for
///   the target agent and returns `Task { state: "submitted" }` (async
///   execution via the gateway AgentDispatcher)
/// - `tasks/send` — creates and executes a task inline, returns result with artifacts
/// - `tasks/get` — retrieves task status by ID (in-memory tasks + best-effort
///   bus-task probe)
/// - `tasks/cancel` — cancels a task by ID
/// - `message/stream` / `tasks/resubscribe` / `tasks/pushNotificationConfig/*`
///   — spec-shaped `UnsupportedOperationError` (-32004)
pub async fn run_acp_server(home_dir: &Path) -> Result<()> {
    // RM-137 H3: an AI employee must not run its own ACP server. Checked
    // before stdin is read, so nothing is ever enqueued.
    if let Some(reason) = acp_server_start_refusal(
        &crate::ai_session_guard::command_markers(),
        &identity_env_present(),
    ) {
        return Err(DuDuClawError::Config(reason));
    }
    info!("Starting DuDuClaw ACP server (A2A protocol over stdio)");

    let mut task_manager = A2ATaskManager::new();
    let mut bus_index = BusTaskIndex::default();

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader
            .read_line(&mut line)
            .await
            .map_err(|e| DuDuClawError::Gateway(format!("Failed to read from stdin: {e}")))?;

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
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        let response = match method {
            "agent/discover" => handle_agent_discover(&id),
            "message/send" => {
                super::message_send::handle_message_send(&id, &params, home_dir, &mut bus_index)
                    .await
            }
            "message/stream"
            | "tasks/resubscribe"
            | "tasks/pushNotificationConfig/set"
            | "tasks/pushNotificationConfig/get" => handle_unsupported_operation(&id, method),
            "tasks/send" => handle_tasks_send(&id, &params, &mut task_manager, home_dir).await,
            "tasks/get" => {
                handle_tasks_get(&id, &params, &task_manager, &bus_index, home_dir).await
            }
            "tasks/cancel" => handle_tasks_cancel(&id, &params, &mut task_manager),
            _ => jsonrpc_error(&id, -32601, &format!("Method not found: {method}")),
        };

        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}
