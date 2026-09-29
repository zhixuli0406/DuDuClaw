//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;
use std::collections::HashMap;

fn viewer_ctx(agent: &str) -> UserContext {
    let mut agent_access = HashMap::new();
    agent_access.insert(agent.to_string(), AccessLevel::Viewer);
    UserContext {
        user_id: "u1".to_string(),
        email: "u1@test.local".to_string(),
        role: UserRole::Employee,
        agent_access,
        must_change_password: false,
    }
}

fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

/// HS4-style fail-closed dispatch gate: non-admins must name an agent
/// they hold a Viewer binding for; anything else is denied at dispatch,
/// before the handler runs.
#[tokio::test]
async fn canvas_get_authz_fails_closed() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = viewer_ctx("alpha");

    // Missing agent_id → rejected (non-admin may not query unscoped).
    let frame = handler.handle("canvas.get", json!({}), &ctx).await;
    assert!(
        error_text(&frame).contains("agent_id parameter is required"),
        "got: {frame:?}"
    );

    // Agent the caller has no binding for → permission denied.
    let frame = handler
        .handle("canvas.get", json!({ "agent_id": "beta" }), &ctx)
        .await;
    assert!(
        error_text(&frame).contains("permission denied"),
        "got: {frame:?}"
    );

    // Bound agent → allowed; empty store yields a null canvas.
    let frame = handler
        .handle("canvas.get", json!({ "agent_id": "alpha" }), &ctx)
        .await;
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => {
            assert!(p["canvas"].is_null());
            assert_eq!(p["history"].as_array().map(Vec::len), Some(0));
        }
        other => panic!("expected ok response, got: {other:?}"),
    }
}

/// Push through the same store the `canvas_push` MCP tool uses, then read
/// back through the RPC: HTML comes back sanitized, history is listed,
/// and a `seq` param retrieves an older retained version.
#[tokio::test]
async fn canvas_get_returns_sanitized_current_and_history() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = viewer_ctx("alpha");

    let store = crate::canvas::CanvasStore::open(home.path()).unwrap();
    let v1 = store.push("alpha", "v1", "<p>第一版</p>").await.unwrap();
    store
        .push(
            "alpha",
            "儀表板",
            "<h1>KPI</h1><script>alert(1)</script><p>營收 100</p>",
        )
        .await
        .unwrap();

    let frame = handler
        .handle("canvas.get", json!({ "agent_id": "alpha" }), &ctx)
        .await;
    let payload = match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok response, got: {other:?}"),
    };
    let html = payload["canvas"]["html"].as_str().unwrap();
    assert!(
        !html.contains("script"),
        "stored html must be sanitized: {html}"
    );
    assert!(html.contains("<h1>KPI</h1>") && html.contains("營收 100"));
    assert_eq!(payload["canvas"]["title"].as_str(), Some("儀表板"));
    assert_eq!(payload["history"].as_array().map(Vec::len), Some(2));

    // Fetch the older version explicitly by seq.
    let frame = handler
        .handle(
            "canvas.get",
            json!({ "agent_id": "alpha", "seq": v1.seq }),
            &ctx,
        )
        .await;
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => {
            assert_eq!(p["canvas"]["title"].as_str(), Some("v1"));
            assert!(p["canvas"]["html"].as_str().unwrap().contains("第一版"));
        }
        other => panic!("expected ok response, got: {other:?}"),
    }
}
