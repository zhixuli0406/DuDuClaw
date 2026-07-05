//! A2A v1.0 `message/send` — real implementation over DuDuClaw's file-based
//! IPC bus (`<home>/bus_queue.jsonl`, consumed by the gateway AgentDispatcher).
//!
//! ## Execution model (honest-async)
//!
//! `message/send` does NOT execute the agent inline. It validates the A2A
//! `Message`, resolves the target agent, and appends an `agent_message` record
//! to `bus_queue.jsonl` — the same queue the MCP `spawn_agent` / delegation
//! path uses. The gateway `AgentDispatcher` polls that queue every ~5s and
//! drives the target agent. The truthful response is therefore an A2A `Task`
//! in state `"submitted"`; we never fake `"completed"`.
//!
//! ## Bus task schema
//!
//! Field-for-field mirror of the `BusMessage` envelope the dispatcher
//! deserializes (`duduclaw-gateway/src/dispatcher.rs`):
//!
//! ```json
//! {
//!   "type": "agent_message",
//!   "message_id": "<uuid — doubles as the A2A task id>",
//!   "agent_id": "<resolved target agent>",
//!   "payload": "<concatenated text parts>",
//!   "timestamp": "<RFC 3339>",
//!   "delegation_depth": 0,
//!   "origin_agent": "a2a-client",
//!   "sender_agent": "a2a-client"
//! }
//! ```
//!
//! Optional envelope fields the dispatcher treats as absent (`response`,
//! `in_reply_to`, `coalesced_ids`, `turn_id`, `session_id`) are omitted, the
//! same way the dispatcher's own serializer skips them.
//!
//! ## `tasks/get` state mapping (best-effort probe)
//!
//! The dispatcher *removes* `agent_message` lines from the queue when it picks
//! them up, and appends an `agent_response` line (`in_reply_to` = original
//! `message_id`) when the agent finishes. Responses may later be reconciled /
//! forwarded to channels and the queue rotated, so absence is ambiguous. The
//! honest mapping is:
//!
//! | Observation in `bus_queue.jsonl`                          | A2A state     |
//! |-----------------------------------------------------------|---------------|
//! | `agent_message` with `message_id == id` still queued      | `submitted`   |
//! | `agent_response` with `in_reply_to == id`                 | `completed` (payload returned as a text artifact) |
//! | neither (line consumed; no response line visible)         | `working` (metadata note: result may already have been delivered via channels or the queue rotated — completion is not observable from here) |
//!
//! Caveat: the dispatcher writes error text into the same `agent_response`
//! shape with no structured failure flag, so a failed run also surfaces as
//! `completed` with the error text as the artifact. This is documented rather
//! than guessed at (no fragile `starts_with("Error:")` heuristics).
//!
//! Task ids are tracked in an in-process [`BusTaskIndex`]; ids from a previous
//! ACP server process return `TaskNotFoundError` (-32001).

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use tracing::warn;

use super::server::{jsonrpc_error, jsonrpc_response};

/// Maximum accepted total text size for one message (bytes). Kept well under
/// the dispatcher's own 100 KB payload drop threshold (BE-H6) so an accepted
/// message is never silently discarded downstream.
pub(crate) const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;

/// `bus_queue.jsonl` size cap — mirrors the MCP `spawn_agent` writer (CLI-H4).
const MAX_QUEUE_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Sender/origin identity recorded on bus tasks submitted via the A2A surface.
/// Lowercase-alphanumeric-with-hyphens so it passes `is_valid_agent_id`-style
/// filters applied to envelope fields elsewhere.
pub(crate) const A2A_SENDER: &str = "a2a-client";

// ── Params parsing ──────────────────────────────────────────

/// Validated `message/send` params.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParsedSendMessage {
    /// Concatenated text parts (joined with `\n`).
    pub text: String,
    /// `message.contextId` if the client provided one.
    pub context_id: Option<String>,
    /// Client-supplied `message.messageId` (echoed back in metadata).
    pub client_message_id: Option<String>,
    /// Number of non-text parts ignored (surfaced in response metadata).
    pub skipped_parts: usize,
    /// Whether `configuration.blocking = true` was requested (unsupported —
    /// noted in metadata; the response is still `submitted`).
    pub blocking_requested: bool,
}

/// Parse/validation failure → spec-shaped JSON-RPC error.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SendParamError {
    /// `-32602` InvalidParams.
    Invalid(String),
    /// `-32004` A2A UnsupportedOperationError.
    Unsupported(String),
}

