//! All public discovery operations reconstruct authority from the connection.
use super::*;
use crate::discovery::service::{self, TrustedCaller};
impl MethodHandler {
    pub(crate) async fn handle_discovery_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let caller = match TrustedCaller::from_user(ctx) { Ok(caller) => caller, Err(error) => return WsFrame::error_response("", &error) };
        let store = match self.task_store().await { Ok(store) => store, Err(frame) => return frame };
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) { Ok(broker) => broker, Err(error) => return WsFrame::error_response("", &error) };
        match service::create_from_value(&self.home_dir, &store, &broker, &caller, params).await {
            Ok(created) => WsFrame::ok_response("", json!(created)),
            Err(error) => WsFrame::error_response("", &error),
        }
    }
    pub(crate) async fn handle_discovery_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let caller = match TrustedCaller::from_user(ctx) { Ok(caller) => caller, Err(error) => return WsFrame::error_response("", &error) };
        let run = params.get("run_id").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "discovery.catalog" => service::catalog(&self.home_dir, &caller,
                params.get("agent_id").and_then(Value::as_str).unwrap_or("")),
            "discovery.list" => service::list(&self.home_dir, &caller,
                params.get("agent_id").and_then(Value::as_str),
                params.get("limit").and_then(Value::as_u64).and_then(|limit|usize::try_from(limit).ok()).unwrap_or(20)).await,
            "discovery.tree" => service::tree(&self.home_dir, &caller, run).await,
            "discovery.artifact" => service::artifact(&self.home_dir, &caller, run,
                params.get("file_id").and_then(Value::as_str)).await,
            "discovery.cancel" => service::cancel(&self.home_dir, &caller, run).await,
            _ => Err("unknown discovery operation".into()),
        };
        match result { Ok(payload) => WsFrame::ok_response("", payload), Err(error) => WsFrame::error_response("", &error) }
    }
}
