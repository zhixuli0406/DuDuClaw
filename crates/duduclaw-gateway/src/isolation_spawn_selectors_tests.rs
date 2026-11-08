//! H13 resolver tests: live delegation policy, target bindings and task
//! ownership for `spawn_agent` / `spawn_ephemeral`.
use super::*;
use std::sync::{Arc, Mutex};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane, Principal},
    ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{bindings::BindingStore, tool_policy::{authorize, ResourceResolver}};
use duduclaw_migration::{contract::{ProposalBinding, ProposalOperationId}, ids::RequestId};
use rusqlite::Connection;
use serde_json::json;

fn t<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
fn own() -> ResourceGrant { ResourceGrant::Employee { id: EMPLOYEE } }

/// A Personal employee `employee-a` run by its owner `alice`, with a task
/// store holding one own and one foreign task.
fn fixture(task: Option<&str>) -> (tempfile::TempDir, GatewayResourceResolver, RequestContext) {
    let home = tempfile::tempdir().unwrap();
    let principal = Principal::Human { user_id: t("alice"), generation: 1 };
    let binding = EmployeeBinding::Personal(t("alice"));
    let mut bindings = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
    bindings.register_principal(&principal).unwrap();
    bindings.create_employee(EMPLOYEE, &binding).unwrap();
    let resolver = GatewayResourceResolver::new(home.path().to_owned(), EMPLOYEE, "employee-a".into(),
        Arc::new(Mutex::new(bindings))).unwrap();
    Connection::open(home.path().join("tasks.db")).unwrap().execute_batch(
        "CREATE TABLE tasks(id TEXT PRIMARY KEY, assigned_to TEXT NOT NULL);
         INSERT INTO tasks VALUES ('own-task','employee-a'),('foreign-task','employee-b');").unwrap();
    let mut resources = vec![own()];
    if let Some(task) = task { resources.push(ResourceGrant::Task { id: t(task) }); }
    let context = RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run: RunId([3; 16]),
        task: task.map(|task| t(task)), principal, employee_binding: binding, lane: Lane::Standard,
        tools: List::new(vec![t("spawn_agent"), t("spawn_ephemeral"), t("wiki_read")]).unwrap(),
        resources: List::new(resources).unwrap(), policy_revision: 1, gateway_epoch: [0; 16],
        issued_at: 1, not_before: 1, expires_at: u64::MAX, nonce: [0; 32], jti: [0; 16],
    });
    (home, resolver, context)
}

fn policy(home: &std::path::Path, value: &str) {
    std::fs::write(home.join("config.toml"), format!("[delegation]\npolicy = \"{value}\"\n")).unwrap();
}

fn receipt(binding: Option<EmployeeBinding>, disabled: bool) -> Receipt {
    Receipt { binding: ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([7; 16]),
        revision: 1 }, proposal_digest: Sha256Digest([7; 32]) }, label: "target".into(), employee: EmployeeId([8; 16]),
        uid: 70_008, bytes: 67_108_864, inodes: 4096, disabled, principal_binding: binding }
}

fn ephemeral() -> Value {
    json!({"instruction":"You review invoices.", "context":"Check the March batch.", "tools":["wiki_read"]})
}

#[test]
fn spawn_agent_refuses_foreign_personal_target() {
    let tenant = EmployeeBinding::Shared(t("tenant-a"));
    let personal_source = EmployeeBinding::Personal(t("alice"));
    // A Personal target is refused whoever owns it (another owner here); it
    // never runs delegated background work.
    for source in [&tenant, &personal_source] {
        for owner in ["bob", "alice"] {
            let target = receipt(Some(EmployeeBinding::Personal(t(owner))), false);
            assert_eq!(target_binding_allowed(source, Some(&target)), Err(VerificationError::BindingMismatch));
        }
    }
    // Not a confirmed protected employee, or disabled: refused.
    assert_eq!(target_binding_allowed(&tenant, None), Err(VerificationError::Invalid));
    assert_eq!(target_binding_allowed(&tenant, Some(&receipt(Some(tenant.clone()), true))), Err(VerificationError::Revoked));
    // A Shared target must be in a Shared source's own tenant.
    let other_tenant = receipt(Some(EmployeeBinding::Shared(t("tenant-b"))), false);
    assert_eq!(target_binding_allowed(&tenant, Some(&other_tenant)), Err(VerificationError::BindingMismatch));
    assert_eq!(target_binding_allowed(&tenant, Some(&receipt(Some(tenant.clone()), false))), Ok(()));
    // Through the resolver: delegation policy allows (open), and with no
    // confirmed-employee index the target cannot be proven Shared — refused.
    let (home, resolver, context) = fixture(None);
    policy(home.path(), "open");
    let args = json!({"agent_id":"target", "task":"Prepare the report."});
    assert!(authorize(&context, "spawn_agent", &args, &resolver).is_err());
}