impl SendParamError {
    pub(crate) fn to_jsonrpc(&self, id: &Value) -> Value {
        match self {
            SendParamError::Invalid(msg) => jsonrpc_error(id, -32602, msg),
            SendParamError::Unsupported(msg) => jsonrpc_error(id, -32004, msg),
        }
    }
}

/// Parse the A2A v1.0 `message/send` params shape:
/// `{ message: { role, parts: [{kind:"text", text}...], messageId?, taskId?,
///   contextId? }, configuration? }`.
///
/// - Non-text parts are skipped with a warn and counted.
/// - Empty/whitespace-only text → `Invalid` (-32602).
/// - Total text > [`MAX_MESSAGE_TEXT_BYTES`] → `Invalid` (-32602). Byte length
///   only — no slicing, so no UTF-8 boundary hazard.
/// - `message.taskId` (multi-turn task continuation) → `Unsupported` (-32004);
///   v1 has no way to append input to an already-dispatched bus task.
pub(crate) fn parse_message_send_params(
    params: &Value,
) -> Result<ParsedSendMessage, SendParamError> {
    let message = match params.get("message") {
        Some(m) if m.is_object() => m,
        Some(_) => {
            return Err(SendParamError::Invalid(
                "'message' must be an A2A Message object".to_string(),
            ));
        }
        None => {
            return Err(SendParamError::Invalid(
                "Missing required parameter: message".to_string(),
            ));
        }
    };

    if message.get("taskId").and_then(|v| v.as_str()).is_some() {
        return Err(SendParamError::Unsupported(
            "message.taskId (task continuation) is not supported: DuDuClaw bus tasks are \
             single-shot; send a new message without taskId"
                .to_string(),
        ));
    }

    let parts = match message.get("parts").and_then(|p| p.as_array()) {
        Some(p) if !p.is_empty() => p,
        _ => {
            return Err(SendParamError::Invalid(
                "message.parts must be a non-empty array".to_string(),
            ));
        }
    };

    let mut texts: Vec<&str> = Vec::new();
    let mut skipped_parts = 0usize;
    for part in parts {
        // A2A v1.0 uses `kind`; tolerate the older draft `type` spelling.
        let kind = part
            .get("kind")
            .or_else(|| part.get("type"))
            .and_then(|k| k.as_str())
            .unwrap_or("");
        if kind == "text"
            && let Some(text) = part.get("text").and_then(|t| t.as_str())
        {
            texts.push(text);
            continue;
        }
        warn!(part_kind = %kind, "message/send: ignoring non-text part");
        skipped_parts += 1;
    }

    // Size check on byte lengths BEFORE joining — avoids allocating an
    // oversized buffer just to reject it. Separator bytes counted too.
    let total_bytes: usize =
        texts.iter().map(|t| t.len()).sum::<usize>() + texts.len().saturating_sub(1);
    if total_bytes > MAX_MESSAGE_TEXT_BYTES {
        return Err(SendParamError::Invalid(format!(
            "message text too large: {total_bytes} bytes (limit {MAX_MESSAGE_TEXT_BYTES})"
        )));
    }

    let text = texts.join("\n");
    if text.trim().is_empty() {
        return Err(SendParamError::Invalid(
            "message contains no non-empty text parts".to_string(),
        ));
    }

    let context_id = message
        .get("contextId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let client_message_id = message
        .get("messageId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let blocking_requested = params
        .get("configuration")
        .and_then(|c| c.get("blocking"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);

    Ok(ParsedSendMessage {
        text,
        context_id,
        client_message_id,
        skipped_parts,
        blocking_requested,
    })
}

// ── Bus enqueue ─────────────────────────────────────────────

/// Build the exact `agent_message` envelope the gateway AgentDispatcher
/// consumes (see module docs for the field-for-field schema).
pub(crate) fn build_bus_task_json(
    message_id: &str,
    agent_id: &str,
    payload: &str,
    timestamp: &str,
) -> Value {
    serde_json::json!({
        "type": "agent_message",
        "message_id": message_id,
        "agent_id": agent_id,
        "payload": payload,
        "timestamp": timestamp,
        "delegation_depth": 0,
        "origin_agent": A2A_SENDER,
        "sender_agent": A2A_SENDER,
    })
}

/// Append one line to `<home>/bus_queue.jsonl` under the mandatory advisory
/// lock (coding convention #3: cross-process JSONL appends must hold
/// `duduclaw_core::with_file_lock`). Fail-closed: any lock/open/write error
/// propagates and no partial line is left behind (single `writeln!` inside
/// the lock).
pub(crate) fn append_bus_task_sync(home_dir: &Path, line: &str) -> std::io::Result<()> {
    let queue_path = home_dir.join("bus_queue.jsonl");
    duduclaw_core::with_file_lock(&queue_path, || {
        if let Ok(meta) = std::fs::metadata(&queue_path)
            && meta.len() > MAX_QUEUE_FILE_SIZE
        {
            return Err(std::io::Error::other(format!(
                "bus_queue.jsonl exceeds {}MB size limit (current: {} bytes); \
                 run `duduclaw bus rotate` or wait for the dispatcher to drain",
                MAX_QUEUE_FILE_SIZE / (1024 * 1024),
                meta.len()
            )));
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&queue_path)?;
        writeln!(f, "{line}")
    })
}

// ── In-process task index ───────────────────────────────────

/// Tracks bus tasks submitted via `message/send` in this server process, so
/// `tasks/get` can distinguish "known task, probe the queue" from "unknown id
/// → TaskNotFoundError". Lost on restart (documented in module docs).
#[derive(Default)]
pub(crate) struct BusTaskIndex {
    tasks: HashMap<String, BusTaskRecord>,
}

/// Metadata retained per submitted bus task.
pub(crate) struct BusTaskRecord {
    pub context_id: String,
}

impl BusTaskIndex {
    pub(crate) fn insert(&mut self, task_id: String, context_id: String) {
        self.tasks.insert(task_id, BusTaskRecord { context_id });
    }

    pub(crate) fn get(&self, task_id: &str) -> Option<&BusTaskRecord> {
        self.tasks.get(task_id)
    }
}

// ── Queue probing (tasks/get) ───────────────────────────────

/// What a scan of `bus_queue.jsonl` reveals about a task id.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BusProbe {
    /// The `agent_message` line is still queued — dispatcher hasn't picked it up.
    Queued,
    /// An `agent_response` with `in_reply_to == id` exists; carries its payload.
    Responded(String),
    /// Neither line visible — consumed/in-flight/rotated (ambiguous).
    Unknown,
}

/// Pure scan of queue-file content for the given task id. See the module-level
/// mapping table for how callers translate this into A2A task states.
pub(crate) fn probe_bus_task_state(queue_content: &str, task_id: &str) -> BusProbe {
    let mut queued = false;
    for line in queue_content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // tolerate a corrupt line; other lines still count
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("agent_response")
                if v.get("in_reply_to").and_then(|x| x.as_str()) == Some(task_id) =>
            {
                let payload = v
                    .get("payload")
                    .and_then(|p| p.as_str())
                    .unwrap_or_default()
                    .to_string();
                return BusProbe::Responded(payload);
            }
            Some("agent_message")
                if v.get("message_id").and_then(|x| x.as_str()) == Some(task_id) =>
            {
                queued = true;
            }
            _ => {}
        }
    }
    if queued { BusProbe::Queued } else { BusProbe::Unknown }
}

