//! WP6 — channel-action → dashboard live feedback.
//!
//! Design principle (user's words): *"看不到的東西使用者會認為它壞掉，即便它存在。"*
//! Everything a channel command persists — a cron routine, a memory write, a
//! synthesised skill — must show up on the dashboard **without a manual
//! refresh**, otherwise the user concludes the feature is broken.
//!
//! ## Why this module exists
//!
//! Before WP6 the dashboard only ever learned about state changes it made
//! itself: [`crate::handlers::Handler::broadcast_event`] is called exclusively
//! from dashboard RPC handlers. Anything created out-of-process — the MCP
//! subprocess that a channel conversation drives — landed in SQLite and stayed
//! invisible until the operator hit reload. `RoutinesPage` / `MemoryBrowser` /
//! `SkillMarketPage` all fetch once on mount and never subscribe, so "存在但看
//! 不到" was the guaranteed outcome.
//!
//! ## Mechanism — no new bus
//!
//! We reuse the two mechanisms that already exist end to end:
//!
//! 1. **`events.db`** ([`crate::events_store::EventBusStore`]) — the
//!    cross-process bus the MCP subprocess already writes `task.created` /
//!    `activity.new` to.
//! 2. **The dashboard WebSocket event channel** (`event_tx`) — the same
//!    `broadcast::Sender<String>` that carries `task.created`,
//!    `channels.status_changed`, `system.update_available`, …
//!
//! The bridge is [`bridge_tick`], called from
//! [`crate::autopilot_engine::spawn_events_db_poll`] — the task that is
//! *already* tailing `events.db` every 2 s. A whitelisted event row is
//! serialized as a `WsFrame::Event` and pushed to every connected dashboard.
//! Existing task bus events keep their autopilot behavior. Approval and
//! agent-status events are dashboard-only; autonomous goal transitions use
//! `emit_live` and never enter the event database or autopilot rule engine.
//!
//! Consumers (`RoutinesPage`, `MemoryBrowser`, `SkillMarketPage`) subscribe and
//! refetch. Refetch-on-event, not incremental patching: these lists are small,
//! and a refetch cannot drift from the server's view.

use std::path::Path;

use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::protocol::WsFrame;

/// A cron/routine row was created, updated, paused/resumed, deleted or run.
/// Payload: `{ action, id?, name?, cron?, agent_id? }`.
pub const EV_CRON_CHANGED: &str = "cron.changed";

/// An agent memory was written / superseded / invalidated.
/// Payload: `{ action, agent_id?, memory_id? }`.
pub const EV_MEMORY_CHANGED: &str = "memory.changed";

/// A skill was synthesised, graduated, recorded or otherwise mutated.
/// Payload: `{ action, agent_id?, skill? }`.
pub const EV_SKILL_CHANGED: &str = "skill.changed";

/// A channel's behavior settings (`channels.config_set` / `channel_config`
/// MCP tool) or access-control list (`channels.access_set` / a pairing
/// subject approved or revoked) changed. Payload:
/// `{ action, channel?, scope_id?, key?, subject? }`.
///
/// W2-2 (E1/E2, D-C1): both write paths for these settings — the dashboard
/// RPCs in `handlers.rs` and the in-channel `channel_config`/`pairing_manage`
/// MCP tools, which share the same `ChannelSettingsManager`/`AccessController`
/// storage — emit this so every connected dashboard session (and the
/// ChannelsPage 行為/存取 tabs specifically) reflect the other write path's
/// change without a manual refresh, closing the "dashboard has zero
/// visibility into channel_config/pairing_manage" gap.
pub const EV_CHANNEL_CONFIG_CHANGED: &str = "channel_config.changed";

/// Every event name this module forwards to the dashboard WebSocket.
///
/// Keep this list closed: the `events.db` tail carries autopilot triggers and
/// OS perception rows too, and those must not be sprayed at every connected
/// browser (`os_file` payloads contain filesystem paths and are admin-gated
/// behind an explicit `os.events.subscribe`).
pub const DASHBOARD_EVENTS: &[&str] = &[
    EV_CRON_CHANGED,
    EV_MEMORY_CHANGED,
    EV_SKILL_CHANGED,
    EV_CHANNEL_CONFIG_CHANGED,
    "task.created",
    "task.updated",
    "approval.created",
    "approval.decided",
    "approval.expired",
    "agent.status_changed",
];

/// Strict dashboard-only limits; the autopilot bus keeps its original semantics.
pub(crate) const MAX_BRIDGE_PAYLOAD_BYTES: usize = 8 * 1024;
pub(crate) const MAX_BRIDGE_EVENTS_PER_TICK: usize = 64;
/// Invalid/missing ids do not use the delivery budget, but still use this work cap.
pub(crate) const MAX_BRIDGE_ROWS_PER_TICK: usize = 256;

/// Extract only the fields defined for this event. Bus metadata is untrusted.
/// Size is checked before even attempting JSON parsing.
fn bridge_hint(event: &str, raw: &str) -> Option<Value> {
    if !DASHBOARD_EVENTS.contains(&event) || raw.len() > MAX_BRIDGE_PAYLOAD_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    let keys: &[&str] = match event {
        "task.created" | "task.updated" => &["id", "assigned_to"],
        "approval.created" | "approval.decided" | "approval.expired" => &["id", "agent_id"],
        "agent.status_changed" => &["agent_id"],
        EV_CRON_CHANGED => &["action", "id", "name", "cron", "agent_id"],
        EV_MEMORY_CHANGED => &["action", "agent_id", "memory_id"],
        EV_SKILL_CHANGED => &["action", "agent_id", "skill"],
        EV_CHANNEL_CONFIG_CHANGED => &["action", "channel", "scope_id", "key", "subject"],
        _ => return None,
    };
    let mut out = serde_json::Map::new();
    for key in keys {
        if let Some(value) = object.get(*key).filter(|v| !v.is_null()) {
            let value = value.as_str()?;
            if value.len() > 512 { return None; }
            if matches!(*key, "agent_id" | "assigned_to") && !duduclaw_core::is_valid_agent_id(value) {
                return None;
            }
            out.insert((*key).into(), json!(value));
        }
    }
    match event {
        "task.created" | "task.updated" => {
            if !duduclaw_core::is_valid_agent_id(out.get("id")?.as_str()?) { return None; }
        }
        "approval.created" | "approval.decided" | "approval.expired" => {
            let id = out.get("id")?.as_str()?;
            if !duduclaw_core::is_valid_agent_id(id) || uuid::Uuid::parse_str(id).is_err() { return None; }
        }
        "agent.status_changed" => { out.get("agent_id")?; }
        _ => {}
    }
    Some(Value::Object(out))
}

