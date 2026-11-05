use super::*;
use duduclaw_isolation::bounded::Text;

fn human(id: &str, generation: u64) -> Principal {
    Principal::Human { user_id: Text::new(id.into()).unwrap(), generation }
}
fn personal(id: &str) -> EmployeeBinding { EmployeeBinding::Personal(Text::new(id.into()).unwrap()) }
fn shared(id: &str) -> EmployeeBinding { EmployeeBinding::Shared(Text::new(id.into()).unwrap()) }
fn fixture() -> BindingStore {
    let mut store = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    store.register_principal(&human("alice", 1)).unwrap();
    store.register_principal(&human("bob", 1)).unwrap();
    store.create_employee(EmployeeId([1; 16]), &personal("alice")).unwrap();
    store
}

#[test]
fn personal_second_binding_and_rebind_are_rejected_by_database() {
    let store = fixture();
    let employee = [1u8; 16];
    for role in ["owner", "operator", "viewer"] {
        assert!(store.connection.execute("INSERT INTO iso_employee_members VALUES (?1,'bob',?2)",
            params![employee.as_slice(), role]).is_err());
    }
    for sql in [
        "UPDATE iso_employee_bindings SET binding_id='bob'",
        "UPDATE iso_employee_bindings SET kind='shared'",
        "UPDATE iso_employee_members SET user_id='bob'",
        "UPDATE iso_employee_members SET role='viewer'",
        "DELETE FROM iso_employee_members",
        "DELETE FROM iso_employee_bindings",
    ] { assert!(store.connection.execute(sql, []).is_err(), "{sql}"); }
    assert!(store.connection.execute("INSERT OR REPLACE INTO iso_employee_bindings VALUES (?1,'personal','bob',0)",
        params![employee.as_slice()]).is_err());
    assert!(store.connection.execute("INSERT OR REPLACE INTO iso_employee_members VALUES (?1,'alice','owner')",
        params![employee.as_slice()]).is_err());
    assert!(store.validate(EmployeeId(employee), &personal("alice"), &human("alice", 1)).is_ok());
    assert!(store.validate(EmployeeId(employee), &personal("alice"), &human("bob", 1)).is_err());
}

#[test]
fn tombstoned_employee_id_cannot_be_recycled() {
    let mut store = fixture();
    let employee = EmployeeId([1; 16]);
    store.tombstone_employee(employee).unwrap();
    assert!(store.validate(employee, &personal("alice"), &human("alice", 1)).is_err());
    assert!(store.create_employee(employee, &personal("alice")).is_err());
    assert!(store.create_employee(employee, &personal("bob")).is_err());
    assert!(store.connection.execute("UPDATE iso_employee_bindings SET tombstone=0", []).is_err());
    assert!(store.connection.execute("DELETE FROM iso_employee_bindings", []).is_err());
    assert!(store.connection.execute("INSERT OR REPLACE INTO iso_employee_bindings VALUES (?1,'personal','alice',0)",
        params![employee.0.as_slice()]).is_err());
}

#[test]
fn principal_generation_and_revocation_are_checked_live() {
    let store = fixture();
    let employee = EmployeeId([1; 16]);
    store.advance_principal(&human("alice", 2)).unwrap();
    assert!(store.validate(employee, &personal("alice"), &human("alice", 1)).is_err());
    assert!(store.validate(employee, &personal("alice"), &human("alice", 2)).is_ok());
    store.revoke_principal(&human("alice", 2)).unwrap();
    assert!(store.validate(employee, &personal("alice"), &human("alice", 2)).is_err());
    assert!(store.advance_principal(&human("alice", 2)).is_err());
    assert!(store.connection.execute("UPDATE iso_principals SET active=1 WHERE id='alice'", []).is_err());
    assert!(store.connection.execute("UPDATE iso_principals SET generation=1 WHERE id='alice'", []).is_err());
    assert!(store.register_principal(&human("alice", 1)).is_err());
    store.advance_principal(&human("alice", 3)).unwrap();
    assert!(store.validate(employee, &personal("alice"), &human("alice", 3)).is_ok());
}

