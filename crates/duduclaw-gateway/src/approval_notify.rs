//! Channel push + inline decision for the generic [`ApprovalBroker`].
//!
//! ## The gap this closes
//!
//! `ApprovalBroker` (`approvals.db`) is the ONE HITL primitive: MCP
//! install-class gates, ActionGuard irreversibility escalations, capability
//! grants, skill activation, knowledge quarantine and autopilot actions all
//! `request()` on it and then block on `await_decision()`, where a TTL lapse
//! counts as DENY (fail-closed). Until this module, the ONLY surface that ever
//! showed a pending row was the dashboard (`approvals.list` / `approvals.decide`
//! RPC). Nothing was ever pushed to a messaging channel.
//!
//! The two sibling notifiers look like they cover it but do not:
//!
//! - `install_notify.rs` pushes the **`install_requests` store** (the dashboard
//!   「安裝簽核申請」 workflow, a completely separate SQLite table with its own
//!   manager→admin two-stage gate). It never reads `approvals.db`.
//! - `goal_notify.rs` pushes goal tasks and the `goal_kickoff` approval only.
//!
//! So an operator who triggered a skill-hub install from Telegram got a
//! `mcp_install` row in `approvals.db`, no message anywhere, and an automatic
//! denial 5 minutes later. This module is the missing wire.
//!
//! ## Shape
//!
//! - **Outbound** [`notify_new_approval`] — called from
//!   [`ApprovalBroker::request`] itself, so every action kind is covered by
//!   construction rather than by remembering to call it at each site.
//!   [`notify_reminder`] sends the ⅔-TTL "about to auto-deny" nudge. Both go
//!   out through the shared [`crate::decision_notify::deliver`] path.
//! - **Inbound** [`apply_decision`] — reached from the unified router
//!   ([`crate::decision_notify::route_press`]), authorized by the shared
//!   matrix and applied via `ApprovalBroker::decide`.
//!
//! Everything is best-effort and fail-soft: a missing token / unreachable
//! destination is logged, never panics, and never blocks the approval itself.

use std::path::Path;

use tracing::info;

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus, SimulationNarrative};
use crate::decision_action::{DecisionAct, DecisionSource};
use crate::decision_notify::{
    approver_links, authorize_press, destination_matches_any, identity_system_active, mapped_role,
    origin_target, refusal_text, resolve_targets, DecisionCard, PressAuth,
};
use crate::task_store::{ActivityRow, TaskStore};

/// Max chars of the (partly external) summary rendered into a channel message.
/// CJK-safe via `truncate_chars` — never a raw byte slice (convention #1).
const SUMMARY_MAX_CHARS: usize = 300;

// ── Message rendering ───────────────────────────────────────────

/// Human-facing zh-TW label for an `action_kind`. Internal identifiers must not
/// leak into an end-user message (project convention: user-facing copy hides
/// implementation detail), so anything unmapped renders as a generic phrase.
pub(crate) fn zh_action_kind(kind: &str) -> &str {
    match kind {
        "mcp_install" => "安裝新技能／工具",
        "mcp_call" => "執行高風險工具",
        "mcp_tool" => "執行高風險工具",
        "capability_grant" => "取得額外權限",
        "skill_create" => "建立新技能",
        "skill_activation" => "啟用技能",
        "knowledge_quarantine" => "寫入待審知識",
        "expert_hooks_enable" => "啟用專家包自動化",
        "autopilot_action" => "執行自動化規則動作",
        "topology_reroute" => "調整團隊分工",
        "induced_rule" => "新增自動化規則",
        "bus_task" => "執行委派任務",
        "browser_action" => "操作瀏覽器",
        _ => "執行需要核可的動作",
    }
}

/// Render the deadline as a local `HH:MM` clock plus the remaining minutes —
/// an ISO timestamp is not something a person reads on a phone.
fn deadline_phrase(rec: &ApprovalRecord) -> String {
    let mins = (rec.ttl_seconds as f64 / 60.0).ceil() as i64;
    match rec.deadline_rfc3339() {
        Some(iso) => match chrono::DateTime::parse_from_rfc3339(&iso) {
            Ok(dt) => format!(
                "{} 前（約 {mins} 分鐘）",
                dt.with_timezone(&chrono::Local).format("%H:%M")
            ),
            Err(_) => format!("約 {mins} 分鐘內"),
        },
        None => format!("約 {mins} 分鐘內"),
    }
}