fn stored_agent_status(home: &Path, agent: &str) -> Option<duduclaw_core::types::AgentStatus> {
    if !duduclaw_core::is_valid_agent_id(agent) { return None; }
    let raw = std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml")).ok()?;
    #[derive(serde::Deserialize)]
    struct StoredConfig { agent: StoredIdentity }
    #[derive(serde::Deserialize)]
    struct StoredIdentity { name: String, status: duduclaw_core::types::AgentStatus }
    let config: StoredConfig = toml::from_str(&raw).ok()?;
    if config.agent.name != agent { return None; }
    Some(config.agent.status)
}

/// Rebuild authorization metadata from authoritative storage. The event is
/// only a changed-id hint; missing rows, malformed ids and owner mismatches
/// never reach the connection gate (including Admin connections).
/// `Err` is a store failure (busy, unreadable, refused path). `Ok(None)` is a
/// missing or forged row and must not be reported as loss.
async fn project_hint(home: &Path, event: &str, hint: Value) -> Result<Option<String>, ()> {
    let payload = match event {
        "task.created" | "task.updated" => {
            let Some(id) = hint.get("id").and_then(Value::as_str) else { return Ok(None); };
            let (owner, status) = match crate::task_store::TaskStore::live_metadata(home, id) {
                Ok(Some(row)) => row,
                Ok(None) => return Ok(None),
                Err(_) => return Err(()),
            };
            if !duduclaw_core::is_valid_agent_id(&owner) { return Ok(None); }
            if hint.get("assigned_to").and_then(Value::as_str).is_some_and(|a| a != owner) { return Ok(None); }
            json!({"id": id, "assigned_to": owner, "status": status})
        }
        "approval.created" | "approval.decided" | "approval.expired" => {
            let Some(id) = hint.get("id").and_then(Value::as_str) else { return Ok(None); };
            let record = match crate::approval::ApprovalStore::get_readonly(
                home, &crate::approval::ApprovalId::from(id.to_owned()),
            ) {
                Ok(Some(record)) => record,
                Ok(None) => return Ok(None),
                Err(_) => return Err(()),
            };
            if !duduclaw_core::is_valid_agent_id(&record.agent_id) { return Ok(None); }
            if hint.get("agent_id").and_then(Value::as_str).is_some_and(|a| a != record.agent_id) { return Ok(None); }
            json!({"id": id, "agent_id": record.agent_id, "action_kind": record.action_kind, "status": record.status})
        }
        "agent.status_changed" => {
            let Some(agent) = hint.get("agent_id").and_then(Value::as_str) else { return Ok(None); };
            let Some(status) = stored_agent_status(home, agent) else { return Ok(None); };
            json!({"agent_id": agent, "status": status})
        }
        _ => {
            if let Some(agent) = hint.get("agent_id").and_then(Value::as_str) {
                if stored_agent_status(home, agent).is_none() { return Ok(None); }
                json!({"agent_id": agent})
            } else {
                json!({})
            }
        }
    };
    Ok(serde_json::to_string(&WsFrame::event(event, payload)).ok())
}

#[cfg(test)]
pub(crate) async fn dashboard_push_frame(home: &Path, event: &str, raw: &str) -> Option<String> {
    let hint = bridge_hint(event, raw)?;
    project_hint(home, event, hint).await.unwrap_or_default()
}

/// At most one successful projection per (event, entity), with separate work
/// and delivery budgets. Every capped tick emits a content-free reconciliation
/// hint, so advancing the cursor cannot silently lose a real invalidation.
pub(crate) async fn bridge_tick(
    home: &Path, rows: &[crate::events_store::EventRow], cursor: &mut i64,
    tx: &tokio::sync::broadcast::Sender<String>,
) {
    let mut delivered = std::collections::HashSet::new();
    let mut examined = 0usize;
    let mut dropped = 0usize;
    let mut oversized = 0usize;
    let mut lost = false;
    for row in rows {
        *cursor = (*cursor).max(row.id);
        if !DASHBOARD_EVENTS.contains(&row.event.as_str()) { continue; }
        if examined >= MAX_BRIDGE_ROWS_PER_TICK {
            dropped += 1;
            lost = true;
            continue;
        }
        examined += 1;
        if row.payload.len() > MAX_BRIDGE_PAYLOAD_BYTES {
            oversized += 1;
            lost = true;
            continue;
        }
        let Some(hint) = bridge_hint(&row.event, &row.payload) else { dropped += 1; continue; };
        let entity = hint.get("id").or_else(|| hint.get("agent_id"))
            .and_then(Value::as_str).unwrap_or("").to_owned();
        let key = (row.event.clone(), entity);
        if delivered.contains(&key) { dropped += 1; continue; }
        // Missing or forged rows must not consume a valid entity's quota.
        // A store error is different: the row may be real, so ask for one
        // refresh and leave the delivery budget for rows that did project.
        match project_hint(home, &row.event, hint).await {
            Err(()) => {
                dropped += 1;
                lost = true;
                continue;
            }
            Ok(None) => {
                dropped += 1;
                continue;
            }
            Ok(Some(frame)) => {
                if delivered.len() >= MAX_BRIDGE_EVENTS_PER_TICK {
                    dropped += 1;
                    lost = true;
                    continue;
                }
                delivered.insert(key);
                let _ = tx.send(frame);
            }
        }
    }
    if lost {
        if let Ok(frame) = serde_json::to_string(&WsFrame::event("events_lost", json!({}))) {
            let _ = tx.send(frame);
        }
    }
    if dropped > 0 || oversized > 0 {
        warn!(dropped, oversized, "dashboard bridge coalesced or rejected rows");
    }
}

