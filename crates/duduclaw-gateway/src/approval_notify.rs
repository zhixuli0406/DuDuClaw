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
//!   out through the shared [`crate::notify_push::push`] path.
//! - **Inbound** [`apply_decision`] — reached from the unified router
//!   ([`crate::decision_notify::route_press`]), authorized by the shared
//!   matrix and applied via `ApprovalBroker::decide`.
//!
//! Everything is best-effort and fail-soft: a missing token / unreachable
//! destination is logged, never panics, and never blocks the approval itself.

use std::path::Path;

use crate::approval::{
    ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus, SimulationNarrative,
};
use crate::decision_action::{DecisionAct, DecisionSource};
use crate::decision_notify::{
    DecisionCard, PressAuth, approver_links, authorize_press, destination_matches_any,
    identity_system_active, mapped_role, origin_target, refusal_text, resolve_targets,
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
        "support_pilot_review" => "檢視合成決策模擬",
        "computer_workspace_admin" => "管理電腦操作工作區",
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
        summary = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(
            &rec.summary,
            SUMMARY_MAX_CHARS
        )),
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
    // Workflow audiences are checked in authenticated, task-scoped dashboard RPCs.
    // Generic channel fallback cannot establish the private audience identity.
    push(home_dir, rec, false).await
}

/// Push the ⅔-TTL "about to auto-deny" reminder. Prefers the destination the
/// original push landed on so the nudge follows the same conversation.
pub async fn notify_reminder(home_dir: &Path, rec: &ApprovalRecord) -> Option<(String, String)> {
    push(home_dir, rec, true).await
}

/// Approval kinds decided in the dashboard only (H1, v1.67.1). Their channel
/// push is a plain notice without decision buttons or a text-reply verb, it is
/// never sent to the conversation the action came from, and a channel press /
/// text decision that still arrives for one is refused (see
/// [`apply_decision`]).
///
/// Members: `knowledge_quarantine`, `workflow_activation` and the LINE
/// inbox operator changes (`channel_ingress::cli_approval`).
///
/// `knowledge_quarantine`: approving it writes knowledge
/// with operator authority (a held claim is promoted, a burst is released),
/// and a channel press can only be authorised by the push destination when no
/// identity system is configured — which, for a claim that came from a chat,
/// could be the very person who made the claim.
///
/// `workflow_activation` joined in F1b: accepting a workflow version grants a
/// standing authority to act, only an Admin may decide it, and a channel
/// press cannot establish that the presser is a current Admin.
/// `computer_workspace_admin` (an operator-terminal regrant / renew / delete
/// of a computer-use workspace, P2-C) is dashboard-only too: the terminal
/// cannot prove who typed the command, so an Admin decides it in the dashboard.
pub(crate) fn is_dashboard_only_kind(kind: &str) -> bool {
    kind == crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE
        || kind == crate::approval::WORKFLOW_ACTIVATION_KIND
        // F2: operator-terminal LINE inbox changes (`duduclaw ops channel-ingress`).
        || kind == crate::channel_ingress::cli_approval::ACTION_KIND
        // P2-C: operator-terminal workspace changes (`duduclaw ops computer-workspaces`).
        || kind == crate::computer_workspaces::cli_approval::ACTION_KIND
}

/// What the dashboard answers when a dashboard-only card is decided after it
/// lapsed. Each kind names itself; any other kind gets the general wording
/// (F5-A: a LINE inbox card used to be called a knowledge review here).
pub(crate) fn dashboard_only_expired_text(kind: &str) -> &'static str {
    if kind == crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE {
        "這則知識審核已逾期，已自動捨棄，無法再核准。"
    } else if kind == crate::approval::WORKFLOW_ACTIVATION_KIND {
        "這個工作流程啟用審核已逾期，工作流程不會啟用；要啟用請重新送審。"
    } else if kind == crate::channel_ingress::cli_approval::ACTION_KIND {
        "這則收件處理指令的審核已逾期，指令不會執行；需要的話請重新下指令。"
    } else if kind == crate::computer_workspaces::cli_approval::ACTION_KIND {
        crate::computer_workspaces::cli_approval::EXPIRED_TEXT
    } else {
        "這則審核已逾期，已自動拒絕，無法再核准。"
    }
}

/// What a refused channel decision for a [`is_dashboard_only_kind`] approval
/// says.
pub(crate) const DASHBOARD_ONLY_REFUSAL: &str =
    "這則審核只能在儀表板的待辦清單決定，請開啟儀表板處理（這裡的回覆不會生效）。";

