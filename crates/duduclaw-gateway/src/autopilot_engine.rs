//! Autopilot trigger engine — evaluates rules against events and executes actions.
//!
//! The `AutopilotEngine` subscribes to a `broadcast::Receiver<AutopilotEvent>`
//! fed by two sources:
//!   1. In-process: WebSocket handlers call `sender.send(...)` when the
//!      dashboard RPC mutates a task / activity.
//!   2. Out-of-process: the MCP subprocess appends events to
//!      `~/.duduclaw/events.jsonl`, and a tail task parses + re-emits
//!      them onto the same broadcast bus.
//!
//! Each received event is matched against enabled rules by `trigger_event`,
//! conditions are evaluated against a flat field map, and the rule's
//! `action` is dispatched (`delegate` / `notify` / `run_skill`). Every
//! execution appends to `autopilot_history` with `success` / `failure`
//! so the dashboard History tab is populated.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::autopilot_store::{AutopilotHistoryRow, AutopilotStore};
use crate::config_crypto::read_encrypted_config_field;
use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};
use crate::task_store::{ActivityRow, TaskStore};

// ─── Event types ────────────────────────────────────────────

/// Strongly-typed events the AutopilotEngine can react to.
///
/// `trigger_event` strings in `autopilot_rules.trigger_event` match these
/// variants via `event_name()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutopilotEvent {
    TaskCreated { task: Value },
    TaskUpdated { task: Value },
    TaskStatusChanged {
        task_id: String,
        from: String,
        to: String,
        task: Value,
    },
    ActivityNew { activity: Value },
    ChannelMessage {
        channel: String,
        agent_id: String,
        text: String,
    },
    AgentIdle {
        agent_id: String,
        idle_minutes: i64,
    },
    CronTick { now: String },
}

impl AutopilotEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::TaskCreated { .. } => "task_created",
            Self::TaskUpdated { .. } => "task_updated",
            Self::TaskStatusChanged { .. } => "task_status_changed",
            Self::ActivityNew { .. } => "activity_new",
            Self::ChannelMessage { .. } => "channel_message",
            Self::AgentIdle { .. } => "agent_idle",
            Self::CronTick { .. } => "cron_tick",
        }
    }

    /// Flatten the event into a top-level field map for condition evaluation.
    pub fn to_fields(&self) -> serde_json::Map<String, Value> {
        let mut map = serde_json::Map::new();
        map.insert("event".into(), Value::String(self.event_name().into()));
        match self {
            Self::TaskCreated { task } | Self::TaskUpdated { task } => {
                map.insert("task".into(), task.clone());
            }
            Self::TaskStatusChanged { task_id, from, to, task } => {
                map.insert("task_id".into(), Value::String(task_id.clone()));
                map.insert("from".into(), Value::String(from.clone()));
                map.insert("to".into(), Value::String(to.clone()));
                map.insert("task".into(), task.clone());
            }
            Self::ActivityNew { activity } => {
                map.insert("activity".into(), activity.clone());
            }
            Self::ChannelMessage { channel, agent_id, text } => {
                map.insert("channel".into(), Value::String(channel.clone()));
                map.insert("agent_id".into(), Value::String(agent_id.clone()));
                map.insert("text".into(), Value::String(text.clone()));
            }
            Self::AgentIdle { agent_id, idle_minutes } => {
                map.insert("agent_id".into(), Value::String(agent_id.clone()));
                map.insert(
                    "idle_minutes".into(),
                    Value::Number((*idle_minutes).into()),
                );
            }
            Self::CronTick { now } => {
                map.insert("now".into(), Value::String(now.clone()));
            }
        }
        map
    }
}

// ─── Condition evaluator ────────────────────────────────────

/// Evaluate a rule's conditions JSON against a flat field map.
///
/// Supported shapes:
///   `{ "all": [ cond, cond, ... ] }`
///   `{ "any": [ cond, cond, ... ] }`
///   `{ "field": "task.priority", "op": "in", "value": ["high","urgent"] }`
/// `null` or missing conditions means "always true".
pub fn evaluate(conditions: &Value, fields: &serde_json::Map<String, Value>) -> bool {
    if conditions.is_null() {
        return true;
    }
    if let Some(all) = conditions.get("all").and_then(|v| v.as_array()) {
        return all.iter().all(|c| evaluate(c, fields));
    }
    if let Some(any) = conditions.get("any").and_then(|v| v.as_array()) {
        return any.iter().any(|c| evaluate(c, fields));
    }
    let field = conditions
        .get("field")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let op = conditions.get("op").and_then(|v| v.as_str()).unwrap_or("eq");
    let expected = conditions.get("value").cloned().unwrap_or(Value::Null);
    let actual = lookup_path_opt(fields, field);
    apply_op(op, actual.as_ref(), &expected)
}

