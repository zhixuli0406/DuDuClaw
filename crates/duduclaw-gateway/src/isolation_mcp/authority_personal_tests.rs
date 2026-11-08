//! B1: Personal employee bindings in the production MCP authority, against
//! the real users.db / apply.db / confirmation stores and the A2 lifecycle
//! read model. Only the root allocator ReadBack is a fixture.
use super::*;
use duduclaw_auth::{AccessLevel, UserRole};
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
    fn stores<'a>(&'a self, root: &'a Root) -> Stores<'a> {
        Stores { home: self.home(), users: &self.users, root }
    }
    fn generation(&self) -> u64 { self.users.isolation_authority_snapshot(None, LABEL).unwrap().acl_generation }
    fn human(&self, user: &str) -> Principal { Principal::Human { user_id: text(user), generation: self.generation() } }
    fn background(&self) -> Principal { Principal::Background { tenant_id: text(&self.tenant), generation: self.generation() } }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
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

fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(text(owner)) }

fn identify(f: &Fixture, root: &Root, agent: &str, employee: EmployeeId, binding: &EmployeeBinding, principal: &Principal)
    -> Result<Identity, VerificationError> {
    identity_with(&f.stores(root), agent, employee, binding, principal, None)
}

/// What `prepare` stores for an identity (policy digest is not under test).
fn prepared(agent: &str, identity: Identity, binding: EmployeeBinding, principal: Principal) -> PreparedAuthority {
    PreparedAuthority { agent: agent.into(), employee: identity.lifecycle.employee, principal, binding, revision: 1,
        policy: "policy".into(), credential_epoch: identity.credential_epoch, policy_epoch: identity.policy_epoch,
        tenant_id: identity.tenant_id, lifecycle: identity.lifecycle, expires_at: u64::MAX, turn: None }
}

/// The identity part of `GatewayAuthority::recheck` followed by `check_epoch`.
fn recheck(f: &Fixture, root: &Root, prepared: &PreparedAuthority) -> Result<(), VerificationError> {
    recheck_identity(&f.stores(root), prepared)?;
    check_current(&f.users, prepared)
}

#[test]
fn personal_authority_owner_human_is_allowed() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt.clone()]);
    let binding = personal(&f.alice);
    let principal = f.human(&f.alice);
    let identity = identify(&f, &root, LABEL, receipt.employee, &binding, &principal).unwrap();
    let alice = f.users.isolation_authority_snapshot(Some(f.alice.as_str()), LABEL).unwrap().human.unwrap();
    assert_eq!(identity.credential_epoch, Some(alice.credential_epoch));
    assert_eq!(identity.lifecycle.binding, binding);
    assert_eq!(identity.lifecycle.employee, receipt.employee);
    assert_eq!(identity.tenant_id, f.tenant);
    let run = prepared(LABEL, identity, binding.clone(), principal.clone());
    assert_eq!(check_current(&f.users, &run), Ok(()));
    assert_eq!(recheck(&f, &root, &run), Ok(()));
    // Repeated rechecks keep succeeding while nothing changes.
    assert_eq!(recheck(&f, &root, &run), Ok(()));
    // An unreachable root allocator fails closed as unavailable.
    assert_eq!(recheck(&f, &Root(vec![]), &run).err(), Some(VerificationError::Unavailable));
    assert_eq!(identify(&f, &Root(vec![]), LABEL, receipt.employee, &binding, &principal).err(),
        Some(VerificationError::Unavailable));
    // The same owner with a stale ACL generation is refused.
    let stale = Principal::Human { user_id: text(&f.alice), generation: f.generation() + 1 };
    assert_eq!(identify(&f, &root, LABEL, receipt.employee, &binding, &stale).err(), Some(VerificationError::Revoked));
    // Another employee id under the label is refused.
    assert_eq!(identify(&f, &root, LABEL, EmployeeId([9; 16]), &binding, &principal).err(),
        Some(VerificationError::Revoked));
    // An owner without the direct binding (count 0) is refused (here users.db
    // already refuses the non-admin principal snapshot).
    f.users.unbind_agent(&f.alice, LABEL).unwrap();
    assert!(identify(&f, &root, LABEL, receipt.employee, &binding, &f.human(&f.alice)).is_err());
    assert_eq!(check_current(&f.users, &run), Err(VerificationError::Revoked));
}

#[test]
fn personal_authority_non_owner_refused() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt.clone()]);
    // Bob is an Admin who may assign any agent; that does not make him the owner.
    let bob = f.human(&f.bob);
    assert!(f.users.isolation_authority_snapshot(Some(f.bob.as_str()), LABEL).unwrap().human.is_some());
    assert_eq!(identify(&f, &root, LABEL, receipt.employee, &personal(&f.alice), &bob).err(),
        Some(VerificationError::Revoked));
    // Claiming another principal than the lifecycle's is a binding mismatch.
    assert_eq!(identify(&f, &root, LABEL, receipt.employee, &personal(&f.bob), &bob).err(),
        Some(VerificationError::BindingMismatch));
    for principal in [bob.clone(), f.human(&f.alice)] {
        assert_eq!(identify(&f, &root, LABEL, receipt.employee, &EmployeeBinding::Shared(text(&f.tenant)), &principal).err(),
            Some(VerificationError::BindingMismatch), "a Personal employee is never treated as Shared");
    }
    // A run association forged for bob is refused by the users.db re-read too.
    let owner = identify(&f, &root, LABEL, receipt.employee, &personal(&f.alice), &f.human(&f.alice)).unwrap();
    let mut forged = prepared(LABEL, owner, personal(&f.alice), bob);
    forged.credential_epoch = Some(f.users.isolation_authority_snapshot(Some(f.bob.as_str()), LABEL).unwrap().human.unwrap().credential_epoch);
    assert_eq!(check_current(&f.users, &forged), Err(VerificationError::Revoked));
}

