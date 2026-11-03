//! WebChat routing on every edition: non-admins never reach main, including
//! bound main agents, queued commands and stored conversations.

use super::*;
use crate::trigger_route::tests::write_agent;
use duduclaw_auth::{AccessLevel, UserRole, UserStatus};
use std::sync::Arc;

struct Fixture {
    _dir: tempfile::TempDir,
    state: WebChatState,
    emp: String,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    crate::team_edition::record(dir.path(), duduclaw_core::EditionProfile::Enterprise);
    let agents = dir.path().join("agents");
    write_agent(&agents, "dudu", "嘟嘟總機", "@嘟嘟總機", "main", "");
    write_agent(
        &agents,
        "finance",
        "財務小幫手",
        "@財務小幫手",
        "specialist",
        "",
    );
    write_agent(
        &agents,
        "sales-rep",
        "業務專員",
        "@sales-rep",
        "specialist",
        "",
    );
    write_agent(
        &agents,
        "faq",
        "常見問題",
        "@常見問題",
        "specialist",
        "\n[channels]\npublic_trigger = true\n",
    );
    let mut registry = duduclaw_agent::AgentRegistry::new(agents);
    registry.scan().await.unwrap();
    let sessions =
        Arc::new(crate::session::SessionManager::new(&dir.path().join("sessions.db")).unwrap());
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let channel_status = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let ctx = Arc::new(crate::channel_reply::ReplyContext::new(
        Arc::new(tokio::sync::RwLock::new(registry)),
        dir.path().to_path_buf(),
        sessions,
        channel_status,
        event_tx,
    ));
    let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
    let emp = db
        .create_user("emp@test.local", "Emp", "EmpPass-12345", UserRole::Employee)
        .unwrap();
    db.bind_agent(&emp.id, "finance", AccessLevel::Operator)
        .unwrap();
    let state = WebChatState::new(
        ctx,
        Arc::new(duduclaw_auth::JwtConfig::new(
            b"test-secret-test-secret-test-sec",
        )),
        Arc::new(db),
    );
    Fixture {
        _dir: dir,
        emp: emp.id,
        state,
    }
}

async fn route(
    f: &Fixture,
    text: &str,
    selected: Option<&str>,
) -> Result<WebRoute, Box<ChatMessage>> {
    decide_webchat_route(
        &f.state,
        &f.emp,
        text,
        selected,
        "dudu",
        "webchat:x",
        "webchat:u",
    )
    .await
}

fn code(frame: &ChatMessage) -> Option<&str> {
    match frame {
        ChatMessage::Error { code, .. } => code.as_deref(),
        _ => None,
    }
}

#[tokio::test]
async fn the_default_employee_is_the_sole_bound_non_main_employee() {
    let f = fixture().await;
    let r = route(&f, "你好", None).await.unwrap();
    assert_eq!(r.agent.as_deref(), Some("finance"));
    assert!(!r.switched);
    let refused = route(&f, "@嘟嘟總機 你好", Some("finance")).await.unwrap_err();
    assert_eq!(code(&refused), Some("agent_unavailable"));
}

#[tokio::test]
async fn calling_another_employee_needs_a_binding() {
    let f = fixture().await;
    let refused = route(&f, "@sales-rep 報價", None).await.unwrap_err();
    assert_eq!(code(&refused), Some("agent_unavailable"));
    let refused = route(&f, "@sales-rep 報價", Some("finance"))
        .await
        .unwrap_err();
    assert_eq!(code(&refused), Some("agent_unavailable"));
    let r = route(&f, "@財務小幫手 幫我算帳", None).await.unwrap();
    assert_eq!(r.agent.as_deref(), Some("finance"));
    assert!(r.switched, "answers in finance's own session bucket");
    let r = route(&f, "幫我算帳", Some("finance")).await.unwrap();
    assert_eq!(
        r,
        WebRoute {
            agent: Some("finance".into()),
            switched: false,
            called: false
        }
    );
}

#[tokio::test]
async fn a_lone_at_sign_returns_the_choices() {
    let f = fixture().await;
    match *route(&f, "@", None).await.unwrap_err() {
        ChatMessage::Error { code, choices, .. } => {
            assert_eq!(code.as_deref(), Some(MENTION_CHOICES_CODE));
            assert_eq!(
                choices.unwrap(),
                vec!["@常見問題".to_string(), "@財務小幫手".to_string()]
            );
        }
        other => panic!("expected choices, got {other:?}"),
    }
}

