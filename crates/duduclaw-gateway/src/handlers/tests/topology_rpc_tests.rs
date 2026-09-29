//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

fn frame_ok(f: &WsFrame) -> bool {
    matches!(f, WsFrame::Response { ok: true, .. })
}

/// `topology.list` returns the routing overrides + pending reroute proposals
/// from `routing_overrides.json`. Missing file ⇒ empty lists (fail-safe);
/// after writing one active override + one pending proposal, they surface.
#[tokio::test]
async fn topology_list_returns_overrides_and_pending() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    // Empty state ⇒ zero of each (fail-safe on a missing file).
    let frame = handler.handle("topology.list", json!({}), &ctx).await;
    assert!(frame_ok(&frame), "got: {frame:?}");
    if let WsFrame::Response {
        payload: Some(p), ..
    } = &frame
    {
        assert_eq!(p["override_count"].as_u64(), Some(0));
        assert_eq!(p["pending_count"].as_u64(), Some(0));
    }

    // Seed one active override + one pending proposal through the D5 module.
    crate::topology_evolution::seed_for_test(home.path(), "billing", "alice", "bob");

    let frame = handler.handle("topology.list", json!({}), &ctx).await;
    let p = match &frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok response, got: {other:?}"),
    };
    assert_eq!(p["override_count"].as_u64(), Some(1));
    assert_eq!(p["pending_count"].as_u64(), Some(1));
    assert_eq!(p["overrides"][0]["from_agent"].as_str(), Some("alice"));
    assert_eq!(p["overrides"][0]["to_agent"].as_str(), Some("bob"));
    assert_eq!(p["overrides"][0]["status"].as_str(), Some("active"));
    assert_eq!(
        p["pending_proposals"][0]["task_class"].as_str(),
        Some("billing")
    );
}
