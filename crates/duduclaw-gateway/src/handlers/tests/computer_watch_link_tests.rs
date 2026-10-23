use super::*;

#[tokio::test]
async fn a_signed_link_never_grants_access_or_reopens_an_absent_session() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".keyfile"), [42; 32]).unwrap();
    duduclaw_core::platform::set_owner_only(&home.path().join(".keyfile")).unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let now = chrono::Utc::now().timestamp();
    let token = crate::computer_watch_links::issue_at(
        home.path(), "alice", "cu-0123456789abcdef0123456789abcdef", now, 60,
    ).unwrap();
    let mut unbound = UserContext::admin_fallback();
    unbound.user_id = "not-a-live-person".into();
    unbound.role = UserRole::Employee;
    for ctx in [unbound, UserContext::admin_fallback()] {
        for token in [&token, "forged-navigation-context"] {
            match handler.handle("computer_sessions.open_link", json!({"token":token}), &ctx).await {
                WsFrame::Response { ok: false, error: Some(error), .. } => {
                    let message = error.as_str().expect("plain refusal");
                    assert!(message.contains("連結已過期"), "{error:?}");
                    assert!(!message.contains("alice"));
                }
                other => panic!("link must not grant or resurrect a session: {other:?}"),
            }
        }
    }
}
