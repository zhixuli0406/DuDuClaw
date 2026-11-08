//! A2 lifecycle read model, Employee proposal principal checks, Personal
//! mirror apply path and the offboarding handover refusal. The root
//! allocator is a fixture; everything else is the real stores.
use super::*;
use duduclaw_auth::members::{MemberBinding, NewMember};
use duduclaw_auth::{AccessLevel, OrgError, UserDb, UserRole, UserStatus};
use duduclaw_migration::confirmation::{ApprovalSession, ConfirmationError, ConfirmationStore, ProposalApplier, ProposalValidator};
use duduclaw_migration::contract::{ProposalField, ProposalOperationId, ProposalValue};
use duduclaw_migration::ids::RequestId;
use LifecycleError::*;

const LABEL: &str = "alice-personal";

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
impl RootAllocation for Root {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError> {
        self.0.iter().find(|r| r.binding == item.binding).cloned().ok_or(Unavailable)
    }
}

struct Fixture { dir: tempfile::TempDir, users: UserDb, tenant: String, alice: String, bob: String }
impl Fixture {
    fn home(&self) -> &Path { self.dir.path() }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let users = UserDb::new(&dir.path().join(USERS_DB)).unwrap();
    let alice = users.create_user("alice@example.test", "Alice", "password1", UserRole::Employee).unwrap().id;
    let bob = users.create_user("bob@example.test", "Bob", "password1", UserRole::Admin).unwrap().id;
    login(dir.path(), &alice);
    login(dir.path(), &bob);
    std::fs::create_dir_all(dir.path().join("migration-authority")).unwrap();
    std::fs::File::create(dir.path().join(APPLY_DB)).unwrap();
    ConfirmationStore::from_connection(Connection::open(dir.path().join(CONFIRMATIONS_DB)).unwrap()).unwrap();
    let tenant = users.isolation_authority_snapshot(None, LABEL).unwrap().tenant_id;
    Fixture { dir, users, tenant, alice, bob }
}

fn sql(home: &Path, db: &str, statement: &str, values: &[&dyn rusqlite::ToSql]) {
    Connection::open(home.join(db)).unwrap().execute(statement, values).unwrap();
}

fn login(home: &Path, id: &str) {
    sql(home, USERS_DB, "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-08T00:00:00Z' WHERE id=?1", &[&id]);
}

fn text(value: &str) -> Text<128> { Text::new(value.into()).unwrap() }

/// Stage, approve and apply (with an accepting applier) one Employee proposal.
fn applied(home: &Path, id: u8, label: &str, extra: &[(&str, &str)]) -> ConfirmationItem {
    let mut store = ConfirmationStore::from_connection(Connection::open(home.join(CONFIRMATIONS_DB)).unwrap()).unwrap();
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

fn personal(owner: &str) -> [(&str, &str); 2] { [("binding_kind", "personal"), ("owner_id", owner)] }

/// The receipt the root would return: principal copied from the proposal.
fn receipt(item: &ConfirmationItem, employee: u8, disabled: bool) -> Receipt {
    let spec = allocation::specification(item).unwrap();
    Receipt { binding: item.binding, label: spec.label, employee: EmployeeId([employee; 16]), uid: 70_000 + u32::from(employee),
        bytes: spec.bytes, inodes: spec.inodes, disabled, principal_binding: spec.binding }
}

fn save(home: &Path, item: &ConfirmationItem, receipt: &Receipt) -> Result<(), ConfirmationError> {
    crate::handlers::save_confirmed_receipt(home, item, receipt)
}

fn mirror(home: &Path, label: &str) -> Option<String> {
    duduclaw_auth::db::personal_agent_owner(&Connection::open(home.join(USERS_DB)).unwrap(), label).unwrap()
}

fn store_receipt(home: &Path, label: &str, receipt: &Receipt) {
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET receipt=?1 WHERE label=?2", &[&serde_json::to_string(receipt).unwrap(), &label]);
}

#[test]
fn lifecycle_personal_owner_from_root_receipt() {
    let f = fixture();
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    let root = receipt(&item, 1, false);
    save(home, &item, &root).unwrap();
    let lifecycle = read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])).unwrap();
    assert_eq!(lifecycle.binding, EmployeeBinding::Personal(text(&f.alice)));
    assert_eq!(lifecycle.personal_owner(), Some(f.alice.as_str()));
    assert_eq!((lifecycle.employee, lifecycle.uid, lifecycle.allocation, lifecycle.label.as_str()),
        (root.employee, root.uid, item.binding, LABEL));
    // The principal comes from the root receipt and must equal the stored copy.
    for principal in [Some(EmployeeBinding::Personal(text(&f.bob))), None, Some(EmployeeBinding::Shared(text(&f.tenant)))] {
        let mut other = root.clone();
        other.principal_binding = principal;
        assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![other])), Err(Stale));
    }
    let mut other_uid = root.clone();
    other_uid.uid += 1;
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![other_uid])), Err(Stale));
    let mut dead = root.clone();
    dead.disabled = true;
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![dead])), Err(Tombstoned));
    // An owner who is no longer Active leaves the employee unusable.
    f.users.set_user_status(&f.alice, UserStatus::Suspended).unwrap();
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])), Err(Denied));
    login(home, &f.alice);
    assert!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])).is_ok());
    // A stored copy whose principal differs from the confirmed proposal is stale.
    let mut forged = root.clone();
    forged.principal_binding = Some(EmployeeBinding::Personal(text(&f.bob)));
    store_receipt(home, LABEL, &forged);
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![forged])), Err(Stale));
}