#[test]
fn spawn_agent_personal_source_to_shared_target_defers_to_owner_card() {
    // The selector does not refuse it: `record_bus_delegation` files the
    // existing owner approval card (`personal_delegation`) and queues nothing
    // until the owner approves.
    let source = EmployeeBinding::Personal(t("alice"));
    for target in [receipt(Some(EmployeeBinding::Shared(t("tenant-a"))), false), receipt(None, false)] {
        assert_eq!(target_binding_allowed(&source, Some(&target)), Ok(()));
    }
}

#[test]
fn spawn_agent_target_must_pass_delegation_policy() {
    let (home, resolver, context) = fixture(None);
    let spawn = duduclaw_mcp_boundary::tool_policy::policy("spawn_agent").unwrap();
    let args = json!({"agent_id":"target", "task":"Prepare the report."});
    // Hierarchy: unrelated (unknown) employees fail closed before any binding read.
    policy(home.path(), "hierarchy");
    assert_eq!(resolver.resolve(&context, spawn, &args), Err(VerificationError::Invalid));
    // Open: the policy passes, so the refusal now comes from the binding read
    // (no apply.db: unavailable, never a grant).
    policy(home.path(), "open");
    assert_ne!(resolver.resolve(&context, spawn, &args), Err(VerificationError::Invalid));
    assert!(resolver.resolve(&context, spawn, &args).is_err());
    // Self-delegation and ephemeral/reserved targets are refused outright.
    for target in ["employee-a", "eph-1a2b3c4d5e6f", "cron"] {
        let args = json!({"agent_id": target, "task":"work"});
        assert_eq!(resolver.resolve(&context, spawn, &args), Err(VerificationError::Invalid), "{target}");
    }
}

#[test]
fn spawn_ephemeral_refused_for_foreign_task() {
    let (home, resolver, context) = fixture(Some("own-task"));
    let call = authorize(&context, "spawn_ephemeral", &ephemeral(), &resolver).unwrap();
    assert_eq!(call.resources(), &[own(), ResourceGrant::Task { id: t("own-task") }]);
    // The task was reassigned mid-run: the next call refuses and is audited.
    Connection::open(home.path().join("tasks.db")).unwrap()
        .execute("UPDATE tasks SET assigned_to='employee-b' WHERE id='own-task'", []).unwrap();
    assert!(authorize(&context, "spawn_ephemeral", &ephemeral(), &resolver).is_err());
    let (foreign_home, resolver, context) = fixture(Some("foreign-task"));
    assert!(authorize(&context, "spawn_ephemeral", &ephemeral(), &resolver).is_err());
    policy(foreign_home.path(), "open");
    assert!(authorize(&context, "spawn_agent", &json!({"agent_id":"target", "task":"work"}), &resolver).is_err());
    // A missing task store refuses (never created).
    let (home, resolver, context) = fixture(Some("own-task"));
    std::fs::remove_file(home.path().join("tasks.db")).unwrap();
    assert!(authorize(&context, "spawn_ephemeral", &ephemeral(), &resolver).is_err());
    assert!(!home.path().join("tasks.db").exists());
}

#[test]
fn spawn_ephemeral_resolves_exactly_the_own_selection() {
    let (home, resolver, context) = fixture(None);
    let call = authorize(&context, "spawn_ephemeral", &ephemeral(), &resolver).unwrap();
    assert_eq!(call.resources(), &[own()]);
    // Nothing the parent run lacks, and no forged identity argument.
    let mut native_only = ephemeral();
    native_only["tools"] = json!(["Bash", "web_search"]);
    assert!(authorize(&context, "spawn_ephemeral", &native_only, &resolver).is_err());
    let mut forged = ephemeral();
    forged["employee_id"] = json!("00000000000000000000000000000000");
    assert!(authorize(&context, "spawn_ephemeral", &forged, &resolver).is_err());
    // Another employee's resolver never vouches for this record.
    let other = GatewayResourceResolver::new(home.path().to_owned(), EmployeeId([9; 16]), "employee-a".into(),
        Arc::new(Mutex::new(BindingStore::new(Connection::open_in_memory().unwrap()).unwrap()))).unwrap();
    assert!(authorize(&context, "spawn_ephemeral", &ephemeral(), &other).is_err());
}
