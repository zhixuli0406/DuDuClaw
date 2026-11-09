use super::*;
use std::sync::Arc;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{RequestContext, resources::ResourceRoots};
use crate::mcp_isolation::{IsolatedCall, ISOLATED_CALL};

/// A verified isolated `worker` call of `EmployeeId([employee; 16])`, signed
/// with the five discovery job tools and its own Employee grant (FD-SEL).
fn call_for(home: &Path, employee: u8) -> Arc<IsolatedCall> {
    Arc::new(IsolatedCall { forks: None,
        home: home.to_owned(), agent: "worker".into(), roots: Arc::new(ResourceRoots::default()),
        parser: None, services: None, execution: None, worker: None, record_resolver: None,
        context: RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([employee; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(duduclaw_mcp_boundary::tool_policy::OWN_DISCOVERY_TOOLS.into_iter()
                .map(|tool| Text::new(tool.into()).unwrap()).collect()).unwrap(),
            resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([employee; 16]) }]).unwrap(),
            policy_revision: 1,
            gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
        }),
    })
}
fn call(home: &Path) -> Arc<IsolatedCall> { call_for(home, 2) }

#[tokio::test]
async fn discovery_isolation_requires_both_verified_scopes_without_minting_identity_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    assert!(TrustedCaller::from_isolated_agent(home, "worker").is_err());
    let call = call(home);
    ISOLATED_CALL.scope(call.clone(), async {
        assert!(caller(home, "worker").is_err(), "missing gateway scope must not fall back to a token");
    }).await;
    duduclaw_gateway::isolation_egress::scope(home, "worker", &call.context, None,
        ISOLATED_CALL.scope(call.clone(), async {
            // FD-SEL: both scopes are not enough without the label's live
            // confirmed lifecycle for this very employee (incarnation binding).
            assert!(caller(home, "worker").is_err());
            assert!(caller(home, "other").is_err());
            assert!(caller(&home.join("other-home"), "worker").is_err());
            assert!(TrustedCaller::from_isolated_agent(home, "other").is_err());
        })).await.unwrap();
    assert!(!home.join("identity.key").exists());
}

/// FD-SEL: the protected caller is the verified capability's employee id at
/// the label's live confirmed incarnation; another employee id under the same
/// label (a recreated employee) is refused.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn fdsel_discovery_caller_binds_exact_employee_and_incarnation() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let _employee = duduclaw_gateway::isolation_mcp::test_support::SharedEmployee::new(home, "worker", EmployeeId([2; 16]));
    for (employee, expected) in [(2u8, true), (9u8, false)] {
        let call = call_for(home, employee);
        duduclaw_gateway::isolation_egress::scope(home, "worker", &call.context, None,
            ISOLATED_CALL.scope(call.clone(), async {
                let resolved = caller(home, "worker");
                assert_eq!(resolved.is_ok(), expected, "{employee}");
                if let Ok(resolved) = resolved {
                    assert_eq!(resolved.isolated_creator(), Some(duduclaw_gateway::discovery::service::IsolatedCreator {
                        employee: [2; 16], incarnation: 0 }));
                }
            })).await.unwrap();
    }
    assert!(!home.join("identity.key").exists());
}

