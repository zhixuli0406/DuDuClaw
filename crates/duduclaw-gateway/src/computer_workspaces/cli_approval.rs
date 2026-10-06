//! Dashboard approval for operator-terminal workspace changes (review H2).
//!
//! `duduclaw ops computer-workspaces fence | revoke | regrant | renew |
//! delete` cannot prove who typed it: anything with a shell under this OS
//! user could. So every state-changing terminal action only runs after an
//! Admin approved it in the dashboard (emergencies go through the
//! dashboard RPCs or the master switch, which do know who is asking):
//!
//! 1. The first run files an ApprovalBroker request of the dashboard-only
//!    kind [`ACTION_KIND`] (channel decisions refused, Admin only), bound to
//!    the action, the workspace id, the fence reason and the workspace's
//!    state version, and exits non-zero with [`GO_TO_DASHBOARD`].
//! 2. Re-running the same command finds the approval; if the workspace's
//!    state version is unchanged it consumes the approval (exactly one run
//!    wins) and acts. A changed state invalidates the request (approved or
//!    waiting, reason `state_changed`) and files a new one.
//!
//! Matching, merging, caps and consumption are the shared operator-CLI gate
//! (`approval::operator_cli_gate`); this module supplies the binding, the
//! texts and [`SPEC`].
//!
//! No setting turns this off. Only `list` needs no approval.
//!
//! Limits (review M-2): this gate binds the product paths (the CLI, the
//! dashboard, channels). An AI employee with unrestricted Bash can bypass
//! it the same way it can bypass any terminal command: it can run the CLI
//! with no DuDuClaw variables, or edit `approvals.db` /
//! `computer_workspaces.db` and the workspace directories directly. Real
//! isolation is not granting Bash, or the task sandbox.

use serde_json::{Value, json};

use super::store::WorkspaceRow;
use crate::approval::operator_cli_gate::{
    self as shared, Binding, Consume, Filing, KindSpec, StatePolicy, Validity,
};
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord};
#[cfg(test)]
use crate::approval::ApprovalStatus;

/// The dashboard-only approval kind.
pub const ACTION_KIND: &str = "computer_workspace_admin";
/// The actor recorded for anything the terminal does.
pub const UNVERIFIED_ACTOR: &str = shared::UNVERIFIED_ACTOR;
/// The sentence every card carries.
pub const CARD_NOTE: &str = "這筆請求由本機指令列建立，系統無法確認下指令的人是誰";
/// What the terminal prints while no usable approval exists.
pub const GO_TO_DASHBOARD: &str = "請到儀表板的待辦核准";
/// How long a request waits for a decision.
pub const TTL_SECS: i64 = 86_400;

/// Every state-changing terminal action: all of them need an approval
/// (appendix C, M-2 as amended: the terminal cannot tell the operator from
/// an employee with Bash, so even the brakes wait for an Admin there; the
/// dashboard RPCs, which know who is asking, stay immediate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatedAction {
    Fence,
    Revoke,
    Regrant,
    Renew,
    Delete,
}

impl GatedAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fence => "fence",
            Self::Revoke => "revoke",
            Self::Regrant => "regrant",
            Self::Renew => "renew",
            Self::Delete => "delete",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fence => "凍結（立刻收回目前 session 的控制權）",
            Self::Revoke => "暫停授權（之後不能讀、寫或掛載）",
            Self::Regrant => "解除撤銷",
            Self::Renew => "延長保留期限",
            Self::Delete => "刪除工作區與其中所有檔案",
        }
    }
}

/// The state an approval is bound to: state, permission revision and lease
/// epoch. Any change (a revoke, a regrant, a new attach, a fence) moves it.
pub fn state_version(row: &WorkspaceRow) -> String {
    format!(
        "{}:{}:{}",
        row.state.as_str(),
        row.permission_revision,
        row.lease_epoch
    )
}

/// The card: workspace id, owner, action and counts only.
pub fn card_summary(action: GatedAction, row: &WorkspaceRow) -> String {
    card_with_reason(action, row, None)
}

fn card_with_reason(action: GatedAction, row: &WorkspaceRow, reason: Option<&str>) -> String {
    let why = reason
        .map(|r| {
            format!(
                "理由（使用者輸入，僅供參考）：「{}」。",
                duduclaw_core::truncate_chars(r, 80)
            )
        })
        .unwrap_or_default();
    format!(
        "電腦操作工作區：{}。工作區 {}，擁有者 {}，{} 個檔案、{} 位元組。{why}{CARD_NOTE}。",
        action.label(),
        row.workspace_id,
        row.owner_agent_id,
        row.files_used,
        row.bytes_used
    )
}

