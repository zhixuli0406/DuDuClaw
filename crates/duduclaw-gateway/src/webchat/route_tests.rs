//! WebChat `@` routing decisions (owner decision b: the default employee is
//! reachable by every signed-in account; calling another needs a binding).

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
async fn the_default_employee_stays_reachable_without_a_binding() {
    let f = fixture().await;
    let r = route(&f, "你好", None).await.unwrap();
    assert_eq!(
        r,
        WebRoute {
            agent: None,
            switched: false,
            called: false
        }
    );
    // Calling it by name while another employee is selected also works.
    let r = route(&f, "@嘟嘟總機 你好", Some("finance")).await.unwrap();
    assert_eq!(r.agent.as_deref(), Some("dudu"));
    assert!(r.switched);
}

#[tokio::test]
async fn calling_another_employee_needs_a_binding() {
    let f = fixture().await;
    let refused = route(&f, "@sales-rep 報價", None).await.unwrap_err();
    assert_eq!(code(&refused), Some(AGENT_FORBIDDEN_CODE));
    let refused = route(&f, "@sales-rep 報價", Some("finance"))
        .await
        .unwrap_err();
    assert_eq!(code(&refused), Some(AGENT_FORBIDDEN_CODE));
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
                vec!["@嘟嘟總機".to_string(), "@常見問題".to_string(), "@財務小幫手".to_string()]
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
    assert_eq!(code(&refused), Some(AGENT_FORBIDDEN_CODE));
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
    assert_eq!(code(&refused), Some(AGENT_FORBIDDEN_CODE));
    match *route_as(&f, &mgr, "@").await.unwrap_err() {
        ChatMessage::Error { choices, .. } => assert_eq!(
            choices.unwrap(),
            vec!["@嘟嘟總機".to_string(), "@常見問題".to_string(), "@業務專員".to_string()]
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
    assert_eq!(code(&refused), Some(AGENT_FORBIDDEN_CODE));
    assert!(!f.state.agent_access_allowed(&mgr, "sales-rep").await);
}

#[tokio::test]
async fn personal_edition_ignores_leftover_reporting_lines_in_webchat() {
    let f = fixture().await;
    let (mgr, _lead) = team(&f);
    crate::team_edition::record(f._dir.path(), duduclaw_core::EditionProfile::Personal);
    let refused = route_as(&f, &mgr, "@sales-rep 報價").await.unwrap_err();
    assert_eq!(code(&refused), Some(AGENT_FORBIDDEN_CODE));
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
    // default and a public employee (callable by `@`) resume.
    assert!(f.state.may_resume_into(&f.emp, "finance", "webchat:x", "webchat:u").await);
    assert!(!f.state.may_resume_into(&f.emp, "sales-rep", "webchat:x", "webchat:u").await);
    assert!(f.state.may_resume_into(&f.emp, "dudu", "webchat:x", "webchat:u").await, "default employee");
    assert!(f.state.may_resume_into(&f.emp, "faq", "webchat:x", "webchat:u").await, "public employee");
    // Explicit selection stays narrower: a public employee is not a partner.
    assert!(!f.state.agent_access_allowed(&f.emp, "faq").await);
    let (mgr, _lead) = team(&f);
    assert!(f.state.may_resume_into(&mgr, "sales-rep", "webchat:x", "webchat:u").await, "team employee");
}

#[tokio::test]
async fn same_agent_at_call_keeps_stronger_recheck_while_plain_selection_does_not() {
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
    for (route, expected) in [(plain, true), (called, false)] {
        assert_eq!(crate::webchat::turns::recheck_turn_access(
            &cancel, route.called,
            f.state.agent_access_allowed(&f.emp, "finance"),
            f.state.may_resume_into(&f.emp, "finance", "webchat:x", "webchat:u"),
        ).await, expected);
    }
}
