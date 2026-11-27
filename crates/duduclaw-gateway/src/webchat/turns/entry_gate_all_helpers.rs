//! Test-only bridges; keep the WebChat implementation and its types private.
use super::*;

pub(crate) async fn arrival(
    state: &WebChatState, user: &str, sid: &str, text: &str, attachment: bool,
) -> Result<(), String> {
    crate::webchat::entry::check(state, user, "worker", sid, user, text, attachment, "all-entry").await
}

pub(crate) async fn pending(
    state: &WebChatState, user: &str, sid: &str,
) -> serde_json::Value {
    use crate::trigger_route::pending::{self, PendingKey, PendingKind};
    let key = PendingKey::new("webchat", sid, user);
    pending::arm_kind(&key, "worker", PendingKind::Assign, "arm", 1);
    let reply = crate::webchat::route::pending_assign_reply(
        state, user, "整理報表", Some("main"), "main", sid, user, None, Some("work"), 2,
    ).await.expect("pending assignment handled");
    let frame = serde_json::to_value(reply).unwrap();
    assert_eq!(pending::peek_kind(&key, 2), if frame["type"] == "error" {
        Some(("worker".into(), PendingKind::Assign))
    } else { None }, "refusal keeps pending state; success claims it");
    frame
}

pub(crate) async fn turn(
    state: &Arc<WebChatState>, user: &str, sid: &str, text: &str, attachment: bool,
) -> serde_json::Value {
    let (tx, mut rx) = outbound_channel();
    let ticket = state.turns.admit(user, sid, None, "all-entry", 3, 10).unwrap();
    let input = TurnInput {
        state: state.clone(), auth_user: user.into(), credential: None, user_id: user.into(),
        agent_id: "worker".into(), requested_agent: Some("worker".into()), effective_agent: Some("worker".into()),
        sid: sid.into(), conv_nonce: None, content: text.into(),
        attachments: if attachment { vec![ChatAttachment { filename: "input.txt".into(),
            mime: Some("text/plain".into()), data_base64: "dGVzdA==".into() }] } else { vec![] },
        assign_event: Some("all-entry".into()), resume_acl: false, arrival_seq: 1,
    };
    run::run(input, TurnSink::new(tx, sid, None, "all-entry"), &ticket).await;
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await.expect("turn answers").expect("socket open");
    let Message::Text(text) = output.message else { panic!("text frame required") };
    serde_json::from_str(&text).unwrap()
}
