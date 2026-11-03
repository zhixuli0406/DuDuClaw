//! Dashboard work admission with real accounts holding Operator bindings.
use super::*;

const SKILL: &str = "zz-main-admission";

fn ok(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("expected success: {other:?}"),
    }
}

fn refused(frame: WsFrame) {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => {
            assert_eq!(error.as_str(), Some("主代理僅管理員可以使用。"));
        }
        other => panic!("expected main refusal: {other:?}"),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    handler: MethodHandler,
    employee: UserContext,
    admin: UserContext,
}

async fn fixture(main_role: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let handler = MethodHandler::new(home.clone()).await;
    handler.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    let system = UserContext::admin_fallback();
    for (id, role) in [("a1", if main_role { "main" } else { "specialist" }), ("z1", "specialist")] {
        ok(handler.handle_agents_create_as(json!({"name": id, "role": role}), Some(&system)).await);
    }
    std::fs::write(home.join("config.toml"), if main_role { "" } else { "[general]\ndefault_agent = \"a1\"\n" }).unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(home.join("config.toml")).unwrap().write_all(b"\n[goal_loop]\nsteering_enabled = true\n").unwrap();
    std::fs::create_dir_all(home.join("skills")).unwrap();
    std::fs::write(home.join(format!("skills/{SKILL}.md")), format!("---\nname: {SKILL}\ndescription: Check admission\n---\nProduce a report.\n")).unwrap();
    handler.registry.write().await.scan().await.unwrap();
    let store = Arc::new(TaskStore::open(&home).unwrap());
    for (id, agent) in [("existing-main", "a1"), ("existing-normal", "z1")] {
        let mut task = TaskRow::new(id.into(), "Test".into(), "Test work".into(), "medium".into(), agent.into(), "system".into());
        task.goal_mode = true;
        store.insert_task(&task).await.unwrap();
    }
    handler.set_task_store(store).await;
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let employee = db.create_user("main-operator@test.invalid", "Operator", "isolated-test-password", UserRole::Employee).unwrap();
    for agent in ["a1", "z1"] { db.bind_agent(&employee.id, agent, AccessLevel::Operator).unwrap(); }
    let admin = db.create_user("main-admin@test.invalid", "Admin", "isolated-test-password", UserRole::Admin).unwrap();
    let employee = db.live_user_context(&employee.id).unwrap().unwrap();
    let admin = db.live_user_context(&admin.id).unwrap().unwrap();
    Fixture { _dir: dir, home, handler, employee, admin }
}

fn requests(target: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("agents.delegate", json!({"agent_id": target, "prompt": "Test admission"})),
        ("tasks.create", json!({"assigned_to": target, "title": "Test admission"})),
        ("tasks.goal_create", json!({"agent_id": target, "description": "Test admission"})),
        ("tasks.update", json!({"task_id": "existing-normal", "assigned_to": target})),
        ("tasks.assign", json!({"task_id": "existing-normal", "agent_id": target})),
        ("tasks.create", json!({"assigned_to": target, "title": "Scheduled admission", "schedule": "daily 09:00"})),
        ("tasks.goal_create", json!({"agent_id": target, "description": "Scheduled admission", "schedule": "daily 09:00"})),
    ]
}

#[tokio::test]
async fn operator_binding_does_not_admit_main_role_or_configured_default() {
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        for (method, params) in requests("a1") {
            refused(f.handler.handle(method, params, &f.employee).await);
        }
        refused(f.handler.handle("tasks.update", json!({"task_id": "existing-main", "description": "New instructions"}), &f.employee).await);
        // A normal employee remains callable through every admitted route.
        for (method, params) in requests("z1") {
            ok(f.handler.handle(method, params, &f.employee).await);
        }
    }
}

#[tokio::test]
async fn work_admission_fails_closed_for_unreadable_and_malformed_config() {
    for unreadable in [true, false] {
        let f = fixture(false).await;
        if unreadable {
            std::fs::remove_file(f.home.join("config.toml")).unwrap();
            std::fs::create_dir(f.home.join("config.toml")).unwrap();
        } else {
            std::fs::write(f.home.join("config.toml"), "[general\ndefault_agent = ").unwrap();
        }
        for (method, params) in requests("z1") {
            refused(f.handler.handle(method, params, &f.employee).await);
        }
        refused(f.handler.handle("tasks.skill_assign", json!({"skill_name": SKILL}), &f.employee).await);
        for action in ["retry", "continue"] {
            refused(f.handler.handle("tasks.goal_decide", json!({"task_id": "existing-normal", "action": action, "message": "Continue this work"}), &f.employee).await);
        }
        refused(f.handler.handle("tasks.steer", json!({"task_id": "existing-normal", "body": "Prepare a report", "client_request_id": "unreadable-direction"}), &f.employee).await);
    }
}

