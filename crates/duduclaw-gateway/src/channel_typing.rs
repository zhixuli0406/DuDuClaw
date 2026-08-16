//! Cross-channel typing / loading indicators.
//!
//! Each platform signals "the bot is working" differently:
//!
//! | Platform  | API                                             | Shows for | Re-send    |
//! |-----------|-------------------------------------------------|-----------|------------|
//! | Telegram  | `sendChatAction` `typing`                       | ≤5 s      | every 4 s  |
//! | Discord   | `POST /channels/{id}/typing` (in discord.rs)    | ~10 s     | every 8 s  |
//! | LINE      | `POST /v2/bot/chat/loading/start` (1:1 only)    | ≤60 s     | every 50 s |
//! | WhatsApp  | `typing_indicator` on the inbound message id    | ≤25 s     | once only  |
//! | Slack     | `assistant.threads.setStatus`                   | ≤2 min    | every 100 s|
//! | MS Teams  | `{"type":"typing"}` activity (in msteams.rs)    | ~3 s      | every 3 s  |
//!
//! `TypingGuard` owns a refresh loop and stops it on drop (RAII, panic-safe)
//! — same pattern as the original Discord implementation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// RAII guard: fires `refresh()` immediately and then every `interval`
/// until dropped. All refresh errors are swallowed (indicators are
/// best-effort; they must never break the reply path).
pub struct TypingGuard {
    active: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for TypingGuard {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.handle.abort();
    }
}

impl TypingGuard {
    /// Start a typing loop. `refresh` is called once right away, then every
    /// `interval` until the guard is dropped.
    pub fn start<F, Fut>(interval: Duration, refresh: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let active = Arc::new(AtomicBool::new(true));
        let flag = active.clone();
        let handle = tokio::spawn(async move {
            loop {
                if !flag.load(Ordering::Acquire) {
                    break;
                }
                refresh().await;
                tokio::time::sleep(interval).await;
            }
        });
        Self { active, handle }
    }
}

/// Telegram: `sendChatAction` typing loop (status lasts ≤5 s → refresh 4 s).
pub fn telegram_typing(
    client: reqwest::Client,
    api_base: String,
    chat_id: i64,
    message_thread_id: Option<i64>,
) -> TypingGuard {
    TypingGuard::start(Duration::from_secs(4), move || {
        let client = client.clone();
        let url = format!("{api_base}/sendChatAction");
        async move {
            let mut body = serde_json::json!({ "chat_id": chat_id, "action": "typing" });
            if let Some(tid) = message_thread_id {
                body["message_thread_id"] = serde_json::json!(tid);
            }
            let _ = client.post(&url).json(&body).send().await;
        }
    })
}

/// LINE: loading animation (1:1 chats only; 60 s max → refresh every 50 s).
/// LINE silently no-ops (202) for group chats and users not viewing the
/// chat, so it is safe to fire unconditionally with a user id.
pub fn line_loading(client: reqwest::Client, channel_token: String, user_id: String) -> TypingGuard {
    TypingGuard::start(Duration::from_secs(50), move || {
        let client = client.clone();
        let token = channel_token.clone();
        let chat_id = user_id.clone();
        async move {
            let _ = client
                .post("https://api.line.me/v2/bot/chat/loading/start")
                .bearer_auth(&token)
                .json(&serde_json::json!({ "chatId": chat_id, "loadingSeconds": 60 }))
                .send()
                .await;
        }
    })
}

/// WhatsApp: typing indicator tied to the inbound message id — shows for
/// ≤25 s or until the reply arrives, and also marks the message read.
/// One-shot (the API can't be refreshed without a new inbound message).
pub async fn whatsapp_typing_once(
    client: &reqwest::Client,
    access_token: &str,
    phone_number_id: &str,
    inbound_message_id: &str,
) {
    let url = format!("https://graph.facebook.com/v20.0/{phone_number_id}/messages");
    let _ = client
        .post(&url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": inbound_message_id,
            "typing_indicator": { "type": "text" }
        }))
        .send()
        .await;
}

