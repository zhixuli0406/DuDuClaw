//! `config.toml [channel_ingress]`: the LINE durable-inbox settings.
//!
//! Read through a small (path, mtime, length) cache so idle workers do not
//! re-parse `config.toml` several times a second. An unreadable or invalid
//! file yields [`IngressConfig::unreadable`], whose `line_enabled` is `false`
//! (fail closed: nothing is dispatched while the setting cannot be read).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// What happens when the LINE reply token is past its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LateReply {
    /// Send the answer with the Push API to the same conversation, after the
    /// same revalidation a reply gets (default; the pre-inbox behaviour).
    Push,
    /// Do not push. An event already late when a worker picks it up is not
    /// run at all; it ends as `failed_before_dispatch` / `late_reply_expired`.
    Fail,
}

impl LateReply {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Fail => "fail",
        }
    }
}

/// Default number of ordinary (non-decision) LINE workers. One conversation
/// is still processed in order; this is how many conversations run at once.
pub(crate) const DEFAULT_LINE_WORKERS: usize = 8;
const MAX_LINE_WORKERS: usize = 64;
pub(crate) const DEFAULT_RETENTION_DAYS: i64 = 90;
pub(crate) const DEFAULT_STUCK_ALERT_MINUTES: i64 = 15;
pub(crate) const DEFAULT_CAPACITY_ALERT_MB: u64 = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IngressConfig {
    /// `line_enabled` (default true). The stop switch: false refuses new
    /// webhooks with 503 and pauses dispatch; the ledger stays.
    pub line_enabled: bool,
    /// `line_late_reply` (default `push`).
    pub late_reply: LateReply,
    /// `line_workers` (default 8, 1..=64). Read when the workers start.
    pub line_workers: usize,
    /// `retention_days` (default 90, minimum 1): finished events, their
    /// attempts and resolutions are deleted after this many days.
    pub retention_days: i64,
    /// `stuck_alert_minutes` (default 15, 0 = off): a conversation whose
    /// waiting messages are held back this long raises one alert.
    pub stuck_alert_minutes: i64,
    /// `capacity_alert_mb` (default 512, 0 = off): database + WAL size that
    /// raises one alert per day.
    pub capacity_alert_mb: u64,
}

impl Default for IngressConfig {
    fn default() -> Self {
        Self {
            line_enabled: true,
            late_reply: LateReply::Push,
            line_workers: DEFAULT_LINE_WORKERS,
            retention_days: DEFAULT_RETENTION_DAYS,
            stuck_alert_minutes: DEFAULT_STUCK_ALERT_MINUTES,
            capacity_alert_mb: DEFAULT_CAPACITY_ALERT_MB,
        }
    }
}

impl IngressConfig {
    /// The configuration used when `config.toml` cannot be read or parsed.
    pub(crate) fn unreadable() -> Self {
        Self {
            line_enabled: false,
            ..Self::default()
        }
    }

    /// Parse `[channel_ingress]` out of a whole `config.toml` table.
    pub(crate) fn from_table(table: &toml::Table) -> Self {
        let mut cfg = Self::default();
        let Some(section) = table.get("channel_ingress").and_then(|v| v.as_table()) else {
            return cfg;
        };
        // A present but non-boolean switch is not "on" (fail closed).
        if let Some(v) = section.get("line_enabled") {
            cfg.line_enabled = v.as_bool().unwrap_or(false);
        }
        let late = section.get("line_late_reply");
        if late.is_some_and(|v| !v.is_str()) {
            // Review L14c: a non-string value is reported, not silently "push".
            tracing::warn!(
                "[channel_ingress] line_late_reply must be the string \"push\" or \"fail\"; using \"push\""
            );
        }
        match late.and_then(|v| v.as_str()) {
            Some("fail") => cfg.late_reply = LateReply::Fail,
            Some("push") | None => {}
            Some(other) => {
                tracing::warn!(
                    value = %duduclaw_core::truncate_chars(other, 32),
                    "[channel_ingress] line_late_reply must be \"push\" or \"fail\"; using \"push\""
                );
            }
        }
        if let Some(n) = section.get("line_workers").and_then(|v| v.as_integer()) {
            cfg.line_workers = (n.max(1) as usize).min(MAX_LINE_WORKERS);
        }
        if let Some(n) = section.get("retention_days").and_then(|v| v.as_integer()) {
            cfg.retention_days = n.max(1);
        }
        if let Some(n) = section
            .get("stuck_alert_minutes")
            .and_then(|v| v.as_integer())
        {
            cfg.stuck_alert_minutes = n.max(0);
        }
        if let Some(n) = section
            .get("capacity_alert_mb")
            .and_then(|v| v.as_integer())
        {
            cfg.capacity_alert_mb = n.max(0) as u64;
        }
        cfg
    }

