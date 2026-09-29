//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ═══════════════════════════════════════════════════════════════
// Live Canvas handlers (G15)
// ═══════════════════════════════════════════════════════════════
impl MethodHandler {
    /// `canvas.get` — the agent's current canvas (or one retained history
    /// version when `seq` is passed) plus history metadata (no bodies — a
    /// version can be 256 KB). Read-only; ACL (Viewer, agent-scoped,
    /// fail-closed) is enforced at dispatch by `check_agent_filter!`, same as
    /// `activity.list`. The HTML returned here was ammonia-sanitized at write
    /// time (`canvas::CanvasStore::push` is the only write path); the
    /// dashboard renders it inside `<iframe sandbox="">` as defense-in-depth.
    pub(crate) async fn handle_canvas_get(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "agent_id is required");
        }
        let store = match crate::canvas::CanvasStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &format!("canvas store: {e}")),
        };
        // The first viewer spins up the MCP-push → dashboard-WS bridge
        // (idempotent latch; pushes happen in the MCP subprocess, which has
        // no handle to `event_tx` — see canvas::ensure_broadcast_bridge).
        if let Some(tx) = self.event_tx.read().await.as_ref() {
            crate::canvas::ensure_broadcast_bridge(self.home_dir.clone(), tx.clone());
        }
        let canvas = match params.get("seq").and_then(|v| v.as_i64()) {
            Some(seq) => store.get_version(agent_id, seq).await,
            None => store.current(agent_id).await,
        };
        let canvas = match canvas {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("canvas get: {e}")),
        };
        let history = match store.history(agent_id).await {
            Ok(h) => h,
            Err(e) => return WsFrame::error_response("", &format!("canvas history: {e}")),
        };
        WsFrame::ok_response(
            "",
            json!({
                "agent_id": agent_id,
                "canvas": canvas.map(|c| json!({
                    "seq": c.seq,
                    "agent_id": c.agent_id,
                    "title": c.title,
                    "html": c.html,
                    "updated_at": c.updated_at,
                })),
                "history": history.iter().map(|v| json!({
                    "seq": v.seq,
                    "title": v.title,
                    "updated_at": v.updated_at,
                    "bytes": v.bytes,
                })).collect::<Vec<_>>(),
            }),
        )
    }
}
