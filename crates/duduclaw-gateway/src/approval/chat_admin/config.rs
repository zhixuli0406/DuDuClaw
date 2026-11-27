use std::{path::Path, sync::Once};

static UNSUPPORTED_CHANNEL_WARNING: Once = Once::new();

use chrono::{DateTime, FixedOffset, Utc};
use serde::Deserialize;

use super::tier::step_up_selectable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChatChannel {
    Telegram,
    Googlechat,
    Line,
    Slack,
    Discord,
}

impl ChatChannel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Googlechat => "googlechat",
            Self::Line => "line",
            Self::Slack => "slack",
            Self::Discord => "discord",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepMethod {
    Web,
    DmTotp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnabledAck {
    pub(crate) by: uuid::Uuid,
    pub(crate) at: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatAdminConfig {
    pub(crate) enabled: bool,
    pub(crate) channels: Vec<ChatChannel>,
    /// Empty means every T2 kind requires step-up. Changes must go through
    /// the settings RPC with an audit record. T1 never requires step-up.
    pub(crate) step_up_exempt_kinds: Vec<String>,
    pub(crate) step_up_methods: Vec<StepMethod>,
    pub(crate) card_valid_hours: u16,
    pub(crate) enabled_ack: Option<EnabledAck>,
}

impl Default for ChatAdminConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channels: vec![ChatChannel::Telegram, ChatChannel::Googlechat],
            step_up_exempt_kinds: Vec::new(),
            step_up_methods: vec![StepMethod::Web],
            card_valid_hours: 168,
            enabled_ack: None,
        }
    }
}

/// Static diagnostics deliberately exclude untrusted configuration contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ConfigError(&'static str);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAck {
    by: String,
    at: String,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    enabled: bool,
    channels: Vec<ChatChannel>,
    step_up_exempt_kinds: Vec<String>,
    step_up_methods: Vec<StepMethod>,
    card_valid_hours: i64,
    enabled_ack: Option<RawAck>,
}

impl Default for RawConfig {
    fn default() -> Self {
        let cfg = ChatAdminConfig::default();
        Self {
            enabled: cfg.enabled,
            channels: cfg.channels,
            step_up_exempt_kinds: cfg.step_up_exempt_kinds,
            step_up_methods: cfg.step_up_methods,
            card_valid_hours: i64::from(cfg.card_valid_hours),
            enabled_ack: None,
        }
    }
}

/// Parse the chat_admin subtable itself, never the enclosing approval table.
/// Sibling approval settings therefore do not enter this strict schema.
pub(crate) fn parse(table: Option<&toml::Value>) -> Result<ChatAdminConfig, ConfigError> {
    let Some(table) = table else { return Ok(ChatAdminConfig::default()); };
    let mut raw: RawConfig = table.clone().try_into()
        .map_err(|_| ConfigError("invalid approval.chat_admin keys, types or values"))?;
    raw.channels.retain(|channel| {
        if matches!(channel, ChatChannel::Telegram | ChatChannel::Googlechat) { return true; }
        UNSUPPORTED_CHANNEL_WARNING.call_once(|| {
            tracing::warn!("chat-admin ignores LINE, Slack and Discord; only Telegram and Google Chat are supported");
        });
        false
    });
    if raw.channels.is_empty() { raw.enabled = false; }
    for (index, channel) in raw.channels.iter().enumerate() {
        if raw.channels.iter().take(index).any(|previous| previous == channel) {
            return Err(ConfigError("duplicate chat_admin channel"));
        }
    }
    for (index, key) in raw.step_up_exempt_kinds.iter().enumerate() {
        if !step_up_selectable(key) {
            return Err(ConfigError("step_up_exempt_kinds must contain only T2 keys"));
        }
        if raw.step_up_exempt_kinds.iter().take(index).any(|previous| previous == key) {
            return Err(ConfigError("duplicate step_up_exempt_kinds entry"));
        }
    }
    if raw.step_up_methods.is_empty() {
        return Err(ConfigError("step_up_methods must not be empty"));
    }
    if !(1..=720).contains(&raw.card_valid_hours) {
        return Err(ConfigError("card_valid_hours must be between 1 and 720"));
    }
    let enabled_ack = match raw.enabled_ack {
        Some(ack) => Some(EnabledAck {
            by: uuid::Uuid::parse_str(&ack.by)
                .map_err(|_| ConfigError("enabled_ack.by must be a UUID"))?,
            at: DateTime::parse_from_rfc3339(&ack.at)
                .map_err(|_| ConfigError("enabled_ack.at must be RFC3339"))?,
        }),
        None => None,
    };
    Ok(ChatAdminConfig {
        enabled: raw.enabled,
        channels: raw.channels,
        step_up_exempt_kinds: raw.step_up_exempt_kinds,
        step_up_methods: raw.step_up_methods,
        card_valid_hours: raw.card_valid_hours as u16,
        enabled_ack,
    })
}

