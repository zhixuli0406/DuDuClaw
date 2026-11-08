//! B2 issuer tests: real authority and lifecycle stores; only root ReadBack is a fixture.
use super::*;
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_isolation::capability::{EmployeeBinding, Lane, Principal};
use crate::isolation_lifecycle::LifecycleError;
use duduclaw_isolation::bounded::Text;
use duduclaw_migration::allocation::{self, Receipt};
use duduclaw_migration::confirmation::{ApprovalSession, ConfirmationError, ConfirmationStore, ProposalApplier, ProposalValidator};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalField,
    ProposalOperationId, ProposalValue};
use duduclaw_migration::ids::RequestId;
use rusqlite::Connection;

const LABEL: &str = "alice-personal";
const USERS_DB: &str = "users.db";

struct Accept;
impl ProposalValidator for Accept {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}
impl ProposalApplier for Accept {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}

/// Root allocator fixture: answers ReadBack with the receipt for the item.
struct Root(Vec<Receipt>);
impl lifecycle::RootAllocation for Root {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError> {
        self.0.iter().find(|r| r.binding == item.binding).cloned().ok_or(LifecycleError::Unavailable)
    }
}

struct Fixture { dir: tempfile::TempDir, users: UserDb, tenant: String, alice: String, bob: String }
impl Fixture {
    fn home(&self) -> &Path { self.dir.path() }
    fn generation(&self) -> u64 { self.users.isolation_authority_snapshot(None, LABEL).unwrap().acl_generation }
    fn human(&self, user: &str) -> Principal { Principal::Human { user_id: text(user), generation: self.generation() } }
    fn background(&self) -> Principal { Principal::Background { tenant_id: text(&self.tenant), generation: self.generation() } }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agents").join(LABEL).join("wiki")).unwrap();
    let users = UserDb::new(&dir.path().join(USERS_DB)).unwrap();
    let alice = users.create_user("alice@example.test", "Alice", "password1", UserRole::Employee).unwrap().id;
    let bob = users.create_user("bob@example.test", "Bob", "password1", UserRole::Admin).unwrap().id;
    login(dir.path(), &alice);
    login(dir.path(), &bob);
    std::fs::create_dir_all(dir.path().join("migration-authority")).unwrap();
    std::fs::File::create(dir.path().join("migration-authority/apply.db")).unwrap();
    ConfirmationStore::from_connection(Connection::open(dir.path().join("migration-confirmations.db")).unwrap()).unwrap();
    let tenant = users.isolation_authority_snapshot(None, LABEL).unwrap().tenant_id;
    Fixture { dir, users, tenant, alice, bob }
}

fn sql(home: &Path, statement: &str, values: &[&dyn rusqlite::ToSql]) {
    Connection::open(home.join(USERS_DB)).unwrap().execute(statement, values).unwrap();
}

fn login(home: &Path, id: &str) {
    sql(home, "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-08T00:00:00Z' WHERE id=?1", &[&id]);
}

fn text(value: &str) -> Text<128> { Text::new(value.into()).unwrap() }

/// Stage, approve and apply (with an accepting applier) one Employee proposal.
fn applied(home: &Path, id: u8, label: &str, extra: &[(&str, &str)]) -> ConfirmationItem {
    let mut store = ConfirmationStore::from_connection(Connection::open(home.join("migration-confirmations.db")).unwrap()).unwrap();
    let fields = [("employee_id", label), ("disk_bytes", "67108864")].iter().chain(extra)
        .map(|(name, value)| ProposalField { name: (*name).into(), value: ProposalValue::Text { value: (*value).into() } })
        .collect();
    let operation = ProposalOperationId { proposal_id: RequestId([id; 16]), revision: 1 };
    let staged = store.stage_proposal(operation, ConfirmationCategory::Employee, fields, &Accept).unwrap();
    let session = ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId([3; 16]));
    store.approve(&[staged.binding], &session, [2; 32], 100, &Accept).unwrap();
    for _ in 0..3 { store.apply_next(&staged.binding, &mut Accept).unwrap(); }
    let item = store.list().unwrap().into_iter().find(|i| i.binding == staged.binding).unwrap();
    assert_eq!(item.status, ConfirmationStatus::Applied);
    item
}

/// The receipt the root would return, saved through the real apply path.
fn confirm(home: &Path, item: &ConfirmationItem, employee: u8) -> Receipt {
    let spec = allocation::specification(item).unwrap();
    let receipt = Receipt { binding: item.binding, label: spec.label, employee: EmployeeId([employee; 16]),
        uid: 70_000 + u32::from(employee), bytes: spec.bytes, inodes: spec.inodes, disabled: false,
        principal_binding: spec.binding };
    crate::handlers::save_confirmed_receipt(home, item, &receipt).unwrap();
    receipt
}

/// A confirmed Personal employee whose owner is bound (as A4 will do).
fn personal_employee(f: &Fixture, id: u8, owner: &str) -> Receipt {
    let item = applied(f.home(), id, LABEL, &[("binding_kind", "personal"), ("owner_id", owner)]);
    let receipt = confirm(f.home(), &item, id);
    f.users.bind_agent(owner, LABEL, AccessLevel::Owner).unwrap();
    receipt
}