/// Process-local senders, keyed by home so isolated gateways never cross streams.
static LIVE_SENDERS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<
    std::path::PathBuf, tokio::sync::broadcast::Sender<String>,
>>> = std::sync::OnceLock::new();

pub(crate) fn register_live_sender(home: &Path, tx: tokio::sync::broadcast::Sender<String>) {
    if let Ok(mut senders) = LIVE_SENDERS.get_or_init(Default::default).lock() {
        senders.insert(home.to_path_buf(), tx);
    }
}

/// Gateway-only invalidation. Unlike `emit`, this never enters autopilot's bus.
pub(crate) fn emit_live(home: &Path, event: &str, payload: Value) {
    let Some(senders) = LIVE_SENDERS.get() else { return };
    let sender = senders.lock().ok().and_then(|s| s.get(home).cloned());
    if let Some(sender) = sender {
        let frame = json!({ "type": "event", "event": event, "payload": payload });
        let _ = sender.send(frame.to_string());
    }
}

/// A balanced turn pair even when a future is cancelled. Contains no prompt,
/// arguments, output or error text. Constructed before the runtime branch.
pub(crate) struct LiveTurn {
    home: std::path::PathBuf,
    payload: Value,
    _goal_round: Option<crate::goal_loop::LiveRuntimeRound>,
}

impl LiveTurn {
    pub(crate) fn start(
        home: &Path, agent_id: &str, task_id: Option<&str>,
        source: &str, runtime: &str,
    ) -> Self {
        let payload = json!({ "agent_id": agent_id, "source": source, "runtime": runtime });
        emit_live(home, "agent.turn_started", payload.clone());
        let goal_round = if matches!(source, "goal" | "responsibility")
            && !crate::goal_loop::TEAM_LIVE_ROUND.try_with(|active| *active).unwrap_or(false)
        {
            task_id.map(|id| crate::goal_loop::LiveRuntimeRound::start(home, agent_id, id))
        } else { None };
        Self { home: home.to_path_buf(), payload, _goal_round: goal_round }
    }

    pub(crate) async fn dispatch(
        home: &Path, agent_id: &str, request: crate::cost_telemetry::RequestType,
        runtime: &str,
    ) -> Self {
        let attribution = crate::runtime::GOAL_ROUND_ATTRIBUTION.try_with(Clone::clone).ok();
        let sender = crate::claude_runner::DELEGATION_ENV.try_with(|env|
            env.get(duduclaw_core::ENV_DELEGATION_SENDER).cloned()).ok().flatten();
        let task_id = attribution.as_ref().map(|a| a.episode_id.as_str());
        let mut source = match sender.as_deref() {
            Some(crate::responsibility::HEARTBEAT_SENDER) => "heartbeat",
            Some(crate::responsibility::GOAL_LOOP_SENDER) => "goal",
            Some("autopilot") => "autopilot",
            _ if attribution.as_ref().is_some_and(|a| a.round.is_some()) => "goal",
            _ => match request {
                crate::cost_telemetry::RequestType::Cron => "cron",
                crate::cost_telemetry::RequestType::Chat => "channel",
                _ => "assign",
            },
        };
        if source == "goal" {
            if let Some(task_id) = task_id {
                if home.join("tasks.db").exists() {
                    if let Ok(store) = crate::task_store::TaskStore::open(home) {
                        if store.in_occurrence_tree(task_id).await.unwrap_or(false) {
                            source = "responsibility";
                        }
                    }
                }
            }
        }
        Self::start(home, agent_id, task_id, source, runtime)
    }
}

impl Drop for LiveTurn {
    fn drop(&mut self) {
        emit_live(&self.home, "agent.turn_finished", self.payload.clone());
    }
}

/// Append a dashboard-feedback event to `events.db`.
///
/// Best-effort and never fatal: feedback is a UX affordance, so a closed or
/// unwritable event store degrades to "the dashboard needs a manual refresh",
/// never to a failed cron insert / memory write / skill graduation.
///
/// Callers in the gateway process could push to `event_tx` directly, but they
/// go through `events.db` on purpose — one code path, one whitelist, and the
/// event survives a dashboard that is not connected at write time long enough
/// for the 2 s tail to pick it up.
pub async fn emit(home_dir: &Path, event: &str, payload: Value) {
    debug_assert!(
        DASHBOARD_EVENTS.contains(&event),
        "dashboard_feedback::emit called with a non-whitelisted event: {event}"
    );
    let store = match crate::events_store::EventBusStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, event, "dashboard feedback: open events.db failed");
            return;
        }
    };
    if let Err(e) = store.append(event, &payload.to_string()).await {
        warn!(error = %e, event, "dashboard feedback: append events.db failed");
    } else {
        debug!(event, "dashboard feedback emitted");
    }
}

