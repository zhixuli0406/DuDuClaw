//! The human gate of forget by source (P2-B, C-1).
//!
//! `plan` files one approval request per plan, bound to the plan id and the
//! plan hash. It can only be decided in the dashboard (channel buttons and
//! replies are refused, `approval_notify::is_dashboard_only_kind`) and only by
//! an Admin (`approvals.decide`). `apply` runs only when that request is
//! approved, still names the same plan hash, and the plan has not expired.
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

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus};

/// Fixed text on every forget card and its channel notice (N3). The system
/// cannot tell who ran `plan`, so it says so instead of naming anyone.
pub const ORIGIN_NOTICE: &str = "這筆請求由本機指令列建立，系統無法確認下指令的人是誰。\
    請確認是你本人或你授權的人建立的，才核准；不確定就拒絕。";

/// `action_kind` of a forget-by-source approval.
pub const ACTION_KIND_MEMORY_FORGET_SOURCE: &str = "memory_forget_source";

/// Shortest approval lifetime (a plan about to expire still gets a minute).
const MIN_TTL_SECONDS: i64 = 60;

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
    broker
        .request(
            &plan.document.agent_id,
            ACTION_KIND_MEMORY_FORGET_SOURCE,
            summary,
            payload,
            ttl,
        )
        .await
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

fn payload_str<'a>(rec: &'a ApprovalRecord, key: &str) -> Option<&'a str> {
    rec.payload.get(key).and_then(|v| v.as_str())
}

/// The gate for `plan`: the newest request filed for its plan id decides.
/// Fails (an `Err`) when the approvals store cannot be read — the caller
/// refuses to apply (fail closed).
pub async fn verdict_for_plan(home: &Path, plan: &ForgetPlan) -> Result<GateVerdict, String> {
    let broker = ApprovalBroker::open(home)?;
    let newest = broker
        .list_by_kind(ACTION_KIND_MEMORY_FORGET_SOURCE)
        .await?
        .into_iter()
        .filter(|r| payload_str(r, "plan_id") == Some(plan.plan_id.as_str()))
        .max_by(|a, b| a.created_at.cmp(&b.created_at));
    let Some(rec) = newest else {
        return Ok(GateVerdict::Missing);
    };
    let approval_id = rec.id.to_string();
    Ok(match rec.status {
        ApprovalStatus::Approved
            if payload_str(&rec, "plan_hash") == Some(plan.plan_hash.as_str()) =>
        {
            GateVerdict::Approved { approval_id }
        }
        ApprovalStatus::Approved => GateVerdict::HashMismatch { approval_id },
        ApprovalStatus::Pending if rec.is_stale(chrono::Utc::now()) => GateVerdict::Refused {
            approval_id,
            status: "expired".to_string(),
        },
        ApprovalStatus::Pending => GateVerdict::Pending { approval_id },
        other => GateVerdict::Refused {
            approval_id,
            status: other.as_str().to_string(),
        },
    })
}

#[cfg(test)]
#[path = "memory_forget_approval_tests.rs"]
mod tests;