fn turn() -> super::super::HostTurn {
    super::super::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
        turn: "issuer-turn".into(), approval_session: None }, egress_turn: None,
        metadata: None, principal_context: None, background: None }
}

fn issue(f: &Fixture, root: &dyn RootAllocation, user: &str)
    -> Result<super::super::RuntimeAuthorization, VerificationError> {
    let lifecycle = lifecycle::read_lifecycle_with(f.home(), LABEL, root).map_err(lifecycle_refusal)?;
    issue_pinned(f, root, user, &lifecycle)
}

fn issue_pinned(f: &Fixture, root: &dyn RootAllocation, user: &str, lifecycle: &EmployeeLifecycle)
    -> Result<super::super::RuntimeAuthorization, VerificationError> {
    let current = f.users.isolation_authority_snapshot(Some(user), LABEL).map_err(|_| VerificationError::Revoked)?;
    let epoch = current.human.as_ref().unwrap().credential_epoch;
    human_authorization_with(f.home().to_owned(), LABEL.into(), lifecycle.employee, user.into(), epoch,
        Lane::Standard, &current, duduclaw_isolation::bounded::List::new(vec![text("tasks_update")]).unwrap(),
        turn(), None, lifecycle, root, &SelectorSources::default(), None)
}

#[test]
fn issuer_personal_owner_gets_personal_binding() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let auth = issue(&f, &Root(vec![receipt.clone()]), &f.alice).unwrap();
    assert_eq!(auth.grant.binding, EmployeeBinding::Personal(text(&f.alice)));
    assert_eq!(auth.grant.principal, f.human(&f.alice));
    assert_eq!(auth.authenticated_epoch, Some(f.users.isolation_authority_snapshot(Some(&f.alice), LABEL)
        .unwrap().human.unwrap().credential_epoch));
    assert_eq!(auth.grant.tools.as_slice(), &[text("tasks_update")]);
    assert!(auth.grant.resources.as_slice().contains(
        &duduclaw_isolation::capability::ResourceGrant::Employee { id: receipt.employee }));
}

#[test]
fn issuer_personal_non_owner_revoked() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt]);
    // Bob's Admin role can assign any employee but cannot impersonate its owner.
    assert_eq!(issue(&f, &root, &f.bob).err(), Some(VerificationError::Revoked));
    f.users.unbind_agent(&f.alice, LABEL).unwrap();
    assert_eq!(issue(&f, &root, &f.alice).err(), Some(VerificationError::Revoked));
}

#[test]
fn issuer_personal_background_refused() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt.clone()]);
    let lifecycle = lifecycle::read_lifecycle_with(f.home(), LABEL, &root).unwrap();
    let current = f.users.isolation_authority_snapshot(Some(&f.alice), LABEL).unwrap();
    let epoch = current.human.as_ref().unwrap().credential_epoch;
    let record: crate::isolation_background::RecordAuthority = serde_json::from_value(serde_json::json!({
        "home": f.home(), "kind": "task", "id": "task-a", "agent": LABEL,
        "employee": receipt.employee, "owner": f.alice, "epoch": epoch,
        "acl_generation": current.acl_generation, "policy_epoch": current.policy_epoch,
        "policy": "policy", "material": "material", "approval": null, "expires_at": u64::MAX,
        "lane": Lane::Standard, "caller": null, "parent_run": null, "parent_agent": null,
        "parent_policy": null, "parent": null, "detached_bus": false,
    })).unwrap();
    // Exercise the exact gate both issuer entry points call before resource I/O.
    // A record claiming Human(owner) is refused as well as Principal::Background
    // (H13b admits only the same-owner delegation shape, see `same_owner`).
    assert_eq!(issuance_facts(LABEL, &lifecycle, &f.background(), &current, None), Err(VerificationError::Revoked));
    assert_eq!(issuance_facts(LABEL, &lifecycle, &f.human(&f.alice), &current, Some(&record)), Err(VerificationError::Revoked));
    assert_eq!(issuance_facts(LABEL, &lifecycle, &f.human(&f.alice), &current, None), Ok(()));
    // A record-backed HostTurn reaches the constructor with a Human snapshot;
    // that representation must not bypass the record gate.
    let mut record_turn = turn();
    record_turn.background = Some(record);
    assert_eq!(human_authorization_with(f.home().to_owned(), LABEL.into(), receipt.employee,
        f.alice.clone(), epoch, Lane::Standard, &current, duduclaw_isolation::bounded::List::new(vec![]).unwrap(),
        record_turn, None, &lifecycle, &root, &SelectorSources::default(), None).err(), Some(VerificationError::Revoked));
}

/// Deterministically change durable state at the post-resource root readback.
struct DuringIssue<'a> {
    root: &'a Root,
    reads: std::cell::Cell<usize>,
    change: Box<dyn Fn() + 'a>,
}
impl RootAllocation for DuringIssue<'_> {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() == 2 { (self.change)(); }
        self.root.read_back(item)
    }
}