#[tokio::test]
async fn a_suspended_account_loses_its_bindings() {
    let f = fixture().await;
    f.state
        .user_db
        .set_user_status(&f.emp, UserStatus::Suspended)
        .unwrap();
    let refused = route(&f, "@財務小幫手 幫我算帳", None).await.unwrap_err();
    assert_eq!(code(&refused), Some("agent_unavailable"));
}

async fn route_as(f: &Fixture, user: &str, text: &str) -> Result<WebRoute, Box<ChatMessage>> {
    decide_webchat_route(&f.state, user, text, None, "dudu", "webchat:x", "webchat:u").await
}

/// mgr (Manager) → lead (Manager) → emp1 (Employee, sales-rep Owner); the
/// fixture's `emp` (finance Operator) reports to nobody.
fn team(f: &Fixture) -> (String, String) {
    crate::team_edition::record(f._dir.path(), duduclaw_core::EditionProfile::Enterprise);
    let db = &f.state.user_db;
    let mk = |email: &str, role| db.create_user(email, email, "EmpPass-12345", role).unwrap().id;
    let mgr = mk("mgr@test.local", UserRole::Manager);
    let lead = mk("lead@test.local", UserRole::Manager);
    let emp1 = mk("emp1@test.local", UserRole::Employee);
    db.set_manager(&lead, Some(&mgr)).unwrap();
    db.set_manager(&emp1, Some(&lead)).unwrap();
    db.bind_agent(&emp1, "sales-rep", AccessLevel::Owner).unwrap();
    (mgr, lead)
}

#[tokio::test]
async fn a_manager_calls_a_reports_employee_but_not_a_peers() {
    let f = fixture().await;
    let (mgr, _lead) = team(&f);
    let r = route_as(&f, &mgr, "@sales-rep 報價").await.unwrap();
    assert_eq!(r.agent.as_deref(), Some("sales-rep"));
    assert!(r.switched);
    let refused = route_as(&f, &mgr, "@財務小幫手 幫我算帳").await.unwrap_err();
    assert_eq!(code(&refused), Some("agent_unavailable"));
    match *route_as(&f, &mgr, "@").await.unwrap_err() {
        ChatMessage::Error { choices, .. } => assert_eq!(
            choices.unwrap(),
            vec!["@常見問題".to_string(), "@業務專員".to_string()]
        ),
        other => panic!("expected choices, got {other:?}"),
    }
    // Explicit selection follows the same set.
    assert!(f.state.agent_access_allowed(&mgr, "sales-rep").await);
    assert!(!f.state.agent_access_allowed(&mgr, "finance").await);
}

#[tokio::test]
async fn cutting_the_reporting_line_takes_effect_on_the_next_message() {
    let f = fixture().await;
    let (mgr, lead) = team(&f);
    // Warm the ACL cache, then change the org: no TTL wait.
    assert!(route_as(&f, &mgr, "@sales-rep 報價").await.is_ok());
    f.state.user_db.set_manager(&lead, None).unwrap();
    let refused = route_as(&f, &mgr, "@sales-rep 報價").await.unwrap_err();
    assert_eq!(code(&refused), Some("agent_unavailable"));
    assert!(!f.state.agent_access_allowed(&mgr, "sales-rep").await);
}

#[tokio::test]
async fn personal_edition_ignores_leftover_reporting_lines_in_webchat() {
    let f = fixture().await;
    let (mgr, _lead) = team(&f);
    crate::team_edition::record(f._dir.path(), duduclaw_core::EditionProfile::Personal);
    let refused = route_as(&f, &mgr, "@sales-rep 報價").await.unwrap_err();
    assert_eq!(code(&refused), Some("agent_unavailable"));
    assert!(!f.state.agent_access_allowed(&mgr, "sales-rep").await);
}

#[tokio::test]
async fn a_widget_visitor_is_not_told_about_a_refusal() {
    let f = fixture().await;
    // A forbidden name is plain text for the default employee.
    let r = route_as(&f, "widget-visitor:abc", "@財務小幫手 幫我算帳").await.unwrap();
    assert_eq!(r, WebRoute { agent: None, switched: false, called: false });
}

