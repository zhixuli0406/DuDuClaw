//! Provider adapters. Never cascade from the proven bot to another account.
use super::{card, Sender, Target};
use std::path::Path;
use serde_json::{json, Value};
use crate::{decision_card::PushedMessage, isolation_http::LiveRequest};

pub(in crate::approval::chat_admin) struct Network<'a> {
    home: &'a Path,
    http: reqwest::Client,
}

impl<'a> Network<'a> {
    pub(in crate::approval::chat_admin) fn new(home: &'a Path) -> Self { Self { home, http: reqwest::Client::new() } }

    pub(in crate::approval::chat_admin) async fn totp_prompt(&self, receipt: &super::receipts::Receipt, text: &str) -> Option<String> {
        duduclaw_mcp_boundary::io_authority::verify_before_io().ok()?;
        if receipt.channel != "telegram" { return None; }
        let target = super::super::sync::target(receipt)?;
        let mid = receipt.message_id.as_deref()?.parse::<i64>().ok().filter(|m| *m > 0)?;
        let token = self.token(&target).await?;
        let response = self.telegram(&token, "sendMessage", json!({"chat_id":receipt.dm,
            "text":text, "reply_parameters":{"message_id":mid,"allow_sending_without_reply":false},
            "reply_markup":{"force_reply":true,"selective":true}})).await.ok()?;
        response["result"]["message_id"].as_i64().filter(|m| *m > 0).map(|m| m.to_string())
    }

    async fn token(&self, target: &Target) -> Option<String> {
        let channel = target.proof.channel.as_str();
        match &target.agent {
            None => crate::config_crypto::read_encrypted_config_field(
                self.home, "channels", crate::otp_delivery::token_field(channel)?,
            ).await.filter(|token| !token.is_empty()),
            Some(agent) => {
                let (home, agent, channel) = (self.home.to_path_buf(), agent.clone(), channel.to_owned());
                let reference = tokio::task::spawn_blocking(move || {
                    if channel != "line" {
                        return crate::config_crypto::agent_channel_token_ref(&home, &agent, &channel);
                    }
                    // LINE uses channel_token; the existing bot-token reader is
                    // for Telegram/Slack/Discord. Keep the same SecretRef rules.
                    let raw = std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml")).ok()?;
                    let root: toml::Value = raw.parse().ok()?;
                    let section = root.get("channels")?.get("line")?;
                    Some(duduclaw_security::secret_ref::SecretRef::from_typed(
                        section.get("channel_token_enc").and_then(toml::Value::as_str),
                        section.get("channel_token").and_then(toml::Value::as_str).unwrap_or(""),
                    ))
                }).await.ok().flatten()?;
                let cfg = duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(self.home).await;
                reference.resolve(&cfg, self.home).await.map(|s| s.expose_owned()).filter(|s| !s.is_empty())
            }
        }
    }

    async fn telegram(&self, token: &str, method: &str, body: Value) -> Result<Value, ()> {
        let response = self.http.post(format!("{}/{method}", crate::telegram::telegram_poll_api_base(token)))
            .json(&body).send_authorized().await.map_err(|_| ())?;
        if !response.status().is_success() { return Err(()); }
        let body: Value = response.json().await.map_err(|_| ())?;
        if body["ok"] != true { return Err(()); }
        Ok(body)
    }
}

#[async_trait::async_trait]
impl Sender for Network<'_> {
    async fn supports(&self, target: &Target) -> bool {
        if target.proof.channel == "googlechat" {
            return crate::googlechat::sending_app_takes_http_clicks(
                self.home, target.agent.as_deref(), &target.proof.proof_dm, true,
            ).await;
        }
        matches!(target.proof.channel.as_str(), "telegram" | "line" | "slack" | "discord")
    }