#[test]
fn personal_authority_member_count_two_refused() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt.clone()]);
    let binding = personal(&f.alice);
    let identity = identify(&f, &root, LABEL, receipt.employee, &binding, &f.human(&f.alice)).unwrap();
    let run = prepared(LABEL, identity, binding.clone(), f.human(&f.alice));
    // A3 refuses a second member; simulate a store that bypassed it. The raw
    // insert leaves the ACL generation unchanged, so the count alone decides.
    assert!(f.users.bind_agent(&f.bob, LABEL, AccessLevel::Viewer).is_err());
    sql(f.home(), "DROP TRIGGER isolation_personal_binding_insert", &[]);
    sql(f.home(), "INSERT INTO user_agent_bindings(user_id,agent_name,access_level,bound_at) VALUES(?1,?2,'viewer','2026-11-08T00:00:00Z')",
        &[&f.bob, &LABEL]);
    assert_eq!(f.users.isolation_authority_snapshot(None, LABEL).unwrap().agent_member_count, 2);
    assert_eq!(f.generation(), match &run.principal { Principal::Human { generation, .. } => *generation, _ => unreachable!() });
    let current = f.human(&f.alice);
    assert_eq!(identify(&f, &root, LABEL, receipt.employee, &binding, &current).err(), Some(VerificationError::Revoked));
    assert_eq!(check_current(&f.users, &run), Err(VerificationError::Revoked));
    assert_eq!(recheck(&f, &root, &run), Err(VerificationError::Revoked));
    // Back to exactly the owner: allowed again.
    f.users.unbind_agent(&f.bob, LABEL).unwrap();
    assert!(identify(&f, &root, LABEL, receipt.employee, &binding, &f.human(&f.alice)).is_ok());
}

#[test]
fn personal_authority_background_refused() {
    let f = fixture();
    let receipt = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![receipt.clone()]);
    let background = f.background();
    // Refused before any store I/O: even an unreachable root reports Revoked.
    for root in [&root, &Root(vec![])] {
        assert_eq!(identify(&f, root, LABEL, receipt.employee, &personal(&f.alice), &background).err(),
            Some(VerificationError::Revoked));
    }
    // A background principal claiming Shared for the Personal employee mismatches.
    assert_eq!(identify(&f, &root, LABEL, receipt.employee, &EmployeeBinding::Shared(text(&f.tenant)), &background).err(),
        Some(VerificationError::BindingMismatch));
    // A forged association never passes the recheck or the users.db re-read.
    let owner = identify(&f, &root, LABEL, receipt.employee, &personal(&f.alice), &f.human(&f.alice)).unwrap();
    let mut forged = prepared(LABEL, owner, personal(&f.alice), background);
    forged.credential_epoch = None;
    assert_eq!(check_current(&f.users, &forged), Err(VerificationError::Revoked));
    assert_eq!(recheck(&f, &root, &forged), Err(VerificationError::Revoked));
    assert_eq!(personal_facts(LABEL, &f.alice, &forged.principal,
        &f.users.isolation_authority_snapshot(None, LABEL).unwrap()), Err(VerificationError::Revoked));
}