#[test]
fn lifecycle_legacy_receipt_is_shared_only() {
    let f = fixture();
    let home = f.home();
    let item = applied(home, 1, "team", &[]);
    let root = receipt(&item, 1, false);
    assert_eq!(root.principal_binding, None);
    save(home, &item, &root).unwrap();
    let roots = Root(vec![root.clone()]);
    let lifecycle = read_lifecycle_with(home, "team", &roots).unwrap();
    assert_eq!(lifecycle.binding, EmployeeBinding::Shared(text(&f.tenant)));
    assert_eq!(lifecycle.personal_owner(), None);
    // Never Personal: a mirror row for the label refuses instead of upgrading.
    assert!(record_personal_mirror(home, "team", &f.alice).unwrap());
    assert_eq!(read_lifecycle_with(home, "team", &roots), Err(Denied));
    assert!(remove_personal_mirror(home, "team", &f.alice).unwrap());
    assert!(read_lifecycle_with(home, "team", &roots).is_ok());
    // Only the pinned tenant: a missing or different pin refuses.
    let other = uuid::Uuid::new_v4().to_string();
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id=?1 WHERE label='team'", &[&other]);
    assert_eq!(read_lifecycle_with(home, "team", &roots), Err(Denied));
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id=NULL WHERE label='team'", &[]);
    assert_eq!(read_lifecycle_with(home, "team", &roots), Err(Denied));
}

#[test]
fn lifecycle_delete_recreate_new_employee_old_tombstoned() {
    let f = fixture();
    let home = f.home();
    let first = applied(home, 1, LABEL, &personal(&f.alice));
    let old_root = receipt(&first, 1, false);
    save(home, &first, &old_root).unwrap();
    f.users.bind_agent(&f.alice, LABEL, AccessLevel::Owner).unwrap();
    let old = read_lifecycle_with(home, LABEL, &Root(vec![old_root.clone()])).unwrap();

    // Admin handover: tombstone, then the mirror and every binding go.
    tombstone_personal_employee(home, LABEL, old.employee).unwrap();
    tombstone_personal_employee(home, LABEL, old.employee).unwrap();
    assert_eq!(mirror(home, LABEL), None);
    assert!(f.users.get_agent_users(LABEL).unwrap().is_empty());
    // Even though the root still reports the allocation live.
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![old_root.clone()])), Err(Tombstoned));
    assert_eq!(recheck_with(home, &old, &Root(vec![old_root.clone()])), Err(Tombstoned));
    // A later apply step for the old employee cannot revive it.
    assert!(save(home, &first, &old_root).is_err());
    assert_eq!(mirror(home, LABEL), None);

    // Recreate under the same label: a new employee identity.
    let second = applied(home, 2, LABEL, &personal(&f.alice));
    let new_root = receipt(&second, 2, false);
    save(home, &second, &new_root).unwrap();
    let roots = Root(vec![old_root.clone(), new_root.clone()]);
    let new = read_lifecycle_with(home, LABEL, &roots).unwrap();
    assert_eq!(new.employee, EmployeeId([2; 16]));
    assert_ne!(new.allocation, old.allocation);
    assert_eq!(recheck_with(home, &old, &roots), Err(Tombstoned), "old capability holders are refused");
    assert_eq!(recheck_with(home, &new, &roots), Ok(new.clone()));
    // Tombstoning by the old identity never touches the new employee.
    assert_eq!(tombstone_personal_employee(home, LABEL, old.employee), Err(Stale));
    assert_eq!(mirror(home, LABEL).as_deref(), Some(f.alice.as_str()));
    assert!(read_lifecycle_with(home, LABEL, &roots).is_ok());
}

