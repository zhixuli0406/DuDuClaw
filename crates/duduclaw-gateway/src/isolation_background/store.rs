//! Gateway-private receipt storage. Records contain no bearer credential.
use std::path::Path;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use duduclaw_isolation::capability::EmployeeBinding;
use super::{RecordAuthority, VerificationError, APPROVAL_KIND, unavailable};

const MAX_BYTES: usize = 64 * 1024;
const MAX_RECORDS: i64 = 10_000;

pub(super) fn material_digest(material: &Value) -> Result<String, VerificationError> {
    // Explicit recursive key ordering also works with serde preserve_order.
    fn ordered(value: &Value) -> Value {
        match value {
            Value::Object(fields) => {
                let sorted: std::collections::BTreeMap<_, _> = fields.iter().collect();
                Value::Object(sorted.into_iter().map(|(key, value)| (key.clone(), ordered(value))).collect())
            }
            Value::Array(values) => Value::Array(values.iter().map(ordered).collect()),
            other => other.clone(),
        }
    }
    let bytes = serde_json::to_vec(&ordered(material)).map_err(|_| VerificationError::Invalid)?;
    if bytes.len() > MAX_BYTES || !material.is_object() { return Err(VerificationError::Invalid); }
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn validate(value: &RecordAuthority) -> Result<(), VerificationError> {
    if !value.home.is_absolute() || !duduclaw_core::is_valid_agent_id(&value.agent)
        || !matches!(value.kind.as_str(), "cron" | "reminder" | "task" | "task_spec" | "heartbeat" | "delegation")
        || value.id.is_empty() || value.id.len() > 256 || value.owner.len() > 256
        || super::user_id(&value.owner).is_err() || value.material.len() != 64
        || !value.material.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(VerificationError::Invalid);
    }
    let mut depth = 0;
    let mut parent = value.parent.as_deref();
    while let Some(value) = parent {
        depth += 1;
        if depth >= duduclaw_core::MAX_DELEGATION_DEPTH { return Err(VerificationError::Invalid); }
        parent = value.parent.as_deref();
    }
    if value.kind == "delegation" && (value.caller.is_none() || value.parent_run.is_none()
        || value.parent_agent.is_none() || value.parent_policy.is_none()) {
        return Err(VerificationError::Invalid);
    }
    if value.detached_bus && value.kind != "delegation" { return Err(VerificationError::Invalid); }
    // v1.81 binding/allocation. Both absent: a receipt stored before v1.81,
    // readable only so a new request can replace it (recheck refuses it).
    // Only one present, or a Personal binding, is never a valid receipt.
    match &value.binding {
        None if value.allocation.is_none() => {}
        Some(binding @ EmployeeBinding::Shared(_)) if value.allocation.is_some() =>
            duduclaw_migration::allocation::validate_principal_binding(binding).map_err(|_| VerificationError::Invalid)?,
        _ => return Err(VerificationError::Invalid),
    }
    Ok(())
}

/// A receipt that may be written or filed for approval: structurally valid
/// and carrying the v1.81 binding and allocation.
pub(super) fn validate_current(value: &RecordAuthority) -> Result<(), VerificationError> {
    validate(value)?;
    if value.is_legacy() { return Err(VerificationError::Invalid); }
    Ok(())
}

const STORE: &str = "isolation_background.db";
const APPROVALS: &str = "approvals.db";

/// SR2 finding 7: the receipt store and approvals.db are opened only through
/// the private-SQLite openers (no-follow walk, owner/mode/link/inode checks
/// on the database and its sidecars, `SQLITE_OPEN_NOFOLLOW`). `None` when the
/// database does not exist; any other refusal is `Unavailable`.
fn open_read_only(home: &Path, name: &str) -> Result<Option<Connection>, VerificationError> {
    match std::fs::symlink_metadata(home.join(name)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unavailable()),
        Ok(_) => {}
    }
    let conn = crate::fs_safe::sqlite::open_existing(home, name, crate::fs_safe::sqlite::Access::ReadOnly)
        .map_err(|_| unavailable())?;
    conn.busy_timeout(std::time::Duration::from_millis(500)).map_err(|_| unavailable())?;
    Ok(Some(conn))
}

/// Writable receipt store, created private (0600) when absent.
fn open_writable(home: &Path) -> Result<Connection, VerificationError> {
    #[cfg(unix)]
    {
        let conn = crate::fs_safe::sqlite::open(home, STORE).map_err(|_| unavailable())?;
        conn.busy_timeout(std::time::Duration::from_millis(500)).map_err(|_| unavailable())?;
        Ok(conn)
    }
    #[cfg(not(unix))]
    { let _ = home; Err(unavailable()) }
}

