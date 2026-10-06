//! `duduclaw ops channel-ingress` — the operator terminal entry point
//! (review I-HIGH-4 (3)).
//!
//! `list` and `show` answer directly. `resolve` (close / retry) and `rerun`
//! change what happens to a customer's message, and the terminal cannot tell
//! the operator from an AI employee with Bash. So the first run files a
//! dashboard-only approval of kind [`ACTION_KIND`] (Admin only, channel
//! decisions refused), bound to the action, the event, the event's state
//! version and a digest of the note / receipt / confirmation, and exits with
//! [`GO_TO_DASHBOARD`]. Re-running the same command after an Admin approved
//! it in the dashboard consumes the approval once and applies it. A changed
//! event state, a different note, an approval older than
//! [`APPROVAL_VALID_MINUTES`] or one not decided in the dashboard is void.
//! Every requested, applied and refused action writes a security audit row
//! (`channel_ingress_cli_action`). The dashboard's `channel_ingress.*` RPCs,
//! which know who is asking, stay immediate. Matching, merging, caps and
//! consumption are the shared operator-CLI gate
//! (`approval::operator_cli_gate`); this module supplies the binding, the
//! texts and [`SPEC`].
//!
//! Limits: an AI employee with unrestricted Bash can still file requests
//! (at most [`MAX_PENDING`] wait at once, three different ones per event)
//! and can edit the database files
//! directly; real isolation is not granting Bash.

use std::path::Path;

use serde_json::{Value, json};

use super::{IngressRow, IngressStore, ResolveRequest, digest};
use crate::approval::operator_cli_gate::{
    self as shared, Binding, Consume, Filing, KindSpec, StatePolicy, ThrottleScope, Validity,
};
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord};

/// The dashboard-only approval kind.
pub const ACTION_KIND: &str = "channel_ingress_admin";
/// The actor recorded for anything the terminal asks for.
pub const UNVERIFIED_ACTOR: &str = shared::UNVERIFIED_ACTOR;
/// The sentence every card carries.
pub const CARD_NOTE: &str = "這筆請求由本機指令列建立，系統無法確認下指令的人是誰";
/// What the terminal prints while no usable approval exists.
pub const GO_TO_DASHBOARD: &str = "請到儀表板的待辦核准";
/// How long a request waits for a decision.
pub const TTL_SECS: i64 = 86_400;
/// How long an approval stays usable after the decision.
pub const APPROVAL_VALID_MINUTES: i64 = 30;
/// Most terminal requests that may wait at once.
pub const MAX_PENDING: usize = shared::DEFAULT_MAX_PENDING_PER_KIND;
/// First pushes per event (or batch) per hour.
pub const PUSH_CAP_PER_HOUR: usize = 2;
/// What the dashboard answers for a decision on an expired card.
pub const EXPIRED_TEXT: &str =
    "這則收件處理指令的審核已逾期，指令不會執行；需要的話請重新下指令。";

/// A state-changing terminal action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliAction {
    Close,
    Retry,
    Rerun,
}

impl CliAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Retry => "retry",
            Self::Rerun => "rerun",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "close" => Some(Self::Close),
            "retry" => Some(Self::Retry),
            "rerun" => Some(Self::Rerun),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Close => "結案（不再處理這則訊息）",
            Self::Retry => "重試（已證明沒有執行過）",
            Self::Rerun => "重新執行（可能重複執行，已確認風險）",
        }
    }
}

/// One terminal request.
#[derive(Debug, Clone)]
pub struct CliRequest {
    pub action: CliAction,
    pub ingress_id: String,
    pub note: String,
    pub provider_receipt: Option<String>,
    pub confirm_duplicate_risk: bool,
}

/// What a state-changing terminal run did.
#[derive(Debug, Clone, PartialEq)]
pub enum CliOutcome {
    /// The approval was consumed and the action applied.
    Applied(Value),
    /// A request was filed now.
    Requested(String),
    /// A request for exactly this is still waiting.
    Pending(String),
}

/// A 64-hex-digit ingress id.
pub fn valid_ingress_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The state an approval is bound to.
pub(super) fn state_version(row: &IngressRow) -> String {
    format!(
        "{}:{}:{}:{}",
        row.status, row.attempt, row.revision, row.run_id
    )
}

fn request_digest(req: &CliRequest) -> String {
    digest(&[
        req.action.as_str(),
        &req.ingress_id,
        &req.note,
        req.provider_receipt.as_deref().unwrap_or(""),
        if req.confirm_duplicate_risk { "1" } else { "0" },
    ])
}

