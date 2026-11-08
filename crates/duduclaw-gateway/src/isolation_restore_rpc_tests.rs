//! F6d RPC bodies (`crate::isolation_restore_rpc`) over the F6c fixture: a
//! Personal employee of alice tombstoned and sealed (real stores), fake root,
//! routes, marks and rule (`isolation_restore_tests.rs`). Child of
//! `isolation_lifecycle_tests` (fixture reuse). The handler gates (system,
//! Admin + MFA + step-up, not listed to AI) are in
//! `handlers/isolation_employee_restore_tests.rs`.
use super::restore::{fakes, now, stored, tombstoned, EMP};
use super::*;
use crate::isolation_restore::{Refusal, RootError};
use crate::isolation_restore_rpc::{self as rpc, RpcError};
use crate::isolation_restore_wiring::{queue_notice, NoticeScope, MAX_NOTICES_PER_DAY};
use duduclaw_auth::mfa::{OwnerFactor, OwnerFactorError};
use serde_json::json;
use std::cell::Cell;

fn employee() -> String { hex::encode(EMP.0) }

/// Ids that are not canonical lowercase 32-hex. `EMP`'s hex is all digits,
/// so `employee().to_uppercase()` would be the valid id itself; the
/// uppercase case uses an id with hex letters (its lowercase form is
/// well-formed, so a loose match would accept it).
fn malformed_ids() -> [String; 4] {
    ["AB".repeat(16), employee()[..30].to_owned(), format!("{}g", &employee()[..31]), format!("{}0", employee())]
}

/// The proof purpose of alice's row (incarnation 0), as the server derives it.
fn purpose0() -> String { format!("restore:{}:0", employee()) }

/// Accepts any code, but only for the purpose of alice's row.
fn totp_ok(factor: OwnerFactor, purpose: &str, _: &str) -> Result<OwnerFactor, OwnerFactorError> {
    assert_eq!(purpose, purpose0(), "the proof is bound to the row acted on");
    Ok(factor)
}

fn file(f: &Fixture, fx: &super::restore::Fakes) -> Result<serde_json::Value, RpcError> {
    rpc::request_restore(f.home(), &fx.env(), &f.alice, &employee(), 0, "totp", "123456", &totp_ok)
}

fn audit_log(home: &Path) -> String { std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default() }

#[test]
fn request_only_by_owner() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let asked = Cell::new(0);
    let check = |factor: OwnerFactor, _: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> { asked.set(asked.get() + 1); Ok(factor) };
    // Another account (here an Admin) gets NOT_FOUND before any factor check.
    for caller in [f.bob.as_str(), "", "system", &f.alice.to_uppercase()] {
        assert_eq!(rpc::request_restore(f.home(), &fx.env(), caller, &employee(), 0, "totp", "1", &check).err(),
            Some(RpcError::Restore(Refusal::NotFound)), "{caller}");
    }
    assert_eq!(asked.get(), 0, "no factor oracle for rows the caller does not own");
    // Malformed ids are refused, never matched loosely.
    for id in malformed_ids() {
        assert_eq!(rpc::request_restore(f.home(), &fx.env(), &f.alice, &id, 0, "totp", "1", &check).err(),
            Some(RpcError::Invalid), "{id}");
    }
    assert_eq!(asked.get(), 0, "no factor check for a malformed id");
    let filed = rpc::request_restore(f.home(), &fx.env(), &f.alice, &employee(), 0, "totp", "1", &check).unwrap();
    assert_eq!(asked.get(), 1);
    assert_eq!(stored(f.home(), filed["request_id"].as_str().unwrap()).requester, f.alice);
    // The owner's listing shows it; bob's listing is empty.
    assert_eq!(rpc::list_restorable(f.home(), &f.alice, now()).unwrap()["restorable"][0]["open_request"]["state"], "requested");
    assert_eq!(rpc::list_restorable(f.home(), &f.bob, now()).unwrap()["restorable"], json!([]));
}

