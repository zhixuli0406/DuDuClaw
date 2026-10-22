//! `knowledge.*` (document knowledge base) RPC routing. Runs before the
//! rest of the dispatch chain; every permission decision lives in
//! `crate::knowledge::rpc`.

use super::*;

impl MethodHandler {
    /// `Some(frame)` for a `knowledge.*` method, `None` otherwise.
    pub(crate) async fn dispatch_kb(
        &self,
        method: &str,
        params: &Value,
        ctx: &UserContext,
    ) -> Option<WsFrame> {
        if !crate::knowledge::rpc::is_knowledge_method(method) {
            return None;
        }
        let users = self.user_db.read().await.clone();
        let result =
            crate::knowledge::rpc::handle(&self.home_dir, method, params, ctx, users).await?;
        Some(match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        })
    }
}
