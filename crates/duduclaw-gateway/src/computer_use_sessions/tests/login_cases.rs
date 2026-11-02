use super::*;
use crate::website_credentials::{self, Upsert};

fn credential(home: &std::path::Path) -> website_credentials::Metadata {
    website_credentials::upsert(home, Upsert {
        id: None, label: "work".into(), login_url: "https://example.com/login".into(),
        allowed_paths: vec!["/password".into()],
        username: "LOGIN-USERNAME-CANARY".into(), password: "LOGIN-PASSWORD-CANARY".into(),
        allowed_agents: vec!["alice".into()],
    }, "admin").unwrap()
}
fn request() -> ActionRequest { ActionRequest::Login { credential: "work".into() } }
async fn insert_login_session(mgr: &ComputerUseSessions, state: &Arc<FakeState>) {
    insert_session(mgr, state, "alice", trusted_config());
    let entry = mgr.lookup("alice").unwrap();
    let mut session = entry.lock().await;
    session.nav_hosts = vec!["example.com".into()];
    session.nav_configured = true;
}
fn trusted_config() -> ComputerUseConfig {
    ComputerUseConfig { auto_confirm_trusted: true, ..Default::default() }
}

#[tokio::test]
async fn verified_employee_grant_and_label_are_required_before_the_backend() {
    let tmp = home();
    credential(tmp.path());
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "bob", trusted_config());
    insert_login_session(&mgr, &state).await;
    assert_eq!(mgr.action("bob", None, None, &request()).await.unwrap_err().code, ErrorCode::Forbidden);
    let missing = ActionRequest::Login { credential: "unknown".into() };
    assert_eq!(mgr.action("alice", None, None, &missing).await.unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(state.login_calls.load(Ordering::SeqCst), 0);
    assert_eq!(mgr.status("bob").await["available_credentials"], json!([]));
    let status = mgr.status("alice").await;
    assert_eq!(status["available_credentials"], json!(["work"]));
    assert!(!status.to_string().contains("LOGIN-USERNAME-CANARY"));
    assert!(!status.to_string().contains("LOGIN-PASSWORD-CANARY"));
    let result = mgr.action("alice", None, None, &request()).await.unwrap();
    assert_eq!(result["status"], "logged_in");
    assert_eq!(state.login_calls.load(Ordering::SeqCst), 1);
    assert_eq!(*state.navigated.lock().unwrap(), vec!["https://example.com/login"]);
    assert_eq!(mgr.lookup("alice").unwrap().lock().await.actions_used, 2);
    assert!(state.executed.lock().unwrap().is_empty());
    assert!(!result.to_string().contains("CANARY"));
    let audit = std::fs::read_to_string(tmp.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("website_credential_used"));
    assert!(!audit.contains("CANARY"));
}

#[tokio::test]
async fn login_has_its_own_policy_gate_and_retains_sensitive_input_confirmation() {
    let tmp = home();
    credential(tmp.path());
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_login_session(&mgr, &state).await;
    write_agent(tmp.path(), "alice", "[capabilities]\ncomputer_use=true\ndenied_tools=['computer_login']\n");
    assert_eq!(mgr.action("alice", None, None, &request()).await.unwrap_err().code, ErrorCode::Forbidden);
    write_agent(tmp.path(), "alice", "[capabilities]\ncomputer_use=true\ndenied_tools=['computer_type']\n");
    assert_eq!(mgr.action("alice", None, None, &request()).await.unwrap()["status"], "logged_in");
    mgr.lookup("alice").unwrap().lock().await.config.auto_confirm_trusted = false;
    assert_eq!(mgr.action("alice", None, None, &request()).await.unwrap_err().code, ErrorCode::ConfirmationRequired);
    assert_eq!(state.login_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn uncleared_login_removes_session_container_and_discards_profile() {
    let tmp = home();
    credential(tmp.path());
    let state = Arc::new(FakeState::default());
    *state.login_outcome.lock().unwrap() = Some(login::LoginStatus::FailedUncleared);
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_login_session(&mgr, &state).await;
    let entry = mgr.entry("alice").unwrap();
    let error = mgr.action("alice", None, None, &request()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::SessionEnded);
    assert!(error.message.contains("新的工作階段"));
    assert!(mgr.lookup("alice").is_none());
    assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    assert!(state.login_discarded.load(Ordering::SeqCst));
    assert!(entry.shared.login_masked.load(Ordering::Acquire));
    assert!(entry.control.stopped.load(Ordering::Acquire));
    assert!(mgr.screenshot("alice", None).await.is_err());
    assert!(mgr.action("alice", None, None, &typed("x")).await.is_err());
    assert!(mgr.action("alice", None, None, &ActionRequest::Navigate {
        url: "https://example.com/".into(),
    }).await.is_err());
    assert_eq!(*state.navigated.lock().unwrap(), vec!["https://example.com/login"]);
}

#[tokio::test]
async fn login_navigation_must_pass_existing_host_tool_and_backend_gates() {
    let tmp = home();
    credential(tmp.path());
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", trusted_config());
    // Missing pinned hosts refuses before the helper sees any secret.
    assert!(mgr.action("alice", None, None, &request()).await.is_err());
    assert_eq!(state.login_calls.load(Ordering::SeqCst), 0);
    {
        let entry = mgr.lookup("alice").unwrap();
        let mut session = entry.lock().await;
        session.nav_hosts = vec!["example.com".into()];
        session.nav_configured = true;
    }
    write_agent(tmp.path(), "alice", "[capabilities]\ncomputer_use=true\ndenied_tools=['computer_navigate']\n");
    assert!(mgr.action("alice", None, None, &request()).await.is_err());
    assert_eq!(state.login_calls.load(Ordering::SeqCst), 0);
    write_agent(tmp.path(), "alice", "[capabilities]\ncomputer_use=true\n");
    state.nav_fail.store(true, Ordering::SeqCst);
    assert!(mgr.action("alice", None, None, &request()).await.is_err());
    assert_eq!(state.login_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_drop_fences_the_session_and_audits_it() {
    let tmp = home();
    credential(tmp.path());
    let state = Arc::new(FakeState::default());
    let entered = Arc::new(tokio::sync::Notify::new());
    *state.login_pause.lock().unwrap() = Some((entered.clone(), Arc::new(tokio::sync::Notify::new())));
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_login_session(&mgr, &state).await;
    let entry = mgr.entry("alice").unwrap();
    {
        let req = request();
        let login = mgr.action("alice", None, None, &req);
        tokio::pin!(login);
        tokio::select! { _ = entered.notified() => {}, _ = &mut login => panic!("unexpected completion") }
        assert!(entry.shared.login_running.load(Ordering::Acquire));
        assert!(entry.session.try_lock().is_err(), "screenshots cannot interleave");
        assert!(entry.shared.login_view_gate.try_read().is_err(), "VNC cannot interleave");
    }
    assert!(entry.control.stopped.load(Ordering::Acquire));
    assert!(entry.shared.login_masked.load(Ordering::Acquire));
    assert!(!entry.shared.login_running.load(Ordering::Acquire));
    let audit = std::fs::read_to_string(tmp.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("cancelled"));
    assert!(!audit.contains("CANARY"));
}
