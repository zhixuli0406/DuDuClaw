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
//! The bridge is [`dashboard_push_frame`], called from
//! [`crate::autopilot_engine::spawn_events_db_poll`] — the task that is
//! *already* tailing `events.db` every 2 s. A whitelisted event row is
//! serialized as a `WsFrame::Event` and pushed to every connected dashboard.
//! [`crate::autopilot_engine::row_to_event`] returns `None` for these event
//! names, so they never reach the autopilot rule engine — this is a pure
//! view-refresh signal, not an automation trigger.
//!
//! Consumers (`RoutinesPage`, `MemoryBrowser`, `SkillMarketPage`) subscribe and
//! refetch. Refetch-on-event, not incremental patching: these lists are small,
//! and a refetch cannot drift from the server's view.

use std::path::Path;

use serde_json::{json, Value};
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

/// A one-time startup migration changed the user's runtime configuration
/// without them asking. Payload: `{ migration, reset_agents, failed_agents }`.
///
/// WP10: silent config rewrites are exactly what caused the 2026-08-04 field
/// incident, so the *corrective* rewrite is not allowed to be silent either —
/// the user sees which agents changed and why.
pub const EV_RUNTIME_MIGRATED: &str = "runtime.migrated";

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
    EV_RUNTIME_MIGRATED,
    EV_CHANNEL_CONFIG_CHANGED,
];

/// Serialize one `events.db` row into a dashboard `WsFrame::Event` JSON string,
/// or `None` when the event is not a dashboard-feedback event.
///
/// Pure — the whole bridge decision lives here so it is unit-testable without a
/// database, a socket, or a running gateway.
///
/// A row whose payload is not valid JSON still forwards (as `null`): the page's
/// reaction is "refetch", so a malformed payload must not swallow the signal —
/// that would reintroduce exactly the invisible-state bug WP6 fixes.
pub fn dashboard_push_frame(event: &str, payload_json: &str) -> Option<String> {
    if !DASHBOARD_EVENTS.contains(&event) {
        return None;
    }
    let payload: Value = serde_json::from_str(payload_json).unwrap_or(Value::Null);
    let frame = WsFrame::Event {
        event: event.to_string(),
        payload,
        seq: None,
        state_version: None,
    };
    serde_json::to_string(&frame).ok()
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
        "schedule_task" => Some((
            EV_CRON_CHANGED,
            json!({
                "action": "created",
                "name": s("name"),
                "cron": s("cron"),
                "agent_id": agent(),
            }),
        )),
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
        "pairing_manage"
            if matches!(s("action").as_deref(), Some("approve") | Some("revoke")) =>
        {
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

    /// The bridge forwards exactly the whitelisted event families and nothing
    /// else — an `os_file` row leaking to every browser would be a privacy
    /// regression (paths / window titles are admin-gated elsewhere).
    #[test]
    fn only_whitelisted_events_are_pushed() {
        assert!(dashboard_push_frame(EV_CRON_CHANGED, "{}").is_some());
        assert!(dashboard_push_frame(EV_MEMORY_CHANGED, "{}").is_some());
        assert!(dashboard_push_frame(EV_SKILL_CHANGED, "{}").is_some());
        assert!(dashboard_push_frame(EV_RUNTIME_MIGRATED, "{}").is_some());
        assert!(dashboard_push_frame(EV_CHANNEL_CONFIG_CHANGED, "{}").is_some());

        assert!(dashboard_push_frame("os_file", r#"{"path":"/secret"}"#).is_none());
        assert!(dashboard_push_frame("task.created", "{}").is_none());
        assert!(dashboard_push_frame("activity.new", "{}").is_none());
    }

    /// The forwarded frame is a well-formed `WsFrame::Event` the dashboard
    /// `ws-client` can route by name.
    #[test]
    fn push_frame_is_a_ws_event_frame() {
        let json = dashboard_push_frame(EV_CRON_CHANGED, r#"{"action":"created","name":"晨報"}"#)
            .expect("whitelisted");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "event");
        assert_eq!(parsed["event"], EV_CRON_CHANGED);
        assert_eq!(parsed["payload"]["action"], "created");
        assert_eq!(parsed["payload"]["name"], "晨報");
    }

    /// WP10 M5 — the one-time runtime migration must reach the dashboard with
    /// the agent list intact, so a config rewrite the user never asked for is
    /// visible rather than log-only.
    #[test]
    fn runtime_migration_event_carries_the_reset_list() {
        let json = dashboard_push_frame(
            EV_RUNTIME_MIGRATED,
            r#"{"migration":"wp10-pty-default-reset","reset_agents":["agnes","bob"],"failed_agents":[]}"#,
        )
        .expect("whitelisted");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "event");
        assert_eq!(parsed["event"], "runtime.migrated");
        assert_eq!(parsed["payload"]["reset_agents"][0], "agnes");
        assert_eq!(parsed["payload"]["reset_agents"][1], "bob");
    }

    /// A malformed payload must not swallow the refresh signal.
    #[test]
    fn malformed_payload_still_forwards() {
        let json = dashboard_push_frame(EV_MEMORY_CHANGED, "not json").expect("still forwards");
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["event"], EV_MEMORY_CHANGED);
        assert!(parsed["payload"].is_null());
    }

    /// Creating a cron routine from a channel raises `cron.changed` carrying
    /// enough context for a toast, and every cron mutation tool is covered.
    #[test]
    fn cron_tools_map_to_cron_changed() {
        let args = json!({ "name": "晨報", "cron": "0 9 * * *", "agent_id": "sam" });
        let (ev, payload) = tool_feedback_event("schedule_task", &args, &json!({}), "").unwrap();
        assert_eq!(ev, EV_CRON_CHANGED);
        assert_eq!(payload["action"], "created");
        assert_eq!(payload["name"], "晨報");
        assert_eq!(payload["cron"], "0 9 * * *");
        assert_eq!(payload["agent_id"], "sam");

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
            "schedule_task",
            &json!({ "name": "晨報", "cron": "0 9 * * *", "agent_id": "sam" }),
            &json!({}),
            "supervisor",
        )
        .unwrap();
        assert_eq!(payload["agent_id"], "sam");

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

        assert!(tool_feedback_event(
            "channel_config",
            &json!({ "channel": "discord", "scope_id": "global", "key": "mention_only" }),
            &json!({}),
            "",
        )
        .is_none());
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
            assert!(tool_feedback_event(
                "pairing_manage",
                &json!({ "action": action, "subject": "u3" }),
                &json!({}),
                "",
            )
            .is_none());
        }
    }

    /// A failed tool persisted nothing — announcing a change would make the
    /// dashboard refetch and show the *absence* of the thing the user was just
    /// told about. Fail-quiet, not fail-loud.
    #[test]
    fn failed_tool_call_raises_nothing() {
        let err = json!({ "isError": true, "content": [] });
        assert!(tool_feedback_event("schedule_task", &json!({ "name": "x" }), &err, "").is_none());
        assert!(tool_feedback_event("memory_store", &json!({}), &err, "").is_none());
        assert!(tool_feedback_event("skill_graduate", &json!({}), &err, "").is_none());
    }

    /// Read-only / unrelated tools stay silent — no refetch storms.
    #[test]
    fn unrelated_tools_raise_nothing() {
        for tool in ["memory_search", "list_cron_tasks", "web_search", "skill_list"] {
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

        let frame = dashboard_push_frame(&row.event, &row.payload).expect("must be pushable");
        let parsed: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(parsed["payload"]["name"], "晨報");
    }
}
