use super::*;
use std::sync::Arc;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
use duduclaw_mcp_boundary::{RequestContext, resources::ResourceRoots};
use crate::mcp_isolation::{IsolatedCall, ISOLATED_CALL};

fn call(home: &Path) -> Arc<IsolatedCall> {
    Arc::new(IsolatedCall { forks: None,
        home: home.to_owned(), agent: "worker".into(), roots: Arc::new(ResourceRoots::default()),
        parser: None, services: None, execution: None, worker: None, record_resolver: None,
        context: RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
            gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
        }),
    })
}

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
            assert!(caller(home, "worker").is_ok());
            assert!(caller(home, "other").is_err());
            assert!(caller(&home.join("other-home"), "worker").is_err());
            assert!(TrustedCaller::from_isolated_agent(home, "other").is_err());
        })).await.unwrap();
    assert!(!home.join("identity.key").exists());
}

#[tokio::test]
async fn discovery_isolation_keeps_creator_acl_for_reads_and_cancellation() {
    use duduclaw_gateway::task_store::{TaskKind, TaskRow, TaskStore};
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
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
    let call = call(home);
    duduclaw_gateway::isolation_egress::scope(home, "worker", &call.context, None,
        ISOLATED_CALL.scope(call.clone(), async {
            for operation in ["discovery_tree", "discovery_cancel"] {
                let result = handle_discovery_query(operation,
                    &serde_json::json!({"run_id":"run-foreign", "agent_id":"other"}), home, "worker").await;
                assert_eq!(result["isError"], true, "{operation}");
                assert!(result.to_string().contains("permission denied"));
            }
            assert_eq!(store.get_task("foreign").await.unwrap().unwrap().status, "queued");
            let listing = handle_discovery_query("discovery_list",
                &serde_json::json!({"agent_id":"other"}), home, "worker").await;
            assert_ne!(listing["isError"], true);
            let listed: Value = serde_json::from_str(listing["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(listed["runs"], serde_json::json!([]));
            let result = handle_discovery_query("discovery_cancel",
                &serde_json::json!({"run_id":"run-own", "agent_id":"other"}), home, "worker").await;
            assert_ne!(result["isError"], true);
            assert_eq!(store.get_task("own").await.unwrap().unwrap().status, "cancelled");
            let artifacts = handle_discovery_query("discovery_artifact",
                &serde_json::json!({"run_id":"run-own"}), home, "worker").await;
            assert_eq!(artifacts["isError"], true);
        })).await.unwrap();
}