pub(super) fn load(home: &Path, kind: &str, id: &str) -> Result<Option<RecordAuthority>, VerificationError> {
    let Some(conn) = open_read_only(home, STORE)? else { return Ok(None) };
    let text: Option<String> = conn.query_row(
        "SELECT receipt FROM authorities WHERE kind=?1 AND record_id=?2 AND length(receipt)<=65536",
        params![kind, id], |row| row.get(0)).optional().map_err(|_| unavailable())?;
    text.map(|text| {
        let value: RecordAuthority = serde_json::from_str(&text).map_err(|_| unavailable())?;
        validate(&value)?;
        if value.home != home || value.kind != kind || value.id != id { return Err(VerificationError::BindingMismatch); }
        Ok(value)
    }).transpose()
}

pub(super) fn save(value: &RecordAuthority) -> Result<(), VerificationError> {
    save_with(value, None)
}

/// SR2 finding 2: store a new delegation receipt and consume the owner's
/// single-use approval `approval` in the same transaction. An approval that
/// was already consumed, or a delegation id that already has a receipt, is
/// `Revoked` and nothing is written.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn save_consuming(value: &RecordAuthority, approval: &str) -> Result<(), VerificationError> {
    if value.kind != "delegation" || approval.is_empty() || approval.len() > 128 {
        return Err(VerificationError::Invalid);
    }
    save_with(value, Some(approval))
}

fn save_with(value: &RecordAuthority, consume: Option<&str>) -> Result<(), VerificationError> {
    // Never persist the pre-v1.81 form; it can only be replaced.
    validate_current(value)?;
    let text = serde_json::to_string(value).map_err(|_| unavailable())?;
    if text.len() > MAX_BYTES { return Err(VerificationError::Invalid); }
    let mut conn = open_writable(&value.home)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS authorities(kind TEXT NOT NULL, record_id TEXT NOT NULL,
          receipt TEXT NOT NULL CHECK(length(receipt)<=65536), PRIMARY KEY(kind,record_id));
        CREATE TABLE IF NOT EXISTS personal_delegation_consumed(approval_id TEXT PRIMARY KEY,
          record_id TEXT NOT NULL, consumed_at INTEGER NOT NULL);")
        .map_err(|_| unavailable())?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| unavailable())?;
    if let Some(approval) = consume {
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM personal_delegation_consumed WHERE approval_id=?1)
                OR EXISTS(SELECT 1 FROM authorities WHERE kind=?2 AND record_id=?3)",
            params![approval, value.kind, value.id], |row| row.get(0)).map_err(|_| unavailable())?;
        if used { return Err(VerificationError::Revoked); }
        tx.execute("INSERT INTO personal_delegation_consumed(approval_id,record_id,consumed_at) VALUES(?1,?2,?3)",
            params![approval, value.id, i64::try_from(super::now()?).map_err(|_| VerificationError::InvalidTime)?])
            .map_err(|_| unavailable())?;
    }
    // An expired authority cannot authorize a live run or a descendant. Remove
    // it before counting so cancelled enqueue attempts cannot exhaust the store
    // permanently. The bounded capacity still applies to unexpired receipts.
    tx.execute("DELETE FROM authorities WHERE json_extract(receipt,'$.expires_at') <= ?1",
        [super::now()?]).map_err(|_| unavailable())?;
    let count: i64 = tx.query_row("SELECT count(*) FROM authorities", [], |row| row.get(0)).map_err(|_| unavailable())?;
    let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM authorities WHERE kind=?1 AND record_id=?2)",
        params![value.kind, value.id], |row| row.get(0)).map_err(|_| unavailable())?;
    if count >= MAX_RECORDS && !exists { return Err(unavailable()); }
    tx.execute("INSERT INTO authorities(kind,record_id,receipt) VALUES(?1,?2,?3)
        ON CONFLICT(kind,record_id) DO UPDATE SET receipt=excluded.receipt",
        params![value.kind, value.id, text]).map_err(|_| unavailable())?;
    tx.commit().map_err(|_| unavailable())
}