#[test]
fn personal_authority_owner_change_mid_run_refused_on_recheck() {
    let f = fixture();
    let first = personal_employee(&f, 1, &f.alice);
    let root = Root(vec![first.clone()]);
    let binding = personal(&f.alice);
    let identity = identify(&f, &root, LABEL, first.employee, &binding, &f.human(&f.alice)).unwrap();
    let run = prepared(LABEL, identity, binding.clone(), f.human(&f.alice));
    assert_eq!(recheck(&f, &root, &run), Ok(()));

    // Admin handover mid-run: tombstone, then recreate the label for bob.
    lifecycle::tombstone_personal_employee(f.home(), LABEL, first.employee).unwrap();
    // users.db alone already refuses the next call (mirror and member gone).
    assert_eq!(check_current(&f.users, &run), Err(VerificationError::Revoked));
    assert_eq!(recheck(&f, &root, &run), Err(VerificationError::Revoked));
    let second = personal_employee(&f, 2, &f.bob);
    let roots = Root(vec![first.clone(), second.clone()]);
    assert_eq!(check_current(&f.users, &run), Err(VerificationError::Revoked));
    assert_eq!(recheck(&f, &roots, &run), Err(VerificationError::Revoked), "old employee is tombstoned");
    // Even with a refreshed ACL generation the old association stays refused.
    let mut refreshed = run.clone();
    refreshed.principal = f.human(&f.alice);
    assert_eq!(check_current(&f.users, &refreshed), Err(VerificationError::Revoked));
    assert_eq!(recheck(&f, &roots, &refreshed), Err(VerificationError::Revoked));
    // Alice cannot reach the new employee; bob, its owner, can.
    assert_eq!(identify(&f, &roots, LABEL, second.employee, &binding, &f.human(&f.alice)).err(),
        Some(VerificationError::BindingMismatch));
    assert!(identify(&f, &roots, LABEL, second.employee, &personal(&f.bob), &f.human(&f.alice)).is_err());
    assert!(identify(&f, &roots, LABEL, second.employee, &personal(&f.bob), &f.human(&f.bob)).is_ok());

    // Member removal mid-run refuses the next call too. The owner is an Admin,
    // whose users.db principal snapshot survives unbinding, so the B1
    // membership check itself is what refuses.
    let g = fixture();
    let receipt = personal_employee(&g, 1, &g.bob);
    let root = Root(vec![receipt.clone()]);
    let identity = identify(&g, &root, LABEL, receipt.employee, &personal(&g.bob), &g.human(&g.bob)).unwrap();
    let run = prepared(LABEL, identity, personal(&g.bob), g.human(&g.bob));
    assert_eq!(recheck(&g, &root, &run), Ok(()));
    g.users.unbind_agent(&g.bob, LABEL).unwrap();
    assert_eq!(check_current(&g.users, &run), Err(VerificationError::Revoked));
    let mut live = run.clone();
    live.principal = g.human(&g.bob);
    assert!(g.users.isolation_authority_snapshot(Some(g.bob.as_str()), LABEL).unwrap().human.is_some());
    assert_eq!(check_current(&g.users, &live), Err(VerificationError::Revoked), "member count 0");
    assert_eq!(recheck(&g, &root, &live), Err(VerificationError::Revoked));
    assert_eq!(identify(&g, &root, LABEL, receipt.employee, &personal(&g.bob), &g.human(&g.bob)).err(),
        Some(VerificationError::Revoked));
}

#[test]
fn shared_authority_unchanged() {
    let f = fixture();
    let item = applied(f.home(), 1, "team", &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let receipt = confirm(f.home(), &item, 1);
    let legacy_item = applied(f.home(), 2, "legacy", &[]);
    let legacy = confirm(f.home(), &legacy_item, 2);
    f.users.bind_agent(&f.alice, "team", AccessLevel::Operator).unwrap();
    f.users.bind_agent(&f.bob, "team", AccessLevel::Viewer).unwrap();
    let root = Root(vec![receipt.clone(), legacy.clone()]);
    let shared = EmployeeBinding::Shared(text(&f.tenant));
    // Human members (any count) and Background principals of the tenant are allowed.
    for principal in [f.human(&f.alice), f.human(&f.bob), f.background()] {
        let identity = identify(&f, &root, "team", receipt.employee, &shared, &principal).unwrap();
        assert_eq!(identity.credential_epoch.is_some(), matches!(principal, Principal::Human { .. }));
        assert_eq!(identity.tenant_id, f.tenant);
        let run = prepared("team", identity, shared.clone(), principal);
        assert_eq!(check_current(&f.users, &run), Ok(()));
        assert_eq!(recheck(&f, &root, &run), Ok(()));
    }
    // A legacy receipt is Shared(pinned tenant).
    assert!(identify(&f, &root, "legacy", legacy.employee, &shared, &f.background()).is_ok());
    // Existing refusals keep their kinds.
    let other = EmployeeBinding::Shared(text(&uuid::Uuid::new_v4().to_string()));
    assert_eq!(identify(&f, &root, "team", receipt.employee, &other, &f.human(&f.alice)).err(),
        Some(VerificationError::BindingMismatch));
    assert_eq!(identify(&f, &root, "team", receipt.employee, &personal(&f.alice), &f.human(&f.alice)).err(),
        Some(VerificationError::BindingMismatch));
    let stale = Principal::Human { user_id: text(&f.alice), generation: f.generation() + 1 };
    assert_eq!(identify(&f, &root, "team", receipt.employee, &shared, &stale).err(), Some(VerificationError::Revoked));
    let foreign = Principal::Background { tenant_id: text("other-tenant"), generation: f.generation() };
    assert_eq!(identify(&f, &root, "team", receipt.employee, &shared, &foreign).err(), Some(VerificationError::Revoked));
    assert_eq!(identify(&f, &root, "team", legacy.employee, &shared, &f.background()).err(),
        Some(VerificationError::Revoked), "wrong employee for the label");
    // A run prepared before an ACL change is refused on recheck.
    let identity = identify(&f, &root, "team", receipt.employee, &shared, &f.human(&f.alice)).unwrap();
    let run = prepared("team", identity, shared.clone(), f.human(&f.alice));
    f.users.unbind_agent(&f.bob, "team").unwrap();
    assert_eq!(check_current(&f.users, &run), Err(VerificationError::Revoked));
}
