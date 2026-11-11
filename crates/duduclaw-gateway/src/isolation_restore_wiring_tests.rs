//! F6d production seams: approval-rule adapters over a real users.db,
//! store marks over the F6h registry, and the fail-closed spawnd calls.
use super::*;
use crate::isolation_restore::ApprovalRule as _;

fn users() -> (tempfile::TempDir, duduclaw_auth::UserDb) {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
    (dir, db)
}

/// The decide RPC passes its verified step-up; every other path passes
/// `false`, so F6c2 refuses before reading the log. Outside a transaction
/// the rule refuses too (the count must be read in the apply tx).
#[test]
fn log_rule_requires_step_up_and_transaction() {
    let (dir, _db) = users();
    let mut conn = Connection::open(dir.path().join("users.db")).unwrap();
    let now = unix_now();
    let tx = conn.transaction().unwrap();
    assert_eq!(LogRule::new(false, now).decide(&tx, "a", "b", RequestKind::SharedRestore), Err("STEP_UP_REQUIRED"));
    // A fresh log (< 7 days) and unknown accounts: refused, never allowed.
    assert!(LogRule::new(true, now).decide(&tx, "a", "b", RequestKind::SharedRestore).is_err());
    // Personal kinds: the owner is the requester (F6c2 precondition holds,
    // so the refusal is the rule's, not INVALID_REQUEST).
    let personal = LogRule::new(true, now).decide(&tx, "a", "a", RequestKind::PersonalRestore);
    assert!(personal.is_err() && personal != Err("INVALID_REQUEST"), "{personal:?}");
    tx.finish().unwrap();
    let autocommit = Connection::open(dir.path().join("users.db")).unwrap();
    assert_eq!(LogRule::new(true, now).decide(&autocommit, "a", "b", RequestKind::SharedRestore), Err("APPROVAL_NOT_IN_TRANSACTION"));
    // The self-approval audit row is only ever written inside the decision's tx.
    assert_eq!(LogRule::new(true, now).record_self_approval(&autocommit, "a", "a", RequestKind::PersonalRestore, "req-1"),
        Err(SELF_APPROVAL_AUDIT_UNAVAILABLE));
}

// The rule opens users.db through `fs_safe::sqlite::open_existing`, which is
// Unix-only (every other platform refuses: always LogUnavailable there).
#[cfg(unix)]
#[test]
fn retire_rule_fails_closed() {
    use crate::isolation_retire::{ApprovalRefusal, ApprovalRule as _};
    let (dir, _db) = users();
    // A fresh step-up of this call (F6fa: passed, never assumed).
    let rule = RetireLogRule { now: unix_now, step_up_verified_at: Some(unix_now() as u64) };
    // A fresh, intact log with no accounts: the non-party approver "b" is not
    // an eligible Admin, so F6c2 refuses `APPROVER_NOT_QUALIFIED` (the log
    // itself is readable; coverage only gates the sole-Admin path).
    assert_eq!(rule.decide(dir.path(), "a", "b", None), Err(ApprovalRefusal::NotQualified));
    assert_eq!(rule.decide(dir.path(), "a", "a", None), Err(ApprovalRefusal::NotQualified), "self, not eligible now");
    // A log missing one of its append-only triggers is unavailable, for every path.
    Connection::open(dir.path().join("users.db")).unwrap().execute_batch("DROP TRIGGER admin_eligibility_log_no_delete").unwrap();
    assert_eq!(rule.decide(dir.path(), "a", "b", None), Err(ApprovalRefusal::LogUnavailable), "log trigger missing");
    assert_eq!(rule.decide(dir.path(), "a", "a", None), Err(ApprovalRefusal::LogUnavailable), "log trigger missing, self");
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(rule.decide(empty.path(), "a", "b", None), Err(ApprovalRefusal::LogUnavailable), "no users.db");
}

/// Marks are the counts `tombstone_effects` records (Gate/Pause stores);
/// a missing store is a positive zero, an unreadable one is `Err`.
#[test]
fn registry_marks_match_tombstone_shape() {
    use crate::isolation_restore::LabelStores as _;
    let dir = crate::fs_safe::private_test_home().unwrap();
    let marks = RegistryMarks.marks(dir.path(), "alice-helper").unwrap();
    assert_eq!(marks, json!({"memory": 0, "sessions": 0, "tasks": 0, "cron": 0}));
    std::fs::write(dir.path().join("memory.db"), b"not a database").unwrap();
    assert_eq!(RegistryMarks.marks(dir.path(), "alice-helper"), Err(()));
    use crate::isolation_retire::LabelStores as _;
    assert_eq!(RegistryCounts.count_under(dir.path(), "alice-helper"), Err(()));
}

/// No spawnd socket in tests: the stop is unconfirmed and idleness is
/// unavailable — never "stopped", never "idle", never "gone".
#[test]
fn spawnd_calls_fail_closed_without_root() {
    let employee = EmployeeId([4; 16]);
    assert_eq!(spawnd_stop_runs(employee), Err(()));
    assert_eq!(spawnd_employee_idle(employee), Err(RootError::IsolationUnavailable));
}