#[test]
fn lifecycle_owner_reassignment_is_stale() {
    let f = fixture();
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    let root = receipt(&item, 1, false);
    save(home, &item, &root).unwrap();
    let roots = Root(vec![root.clone()]);
    // users.db reassigned to another owner behind the lifecycle.
    assert!(remove_personal_mirror(home, LABEL, &f.alice).unwrap());
    assert!(record_personal_mirror(home, LABEL, &f.bob).unwrap());
    assert_eq!(read_lifecycle_with(home, LABEL, &roots), Err(Stale));
    // A missing mirror is not "Shared".
    assert!(remove_personal_mirror(home, LABEL, &f.bob).unwrap());
    assert_eq!(read_lifecycle_with(home, LABEL, &roots), Err(Stale));
    // A tombstone never removes another owner's mirror.
    assert!(record_personal_mirror(home, LABEL, &f.bob).unwrap());
    assert_eq!(tombstone_personal_employee(home, LABEL, root.employee), Err(Stale));
    assert_eq!(mirror(home, LABEL).as_deref(), Some(f.bob.as_str()));
    // Re-applying the same employee for a different owner is stale.
    let f = fixture();
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    save(home, &item, &receipt(&item, 1, false)).unwrap();
    let rebound = applied(home, 2, LABEL, &personal(&f.bob));
    assert!(matches!(save(home, &rebound, &receipt(&rebound, 1, false)), Err(ConfirmationError::Stale)));
    assert_eq!(mirror(home, LABEL).as_deref(), Some(f.alice.as_str()));
}

#[test]
fn lifecycle_unreadable_store_unavailable() {
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(read_lifecycle_with(empty.path(), LABEL, &Root(vec![])), Err(Unavailable));
    assert_eq!(validate_proposed_binding(empty.path(), LABEL, None), Err(Unavailable));

    let f = fixture();
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    let root = receipt(&item, 1, false);
    save(home, &item, &root).unwrap();
    let roots = Root(vec![root.clone()]);
    assert!(read_lifecycle_with(home, LABEL, &roots).is_ok());
    // Root allocator unreachable.
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![])), Err(Unavailable));
    // Confirmation store missing.
    let confirmations = home.join(CONFIRMATIONS_DB);
    let moved = home.join("moved.db");
    std::fs::rename(&confirmations, &moved).unwrap();
    assert_eq!(read_lifecycle_with(home, LABEL, &roots), Err(Unavailable));
    std::fs::rename(&moved, &confirmations).unwrap();
    // Corrupt stored receipt.
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET receipt='{' WHERE label=?1", &[&LABEL]);
    assert_eq!(read_lifecycle_with(home, LABEL, &roots), Err(Unavailable));
    store_receipt(home, LABEL, &root);
    assert!(read_lifecycle_with(home, LABEL, &roots).is_ok());
    // Unreadable Personal mirror: never treated as "not Personal".
    sql(home, USERS_DB, "DROP TABLE isolation_personal_agents", &[]);
    assert_eq!(read_lifecycle_with(home, LABEL, &roots), Err(Unavailable));
    assert_eq!(record_personal_mirror(home, "other", &f.alice), Err(Unavailable));
    // Not a database at all, then missing.
    // Larger than one page, so SQLite reads (and rejects) the header.
    std::fs::write(home.join(APPLY_DB), vec![b'x'; 8192]).unwrap();
    assert_eq!(read_lifecycle_with(home, LABEL, &roots), Err(Unavailable));
    std::fs::remove_file(home.join(APPLY_DB)).unwrap();
    assert_eq!(read_lifecycle_with(home, LABEL, &roots), Err(Unavailable));
    assert_eq!(tombstone_personal_employee(home, LABEL, root.employee), Err(Unavailable));
}