    /// Load (cached by file mtime and length) from `<home>/config.toml`.
    pub(crate) async fn load(home: &Path) -> Self {
        let path = home.join("config.toml");
        let Ok(meta) = tokio::fs::metadata(&path).await else {
            return Self::unreadable();
        };
        let stamp = (meta.modified().ok(), meta.len());
        if let Some(hit) = cache().lock().ok().and_then(|c| {
            c.iter()
                .find(|e| e.0 == path && e.1 == stamp)
                .map(|e| e.2.clone())
        }) {
            return hit;
        }
        let parsed = match tokio::fs::read_to_string(&path).await {
            Ok(text) => match text.parse::<toml::Table>() {
                Ok(table) => Self::from_table(&table),
                Err(_) => Self::unreadable(),
            },
            Err(_) => Self::unreadable(),
        };
        if let Ok(mut c) = cache().lock() {
            c.retain(|e| e.0 != path);
            if c.len() > 64 {
                c.clear();
            }
            c.push((path, stamp, parsed.clone()));
        }
        parsed
    }
}

type CacheEntry = (PathBuf, (Option<std::time::SystemTime>, u64), IngressConfig);

fn cache() -> &'static Mutex<Vec<CacheEntry>> {
    static CACHE: OnceLock<Mutex<Vec<CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> IngressConfig {
        IngressConfig::from_table(&text.parse().unwrap())
    }

    #[test]
    fn defaults_keep_the_shipped_push_behaviour() {
        let cfg = parse("");
        assert!(cfg.line_enabled);
        assert_eq!(cfg.late_reply, LateReply::Push);
        assert_eq!(cfg.line_workers, DEFAULT_LINE_WORKERS);
        assert_eq!(cfg.retention_days, 90);
    }

    #[test]
    fn explicit_values_are_clamped_and_bad_switch_fails_closed() {
        let cfg = parse(
            "[channel_ingress]\nline_enabled='yes'\nline_late_reply='fail'\nline_workers=0\nretention_days=-3\n",
        );
        assert!(!cfg.line_enabled);
        assert_eq!(cfg.late_reply, LateReply::Fail);
        assert_eq!(cfg.line_workers, 1);
        assert_eq!(cfg.retention_days, 1);
        assert_eq!(
            parse("[channel_ingress]\nline_workers=1000\nline_late_reply='nope'\n").line_workers,
            MAX_LINE_WORKERS
        );
        assert_eq!(
            parse("[channel_ingress]\nline_late_reply='nope'\n").late_reply,
            LateReply::Push
        );
    }

    #[tokio::test]
    async fn unreadable_config_disables_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!IngressConfig::load(dir.path()).await.line_enabled);
        std::fs::write(dir.path().join("config.toml"), "not = [toml").unwrap();
        assert!(!IngressConfig::load(dir.path()).await.line_enabled);
        std::fs::write(dir.path().join("config.toml"), "[channel_ingress]\n").unwrap();
        assert!(IngressConfig::load(dir.path()).await.line_enabled);
    }
}