/// D2 (arXiv:2603.11677): the "若核准，接下來預計：…" forward-trajectory
/// line for this approval, or an empty string when there is nothing to show
/// (no [`ApprovalRecord::simulation`], or a narrative with no
/// sentence-shaped/risk content — [`SimulationNarrative::as_trajectory`]
/// degrades to `None` in both cases). Pure UX enhancement: never fails the
/// push, only omits the line — matches the task's degrade contract ("模擬產
/// 生失敗時...照舊只出按鈕，不阻塞推播").
fn trajectory_line(rec: &ApprovalRecord) -> String {
    rec.simulation
        .as_ref()
        .map(SimulationNarrative::from_json)
        .and_then(|n| n.as_trajectory())
        .map(|t| format!("\n{t}"))
        .unwrap_or_default()
}

/// The zh-TW body of a pending-approval push.
///
/// L4 (injection hardening): `rec.summary` is caller-supplied free text
/// (e.g. a skill/tool description) and `trajectory_line`'s output ultimately
/// derives from `ApprovalRecord::simulation`, an LLM-narrated field — both
/// are untrusted text folded into a channel message. `pending_summary_for_channel`
/// (`approval.rs`, out of scope for this change) already `xml_escape`s the
/// equivalent fields for its own message shape; this function mirrors that
/// treatment for the button-push body so a crafted summary/trajectory can't
/// forge a fake section boundary in the rendered message.
pub(crate) fn approval_body(rec: &ApprovalRecord, reminder: bool) -> String {
    let head = if reminder {
        "⏰ 這項核可快到期了，逾時會自動拒絕"
    } else {
        "🔔 需要您的確認"
    };
    format!(
        "{prefix}\n{head}\n\
         AI 員工：{agent}\n\
         想做的事：{kind}\n\
         內容：{summary}{trajectory}\n\
         期限：{deadline}未回覆將自動拒絕\n\
         編號：{id}",
        prefix = crate::decision_notify::reason_prefix(DecisionSource::Approval),
        agent = crate::goal_state::xml_escape(&rec.agent_id),
        kind = zh_action_kind(&rec.action_kind),
        summary = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.summary, SUMMARY_MAX_CHARS)),
        trajectory = crate::goal_state::xml_escape(&trajectory_line(rec)),
        deadline = deadline_phrase(rec),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

// ── Outbound ────────────────────────────────────────────────────

/// Push a freshly-filed approval to the first reachable destination in the
/// chain. Returns the `(channel, chat_id)` actually delivered to (persisted by
/// the broker so the reminder and the inbound press can use it), or `None`
/// when nothing was reachable.
pub async fn notify_new_approval(
    home_dir: &Path,
    rec: &ApprovalRecord,
) -> Option<(String, String)> {
    push(home_dir, rec, false).await
}

/// Push the ⅔-TTL "about to auto-deny" reminder. Prefers the destination the
/// original push landed on so the nudge follows the same conversation.
pub async fn notify_reminder(home_dir: &Path, rec: &ApprovalRecord) -> Option<(String, String)> {
    push(home_dir, rec, true).await
}

async fn push(home_dir: &Path, rec: &ApprovalRecord, reminder: bool) -> Option<(String, String)> {
    // A reminder retraces the delivered destination; a first push resolves the
    // chain. (`notify_channel` is only ever set after a successful delivery.)
    let targets = match (
        reminder,
        rec.notify_channel.as_deref(),
        rec.notify_chat_id.as_deref(),
    ) {
        (true, Some(ch), Some(id)) if !ch.is_empty() && !id.is_empty() => {
            vec![(ch.to_string(), id.to_string())]
        }
        _ => resolve_targets(
            origin_target(),
            crate::goal_notify::agent_notify_target(home_dir, &rec.agent_id),
            approver_links(home_dir),
        ),
    };
    if targets.is_empty() {
        return None;
    }

    let http = reqwest::Client::new();
    let body = approval_body(rec, reminder);
    // A clickable deep link to the unified inbox — `None` when no dashboard
    // base URL is configured/derivable, in which case the rendered text stays
    // exactly as it reads without this feature.
    let link = crate::deep_link::deep_link(home_dir, crate::deep_link::DeepLinkKind::Approval, rec.id.as_str());
    let card = DecisionCard {
        source: DecisionSource::Approval,
        decision_id: rec.id.as_str(),
        body: &body,
        link: link.as_deref(),
        no_button_hint: "此通道無法顯示按鈕，請至儀表板的待辦決定頁同意或拒絕，\
                         或改用 Telegram／Slack／Discord／LINE 直接按按鈕。",
    };
    let mut delivered: Option<(String, String)> = None;

    for (channel, chat_id) in targets {
        let Some(token) =
            crate::goal_notify::channel_token(home_dir, &rec.agent_id, &channel).await
        else {
            info!(approval_id = %rec.id, %channel, "approval push: no bot token; skipping");
            continue;
        };
        let ok = crate::decision_notify::deliver(home_dir, &http, &channel, &token, &chat_id, &card).await;
        if ok && delivered.is_none() {
            delivered = Some((channel, chat_id));
        }
    }
    delivered
}