#[tokio::test]
async fn resume_follows_the_same_rule_as_the_at_list() {
    let f = fixture().await;
    // finance (bound) resumes; sales-rep (not callable) does not; the
    // main never resumes, while a public non-main employee still can.
    assert!(f.state.may_resume_into(&f.emp, "finance", "webchat:x", "webchat:u").await);
    assert!(!f.state.may_resume_into(&f.emp, "sales-rep", "webchat:x", "webchat:u").await);
    assert!(!f.state.may_resume_into(&f.emp, "dudu", "webchat:x", "webchat:u").await, "main is admin-only");
    assert!(f.state.may_resume_into(&f.emp, "faq", "webchat:x", "webchat:u").await, "public employee");
    // Explicit selection stays narrower: a public employee is not a partner.
    assert!(!f.state.agent_access_allowed(&f.emp, "faq").await);
    let (mgr, _lead) = team(&f);
    assert!(f.state.may_resume_into(&mgr, "sales-rep", "webchat:x", "webchat:u").await, "team employee");
}

#[tokio::test]
async fn both_plain_and_mentioned_non_admin_turns_recheck_before_execution() {
    let f = fixture().await;
    let plain = route(&f, "幫我算帳", Some("finance")).await.unwrap();
    let called = route(&f, "@財務小幫手 幫我算帳", Some("finance")).await.unwrap();
    assert!(!plain.called);
    assert!(called.called);
    assert!(!called.switched, "same selected employee still needs the call predicate");

    // Pause the employee after admission, as can happen while this turn waits.
    let path = f._dir.path().join("agents/finance/agent.toml");
    let config = std::fs::read_to_string(&path).unwrap()
        .replace("status = \"active\"", "status = \"paused\"");
    std::fs::write(path, config).unwrap();
    let mut registry = duduclaw_agent::AgentRegistry::new(f._dir.path().join("agents"));
    registry.scan().await.unwrap();
    *f.state.ctx.registry.write().await = registry;
    assert!(f.state.agent_access_allowed(&f.emp, "finance").await);
    assert!(!f.state.may_resume_into(&f.emp, "finance", "webchat:x", "webchat:u").await);

    let cancel = tokio_util::sync::CancellationToken::new();
    for route in [plain, called] {
        assert!(!crate::webchat::turns::recheck_turn_access(
            &cancel, route.called,
            async {
                f.state.authorize_target(&f.emp, "finance", true).await.is_ok()
                    && f.state.agent_access_allowed(&f.emp, "finance").await
            },
            f.state.may_resume_into(&f.emp, "finance", "webchat:x", "webchat:u"),
        ).await);
    }
}

#[tokio::test]
async fn primary_selection_never_guesses_among_multiple_or_stale_bindings() {
    let f = fixture().await;
    let db = &f.state.user_db;
    assert_eq!(f.state.resolve_new_agent(&f.emp).await.unwrap().as_deref(), Some("finance"));
    db.bind_agent(&f.emp, "sales-rep", AccessLevel::Owner).unwrap();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await, Err("multiple_agents"));
    db.set_primary_agent(&f.emp, Some("sales-rep")).unwrap();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await.unwrap().as_deref(), Some("sales-rep"));
    db.bind_agent(&f.emp, "dudu", AccessLevel::Owner).unwrap();
    db.set_primary_agent(&f.emp, Some("dudu")).unwrap();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await, Err("multiple_agents"));
    let frame = f.state.authorize_target(&f.emp, "dudu", false).await.unwrap_err();
    assert_eq!(code(&frame), Some("agent_unavailable"));
    db.unbind_agent(&f.emp, "dudu").unwrap();
    // A stale primary must not select the remaining Owner automatically.
    assert_eq!(f.state.resolve_new_agent(&f.emp).await, Err("multiple_agents"));
    db.unbind_agent(&f.emp, "finance").unwrap();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await.unwrap().as_deref(), Some("sales-rep"));
    db.unbind_agent(&f.emp, "sales-rep").unwrap();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await, Err("no_agents"));
}