fn payload(action: GatedAction, row: &WorkspaceRow, reason: Option<&str>) -> Value {
    json!({
        "action": action.as_str(),
        "workspace_id": row.workspace_id,
        "owner": row.owner_agent_id,
        "state_version": state_version(row),
        "files_used": row.files_used,
        "bytes_used": row.bytes_used,
        "reason": reason,
        "requested_by": UNVERIFIED_ACTOR,
    })
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// An approval for exactly this action and state was consumed: act now.
    Proceed(ApprovalId),
    /// A request was filed now.
    Requested(ApprovalId),
    /// A request for this action and state is still waiting.
    Pending(ApprovalId),
    /// Too many requests wait already; nothing was filed.
    Throttled(usize),
    /// The approval was used or invalidated by another run a moment ago.
    AlreadyClaimed(ApprovalId),
}

/// The request digest: action, workspace and (for a fence) the reason.
fn request_digest(action: GatedAction, workspace_id: &str, reason: Option<&str>) -> String {
    shared::digest(&[action.as_str(), workspace_id, reason.unwrap_or("")])
}

/// [`gate_with_reason`] without a reason.
pub async fn gate(
    broker: &ApprovalBroker,
    action: GatedAction,
    row: &WorkspaceRow,
    valid_minutes: i64,
) -> Result<Gate, String> {
    gate_with_reason(broker, action, row, None, valid_minutes).await
}

/// Check for a usable approval, or file a request (see the module doc).
///
/// - An approval counts only when an Admin decided it in the dashboard
///   (`dashboard:` prefix), for the same reason, bound to the current state
///   version and decided within `valid_minutes`; anything else is
///   invalidated so rows do not pile up.
/// - One waiting request per (action, workspace, reason): a state change
///   invalidates it (`state_changed`) and files a new one.
pub async fn gate_with_reason(
    broker: &ApprovalBroker,
    action: GatedAction,
    row: &WorkspaceRow,
    reason: Option<&str>,
    valid_minutes: i64,
) -> Result<Gate, String> {
    let bind = Binding {
        action: action.as_str(),
        target: &row.workspace_id,
        request_digest: request_digest(action, &row.workspace_id, reason),
        state: state_version(row),
        state_policy: StatePolicy::MustMatch,
        push_scope: None,
    };
    let summary = card_with_reason(action, row, reason);
    let filing = Filing {
        agent_id: &row.owner_agent_id,
        summary: &summary,
        extra: payload(action, row, reason),
        ttl_secs: TTL_SECS,
    };
    let (decided, _voided) = shared::gate(
        broker,
        &SPEC,
        &bind,
        filing,
        Some(valid_minutes),
        chrono::Utc::now(),
    )
    .await?;
    Ok(match decided {
        shared::Gate::Proceed(claim) => Gate::Proceed(claim.id),
        shared::Gate::Requested(id) => Gate::Requested(id),
        shared::Gate::Pending(id) => Gate::Pending(id),
        shared::Gate::Throttled { waiting, .. } => Gate::Throttled(waiting),
        shared::Gate::AlreadyClaimed(id) => Gate::AlreadyClaimed(id),
    })
}

/// One phase of a terminal action, for the security audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliPhase {
    /// A request was filed or is still waiting.
    Requested,
    /// The approval was consumed and the action ran.
    Applied,
    /// Refused (environment, bad input, state changed, action failed …).
    Refused,
}

impl CliPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Applied => "applied",
            Self::Refused => "refused",
        }
    }
}

/// Security audit row for one terminal action (`audit.jsonl`, event
/// `computer_workspace_cli_action`). `reason` is a fixed code chosen by the
/// caller, never free text; the actor is always [`UNVERIFIED_ACTOR`].
pub fn audit_cli_action(
    home: &std::path::Path,
    phase: CliPhase,
    action: &str,
    workspace_id: &str,
    approval_id: Option<&str>,
    reason: &str,
) {
    let ws = if super::paths::valid_workspace_id(workspace_id) {
        workspace_id
    } else {
        "invalid"
    };
    let severity = match phase {
        CliPhase::Applied => duduclaw_security::audit::Severity::Info,
        _ => duduclaw_security::audit::Severity::Warning,
    };
    let event = duduclaw_security::audit::AuditEvent::new(
        "computer_workspace_cli_action",
        UNVERIFIED_ACTOR,
        severity,
        json!({
            "phase": phase.as_str(),
            "action": duduclaw_core::truncate_chars(action, 16),
            "workspace_id": ws,
            "approval_id": approval_id,
            "reason": duduclaw_core::truncate_chars(reason, 48),
        }),
    );
    duduclaw_security::audit::append_audit_event(home, &event);
}

