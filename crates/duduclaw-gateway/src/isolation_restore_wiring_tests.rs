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
}

#[test]
fn retire_rule_fails_closed() {
    use crate::isolation_retire::{ApprovalRefusal, ApprovalRule as _};
    let (dir, _db) = users();
    let rule = RetireLogRule { now: unix_now };
    assert_eq!(rule.decide(dir.path(), "a", "b", None), Err(ApprovalRefusal::LogUnavailable));
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