#[test]
fn issuer_lifecycle_change_during_issue_refused() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt.clone()]);
    let changing = DuringIssue { root: &root, reads: std::cell::Cell::new(0), change: Box::new(|| {
        lifecycle::tombstone_personal_employee(f.home(), LABEL, receipt.employee).unwrap();
    }) };
    assert_eq!(issue(&f, &changing, &f.alice).err(), Some(VerificationError::Revoked));
    assert_eq!(changing.reads.get(), 2);

    // Pin before catalog I/O, then replace the label with a new employee for
    // the same owner. A fresh ACL snapshot must not hide the replacement.
    let f = fixture();
    let first = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![first.clone()]);
    let pinned = lifecycle::read_lifecycle_with(f.home(), LABEL, &root).unwrap();
    lifecycle::tombstone_personal_employee(f.home(), LABEL, first.employee).unwrap();
    let second = personal_employee(&f, 2, &f.alice);
    assert_eq!(issue_pinned(&f, &Root(vec![first, second]), &f.alice, &pinned).err(), Some(VerificationError::Revoked));
}

#[test]
fn issuer_shared_unchanged() {
    for legacy in [false, true] {
        let f = fixture();
        let fields = if legacy { vec![] } else { vec![("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())] };
        let item = applied(f.home(), 1, LABEL, &fields);
        let receipt = confirm(f.home(), &item, 1);
        let root = Root(vec![receipt.clone()]);
        let auth = issue(&f, &root, &f.bob).unwrap();
        assert_eq!(auth.grant.binding, EmployeeBinding::Shared(text(&f.tenant)));
        assert_eq!(auth.grant.principal, f.human(&f.bob));
        assert_eq!(auth.grant.lane, Lane::Standard);
        assert_eq!(auth.grant.expires_at - auth.grant.not_before, 30 * 60);
        use duduclaw_isolation::capability::ResourceGrant;
        let resources = auth.grant.resources.as_slice();
        assert!(resources.contains(&ResourceGrant::EmployeeScope { id: receipt.employee }));
        assert_eq!(resources.len(), 9);
        assert!(resources.iter().all(|grant| matches!(grant,
            ResourceGrant::Employee { .. } | ResourceGrant::EmployeeScope { .. } | ResourceGrant::Confined { .. })));
        let lifecycle = lifecycle::read_lifecycle_with(f.home(), LABEL, &root).unwrap();
        let current = f.users.isolation_authority_snapshot(Some(&f.bob), LABEL).unwrap();
        let record: crate::isolation_background::RecordAuthority = serde_json::from_value(serde_json::json!({
            "home": f.home(), "kind": "task", "id": "task-a", "agent": LABEL, "employee": receipt.employee,
            "owner": f.bob, "epoch": 1, "acl_generation": 1, "policy_epoch": 1, "policy": "policy",
            "material": "material", "approval": null, "expires_at": u64::MAX, "lane": Lane::Standard,
            "caller": null, "parent_run": null, "parent_agent": null, "parent_policy": null, "parent": null,
            "detached_bus": false })).unwrap();
        assert_eq!(issuance_facts(LABEL, &lifecycle, &f.human(&f.bob), &current, Some(&record)), Ok(()));
    }
}

#[test]
fn issuer_lifecycle_errors_fail_closed() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    assert_eq!(issue(&f, &Root(vec![]), &f.alice).err(), Some(VerificationError::Unavailable));
    let mut stale = receipt.clone();
    stale.uid += 1;
    assert_eq!(issue(&f, &Root(vec![stale]), &f.alice).err(), Some(VerificationError::Revoked));
    sql(f.home(), "UPDATE users SET status='suspended' WHERE id=?1", &[&f.alice]);
    let root = Root(vec![receipt.clone()]);
    assert_eq!(issue(&f, &root, &f.alice).err(), Some(VerificationError::Revoked));
    login(f.home(), &f.alice);
    lifecycle::tombstone_personal_employee(f.home(), LABEL, receipt.employee).unwrap();
    assert_eq!(issue(&f, &root, &f.alice).err(), Some(VerificationError::Revoked));
}

#[test]
fn issuer_personal_membership_change_during_issue_refused() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt]);
    let changing = DuringIssue { root: &root, reads: std::cell::Cell::new(0), change: Box::new(|| {
        // Bypass A3 without bumping the ACL epoch: personal_facts must still
        // detect the changed member count on the final users.db read.
        sql(f.home(), "DROP TRIGGER isolation_personal_binding_insert", &[]);
        sql(f.home(), "INSERT INTO user_agent_bindings(user_id,agent_name,access_level,bound_at) VALUES(?1,?2,'viewer','2026-11-08T00:00:00Z')",
            &[&f.bob, &LABEL]);
    }) };
    assert_eq!(issue(&f, &changing, &f.alice).err(), Some(VerificationError::Revoked));
    assert_eq!(changing.reads.get(), 2);
}

