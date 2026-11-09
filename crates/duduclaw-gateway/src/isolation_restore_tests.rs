//! F6c same-id restore engine (design r5 §3.4–§3.7) against the real stores
//! of the lifecycle fixture: a Personal employee is tombstoned (F6a), then
//! the owner files and an Admin approves. The root probe, routes, store
//! marks, run stopper and approval rule (F6c2) are fakes.
//!
//! Child of `isolation_lifecycle_tests` (fixture reuse); crash recovery and
//! the audit trail are in `isolation_restore_recovery_tests.rs`.
use super::*;
use crate::isolation_quarantine::RunStopper;
use crate::isolation_restore::*;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;

pub(super) const EMP: EmployeeId = EmployeeId([1; 16]);

pub(super) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

pub(super) fn marks() -> Value { json!({"memory": 3, "sessions": 1, "tasks": 0, "cron": 0}) }

#[derive(Clone)]
pub(super) struct Probe {
    pub(super) receipt: Receipt,
    pub(super) phase: RootPhase,
    pub(super) idle: Option<bool>,
    pub(super) idle_answer: Result<bool, RootError>,
    pub(super) error: Option<RootError>,
}

impl Probe {
    pub(super) fn ready(receipt: &Receipt) -> Self {
        Self { receipt: receipt.clone(), phase: RootPhase::Ready, idle: Some(true), idle_answer: Ok(true), error: None }
    }
}

impl RootProbe for Probe {
    fn read_back(&self, item: &ConfirmationItem) -> Result<RootReadBack, RootError> {
        if let Some(error) = self.error { return Err(error); }
        if self.receipt.binding != item.binding { return Err(RootError::Transport); }
        Ok(RootReadBack { receipt: self.receipt.clone(), phase: self.phase, idle: self.idle })
    }
    fn employee_idle(&self, _: EmployeeId) -> Result<bool, RootError> { self.idle_answer }
}

pub(super) struct Routes(pub(super) Rc<RefCell<Vec<(String, u64)>>>);
impl RouteSource for Routes {
    fn routes_for(&self, _: &EmployeeBinding, _: &str) -> Result<Vec<(String, u64)>, ()> { Ok(self.0.borrow().clone()) }
    fn detail_routes_naming(&self, _: EmployeeId) -> Result<Vec<String>, ()> { Ok(Vec::new()) }
}

pub(super) struct Marks(pub(super) Value);
impl LabelStores for Marks {
    fn marks(&self, _: &Path, _: &str) -> Result<Value, ()> { Ok(self.0.clone()) }
}

/// F6c2 stand-in: any approver; self-approval exactly when approver == requester.
/// The self-approval row is F6c2's real `record_self_approval` on the given
/// connection (refused outside a transaction).
pub(super) struct AllowSelf;
impl ApprovalRule for AllowSelf {
    fn decide(&self, _: &Connection, requester: &str, approver: &str, _: RequestKind) -> Result<Approval, &'static str> {
        Ok(Approval { self_approved: requester == approver })
    }
    fn record_self_approval(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind, subject: &str)
        -> Result<(), &'static str> {
        let kind = crate::isolation_restore_wiring::approval_kind(kind);
        let request = duduclaw_auth::admin_approval_rule::ApprovalRequest { kind, requester_user_id: requester,
            owner_user_id: kind.is_personal().then_some(requester), approver_user_id: approver, fresh_step_up: true, now: now() };
        duduclaw_auth::admin_approval_rule::record_self_approval(users, &request, subject).map_err(|_| "SELF_APPROVAL_AUDIT_UNAVAILABLE")
    }
    /// F6fa2 stand-in: any third account approves; a party never does.
    fn decide_admin_filed(&self, _: &Connection, filer: &str, owner: &str, approver: &str, _: RequestKind)
        -> Result<Approval, &'static str> {
        if approver == filer || approver == owner { Err("SECOND_ADMIN_REQUIRED") } else { Ok(Approval { self_approved: false }) }
    }
    fn check_admin_filing(&self, _: &Connection, filer: &str, owner: &str) -> Result<(), &'static str> {
        if filer == owner { Err("INVALID_REQUEST") } else { Ok(()) }
    }
}

/// F6fa2 stand-in for a home whose only qualifying Admin is the filer: the
/// rule's answer for Admin-filed requests (no sole-Admin path); owner-filed
/// requests as [`AllowSelf`].
pub(super) struct SoleAdmin;
impl ApprovalRule for SoleAdmin {
    fn decide(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind) -> Result<Approval, &'static str> {
        AllowSelf.decide(users, requester, approver, kind)
    }
    fn record_self_approval(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind, subject: &str)
        -> Result<(), &'static str> {
        AllowSelf.record_self_approval(users, requester, approver, kind, subject)
    }
    fn decide_admin_filed(&self, _: &Connection, _: &str, _: &str, _: &str, _: RequestKind) -> Result<Approval, &'static str> {
        Err("NO_SECOND_ADMIN")
    }
    fn check_admin_filing(&self, _: &Connection, _: &str, _: &str) -> Result<(), &'static str> { Err("NO_SECOND_ADMIN") }
}

/// [`AllowSelf`] that logs every call: `("decide" | "record", in_transaction)`.
pub(super) struct Recording(pub(super) Rc<RefCell<Vec<(&'static str, bool)>>>);
impl ApprovalRule for Recording {
    fn decide(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind) -> Result<Approval, &'static str> {
        self.0.borrow_mut().push(("decide", !users.is_autocommit()));
        AllowSelf.decide(users, requester, approver, kind)
    }
    fn record_self_approval(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind, subject: &str)
        -> Result<(), &'static str> {
        self.0.borrow_mut().push(("record", !users.is_autocommit()));
        AllowSelf.record_self_approval(users, requester, approver, kind, subject)
    }
}

pub(super) struct Unconfirmed;
impl RunStopper for Unconfirmed {
    fn stop_employee_runs(&self, _: EmployeeId) -> Result<Vec<String>, ()> { Err(()) }
}