#[tokio::test]
async fn configured_default_and_all_main_roles_are_denied_even_when_bound() {
    let f = fixture().await;
    std::fs::write(f._dir.path().join("config.toml"), "[general]\ndefault_agent = \"finance\"\n").unwrap();
    assert_eq!(code(&f.state.authorize_target(&f.emp, "finance", false).await.unwrap_err()), Some("agent_unavailable"));
    assert_eq!(f.state.resolve_new_agent(&f.emp).await, Err("no_agents"));
    f.state.user_db.bind_agent(&f.emp, "dudu", AccessLevel::Owner).unwrap();
    assert_eq!(code(&f.state.authorize_target(&f.emp, "dudu", false).await.unwrap_err()), Some("agent_unavailable"));
}

#[tokio::test]
async fn failed_identity_and_configuration_lookups_deny_all_dashboard_calls() {
    let f = fixture().await;
    assert_eq!(f.state.resolve_new_agent("missing-user").await, Err("unavailable"));
    assert!(f.state.authorize_target("missing-user", "dudu", true).await.is_err());
    assert!(f.state.authorize_target("missing-user", "faq", true).await.is_err());
    assert!(route_as(&f, "missing-user", "@常見問題 問題").await.is_err());
    std::fs::write(f._dir.path().join("config.toml"), "invalid = [").unwrap();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await, Err("unavailable"));
    assert!(f.state.authorize_target(&f.emp, "finance", false).await.is_err());
}

#[tokio::test]
async fn admin_personal_default_and_widget_default_keep_the_legacy_path() {
    let f = fixture().await;
    let admin = f.state.user_db.create_user("admin@test.local", "Admin", "EmpPass-12345", UserRole::Admin).unwrap();
    crate::team_edition::record(f._dir.path(), duduclaw_core::EditionProfile::Personal);
    assert_eq!(f.state.resolve_new_agent(&admin.id).await.unwrap(), None);
    assert!(f.state.authorize_target(&admin.id, "dudu", false).await.is_ok());
    assert_eq!(route_as(&f, &admin.id, "你好").await.unwrap().agent, None);
    assert_eq!(f.state.resolve_new_agent("widget-visitor:test").await.unwrap(), None);
    assert_eq!(route_as(&f, "widget-visitor:test", "你好").await.unwrap().agent, None);
    // Visitors cannot select even a public employee through a mention.
    assert_eq!(route_as(&f, "widget-visitor:test", "@常見問題 問題").await.unwrap().agent, None);
}

#[tokio::test]
async fn webchat_assignment_rights_exclude_main_but_other_channels_keep_their_policy() {
    use crate::trigger_route::assign::{assign_right, assignable_ids, AssignRight};
    let f = fixture().await;
    f.state.user_db.bind_agent(&f.emp, "dudu", AccessLevel::Owner).unwrap();
    let web = f.state.sender_access(&f.emp).await;
    let plain = SenderAccess::from_effective(f.state.acl_snapshot(&f.emp).await.unwrap().access);
    let reg = f.state.ctx.registry.read().await;
    assert_eq!(assign_right(&reg, &web, "dudu", Some("dudu")), AssignRight::Denied);
    assert_eq!(assignable_ids(&reg, &web, Some("dudu")), vec!["finance"]);
    assert!(matches!(assign_right(&reg, &plain, "dudu", Some("dudu")), AssignRight::Allowed(_)));
}

#[tokio::test]
async fn pending_assignment_rechecks_main_after_it_was_armed() {
    use crate::trigger_route::pending::{self, PendingKey, PendingKind};
    let f = fixture().await;
    f.state.user_db.bind_agent(&f.emp, "dudu", AccessLevel::Owner).unwrap();
    let sid = format!("webchat:{}", f.emp);
    let key = PendingKey::new("webchat", &sid, &f.emp);
    let seq = pending::next_seq();
    pending::arm_kind(&key, "dudu", PendingKind::Assign, "old-arm", seq);
    let reply = pending_assign_reply(&f.state, &f.emp, "Prepare report", Some("finance"),
        "dudu", &sid, "webchat:u", None, Some("pending-body"), pending::next_seq()).await.unwrap();
    assert!(matches!(reply, ChatMessage::AssistantDone { task: None, .. }));
    assert!(!f._dir.path().join("tasks.db").exists(), "refused assignment creates no task store");
}