/// `me.isolation.restore_challenge`: ownership first (no factor oracle),
/// then the issuer gets the purpose of exactly that row.
#[test]
fn restore_challenge_only_by_owner_and_bound_to_row() {
    let f = fixture();
    let (_, _root) = tombstoned(&f);
    let asked = std::cell::RefCell::new(Vec::new());
    let issue = |purpose: &str| -> Result<&'static str, OwnerFactorError> { asked.borrow_mut().push(purpose.to_owned()); Ok("issued") };
    for caller in [f.bob.as_str(), "", "system"] {
        assert_eq!(rpc::restore_challenge(f.home(), caller, &employee(), 0, &issue).err(), Some(RpcError::Restore(Refusal::NotFound)));
    }
    assert_eq!(rpc::restore_challenge(f.home(), &f.alice, &employee(), 1, &issue).err(), Some(RpcError::Restore(Refusal::NotFound)),
        "no row at that incarnation");
    for id in malformed_ids() {
        assert_eq!(rpc::restore_challenge(f.home(), &f.alice, &id, 0, &issue).err(), Some(RpcError::Invalid), "{id}");
    }
    assert!(asked.borrow().is_empty(), "no challenge for rows the caller does not own");
    assert_eq!(rpc::restore_challenge(f.home(), &f.alice, &employee(), 0, &issue), Ok("issued"));
    assert_eq!(*asked.borrow(), vec![purpose0()]);
    let locked = |_: &str| -> Result<(), OwnerFactorError> { Err(OwnerFactorError::RateLimited) };
    let refused = rpc::restore_challenge(f.home(), &f.alice, &employee(), 0, &locked).unwrap_err();
    assert_eq!(refused.code(), "FACTOR_RATE_LIMITED");
}

#[test]
fn admin_cannot_file_personal_request() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    // bob is an Admin with a valid factor: still not the receipt owner.
    assert_eq!(rpc::request_restore(f.home(), &fx.env(), &f.bob, &employee(), 0, "totp", "1", &totp_ok).err(),
        Some(RpcError::Restore(Refusal::NotFound)));
    // The engine refuses a proof naming anyone but the owner too.
    let proof = crate::isolation_restore::OwnerProof { user_id: &f.bob, factor: "totp" };
    assert_eq!(crate::isolation_restore::file_personal_request(f.home(), &fx.env(), EMP, 0, Some(proof)).err(),
        Some(Refusal::NotFound));
    let db = Connection::open(f.home().join(APPLY_DB)).unwrap();
    let filed: i64 = db.query_row("SELECT count(*) FROM employee_restores", [], |r| r.get(0)).unwrap();
    assert_eq!(filed, 0, "nothing filed");
}

#[test]
fn request_requires_strongest_factor() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let weaker = |_: OwnerFactor, _: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> { Err(OwnerFactorError::NotStrongest(OwnerFactor::Passkey)) };
    let refused = rpc::request_restore(f.home(), &fx.env(), &f.alice, &employee(), 0, "totp", "1", &weaker).unwrap_err();
    assert_eq!((refused.code(), refused), ("FACTOR_NOT_STRONGEST", RpcError::Factor(OwnerFactorError::NotStrongest(OwnerFactor::Passkey))));
    let failed = |_: OwnerFactor, _: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> { Err(OwnerFactorError::Failed) };
    assert_eq!(rpc::request_restore(f.home(), &fx.env(), &f.alice, &employee(), 0, "totp", "000000", &failed).err(),
        Some(RpcError::Factor(OwnerFactorError::Failed)));
    // Unknown factor names never reach the check.
    assert_eq!(rpc::request_restore(f.home(), &fx.env(), &f.alice, &employee(), 0, "sms", "1", &totp_ok).err(),
        Some(RpcError::Factor(OwnerFactorError::Denied)));
    let db = Connection::open(f.home().join(APPLY_DB)).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM employee_restores", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn request_rate_limits() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    for _ in 0..crate::isolation_restore::MAX_REQUESTS_PER_ROW {
        let filed = file(&f, &fx).unwrap();
        // One open request per row (L1).
        assert_eq!(file(&f, &fx).err(), Some(RpcError::Restore(Refusal::RestoreInProgress)));
        rpc::admin_cancel(f.home(), &f.bob, filed["request_id"].as_str().unwrap(), now()).unwrap();
    }
    let limited = file(&f, &fx).unwrap_err();
    assert_eq!((limited, limited.code()), (RpcError::Restore(Refusal::RateLimited), "RATE_LIMITED"));
}

#[test]
fn notice_caps() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let at = now();
    let alice = NoticeScope::Owner(f.alice.clone());
    let to = vec!["admin@example.test".to_owned()];
    for _ in 0..MAX_NOTICES_PER_DAY {
        assert_eq!(queue_notice(home, &alice, "isolation.employee_restore_requested", &to, &json!({"label": LABEL}), true, at), Ok(true));
    }
    assert_eq!(queue_notice(home, &alice, "isolation.employee_restore_requested", &to, &json!({"label": LABEL}), true, at), Ok(false),
        "the 6th capped notice of the day is dropped");
    assert!(audit_log(home).contains("isolation.employee_restore_notice_suppressed"));
    // Security notices are never dropped; another scope has its own budget;
    // the window is a rolling day.
    assert_eq!(queue_notice(home, &alice, "isolation.employee_restored", &to, &json!({}), false, at), Ok(true));
    assert_eq!(queue_notice(home, &NoticeScope::Tenant("t".into()), "x", &to, &json!({}), true, at), Ok(true));
    assert_eq!(queue_notice(home, &alice, "x", &to, &json!({}), true, at + 86_401), Ok(true));
    // Through the RPC: a request beyond the cap still files, unnotified.
    let fx = fakes(&root);
    let filed = file(&f, &fx).unwrap();
    assert_eq!(filed["notified"], false);
}

