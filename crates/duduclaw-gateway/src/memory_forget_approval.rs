//! The human gate of forget by source (P2-B, C-1).
//!
//! `plan` files one approval request per plan, bound to the plan id and the
//! plan hash, through the shared operator-CLI gate
//! (`approval::operator_cli_gate`, [`SPEC`]). It can only be decided in the
//! dashboard (channel buttons and replies are refused,
//! `approval_notify::is_dashboard_only_kind`) and only by an Admin
//! (`approvals.decide`). `apply` runs only when that request was approved in
//! the dashboard (`decided_by` starts with `dashboard:`), still names the
//! same plan hash, and the plan has not expired. The approval is not
//! consumed: the plan itself can be applied only once. At most a few
//! requests wait at once and pushes are capped per namespace per hour.
//! There is no setting that turns this gate off.
//!
//! What this does and does not guarantee (N1): through the product's own
//! commands an AI employee cannot approve its own request. An employee
//! running as the same operating-system user that can execute arbitrary
//! commands and deliberately evades the file guard can still rewrite the
//! local databases directly (`approvals.db`, `memory.db`). Real isolation
//! comes from not granting Bash or from running the employee in the task
//! sandbox.
//!
//! The card carries counts and source labels only, never memory content.

use std::path::Path;

use duduclaw_memory::ForgetPlan;

use crate::approval::operator_cli_gate::{
    self as shared, Binding, Consume, Filing, KindSpec, StatePolicy, Validity,
};
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord};
#[cfg(test)]
use crate::approval::ApprovalStatus;

/// Fixed text on every forget card and its channel notice (N3). The system
/// cannot tell who ran `plan`, so it says so instead of naming anyone.
pub const ORIGIN_NOTICE: &str = "這筆請求由本機指令列建立，系統無法確認下指令的人是誰。\
    請確認是你本人或你授權的人建立的，才核准；不確定就拒絕。";

/// `action_kind` of a forget-by-source approval.
pub const ACTION_KIND_MEMORY_FORGET_SOURCE: &str = "memory_forget_source";

/// Shortest approval lifetime (a plan about to expire still gets a minute).
const MIN_TTL_SECONDS: i64 = 60;

/// The binding action of every forget request.
const GATE_ACTION: &str = "forget_source";

/// First pushes per namespace per hour.
pub const PUSH_CAP_PER_HOUR: usize = 2;

/// What the dashboard answers for a decision on an expired card.
pub const EXPIRED_TEXT: &str = "這筆忘記請求已逾期，已自動拒絕；請重新建立計畫。";

/// Where an apply stands with respect to the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// Approved in the dashboard for exactly this plan and hash.
    Approved { approval_id: String },
    /// Still waiting for a decision.
    Pending { approval_id: String },
    /// Denied, withdrawn or expired.
    Refused { approval_id: String, status: String },
    /// The approved request names another plan hash.
    HashMismatch { approval_id: String },
    /// No request was ever filed for this plan.
    Missing,
}

/// File the approval request for `plan`. `summary` is the operator-facing
/// one-paragraph description (counts and source labels only).
pub async fn request_for_plan(
    home: &Path,
    plan: &ForgetPlan,
    summary: &str,
    counts: serde_json::Value,
) -> Result<ApprovalId, String> {
    let broker = ApprovalBroker::open(home)?;
    let ttl = chrono::DateTime::parse_from_rfc3339(&plan.document.expires_at)
        .map(|t| (t.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds())
        .unwrap_or(MIN_TTL_SECONDS)
        .max(MIN_TTL_SECONDS);
    let payload = serde_json::json!({
        "plan_id": plan.plan_id,
        "plan_hash": plan.plan_hash,
        "agent_id": plan.document.agent_id,
        "plan_expires_at": plan.document.expires_at,
        "counts": counts,
    });
    let bind = binding(plan);
    let filing = Filing {
        agent_id: &plan.document.agent_id,
        summary,
        extra: payload,
        ttl_secs: ttl,
    };
    let (decided, _) = shared::gate(&broker, &SPEC, &bind, filing, None, chrono::Utc::now()).await?;
    match decided {
        shared::Gate::Requested(id)
        | shared::Gate::Pending(id)
        | shared::Gate::AlreadyClaimed(id) => Ok(id),
        shared::Gate::Proceed(claim) => Ok(claim.id),
        shared::Gate::Throttled { waiting, .. } => Err(format!(
            "已有 {waiting} 筆依來源刪除記憶的請求在等核准，請先到儀表板處理"
        )),
    }
}

fn binding(plan: &ForgetPlan) -> Binding<'_> {
    Binding {
        action: GATE_ACTION,
        target: &plan.plan_id,
        request_digest: plan.plan_hash.clone(),
        state: String::new(),
        state_policy: StatePolicy::MustMatch,
        push_scope: Some(&plan.document.agent_id),
    }
}