pub(super) struct Fakes {
    pub(super) probe: Probe,
    pub(super) routes: Routes,
    pub(super) stores: Box<dyn LabelStores>,
    pub(super) runs: Box<dyn RunStopper>,
    pub(super) rule: Box<dyn ApprovalRule>,
    pub(super) cap: usize,
    pub(super) now: i64,
}

impl Fakes {
    pub(super) fn env(&self) -> RestoreEnv<'_> {
        RestoreEnv { root: &self.probe, routes: &self.routes, stores: self.stores.as_ref(), runs: self.runs.as_ref(),
            rule: self.rule.as_ref(), agent_cap: self.cap, actor: "admin-user", now: self.now }
    }
}

pub(super) fn fakes(root: &Receipt) -> Fakes {
    Fakes { probe: Probe::ready(root), routes: Routes(Rc::new(RefCell::new(Vec::new()))), stores: Box::new(Marks(marks())),
        runs: Box::new(NoLiveRuns), rule: Box::new(AllowSelf), cap: 0, now: now() }
}

pub(super) fn write_tree(home: &Path, label: &str) {
    let dir = home.join("agents").join(label);
    std::fs::create_dir_all(dir.join("SKILLS")).unwrap();
    std::fs::write(dir.join("SOUL.md"), "soul").unwrap();
    std::fs::write(dir.join("SKILLS/a.md"), "skill").unwrap();
    std::fs::write(dir.join("agent.toml"), format!("[agent]\nname = \"{label}\"\n")).unwrap();
}

/// alice's Personal employee, confirmed, then tombstoned and sealed; the
/// store marks F6h will record at tombstone are written by hand.
pub(super) fn tombstone_with(f: &Fixture, runs: &dyn RunStopper) -> (ConfirmationItem, Receipt) {
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    let root = receipt(&item, 1, false);
    save(home, &item, &root).unwrap();
    write_tree(home, LABEL);
    let outcome = tombstone_employee(home, LABEL, root.employee, &TombstoneEnv { actor: "admin-user", runs, now: now() }).unwrap();
    assert_eq!(outcome.seal, "sealed");
    // F6h's tombstone_effects records the real marks now; the fake store
    // marks of these tests replace them (F6d: unconditional).
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET store_marks=?1", &[&marks().to_string()]);
    (item, root)
}

pub(super) fn tombstoned(f: &Fixture) -> (ConfirmationItem, Receipt) { tombstone_with(f, &NoLiveRuns) }

pub(super) fn file(f: &Fixture, fx: &Fakes) -> Result<RestoreRequest, Refusal> {
    file_personal_request(f.home(), &fx.env(), EMP, 0, Some(OwnerProof { user_id: &f.alice, factor: "totp" }))
        .map(|filed| filed.request)
}

pub(super) fn row(home: &Path, incarnation: u32) -> Quarantined {
    read_row(&Connection::open(home.join(APPLY_DB)).unwrap(), EMP, incarnation).unwrap().unwrap()
}

pub(super) fn stored(home: &Path, id: &str) -> RestoreRequest {
    read_request(&Connection::open(home.join(APPLY_DB)).unwrap(), id).unwrap().unwrap()
}

pub(super) fn hook(f: impl FnMut(HookPoint) -> bool + 'static) { RESTORE_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f))); }
pub(super) fn unhook() { RESTORE_HOOK.with(|h| *h.borrow_mut() = None); }

pub(super) fn in_quarantine(home: &Path) -> bool {
    quarantine_path(home, LABEL, EMP, 0).join("SOUL.md").exists() && !home.join("agents").join(LABEL).exists()
}

/// The happy path: owner files, another Admin approves; the same id comes
/// back with incarnation + 1, the mirror, the tree, ACL and policy bumps.
#[test]
fn restore_same_id_same_owner_ok() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let before = f.users.isolation_authority_snapshot(None, LABEL).unwrap();
    let request = file(&f, &fx).unwrap();
    assert_eq!((request.state.as_str(), request.kind, request.requester.as_str()), ("requested", RequestKind::PersonalRestore, f.alice.as_str()));
    assert_eq!(request.digest.len(), 64);
    // A second request for the same row is refused while one is open (L1).
    assert_eq!(file(&f, &fx).err(), Some(Refusal::RestoreInProgress));
    let restored = approve(home, &fx.env(), &request.request_id, &f.bob).unwrap();
    assert_eq!((restored.incarnation, restored.self_approved), (1, false));
    let lifecycle = read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])).unwrap();
    assert_eq!((lifecycle.employee, lifecycle.incarnation, lifecycle.uid, lifecycle.label.as_str()), (EMP, 1, root.uid, LABEL));
    assert_eq!(mirror(home, LABEL).as_deref(), Some(f.alice.as_str()));
    assert_eq!(f.users.check_agent_access(&f.alice, LABEL).unwrap(), Some(AccessLevel::Owner));
    assert!(home.join("agents").join(LABEL).join("SKILLS/a.md").exists());
    assert!(!quarantine_path(home, LABEL, EMP, 0).exists());
    let q = row(home, 0);
    assert_eq!((q.seal.as_str(), q.closed_by_request.as_deref()), ("restored", Some(request.request_id.as_str())));
    let done = stored(home, &request.request_id);
    assert_eq!((done.state.as_str(), done.approver.as_deref(), done.self_approved), ("done", Some(f.bob.as_str()), Some(false)));
    // A7/H1a: both epochs bumped.
    let after = f.users.isolation_authority_snapshot(None, LABEL).unwrap();
    assert!(after.acl_generation > before.acl_generation);
    assert!(after.policy_epoch > before.policy_epoch);
    // A done request cannot be approved again.
    assert_eq!(approve(home, &fx.env(), &request.request_id, &f.bob).err(), Some(Refusal::NotFound));
}

