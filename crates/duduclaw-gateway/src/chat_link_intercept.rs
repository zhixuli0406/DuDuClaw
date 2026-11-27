//! RM-143 control-plane ingress: consumed codes never enter model admission,
//! conversation history, memory, or the bus. Adapter identity is trusted only
//! after the adapter's own transport and sender checks.

use std::path::Path;

use duduclaw_auth::{UserDb, chat_link::{self, MatchOutcome}};
use crate::trigger_route::texts::Lang;

/// The configured sending bot/app, not its credential or human principal.
/// In particular, an employee named `shared` differs from the shared app.
pub(crate) fn account_label(channel: &str, agent: Option<&str>) -> String {
    agent.map(|id| crate::agent_channels::label(channel, id))
        .unwrap_or_else(|| channel.to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeRoute {
    Conversation,
    Consume,
    Match,
}

/// Pure admission decision, also used by transports that must never match.
pub(crate) fn route(text: &str, may_match: bool) -> CodeRoute {
    if chat_link::parse_code(text).is_none() {
        CodeRoute::Conversation
    } else if may_match {
        CodeRoute::Match
    } else {
        CodeRoute::Consume
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterceptReply {
    Matched,
    InvalidOrExpired,
    MustUsePrivateChat,
    Silent,
    Unavailable,
}

impl InterceptReply {
    pub(crate) fn text(self, lang: Lang) -> Option<&'static str> {
        Some(match (self, lang) {
            (Self::Silent, _) => return None,
            (Self::Matched, Lang::ZhTw) => "已收到，請回到儀表板確認。",
            (Self::Matched, Lang::En) => "Received. Please return to the dashboard to confirm.",
            (Self::Matched, Lang::Ja) => "受け取りました。ダッシュボードに戻って確認してください。",
            (Self::InvalidOrExpired, Lang::ZhTw) => "代碼無效或已過期。",
            (Self::InvalidOrExpired, Lang::En) => "The code is invalid or has expired.",
            (Self::InvalidOrExpired, Lang::Ja) => "コードが無効か、有効期限が切れています。",
            (Self::MustUsePrivateChat, Lang::ZhTw) => "請在私訊貼上代碼。",
            (Self::MustUsePrivateChat, Lang::En) => "Please paste the code in a direct message.",
            (Self::MustUsePrivateChat, Lang::Ja) => "ダイレクトメッセージにコードを貼り付けてください。",
            (Self::Unavailable, Lang::ZhTw) => "目前無法處理，請稍後再試。",
            (Self::Unavailable, Lang::En) => "Unable to process this right now. Please try again later.",
            (Self::Unavailable, Lang::Ja) => "現在処理できません。しばらくしてからもう一度お試しください。",
        })
    }
}

/// Hide whole-message handshake codes before quoting or persisting context.
pub(crate) fn context_text(text: &str) -> &str {
    if chat_link::parse_code(text).is_some() { "[連結代碼已隱藏]" } else { text }
}

/// Blocking core: None alone permits normal conversation processing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle(
    db: &UserDb, channel: &str, account: &str, principal: &str, dm: &str,
    display_name: &str, is_private: bool, text: &str,
) -> Option<InterceptReply> {
    let code = chat_link::parse_code(text)?;
    let reply = match chat_link::match_code(
        db, channel, &code, principal, account, dm,
        &duduclaw_core::truncate_chars(&duduclaw_core::plain_line(display_name, 200), 200), is_private,
    ) {
        Ok(MatchOutcome::Matched) => InterceptReply::Matched,
        Ok(MatchOutcome::MustUsePrivateChat) => InterceptReply::MustUsePrivateChat,
        // Group pastes get the same private-chat guidance even for unknown
        // codes; matching and throttling still happen in the auth store.
        Ok(MatchOutcome::InvalidOrExpired) if !is_private => InterceptReply::MustUsePrivateChat,
        Ok(MatchOutcome::InvalidOrExpired) => InterceptReply::InvalidOrExpired,
        Ok(MatchOutcome::Throttled) => InterceptReply::Silent,
        Err(_) => InterceptReply::Unavailable,
    };
    tracing::debug!(channel, outcome = ?reply, "Chat link intercepted");
    Some(reply)
}

/// Open and use SQLite entirely on a blocking worker. Missing/corrupt stores
/// and worker failures consume the code; no lock guard crosses this await.
/// The outer Option means consumed; the inner Option is an optional reply.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_at_home(
    home: &Path, channel: &'static str, account: String, principal: String,
    dm: String, display_name: String, is_private: bool, text: &str,
) -> Option<Option<&'static str>> {
    chat_link::parse_code(text)?;
    let home = home.to_path_buf();
    let text = text.to_owned();
    let result = tokio::task::spawn_blocking(move || {
        let lang = crate::trigger_route::texts::lang_for(&home);
        let reply = match crate::decision_notify::open_user_db(&home) {
            Ok(Some(db)) => handle(&db, channel, &account, &principal, &dm,
                &display_name, is_private, &text).unwrap_or(InterceptReply::Unavailable),
            Ok(None) | Err(_) => InterceptReply::Unavailable,
        };
        reply.text(lang)
    }).await;
    Some(match result {
        Ok(reply) => reply,
        Err(_) => {
            tracing::warn!(channel, outcome = "unavailable", "Chat link worker failed");
            InterceptReply::Unavailable.text(Lang::ZhTw)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::{UserRole, chat_link::HandshakeStatus};

    fn fixture() -> (tempfile::TempDir, UserDb, String) {
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("admin@example.test", "Admin", "password-test", UserRole::Admin).unwrap();
        (home, db, user.id)
    }

    fn receive(db: &UserDb, code: &str, private: bool) -> Option<InterceptReply> {
        handle(db, "telegram", &account_label("telegram", None), "123", "123", "王小明", private, code)
    }

    #[test]
    fn matched_display_is_normalized_before_storage() {
        let (_home, db, user) = fixture();
        let started = chat_link::start(&db, &user, "telegram", "session").unwrap();
        let name = format!("王\n\r\t\u{1b}\u{202E}\u{200B}{}", "明".repeat(250));
        assert_eq!(handle(&db, "telegram", "telegram", "123", "123", &name, true,
            &started.code), Some(InterceptReply::Matched));
        let state = chat_link::status(&db, &user, &started.handshake_id).unwrap().unwrap();
        assert_eq!(state.matched_display.unwrap(), format!("王{}", "明".repeat(199)));
    }

    #[test]
    fn canonical_accounts_distinguish_shared_from_employee_named_shared() {
        for channel in ["telegram", "googlechat"] {
            assert_eq!(account_label(channel, None), channel);
            assert_eq!(account_label(channel, Some("shared")), crate::agent_channels::label(channel, "shared"));
            assert_ne!(account_label(channel, None), account_label(channel, Some("shared")));
        }
    }

    #[test]
    fn non_code_continues_and_private_match_creates_no_binding() {
        let (_home, db, user) = fixture();
        assert_eq!(receive(&db, "hello", true), None);
        for channel in ["telegram", "googlechat"] {
            let started = chat_link::start(&db, &user, channel, "session").unwrap();
            let account = account_label(channel, Some("assistant"));
            assert_eq!(handle(&db, channel, &account, "123", "123", "王".repeat(250).as_str(), true,
                &started.code), Some(InterceptReply::Matched));
            let status = chat_link::status(&db, &user, &started.handshake_id).unwrap().unwrap();
            assert_eq!(status.status, HandshakeStatus::Matched);
            assert_eq!(status.matched_account.as_deref(), Some(account.as_str()));
            assert_eq!(status.matched_display.unwrap().chars().count(), 200);
            assert!(db.channel_identities_for_user(&user).unwrap().is_empty());
        }
        assert_eq!(InterceptReply::Matched.text(Lang::ZhTw), Some("已收到，請回到儀表板確認。"));
    }

    #[test]
    fn group_paste_voids_code_and_replies_private_chat_text() {
        let (_home, db, user) = fixture();
        let started = chat_link::start(&db, &user, "telegram", "session").unwrap();
        let reply = receive(&db, &started.code, false).unwrap();
        assert_eq!(reply.text(Lang::ZhTw), Some("請在私訊貼上代碼。"));
        assert_eq!(chat_link::status(&db, &user, &started.handshake_id).unwrap().unwrap().status,
            HandshakeStatus::Rejected);
        assert_eq!(receive(&db, &started.code, true), Some(InterceptReply::InvalidOrExpired));
    }

    #[test]
    fn throttled_is_consumed_silently_even_for_group_pastes() {
        let (_home, db, user) = fixture();
        // With no handshakes this syntactically valid code cannot match.
        for _ in 0..5 {
            assert_eq!(receive(&db, "DUDU-0000-0000", true), Some(InterceptReply::InvalidOrExpired));
        }
        let started = chat_link::start(&db, &user, "telegram", "session").unwrap();
        for private in [true, false] {
            let reply = receive(&db, &started.code, private).unwrap();
            assert_eq!(reply, InterceptReply::Silent);
            assert_eq!(reply.text(Lang::ZhTw), None);
        }
        assert_eq!(chat_link::status(&db, &user, &started.handshake_id).unwrap().unwrap().status,
            HandshakeStatus::Waiting);
    }

    #[test]
    fn database_write_failure_consumes_code() {
        let (home, _db, _user) = fixture();
        let db = UserDb::open_read_only(&home.path().join("users.db")).unwrap();
        assert_eq!(receive(&db, "DUDU-0000-0000", true), Some(InterceptReply::Unavailable));
    }

    #[tokio::test]
    async fn absent_or_corrupt_store_never_falls_through() {
        let home = tempfile::tempdir().unwrap();
        for corrupt in [false, true] {
            if corrupt { std::fs::write(home.path().join("users.db"), b"not a database").unwrap(); }
            assert_eq!(handle_at_home(home.path(), "telegram", "telegram".into(), "123".into(),
                "123".into(), "Name".into(), true, "DUDU-0000-0000").await,
                Some(Some("目前無法處理，請稍後再試。")));
            if !corrupt { assert!(!home.path().join("users.db").exists()); }
        }
    }
}

pub(crate) const NO_CONFIGURED_ACCOUNTS: &str = "目前沒有可連結的機器人或聊天應用程式，請先完成設定；Google Chat 需使用 HTTP 連線。";

const CONFIG_UNAVAILABLE: &str = "目前無法處理聊天連結，請重新整理後再試。";

fn configured_string(table: &toml::Table, key: &str) -> bool {
    table.get(key).and_then(toml::Value::as_str).is_some_and(|s| !s.trim().is_empty())
}

fn configured_secret(table: &toml::Table, key: &str) -> bool {
    configured_string(table, key) || configured_string(table, &format!("{key}_enc"))
}

#[derive(Debug, Clone)]
pub(crate) struct ConfiguredAccount {
    pub(crate) channel: String,
    pub(crate) account: String,
    pub(crate) agent_id: Option<String>,
    pub(crate) display_name: String,
    pub(crate) project_number: Option<String>,
}

fn target(channel: &str, agent: Option<&str>, name: &str, project: Option<&str>) -> ConfiguredAccount {
    ConfiguredAccount { channel: channel.into(), account: account_label(channel, agent),
        agent_id: agent.map(str::to_owned), display_name: name.into(),
        project_number: project.map(str::to_owned) }
}

/// Local configuration only: never resolve SecretRefs or call getMe. Like the
/// existing channel status RPC, encrypted credentials count as configured.
pub(crate) fn configured_accounts(home: &Path, channel: &str, agents: &[String]) -> Result<Vec<ConfiguredAccount>, &'static str> {
    if !chat_link::CHANNELS.contains(&channel) {
        return Err(CONFIG_UNAVAILABLE);
    }
    let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| CONFIG_UNAVAILABLE)?;
    let config: toml::Table = raw.parse().map_err(|_| CONFIG_UNAVAILABLE)?;
    let mut result = Vec::new();
    if let Some(value) = config.get("channels") {
        let ch = value.as_table().ok_or(CONFIG_UNAVAILABLE)?;
        if channel == "telegram" && configured_secret(ch, "telegram_bot_token") {
            result.push(target(channel, None, "共用 Telegram 機器人", None));
        } else if channel == "googlechat" {
            let allowed = ["project_number", "service_account_json", "service_account_json_enc",
                "connection", "pubsub_subscription"];
            let valid_fields = ch.keys().filter_map(|k| k.strip_prefix("googlechat_"))
                .all(|k| allowed.contains(&k));
            // Same fail-closed transport rules as shared_http_enabled and
            // sending_app_takes_http_clicks: default HTTP, empty subscription.
            let http = ch.get("googlechat_connection").map_or(true, |v| v.as_str() == Some("http"))
                && ch.get("googlechat_pubsub_subscription").map_or(true, |v| v.as_str() == Some(""));
            if valid_fields && http && configured_secret(ch, "googlechat_service_account_json") {
                if let Some(project) = ch.get("googlechat_project_number").and_then(toml::Value::as_str)
                    .filter(|p| crate::agent_channels::googlechat_config::validate_project_number(p).is_ok()) {
                    result.push(target(channel, None, "共用 Google Chat 應用程式", Some(project)));
                }
            }
        }
    }
    for id in agents {
        if !duduclaw_core::is_valid_agent_id(id) { return Err(CONFIG_UNAVAILABLE); }
        let raw = std::fs::read_to_string(home.join("agents").join(id).join("agent.toml"))
            .map_err(|_| CONFIG_UNAVAILABLE)?;
        let config: toml::Table = raw.parse().map_err(|_| CONFIG_UNAVAILABLE)?;
        let Some(channels) = config.get("channels") else { continue; };
        let Some(value) = channels.as_table().ok_or(CONFIG_UNAVAILABLE)?.get(channel) else { continue; };
        let section = value.as_table().ok_or(CONFIG_UNAVAILABLE)?;
        if channel == "telegram" && configured_secret(section, "bot_token") {
            result.push(target(channel, Some(id), id, None));
        } else if channel == "googlechat" {
            let cfg: duduclaw_core::types::GoogleChatChannelConfig = value.clone().try_into()
                .map_err(|_| CONFIG_UNAVAILABLE)?;
            if cfg.connection == "http" && cfg.pubsub_subscription.trim().is_empty()
                && crate::agent_channels::store::googlechat_configured(Some(&cfg))
                && crate::agent_channels::googlechat_config::validate_project_number(&cfg.project_number).is_ok() {
                result.push(target(channel, Some(id), id, Some(&cfg.project_number)));
            }
        }
    }
    if result.is_empty() {
        return Err(NO_CONFIGURED_ACCOUNTS);
    }
    Ok(result)
}