    async fn send(&self, target: &Target, card: &card::ChatCard, approval_id: &str) -> Result<Option<PushedMessage>, ()> {
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|_| ())?;
        let channel = target.proof.channel.as_str();
        let dm = &target.proof.proof_dm;
        if channel == "googlechat" {
            let message_id = crate::googlechat::send_untrusted_card_as(self.home, target.agent.as_deref(), dm,
                json!({"text": format!("{}\n{}", card.title, card.body),
                    "cardsV2":crate::channel_format::googlechat_approval_card(card)})).await.map_err(|_| ())?;
            return Ok(Some(PushedMessage { edit_chat_id: dm.clone(), message_id }));
        }
        let token = self.token(target).await.ok_or(())?;
        if channel == "telegram" {
            let (approve, deny) = card.button_labels();
            let body = telegram_card(dm, card, approve, deny);
            let response = self.telegram(&token, "sendMessage", body).await?;
            let message_id = response["result"]["message_id"].as_i64().filter(|id| *id > 0).ok_or(())?;
            return Ok(Some(PushedMessage { edit_chat_id: dm.clone(), message_id: message_id.to_string() }));
        }
        let text = format!("{}\n{}", card.title, card.body);
        let markup = crate::channel_format::decision_markup(channel, crate::decision_action::DecisionSource::Approval, approval_id).ok_or(())?;
        if channel == "discord" {
            // proof_dm already identifies the DM. Never run the legacy
            // recipient_id -> DM resolver (or its channel fallback) here.
            let body = crate::channel_sender::card_message_payload(channel, dm, &text, markup, true).map_err(|_| ())?;
            let response = self.http.post(format!("https://discord.com/api/v10/channels/{dm}/messages"))
                .header("Authorization", format!("Bot {token}")).json(&body).send_authorized().await.map_err(|_| ())?;
            if !response.status().is_success() { return Err(()); }
            let body: Value = response.json().await.map_err(|_| ())?;
            let message_id = body["id"].as_str().filter(|id| id.parse::<u64>().is_ok()).ok_or(())?.to_owned();
            return Ok(Some(PushedMessage { edit_chat_id: dm.clone(), message_id }));
        }
        crate::channel_sender::send_untrusted_card(self.home, &self.http, channel, &token, dm, &text, Some(markup))
            .await.map_err(|_| ())
    }

    async fn replace(&self, target: &Target, message: Option<&PushedMessage>) -> bool {
        const TEXT: &str = "請到儀表板處理";
        let channel = target.proof.channel.as_str();
        if channel != "line" {
            let Some(message) = message else { return false; };
            return self.edit(target, message, TEXT).await;
        }
        // LINE cannot edit a pushed message. Replace it with a plain follow-up;
        // the old buttons have no receipt and therefore no decision authority.
        let Some(token) = self.token(target).await else { return false; };
        crate::channel_sender::send_plain_text(self.home, &self.http, channel, &token, &target.proof.proof_dm, TEXT).await
    }

    async fn edit(&self, target: &Target, message: &PushedMessage, text: &str) -> bool {
        if duduclaw_mcp_boundary::io_authority::verify_before_io().is_err() { return false; }
        let channel = target.proof.channel.as_str();
        if channel == "googlechat" {
            return crate::googlechat::replace_approval_card_as(self.home, target.agent.as_deref(), &message.message_id, text).await.is_ok();
        }
        if !matches!(channel, "telegram" | "slack" | "discord") { return false; }
        let Some(token) = self.token(target).await else { return false; };
        if channel == "telegram" {
            let Some(mid) = message.message_id.parse::<i64>().ok().filter(|id| *id > 0) else { return false; };
            return self.telegram(&token, "editMessageText", json!({"chat_id":target.proof.proof_dm,
                "message_id":mid, "text":text, "reply_markup":{"inline_keyboard":[]}})).await.is_ok();
        }
        crate::decision_card::edit_channel_message(&self.http, channel, &token,
            &message.edit_chat_id, &message.message_id, text).await.is_ok()
    }

    async fn remind(&self, target: &Target, message: Option<&str>, text: &str) -> bool {
        let channel = target.proof.channel.as_str();
        let dm = &target.proof.proof_dm;
        if channel == "googlechat" {
            return crate::googlechat::remind_approval_card_as(self.home, target.agent.as_deref(), dm, message, text).await.is_ok();
        }
        let Some(token) = self.token(target).await else { return false; };
        if channel == "telegram" {
            let Some(mid) = message.and_then(|m| m.parse::<i64>().ok()) else { return false; };
            return self.telegram(&token, "sendMessage", telegram_reminder(dm, mid, text)).await.is_ok();
        }
        if channel == "discord" {
            let mut body = json!({"content":text, "allowed_mentions":{"parse":[]}, "flags":4});
            if let Some(mid) = message { body["message_reference"] = json!({"message_id":mid, "fail_if_not_exists":false}); }
            return self.http.post(format!("https://discord.com/api/v10/channels/{dm}/messages"))
                .header("Authorization", format!("Bot {token}")).json(&body).send_authorized().await
                .is_ok_and(|r| r.status().is_success());
        }
        if channel == "slack" {
            let mut body = json!({"channel":dm, "text":text, "mrkdwn":false, "parse":"none", "unfurl_links":false});
            if let Some(mid) = message { body["thread_ts"] = json!(mid); }
            let Ok(response) = self.http.post("https://slack.com/api/chat.postMessage")
                .bearer_auth(token).json(&body).send_authorized().await else { return false; };
            return response.json::<Value>().await.is_ok_and(|v| v["ok"] == true);
        }
        crate::channel_sender::send_plain_text(self.home, &self.http, channel, &token, dm, text).await
    }
}

