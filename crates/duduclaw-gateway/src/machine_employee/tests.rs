use super::*;
use std::sync::Arc;

use duduclaw_agent::AgentRegistry;
use duduclaw_auth::{AccessLevel, UserContext, UserDb, UserRole};
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::protocol::WsFrame;

struct Fixture {
    home: tempfile::TempDir,
    registry: Arc<RwLock<AgentRegistry>>,
    db: Arc<UserDb>,
    admin: UserContext,
    employee: UserContext,
    manager: UserContext,
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
    for (id, role) in [("operator", "main"), ("ordinary", "specialist")] {
        crate::trigger_route::tests::write_agent(
            &home.path().join("agents"), id, id, &format!("@{id}"), role,
            "\n[channels]\npublic_trigger = true\n",
        );
    }
    preset::install_system_operator_preset(home.path(), false).unwrap();
    preset::bind(home.path(), "operator", &home.path().join("agents/operator"),
        "system-operator", "test", "test").unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let account = |name: &str, role| {
        let user = db.create_user(&format!("{name}@test.invalid"), name, "Test-password-123!", role).unwrap();
        // Simulate bindings from before the restriction; they must survive.
        db.bind_agent(&user.id, "operator", AccessLevel::Owner).unwrap();
        db.bind_agent(&user.id, "ordinary", AccessLevel::Owner).unwrap();
        UserContext::from_live(&user, &db.effective_access(&user.id).unwrap().unwrap())
    };
    let admin = account("admin", UserRole::Admin);
    let employee = account("employee", UserRole::Employee);
    let manager = account("manager", UserRole::Manager);
    let mut reg = AgentRegistry::new(home.path().join("agents"));
    reg.scan().await.unwrap();
    Fixture { home, registry: Arc::new(RwLock::new(reg)), db, admin, employee, manager }
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("expected success: {other:?}"),
    }
}

fn refused(frame: WsFrame) {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } => {
            assert_eq!(error.as_str(), Some(REFUSAL));
        }
        other => panic!("expected machine-employee refusal: {other:?}"),
    }
}

#[tokio::test]
async fn server_resolved_preset_limits_binding_tasks_goals_and_callable_list() {
    let f = fixture().await;
    let h = crate::handlers::MethodHandler::new(f.home.path().to_path_buf()).await;
    h.set_user_db(f.db.clone(), Arc::new(duduclaw_auth::JwtConfig::new(b"isolated-machine-test-signing-key"))).await;
    h.set_task_store(Arc::new(crate::task_store::TaskStore::open(f.home.path()).unwrap())).await;
    crate::team_edition::record(f.home.path(), duduclaw_core::EditionProfile::Enterprise);
    // This is the same server-side predicate used by users.bind_agent,
    // tasks.create and agents.callable; exercise their handlers directly.
    for user in [&f.employee, &f.manager] {
        refused(h.handle_users_bind_agent(json!({"user_id":user.user_id,"agent_name":"operator"}), &f.admin).await);
        refused(h.handle_tasks_create(json!({"assigned_to":"operator","title":"run Bash"}), user).await);
        refused(h.handle_tasks_goal_create(json!({"agent_id":"operator","description":"run Bash"}), user).await);
        refused(h.handle_tasks_goal_create(json!({"agent_id":"operator","description":"run Bash", "schedule":"0 * * * *"}), user).await);
        let listed = payload(h.handle_agents_callable(user, json!({})).await);
        assert!(!listed["agents"].as_array().unwrap().iter().any(|a| a["name"] == "operator"));
        assert_ne!(listed["default_agent"], "operator");
        assert!(f.db.check_agent_access(&user.user_id, "operator").unwrap().is_some(), "old binding is retained");
        payload(h.handle_tasks_create(json!({"assigned_to":"ordinary","title":"authorized Bash"}), user).await);
    }
    payload(h.handle_users_bind_agent(json!({"user_id":f.admin.user_id,"agent_name":"operator"}), &f.admin).await);
    payload(h.handle_tasks_create(json!({"assigned_to":"operator","title":"Admin work"}), &f.admin).await);
    let listed = payload(h.handle_agents_callable(&f.admin, json!({})).await);
    assert!(listed["agents"].as_array().unwrap().iter().any(|a| a["name"] == "operator"));
}