// ── Inbound ─────────────────────────────────────────────────────

/// The destinations this approval was (or would have been) delivered to, for
/// the destination branch of [`authorize_press`].
///
/// The broker persists the destination a successful push landed on, so unlike
/// the sources that re-derive it, this is an actual delivery record. `None`
/// before any push has succeeded ⇒ no destination authority exists yet.
pub(crate) fn delivered_targets(rec: &ApprovalRecord) -> Vec<(String, String)> {
    match (rec.notify_channel.as_deref(), rec.notify_chat_id.as_deref()) {
        (Some(ch), Some(id)) if !ch.is_empty() && !id.is_empty() => {
            vec![(ch.to_string(), id.to_string())]
        }
        _ => Vec::new(),
    }
}

/// Handle an approval button press from a channel.
///
/// Returns:
/// - `None` — `action_data` is not a generic approval action (the dispatcher
///   falls through to its other handlers).
/// - `Some(Ok(msg))` — decision applied; `msg` is the zh-TW ack to show.
/// - `Some(Err(msg))` — an error/refusal to show the presser.
pub async fn decide_from_channel(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    action_data: &str,
) -> Option<Result<String, String>> {
    let action = crate::decision_action::parse(action_data)?;
    if action.source != DecisionSource::Approval {
        return None;
    }
    Some(apply_decision(home_dir, channel, channel_user_id, &action.id, action.approve()).await)
}

/// Apply an already-decoded approve/deny to `approvals.db`. Called by the
/// unified inbound router as well as this module's own thin wrapper.
pub(crate) async fn apply_decision(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    approval_id: &str,
    approve: bool,
) -> Result<String, String> {
    let broker = ApprovalBroker::open(home_dir).map_err(|e| format!("開啟審批資料庫失敗：{e}"))?;
    let id = ApprovalId::from(approval_id.to_string());
    let Some(rec) = broker.get(&id).await.map_err(|e| e.to_string())? else {
        return Err("找不到這筆核可（可能已過期並被清除）".into());
    };
    if rec.status.is_terminal() {
        return Ok(match rec.status {
            ApprovalStatus::Approved => "這項要求先前已同意。".into(),
            ApprovalStatus::Denied => "這項要求先前已拒絕。".into(),
            _ => "這項要求逾時未決，已自動拒絕，請重新發起。".to_string(),
        });
    }

    // ── authorization ───────────────────────────────────────
    // One matrix for every decision source (see `decision_notify`): a mapped
    // Active dashboard user decides by role; with no identity system
    // configured at all, only a press from the exact account the approval was
    // delivered to is honoured. Fail-closed everywhere else.
    let auth = authorize_press(
        mapped_role(home_dir, channel, channel_user_id),
        identity_system_active(home_dir),
        destination_matches_any(&delivered_targets(&rec), channel, channel_user_id),
    );
    if auth != PressAuth::Allow {
        return Err(refusal_text(auth, "核准"));
    }

    // ── decide ──────────────────────────────────────────────
    let decided_by = format!("channel:{channel}:{channel_user_id}");
    broker.decide(&id, approve, &decided_by).await?;

    // Activity Feed (telemetry, never control flow).
    if let Ok(store) = TaskStore::open(home_dir) {
        let verb = if approve { "同意" } else { "拒絕" };
        let row = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "approval.channel_decision".to_string(),
            agent_id: rec.agent_id.clone(),
            task_id: None,
            summary: format!(
                "人工{verb}「{}」（審批 {}，來自 {channel}）",
                zh_action_kind(&rec.action_kind),
                duduclaw_core::truncate_chars(rec.id.as_str(), 8)
            ),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: None,
        };
        if let Err(e) = store.append_activity(&row).await {
            tracing::debug!(error = %e, "approval-notify: activity append failed (non-fatal)");
        }
    }

    // Best-effort, detached card collapse — an edit is cosmetic and must not
    // delay or fail a decision that is already durable in `approvals.db` by
    // this point (see `goal_notify::spawn_goal_task_collapse`'s doc comment
    // for the same rationale).
    //
    // An install-class refusal reads softer ("已婉拒") than a high-risk one
    // ("已拒絕"); the acknowledgement and the collapsed card take that verb
    // from the same place, so a person is told the same word twice.
    let card_verb = settled_verb_for(&rec, approve);
    spawn_approval_collapse(home_dir.to_path_buf(), rec.clone(), channel.to_string(), channel_user_id.to_string(), card_verb);

    Ok(format!(
        "{} {}：{}",
        card_verb.emoji(),
        card_verb.label(),
        zh_action_kind(&rec.action_kind)
    ))
}