#[test]
fn request_expires() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    let filed = file(&f, &fx).unwrap();
    let id = filed["request_id"].as_str().unwrap().to_owned();
    let expires = filed["expires_at"].as_i64().unwrap();
    assert!(expires <= now() + crate::isolation_restore::REQUEST_TTL_SECS);
    // Past the 72 h: listings expire it, approval refuses it.
    let later = expires + 1;
    let listed = rpc::list_restorable(f.home(), &f.alice, later).unwrap();
    assert_eq!(listed["restorable"][0]["open_request"], json!(null));
    assert_eq!(stored(f.home(), &id).state, "expired");
    fx.now = later;
    let notices = Vec::<String>::new;
    assert_eq!(rpc::decide(f.home(), &fx.env(), &f.bob, &id, true, &notices).err(), Some(RpcError::Restore(Refusal::Expired)));
}

#[test]
fn cancel_by_requester_or_admin() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let first = file(&f, &fx).unwrap();
    let id = first["request_id"].as_str().unwrap();
    // Another member is not the requester (no oracle); the owner needs a factor.
    assert_eq!(rpc::cancel_own(f.home(), &f.bob, id, "totp", "1", &totp_ok, now()).err(), Some(RpcError::Restore(Refusal::NotFound)));
    let failed = |_: OwnerFactor, _: &str, _: &str| -> Result<OwnerFactor, OwnerFactorError> { Err(OwnerFactorError::Failed) };
    assert_eq!(rpc::cancel_own(f.home(), &f.alice, id, "totp", "0", &failed, now()).err(), Some(RpcError::Factor(OwnerFactorError::Failed)));
    assert_eq!(rpc::cancel_own(f.home(), &f.alice, id, "totp", "1", &totp_ok, now()).unwrap()["state"], "cancelled");
    assert_eq!(stored(f.home(), id).state, "cancelled");
    // An Admin cancels any open request.
    let second = file(&f, &fx).unwrap();
    let id = second["request_id"].as_str().unwrap();
    assert_eq!(rpc::admin_cancel(f.home(), &f.bob, id, now()).unwrap()["state"], "cancelled");
    // A closed request cannot be cancelled again.
    assert_eq!(rpc::admin_cancel(f.home(), &f.bob, id, now()).err(), Some(RpcError::Restore(Refusal::NotFound)));
    assert!(audit_log(f.home()).contains("isolation.employee_restore_cancelled"));
}

/// Admin re-grant after an approved restore: one listed item, exact match,
/// audited; the restore itself re-granted nothing.
#[test]
fn regrant_requires_admin_step_up_and_audits() {
    use duduclaw_core::org_store::{self as org, OrgEntry};
    let f = fixture();
    let home = f.home();
    org::with_org_lock(home, |guard| {
        org::upsert(guard, LABEL, OrgEntry::new("", "ops"))?;
        org::upsert(guard, "carol-desk", OrgEntry::new(LABEL, "ops"))?;
        Ok(())
    }).unwrap();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    assert_eq!(org::load_for_write(home).unwrap().get("carol-desk"), Some(&OrgEntry::new("", "ops")), "re-rooted at tombstone");
    let filed = file(&f, &fx).unwrap();
    // Before the restore the row is not `restored`: nothing to re-grant.
    assert_eq!(rpc::regrant(home, &f.bob, &employee(), 0, "reports_to:carol-desk").err(), Some(RpcError::Restore(Refusal::NotFound)));
    let notices = Vec::<String>::new;
    // The label's own org entry is in the seal digest and unchanged; only
    // carol's edge moved (re-rooted), so the restore goes through.
    let done = rpc::decide(home, &fx.env(), &f.bob, filed["request_id"].as_str().unwrap(), true, &notices).unwrap();
    assert!(done["label_state"]["regrant"].as_array().unwrap().contains(&json!("reports_to:carol-desk")), "{done}");
    assert_eq!(org::load_for_write(home).unwrap().get("carol-desk"), Some(&OrgEntry::new("", "ops")), "never automatic");
    // Exact item only; the incarnation must be the restored row's.
    assert_eq!(rpc::regrant(home, &f.bob, &employee(), 0, "reports_to:carol").err(), Some(RpcError::Restore(Refusal::NotFound)));
    assert_eq!(rpc::regrant(home, &f.bob, &employee(), 1, "reports_to:carol-desk").err(), Some(RpcError::Restore(Refusal::NotFound)));
    assert_eq!(rpc::regrant(home, &f.bob, &employee(), 0, "reports_to:carol-desk").unwrap()["state"], "regranted");
    assert_eq!(org::load_for_write(home).unwrap().get("carol-desk"), Some(&OrgEntry::new(LABEL, "ops")));
    let log = audit_log(home);
    assert!(log.contains("isolation.employee_restore_regranted") && log.contains(&f.bob), "{log}");
}