pub(super) fn check_approval(value: &RecordAuthority, approval: &str) -> Result<(), VerificationError> {
    let conn = open_read_only(&value.home, APPROVALS)?.ok_or_else(unavailable)?;
    let (kind, agent, status, payload, decider, invalidated): (String, String, String, String, Option<String>, Option<String>) =
        conn.query_row("SELECT action_kind,agent_id,status,payload,decided_by,invalidated_reason
            FROM approvals WHERE id=?1 AND length(payload)<=131072
            AND julianday(decided_at) >= julianday(created_at)
            AND julianday(decided_at) <= julianday('now')
            AND julianday(decided_at) < julianday(created_at) + ttl_seconds / 86400.0", [approval],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
            .map_err(|_| VerificationError::Revoked)?;
    let dashboard = decider.as_deref().and_then(|v| v.strip_prefix("dashboard:")).is_some_and(|v| !v.is_empty());
    if kind != APPROVAL_KIND || agent != value.agent || status != "approved" || !dashboard || invalidated.is_some() {
        return Err(VerificationError::Revoked);
    }
    let payload: Value = serde_json::from_str(&payload).map_err(|_| unavailable())?;
    let mut expected = value.clone(); expected.approval = None;
    if payload.get("authority") != Some(&serde_json::to_value(expected).map_err(|_| unavailable())?)
        || payload.get("record").map(material_digest).transpose()?.as_deref() != Some(&value.material) {
        return Err(VerificationError::BindingMismatch);
    }
    Ok(())
}

pub(super) fn approval_pending(value: &RecordAuthority) -> Result<bool, VerificationError> {
    let Some(approval) = &value.approval else { return Ok(false); };
    let conn = open_read_only(&value.home, APPROVALS)?.ok_or_else(unavailable)?;
    conn.query_row("SELECT EXISTS(SELECT 1 FROM approvals WHERE id=?1 AND action_kind=?2
        AND status='pending' AND invalidated_reason IS NULL
        AND julianday('now') < julianday(created_at) + ttl_seconds / 86400.0)",
        params![approval, APPROVAL_KIND], |row| row.get(0)).map_err(|_| unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_material_digest_is_order_independent_bounded_and_content_bound() {
        let a: Value = serde_json::from_str(r#"{"b":{"z":1,"a":2},"a":[1,2]}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":[1,2],"b":{"a":2,"z":1}}"#).unwrap();
        assert_eq!(material_digest(&a).unwrap(), material_digest(&b).unwrap());
        assert_ne!(material_digest(&a).unwrap(), material_digest(&serde_json::json!({"a":[2,1]})).unwrap());
        assert!(material_digest(&serde_json::json!({"data":"x".repeat(MAX_BYTES)})).is_err());
        assert!(material_digest(&Value::Null).is_err());
    }
    #[test]
    fn background_missing_receipt_does_not_create_store() {
        let home = crate::fs_safe::private_test_home().unwrap();
        assert!(load(home.path(), "cron", "missing").unwrap().is_none());
        assert!(!home.path().join("isolation_background.db").exists());
    }

    #[test]
    fn background_receipt_roundtrip_replacement_and_expired_reclamation() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let mut first = super::super::tests::receipt(home.path());
        save(&first).unwrap();
        assert_eq!(load(home.path(), "cron", &first.id).unwrap(), Some(first.clone()));
        let original = first.clone();
        first.epoch += 1;
        save(&first).unwrap();
        assert_ne!(load(home.path(), "cron", &first.id).unwrap(), Some(original));
        first.expires_at = 1;
        save(&first).unwrap();
        let mut second = super::super::tests::receipt(home.path());
        second.id = "next-record".into();
        save(&second).unwrap();
        assert!(load(home.path(), "cron", &first.id).unwrap().is_none());
        assert_eq!(load(home.path(), "cron", &second.id).unwrap(), Some(second));
    }

    #[test]
    fn background_approval_requires_exact_payload_admin_decision_and_decision_window() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let receipt = super::super::tests::receipt(home.path());
        let conn = Connection::open(home.path().join("approvals.db")).unwrap();
        conn.execute_batch("CREATE TABLE approvals(id TEXT,action_kind TEXT,agent_id TEXT,status TEXT,
            payload TEXT,decided_by TEXT,invalidated_reason TEXT,created_at TEXT,decided_at TEXT,ttl_seconds INTEGER);").unwrap();
        let mut pending = receipt.clone(); pending.approval = None;
        let payload = serde_json::json!({"authority":pending,"record":{"task":"original"}}).to_string();
        conn.execute("INSERT INTO approvals VALUES ('approval-id',?1,'alice','approved',?2,'dashboard:admin',NULL,
            datetime('now','-1 hour'),datetime('now','-30 minutes'),86400)", params![APPROVAL_KIND, payload]).unwrap();
        assert!(check_approval(&receipt, "approval-id").is_ok());
        for sql in ["UPDATE approvals SET status='pending'", "UPDATE approvals SET decided_by='channel:admin'",
            "UPDATE approvals SET invalidated_reason='cancelled'", "UPDATE approvals SET agent_id='bob'",
            "UPDATE approvals SET decided_at=datetime('now','+1 minute')",
            "UPDATE approvals SET created_at=datetime('now','-2 days')", "UPDATE approvals SET payload='{}'"] {
            conn.execute_batch(sql).unwrap();
            assert!(check_approval(&receipt, "approval-id").is_err(), "{sql}");
            conn.execute("UPDATE approvals SET status='approved',decided_by='dashboard:admin',
                invalidated_reason=NULL,agent_id='alice',created_at=datetime('now','-1 hour'),
                decided_at=datetime('now','-30 minutes'),payload=?1", [&payload]).unwrap();
        }
        let mut forged = receipt.clone(); forged.epoch += 1;
        assert!(check_approval(&forged, "approval-id").is_err());
        conn.execute_batch("UPDATE approvals SET status='pending',decided_at=NULL,decided_by=NULL").unwrap();
        assert!(approval_pending(&receipt).unwrap());
        conn.execute_batch("UPDATE approvals SET created_at=datetime('now','-2 days')").unwrap();
        assert!(!approval_pending(&receipt).unwrap());
    }
}