/// Walk a dotted path and return `Some(value)` only when every segment
/// exists. Returns `None` when any segment is missing — callers MUST
/// distinguish "field absent" from "field set to null", because allowing
/// `eq null` to match an absent field caused 5/5 autopilot mass-fire bug
/// (RFC-22 P1-9b).
fn lookup_path_opt(
    fields: &serde_json::Map<String, Value>,
    path: &str,
) -> Option<Value> {
    if path.is_empty() {
        return None;
    }
    let mut parts = path.split('.');
    let head = parts.next()?;
    let mut current = fields.get(head).cloned()?;
    for part in parts {
        current = current.get(part).cloned()?;
    }
    Some(current)
}

/// Backward-compatible wrapper used by `render_template`. Missing fields
/// render to empty string (existing behavior covered by `render_unknown_key_is_empty`),
/// but condition evaluation uses `lookup_path_opt` directly so a missing
/// field never spuriously matches `eq null` / `eq ""`.
fn lookup_path(fields: &serde_json::Map<String, Value>, path: &str) -> Value {
    lookup_path_opt(fields, path).unwrap_or(Value::Null)
}

fn apply_op(op: &str, actual: Option<&Value>, expected: &Value) -> bool {
    // Missing fields never satisfy any comparison — including `eq null`.
    // Callers wanting "field is absent" semantics should use a dedicated
    // op (none currently defined). See RFC-22 P1-9b for the original bug.
    let Some(actual) = actual else {
        return false;
    };
    match op {
        "eq" => actual == expected,
        "neq" => actual != expected,
        "in" => expected
            .as_array()
            .map(|arr| arr.iter().any(|v| v == actual))
            .unwrap_or(false),
        "not_in" => expected
            .as_array()
            .map(|arr| !arr.iter().any(|v| v == actual))
            .unwrap_or(true),
        "gt" => number_pair(actual, expected).map(|(a, e)| a > e).unwrap_or(false),
        "gte" => number_pair(actual, expected).map(|(a, e)| a >= e).unwrap_or(false),
        "lt" => number_pair(actual, expected).map(|(a, e)| a < e).unwrap_or(false),
        "lte" => number_pair(actual, expected).map(|(a, e)| a <= e).unwrap_or(false),
        "contains" => match (actual, expected) {
            (Value::String(a), Value::String(e)) => a.contains(e.as_str()),
            (Value::Array(a), _) => a.iter().any(|v| v == expected),
            _ => false,
        },
        _ => false,
    }
}

fn number_pair(a: &Value, b: &Value) -> Option<(f64, f64)> {
    let to_f = |v: &Value| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64));
    Some((to_f(a)?, to_f(b)?))
}

// ─── Template rendering ─────────────────────────────────────

