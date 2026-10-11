//! Customer export RPCs; authorization remains in the shared service.
use super::*;
impl MethodHandler {
    pub(crate) async fn handle_data_exports_create(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let request = match serde_json::from_value::<crate::data_exports::ExportRequest>(params) {
            Ok(v) => v,
            Err(_) => return WsFrame::error_response("", "EXPORT_INVALID_REQUEST"),
        };
        match crate::data_exports::create(&self.home_dir, ctx, request).await {
            Ok(id) => WsFrame::ok_response("", json!({"export_id":id})),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
    pub(crate) async fn handle_data_exports_get(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let id = params
            .get("export_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        match tokio::task::spawn_blocking(move || crate::data_exports::get(&home, &ctx, &id)).await
        {
            Ok(Ok(status)) => WsFrame::ok_response("", json!(status)),
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(_) => WsFrame::error_response("", "EXPORT_IO"),
        }
    }
    pub(crate) async fn handle_data_exports_cancel(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let id = params
            .get("export_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        match tokio::task::spawn_blocking(move || crate::data_exports::cancel(&home, &ctx, &id))
            .await
        {
            Ok(Ok(status)) => WsFrame::ok_response("", json!(status)),
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(_) => WsFrame::error_response("", "EXPORT_IO"),
        }
    }
}
