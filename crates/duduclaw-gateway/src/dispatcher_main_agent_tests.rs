//! Exercise the consumers without calling the MCP front-door checks.
use super::*;
use crate::message_queue::{MessageQueue, MessageStatus, QueueMessage};

struct Fixture {
    home: tempfile::TempDir,
    registry: Arc<RwLock<AgentRegistry>>,
    employee: String,
    admin: String,
}

impl Fixture {
    async fn new(mode: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let employee = db.create_user("employee@test.invalid", "Employee", "isolated-test-password",
            duduclaw_auth::UserRole::Employee).unwrap().id;
        let admin = db.create_user("admin@test.invalid", "Admin", "isolated-test-password",
            duduclaw_auth::UserRole::Admin).unwrap().id;
        // Explicit Owner bindings must not authorize main, in either mode.
        for agent in ["sales", "main", "configured"] {
            db.bind_agent(&employee, agent, duduclaw_auth::AccessLevel::Owner).unwrap();
        }
        for (name, role, reports_to) in [
            ("main", "main", ""), ("sales", "worker", "main"),
            ("configured", "worker", "main"),
        ] {
            crate::trigger_route::tests::write_agent(&home.path().join("agents"),
                name, name, "", role, "");
            let path = home.path().join("agents").join(name).join("agent.toml");
            let raw = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, raw.replace("reports_to = \"\"",
                &format!("reports_to = \"{reports_to}\"\ndepartment = \"sales\""))).unwrap();
        }
        std::fs::write(home.path().join("config.toml"), format!(
            "[general]\ndefault_agent = 'configured'\n[dispatch]\nenabled = true\n[delegation]\npolicy = 'department'\nhuman_principal = '{mode}'\n",
        )).unwrap();
        let mut registry = AgentRegistry::new(home.path().join("agents"));
        registry.scan().await.unwrap();
        assert_eq!(registry.list().len(), 3, "principal fixtures must load");
        Self { home, registry: Arc::new(RwLock::new(registry)),
            employee: format!("user:{employee}"), admin: format!("user:{admin}") }
    }

    fn message(&self, principal: Option<&str>, target: &str, sender: &str) -> QueueMessage {
        QueueMessage {
            id: uuid::Uuid::new_v4().to_string(), sender: sender.into(), target: target.into(),
            payload: "work".into(), status: MessageStatus::Pending, retry_count: 0,
            delegation_depth: 1, origin_agent: Some(sender.into()), sender_agent: Some(sender.into()),
            error: None, response: None, created_at: Utc::now().to_rfc3339(), acked_at: None,
            completed_at: None, reply_channel: None, turn_id: None, session_id: None,
            upstream_unknown: false, lane: None, on_behalf_of: principal.map(str::to_owned),
        }
    }
}

#[tokio::test]
async fn main_gate_precedes_record_mode_bindings_and_system_sender_exemptions() {
    for mode in ["record", "enforce"] {
        let f = Fixture::new(mode).await;
        for edition in [duduclaw_core::EditionProfile::Personal, duduclaw_core::EditionProfile::Enterprise] {
            crate::team_edition::record(f.home.path(), edition);
            for target in ["main", "configured", "default"] {
                for sender in [Some("sales"), Some("goal-loop-driver"), Some("heartbeat"), None] {
                    let result = crate::delegation_gate::gate_bus_dispatch(
                        f.home.path(), &f.registry, sender, None, target, "test-main", "sqlite_queue",
                        Some(&f.employee),
                    ).await;
                    assert!(matches!(result, crate::delegation_gate::GateOutcome::Deny(_)),
                        "{mode} {target} {sender:?}: {result:?}");
                }
            }
        }
    }
}

#[tokio::test]
async fn sqlite_and_bus_consumers_refuse_non_admin_even_without_mcp_check() {
    for mode in ["record", "enforce"] {
        let f = Fixture::new(mode).await;
        let queue = Arc::new(MessageQueue::open(f.home.path()).unwrap());
        crate::model_call_probe::set_dry_run(true);
        let before = crate::model_call_probe::calls();
        for target in ["main", "configured"] {
            let row = f.message(Some(&f.employee), target, "sales");
            queue.enqueue(&row).await.unwrap();
            poll_and_dispatch_sqlite(&queue, f.home.path(), &f.registry, None, None).await.unwrap();
            assert_eq!(queue.get_by_id(&row.id).await.unwrap().unwrap().status, MessageStatus::Failed);
            let bus = serde_json::json!({"type":"agent_message", "message_id":row.id,
                "agent_id":target, "payload":"work", "timestamp":Utc::now().to_rfc3339(),
                "sender_agent":"sales", "origin_agent":"sales", "spawned":true,
                "on_behalf_of":f.employee});
            std::fs::write(f.home.path().join("bus_queue.jsonl"), format!("{bus}\n")).unwrap();
            poll_and_dispatch(f.home.path(), &f.registry, None, None).await.unwrap();
            let output = std::fs::read_to_string(f.home.path().join("bus_queue.jsonl")).unwrap();
            let response: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
            assert_eq!(response["type"], "agent_response");
            assert_eq!(response["in_reply_to"], row.id);
        }
        assert_eq!(crate::model_call_probe::calls(), before, "denial must precede runtime");
        crate::model_call_probe::set_dry_run(false);
    }
}

