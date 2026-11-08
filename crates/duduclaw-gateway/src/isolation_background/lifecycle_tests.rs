//! B3: background receipts record the employee binding and allocation from
//! the A2 lifecycle and are refused when it no longer matches. The root
//! allocator is a fixture; apply.db, the confirmation store and users.db are
//! the real stores (same setup as `isolation_lifecycle_tests.rs`).
use super::*;
use duduclaw_auth::{UserDb, UserRole};
use duduclaw_isolation::bounded::Text;
use duduclaw_migration::allocation::{self, Receipt};
use duduclaw_migration::confirmation::{ApprovalSession, ConfirmationError, ConfirmationStore, ProposalApplier, ProposalValidator};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationItem, ConfirmationStatus, ProposalField,
    ProposalOperationId, ProposalValue};
use duduclaw_migration::ids::RequestId;
use rusqlite::{params, Connection};

const APPLY_DB: &str = "migration-authority/apply.db";
const CONFIRMATIONS_DB: &str = "migration-confirmations.db";
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
impl RootAllocation for Root {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError> {
        self.0.iter().find(|r| r.binding == item.binding).cloned().ok_or(LifecycleError::Unavailable)
    }
}

struct Fixture { dir: tempfile::TempDir, tenant: String, alice: String }
impl Fixture {
    fn home(&self) -> &Path { self.dir.path() }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let users = UserDb::new(&dir.path().join(USERS_DB)).unwrap();
    let alice = users.create_user("alice@example.test", "Alice", "password1", UserRole::Employee).unwrap().id;
    sql(dir.path(), USERS_DB,
        "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-08T00:00:00Z' WHERE id=?1", &[&alice]);
    std::fs::create_dir_all(dir.path().join("migration-authority")).unwrap();
    std::fs::File::create(dir.path().join(APPLY_DB)).unwrap();
    ConfirmationStore::from_connection(Connection::open(dir.path().join(CONFIRMATIONS_DB)).unwrap()).unwrap();
    let tenant = users.isolation_authority_snapshot(None, "team").unwrap().tenant_id;
    Fixture { dir, tenant, alice }
}