/// `owner_eligible` at request, at approval and in the mirror transaction.
#[test]
fn restore_refuses_ineligible_owner() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let forced = "UPDATE users SET must_change_password=1 WHERE id=?1";
    // 1. Request.
    sql(home, USERS_DB, forced, &[&f.alice]);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::OwnerIneligible));
    login(home, &f.alice);
    // 2. Approval.
    let first = file(&f, &fx).unwrap();
    sql(home, USERS_DB, forced, &[&f.alice]);
    assert_eq!(approve(home, &fx.env(), &first.request_id, &f.bob).err(), Some(Refusal::OwnerIneligible));
    assert_eq!(stored(home, &first.request_id).state, "failed");
    login(home, &f.alice);
    // 3. The users.db transaction of the apply step (after the move).
    let second = file(&f, &fx).unwrap();
    let (path, alice) = (home.to_path_buf(), f.alice.clone());
    hook(move |point| {
        if point == HookPoint::AfterMove { sql(&path, USERS_DB, "UPDATE users SET must_change_password=1 WHERE id=?1", &[&alice]); }
        false
    });
    let refused = approve(home, &fx.env(), &second.request_id, &f.bob);
    unhook();
    assert_eq!(refused.err(), Some(Refusal::OwnerIneligible));
    assert!(in_quarantine(home), "moved back");
    assert_eq!(row(home, 0).seal, "sealed");
    assert_eq!(stored(home, &second.request_id).state, "failed");
    assert_eq!(mirror(home, LABEL), None);
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root])), Err(Tombstoned));
}

/// Q3: a credential change by anyone but the owner refuses, attributed or not.
#[test]
fn restore_refuses_credential_change_by_non_owner() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let conn = Connection::open(home.join(USERS_DB)).unwrap();
    let since = duduclaw_auth::credential_changes::high_water(&conn).unwrap();
    conn.execute("UPDATE users SET password_hash='reset-by-admin' WHERE id=?1", [&f.alice]).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::CredentialChanged), "unattributed");
    duduclaw_auth::credential_changes::stamp_actor(&conn, since, &f.bob).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::CredentialChanged), "by another account");
    // An email change without any log row (snapshot differs) refuses too.
    let g = fixture();
    let (_, root) = tombstoned(&g);
    sql(g.home(), USERS_DB, "DROP TRIGGER credential_changes_email", &[]);
    sql(g.home(), USERS_DB, "UPDATE users SET email='other@example.test' WHERE id=?1", &[&g.alice]);
    assert_eq!(file(&g, &fakes(&root)).err(), Some(Refusal::CredentialChanged));
}

/// Q3: the owner's own, owner-authenticated changes do not block.
#[test]
fn restore_allows_owner_own_credential_change() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let conn = Connection::open(home.join(USERS_DB)).unwrap();
    let since = duduclaw_auth::credential_changes::high_water(&conn).unwrap();
    conn.execute("UPDATE users SET password_hash='owner-changed' WHERE id=?1", [&f.alice]).unwrap();
    conn.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'TOTP-ENC',0)", [&f.alice]).unwrap();
    duduclaw_auth::credential_changes::stamp_owner(&conn, since, &f.alice).unwrap();
    let request = file(&f, &fx).unwrap();
    assert_eq!(approve(home, &fx.env(), &request.request_id, &f.bob).unwrap().incarnation, 1);
}

/// H1a: an unfinished users.db side is retried once; a mirror that the
/// retry cannot remove refuses `TOMBSTONE_INCOMPLETE`.
#[test]
fn restore_refuses_incomplete_tombstone() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    assert!(record_personal_mirror(home, LABEL, &f.bob).unwrap());
    assert_eq!(file(&f, &fx).err(), Some(Refusal::TombstoneIncomplete));
    assert!(remove_personal_mirror(home, LABEL, &f.bob).unwrap());
    // The owner's own leftover mirror is what the retry removes.
    assert!(record_personal_mirror(home, LABEL, &f.alice).unwrap());
    file(&f, &fx).unwrap();
    assert_eq!(mirror(home, LABEL), None);
    assert!(f.users.get_agent_users(LABEL).unwrap().is_empty());
}

/// H2/S13: a pending fence is retried first; still pending refuses.
#[test]
fn restore_refuses_pending_fence_and_retries() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstone_with(&f, &Unconfirmed);
    assert_eq!(row(home, 0).pending_fences, vec![crate::isolation_quarantine::RUN_STOP_FENCE.to_owned()]);
    let mut fx = fakes(&root);
    fx.runs = Box::new(Unconfirmed);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::FencePending));
    fx.runs = Box::new(NoLiveRuns);
    file(&f, &fx).unwrap();
    assert!(row(home, 0).pending_fences.is_empty());
}

/// Q5: 90 days after the tombstone, at request and at approval.
#[test]
fn restore_refuses_after_90_days() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let at = row(home, 0).quarantined_at;
    let mut fx = fakes(&root);
    fx.now = at + RESTORE_WINDOW_SECS + 1;
    assert_eq!(file(&f, &fx).err(), Some(Refusal::Expired));
    fx.now = at + RESTORE_WINDOW_SECS - 60;
    let request = file(&f, &fx).unwrap();
    assert_eq!(request.expires_at, at + RESTORE_WINDOW_SECS, "never past the window");
    fx.now = at + RESTORE_WINDOW_SECS + 1;
    assert_eq!(approve(home, &fx.env(), &request.request_id, &f.bob).err(), Some(Refusal::Expired));
    assert_eq!(stored(home, &request.request_id).state, "expired");
    assert!(in_quarantine(home));
}

/// Q6: only a `sealed` row is restorable.
#[test]
fn restore_refuses_unsealed_or_oversize() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    for seal in ["pending", "oversize", "invalid", "absent"] {
        sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal=?1", &[&seal]);
        assert_eq!(file(&f, &fx).err(), Some(Refusal::NotSealed), "{seal}");
    }
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal='sealed'", &[]);
    file(&f, &fx).unwrap();
}

/// L2: the agent cap is checked again at approval.
#[test]
fn restore_rechecks_quota() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let team = applied(home, 2, "team", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    save(home, &team, &receipt(&team, 2, false)).unwrap();
    let mut fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    fx.cap = 1;
    assert_eq!(approve(home, &fx.env(), &request.request_id, &f.bob).err(), Some(Refusal::QuotaExceeded));
    assert_eq!(stored(home, &request.request_id).state, "requested", "transient: the request stays open");
    fx.cap = 2;
    assert_eq!(approve(home, &fx.env(), &request.request_id, &f.bob).unwrap().incarnation, 1);
}

