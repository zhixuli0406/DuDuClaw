//! F6k Shared restore (design r5 §3.9, §3.4, §3.7; `crate::isolation_restore_shared`)
//! over the real stores of the lifecycle fixture: a Shared employee `team-x`
//! of the pinned tenant with three members (alice operator, bob owner, carol
//! viewer) is tombstoned and sealed (F6a), an Admin files, another Admin
//! approves through the engine. Root, engine routes, store marks, run
//! stopper and approval rule are the F6c fakes (`isolation_restore_tests.rs`);
//! the tickable tenant routes are a fake except in the catalog test.
//!
//! Child of `isolation_lifecycle_tests` (fixture reuse); Unix only; not
//! compiled or run on the macOS worktree (Linux builder).
use super::restore::{fakes, marks, now, stored, write_tree, AllowSelf, Fakes};
use super::*;
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};
use crate::isolation_restore::{self as engine, Approval, ApprovalRule, Refusal, RequestKind};
use crate::isolation_restore_shared::*;
use duduclaw_mcp_boundary::VerificationError;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

const TEAM: &str = "team-x";
const TEAM_EMP: EmployeeId = EmployeeId([2; 16]);

struct Team { f: Fixture, carol: String, root: Receipt }
impl Team { fn home(&self) -> &Path { self.f.home() } }

/// `team-x` confirmed, three members bound, then tombstoned and sealed; the
/// store marks are the fakes' (as the F6c fixture does).
fn team() -> Team {
    let f = fixture();
    let home = f.home();
    let carol = f.users.create_user("carol@example.test", "Carol", "password1", UserRole::Employee).unwrap().id;
    login(home, &carol);
    let item = applied(home, 2, TEAM, &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = receipt(&item, 2, false);
    save(home, &item, &root).unwrap();
    f.users.bind_agent(&f.alice, TEAM, AccessLevel::Operator).unwrap();
    f.users.bind_agent(&f.bob, TEAM, AccessLevel::Owner).unwrap();
    f.users.bind_agent(&carol, TEAM, AccessLevel::Viewer).unwrap();
    write_tree(home, TEAM);
    let outcome = tombstone_employee(home, TEAM, root.employee, &TombstoneEnv { actor: "admin-user", runs: &NoLiveRuns, now: now() })
        .unwrap();
    assert_eq!(outcome.seal, "sealed");
    sql(home, APPLY_DB, "UPDATE employee_quarantine SET store_marks=?1", &[&marks().to_string()]);
    Team { f, carol, root }
}

struct Offered(Vec<TenantRoute>);
impl TenantRouteSource for Offered {
    fn tickable(&self, _: &str) -> Result<Vec<TenantRoute>, ()> { Ok(self.0.clone()) }
}

fn route(account: &str, generation: u64, kind: ConnectorKind) -> TenantRoute {
    TenantRoute { account: account.into(), generation, provider: "google".into(), kind }
}

/// Two tenant routes are tickable; the engine's catalog list also holds a
/// per-employee detail route, which is never offered.
fn offered() -> Offered {
    Offered(vec![route("acct-a", 1, ConnectorKind::SharedTenant), route("acct-b", 1, ConnectorKind::SharedHome)])
}

fn fakes_for(t: &Team) -> Fakes {
    let fx = fakes(&t.root);
    *fx.routes.0.borrow_mut() = vec![("acct-a".into(), 1), ("acct-b".into(), 1), ("detail-x".into(), 1)];
    fx
}

fn member(user: &str, level: &str) -> Value { json!({"user_id": user, "access_level": level}) }
fn tick(account: &str, generation: u64) -> Value { json!({"account": account, "generation": generation}) }

fn file_by(t: &Team, fx: &Fakes, filer: &str, members: Value, ticks: Value) -> Result<SharedFiled, SharedError> {
    file_shared_request(t.home(), &fx.env(), &offered(), TEAM_EMP, 0, SharedFiler { admin_user_id: filer, fresh_step_up: true },
        &members, &ticks)
}
fn file(t: &Team, fx: &Fakes, members: Value, ticks: Value) -> Result<SharedFiled, SharedError> {
    file_by(t, fx, &t.f.bob, members, ticks)
}

fn approve_as(t: &Team, fx: &Fakes, id: &str, approver: &str) -> Result<engine::Restored, SharedError> {
    approve(t.home(), &fx.env(), &stored(t.home(), id), approver)
}

/// The member's binding on `team-x` in users.db (level), if any.
fn bound(home: &Path, user: &str) -> Option<String> {
    Connection::open(home.join(USERS_DB)).unwrap().query_row(
        "SELECT access_level FROM user_agent_bindings WHERE user_id=?1 AND agent_name=?2", [user, TEAM], |r| r.get(0))
        .optional().unwrap()
}

fn shared_binding(t: &Team) -> EmployeeBinding { EmployeeBinding::Shared(Text::new(t.f.tenant.clone()).unwrap()) }

fn edit_snapshot(home: &Path, edit: impl FnOnce(&mut Value)) {
    let db = Connection::open(home.join(APPLY_DB)).unwrap();
    let text: String = db.query_row("SELECT principal_snapshot FROM employee_quarantine WHERE employee_hex=?1",
        [hex::encode(TEAM_EMP.0)], |r| r.get(0)).unwrap();
    let mut snapshot: Value = serde_json::from_str(&text).unwrap();
    edit(&mut snapshot);
    db.execute("UPDATE employee_quarantine SET principal_snapshot=?1 WHERE employee_hex=?2",
        rusqlite::params![snapshot.to_string(), hex::encode(TEAM_EMP.0)]).unwrap();
}

/// Any approver but the requester (another qualifying Admin exists).
struct TwoAdmins;
impl ApprovalRule for TwoAdmins {
    fn decide(&self, _: &Connection, requester: &str, approver: &str, _: RequestKind) -> Result<Approval, &'static str> {
        if requester == approver { Err("SECOND_ADMIN_REQUIRED") } else { Ok(Approval { self_approved: false }) }
    }
    fn record_self_approval(&self, _: &Connection, _: &str, _: &str, _: RequestKind, _: &str) -> Result<(), &'static str> {
        Err("UNEXPECTED_SELF_APPROVAL")
    }
}