/// Slack: AI-app status under the composer (auto-clears when the app
/// replies; 2-min timeout → refresh every 100 s). Requires the thread's
/// `thread_ts`; fails soft on workspaces/apps without the feature.
pub fn slack_status(
    client: reqwest::Client,
    bot_token: String,
    channel_id: String,
    thread_ts: String,
) -> TypingGuard {
    TypingGuard::start(Duration::from_secs(100), move || {
        let client = client.clone();
        let token = bot_token.clone();
        let channel = channel_id.clone();
        let ts = thread_ts.clone();
        async move {
            let _ = client
                .post("https://slack.com/api/assistant.threads.setStatus")
                .header("Authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "channel_id": channel,
                    "thread_ts": ts,
                    "status": "正在思考…"
                }))
                .send()
                .await;
        }
    })
}

/// Build a typing/loading indicator guard for an already-resolved
/// `(channel_type, chat_id, thread_id, token)` tuple, if that channel type
/// has typing support wired up here. Used by task-dispatch paths (bus
/// delegation, SQLite queue, cron, reminders) which resolve their own
/// channel token via each subsystem's existing cascade and then just need
/// "start typing, stop on drop" — they should never need to know the
/// per-platform API shape.
///
/// Returns `None` for unsupported channel types, an empty token, or a
/// chat id that fails to parse for the target platform's expected id type
/// (e.g. Telegram wants a numeric chat id). Always best-effort: callers
/// must not let a `None` here affect task execution.
pub fn typing_guard_for(
    client: reqwest::Client,
    channel_type: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    token: &str,
) -> Option<TypingGuard> {
    if token.is_empty() {
        return None;
    }
    match channel_type {
        "telegram" => {
            let cid = chat_id.parse::<i64>().ok()?;
            let tid = thread_id.and_then(|t| t.parse::<i64>().ok());
            let api_base = format!("https://api.telegram.org/bot{token}");
            Some(telegram_typing(client, api_base, cid, tid))
        }
        _ => {
            // WP-9B: this helper is deliberately narrower than the channel
            // capability table — several channels (discord/line/whatsapp/
            // slack/teams) DO have a typing API (see the module doc table
            // above and `crate::channel_capabilities`), but their live
            // handlers resolve the extra per-message context (thread_ts,
            // inbound message id, …) this cross-cutting cron/dispatch path
            // doesn't have. `debug!` (not `warn!`) because this path runs on
            // every dispatch tick for every channel — a `warn!` here would
            // be log spam for expected, by-design degradation rather than a
            // signal worth alerting on.
            crate::channel_capabilities::debug_unsupported(
                channel_type,
                crate::channel_capabilities::Capability::TypingIndicator,
                "typing_guard_for: not wired for this channel's task-dispatch path",
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[tokio::test]
    async fn guard_fires_and_stops_on_drop() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let guard = TypingGuard::start(Duration::from_millis(10), move || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(Duration::from_millis(35)).await;
        drop(guard);
        let at_drop = count.load(Ordering::SeqCst);
        assert!(at_drop >= 2, "expected ≥2 refreshes, got {at_drop}");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(count.load(Ordering::SeqCst), at_drop, "loop kept running after drop");
    }

    #[tokio::test]
    async fn typing_guard_for_rejects_bad_input() {
        let client = reqwest::Client::new();
        // Empty token — never start a guard even for a supported channel.
        assert!(typing_guard_for(client.clone(), "telegram", "12345", None, "").is_none());
        // Unsupported channel type.
        assert!(typing_guard_for(client.clone(), "discord", "12345", None, "tok").is_none());
        // Non-numeric Telegram chat id.
        assert!(typing_guard_for(client.clone(), "telegram", "not-a-number", None, "tok").is_none());
    }

    #[tokio::test]
    async fn typing_guard_for_accepts_valid_telegram() {
        let client = reqwest::Client::new();
        let guard = typing_guard_for(client, "telegram", "-1001234567890", Some("7"), "tok");
        assert!(guard.is_some());
    }
}
