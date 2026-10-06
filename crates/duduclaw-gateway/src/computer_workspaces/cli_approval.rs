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
//!    the action, the workspace id and the workspace's state version, and
//!    exits non-zero with [`GO_TO_DASHBOARD`].
//! 2. Re-running the same command finds the approval; if the workspace's
//!    state version is unchanged it consumes the approval (exactly one run
//!    wins) and acts. A changed state invalidates the request and files a
//!    new one.
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
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus};

/// The dashboard-only approval kind.
pub const ACTION_KIND: &str = "computer_workspace_admin";
/// The actor recorded for anything the terminal does.
pub const UNVERIFIED_ACTOR: &str = "本機指令列（身分未驗證）";
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
    format!(
        "電腦操作工作區：{}。工作區 {}，擁有者 {}，{} 個檔案、{} 位元組。{CARD_NOTE}。",
        action.label(),
        row.workspace_id,
        row.owner_agent_id,
        row.files_used,
        row.bytes_used
    )
}

fn payload(action: GatedAction, row: &WorkspaceRow) -> Value {
    json!({
        "action": action.as_str(),
        "workspace_id": row.workspace_id,
        "owner": row.owner_agent_id,
        "state_version": state_version(row),
        "files_used": row.files_used,
        "bytes_used": row.bytes_used,
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
}

fn matches(rec_payload: &Value, action: GatedAction, workspace_id: &str) -> bool {
    rec_payload.get("action").and_then(Value::as_str) == Some(action.as_str())
        && rec_payload.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
}

/// Whether an approved request is still inside its validity window
/// (`valid_minutes` after the decision). An unparsable or missing decision
/// time is outside it (fail closed).
fn still_valid(
    decided_at: Option<&str>,
    valid_minutes: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    decided_at
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| {
            let age = now.signed_duration_since(t.with_timezone(&chrono::Utc));
            age >= chrono::Duration::zero() && age <= chrono::Duration::minutes(valid_minutes)
        })
}

/// Check for a usable approval, or file a request (see the module doc).
///
/// - An approval counts only when an Admin decided it in the dashboard
///   (`dashboard:` prefix), it is bound to the current state version and it
///   was decided within `valid_minutes`; anything else is invalidated so
///   rows do not pile up.
/// - At most one pending request per (action, workspace): a state change
///   updates that request's text in place (no new push); duplicates are
///   invalidated.
pub async fn gate(
    broker: &ApprovalBroker,
    action: GatedAction,
    row: &WorkspaceRow,
    valid_minutes: i64,
) -> Result<Gate, String> {
    let version = state_version(row);
    let now = chrono::Utc::now();
    let mut pending: Option<ApprovalId> = None;
    for rec in broker.list_by_kind(ACTION_KIND).await? {
        if !matches(&rec.payload, action, &row.workspace_id) {
            continue;
        }
        let same_state =
            rec.payload.get("state_version").and_then(Value::as_str) == Some(version.as_str());
        match rec.status {
            ApprovalStatus::Approved => {
                let by_dashboard = rec
                    .decided_by
                    .as_deref()
                    .is_some_and(|by| by.starts_with("dashboard:"));
                let why = if !by_dashboard {
                    Some("not_dashboard_decision")
                } else if !same_state {
                    Some("state_changed")
                } else if !still_valid(rec.decided_at.as_deref(), valid_minutes, now) {
                    Some("approval_expired")
                } else {
                    None
                };
                if let Some(why) = why {
                    broker.invalidate_request(&rec.id, why).await?;
                    continue;
                }
                // Consume: only the run whose nonce the row ends up carrying
                // acts (the UPDATE only matches a still-approved row).
                let nonce = format!("consumed:{}", uuid::Uuid::new_v4().as_simple());
                broker.invalidate_request(&rec.id, &nonce).await?;
                let after = broker.get(&rec.id).await?;
                if after.is_some_and(|r| r.invalidated_reason.as_deref() == Some(nonce.as_str())) {
                    return Ok(Gate::Proceed(rec.id));
                }
            }
            ApprovalStatus::Pending => {
                // `get` settles a pending row past its TTL (fail closed).
                let live = broker
                    .get(&rec.id)
                    .await?
                    .is_some_and(|r| r.status == ApprovalStatus::Pending && !r.is_stale(now));
                if !live {
                    continue;
                }
                if pending.is_some() {
                    broker.invalidate_request(&rec.id, "duplicate").await?;
                    continue;
                }
                if !same_state {
                    // Same request, new state: update its text, no new push.
                    broker
                        .replace_text(&rec.id, &card_summary(action, row), &payload(action, row))
                        .await?;
                }
                pending = Some(rec.id);
            }
            _ => {}
        }
    }
    if let Some(id) = pending {
        return Ok(Gate::Pending(id));
    }
    let id = broker
        .request(
            &row.owner_agent_id,
            ACTION_KIND,
            &card_summary(action, row),
            payload(action, row),
            TTL_SECS,
        )
        .await?;
    Ok(Gate::Requested(id))
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

/// Whether one more push for `rec`'s workspace fits this hour's cap. When
/// it does not, an audit event `admin_approval_push_suppressed` is written
/// and the request stays in the dashboard inbox only.
pub async fn push_allowed(home: &std::path::Path, rec: &ApprovalRecord) -> bool {
    let Some(ws) = rec
        .payload
        .get("workspace_id")
        .and_then(Value::as_str)
        .filter(|id| super::paths::valid_workspace_id(id))
        .map(str::to_string)
    else {
        return false;
    };
    let Ok(broker) = ApprovalBroker::open(home) else {
        return false;
    };
    let Ok(all) = broker.list_by_kind(ACTION_KIND).await else {
        return false;
    };
    let hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
    let pushed = all
        .iter()
        .filter(|r| r.id != rec.id && r.notify_channel.is_some())
        .filter(|r| r.payload.get("workspace_id").and_then(Value::as_str) == Some(ws.as_str()))
        .filter(|r| {
            chrono::DateTime::parse_from_rfc3339(&r.created_at)
                .is_ok_and(|t| t.with_timezone(&chrono::Utc) >= hour_ago)
        })
        .count();
    if pushed < PUSH_CAP_PER_HOUR {
        return true;
    }
    let home = home.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || {
        super::shared::shared_blocking(&home).and_then(|s| {
            s.note(
                &ws,
                "admin_approval_push_suppressed",
                "system:approval_notify",
                json!({"pushed_last_hour": pushed}),
            )
        })
    })
    .await;
    false
}

#[cfg(all(test, unix))]
#[path = "cli_approval_tests.rs"]
mod tests;
