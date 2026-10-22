//! Editable thinking-status messages for Telegram, Discord and Slack
//! ([`super::channel_presenter::StatusSurface`]). Best effort: a failed post
//! or edit is logged at debug level and never affects the reply.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::channel_format::{self, StatusView};
use super::channel_presenter::StatusSurface;

const DISCORD_API: &str = "https://discord.com/api/v10";
const SLACK_API: &str = "https://slack.com/api";

/// Test builds route a synthetic token's requests to a local fixture.
fn routed(token: &str, url: String) -> String {
    #[cfg(test)]
    {
        crate::test_channel_provider::url(token, &url)
    }
    #[cfg(not(test))]
    {
        let _ = token;
        url
    }
}

async fn send_json(req: reqwest::RequestBuilder, what: &'static str) -> Option<Value> {
    match req.send().await {
        Ok(resp) => match resp.json::<Value>().await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::debug!(error = %e, what, "thinking status: unreadable response");
                None
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, what, "thinking status request failed");
            None
        }
    }
}

/// Telegram: `sendMessage` / `editMessageText` / `deleteMessage` against the
/// bot's `api_base` (`https://api.telegram.org/bot<token>`).
pub struct TelegramStatus {
    pub http: reqwest::Client,
    pub api_base: String,
    pub chat_id: i64,
    pub thread_id: Option<i64>,
}

#[async_trait]
impl StatusSurface for TelegramStatus {
    async fn post(&self, view: &StatusView) -> Option<String> {
        let (text, mode) = channel_format::telegram(view);
        let mut body = json!({"chat_id": self.chat_id, "text": text, "parse_mode": mode,
            "disable_notification": true});
        if let Some(tid) = self.thread_id {
            body["message_thread_id"] = json!(tid);
        }
        let req = self
            .http
            .post(format!("{}/sendMessage", self.api_base))
            .json(&body);
        let data = send_json(req, "telegram sendMessage").await?;
        data.pointer("/result/message_id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string())
    }

    async fn edit(&self, id: &str, view: &StatusView) {
        let Ok(message_id) = id.parse::<i64>() else {
            return;
        };
        let (text, mode) = channel_format::telegram(view);
        let req = self
            .http
            .post(format!("{}/editMessageText", self.api_base))
            .json(&json!({"chat_id": self.chat_id, "message_id": message_id,
                "text": text, "parse_mode": mode}));
        send_json(req, "telegram editMessageText").await;
    }

    async fn delete(&self, id: &str) {
        let Ok(message_id) = id.parse::<i64>() else {
            return;
        };
        let req = self
            .http
            .post(format!("{}/deleteMessage", self.api_base))
            .json(&json!({"chat_id": self.chat_id, "message_id": message_id}));
        send_json(req, "telegram deleteMessage").await;
    }
}

/// Discord: channel messages, mentions disabled (a summary can never ping).
pub struct DiscordStatus {
    pub http: reqwest::Client,
    pub token: String,
    pub channel_id: String,
}

impl DiscordStatus {
    fn url(&self, suffix: &str) -> String {
        routed(
            &self.token,
            format!("{DISCORD_API}/channels/{}/messages{suffix}", self.channel_id),
        )
    }

    fn body(view: &StatusView) -> Value {
        json!({"content": channel_format::discord(view), "allowed_mentions": {"parse": []}})
    }
}

#[async_trait]
impl StatusSurface for DiscordStatus {
    async fn post(&self, view: &StatusView) -> Option<String> {
        let req = self
            .http
            .post(self.url(""))
            .header("Authorization", format!("Bot {}", self.token))
            .json(&Self::body(view));
        let data = send_json(req, "discord post").await?;
        data.get("id").and_then(Value::as_str).map(str::to_string)
    }

    async fn edit(&self, id: &str, view: &StatusView) {
        let req = self
            .http
            .patch(self.url(&format!("/{id}")))
            .header("Authorization", format!("Bot {}", self.token))
            .json(&Self::body(view));
        send_json(req, "discord edit").await;
    }

    async fn delete(&self, id: &str) {
        let req = self
            .http
            .delete(self.url(&format!("/{id}")))
            .header("Authorization", format!("Bot {}", self.token));
        if let Err(e) = req.send().await {
            tracing::debug!(error = %e, "thinking status: discord delete failed");
        }
    }
}

/// Slack: `chat.postMessage` / `chat.update` / `chat.delete` in the thread.
pub struct SlackStatus {
    pub http: reqwest::Client,
    pub token: String,
    pub channel: String,
    pub thread_ts: Option<String>,
}

impl SlackStatus {
    fn call(&self, method: &str, body: Value) -> reqwest::RequestBuilder {
        self.http
            .post(routed(&self.token, format!("{SLACK_API}/{method}")))
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&body)
    }
}

#[async_trait]
impl StatusSurface for SlackStatus {
    async fn post(&self, view: &StatusView) -> Option<String> {
        let (text, blocks) = channel_format::slack(view);
        let mut body = json!({"channel": self.channel, "text": text, "blocks": blocks});
        if let Some(th) = &self.thread_ts {
            body["thread_ts"] = json!(th);
        }
        let data = send_json(self.call("chat.postMessage", body), "slack post").await?;
        (data.get("ok").and_then(Value::as_bool) == Some(true))
            .then(|| data.get("ts").and_then(Value::as_str).map(str::to_string))
            .flatten()
    }

    async fn edit(&self, id: &str, view: &StatusView) {
        let (text, blocks) = channel_format::slack(view);
        let body = json!({"channel": self.channel, "ts": id, "text": text, "blocks": blocks});
        send_json(self.call("chat.update", body), "slack update").await;
    }

    async fn delete(&self, id: &str) {
        let body = json!({"channel": self.channel, "ts": id});
        send_json(self.call("chat.delete", body), "slack delete").await;
    }
}

#[cfg(test)]
#[path = "channel_surfaces_tests.rs"]
mod tests;