#[tokio::test]
async fn admin_and_legacy_token_keep_main_work_admission() {
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        let fallback = UserContext::admin_fallback();
        for ctx in [&f.admin, &fallback] {
            for (method, params) in requests("a1") {
                let scheduled = params.get("schedule").is_some();
                let frame = f.handler.handle(method, params, ctx).await;
                if ctx.user_id == "system" && scheduled {
                    // Token auth has no account to revalidate at fire time.
                    assert!(matches!(frame, WsFrame::Response { ok: false, error: Some(ref e), .. }
                        if e.as_str() == Some("account_required: a scheduled assignment needs a signed-in account")));
                } else {
                    ok(frame);
                }
            }
            let result = ok(f.handler.handle("tasks.skill_assign", json!({"skill_name": SKILL}), ctx).await);
            assert_eq!(result["runner"]["agent_id"], "a1");
        }
    }
}

#[tokio::test]
async fn skill_assign_skips_main_and_refuses_when_only_main_is_enabled() {
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        let result = ok(f.handler.handle("tasks.skill_assign", json!({"skill_name": SKILL}), &f.employee).await);
        assert_eq!(result["runner"]["agent_id"], "z1");
        duduclaw_agent::skill_state::set_disabled(&f.home, "z1", &[SKILL.into()], true).unwrap();
        assert!(matches!(f.handler.handle("tasks.skill_assign", json!({"skill_name": SKILL}), &f.employee).await, WsFrame::Response { ok: false, .. }));
    }
}

#[tokio::test]
async fn existing_main_work_cannot_be_retried_continued_or_steered() {
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        let store = TaskStore::open(&f.home).unwrap();
        for action in ["retry", "continue"] {
            let status = if action == "retry" { "needs_human" } else { "done" };
            store.update_task("existing-main", &json!({"status": status})).await.unwrap();
            store.update_task("existing-normal", &json!({"status": status})).await.unwrap();
            refused(f.handler.handle("tasks.goal_decide", json!({"task_id": "existing-main", "action": action, "message": "Continue this work"}), &f.employee).await);
            assert_eq!(store.get_task("existing-main").await.unwrap().unwrap().status, status);
            ok(f.handler.handle("tasks.goal_decide", json!({"task_id": "existing-normal", "action": action, "message": "Continue this work"}), &f.employee).await);
            assert_eq!(store.get_task("existing-normal").await.unwrap().unwrap().status, "pending");
            let fallback = UserContext::admin_fallback();
            for ctx in [&f.admin, &fallback] {
                store.update_task("existing-main", &json!({"status": status})).await.unwrap();
                ok(f.handler.handle("tasks.goal_decide", json!({"task_id": "existing-main", "action": action, "message": "Continue this work"}), ctx).await);
            }
        }
        refused(f.handler.handle("tasks.steer", json!({"task_id": "existing-main", "body": "Prepare a report", "client_request_id": "main-direction"}), &f.employee).await);
        ok(f.handler.handle("tasks.steer", json!({"task_id": "existing-normal", "body": "Prepare a report", "client_request_id": "normal-direction"}), &f.employee).await);
        let fallback = UserContext::admin_fallback();
        for (index, ctx) in [&f.admin, &fallback].into_iter().enumerate() {
            ok(f.handler.handle("tasks.steer", json!({"task_id": "existing-main", "body": "Prepare a report", "client_request_id": format!("admin-direction-{index}")}), ctx).await);
        }
    }
}

