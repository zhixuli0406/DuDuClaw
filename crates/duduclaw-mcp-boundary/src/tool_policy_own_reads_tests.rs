//! FD-SEL boundary tests for the own discovery-job and fork-read selectors.
use super::*;
use crate::tool_policy::{authorize, eligible, policy, ResourceKind, ResourceResolver};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, EmployeeBinding,
    Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
use serde_json::json;

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn own() -> ResourceGrant { ResourceGrant::Employee { id: EmployeeId([2; 16]) } }
fn record(tools: &[&str], resources: Vec<ResourceGrant>) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: text("owner"), generation: 1 },
        employee_binding: EmployeeBinding::Shared(text("tenant")), lane: Lane::Standard,
        tools: List::new(tools.iter().map(|tool| text(tool)).collect()).unwrap(),
        resources: List::new(resources).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [0; 32], jti: [0; 16],
    })
}

/// A resolver that would widen the call; the own-read path never asks it.
struct Widening;
impl ResourceResolver for Widening {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        panic!("FD-SEL own reads never consult the resolver")
    }
}

fn valid_arguments(name: &str) -> Value {
    match name {
        "discovery_catalog" | "discovery_list" => json!({}),
        "discovery_tree" | "discovery_cancel" | "discovery_artifact" => json!({"run_id":"run-own"}),
        "diff_branches" => json!({"fork_id":"a".repeat(32), "left_branch_id":"b".repeat(32), "right_branch_id":"c".repeat(32)}),
        _ => json!({"fork_id":"a".repeat(32)}),
    }
}

#[test]
fn fdsel_policies_need_only_the_own_employee_grant() {
    assert_eq!(OWN_DISCOVERY_TOOLS.iter().chain(OWN_FORK_READ_TOOLS.iter()).copied().collect::<Vec<_>>(),
        OWN_READ_TOOLS.to_vec());
    for name in OWN_READ_TOOLS {
        assert_eq!(policy(name).expect(name).required, &[ResourceKind::Employee], "{name}");
        // Eligible from the own Employee grant alone: no Workspace, Task or scope.
        assert!(eligible(&record(&[name], vec![own()]), name), "{name}");
        assert_eq!(own_read_selector(&record(&[name], vec![own()]), name).unwrap(), vec![own()], "{name}");
        // Another employee's grant, no grant, or an unsigned tool: not eligible.
        assert!(!eligible(&record(&[name], vec![ResourceGrant::Employee { id: EmployeeId([9; 16]) }]), name), "{name}");
        assert!(!eligible(&record(&[name], vec![]), name), "{name}");
        assert!(!eligible(&record(&[], vec![own()]), name), "{name}");
        // A Workspace grant adds nothing and is never selected.
        let with_workspace = record(&[name], vec![own(), ResourceGrant::Workspace { handle: text("employee-work") }]);
        assert_eq!(own_read_selector(&with_workspace, name).unwrap(), vec![own()], "{name}");
    }
    for name in crate::fork_policy::DEFERRED_TOOLS.into_iter().chain(["create_task", "file_read", "unknown"]) {
        assert!(own_read_selector(&record(&[name], vec![own()]), name).is_err(), "{name}");
    }
}

#[test]
fn fdsel_authorize_selects_exactly_the_own_employee_without_the_resolver() {
    for name in OWN_READ_TOOLS {
        let context = record(&[name], vec![own(), ResourceGrant::Workspace { handle: text("employee-work") }]);
        let call = authorize(&context, name, &valid_arguments(name), &Widening).unwrap();
        assert_eq!(call.resources(), &[own()], "{name}");
        assert_eq!(call.name(), name);
        let foreign = record(&[name], vec![ResourceGrant::Employee { id: EmployeeId([9; 16]) }]);
        assert_eq!(authorize(&foreign, name, &valid_arguments(name), &Widening).err(),
            Some(VerificationError::Invalid), "{name}");
    }
}

#[test]
fn fdsel_own_read_arguments_are_exact() {
    for name in OWN_READ_TOOLS {
        assert!(check_own_read_arguments(name, &valid_arguments(name)).is_ok(), "{name}");
        // Identity, run, root and workspace selectors never ride along.
        for key in ["employee_id", "principal", "root", "cwd", "run", "workspace", "creator_id", "incarnation"] {
            let mut forged = valid_arguments(name);
            forged[key] = json!("forged");
            assert!(check_own_read_arguments(name, &forged).is_err(), "{name} {key}");
        }
        assert!(check_own_read_arguments(name, &json!([])).is_err(), "{name}");
    }
    assert!(check_own_read_arguments("discovery_list", &json!({"agent_id":"worker","limit":100})).is_ok());
    assert!(check_own_read_arguments("discovery_list", &json!({"agent_id":null,"limit":null})).is_ok());
    assert!(check_own_read_arguments("discovery_catalog", &json!({"agent_id":"worker"})).is_ok());
    assert!(check_own_read_arguments("discovery_artifact", &json!({"run_id":"run-own","file_id":format!("file-{}", "a".repeat(64))})).is_ok());
    for (name, arguments) in [
        ("discovery_list", json!({"limit":0})), ("discovery_list", json!({"limit":101})),
        ("discovery_list", json!({"limit":"20"})), ("discovery_list", json!({"agent_id":"../other"})),
        ("discovery_catalog", json!({"agent_id":"*"})), ("discovery_tree", json!({})),
        ("discovery_tree", json!({"run_id":null})), ("discovery_tree", json!({"run_id":"../run"})),
        ("discovery_tree", json!({"run_id":"run-own","agent_id":"other"})),
        ("discovery_cancel", json!({"run_id":"r".repeat(OWN_READ_ID_MAX + 1)})),
        ("discovery_artifact", json!({"run_id":"run-own","file_id":"/etc/passwd"})),
        ("inspect_branches", json!({"fork_id":7})),
        // The legacy schema's names are not the protected listing's.
        ("diff_branches", json!({"fork_id":"a".repeat(32),"branch_a":"b","branch_b":"c"})),
        ("fork_cost", json!({"fork_id":"a".repeat(32),"branch_id":"b".repeat(32)})),
        ("merge_or_select", json!({"fork_id":"a".repeat(32)})),
    ] {
        assert!(check_own_read_arguments(name, &arguments).is_err(), "{name} {arguments}");
    }
}

/// The issuer no longer drops these tools for a selected Workspace grant, and
/// none of them is a tracked selector gap any more.
#[test]
fn fdsel_tools_left_the_pending_selector_list() {
    use crate::workspace_selection::{drops_from_signed_with, needs_execution_workspace, PENDING_WORKSPACE_SELECTOR_TOOLS};
    for name in OWN_READ_TOOLS {
        assert!(!PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&name), "{name}");
        for scoped in [true, false] {
            assert!(!needs_execution_workspace(name, scoped), "{name}");
            for (helpers, recording) in [(false, false), (true, false), (false, true), (true, true)] {
                assert!(!drops_from_signed_with(name, scoped, helpers, recording), "{name}");
            }
        }
    }
}