// ── Handlers ────────────────────────────────────────────────

/// Handle A2A v1.0 `message/send`: validate → resolve target agent → enqueue
/// bus task → return a `Task { state: "submitted" }` envelope.
///
/// **Target-agent resolution**: `run_acp_server` is instance-global (started
/// with only the DuDuClaw home, no per-agent identity), so the card represents
/// the *installation*. Following the RFC-25 convention already used by
/// `tasks/send`, `message.contextId` names the target agent; absent/`"default"`
/// resolves to the registry's Main-role agent (the team root).
pub(crate) async fn handle_message_send(
    id: &Value,
    params: &Value,
    home_dir: &Path,
    index: &mut BusTaskIndex,
) -> Value {
    let parsed = match parse_message_send_params(params) {
        Ok(p) => p,
        Err(e) => return e.to_jsonrpc(id),
    };

    let context_id = parsed
        .context_id
        .clone()
        .unwrap_or_else(|| "default".to_string());

    let target_agent = match super::handlers::resolve_send_target(home_dir, &context_id).await {
        Ok(agent) => agent,
        Err((code, msg)) => return jsonrpc_error(id, code, &msg),
    };

    enqueue_and_respond(id, &parsed, &context_id, &target_agent, home_dir, index).await
}

/// Enqueue the bus task and build the spec-shaped `Task` response. Split from
/// [`handle_message_send`] so tests can exercise the write path + response
/// shape without a populated agent registry.
pub(crate) async fn enqueue_and_respond(
    id: &Value,
    parsed: &ParsedSendMessage,
    context_id: &str,
    target_agent: &str,
    home_dir: &Path,
    index: &mut BusTaskIndex,
) -> Value {
    let task_id = uuid::Uuid::new_v4().to_string();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let entry = build_bus_task_json(&task_id, target_agent, &parsed.text, &timestamp);
    let line = entry.to_string();

    let home = home_dir.to_path_buf();
    let appended: std::io::Result<()> = tokio::task::spawn_blocking(move || {
        append_bus_task_sync(&home, &line)
    })
    .await
    .map_err(|e| std::io::Error::other(format!("bus append task panicked: {e}")))
    .and_then(|r| r);

    if let Err(e) = appended {
        // Fail-closed: nothing was durably enqueued → JSON-RPC internal error.
        return jsonrpc_error(id, -32603, &format!("failed to enqueue bus task: {e}"));
    }

    index.insert(task_id.clone(), context_id.to_string());

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "targetAgent".to_string(),
        Value::String(target_agent.to_string()),
    );
    metadata.insert(
        "transport".to_string(),
        Value::String("duduclaw-bus".to_string()),
    );
    metadata.insert(
        "note".to_string(),
        Value::String(
            "Execution is asynchronous: the DuDuClaw AgentDispatcher consumes the bus queue. \
             Poll tasks/get for best-effort status; final replies may be delivered via the \
             agent's configured channels."
                .to_string(),
        ),
    );
    if parsed.skipped_parts > 0 {
        metadata.insert(
            "skippedNonTextParts".to_string(),
            Value::from(parsed.skipped_parts),
        );
    }
    if let Some(ref mid) = parsed.client_message_id {
        metadata.insert("clientMessageId".to_string(), Value::String(mid.clone()));
    }
    if parsed.blocking_requested {
        metadata.insert(
            "blockingUnsupported".to_string(),
            Value::String(
                "configuration.blocking=true was requested but is not supported; \
                 the task was submitted asynchronously"
                    .to_string(),
            ),
        );
    }

    jsonrpc_response(
        id,
        serde_json::json!({
            "id": task_id,
            "contextId": context_id,
            "status": {
                "state": "submitted",
                "timestamp": timestamp,
            },
            "kind": "task",
            "metadata": Value::Object(metadata),
        }),
    )
}

