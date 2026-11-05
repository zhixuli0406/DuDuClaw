//! Authenticated host control independent of the recording employee's D8 slot.
use super::*;
impl MethodHandler {
    pub(crate) async fn handle_isolation_recording(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if !matches!(method, "isolation.recording_status" | "isolation.recording_stop") {
            return WsFrame::error_response("", "無法執行這項錄製操作。");
        }
        let Some(fields) = params.as_object() else { return WsFrame::error_response("", "錄製識別碼無效。"); };
        if fields.len() != 1 || !fields.contains_key("id") { return WsFrame::error_response("", "錄製識別碼無效。"); }
        let Some(id) = params["id"].as_str() else { return WsFrame::error_response("", "錄製識別碼無效。"); };
        #[cfg(target_os = "linux")]
        {
            let (home, ctx, id, stop) = (self.home_dir.clone(), ctx.clone(), id.to_owned(), method == "isolation.recording_stop");
            match tokio::task::spawn_blocking(move || crate::isolation_mcp::control_recording(&home, &ctx, &id, stop)).await {
                Ok(Ok(value)) => WsFrame::ok_response("", value),
                _ => WsFrame::error_response("", "無法確認管理員授權或錄製狀態，已拒絕操作。"),
            }
        }
        #[cfg(not(target_os = "linux"))]
        { let _ = (id, ctx); WsFrame::error_response("", "這個環境尚未提供受保護錄製。"); }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn isolation_recording_rpc_refuses_untrusted_callers_and_authority_arguments() {
        let home = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let mut caller = UserContext::admin_fallback(); caller.role = UserRole::Employee;
        for method in ["isolation.recording_status", "isolation.recording_stop"] {
            for params in [json!({"id":"09".repeat(16)}), json!({"id":"09".repeat(16),"employee_id":"forged"}), json!({"id":"../recording"})] {
                let result = handler.handle(method, params, &caller).await;
                assert!(!matches!(result, WsFrame::Response { ok: true, .. }));
            }
        }
        assert!(!home.path().join("isolation-execution-jobs.db").exists());
    }
}
