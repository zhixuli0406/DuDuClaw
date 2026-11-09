//! Blocking bodies of the F6i purge RPCs (child module of `isolation_purge`).
//! The dashboard handler (`handlers/isolation_employee_purge.rs`) checks
//! Admin + current MFA + a fresh migration step-up with its CSRF token and
//! refuses `system`; the early-purge decision rides on
//! `isolation.employee_restore_decide` (`handlers/isolation_employee_restore.rs`
//! calls [`decide_early`] first and falls through for every other kind).
//!
//! - [`quarantine_purge_rpc`]: `isolation.quarantine_purge` → `purged` (≥ 90
//!   days, inside the call, then the F6l retire when ticked) or `requested`
//!   (< 90 days with `confirm_early`).
//! - [`decide_early`]: approve (the purge runs inside the call, then the
//!   retire when the request ticked it) or deny an `early_purge` request.
//! - [`extend_view`]: the `purge` block of each quarantine row in
//!   `isolation.employee_list` `restores.quarantine[]`.
use super::*;
use crate::isolation_restore::Close;
use crate::isolation_restore_rpc::RpcError;
use crate::isolation_retire::{purge_hook, ProposeEnv, ProposeOutcome, RetireRoot};

impl From<PurgeError> for RpcError {
    fn from(error: PurgeError) -> Self {
        match error { PurgeError::Restore(refusal) => Self::Restore(refusal), other => Self::Purge(other) }
    }
}