#[tokio::test]
async fn broken_acl_database_denies_even_after_a_successful_lookup() {
    let f = fixture().await;
    assert!(f.state.resolve_new_agent(&f.emp).await.is_ok());
    assert!(f.state.may_resume_into(&f.emp, "finance", "webchat:x", "webchat:u").await);
    rusqlite::Connection::open(f._dir.path().join("users.db")).unwrap()
        .execute_batch("DROP TABLE user_agent_bindings;").unwrap();
    f.state.acl_cache.lock().await.clear();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await, Err("unavailable"));
    assert!(f.state.authorize_target(&f.emp, "finance", false).await.is_err());
    assert!(!f.state.may_resume_into(&f.emp, "finance", "webchat:x", "webchat:u").await);
}

#[tokio::test]
async fn admin_and_widget_session_info_are_byte_compatible() {
    let f = fixture().await;
    let admin = f.state.user_db.create_user("admin-frame@test.local", "Admin", "EmpPass-12345", UserRole::Admin).unwrap();
    let state = Arc::new(f.state);
    let original = serde_json::to_string(&crate::webchat::build_session_info_frame(&state, "webchat:x", None).await).unwrap();
    for user in [admin.id.as_str(), "widget-visitor:frame"] {
        let frame = crate::webchat::access::user_session_info(&state, "webchat:x", None, user).await;
        assert_eq!(serde_json::to_string(&frame).unwrap(), original);
    }
}

#[tokio::test]
async fn every_main_role_is_blocked_not_only_the_registrys_first_main() {
    let f = fixture().await;
    write_agent(&f._dir.path().join("agents"), "second-main", "Other main", "@other-main", "main", "");
    f.state.ctx.registry.write().await.scan().await.unwrap();
    f.state.user_db.bind_agent(&f.emp, "second-main", AccessLevel::Owner).unwrap();
    assert_eq!(code(&f.state.authorize_target(&f.emp, "second-main", false).await.unwrap_err()), Some("agent_unavailable"));
    assert_eq!(f.state.resolve_new_agent(&f.emp).await.unwrap().as_deref(), Some("finance"));
}

#[tokio::test]
async fn schedule_commands_bind_the_checked_full_id_and_refuse_missing_rows() {
    use crate::assign_schedule::{manage, AssignKind, AssignPayload, Creator, NewSchedule, ScheduleSpec};
    use crate::trigger_route::assign::{AssignCommand, ScheduleCommand};
    let f = fixture().await;
    let store = crate::task_store::TaskStore::open(f._dir.path()).unwrap();
    let creator = Creator::new(&f.emp, false).unwrap();
    let row = manage::create(f._dir.path(), &store, &creator, NewSchedule {
        agent_id: "finance".into(), kind: AssignKind::Goal,
        payload: AssignPayload { description: "Finance report".into(), ..Default::default() },
        spec: ScheduleSpec::Daily { hour: 9, minute: 0 },
        timezone: Some("Asia/Taipei".into()), created_via: "dashboard".into(), source: None,
    }, chrono::Utc::now()).await.unwrap();
    let mut cmd = crate::chat_commands::ChatCommand::Assign(AssignCommand::Schedules(
        ScheduleCommand::Pause(duduclaw_core::truncate_chars(&row.id, 8).to_string()),
    ));
    f.state.authorize_command(&f.emp, &mut cmd).await.unwrap();
    assert!(matches!(cmd, crate::chat_commands::ChatCommand::Assign(AssignCommand::Schedules(
        ScheduleCommand::Pause(id))) if id == row.id));
    let mut missing = crate::chat_commands::ChatCommand::Assign(AssignCommand::Schedules(
        ScheduleCommand::Resume("missing".into()),
    ));
    assert_eq!(code(&f.state.authorize_command(&f.emp, &mut missing).await.unwrap_err()), Some("schedule_not_found"));
}