/// Two discovery tasks of `worker`: `own` (filed by this protected employee:
/// creator binding `(EmployeeId([2; 16]), 0)`) and `foreign` (another creator).
#[cfg(target_os = "linux")]
async fn bound_fixture(home: &Path) -> duduclaw_gateway::task_store::TaskStore {
    use duduclaw_gateway::task_store::{TaskKind, TaskRow, TaskStore};
    let _employee = duduclaw_gateway::isolation_mcp::test_support::SharedEmployee::new(home, "worker", EmployeeId([2; 16]));
    std::fs::create_dir_all(home.join("agents/worker")).unwrap();
    std::fs::write(home.join("agents/worker/agent.toml"), "[agent]\nname = 'worker'\n").unwrap();
    let store = TaskStore::open(home).unwrap();
    for (id, creator) in [("own", "agent:worker"), ("foreign", "agent:other")] {
        let mut task = TaskRow::new(id.into(), "Explore".into(), "Fixture".into(),
            "medium".into(), "worker".into(), creator.into());
        task.kind = TaskKind::Discovery;
        task.status = "queued".into();
        task.discovery_run_id = Some(format!("run-{id}"));
        task.discovery_spec_json = Some("{}".into());
        store.insert_task(&task).await.unwrap();
    }
    duduclaw_gateway::discovery::DiscoveryStore::open(home).unwrap()
        .save_isolated_creator("own", "run-own", &[2; 16], 0).unwrap();
    store
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn discovery_isolation_keeps_creator_acl_for_reads_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let store = bound_fixture(home).await;
    let call = call(home);
    duduclaw_gateway::isolation_egress::scope(home, "worker", &call.context, None,
        ISOLATED_CALL.scope(call.clone(), async {
            for operation in ["discovery_tree", "discovery_cancel"] {
                let result = handle_discovery_query(operation, &serde_json::json!({"run_id":"run-foreign"}), home, "worker").await;
                assert_eq!(result["isError"], true, "{operation}");
                assert!(result.to_string().contains(duduclaw_gateway::discovery::service::DISCOVERY_NOT_FOUND));
            }
            assert_eq!(store.get_task("foreign").await.unwrap().unwrap().status, "queued");
            let listing = handle_discovery_query("discovery_list",
                &serde_json::json!({"agent_id":"other"}), home, "worker").await;
            assert_ne!(listing["isError"], true);
            let listed: Value = serde_json::from_str(listing["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(listed["runs"], serde_json::json!([]));
            let result = handle_discovery_query("discovery_cancel", &serde_json::json!({"run_id":"run-own"}), home, "worker").await;
            assert_ne!(result["isError"], true, "{result}");
            assert_eq!(store.get_task("own").await.unwrap().unwrap().status, "cancelled");
            let artifacts = handle_discovery_query("discovery_artifact",
                &serde_json::json!({"run_id":"run-own"}), home, "worker").await;
            assert_eq!(artifacts["isError"], true);
        })).await.unwrap();
}

/// FD-SEL: own runs only, with not-found parity: a foreign run, an unknown run
/// and the own run seen from another employee id all answer the same text;
/// extra selectors and a record without the own Employee grant refuse before
/// the service is reached.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn fdsel_discovery_tools_answer_foreign_like_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let store = bound_fixture(home).await;
    let call = call(home);
    let text = |value: &Value| value["content"][0]["text"].as_str().unwrap_or_default().to_owned();
    duduclaw_gateway::isolation_egress::scope(home, "worker", &call.context, None,
        ISOLATED_CALL.scope(call.clone(), async {
            let unknown = handle_discovery_query("discovery_tree", &serde_json::json!({"run_id":"run-missing"}), home, "worker").await;
            assert_eq!(unknown["isError"], true);
            for operation in ["discovery_tree", "discovery_cancel", "discovery_artifact"] {
                let foreign = handle_discovery_query(operation, &serde_json::json!({"run_id":"run-foreign"}), home, "worker").await;
                assert_eq!(text(&foreign), text(&unknown), "{operation}: a foreign run must look like an unknown run");
            }
            // The own run passes the creator ACL; this fixture's placeholder
            // request is only refused afterwards (full own reads: gateway
            // `fdsel_isolated_discovery_sees_and_cancels_only_its_own_bound_requests`).
            let own = handle_discovery_query("discovery_tree", &serde_json::json!({"run_id":"run-own"}), home, "worker").await;
            assert_ne!(text(&own), text(&unknown), "{own}");
            // Exact protected schema: identity/run selectors refuse before the service.
            for forged in [serde_json::json!({"run_id":"run-own","agent_id":"worker"}),
                serde_json::json!({"run_id":"run-own","employee_id":"x"}), serde_json::json!({})] {
                let refused = handle_discovery_query("discovery_cancel", &forged, home, "worker").await;
                assert_eq!(refused["isError"], true, "{forged}");
                assert!(!text(&refused).contains("discovery not found"), "{forged}");
            }
            assert_eq!(store.get_task("own").await.unwrap().unwrap().status, "queued", "no forged call cancelled");
        })).await.unwrap();
    // The same label under another employee id: the own run is not its own.
    let other = call_for(home, 9);
    duduclaw_gateway::isolation_egress::scope(home, "worker", &other.context, None,
        ISOLATED_CALL.scope(other.clone(), async {
            let refused = handle_discovery_query("discovery_cancel", &serde_json::json!({"run_id":"run-own"}), home, "worker").await;
            assert_eq!(refused["isError"], true);
        })).await.unwrap();
    // A record without its own Employee grant is refused by the selector.
    let mut record = call.context.record().clone();
    record.resources = List::new(vec![]).unwrap();
    let unselected = Arc::new(IsolatedCall { forks: None, home: home.to_owned(), agent: "worker".into(),
        roots: Arc::new(ResourceRoots::default()), parser: None, services: None, execution: None, worker: None,
        record_resolver: None, context: RequestContext::from_verified_record(record) });
    duduclaw_gateway::isolation_egress::scope(home, "worker", &unselected.context, None,
        ISOLATED_CALL.scope(unselected.clone(), async {
            let refused = handle_discovery_query("discovery_cancel", &serde_json::json!({"run_id":"run-own"}), home, "worker").await;
            assert_eq!(refused["isError"], true);
        })).await.unwrap();
    assert_eq!(store.get_task("own").await.unwrap().unwrap().status, "queued");
}

/// FD-SEL: without an isolated call the handler is the original one.
#[tokio::test]
async fn fdsel_unprotected_discovery_query_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    // No verified isolated call: no protected schema check; the original
    // signed-identity requirement still answers.
    let result = handle_discovery_query("discovery_tree", &serde_json::json!({"run_id":"run-own","extra":1}),
        dir.path(), "worker").await;
    assert_eq!(result["isError"], true);
    assert!(!result.to_string().contains("探索操作"), "{result}");
}
