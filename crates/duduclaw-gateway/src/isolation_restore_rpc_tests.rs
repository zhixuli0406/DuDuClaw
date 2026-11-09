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

// ── F6fa2: an Admin files for an owner without a second factor ─────────

fn file_for_owner(f: &Fixture, fx: &super::restore::Fakes) -> Result<serde_json::Value, RpcError> {
    rpc::request_restore_for_owner(f.home(), &fx.env(), &f.bob, &employee(), 0, true)
}

fn restores_filed(home: &Path) -> i64 {
    Connection::open(home.join(APPLY_DB)).unwrap().query_row("SELECT count(*) FROM employee_restores", [], |r| r.get(0)).unwrap()
}

/// `(recipients, capped)` of every queued notice of `event`.
fn notices_of(home: &Path, event: &str) -> Vec<(Vec<String>, bool)> {
    let db = Connection::open(home.join(APPLY_DB)).unwrap();
    let mut statement = db.prepare("SELECT recipients,capped FROM employee_restore_notices WHERE event=?1 ORDER BY seq").unwrap();
    let rows = statement.query_map([event], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))).unwrap()
        .map(|row| row.unwrap()).map(|(to, capped)| (serde_json::from_str(&to).unwrap(), capped)).collect();
    rows
}

/// Decision 2: alice had no second factor at the tombstone, so an Admin (bob,
/// fresh step-up) files her restore. The row records `filed_by_admin` +
/// `owner_unverified`; the requester is the Admin, the owner the receipt's.
/// Every owner-filed check still applies: an Admin password reset, an
/// ineligible owner and the 90-day window refuse.
#[test]
fn f6fa2_admin_files_for_owner_without_second_factor() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let baseline = duduclaw_auth::mfa::owner_factor::FactorBaseline::from_snapshot(&super::restore::row(home, 0).principal_snapshot)
        .expect("baseline recorded");
    assert_eq!(baseline.strongest, None, "alice had no second factor at the tombstone");
    // A stale step-up, the owner herself and malformed ids file nothing.
    assert_eq!(rpc::request_restore_for_owner(home, &fx.env(), &f.bob, &employee(), 0, false).err(),
        Some(RpcError::Restore(Refusal::Approval("STEP_UP_REQUIRED"))));
    let own = rpc::request_restore_for_owner(home, &fx.env(), &f.alice, &employee(), 0, true).unwrap_err();
    assert_eq!((own, own.code()), (RpcError::Restore(Refusal::FilerIsOwner), "FILER_IS_OWNER"));
    for id in malformed_ids() {
        assert_eq!(rpc::request_restore_for_owner(home, &fx.env(), &f.bob, &id, 0, true).err(), Some(RpcError::Invalid), "{id}");
    }
    assert_eq!(rpc::request_restore_for_owner(home, &fx.env(), &f.bob, &employee(), 1, true).err(),
        Some(RpcError::Restore(Refusal::NotFound)), "no row at that incarnation");
    assert_eq!(restores_filed(home), 0);
    let filed = file_for_owner(&f, &fx).unwrap();
    assert_eq!((filed["state"].as_str(), filed["kind"].as_str(), filed["label"].as_str(), filed["owner_user_id"].as_str()),
        (Some("requested"), Some("personal_restore"), Some(LABEL), Some(f.alice.as_str())));
    assert_eq!((filed["filed_by_admin"].as_str(), filed["owner_unverified"].as_bool(), filed["second_admin_required"].as_bool()),
        (Some(f.bob.as_str()), Some(true), Some(true)));
    let request = stored(home, filed["request_id"].as_str().unwrap());
    assert_eq!((request.requester.as_str(), request.owner.as_deref(), request.filed_by_admin.as_deref()),
        (f.bob.as_str(), Some(f.alice.as_str()), Some(f.bob.as_str())));
    let flag: i64 = Connection::open(home.join(APPLY_DB)).unwrap().query_row(
        "SELECT owner_unverified FROM employee_restores WHERE request_id=?1", [&request.request_id], |r| r.get(0)).unwrap();
    assert_eq!(flag, 1);
    assert!(audit_log(home).contains("isolation.employee_restore_requested_for_owner"));
    // One open request per row, as for the owner's own.
    assert_eq!(file_for_owner(&f, &fx).err(), Some(RpcError::Restore(Refusal::RestoreInProgress)));

    // Credential change by someone else (an Admin password reset) still refuses.
    let g = fixture();
    let (_, root) = tombstoned(&g);
    g.users.update_user(&g.alice, None, None, Some("admin-set-password")).unwrap();
    assert_eq!(file_for_owner(&g, &fakes(&root)).err(), Some(RpcError::Restore(Refusal::CredentialChanged)));
    // So does an owner who cannot use the account now.
    let g = fixture();
    let (_, root) = tombstoned(&g);
    sql(g.home(), USERS_DB, "UPDATE users SET must_change_password=1 WHERE id=?1", &[&g.alice]);
    assert_eq!(file_for_owner(&g, &fakes(&root)).err(), Some(RpcError::Restore(Refusal::OwnerIneligible)));
    // And the 90-day window.
    let g = fixture();
    let (_, root) = tombstoned(&g);
    let mut late = fakes(&root);
    late.now = super::restore::row(g.home(), 0).quarantined_at + crate::isolation_restore::RESTORE_WINDOW_SECS + 1;
    assert_eq!(file_for_owner(&g, &late).err(), Some(RpcError::Restore(Refusal::Expired)));
}

