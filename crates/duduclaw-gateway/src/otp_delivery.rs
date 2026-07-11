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
use crate::config_crypto::read_encrypted_config_field;

/// Sends an already-composed OTP message to a channel DM. Fail-closed: a
/// missing token or a send error is an `Err` — the caller must never fall
/// through to "code sent" when delivery did not happen.
#[async_trait]
pub trait OtpDeliverer: Send + Sync {
    async fn deliver(&self, channel: &str, chat_id: &str, text: &str) -> Result<(), String>;
}

/// Maps a channel to its global `[channels]` bot-token config field. Only the
/// 1:1-DM-capable channels are supported for OTP; anything else is rejected.
fn token_field(channel: &str) -> Option<&'static str> {
    match channel {
        "telegram" => Some("telegram_bot_token"),
        "line" => Some("line_channel_token"),
        "discord" => Some("discord_bot_token"),
        "slack" => Some("slack_bot_token"),
        _ => None,
    }
}

/// Production deliverer: resolves the global channel bot token from
/// `~/.duduclaw/config.toml` (encrypted-field + secret-reference aware) at send
/// time and dispatches through the shared channel-sender factory.
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
        let token = read_encrypted_config_field(&self.home_dir, "channels", field)
            .await
            .ok_or_else(|| format!("channels.{field} not configured"))?;

        let target = ChannelTarget {
            channel_type: channel.to_string(),
            chat_id: chat_id.to_string(),
            token,
            extra_id: None,
        };
        create_sender(&target, self.http.clone())
            .send_text(text)
            .await
            .map_err(|e| format!("otp delivery failed: {e}"))
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
}