fn read_config(home: &Path) -> Result<ChatAdminConfig, ConfigError> {
    let raw = std::fs::read_to_string(home.join("config.toml"))
        .map_err(|_| ConfigError("cannot read config.toml"))?;
    let root: toml::Table = raw.parse()
        .map_err(|_| ConfigError("cannot parse config.toml"))?;
    let table = match root.get("approval") {
        None => None,
        Some(approval) => approval.as_table()
            .ok_or(ConfigError("approval must be a table"))?.get("chat_admin"),
    };
    parse(table)
}

/// Blocking filesystem read. Any failure returns the disabled default.
pub(crate) fn load(home: &Path) -> ChatAdminConfig {
    match read_config(home) {
        Ok(cfg) => cfg,
        Err(error) => {
            tracing::warn!(%error, "chat-admin approvals disabled: configuration unavailable or invalid");
            ChatAdminConfig::default()
        }
    }
}

/// Blocking filesystem and users.db reads: async callers must use spawn_blocking.
/// Recheck the acknowledging user's live role/status; never infer solo authority.
pub(crate) fn effective(home: &Path, users: Option<&duduclaw_auth::UserDb>) -> Option<ChatAdminConfig> {
    let cfg = load(home);
    if !cfg.enabled { return None; }
    let ack = cfg.enabled_ack.as_ref()?;
    let context = match users?.live_user_context(&ack.by.to_string()) {
        Ok(context) => context?,
        Err(_) => {
            tracing::warn!("chat-admin approvals disabled: cannot read acknowledging user");
            return None;
        }
    };
    if context.role != duduclaw_auth::UserRole::Admin { return None; }
    Some(cfg)
}