#[test]
fn employee_proposal_personal_owner_requires_active_logged_in_user() {
    let f = fixture();
    let home = f.home();
    let carol = f.users.create_user("carol@example.test", "Carol", "password1", UserRole::Employee).unwrap().id;
    sql(home, USERS_DB, "UPDATE users SET must_change_password=0,last_login=NULL WHERE id=?1", &[&carol]);
    let owner = |id: &str| EmployeeBinding::Personal(text(id));
    let check = |id: &str| validate_proposed_binding(home, LABEL, Some(&owner(id)));
    assert_eq!(check(&carol), Err(Denied), "never logged in");
    sql(home, USERS_DB, "UPDATE users SET last_login='' WHERE id=?1", &[&carol]);
    assert_eq!(check(&carol), Err(Denied), "empty login marker");
    login(home, &carol);
    assert_eq!(check(&carol), Ok(()));
    sql(home, USERS_DB, "UPDATE users SET must_change_password=1 WHERE id=?1", &[&carol]);
    assert_eq!(check(&carol), Err(Denied), "forced password change");
    // The apply path applies the same rule in its own transaction.
    let item = applied(home, 1, LABEL, &personal(&carol));
    assert!(save(home, &item, &receipt(&item, 1, false)).is_err());
    assert_eq!(mirror(home, LABEL), None);
    login(home, &carol);
    for status in [UserStatus::Suspended, UserStatus::Offboarded] {
        f.users.set_user_status(&carol, status).unwrap();
        assert_eq!(check(&carol), Err(Denied), "{status}");
        login(home, &carol);
    }
    assert_eq!(check("missing-user"), Err(Denied));
    assert_eq!(check(&carol), Ok(()));
    // The label must be free of other members and other recorded owners.
    f.users.bind_agent(&f.bob, LABEL, AccessLevel::Viewer).unwrap();
    assert_eq!(check(&carol), Err(Denied));
    let mut users = Connection::open(home.join(USERS_DB)).unwrap();
    let tx = users.transaction().unwrap();
    tx.execute("DELETE FROM user_agent_bindings WHERE agent_name=?1", [LABEL]).unwrap();
    tx.commit().unwrap();
    assert!(record_personal_mirror(home, LABEL, &f.alice).unwrap());
    assert_eq!(check(&carol), Err(Denied));
    assert_eq!(check(&f.alice), Ok(()));
    // Shared and legacy proposals never claim a Personal label.
    assert_eq!(validate_proposed_binding(home, LABEL, None), Err(Denied));
    assert_eq!(validate_proposed_binding(home, LABEL, Some(&EmployeeBinding::Shared(text(&f.tenant)))), Err(Denied));
}

#[test]
fn shared_tenant_must_match_pin() {
    let f = fixture();
    let home = f.home();
    let shared = |tenant: &str| EmployeeBinding::Shared(text(tenant));
    let other = uuid::Uuid::new_v4().to_string();
    assert_eq!(validate_proposed_binding(home, "team", Some(&shared(&f.tenant))), Ok(()));
    assert_eq!(validate_proposed_binding(home, "team", Some(&shared(&other))), Err(Denied));
    // Apply refuses a Shared receipt for another tenant before writing a row.
    let foreign = applied(home, 1, "team", &[("binding_kind", "shared"), ("tenant_id", other.as_str())]);
    assert!(save(home, &foreign, &receipt(&foreign, 1, false)).is_err());
    assert!(confirmed_row(home, "team").unwrap().is_none());
    let item = applied(home, 2, "team", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = receipt(&item, 2, false);
    save(home, &item, &root).unwrap();
    let roots = Root(vec![root.clone()]);
    assert_eq!(read_lifecycle_with(home, "team", &roots).unwrap().binding, shared(&f.tenant));
    // A pin that no longer matches the installation tenant refuses.
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id=?1 WHERE label='team'", &[&other]);
    assert_eq!(validate_proposed_binding(home, "team", Some(&shared(&f.tenant))), Err(Denied));
    assert_eq!(read_lifecycle_with(home, "team", &roots), Err(Denied));
    assert!(save(home, &item, &root).is_err(), "the pin is never moved");
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id=?1 WHERE label='team'", &[&f.tenant]);
    assert!(read_lifecycle_with(home, "team", &roots).is_ok());
    // A stored Shared principal for another tenant than the pin refuses.
    let mut forged = root.clone();
    forged.principal_binding = Some(shared(&other));
    store_receipt(home, "team", &forged);
    assert_eq!(read_lifecycle_with(home, "team", &Root(vec![forged])), Err(Denied));
}

#[test]
fn personal_mirror_written_on_apply() {
    let f = fixture();
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    assert_eq!(mirror(home, LABEL), None);
    // A receipt whose principal differs from the confirmed proposal is refused.
    let mut wrong = receipt(&item, 1, false);
    wrong.principal_binding = Some(EmployeeBinding::Personal(text(&f.bob)));
    assert!(matches!(save(home, &item, &wrong), Err(ConfirmationError::Stale)));
    assert_eq!(mirror(home, LABEL), None);
    // Stage, publish and read-back each save; the mirror is written once.
    for _ in 0..3 { save(home, &item, &receipt(&item, 1, false)).unwrap(); }
    assert_eq!(mirror(home, LABEL).as_deref(), Some(f.alice.as_str()));
    assert!(f.users.bind_agent(&f.bob, LABEL, AccessLevel::Viewer).is_err(), "Personal exclusivity is live");
    // Compensation: the disabled row first, then the mirror.
    save(home, &item, &receipt(&item, 1, true)).unwrap();
    assert_eq!(mirror(home, LABEL), None);
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![receipt(&item, 1, false)])), Err(Tombstoned));

    // The apply step is refused, with no confirmed row, when the mirror write fails.
    let conflict = applied(home, 2, "solo", &personal(&f.alice));
    f.users.bind_agent(&f.bob, "solo", AccessLevel::Viewer).unwrap();
    assert!(save(home, &conflict, &receipt(&conflict, 2, false)).is_err());
    assert!(confirmed_row(home, "solo").unwrap().is_none());
    assert_eq!(mirror(home, "solo"), None);
    let other = applied(home, 3, "other", &personal(&f.alice));
    assert!(record_personal_mirror(home, "other", &f.bob).unwrap());
    assert!(matches!(save(home, &other, &receipt(&other, 3, false)), Err(ConfirmationError::Stale)));
    assert!(confirmed_row(home, "other").unwrap().is_none());
    assert_eq!(mirror(home, "other").as_deref(), Some(f.bob.as_str()), "another owner's mirror is untouched");
}