#[tokio::test]
async fn system_operator_access_cannot_be_reached_through_default_public_or_existing_binding() {
    let f = fixture().await;
    let reg = f.registry.read().await;
    let agent = reg.get("operator").unwrap();
    assert!(requires_admin(agent));
    for user in [&f.employee, &f.manager] {
        let sender = crate::trigger_route::SenderAccess::from_effective(Arc::new(f.db.effective_access(&user.user_id).unwrap().unwrap()));
        assert!(!sender.may_call(agent, Some("operator")));
        assert_eq!(crate::trigger_route::assign::assign_right(&reg, &sender, "operator", Some("operator")),
            crate::trigger_route::assign::AssignRight::Denied);
        assert!(!crate::trigger_route::callable_ids(&reg, &sender, Some("operator")).contains(&"operator".to_string()));
        assert!(check_loaded(f.home.path(), agent, Some(&format!("user:{}", user.user_id))).is_err());
    }
    assert!(!crate::trigger_route::SenderAccess::unmapped().may_call(agent, Some("operator")));
    for principal in ["unmapped:line:owner", "unmapped:telegram:owner"] {
        assert!(check_loaded(f.home.path(), agent, Some(principal)).is_err());
        assert!(check_target(f.home.path(), "operator", Some(principal)).is_err());
    }
    assert!(check_loaded(f.home.path(), agent, None).is_ok(), "unattended work stays permitted");
    assert!(check_loaded(f.home.path(), agent, Some("bad-principal")).is_err());
    assert!(check_loaded(f.home.path(), agent, Some(&format!("user:{}", f.admin.user_id))).is_ok());
    crate::team_edition::record(f.home.path(), duduclaw_core::EditionProfile::Personal);
    assert!(requires_admin(agent), "a lapsed license must not reopen a multi-account deployment");
    assert!(!crate::trigger_route::SenderAccess::unmapped().may_call(agent, Some("operator")));
    for user in [&f.employee, &f.manager] {
        let sender = crate::trigger_route::SenderAccess::from_effective(Arc::new(f.db.effective_access(&user.user_id).unwrap().unwrap()));
        assert!(!sender.may_call(agent, Some("operator")));
        assert!(!crate::trigger_route::callable_ids(&reg, &sender, Some("operator")).contains(&"operator".to_string()));
        let principal = format!("user:{}", user.user_id);
        assert!(check_loaded(f.home.path(), agent, Some(&principal)).is_err());
        assert!(check_target(f.home.path(), "operator", Some(&principal)).is_err());
    }
    for principal in ["unmapped:line:owner", "unmapped:telegram:owner"] {
        assert!(check_loaded(f.home.path(), agent, Some(principal)).is_err());
        assert!(check_target(f.home.path(), "operator", Some(principal)).is_err());
    }
    for principal in [None, Some(format!("user:{}", f.admin.user_id))] {
        assert!(check_loaded(f.home.path(), agent, principal.as_deref()).is_ok());
        assert!(check_target(f.home.path(), "operator", principal.as_deref()).is_ok());
    }
}

