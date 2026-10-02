//! `agents.inspect` must return the two task-sandbox switches the agent edit
//! page binds to (`[container] sandbox_enabled` / `network_access`). They
//! used to be absent, so the page always rendered both as off.
use super::*;

fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("not ok: {other:?}"),
    }
}

#[tokio::test]
async fn inspect_returns_sandbox_switches_absent_as_false_then_written_values() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    payload(
        &handler
            .handle_agents_create(json!({ "name": "boxed", "display_name": "Boxed" }))
            .await,
    );

    // The create template's `[container]` has neither key ⇒ false.
    let p = payload(&handler.handle_agents_inspect(json!({ "agent_id": "boxed" })).await);
    assert_eq!(p["sandbox_enabled"], false, "{p}");
    assert_eq!(p["network_access"], false, "{p}");

    payload(
        &handler
            .handle_agents_update(json!({
                "agent_id": "boxed",
                "sandbox_enabled": true,
                "network_access": true,
            }))
            .await,
    );
    let p = payload(&handler.handle_agents_inspect(json!({ "agent_id": "boxed" })).await);
    assert_eq!(p["sandbox_enabled"], true, "{p}");
    assert_eq!(p["network_access"], true, "{p}");

    payload(
        &handler
            .handle_agents_update(json!({ "agent_id": "boxed", "network_access": false }))
            .await,
    );
    let p = payload(&handler.handle_agents_inspect(json!({ "agent_id": "boxed" })).await);
    assert_eq!(p["sandbox_enabled"], true, "{p}");
    assert_eq!(p["network_access"], false, "{p}");
}
