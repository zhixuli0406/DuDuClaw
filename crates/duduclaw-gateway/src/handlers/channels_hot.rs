//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// `channels.pairing_list` — list approved pairing subjects (E2). This
    /// list is shared across channel types (`AccessController`'s persisted
    /// `approved` set carries no channel dimension — matching the
    /// `pairing_manage action=list` MCP tool exactly), so unlike the other
    /// five methods it takes no `channel` param.
    /// Response: `{ success, approved: string[] }`.
    pub(crate) async fn handle_channels_pairing_list(&self) -> WsFrame {
        let ctrl = crate::access_control::AccessController::with_persistence(
            self.home_dir.join("access_control.json"),
        );
        let approved = ctrl.runtime_approved_users().await;
        WsFrame::ok_response("", json!({ "success": true, "approved": approved }))
    }

    /// `channels.pairing_revoke` — revoke an approved pairing subject (E2).
    /// Params: `{ subject }`. Response: `{ success, subject, revoked }`.
    pub(crate) async fn handle_channels_pairing_revoke(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let subject = match params.get("subject").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing required parameter: subject"),
        };
        let ctrl = crate::access_control::AccessController::with_persistence(
            self.home_dir.join("access_control.json"),
        );
        ctrl.revoke_user(&subject).await;

        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "channel_pairing_revoke",
                &subject,
                duduclaw_security::audit::Severity::Warning,
                json!({ "actor": ctx.user_id, "source": "dashboard" }),
            ),
        );
        crate::dashboard_feedback::emit(
            &self.home_dir,
            crate::dashboard_feedback::EV_CHANNEL_CONFIG_CHANGED,
            json!({ "action": "pairing_revoke", "subject": subject }),
        )
        .await;

        info!(
            subject = subject.as_str(),
            "channels.pairing_revoke completed"
        );
        WsFrame::ok_response(
            "",
            json!({ "success": true, "subject": subject, "revoked": true }),
        )
    }

    pub(crate) async fn handle_channels_remove(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };

        // Per-agent channel: format "discord:agent_name", "telegram:agent_name", etc.
        if let Some((platform, agent_name)) = channel_type.split_once(':') {
            let channel_section = match platform {
                "discord" | "telegram" | "slack" => platform,
                _ => {
                    return WsFrame::error_response(
                        "",
                        &format!("Unknown channel platform: {platform}"),
                    );
                }
            };

            // Clear the [channels.{platform}] section in the agent's agent.toml
            let agent_name_owned = agent_name.to_string();
            let channel_section_owned = channel_section.to_string();
            if let Err(e) = self
                .update_agent_toml(&agent_name_owned, |table| {
                    if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut())
                    {
                        channels.remove(&channel_section_owned);
                    }
                    Ok(())
                })
                .await
            {
                return WsFrame::error_response("", &format!("Failed to update agent config: {e}"));
            }

            // Hot-stop the per-agent bot
            self.hot_stop_channel(channel_type).await;

            info!(channel_type, "Per-agent channel removed and stopped");
            return WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "type": channel_type,
                }),
            );
        }

        // Global channel removal
        let token_key = match channel_type {
            "line" => "line_channel_token",
            "telegram" => "telegram_bot_token",
            "discord" => "discord_bot_token",
            "slack" => "slack_bot_token",
            "whatsapp" => "whatsapp_access_token",
            "feishu" => "feishu_app_id",
            "googlechat" => "googlechat_service_account_json",
            "teams" => "teams_app_password",
            "wecom" => "wecom_corp_secret",
            "dingtalk" => "dingtalk_app_secret",
            _ => {
                return WsFrame::error_response(
                    "",
                    &format!("Unknown channel type: {channel_type}"),
                );
            }
        };

        // Companion fields cleared alongside the primary token.
        let companion_fields: &[&str] = match channel_type {
            "line" => &["line_channel_secret"],
            "slack" => &["slack_app_token"],
            "whatsapp" => &[
                "whatsapp_phone_number_id",
                "whatsapp_verify_token",
                "whatsapp_app_secret",
            ],
            "feishu" => &["feishu_app_secret", "feishu_verification_token"],
            "googlechat" => &["googlechat_project_number"],
            "teams" => &["teams_app_id", "teams_tenant_id"],
            "wecom" => &[
                "wecom_corp_id",
                "wecom_agent_id",
                "wecom_callback_token",
                "wecom_encoding_aes_key",
            ],
            "dingtalk" => &["dingtalk_app_key"],
            _ => &[],
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
            channels.insert(token_key.to_string(), toml::Value::String(String::new()));
            // Also clear the encrypted version
            let enc_key = format!("{token_key}_enc");
            channels.insert(enc_key, toml::Value::String(String::new()));
            for field in companion_fields {
                channels.insert((*field).to_string(), toml::Value::String(String::new()));
                channels.insert(format!("{field}_enc"), toml::Value::String(String::new()));
            }
        }

        // XC.2: atomic write (temp + rename).
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }

        // Hot-stop: abort the running global channel bot task
        self.hot_stop_channel(channel_type).await;

        // Re-launch per-agent bots since the global bot was deduplicating their tokens.
        let mut restarted_agents = Vec::new();
        let ctx_opt = self.reply_ctx.read().await.clone();
        if let Some(ctx) = ctx_opt {
            let per_agent_handles: Vec<(String, tokio::task::JoinHandle<()>)> = match channel_type {
                "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx).await,
                "telegram" => crate::telegram::start_telegram_bots(&self.home_dir, ctx).await,
                "slack" => crate::slack::start_slack_bots(&self.home_dir, ctx).await,
                _ => Vec::new(),
            };
            for (label, h) in per_agent_handles {
                restarted_agents.push(label.clone());
                self.register_channel_handle(&label, h).await;
            }
        }

        info!(channel_type, restarted = ?restarted_agents, "Channel removed and stopped");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "type": channel_type,
                "restarted_per_agent": restarted_agents,
            }),
        )
    }

    // ── Channel hot-start/stop ────────────────────────────────

    /// Launch a channel bot immediately after config is saved.
    pub(crate) async fn hot_start_channel(&self, channel_type: &str) -> bool {
        let ctx = match self.reply_ctx.read().await.clone() {
            Some(ctx) => ctx,
            None => {
                warn!(
                    channel_type,
                    "Cannot hot-start channel: ReplyContext not available"
                );
                return false;
            }
        };

        // Stop existing instance first (if any)
        self.hot_stop_channel(channel_type).await;

        let home = self.home_dir.clone();
        let handle = match channel_type {
            "telegram" => crate::telegram::start_telegram_bot(&home, ctx).await,
            "discord" => crate::discord::start_discord_bot(&home, ctx).await,
            "slack" => crate::slack::start_slack_bot(&home, ctx).await,
            "line" => {
                // LINE uses a webhook (axum route is always mounted) — no background
                // task. The handler reads the token per request, so saving config is
                // enough; just refresh the connection status so the dashboard flips
                // from "連線中" to connected immediately.
                crate::line::refresh_line_status(&home, ctx.clone()).await;
                info!("LINE channel updated; status refreshed (webhook always mounted)");
                return true;
            }
            "whatsapp" | "feishu" | "googlechat" | "teams" | "wecom" | "dingtalk" => {
                // These webhook routers are mounted at boot with their config
                // baked into router state — a gateway restart is required for
                // a first-time setup to take effect.
                info!(
                    channel_type,
                    "Webhook channel config saved — restart the gateway to (re)mount the endpoint"
                );
                return false;
            }
            _ => None,
        };

        match handle {
            Some(h) => {
                info!(channel_type, "Channel hot-started successfully");
                self.channel_handles
                    .lock()
                    .await
                    .insert(channel_type.to_string(), h);
                true
            }
            None => {
                warn!(
                    channel_type,
                    "Channel hot-start failed (check token validity)"
                );
                false
            }
        }
    }

    /// Stop a running channel bot task.
    pub(crate) async fn hot_stop_channel(&self, channel_type: &str) {
        let mut handles = self.channel_handles.lock().await;
        if let Some(handle) = handles.remove(channel_type) {
            handle.abort();
            info!(channel_type, "Channel bot stopped");
        }
        // Always clear runtime status (handle may already be gone if bot crashed)
        let mut status = self.channel_status.write().await;
        status.remove(channel_type);
    }

    /// Hot-restart per-agent channel bots (Telegram / Discord) after a token
    /// change persisted to agent.toml. Returns the labels that were re-armed
    /// (e.g. `["telegram:agnes"]`).
    ///
    /// Without this, `agents.update` would write the new token but the running
    /// bot loop keeps using the old captured token until gateway restart.
    /// LINE / WhatsApp / Feishu are not handled here — LINE is webhook-based
    /// (no background task), the others lack hot-restart helpers and still
    /// require gateway restart for token changes.
    pub(crate) async fn hot_restart_agent_channels(
        &self,
        channel_types: &[&str],
        agent_name: &str,
    ) -> Vec<String> {
        let ctx = match self.reply_ctx.read().await.clone() {
            Some(ctx) => ctx,
            None => return Vec::new(),
        };

        let mut restarted = Vec::new();
        for ch in channel_types {
            let label = format!("{ch}:{agent_name}");
            self.hot_stop_channel(&label).await;

            let handles: Vec<(String, tokio::task::JoinHandle<()>)> = match *ch {
                "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx.clone()).await,
                "telegram" => {
                    crate::telegram::start_telegram_bots(&self.home_dir, ctx.clone()).await
                }
                "slack" => crate::slack::start_slack_bots(&self.home_dir, ctx.clone()).await,
                _ => Vec::new(),
            };
            for (l, h) in handles {
                if l == label {
                    restarted.push(l.clone());
                }
                self.register_channel_handle(&l, h).await;
            }
        }
        restarted
    }
}
