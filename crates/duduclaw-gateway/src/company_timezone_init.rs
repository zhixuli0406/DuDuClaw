//! One-time company timezone initialization, invoked only by the gateway lock holder.
use std::path::Path;

use duduclaw_core::company_timezone::{self, InitDecision};
use tracing::warn;

use crate::handlers::config_commit::{
    commit_home_table_snapshot, read_home_table_snapshot, replace_owner_only, table_at_mut,
};

const MARKER: &str = "state/company-timezone-init.json";

fn initialization_pending(home: &Path) -> bool {
    match home.join(MARKER).try_exists() {
        Ok(exists) => !exists,
        Err(error) => {
            warn!(%error, "Cannot inspect company timezone initialization marker");
            false
        }
    }
}

pub fn run_once(home: &Path) {
    // Do not even detect the host zone after initialization, including after
    // an administrator clears the company setting.
    if !initialization_pending(home) { return; }
    run_once_with(home, company_timezone::detect_host_zone());
}

pub(crate) fn run_once_with(home: &Path, detected: Option<String>) {
    if !initialization_pending(home) { return; }
    let path = home.join("config.toml");
    match path.try_exists() {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            warn!(%error, "Cannot inspect company timezone configuration for initialization");
            return;
        }
    }
    let (mut table, snapshot) = match read_home_table_snapshot(&path) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(%error, "Cannot read company timezone configuration for initialization");
            return;
        }
    };
    let config_has_key = match table.get("general") {
        None => false,
        Some(value) => match value.as_table() {
            Some(general) => general.contains_key("company_timezone"),
            None => {
                warn!("Invalid general table; company timezone initialization deferred");
                return;
            }
        },
    };
    let result = match company_timezone::upgrade_init_decision(config_has_key, false, detected.as_deref()) {
        InitDecision::AlreadyDone => return,
        InitDecision::AlreadySet => "already_set",
        InitDecision::Unavailable => "unavailable",
        InitDecision::Write(name) => {
            let general = match table_at_mut(&mut table, &["general"]) {
                Ok(general) => general,
                Err(error) => {
                    warn!(%error, "Cannot prepare company timezone initialization");
                    return;
                }
            };
            general.insert("company_timezone".into(), toml::Value::String(name.clone()));
            general.insert("company_timezone_source".into(), toml::Value::String("detected_at_upgrade".into()));
            if let Err(error) = commit_home_table_snapshot(&path, snapshot, &table) {
                warn!(%error, "Cannot commit company timezone initialization");
                return;
            }
            duduclaw_security::audit::log_config_changed(home, "system", "system", &[
                format!("general.company_timezone = \"{name}\" (detected at upgrade)"),
            ]);
            "written"
        }
    };
    let marker = serde_json::json!({
        "version": "1.86.0", "result": result, "at": chrono::Utc::now().to_rfc3339(),
    });
    // Publish a complete marker with owner-only file permissions.
    if let Err(error) = std::fs::create_dir_all(home.join("state"))
        .and_then(|_| replace_owner_only(&home.join(MARKER), &marker.to_string()))
    {
        warn!(%error, "Cannot write company timezone initialization marker");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use company_timezone::CompanyZone;

    fn marker(home: &Path) -> serde_json::Value {
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(MARKER)).unwrap(),
        ).unwrap();
        assert_eq!(value["version"], "1.86.0");
        assert!(chrono::DateTime::parse_from_rfc3339(value["at"].as_str().unwrap()).is_ok());
        value
    }

    #[test]
    fn init_writes_detected_zone_once_and_marker() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "# keep this comment\n[general]\nname = 'test'\n").unwrap();
        run_once_with(home.path(), Some("Asia/Taipei".into()));
        assert_eq!(company_timezone::read(home.path()), CompanyZone::Set(chrono_tz::Asia::Taipei));
        assert_eq!(company_timezone::read_source(home.path()).as_deref(), Some("detected_at_upgrade"));
        assert_eq!(marker(home.path())["result"], "written");
        let written = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
        assert!(written.contains("# keep this comment"));
        let audit_path = home.path().join("security_audit.jsonl");
        let audit = std::fs::read_to_string(&audit_path).unwrap();
        let events: Vec<serde_json::Value> = audit.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(events.iter().filter(|event| event["event_type"] == "config_changed").count(), 1);
        let original_marker = std::fs::read(home.path().join(MARKER)).unwrap();
        run_once_with(home.path(), Some("Asia/Tokyo".into()));
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), written);
        assert_eq!(std::fs::read_to_string(audit_path).unwrap(), audit);
        assert_eq!(std::fs::read(home.path().join(MARKER)).unwrap(), original_marker);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(home.path().join(MARKER)).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn init_never_overwrites_manual_value() {
        for value in ["'Asia/Tokyo'", "''", "'Not/AZone'", "8"] {
            let home = tempfile::tempdir().unwrap();
            let original = format!("[general]\ncompany_timezone = {value}\n");
            std::fs::write(home.path().join("config.toml"), &original).unwrap();
            run_once_with(home.path(), Some("Asia/Taipei".into()));
            assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
            assert_eq!(company_timezone::read_source(home.path()), None);
            assert_eq!(marker(home.path())["result"], "already_set");
        }
    }

    #[test]
    fn init_detection_failure_leaves_unset_and_marks_done() {
        for detected in [None, Some("Not/AZone".into())] {
            let home = tempfile::tempdir().unwrap();
            let original = "[general]\nname = 'test'\n";
            std::fs::write(home.path().join("config.toml"), original).unwrap();
            run_once_with(home.path(), detected);
            assert_eq!(company_timezone::read(home.path()), CompanyZone::Unset);
            assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
            assert_eq!(marker(home.path())["result"], "unavailable");
            run_once_with(home.path(), Some("Asia/Taipei".into()));
            assert_eq!(company_timezone::read(home.path()), CompanyZone::Unset);
        }
    }

    #[test]
    fn init_not_repeated_after_admin_clears() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[general]\nname = 'test'\n").unwrap();
        run_once_with(home.path(), Some("Asia/Taipei".into()));
        assert_eq!(marker(home.path())["result"], "written");
        std::fs::write(home.path().join("config.toml"), "[general]\n").unwrap();
        let original_marker = std::fs::read(home.path().join(MARKER)).unwrap();
        run_once_with(home.path(), Some("Asia/Tokyo".into()));
        assert_eq!(company_timezone::read(home.path()), CompanyZone::Unset);
        assert_eq!(std::fs::read(home.path().join(MARKER)).unwrap(), original_marker);
    }

    #[test]
    fn init_without_config_file_defers() {
        let home = tempfile::tempdir().unwrap();
        run_once_with(home.path(), Some("Asia/Taipei".into()));
        assert!(!home.path().join("config.toml").exists());
        assert!(!home.path().join(MARKER).exists());

        std::fs::write(home.path().join("config.toml"), "[general]\nname = 'test'\n").unwrap();
        run_once_with(home.path(), Some("Asia/Taipei".into()));
        assert_eq!(company_timezone::read(home.path()), CompanyZone::Set(chrono_tz::Asia::Taipei));
        assert_eq!(marker(home.path())["result"], "written");
    }

    #[test]
    fn init_unparseable_config_writes_no_marker() {
        for original in ["[broken", "general = false"] {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("config.toml"), original).unwrap();
            run_once_with(home.path(), Some("Asia/Taipei".into()));
            assert!(!home.path().join(MARKER).exists());
            assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
        }
    }
}