#[tokio::test]
async fn admin_and_background_main_delegation_still_dispatch() {
    let f = Fixture::new("record").await;
    let queue = Arc::new(MessageQueue::open(f.home.path()).unwrap());
    crate::model_call_probe::set_dry_run(true);
    let before = crate::model_call_probe::calls();
    for (principal, sender) in [(Some(f.admin.as_str()), "sales"), (None, "cron"), (None, "heartbeat")] {
        let row = f.message(principal, "main", sender);
        queue.enqueue(&row).await.unwrap();
        poll_and_dispatch_sqlite(&queue, f.home.path(), &f.registry, None, None).await.unwrap();
        assert_eq!(queue.get_by_id(&row.id).await.unwrap().unwrap().status, MessageStatus::Done);
    }
    assert_eq!(crate::model_call_probe::calls(), before + 3);
    crate::model_call_probe::set_dry_run(false);
}

#[tokio::test]
async fn mapped_channel_accounts_obey_main_gate_unmapped_senders_keep_existing_policy() {
    let f = Fixture::new("record").await;
    let user_id = f.employee.strip_prefix("user:").unwrap();
    let mapped = crate::on_behalf_of::principal_for_requester(f.home.path(),
        &duduclaw_knowledge::requester::RequesterIdentity::ChannelUser {
            channel: "line".into(), chat: "chat".into(), sender: "sender".into(), user_id: Some(user_id.into()),
        });
    for (principal, denied) in [(mapped.as_deref(), true), (Some("unmapped:line:0123456789abcdef"), false)] {
        let outcome = crate::delegation_gate::gate_bus_dispatch(f.home.path(), &f.registry,
            Some("sales"), None, "main", "mapped-channel", "bus_dispatch", principal).await;
        assert_eq!(matches!(outcome, crate::delegation_gate::GateOutcome::Deny(_)), denied);
    }
}

#[tokio::test]
async fn delegation_rechecks_role_after_queue_authorization_before_runtime() {
    let f = Fixture::new("record").await;
    let target = format!("sales-race-{}", uuid::Uuid::new_v4().simple());
    crate::trigger_route::tests::write_agent(&f.home.path().join("agents"),
        &target, &target, "", "worker", "");
    let path = f.home.path().join("agents").join(&target).join("agent.toml");
    f.registry.write().await.scan().await.unwrap();
    let queue = Arc::new(MessageQueue::open(f.home.path()).unwrap());
    let row = f.message(Some(&f.employee), &target, "cron");
    queue.enqueue(&row).await.unwrap();
    let barrier = crate::responsibility::test_hooks::install("autonomy_dispatch_handoff", &target);
    crate::model_call_probe::set_dry_run(true);
    let before = crate::model_call_probe::calls();
    tokio::join!(async {
        poll_and_dispatch_sqlite(&queue, f.home.path(), &f.registry, None, None).await.unwrap();
    }, async {
        barrier.arrived.wait().await;
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, raw.replace("role = \"worker\"", "role = \"main\"")).unwrap();
        f.registry.write().await.scan().await.unwrap();
        barrier.release.wait().await;
    });
    assert_eq!(crate::model_call_probe::calls(), before);
    assert_eq!(queue.get_by_id(&row.id).await.unwrap().unwrap().status, MessageStatus::Failed);
    crate::model_call_probe::set_dry_run(false);
}