#[tokio::test]
async fn queued_work_rechecks_even_internal_senders_and_record_mode() {
    let f = fixture().await;
    for sender in [None, Some("goal-loop-driver"), Some("heartbeat-scheduler"), Some("ordinary")] {
        for principal in [Some(format!("user:{}", f.employee.user_id)), Some("malformed".into()), Some(String::new())] {
            let outcome = crate::delegation_gate::gate_bus_dispatch(
                f.home.path(), &f.registry, sender, None, "operator", "test-row", "sqlite_queue", principal.as_deref(),
            ).await;
            assert!(matches!(outcome, crate::delegation_gate::GateOutcome::Deny(_)));
        }
    }
    for principal in [None, Some(format!("user:{}", f.admin.user_id))] {
        let outcome = crate::delegation_gate::gate_bus_dispatch(
            f.home.path(), &f.registry, None, None, "operator", "old-row", "bus_queue", principal.as_deref(),
        ).await;
        assert!(!matches!(outcome, crate::delegation_gate::GateOutcome::Deny(_)));
    }
    let principal = format!("user:{}", f.employee.user_id);
    let gate_default = || crate::delegation_gate::gate_bus_dispatch(
        f.home.path(), &f.registry, None, None, "default", "alias-row", "sqlite_queue", Some(&principal),
    );
    assert!(matches!(gate_default().await, crate::delegation_gate::GateOutcome::Deny(_)));
    // Same main-role alias after unbinding the machine preset is normal work.
    let bindings = f.home.path().join("preset_bindings.toml");
    std::fs::write(bindings, "schema = 1\n").unwrap();
    f.registry.write().await.scan().await.unwrap();
    assert!(!matches!(gate_default().await, crate::delegation_gate::GateOutcome::Deny(_)));
    assert!(check_target(f.home.path(), "eph-normal", Some(&principal)).is_ok(),
        "ordinary ephemeral targets keep the existing admission and identity checks");
}

#[tokio::test]
async fn bound_human_chat_stops_before_any_machine_employee_runtime() {
    let f = fixture().await;
    let sessions = Arc::new(crate::session::SessionManager::new(&f.home.path().join("sessions.db")).unwrap());
    let status = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let ctx = crate::channel_reply::ReplyContext::new(
        f.registry.clone(), f.home.path().to_path_buf(), sessions, status, tx,
    );
    for user in [&f.employee, &f.manager] {
        let reply = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
            user.user_id.clone(),
            crate::channel_reply::build_reply_for_agent(
                "run Bash", &ctx, "operator", "webchat:machine-test", &user.user_id, None,
            ),
        ).await;
        assert_eq!(reply, REFUSAL);
        let goal_reply = crate::chat_commands::with_goal_principal(
            Some((format!("user:{}", user.user_id), "webchat".into())),
            crate::chat_commands::handle_goal_create(
                &ctx, "webchat:machine-test", "operator", "run Bash", None, None, None, None, false,
            ),
        ).await;
        assert_eq!(goal_reply, REFUSAL);
    }
    assert!(crate::machine_employee::check_chat_goal(&ctx, "default", "webchat:machine-test").await.is_err(),
        "unmapped chat is not unattended work, including the default alias");
    assert!(crate::machine_employee::check_chat_goal(&ctx, "operator", "cron:machine-test").await.is_ok());
    let admin_goal = crate::chat_commands::with_goal_principal(
        Some((format!("user:{}", f.admin.user_id), "webchat".into())),
        crate::chat_commands::handle_goal_create(
            &ctx, "webchat:machine-test", "operator", "Admin goal", None, None, None, None, false,
        ),
    ).await;
    assert_ne!(admin_goal, REFUSAL);
    let tasks = crate::task_store::TaskStore::open(f.home.path()).unwrap()
        .list_tasks_filtered(None, Some("operator"), None, None).await.unwrap();
    assert_eq!(tasks.len(), 1, "only the Admin goal may be created");
    assert_eq!(tasks[0].on_behalf_of.as_deref(), Some(format!("user:{}", f.admin.user_id).as_str()));
}

#[tokio::test]
async fn shared_goal_core_refuses_human_machine_work_before_inserting() {
    let f = fixture().await;
    let store = crate::task_store::TaskStore::open(f.home.path()).unwrap();
    for principal in [format!("user:{}", f.employee.user_id), "malformed".into()] {
        let err = crate::goal_create_core::create_goal_task(f.home.path(), &store,
            crate::goal_create_core::GoalCreateRequest {
                agent_id: "operator".into(), description: "run Bash".into(),
                on_behalf_of: Some(principal), ..Default::default()
            },
        ).await.unwrap_err();
        assert_eq!(err, REFUSAL);
    }
    assert!(store.list_tasks_filtered(None, Some("operator"), None, None).await.unwrap().is_empty());
}

