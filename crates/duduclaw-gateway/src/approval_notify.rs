//! WP20 — channel push + inline decision for the generic [`ApprovalBroker`].
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
//! ## Shape (mirrors `goal_notify`)
//!
//! - **Outbound** [`notify_new_approval`] — called from
//!   [`ApprovalBroker::request`] itself, so every action kind is covered by
//!   construction rather than by remembering to call it at each site.
//!   [`notify_reminder`] sends the ⅔-TTL "about to auto-deny" nudge.
//! - **Inbound** [`decide_from_channel`] — a `duduclaw:approval_ok|no:{id}`
//!   press is authorized ([`authorize_press`]) and applied via
//!   `ApprovalBroker::decide`.
//!
//! Everything is best-effort and fail-soft: a missing token / unreachable
//! destination is logged, never panics, and never blocks the approval itself.

use std::path::Path;

use serde_json::json;
use tracing::{info, warn};

use duduclaw_auth::models::{UserRole, UserStatus};
use duduclaw_auth::UserDb;

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus, SimulationNarrative};
use crate::channel_format;
use crate::task_store::{ActivityRow, TaskStore};

/// Max chars of the (partly external) summary rendered into a channel message.
/// CJK-safe via `truncate_chars` — never a raw byte slice (convention #1).
const SUMMARY_MAX_CHARS: usize = 300;

/// Channels that can render inline approve/deny buttons today — the same four
/// whose inbound dispatchers are wired to [`decide_from_channel`].
fn channel_supports_buttons(channel: &str) -> bool {
    matches!(channel, "telegram" | "slack" | "discord" | "line")
}

/// External channels a pending approval may be pushed to. A session id like
/// `webchat:<conn>#agent:…` names a transport with no bot-push API, so it must
/// never be mistaken for a destination.
fn is_pushable_channel(channel: &str) -> bool {
    matches!(
        channel,
        "telegram" | "slack" | "discord" | "line" | "whatsapp" | "feishu" | "googlechat" | "teams"
    )
}

// ── Destination resolution ──────────────────────────────────────

/// Parse a `DUDUCLAW_REPLY_CHANNEL`-style session id into a pushable
/// `(channel, chat_id)` destination.
///
/// Grammar matches `dispatcher::parse_reply_channel`: `<type>:<id>[:<thread>]`,
/// with the `<type>:thread:<id>` marker collapsing to `<id>`. Returns `None`
/// for a malformed value or a non-pushable transport (webchat, dashboard, …) —
/// the caller then falls through to the next link in the chain.
pub(crate) fn parse_origin(reply_channel: &str) -> Option<(String, String)> {
    let rc = reply_channel.trim();
    let parts: Vec<&str> = rc.splitn(3, ':').collect();
    if parts.len() < 2 {
        return None;
    }
    let channel = parts[0].trim();
    if !is_pushable_channel(channel) {
        return None;
    }
    let chat_id = if parts.len() == 3 && parts[1] == "thread" {
        parts[2]
    } else {
        parts[1]
    };
    let chat_id = chat_id.trim();
    // A composed WebChat-style id (`…#agent:…`) is not a chat id.
    if chat_id.is_empty() || chat_id.contains('#') {
        return None;
    }
    Some((channel.to_string(), chat_id.to_string()))
}

/// The conversation the approval was raised from, if any.
///
/// Two lookups, in order: the in-process `REPLY_CHANNEL` task-local (gateway
/// callers — autopilot, wiki ingest, skill activation) and the
/// `DUDUCLAW_REPLY_CHANNEL` env var (the `duduclaw mcp-server` child process,
/// which inherits it from the CLI spawn). This is why no new plumbing is
/// needed for the customer's case: the MCP server that files the `mcp_install`
/// approval already knows it was reached from `telegram:<chat_id>`.
fn origin_target() -> Option<(String, String)> {
    if let Ok(rc) = crate::claude_runner::REPLY_CHANNEL.try_with(|ch| ch.clone()) {
        if let Some(t) = parse_origin(&rc) {
            return Some(t);
        }
    }
    let rc = std::env::var(duduclaw_core::ENV_REPLY_CHANNEL).ok()?;
    parse_origin(&rc)
}