/// Best-effort `tasks/get` for a bus task submitted via `message/send`.
/// Applies the mapping table from the module docs.
pub(crate) async fn handle_bus_task_get(
    id: &Value,
    task_id: &str,
    record: &BusTaskRecord,
    home_dir: &Path,
) -> Value {
    let queue_path = home_dir.join("bus_queue.jsonl");
    let content = tokio::fs::read_to_string(&queue_path)
        .await
        .unwrap_or_default();

    let timestamp = chrono::Utc::now().to_rfc3339();
    let base_metadata = |note: &str| {
        serde_json::json!({
            "transport": "duduclaw-bus",
            "note": note,
        })
    };

    let task = match probe_bus_task_state(&content, task_id) {
        BusProbe::Queued => serde_json::json!({
            "id": task_id,
            "contextId": record.context_id,
            "status": { "state": "submitted", "timestamp": timestamp },
            "kind": "task",
            "metadata": base_metadata("agent_message still queued; dispatcher has not picked it up yet"),
        }),
        BusProbe::Responded(text) => serde_json::json!({
            "id": task_id,
            "contextId": record.context_id,
            "status": { "state": "completed", "timestamp": timestamp },
            "kind": "task",
            "artifacts": [{
                "artifactId": format!("{task_id}-response"),
                "name": "response",
                "parts": [{ "kind": "text", "text": text }],
            }],
            "metadata": base_metadata(
                "agent_response found on the bus; note the dispatcher writes error text into \
                 the same envelope, so inspect the artifact text for failures",
            ),
        }),
        BusProbe::Unknown => serde_json::json!({
            "id": task_id,
            "contextId": record.context_id,
            "status": { "state": "working", "timestamp": timestamp },
            "kind": "task",
            "metadata": base_metadata(
                "bus line consumed by the dispatcher; completion is not observable from the \
                 queue — DuDuClaw delegation results are delivered via the agent's channels",
            ),
        }),
    };

    jsonrpc_response(id, task)
}