/// Tenant == apply.db pin == users.db tenant, and the tombstone snapshot's
/// `tenant_id` must name that pin too.
#[test]
fn shared_restore_tenant_must_equal_pin() {
    let t = team();
    let fx = fakes_for(&t);
    let home = t.home();
    let members = json!([member(&t.f.alice, "operator")]);
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id='other-tenant' WHERE label=?1", &[&TEAM]);
    assert_eq!(file(&t, &fx, members.clone(), json!([])).err(), Some(SharedError::Restore(Refusal::TenantMismatch)));
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id=?1 WHERE label=?2", &[&t.f.tenant, &TEAM]);
    edit_snapshot(home, |snapshot| snapshot["tenant_id"] = json!("other-tenant"));
    assert_eq!(file(&t, &fx, members.clone(), json!([])).err(), Some(SharedError::Restore(Refusal::TenantMismatch)));
    let tenant = t.f.tenant.clone();
    edit_snapshot(home, move |snapshot| snapshot["tenant_id"] = json!(tenant));
    // A stale step-up or a Personal filing path never reaches the row.
    assert_eq!(file_shared_request(home, &fx.env(), &offered(), TEAM_EMP, 0,
        SharedFiler { admin_user_id: &t.f.bob, fresh_step_up: false }, &members, &json!([])).err(),
        Some(SharedError::Restore(Refusal::Approval("STEP_UP_REQUIRED"))));
    let filed = file(&t, &fx, members, json!([])).unwrap();
    assert_eq!((filed.request.kind, filed.request.tenant.as_str(), filed.request.owner.as_deref()),
        (RequestKind::SharedRestore, t.f.tenant.as_str(), None));
    // The pin moving after filing refuses at approval (common checks, tx1/tx2).
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id='other-tenant' WHERE label=?1", &[&TEAM]);
    assert_eq!(approve_as(&t, &fx, &filed.request.request_id, "admin-2").err(), Some(SharedError::Restore(Refusal::TenantMismatch)));
    assert_eq!(bound(home, &t.f.alice), None);
}

