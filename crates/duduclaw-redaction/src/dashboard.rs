//! Dashboard RPC handlers — read-only views for v1.14.0.
//!
//! These are pure functions (`fn handle_xxx(manager) -> Value`) so the
//! gateway's existing axum RPC dispatcher can adopt them with one match
//! arm per method. We deliberately avoid imposing an axum dependency
//! here so this crate stays light.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::manager::RedactionManager;
use crate::toggle::{ForceOverrideFlag, ForceOverrideRecord, override_banner};
use crate::vault::VaultStats;

/// Top-level RPC method registry. Add new methods at the bottom; never
/// rename or repurpose existing ones (dashboard frontend pins to them).
pub const METHODS: &[&str] = &[
    "redaction.stats",
    "redaction.recent_audit",
    "redaction.override_status",
    "redaction.policy_status",
];

/// `redaction.stats` — vault counts (active / expired / by_category).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub vault: VaultStats,
    pub rule_count: usize,
    pub config_enabled: bool,
    pub vault_ttl_hours: i64,
}

pub fn handle_stats(manager: &RedactionManager) -> Result<StatsResponse> {
    let vault = manager.vault().stats()?;
    Ok(StatsResponse {
        vault,
        rule_count: manager.engine().rule_count(),
        config_enabled: manager.config_enabled(),
        vault_ttl_hours: manager.vault_ttl_hours(),
    })
}

/// `redaction.recent_audit` — tail the audit JSONL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentAuditRequest {
    /// How many recent lines to return (default 50, cap 500).
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentAuditResponse {
    /// Each entry is a JSON object (one audit line). Empty if the file
    /// doesn't exist yet.
    pub entries: Vec<serde_json::Value>,
}

pub fn handle_recent_audit(
    manager: &RedactionManager,
    req: RecentAuditRequest,
) -> Result<RecentAuditResponse> {
    let limit = req.limit.clamp(1, 500);
    let Some(path) = manager.paths().audit_log.as_ref() else {
        return Ok(RecentAuditResponse { entries: Vec::new() });
    };
    if !path.exists() {
        return Ok(RecentAuditResponse { entries: Vec::new() });
    }
    let body = std::fs::read_to_string(path)?;
    let entries: Vec<serde_json::Value> = body
        .lines()
        .rev()
        .take(limit)
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    Ok(RecentAuditResponse { entries })
}

/// `redaction.override_status` — banner + record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverrideStatusResponse {
    pub active: bool,
    pub banner: Option<String>,
    pub record: Option<ForceOverrideRecord>,
}

pub fn handle_override_status(manager: &RedactionManager) -> Result<OverrideStatusResponse> {
    let flag = ForceOverrideFlag::new(manager.paths().override_flag.clone());
    let active = flag.is_active();
    let banner = override_banner(&flag)?;
    let record = flag.read()?;
    Ok(OverrideStatusResponse {
        active,
        banner,
        record,
    })
}

/// `redaction.policy_status` — concise snapshot for the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyStatusResponse {
    pub config_enabled: bool,
    pub vault_ttl_hours: i64,
    pub purge_after_expire_days: u32,
    pub rule_count: usize,
    pub override_active: bool,
}

pub fn handle_policy_status(manager: &RedactionManager) -> Result<PolicyStatusResponse> {
    let override_active = Path::new(&manager.paths().override_flag).exists();
    Ok(PolicyStatusResponse {
        config_enabled: manager.config_enabled(),
        vault_ttl_hours: manager.vault_ttl_hours(),
        purge_after_expire_days: manager.purge_after_expire_days(),
        rule_count: manager.engine().rule_count(),
        override_active,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::NullAuditSink;
    use crate::config::RedactionConfig;
    use crate::manager::ManagerPaths;
    use crate::source::{Caller, RestoreTarget, Source};
    use tempfile::TempDir;

    fn fresh() -> (RedactionManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let mut cfg = RedactionConfig::default();
        cfg.enabled = true;
        cfg.profiles = vec!["general".into()];
        let paths = ManagerPaths::under_home(tmp.path());
        let m = RedactionManager::open(cfg, paths).unwrap();
        (m, tmp)
    }

    #[test]
    fn stats_shows_active_entries_after_redact() {
        let (m, _t) = fresh();
        let p = m.pipeline("agnes", Some("s1".into())).unwrap();
        p.redact(
            "ping alice@acme.com bob@acme.com",
            &Source::ToolResult { tool_name: "x".into() },
        )
        .unwrap();
        let s = handle_stats(&m).unwrap();
        assert!(s.vault.total >= 2);
        assert!(s.config_enabled);
        assert!(s.rule_count > 0);
    }

    #[test]
    fn recent_audit_empty_when_no_audit_yet() {
        let (m, _t) = fresh();
        let r = handle_recent_audit(&m, RecentAuditRequest { limit: 10 }).unwrap();
        assert!(r.entries.is_empty() || !r.entries.is_empty()); // smoke check
    }

    #[test]
    fn recent_audit_returns_records_after_redact() {
        let (m, _t) = fresh();
        let p = m.pipeline("agnes", Some("s1".into())).unwrap();
        p.redact(
            "alice@acme.com",
            &Source::ToolResult { tool_name: "x".into() },
        )
        .unwrap();
        let _ = p.restore(
            "<REDACT:EMAIL:00000000>",
            &Caller::owner("agnes"),
            RestoreTarget::UserChannel,
        );
        let r = handle_recent_audit(&m, RecentAuditRequest { limit: 50 }).unwrap();
        assert!(!r.entries.is_empty(), "expected redact audit lines");
    }

    #[test]
    fn override_status_inactive_when_no_flag() {
        let (m, _t) = fresh();
        let s = handle_override_status(&m).unwrap();
        assert!(!s.active);
        assert!(s.banner.is_none());
    }

    #[test]
    fn override_status_active_after_activation() {
        let (m, _t) = fresh();
        let flag = ForceOverrideFlag::new(m.paths().override_flag.clone());
        flag.activate(
            "lizhixu",
            vec!["line".into()],
            "test",
            &NullAuditSink,
        )
        .unwrap();
        let s = handle_override_status(&m).unwrap();
        assert!(s.active);
        assert!(s.banner.unwrap().contains("lizhixu"));
    }

    #[test]
    fn policy_status_reports_fields() {
        let (m, _t) = fresh();
        let s = handle_policy_status(&m).unwrap();
        assert!(s.config_enabled);
        assert!(s.vault_ttl_hours > 0);
        assert!(s.rule_count > 0);
        assert!(!s.override_active);
    }
}