/// The settled verb for this approval. Install-class rows carry the softer
/// refusal wording; everything else is the generic high-risk pair.
fn settled_verb_for(rec: &ApprovalRecord, approve: bool) -> crate::decision_card::DecisionVerb {
    let source = if rec.action_kind == "mcp_install" {
        DecisionSource::Install
    } else {
        DecisionSource::Approval
    };
    let act = if approve { DecisionAct::Approve } else { DecisionAct::Deny };
    crate::decision_notify::settled_verb(source, act)
}

/// The one-line identifying summary regenerated fresh from an approval
/// record — shared by every collapse path (channel press and dashboard
/// decision alike) so the collapsed card always reads the same regardless of
/// where the decision was made.
fn approval_collapse_summary(rec: &ApprovalRecord) -> String {
    format!(
        "🔔 {}：{}",
        zh_action_kind(&rec.action_kind),
        duduclaw_core::truncate_chars(&rec.summary, SUMMARY_MAX_CHARS),
    )
}

/// Spawn a best-effort, fire-and-forget attempt to retire this approval's
/// channel cards. Detached so a slow or unreachable channel API can never
/// delay a decision that is already durable in `approvals.db`.
///
/// Retires EVERY card, not just the presser's: an approval with no
/// originating conversation fans out to all approvers, and leaving the other
/// copies showing live buttons is the stale-card problem in-place collapse
/// exists to remove.
fn spawn_approval_collapse(
    home_dir: std::path::PathBuf,
    rec: ApprovalRecord,
    channel: String,
    channel_user_id: String,
    verb: crate::decision_card::DecisionVerb,
) {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let decider = crate::decision_card::resolve_decider_name(&home_dir, &channel, &channel_user_id);
        let summary = approval_collapse_summary(&rec);
        let home = home_dir.clone();
        let agent = rec.agent_id.clone();
        crate::decision_card::collapse_all(
            &home_dir,
            &http,
            DecisionSource::Approval.namespace(),
            rec.id.as_str(),
            &summary,
            verb,
            decider.as_deref(),
            move |ch: String| {
                let home = home.clone();
                let agent = agent.clone();
                async move { crate::goal_notify::channel_token(&home, &agent, &ch).await }
            },
            Some((channel.as_str(), channel_user_id.as_str())),
        )
        .await;
    });
}