/// Members only from the tombstone snapshot: outsiders, duplicates,
/// malformed entries and extra fields refuse; nothing is filed.
#[test]
fn shared_restore_members_only_from_snapshot() {
    let t = team();
    let fx = fakes_for(&t);
    let outsider = t.f.users.create_user("dave@example.test", "Dave", "password1", UserRole::Employee).unwrap().id;
    login(t.home(), &outsider);
    for members in [
        json!([member(&outsider, "viewer")]),
        json!([member(&t.f.alice, "viewer"), member(&t.f.alice, "viewer")]),
        json!([member(&t.f.alice.to_uppercase(), "viewer")]),
        json!([{"user_id": t.f.alice, "access_level": "viewer", "is_primary": true}]),
        json!([{"user_id": t.f.alice}]),
        json!([member(&t.f.alice, "admin")]),
        json!({"user_id": t.f.alice}),
    ] {
        assert_eq!(file(&t, &fx, members.clone(), json!([])).err(), Some(SharedError::MembersInvalid), "{members}");
    }
    assert_eq!(file(&t, &fx, json!([]), json!([])).err(), Some(SharedError::NoMembers));
    let open: i64 = Connection::open(t.home().join(APPLY_DB)).unwrap()
        .query_row("SELECT count(*) FROM employee_restores", [], |r| r.get(0)).unwrap();
    assert_eq!(open, 0, "no request filed for a refused selection");
    let filed = file(&t, &fx, json!([member(&t.f.alice, "operator"), member(&t.f.bob, "owner")]), json!([])).unwrap();
    let mut ids: Vec<&str> = vec![t.f.alice.as_str(), t.f.bob.as_str()];
    ids.sort_unstable();
    assert_eq!(filed.members.iter().map(|m| m.user_id.as_str()).collect::<Vec<_>>(), ids, "canonical order");
    // The approval call re-validates against the snapshot: a member removed
    // from it (records disagree) fails the request.
    edit_snapshot(t.home(), |snapshot| snapshot["members"] = json!([]));
    assert_eq!(approve_as(&t, &fx, &filed.request.request_id, "admin-2").err(), Some(SharedError::MembersChanged));
}

/// A selected member who is no longer active is dropped and reported, never
/// bound; the options view shows them as inactive.
#[test]
fn shared_restore_inactive_member_dropped() {
    let t = team();
    let fx = fakes_for(&t);
    let home = t.home();
    sql(home, USERS_DB, "UPDATE users SET status='offboarded' WHERE id=?1", &[&t.carol]);
    let options = options(home, &offered(), &hex::encode(TEAM_EMP.0), 0, now()).unwrap();
    let carol = options["members"].as_array().unwrap().iter().find(|m| m["user_id"] == json!(t.carol)).unwrap().clone();
    assert_eq!((carol["active"].clone(), carol["access_level"].clone()), (json!(false), json!("viewer")));
    // Only inactive members selected: nothing to bind.
    assert_eq!(file(&t, &fx, json!([member(&t.carol, "viewer")]), json!([])).err(), Some(SharedError::NoMembers));
    let filed = file(&t, &fx, json!([member(&t.f.alice, "operator"), member(&t.carol, "viewer")]), json!([])).unwrap();
    assert_eq!(filed.members, vec![Selected { user_id: t.f.alice.clone(), access_level: "operator" }]);
    assert_eq!(filed.dropped, vec![json!({"user_id": t.carol, "access_level": "viewer", "reason": "inactive"})]);
    assert_eq!(filed.request.member_selection, Some(json!([member(&t.f.alice, "operator")])), "digest covers the kept members only");
    approve_as(&t, &fx, &filed.request.request_id, "admin-2").unwrap();
    assert_eq!(bound(home, &t.f.alice).as_deref(), Some("operator"));
    assert_eq!(bound(home, &t.carol), None);
}