/// Pushes per workspace per hour for terminal-filed requests (review M-3).
pub const PUSH_CAP_PER_HOUR: usize = 2;

/// The dashboard's answer when one of these requests is past its deadline.
pub const EXPIRED_TEXT: &str =
    "這筆電腦操作工作區的管理請求已逾期並自動拒絕；需要的話請在終端機重新執行同一個指令。";

/// The plain channel notice for one of these requests: action, workspace,
/// owner, the unverified-terminal sentence, where to decide, deadline. No
/// decision verb (decisions happen in the dashboard only).
pub fn notice_body(rec: &ApprovalRecord, reminder: bool, deadline: &str) -> String {
    let head = if reminder {
        "⏰ 有一筆電腦操作工作區的管理請求快到期了，逾時會自動拒絕"
    } else {
        "📥 有一筆電腦操作工作區的管理請求等待管理員處理"
    };
    let action = match rec.payload.get("action").and_then(Value::as_str) {
        Some("fence") => GatedAction::Fence.label(),
        Some("revoke") => GatedAction::Revoke.label(),
        Some("regrant") => GatedAction::Regrant.label(),
        Some("renew") => GatedAction::Renew.label(),
        Some("delete") => GatedAction::Delete.label(),
        _ => "管理工作區",
    };
    let ws = rec
        .payload
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|id| super::paths::valid_workspace_id(id))
        .unwrap_or("（無法辨識）");
    let owner = rec
        .payload
        .get("owner")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!(
        "{head}\n\
         動作：{action}\n\
         工作區：{ws}\n\
         擁有者（AI 員工）：{owner}\n\
         {CARD_NOTE}。\n\
         只能由管理員在儀表板的待辦清單決定。\n\
         期限：{deadline}未決定將自動拒絕\n\
         編號：{id}",
        owner = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(owner, 64)),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

/// Whether one more push for `rec`'s workspace fits this hour's cap (the
/// shared operator-CLI push cap). When it does not, a workspace event
/// `admin_approval_push_suppressed` is written and the request stays in the
/// dashboard inbox only.
pub async fn push_allowed(home: &std::path::Path, rec: &ApprovalRecord) -> bool {
    shared::push_allowed(home, rec).await
}

/// Workspace event for a push past the hourly cap.
fn note_push_suppressed(home: &std::path::Path, rec: &ApprovalRecord, pushed: usize) {
    let ws = shared::binding_of(rec)
        .map(|b| b.target)
        .or_else(|| {
            rec.payload
                .get("workspace_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|id| super::paths::valid_workspace_id(id));
    let Some(ws) = ws else { return };
    let _ = super::shared::shared_blocking(home).and_then(|s| {
        s.note(
            &ws,
            "admin_approval_push_suppressed",
            "system:approval_notify",
            json!({"pushed_last_hour": pushed}),
        )
    });
}

/// `[computer_use.workspaces] admin_approval_minutes` (unreadable ⇒ 30).
fn configured_minutes(home: &std::path::Path) -> i64 {
    super::config::load(home)
        .map(|c| i64::from(c.admin_approval_minutes))
        .unwrap_or(30)
}

/// This kind in the shared operator-CLI gate registry.
pub const SPEC: KindSpec = KindSpec {
    kind: ACTION_KIND,
    validity: Validity::Config(configured_minutes),
    consume: Consume::Once,
    reminders: true,
    max_pending_per_target: shared::DEFAULT_MAX_PENDING_PER_TARGET,
    max_pending_per_kind: shared::DEFAULT_MAX_PENDING_PER_KIND,
    push_cap_per_hour: Some(PUSH_CAP_PER_HOUR),
    legacy_scope_key: "workspace_id",
    admin_refusal: "電腦操作工作區的管理請求只能由管理員（Admin）核准。",
    expired_text: EXPIRED_TEXT,
    channel_refusal: None,
    notice: notice_body,
    on_push_suppressed: note_push_suppressed,
};

#[cfg(all(test, unix))]
#[path = "cli_approval_tests.rs"]
mod tests;