/// Very simple `{field.subfield}` interpolation.
///
/// Unknown keys resolve to empty string. `{ }` with no valid closing brace
/// are left alone. Intended for action templates like
/// `"🚨 Urgent task: {task.title}"`.
pub fn render_template(
    template: &str,
    fields: &serde_json::Map<String, Value>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        match after_open.find('}') {
            Some(close) => {
                let key = &after_open[..close];
                let v = lookup_path(fields, key);
                let s = match v {
                    Value::String(s) => s,
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                out.push_str(&s);
                rest = &after_open[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

// ─── Engine ─────────────────────────────────────────────────

/// Maximum number of times a single rule can fire within `CIRCUIT_WINDOW`
/// before the breaker trips Open.
const CIRCUIT_MAX_FIRES: usize = 10;
/// Sliding window for counting fires while Closed.
const CIRCUIT_WINDOW: Duration = Duration::from_secs(60);
/// Duration the breaker stays Open before transitioning to HalfOpen.
const CIRCUIT_OPEN_COOLDOWN: Duration = Duration::from_secs(60);
/// Window of observation in HalfOpen. If a fire occurs within this
/// window without re-tripping, the breaker returns to Closed.
const CIRCUIT_HALF_OPEN_PROBE_WINDOW: Duration = Duration::from_secs(30);

/// Three-state circuit breaker state per rule.
///
/// Transitions:
///   `Closed` → (>= CIRCUIT_MAX_FIRES in CIRCUIT_WINDOW) → `Open`
///   `Open` → (after CIRCUIT_OPEN_COOLDOWN) → `HalfOpen`
///   `HalfOpen` → (one probe fire succeeds and no immediate retrip) → `Closed`
///   `HalfOpen` → (any fire within probe window that would trip) → `Open`
#[derive(Debug, Clone)]
enum CircuitState {
    /// Normal operation. `fires` tracks timestamps within the sliding window.
    Closed { fires: Vec<std::time::Instant> },
    /// Tripped; all requests blocked until `opened_at + CIRCUIT_OPEN_COOLDOWN`.
    Open { opened_at: std::time::Instant },
    /// Probe state after cooldown. One probe was allowed at `probed_at`;
    /// subsequent fires within `CIRCUIT_HALF_OPEN_PROBE_WINDOW` re-trip.
    HalfOpen { probed_at: std::time::Instant },
}

impl CircuitState {
    fn new_closed() -> Self {
        Self::Closed { fires: Vec::new() }
    }
}

pub struct AutopilotEngine {
    home_dir: PathBuf,
    store: Arc<AutopilotStore>,
    task_store: Arc<TaskStore>,
    message_queue: Option<Arc<MessageQueue>>,
    event_rx: broadcast::Receiver<AutopilotEvent>,
    /// Per-rule circuit breaker state.
    /// Mutex never held across `.await` (pure in-memory map operations).
    circuit: tokio::sync::Mutex<std::collections::HashMap<String, CircuitState>>,
}

impl AutopilotEngine {
    pub fn new(
        home_dir: PathBuf,
        store: Arc<AutopilotStore>,
        task_store: Arc<TaskStore>,
        message_queue: Option<Arc<MessageQueue>>,
        event_rx: broadcast::Receiver<AutopilotEvent>,
    ) -> Self {
        Self {
            home_dir,
            store,
            task_store,
            message_queue,
            event_rx,
            circuit: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Three-state circuit breaker.
    ///
    /// - **Closed**: normal. Allow, record fire in sliding window. If the
    ///   count hits `CIRCUIT_MAX_FIRES` within `CIRCUIT_WINDOW`, trip to
    ///   `Open`.
    /// - **Open**: block every request until `CIRCUIT_OPEN_COOLDOWN`
    ///   elapses, then transition to `HalfOpen` on the next call.
    /// - **HalfOpen**: allow exactly one probe fire. Subsequent fires
    ///   within `CIRCUIT_HALF_OPEN_PROBE_WINDOW` re-trip to `Open`. If no
    ///   second fire occurs during the probe window, the next call sees
    ///   a fresh `Closed` state.
    ///
    /// Returns `(allowed, newly_entered_state)` — `newly_entered_state`
    /// is `Some(name)` only on transitions so callers can log / notify.
    async fn circuit_check(&self, rule_id: &str) -> (bool, Option<&'static str>) {
        let now = std::time::Instant::now();
        let mut map = self.circuit.lock().await;
        let state = map.entry(rule_id.to_string()).or_insert_with(CircuitState::new_closed);

        match state {
            CircuitState::Closed { fires } => {
                fires.retain(|t| now.duration_since(*t) < CIRCUIT_WINDOW);
                if fires.len() >= CIRCUIT_MAX_FIRES {
                    *state = CircuitState::Open { opened_at: now };
                    return (false, Some("open"));
                }
                fires.push(now);
                (true, None)
            }
            CircuitState::Open { opened_at } => {
                if now.duration_since(*opened_at) < CIRCUIT_OPEN_COOLDOWN {
                    (false, None)
                } else {
                    // Cooldown elapsed → enter HalfOpen and allow one probe.
                    *state = CircuitState::HalfOpen { probed_at: now };
                    (true, Some("half_open"))
                }
            }
            CircuitState::HalfOpen { probed_at } => {
                if now.duration_since(*probed_at) < CIRCUIT_HALF_OPEN_PROBE_WINDOW {
                    // Any fire within the probe window → re-trip to Open.
                    *state = CircuitState::Open { opened_at: now };
                    (false, Some("open"))
                } else {
                    // Probe window elapsed without re-trip → reset to Closed
                    // and count this fire as the first of a fresh window.
                    *state = CircuitState::Closed { fires: vec![now] };
                    (true, Some("closed"))
                }
            }
        }
    }

    pub async fn run(mut self) {
        info!("AutopilotEngine started");
        loop {
            match self.event_rx.recv().await {
                Ok(event) => {
                    if let Err(e) = self.process_event(&event).await {
                        warn!(
                            event = event.event_name(),
                            error = %e,
                            "autopilot: process_event failed"
                        );
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Escalated from warn → error because a dropped event can
                    // mean an autopilot rule (e.g. "urgent task → notify
                    // on-call") is silently missed. Visibility here is the
                    // only trail since the dashboard's autopilot_history
                    // won't show a fire for lost events.
                    error!(
                        dropped_events = n,
                        "autopilot: event bus lagged — {n} events dropped, rules not evaluated \
                         (investigate slow DB or raise channel capacity)"
                    );
                    // Detach the DB write so this lag-handling path itself
                    // stays cheap — awaiting `append_activity` here would
                    // block the recv loop and amplify further event drops
                    // (classic logging-while-burning anti-pattern).
                    let ts = self.task_store.clone();
                    tokio::spawn(async move {
                        let _ = ts.append_activity(&crate::task_store::ActivityRow {
                            id: uuid::Uuid::new_v4().to_string(),
                            event_type: "autopilot_lag".into(),
                            agent_id: "autopilot".into(),
                            task_id: None,
                            summary: format!("Autopilot dropped {n} events (bus lagged)"),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            metadata: Some(
                                serde_json::json!({ "dropped_events": n }).to_string(),
                            ),
                        }).await;
                    });
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("autopilot: event bus closed, stopping");
                    break;
                }
            }
        }
    }

    async fn process_event(&self, event: &AutopilotEvent) -> Result<(), String> {
        let event_name = event.event_name();
        let fields = event.to_fields();
        let rules = self.store.list_rules().await?;
        for rule in rules
            .iter()
            .filter(|r| r.enabled && r.trigger_event == event_name)
        {
            let conditions: Value =
                serde_json::from_str(&rule.conditions).unwrap_or(Value::Null);
            if !evaluate(&conditions, &fields) {
                continue;
            }
            debug!(rule = %rule.name, event = event_name, "autopilot: rule matched");

            // Circuit breaker — protects against self-reinforcing loops
            // (e.g. `task_created → delegate → agent creates task → ...`).
            // Three-state: Closed (normal) / Open (cooldown) / HalfOpen
            // (probe one request; re-trip on any retry within probe window).
            let (allowed, transitioned) = self.circuit_check(&rule.id).await;
            if let Some(new_state) = transitioned {
                info!(
                    rule = %rule.name,
                    rule_id = %rule.id,
                    new_state,
                    "autopilot: circuit breaker state change"
                );
                // Record transitions to dashboard history so operators can
                // see the breaker tripping / recovering on the Activity tab.
                let (result_tag, details) = match new_state {
                    "open" => (
                        "circuit_open",
                        format!(
                            "circuit breaker TRIPPED — rule blocked for {}s (suspected loop)",
                            CIRCUIT_OPEN_COOLDOWN.as_secs()
                        ),
                    ),
                    "half_open" => (
                        "circuit_half_open",
                        format!(
                            "circuit breaker HALF-OPEN — probing with this request; retries within {}s will re-trip",
                            CIRCUIT_HALF_OPEN_PROBE_WINDOW.as_secs()
                        ),
                    ),
                    _ => ("circuit_closed", "circuit breaker CLOSED — probe succeeded, rule restored".into()),
                };
                let _ = self.store.append_history(&AutopilotHistoryRow {
                    id: uuid::Uuid::new_v4().to_string(),
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    triggered_at: chrono::Utc::now().to_rfc3339(),
                    result: result_tag.into(),
                    details: Some(details),
                }).await;
            }
            if !allowed {
                continue;
            }
            let action: Value =
                serde_json::from_str(&rule.action).unwrap_or(Value::Null);
            let outcome = self
                .execute_action(&rule.id, &rule.name, &action, &fields)
                .await;

            let history = AutopilotHistoryRow {
                id: uuid::Uuid::new_v4().to_string(),
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                triggered_at: chrono::Utc::now().to_rfc3339(),
                result: if outcome.is_ok() {
                    "success".into()
                } else {
                    "failure".into()
                },
                details: match &outcome {
                    Ok(_) => Some(format!("Triggered by {event_name}")),
                    Err(e) => Some(e.clone()),
                },
            };
            let _ = self.store.append_history(&history).await;

            let summary = match &outcome {
                Ok(_) => format!("Autopilot rule '{}' fired on {event_name}", rule.name),
                Err(e) => format!(
                    "Autopilot rule '{}' failed on {event_name}: {e}",
                    rule.name
                ),
            };
            let _ = self
                .task_store
                .append_activity(&ActivityRow {
                    id: uuid::Uuid::new_v4().to_string(),
                    event_type: "autopilot_triggered".into(),
                    agent_id: "autopilot".into(),
                    task_id: None,
                    summary,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    metadata: Some(
                        serde_json::json!({
                            "rule_id": rule.id,
                            "success": outcome.is_ok(),
                        })
                        .to_string(),
                    ),
                })
                .await;
        }
        Ok(())
    }

    async fn execute_action(
        &self,
        rule_id: &str,
        rule_name: &str,
        action: &Value,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        let action_type = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match action_type {
            "delegate" => self.action_delegate(action, fields).await,
            "notify" => self.action_notify(action, fields).await,
            "run_skill" => self.action_run_skill(action, fields).await,
            "" => Err(format!("rule {rule_name}/{rule_id}: action.type required")),
            other => Err(format!(
                "rule {rule_name}/{rule_id}: unknown action.type {other}"
            )),
        }
    }

    async fn action_delegate(
        &self,
        action: &Value,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        let target = action
            .get("target_agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "delegate.target_agent required".to_string())?;
        let prompt_template = action
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "delegate.prompt required".to_string())?;
        let prompt = render_template(prompt_template, fields);
        self.enqueue_prompt(target, &prompt).await
    }

    async fn action_notify(
        &self,
        action: &Value,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        let channel = action
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "notify.channel required".to_string())?;
        let chat_id = action
            .get("chat_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "notify.chat_id required".to_string())?;
        let text_template = action
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "notify.text required".to_string())?;
        let text = render_template(text_template, fields);
        let token = resolve_channel_token(&self.home_dir, channel).await?;
        send_channel_text(channel, chat_id, &token, &text).await
    }

    async fn action_run_skill(
        &self,
        action: &Value,
        fields: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        let target = action
            .get("target_agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "run_skill.target_agent required".to_string())?;
        let skill = action
            .get("skill_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "run_skill.skill_name required".to_string())?;

        // ── Path-traversal guard ──────────────────────────────
        // Both `target` and `skill` feed into a filesystem path; a
        // crafted rule could otherwise read arbitrary files via
        // `../../../etc/passwd`.
        if !is_safe_agent_id(target) {
            return Err(format!("invalid target_agent: {target}"));
        }
        if !is_safe_skill_name(skill) {
            return Err(format!("invalid skill_name: {skill}"));
        }

        let skills_dir = self.home_dir.join("agents").join(target).join("SKILLS");
        let skill_path = skills_dir.join(format!("{skill}.md"));

        // Canonicalize and confirm the result is still contained by
        // `<home>/agents/<target>/SKILLS/`. Defense in depth — the
        // alphanumeric-only checks above already reject traversal.
        match tokio::fs::canonicalize(&skill_path).await {
            Ok(canon) => {
                if let Ok(allowed) = tokio::fs::canonicalize(&skills_dir).await {
                    if !canon.starts_with(&allowed) {
                        return Err(format!(
                            "skill path escapes SKILLS dir: {canon:?}"
                        ));
                    }
                }
            }
            Err(e) => return Err(format!("canonicalize {skill_path:?}: {e}")),
        }

        let skill_body = tokio::fs::read_to_string(&skill_path)
            .await
            .map_err(|e| format!("read skill {skill_path:?}: {e}"))?;
        let prompt = format!(
            "Execute skill `{skill}`:\n\n{skill_body}\n\nEvent context:\n{}",
            Value::Object(fields.clone())
        );
        self.enqueue_prompt(target, &prompt).await
    }

    async fn enqueue_prompt(&self, target: &str, prompt: &str) -> Result<(), String> {
        let mq = match &self.message_queue {
            Some(q) => q.clone(),
            None => return Err("message queue not available".into()),
        };
        let msg = QueueMessage {
            id: uuid::Uuid::new_v4().to_string(),
            sender: "autopilot".into(),
            target: target.to_string(),
            payload: prompt.to_string(),
            status: MessageStatus::Pending,
            retry_count: 0,
            delegation_depth: 0,
            origin_agent: Some("autopilot".into()),
            sender_agent: Some("autopilot".into()),
            error: None,
            response: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            acked_at: None,
            completed_at: None,
            reply_channel: None,
            turn_id: None,
            session_id: None,
        };
        mq.enqueue(&msg).await
    }
}

// ─── Filesystem safety helpers ──────────────────────────────

/// True when `id` is a valid agent directory name.
///
/// Allowlist: lowercase alphanumeric + `-` + `_`, 1-64 chars, must not
/// start with `.`. Blocks traversal characters (`/`, `\`, `.`) and any
/// Unicode surprise. Mirrors `duduclaw-cli::is_valid_agent_id` semantics.
fn is_safe_agent_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// True when `name` is a valid skill file stem — same allowlist as agent
/// ids so we never open a path outside the agent's SKILLS/ directory.
fn is_safe_skill_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 128 {
        return false;
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ─── Channel send helpers ───────────────────────────────────

async fn resolve_channel_token(home_dir: &Path, channel: &str) -> Result<String, String> {
    let field = match channel {
        "telegram" => "telegram_bot_token",
        "line" => "line_channel_token",
        "discord" => "discord_bot_token",
        "slack" => "slack_bot_token",
        other => return Err(format!("unsupported notify.channel: {other}")),
    };
    read_encrypted_config_field(home_dir, "channels", field)
        .await
        .ok_or_else(|| format!("channels.{field} not configured"))
}

/// Lazily-initialized shared HTTP client for autopilot notify actions.
///
/// Reusing a single `reqwest::Client` keeps the connection pool warm —
/// critical when many rules fire notify actions in a burst (which would
/// otherwise spawn a fresh TCP+TLS handshake per call).
fn notify_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(4)
            .build()
            .expect("reqwest client build (autopilot notify)")
    })
}

async fn send_channel_text(
    channel: &str,
    chat_id: &str,
    token: &str,
    text: &str,
) -> Result<(), String> {
    let client = notify_http_client();
    match channel {
        "telegram" => {
            let url = format!("https://api.telegram.org/bot{token}/sendMessage");
            let resp = client
                .post(&url)
                .json(&serde_json::json!({ "chat_id": chat_id, "text": text }))
                .send()
                .await
                .map_err(|e| format!("telegram send: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("telegram API {}", resp.status()));
            }
            Ok(())
        }
        "line" => {
            let url = "https://api.line.me/v2/bot/message/push";
            let resp = client
                .post(url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "to": chat_id,
                    "messages": [{ "type": "text", "text": text }],
                }))
                .send()
                .await
                .map_err(|e| format!("line send: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("line API {}", resp.status()));
            }
            Ok(())
        }
        "discord" => {
            let url =
                format!("https://discord.com/api/v10/channels/{chat_id}/messages");
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bot {token}"))
                .json(&serde_json::json!({ "content": text }))
                .send()
                .await
                .map_err(|e| format!("discord send: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("discord API {}", resp.status()));
            }
            Ok(())
        }
        other => Err(format!("unsupported channel: {other}")),
    }
}

// ─── SQLite event bus poll bridge ───────────────────────────

/// How often to prune old events.
const EVENTS_PRUNE_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// Events older than this many days are deleted during prune.
const EVENTS_RETENTION_DAYS: i64 = 7;
/// Max rows returned per poll — bounds per-cycle memory and work.
const EVENTS_POLL_BATCH: i64 = 1000;

/// Convert a `(event, payload)` row from `events.db` into a typed
/// `AutopilotEvent`. Returns `None` for events the engine doesn't handle.
fn row_to_event(event: &str, payload_json: &str) -> Option<AutopilotEvent> {
    let payload: Value = serde_json::from_str(payload_json).unwrap_or(Value::Null);
    match event {
        "task.created" => Some(AutopilotEvent::TaskCreated { task: payload }),
        "task.updated" => Some(AutopilotEvent::TaskUpdated { task: payload }),
        "activity.new" => Some(AutopilotEvent::ActivityNew { activity: payload }),
        _ => None,
    }
}

/// Poll `events.db` for new rows appended by MCP subprocess(es) and
/// re-emit them onto the AutopilotEngine broadcast channel.
///
/// Replaces the former `events.jsonl` file-based bus. SQLite removes
/// every correctness concern the file bus had: row inserts are atomic,
/// concurrent writers are safe via WAL + busy_timeout, the monotonic
/// auto-increment `id` gives readers a simple `WHERE id > ?` watermark,
/// and old rows are pruned by a background task.
///
/// ## Correctness notes
///
/// - On startup `last_seen_id` is seeded to `MAX(id)` so historical
///   events from previous gateway runs are NOT replayed.
/// - Each poll runs a single parameterized `SELECT … WHERE id > ?` with
///   `LIMIT EVENTS_POLL_BATCH`, so cost is O(new rows) regardless of
///   total table size.
/// - Background prune (every `EVENTS_PRUNE_INTERVAL`) deletes rows older
///   than `EVENTS_RETENTION_DAYS` so the table stays bounded.
pub fn spawn_events_db_poll(
    store: Arc<crate::events_store::EventBusStore>,
    tx: broadcast::Sender<AutopilotEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Seed to current MAX(id) — skip historical events on restart.
        let mut last_seen_id = store.max_id().await.unwrap_or(0);
        let mut last_prune = std::time::Instant::now();
        info!(seed_id = last_seen_id, "events.db poll task started");

        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;

            match store.fetch_since(last_seen_id, EVENTS_POLL_BATCH).await {
                Ok(rows) if !rows.is_empty() => {
                    let batch_size = rows.len();
                    for row in &rows {
                        if let Some(ev) = row_to_event(&row.event, &row.payload) {
                            let _ = tx.send(ev);
                        }
                    }
                    // Rows come back ordered by id ASC — advance watermark.
                    if let Some(tail) = rows.last() {
                        last_seen_id = tail.id;
                    }
                    debug!(batch_size, last_seen_id, "events.db poll tick");
                }
                Ok(_) => { /* no new events */ }
                Err(e) => {
                    warn!(error = %e, "events_store.fetch_since failed");
                }
            }

            // Periodic retention — keeps events.db size bounded.
            if last_prune.elapsed() >= EVENTS_PRUNE_INTERVAL {
                let cutoff = (chrono::Utc::now()
                    - chrono::Duration::days(EVENTS_RETENTION_DAYS))
                    .to_rfc3339();
                match store.prune_before(&cutoff).await {
                    Ok(n) if n > 0 => debug!(deleted = n, "pruned old events"),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "prune events failed"),
                }
                last_prune = std::time::Instant::now();
            }
        }
    })
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fields_from(json: Value) -> serde_json::Map<String, Value> {
        json.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn eval_null_is_true() {
        let m = serde_json::Map::new();
        assert!(evaluate(&Value::Null, &m));
    }

    #[test]
    fn eval_simple_eq() {
        let fields = fields_from(serde_json::json!({
            "task": { "priority": "urgent" }
        }));
        let cond = serde_json::json!({
            "field": "task.priority",
            "op": "eq",
            "value": "urgent"
        });
        assert!(evaluate(&cond, &fields));
    }

    #[test]
    fn eval_all_short_circuit_false() {
        let fields = fields_from(serde_json::json!({
            "task": { "priority": "low" }
        }));
        let cond = serde_json::json!({
            "all": [
                { "field": "task.priority", "op": "in", "value": ["high", "urgent"] },
                { "field": "task.priority", "op": "eq", "value": "low" }
            ]
        });
        assert!(!evaluate(&cond, &fields));
    }

    #[test]
    fn eval_any_one_true() {
        let fields = fields_from(serde_json::json!({
            "task": { "priority": "medium" }
        }));
        let cond = serde_json::json!({
            "any": [
                { "field": "task.priority", "op": "eq", "value": "high" },
                { "field": "task.priority", "op": "eq", "value": "medium" }
            ]
        });
        assert!(evaluate(&cond, &fields));
    }

    #[test]
    fn eval_gt_and_lt() {
        let fields = fields_from(serde_json::json!({ "x": 10 }));
        let gt = serde_json::json!({ "field": "x", "op": "gt", "value": 5 });
        let lt = serde_json::json!({ "field": "x", "op": "lt", "value": 5 });
        assert!(evaluate(&gt, &fields));
        assert!(!evaluate(&lt, &fields));
    }

    #[test]
    fn eval_contains_string() {
        let fields = fields_from(serde_json::json!({ "msg": "hello world" }));
        let cond = serde_json::json!({ "field": "msg", "op": "contains", "value": "world" });
        assert!(evaluate(&cond, &fields));
    }

    // ── RFC-22 P1-9b regression tests: missing-field never matches ──

    #[test]
    fn missing_field_does_not_match_eq_null() {
        // 5/5 root cause: hand-inserted events.db payload was wrapped
        // in {"task":{...}} so to_fields produced task = {"task":{...}},
        // making `task.assigned_to` lookup miss → eq null matched →
        // autopilot fired for all 5 tasks instead of 1 unassigned.
        let fields = fields_from(serde_json::json!({
            "task": { "task": { "assigned_to": "duduclaw-eng-infra" } }
        }));
        let cond = serde_json::json!({
            "field": "task.assigned_to",
            "op": "eq",
            "value": null
        });
        assert!(
            !evaluate(&cond, &fields),
            "missing field must NOT match eq null"
        );
    }

    #[test]
    fn missing_field_does_not_match_eq_empty_string() {
        let fields = fields_from(serde_json::json!({"x": 1}));
        let cond = serde_json::json!({
            "field": "task.assigned_to",
            "op": "eq",
            "value": ""
        });
        assert!(
            !evaluate(&cond, &fields),
            "missing field must NOT match eq empty string"
        );
    }

    #[test]
    fn explicit_null_value_still_matches_eq_null() {
        // The fix must not regress: a payload that explicitly sets
        // `assigned_to: null` should still match `eq null`.
        let fields = fields_from(serde_json::json!({
            "task": { "assigned_to": null }
        }));
        let cond = serde_json::json!({
            "field": "task.assigned_to",
            "op": "eq",
            "value": null
        });
        assert!(
            evaluate(&cond, &fields),
            "explicit null value should match eq null"
        );
    }

    #[test]
    fn missing_field_fails_all_ops() {
        // Defense in depth: missing field must short-circuit every op
        // to false (no spurious in/contains/gt match either).
        let fields = fields_from(serde_json::json!({}));
        for (op, value) in [
            ("eq", serde_json::json!(null)),
            ("neq", serde_json::json!("anything")),
            ("in", serde_json::json!(["a", "b"])),
            ("not_in", serde_json::json!(["a", "b"])),
            ("contains", serde_json::json!("x")),
            ("gt", serde_json::json!(0)),
            ("lt", serde_json::json!(100)),
        ] {
            let cond = serde_json::json!({
                "field": "missing.path", "op": op, "value": value
            });
            assert!(
                !evaluate(&cond, &fields),
                "op {op} must return false when field is missing"
            );
        }
    }

    #[test]
    fn render_basic() {
        let fields = fields_from(serde_json::json!({
            "task": { "title": "Ship v2", "priority": "urgent" }
        }));
        let out = render_template(
            "[{task.priority}] {task.title}!",
            &fields,
        );
        assert_eq!(out, "[urgent] Ship v2!");
    }

    #[test]
    fn render_unknown_key_is_empty() {
        let fields = fields_from(serde_json::json!({}));
        let out = render_template("hi {nothere}", &fields);
        assert_eq!(out, "hi ");
    }

    #[test]
    fn render_unmatched_open_brace_preserved() {
        let fields = fields_from(serde_json::json!({}));
        let out = render_template("a {b", &fields);
        assert_eq!(out, "a {b");
    }

    #[test]
    fn event_name_mapping() {
        assert_eq!(
            AutopilotEvent::TaskCreated { task: Value::Null }.event_name(),
            "task_created"
        );
        assert_eq!(
            AutopilotEvent::TaskStatusChanged {
                task_id: "t1".into(),
                from: "todo".into(),
                to: "in_progress".into(),
                task: Value::Null,
            }
            .event_name(),
            "task_status_changed"
        );
    }

    #[test]
    fn safe_agent_id_rejects_traversal() {
        assert!(is_safe_agent_id("agnes"));
        assert!(is_safe_agent_id("agent-01"));
        assert!(is_safe_agent_id("agent_01"));
        assert!(!is_safe_agent_id("../etc"));
        assert!(!is_safe_agent_id("agent/sub"));
        assert!(!is_safe_agent_id(".hidden"));
        assert!(!is_safe_agent_id(""));
        assert!(!is_safe_agent_id(&"a".repeat(100)));
    }

    #[test]
    fn safe_skill_name_rejects_traversal() {
        assert!(is_safe_skill_name("pricing-audit"));
        assert!(is_safe_skill_name("deploy_v2"));
        assert!(!is_safe_skill_name("../passwd"));
        assert!(!is_safe_skill_name("skill/subdir"));
        assert!(!is_safe_skill_name(""));
    }

    #[test]
    fn eval_unknown_op_returns_false() {
        let fields = fields_from(serde_json::json!({ "x": 1 }));
        let cond = serde_json::json!({ "field": "x", "op": "regex_match", "value": "^1" });
        assert!(!evaluate(&cond, &fields));
    }

    #[test]
    fn eval_not_in_defaults_to_true_when_value_not_array() {
        let fields = fields_from(serde_json::json!({ "x": 1 }));
        let cond = serde_json::json!({ "field": "x", "op": "not_in", "value": "not-an-array" });
        // Graceful degradation — if the rule author mistyped, err on the
        // side of "don't fire" → not_in is true (nothing excluded).
        assert!(evaluate(&cond, &fields));
    }

    async fn make_engine() -> (AutopilotEngine, std::path::PathBuf) {
        let tmp_dir = std::env::temp_dir()
            .join(format!("duduclaw-circuit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let store = Arc::new(AutopilotStore::open(&tmp_dir).unwrap());
        let ts = Arc::new(TaskStore::open(&tmp_dir).unwrap());
        let (_tx, rx) = tokio::sync::broadcast::channel(16);
        let engine = AutopilotEngine::new(tmp_dir.clone(), store, ts, None, rx);
        (engine, tmp_dir)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn circuit_closed_to_open_on_burst() {
        let (engine, tmp) = make_engine().await;

        // First CIRCUIT_MAX_FIRES calls allowed, last one transitions to Open
        for i in 0..CIRCUIT_MAX_FIRES {
            let (allowed, transition) = engine.circuit_check("rule-x").await;
            assert!(allowed, "fire #{i} should be allowed");
            assert!(transition.is_none(), "no transition expected at fire #{i}");
        }
        // Next fire trips to Open
        let (allowed, transition) = engine.circuit_check("rule-x").await;
        assert!(!allowed);
        assert_eq!(transition, Some("open"));

        // Subsequent calls while Open stay blocked (no further transition)
        let (allowed, transition) = engine.circuit_check("rule-x").await;
        assert!(!allowed);
        assert_eq!(transition, None);

        // Independent rule unaffected
        let (allowed, _) = engine.circuit_check("rule-y").await;
        assert!(allowed);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn circuit_open_to_half_open_after_cooldown() {
        let (engine, tmp) = make_engine().await;
        // Manually seed Open state with opened_at in the past
        {
            let mut map = engine.circuit.lock().await;
            map.insert(
                "rule-z".into(),
                CircuitState::Open {
                    opened_at: std::time::Instant::now()
                        - CIRCUIT_OPEN_COOLDOWN
                        - Duration::from_secs(1),
                },
            );
        }
        // Next check should transition Open → HalfOpen and allow the probe
        let (allowed, transition) = engine.circuit_check("rule-z").await;
        assert!(allowed);
        assert_eq!(transition, Some("half_open"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn circuit_half_open_reopens_on_retry() {
        let (engine, tmp) = make_engine().await;
        // Seed HalfOpen state with recent probe
        {
            let mut map = engine.circuit.lock().await;
            map.insert(
                "rule-w".into(),
                CircuitState::HalfOpen {
                    probed_at: std::time::Instant::now(),
                },
            );
        }
        // Any fire within the probe window → re-trip to Open
        let (allowed, transition) = engine.circuit_check("rule-w").await;
        assert!(!allowed);
        assert_eq!(transition, Some("open"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn circuit_half_open_closes_after_quiet_probe_window() {
        let (engine, tmp) = make_engine().await;
        // Seed HalfOpen state with probed_at far in the past
        {
            let mut map = engine.circuit.lock().await;
            map.insert(
                "rule-q".into(),
                CircuitState::HalfOpen {
                    probed_at: std::time::Instant::now()
                        - CIRCUIT_HALF_OPEN_PROBE_WINDOW
                        - Duration::from_secs(1),
                },
            );
        }
        // Silent probe window elapsed → next fire transitions to Closed
        let (allowed, transition) = engine.circuit_check("rule-q").await;
        assert!(allowed);
        assert_eq!(transition, Some("closed"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
