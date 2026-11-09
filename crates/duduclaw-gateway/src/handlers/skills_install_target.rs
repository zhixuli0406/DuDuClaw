//! SR5 item 1 (native Linux isolation v1.81): where a dashboard skill / MCP
//! install may write.
//!
//! `skills.install` names its employee in `scope` (not `agent_id`), and the
//! approved-request paths (`install_requests.decide` → `execute_approved_install`,
//! the channel press → `install_notify::apply_install_request`) replay that
//! `scope` / `agent_id` long after the request was filed. Three rules, shared
//! by every path:
//!
//! - **Scope is an employee key.** [`scope_employee`] is the employee a
//!   `scope` names (`None` for `global` / `department:*`). The P1g default
//!   deny reads it (`personal_p1g_gate::EMPLOYEE_SCOPE_KEYS`) and
//!   `skills.install` is a P1e config write on it (`personal_data_gate::
//!   write_scoped`): owner, or Admin + fresh step-up + audit row.
//! - **Live employees only.** [`live_employee_dir`] refuses a label whose
//!   lifecycle says it is not live — the F6h writer gate
//!   (`isolation_label_stores::check_label_writable`: an open quarantine row,
//!   a disabled confirmed row, an unreadable index all refuse; protected mode
//!   only) — and a label without a real `agents/<label>/agent.toml` (any
//!   mode). It never creates `agents/<label>`; the skill loader only creates
//!   `SKILLS/` inside an existing employee tree.
//! - **Approved requests re-check ownership.** [`authorize_requested_write`]:
//!   in protected mode a Personal employee's install runs only when the
//!   requester is still its owner (the Admin signature is no break-glass), and
//!   only after a config-write audit row (`isolation.personal_config_write`)
//!   naming requester, decider, employee and owner was written (a failed
//!   write refuses). Anything else is refused and audited best effort.
use std::path::{Path, PathBuf};

use duduclaw_auth::UserDb;
use serde_json::json;

use super::personal_data_gate::{WRITE_ACTION, WRITE_REFUSED_ACTION};
use crate::personal_visibility as visibility;

/// The install target is not a live employee (unknown, tombstoned,
/// quarantined or disabled). Nothing was written.
pub(crate) const NOT_LIVE_TARGET: &str = "找不到這位在職的 AI 員工（可能已停用或註銷），未安裝。 / This is not a live \
    employee (unknown, disabled or tombstoned); nothing was installed.";
/// The id a `scope` / `agent_id` carries is not a valid employee id.
pub(crate) const INVALID_TARGET: &str = "Invalid agent_id for scope";
/// An approved request names another person's Personal employee.
pub(crate) const REQUEST_NOT_OWNER: &str = "這位 AI 員工是某位同仁的個人專屬員工，只有它的擁有者提出的安裝申請才能執行。 / \
    This is a person's Personal employee; only an install requested by its owner may run.";
const AUDIT_FAILED_WRITE: &str = "無法寫入稽核紀錄，已拒絕變更。 / The audit row could not be written; refused.";

/// The employee a skill install `scope` names. `global` and
/// `department:<name>` are org-wide layers (no employee); anything else is
/// an employee label. Blank is no employee (the handler refuses it).
pub(crate) fn scope_employee(scope: &str) -> Option<&str> {
    if scope.is_empty() || scope == "global" || scope.starts_with("department:") {
        None
    } else {
        Some(scope)
    }
}

/// The live employee tree `agents/<label>` an install may write into.
/// Never creates it. Protected mode is read from the F6h gate's switch.
pub(crate) fn live_employee_dir(home: &Path, label: &str) -> Result<PathBuf, &'static str> {
    if !super::is_valid_agent_id(label) {
        return Err(INVALID_TARGET);
    }
    // F6h writer gate: open quarantine row / disabled row / unreadable index.
    crate::isolation_label_stores::check_label_writable(home, label).map_err(|refusal| refusal.message())?;
    // Lifecycle read (every mode): a real employee tree with its agent.toml.
    let dir = home.join("agents").join(label);
    let is_dir = std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_dir());
    let has_toml = std::fs::symlink_metadata(dir.join("agent.toml")).is_ok_and(|meta| meta.file_type().is_file());
    if !is_dir || !has_toml {
        return Err(NOT_LIVE_TARGET);
    }
    Ok(dir)
}

/// The employee an install request's payload writes into: a skill's
/// employee `scope`, an MCP request's `agent_id`. `None` = an org-wide
/// skill layer (or a malformed payload, refused by the executor).
pub(crate) fn request_employee(req: &crate::install_requests::InstallRequest) -> Option<&str> {
    let payload = &req.payload;
    match req.kind.as_str() {
        "skill" => payload.get("scope").and_then(serde_json::Value::as_str).and_then(scope_employee),
        "mcp" => payload.get("agent_id").and_then(serde_json::Value::as_str).filter(|agent| !agent.is_empty()),
        _ => None,
    }
}

/// The audit `method` of an approved request's write.
pub(crate) fn request_method(req: &crate::install_requests::InstallRequest) -> &'static str {
    if req.kind == "mcp" { "mcp.install_request" } else { "skills.install_request" }
}

/// What an approved install request is about to write.
pub(crate) struct RequestedWrite<'a> {
    /// `skills.install_request` / `mcp.install_request` (audit `method`).
    pub method: &'a str,
    pub request_id: &'a str,
    pub requester: &'a str,
    pub decider: &'a str,
    pub employee: &'a str,
}

/// Re-check the Personal ownership of an approved request's employee at
/// execution time (see the module docs). `protected = false`: `Ok` without
/// I/O. Shared / unknown employees: `Ok` (the live check still applies).
pub(crate) fn authorize_requested_write(
    home: &Path,
    db: Option<&UserDb>,
    protected: bool,
    write: &RequestedWrite<'_>,
) -> Result<(), &'static str> {
    if !protected {
        return Ok(());
    }
    let owner = match visibility::personal_owner(home, write.employee) {
        Ok(None) => return Ok(()),
        Ok(Some(owner)) => owner,
        Err(_) => return Err(visibility::OWNERSHIP_UNAVAILABLE),
    };
    let detail = json!({
        "user_id": write.requester,
        "decider_user_id": write.decider,
        "employee": write.employee,
        "owner_user_id": owner,
        "category": visibility::DataCategory::Config.as_str(),
        "method": write.method,
        "request_id": write.request_id,
    })
    .to_string();
    let person = !write.requester.is_empty() && write.requester != "system";
    if !person || owner != write.requester {
        if let Some(db) = db {
            let _ = db.log_action(Some(write.decider), WRITE_REFUSED_ACTION, Some(write.employee),
                Some(detail.as_str()), None);
        }
        return Err(REQUEST_NOT_OWNER);
    }
    // SR5 round 3 (nit): the config-write audit row is written only once the
    // target is a live employee tree, never ahead of a refusal (a tombstoned,
    // quarantined or missing tree is refused and audited as refused).
    if let Err(refusal) = live_employee_dir(home, write.employee) {
        if let Some(db) = db {
            let _ = db.log_action(Some(write.decider), WRITE_REFUSED_ACTION, Some(write.employee),
                Some(detail.as_str()), None);
        }
        return Err(refusal);
    }
    db.ok_or(AUDIT_FAILED_WRITE)?
        .log_action(Some(write.requester), WRITE_ACTION, Some(write.employee), Some(detail.as_str()), None)
        .map_err(|_| AUDIT_FAILED_WRITE)
}

#[cfg(all(test, unix))]
#[path = "skills_install_target_tests.rs"]
mod tests;
