//! One-time upgrade migration for `config.toml [odoo] allow_private_network`.
//!
//! Before this switch existed the runtime never checked the Odoo server url,
//! so an install whose Odoo lives on an internal address (global url or a
//! per-employee `agent.toml [odoo] url`) worked. The connector now refuses
//! internal addresses unless an administrator turns the switch on. To avoid
//! cutting such customers off on upgrade, the first boot after the upgrade
//! resolves the configured urls and, when any of them lands on an internal
//! address, writes `allow_private_network = true` (comment-preserving, through
//! the same locked commit as the dashboard) plus an audit event and an info
//! log. New installs and installs without internal urls keep the default
//! `false`.
//!
//! Runs once (marker under `<home>/migrations/`), so an administrator who
//! later turns the switch off, or adds an internal url afterwards, is never
//! silently overridden. Never blocks boot: every failure degrades to a `warn!`.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use duduclaw_security::audit::{AuditEvent, Severity};
use tracing::{info, warn};

const MARKER_NAME: &str = "odoo-allow-private-network.done";

/// What one pass did (returned for tests and boot logging).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// A marker already existed.
    pub already_done: bool,
    /// The switch was written as `true` by this pass.
    pub enabled: bool,
    /// The switch was already present in `config.toml`; left alone.
    pub switch_present: bool,
    /// Why the switch was enabled (host names only, never full urls).
    pub hosts: Vec<String>,
    pub error: Option<String>,
}

fn marker_path(home: &Path) -> PathBuf {
    home.join("migrations").join(MARKER_NAME)
}