/// A marker row, an `agents/<label>` entry or an open proposal holds the label.
#[test]
fn restore_refuses_open_proposal_or_marker() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    sql(home, APPLY_DB, "CREATE TABLE IF NOT EXISTS employee_dir_markers (label TEXT PRIMARY KEY, owner_user_id TEXT NOT NULL,
        digest TEXT NOT NULL, created_at TEXT NOT NULL)", &[]);
    sql(home, APPLY_DB, "INSERT INTO employee_dir_markers VALUES(?1,?2,'d','now')", &[&LABEL, &f.alice]);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::LabelTaken), "marker");
    sql(home, APPLY_DB, "DELETE FROM employee_dir_markers", &[]);
    std::fs::create_dir(home.join("agents").join(LABEL)).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::LabelTaken), "agents/<label>");
    std::fs::remove_dir(home.join("agents").join(LABEL)).unwrap();
    let mut store = ConfirmationStore::from_connection(Connection::open(home.join(CONFIRMATIONS_DB)).unwrap()).unwrap();
    let fields = [("employee_id", LABEL), ("disk_bytes", "67108864")].iter()
        .map(|(name, value)| ProposalField { name: (*name).into(), value: ProposalValue::Text { value: (*value).into() } }).collect();
    store.stage_proposal(ProposalOperationId { proposal_id: RequestId([9; 16]), revision: 1 }, ConfirmationCategory::Employee,
        fields, &Accept).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::LabelPending), "open proposal");
}

/// §3.5 step 1 (M1): a tampered quarantine is `invalid`, the request fails.
#[test]
fn restore_integrity_mismatch_in_quarantine() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    std::fs::write(quarantine_path(home, LABEL, EMP, 0).join("SOUL.md"), "tampered").unwrap();
    assert_eq!(approve(home, &fx.env(), &request.request_id, &f.bob).err(), Some(Refusal::Integrity));
    assert_eq!(row(home, 0).seal, "invalid");
    let failed = stored(home, &request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some("INTEGRITY")));
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root])), Err(Tombstoned));
    assert_eq!(file(&f, &fx).err(), Some(Refusal::NotSealed));
}

/// §3.5 b: a ctime change after the move is caught by the stat re-walk; the
/// tree goes back and the row needs a reseal (which then reads `invalid`).
#[test]
fn restore_ctime_change_after_move_moves_back() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    let soul = home.join("agents").join(LABEL).join("SOUL.md");
    hook(move |point| {
        if point == HookPoint::AfterMove {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::fs::set_permissions(&soul, std::fs::Permissions::from_mode(0o751)).unwrap();
        }
        false
    });
    let refused = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(refused.err(), Some(Refusal::Integrity));
    assert!(in_quarantine(home), "moved back");
    assert_eq!(row(home, 0).seal, "needs_reseal");
    assert_eq!(stored(home, &request.request_id).state, "failed");
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])), Err(Tombstoned));
    let recovered = recover_label(home, &fx.env(), LABEL).unwrap();
    assert_eq!(recovered.resealed, vec![(format!("{}-0", hex::encode(EMP.0)), "invalid".to_owned())]);
    assert_eq!(row(home, 0).seal, "invalid");
}

/// Q4: the route list is digest-bound; a change at approval or inside the
/// apply transaction refuses (`ROUTES_CHANGED`, the requester files again).
#[test]
fn restore_routes_changed_refused() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    *fx.routes.0.borrow_mut() = vec![("acct-google".into(), 1)];
    let first = file(&f, &fx).unwrap();
    *fx.routes.0.borrow_mut() = vec![("acct-google".into(), 2)];
    assert_eq!(approve(home, &fx.env(), &first.request_id, &f.bob).err(), Some(Refusal::RoutesChanged));
    assert_eq!(stored(home, &first.request_id).failure.as_deref(), Some("ROUTES_CHANGED"));
    let second = file(&f, &fx).unwrap();
    let routes = fx.routes.0.clone();
    hook(move |point| {
        if point == HookPoint::AfterMove { routes.borrow_mut().push(("acct-new".into(), 1)); }
        false
    });
    let refused = approve(home, &fx.env(), &second.request_id, &f.bob);
    unhook();
    assert_eq!(refused.err(), Some(Refusal::RoutesChanged));
    assert!(in_quarantine(home));
    assert_eq!(row(home, 0).seal, "sealed");
}

/// §3.3: store marks must be recorded and unchanged.
#[test]
fn restore_store_marks_changed_refused() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    fx.stores = Box::new(Marks(json!({"memory": 4, "sessions": 1, "tasks": 0, "cron": 0})));
    assert_eq!(file(&f, &fx).err(), Some(Refusal::StoresChanged));
    // The production registry (until F6h) is unreadable: never "unchanged".
    fx.stores = Box::new(UnregisteredStores);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::StoresUnavailable));
    fx.stores = Box::new(Marks(marks()));
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET store_marks=NULL", &[]);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::StoresUnavailable), "unrecorded");
}

/// H4: the org entry is in the seal; a changed entry is `ORG_CHANGED` and
/// leaves the (untouched) tree sealed.
#[test]
fn restore_org_entry_changed_refused() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    duduclaw_core::org_store::with_org_lock(home, |org| {
        duduclaw_core::org_store::upsert(org, LABEL, duduclaw_core::org_store::OrgEntry::new("", "Sales"))
    }).unwrap();
    assert_eq!(approve(home, &fx.env(), &request.request_id, &f.bob).err(), Some(Refusal::OrgChanged));
    assert_eq!(row(home, 0).seal, "sealed");
    assert_eq!(stored(home, &request.request_id).failure.as_deref(), Some("ORG_CHANGED"));
    assert!(in_quarantine(home));
}

/// M8: a live run, unit, lease or process of the employee refuses.
#[test]
fn restore_root_live_process_refused() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    fx.probe.idle = Some(false);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::Root(RootClass::Busy)));
    fx.probe.idle = None;
    fx.probe.idle_answer = Ok(false);
    assert_eq!(restore_precheck(f.home(), LABEL, EMP, &fx.probe), Ok(RootClass::Busy));
    assert_eq!(file(&f, &fx).err(), Some(Refusal::Root(RootClass::Busy)));
}