#[test]
fn offboarding_personal_owner_refuses_transfer_with_message() {
    let f = fixture();
    let home = f.home();
    let item = applied(home, 1, LABEL, &personal(&f.alice));
    let root = receipt(&item, 1, false);
    save(home, &item, &root).unwrap();
    f.users.bind_agent(&f.alice, LABEL, AccessLevel::Owner).unwrap();
    f.users.bind_agent(&f.alice, "team", AccessLevel::Operator).unwrap();

    let error = f.users.offboard_user(&f.alice, Some(&f.bob), Some(&f.bob)).unwrap_err();
    assert_eq!(error, OrgError::PersonalEmployee(LABEL.into()));
    assert_eq!(error.code(), "personal_employee_handover_required");
    let message = error.to_string();
    assert!(message.contains(LABEL) && message.contains("personal employee") && message.contains("admin"), "{message}");
    // Nothing of the offboarding was committed.
    assert_eq!(f.users.get_user(&f.alice).unwrap().unwrap().status, UserStatus::Active);
    assert_eq!(f.users.check_agent_access(&f.alice, LABEL).unwrap(), Some(AccessLevel::Owner));
    assert_eq!(f.users.check_agent_access(&f.bob, "team").unwrap(), None);

    // New members cannot be bound to a Personal employee either.
    let admin = f.users.get_user(&f.bob).unwrap().unwrap();
    let member = NewMember { display_name: "New".into(), email: "new@example.test".into(), role: UserRole::Employee,
        department: None, primary_agent_id: LABEL.into(), invite: false,
        bindings: vec![MemberBinding { agent_name: LABEL.into(), access_level: AccessLevel::Operator }] };
    assert_eq!(f.users.create_member_at_epoch(&member, &admin.id, admin.credential_epoch).err(), Some("member_agent_personal"));
    assert_eq!(f.users.list_users().unwrap().len(), 2);

    // The admin handover through the lifecycle, then the transfer proceeds
    // without the Personal employee.
    tombstone_personal_employee(home, LABEL, root.employee).unwrap();
    assert_eq!(read_lifecycle_with(home, LABEL, &Root(vec![root.clone()])), Err(Tombstoned));
    let outcome = f.users.offboard_user(&f.alice, Some(&f.bob), Some(&f.bob)).unwrap();
    assert_eq!(outcome.transferred_agents, vec!["team".to_string()]);
    assert_eq!(f.users.check_agent_access(&f.bob, LABEL).unwrap(), None);
}