/// Nothing is re-bound automatically: snapshot members not selected stay
/// unbound; the restored employee is the same id at incarnation + 1.
#[test]
fn shared_restore_no_auto_rebind() {
    let t = team();
    let fx = fakes_for(&t);
    let home = t.home();
    let filed = file(&t, &fx, json!([member(&t.f.alice, "viewer")]), json!([])).unwrap();
    let restored = approve_as(&t, &fx, &filed.request.request_id, "admin-2").unwrap();
    assert_eq!((restored.incarnation, restored.self_approved), (1, false));
    assert_eq!(bound(home, &t.f.alice).as_deref(), Some("viewer"));
    assert_eq!(bound(home, &t.f.bob), None, "the old owner is not re-bound");
    assert_eq!(bound(home, &t.carol), None);
    let lifecycle = read_lifecycle_with(home, TEAM, &Root(vec![t.root.clone()])).unwrap();
    assert_eq!((lifecycle.employee, lifecycle.incarnation), (TEAM_EMP, 1));
    // The decide reply lists exactly the re-bound members; work stays paused.
    let mut reply = json!({});
    after_restore(home, &stored(home, &filed.request.request_id), restored.incarnation, &mut reply, now());
    assert_eq!(reply["members"], json!([member(&t.f.alice, "viewer")]));
    assert_eq!((reply["work_paused"].clone(), reply["members_notified"].clone()), (json!(true), json!(1)));
}

/// Tenant routes are re-approved by the ticked list only: a ticked route
/// that moved refuses `ROUTES_CHANGED`; after restore an unticked route is
/// refused at use and the ticked one allowed for the new incarnation.
#[test]
fn shared_restore_tenant_routes_reapproved_not_reactivated() {
    let t = team();
    let fx = fakes_for(&t);
    let home = t.home();
    let members = json!([member(&t.f.alice, "operator")]);
    // Only offered routes, at their current generation, once.
    assert_eq!(file(&t, &fx, members.clone(), json!([tick("acct-a", 2)])).err(), Some(SharedError::RouteNotOffered));
    assert_eq!(file(&t, &fx, members.clone(), json!([tick("gone", 1)])).err(), Some(SharedError::RouteNotOffered));
    assert_eq!(file(&t, &fx, members.clone(), json!([tick("acct-a", 1), tick("acct-a", 1)])).err(), Some(SharedError::Invalid));
    // Re-generated between filing and approval: the request must be filed again.
    let first = file(&t, &fx, members.clone(), json!([tick("acct-a", 1)])).unwrap();
    *fx.routes.0.borrow_mut() = vec![("acct-a".into(), 2), ("acct-b".into(), 1)];
    assert_eq!(approve_as(&t, &fx, &first.request.request_id, "admin-2").err(), Some(SharedError::Restore(Refusal::RoutesChanged)));
    assert_eq!(stored(home, &first.request.request_id).state, "failed");
    // An unticked route changing does not matter; the ticked one is exact.
    *fx.routes.0.borrow_mut() = vec![("acct-a".into(), 1), ("acct-b".into(), 1)];
    let filed = file(&t, &fx, members, json!([tick("acct-a", 1)])).unwrap();
    *fx.routes.0.borrow_mut() = vec![("acct-a".into(), 1), ("acct-b".into(), 7)];
    let restored = approve_as(&t, &fx, &filed.request.request_id, "admin-2").unwrap();
    let binding = shared_binding(&t);
    assert_eq!(tenant_route_gate(home, TEAM_EMP, restored.incarnation, &binding, "acct-a", ConnectorKind::SharedTenant), Ok(()));
    assert_eq!(tenant_route_gate(home, TEAM_EMP, restored.incarnation, &binding, "acct-b", ConnectorKind::SharedHome),
        Err(VerificationError::Revoked), "not ticked: not re-approved");
    let mut reply = json!({});
    after_restore(home, &stored(home, &filed.request.request_id), restored.incarnation, &mut reply, now());
    assert_eq!(reply["routes_reapproved"], json!([{"account": "acct-a", "generation": 1, "provider": "google", "kind": "shared_tenant"}]));
    // Fail closed: a missing selection record refuses every tenant route.
    sql(home, APPLY_DB, "DELETE FROM shared_restore_selections", &[]);
    assert_eq!(tenant_route_gate(home, TEAM_EMP, restored.incarnation, &binding, "acct-a", ConnectorKind::SharedTenant),
        Err(VerificationError::Revoked));
    sql(home, APPLY_DB, "DROP TABLE shared_restore_selections", &[]);
    assert_eq!(tenant_route_gate(home, TEAM_EMP, restored.incarnation, &binding, "acct-a", ConnectorKind::SharedTenant),
        Err(VerificationError::Unavailable));
}