/// S5d: a transport failure or no reply is unavailable, never gone.
#[test]
fn restore_transport_error_is_unavailable_not_gone() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    for error in [RootError::Transport, RootError::NoReply, RootError::InvalidRequest] {
        fx.probe.error = Some(error);
        assert_eq!(restore_precheck(home, LABEL, EMP, &fx.probe), Ok(RootClass::Unavailable), "{error:?}");
    }
    assert_eq!(file(&f, &fx).err(), Some(Refusal::Root(RootClass::Unavailable)));
    // The production probe on a host without the root socket.
    let production = restore_precheck(home, LABEL, EMP, &SpawndRootProbe).unwrap();
    assert!(!matches!(production, RootClass::Gone | RootClass::Ok), "{production:?}");
    // Only an authoritative NotFound is gone.
    fx.probe.error = Some(RootError::NotFound);
    assert_eq!(restore_precheck(home, LABEL, EMP, &fx.probe), Ok(RootClass::Gone));
}

/// S4: any ReadBack mismatch (principal, UID, quota, label) or spawnd
/// `Inconsistent` is inconsistent, never gone; retiring is retired.
#[test]
fn restore_readback_principal_mismatch_is_inconsistent_not_gone() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    let mutations: [fn(&mut Receipt, &str); 5] = [
        |r, bob| r.principal_binding = Some(EmployeeBinding::Personal(text(bob))),
        |r, _| r.uid += 1,
        |r, _| r.bytes += 1,
        |r, _| r.inodes += 1,
        |r, _| r.disabled = true,
    ];
    for mutate in mutations {
        let mut probe = Probe::ready(&root);
        mutate(&mut probe.receipt, &f.bob);
        assert_eq!(restore_precheck(home, LABEL, EMP, &probe), Ok(RootClass::Inconsistent));
    }
    fx.probe.error = Some(RootError::Inconsistent);
    assert_eq!(restore_precheck(home, LABEL, EMP, &fx.probe), Ok(RootClass::Inconsistent));
    assert_eq!(file(&f, &fx).err(), Some(Refusal::Root(RootClass::Inconsistent)));
    fx.probe.error = None;
    for (phase, class) in [(RootPhase::Retiring, RootClass::Retired), (RootPhase::Retired, RootClass::Retired),
        (RootPhase::Disabled, RootClass::Inconsistent), (RootPhase::Planned, RootClass::Unavailable)] {
        fx.probe.phase = phase;
        assert_eq!(restore_precheck(home, LABEL, EMP, &fx.probe), Ok(class), "{phase:?}");
    }
}

/// S5d: the 4 s worker timeout and `IsolationUnavailable` are unavailable.
#[test]
fn restore_timeout_is_unavailable_not_gone() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    for error in [RootError::Timeout, RootError::IsolationUnavailable, RootError::NotReady] {
        fx.probe.error = Some(error);
        assert_eq!(restore_precheck(f.home(), LABEL, EMP, &fx.probe), Ok(RootClass::Unavailable), "{error:?}");
        assert_eq!(file(&f, &fx).err(), Some(Refusal::Root(RootClass::Unavailable)));
    }
}

/// M8/S17: idleness is required; an unanswered idle readback refuses.
#[test]
fn restore_readback_requires_idle() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    fx.probe.idle = None;
    for (answer, class) in [(Err(RootError::IsolationUnavailable), RootClass::Unavailable),
        (Err(RootError::Timeout), RootClass::Unavailable), (Err(RootError::NotFound), RootClass::Inconsistent),
        (Ok(true), RootClass::Ok)] {
        fx.probe.idle_answer = answer;
        assert_eq!(restore_precheck(home, LABEL, EMP, &fx.probe), Ok(class), "{answer:?}");
    }
    // The production probe (F6d spawnd EmployeeIdle client) with no spawnd
    // socket in the test environment: a transport error, unavailable.
    assert_eq!(SpawndRootProbe.employee_idle(EMP), Err(RootError::IsolationUnavailable));
    fx.probe.idle = Some(true);
    file(&f, &fx).unwrap();
}

/// The normal apply path still never revives a disabled row of the same
/// id; only the restore engine does.
#[test]
fn restore_apply_path_still_never_revives_disabled_row() {
    let f = fixture();
    let home = f.home();
    let (item, root) = tombstoned(&f);
    assert!(save(home, &item, &receipt(&item, 1, false)).is_err());
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])), Err(Tombstoned));
    assert!(confirmed_receipt(home, LABEL).unwrap().unwrap().disabled);
    assert_eq!(mirror(home, LABEL), None);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    approve(home, &fx.env(), &request.request_id, &f.bob).unwrap();
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root])).unwrap().incarnation, 1);
}

/// S7: restore → tombstone → purge → retire. The restored inc-0 row counts
/// as closed; the label's rows are all closed once the inc-1 row is purged
/// (F6i, simulated) and the single per-id root row then retires (F6l,
/// simulated), after which nothing on the id is restorable.
#[test]
fn restore_then_tombstone_purge_retire_frees_label() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    approve(home, &fx.env(), &request.request_id, &f.bob).unwrap();
    assert!(all_rows_closed(home, LABEL).unwrap(), "restored counts as closed");
    let again = tombstone_employee(home, LABEL, EMP, &TombstoneEnv { actor: "admin-user", runs: &NoLiveRuns, now: now() }).unwrap();
    assert_eq!((again.incarnation, again.seal.as_str()), (1, "sealed"));
    assert_eq!(row(home, 0).seal, "restored");
    assert!(!all_rows_closed(home, LABEL).unwrap(), "inc 1 is open");
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET seal='purged' WHERE incarnation=1", &[]);
    assert!(all_rows_closed(home, LABEL).unwrap());
    let roots: i64 = Connection::open(home.join(APPLY_DB)).unwrap()
        .query_row("SELECT count(*) FROM root_allocations WHERE root_state='allocated'", [], |r| r.get(0)).unwrap();
    assert_eq!(roots, 1, "one root row per id");
    sql(home, APPLY_DB, "UPDATE root_allocations SET root_state='retired'", &[]);
    assert_eq!(restore_precheck(home, LABEL, EMP, &fx.probe), Ok(RootClass::Retired));
}

