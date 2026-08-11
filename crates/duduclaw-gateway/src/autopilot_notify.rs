//! Circuit-breaker trip notification + "暫停這條規則" inline decision.
//!
//! ## The gap this closes
//!
//! `AutopilotEngine`'s per-rule circuit breaker (see `autopilot_engine.rs`'s
//! Closed/Open/HalfOpen state machine) already protects the platform from a
//! self-reinforcing loop — the moment a rule trips it is blocked. But until
//! this module, the ONLY visible trace of a trip was a dashboard
//! `autopilot_history` row and an Activity Feed entry: an operator who wasn't
//! looking at the dashboard when a rule looped never found out until the next
//! time they happened to check. This mirrors `goal_notify.rs` /
//! `approval_notify.rs`: a channel push on the notable transition, plus an
//! inline "暫停這條規則" action on the four button-capable channels.
//!
//! ## Shape
//!
//! - **Outbound** [`notify_circuit_open`] — called from
//!   `AutopilotEngine::fire_matched_rule` on a FRESH Closed→Open trip only
//!   (never on a HalfOpen probe re-trip of the same open episode — see the
//!   dedup note below). Fire-and-forget from the caller's perspective: the
//!   breaker's own blocking behavior has already taken effect by the time
//!   this is invoked, so a push failure here can never weaken the
//!   protection itself ("推播是化妝，保護是本體").
//! - **Inbound** [`decide_from_channel`] — a `duduclaw:autopilot_pause:{id}`
//!   press is authorized with the exact same posture as
//!   [`crate::approval_notify::authorize_press`] (reused directly, not
//!   re-implemented) and applied via `AutopilotStore::update_rule`.
//!
//! ## Dedup
//!
//! A rule can only be "freshly tripped" once per open episode: Closed→Open
//! is the notify-worthy transition. A HalfOpen probe failing and re-tripping
//! the SAME episode must NOT push a second notice — `autopilot_engine.rs`
//! reports that case under a distinct `"open_retrip"` transition tag and
//! only calls [`notify_circuit_open`] on the fresh `"open"` tag. The next
//! genuinely fresh trip (after the breaker has fully returned to Closed)
//! notifies again.
//!
//! ## Target resolution
//!
//! A rule row has no explicit `agent_id` column. [`resolve_rule_target_agent`]
//! resolves it best-effort from the rule's own `action`/`conditions` — the
//! same `{"field":"agent_id",...}` condition shape `rule_induction.rs`
//! already writes and `handlers.rs`'s OS-status page already reads. A rule
//! with no resolvable agent gets no proactive circuit-trip notice; it stays
//! fully protected by the breaker, only the notification is skipped.

use std::path::Path;

use serde_json::json;
use tracing::info;

use crate::autopilot_store::{AutopilotHistoryRow, AutopilotRuleRow, AutopilotStore};
use crate::decision_action::{DecisionAct, DecisionSource};
use crate::decision_notify::{
    authorize_press, destination_matches_any, identity_system_active, mapped_role, refusal_text,
    DecisionCard, PressAuth,
};
use crate::task_store::{ActivityRow, TaskStore};

// ── Target resolution ────────────────────────────────────────────

/// Best-effort resolution of the agent a rule's action targets.
///
/// `delegate`/`run_skill` actions carry an explicit `target_agent`; anything
/// else (`notify`, `proactive_notify`, or a malformed action) falls back to a
/// scoping condition leaf inside `conditions`. Two field names are honoured,
/// in [`AGENT_SCOPING_FIELDS`] order. `None` when none is present.
pub(crate) fn resolve_rule_target_agent(rule: &AutopilotRuleRow) -> Option<String> {
    if let Ok(action) = serde_json::from_str::<serde_json::Value>(&rule.action) {
        if let Some(agent) = action.get("target_agent").and_then(|v| v.as_str()) {
            let agent = agent.trim();
            if !agent.is_empty() {
                return Some(agent.to_string());
            }
        }
    }
    let conditions: serde_json::Value = serde_json::from_str(&rule.conditions).ok()?;
    AGENT_SCOPING_FIELDS
        .iter()
        .find_map(|field| find_agent_condition(&conditions, field))
}

