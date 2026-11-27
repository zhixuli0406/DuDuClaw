//! Dashboard work admission; MCP delegation retains its own policy.
use super::*;

impl MethodHandler {
    pub(crate) async fn deny_main_for_non_admin(
        &self, ctx: &UserContext, agent_id: &str,
    ) -> Result<(), WsFrame> {
        let refuse = || WsFrame::error_response("", "主代理僅管理員可以使用。");
        let mut live = live_reader_context(&self.home_dir, ctx)
            .map_err(|_| WsFrame::error_response("", PERMISSION_DENIED))?;
        live.origin = ctx.origin;
        if live.is_admin() && live.origin != duduclaw_auth::acl::ContextOrigin::MemberKey
            && live.member_key_id.is_none() { return Ok(()); }
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

impl MethodHandler {
    /// A list grant adds assignment access only to an effective main/default employee.
    pub(crate) async fn main_assignment_admitted(&self, ctx: &UserContext, agent_id: &str) -> bool {
        let Ok(mut live) = live_reader_context(&self.home_dir, ctx) else { return false; };
        live.origin = ctx.origin;
        if live.member_key_id.is_some() { return false; }
        let sender = crate::trigger_route::SenderAccess::no_bindings(&live.user_id)
            .with_credential_origin(live.origin)
            .for_assign(&self.home_dir).await;
        let reg = self.registry.read().await;
        reg.get(agent_id).is_some_and(|agent| sender.main_assign_allowed(agent))
    }

    /// Opt-in for non-scheduled work only; management keeps the legacy sibling.
    pub(crate) async fn deny_main_unless_admitted(
        &self, ctx: &UserContext, agent_id: &str, use_: crate::on_behalf_of::access::MainUse,
    ) -> Result<(), WsFrame> {
        let mut live = live_reader_context(&self.home_dir, ctx)
            .map_err(|_| WsFrame::error_response("", PERMISSION_DENIED))?;
        live.origin = ctx.origin;
        if live.is_admin() && live.origin != duduclaw_auth::acl::ContextOrigin::MemberKey
            && live.member_key_id.is_none() { return Ok(()); }
        if let Some(refusal) = crate::machine_employee::refusal_for_known_target(
            &self.home_dir, &live.user_id, agent_id,
        ) {
            return Err(WsFrame::error_response("", refusal));
        }
        let agent = {
            let reg = self.registry.read().await;
            reg.get(agent_id).cloned().map(Box::new)
        }.ok_or_else(|| WsFrame::error_response("", "主代理僅管理員可以使用。"))?;
        if live.origin != duduclaw_auth::acl::ContextOrigin::DashboardSession || live.member_key_id.is_some() {
            let denied = self.deny_main_for_non_admin(&live, agent_id).await;
            // Keep this credential's legacy authorization; use the list-aware
            // check only to explain an already refused main/default target.
            let principal = format!("user:{}", live.user_id);
            if denied.is_err()
                && crate::on_behalf_of::access::main_list_refusal(&self.home_dir, Some(&principal))
                    == crate::on_behalf_of::access::MAIN_TURN_REFUSAL
            {
                let reason = Box::pin(crate::on_behalf_of::access::check_loaded_main_target_for(
                    &self.home_dir, Some(&principal), &agent, use_,
                )).await;
                if matches!(reason, Err(ref reason) if reason == crate::on_behalf_of::access::MAIN_TURN_REFUSAL) {
                    return Err(WsFrame::error_response("", crate::on_behalf_of::access::MAIN_TURN_REFUSAL));
                }
            }
            return denied;
        }
        Box::pin(crate::on_behalf_of::access::check_loaded_main_target_for(
            &self.home_dir, Some(&format!("user:{}", live.user_id)), &agent, use_,
        )).await.map_err(|reason| WsFrame::error_response("", &reason))
    }
}