/// Supply the approval's own expiry (ApprovalRecord::expires_at) and the
/// send-time clock. Never extend that expiry; overflow/invalid config denies.
pub(crate) fn card_valid_until(
    cfg: &ChatAdminConfig,
    now: DateTime<Utc>,
    approval_expiry: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !(1..=720).contains(&cfg.card_valid_hours) { return None; }
    let deadline = now.checked_add_signed(chrono::Duration::hours(i64::from(cfg.card_valid_hours)))?;
    Some(deadline.min(approval_expiry))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(text: &str) -> Result<ChatAdminConfig, ConfigError> {
        let value: toml::Value = text.parse().unwrap();
        parse(Some(&value))
    }

    #[test]
    fn missing_subtable_is_disabled() {
        let cfg = parse(None).unwrap();
        assert!(!cfg.enabled);
        assert_eq!(cfg.channels.iter().map(|channel| channel.as_str()).collect::<Vec<_>>(),
            vec!["telegram", "googlechat"]);
        assert!(cfg.step_up_exempt_kinds.is_empty());
        assert_eq!(cfg.step_up_methods, vec![StepMethod::Web]);
        assert_eq!(cfg.card_valid_hours, 168);
        assert!(cfg.enabled_ack.is_none());
        let home = tempfile::tempdir().unwrap();
        assert_eq!(load(home.path()), cfg);
    }

    #[test]
    fn invalid_settings_are_rejected() {
        for text in [
            "unknown = true", "enabled = 'true'", "channels = ['unknown']",
            "channels = 'telegram'", "channels = ['Telegram']", "channels = [1]",
            "channels = ['telegram', 'telegram']",
            "step_up_exempt_kinds = ['cli_egress', 'cli_egress']",
            "step_up_exempt_kinds = ['knowledge_quarantine']",
            "step_up_exempt_kinds = ['knowledge_add_source']", "step_up_exempt_kinds = ['unknown']",
            "step_up_exempt_kinds = 'cli_egress'", "step_up_methods = []",
            "step_up_methods = ['unknown']", "step_up_methods = 'web'",
            "card_valid_hours = 0", "card_valid_hours = 721",
            "card_valid_hours = 1.5", "card_valid_hours = '168'",
            "enabled_ack = { by = 'invalid', at = '2026-11-27T00:00:00Z' }",
            "enabled_ack = { by = '00000000-0000-0000-0000-000000000001', at = 'invalid' }",
            "enabled_ack = { by = '00000000-0000-0000-0000-000000000001' }",
            "enabled_ack = { by = '00000000-0000-0000-0000-000000000001', at = '2026-11-27T00:00:00Z', extra = true }",
        ] {
            assert!(parse_text(text).is_err(), "{text}");
        }
        assert!(parse(Some(&toml::Value::Boolean(true))).is_err());
    }

    #[test]
    fn valid_settings_and_boundaries() {
        for hours in [1, 720] {
            let text = format!("enabled = true\nchannels = ['telegram', 'googlechat', 'line', 'slack', 'discord']\nstep_up_methods = ['web', 'dm_totp']\ncard_valid_hours = {hours}\nstep_up_exempt_kinds = [{}]",
                super::super::STEP_UP_KIND_KEYS.iter().map(|key| format!("\"{key}\""))
                    .collect::<Vec<_>>().join(","));
            let cfg = parse_text(&text).unwrap();
            assert!(cfg.enabled);
            assert_eq!(cfg.card_valid_hours, hours);
            assert_eq!(cfg.channels, vec![ChatChannel::Telegram, ChatChannel::Googlechat]);
            assert_eq!(cfg.step_up_methods, vec![StepMethod::Web, StepMethod::DmTotp]);
        }
        assert!(parse_text("channels = []").is_ok());
    }

    #[test]
    fn unsupported_channels_are_ignored_and_empty_selection_disables() {
        for channels in ["[]", "['line']", "['slack']", "['discord']", "['line','slack','discord']"] {
            let cfg = parse_text(&format!("enabled = true\nchannels = {channels}")).unwrap();
            assert!(!cfg.enabled);
            assert!(cfg.channels.is_empty());
        }
        let cfg = parse_text("enabled = true\nchannels = ['line','googlechat','slack','telegram','discord']").unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.channels, vec![ChatChannel::Googlechat, ChatChannel::Telegram]);
        assert!(parse_text("channels = ['line', 'unknown']").is_err());
    }

    #[test]
    fn old_opt_in_key_is_unknown_and_disables_feature() {
        let home = tempfile::tempdir().unwrap();
        for value in ["[]", "['cli_egress']"] {
            let text = format!("enabled = true\nstep_up_kinds = {value}");
            assert!(parse_text(&text).is_err());
            std::fs::write(home.path().join("config.toml"), format!("[approval.chat_admin]\n{text}")).unwrap();
            assert_eq!(load(home.path()), ChatAdminConfig::default());
        }
    }

    #[test]
    fn load_fails_closed_and_ignores_approval_siblings() {
        let home = tempfile::tempdir().unwrap();
        for text in ["garbage [", "approval = false", "[approval.chat_admin]\nenabled = true\nunknown = 1"] {
            std::fs::write(home.path().join("config.toml"), text).unwrap();
            assert_eq!(load(home.path()), ChatAdminConfig::default());
        }
        std::fs::write(home.path().join("config.toml"),
            "[approval]\nskill_ttl_days = 7\nother_feature = true\n[approval.chat_admin]\nenabled = true").unwrap();
        assert!(load(home.path()).enabled);
        std::fs::write(home.path().join("config.toml"), "[approval]\nskill_ttl_days = 7").unwrap();
        assert_eq!(load(home.path()), ChatAdminConfig::default());
    }

    #[test]
    fn missing_ack_means_off() {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        std::fs::write(home.path().join("config.toml"), "[approval.chat_admin]\nenabled = true").unwrap();
        assert!(effective(home.path(), Some(&db)).is_none());
    }

    #[test]
    fn ack_requires_live_active_admin() {
        use duduclaw_auth::{UserDb, UserRole, UserStatus};
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("ack@example.com", "Ack", "test-password", UserRole::Admin).unwrap();
        let write_ack = |by: &str| {
            std::fs::write(home.path().join("config.toml"), format!(
                "[approval.chat_admin]\nenabled = true\nenabled_ack = {{ by = '{by}', at = '2026-11-27T00:00:00Z' }}"
            )).unwrap();
        };
        write_ack(&user.id);
        assert!(effective(home.path(), Some(&db)).is_some());
        assert!(effective(home.path(), None).is_none());
        db.set_user_status(&user.id, UserStatus::Suspended).unwrap();
        assert!(effective(home.path(), Some(&db)).is_none());
        db.set_user_status(&user.id, UserStatus::Offboarded).unwrap();
        assert!(effective(home.path(), Some(&db)).is_none());
        let manager = db.create_user("manager@example.com", "Manager", "test-password", UserRole::Manager).unwrap();
        write_ack(&manager.id);
        assert!(effective(home.path(), Some(&db)).is_none());
        write_ack(&uuid::Uuid::new_v4().to_string());
        assert!(effective(home.path(), Some(&db)).is_none());
        write_ack("not-a-uuid");
        assert!(effective(home.path(), Some(&db)).is_none());
    }

    #[test]
    fn card_deadline_never_extends_approval_expiry() {
        let cfg = ChatAdminConfig::default();
        let now = DateTime::parse_from_rfc3339("2026-11-27T00:00:00Z").unwrap().with_timezone(&Utc);
        for hours in [-1, 1, 168, 720] {
            let expiry = now + chrono::Duration::hours(hours);
            assert_eq!(card_valid_until(&cfg, now, expiry),
                Some((now + chrono::Duration::hours(168)).min(expiry)));
        }
        assert!(card_valid_until(&cfg, DateTime::<Utc>::MAX_UTC, DateTime::<Utc>::MAX_UTC).is_none());
        let invalid = ChatAdminConfig { card_valid_hours: 0, ..cfg };
        assert!(card_valid_until(&invalid, now, now).is_none());
    }
}
