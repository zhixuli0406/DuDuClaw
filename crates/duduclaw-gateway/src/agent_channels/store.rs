//! Where per-employee channel credentials live: `agent.toml [channels.*]`,
//! encrypted through `config_crypto` (the same keyfile scheme as every other
//! channel secret, behind `SecretRef` so it can move to another backend).
//!
//! Unlike the older per-agent token writers, these never fall back to a
//! plaintext copy: `agent.toml` sits in the employee's own directory, so a
//! save that cannot encrypt is refused instead.

use std::path::Path;

use duduclaw_security::secret_manager::SecretManagerConfig;
use duduclaw_security::secret_ref::{Secret, SecretRef};

/// Refusal when the keyfile cannot encrypt (zh-TW, user-facing).
pub const ENCRYPTION_UNAVAILABLE: &str = "無法加密儲存這組憑證（本機加密金鑰無法讀寫），為了不把憑證以明文存進 AI 員工的資料夾，這次沒有儲存。請檢查 DuDuClaw 資料夾的寫入權限後再試。";

/// Encrypt `value` or refuse (never plaintext).
pub fn encrypt_or_refuse(value: &str, home: &Path) -> Result<String, String> {
    crate::config_crypto::encrypt_value(value, home)
        .ok_or_else(|| ENCRYPTION_UNAVAILABLE.to_string())
}

fn channel_section<'a>(
    table: &'a mut toml::Table,
    channel: &str,
) -> Result<&'a mut toml::Table, String> {
    table
        .entry("channels")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("agent.toml 的 [channels] 格式不正確")?
        .entry(channel)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("agent.toml 的 [channels.{channel}] 格式不正確"))
}

/// Write the employee's own Telegram bot (ciphertext only).
pub fn apply_telegram(table: &mut toml::Table, token_enc: &str) -> Result<(), String> {
    let section = channel_section(table, "telegram")?;
    section.remove("bot_token");
    section.insert(
        "bot_token_enc".into(),
        toml::Value::String(token_enc.to_string()),
    );
    Ok(())
}

/// Write the employee's own Google Chat app. `sa_enc = None` keeps the stored
/// service-account key (project number change only).
pub fn apply_googlechat(
    table: &mut toml::Table,
    project_number: &str,
    sa_enc: Option<&str>,
) -> Result<(), String> {
    let section = channel_section(table, "googlechat")?;
    section.insert(
        "project_number".into(),
        toml::Value::String(project_number.to_string()),
    );
    if let Some(enc) = sa_enc {
        section.remove("service_account_json");
        section.insert(
            "service_account_json_enc".into(),
            toml::Value::String(enc.to_string()),
        );
    }
    Ok(())
}

/// Remove `[channels.<channel>]`. `true` when something was removed.
pub fn remove_channel(table: &mut toml::Table, channel: &str) -> bool {
    table
        .get_mut("channels")
        .and_then(|v| v.as_table_mut())
        .is_some_and(|channels| channels.remove(channel).is_some())
}

/// An employee's own Google Chat app as configured on disk right now.
pub struct AgentChatApp {
    pub project_number: String,
    pub service_account: Secret,
}

fn agent_toml(home: &Path, agent_id: &str) -> Option<toml::Table> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return None;
    }
    let path = home.join("agents").join(agent_id).join("agent.toml");
    std::fs::read_to_string(path).ok()?.parse().ok()
}

fn googlechat_section(home: &Path, agent_id: &str) -> Option<toml::Table> {
    agent_toml(home, agent_id)?
        .get("channels")?
        .as_table()?
        .get("googlechat")?
        .as_table()
        .cloned()
}

/// Read the employee's own Chat app fresh from `agent.toml` (per request,
/// never cached). `None` when not (completely) configured or the key cannot
/// be resolved — callers fail closed.
pub async fn agent_chat_app(home: &Path, agent_id: &str) -> Option<AgentChatApp> {
    let section = googlechat_section(home, agent_id)?;
    let project_number = section
        .get("project_number")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|p| !p.is_empty())?
        .to_string();
    let reference = SecretRef::from_typed(
        section
            .get("service_account_json_enc")
            .and_then(|v| v.as_str()),
        section
            .get("service_account_json")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    );
    let sm_cfg = SecretManagerConfig::load_from_home(home).await;
    let service_account = reference.resolve(&sm_cfg, home).await?;
    Some(AgentChatApp {
        project_number,
        service_account,
    })
}

/// Whether the employee has a Google Chat app section with a project number
/// (cheap presence check from the registry config; no decryption).
pub fn googlechat_configured(cfg: Option<&duduclaw_core::types::GoogleChatChannelConfig>) -> bool {
    cfg.is_some_and(|g| {
        !g.project_number.trim().is_empty()
            && (g
                .service_account_json_enc
                .as_ref()
                .is_some_and(|e| !e.is_empty())
                || !g.service_account_json.is_empty())
    })
}

/// Whether the employee has its own Telegram bot configured (no decryption).
pub fn telegram_configured(cfg: Option<&duduclaw_core::types::TelegramChannelConfig>) -> bool {
    cfg.is_some_and(|t| {
        !t.bot_token.is_empty() || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())
    })
}