/// Per-employee detail routes are never offered or ticked (they were
/// revoked at tombstone and need `isolation.connector_provision` again);
/// the production source lists only active tenant-wide routes of the tenant.
#[test]
fn shared_restore_detail_routes_need_provisioning() {
    let t = team();
    let fx = fakes_for(&t);
    let home = t.home();
    // `detail-x` is in the engine's catalog list but not tickable.
    assert_eq!(file(&t, &fx, json!([member(&t.f.alice, "operator")]), json!([tick("detail-x", 1)])).err(),
        Some(SharedError::RouteNotOffered));
    let catalog = ConnectorCatalog::open(home).unwrap();
    let binding = shared_binding(&t);
    let tenant_route = catalog.register("tenant", "google", ConnectorKind::SharedTenant, &binding).unwrap();
    let home_route = catalog.register("home", "github", ConnectorKind::SharedHome, &binding).unwrap();
    catalog.register("db", "database", ConnectorKind::SharedTenant, &binding).unwrap();
    let revoked = catalog.register("old", "google", ConnectorKind::SharedTenant, &binding).unwrap();
    catalog.revoke(&revoked).unwrap();
    catalog.register("other", "google", ConnectorKind::SharedTenant,
        &EmployeeBinding::Shared(Text::new("other-tenant".into()).unwrap())).unwrap();
    catalog.register("alice", "google", ConnectorKind::Personal, &EmployeeBinding::Personal(Text::new(t.f.alice.clone()).unwrap()))
        .unwrap();
    let listed = CatalogTenantRoutes(home).tickable(&t.f.tenant).unwrap();
    let mut expected = vec![tenant_route.as_str().to_owned(), home_route.as_str().to_owned()];
    expected.sort();
    assert_eq!(listed.iter().map(|route| route.account.clone()).collect::<Vec<_>>(), expected,
        "no detail, revoked, other-tenant or Personal route");
    // After restore the reply says detail routes need fresh provisioning.
    let filed = file(&t, &fx, json!([member(&t.f.alice, "operator")]), json!([])).unwrap();
    let restored = approve_as(&t, &fx, &filed.request.request_id, "admin-2").unwrap();
    let mut reply = json!({});
    after_restore(home, &stored(home, &filed.request.request_id), restored.incarnation, &mut reply, now());
    assert_eq!((reply["routes_need_provisioning"].clone(), reply["routes_reapproved"].clone()), (json!(true), json!([])));
}

/// R1/S2: the filing Admin cannot approve while another qualifying Admin
/// exists (the request stays open); another Admin can. With no other
/// qualifying Admin the requester self-approves (§2 SA keeps that path for
/// Shared), recorded `self_approved`.
#[test]
fn shared_restore_second_admin_required() {
    let t = team();
    let mut fx = fakes_for(&t);
    fx.rule = Box::new(TwoAdmins);
    let home = t.home();
    let filed = file(&t, &fx, json!([member(&t.f.alice, "operator")]), json!([])).unwrap();
    assert_eq!(filed.request.requester, t.f.bob);
    assert_eq!(approve_as(&t, &fx, &filed.request.request_id, &t.f.bob).err(),
        Some(SharedError::Restore(Refusal::Approval("SECOND_ADMIN_REQUIRED"))));
    assert_eq!(stored(home, &filed.request.request_id).state, "requested");
    assert_eq!(bound(home, &t.f.alice), None);
    let restored = approve_as(&t, &fx, &filed.request.request_id, "admin-2").unwrap();
    assert!(!restored.self_approved);
    let done = stored(home, &filed.request.request_id);
    assert_eq!((done.state.as_str(), done.approver.as_deref(), done.self_approved), ("done", Some("admin-2"), Some(false)));
    // Sole Admin: self-approval with the audit row.
    let sole = team();
    let mut fx = fakes_for(&sole);
    fx.rule = Box::new(AllowSelf);
    let filed = file(&sole, &fx, json!([member(&sole.f.alice, "operator")]), json!([])).unwrap();
    let restored = approve_as(&sole, &fx, &filed.request.request_id, &sole.f.bob).unwrap();
    assert!(restored.self_approved);
    assert_eq!(stored(sole.home(), &filed.request.request_id).self_approved, Some(true));
}