/// Map a completed MCP tool call to the dashboard-feedback event it should
/// raise, or `None` when the tool changes nothing a dashboard page shows.
///
/// Pure so the whole mapping — including the "don't announce a failure as a
/// change" rule — is testable without an MCP server.
///
/// `result` is the tool's JSON-RPC result object; a truthy `isError` means the
/// tool failed and nothing was persisted, so no event is raised.
/// `caller_agent` is the agent the MCP subprocess is acting for (the memory
/// write namespace / delegation sender / default agent, resolved at the
/// dispatch site). Most tools do not take an explicit `agent_id` argument, so
/// without this the payload carried `agent_id: null` and the dashboard's
/// per-agent filter degraded to "refetch on anything" — every agent's memory
/// write refreshed every other agent's MemoryBrowser. Pass `""` when the caller
/// genuinely is not known; the field is then omitted rather than guessed.
pub fn tool_feedback_event(
    tool_name: &str,
    args: &Value,
    result: &Value,
    caller_agent: &str,
) -> Option<(&'static str, Value)> {
    let failed = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if failed {
        return None;
    }

    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
    // An explicit argument always wins (a supervisor scheduling work for a
    // report names the target); otherwise attribute to the calling agent.
    let agent = || -> Option<String> {
        s("agent_id").or_else(|| {
            let c = caller_agent.trim();
            (!c.is_empty()).then(|| c.to_string())
        })
    };

    match tool_name {
        // ── Routines ────────────────────────────────────────────────────────
        // `tasks_create` with a cron `schedule` writes a cron row (named by
        // `title`, run as `assigned_to` or the caller). A board task, a goal
        // and an RFC3339 instant (the reminder rail) change no routine.
        "tasks_create" => {
            let schedule = s("schedule")?;
            let schedule = schedule.trim();
            if schedule.is_empty() || chrono::DateTime::parse_from_rfc3339(schedule).is_ok() {
                return None;
            }
            Some((
                EV_CRON_CHANGED,
                json!({
                    "action": "created",
                    "name": s("title"),
                    "cron": schedule,
                    "agent_id": s("assigned_to").or_else(|| agent()),
                }),
            ))
        }
        "update_cron_task" => Some((
            EV_CRON_CHANGED,
            json!({ "action": "updated", "id": s("id"), "name": s("name") }),
        )),
        "delete_cron_task" => Some((
            EV_CRON_CHANGED,
            json!({ "action": "removed", "id": s("id"), "name": s("name") }),
        )),
        "pause_cron_task" => Some((
            EV_CRON_CHANGED,
            json!({
                "action": "toggled",
                "id": s("id"),
                "name": s("name"),
                "enabled": args.get("enabled").and_then(|v| v.as_bool()),
            }),
        )),
        "run_cron_task" => Some((
            EV_CRON_CHANGED,
            json!({ "action": "ran", "id": s("id"), "name": s("name") }),
        )),

        // ── Memory ──────────────────────────────────────────────────────────
        // `memory_store` returns the canonical `memory_id` at the top level;
        // the others mutate existing rows, so the page just refetches.
        "memory_store" => Some((
            EV_MEMORY_CHANGED,
            json!({
                "action": "stored",
                "agent_id": agent(),
                "memory_id": result.get("memory_id").and_then(|v| v.as_str()),
            }),
        )),
        "user_profile_record" => Some((
            EV_MEMORY_CHANGED,
            json!({ "action": "profile_recorded", "agent_id": agent() }),
        )),
        "memory_improve" => Some((
            EV_MEMORY_CHANGED,
            json!({ "action": "improved", "agent_id": agent() }),
        )),
        "memory_alias_add" => Some((
            EV_MEMORY_CHANGED,
            json!({ "action": "alias_added", "agent_id": agent() }),
        )),
        "memory_invalidate_by_origin" => Some((
            EV_MEMORY_CHANGED,
            json!({ "action": "invalidated", "agent_id": agent() }),
        )),

        // ── Skills ──────────────────────────────────────────────────────────
        "skill_graduate" => Some((
            EV_SKILL_CHANGED,
            json!({
                "action": "graduated",
                "agent_id": agent(),
                "skill": s("skill_name").or_else(|| s("name")),
            }),
        )),
        "skill_synthesis_run" => Some((
            EV_SKILL_CHANGED,
            json!({ "action": "synthesized", "agent_id": agent() }),
        )),
        "skill_from_recording" => Some((
            EV_SKILL_CHANGED,
            json!({
                "action": "recorded",
                "agent_id": agent(),
                "skill": s("skill_name").or_else(|| s("name")),
            }),
        )),

        // ── Channel settings (W2-2, E1/E2) ─────────────────────────────────
        // `channel_config` is one MCP tool for both read and write — a `value`
        // argument present means "set"; omitted means "get" and must stay
        // silent (a refetch storm on every read would defeat the purpose of
        // a targeted feedback signal).
        "channel_config" if args.get("value").and_then(|v| v.as_str()).is_some() => Some((
            EV_CHANNEL_CONFIG_CHANGED,
            json!({
                "action": "config_set",
                "channel": s("channel"),
                "scope_id": s("scope_id"),
                "key": s("key"),
            }),
        )),
        // Only `approve`/`revoke` mutate the approved-subject list that
        // `channels.pairing_list` surfaces; `generate` only creates an
        // ephemeral pending code and `list` is read-only.
        "pairing_manage" if matches!(s("action").as_deref(), Some("approve") | Some("revoke")) => {
            Some((
                EV_CHANNEL_CONFIG_CHANGED,
                json!({
                    "action": format!("pairing_{}", s("action").unwrap_or_default()),
                    "subject": s("subject"),
                }),
            ))
        }

        _ => None,
    }
}