pub(super) fn short(id: &str) -> String {
    duduclaw_core::truncate_chars(id, 12).to_string()
}

fn card_summary(req: &CliRequest, row: &IngressRow) -> String {
    format!(
        "LINE 收件匣：{}。事件 {}，目前狀態 {}（{}）。操作理由：{}。{CARD_NOTE}。",
        req.action.label(),
        short(&row.id),
        row.status,
        row.reason.as_deref().unwrap_or("無"),
        duduclaw_core::truncate_chars(&req.note, 200),
    )
}

fn payload(req: &CliRequest, row: &IngressRow) -> Value {
    json!({
        "action": req.action.as_str(),
        "ingress_id": row.id,
        "state_version": state_version(row),
        "request_digest": request_digest(req),
        "status": row.status,
        "note": req.note,
        "provider_receipt": req.provider_receipt,
        "confirm_duplicate_risk": req.confirm_duplicate_risk,
        "requested_by": UNVERIFIED_ACTOR,
    })
}

/// Security audit row for one terminal action. `reason` is a fixed code.
pub fn audit(
    home: &Path,
    phase: &str,
    action: &str,
    ingress_id: &str,
    approval: Option<&str>,
    reason: &str,
) {
    let severity = if phase == "applied" {
        duduclaw_security::audit::Severity::Info
    } else {
        duduclaw_security::audit::Severity::Warning
    };
    let event = duduclaw_security::audit::AuditEvent::new(
        "channel_ingress_cli_action",
        UNVERIFIED_ACTOR,
        severity,
        json!({
            "phase": phase,
            "action": duduclaw_core::truncate_chars(action, 16),
            "ingress_id": if valid_ingress_id(ingress_id) { ingress_id } else { "invalid" },
            "approval_id": approval,
            "reason": duduclaw_core::truncate_chars(reason, 48),
        }),
    );
    duduclaw_security::audit::append_audit_event(home, &event);
}

/// Replace LINE account / conversation ids (a group, room or user id) with
/// a short digest in terminal output (review L10). The digest is stable,
/// so events of one conversation still line up.
pub fn redact_line_ids(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if matches!(k.as_str(), "conversation" | "account") {
                    if let Value::String(s) = val {
                        *s = format!("#{}", short(&digest(&["line-id", s])));
                    }
                } else {
                    redact_line_ids(val);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_line_ids),
        _ => {}
    }
}

/// `list`: newest events (never payloads or reply tokens) and counts.
pub async fn list(home: &Path, before_seq: Option<i64>) -> Result<Value, String> {
    let store = IngressStore::open_current(home)?;
    let rows = store.list_page(before_seq).await?;
    let mut out = json!({"events": rows, "summary": store.summary().await?});
    redact_line_ids(&mut out);
    Ok(out)
}

/// `show`: one event with its attempts and run authorizations.
pub async fn show(home: &Path, ingress_id: &str) -> Result<Value, String> {
    if !valid_ingress_id(ingress_id) {
        return Err("ingress_id 格式不正確（64 位十六進位）".into());
    }
    let mut out = IngressStore::open_current(home)?
        .inspect(ingress_id, None, None)
        .await?
        .ok_or_else(|| "找不到這筆收件紀錄".to_string())?;
    redact_line_ids(&mut out);
    Ok(out)
}

pub(super) fn applicable(
    req: &CliRequest,
    row: &IngressRow,
    late: super::config::LateReply,
) -> Result<(), String> {
    if req.action != CliAction::Close
        && !super::resolve::rerun_can_deliver(row.received_at, chrono::Utc::now().timestamp(), late)
    {
        return Err(super::resolve::LATE_FAIL_REFUSAL.into());
    }
    let ok = match req.action {
        CliAction::Close => matches!(
            row.status.as_str(),
            "uncertain" | "quarantined" | "undelivered" | "failed_before_dispatch"
        ),
        CliAction::Retry => super::resolve::retry_allowed(&row.status, row.reason.as_deref()),
        CliAction::Rerun => {
            super::resolve::rerun_allowed(&row.status, row.reason.as_deref())
                && req.confirm_duplicate_risk
        }
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "這筆事件目前是 {}，不能{}。",
            row.status,
            req.action.label()
        ))
    }
}

pub(super) enum Gate {
    /// An approval was consumed: its id, decider and payload.
    Proceed(ApprovalId, String, Value),
    Requested(ApprovalId),
    Pending(ApprovalId),
}