fn sql(home: &Path, db: &str, statement: &str, values: &[&dyn rusqlite::ToSql]) {
    Connection::open(home.join(db)).unwrap().execute(statement, values).unwrap();
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

/// The receipt the root would return: principal copied from the proposal.
fn root_receipt(item: &ConfirmationItem, employee: u8, disabled: bool) -> Receipt {
    let spec = allocation::specification(item).unwrap();
    Receipt { binding: item.binding, label: spec.label, employee: EmployeeId([employee; 16]), uid: 70_000 + u32::from(employee),
        bytes: spec.bytes, inodes: spec.inodes, disabled, principal_binding: spec.binding }
}

/// A confirmed Shared employee `label` (v1.81 proposal with binding fields).
fn shared(f: &Fixture, id: u8, label: &str) -> (ConfirmationItem, Receipt) {
    let item = applied(f.home(), id, label, &[("binding_kind", "shared"), ("tenant_id", f.tenant.as_str())]);
    let root = root_receipt(&item, id, false);
    crate::handlers::save_confirmed_receipt(f.home(), &item, &root).unwrap();
    (item, root)
}

/// A cron receipt for `agent` built from exactly what capture records.
fn captured(home: &Path, agent: &str, root: &Root) -> RecordAuthority {
    let (employee, binding, allocation, incarnation) = capture_binding(home, agent, root).unwrap().expect("a Shared employee");
    let mut value = super::tests::receipt(home);
    value.agent = agent.into();
    value.employee = employee;
    value.binding = Some(binding);
    value.allocation = Some(allocation);
    value.incarnation = incarnation;
    value
}

/// F6b A3: a receipt captured for one incarnation never authorizes another
/// incarnation of the same employee id (a restore bumps it, §3.5 d).
#[test]
fn prior_incarnation_record_authority_refused() {
    let f = fixture();
    let home = f.home();
    let (_, root) = shared(&f, 1, "team");
    let roots = Root(vec![root]);
    let value = captured(home, "team", &roots);
    assert_eq!(value.incarnation, 0);
    // Incarnation 0 is omitted from the stored form (and approval payload),
    // so receipts approved before F6 keep matching byte for byte.
    let encoded = serde_json::to_value(&value).unwrap();
    assert!(encoded.get("incarnation").is_none() && encoded.get("caller_incarnation").is_none());
    assert_eq!(check_receipt_lifecycle(&value, &roots), Ok(()));
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET incarnation=1 WHERE label='team'", &[]);
    assert_eq!(check_receipt_lifecycle(&value, &roots), Err(VerificationError::Revoked));
    let fresh = captured(home, "team", &roots);
    assert_eq!((fresh.employee, fresh.incarnation), (value.employee, 1));
    assert_eq!(check_receipt_lifecycle(&fresh, &roots), Ok(()));
    assert_eq!(serde_json::to_value(&fresh).unwrap()["incarnation"], json!(1));
    store::save(&fresh).unwrap();
    assert_eq!(store::load(home, "cron", &fresh.id).unwrap(), Some(fresh.clone()));
    // A forged receipt naming the new incarnation is refused once it moves on.
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET incarnation=2 WHERE label='team'", &[]);
    assert_eq!(check_receipt_lifecycle(&fresh, &roots), Err(VerificationError::Revoked));
    // The caller (delegating source) incarnation is part of the receipt.
    let mut delegated = fresh.clone();
    delegated.caller = Some(fresh.employee);
    delegated.caller_incarnation = 2;
    assert_eq!(serde_json::to_value(&delegated).unwrap()["caller_incarnation"], json!(2));
}

#[test]
fn background_receipt_records_binding_and_allocation() {
    let f = fixture();
    let home = f.home();
    let (item, root) = shared(&f, 1, "team");
    let roots = Root(vec![root.clone()]);
    let value = captured(home, "team", &roots);
    assert_eq!(value.employee, root.employee);
    assert_eq!(value.binding, Some(EmployeeBinding::Shared(text(&f.tenant))));
    assert_eq!(value.allocation, Some(item.binding));
    assert!(!value.is_legacy());
    assert_eq!(check_receipt_lifecycle(&value, &roots), Ok(()));
    // Persisted and part of the approval payload (the serialized authority).
    store::save(&value).unwrap();
    assert_eq!(store::load(home, "cron", &value.id).unwrap(), Some(value.clone()));
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(encoded["binding"], json!({"kind": "shared", "id": f.tenant}));
    assert_eq!(encoded["allocation"], serde_json::to_value(item.binding).unwrap());
    // An allocation confirmed before v1.81 (no principal in the root
    // receipt) is Shared in the pinned tenant, and so is its record.
    let old = applied(home, 2, "legacy-team", &[]);
    let old_root = root_receipt(&old, 2, false);
    assert_eq!(old_root.principal_binding, None);
    crate::handlers::save_confirmed_receipt(home, &old, &old_root).unwrap();
    let roots = Root(vec![root, old_root]);
    let value = captured(home, "legacy-team", &roots);
    assert_eq!(value.binding, Some(EmployeeBinding::Shared(text(&f.tenant))));
    assert_eq!(value.allocation, Some(old.binding));
    assert_eq!(check_receipt_lifecycle(&value, &roots), Ok(()));
}

#[test]
fn background_recheck_refuses_binding_change() {
    let f = fixture();
    let home = f.home();
    let (_, root) = shared(&f, 1, "team");
    let roots = Root(vec![root.clone()]);
    let value = captured(home, "team", &roots);
    let current = current_lifecycle(home, "team", &roots).unwrap();
    assert_eq!(check_lifecycle(&value, &current), Ok(()));
    let other = uuid::Uuid::new_v4().to_string();
    // Every recorded identity must equal the live lifecycle.
    let mut changes = Vec::new();
    for binding in [EmployeeBinding::Shared(text(&other)), EmployeeBinding::Personal(text(&f.alice))] {
        let mut changed = current.clone(); changed.binding = binding; changes.push(changed);
    }
    let mut changed = current.clone(); changed.allocation = super::tests::allocation(9); changes.push(changed);
    let mut changed = current.clone(); changed.employee = EmployeeId([9; 16]); changes.push(changed);
    let mut changed = current.clone(); changed.label = "other".into(); changes.push(changed);
    for changed in &changes {
        assert_eq!(check_lifecycle(&value, changed), Err(VerificationError::Revoked), "{changed:?}");
    }
    // Store-backed: the receipt's binding or allocation differs from the stores.
    let mut forged = value.clone();
    forged.binding = Some(EmployeeBinding::Shared(text(&other)));
    assert_eq!(check_receipt_lifecycle(&forged, &roots), Err(VerificationError::Revoked));
    let mut forged = value.clone();
    forged.allocation = Some(super::tests::allocation(9));
    assert_eq!(check_receipt_lifecycle(&forged, &roots), Err(VerificationError::Revoked));
    // Tenant: the pin no longer matching the installation refuses, and the
    // owner snapshot's tenant must equal the recorded one.
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id=?1 WHERE label='team'", &[&other]);
    assert_eq!(check_receipt_lifecycle(&value, &roots), Err(VerificationError::Revoked));
    sql(home, APPLY_DB, "UPDATE confirmed_employees SET tenant_id=?1 WHERE label='team'", &[&f.tenant]);
    assert_eq!(check_receipt_lifecycle(&value, &roots), Ok(()));
    let binding = value.binding.clone().unwrap();
    assert_eq!(check_tenant(&binding, &f.tenant), Ok(()));
    assert_eq!(check_tenant(&binding, &other), Err(VerificationError::Revoked));
    assert_eq!(check_tenant(&EmployeeBinding::Personal(text(&f.tenant)), &f.tenant), Err(VerificationError::Revoked));
    // Root unreachable: unavailable, never a pass.
    assert_eq!(check_receipt_lifecycle(&value, &Root(vec![])), Err(VerificationError::Unavailable));
}

#[test]
fn background_recheck_refuses_tombstoned_employee() {
    let f = fixture();
    let home = f.home();
    let (item, root) = shared(&f, 1, "team");
    let value = captured(home, "team", &Root(vec![root.clone()]));
    // The root reports the allocation disabled.
    let mut dead = root.clone();
    dead.disabled = true;
    assert_eq!(check_receipt_lifecycle(&value, &Root(vec![dead.clone()])), Err(VerificationError::Revoked));
    // The confirmed row is disabled (compensation) while the root still
    // reports it live.
    crate::handlers::save_confirmed_receipt(home, &item, &dead).unwrap();
    assert_eq!(check_receipt_lifecycle(&value, &Root(vec![root.clone()])), Err(VerificationError::Revoked));
    // Recreated under the same label: a new employee and allocation. The old
    // receipt stays refused; only a new capture authorizes the new employee.
    let (second, new_root) = shared(&f, 2, "team");
    let roots = Root(vec![root, new_root.clone()]);
    assert_eq!(check_receipt_lifecycle(&value, &roots), Err(VerificationError::Revoked));
    let fresh = captured(home, "team", &roots);
    assert_eq!((fresh.employee, fresh.allocation), (new_root.employee, Some(second.binding)));
    assert_ne!(fresh.allocation, value.allocation);
    assert_eq!(check_receipt_lifecycle(&fresh, &roots), Ok(()));
}

#[test]
fn background_legacy_receipt_requires_reapproval() {
    let f = fixture();
    let home = f.home();
    let (_, root) = shared(&f, 1, "team");
    let roots = Root(vec![root]);
    let value = captured(home, "team", &roots);
    store::save(&value).unwrap();
    // A receipt written before v1.81: same fields, no binding/allocation.
    let mut old = serde_json::to_value(&value).unwrap();
    let fields = old.as_object_mut().unwrap();
    fields.remove("binding");
    fields.remove("allocation");
    sql(home, "isolation_background.db", "UPDATE authorities SET receipt=?1 WHERE record_id=?2",
        &[&old.to_string(), &value.id]);
    // Still readable, so a new request can replace it ...
    let legacy = store::load(home, "cron", &value.id).unwrap().unwrap();
    assert!(legacy.is_legacy());
    assert_eq!((legacy.binding.as_ref(), legacy.allocation), (None, None));
    // ... but it never authorizes, although the employee itself is unchanged.
    assert_eq!(check_receipt_lifecycle(&legacy, &roots), Err(VerificationError::Revoked));
    assert_eq!(recheck_receipt(&legacy, false, false, &roots), Err(VerificationError::Revoked));
    assert_eq!(recheck_receipt(&legacy, false, true, &roots), Err(VerificationError::Revoked));
    // As an ancestor of a delegation it is refused the same way.
    assert_eq!(recheck_receipt(&legacy, true, true, &roots), Err(VerificationError::Revoked));
    // It can never be written back in that form.
    assert_eq!(store::save(&legacy), Err(VerificationError::Invalid));
    assert_eq!(store::validate_current(&legacy), Err(VerificationError::Invalid));
    // Half-upgraded receipts are invalid, not legacy.
    let mut half = value.clone();
    half.allocation = None;
    assert!(!half.is_legacy());
    assert_eq!(store::validate(&half), Err(VerificationError::Invalid));
    let mut half = value.clone();
    half.binding = None;
    assert_eq!(store::validate(&half), Err(VerificationError::Invalid));
    // deny_unknown_fields still applies to every stored receipt.
    let mut unknown = old.clone();
    unknown.as_object_mut().unwrap().insert("extra".into(), json!(1));
    sql(home, "isolation_background.db", "UPDATE authorities SET receipt=?1 WHERE record_id=?2",
        &[&unknown.to_string(), &value.id]);
    assert!(store::load(home, "cron", &value.id).is_err());
    // An approval decided for the old form never matches a receipt that
    // carries the new fields.
    let conn = Connection::open(home.join("approvals.db")).unwrap();
    conn.execute_batch("CREATE TABLE approvals(id TEXT,action_kind TEXT,agent_id TEXT,status TEXT,
        payload TEXT,decided_by TEXT,invalidated_reason TEXT,created_at TEXT,decided_at TEXT,ttl_seconds INTEGER);").unwrap();
    let mut authority = old.clone();
    authority["approval"] = Value::Null;
    let payload = json!({"authority": authority, "record": {"task": "original"}}).to_string();
    conn.execute("INSERT INTO approvals VALUES ('approval-id',?1,'team','approved',?2,'dashboard:admin',NULL,
        datetime('now','-1 hour'),datetime('now','-30 minutes'),86400)", params![APPROVAL_KIND, payload]).unwrap();
    assert!(store::check_approval(&value, "approval-id").is_err());
    assert!(store::check_approval(&legacy, "approval-id").is_err());
}

#[tokio::test]
async fn background_legacy_refusal_reason_is_scoped_and_not_a_grant() {
    assert_eq!(refusal_reason(), None);
    REFUSAL.scope(Some(LEGACY_REFUSAL), RECORD.scope(Mutex::new(None), async {
        assert_eq!(refusal_reason(), Some(LEGACY_REFUSAL));
        assert!(take_record().is_err());
        assert_eq!(tokio::spawn(async { refusal_reason() }).await.unwrap(), None);
    })).await;
    assert_eq!(refusal_reason(), None);
}

#[test]
fn background_personal_employee_refused() {
    let f = fixture();
    let home = f.home();
    let label = "alice-personal";
    let item = applied(home, 1, label, &[("binding_kind", "personal"), ("owner_id", f.alice.as_str())]);
    let root = root_receipt(&item, 1, false);
    crate::handlers::save_confirmed_receipt(home, &item, &root).unwrap();
    let roots = Root(vec![root.clone()]);
    // The employee itself is live and owned by alice ...
    let current = current_lifecycle(home, label, &roots).unwrap();
    assert_eq!(current.personal_owner(), Some(f.alice.as_str()));
    // ... but capture refuses up front, before any receipt or card exists.
    assert_eq!(capture_binding(home, label, &roots), Ok(None));
    assert_eq!(RequestError::PersonalEmployee.user_message(), Some(PERSONAL_REFUSAL));
    assert_eq!(RequestError::Failed(VerificationError::Revoked).user_message(), None);
    // A receipt carrying the Personal binding is never valid, and never stored.
    let mut value = super::tests::receipt(home);
    value.agent = label.into();
    value.employee = root.employee;
    value.binding = Some(current.binding.clone());
    value.allocation = Some(item.binding);
    assert_eq!(store::validate(&value), Err(VerificationError::Invalid));
    assert_eq!(store::save(&value), Err(VerificationError::Invalid));
    assert!(store::load(home, "cron", &value.id).unwrap().is_none());
    assert_eq!(check_lifecycle(&value, &current), Err(VerificationError::Revoked));
    // A Shared-form receipt for the Personal employee is refused as a binding change.
    value.binding = Some(EmployeeBinding::Shared(text(&f.tenant)));
    assert_eq!(check_receipt_lifecycle(&value, &roots), Err(VerificationError::Revoked));
}

#[test]
fn background_shared_unchanged_happy_path() {
    let f = fixture();
    let home = f.home();
    let (_, root) = shared(&f, 1, "team");
    let roots = Root(vec![root]);
    let value = captured(home, "team", &roots);
    store::validate_current(&value).unwrap();
    store::save(&value).unwrap();
    let stored = store::load(home, "cron", &value.id).unwrap().unwrap();
    assert_eq!(stored, value);
    // Nothing changed: every re-read passes, repeatedly.
    for _ in 0..3 { assert_eq!(check_receipt_lifecycle(&stored, &roots), Ok(())); }
    assert_eq!(check_tenant(stored.binding.as_ref().unwrap(), &f.tenant), Ok(()));
    // The capture is stable: the same lifecycle yields the same identity.
    let again = captured(home, "team", &roots);
    assert_eq!((again.employee, &again.binding, again.allocation), (value.employee, &value.binding, value.allocation));
}