fn telegram_card(dm: &str, card: &card::ChatCard, approve: &str, deny: &str) -> Value {
    json!({"chat_id":dm, "text":card::render_telegram_html(card), "parse_mode":"HTML",
        "link_preview_options":{"is_disabled":true}, "reply_markup":{"inline_keyboard":[[
            {"text":approve,"callback_data":card.approve_data}, {"text":deny,"callback_data":card.deny_data}
        ]]}})
}

fn telegram_reminder(dm: &str, message: i64, text: &str) -> Value {
    json!({"chat_id":dm, "text":text, "reply_to_message_id":message, "link_preview_options":{"is_disabled":true}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_payload_uses_html_and_reminder_replies_without_buttons() {
        let card = card::ChatCard { title:"<title>".into(), body:"<body>".into(), approve_data:"approve".into(), deny_data:"deny".into() };
        let payload = telegram_card("123", &card, "核准", "拒絕");
        assert_eq!(payload["parse_mode"], "HTML");
        assert_eq!(payload["reply_markup"]["inline_keyboard"][0][0]["callback_data"], "approve");
        assert!(payload["text"].as_str().unwrap().contains("&lt;body&gt;"));
        let reminder = telegram_reminder("123", 42, "soon");
        assert_eq!(reminder["reply_to_message_id"], 42);
        assert!(reminder.get("reply_markup").is_none());
        assert!(reminder.get("parse_mode").is_none());
    }

    #[tokio::test]
    async fn tokens_are_pinned_to_the_proven_bot_without_reports_to_fallback() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/anna")).unwrap();
        std::fs::write(home.path().join("config.toml"), "[channels]\ntelegram_bot_token='shared-fixture'\n").unwrap();
        std::fs::write(home.path().join("agents/anna/agent.toml"),
            "[agent]\nname='anna'\nreports_to='boss'\n[channels.telegram]\nbot_token='own-fixture'\n").unwrap();
        let network = Network::new(home.path());
        let mut target = Target { user_id:uuid::Uuid::new_v4().to_string(), agent:None,
            proof:duduclaw_auth::otp::ChatApprovalProof { channel:"telegram".into(),
                channel_user_id:"123".into(), proof_dm:"123".into(), proof_account:"telegram".into(), proven_at:0 } };
        assert_eq!(network.token(&target).await.as_deref(), Some("shared-fixture"));
        target.agent = Some("anna".into()); target.proof.proof_account = "telegram:anna".into();
        assert_eq!(network.token(&target).await.as_deref(), Some("own-fixture"));
        std::fs::write(home.path().join("agents/anna/agent.toml"), "[agent]\nname='anna'\nreports_to='boss'\n").unwrap();
        assert!(network.token(&target).await.is_none());
    }

}