#[tokio::test]
async fn manager_controls_cannot_resume_or_trigger_main_work() {
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        let db = UserDb::new(&f.home.join("users.db")).unwrap();
        let manager = db.create_user("main-manager@test.invalid", "Manager", "isolated-test-password", UserRole::Manager).unwrap();
        for agent in ["a1", "z1"] { db.bind_agent(&manager.id, agent, AccessLevel::Operator).unwrap(); }
        let manager = db.live_user_context(&manager.id).unwrap().unwrap();
        for method in ["agents.resume", "autonomy.resume", "heartbeat.trigger"] {
            refused(f.handler.handle(method, json!({"agent_id": "a1"}), &manager).await);
            let normal = f.handler.handle(method, json!({"agent_id": "z1"}), &manager).await;
            if method == "heartbeat.trigger" {
                assert!(matches!(normal, WsFrame::Response { ok: false, error: Some(ref e), .. } if e.as_str() == Some("Heartbeat scheduler not started")));
            } else {
                ok(normal);
            }
            let fallback = UserContext::admin_fallback();
            for ctx in [&f.admin, &fallback] {
                let frame = f.handler.handle(method, json!({"agent_id": "a1"}), ctx).await;
                if method == "heartbeat.trigger" {
                    assert!(matches!(frame, WsFrame::Response { ok: false, error: Some(ref e), .. } if e.as_str() == Some("Heartbeat scheduler not started")));
                } else {
                    ok(frame);
                }
            }
        }
        std::fs::remove_file(f.home.join("config.toml")).unwrap();
        std::fs::create_dir(f.home.join("config.toml")).unwrap();
        for method in ["agents.resume", "autonomy.resume", "heartbeat.trigger"] {
            refused(f.handler.handle(method, json!({"agent_id": "z1"}), &manager).await);
        }
    }
}

