//! The shared Google Chat app with no employee to answer refuses instead of
//! running a turn with an empty agent id.
use super::*;

fn test_ctx(home: &std::path::Path) -> ReplyContext {
    let registry = Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.join("agents")),
    ));
    let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let status: ChannelStatusMap = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
}

#[tokio::test]
async fn shared_google_chat_app_without_an_employee_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx(dir.path());
    let reply = Box::pin(build_reply_with_session_inner(
        "hello", &ctx, None, "googlechat:spaces/A", "users/x", None,
    ))
    .await;
    assert_eq!(reply, NO_RESPONDER_TEXT);
}