/// What one terminal request is matched on. A single-event request binds
/// the event's state version: an approval granted for another state is
/// void. A batch binds the item set only while it waits: a pending card
/// whose set no longer matches is withdrawn and filed again (review L14e:
/// never rewritten in place), and an approved batch is checked item by item
/// when applied.
pub(super) struct GateSpec {
    pub action: String,
    pub target: String,
    pub request_digest: String,
    pub state: String,
    pub approval_needs_same_state: bool,
    pub summary: String,
    pub payload: Value,
}

pub(super) async fn gate(broker: &ApprovalBroker, spec: GateSpec) -> Result<Gate, String> {
    let bind = Binding {
        action: &spec.action,
        target: &spec.target,
        request_digest: spec.request_digest.clone(),
        state: spec.state.clone(),
        state_policy: if spec.approval_needs_same_state {
            StatePolicy::MustMatch
        } else {
            StatePolicy::PendingOnly
        },
        push_scope: None,
    };
    let filing = Filing {
        agent_id: "gateway",
        summary: &spec.summary,
        extra: spec.payload,
        ttl_secs: TTL_SECS,
    };
    let (decided, _voided) = shared::gate(
        broker,
        &SPEC,
        &bind,
        filing,
        Some(APPROVAL_VALID_MINUTES),
        chrono::Utc::now(),
    )
    .await?;
    match decided {
        shared::Gate::Proceed(claim) => Ok(Gate::Proceed(claim.id, claim.decided_by, claim.payload)),
        shared::Gate::Requested(id) => Ok(Gate::Requested(id)),
        shared::Gate::Pending(id) => Ok(Gate::Pending(id)),
        shared::Gate::Throttled {
            waiting,
            scope: ThrottleScope::Kind,
        } => Err(format!(
            "已有 {waiting} 筆指令列請求等待核准，請先到儀表板處理後再送新的請求。"
        )),
        shared::Gate::Throttled { waiting, .. } => Err(format!(
            "這筆事件已有 {waiting} 筆不同內容的請求等待核准，請先到儀表板處理後再送新的請求。"
        )),
        shared::Gate::AlreadyClaimed(id) => Err(format!(
            "這筆核准（編號 {}）已被另一次執行用掉或作廢，這次沒有執行任何動作；需要的話請重新執行指令，送出新的核准請求。",
            short(id.as_str())
        )),
    }
}

async fn single_gate(
    broker: &ApprovalBroker,
    req: &CliRequest,
    row: &IngressRow,
) -> Result<Gate, String> {
    gate(
        broker,
        GateSpec {
            action: req.action.as_str().to_string(),
            target: row.id.clone(),
            request_digest: request_digest(req),
            state: state_version(row),
            approval_needs_same_state: true,
            summary: card_summary(req, row),
            payload: payload(req, row),
        },
    )
    .await
}