#[tokio::test]
async fn resuming_an_existing_main_schedule_is_refused() {
    use crate::assign_schedule::{manage, AssignKind, AssignPayload, Creator, NewSchedule, ScheduleSpec};
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        let store = TaskStore::open(&f.home).unwrap();
        let creator = Creator::new(&f.employee.user_id, false).unwrap();
        for agent in ["a1", "z1"] {
            // Model a schedule saved before this hotfix. RPC admission must
            // reject resuming it even though its creator remains Operator.
            let row = manage::create(&f.home, &store, &creator, NewSchedule {
                agent_id: "z1".into(), kind: AssignKind::Task,
                payload: AssignPayload { title: "Scheduled work".into(), ..Default::default() },
                spec: ScheduleSpec::Daily { hour: 9, minute: 0 },
                timezone: Some("Asia/Taipei".into()), created_via: "dashboard".into(), source: None,
            }, Utc::now()).await.unwrap();
            // Seed the legacy target directly: current creation correctly
            // refuses a non-Admin main schedule before it can be persisted.
            rusqlite::Connection::open(store.db_path()).unwrap().execute(
                "UPDATE assign_schedules SET agent_id=?1 WHERE id=?2",
                rusqlite::params![agent, row.id],
            ).unwrap();
            ok(f.handler.handle("assign_schedules.pause", json!({"id": row.id}), &f.employee).await);
            let resumed = f.handler.handle("assign_schedules.resume", json!({"id": row.id}), &f.employee).await;
            if agent == "a1" {
                refused(resumed);
                assert_eq!(store.get_assign_schedule(&row.id).await.unwrap().unwrap().status, "paused");
            } else {
                ok(resumed);
                assert_eq!(store.get_assign_schedule(&row.id).await.unwrap().unwrap().status, "active");
                // Resume is a paused -> active transition, not an idempotent
                // operation. Give the Admin check its own paused schedule.
                ok(f.handler.handle("assign_schedules.pause", json!({"id": row.id}), &f.employee).await);
            }
            assert_eq!(store.get_assign_schedule(&row.id).await.unwrap().unwrap().status, "paused");
            ok(f.handler.handle("assign_schedules.resume", json!({"id": row.id}), &f.admin).await);
            assert_eq!(store.get_assign_schedule(&row.id).await.unwrap().unwrap().status, "active");
            let config_path = f.home.join("config.toml");
            let config = std::fs::read_to_string(&config_path).unwrap();
            for unreadable in [true, false] {
                if unreadable {
                    std::fs::remove_file(&config_path).unwrap();
                    std::fs::create_dir(&config_path).unwrap();
                } else {
                    std::fs::write(&config_path, "[broken").unwrap();
                }
                refused(f.handler.handle("assign_schedules.resume", json!({"id": row.id}), &f.employee).await);
                if unreadable { std::fs::remove_dir(&config_path).unwrap(); }
                std::fs::write(&config_path, &config).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn personal_non_admin_is_refused_main_but_admin_keeps_admission() {
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        f.handler.set_edition_override(Some(duduclaw_core::EditionProfile::Personal)).await;
        refused(f.handler.deny_main_for_non_admin(&f.employee, "a1").await.unwrap_err());
        refused(f.handler.handle("tasks.create", json!({"assigned_to": "a1", "title": "Personal main work"}), &f.employee).await);
        ok(f.handler.handle("tasks.create", json!({"assigned_to": "a1", "title": "Personal admin work"}), &f.admin).await);
        std::fs::write(f.home.join("config.toml"), "[broken").unwrap();
        refused(f.handler.deny_main_for_non_admin(&f.employee, "a1").await.unwrap_err());
        assert!(f.handler.deny_main_for_non_admin(&f.admin, "a1").await.is_ok());
    }
}

#[tokio::test]
async fn agents_update_refuses_all_main_updates_even_with_owner_access() {
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        let db = UserDb::new(&f.home.join("users.db")).unwrap();
        for agent in ["a1", "z1"] {
            db.bind_agent(&f.employee.user_id, agent, AccessLevel::Owner).unwrap();
            ok(f.handler.handle("agents.update", json!({"agent_id": agent, "status": "paused"}), &f.admin).await);
        }
        let owner = db.live_user_context(&f.employee.user_id).unwrap().unwrap();
        for edition in [duduclaw_core::EditionProfile::Enterprise, duduclaw_core::EditionProfile::Personal] {
            f.handler.set_edition_override(Some(edition)).await;
            let path = f.home.join("agents/a1/agent.toml");
            let before = std::fs::read(&path).unwrap();
            for fields in [
                json!({"heartbeat_enabled": true}),
                json!({"heartbeat_cron": "*/1 * * * *"}),
                json!({"status": "active"}),
            ] {
                let mut params = fields;
                params["agent_id"] = json!("a1");
                refused(f.handler.handle("agents.update", params, &owner).await);
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
            assert_eq!(f.handler.registry.read().await.get("a1").unwrap().config.agent.status, duduclaw_core::types::AgentStatus::Paused);
            ok(f.handler.handle("agents.update", json!({"agent_id": "z1", "heartbeat_enabled": true}), &owner).await);
            ok(f.handler.handle("agents.update", json!({"agent_id": "a1", "heartbeat_enabled": true}), &f.admin).await);
            ok(f.handler.handle("agents.update", json!({"agent_id": "a1", "status": "paused"}), &f.admin).await);
        }
    }
}

#[tokio::test]
async fn every_approval_decision_on_main_is_refused_without_mutation() {
    use crate::approval::{ApprovalBroker, ApprovalStatus};
    for main_role in [true, false] {
        let f = fixture(main_role).await;
        let broker = ApprovalBroker::open(&f.home).unwrap();
        let db = UserDb::new(&f.home.join("users.db")).unwrap();
        let user = db.create_user("approval-manager@test.invalid", "Manager", "isolated-test-password", UserRole::Manager).unwrap();
        for agent in ["a1", "z1"] { db.bind_agent(&user.id, agent, AccessLevel::Operator).unwrap(); }
        let manager = db.live_user_context(&user.id).unwrap().unwrap();
        for approve in [true, false] {
            for kind in ["mcp_tool", "discovery", crate::responsibility::DECISION_KIND] {
                let id = broker.request("a1", kind, "Main decision", json!({}), 300).await.unwrap();
                let before = serde_json::to_value(broker.get(&id).await.unwrap().unwrap()).unwrap();
                refused(f.handler.handle("approvals.decide", json!({"id": id.as_str(), "approve": approve}), &manager).await);
                assert_eq!(serde_json::to_value(broker.get(&id).await.unwrap().unwrap()).unwrap(), before);
            }
            // Normal Operator-bound employees still permit both decisions;
            // Admins retain the same decisions on main.
            for (agent, ctx) in [("z1", &manager), ("a1", &f.admin)] {
                let id = broker.request(agent, "mcp_tool", "Allowed decision", json!({}), 300).await.unwrap();
                ok(f.handler.handle("approvals.decide", json!({"id": id.as_str(), "approve": approve}), ctx).await);
                let row = broker.get(&id).await.unwrap().unwrap();
                assert_eq!(row.status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
            }
        }
    }
}

#[tokio::test]
async fn approval_without_agent_requires_durable_task_attribution_for_non_admin() {
    use crate::approval::{ApprovalBroker, ApprovalStatus};
    let f = fixture(true).await;
    let broker = ApprovalBroker::open(&f.home).unwrap();
    let db = UserDb::new(&f.home.join("users.db")).unwrap();
    let user = db.create_user("empty-approval-manager@test.invalid", "Manager", "isolated-test-password", UserRole::Manager).unwrap();
    for agent in ["a1", "z1"] { db.bind_agent(&user.id, agent, AccessLevel::Operator).unwrap(); }
    let manager = db.live_user_context(&user.id).unwrap().unwrap();
    for approve in [true, false] {
        for task in [None, Some("missing-task"), Some("existing-main"), Some("existing-normal")] {
            let payload = task.map_or_else(|| json!({"agent_id": "z1"}), |task| json!({"task_id": task}));
            let id = broker.request("", "mcp_tool", "Legacy attribution", payload, 300).await.unwrap();
            let before = serde_json::to_value(broker.get(&id).await.unwrap().unwrap()).unwrap();
            let frame = f.handler.handle("approvals.decide", json!({"id": id.as_str(), "approve": approve}), &manager).await;
            if task == Some("existing-normal") {
                ok(frame);
                assert_eq!(broker.get(&id).await.unwrap().unwrap().status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
            } else {
                if task == Some("existing-main") { refused(frame); } else {
                    assert!(matches!(frame, WsFrame::Response { ok: false, error: Some(ref error), .. } if error.as_str() == Some(PERMISSION_DENIED)));
                }
                assert_eq!(serde_json::to_value(broker.get(&id).await.unwrap().unwrap()).unwrap(), before);
                let admin_frame = f.handler.handle("approvals.decide", json!({"id": id.as_str(), "approve": approve}), &f.admin).await;
                if task == Some("missing-task") {
                    // Existing task privacy still refuses a dangling task ref,
                    // even for an Admin. The main gate does not replace it.
                    assert!(matches!(admin_frame, WsFrame::Response { ok: false, error: Some(ref error), .. } if error.as_str() == Some(PERMISSION_DENIED)));
                    assert_eq!(serde_json::to_value(broker.get(&id).await.unwrap().unwrap()).unwrap(), before);
                } else {
                    ok(admin_frame);
                }
            }
        }
    }
}

#[tokio::test]
async fn bound_approval_decisions_gate_main_before_the_early_return() {
    use crate::approval::{ApprovalBroker, ApprovalStatus, DecisionContext, ExecutionBinding, RequestKind, payload_hash, policy_revision};
    let f = fixture(true).await;
    let db = UserDb::new(&f.home.join("users.db")).unwrap();
    let user = db.create_user("bound-approval-manager@test.invalid", "Manager", "isolated-test-password", UserRole::Manager).unwrap();
    for agent in ["a1", "z1"] { db.bind_agent(&user.id, agent, AccessLevel::Operator).unwrap(); }
    let manager = db.live_user_context(&user.id).unwrap().unwrap();
    let broker = ApprovalBroker::open(&f.home).unwrap();
    for approve in [true, false] {
        for agent in ["a1", "z1"] {
            let payload = json!({"name": "test_effect"});
            let binding = ExecutionBinding {
                schema_version: 1,
                run_id: uuid::Uuid::new_v4().to_string(),
                run_origin_kind: "ingress".into(),
                actor_principal: agent.into(),
                decision_context: DecisionContext {
                    channel: "dashboard".into(), account_id: "fixture".into(),
                    conversation_id: "fixture".into(), principal_id: manager.user_id.clone(),
                },
                task_id: None, task_revision: None, task_snapshot_hash: None,
                payload_hash: payload_hash(&payload),
                policy_revision: policy_revision(&f.home, agent).unwrap(),
                cwd: None, environment_hash: payload_hash(&json!({})), file_hashes: Default::default(),
                expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
                resume_handler: "live_only_v1".into(), resume_version: 1,
            };
            let id = broker.request_bound(RequestKind::Approval, agent, "Bound effect", payload, binding).await.unwrap();
            let before = serde_json::to_value(broker.get(&id).await.unwrap().unwrap()).unwrap();
            let frame = f.handler.handle("approvals.decide", json!({"id": id.as_str(), "approve": approve}), &manager).await;
            if agent == "a1" {
                refused(frame);
                assert_eq!(serde_json::to_value(broker.get(&id).await.unwrap().unwrap()).unwrap(), before);
                ok(f.handler.handle("approvals.decide", json!({"id": id.as_str(), "approve": approve}), &f.admin).await);
            } else { ok(frame); }
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
        }
    }
}

#[tokio::test]
async fn unavailable_identity_precedes_main_target_refusal() {
    let f = fixture(true).await;
    let db = UserDb::new(&f.home.join("users.db")).unwrap();
    db.set_user_status(&f.admin.user_id, duduclaw_auth::UserStatus::Suspended).unwrap();
    let frame = f.handler.deny_main_for_non_admin(&f.admin, "a1").await.unwrap_err();
    assert!(matches!(frame, WsFrame::Response { ok: false, error: Some(ref e), .. }
        if e.as_str() == Some("permission denied")));
}
