//! RM-173 (v1.81.0): conversation auto-archive / purge are admin settings.
//!
//! ```toml
//! [sessions]
//! archive_after_days = 30   # 0 = never archive (收起); max 3650
//! purge_after_days = 0      # 0 = never purge; counted from archived_at; max 3650
//! ```
//!
//! The 6-hourly maintenance task re-reads these on every run. Every doubt
//! fails toward KEEPING data: a missing, unreadable or malformed config, a
//! non-table `[sessions]`, or an invalid `purge_after_days` never purges. An
//! invalid `archive_after_days` keeps the default (archiving is reversible —
//! a new message or a restore lifts it).

use std::path::Path;

use serde_json::{Value, json};

use crate::session::SessionManager;

pub const SECTION: &str = "sessions";
pub const ARCHIVE_KEY: &str = "archive_after_days";
pub const PURGE_KEY: &str = "purge_after_days";
pub const DEFAULT_ARCHIVE_AFTER_DAYS: u64 = 30;
pub const DEFAULT_PURGE_AFTER_DAYS: u64 = 0;
pub const MAX_DAYS: u64 = 3650;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Days without activity before a conversation is archived; 0 = never.
    pub archive_after_days: u64,
    /// Days after `archived_at` before its content is deleted; 0 = never.
    pub purge_after_days: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            archive_after_days: DEFAULT_ARCHIVE_AFTER_DAYS,
            purge_after_days: DEFAULT_PURGE_AFTER_DAYS,
        }
    }
}

impl Retention {
    pub fn archive_after(&self) -> Option<u64> {
        (self.archive_after_days > 0).then_some(self.archive_after_days)
    }

    pub fn purge_after(&self) -> Option<u64> {
        (self.purge_after_days > 0).then_some(self.purge_after_days)
    }

    pub fn to_json(self) -> Value {
        json!({ARCHIVE_KEY: self.archive_after_days, PURGE_KEY: self.purge_after_days})
    }
}

fn day_value(value: &toml::Value) -> Option<u64> {
    value
        .as_integer()
        .and_then(|n| u64::try_from(n).ok())
        .filter(|n| *n <= MAX_DAYS)
}

/// Effective settings of a parsed `config.toml`.
pub fn from_table(table: &toml::Table) -> Retention {
    let mut out = Retention::default();
    let Some(section) = table.get(SECTION).and_then(toml::Value::as_table) else {
        return out;
    };
    if let Some(n) = section.get(ARCHIVE_KEY).and_then(day_value) {
        out.archive_after_days = n;
    }
    // Absent or invalid ⇒ never purge.
    out.purge_after_days = section.get(PURGE_KEY).and_then(day_value).unwrap_or(0);
    out
}

/// Effective settings of `<home>/config.toml`; never fails (see module docs).
pub fn load(home: &Path) -> Retention {
    std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|raw| raw.parse::<toml::Table>().ok())
        .map(|table| from_table(&table))
        .unwrap_or_default()
}

/// Write-time validation for the raw config editor: refuse what the reader
/// would otherwise silently ignore.
pub fn validate_table(table: &toml::Table) -> Result<(), String> {
    let Some(section) = table.get(SECTION) else {
        return Ok(());
    };
    let section = section.as_table().ok_or("[sessions] must be a table")?;
    for key in [ARCHIVE_KEY, PURGE_KEY] {
        if let Some(value) = section.get(key) {
            if day_value(value).is_none() {
                return Err(format!(
                    "sessions.{key} must be an integer between 0 and {MAX_DAYS}"
                ));
            }
        }
    }
    Ok(())
}

/// One maintenance run: read the settings now, then archive / purge.
pub async fn run(home: &Path, sessions: &SessionManager) -> crate::session::RetentionOutcome {
    let settings = load(home);
    match sessions.apply_retention(settings).await {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!("Session retention error: {e}");
            crate::session::RetentionOutcome::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Retention {
        from_table(&raw.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn rm173_defaults_archive_thirty_days_and_never_purge() {
        assert_eq!(parse(""), Retention { archive_after_days: 30, purge_after_days: 0 });
        assert_eq!(parse("[sessions]\n"), Retention::default());
        assert_eq!(Retention::default().purge_after(), None);
        assert_eq!(Retention::default().archive_after(), Some(30));
    }

    #[test]
    fn rm173_zero_disables_each_step_and_values_are_read() {
        let r = parse("[sessions]\narchive_after_days = 0\npurge_after_days = 0\n");
        assert_eq!((r.archive_after(), r.purge_after()), (None, None));
        let r = parse("[sessions]\narchive_after_days = 7\npurge_after_days = 365\n");
        assert_eq!((r.archive_after(), r.purge_after()), (Some(7), Some(365)));
        let r = parse("[sessions]\narchive_after_days = 3650\npurge_after_days = 3650\n");
        assert_eq!((r.archive_after_days, r.purge_after_days), (3650, 3650));
    }

    #[test]
    fn rm173_invalid_values_fail_toward_keeping_data() {
        for raw in [
            "[sessions]\npurge_after_days = -1\n",
            "[sessions]\npurge_after_days = 3651\n",
            "[sessions]\npurge_after_days = \"30\"\n",
            "sessions = 5\n",
        ] {
            assert_eq!(parse(raw).purge_after(), None, "{raw}");
        }
        assert_eq!(parse("[sessions]\narchive_after_days = \"x\"\n").archive_after_days, 30);
        assert!(validate_table(&"[sessions]\npurge_after_days = 3651\n".parse().unwrap()).is_err());
        assert!(validate_table(&"[sessions]\narchive_after_days = -2\n".parse().unwrap()).is_err());
        assert!(validate_table(&"sessions = 1\n".parse().unwrap()).is_err());
        assert!(validate_table(&"[sessions]\narchive_after_days = 0\npurge_after_days = 90\n".parse().unwrap()).is_ok());
    }

    #[test]
    fn rm173_unreadable_config_archives_by_default_and_never_purges() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(load(home.path()), Retention::default(), "missing file");
        std::fs::write(home.path().join("config.toml"), "[sessions\npurge_after_days = 1").unwrap();
        assert_eq!(load(home.path()), Retention::default(), "malformed file");
        std::fs::remove_file(home.path().join("config.toml")).unwrap();
        std::fs::create_dir(home.path().join("config.toml")).unwrap();
        let r = load(home.path());
        assert_eq!((r.archive_after(), r.purge_after()), (Some(30), None), "unreadable");
        std::fs::remove_dir(home.path().join("config.toml")).unwrap();
        std::fs::write(home.path().join("config.toml"), "[sessions]\narchive_after_days = 5\npurge_after_days = 9\n").unwrap();
        assert_eq!(load(home.path()), Retention { archive_after_days: 5, purge_after_days: 9 });
    }
}
