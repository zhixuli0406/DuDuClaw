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

#[path = "isolation_restore_recovery_tests.rs"]
mod recovery;