/// Spawn a best-effort, fire-and-forget attempt to retire this approval's
/// channel cards after a **dashboard** decision (`handlers.rs`'s
/// `approvals.decide` RPC — H1 of the unified-decision hand-off, 07
/// §6). Mirrors [`spawn_approval_collapse`] but the decider is a resolved
/// dashboard display name rather than a channel identity, and there is no
/// channel destination to fall back to on a total collapse miss — the
/// dashboard RPC already carries its own acknowledgement, so a miss stays
/// silent rather than pushing a new message anywhere.
pub(crate) fn spawn_dashboard_collapse(
    home_dir: std::path::PathBuf,
    rec: ApprovalRecord,
    approve: bool,
    decider_name: Option<String>,
) {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let verb = settled_verb_for(&rec, approve);
        let summary = approval_collapse_summary(&rec);
        let home = home_dir.clone();
        let agent = rec.agent_id.clone();
        crate::decision_card::collapse_all(
            &home_dir,
            &http,
            DecisionSource::Approval.namespace(),
            rec.id.as_str(),
            &summary,
            verb,
            decider_name.as_deref(),
            move |ch: String| {
                let home = home.clone();
                let agent = agent.clone();
                async move { crate::goal_notify::channel_token(&home, &agent, &ch).await }
            },
            None,
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalStatus;
    use chrono::Utc;
    use serde_json::json;

    fn rec(kind: &str) -> ApprovalRecord {
        ApprovalRecord {
            id: ApprovalId::from("ap-123456789".to_string()),
            agent_id: "sales-bot".into(),
            action_kind: kind.into(),
            summary: "安裝 skill：客戶名單整理".into(),
            payload: json!({}),
            status: ApprovalStatus::Pending,
            created_at: Utc::now().to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: 300,
            notify_channel: None,
            notify_chat_id: None,
            reminded_at: None,
            simulation: None,
        }
    }

    // NOTE: destination resolution (`parse_origin`/`resolve_targets`) and the
    // authorization matrix (`authorize_press`/destination matching) are shared
    // by every decision source and are tested in `decision_notify`. What stays
    // here is what is specific to `approvals.db`.

    // ── H1: settled verb + summary feeding both collapse paths ─────
    // (channel press's `spawn_approval_collapse` and the dashboard's
    // `spawn_dashboard_collapse`, `handlers.rs`'s `approvals.decide` RPC)

    #[test]
    fn settled_verb_for_install_deny_reads_softer_than_generic_deny() {
        assert_eq!(settled_verb_for(&rec("mcp_install"), false), crate::decision_card::DecisionVerb::DeclinedInstall);
        assert_eq!(settled_verb_for(&rec("mcp_call"), false), crate::decision_card::DecisionVerb::Denied);
    }

    #[test]
    fn settled_verb_for_approve_is_always_approved_regardless_of_kind() {
        assert_eq!(settled_verb_for(&rec("mcp_install"), true), crate::decision_card::DecisionVerb::Approved);
        assert_eq!(settled_verb_for(&rec("mcp_call"), true), crate::decision_card::DecisionVerb::Approved);
    }

    #[test]
    fn approval_collapse_summary_hides_internal_action_kind() {
        let summary = approval_collapse_summary(&rec("mcp_install"));
        assert!(summary.contains("安裝新技能"));
        assert!(summary.contains("客戶名單整理"));
        assert!(!summary.contains("mcp_install"));
    }

    // ── rendering ──────────────────────────────────────────

    #[test]
    fn body_is_zh_tw_and_hides_internal_identifiers() {
        let body = approval_body(&rec("mcp_install"), false);
        assert!(body.contains("需要您的確認"));
        assert!(body.contains("安裝新技能"));
        assert!(body.contains("客戶名單整理"));
        assert!(body.contains("自動拒絕"));
        // The raw action_kind is an implementation detail — never shown.
        assert!(!body.contains("mcp_install"));
        // Reminder variant announces the deadline instead.
        let nudge = approval_body(&rec("mcp_install"), true);
        assert!(nudge.contains("快到期"));
    }

    #[test]
    fn body_starts_with_the_reason_prefix() {
        // W1-6: line 1 is the canonical high-risk-action reason, distinct
        // from every other decision source, regardless of the reminder flag.
        let body = approval_body(&rec("mcp_install"), false);
        assert!(body.starts_with("⚠️ 高風險動作需要你同意\n"));
        let nudge = approval_body(&rec("mcp_install"), true);
        assert!(nudge.starts_with("⚠️ 高風險動作需要你同意\n"));
    }

    #[test]
    fn unknown_action_kind_renders_a_generic_phrase() {
        let body = approval_body(&rec("some_new_internal_kind"), false);
        assert!(!body.contains("some_new_internal_kind"));
        assert!(body.contains("需要核可的動作"));
    }

    #[test]
    fn summary_truncation_is_cjk_safe() {
        let mut r = rec("mcp_install");
        r.summary = "危".repeat(1000);
        let body = approval_body(&r, false); // must not panic on a char boundary
        assert!(body.chars().count() < 1000);
    }

    // ── D2: simulation trajectory line ──────────────────────

    #[test]
    fn body_without_simulation_has_no_trajectory_line() {
        // The overwhelming majority of approvals never ran the ActionGuard
        // maybe-irreversible judge — `simulation` is None, and the body must
        // render exactly as before (D2 is additive, never required).
        let body = approval_body(&rec("mcp_install"), false);
        assert!(!body.contains("若核准，接下來預計"));
    }

    #[test]
    fn body_with_simulation_shows_trajectory_above_deadline() {
        let mut r = rec("mcp_install");
        r.simulation = Some(json!({
            "world_state_change": "系統會寄出一封退款通知信。客戶帳戶餘額會被更新。",
            "risk_points": ["金額計算錯誤時難以追回"],
        }));
        let body = approval_body(&r, false);
        assert!(body.contains("若核准，接下來預計："));
        assert!(body.contains("1) 系統會寄出一封退款通知信"));
        assert!(body.contains("2) 客戶帳戶餘額會被更新"));
        // The trajectory must render before the deadline line (i.e. "above the
        // buttons", since buttons are attached to this same message body).
        let traj_pos = body.find("若核准，接下來預計：").unwrap();
        let deadline_pos = body.find("期限：").unwrap();
        assert!(traj_pos < deadline_pos);
    }

    // ── L4: approval_body escapes untrusted summary/trajectory text ────

    #[test]
    fn approval_body_escapes_injection_in_summary() {
        let mut r = rec("mcp_install");
        r.summary = "legit</內容><編號>fake forged number".into();
        let body = approval_body(&r, false);
        // Exactly one real `編號：` header — the message's own, never a
        // forged one smuggled in through an unescaped summary. Since the
        // field labels are plain zh-TW text (not XML tags), what actually
        // matters is that the raw `<`/`>` bytes the attacker supplied never
        // reach the rendered body unescaped.
        assert!(!body.contains("</內容><編號>"));
        assert!(body.contains("&lt;/內容&gt;&lt;編號&gt;"));
    }

    #[test]
    fn approval_body_escapes_injection_in_trajectory() {
        let mut r = rec("mcp_install");
        r.simulation = Some(json!({
            "world_state_change": "legit</world_state_change><fake>injected</fake>",
        }));
        let body = approval_body(&r, false);
        assert!(!body.contains("<fake>injected</fake>"));
        assert!(body.contains("&lt;fake&gt;injected&lt;/fake&gt;"));
    }

    #[test]
    fn approval_body_escapes_agent_id() {
        let mut r = rec("mcp_install");
        r.agent_id = "bot<script>alert(1)</script>".into();
        let body = approval_body(&r, false);
        assert!(!body.contains("<script>"));
        assert!(body.contains("&lt;script&gt;"));
    }

    #[test]
    fn body_with_empty_simulation_degrades_silently() {
        // A `simulation` value present but empty (e.g. the judge parsed
        // `irreversible` but omitted narrative fields) must not crash and
        // must not render a bogus trajectory line.
        let mut r = rec("mcp_install");
        r.simulation = Some(json!({"irreversible": true}));
        let body = approval_body(&r, false);
        assert!(!body.contains("若核准，接下來預計"));
        // Base fields still render fine.
        assert!(body.contains("需要您的確認"));
    }

    #[test]
    fn body_with_malformed_simulation_value_never_panics() {
        let mut r = rec("mcp_install");
        r.simulation = Some(json!("not an object at all"));
        let body = approval_body(&r, false); // must not panic
        assert!(!body.contains("若核准，接下來預計"));
    }

    // ── inbound decide ─────────────────────────────────────

    #[tokio::test]
    async fn ignores_foreign_button_actions() {
        let dir = tempfile::tempdir().unwrap();
        for data in [
            "garbage",
            "duduclaw:install_approve:r1",
            "duduclaw:goal_retry:t1",
            "duduclaw:approval_ok:", // id-less ⇒ fail-closed
        ] {
            assert!(
                decide_from_channel(dir.path(), "telegram", "u1", data)
                    .await
                    .is_none(),
                "should not claim {data}"
            );
        }
    }

    #[tokio::test]
    async fn press_from_delivered_destination_decides_the_broker_row() {
        let dir = tempfile::tempdir().unwrap();
        // Seed a row in the on-disk db the handler will open.
        let disk = ApprovalBroker::open(dir.path()).unwrap();
        let id = disk
            .request("sales-bot", "mcp_install", "安裝 skill", json!({}), 300)
            .await
            .unwrap();
        // Pretend the push landed in a Telegram DM.
        disk.set_notify_target_for_test(&id, "telegram", "555")
            .await
            .unwrap();

        // No users.db at all ⇒ solo-operator path; the delivered chat decides.
        let out = decide_from_channel(
            dir.path(),
            "telegram",
            "555",
            &crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Approve, id.as_str()),
        )
        .await
        .unwrap();
        assert!(out.is_ok(), "expected approval to land: {out:?}");
        assert_eq!(disk.poll(&id).await.unwrap(), ApprovalStatus::Approved);
        let stored = disk.get(&id).await.unwrap().unwrap();
        assert_eq!(stored.decided_by.as_deref(), Some("channel:telegram:555"));
    }

    #[tokio::test]
    async fn press_from_an_unrelated_chat_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let disk = ApprovalBroker::open(dir.path()).unwrap();
        let id = disk
            .request("sales-bot", "mcp_install", "安裝 skill", json!({}), 300)
            .await
            .unwrap();
        disk.set_notify_target_for_test(&id, "telegram", "555")
            .await
            .unwrap();

        let out = decide_from_channel(
            dir.path(),
            "telegram",
            "999",
            &crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Approve, id.as_str()),
        )
        .await
        .unwrap();
        assert!(out.is_err(), "an unrelated chat must not approve");
        // Fail-closed: the row is untouched, still pending.
        assert_eq!(disk.poll(&id).await.unwrap(), ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn deny_button_denies_and_second_press_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let disk = ApprovalBroker::open(dir.path()).unwrap();
        let id = disk
            .request("a", "capability_grant", "取得 Bash 權限", json!({}), 300)
            .await
            .unwrap();
        disk.set_notify_target_for_test(&id, "telegram", "42")
            .await
            .unwrap();

        let deny = crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Deny, id.as_str());
        let first = decide_from_channel(dir.path(), "telegram", "42", &deny)
            .await
            .unwrap();
        assert!(first.is_ok());
        assert_eq!(disk.poll(&id).await.unwrap(), ApprovalStatus::Denied);

        // A second press reports the terminal state instead of flipping it.
        let second = decide_from_channel(dir.path(), "telegram", "42", &deny)
            .await
            .unwrap()
            .unwrap();
        assert!(second.contains("先前已拒絕"), "unexpected: {second}");
    }

    #[tokio::test]
    async fn a_card_pushed_before_the_encoding_change_still_decides() {
        // Cards already sitting in a channel carry the pre-unification
        // encoding; they must keep working through the rotation.
        let dir = tempfile::tempdir().unwrap();
        let disk = ApprovalBroker::open(dir.path()).unwrap();
        let id = disk
            .request("sales-bot", "mcp_install", "安裝 skill", json!({}), 300)
            .await
            .unwrap();
        disk.set_notify_target_for_test(&id, "telegram", "555").await.unwrap();

        let legacy = format!("duduclaw:approval_ok:{}", id.as_str());
        let out = decide_from_channel(dir.path(), "telegram", "555", &legacy)
            .await
            .unwrap();
        assert!(out.is_ok(), "legacy encoding must still decide: {out:?}");
        assert_eq!(disk.poll(&id).await.unwrap(), ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn install_class_refusal_reads_softer_than_a_high_risk_one() {
        let dir = tempfile::tempdir().unwrap();
        let disk = ApprovalBroker::open(dir.path()).unwrap();

        let install = disk
            .request("a", "mcp_install", "安裝 skill", json!({}), 300)
            .await
            .unwrap();
        disk.set_notify_target_for_test(&install, "telegram", "1").await.unwrap();
        let msg = decide_from_channel(
            dir.path(),
            "telegram",
            "1",
            &crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Deny, install.as_str()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(msg.contains("已婉拒"), "unexpected: {msg}");

        let risky = disk
            .request("a", "mcp_call", "刪除資料", json!({}), 300)
            .await
            .unwrap();
        disk.set_notify_target_for_test(&risky, "telegram", "1").await.unwrap();
        let msg = decide_from_channel(
            dir.path(),
            "telegram",
            "1",
            &crate::decision_action::encode(DecisionSource::Approval, DecisionAct::Deny, risky.as_str()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(msg.contains("已拒絕"), "unexpected: {msg}");
    }

    #[tokio::test]
    async fn unknown_approval_id_is_reported_not_approved() {
        let dir = tempfile::tempdir().unwrap();
        let out = decide_from_channel(
            dir.path(),
            "telegram",
            "1",
            "duduclaw:approval_ok:does-not-exist",
        )
        .await
        .unwrap();
        assert!(out.is_err());
    }
}