/// `resolve` / `rerun`: file a dashboard approval, or apply one that an
/// Admin already granted for exactly this request and state.
pub async fn request_or_apply(home: &Path, req: &CliRequest) -> Result<CliOutcome, String> {
    let act = req.action.as_str();
    let refuse = |reason: &str, msg: String| {
        audit(home, "refused", act, &req.ingress_id, None, reason);
        msg
    };
    if !valid_ingress_id(&req.ingress_id) {
        return Err(refuse(
            "invalid_id",
            "ingress_id 格式不正確（64 位十六進位）".into(),
        ));
    }
    if req.note.trim().is_empty() || req.note.len() > super::resolve::NOTE_MAX_BYTES {
        return Err(refuse(
            "invalid_note",
            "請用 --note 寫明理由（2000 位元組以內）".into(),
        ));
    }
    let store = IngressStore::open_current(home).map_err(|e| refuse("store_unavailable", e))?;
    let row = store
        .get(&req.ingress_id)
        .await
        .map_err(|e| refuse("store_unavailable", e))?
        .ok_or_else(|| refuse("not_found", "找不到這筆收件紀錄".into()))?;
    let late = super::config::IngressConfig::load(home).await.late_reply;
    applicable(req, &row, late).map_err(|m| refuse("not_applicable", m))?;
    let broker = ApprovalBroker::open(home).map_err(|e| refuse("approvals_unavailable", e))?;
    match single_gate(&broker, req, &row)
        .await
        .map_err(|e| refuse("approvals_unavailable", e))?
    {
        Gate::Requested(id) => {
            audit(
                home,
                "requested",
                act,
                &row.id,
                Some(id.as_str()),
                "awaiting_dashboard_approval",
            );
            Ok(CliOutcome::Requested(id.as_str().to_string()))
        }
        Gate::Pending(id) => {
            audit(
                home,
                "requested",
                act,
                &row.id,
                Some(id.as_str()),
                "awaiting_dashboard_approval",
            );
            Ok(CliOutcome::Pending(id.as_str().to_string()))
        }
        Gate::Proceed(id, decided_by, _) => {
            let actor = format!("{UNVERIFIED_ACTOR}；核准者 {decided_by}");
            let result = store
                .resolve_request(&ResolveRequest {
                    id: &row.id,
                    expected_revision: &row.revision,
                    expected_attempt: row.attempt,
                    action: act,
                    confirm_duplicate_risk: req.confirm_duplicate_risk,
                    actor: &actor,
                    note: &req.note,
                    provider_receipt: req.provider_receipt.as_deref(),
                    now: chrono::Utc::now().timestamp(),
                    late_reply: late,
                })
                .await;
            match result {
                Ok(()) => {
                    audit(home, "applied", act, &row.id, Some(id.as_str()), "applied");
                    Ok(CliOutcome::Applied(json!({
                        "ingress_id": row.id,
                        "action": act,
                        "approval_id": id.as_str(),
                    })))
                }
                Err(e) => {
                    audit(
                        home,
                        "refused",
                        act,
                        &row.id,
                        Some(id.as_str()),
                        "action_failed",
                    );
                    Err(format!("核准已使用，需要重新申請：{e}"))
                }
            }
        }
    }
}

fn spec_notice(rec: &ApprovalRecord, reminder: bool, _deadline: &str) -> String {
    notice_body(rec, reminder)
}

/// Security audit row for a push past the hourly cap.
fn audit_push_suppressed(home: &Path, rec: &ApprovalRecord, pushed: usize) {
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "channel_ingress_approval_push_suppressed",
            UNVERIFIED_ACTOR,
            duduclaw_security::audit::Severity::Info,
            json!({
                "approval_id": duduclaw_core::truncate_chars(rec.id.as_str(), 36),
                "pushed_last_hour": pushed,
            }),
        ),
    );
}

/// This kind in the shared operator-CLI gate registry.
pub const SPEC: KindSpec = KindSpec {
    kind: ACTION_KIND,
    validity: Validity::Fixed(APPROVAL_VALID_MINUTES),
    consume: Consume::Once,
    reminders: true,
    max_pending_per_target: shared::DEFAULT_MAX_PENDING_PER_TARGET,
    max_pending_per_kind: MAX_PENDING,
    push_cap_per_hour: Some(PUSH_CAP_PER_HOUR),
    legacy_scope_key: "ingress_id",
    admin_refusal: "LINE 收件匣的指令列請求只能由管理員（Admin）核准。",
    expired_text: EXPIRED_TEXT,
    channel_refusal: None,
    notice: spec_notice,
    on_push_suppressed: audit_push_suppressed,
};

/// The plain channel notice for one of these requests: action, event,
/// where to decide, deadline. No note text and no decision verb.
pub fn notice_body(rec: &ApprovalRecord, reminder: bool) -> String {
    let head = if reminder {
        "⏰ 有一筆 LINE 收件匣的管理請求快到期了，逾時會自動拒絕"
    } else {
        "📥 有一筆 LINE 收件匣的管理請求等待管理員處理"
    };
    let action = rec
        .payload
        .get("action")
        .and_then(Value::as_str)
        .and_then(CliAction::parse)
        .map(CliAction::label)
        .unwrap_or("處理收件事件");
    let batch = rec.payload.get("batch").and_then(Value::as_bool) == Some(true);
    let event = if batch {
        format!(
            "批次 {} 則",
            rec.payload
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        )
    } else {
        rec.payload
            .get("ingress_id")
            .and_then(Value::as_str)
            .filter(|id| valid_ingress_id(id))
            .map(short)
            .unwrap_or_else(|| "（無法辨識）".into())
    };
    format!(
        "{head}\n動作：{action}\n事件：{event}\n{CARD_NOTE}。\n只能由管理員在儀表板的待辦清單決定。\n編號：{}",
        duduclaw_core::truncate_chars(rec.id.as_str(), 8)
    )
}

#[cfg(test)]
#[path = "cli_approval_tests.rs"]
mod tests;
