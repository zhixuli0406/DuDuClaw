//! Package A operations whose immediate effect is gateway-owned queue admission.
//! The original handlers retain target/delegation ACLs and background leases.
//! Queue acceptance is not a protected run: target ingress must independently
//! derive fresh launch authority, and ephemeral targets still need provisioning.

pub(crate) fn is_target_dispatch(name: &str) -> bool {
    matches!(name, "create_task" | "spawn_agent" | "spawn_ephemeral")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, sync::Arc};
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId,
        CapabilityRecord, EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
    use duduclaw_mcp_boundary::{RequestContext, VerificationError, resources::ResourceRoots};
    use duduclaw_gateway::isolation_egress::{RunMetadata, RunTurnIdentity, RunTurnResolver};
    use crate::mcp_isolation::{BoundaryRoute, IsolatedCall, ISOLATED_CALL};
    use serde_json::{Value, json};

    fn call(home: &Path) -> Arc<IsolatedCall> {
        Arc::new(IsolatedCall { forks: None,
            home: home.into(), agent: "caller".into(), roots: Arc::new(ResourceRoots::default()),
            parser: None, services: None, execution: None, worker: None, record_resolver: None,
            context: RequestContext::from_verified_record(CapabilityRecord {
                id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
                task: None,
                principal: Principal::Background { tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 },
                employee_binding: EmployeeBinding::Shared(Text::new("tenant".into()).unwrap()), lane: Lane::Standard,
                tools: List::new(["create_task", "spawn_agent", "spawn_ephemeral"].into_iter()
                    .map(|name| Text::new(name.into()).unwrap()).collect()).unwrap(),
                resources: List::new(vec![]).unwrap(), policy_revision: 1, gateway_epoch: [4; 16],
                issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
            }),
        })
    }

    #[derive(Clone)]
    struct Metadata;
    impl RunTurnResolver for Metadata {
        fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
            Ok(RunTurnIdentity { turn: "host-turn".into(), approval_session: Some("host-session".into()) })
        }
        fn metadata(&self, _: &RequestContext) -> Result<Option<RunMetadata>, VerificationError> {
            Ok(Some(RunMetadata { delegation_depth: 1, hop_depth: 2,
                delegation_origin: Some("origin".into()), ..RunMetadata::root() }))
        }
    }

    fn write_agent(home: &Path, id: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), format!(
            "[agent]\nname = '{id}'\nstatus = 'active'\nrole = 'specialist'\n")).unwrap();
    }

    fn text(value: &Value) -> &str {
        value["content"][0]["text"].as_str().unwrap_or("")
    }

    #[test]
    fn target_dispatch_routes_preserve_original_target_arguments() {
        for name in ["create_task", "spawn_agent", "spawn_ephemeral"] {
            assert!(is_target_dispatch(name));
            assert!(matches!(crate::mcp_isolation::boundary_route(name), Some(BoundaryRoute::GatewayHandler(_))));
        }
        for name in ["spawn_agent_extra", "fork_run", "inspect_branches", "diff_branches",
            "merge_or_select", "terminate_branch", "fork_cost", "execute_program", "office_script"] {
            assert!(!is_target_dispatch(name));
        }
        assert!(crate::mcp_isolation::check_tool_arguments("spawn_agent",
            &json!({"agent_id":"target", "task":"work"})).is_ok());
        assert!(crate::mcp_isolation::check_tool_arguments("create_task",
            &json!({"goal":"work", "steps":[{"description":"step", "agent":"target"}]})).is_ok());
        for field in ["principal", "employee_id", "run_id", "cwd", "root"] {
            let mut args = json!({"agent_id":"target", "task":"work"});
            args[field] = json!("forged");
            assert!(crate::mcp_isolation::check_tool_arguments("spawn_agent", &args).is_err(), "{field}");
        }
    }

    #[tokio::test]
    async fn target_dispatch_enqueues_with_host_identity_and_no_helper_backend() {
        let home = tempfile::tempdir().unwrap();
        write_agent(home.path(), "caller");
        write_agent(home.path(), "target");
        std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy = 'open'\n").unwrap();
        let call = call(home.path());
        let context = call.context.clone();
        duduclaw_gateway::isolation_egress::scope(home.path(), "caller", &context, Some(Arc::new(Metadata)),
            ISOLATED_CALL.scope(call, async {
                let arguments = json!({"agent_id":"target", "task":"work", "session_key":"requested-session",
                    "sender_agent":"forged", "origin_agent":"forged", "delegation_depth":0,
                    "hop_depth":0, "turn_id":"forged", "on_behalf_of":"user:forged"});
                // This route only enters the existing handler; no A backend is installed.
                assert!(crate::mcp_isolation::dispatch_resource_tool(home.path(), "spawn_agent", &arguments).await.is_none());
                assert_eq!(crate::mcp::get_default_agent(home.path()).await, "caller");
                let result = crate::mcp::handle_spawn_agent(&arguments, home.path(), "caller").await;
                assert_ne!(result["isError"], true, "{result}");
                let entries = std::fs::read_to_string(home.path().join("bus_queue.jsonl")).unwrap();
                let entry: Value = serde_json::from_str(entries.lines().last().unwrap()).unwrap();
                assert_eq!(entry["agent_id"], "target");
                assert_eq!(entry["sender_agent"], "caller");
                assert_eq!(entry["origin_agent"], "origin");
                assert_eq!(entry["delegation_depth"], 2);
                assert_eq!(entry["hop_depth"], 3);
                assert_eq!(entry["turn_id"], "host-turn");
                assert_eq!(entry["session_id"], "host-session");
                assert_eq!(entry["session_key"], "requested-session");
                assert!(entry.get("on_behalf_of").is_none());
                assert!(!text(&result).is_empty());
            })).await.unwrap();
    }

    #[tokio::test]
    async fn target_dispatch_without_host_metadata_refuses_instead_of_resetting_depth() {
        let home = tempfile::tempdir().unwrap();
        write_agent(home.path(), "caller");
        write_agent(home.path(), "target");
        std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy = 'open'\n").unwrap();
        ISOLATED_CALL.scope(call(home.path()), async {
            let result = crate::mcp::handle_spawn_agent(&json!({"agent_id":"target", "task":"work", "delegation_depth":0}),
                home.path(), "caller").await;
            assert_eq!(result["isError"], true, "{result}");
            assert!(text(&result).contains("delegation depth limit"), "{result}");
            assert!(!home.path().join("bus_queue.jsonl").exists());
        }).await;
    }

    #[tokio::test]
    async fn target_dispatch_create_task_retains_delegation_and_background_gates() {
        let home = tempfile::tempdir().unwrap();
        write_agent(home.path(), "caller");
        write_agent(home.path(), "target");
        std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy = 'hierarchy'\n").unwrap();
        ISOLATED_CALL.scope(call(home.path()), async {
            let result = crate::mcp::handle_create_task(&json!({"goal":"work",
                "steps":[{"description":"step", "agent":"target"}]}), home.path(), "caller").await;
            assert_eq!(result["isError"], true, "{result}");
            assert!(text(&result).contains("step 0:"), "{result}");
            assert!(!home.path().join("bus_queue.jsonl").exists());

            duduclaw_core::autonomy_gate::set_pause(home.path(), Some("caller"), "fixture", "operator").unwrap();
            let result = crate::mcp::handle_create_task(&json!({"goal":"work",
                "steps":[{"description":"step"}]}), home.path(), "caller").await;
            assert_eq!(result["isError"], true, "{result}");
            assert!(text(&result).contains("暫停"), "{result}");
            assert!(!home.path().join("bus_queue.jsonl").exists());
        }).await;
    }
    #[tokio::test]
    async fn isolated_i1b_fork_mutations_never_enter_legacy_host_paths() {
        let home = tempfile::tempdir().unwrap();
        ISOLATED_CALL.scope(call(home.path()), async {
            for result in [crate::mcp_fork::handle_fork_run(&json!({"prompt":"x"}), home.path(), "caller").await,
                crate::mcp_fork::handle_merge_or_select(&json!({"fork_id":"f","branch_id":"b"}), home.path(), "caller").await,
                crate::mcp_fork::handle_terminate_branch(&json!({"fork_id":"f","branch_id":"b"}), home.path(), "caller").await] {
                assert_eq!(result["isError"], true);
                assert!(!text(&result).is_empty());
            }
            assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
        }).await;
    }

    struct HostForkService;
    impl duduclaw_mcp_boundary::forks::ForkToolService for HostForkService {
        fn supports(&self, name: &str) -> bool { name == "inspect_branches" }
        fn execute<'a>(&'a self, _: &'a RequestContext, name: &'a str, args: &'a Value)
            -> duduclaw_mcp_boundary::execution::PolicyFuture<'a, Value> {
            Box::pin(async move { Ok(json!({"source":"host-owned-store","tool":name,"fork_id":args["fork_id"]})) })
        }
    }

    struct NeverRunForkService;
    impl duduclaw_mcp_boundary::forks::ForkToolService for NeverRunForkService {
        fn supports(&self, _: &str) -> bool { true }
        fn execute<'a>(&'a self, _: &'a RequestContext, _: &'a str, _: &'a Value)
            -> duduclaw_mcp_boundary::execution::PolicyFuture<'a, Value> {
            panic!("deferred fork entered an injected service")
        }
    }

    #[tokio::test]
    async fn isolated_i1b_round6_fork_refusal_precedes_stores_and_injected_services() {
        let home = tempfile::tempdir().unwrap();
        let mut owned = Arc::try_unwrap(call(home.path())).ok().unwrap();
        let mut record = owned.context.record().clone();
        record.tools = List::new(duduclaw_mcp_boundary::fork_policy::DEFERRED_TOOLS.into_iter()
            .map(|name| Text::new(name.into()).unwrap()).collect()).unwrap();
        owned.context = RequestContext::from_verified_record(record);
        owned.forks = Some(Arc::new(NeverRunForkService));
        ISOLATED_CALL.scope(Arc::new(owned), async {
            for language in ["zh-TW", "en", "ja"] {
                std::fs::write(home.path().join("config.toml"),
                    format!("[general]\ndefault_language='{language}'\n")).unwrap();
                let args = json!({"fork_id":"../../escape","branch_id":"forged","prompt":"test"});
                for result in [crate::mcp_fork::handle_fork_run(&args, home.path(), "caller").await,
                    crate::mcp_fork::handle_merge_or_select(&args, home.path(), "caller").await,
                    crate::mcp_fork::handle_terminate_branch(&args, home.path(), "caller").await] {
                    assert_eq!(result["isError"], true);
                    // Fork handlers retain their existing error envelope, including the prefix.
                    assert_eq!(text(&result), format!("Error: {}",
                        duduclaw_mcp_boundary::fork_policy::message(Some(language))));
                }
                assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1, "only config exists");
            }
        }).await;
    }

    #[tokio::test]
    async fn isolated_i1b_fork_host_route_retains_opt_in_and_never_opens_legacy_store() {
        use duduclaw_isolation::capability::ResourceGrant;
        let home = tempfile::tempdir().unwrap();
        write_agent(home.path(), "caller");
        let mut owned = Arc::try_unwrap(call(home.path())).ok().unwrap();
        let mut record = owned.context.record().clone();
        record.tools = List::new(vec![Text::new("inspect_branches".into()).unwrap()]).unwrap();
        record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
            ResourceGrant::Workspace { handle: Text::new("employee-work".into()).unwrap() }]).unwrap();
        owned.context = RequestContext::from_verified_record(record);
        owned.forks = Some(Arc::new(HostForkService));
        ISOLATED_CALL.scope(Arc::new(owned), async {
            let arguments = json!({"fork_id":"opaque"});
            let disabled = crate::mcp_fork::handle_inspect_branches(&arguments, home.path(), "caller").await;
            assert_eq!(disabled["isError"], true);
            std::fs::write(home.path().join("agents/caller/agent.toml"),
                "[agent]\nname='caller'\nstatus='active'\n[fork]\nenabled=true\n").unwrap();
            let enabled = crate::mcp_fork::handle_inspect_branches(&arguments, home.path(), "caller").await;
            assert_ne!(enabled["isError"], true, "{enabled}");
            assert!(text(&enabled).contains("host-owned-store"));
            let wrong = crate::mcp_fork::handle_inspect_branches(&arguments, home.path(), "other").await;
            assert_eq!(wrong["isError"], true);
            assert!(!home.path().join("fork_store.db").exists());
        }).await;
    }

}