#[tokio::test]
async fn schedule_commands_refuse_ambiguous_prefixes_without_rewriting_them() {
    use crate::assign_schedule::{manage, AssignKind, AssignPayload, Creator, NewSchedule, ScheduleSpec};
    use crate::trigger_route::assign::{AssignCommand, ScheduleCommand};
    let f = fixture().await;
    let store = crate::task_store::TaskStore::open(f._dir.path()).unwrap();
    let creator = Creator::new(&f.emp, false).unwrap();
    let mut row = manage::create(f._dir.path(), &store, &creator, NewSchedule {
        agent_id: "finance".into(), kind: AssignKind::Goal,
        payload: AssignPayload { description: "Report".into(), ..Default::default() },
        spec: ScheduleSpec::Daily { hour: 9, minute: 0 },
        timezone: Some("Asia/Taipei".into()), created_via: "dashboard".into(), source: None,
    }, chrono::Utc::now()).await.unwrap();
    for id in ["collision-first", "collision-second"] {
        row.id = id.into();
        store.insert_assign_schedule(&row, 200).await.unwrap();
    }
    for control in [ScheduleCommand::Pause("collisio".into()), ScheduleCommand::Resume("collisio".into()), ScheduleCommand::Remove("collisio".into())] {
        let mut cmd = crate::chat_commands::ChatCommand::Assign(AssignCommand::Schedules(control));
        assert_eq!(code(&f.state.authorize_command(&f.emp, &mut cmd).await.unwrap_err()), Some("schedule_not_found"));
        assert!(matches!(cmd, crate::chat_commands::ChatCommand::Assign(AssignCommand::Schedules(
            ScheduleCommand::Pause(id) | ScheduleCommand::Resume(id) | ScheduleCommand::Remove(id)
        )) if id == "collisio"));
    }
    let mut full = crate::chat_commands::ChatCommand::Assign(AssignCommand::Schedules(ScheduleCommand::Pause("collision-first".into())));
    f.state.authorize_command(&f.emp, &mut full).await.unwrap();
}

#[tokio::test]
async fn personal_non_admin_cannot_use_main_default_explicit_resume_or_session_info() {
    let f = fixture().await;
    crate::team_edition::record(f._dir.path(), duduclaw_core::EditionProfile::Personal);
    // Edition changes do not lift the main restriction, even with a binding.
    f.state.user_db.bind_agent(&f.emp, "dudu", AccessLevel::Operator).unwrap();
    assert_eq!(f.state.resolve_new_agent(&f.emp).await.unwrap().as_deref(), Some("finance"));
    assert_eq!(route(&f, "Hello", None).await.unwrap().agent.as_deref(), Some("finance"));
    assert_eq!(code(&route(&f, "Hello", Some("dudu")).await.unwrap_err()), Some("agent_unavailable"));
    assert_eq!(code(&f.state.authorize_target(&f.emp, "dudu", false).await.unwrap_err()), Some("agent_unavailable"));
    assert!(!f.state.may_resume_into(&f.emp, "dudu", "webchat:x", "webchat:u").await);
    assert!(!f.state.agent_access_allowed(&f.emp, "dudu").await);
    assert!(route(&f, "@嘟嘟總機 Hello", None).await.is_err());
    let state = Arc::new(f.state);
    let actual = crate::webchat::access::user_session_info(&state, "webchat:x", Some("dudu"), &f.emp).await;
    let actual = serde_json::to_value(actual).unwrap();
    assert_eq!(actual["resolved_agent"], serde_json::Value::Null);
    assert_eq!(actual["assignment_error"]["reason"], "unavailable");
}

#[tokio::test]
async fn non_admin_target_and_mention_refusals_are_identical() {
    let f = fixture().await;
    let expected = serde_json::to_value(crate::webchat::access::agent_unavailable()).unwrap();
    for target in ["does-not-exist", "sales-rep", "dudu"] {
        let frame = f.state.authorize_target(&f.emp, target, false).await.unwrap_err();
        assert_eq!(serde_json::to_value(frame).unwrap(), expected, "target {target}");
    }
    for text in ["@does-not-exist hello", "@sales-rep hello", "@嘟嘟總機 hello"] {
        let frame = route(&f, text, Some("finance")).await.unwrap_err();
        assert_eq!(serde_json::to_value(frame).unwrap(), expected, "mention {text}");
    }
    // Composite list requests must not reveal whether an inaccessible name
    // exists either. Both expose only the caller's allowed employees.
    for suffix in [" @", " @財"] {
        let absent = route(&f, &format!("@does-not-exist{suffix}"), Some("finance")).await.unwrap_err();
        let forbidden = route(&f, &format!("@sales-rep{suffix}"), Some("finance")).await.unwrap_err();
        assert_eq!(serde_json::to_value(absent).unwrap(), serde_json::to_value(forbidden).unwrap());
    }
    assert!(route(&f, "Contact ann@example.com", Some("finance")).await.is_ok());
    assert!(route(&f, "meet @ 5pm", Some("finance")).await.is_ok());
    assert_eq!(code(&f.state.authorize_resume_target(&f.emp, "dudu").await.unwrap_err()),
        Some("main_agent_admin_only"));
}

