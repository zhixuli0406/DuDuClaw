//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// W0-2: `channels.test` used to only check whether a token FIELD was
    /// non-empty — a revoked/expired/permission-stripped token reported
    /// "success" and the user only found out the channel was actually dead
    /// when a real message needed to go out (breakpoint #5,
    /// `commercial/docs/ux-redesign-2026-08/01-current-state-map.md`).
    ///
    /// Now: when a live destination can be found, an actual test message is
    /// sent through the shared `channel_sender::ChannelSender` impls (never a
    /// bespoke duplicate of their HTTP logic — the `require_api_success` /
    /// `require_slack_ok` / `require_feishu_code_zero` fix in
    /// `channel_sender.rs` makes those impls detect a platform-level
    /// rejection, which they previously did not). When no destination is
    /// known yet (nobody has set `agent.toml [proactive] notify_chat_id` for
    /// this platform, and the platform's own conversation-reference store has
    /// never seen an inbound message), the result is honestly degraded to
    /// `credential_only` — never reported as a successful send.
    pub(crate) async fn handle_channels_test(&self, params: Value) -> WsFrame {
        let channel_type = params
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        info!(channel_type, "channels.test requested");

        let (platform, agent_hint) = match channel_type.split_once(':') {
            Some((p, a)) => (p, Some(a)),
            None => (channel_type, None),
        };

        match agent_hint {
            Some(agent_name) => self.test_per_agent_channel(platform, agent_name).await,
            None => self.test_global_channel(platform).await,
        }
    }

    /// Per-agent channel test (`discord:<agent>` / `telegram:<agent>` /
    /// `slack:<agent>`) — the only platforms with a real per-agent bot token.
    pub(crate) async fn test_per_agent_channel(&self, platform: &str, agent_name: &str) -> WsFrame {
        if !matches!(platform, "discord" | "telegram" | "slack") {
            return WsFrame::error_response("", &format!("Unknown channel platform: {platform}"));
        }

        let reg = self.registry.read().await;
        let Some(agent) = reg.get(agent_name) else {
            drop(reg);
            return WsFrame::error_response("", &format!("找不到 AI 員工「{agent_name}」"));
        };
        let configured = agent_has_own_channel_token(agent, platform);
        let target = agent_proactive_target(agent, platform);
        drop(reg);

        if !configured {
            return channel_test_result(
                false,
                "credential_only",
                &format!("{platform} token 尚未設定，請先在通道設定中填入。"),
            );
        }

        let Some(chat_id) = target else {
            return channel_test_result(
                false,
                "credential_only",
                &format!(
                    "{platform} 憑證已設定，但「{agent_name}」尚未設定預設通知目的地\
                     （agent.toml [proactive] notify_chat_id）。僅驗證憑證存在，未實際發送。"
                ),
            );
        };

        let Some(token) =
            crate::goal_notify::channel_token(&self.home_dir, agent_name, platform).await
        else {
            return channel_test_result(
                false,
                "credential_only",
                &format!("{platform} token 讀取失敗，僅驗證憑證存在，未實際發送。"),
            );
        };

        self.send_channel_test_message(platform, &chat_id, &token)
            .await
    }

    /// Global (single shared bot) channel test — `line` / `telegram` /
    /// `discord` / `slack` / `whatsapp` / `feishu` / `googlechat` / `teams` /
    /// `wecom` / `dingtalk`.
    pub(crate) async fn test_global_channel(&self, platform: &str) -> WsFrame {
        let token_key = match platform {
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
                return WsFrame::error_response("", &format!("Unknown channel type: {platform}"));
            }
        };

        let token = crate::config_crypto::read_encrypted_config_field(
            &self.home_dir,
            "channels",
            token_key,
        )
        .await
        .filter(|t| !t.is_empty());

        let Some(token) = token else {
            return channel_test_result(
                false,
                "credential_only",
                &format!("{platform} token 尚未設定，請先在通道設定中填入。"),
            );
        };

        let Some((_agent_name, chat_id)) = self.resolve_global_test_destination(platform).await
        else {
            return channel_test_result(
                false,
                "credential_only",
                &format!(
                    "{platform} 憑證已設定，但尚無可用的通知目的地（尚未有 AI 員工設定 \
                     [proactive] notify_chat_id，此通道也還沒收過任何一則訊息）。\
                     僅驗證憑證存在，未實際發送。"
                ),
            );
        };

        self.send_channel_test_message(platform, &chat_id, &token)
            .await
    }

    /// Best-effort destination for a GLOBAL channel test: prefer
    /// `[general] default_agent`'s `[proactive]` target, else the first
    /// (alphabetical) agent whose `[proactive] notify_channel` matches and
    /// who does NOT own a distinct per-agent bot for this platform (that
    /// agent's chat id belongs to a different bot token and would be
    /// unreachable by the global one under test).
    pub(crate) async fn resolve_global_test_destination(&self, platform: &str) -> Option<(String, String)> {
        let reg = self.registry.read().await;
        let per_agent_capable = matches!(platform, "discord" | "telegram" | "slack");

        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let default_agent = table
            .get("general")
            .and_then(|v| v.as_table())
            .and_then(|g| g.get("default_agent"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let mut names: Vec<String> = reg
            .list()
            .iter()
            .map(|a| a.config.agent.name.clone())
            .collect();
        names.sort();
        if let Some(d) = default_agent {
            if let Some(pos) = names.iter().position(|n| *n == d) {
                let d = names.remove(pos);
                names.insert(0, d);
            }
        }

        for name in names {
            let Some(agent) = reg.get(&name) else {
                continue;
            };
            if per_agent_capable && agent_has_own_channel_token(agent, platform) {
                continue;
            }
            if let Some(chat_id) = agent_proactive_target(agent, platform) {
                return Some((name, chat_id));
            }
        }
        None
    }

    /// Actually dispatch the zh-TW test message through the shared
    /// `channel_sender::ChannelSender` implementations — reused verbatim, not
    /// re-derived, so `channels.test` exercises the exact same code path a
    /// real proactive/goal/approval push would use.
    pub(crate) async fn send_channel_test_message(
        &self,
        platform: &str,
        chat_id: &str,
        token: &str,
    ) -> WsFrame {
        const TEST_MESSAGE: &str = "✅ DuDuClaw 通道測試成功——這則訊息代表此通道可正常送出。";

        let sender: Box<dyn crate::channel_sender::ChannelSender> = match platform {
            "googlechat" => crate::channel_sender::create_googlechat_sender(
                self.home_dir.clone(),
                chat_id.to_string(),
                String::new(),
            ),
            "teams" => crate::channel_sender::create_teams_sender(
                self.home_dir.clone(),
                chat_id.to_string(),
                String::new(),
            ),
            "wecom" => crate::channel_sender::create_wecom_sender(
                self.home_dir.clone(),
                chat_id.to_string(),
            ),
            "dingtalk" => crate::channel_sender::create_dingtalk_sender(
                self.home_dir.clone(),
                chat_id.to_string(),
                String::new(),
            ),
            _ => {
                // whatsapp needs the (plaintext, non-secret) phone_number_id
                // alongside the access token — every other token-based
                // channel leaves `extra_id` unset.
                let extra_id = if platform == "whatsapp" {
                    crate::config_crypto::read_encrypted_config_field(
                        &self.home_dir,
                        "channels",
                        "whatsapp_phone_number_id",
                    )
                    .await
                    .filter(|s| !s.is_empty())
                } else {
                    None
                };
                let target = crate::channel_sender::ChannelTarget {
                    channel_type: platform.to_string(),
                    chat_id: chat_id.to_string(),
                    token: token.to_string(),
                    extra_id,
                };
                crate::channel_sender::create_sender(&target, reqwest::Client::new())
            }
        };

        match sender.send_text(TEST_MESSAGE).await {
            Ok(()) => channel_test_result(
                true,
                "live",
                &format!("測試訊息已送出，請至 {platform} 確認收到。"),
            ),
            Err(e) => {
                warn!(platform, chat_id, error = %e.0, "channels.test: live send failed");
                let detail = classify_channel_send_error(&e.0);
                channel_test_result(false, "live", &detail)
            }
        }
    }
}