#[test]
fn shared_membership_cannot_use_personal_connectors() {
    let mut store = fixture();
    let tenant = Principal::Background { tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 };
    store.register_principal(&tenant).unwrap();
    let employee = EmployeeId([2; 16]);
    store.create_employee(employee, &shared("tenant")).unwrap();
    assert!(store.validate(employee, &shared("tenant"), &human("alice", 1)).is_err());
    store.add_shared_member(employee, "alice", "operator").unwrap();
    store.add_shared_member(employee, "bob", "viewer").unwrap();
    assert!(store.validate(employee, &shared("tenant"), &human("alice", 1)).is_ok());
    assert!(store.validate(employee, &shared("tenant"), &tenant).is_ok());
    store.register_connector("personal-account", &personal("alice")).unwrap();
    store.register_connector("shared-account", &shared("tenant")).unwrap();
    assert!(store.authorize_connector(employee, &shared("tenant"), &human("alice", 1), "personal-account").is_err());
    assert!(store.authorize_connector(employee, &personal("alice"), &human("alice", 1), "personal-account").is_err());
    assert!(store.authorize_connector(employee, &shared("tenant"), &human("alice", 1), "shared-account").is_ok());
    assert!(store.authorize_connector(EmployeeId([1; 16]), &personal("alice"), &human("alice", 1), "personal-account").is_ok());
    store.remove_shared_member(employee, "alice").unwrap();
    assert!(store.validate(employee, &shared("tenant"), &human("alice", 1)).is_err());
    store.add_shared_member(employee, "alice", "operator").unwrap();
    assert!(store.validate(employee, &shared("tenant"), &human("alice", 1)).is_err());
    assert!(store.validate(employee, &shared("tenant"), &human("alice", 2)).is_ok());
    assert!(store.remove_shared_member(EmployeeId([1; 16]), "alice").is_err());
    store.revoke_connector("shared-account").unwrap();
    assert!(store.authorize_connector(employee, &shared("tenant"), &tenant, "shared-account").is_err());
    assert!(store.connection.execute("UPDATE iso_connectors SET revoked=0", []).is_err());
    assert!(store.connection.execute("UPDATE iso_connectors SET binding_id='other'", []).is_err());
}

#[test]
fn unknown_missing_and_malformed_authorities_fail_closed() {
    let mut store = fixture();
    assert!(store.create_employee(EmployeeId([3; 16]), &personal("missing")).is_err());
    assert!(store.validate(EmployeeId([3; 16]), &personal("alice"), &human("alice", 1)).is_err());
    assert!(store.register_principal(&human("", 1)).is_err());
    assert!(store.register_principal(&human("u", u64::MAX)).is_err());
    store.connection.execute_batch("DROP TABLE iso_employee_members").unwrap();
    assert_eq!(store.validate(EmployeeId([1; 16]), &personal("alice"), &human("alice", 1)), Err(BindingError::Unavailable));
}

#[test]
fn binding_checked_authority_observes_durable_revocation() {
    use crate::{LiveAuthority, LiveSnapshot};
    use duduclaw_isolation::{execution::RunHandle, ids::RunId, ipc::{ControlConnectionId, RunStatus}};
    struct Runs;
    impl LiveAuthority for Runs {
        fn snapshot(&self, employee: EmployeeId, run: RunHandle) -> std::result::Result<LiveSnapshot, crate::VerificationError> {
            Ok(LiveSnapshot { employee, run, control: ControlConnectionId([1; 16]), status: RunStatus::Running,
                principal: human("alice", 1), binding: personal("alice"), policy_revision: 1, agent_slug: "main".into() })
        }
    }
    let bindings = std::sync::Arc::new(std::sync::Mutex::new(fixture()));
    let authority = BindingCheckedAuthority { run_policy: std::sync::Arc::new(Runs), bindings: bindings.clone() };
    let employee = EmployeeId([1; 16]);
    let run = RunHandle(RunId([2; 16]));
    assert!(authority.snapshot(employee, run).is_ok());
    bindings.lock().unwrap().revoke_principal(&human("alice", 1)).unwrap();
    assert!(authority.snapshot(employee, run).is_err());
}

#[test]
fn product_home_selection_preserves_personal_ownership_and_shared_tenant_boundaries() {
    let mut store = fixture();
    let personal_employee = EmployeeId([1; 16]);
    store.register_connector("company-home", &shared("company")).unwrap();
    store.register_connector("bob-private", &personal("bob")).unwrap();
    assert!(store.authorize_product_home_connector(personal_employee, &personal("alice"),
        &human("alice", 1), "company-home", &shared("company")).is_ok());
    assert!(store.authorize_product_home_connector(personal_employee, &personal("alice"),
        &human("alice", 1), "bob-private", &personal("bob")).is_err());
    let principal = Principal::Background { tenant_id: Text::new("other".into()).unwrap(), generation: 1 };
    store.register_principal(&principal).unwrap();
    let shared_employee = EmployeeId([2; 16]);
    store.create_employee(shared_employee, &shared("other")).unwrap();
    assert!(store.authorize_product_home_connector(shared_employee, &shared("other"),
        &principal, "company-home", &shared("company")).is_err());
    store.revoke_connector("company-home").unwrap();
    assert!(store.authorize_product_home_connector(personal_employee, &personal("alice"),
        &human("alice", 1), "company-home", &shared("company")).is_err());
}