#[tokio::test]
async fn admin_target_errors_and_unknown_mentions_keep_legacy_responses() {
    let f = fixture().await;
    let admin = f.state.user_db.create_user("admin-target@test.local", "Admin", "EmpPass-12345", UserRole::Admin).unwrap();
    let detailed = ChatMessage::error("unknown agent: absent");
    let expected = serde_json::to_value(&detailed).unwrap();
    assert_eq!(serde_json::to_value(f.state.target_error(&admin.id, detailed).await).unwrap(), expected);
    assert_eq!(route_as(&f, &admin.id, "@does-not-exist hello").await.unwrap().agent, None);
    assert!(f.state.authorize_target(&admin.id, "dudu", false).await.is_ok());
    let access = f.state.sender_access(&admin.id).await;
    let reg = f.state.ctx.registry.read().await;
    let frame = route_outcome(&f.state, &reg, &access, &admin.id, "dudu",
        TriggerRoute::Forbidden { agent: "sales-rep".into() }).unwrap_err();
    assert_eq!(code(&frame), Some(AGENT_FORBIDDEN_CODE));
}

#[tokio::test]
async fn reply_builder_rechecks_final_agent_after_role_or_default_changes() {
    for change in ["role", "default", "removed"] {
        let f = fixture().await;
        assert!(f.state.authorize_target(&f.emp, "finance", false).await.is_ok());
        let path = f._dir.path().join("agents/finance/agent.toml");
        match change {
            "role" => {
                let raw = std::fs::read_to_string(&path).unwrap().replace("role = \"specialist\"", "role = \"main\"");
                std::fs::write(&path, raw).unwrap();
                f.state.ctx.registry.write().await.scan().await.unwrap();
            }
            "default" => std::fs::write(f._dir.path().join("config.toml"), "[general]\ndefault_agent = \"finance\"\n").unwrap(),
            _ => {
                std::fs::remove_file(path).unwrap();
                let mut reg = duduclaw_agent::AgentRegistry::new(f._dir.path().join("agents"));
                reg.scan().await.unwrap();
                *f.state.ctx.registry.write().await = reg;
            }
        }
        let mut model = crate::agent_channels::e2e_seam::arm_waiting(f._dir.path());
        // Bypass the earlier WebChat gate deliberately, just as a turn that
        // passed it immediately before an administrator changed the target.
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5),
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(f.emp.clone(),
                crate::channel_reply::build_reply_for_agent("Hello", &f.state.ctx,
                    "finance", "webchat:final-agent-race", "webchat:user", None)),
        ).await.expect("refusal must return before the model seam");
        let expected = if change == "removed" {
            crate::channel_reply::SELECTED_AGENT_UNAVAILABLE
        } else {
            crate::webchat::access::AGENT_UNAVAILABLE_TEXT
        };
        assert_eq!(reply, expected, "change {change}");
        assert!(model.entered.try_recv().is_err(), "no model invocation for {change}");
        assert!(f.state.ctx.session_manager.get_messages("webchat:final-agent-race").await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn final_reply_main_gate_preserves_admin_token_admin_and_widget() {
    for kind in ["admin", "token-admin", "widget"] {
        let f = fixture().await;
        let admin = f.state.user_db.create_user("final-admin@test.local", "Admin", "EmpPass-12345", UserRole::Admin).unwrap();
        let principal = match kind {
            "admin" => admin.id,
            "token-admin" => "system".into(),
            _ => "widget-visitor:main-gate".into(),
        };
        let mut model = crate::agent_channels::e2e_seam::arm_waiting(f._dir.path());
        let (reply, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(principal,
                    crate::channel_reply::build_reply_for_agent("Hello", &f.state.ctx,
                        "dudu", "webchat:main-unchanged", "webchat:user", None)),
                async {
                    let entered = model.entered.recv().await.expect("main model seam reached");
                    entered.release.send(()).unwrap();
                },
            )
        }).await.expect("privileged/widget main route must reach the seam");
        assert!(reply.contains("OWN_BOT_REPLY agent=dudu"), "{kind}: {reply}");
    }
}