// ── F6fa ────────────────────────────────────────────────────────────────

/// Review item 1: the tombstone records the owner's factor baseline, and the
/// production factor check reads it from the caller's own row (by the
/// server-built purpose). A row tombstoned before F6fa has none: refused
/// `FACTOR_BASELINE_MISSING`, never the live account's factors.
#[test]
fn f6fa_legacy_snapshot_without_factor_ids_refused() {
    use duduclaw_auth::mfa::owner_factor::FactorBaseline;
    let f = fixture();
    let home = f.home();
    sql(home, USERS_DB, "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'TOTP-ENC',0)", &[&f.alice]);
    let (_, _root) = tombstoned(&f);
    let snapshot = super::restore::row(home, 0).principal_snapshot;
    let recorded = FactorBaseline::from_snapshot(&snapshot).expect("recorded at the tombstone");
    assert_eq!(recorded.strongest, Some(OwnerFactor::Totp));
    assert!(recorded.totp.is_some() && recorded.passkeys.is_empty());
    assert!(!snapshot.to_string().contains("TOTP-ENC"), "a digest, never the stored secret");
    assert_eq!(rpc::factor_baseline(home, &f.alice, &purpose0()), Ok(recorded));
    let adopt = format!("adopt:{}:0", employee());
    assert!(rpc::factor_baseline(home, &f.alice, &adopt).is_ok(), "the adoption purpose names the same row");
    // Only the caller's own row, only a canonical purpose of it.
    assert_eq!(rpc::factor_baseline(home, &f.bob, &purpose0()), Err(OwnerFactorError::Denied));
    for bad in [format!("restore:{}:01", employee()), format!("restore:{}:1", employee()), "restore:x:0".to_owned(),
        format!("login:{}:0", employee())] {
        assert_eq!(rpc::factor_baseline(home, &f.alice, &bad), Err(OwnerFactorError::Denied), "{bad}");
    }
    // The legacy shape (no `owner_factors`).
    let mut legacy = snapshot.clone();
    legacy.as_object_mut().unwrap().remove("owner_factors");
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET principal_snapshot=?1", &[&legacy.to_string()]);
    assert_eq!(rpc::factor_baseline(home, &f.alice, &purpose0()), Err(OwnerFactorError::BaselineMissing));
    let refused = RpcError::Factor(OwnerFactorError::BaselineMissing);
    assert_eq!(refused.code(), "FACTOR_BASELINE_MISSING");
    assert!(!refused.message().is_empty());
}

/// Review item 5: a sole-Admin self-approval notifies the log addresses the
/// rule read **and** (Personal) the tombstone-time notice address.
#[test]
fn f6fa_self_approval_notice_reaches_tombstone_address() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let address = super::restore::row(f.home(), 0).principal_snapshot["notice_address"].as_str().unwrap().to_owned();
    let filed = file(&f, &fx).unwrap();
    let id = filed["request_id"].as_str().unwrap().to_owned();
    let log = || vec!["admin-log@example.test".to_owned()];
    let done = rpc::decide(f.home(), &fx.env(), &f.alice, &id, true, &log).unwrap();
    assert_eq!(done["self_approved"], true);
    let recipients: String = Connection::open(f.home().join(APPLY_DB)).unwrap().query_row(
        "SELECT recipients FROM employee_restore_notices WHERE event='isolation.employee_restore_self_approved'", [], |r| r.get(0)).unwrap();
    let recipients: Vec<String> = serde_json::from_str(&recipients).unwrap();
    assert!(recipients.contains(&address) && recipients.contains(&"admin-log@example.test".to_owned()), "{recipients:?}");
}