/// Odoo urls configured in `config.toml [odoo]` and every
/// `agents/*/agent.toml [odoo]`.
fn configured_urls(home: &Path, global: &toml::Table) -> Vec<String> {
    let mut urls = Vec::new();
    let url_of = |t: &toml::Table| {
        t.get("odoo")
            .and_then(|v| v.as_table())
            .and_then(|o| o.get("url"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    urls.extend(url_of(global));
    if let Ok(rd) = std::fs::read_dir(home.join("agents")) {
        for entry in rd.flatten() {
            let path = entry.path().join("agent.toml");
            if let Ok(raw) = std::fs::read_to_string(&path)
                && let Ok(t) = raw.parse::<toml::Table>()
            {
                urls.extend(url_of(&t));
            }
        }
    }
    urls
}

/// Does `url` point at an internal (non-public, non-loopback-exception)
/// address? Literal hosts are judged directly, names are resolved (5 s cap);
/// an unresolvable name counts as "no".
async fn is_internal(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_string();
    // The strict-localhost HTTP exception never needed the switch.
    if parsed.scheme() == "http" && matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
        return None;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']').to_string();
    let addrs: Vec<IpAddr> = if let Ok(ip) = bare.parse::<IpAddr>() {
        vec![ip]
    } else {
        let port = parsed.port_or_known_default()?;
        let lookup = tokio::net::lookup_host((bare.as_str(), port));
        match tokio::time::timeout(Duration::from_secs(5), lookup).await {
            Ok(Ok(it)) => it.map(|a| a.ip()).collect(),
            _ => return None,
        }
    };
    addrs
        .iter()
        .any(|ip| duduclaw_odoo::url_guard::is_private_ip(*ip))
        .then_some(host)
}

/// Run the migration once for `home`.
pub async fn run(home: &Path) -> MigrationReport {
    let mut report = MigrationReport::default();
    if marker_path(home).exists() {
        report.already_done = true;
        return report;
    }
    let config_path = home.join("config.toml");
    let raw = match crate::handlers::config_commit::read_text_or_empty(&config_path) {
        Ok(r) => r,
        Err(e) => {
            // Unreadable config: do not guess, do not write the marker.
            report.error = Some(e);
            return report;
        }
    };
    let mut table: toml::Table = match raw.parse() {
        Ok(t) => t,
        Err(_) => {
            report.error = Some("config.toml is not valid TOML".into());
            return report;
        }
    };
    let present = table
        .get("odoo")
        .and_then(|v| v.as_table())
        .is_some_and(|o| o.contains_key("allow_private_network"));
    if present {
        report.switch_present = true;
    } else {
        for url in configured_urls(home, &table) {
            if let Some(host) = is_internal(&url).await
                && !report.hosts.contains(&host)
            {
                report.hosts.push(host);
            }
        }
        if !report.hosts.is_empty() {
            let odoo = table
                .entry("odoo")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(o) = odoo.as_table_mut() {
                o.insert("allow_private_network".into(), toml::Value::Boolean(true));
            }
            let hash = crate::handlers::config_commit::content_hash(&raw);
            match crate::handlers::config_commit::commit_table_locked(&config_path, hash, &table).await {
                Ok(()) => {
                    report.enabled = true;
                    audit(home, &report.hosts);
                    info!(
                        hosts = ?report.hosts,
                        "Odoo allow_private_network enabled by upgrade migration \
                         (an Odoo url resolves to an internal address); an administrator can turn it off in the Odoo settings"
                    );
                }
                Err(e) => {
                    // config.toml changed under us: retry on the next boot.
                    warn!("Odoo allow_private_network migration not applied: {e}");
                    report.error = Some(e);
                    return report;
                }
            }
        }
    }
    if let Err(e) = write_marker(home) {
        warn!("Odoo allow_private_network migration marker not written: {e}");
        report.error = Some(e);
    }
    report
}

fn write_marker(home: &Path) -> Result<(), String> {
    let path = marker_path(home);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, "done\n").map_err(|e| e.to_string())
}

fn audit(home: &Path, hosts: &[String]) {
    let event = AuditEvent::new(
        "odoo_allow_private_network_auto_enabled",
        "odoo",
        Severity::Warning,
        serde_json::json!({
            "hosts": hosts,
            "reason": "existing Odoo url resolves to an internal address (upgrade migration)",
        }),
    );
    crate::security_autopilot::audit_and_emit(home, &event);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        let p = home.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn switch(home: &Path) -> Option<bool> {
        let t: toml::Table = std::fs::read_to_string(home.join("config.toml")).ok()?.parse().ok()?;
        t.get("odoo")?.as_table()?.get("allow_private_network")?.as_bool()
    }

    #[tokio::test]
    async fn global_internal_url_turns_the_switch_on_and_keeps_comments() {
        let h = tempfile::tempdir().unwrap();
        write(h.path(), "config.toml", "# keep me\n[odoo]\nurl = \"https://192.168.1.20\"\ndb = \"d\"\n");
        let r = run(h.path()).await;
        assert!(r.enabled, "{r:?}");
        assert_eq!(r.hosts, vec!["192.168.1.20".to_string()]);
        assert_eq!(switch(h.path()), Some(true));
        let text = std::fs::read_to_string(h.path().join("config.toml")).unwrap();
        assert!(text.contains("# keep me"));
        let audit = std::fs::read_to_string(h.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("odoo_allow_private_network_auto_enabled"));
        // Second pass is a no-op (marker).
        assert!(run(h.path()).await.already_done);
    }

    #[tokio::test]
    async fn agent_level_internal_url_also_counts() {
        let h = tempfile::tempdir().unwrap();
        write(h.path(), "config.toml", "[odoo]\nurl = \"https://erp.example.com\"\n");
        write(h.path(), "agents/a1/agent.toml", "[odoo]\nurl = \"http://10.1.2.3:8069\"\n");
        let r = run(h.path()).await;
        assert!(r.enabled, "{r:?}");
        assert_eq!(switch(h.path()), Some(true));
    }

    #[tokio::test]
    async fn public_or_localhost_urls_leave_the_default_off_and_never_retry() {
        let h = tempfile::tempdir().unwrap();
        write(h.path(), "config.toml", "[odoo]\nurl = \"http://127.0.0.1:8069\"\n");
        write(h.path(), "agents/a1/agent.toml", "[odoo]\nurl = \"https://8.8.8.8\"\n");
        let r = run(h.path()).await;
        assert!(!r.enabled);
        assert_eq!(switch(h.path()), None);
        // An internal url added later must not be auto-enabled by a re-run.
        write(h.path(), "agents/a2/agent.toml", "[odoo]\nurl = \"https://10.0.0.9\"\n");
        let r2 = run(h.path()).await;
        assert!(r2.already_done && !r2.enabled);
        assert_eq!(switch(h.path()), None);
    }

    #[tokio::test]
    async fn existing_switch_value_is_never_overridden() {
        let h = tempfile::tempdir().unwrap();
        write(
            h.path(),
            "config.toml",
            "[odoo]\nurl = \"https://10.0.0.5\"\nallow_private_network = false\n",
        );
        let r = run(h.path()).await;
        assert!(r.switch_present && !r.enabled);
        assert_eq!(switch(h.path()), Some(false));
    }

    #[tokio::test]
    async fn fresh_install_without_odoo_stays_default() {
        let h = tempfile::tempdir().unwrap();
        let r = run(h.path()).await;
        assert!(!r.enabled && r.error.is_none());
        assert_eq!(switch(h.path()), None);
    }
}