#[tokio::test]
async fn unknown_and_forbidden_mentions_never_become_an_armed_assignment_body() {
    use crate::trigger_route::pending::{self, PendingKey, PendingKind};
    let f = fixture().await;
    for text in ["@missing-employee Hello", "@sales-rep Hello", "@嘟嘟總機 Hello"] {
        let sid = format!("webchat:pending-mention:{}", uuid::Uuid::new_v4());
        let key = PendingKey::new("webchat", &sid, &f.emp);
        pending::arm_kind(&key, "finance", PendingKind::Assign, "arm", pending::next_seq());
        let frame = pending_assign_reply(&f.state, &f.emp, text, Some("finance"),
            "dudu", &sid, "webchat:user", None, Some("body"), pending::next_seq()).await;
        assert!(frame.is_none());
        assert_eq!(code(&route(&f, text, Some("finance")).await.unwrap_err()), Some("agent_unavailable"));
        assert!(!f._dir.path().join("tasks.db").exists());
    }
}

#[tokio::test]
async fn assignment_commands_hide_unavailable_names_and_private_exact_matches() {
    use crate::trigger_route::assign::{self, AssignCaller, AssignCommand, Plan};
    use crate::trigger_route::texts::Lang;
    let f = fixture().await;
    let expected = serde_json::to_value(crate::webchat::access::agent_unavailable()).unwrap();
    for text in ["@missing work", "@sales-rep work", "@dudu work"] {
        let mut cmd = crate::chat_commands::ChatCommand::Assign(AssignCommand::Text(text.into()));
        let denied = f.state.authorize_command(&f.emp, &mut cmd).await.unwrap_err();
        assert_eq!(serde_json::to_value(denied).unwrap(), expected);
    }
    write_agent(&f._dir.path().join("agents"), "fin", "Private", "@fin", "specialist", "");
    f.state.ctx.registry.write().await.scan().await.unwrap();
    let mut cmd = crate::chat_commands::ChatCommand::Assign(AssignCommand::Text("@fin work".into()));
    f.state.authorize_command(&f.emp, &mut cmd).await.unwrap();
    let access = f.state.sender_access(&f.emp).await;
    let caller = AssignCaller {
        home: f._dir.path(), channel: "webchat", conversation: "webchat:assign-private",
        sender: &f.emp, event: None, seq: 0, fallback: Some("finance"), access,
    };
    let command = AssignCommand::Text("@fin work".into());
    let with_hidden = {
        let reg = f.state.ctx.registry.read().await;
        let private = assign::plan(&reg, Lang::ZhTw, &command, &caller);
        assert!(matches!(&private, Plan::Reply(reply) if reply.list.as_ref().is_some_and(|list| list.len() == 1)));
        let channel = AssignCaller { channel: "line", ..caller.clone() };
        assert_ne!(assign::plan(&reg, Lang::ZhTw, &command, &channel), private,
            "other channels keep their existing exact private-name refusal");
        private
    };
    let admin = f.state.user_db.create_user("assign-admin@test.local", "Admin", "EmpPass-12345", UserRole::Admin).unwrap();
    f.state.authorize_command(&admin.id, &mut cmd).await.unwrap();
    let admin_caller = AssignCaller { access: f.state.sender_access(&admin.id).await, ..caller.clone() };
    {
        let reg = f.state.ctx.registry.read().await;
        assert!(matches!(assign::plan(&reg, Lang::ZhTw, &command, &admin_caller),
            Plan::Create(spec) if spec.agent == "fin"), "Admin keeps the exact target");
    }
    std::fs::remove_file(f._dir.path().join("agents/fin/agent.toml")).unwrap();
    f.state.ctx.registry.write().await.scan().await.unwrap();
    let reg = f.state.ctx.registry.read().await;
    assert_eq!(assign::plan(&reg, Lang::ZhTw, &command, &caller), with_hidden,
        "a private exact match must not shadow the visible partial match");
}