fn critical_events(home: &Path, event: &str) -> usize {
    audit_log(home).lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| entry["event_type"] == event && entry["severity"] == "critical").count()
}

/// Review item 5: an unreadable eligibility log (no Admin recipients) and an
/// uncapped security notice that cannot be queued are audited Critical,
/// never swallowed; a capped notice that cannot be queued is not.
#[test]
fn f6fa_notice_failures_audited_critical() {
    use crate::isolation_restore_wiring::{admin_notice_addresses, queue_or_alert};
    // No users.db at all: no addresses, one Critical audit naming the event.
    let empty = crate::fs_safe::private_test_home().unwrap();
    assert!(admin_notice_addresses(empty.path(), LABEL, "isolation.employee_restored", now()).is_empty());
    assert_eq!(critical_events(empty.path(), "isolation.employee_restore_notice_recipients_unavailable"), 1);
    assert!(audit_log(empty.path()).contains("isolation.employee_restored"));
    // The outbox refuses inserts.
    let f = fixture();
    let home = f.home();
    let (_, _root) = tombstoned(&f);
    sql(home, APPLY_DB, "CREATE TABLE IF NOT EXISTS employee_restore_notices (seq INTEGER PRIMARY KEY AUTOINCREMENT,
        scope TEXT NOT NULL, event TEXT NOT NULL, recipients TEXT NOT NULL, details TEXT NOT NULL,
        capped INTEGER NOT NULL CHECK (capped IN (0, 1)), at INTEGER NOT NULL, delivered_at INTEGER)", &[]);
    sql(home, APPLY_DB, "CREATE TRIGGER outbox_broken BEFORE INSERT ON employee_restore_notices
        BEGIN SELECT RAISE(ABORT, 'outbox broken'); END", &[]);
    let scope = NoticeScope::Owner(f.alice.clone());
    let to = vec!["admin@example.test".to_owned()];
    assert!(!queue_or_alert(home, &scope, "isolation.employee_restore_requested", &to, &json!({"label": LABEL}), true, now()));
    assert_eq!(critical_events(home, "isolation.employee_restore_notice_unqueued"), 0, "capped: logged, not Critical");
    assert!(!queue_or_alert(home, &scope, "isolation.employee_restored", &to, &json!({"label": LABEL}), false, now()));
    assert_eq!(critical_events(home, "isolation.employee_restore_notice_unqueued"), 1);
}

/// M6/S17: every path that takes both stores takes apply.db before
/// users.db. While another connection holds apply.db's writer lock, the
/// restore request and approval paths block on apply.db without holding a
/// users.db write lock (users.db stays writable), then complete.
#[test]
fn lock_order_apply_db_before_users_db() {
    let f = fixture();
    let home = f.home().to_path_buf();
    let (_, root) = tombstoned(&f);
    let alice = f.alice.clone();
    let bob = f.bob.clone();
    // The approve step decides the request the request step filed: a second
    // filing for the same row would be refused while that one is open (L1).
    let mut filed: Option<String> = None;
    for step in ["request", "approve"] {
        let pending = if step == "approve" {
            Some(filed.clone().expect("the request step filed the open request"))
        } else { None };
        let mut apply = Connection::open(home.join(APPLY_DB)).unwrap();
        let held = apply.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
        let (home2, root2, alice2, bob2) = (home.clone(), root.clone(), alice.clone(), bob.clone());
        let worker = std::thread::spawn(move || {
            let fx = fakes(&root2);
            match &pending {
                None => rpc::request_restore(&home2, &fx.env(), &alice2, &hex::encode(EMP.0), 0, "totp", "1", &totp_ok)
                    .map(|value| value["request_id"].as_str().map(str::to_owned)),
                Some(id) => { let notices = Vec::<String>::new; rpc::decide(&home2, &fx.env(), &bob2, id, true, &notices).map(|_| None) }
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(400));
        // The worker waits on apply.db; it holds no users.db write lock.
        let users = Connection::open(home.join(USERS_DB)).unwrap();
        users.busy_timeout(std::time::Duration::from_millis(0)).unwrap();
        users.execute_batch("BEGIN IMMEDIATE; COMMIT;").expect("users.db must not be locked while apply.db is waited for");
        held.commit().unwrap();
        drop(apply);
        let result = worker.join().unwrap();
        assert!(!matches!(result, Err(RpcError::Restore(Refusal::Root(_)))), "{step}: {result:?}");
        if step == "request" {
            assert!(result.is_ok(), "{result:?}");
            filed = result.unwrap();
            assert!(filed.is_some(), "the request RPC returns its request_id");
        }
    }
    let _ = RootError::Transport;
}
