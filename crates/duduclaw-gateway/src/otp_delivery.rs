//! OTP delivery abstraction (WP12 blocker resolution).
//!
//! The login OTP handler must push a code to a user's 1:1 channel DM **before**
//! they are authenticated, but `AppState` deliberately holds no channel config
//! (tokens live encrypted in `config.toml`). Rather than thread raw config +
//! the secret manager into the critical auth handler, we invert the dependency:
//! a thin [`OtpDeliverer`] trait is injected into `AppState`, and the concrete
//! [`ConfigOtpDeliverer`] resolves the per-channel bot token on demand through
//! the existing `config_crypto` helper and sends via the existing
//! `channel_sender` factory. The handler stays transport-agnostic and the whole
//! thing is trivially mockable in tests.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::channel_sender::{create_sender, ChannelTarget};
use crate::config_crypto::channel_dm_token_candidates;

/// Sends an already-composed OTP message to a channel DM. Fail-closed: a
/// missing token or a send error is an `Err` — the caller must never fall
/// through to "code sent" when delivery did not happen.
#[async_trait]
pub trait OtpDeliverer: Send + Sync {
    async fn deliver(&self, channel: &str, chat_id: &str, text: &str) -> Result<(), String>;
}

/// Maps a channel to its global `[channels]` bot-token config field. Only the
/// 1:1-DM-capable channels are supported for OTP; anything else is rejected.
/// Also reused by `install_notify` (same "DM a linked dashboard user" shape).
pub(crate) fn token_field(channel: &str) -> Option<&'static str> {
    match channel {
        "telegram" => Some("telegram_bot_token"),
        "line" => Some("line_channel_token"),
        "discord" => Some("discord_bot_token"),
        "slack" => Some("slack_bot_token"),
        _ => None,
    }
}

/// Production deliverer: resolves every candidate bot token — global
/// `[channels]` first, then per-agent `[channels.<channel>]` (encrypted-field
/// + secret-reference aware) — at send time and tries each through the shared
/// channel-sender factory until one delivery succeeds. The per-agent fallback
/// exists because a deployment whose only bot is agent-scoped must still be
/// able to DM login codes (2026-08-13 outage: global token cleared when the
/// Telegram channel moved onto the agent, OTP silently died while the bot
/// itself stayed online).
pub struct ConfigOtpDeliverer {
    home_dir: PathBuf,
    http: reqwest::Client,
}

impl ConfigOtpDeliverer {
    pub fn new(home_dir: PathBuf, http: reqwest::Client) -> Self {
        Self { home_dir, http }
    }
}

#[async_trait]
impl OtpDeliverer for ConfigOtpDeliverer {
    async fn deliver(&self, channel: &str, chat_id: &str, text: &str) -> Result<(), String> {
        let field = token_field(channel)
            .ok_or_else(|| format!("channel {channel} does not support OTP delivery"))?;
        let candidates = channel_dm_token_candidates(&self.home_dir, channel).await;
        if candidates.is_empty() {
            return Err(format!(
                "channels.{field} not configured (no global or agent-level {channel} bot token)"
            ));
        }

        // Try each candidate until one delivery succeeds. On Telegram only the
        // bot the user has actually talked to can DM them, and we don't record
        // which bot that is at bind time — trying in deterministic order is
        // the robust resolution. Fail-closed: all-candidates-failed is an Err.
        let mut last_err = String::new();
        for token in candidates {
            let target = ChannelTarget {
                channel_type: channel.to_string(),
                chat_id: chat_id.to_string(),
                token,
                extra_id: None,
            };
            match create_sender(&target, self.http.clone()).send_text(text).await {
                Ok(()) => return Ok(()),
                Err(e) => last_err = format!("otp delivery failed: {e}"),
            }
        }
        Err(last_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A test deliverer that records what it was asked to send.
    #[derive(Default)]
    pub struct MockDeliverer {
        pub sent: Arc<Mutex<Vec<(String, String, String)>>>,
        pub fail: bool,
    }

    #[async_trait]
    impl OtpDeliverer for MockDeliverer {
        async fn deliver(&self, channel: &str, chat_id: &str, text: &str) -> Result<(), String> {
            if self.fail {
                return Err("mock failure".into());
            }
            self.sent
                .lock()
                .unwrap()
                .push((channel.into(), chat_id.into(), text.into()));
            Ok(())
        }
    }

    #[test]
    fn token_field_only_supports_dm_channels() {
        assert_eq!(token_field("telegram"), Some("telegram_bot_token"));
        assert_eq!(token_field("discord"), Some("discord_bot_token"));
        assert_eq!(token_field("webchat"), None);
        assert_eq!(token_field("feishu"), None);
    }

    #[tokio::test]
    async fn mock_records_delivery() {
        let mock = MockDeliverer::default();
        mock.deliver("telegram", "tg-123", "code 000000").await.unwrap();
        assert_eq!(mock.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn mock_failure_propagates() {
        let mock = MockDeliverer { fail: true, ..Default::default() };
        assert!(mock.deliver("telegram", "tg-123", "x").await.is_err());
    }

    /// Fail-closed when neither the global config nor any agent carries a
    /// token — and the error must say so (it lands in the audit log the
    /// operator reads during an outage).
    #[tokio::test]
    async fn no_tokens_anywhere_is_a_configured_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[channels]\n").unwrap();
        let d = ConfigOtpDeliverer::new(home.path().to_path_buf(), reqwest::Client::new());
        let err = d.deliver("telegram", "tg-123", "code").await.unwrap_err();
        assert!(err.contains("not configured"), "got: {err}");
        assert!(err.contains("agent-level"), "got: {err}");
    }
}