/// Decision 2 is never a fallback: an owner who had **any** second factor at
/// the tombstone (TOTP, passkey or email factor) is refused
/// `OWNER_HAD_SECOND_FACTOR` (still true after that factor is removed), and
/// keeps only the owner path with that factor. A snapshot without the factor
/// baseline cannot prove "no factor": `SNAPSHOT_INCOMPLETE`.
#[test]
fn f6fa2_admin_cannot_file_when_owner_had_second_factor() {
    for (factor, enrol, remove) in [
        ("totp", "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'TOTP-ENC',0)",
            "DELETE FROM mfa_credentials WHERE user_id=?1"),
        ("passkey", "INSERT INTO mfa_passkeys(id,user_id,name,credential,created_at) VALUES('alice-key',?1,'k','{}',1)",
            "DELETE FROM mfa_passkeys WHERE user_id=?1"),
        ("email", "INSERT INTO mfa_email_credentials(user_id,email,verified_at) VALUES(?1,'alice@example.test',1)",
            "DELETE FROM mfa_email_credentials WHERE user_id=?1"),
    ] {
        let f = fixture();
        let home = f.home();
        sql(home, USERS_DB, enrol, &[&f.alice]);
        let (_, root) = tombstoned(&f);
        let fx = fakes(&root);
        let refused = file_for_owner(&f, &fx).unwrap_err();
        assert_eq!((refused, refused.code()), (RpcError::Restore(Refusal::OwnerHadSecondFactor), "OWNER_HAD_SECOND_FACTOR"), "{factor}");
        assert!(!refused.message().is_empty());
        // Removing the factor afterwards changes nothing: the tombstone's baseline decides.
        sql(home, USERS_DB, remove, &[&f.alice]);
        assert_eq!(file_for_owner(&f, &fx).err(), Some(RpcError::Restore(Refusal::OwnerHadSecondFactor)), "{factor}");
        assert_eq!(restores_filed(home), 0, "{factor}");
        assert_eq!(rpc::admin_view(home, now()).unwrap()["quarantine"][0]["admin_may_file_for_owner"], false, "{factor}");
    }
    // The owner path is unchanged for such an owner.
    let f = fixture();
    sql(f.home(), USERS_DB, "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'TOTP-ENC',0)", &[&f.alice]);
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    assert!(file(&f, &fx).is_ok(), "the owner files with the tombstone's factor");
    // A legacy snapshot (no factor baseline) cannot show "no second factor".
    let g = fixture();
    let (_, root) = tombstoned(&g);
    let mut snapshot = super::restore::row(g.home(), 0).principal_snapshot;
    snapshot.as_object_mut().unwrap().remove("owner_factors");
    sql(g.home(), APPLY_DB, "UPDATE employee_quarantine SET principal_snapshot=?1", &[&snapshot.to_string()]);
    assert_eq!(file_for_owner(&g, &fakes(&root)).err(), Some(RpcError::Restore(Refusal::SnapshotIncomplete)));
}