/// Pure destination chain: **origin conversation → the agent's own control
/// channel → the linked channels of everyone who may decide**.
///
/// Rationale for the order: pushing back into the conversation that triggered
/// the action is the only destination guaranteed to be watched by the person
/// who is waiting on it. `[proactive]` is the agent-level fallback every other
/// proactive push already uses. The admin fan-out is the last resort so an
/// approval raised by a cron/autopilot path still reaches a human.
///
/// Duplicate destinations are collapsed (an admin whose linked chat IS the
/// origin must not be messaged twice).
pub(crate) fn resolve_targets(
    origin: Option<(String, String)>,
    agent_default: Option<(String, String)>,
    approver_links: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let chosen = match (origin, agent_default) {
        (Some(o), _) => vec![o],
        (None, Some(a)) => vec![a],
        (None, None) => approver_links,
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for t in chosen {
        if t.0.trim().is_empty() || t.1.trim().is_empty() || !is_pushable_channel(&t.0) {
            continue;
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Verified channel identities of every Active Admin/Manager — the humans the
/// dashboard would let decide this. Empty when `users.db` does not exist (a
/// solo deployment that never onboarded dashboard users).
fn approver_links(home_dir: &Path) -> Vec<(String, String)> {
    let Some(db) = open_user_db(home_dir) else {
        return Vec::new();
    };
    let Ok(users) = db.list_users() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for u in users {
        if u.status != UserStatus::Active || !matches!(u.role, UserRole::Admin | UserRole::Manager)
        {
            continue;
        }
        if let Ok(idents) = db.verified_channels_for_user(&u.id) {
            for i in idents {
                out.push((i.channel, i.channel_user_id));
            }
        }
    }
    out
}

/// Open `users.db` ONLY if it already exists. `UserDb::new` would create the
/// file; the MCP-server child process must not conjure an auth database as a
/// side effect of filing an approval.
fn open_user_db(home_dir: &Path) -> Option<UserDb> {
    let path = home_dir.join("users.db");
    if !path.exists() {
        return None;
    }
    UserDb::new(&path).ok()
}

// ── Message rendering ───────────────────────────────────────────

/// Human-facing zh-TW label for an `action_kind`. Internal identifiers must not
/// leak into an end-user message (project convention: user-facing copy hides
/// implementation detail), so anything unmapped renders as a generic phrase.
fn zh_action_kind(kind: &str) -> &str {
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
        "{head}\n\
         AI 員工：{agent}\n\
         想做的事：{kind}\n\
         內容：{summary}{trajectory}\n\
         期限：{deadline}未回覆將自動拒絕\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&rec.agent_id),
        kind = zh_action_kind(&rec.action_kind),
        summary = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.summary, SUMMARY_MAX_CHARS)),
        trajectory = crate::goal_state::xml_escape(&trajectory_line(rec)),
        deadline = deadline_phrase(rec),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

/// Per-channel button markup for a generic approval.
fn approval_markup(channel: &str, approval_id: &str) -> serde_json::Value {
    match channel {
        "telegram" => channel_format::telegram_broker_approval_buttons(approval_id),
        "discord" => channel_format::discord_broker_approval_buttons(approval_id),
        "slack" => channel_format::slack_broker_approval_buttons(approval_id),
        "line" => channel_format::line_broker_approval_quick_reply(approval_id),
        _ => json!({}),
    }
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
    let mut delivered: Option<(String, String)> = None;

    for (channel, chat_id) in targets {
        let Some(token) =
            crate::goal_notify::channel_token(home_dir, &rec.agent_id, &channel).await
        else {
            info!(approval_id = %rec.id, %channel, "approval push: no bot token; skipping");
            continue;
        };
        let ok = if channel_supports_buttons(&channel) {
            let markup = approval_markup(&channel, rec.id.as_str());
            match crate::goal_notify::send_with_markup(
                &http, &channel, &token, &chat_id, &body, markup,
            )
            .await
            {
                Ok(()) => true,
                Err(e) => {
                    warn!(approval_id = %rec.id, %channel, error = %e, "approval push failed");
                    false
                }
            }
        } else {
            // No inline buttons on this transport — degrade to text plus the
            // two ways a human can still act.
            let text = format!(
                "{body}\n\n此通道無法顯示按鈕，請至儀表板「審批」頁核准或拒絕，\
                 或改用 Telegram／Slack／Discord／LINE 直接按按鈕。"
            );
            crate::goal_notify::send_plain_text(&http, &channel, &token, &chat_id, &text).await
        };
        if ok && delivered.is_none() {
            delivered = Some((channel, chat_id));
        }
    }
    delivered
}

// ── Inbound ─────────────────────────────────────────────────────

/// The authorization verdict for one button press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PressAuth {
    Allow,
    /// The presser maps to a dashboard user without approval rights.
    DenyNotApprover,
    /// The presser has no usable identity and did not press from the
    /// destination this approval was delivered to.
    DenyUnknown,
}

/// Pure authorization for an approval button press. Fail-closed: anything not
/// explicitly allowed is a denial.
///
/// - A mapped, Active dashboard user decides by **role** — Admin/Manager may
///   approve, everyone else may not. This keeps the enterprise
///   separation-of-duties identical to the install-request path (an employee
///   never signs off their own request).
/// - When the deployment has **no channel-approver identity at all**
///   (`identity_system_active == false` — nobody Active with Admin/Manager role
///   has linked a channel), the only authority available is the destination the
///   operator configured (or the conversation that raised the request), so a
///   press from that exact destination is honoured. Without this, a solo
///   operator who never created dashboard users can never approve anything from
///   a phone — which is precisely the failure being fixed. It does NOT loosen a
///   configured deployment: as soon as one approver links a channel, role rules
///   apply to everyone.
pub(crate) fn authorize_press(
    mapped_role: Option<UserRole>,
    identity_system_active: bool,
    destination_match: bool,
) -> PressAuth {
    match mapped_role {
        Some(UserRole::Admin) | Some(UserRole::Manager) => PressAuth::Allow,
        Some(_) => PressAuth::DenyNotApprover,
        None => {
            if !identity_system_active && destination_match {
                PressAuth::Allow
            } else {
                PressAuth::DenyUnknown
            }
        }
    }
}

/// True when the press came from the **individual account** this approval was
/// delivered to.
///
/// Deliberately matches `channel_user_id` ONLY, never the chat/conversation id.
/// Matching the conversation would mean that when an approval lands in a group,
/// *every member of that group* satisfies the solo-operator branch of
/// [`authorize_press`] — a group back-door where any colleague (or anyone
/// invited to the chat) can approve a skill install. Telegram / Discord / LINE
/// report `channel_user_id == chat_id` for a 1:1 DM, which is the destination
/// this branch actually exists to serve, so DMs are unaffected.
///
/// Consequence, stated plainly: an approval delivered to a **group** cannot be
/// decided through the solo-operator branch at all. That deployment must bind a
/// dashboard identity (role-based authorization) or decide from the dashboard —
/// which is the correct answer for a shared conversation.
fn destination_matches(rec: &ApprovalRecord, channel: &str, channel_user_id: &str) -> bool {
    let Some(dest_ch) = rec.notify_channel.as_deref() else {
        return false;
    };
    let Some(dest_id) = rec.notify_chat_id.as_deref() else {
        return false;
    };
    // Exact equality only — no `contains`/`starts_with` for a routing or
    // security decision (project convention #2).
    dest_ch == channel && dest_id == channel_user_id
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
    let (approval_id, approve) = channel_format::parse_approval_action(action_data)?;
    Some(apply_decision(home_dir, channel, channel_user_id, &approval_id, approve).await)
}

async fn apply_decision(
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
            ApprovalStatus::Approved => "這項要求先前已被同意。".into(),
            ApprovalStatus::Denied => "這項要求先前已被拒絕。".into(),
            _ => "這項要求已逾時自動拒絕，請重新發起。".to_string(),
        });
    }

    // ── authorization ───────────────────────────────────────
    let db = open_user_db(home_dir);
    // VERIFIED bindings only: an unverified `channel_identities` row is just an
    // unconfirmed claim typed into the binding form, never proof of identity.
    let mapped_role = db.as_ref().and_then(|db| {
        let uid = db
            .find_verified_user_id_by_channel(channel, channel_user_id)
            .ok()
            .flatten()?;
        let u = db.get_user(&uid).ok().flatten()?;
        (u.status == UserStatus::Active).then_some(u.role)
    });
    let identity_system_active = !approver_links(home_dir).is_empty();
    let dest_match = destination_matches(&rec, channel, channel_user_id);

    match authorize_press(mapped_role, identity_system_active, dest_match) {
        PressAuth::Allow => {}
        PressAuth::DenyNotApprover => return Err("您沒有核准權限".into()),
        PressAuth::DenyUnknown => {
            return Err("此帳號尚未連結儀表板身分，無法核准。請先於儀表板以此通道登入綁定。".into())
        }
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

    Ok(if approve {
        format!("✅ 已同意：{}", zh_action_kind(&rec.action_kind))
    } else {
        format!("❌ 已拒絕：{}", zh_action_kind(&rec.action_kind))
    })
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

    // ── destination resolution ─────────────────────────────

    #[test]
    fn parse_origin_accepts_channel_sessions() {
        assert_eq!(
            parse_origin("telegram:12345"),
            Some(("telegram".into(), "12345".into()))
        );
        // thread-scoped session → chat id, thread dropped
        assert_eq!(
            parse_origin("telegram:12345:678"),
            Some(("telegram".into(), "12345".into()))
        );
        // the `<type>:thread:<id>` marker collapses to <id>
        assert_eq!(
            parse_origin("discord:thread:999"),
            Some(("discord".into(), "999".into()))
        );
    }

    #[test]
    fn parse_origin_rejects_non_pushable_and_malformed() {
        // WebChat has no bot-push API — never a destination.
        assert_eq!(parse_origin("webchat:conn-1"), None);
        assert_eq!(parse_origin("webchat:conn#agent:a#conv:n"), None);
        assert_eq!(parse_origin("dashboard:alice"), None);
        assert_eq!(parse_origin("telegram"), None);
        assert_eq!(parse_origin("telegram:"), None);
        assert_eq!(parse_origin(""), None);
    }

    #[test]
    fn targets_prefer_the_origin_conversation() {
        // The customer's case: install triggered from Telegram → the approval
        // goes back to THAT chat, even though the agent has another default
        // and admins are linked elsewhere.
        let got = resolve_targets(
            Some(("telegram".into(), "555".into())),
            Some(("slack".into(), "C1".into())),
            vec![("discord".into(), "D1".into())],
        );
        assert_eq!(got, vec![("telegram".to_string(), "555".to_string())]);
    }

    #[test]
    fn targets_fall_back_to_agent_default_then_approvers() {
        let agent_only = resolve_targets(
            None,
            Some(("slack".into(), "C1".into())),
            vec![("discord".into(), "D1".into())],
        );
        assert_eq!(agent_only, vec![("slack".to_string(), "C1".to_string())]);

        let approvers = resolve_targets(
            None,
            None,
            vec![
                ("discord".into(), "D1".into()),
                ("line".into(), "U9".into()),
            ],
        );
        assert_eq!(approvers.len(), 2);

        // Nothing anywhere ⇒ no destination (caller logs the config gap).
        assert!(resolve_targets(None, None, vec![]).is_empty());
    }

    #[test]
    fn targets_drop_blank_and_unpushable_and_dedupe() {
        let got = resolve_targets(
            None,
            None,
            vec![
                ("telegram".into(), "1".into()),
                ("telegram".into(), "1".into()), // duplicate admin link
                ("webchat".into(), "x".into()),  // not pushable
                ("telegram".into(), "  ".into()), // blank id
                ("".into(), "1".into()),
            ],
        );
        assert_eq!(got, vec![("telegram".to_string(), "1".to_string())]);
    }

    // ── authorization ──────────────────────────────────────

    #[test]
    fn approvers_are_allowed_by_role() {
        assert_eq!(
            authorize_press(Some(UserRole::Admin), true, false),
            PressAuth::Allow
        );
        assert_eq!(
            authorize_press(Some(UserRole::Manager), true, false),
            PressAuth::Allow
        );
    }

    #[test]
    fn employee_press_is_refused_even_from_the_destination() {
        // Separation of duties: a mapped non-approver never approves, not even
        // in the chat the approval was delivered to.
        assert_eq!(
            authorize_press(Some(UserRole::Employee), true, true),
            PressAuth::DenyNotApprover
        );
        assert_eq!(
            authorize_press(Some(UserRole::Employee), false, true),
            PressAuth::DenyNotApprover
        );
    }

    #[test]
    fn unmapped_press_is_refused_when_approvers_exist() {
        // A configured deployment stays strict: a random chat member cannot
        // approve just because the message landed in their chat.
        assert_eq!(authorize_press(None, true, true), PressAuth::DenyUnknown);
        assert_eq!(authorize_press(None, true, false), PressAuth::DenyUnknown);
    }

    #[test]
    fn solo_operator_may_decide_from_the_delivered_destination() {
        // No dashboard approver identities exist at all → the operator-configured
        // (or originating) conversation is the only authority available.
        assert_eq!(authorize_press(None, false, true), PressAuth::Allow);
        // …but only from THAT conversation.
        assert_eq!(authorize_press(None, false, false), PressAuth::DenyUnknown);
    }

    #[test]
    fn destination_match_requires_exact_channel_and_user() {
        let mut r = rec("mcp_install");
        assert!(
            !destination_matches(&r, "telegram", "555"),
            "never delivered ⇒ no match"
        );
        r.notify_channel = Some("telegram".into());
        r.notify_chat_id = Some("555".into());
        // 1:1 DM — the account the approval was delivered to.
        assert!(destination_matches(&r, "telegram", "555"));
        // wrong channel / wrong account
        assert!(!destination_matches(&r, "slack", "555"));
        assert!(!destination_matches(&r, "telegram", "666"));
        // no substring shortcuts (convention #2)
        assert!(!destination_matches(&r, "telegram", "5555"));
    }

    #[test]
    fn group_delivery_is_not_a_back_door_for_its_members() {
        // Approval delivered to a GROUP chat (id 555). A member of that group
        // presses; their own account id is 999. Matching the *conversation*
        // would let every member of the group approve — it must not.
        let mut r = rec("mcp_install");
        r.notify_channel = Some("telegram".into());
        r.notify_chat_id = Some("555".into()); // group chat id
        assert!(
            !destination_matches(&r, "telegram", "999"),
            "a group member must not satisfy the solo-operator branch"
        );
        // …and that refusal is what the authorization layer sees.
        assert_eq!(
            authorize_press(None, false, destination_matches(&r, "telegram", "999")),
            PressAuth::DenyUnknown
        );
    }

    #[test]
    fn dm_delivery_still_authorizes_the_owner() {
        // The DM case this branch exists for: TG/Discord/LINE report
        // channel_user_id == chat_id for a 1:1, so the owner still gets through.
        let mut r = rec("mcp_install");
        r.notify_channel = Some("telegram".into());
        r.notify_chat_id = Some("555".into()); // == the owner's user id in a DM
        assert!(destination_matches(&r, "telegram", "555"));
        assert_eq!(
            authorize_press(None, false, destination_matches(&r, "telegram", "555")),
            PressAuth::Allow
        );
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
            &channel_format::approval_action_id(true, id.as_str()),
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
            &channel_format::approval_action_id(true, id.as_str()),
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

        let deny = channel_format::approval_action_id(false, id.as_str());
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
        assert!(second.contains("先前已被拒絕"), "unexpected: {second}");
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
