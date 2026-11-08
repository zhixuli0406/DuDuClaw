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
    // A Personal target of another owner is refused, and every Personal
    // target of a Shared source (H13b keeps only alice → alice).
    for (source, owner) in [(&tenant, "bob"), (&tenant, "alice"), (&personal_source, "bob")] {
        let target = receipt(Some(EmployeeBinding::Personal(t(owner))), false);
        assert_eq!(target_binding_allowed(source, Some(&target)), Err(VerificationError::BindingMismatch));
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

/// One users.db snapshot for `(human, agent)` (the facts `same_owner` reads).
fn snapshot(tenant: &str, human: &str, owner: Option<&str>, agent: &str, members: u32)
    -> duduclaw_auth::IsolationAuthoritySnapshot {
    let mut user = duduclaw_auth::UserContext::admin_fallback();
    user.user_id = human.into();
    user.agent_access.insert(agent.into(), duduclaw_auth::AccessLevel::Owner);
    duduclaw_auth::IsolationAuthoritySnapshot { tenant_id: tenant.into(), acl_generation: 1, policy_epoch: 1,
        human: Some(user), personal_owner: owner.map(Into::into), agent_member_count: members }
}

#[test]
fn h13b_same_owner_personal_delegation_allowed() {
    use crate::isolation_background::same_owner;
    let alice = EmployeeBinding::Personal(t("alice"));
    // Selector: a Personal source may target another Personal employee of the
    // very same owner (no owner card: the owner started this run).
    assert_eq!(target_binding_allowed(&alice, Some(&receipt(Some(alice.clone()), false))), Ok(()));
    // Capture/recheck (the receipt path) apply the same rule, with the run's
    // verified requester required to be exactly that owner …
    assert_eq!(same_owner::personal_target_allowed(&alice, &alice, "alice"), Ok(()));
    // … and the fresh users.db facts of the target showing that owner as its
    // human, mirror owner and only member.
    assert_eq!(same_owner::check_binding_facts(&alice, "target", &snapshot("tenant-a", "alice", Some("alice"), "target", 1)),
        Ok(()));
    // The record side: the source run must be owner-initiated.
    let (_home, _resolver, context) = fixture(None);
    assert_eq!(tool_policy::spawn::ephemeral_parent_allowed(&context), Ok(()));
    // A disabled same-owner target is still refused.
    assert_eq!(target_binding_allowed(&alice, Some(&receipt(Some(alice.clone()), true))), Err(VerificationError::Revoked));
}

#[test]
fn h13b_cross_owner_personal_delegation_refused() {
    use crate::isolation_background::same_owner;
    let (alice, bob) = (EmployeeBinding::Personal(t("alice")), EmployeeBinding::Personal(t("bob")));
    let tenant = EmployeeBinding::Shared(t("tenant-a"));
    // Another owner's Personal employee: refused at the selector and at capture,
    // never turned into an owner card.
    assert_eq!(target_binding_allowed(&alice, Some(&receipt(Some(bob.clone()), false))), Err(VerificationError::BindingMismatch));
    assert_eq!(same_owner::personal_target_allowed(&alice, &bob, "alice"), Err(VerificationError::BindingMismatch));
    // Same owner, but the run was not started by that owner.
    assert_eq!(same_owner::personal_target_allowed(&alice, &alice, "bob"), Err(VerificationError::BindingMismatch));
    // A Shared source never reaches a Personal employee.
    assert_eq!(same_owner::personal_target_allowed(&tenant, &alice, "alice"), Err(VerificationError::BindingMismatch));
    // Fresh users.db facts that no longer show the owner refuse at recheck.
    for current in [snapshot("tenant-a", "alice", Some("bob"), "target", 1), snapshot("tenant-a", "bob", Some("alice"), "target", 1),
        snapshot("tenant-a", "alice", Some("alice"), "target", 2), snapshot("tenant-a", "alice", None, "target", 1),
        snapshot("tenant-a", "alice", Some("alice"), "other", 1)] {
        assert_eq!(same_owner::check_binding_facts(&alice, "target", &current), Err(VerificationError::Revoked));
    }
}

#[test]
fn h13b_shared_unchanged() {
    use crate::isolation_background::same_owner;
    let (tenant, other) = (EmployeeBinding::Shared(t("tenant-a")), EmployeeBinding::Shared(t("tenant-b")));
    assert_eq!(target_binding_allowed(&tenant, Some(&receipt(Some(tenant.clone()), false))), Ok(()));
    assert_eq!(target_binding_allowed(&tenant, Some(&receipt(Some(other.clone()), false))), Err(VerificationError::BindingMismatch));
    assert_eq!(target_binding_allowed(&tenant, Some(&receipt(None, false))), Ok(()));
    // Personal → Shared keeps the owner card path (not refused here).
    assert_eq!(target_binding_allowed(&EmployeeBinding::Personal(t("alice")), Some(&receipt(Some(tenant.clone()), false))), Ok(()));
    // Capture: a Shared target is never subject to the same-owner rule …
    for source in [&tenant, &EmployeeBinding::Personal(t("alice"))] {
        assert_eq!(same_owner::personal_target_allowed(source, &tenant, "anyone"), Ok(()));
    }
    // … and its users.db check is still exactly the tenant comparison.
    assert_eq!(same_owner::check_binding_facts(&tenant, "target", &snapshot("tenant-a", "bob", None, "target", 3)), Ok(()));
    assert_eq!(same_owner::check_binding_facts(&tenant, "target", &snapshot("tenant-b", "bob", None, "target", 3)),
        Err(VerificationError::Revoked));
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