/// Decision 2: a *different* qualifying Admin approves an Admin-filed
/// request. The filer and the owner are refused `SECOND_ADMIN_REQUIRED`
/// (the request stays open); a third Admin approves, never `self_approved`.
/// With no other qualifying Admin the filing itself is refused
/// `NO_SECOND_ADMIN` — the sole-Admin self-approval path is not available.
#[test]
fn f6fa2_admin_filed_request_needs_different_admin() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let filed = file_for_owner(&f, &fx).unwrap();
    let id = filed["request_id"].as_str().unwrap().to_owned();
    let notices = Vec::<String>::new;
    for party in [f.bob.as_str(), f.alice.as_str()] {
        let refused = rpc::decide(home, &fx.env(), party, &id, true, &notices).unwrap_err();
        assert_eq!((refused, refused.code()), (RpcError::Restore(Refusal::Approval("SECOND_ADMIN_REQUIRED")), "SECOND_ADMIN_REQUIRED"));
        assert_eq!(refused.message(), "需由另一位管理員核准");
        assert_eq!(stored(home, &id).state, "requested");
    }
    let done = rpc::decide(home, &fx.env(), "carol-admin", &id, true, &notices).unwrap();
    assert_eq!((done["state"].as_str(), done["self_approved"].as_bool()), (Some("done"), Some(false)));
    assert_eq!(stored(home, &id).approver.as_deref(), Some("carol-admin"));
    assert_eq!(notices_of(home, "isolation.employee_restore_self_approved"), vec![], "never a self-approval");
    // Sole qualifying Admin: nobody else could approve, so nothing is filed.
    let g = fixture();
    let (_, root) = tombstoned(&g);
    let sole = super::restore::Fakes { rule: Box::new(super::restore::SoleAdmin), ..fakes(&root) };
    let refused = file_for_owner(&g, &sole).unwrap_err();
    assert_eq!((refused, refused.code()), (RpcError::Restore(Refusal::Approval("NO_SECOND_ADMIN")), "NO_SECOND_ADMIN"));
    assert!(refused.message().contains("另一位管理員"), "{}", refused.message());
    assert_eq!(restores_filed(g.home()), 0);
    // An owner-filed request in the same home still has the sole-Admin path.
    let owner_filed = file(&g, &sole).unwrap();
    let done = rpc::decide(g.home(), &sole.env(), &g.alice, owner_filed["request_id"].as_str().unwrap(), true, &notices).unwrap();
    assert_eq!(done["self_approved"], true);
}