fn self_approval_rows(home: &Path, subject: &str) -> i64 {
    Connection::open(home.join(USERS_DB)).unwrap().query_row(
        "SELECT count(*) FROM auth_audit_log WHERE action=?1 AND target=?2",
        rusqlite::params![duduclaw_auth::admin_approval_rule::SELF_APPROVED_AUDIT_ACTION, subject], |r| r.get(0)).unwrap()
}

/// F6c2 (§3.4 sole Admin): the rule runs twice (tx1's read-only check, then
/// the users.db write tx of the apply), but the users.db
/// `isolation.employee_restore_self_approved` row is written once, by the
/// apply's write step, right after its own decide and in that transaction
/// (the real F6c2 writer refuses an autocommit connection, and the row is
/// in users.db after the apply). Another Admin's approval writes none.
#[test]
fn self_approval_row_written_once_in_apply_tx() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let calls = Rc::new(RefCell::new(Vec::new()));
    let fx = Fakes { rule: Box::new(Recording(calls.clone())), ..fakes(&root) };
    let request = file(&f, &fx).unwrap();
    calls.borrow_mut().clear();
    let restored = approve(home, &fx.env(), &request.request_id, &f.alice).unwrap();
    assert!(restored.self_approved);
    assert_eq!(*calls.borrow(), vec![("decide", true), ("decide", true), ("record", true)],
        "tx1 decides only; the apply decides and records in one transaction");
    assert_eq!(self_approval_rows(home, &request.request_id), 1);
    assert_eq!(stored(home, &request.request_id).self_approved, Some(true));
    // Another Admin approving: the rule never records, no row.
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let calls = Rc::new(RefCell::new(Vec::new()));
    let fx = Fakes { rule: Box::new(Recording(calls.clone())), ..fakes(&root) };
    let request = file(&f, &fx).unwrap();
    calls.borrow_mut().clear();
    assert!(!approve(home, &fx.env(), &request.request_id, &f.bob).unwrap().self_approved);
    assert_eq!(*calls.borrow(), vec![("decide", true), ("decide", true)]);
    assert_eq!(self_approval_rows(home, &request.request_id), 0);
}

// ── F6fa (security fix round, part a) ───────────────────────────────────

/// Review item 2 (L2 race): another employee goes live after the approval
/// call's outside checks (here: between them and tx1). The apply
/// transaction recounts under the apply.db writer lock and refuses; the
/// tree goes back to the quarantine.
#[test]
fn f6fa_quota_recounted_in_apply_tx() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let mut fx = fakes(&root);
    fx.cap = 1;
    let request = file(&f, &fx).unwrap();
    let (path, tenant) = (home.to_path_buf(), f.tenant.clone());
    hook(move |point| {
        if point == HookPoint::BeforeTx1 {
            let team = applied(&path, 2, "team", &[("binding_kind", "shared"), ("tenant_id", tenant.as_str())]);
            save(&path, &team, &receipt(&team, 2, false)).unwrap();
        }
        false
    });
    let refused = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(refused.err(), Some(Refusal::QuotaExceeded));
    assert!(in_quarantine(home), "moved back (never moved)");
    assert_eq!(row(home, 0).seal, "sealed");
    let failed = stored(home, &request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some("QUOTA_EXCEEDED")));
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root])), Err(Tombstoned));
    assert_eq!(mirror(home, LABEL), None);
}

/// Review item 2 (label race): an Employee proposal for the label staged
/// after the outside checks is seen inside the apply transaction.
#[test]
fn f6fa_open_proposal_rechecked_in_apply_tx() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    let path = home.to_path_buf();
    hook(move |point| {
        if point == HookPoint::BeforeTx1 {
            let mut store = ConfirmationStore::from_connection(Connection::open(path.join(CONFIRMATIONS_DB)).unwrap()).unwrap();
            let fields = [("employee_id", LABEL), ("disk_bytes", "67108864")].iter()
                .map(|(name, value)| ProposalField { name: (*name).into(), value: ProposalValue::Text { value: (*value).into() } }).collect();
            store.stage_proposal(ProposalOperationId { proposal_id: RequestId([8; 16]), revision: 1 }, ConfirmationCategory::Employee,
                fields, &Accept).unwrap();
        }
        false
    });
    let refused = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(refused.err(), Some(Refusal::LabelPending));
    assert!(in_quarantine(home));
    assert_eq!(row(home, 0).seal, "sealed");
    assert_eq!(stored(home, &request.request_id).failure.as_deref(), Some("LABEL_PENDING"));
}

/// Review item 2 (Q3 race): an Admin resets the owner's password after the
/// approval call's outside check, while the apply holds apply.db but not
/// yet users.db. The users.db write transaction of the apply re-runs the
/// credential check and refuses; nothing of the restore remains.
#[test]
fn f6fa_credential_change_rechecked_in_apply_tx() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    let (path, alice) = (home.to_path_buf(), f.alice.clone());
    hook(move |point| {
        if point == HookPoint::AfterMove {
            sql(&path, USERS_DB, "UPDATE users SET password_hash='reset-by-admin' WHERE id=?1", &[&alice]);
        }
        false
    });
    let refused = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(refused.err(), Some(Refusal::CredentialChanged));
    assert!(in_quarantine(home), "moved back");
    assert_eq!(row(home, 0).seal, "sealed");
    assert_eq!(stored(home, &request.request_id).failure.as_deref(), Some("CREDENTIAL_CHANGED"));
    assert_eq!(mirror(home, LABEL), None);
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root])), Err(Tombstoned));
}

