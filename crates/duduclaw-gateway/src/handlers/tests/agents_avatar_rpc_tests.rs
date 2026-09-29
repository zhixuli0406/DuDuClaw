//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;
use std::collections::HashMap;

fn viewer_ctx(agents: &[&str]) -> UserContext {
    let mut agent_access = HashMap::new();
    for a in agents {
        agent_access.insert((*a).to_string(), AccessLevel::Viewer);
    }
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

/// E1: `agents.avatar` returns the stored image as a data URI, `null` when
/// none exists, and is denied for agents the caller can't see (fail-closed
/// at dispatch, before the handler runs).
#[tokio::test]
async fn agents_avatar_reads_image_null_and_denies() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = viewer_ctx(&["alpha", "beta"]);

    // "alpha" has an uploaded avatar on disk → data URI comes back.
    let alpha_dir = home.path().join("agents").join("alpha");
    std::fs::create_dir_all(&alpha_dir).unwrap();
    // Minimal PNG magic bytes; the read path does not re-validate content.
    std::fs::write(alpha_dir.join("avatar.png"), b"\x89PNG\r\n\x1a\nfake").unwrap();

    let frame = handler
        .handle("agents.avatar", json!({ "agent_id": "alpha" }), &ctx)
        .await;
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => {
            assert_eq!(p["has_avatar"].as_bool(), Some(true));
            let uri = p["avatar"].as_str().expect("avatar data uri");
            assert!(uri.starts_with("data:image/png;base64,"), "got: {uri}");
        }
        other => panic!("expected ok response, got: {other:?}"),
    }

    // "beta" is bound but has no avatar file → null / has_avatar=false.
    std::fs::create_dir_all(home.path().join("agents").join("beta")).unwrap();
    let frame = handler
        .handle("agents.avatar", json!({ "agent_id": "beta" }), &ctx)
        .await;
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => {
            assert_eq!(p["has_avatar"].as_bool(), Some(false));
            assert!(p["avatar"].is_null());
        }
        other => panic!("expected ok response, got: {other:?}"),
    }

    // "gamma" is not in the caller's bindings → permission denied at dispatch.
    let frame = handler
        .handle("agents.avatar", json!({ "agent_id": "gamma" }), &ctx)
        .await;
    assert!(
        error_text(&frame).contains("permission denied"),
        "unseen agent must be denied, got: {frame:?}"
    );
}