/// Decision 2: the owner's tombstone-time notice address is told at filing
/// and at approval, uncapped (never dropped by the L1 cap) — even after the
/// owner moved to another address (an owner-stamped change Q3 allows).
#[test]
fn f6fa2_admin_filed_request_notifies_tombstone_address() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let address = super::restore::row(home, 0).principal_snapshot["notice_address"].as_str().unwrap().to_owned();
    assert_eq!(address, "alice@example.test");
    // The owner changes her own address afterwards (owner-authenticated).
    let users = Connection::open(home.join(USERS_DB)).unwrap();
    let since = duduclaw_auth::credential_changes::high_water(&users).unwrap();
    users.execute("UPDATE users SET email='alice-new@example.test' WHERE id=?1", [&f.alice]).unwrap();
    duduclaw_auth::credential_changes::stamp_owner(&users, since, &f.alice).unwrap();
    // The L1 cap is spent for alice today: the filing notice is not capped.
    for _ in 0..MAX_NOTICES_PER_DAY {
        queue_notice(home, &NoticeScope::Owner(f.alice.clone()), "x", &[], &json!({"label": LABEL}), true, now()).unwrap();
    }
    let filed = file_for_owner(&f, &fx).unwrap();
    assert_eq!(filed["notified"], true);
    let at_filing = notices_of(home, "isolation.employee_restore_requested_for_owner");
    assert_eq!(at_filing.len(), 1);
    assert!(at_filing[0].0.contains(&address), "{at_filing:?}");
    assert!(!at_filing[0].0.contains(&"alice-new@example.test".to_owned()), "the tombstone-time address, not the live one");
    assert!(!at_filing[0].1, "uncapped");
    let notices = Vec::<String>::new;
    rpc::decide(home, &fx.env(), "carol-admin", filed["request_id"].as_str().unwrap(), true, &notices).unwrap();
    let at_approval = notices_of(home, "isolation.employee_restored");
    assert_eq!(at_approval.len(), 1);
    assert!(at_approval[0].0.contains(&address) && !at_approval[0].1, "{at_approval:?}");
    let details: String = Connection::open(home.join(APPLY_DB)).unwrap().query_row(
        "SELECT details FROM employee_restore_notices WHERE event='isolation.employee_restored'", [], |r| r.get(0)).unwrap();
    let details: serde_json::Value = serde_json::from_str(&details).unwrap();
    assert_eq!((details["owner_unverified"].as_bool(), details["filed_by_admin"].as_str()), (Some(true), Some(f.bob.as_str())));
}

/// Decision 2: the flag is in every view the UI reads — the owner's
/// `me.isolation.list_restorable` and the Admin `restores` view (quarantine
/// row `admin_may_file_for_owner`, open request and request list
/// `filed_by_admin` / `owner_unverified`), so the UI can mark
/// 主人未用兩步驟驗證確認. An owner-filed request reads `null` / `false`.
#[test]
fn f6fa2_admin_filed_flag_in_views() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let view = rpc::admin_view(home, now()).unwrap();
    assert_eq!(view["quarantine"][0]["admin_may_file_for_owner"], true);
    // Owner-filed first: not flagged.
    let own = file(&f, &fx).unwrap();
    let view = rpc::admin_view(home, now()).unwrap();
    assert_eq!((&view["requests"][0]["filed_by_admin"], &view["requests"][0]["owner_unverified"]), (&json!(null), &json!(false)));
    assert_eq!(view["quarantine"][0]["open_request"]["owner_unverified"], false);
    rpc::admin_cancel(home, &f.bob, own["request_id"].as_str().unwrap(), now()).unwrap();
    // Admin-filed: flagged everywhere.
    let filed = file_for_owner(&f, &fx).unwrap();
    let id = filed["request_id"].as_str().unwrap();
    let view = rpc::admin_view(home, now()).unwrap();
    let listed = view["requests"].as_array().unwrap().iter().find(|r| r["request_id"] == id).unwrap().clone();
    assert_eq!((listed["filed_by_admin"].as_str(), listed["owner_unverified"].as_bool(), listed["owner_user_id"].as_str(),
        listed["requester_user_id"].as_str()), (Some(f.bob.as_str()), Some(true), Some(f.alice.as_str()), Some(f.bob.as_str())));
    let open = &view["quarantine"][0]["open_request"];
    assert_eq!((open["request_id"].as_str(), open["filed_by_admin"].as_str(), open["owner_unverified"].as_bool()),
        (Some(id), Some(f.bob.as_str()), Some(true)));
    let mine = rpc::list_restorable(home, &f.alice, now()).unwrap();
    let open = &mine["restorable"][0]["open_request"];
    assert_eq!((open["filed_by_admin"].as_str(), open["owner_unverified"].as_bool(), open["kind"].as_str()),
        (Some(f.bob.as_str()), Some(true), Some("personal_restore")));
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