/// Review item 3: each differing snapshot field needs an owner row of a
/// field that can change it. An owner-stamped password change does not
/// explain an email change that left no log row (the old check passed any
/// difference as soon as some owner row existed).
#[test]
fn f6fa_q3_maps_each_snapshot_field_to_its_log_fields() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let conn = Connection::open(home.join(USERS_DB)).unwrap();
    let since = duduclaw_auth::credential_changes::high_water(&conn).unwrap();
    conn.execute("UPDATE users SET password_hash='owner-changed' WHERE id=?1", [&f.alice]).unwrap();
    duduclaw_auth::credential_changes::stamp_owner(&conn, since, &f.alice).unwrap();
    // The owner's own password change alone passes (owner-stamped; the epoch is not compared, F6fa2).
    let first = file(&f, &fx).unwrap();
    close_request(home, &first.request_id, &f.bob, Close::Cancelled, now()).unwrap();
    // An email change with no log row (trigger removed): not explained.
    conn.execute_batch("DROP TRIGGER credential_changes_email").unwrap();
    conn.execute("UPDATE users SET email='other@example.test' WHERE id=?1", [&f.alice]).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::CredentialChanged));
    assert!(SNAPSHOT_LOG_FIELDS.contains(&("email", &["email"][..])));
}

/// Review item 3: the log is read after the `credential_seq` recorded at the
/// tombstone, never by wall clock. A non-owner row written after the
/// tombstone with a clock set back is still counted; a row from before the
/// tombstone with a clock set ahead is not.
#[test]
fn f6fa_q3_counts_by_seq_not_clock() {
    let f = fixture();
    let home = f.home();
    // Before the tombstone, stamped an hour in the future.
    let users = Connection::open(home.join(USERS_DB)).unwrap();
    users.execute("INSERT INTO credential_changes(user_id,field,actor_user_id,owner_authenticated,at) VALUES(?1,'password',NULL,0,?2)",
        rusqlite::params![f.alice, now() + 3_600]).unwrap();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let q = row(home, 0);
    let recorded = q.principal_snapshot["credential_seq"].as_i64().unwrap();
    assert_eq!(recorded, duduclaw_auth::credential_changes::high_water(&users).unwrap(), "seq recorded at the tombstone");
    let request = file(&f, &fx).unwrap();
    close_request(home, &request.request_id, &f.bob, Close::Cancelled, now()).unwrap();
    // After the tombstone, stamped a day before it.
    users.execute("INSERT INTO credential_changes(user_id,field,actor_user_id,owner_authenticated,at) VALUES(?1,'password',NULL,0,?2)",
        rusqlite::params![f.alice, q.quarantined_at - 86_400]).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::CredentialChanged));
}

/// A row tombstoned before F6fa has no `credential_seq` (and no factor
/// baseline): refused `SNAPSHOT_INCOMPLETE`, never a wall-clock fallback.
#[test]
fn f6fa_legacy_snapshot_without_credential_seq_refused() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let mut snapshot = row(home, 0).principal_snapshot;
    assert!(snapshot["credential_seq"].is_i64());
    snapshot.as_object_mut().unwrap().remove("credential_seq");
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET principal_snapshot=?1", &[&snapshot.to_string()]);
    assert_eq!(file(&f, &fx).err(), Some(Refusal::SnapshotIncomplete));
    assert_eq!(Refusal::SnapshotIncomplete.code(), "SNAPSHOT_INCOMPLETE");
}

/// Review item 4: the engine accepts only the three factor names the RPC can
/// verify; `google_reauth` (no such verifier) is refused.
#[test]
fn f6fa_google_reauth_not_an_accepted_factor() {
    let f = fixture();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    assert!(!PROOF_FACTORS.contains(&"google_reauth"));
    let proof = OwnerProof { user_id: &f.alice, factor: "google_reauth" };
    assert_eq!(file_personal_request(f.home(), &fx.env(), EMP, 0, Some(proof)).err(), Some(Refusal::FactorRequired));
    let proof = OwnerProof { user_id: &f.alice, factor: "totp" };
    assert!(file_personal_request(f.home(), &fx.env(), EMP, 0, Some(proof)).is_ok());
}

/// Review item 6 (§3.5 d "re-insert the marker row", verified): the
/// creation marker only lets a Personal *proposal* adopt a directory it
/// created while no live row holds the label. A restored employee's label
/// is held by its live confirmed row (`check_marker` refuses LABEL_TAKEN
/// before reading any marker) and by the R7 / restore checks, so the restore
/// writes no marker: re-inserting one would protect nothing and would let a
/// later proposal flow treat the restored tree as its own.
#[test]
fn f6fa_restored_label_protected_without_marker() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let request = file(&f, &fx).unwrap();
    approve(home, &fx.env(), &request.request_id, &f.bob).unwrap();
    let db = Connection::open(home.join(APPLY_DB)).unwrap();
    let table: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='employee_dir_markers')",
        [], |r| r.get(0)).unwrap();
    if table {
        let marked: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM employee_dir_markers WHERE label=?1)", [LABEL], |r| r.get(0)).unwrap();
        assert!(!marked, "no marker after a restore");
    }
    let refused = crate::handlers::employee_dirs::check_marker(&db, home, &home.join("agents"), LABEL, &f.alice).err();
    assert!(refused.is_some_and(|message| message.contains("已被使用")), "{refused:?}");
}

// ── F6fa2 (owner decisions 2026-11-09) ──────────────────────────────────

fn credential_epoch(home: &Path, user: &str) -> i64 {
    Connection::open(home.join(USERS_DB)).unwrap()
        .query_row("SELECT credential_epoch FROM users WHERE id=?1", [user], |r| r.get(0)).unwrap()
}

/// Decision 1: "log out all devices" (the owner's `remember_revoke`, and an
/// Admin's revoke-all, which bumps the epoch the same way) after the
/// tombstone changes only `credential_epoch`: no password, email or factor,
/// no `credential_changes` row. It never blocks a restore (request, approval
/// and the apply transaction's re-check all pass).
#[test]
fn f6fa2_session_revoke_after_tombstone_allows_restore() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let snapshot = row(home, 0).principal_snapshot;
    let recorded = snapshot["credential_epoch"].as_i64().unwrap();
    let seq = snapshot["credential_seq"].as_i64().unwrap();
    // The owner logs out all devices; an Admin then revokes all of the owner's sessions.
    f.users.remember_revoke(&f.alice, None).unwrap();
    sql(home, USERS_DB, "UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", &[&f.alice]);
    assert!(credential_epoch(home, &f.alice) >= recorded + 2, "the epoch moved");
    let users = Connection::open(home.join(USERS_DB)).unwrap();
    assert!(duduclaw_auth::credential_changes::changes_after(&users, &f.alice, seq, 10).unwrap().is_empty(),
        "a session revocation is not a credential change");
    assert!(!SNAPSHOT_LOG_FIELDS.iter().any(|(field, _)| *field == "credential_epoch"), "the epoch is not compared");
    let request = file(&f, &fx).unwrap();
    // And once more between the approval call's checks and the apply transaction.
    let (path, alice) = (home.to_path_buf(), f.alice.clone());
    hook(move |point| {
        if point == HookPoint::AfterMove {
            sql(&path, USERS_DB, "UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", &[&alice]);
        }
        false
    });
    let restored = approve(home, &fx.env(), &request.request_id, &f.bob);
    unhook();
    assert_eq!(restored.unwrap().incarnation, 1);
    assert_eq!(mirror(home, LABEL).as_deref(), Some(f.alice.as_str()));
}

