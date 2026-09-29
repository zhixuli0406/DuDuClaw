//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// WP9: mint a one-time bind token + Telegram deep-link for a target AI
    /// employee on the company's shared Telegram bot. The frontend renders a
    /// QR of the returned `deep_link`. The bot username is resolved live from
    /// the configured global token via getMe — never hardcoded. Admin only.
    /// LINE OA add-friend payload (WP1.1 LINE QR onboarding). The dashboard
    /// renders the QR locally from `add_friend_url` — no external QR service.
    pub(crate) async fn handle_line_add_friend(&self) -> WsFrame {
        match crate::line::fetch_line_add_friend_info(&self.home_dir).await {
            Ok((add_friend_url, basic_id, display_name)) => WsFrame::ok_response(
                "",
                json!({
                    "add_friend_url": add_friend_url,
                    "basic_id": basic_id,
                    "display_name": display_name,
                }),
            ),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_telegram_bind_token(&self, params: Value) -> WsFrame {
        let agent = params
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if agent.is_empty() {
            return WsFrame::error_response("", "缺少 agent 參數");
        }
        // Fail-closed: the target agent must exist in the registry.
        if self.registry.read().await.get(agent).is_none() {
            return WsFrame::error_response("", &format!("找不到 AI 員工「{agent}」"));
        }

        let ttl_minutes = params
            .get("ttl_minutes")
            .and_then(|v| v.as_i64())
            .unwrap_or(crate::agent_binding::DEFAULT_TTL_MINUTES);
        let max_uses = params
            .get("max_uses")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .unwrap_or(crate::agent_binding::DEFAULT_MAX_USES);

        // The shared bot is the global config.toml token.
        let token = match crate::config_crypto::read_encrypted_config_field(
            &self.home_dir,
            "channels",
            "telegram_bot_token",
        )
        .await
        {
            Some(t) if !t.is_empty() => t,
            _ => {
                return WsFrame::error_response(
                    "",
                    "尚未設定共用 Telegram Bot Token（請先在通道新增全域 Telegram bot）",
                );
            }
        };

        // Resolve the bot username live — needed to build the t.me deep-link.
        let bot_username = match fetch_telegram_bot_username(&token).await {
            Some(u) if !u.is_empty() => u,
            _ => {
                return WsFrame::error_response(
                    "",
                    "無法連線 Telegram 取得 bot 帳號，請確認 Bot Token 有效",
                );
            }
        };

        // Cross-process safe: the store reloads from disk before every op, so
        // the gateway's ReplyContext instance sees this write on redeem.
        let store = crate::agent_binding::AgentBindingStore::with_persistence(
            self.home_dir.join("agent_bindings.json"),
        );
        let bind_token = store
            .generate_bind_token("telegram", agent, ttl_minutes, max_uses)
            .await;
        let deep_link = format!("https://t.me/{bot_username}?start={bind_token}");

        let effective_ttl = if ttl_minutes <= 0 {
            crate::agent_binding::DEFAULT_TTL_MINUTES
        } else {
            ttl_minutes.min(crate::agent_binding::MAX_TTL_MINUTES)
        };
        let effective_uses = if max_uses == 0 {
            crate::agent_binding::DEFAULT_MAX_USES
        } else {
            max_uses.min(crate::agent_binding::MAX_USES_LIMIT)
        };

        WsFrame::ok_response(
            "",
            json!({
                "agent": agent,
                "token": bind_token,
                "bot_username": bot_username,
                "deep_link": deep_link,
                "expires_in_minutes": effective_ttl,
                "max_uses": effective_uses,
            }),
        )
    }
}