#[tokio::test]
async fn named_principal_and_default_configuration_fail_closed() {
    let f = Fixture::new("record").await;
    let registry = f.registry.read().await;
    let main = registry.get("main").unwrap();
    for principal in ["user:missing", "unknown:rejected-principal", ""] {
        assert!(crate::on_behalf_of::access::check_loaded_main_target(
            f.home.path(), Some(principal), main,
        ).await.is_err());
    }
    let ordinary = registry.get("sales").unwrap();
    for config in ["[general", "general = []", "[general]\ndefault_agent = 42\n"] {
        std::fs::write(f.home.path().join("config.toml"), config).unwrap();
        assert!(crate::on_behalf_of::access::check_loaded_main_target(
            f.home.path(), Some(&f.employee), ordinary,
        ).await.is_err());
        assert!(crate::on_behalf_of::access::check_loaded_main_target(
            f.home.path(), Some(&f.admin), main,
        ).await.is_ok(), "Admin keeps its existing behavior");
    }
}

#[tokio::test]
async fn taskspec_restores_principal_for_every_step_and_keeps_legacy_background() {
    use crate::task_spec::{Step, StepStatus, TaskSpec, TaskStatus};
    let f = Fixture::new("record").await;
    let step = |agent: &str| Step {
        id: 0, description: "work".into(), agent: agent.into(), depends_on: vec![],
        acceptance_criteria: vec![], status: StepStatus::Pending, result: None, retry_count: 0,
    };
    let dir = f.home.path().join("agents/sales");
    crate::model_call_probe::set_dry_run(true);
    let mut first = step("sales");
    first.id = 0;
    let mut second = step("main");
    second.id = 1;
    second.depends_on = vec![0];
    let mut spec = TaskSpec::new("sales", "Report then delegate", vec![first, second]);
    spec.on_behalf_of = Some(f.employee.clone());
    let before = crate::model_call_probe::calls();
    let error = dispatch_taskspec(f.home.path(), &f.registry, &mut spec, &dir).await.unwrap_err();
    assert!(error.contains("主代理"), "{error}");
    assert_eq!(crate::model_call_probe::calls() - before, 1, "only the ordinary step runs");
    assert_eq!(spec.steps[0].status, StepStatus::Passed);
    assert_eq!(spec.status, TaskStatus::Failed);
    assert_eq!(TaskSpec::load(&dir, &spec.task_id).unwrap().on_behalf_of, Some(f.employee.clone()));
    assert!(crate::on_behalf_of::current().is_none(), "scope must not leak to the next task");
    for principal in [None, Some(f.admin.clone())] {
        let mut spec = TaskSpec::new("main", "Legacy or Admin work", vec![step("")]);
        spec.on_behalf_of = principal;
        let before = crate::model_call_probe::calls();
        dispatch_taskspec(f.home.path(), &f.registry, &mut spec, &f.home.path().join("agents/main"))
            .await.unwrap();
        assert_eq!(crate::model_call_probe::calls() - before, 1);
        assert_eq!(spec.status, TaskStatus::Completed);
    }
    crate::model_call_probe::set_dry_run(false);
}

#[tokio::test]
async fn handwritten_queues_without_sender_or_principal_are_refused_and_audited() {
    for mode in ["record", "enforce"] {
        let f = Fixture::new(mode).await;
        let queue = Arc::new(MessageQueue::open(f.home.path()).unwrap());
        crate::model_call_probe::set_dry_run(true);
        let before = crate::model_call_probe::calls();
        for target in ["main", "configured", "default"] {
            for blank in [false, true] {
                let id = uuid::Uuid::new_v4().to_string();
                // Deliberately bypass every producer and enqueue API. SQLite's
                // legacy sender column is NOT NULL, so the absent identity is "".
                let conn = rusqlite::Connection::open(f.home.path().join("message_queue.db")).unwrap();
                conn.execute(
                    "INSERT INTO message_queue(id,sender,target,payload,created_at,sender_agent,origin_agent)
                     VALUES (?1,'',?2,'work',?3,?4,?4)",
                    rusqlite::params![id, target, Utc::now().to_rfc3339(), blank.then_some("  ")],
                ).unwrap();
                drop(conn);
                poll_and_dispatch_sqlite(&queue, f.home.path(), &f.registry, None, None).await.unwrap();
                assert_eq!(queue.get_by_id(&id).await.unwrap().unwrap().status, MessageStatus::Failed);
                let mut bus = serde_json::json!({"type":"agent_message", "message_id":id,
                    "agent_id":target, "payload":"work", "timestamp":Utc::now().to_rfc3339()});
                if blank {
                    bus["sender_agent"] = serde_json::json!("  ");
                    bus["origin_agent"] = serde_json::json!("");
                }
                std::fs::write(f.home.path().join("bus_queue.jsonl"), format!("{bus}\n")).unwrap();
                poll_and_dispatch(f.home.path(), &f.registry, None, None).await.unwrap();
                let output = std::fs::read_to_string(f.home.path().join("bus_queue.jsonl")).unwrap();
                let response: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
                assert_eq!(response["type"], "agent_response");
                assert_eq!(response["in_reply_to"], id);
            }
        }
        assert_eq!(crate::model_call_probe::calls(), before, "handwritten rows must never reach runtime");
        crate::model_call_probe::set_dry_run(false);
        let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
        let denied: Vec<serde_json::Value> = audit.lines().map(|line| serde_json::from_str(line).unwrap())
            .filter(|event: &serde_json::Value| event["event_type"] == "delegation_denied").collect();
        assert_eq!(denied.len(), 12);
        assert!(denied.iter().all(|event| event["details"]["reason"] == "invalid_agent_id"));
        for rail in ["bus_dispatch", "sqlite_queue"] {
            assert_eq!(denied.iter().filter(|e| e["details"]["path_kind"] == rail).count(), 6);
        }
    }
}