/// Decision 1, the other side: a session revocation does not excuse a real
/// credential change. An Admin's password reset of the owner (with or
/// without a revoke-all) still refuses `CREDENTIAL_CHANGED`, as does a
/// second factor enrolled for the owner by someone else.
#[test]
fn f6fa2_admin_password_reset_still_refuses() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    f.users.remember_revoke(&f.alice, None).unwrap();
    // The Admin-side password writer (unattributed: not the owner's own session).
    f.users.update_user(&f.alice, None, None, Some("admin-set-password")).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::CredentialChanged));
    let users = Connection::open(home.join(USERS_DB)).unwrap();
    let since = row(home, 0).principal_snapshot["credential_seq"].as_i64().unwrap();
    duduclaw_auth::credential_changes::stamp_actor(&users, since, &f.bob).unwrap();
    assert_eq!(file(&f, &fx).err(), Some(Refusal::CredentialChanged), "attributed to the Admin");
    // A factor enrolled for the owner by someone else refuses too.
    let g = fixture();
    let (_, root) = tombstoned(&g);
    sql(g.home(), USERS_DB, "UPDATE users SET credential_epoch=credential_epoch+1 WHERE id=?1", &[&g.alice]);
    sql(g.home(), USERS_DB, "INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'TOTP-ENC',0)", &[&g.alice]);
    assert_eq!(file(&g, &fakes(&root)).err(), Some(Refusal::CredentialChanged));
}

/// Decision 2 (engine): the Admin-filed request for an owner without a
/// second factor is digest-bound to its filer and flag, and its approval
/// goes through the admin-filed rule (a party approver is refused; with no
/// other qualifying Admin, `NO_SECOND_ADMIN` — never self-approval).
#[test]
fn f6fa2_admin_filed_request_engine_rule() {
    let f = fixture();
    let home = f.home();
    let (_, root) = tombstoned(&f);
    let fx = fakes(&root);
    let filer = AdminFiler { admin_user_id: &f.bob, fresh_step_up: true };
    let filed = file_personal_request_for_owner(home, &fx.env(), EMP, 0, filer).unwrap().request;
    assert_eq!((filed.requester.as_str(), filed.filed_by_admin.as_deref(), filed.owner.as_deref(), filed.owner_unverified()),
        (f.bob.as_str(), Some(f.bob.as_str()), Some(f.alice.as_str()), true));
    assert_eq!(stored(home, &filed.request_id), filed);
    // The digest binds the filer: an owner-filed twin of the row digests differently.
    let twin = RestoreRequest { filed_by_admin: None, ..filed.clone() };
    let sealed = row(home, 0).digest.unwrap();
    assert_ne!(request_digest(&twin, &sealed), filed.digest);
    // Flipping the flag in the store makes the row unreadable (never a silent owner-filed request).
    sql(home, APPLY_DB, "UPDATE employee_restores SET owner_unverified=0 WHERE request_id=?1", &[&filed.request_id]);
    assert_eq!(read_request(&Connection::open(home.join(APPLY_DB)).unwrap(), &filed.request_id).err(), Some(Refusal::Unavailable));
    sql(home, APPLY_DB, "UPDATE employee_restores SET owner_unverified=1 WHERE request_id=?1", &[&filed.request_id]);
    // The filer and the owner never approve; a third Admin does, not self-approved.
    assert_eq!(approve(home, &fx.env(), &filed.request_id, &f.bob).err(), Some(Refusal::Approval("SECOND_ADMIN_REQUIRED")));
    assert_eq!(approve(home, &fx.env(), &filed.request_id, &f.alice).err(), Some(Refusal::Approval("SECOND_ADMIN_REQUIRED")));
    assert_eq!(stored(home, &filed.request_id).state, "requested", "a refused approver leaves it open");
    let restored = approve(home, &fx.env(), &filed.request_id, "carol-admin").unwrap();
    assert_eq!((restored.incarnation, restored.self_approved), (1, false));
    let done = stored(home, &filed.request_id);
    assert_eq!((done.state.as_str(), done.approver.as_deref(), done.self_approved), ("done", Some("carol-admin"), Some(false)));
    // A sole-Admin home: filing refuses NO_SECOND_ADMIN.
    let g = fixture();
    let (_, root) = tombstoned(&g);
    let sole = Fakes { rule: Box::new(SoleAdmin), ..fakes(&root) };
    let filer = AdminFiler { admin_user_id: &g.bob, fresh_step_up: true };
    assert_eq!(file_personal_request_for_owner(g.home(), &sole.env(), EMP, 0, filer).err(), Some(Refusal::Approval("NO_SECOND_ADMIN")));
    // The production rule without the admin-filed decision refuses (fail closed).
    let unavailable = Fakes { rule: Box::new(ApprovalRuleUnavailable), ..fakes(&root) };
    assert_eq!(file_personal_request_for_owner(g.home(), &unavailable.env(), EMP, 0, filer).err(),
        Some(Refusal::Approval(ADMIN_FILED_RULE_UNAVAILABLE)));
}

#[path = "isolation_restore_recovery_tests.rs"]
mod recovery;
/// F6g adoption under a new employee id (same fixture and fakes).
#[path = "isolation_adopt_tests.rs"]
mod adopt;