/// Exact lowercase hex of `len` characters.
fn hex_id(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn employee_id(value: &str) -> Result<EmployeeId, RpcError> {
    if !hex_id(value, 32) { return Err(RpcError::Invalid); }
    let bytes = hex::decode(value).map_err(|_| RpcError::Invalid)?;
    Ok(EmployeeId(<[u8; 16]>::try_from(bytes.as_slice()).map_err(|_| RpcError::Invalid)?))
}

/// §3.8 / §3.12: stage the retire right after the purge committed when the
/// dialog's 同時釋放主機資源 box was ticked (`actor` = the Admin of this call,
/// the proposer). A refusal never undoes the purge; it is reported and the
/// Admin may retry with `isolation.employee_retire_propose`.
fn retire_after(home: &Path, actor: &str, now: i64, purged: &Purged, root: &dyn RetireRoot) -> Value {
    if !purged.retire { return Value::Null; }
    let env = ProposeEnv { actor, root, notifier: &wiring::OutboxNotifier, now };
    match purge_hook(home, purged.employee, true, &env) {
        Ok(None) => Value::Null,
        Ok(Some(ProposeOutcome::Staged { proposal, early })) => json!({"state": "staged",
            "proposal_id": hex::encode(proposal.proposal_id.0), "revision": proposal.revision, "early": early, "approval_required": true}),
        Ok(Some(ProposeOutcome::RootAbsent)) => json!({"state": "root_absent"}),
        Ok(Some(ProposeOutcome::AlreadyRetired)) => json!({"state": "retired"}),
        Err(error) => json!({"state": "refused", "code": error.code(), "message": error.message()}),
    }
}

fn purged_value(purged: &Purged, retire: Value) -> Value {
    json!({"state": "purged", "employee_id": hex::encode(purged.employee.0), "incarnation": purged.incarnation,
        "label": purged.label, "early": purged.request_id.is_some(), "request_id": purged.request_id,
        "approver_user_id": purged.approver, "self_approved": purged.self_approved, "purged_at": purged.purged_at,
        "removed": {"entries": purged.removed.entries, "file_bytes": purged.removed.file_bytes},
        "row_stores": purged.row_stores, "label_stores": purged.label_stores, "retire": retire})
}

/// `isolation.quarantine_purge` (Admin + fresh step-up, checked by the caller;
/// `env.actor` is that Admin).
pub(crate) fn quarantine_purge_rpc(home: &Path, env: &RestoreEnv<'_>, employee: &str, incarnation: u32, options: PurgeOptions,
    retire_root: &dyn RetireRoot) -> Result<Value, RpcError> {
    let employee = employee_id(employee)?;
    match purge(home, env, employee, incarnation, options)? {
        PurgeOutcome::Purged(purged) => {
            let retire = retire_after(home, env.actor, env.now, &purged, retire_root);
            Ok(purged_value(&purged, retire))
        }
        PurgeOutcome::Requested { request, preview, notified } => Ok(json!({"state": "requested",
            "request_id": request.request_id, "kind": request.kind.as_str(), "label": request.label,
            "employee_id": hex::encode(request.employee.0), "incarnation": request.incarnation, "expires_at": request.expires_at,
            "approval_required": true, "retire": options.retire, "lost": preview, "notified": notified})),
    }
}

/// `isolation.employee_restore_decide` for an `early_purge` request; `None`
/// for any other request (the restore decide handles it, including the
/// malformed-id refusal).
pub(crate) fn decide_early(home: &Path, env: &RestoreEnv<'_>, rule: &dyn PurgeRule, approver: &str, id: &str, approve: bool,
    retire_root: &dyn RetireRoot) -> Option<Result<Value, RpcError>> {
    if !hex_id(id, 32) { return None; }
    let request = match open(home).and_then(|db| Ok(restore::read_request(&db, id)?)) {
        Ok(Some(request)) if request.kind == RequestKind::EarlyPurge => request,
        Ok(_) => return None,
        Err(error) => return Some(Err(error.into())),
    };
    Some(decide_request(home, env, rule, approver, &request, approve, retire_root))
}

fn decide_request(home: &Path, env: &RestoreEnv<'_>, rule: &dyn PurgeRule, approver: &str, request: &RestoreRequest, approve: bool,
    retire_root: &dyn RetireRoot) -> Result<Value, RpcError> {
    let id = request.request_id.as_str();
    if !approve {
        restore::close_request(home, id, approver, Close::Denied, env.now)?;
        if let Ok(q) = read_row(home, request.employee, request.incarnation) {
            notify(home, &q, "isolation.quarantine_purge_denied", json!({"request_id": id, "approver_user_id": approver}), env.now);
        }
        return Ok(json!({"request_id": id, "kind": "early_purge", "state": "denied"}));
    }
    let purged = approve_early(home, env, rule, id, approver)?;
    if purged.self_approved == Some(true) {
        // S16: a sole-Admin self-approval reaches every log-eligible Admin and the owner.
        if let Ok(q) = read_row(home, request.employee, request.incarnation) {
            notify(home, &q, "isolation.quarantine_purge_self_approved", json!({"request_id": id, "approver_user_id": approver}), env.now);
        }
    }
    let retire = retire_after(home, approver, env.now, &purged, retire_root);
    Ok(json!({"request_id": id, "kind": "early_purge", "state": "done", "label": purged.label,
        "self_approved": purged.self_approved, "purged": purged_value(&purged, Value::Null), "retire": retire}))
}

/// The `purge` block of one quarantine row in the Admin view: whether a
/// purge may start from its state, until when it is early (< 90 days), and
/// what the row records (`early_purge` request id, `purged_at`).
pub(crate) fn extend_view(db: &Connection, q: &Quarantined, mut value: Value, now: i64) -> Result<Value, Refusal> {
    let (early_purge, purged_at): (Option<String>, Option<i64>) = db.query_row(
        "SELECT early_purge,purged_at FROM employee_quarantine WHERE employee_hex=?1 AND incarnation=?2",
        params![hex::encode(q.employee.0), q.incarnation], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|_| Refusal::Unavailable)?;
    let after = q.quarantined_at.saturating_add(PURGE_AFTER_SECS);
    value["purge"] = json!({"purgeable": PURGEABLE.contains(&q.seal.as_str()), "purge_after": after, "early": now < after,
        "early_purge": early_purge, "purged_at": purged_at});
    Ok(value)
}