#[tokio::test]
async fn mirror_flags_do_not_choose_the_preset_and_unresolved_binding_stays_restricted() {
    let f = fixture().await;
    let principal = format!("user:{}", f.employee.user_id);
    let ordinary = f.home.path().join("agents/ordinary/agent.toml");
    let raw = std::fs::read_to_string(&ordinary).unwrap();
    std::fs::write(&ordinary, format!("{raw}\n[preset]\nid = 'system-operator'\n")).unwrap();
    assert!(check_target(f.home.path(), "ordinary", Some(&principal)).is_ok());
    // Removing the preset file creates an Unresolved server binding, never
    // an unbound employee or permission to run its raw configuration.
    std::fs::remove_file(f.home.path().join("presets/system-operator/preset.toml")).unwrap();
    assert!(check_target(f.home.path(), "operator", Some(&principal)).is_err());
    assert!(check_target(f.home.path(), "operator", None).is_ok());
}

#[tokio::test]
async fn personal_machine_access_requires_single_owner_evidence() {
    for scenario in ["missing", "empty", "admin", "employee", "corrupt", "suspended", "offboarded", "list-error"] {
        let home = tempfile::tempdir().unwrap();
        crate::trigger_route::tests::write_agent(
            &home.path().join("agents"), "operator", "Operator", "@operator", "main", "",
        );
        preset::install_system_operator_preset(home.path(), false).unwrap();
        preset::bind(home.path(), "operator", &home.path().join("agents/operator"),
            "system-operator", "test", "test").unwrap();
        let path = home.path().join("users.db");
        if scenario == "corrupt" {
            std::fs::write(&path, "not a SQLite database").unwrap();
        } else if scenario != "missing" {
            let db = UserDb::new(&path).unwrap();
            if scenario != "empty" {
                let role = if scenario == "employee" { UserRole::Employee } else { UserRole::Admin };
                db.create_user("owner@test.invalid", "Owner", "Test-password-123!", role).unwrap();
            }
            if matches!(scenario, "suspended" | "offboarded") {
                crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
                let extra = db.create_user("extra@test.invalid", "Extra", "Test-password-123!", UserRole::Employee).unwrap();
                rusqlite::Connection::open(&path).unwrap()
                    .execute("UPDATE users SET status=?1 WHERE id=?2", [scenario, &extra.id]).unwrap();
                assert_eq!(db.list_users().unwrap().len(), 2);
            }
            if scenario == "list-error" {
                rusqlite::Connection::open(&path).unwrap()
                    .execute("UPDATE users SET role='invalid'", []).unwrap();
                assert!(db.list_users().is_err());
            }
        }
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Personal);
        let allowed = matches!(scenario, "missing" | "empty" | "admin");
        let mut reg = AgentRegistry::new(home.path().join("agents"));
        reg.scan().await.unwrap();
        let agent = reg.get("operator").unwrap();
        assert_eq!(requires_admin(agent), !allowed, "{scenario}");
        assert_eq!(crate::trigger_route::SenderAccess::unmapped().may_call(agent, Some("operator")), allowed, "{scenario}");
        for principal in ["unmapped:line:owner", "unmapped:telegram:owner"] {
            assert_eq!(check_loaded(home.path(), agent, Some(principal)).is_ok(), allowed, "{scenario}");
            assert_eq!(check_target(home.path(), "operator", Some(principal)).is_ok(), allowed, "{scenario}");
        }
        assert!(check_loaded(home.path(), agent, None).is_ok(), "{scenario}");
        if scenario == "missing" {
            assert!(!path.exists());
        }
    }
}