/// The zh-TW body of the plain notice for a dashboard-only approval. Carries
/// no claim text, no stored value and no decision verb — only that something
/// is waiting, for which AI employee, and until when.
pub(crate) fn dashboard_only_notice_body(rec: &ApprovalRecord, reminder: bool) -> String {
    if rec.action_kind == crate::approval::WORKFLOW_ACTIVATION_KIND {
        return activation_notice_body(rec, reminder);
    }
    if rec.action_kind == crate::channel_ingress::cli_approval::ACTION_KIND {
        return crate::channel_ingress::cli_approval::notice_body(rec, reminder);
    }
    if rec.action_kind == crate::computer_workspaces::cli_approval::ACTION_KIND {
        return crate::computer_workspaces::cli_approval::notice_body(
            rec,
            reminder,
            &deadline_phrase(rec),
        );
    }
    let head = if reminder {
        "⏰ 有一則知識審核快到期了，逾時會自動捨棄"
    } else {
        "📥 有一則知識等待審核"
    };
    let what = match rec.payload.get("disposition").and_then(|v| v.as_str()) {
        Some(d) if d == crate::wiki_ingest::DISPOSITION_TRUST_HELD => {
            "對話中的新說法和目前採用的內容不一致，尚未套用"
        }
        _ => "同一來源短時間內寫入大量知識，已暫時隔離",
    };
    format!(
        "{head}\n\
         AI 員工：{agent}\n\
         狀況：{what}\n\
         這類知識變更只能在儀表板的待辦清單決定，請開啟儀表板審核。\n\
         期限：{deadline}未審核將自動捨棄\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.agent_id, 64)),
        deadline = deadline_phrase(rec),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

/// The plain notice for a workflow activation: no workflow name, steps or
/// targets (those live behind the dashboard's access checks), only that an
/// activation waits for an Admin and when it lapses.
pub(crate) fn activation_notice_body(rec: &ApprovalRecord, reminder: bool) -> String {
    let head = if reminder {
        "⏰ 有一個工作流啟用審核快到期了，逾時會自動拒絕，工作流不會啟用"
    } else {
        "📥 有一個工作流等待啟用審核"
    };
    format!(
        "{head}\n\
         AI 員工：{agent}\n\
         啟用審核只能由管理員在儀表板的待辦清單決定，請開啟儀表板處理。\n\
         期限：{deadline}未審核將自動拒絕\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.agent_id, 64)),
        deadline = deadline_phrase(rec),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

/// Destinations for a dashboard-only notice: the agent's own control
/// channel, else the approvers' linked chats — never the originating
/// conversation, which is excluded explicitly rather than by relying on the
/// reply-channel task-local being absent.
fn dashboard_only_targets(
    home_dir: &Path,
    rec: &ApprovalRecord,
    reminder: bool,
) -> Vec<(String, String)> {
    let origin = origin_target();
    let base = match (
        reminder,
        rec.notify_channel.as_deref(),
        rec.notify_chat_id.as_deref(),
    ) {
        (true, Some(ch), Some(id)) if !ch.is_empty() && !id.is_empty() => {
            vec![(ch.to_string(), id.to_string())]
        }
        _ => resolve_targets(
            None,
            crate::goal_notify::agent_notify_target(home_dir, &rec.agent_id),
            approver_links(home_dir),
        ),
    };
    // R-L1: the origin recorded on the card at filing (captured before the
    // distillation was spawned) is the reliable one; the task-local is a
    // second line for callers that still run inside the reply scope.
    let recorded = match (
        rec.payload.get("origin_channel").and_then(|v| v.as_str()),
        rec.payload.get("origin_chat_id").and_then(|v| v.as_str()),
    ) {
        (Some(ch), Some(chat)) => Some((ch.to_string(), chat.to_string())),
        _ => None,
    };
    base.into_iter()
        .filter(|t| origin.as_ref() != Some(t) && recorded.as_ref() != Some(t))
        .collect()
}

/// Test seam: the notice targets for a first push of `rec`.
#[cfg(test)]
pub(crate) fn dashboard_only_targets_for_test(
    home_dir: &Path,
    rec: &ApprovalRecord,
) -> Vec<(String, String)> {
    dashboard_only_targets(home_dir, rec, false)
}

/// Push the plain dashboard-only notice: no buttons, no card record (so no
/// text-reply decision can find it), first reachable destination wins.
async fn push_dashboard_only(
    home_dir: &Path,
    rec: &ApprovalRecord,
    reminder: bool,
) -> Option<(String, String)> {
    let targets = dashboard_only_targets(home_dir, rec, reminder);
    if targets.is_empty() {
        return None;
    }
    // Terminal-filed workspace requests: at most a few pushes per workspace
    // per hour (review M-3); beyond that only the inbox and an audit row.
    if !reminder
        && rec.action_kind == crate::computer_workspaces::cli_approval::ACTION_KIND
        && !crate::computer_workspaces::cli_approval::push_allowed(home_dir, rec).await
    {
        return None;
    }
    let policy = crate::notify_governance::QuietPolicy {
        window: crate::notify_governance::load_global_window(home_dir),
        tz: crate::notify_governance::NotifyTz::System,
    };
    let level = crate::decision_notify::notify_level(DecisionSource::Approval);
    if policy.decide(level, chrono::Utc::now()).is_some() {
        // Quiet hours: the inbox still shows it; the reminder retries.
        return None;
    }
    let mut text = dashboard_only_notice_body(rec, reminder);
    if let Some(url) = crate::deep_link::deep_link(
        home_dir,
        crate::deep_link::DeepLinkKind::Approval,
        rec.id.as_str(),
    ) {
        text.push_str(&format!("\n\n👉 {url}"));
    }
    let http = reqwest::Client::new();
    for (channel, chat_id) in targets {
        let Some(token) =
            crate::goal_notify::channel_token(home_dir, &rec.agent_id, &channel).await
        else {
            continue;
        };
        if crate::channel_sender::send_plain_text(
            home_dir, &http, &channel, &token, &chat_id, &text,
        )
        .await
        {
            return Some((channel, chat_id));
        }
    }
    None
}

async fn push(home_dir: &Path, rec: &ApprovalRecord, reminder: bool) -> Option<(String, String)> {
    if is_dashboard_only_kind(&rec.action_kind) {
        return push_dashboard_only(home_dir, rec, reminder).await;
    }
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

    let body = approval_body(rec, reminder);
    // A clickable deep link to the unified inbox — `None` when no dashboard
    // base URL is configured/derivable, in which case the rendered text stays
    // exactly as it reads without this feature.
    let link = crate::deep_link::deep_link(
        home_dir,
        crate::deep_link::DeepLinkKind::Approval,
        rec.id.as_str(),
    );
    let card = DecisionCard {
        source: DecisionSource::Approval,
        decision_id: rec.id.as_str(),
        body: &body,
        link: link.as_deref(),
        no_button_hint: "此通道無法顯示按鈕，請至儀表板的待辦決定頁同意或拒絕，\
                         或改用 Telegram／Slack／Discord／LINE 直接按按鈕。",
    };
    // O5: the token cascade + per-target deliver + first-success bookkeeping
    // this function used to inline is `notify_push::push`. Which destinations
    // (the chain above) and what the card says stay here.
    crate::notify_push::push(
        home_dir,
        &card,
        &crate::notify_push::NotifyDest::Agent {
            agent_id: rec.agent_id.clone(),
            targets,
        },
    )
    .await
    .delivered
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
    Some(
        apply_decision(
            home_dir,
            channel,
            channel_user_id,
            &action.id,
            action.approve(),
        )
        .await,
    )
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
    if rec.binding.is_some() || rec.request_kind != crate::approval::RequestKind::Approval {
        return Err("此請求需由完整的帳號與會話身分入口處理。".into());
    }
    let role = mapped_role(home_dir, channel, channel_user_id);
    // A generic manager or destination-only channel press cannot inspect the
    // admin-only Decision Lab. Check terminal rows before disclosing status.
    if rec.action_kind == "support_pilot_review" && role != Some(duduclaw_auth::UserRole::Admin) {
        return Err("此人工檢視需由可開啟 Decision Lab 的管理員決定。".into());
    }
    // H1: decided in the dashboard only — an older card's button, a crafted
    // callback or a text reply never decides it; the approval stays pending.
    if is_dashboard_only_kind(&rec.action_kind) {
        return Err(DASHBOARD_ONLY_REFUSAL.into());
    }
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
        role,
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
    spawn_approval_collapse(
        home_dir.to_path_buf(),
        rec.clone(),
        channel.to_string(),
        channel_user_id.to_string(),
        card_verb,
    );

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
    let act = if approve {
        DecisionAct::Approve
    } else {
        DecisionAct::Deny
    };
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
        let decider =
            crate::decision_card::resolve_decider_name(&home_dir, &channel, &channel_user_id);
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
    use duduclaw_auth::{UserDb, UserRole};
    use serde_json::json;

    #[tokio::test]
    async fn synthetic_pilot_review_channel_decision_requires_verified_admin() {
        let home = tempfile::tempdir().unwrap();
        let users = UserDb::new(&home.path().join("users.db")).unwrap();
        let manager = users
            .create_user(
                "manager@example.test",
                "Manager",
                "test-password",
                UserRole::Manager,
            )
            .unwrap();
        let admin = users
            .create_user(
                "admin@example.test",
                "Admin",
                "test-password",
                UserRole::Admin,
            )
            .unwrap();
        users
            .bind_channel_identity(&manager.id, "telegram", "manager-dm", true)
            .unwrap();
        users
            .bind_channel_identity(&admin.id, "telegram", "admin-dm", true)
            .unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let approval = broker
            .request(
                "requester",
                "support_pilot_review",
                "Inspect a synthetic run",
                json!({}),
                3600,
            )
            .await
            .unwrap();
        assert!(
            apply_decision(
                home.path(),
                "telegram",
                "manager-dm",
                approval.as_str(),
                true
            )
            .await
            .is_err()
        );
        assert!(
            apply_decision(
                home.path(),
                "telegram",
                "unknown-dm",
                approval.as_str(),
                true
            )
            .await
            .is_err()
        );
        assert_eq!(
            broker.get(&approval).await.unwrap().unwrap().status,
            ApprovalStatus::Pending
        );
        assert!(
            apply_decision(home.path(), "telegram", "admin-dm", approval.as_str(), true)
                .await
                .is_ok()
        );
        assert_eq!(
            broker.get(&approval).await.unwrap().unwrap().status,
            ApprovalStatus::Approved
        );
        assert!(
            apply_decision(
                home.path(),
                "telegram",
                "manager-dm",
                approval.as_str(),
                true,
            )
            .await
            .is_err(),
            "a non-admin must not read this review's terminal decision through a channel",
        );
    }

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
            request_kind: crate::approval::RequestKind::Approval,
            binding: None,
            answer: None,
            invalidated_reason: None,
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
        assert_eq!(
            settled_verb_for(&rec("mcp_install"), false),
            crate::decision_card::DecisionVerb::DeclinedInstall
        );
        assert_eq!(
            settled_verb_for(&rec("mcp_call"), false),
            crate::decision_card::DecisionVerb::Denied
        );
    }

    #[test]
    fn settled_verb_for_approve_is_always_approved_regardless_of_kind() {
        assert_eq!(
            settled_verb_for(&rec("mcp_install"), true),
            crate::decision_card::DecisionVerb::Approved
        );
        assert_eq!(
            settled_verb_for(&rec("mcp_call"), true),
            crate::decision_card::DecisionVerb::Approved
        );
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
            &crate::decision_action::encode(
                DecisionSource::Approval,
                DecisionAct::Approve,
                id.as_str(),
            ),
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
            &crate::decision_action::encode(
                DecisionSource::Approval,
                DecisionAct::Approve,
                id.as_str(),
            ),
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

        let deny = crate::decision_action::encode(
            DecisionSource::Approval,
            DecisionAct::Deny,
            id.as_str(),
        );
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
        disk.set_notify_target_for_test(&id, "telegram", "555")
            .await
            .unwrap();

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
        disk.set_notify_target_for_test(&install, "telegram", "1")
            .await
            .unwrap();
        let msg = decide_from_channel(
            dir.path(),
            "telegram",
            "1",
            &crate::decision_action::encode(
                DecisionSource::Approval,
                DecisionAct::Deny,
                install.as_str(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(msg.contains("已婉拒"), "unexpected: {msg}");

        let risky = disk
            .request("a", "mcp_call", "刪除資料", json!({}), 300)
            .await
            .unwrap();
        disk.set_notify_target_for_test(&risky, "telegram", "1")
            .await
            .unwrap();
        let msg = decide_from_channel(
            dir.path(),
            "telegram",
            "1",
            &crate::decision_action::encode(
                DecisionSource::Approval,
                DecisionAct::Deny,
                risky.as_str(),
            ),
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

    // ── H1: knowledge review is dashboard-only ─────────────────

    #[tokio::test]
    async fn knowledge_quarantine_cannot_be_decided_from_a_channel() {
        let dir = tempfile::tempdir().unwrap();
        let disk = ApprovalBroker::open(dir.path()).unwrap();
        let id = disk
            .request(
                "sales-bot",
                "knowledge_quarantine",
                "知識審核",
                json!({ "disposition": "trust_held", "promote_on_approve": true }),
                300,
            )
            .await
            .unwrap();
        // Even the exact destination the notice landed on (the solo-operator
        // authority path) cannot decide it — by button, legacy button or
        // text reply (all of which route through `apply_decision`).
        disk.set_notify_target_for_test(&id, "telegram", "555")
            .await
            .unwrap();
        for data in [
            crate::decision_action::encode(
                DecisionSource::Approval,
                DecisionAct::Approve,
                id.as_str(),
            ),
            crate::decision_action::encode(
                DecisionSource::Approval,
                DecisionAct::Deny,
                id.as_str(),
            ),
            format!("duduclaw:approval_ok:{}", id.as_str()),
        ] {
            let out = decide_from_channel(dir.path(), "telegram", "555", &data)
                .await
                .unwrap();
            assert_eq!(out, Err(DASHBOARD_ONLY_REFUSAL.to_string()));
        }
        assert_eq!(disk.poll(&id).await.unwrap(), ApprovalStatus::Pending);
    }

    #[test]
    fn dashboard_only_notice_has_no_claim_text_and_points_to_the_dashboard() {
        let mut r = rec("knowledge_quarantine");
        r.summary = "對話中有一則關於「退款」的新說法 內容摘要：永久退款".into();
        r.payload = json!({ "disposition": "trust_held" });
        let body = dashboard_only_notice_body(&r, false);
        assert!(body.contains("儀表板"));
        assert!(!body.contains("永久退款") && !body.contains("退款"));
        assert!(!body.contains("knowledge_quarantine"));
        // No reply verb is offered.
        assert!(!body.contains("同意") && !body.contains("核准"));
        r.payload = json!({});
        assert!(dashboard_only_notice_body(&r, true).contains("快到期"));
    }

    #[test]
    fn each_dashboard_only_kind_has_its_own_expiry_text() {
        let knowledge = dashboard_only_expired_text(crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE);
        let activation = dashboard_only_expired_text(crate::approval::WORKFLOW_ACTIVATION_KIND);
        let ingress = dashboard_only_expired_text(crate::channel_ingress::cli_approval::ACTION_KIND);
        let other = dashboard_only_expired_text("something_else");
        assert!(knowledge.contains("知識"));
        assert!(activation.contains("工作流程") && !activation.contains("知識"));
        assert!(ingress.contains("收件") && !ingress.contains("知識"));
        assert!(!other.contains("知識") && other.contains("逾期"));
    }

    #[test]
    fn activation_is_dashboard_only_with_its_own_notice() {
        assert!(is_dashboard_only_kind(crate::approval::WORKFLOW_ACTIVATION_KIND));
        assert!(!DASHBOARD_ONLY_REFUSAL.contains("知識"));
        let mut r = rec(crate::approval::WORKFLOW_ACTIVATION_KIND);
        r.summary = "接受固定工作流版本與排程範圍".into();
        r.payload = json!({"kind": "workflow_activation", "draft_id": "secret-draft"});
        let body = dashboard_only_notice_body(&r, false);
        assert!(body.contains("工作流等待啟用審核") && body.contains("管理員"));
        assert!(!body.contains("知識") && !body.contains("secret-draft"));
        assert!(!body.contains("同意") && !body.contains("核准"));
        let reminder = dashboard_only_notice_body(&r, true);
        assert!(reminder.contains("快到期") && reminder.contains("不會啟用"));
    }

    #[tokio::test]
    async fn dashboard_only_notice_never_targets_the_originating_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agents").join("sales-bot");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            "[proactive]\nnotify_channel = \"telegram\"\nnotify_chat_id = \"555\"\n",
        )
        .unwrap();
        let r = rec("knowledge_quarantine");
        let home = dir.path().to_path_buf();
        // The claim came from the very chat the agent's control channel
        // points at: excluded explicitly.
        let got = crate::claude_runner::REPLY_CHANNEL
            .scope("telegram:555".to_string(), async move {
                dashboard_only_targets(&home, &r, false)
            })
            .await;
        assert!(got.is_empty(), "{got:?}");
        // A different origin leaves the control channel in place, and the
        // origin itself is never chosen.
        let r = rec("knowledge_quarantine");
        let home = dir.path().to_path_buf();
        let got = crate::claude_runner::REPLY_CHANNEL
            .scope("telegram:777".to_string(), async move {
                dashboard_only_targets(&home, &r, false)
            })
            .await;
        assert_eq!(got, vec![("telegram".to_string(), "555".to_string())]);
    }
}
