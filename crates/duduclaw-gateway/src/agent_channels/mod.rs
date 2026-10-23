//! Per-employee channel identities: an AI employee's own Telegram bot and its
//! own Google Chat app.
//!
//! - Telegram: `agent.toml [channels.telegram] bot_token_enc`, one long-poll
//!   per bot (`crate::telegram`). Replies go out with the same bot.
//! - Google Chat: `agent.toml [channels.googlechat] project_number` +
//!   `service_account_json_enc`; Google posts the app's events to
//!   `/webhook/googlechat/agents/<id>` and each request is verified against
//!   THAT app's project number (`crate::googlechat_agents`).
//!
//! An employee without its own bot or app keeps the shared one (unchanged).
//! A bot or app belongs to one employee only (`conflicts`). Credentials are
//! stored through `config_crypto` (encrypted), never as plaintext in the
//! employee's directory (`store`).

pub mod conflicts;
#[cfg(debug_assertions)]
pub mod dev_override;
pub mod gchat_spaces;
pub mod googlechat_config;
pub mod identity;
pub mod status;
pub mod store;
pub mod telegram_probe;

#[cfg(test)]
pub(crate) mod e2e_gchat_tests;
#[cfg(test)]
mod e2e_reply_tests;
#[cfg(test)]
pub(crate) mod e2e_seam;
#[cfg(test)]
pub(crate) mod e2e_tests;
#[cfg(test)]
mod tests;

/// Serialises every bind of an employee's own bot / app in this process, so
/// the duplicate check and the write it guards cannot interleave with another
/// bind (B11 L3).
pub async fn binding_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// Status-map label of an employee's own bot on `channel` (`telegram:<id>`).
pub fn label(channel: &str, agent_id: &str) -> String {
    format!("{channel}:{agent_id}")
}

/// Path Google posts an employee's own Chat app events to.
pub fn googlechat_endpoint_path(agent_id: &str) -> String {
    format!("/webhook/googlechat/agents/{agent_id}")
}