/// S11: a level above the snapshot level refuses at filing; a stored
/// selection raised afterwards (even with a recomputed digest) fails the
/// request at approval and binds nothing. Lowering is allowed.
#[test]
fn shared_restore_cannot_raise_access_level() {
    let t = team();
    let fx = fakes_for(&t);
    let home = t.home();
    for members in [json!([member(&t.f.alice, "owner")]), json!([member(&t.carol, "operator")])] {
        assert_eq!(file(&t, &fx, members.clone(), json!([])).err(), Some(SharedError::LevelRaised), "{members}");
    }
    let filed = file(&t, &fx, json!([member(&t.f.alice, "viewer")]), json!([])).unwrap();
    let mut forged = stored(home, &filed.request.request_id);
    forged.member_selection = Some(json!([member(&t.f.alice, "owner")]));
    let sealed = engine::read_row(&Connection::open(home.join(APPLY_DB)).unwrap(), TEAM_EMP, 0).unwrap().unwrap();
    let digest = engine::request_digest(&forged, sealed.digest.as_deref().unwrap());
    sql(home, APPLY_DB, "UPDATE employee_restores SET member_selection=?1, digest=?2 WHERE request_id=?3",
        &[&forged.member_selection.as_ref().unwrap().to_string(), &digest, &forged.request_id]);
    assert_eq!(approve_as(&t, &fx, &filed.request.request_id, "admin-2").err(), Some(SharedError::MembersChanged));
    assert_eq!(bound(home, &t.f.alice), None);
    // Lower than the snapshot: bob owner → viewer.
    let filed = file(&t, &fx, json!([member(&t.f.bob, "viewer")]), json!([])).unwrap();
    approve_as(&t, &fx, &filed.request.request_id, "admin-2").unwrap();
    assert_eq!(bound(home, &t.f.bob).as_deref(), Some("viewer"));
}

/// M5/S17: members re-checked at approval — a selected member deactivated
/// after filing fails the request `MEMBERS_CHANGED`; a selection edited
/// without its digest fails the engine's digest check; nothing is bound.
#[test]
fn shared_restore_members_changed_refused() {
    let t = team();
    let fx = fakes_for(&t);
    let home = t.home();
    let filed = file(&t, &fx, json!([member(&t.f.alice, "operator"), member(&t.carol, "viewer")]), json!([])).unwrap();
    sql(home, USERS_DB, "UPDATE users SET status='suspended' WHERE id=?1", &[&t.carol]);
    assert_eq!(approve_as(&t, &fx, &filed.request.request_id, "admin-2").err(), Some(SharedError::MembersChanged));
    let failed = stored(home, &filed.request.request_id);
    assert_eq!((failed.state.as_str(), failed.failure.as_deref()), ("failed", Some("MEMBERS_CHANGED")));
    assert_eq!((bound(home, &t.f.alice), bound(home, &t.carol)), (None, None));
    // Edited selection, digest left alone.
    login(home, &t.carol);
    let filed = file(&t, &fx, json!([member(&t.f.alice, "operator")]), json!([])).unwrap();
    let widened = json!([member(&t.f.alice, "operator"), member(&t.carol, "viewer")]);
    let mut widened_sorted: Vec<Value> = widened.as_array().unwrap().clone();
    widened_sorted.sort_by(|a, b| a["user_id"].as_str().cmp(&b["user_id"].as_str()));
    sql(home, APPLY_DB, "UPDATE employee_restores SET member_selection=?1 WHERE request_id=?2",
        &[&Value::Array(widened_sorted).to_string(), &filed.request.request_id]);
    assert_eq!(approve_as(&t, &fx, &filed.request.request_id, "admin-2").err(), Some(SharedError::Restore(Refusal::Inconsistent)));
    assert_eq!((bound(home, &t.f.alice), bound(home, &t.carol)), (None, None));
    assert_eq!(stored(home, &filed.request.request_id).state, "failed");
}