#[tokio::test]
async fn employee_sender_without_principal_cannot_borrow_system_origin_for_main() {
    for mode in ["record", "enforce"] {
        let f = Fixture::new(mode).await;
        for policy in ["open", "department", "hierarchy"] {
            let config = format!("[general]\ndefault_agent = 'configured'\n[delegation]\npolicy = '{policy}'\nhuman_principal = '{mode}'\n");
            std::fs::write(f.home.path().join("config.toml"), config).unwrap();
            for target in ["main", "configured", "default"] {
                for (sender, origin) in [(Some("sales"), None), (Some("sales"), Some("cron")),
                    (None, Some("sales")), (Some("  "), Some("sales")), (Some("a2a-client"), None)] {
                    let result = crate::delegation_gate::gate_bus_dispatch(
                        f.home.path(), &f.registry, sender, origin, target,
                        "principal-free", "bus_dispatch", None,
                    ).await;
                    assert!(matches!(result, crate::delegation_gate::GateOutcome::Deny(_)),
                        "{mode} {policy} {target} {sender:?} {origin:?}: {result:?}");
                }
                for sender in duduclaw_core::SYSTEM_SENDERS {
                    let result = crate::delegation_gate::gate_bus_dispatch(
                        f.home.path(), &f.registry, Some(sender), None, target,
                        "system-background", "sqlite_queue", None,
                    ).await;
                    assert!(matches!(result, crate::delegation_gate::GateOutcome::Allow),
                        "established system sender {sender}: {result:?}");
                }
            }
        }
    }
}

#[tokio::test]
async fn origin_only_background_queue_cannot_gain_system_authority_after_promotion() {
    for promote_default in [false, true] {
        let f = Fixture::new("record").await;
        let target = format!("origin-race-{}", uuid::Uuid::new_v4().simple());
        crate::trigger_route::tests::write_agent(&f.home.path().join("agents"),
            &target, &target, "", "worker", "");
        let path = f.home.path().join("agents").join(&target).join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, raw.replace("reports_to = \"\"",
            "reports_to = \"sales\"\ndepartment = \"sales\"")).unwrap();
        f.registry.write().await.scan().await.unwrap();
        let queue = Arc::new(MessageQueue::open(f.home.path()).unwrap());
        let mut row = f.message(None, &target, "sales");
        row.sender_agent = None;
        // The generic sender column is not the delegation identity. It must
        // not replace the admitted origin when the final runtime gate runs.
        row.sender = "cron".into();
        queue.enqueue(&row).await.unwrap();
        let barrier = crate::responsibility::test_hooks::install("autonomy_dispatch_handoff", &target);
        crate::model_call_probe::set_dry_run(true);
        let before = crate::model_call_probe::calls();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(async {
                poll_and_dispatch_sqlite(&queue, f.home.path(), &f.registry, None, None).await.unwrap();
            }, async {
                barrier.arrived.wait().await;
                if promote_default {
                    std::fs::write(f.home.path().join("config.toml"), format!(
                        "[general]\ndefault_agent = '{target}'\n[dispatch]\nenabled = true\n",
                    )).unwrap();
                } else {
                    let raw = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(&path, raw.replace("role = \"worker\"", "role = \"main\"")).unwrap();
                    f.registry.write().await.scan().await.unwrap();
                }
                barrier.release.wait().await;
            });
        }).await.expect("admitted employee-origin row must reach the promotion seam");
        assert_eq!(crate::model_call_probe::calls(), before);
        assert_eq!(queue.get_by_id(&row.id).await.unwrap().unwrap().status, MessageStatus::Failed);
        crate::model_call_probe::set_dry_run(false);
    }
}