/// The zh-TW channel notice for a pending forget request: who, how much,
/// until when. No memory content, no decision verb — it can only be decided
/// in the dashboard.
pub(crate) fn notice_body(rec: &ApprovalRecord, reminder: bool, deadline: &str) -> String {
    let head = if reminder {
        "⏰ 有一筆依來源刪除記憶的請求快到期了，逾時會自動拒絕"
    } else {
        "📥 有一筆依來源刪除記憶的請求等待管理員核准"
    };
    let counts = rec.payload.get("counts");
    let n = |k: &str| {
        counts
            .and_then(|c| c.get(k))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };
    format!(
        "{head}\n\
         命名空間：{agent}\n\
         建立時間：{created}\n\
         影響：記憶 {mem} 筆、對話訊息 {msg} 則、知識頁 {wiki} 頁\n\
         {ORIGIN_NOTICE}\n\
         這類請求只能由管理員在儀表板的待辦清單核准。\n\
         期限：{deadline}未核准將自動拒絕\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.agent_id, 64)),
        created = duduclaw_core::truncate_chars(&rec.created_at, 19).replace('T', " "),
        mem = n("memories"),
        msg = n("session_messages"),
        wiki = n("wiki_pages"),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

/// The gate for `plan`: the newest request filed for its plan id decides.
/// An approval counts only when decided in the dashboard. Fails (an `Err`)
/// when the approvals store cannot be read — the caller refuses to apply
/// (fail closed).
pub async fn verdict_for_plan(home: &Path, plan: &ForgetPlan) -> Result<GateVerdict, String> {
    let broker = ApprovalBroker::open(home)?;
    let v = shared::verdict(
        &broker,
        &SPEC,
        GATE_ACTION,
        &plan.plan_id,
        &plan.plan_hash,
        None,
        chrono::Utc::now(),
    )
    .await?;
    Ok(match v {
        shared::Verdict::Approved { id } => GateVerdict::Approved {
            approval_id: id.to_string(),
        },
        shared::Verdict::Pending { id } => GateVerdict::Pending {
            approval_id: id.to_string(),
        },
        shared::Verdict::Refused { id, status } => GateVerdict::Refused {
            approval_id: id.to_string(),
            status,
        },
        shared::Verdict::Mismatch { id } => GateVerdict::HashMismatch {
            approval_id: id.to_string(),
        },
        shared::Verdict::Missing => GateVerdict::Missing,
    })
}

/// Security audit row for a push past the hourly cap.
fn audit_push_suppressed(home: &Path, rec: &ApprovalRecord, pushed: usize) {
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "memory_forget_approval_push_suppressed",
            &rec.agent_id,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({"pushed_last_hour": pushed}),
        ),
    );
}

/// This kind in the shared operator-CLI gate registry.
pub const SPEC: KindSpec = KindSpec {
    kind: ACTION_KIND_MEMORY_FORGET_SOURCE,
    validity: Validity::UntilRequestExpiry,
    consume: Consume::Never,
    reminders: true,
    max_pending_per_target: shared::DEFAULT_MAX_PENDING_PER_TARGET,
    max_pending_per_kind: shared::DEFAULT_MAX_PENDING_PER_KIND,
    push_cap_per_hour: Some(PUSH_CAP_PER_HOUR),
    legacy_scope_key: "agent_id",
    admin_refusal: "依來源刪除記憶的請求只有管理員（Admin）能決定",
    expired_text: EXPIRED_TEXT,
    channel_refusal: None,
    notice: notice_body,
    on_push_suppressed: audit_push_suppressed,
};

#[cfg(test)]
#[path = "memory_forget_approval_tests.rs"]
mod tests;
