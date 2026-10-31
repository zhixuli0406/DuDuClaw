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
    for scenario in ["missing", "empty", "admin", "employee", "corrupt", "suspended", "offboarded", "sole-suspended", "sole-offboarded", "password-change", "row-error", "list-error"] {
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
            if let Some(status) = scenario.strip_prefix("sole-") {
                rusqlite::Connection::open(&path).unwrap()
                    .execute("UPDATE users SET status=?1", [status]).unwrap();
                assert_eq!(db.list_users().unwrap().len(), 1);
            }
            if scenario == "password-change" {
                rusqlite::Connection::open(&path).unwrap()
                    .execute("UPDATE users SET must_change_password=1", []).unwrap();
            }
            if scenario == "row-error" {
                rusqlite::Connection::open(&path).unwrap()
                    .execute("UPDATE users SET department=x'FF'", []).unwrap();
                assert!(db.list_users().is_err());
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
        assert_eq!(requires_admin_for_id(home.path(), "operator"), !allowed, "{scenario}");
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

#[tokio::test]
async fn reader_identity_check_is_read_only_and_fails_closed() {
    let f = fixture().await;
    assert!(requires_admin_for_id(f.home.path(), "operator"));
    assert!(!requires_admin_for_id(f.home.path(), "ordinary"));
    assert!(!requires_admin_for_id(f.home.path(), "missing"));
    assert!(requires_admin_for_id(f.home.path(), "../operator"));
    std::fs::remove_file(f.home.path().join("presets/system-operator/preset.toml")).unwrap();
    assert!(requires_admin_for_id(f.home.path(), "operator"), "unresolved binding");
    let path = preset::bindings_path(f.home.path());
    for content in ["not [ toml", "schema = 2", "agents = 1", "[agents.ordinary]\nversion = '1'"] {
        std::fs::write(&path, content).unwrap();
        assert!(requires_admin_for_id(f.home.path(), "ordinary"), "{content}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(requires_admin_for_id(f.home.path(), "ordinary"), "unreadable binding");
    std::fs::remove_dir(&path).unwrap();
    assert!(!requires_admin_for_id(f.home.path(), "ordinary"), "plain unbound employee");
    assert!(!path.exists(), "a read must not recreate the binding store");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(f.home.path().join("missing-bindings"), &path).unwrap();
        assert!(requires_admin_for_id(f.home.path(), "ordinary"), "dangling binding symlink");
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(f.home.path().join("missing-agent"), f.home.path().join("agents/dangling")).unwrap();
        assert!(requires_admin_for_id(f.home.path(), "dangling"), "dangling employee symlink");
    }
    std::fs::remove_dir_all(f.home.path().join("agents/ordinary")).unwrap();
    std::fs::write(f.home.path().join("agents/ordinary"), "not a directory").unwrap();
    assert!(requires_admin_for_id(f.home.path(), "ordinary"), "unreadable employee directory");
}

#[tokio::test]
async fn narrowed_context_keeps_admin_and_ordinary_bindings_and_does_not_write() {
    let f = fixture().await;
    for original in [&f.employee, &f.manager] {
        let narrowed = without_restricted_access(f.home.path(), original.clone());
        assert!(!narrowed.agent_access.contains_key("operator"));
        assert!(narrowed.agent_access.contains_key("ordinary"));
        assert!(original.agent_access.contains_key("operator"));
        assert!(f.db.live_user_context(&original.user_id).unwrap().unwrap().agent_access.contains_key("operator"));
    }
    // Even an unreadable/non-directory home cannot affect Admin authority.
    let bad_home = f.home.path().join("not-a-directory");
    std::fs::write(&bad_home, "unreadable as a directory").unwrap();
    let admin = without_restricted_access(&bad_home, f.admin.clone());
    assert_eq!(admin.agent_access, f.admin.agent_access);
    assert_eq!(admin.team_access, f.admin.team_access);
}

#[test]
fn known_identity_sources_call_the_narrowing_helper() {
    for (source, boundary) in [
        (include_str!("../handlers/live_context.rs"), "return Ok(crate::machine_employee::without_restricted_access(&self.home_dir, ctx.clone()))"),
        (include_str!("../handlers/live_context.rs"), "Ok(crate::machine_employee::without_restricted_access(&self.home_dir, live))"),
        (include_str!("../review_evidence/audience.rs"), "Ok(crate::machine_employee::without_restricted_access(home, duduclaw_auth::UserContext {"),
        (include_str!("../knowledge/principals.rs"), "crate::machine_employee::requires_admin_for_id(home, &agent)"),
        (include_str!("../assign_schedule/manage.rs"), "crate::machine_employee::requires_admin_for_id(home, agent)"),
        (include_str!("../recipient_guard.rs"), "crate::machine_employee::requires_admin_for_id(home, agent)"),
        (include_str!("../computer_profiles/mod.rs"), "crate::machine_employee::restriction_known(home, agent)"),
        (include_str!("../team_oversight.rs"), "crate::machine_employee::live_user_context(home, db, &ctx.user_id)"),
        (include_str!("../server.rs"), "crate::machine_employee::live_user_context(state.handler.home_dir(), &state.user_db, &claims.sub)"),
        (include_str!("../server.rs"), ".map(|ctx| crate::machine_employee::without_restricted_access(state.handler.home_dir(), ctx))"),
    ] {
        assert!(source.contains(boundary), "missing identity narrowing: {boundary}");
    }
}

#[tokio::test]
async fn request_local_lookup_matches_individual_checks_and_parses_once() {
    let f = fixture().await;
    let ids = ["ordinary", "operator", "missing", "../operator"];
    for malformed in [false, true] {
        if malformed { std::fs::write(preset::bindings_path(f.home.path()), "not [ toml").unwrap(); }
        assert_eq!(restriction_known(f.home.path(), "ordinary"), if malformed { None } else { Some(false) });
        let mut ctx = f.employee.clone();
        for id in ids {
            ctx.agent_access.insert(id.into(), duduclaw_auth::AccessLevel::Owner);
            ctx.team_access.insert(id.into(), duduclaw_auth::TeamGrant { level: AccessLevel::Operator, via_user: f.employee.user_id.clone(), depth: 1 });
        }
        let expected: Vec<_> = ids.iter().map(|id| !requires_admin_for_id(f.home.path(), id)).collect();
        let narrowed = without_restricted_access(f.home.path(), ctx);
        for (id, keep) in ids.iter().zip(expected) {
            assert_eq!(narrowed.agent_access.contains_key(*id), keep, "{id}");
            assert_eq!(narrowed.team_access.contains_key(*id), keep, "{id}");
        }
    }
    let lookup = RestrictionLookup::new(f.home.path());
    assert!(lookup.requires_admin("ordinary"));
    // Once read, both direct and team grants use this call's snapshot.
    std::fs::write(preset::bindings_path(f.home.path()), "schema = 1").unwrap();
    assert!(lookup.requires_admin("ordinary"));
    assert!(!requires_admin_for_id(f.home.path(), "ordinary"));
}
