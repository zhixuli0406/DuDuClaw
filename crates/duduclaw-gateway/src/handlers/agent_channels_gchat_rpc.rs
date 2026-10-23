//! `agents.channels.googlechat.*` — bind / unbind an AI employee's own
//! Google Chat app (admin only; see `agent_channels_rpc`).

use super::*;

use super::agent_channels_rpc::{GOOGLECHAT_REMOVE, GOOGLECHAT_SET};
use crate::agent_channels::{self, conflicts, googlechat_config, identity, store};

impl MethodHandler {
    pub(super) async fn handle_agent_googlechat_set(
        &self,
        agent_id: &str,
        params: &Value,
        live: &UserContext,
        session: &UserContext,
    ) -> WsFrame {
        if let Err(e) = crate::product_policy::require_channel(&self.home_dir, "googlechat") {
            return WsFrame::error_response("", &e);
        }
        let raw_project = params
            .get("project_number")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let project_number = match googlechat_config::validate_project_number(raw_project) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if self.registry.read().await.get(agent_id).is_none() {
            return WsFrame::error_response("", "Agent not found");
        }
        // The key is write-only: omitted = keep the stored one (project
        // number change only), which must then exist.
        let raw_key = params
            .get("service_account_json")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let sa_enc = match raw_key {
            Some(json) => {
                if let Err(e) = googlechat_config::validate_service_account_json(json) {
                    return WsFrame::error_response("", &e);
                }
                match store::encrypt_or_refuse(json, &self.home_dir) {
                    Ok(enc) => Some(enc),
                    Err(e) => return WsFrame::error_response("", &e),
                }
            }
            None if store::agent_chat_app(&self.home_dir, agent_id)
                .await
                .is_some() =>
            {
                None
            }
            None => {
                return WsFrame::error_response(
                    "",
                    "請貼上這個 Chat 應用程式的服務帳戶金鑰（JSON）。",
                );
            }
        };
        // Held across the duplicate check and the write (B11 L3).
        let _bind = agent_channels::binding_lock().await;
        if let Some(holder) = conflicts::googlechat_project_holder(
            &self.home_dir,
            &self.registry,
            &project_number,
            agent_id,
        )
        .await
        {
            return WsFrame::error_response("", &holder.googlechat_refusal());
        }
        let project_for_write = project_number.clone();
        if let Err(e) = self
            .update_agent_toml_as(
                agent_id,
                GOOGLECHAT_SET,
                Some(live),
                Some(session),
                move |t| store::apply_googlechat(t, &project_for_write, sa_enc.as_deref()),
            )
            .await
        {
            return WsFrame::error_response("", &format!("儲存失敗：{e}"));
        }
        crate::googlechat_agents::forget(agent_id);
        let label = agent_channels::label("googlechat", agent_id);
        identity::clear(&label);
        let probe = match self.reply_ctx.read().await.clone() {
            Some(ctx) => crate::googlechat_agents::probe(&ctx, agent_id).await,
            None => Err("gateway not ready".to_string()),
        };
        self.audit_agent_channel("agent_channel_bound", agent_id, "googlechat", live);
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent_id": agent_id,
                "project_number": project_number,
                "endpoint_path": agent_channels::googlechat_endpoint_path(agent_id),
                "service_account_email": identity::get(&label),
                "verified": probe.is_ok(),
                "error": probe.err().map(|e| crate::secret_redact::redact_secrets(&e).into_owned()),
            }),
        )
    }

    pub(super) async fn handle_agent_googlechat_remove(
        &self,
        agent_id: &str,
        live: &UserContext,
        session: &UserContext,
    ) -> WsFrame {
        if let Err(e) = self
            .update_agent_toml_as(
                agent_id,
                GOOGLECHAT_REMOVE,
                Some(live),
                Some(session),
                |t| {
                    store::remove_channel(t, "googlechat");
                    Ok(())
                },
            )
            .await
        {
            return WsFrame::error_response("", &format!("移除失敗：{e}"));
        }
        crate::googlechat_agents::forget(agent_id);
        let label = agent_channels::label("googlechat", agent_id);
        self.channel_status.write().await.remove(&label);
        identity::clear(&label);
        self.audit_agent_channel("agent_channel_removed", agent_id, "googlechat", live);
        WsFrame::ok_response("", json!({ "success": true, "agent_id": agent_id }))
    }
}
