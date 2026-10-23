//! One bot / one Chat app belongs to one employee.
//!
//! Telegram's `getUpdates` is exclusive per bot: two employees configured with
//! the same bot used to mean the second one was silently skipped at start
//! ("first claim wins") and its page still looked configured. Saves now refuse
//! a bot or Chat app another employee already uses, and the start path marks
//! a leftover duplicate with an explicit error.

use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_security::secret_manager::SecretManagerConfig;
use tokio::sync::RwLock;

/// Who already uses a bot / app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// The shared (global `config.toml`) bot or app.
    Shared,
    /// Another employee's own bot or app.
    Employee { id: String, display_name: String },
}

impl Holder {
    /// Refusal for a Telegram bot already in use (zh-TW, user-facing).
    pub fn telegram_refusal(&self) -> String {
        match self {
            Holder::Shared => {
                "這個 Bot 已經是共用 Bot，請到 BotFather 另建一個 Bot 給這位 AI 員工。".into()
            }
            Holder::Employee { display_name, .. } => format!(
                "這個 Bot 已經是「{display_name}」的專屬 Bot。一個 Bot 只能給一位 AI 員工使用，請到 BotFather 另建一個。"
            ),
        }
    }

    /// Refusal for a Google Chat app already in use (zh-TW, user-facing).
    pub fn googlechat_refusal(&self) -> String {
        match self {
            Holder::Shared => "這個 Google Chat 應用程式（同一個專案編號）已經是共用的 Google Chat 設定。每位 AI 員工要用自己的 Google Cloud 專案建立 Chat 應用程式。".into(),
            Holder::Employee { display_name, .. } => format!(
                "這個 Google Chat 應用程式（同一個專案編號）已經是「{display_name}」的專屬應用程式。每位 AI 員工要用自己的 Google Cloud 專案建立 Chat 應用程式。"
            ),
        }
    }
}

/// The bot id (the part before `:`) of a Telegram token. Two tokens with the
/// same id are the same bot (a regenerated token keeps the id).
pub fn telegram_bot_id(token: &str) -> Option<&str> {
    let (id, _) = token.trim().split_once(':')?;
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then_some(id)
}

struct AgentChannelRow {
    id: String,
    display_name: String,
    telegram: Option<duduclaw_core::types::TelegramChannelConfig>,
    googlechat: Option<duduclaw_core::types::GoogleChatChannelConfig>,
}

async fn agent_rows(registry: &Arc<RwLock<AgentRegistry>>) -> Vec<AgentChannelRow> {
    let reg = registry.read().await;
    let mut rows: Vec<AgentChannelRow> = reg
        .list()
        .into_iter()
        .map(|a| AgentChannelRow {
            id: a.config.agent.name.clone(),
            display_name: a.config.agent.display_name.clone(),
            telegram: a.config.channels.as_ref().and_then(|c| c.telegram.clone()),
            googlechat: a
                .config
                .channels
                .as_ref()
                .and_then(|c| c.googlechat.clone()),
        })
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// The employee (other than `except_agent`) whose own bot is the same bot as
/// `token`. The shared bot is not reported here: moving the shared bot to an
/// employee is allowed (the employee's poller then replaces the shared one).
pub async fn telegram_bot_holder(
    home: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    token: &str,
    except_agent: &str,
) -> Option<Holder> {
    let wanted = telegram_bot_id(token)?;
    let sm_cfg = SecretManagerConfig::load_from_home(home).await;
    for row in agent_rows(registry).await {
        if row.id == except_agent {
            continue;
        }
        let Some(tg) = &row.telegram else { continue };
        let Some(secret) = crate::config_crypto::resolve_agent_token(
            &tg.bot_token_enc,
            &tg.bot_token,
            home,
            &sm_cfg,
        )
        .await
        else {
            continue;
        };
        let other = crate::config_crypto::repair_telegram_token(secret.expose()).into_owned();
        if telegram_bot_id(&other) == Some(wanted) {
            return Some(Holder::Employee {
                id: row.id,
                display_name: row.display_name,
            });
        }
    }
    None
}

/// Whether the shared bot (`config.toml [channels] telegram_bot_token`) is
/// the same bot as `token`.
pub async fn shared_telegram_is(home: &Path, token: &str) -> bool {
    let Some(wanted) = telegram_bot_id(token) else {
        return false;
    };
    crate::config_crypto::read_encrypted_config_field(home, "channels", "telegram_bot_token")
        .await
        .map(|t| crate::config_crypto::repair_telegram_token(&t).into_owned())
        .is_some_and(|shared| telegram_bot_id(&shared) == Some(wanted))
}

/// Who (other than `except_agent`) already uses the Google Chat app of
/// `project_number`: the shared app or another employee's.
pub async fn googlechat_project_holder(
    home: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    project_number: &str,
    except_agent: &str,
) -> Option<Holder> {
    let wanted = project_number.trim();
    if wanted.is_empty() {
        return None;
    }
    let shared = crate::config_crypto::read_encrypted_config_field(
        home,
        "channels",
        "googlechat_project_number",
    )
    .await;
    if shared.as_deref().map(str::trim) == Some(wanted) {
        return Some(Holder::Shared);
    }
    agent_rows(registry).await.into_iter().find_map(|row| {
        let same = row.id != except_agent
            && row
                .googlechat
                .as_ref()
                .is_some_and(|g| g.project_number.trim() == wanted);
        same.then_some(Holder::Employee {
            id: row.id,
            display_name: row.display_name,
        })
    })
}