/// Condition fields that name the agent a rule is about, most specific first.
///
/// - `agent_id` is the shape `rule_induction.rs` writes for induced rules and
///   `handlers.rs::rule_targets_agent` reads for the OS-status page. It is a
///   real event field for `ChannelMessage` / `AgentIdle` / `OsFileEvent`.
/// - `task.assigned_to` is the shape a task rule must use: `TaskCreated` and
///   `TaskUpdated` expose only `event` and `task`
///   (`AutopilotEngine::to_fields`), so `agent_id` never resolves for them and
///   scoping by the task's assignee is the only thing a rule author can
///   write. Without this fallback the most natural task rule there is — filter
///   on `task.assigned_to`, then notify — had no resolvable target, so its
///   circuit-breaker trip was silently never pushed anywhere ("rule has no
///   resolvable target agent"). Confirmed against a live trip.
const AGENT_SCOPING_FIELDS: [&str; 2] = ["agent_id", "task.assigned_to"];

/// Walk a parsed `conditions` tree (the `{"all"/"any": [...]}` shape
/// `autopilot_engine::evaluate` understands) for the first leaf naming
/// `field` and return its value. Depth-first, leftmost-first — deterministic
/// for a rule authored with a single scoping condition, which is the only
/// shape induced rules or the dashboard rule builder produce today.
fn find_agent_condition(v: &serde_json::Value, field: &str) -> Option<String> {
    if v.get("field").and_then(|f| f.as_str()) == Some(field) {
        if let Some(value) = v.get("value") {
            match value {
                serde_json::Value::String(s) if !s.trim().is_empty() => {
                    return Some(s.clone());
                }
                serde_json::Value::Array(arr) => {
                    if let Some(first) =
                        arr.iter().find_map(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty())
                    {
                        return Some(first.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    for key in ["all", "any"] {
        if let Some(arr) = v.get(key).and_then(|a| a.as_array()) {
            for item in arr {
                if let Some(found) = find_agent_condition(item, field) {
                    return Some(found);
                }
            }
        }
    }
    None
}

// ── Message rendering ───────────────────────────────────────────

/// The zh-TW symptom line for a fresh circuit-open trip. `rule_name` is
/// dashboard-authored free text folded into a channel message — escaped for
/// the same reason `approval_notify::approval_body` escapes its interpolated
/// fields (a crafted name must not forge a fake section boundary in the
/// rendered message).
fn circuit_open_body(rule_name: &str, max_fires: usize, cooldown_secs: u64) -> String {
    format!(
        "{prefix}\n⚠️ 自動規則〈{name}〉短時間內連續觸發 {max_fires} 次，已暫停 {cooldown_secs} 秒（自動保護）。",
        prefix = crate::decision_notify::reason_prefix(DecisionSource::Autopilot),
        name = crate::goal_state::xml_escape(rule_name),
    )
}

// ── Outbound ────────────────────────────────────────────────────

/// Push the circuit-open symptom notice + "暫停這條規則" button to the
/// rule's resolved target agent's `[proactive]` destination.
///
/// Best-effort and fail-soft by construction, matching every other notify
/// module: no resolvable target agent, no `[proactive]` destination, or no
/// bot token all degrade to a logged no-op — never an error the caller must
/// handle. Callers (`autopilot_engine.rs`) additionally run this detached
/// (`tokio::spawn`) so a slow/unreachable channel API can never delay
/// processing of the next event.
pub async fn notify_circuit_open(
    home_dir: &Path,
    rule: &AutopilotRuleRow,
    max_fires: usize,
    cooldown_secs: u64,
) {
    let Some(agent_id) = resolve_rule_target_agent(rule) else {
        info!(
            rule = %rule.name,
            "autopilot-notify: rule has no resolvable target agent — skipping circuit-open push"
        );
        return;
    };
    let Some((channel, chat_id)) = crate::goal_notify::agent_notify_target(home_dir, &agent_id)
    else {
        info!(
            rule = %rule.name, agent = %agent_id,
            "autopilot-notify: agent has no [proactive] destination — skipping circuit-open push"
        );
        return;
    };
    let body = circuit_open_body(&rule.name, max_fires, cooldown_secs);
    let link = crate::deep_link::deep_link(home_dir, crate::deep_link::DeepLinkKind::Autopilot, &rule.id);
    const CIRCUIT_OPEN_NO_BUTTON_HINT: &str = "如需暫停此規則，請至儀表板的自動規則頁操作。";
    // W3-1 (D5): an "a rule tripped" card is a needs-to-know, not a
    // needs-to-know-*now* — deferred to the handback (buttons intact) rather
    // than interrupting a human mid-conversation.
    if crate::goal_notify::takeover_defer(
        home_dir,
        &agent_id,
        &channel,
        &chat_id,
        crate::notify_governance::NotifyLevel::Confirm,
        "autopilot.circuit_open",
        &body,
        Some((
            DecisionSource::Autopilot,
            rule.id.as_str(),
            link.as_deref(),
            CIRCUIT_OPEN_NO_BUTTON_HINT,
        )),
    )
    .is_some()
    {
        return;
    }
    let Some(token) = crate::goal_notify::channel_token(home_dir, &agent_id, &channel).await
    else {
        info!(rule = %rule.name, %channel, "autopilot-notify: no bot token — skipping circuit-open push");
        return;
    };

    let http = reqwest::Client::new();
    let card = DecisionCard {
        source: DecisionSource::Autopilot,
        decision_id: &rule.id,
        body: &body,
        link: link.as_deref(),
        no_button_hint: CIRCUIT_OPEN_NO_BUTTON_HINT,
    };
    crate::decision_notify::deliver(home_dir, &http, &channel, &token, &chat_id, &card).await;
}

// ── Inbound ─────────────────────────────────────────────────────

/// True when a rule's push would have been delivered to exactly
/// `(channel, channel_user_id)`. Unlike [`crate::approval_notify`]'s
/// `ApprovalRecord`, an `AutopilotRuleRow` carries no `notify_channel`/
/// `notify_chat_id` of its own — but the destination is a pure function of
/// the rule's resolved target agent's `[proactive]` config, so it is
/// re-derived here rather than persisted. Exact equality only (project
/// convention: no substring shortcuts for a security/routing decision).
fn delivered_targets(home_dir: &Path, rule: &AutopilotRuleRow) -> Vec<(String, String)> {
    resolve_rule_target_agent(rule)
        .and_then(|agent_id| crate::goal_notify::agent_notify_target(home_dir, &agent_id))
        .into_iter()
        .collect()
}

/// Handle an autopilot-pause button press from a channel.
///
/// Returns:
/// - `None` — `action_data` is not an autopilot-pause action (the dispatcher
///   falls through to its other handlers).
/// - `Some(Ok(msg))` — the rule was paused (or already was); `msg` is the
///   zh-TW ack to show.
/// - `Some(Err(msg))` — an error/refusal to show the presser.
pub async fn decide_from_channel(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    action_data: &str,
) -> Option<Result<String, String>> {
    let action = crate::decision_action::parse(action_data)?;
    if action.source != DecisionSource::Autopilot {
        return None;
    }
    Some(apply_pause(home_dir, channel, channel_user_id, &action.id).await)
}

/// Apply an already-decoded pause to `autopilot.db`. Called by the unified
/// inbound router as well as this module's own thin wrapper.
pub(crate) async fn apply_pause(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    rule_id: &str,
) -> Result<String, String> {
    let store = AutopilotStore::open(home_dir).map_err(|e| format!("開啟自動化規則資料庫失敗：{e}"))?;
    let Some(rule) = store.get_rule(rule_id).await.map_err(|e| e.to_string())? else {
        return Err("找不到此規則（可能已被刪除）".into());
    };
    if !rule.enabled {
        return Ok(format!("規則「{}」已是暫停狀態。", rule.name));
    }

    // ── authorization ───────────────────────────────────────
    // Same posture as `approval_notify::apply_decision`: a mapped, Active
    // dashboard user decides by role (Admin/Manager); with no identity system
    // configured at all, only a press from the exact destination the notice
    // was delivered to is honoured. Fail-closed everywhere else.
    let auth = authorize_press(
        mapped_role(home_dir, channel, channel_user_id),
        identity_system_active(home_dir),
        destination_matches_any(&delivered_targets(home_dir, &rule), channel, channel_user_id),
    );
    if auth != PressAuth::Allow {
        return Err(refusal_text(auth, "暫停規則"));
    }

    // ── apply ────────────────────────────────────────────────
    store.update_rule(rule_id, &json!({ "enabled": false })).await?;

    let _ = store
        .append_history(&AutopilotHistoryRow {
            id: uuid::Uuid::new_v4().to_string(),
            rule_id: rule_id.to_string(),
            rule_name: rule.name.clone(),
            triggered_at: chrono::Utc::now().to_rfc3339(),
            result: "paused_by_human".into(),
            details: Some(format!("人工從 {channel} 暫停此規則（自動保護跳閘後）")),
        })
        .await;

    if let Ok(ts) = TaskStore::open(home_dir) {
        append_activity(
            &ts,
            "autopilot.rule_paused",
            "autopilot",
            None,
            &format!(
                "人工暫停自動規則「{}」（來自 {channel}:{channel_user_id}）",
                rule.name
            ),
        )
        .await;
    }

    // Best-effort, detached card collapse — an edit is cosmetic and must not
    // delay or fail a decision that is already durable in `autopilot.db` by
    // this point (mirrors `goal_notify::spawn_goal_task_collapse`'s rationale).
    spawn_pause_collapse(home_dir.to_path_buf(), rule.clone(), channel.to_string(), channel_user_id.to_string());

    Ok(format!("已暫停規則「{}」。", rule.name))
}

/// Spawn a best-effort, fire-and-forget attempt to retire this rule's
/// circuit-trip cards. Detached so a slow or unreachable channel API can
/// never delay or fail the pause that already landed in `autopilot.db`.
fn spawn_pause_collapse(
    home_dir: std::path::PathBuf,
    rule: AutopilotRuleRow,
    channel: String,
    channel_user_id: String,
) {
    tokio::spawn(async move {
        let Some(agent_id) = resolve_rule_target_agent(&rule) else { return };
        let http = reqwest::Client::new();
        let decider = crate::decision_card::resolve_decider_name(&home_dir, &channel, &channel_user_id);
        let summary = format!("⚠️ 自動規則：{}", duduclaw_core::truncate_chars(&rule.name, 60));
        let home = home_dir.clone();
        crate::decision_card::collapse_all(
            &home_dir,
            &http,
            DecisionSource::Autopilot.namespace(),
            &rule.id,
            &summary,
            crate::decision_notify::settled_verb(DecisionSource::Autopilot, DecisionAct::Pause),
            decider.as_deref(),
            move |ch: String| {
                let home = home.clone();
                let agent = agent_id.clone();
                async move { crate::goal_notify::channel_token(&home, &agent, &ch).await }
            },
            Some((channel.as_str(), channel_user_id.as_str())),
        )
        .await;
    });
}

/// Best-effort Activity Feed append (telemetry, never control flow).
async fn append_activity(
    store: &TaskStore,
    event_type: &str,
    agent_id: &str,
    task_id: Option<&str>,
    summary: &str,
) {
    let row = ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.to_string(),
        task_id: task_id.map(str::to_string),
        summary: summary.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: None,
    };
    if let Err(e) = store.append_activity(&row).await {
        tracing::debug!(error = %e, "autopilot-notify: activity append failed (non-fatal)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::models::UserRole;

    fn rule(action: serde_json::Value, conditions: serde_json::Value) -> AutopilotRuleRow {
        AutopilotRuleRow {
            id: "r-1".into(),
            name: "測試規則".into(),
            enabled: true,
            trigger_event: "task_created".into(),
            conditions: conditions.to_string(),
            action: action.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
            sequence: None,
            metadata: None,
        }
    }

    // ── target resolution ────────────────────────────────────

    #[test]
    fn resolves_target_agent_from_delegate_action() {
        let r = rule(json!({"type": "delegate", "target_agent": "bruno", "prompt": "p"}), json!(null));
        assert_eq!(resolve_rule_target_agent(&r), Some("bruno".to_string()));
    }

    #[test]
    fn resolves_target_agent_from_run_skill_action() {
        let r = rule(
            json!({"type": "run_skill", "target_agent": "alice", "skill_name": "s"}),
            json!(null),
        );
        assert_eq!(resolve_rule_target_agent(&r), Some("alice".to_string()));
    }

    #[test]
    fn resolves_target_agent_from_conditions_when_action_has_none() {
        let r = rule(
            json!({"type": "notify", "channel": "telegram", "chat_id": "1", "text": "hi"}),
            json!({"all": [{"field": "agent_id", "op": "eq", "value": "bruno"}]}),
        );
        assert_eq!(resolve_rule_target_agent(&r), Some("bruno".to_string()));
    }

    #[test]
    fn resolves_target_agent_from_nested_any_conditions() {
        let r = rule(
            json!({"type": "notify"}),
            json!({"any": [
                {"field": "priority", "op": "eq", "value": "high"},
                {"field": "agent_id", "op": "in", "value": ["carol", "dave"]}
            ]}),
        );
        assert_eq!(resolve_rule_target_agent(&r), Some("carol".to_string()));
    }

    #[test]
    fn resolves_target_agent_from_task_assigned_to_condition() {
        // `TaskCreated`/`TaskUpdated` only expose `event` and `task`, so a
        // task rule can only scope on the assignee — that must resolve as the
        // notify target, or a circuit-breaker trip has nowhere to go.
        let r = rule(
            json!({"type": "notify", "channel": "telegram", "chat_id": "1", "text": "hi"}),
            json!({"all": [{"field": "task.assigned_to", "op": "eq", "value": "bruno"}]}),
        );
        assert_eq!(resolve_rule_target_agent(&r), Some("bruno".to_string()));
    }

    #[test]
    fn resolves_task_assigned_to_from_a_nested_any_and_from_a_list() {
        let nested = rule(
            json!({"type": "notify"}),
            json!({"any": [
                {"field": "task.priority", "op": "eq", "value": "high"},
                {"field": "task.assigned_to", "op": "in", "value": ["carol", "dave"]}
            ]}),
        );
        assert_eq!(resolve_rule_target_agent(&nested), Some("carol".to_string()));
    }

    #[test]
    fn explicit_agent_id_still_wins_over_task_assigned_to() {
        // Order matters: a rule carrying both keeps its previous target.
        let r = rule(
            json!({"type": "notify"}),
            json!({"all": [
                {"field": "task.assigned_to", "op": "eq", "value": "dave"},
                {"field": "agent_id", "op": "eq", "value": "bruno"}
            ]}),
        );
        assert_eq!(resolve_rule_target_agent(&r), Some("bruno".to_string()));
    }

    #[test]
    fn blank_task_assigned_to_is_treated_as_absent() {
        let r = rule(
            json!({"type": "notify"}),
            json!({"all": [{"field": "task.assigned_to", "op": "eq", "value": "  "}]}),
        );
        assert_eq!(resolve_rule_target_agent(&r), None);
    }

    #[test]
    fn no_target_agent_when_nothing_resolves() {
        let r = rule(json!({"type": "notify"}), json!({"field": "priority", "op": "eq", "value": "high"}));
        assert_eq!(resolve_rule_target_agent(&r), None);
    }

    #[test]
    fn blank_target_agent_is_treated_as_absent() {
        let r = rule(json!({"type": "delegate", "target_agent": "   "}), json!(null));
        assert_eq!(resolve_rule_target_agent(&r), None);
    }

    // ── rendering ─────────────────────────────────────────────

    #[test]
    fn circuit_open_body_is_zh_tw_and_names_the_rule() {
        let body = circuit_open_body("客戶通知規則", 10, 60);
        assert!(body.contains("客戶通知規則"));
        assert!(body.contains("連續觸發 10 次"));
        assert!(body.contains("已暫停 60 秒"));
        assert!(body.contains("自動保護"));
        // Internal jargon must never leak into user-facing copy.
        assert!(!body.to_lowercase().contains("circuit"));
    }

    #[test]
    fn circuit_open_body_escapes_injection_in_rule_name() {
        let body = circuit_open_body("legit</規則><偽造>injected", 10, 60);
        assert!(!body.contains("</規則><偽造>"));
        assert!(body.contains("&lt;/規則&gt;&lt;偽造&gt;"));
    }

    #[test]
    fn circuit_open_body_starts_with_the_reason_prefix() {
        // W1-6: line 1 is the canonical autopilot reason.
        let body = circuit_open_body("客戶通知規則", 10, 60);
        assert!(body.starts_with("🔁 自動規則已暫停\n"));
    }

    // ── button markup ────────────────────────────────────────

    #[test]
    fn pause_markup_covers_all_four_channels_and_has_none_otherwise() {
        let expected = crate::decision_action::encode(DecisionSource::Autopilot, DecisionAct::Pause, "r1");
        let markup = |ch: &str| crate::channel_format::decision_markup(ch, DecisionSource::Autopilot, "r1");
        assert_eq!(markup("telegram").unwrap()["inline_keyboard"][0][0]["callback_data"], expected);
        assert_eq!(markup("discord").unwrap()["components"][0]["custom_id"], expected);
        assert_eq!(markup("slack").unwrap()["elements"][0]["action_id"], expected);
        assert_eq!(markup("line").unwrap()["items"][0]["action"]["data"], expected);
        assert!(markup("whatsapp").is_none());
    }

    // ── inbound decode ────────────────────────────────────────

    #[tokio::test]
    async fn decide_from_channel_ignores_non_pause_actions() {
        let dir = tempfile::tempdir().unwrap();
        for data in ["garbage", "duduclaw:approval_ok:a1", "duduclaw:goal_retry:t1", "duduclaw:autopilot_pause:"] {
            assert!(
                decide_from_channel(dir.path(), "telegram", "u1", data).await.is_none(),
                "should not claim {data}"
            );
        }
    }

    #[tokio::test]
    async fn pause_button_disables_the_rule_and_records_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = AutopilotStore::open(dir.path()).unwrap();
        let row = AutopilotRuleRow {
            id: "r-42".into(),
            name: "迴圈規則".into(),
            enabled: true,
            trigger_event: "task_created".into(),
            conditions: json!({"all": [{"field": "agent_id", "op": "eq", "value": "bruno"}]}).to_string(),
            action: json!({"type": "notify", "channel": "telegram", "chat_id": "1", "text": "hi"}).to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
            sequence: None,
            metadata: None,
        };
        store.insert_rule(&row).await.unwrap();

        // No users.db, no agent [proactive] config at all ⇒ destination never
        // matches ⇒ solo-operator branch also refuses (no destination proof).
        let out = decide_from_channel(dir.path(), "telegram", "555", &crate::decision_action::encode(DecisionSource::Autopilot, DecisionAct::Pause, "r-42"))
            .await
            .unwrap();
        assert!(out.is_err(), "no destination proof ⇒ must refuse: {out:?}");

        let after = store.get_rule("r-42").await.unwrap().unwrap();
        assert!(after.enabled, "an unauthorized press must never disable the rule");
    }

    #[tokio::test]
    async fn authorized_press_pauses_the_rule_and_writes_history() {
        let dir = tempfile::tempdir().unwrap();
        // The rule's resolved target agent's [proactive] destination is
        // exactly the pressing account — the solo-operator authorization
        // branch (no users.db at all) honours a press from there.
        let agent_dir = dir.path().join("agents").join("bruno");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            "[proactive]\nnotify_channel = \"telegram\"\nnotify_chat_id = \"555\"\n",
        )
        .unwrap();

        let store = AutopilotStore::open(dir.path()).unwrap();
        let row = AutopilotRuleRow {
            id: "r-ok".into(),
            name: "迴圈規則".into(),
            enabled: true,
            trigger_event: "task_created".into(),
            conditions: json!({"all": [{"field": "agent_id", "op": "eq", "value": "bruno"}]}).to_string(),
            action: json!({"type": "notify", "channel": "telegram", "chat_id": "1", "text": "hi"}).to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
            sequence: None,
            metadata: None,
        };
        store.insert_rule(&row).await.unwrap();

        let out = decide_from_channel(
            dir.path(),
            "telegram",
            "555",
            &crate::decision_action::encode(DecisionSource::Autopilot, DecisionAct::Pause, "r-ok"),
        )
        .await
        .unwrap();
        assert!(out.is_ok(), "expected pause to land: {out:?}");

        let after = store.get_rule("r-ok").await.unwrap().unwrap();
        assert!(!after.enabled, "rule must be disabled after an authorized pause");

        let history = store.list_history(Some("r-ok"), 10).await.unwrap();
        assert!(
            history.iter().any(|h| h.result == "paused_by_human"),
            "expected a paused_by_human history row: {history:?}"
        );
    }

    #[tokio::test]
    async fn press_from_an_unrelated_account_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agents").join("bruno");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            "[proactive]\nnotify_channel = \"telegram\"\nnotify_chat_id = \"555\"\n",
        )
        .unwrap();

        let store = AutopilotStore::open(dir.path()).unwrap();
        let row = AutopilotRuleRow {
            id: "r-bad".into(),
            name: "迴圈規則".into(),
            enabled: true,
            trigger_event: "task_created".into(),
            conditions: json!({"all": [{"field": "agent_id", "op": "eq", "value": "bruno"}]}).to_string(),
            action: json!({"type": "notify"}).to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
            sequence: None,
            metadata: None,
        };
        store.insert_rule(&row).await.unwrap();

        // Different account than the one the notice would have gone to.
        let out = decide_from_channel(
            dir.path(),
            "telegram",
            "999",
            &crate::decision_action::encode(DecisionSource::Autopilot, DecisionAct::Pause, "r-bad"),
        )
        .await
        .unwrap();
        assert!(out.is_err(), "an unrelated account must not pause the rule");
        let after = store.get_rule("r-bad").await.unwrap().unwrap();
        assert!(after.enabled, "refused press must never disable the rule");
    }

    #[tokio::test]
    async fn pause_button_is_a_noop_when_already_paused() {
        let dir = tempfile::tempdir().unwrap();
        let store = AutopilotStore::open(dir.path()).unwrap();
        let row = AutopilotRuleRow {
            id: "r-7".into(),
            name: "已暫停規則".into(),
            enabled: false,
            trigger_event: "task_created".into(),
            conditions: "null".into(),
            action: json!({"type": "notify"}).to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
            sequence: None,
            metadata: None,
        };
        store.insert_rule(&row).await.unwrap();

        let out = decide_from_channel(dir.path(), "telegram", "1", &crate::decision_action::encode(DecisionSource::Autopilot, DecisionAct::Pause, "r-7"))
            .await
            .unwrap();
        assert_eq!(out, Ok("規則「已暫停規則」已是暫停狀態。".to_string()));
    }

    #[tokio::test]
    async fn unknown_rule_id_is_reported_not_paused() {
        let dir = tempfile::tempdir().unwrap();
        let out = decide_from_channel(dir.path(), "telegram", "1", "duduclaw:autopilot_pause:does-not-exist")
            .await
            .unwrap();
        assert!(out.is_err());
    }

    // ── authorization reuse sanity ────────────────────────────
    // `autopilot_notify` deliberately reuses `approval_notify::authorize_press`
    // rather than re-implementing the role/destination matrix — this just
    // pins the import compiles and behaves as documented from this module.

    #[test]
    fn authorize_press_reused_from_approval_notify() {
        assert_eq!(authorize_press(Some(UserRole::Admin), true, false), PressAuth::Allow);
        assert_eq!(authorize_press(None, true, true), PressAuth::DenyUnknown);
        assert_eq!(authorize_press(None, false, true), PressAuth::Allow);
    }
}
