//! Dashboard work admission; MCP delegation retains its own policy.
use super::*;

impl MethodHandler {
    pub(crate) async fn deny_main_for_non_admin(
        &self, ctx: &UserContext, agent_id: &str,
    ) -> Result<(), WsFrame> {
        let refuse = || WsFrame::error_response("", "主代理僅管理員可以使用。");
        let live = live_reader_context(&self.home_dir, ctx)
            .map_err(|_| WsFrame::error_response("", PERMISSION_DENIED))?;
        if live.is_admin() { return Ok(()); }
        if let Some(refusal) = crate::machine_employee::refusal_for_known_target(
            &self.home_dir, &live.user_id, agent_id,
        ) {
            return Err(WsFrame::error_response("", refusal));
        }
        let default = crate::webchat::access::gateway_default_id(&self.home_dir)
            .await.map_err(|_| refuse())?;
        let sender = crate::trigger_route::SenderAccess::no_bindings(&live.user_id)
            .for_webchat(Ok(default));
        let reg = self.registry.read().await;
        let agent = reg.get(agent_id).ok_or_else(refuse)?;
        if sender.webchat_allows(agent) { Ok(()) } else { Err(refuse()) }
    }
}