/// Emit the dashboard-feedback event (if any) for a completed MCP tool call.
///
/// One call site: the MCP dispatch tail in `duduclaw-cli`. Keeping the mapping
/// and the write here means adding a new fed-back tool is a one-line change in
/// [`tool_feedback_event`], not another scattered emit.
pub async fn emit_for_tool(
    home_dir: &Path,
    tool_name: &str,
    args: &Value,
    result: &Value,
    caller_agent: &str,
) {
    if let Some((event, payload)) = tool_feedback_event(tool_name, args, result, caller_agent) {
        emit(home_dir, event, payload).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn independent_broker_request_reaches_dashboard_bridge_without_private_payload() {
        let home = tempfile::tempdir().unwrap();
        // A separately opened broker models the mcp-server process: no access
        // to the gateway sender, only the common SQLite bus.
        let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
        let id = broker.request("worker", "goal_kickoff", "SECRET SUMMARY",
            json!({ "prompt": "SECRET PROMPT", "arguments": "SECRET ARGS" }), 60)
            .await.unwrap();
        let tail = crate::events_store::EventBusStore::open(home.path()).unwrap();
        let rows = tail.fetch_since(0, 100).await.unwrap();
        let row = rows.iter().find(|r| r.event == "approval.created").unwrap();
        let frame = dashboard_push_frame(home.path(), &row.event, &row.payload).await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["payload"]["id"], id.to_string());
        assert!(!frame.contains("SECRET"));
    }

    #[tokio::test]
    async fn task_bus_projection_excludes_prose_and_rejects_forged_owner() {
        let home = tempfile::tempdir().unwrap();
        let store = crate::task_store::TaskStore::open(home.path()).unwrap();
        let task = crate::task_store::TaskRow::new("t1".into(), "SECRET".into(), "SECRET".into(),
            "normal".into(), "worker".into(), "system".into());
        store.insert_task(&task).await.unwrap();
        let frame = dashboard_push_frame(home.path(), "task.created", &json!({
            "id": "t1", "assigned_to": "worker", "status": "forged",
            "description": "SECRET", "result_summary": "SECRET", "arguments": "SECRET"
        }).to_string()).await.unwrap();
        assert!(!frame.contains("SECRET"));
        assert!(!frame.contains("forged"));
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["payload"].as_object().unwrap().len(), 3);
        for payload in [json!({"id":"t1", "assigned_to":"attacker"}),
            json!({"id":"missing"}), json!({"id":"../t1"}), json!({"id":123})] {
            assert!(dashboard_push_frame(home.path(), "task.updated", &payload.to_string()).await.is_none());
        }
    }

    #[tokio::test]
    async fn approval_and_agent_bridge_use_authoritative_metadata() {
        let home = tempfile::tempdir().unwrap();
        let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
        let id = broker.request("worker", "tool_call", "SECRET", json!({}), 300).await.unwrap();
        let bad = json!({"id":id.as_str(), "agent_id":"attacker", "status":"approved"});
        assert!(dashboard_push_frame(home.path(), "approval.decided", &bad.to_string()).await.is_none());
        let hint = json!({"id":id.as_str(), "status":"approved", "action_kind":"forged"});
        let line = dashboard_push_frame(home.path(), "approval.created", &hint.to_string()).await.unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["payload"]["agent_id"], "worker");
        assert_eq!(value["payload"]["status"], "pending");
        assert_eq!(value["payload"]["action_kind"], "tool_call");
        std::fs::create_dir_all(home.path().join("agents/worker")).unwrap();
        std::fs::write(home.path().join("agents/worker/agent.toml"),
            "[agent]\nname = \"worker\"\nstatus = \"paused\"\n").unwrap();
        let hint = json!({"agent_id":"worker", "status":"active", "secret":"SECRET"});
        let line = dashboard_push_frame(home.path(), "agent.status_changed", &hint.to_string()).await.unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["payload"]["status"], "paused");
        assert!(!line.contains("SECRET"));
        for id in ["../worker", "missing"] {
            assert!(dashboard_push_frame(home.path(), "agent.status_changed",
                &json!({"agent_id":id}).to_string()).await.is_none());
        }
        std::fs::write(home.path().join("agents/worker/agent.toml"), "bad toml").unwrap();
        assert!(dashboard_push_frame(home.path(), "agent.status_changed", &hint.to_string()).await.is_none());
    }

    #[tokio::test]
    async fn bridge_reads_existing_schema_without_initializing_or_migrating() {
        let home = tempfile::tempdir().unwrap();
        assert!(dashboard_push_frame(home.path(), "task.created", r#"{"id":"t1"}"#).await.is_none());
        assert!(!home.path().join("tasks.db").exists());
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("CREATE TABLE tasks(id TEXT PRIMARY KEY, assigned_to TEXT, status TEXT);
            INSERT INTO tasks VALUES ('t1', 'worker', 'todo');").unwrap();
        assert!(dashboard_push_frame(home.path(), "task.created", r#"{"id":"t1"}"#).await.is_some());
        let count: i64 = conn.query_row("SELECT count(*) FROM sqlite_master WHERE name='task_authority_epochs'",
            [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn dispatch_sources_are_closed_and_runtime_independent() {
        let home = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(32);
        register_live_sender(home.path(), tx);
        for runtime in ["claude", "codex", "grok", "openai_compat"] {
            for (sender, expected) in [
                (crate::responsibility::HEARTBEAT_SENDER, "heartbeat"),
                (crate::responsibility::GOAL_LOOP_SENDER, "goal"),
                ("autopilot", "autopilot"), ("untrusted prompt text", "assign"),
            ] {
                let env = std::collections::HashMap::from([
                    (duduclaw_core::ENV_DELEGATION_SENDER.to_string(), sender.to_string())]);
                let turn = crate::claude_runner::DELEGATION_ENV.scope(env,
                    LiveTurn::dispatch(home.path(), "worker", crate::cost_telemetry::RequestType::Dispatch, runtime)
                ).await;
                let start: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
                assert_eq!(start["payload"]["source"], expected);
                assert_eq!(start["payload"]["runtime"], runtime);
                drop(turn);
                let finish: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
                assert_eq!(finish["event"], "agent.turn_finished");
            }
        }
        assert!(!home.path().join("events.db").exists());
    }

    #[test]
    fn balanced_turn_pair_is_in_process_and_home_scoped() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        register_live_sender(home.path(), tx);
        let turn = LiveTurn::start(home.path(), "worker", Some("private-task"), "channel", "codex");
        let first: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(first["event"], "agent.turn_started");
        assert_eq!(first["payload"]["runtime"], "codex");
        emit_live(other.path(), "agent.turn_started", json!({}));
        assert!(rx.try_recv().is_err());
        drop(turn);
        let last: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(last["event"], "agent.turn_finished");
        assert_eq!(first["payload"], last["payload"]);
        assert_eq!(first["payload"].as_object().unwrap().len(), 3);
        assert!(first["payload"].get("session_id").is_none());
        assert!(first["payload"].get("task_id").is_none());
        assert!(!home.path().join("events.db").exists());
    }

    /// The bridge forwards exactly the whitelisted event families and nothing
    /// else — an `os_file` row leaking to every browser would be a privacy
    /// regression (paths / window titles are admin-gated elsewhere).
    #[tokio::test]
    async fn bridge_schema_is_closed_and_invalid_json_is_dropped() {
        let home = tempfile::tempdir().unwrap();
        for event in [EV_CRON_CHANGED, EV_MEMORY_CHANGED, EV_SKILL_CHANGED, EV_CHANNEL_CONFIG_CHANGED] {
            assert!(dashboard_push_frame(home.path(), event, "{}").await.is_some());
            assert!(dashboard_push_frame(home.path(), event, "not json").await.is_none());
            assert!(dashboard_push_frame(home.path(), event, r#"{"action":42}"#).await.is_none());
        }
        for event in ["os_file", "activity.new", "task.created", "approval.created", "agent.status_changed"] {
            assert!(dashboard_push_frame(home.path(), event, "{}").await.is_none());
        }
        let frame = dashboard_push_frame(home.path(), EV_CRON_CHANGED,
            r#"{"action":"created","name":"morning","secret":"never forwarded"}"#).await.unwrap();
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["payload"], json!({}));
        assert!(parsed["payload"].get("secret").is_none());
    }

    #[tokio::test]
    async fn oversized_payload_is_rejected_before_json_parse_and_flood_advances_cursor() {
        let home = tempfile::tempdir().unwrap();
        // Deeply nested invalid JSON would hit the parser recursion limit;
        // the byte gate rejects it before the parser is invoked.
        let oversized = "[".repeat(MAX_BRIDGE_PAYLOAD_BYTES + 1);
        assert!(bridge_hint(EV_MEMORY_CHANGED, &oversized).is_none());
        // Otherwise-valid JSON must also be rejected, even if the oversized
        // field would be discarded by projection. This pins the byte cap.
        let valid_oversized = json!({"extra": "x".repeat(MAX_BRIDGE_PAYLOAD_BYTES)}).to_string();
        assert!(bridge_hint(EV_MEMORY_CHANGED, &valid_oversized).is_none());
        assert!(bridge_hint(EV_MEMORY_CHANGED, r#"{"extra":"small"}"#).is_some());
        let (tx, mut rx) = tokio::sync::broadcast::channel(128);
        let mut rows = Vec::new();
        for i in 1..=100 {
            rows.push(crate::events_store::EventRow { id:i, event: EV_CRON_CHANGED.into(),
                payload:json!({"id":format!("cron-{i}")}).to_string(), ts:String::new(), source:None });
        }
        rows.push(crate::events_store::EventRow { id:101, event: EV_MEMORY_CHANGED.into(),
            payload:oversized, ts:String::new(), source:None });
        let mut cursor = 0;
        bridge_tick(home.path(), &rows, &mut cursor, &tx).await;
        assert_eq!(cursor, 101);
        let mut count = 0;
        while rx.try_recv().is_ok() { count += 1; }
        assert_eq!(count, MAX_BRIDGE_EVENTS_PER_TICK + 1); // includes one events_lost
        for row in &mut rows { row.payload = json!({"id":"same"}).to_string(); }
        bridge_tick(home.path(), &rows[..100], &mut cursor, &tx).await;
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
        let mut next = rows[0].clone(); next.id = 102;
        bridge_tick(home.path(), &[next], &mut cursor, &tx).await;
        assert_eq!(cursor, 102);
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn fake_id_flood_delivers_real_invalidation_or_requests_reconciliation() {
        let home = tempfile::tempdir().unwrap();
        let store = crate::task_store::TaskStore::open(home.path()).unwrap();
        let task = crate::task_store::TaskRow::new("real".into(), "private".into(), "private".into(),
            "normal".into(), "worker".into(), "system".into());
        store.insert_task(&task).await.unwrap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(128);
        let mut cursor = 0;
        for fake_count in [64usize, MAX_BRIDGE_ROWS_PER_TICK, MAX_BRIDGE_ROWS_PER_TICK + 100] {
            // Repeat each flood across ticks: prior invalid rows cannot reserve
            // quota, and a real entity beyond the work cap is covered by loss.
            for _ in 0..3 {
                let mut rows: Vec<_> = (0..fake_count).map(|i| crate::events_store::EventRow {
                    id: cursor + i as i64 + 1, event: "task.updated".into(),
                    payload: json!({"id": format!("missing-{i}")}).to_string(),
                    ts: String::new(), source: None,
                }).collect();
                rows.push(crate::events_store::EventRow {
                    id: cursor + fake_count as i64 + 1, event: "task.updated".into(),
                    payload: json!({"id": "real"}).to_string(), ts: String::new(), source: None,
                });
                bridge_tick(home.path(), &rows, &mut cursor, &tx).await;
                assert_eq!(cursor, rows.last().unwrap().id);
                let mut real = 0;
                let mut lost = 0;
                while let Ok(raw) = rx.try_recv() {
                    let frame: Value = serde_json::from_str(&raw).unwrap();
                    match frame["event"].as_str().unwrap() {
                        "task.updated" => { assert_eq!(frame["payload"]["id"], "real"); real += 1; }
                        "events_lost" => { assert_eq!(frame["payload"], json!({})); lost += 1; }
                        other => panic!("unexpected event {other}"),
                    }
                }
                if fake_count < MAX_BRIDGE_ROWS_PER_TICK {
                    assert_eq!((real, lost), (1, 0));
                } else {
                    assert_eq!((real, lost), (0, 1));
                }
            }
        }
    }

    /// A failed authority read is not a missing row. Forged and absent ids
    /// still advance the cursor without `events_lost` or a delivery slot.
    #[tokio::test]
    async fn lookup_error_sets_events_lost_and_missing_rows_do_not() {
        fn row(id: i64, event: &str, payload: Value) -> crate::events_store::EventRow {
            crate::events_store::EventRow {
                id,
                event: event.into(),
                payload: payload.to_string(),
                ts: String::new(),
                source: None,
            }
        }
        fn take(rx: &mut tokio::sync::broadcast::Receiver<String>) -> Vec<Value> {
            let mut out = Vec::new();
            while let Ok(raw) = rx.try_recv() {
                out.push(serde_json::from_str(&raw).unwrap());
            }
            out
        }

        let home = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut cursor = 0;
        bridge_tick(
            home.path(),
            &[row(1, "task.updated", json!({"id": "real-task"}))],
            &mut cursor,
            &tx,
        )
        .await;
        assert_eq!(cursor, 1);
        let frames = take(&mut rx);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["event"], "events_lost");
        assert_eq!(frames[0]["payload"], json!({}));

        let _store = crate::task_store::TaskStore::open(home.path()).unwrap();
        bridge_tick(
            home.path(),
            &[
                row(2, "task.updated", json!({"id": "not-here"})),
                row(3, "task.updated", json!({"id": "../nope"})),
            ],
            &mut cursor,
            &tx,
        )
        .await;
        assert_eq!(cursor, 3);
        assert!(take(&mut rx).is_empty());

        std::fs::create_dir(home.path().join("approvals.db")).unwrap();
        bridge_tick(
            home.path(),
            &[row(
                4,
                "approval.created",
                json!({"id": "11111111-1111-4111-8111-111111111111"}),
            )],
            &mut cursor,
            &tx,
        )
        .await;
        assert_eq!(cursor, 4);
        let frames = take(&mut rx);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["event"], "events_lost");
        assert_eq!(frames[0]["payload"], json!({}));

        let flood = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(128);
        let mut rows: Vec<_> = (1..=80)
            .map(|i| row(i, "task.updated", json!({"id": format!("busy-{i}")})))
            .collect();
        rows.push(row(81, EV_CRON_CHANGED, json!({"id": "cron-kept", "action": "created"})));
        let mut cursor = 0;
        bridge_tick(flood.path(), &rows, &mut cursor, &tx).await;
        assert_eq!(cursor, 81);
        let frames = take(&mut rx);
        assert_eq!(frames.iter().filter(|frame| frame["event"] == "events_lost").count(), 1);
        assert_eq!(frames.iter().filter(|frame| frame["event"] == EV_CRON_CHANGED).count(), 1);
        assert!(frames.iter().all(|frame| frame["event"] == "events_lost" || frame["event"] == EV_CRON_CHANGED));
    }

    #[tokio::test]
    async fn valid_entity_budget_overflow_emits_one_empty_loss_frame() {
        let home = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::broadcast::channel(128);
        let rows: Vec<_> = (1..=100).map(|i| crate::events_store::EventRow {
            id: i, event: EV_CRON_CHANGED.into(),
            payload: json!({"id": format!("cron-{i}")}).to_string(),
            ts: String::new(), source: None,
        }).collect();
        let mut cursor = 0;
        bridge_tick(home.path(), &rows, &mut cursor, &tx).await;
        let mut delivered = 0;
        let mut lost = 0;
        while let Ok(raw) = rx.try_recv() {
            let frame: Value = serde_json::from_str(&raw).unwrap();
            if frame["event"] == "events_lost" {
                assert_eq!(frame["payload"], json!({}));
                lost += 1;
            } else { delivered += 1; }
        }
        assert_eq!((delivered, lost, cursor), (MAX_BRIDGE_EVENTS_PER_TICK, 1, 100));
    }

    /// Creating a cron routine from a channel raises `cron.changed` carrying
    /// enough context for a toast, and every cron mutation tool is covered.
    #[test]
    fn cron_tools_map_to_cron_changed() {
        let args = json!({ "title": "晨報", "schedule": "0 9 * * *", "assigned_to": "sam" });
        let (ev, payload) = tool_feedback_event("tasks_create", &args, &json!({}), "").unwrap();
        assert_eq!(ev, EV_CRON_CHANGED);
        assert_eq!(payload["action"], "created");
        assert_eq!(payload["name"], "晨報");
        assert_eq!(payload["cron"], "0 9 * * *");
        assert_eq!(payload["agent_id"], "sam");

        // A board task, a goal, and a one-shot instant (the reminder rail)
        // change nothing on the routines page.
        assert!(tool_feedback_event("tasks_create", &json!({ "title": "x" }), &json!({}), "a").is_none());
        assert!(
            tool_feedback_event(
                "tasks_create",
                &json!({ "title": "x", "schedule": "2026-10-01T09:00:00+08:00" }),
                &json!({}),
                "a"
            )
            .is_none()
        );
        // The removed name raises nothing: a call to it creates nothing.
        assert!(tool_feedback_event("schedule_task", &json!({ "name": "x", "cron": "0 9 * * *" }), &json!({}), "a").is_none());

        for tool in [
            "update_cron_task",
            "delete_cron_task",
            "pause_cron_task",
            "run_cron_task",
        ] {
            let (ev, _) = tool_feedback_event(tool, &json!({ "id": "x" }), &json!({}), "")
                .unwrap_or_else(|| panic!("{tool} must feed back"));
            assert_eq!(ev, EV_CRON_CHANGED, "{tool}");
        }
    }

    /// Memory and skill writes reach their own pages.
    #[test]
    fn memory_and_skill_tools_map_to_their_events() {
        let (ev, payload) = tool_feedback_event(
            "memory_store",
            &json!({ "content": "客戶偏好早上開會" }),
            &json!({ "memory_id": "m-1" }),
            "agnes",
        )
        .unwrap();
        assert_eq!(ev, EV_MEMORY_CHANGED);
        assert_eq!(payload["memory_id"], "m-1");

        for tool in [
            "user_profile_record",
            "memory_improve",
            "memory_alias_add",
            "memory_invalidate_by_origin",
        ] {
            let (ev, _) = tool_feedback_event(tool, &json!({}), &json!({}), "")
                .unwrap_or_else(|| panic!("{tool} must feed back"));
            assert_eq!(ev, EV_MEMORY_CHANGED, "{tool}");
        }

        let (ev, payload) = tool_feedback_event(
            "skill_graduate",
            &json!({ "skill_name": "invoice-ocr", "agent_id": "sam" }),
            &json!({}),
            "",
        )
        .unwrap();
        assert_eq!(ev, EV_SKILL_CHANGED);
        assert_eq!(payload["skill"], "invoice-ocr");

        for tool in ["skill_synthesis_run", "skill_from_recording"] {
            let (ev, _) = tool_feedback_event(tool, &json!({}), &json!({}), "")
                .unwrap_or_else(|| panic!("{tool} must feed back"));
            assert_eq!(ev, EV_SKILL_CHANGED, "{tool}");
        }
    }

    /// M2 — the payload must name an agent even when the tool takes no
    /// `agent_id` argument, otherwise `MemoryBrowser`'s per-agent filter has
    /// nothing to match on and every agent's write refreshes every other
    /// agent's view. An explicit argument still wins over the caller.
    #[test]
    fn caller_agent_attributes_tools_that_take_no_agent_id() {
        // `memory_store` has no agent_id parameter at all — attribution can
        // only come from the caller (its memory write namespace).
        let (_, payload) = tool_feedback_event(
            "memory_store",
            &json!({ "content": "客戶偏好早上開會" }),
            &json!({ "memory_id": "m-1" }),
            "agnes",
        )
        .unwrap();
        assert_eq!(payload["agent_id"], "agnes");

        // An explicit argument wins: a supervisor scheduling work for a report
        // must attribute to the report, not to itself.
        let (_, payload) = tool_feedback_event(
            "tasks_create",
            &json!({ "title": "晨報", "schedule": "0 9 * * *", "assigned_to": "sam" }),
            &json!({}),
            "supervisor",
        )
        .unwrap();
        assert_eq!(payload["agent_id"], "sam");
        // No assignee ⇒ the cron row runs as the caller.
        let (_, payload) = tool_feedback_event(
            "tasks_create",
            &json!({ "title": "晨報", "schedule": "0 9 * * *" }),
            &json!({}),
            "supervisor",
        )
        .unwrap();
        assert_eq!(payload["agent_id"], "supervisor");

        // Unknown caller ⇒ null, not a guess. The dashboard treats a missing
        // agent_id as "might be mine" and refetches, which is the safe default.
        let (_, payload) =
            tool_feedback_event("memory_improve", &json!({}), &json!({}), "  ").unwrap();
        assert!(payload["agent_id"].is_null());
    }

    /// `channel_config` only feeds back when it actually wrote a value; a
    /// read (`value` omitted) must stay silent (W2-2).
    #[test]
    fn channel_config_set_feeds_back_but_get_does_not() {
        let (ev, payload) = tool_feedback_event(
            "channel_config",
            &json!({ "channel": "discord", "scope_id": "global", "key": "mention_only", "value": "true" }),
            &json!({}),
            "",
        )
        .unwrap();
        assert_eq!(ev, EV_CHANNEL_CONFIG_CHANGED);
        assert_eq!(payload["action"], "config_set");
        assert_eq!(payload["channel"], "discord");
        assert_eq!(payload["key"], "mention_only");

        assert!(
            tool_feedback_event(
                "channel_config",
                &json!({ "channel": "discord", "scope_id": "global", "key": "mention_only" }),
                &json!({}),
                "",
            )
            .is_none()
        );
    }

    /// `pairing_manage` only feeds back on `approve`/`revoke` (the
    /// approved-subject list `channels.pairing_list` reads); `generate` and
    /// `list` must stay silent (W2-2).
    #[test]
    fn pairing_manage_approve_revoke_feed_back_generate_list_do_not() {
        let (ev, payload) = tool_feedback_event(
            "pairing_manage",
            &json!({ "action": "revoke", "subject": "u1" }),
            &json!({}),
            "",
        )
        .unwrap();
        assert_eq!(ev, EV_CHANNEL_CONFIG_CHANGED);
        assert_eq!(payload["action"], "pairing_revoke");
        assert_eq!(payload["subject"], "u1");

        let (ev, payload) = tool_feedback_event(
            "pairing_manage",
            &json!({ "action": "approve", "subject": "u2" }),
            &json!({}),
            "",
        )
        .unwrap();
        assert_eq!(ev, EV_CHANNEL_CONFIG_CHANGED);
        assert_eq!(payload["action"], "pairing_approve");

        for action in ["generate", "list"] {
            assert!(
                tool_feedback_event(
                    "pairing_manage",
                    &json!({ "action": action, "subject": "u3" }),
                    &json!({}),
                    "",
                )
                .is_none()
            );
        }
    }

    /// A failed tool persisted nothing — announcing a change would make the
    /// dashboard refetch and show the *absence* of the thing the user was just
    /// told about. Fail-quiet, not fail-loud.
    #[test]
    fn failed_tool_call_raises_nothing() {
        let err = json!({ "isError": true, "content": [] });
        assert!(
            tool_feedback_event("tasks_create", &json!({ "title": "x", "schedule": "0 9 * * *" }), &err, "")
                .is_none()
        );
        assert!(tool_feedback_event("memory_store", &json!({}), &err, "").is_none());
        assert!(tool_feedback_event("skill_graduate", &json!({}), &err, "").is_none());
    }

    /// Read-only / unrelated tools stay silent — no refetch storms.
    #[test]
    fn unrelated_tools_raise_nothing() {
        for tool in [
            "memory_search",
            "list_cron_tasks",
            "web_search",
            "skill_list",
        ] {
            assert!(
                tool_feedback_event(tool, &json!({}), &json!({}), "").is_none(),
                "{tool} must not feed back"
            );
        }
    }

    /// End-to-end at the store level: an emitted event is readable by the same
    /// `fetch_since` tail the bridge uses, and converts to a pushable frame.
    #[tokio::test]
    async fn emitted_event_is_readable_by_the_tail_and_pushable() {
        let home = tempfile::tempdir().unwrap();
        emit(
            home.path(),
            EV_CRON_CHANGED,
            json!({ "action": "created", "name": "晨報" }),
        )
        .await;

        let store = crate::events_store::EventBusStore::open(home.path()).unwrap();
        let rows = store.fetch_since(0, 10).await.unwrap();
        let row = rows
            .iter()
            .find(|r| r.event == EV_CRON_CHANGED)
            .expect("cron.changed must be persisted");

        let frame = dashboard_push_frame(home.path(), &row.event, &row.payload).await.expect("must be pushable");
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["payload"], json!({}));
    }
}
