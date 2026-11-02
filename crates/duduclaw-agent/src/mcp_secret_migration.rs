//! Boot migration uses the same cross-process config lock as every writer.
use std::path::Path;

#[derive(Default)]
pub struct MigrationReport {
    pub migrated: usize,
    pub failed: std::collections::HashSet<String>,
    pub scan_complete: bool,
}

pub fn migrate_all(home: &Path) -> MigrationReport {
    let mut report = MigrationReport::default();
    let agents = match std::fs::read_dir(home.join("agents")) {
        Ok(agents) => agents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return report,
        Err(_) => {
            tracing::warn!("MCP secret migration could not list employees");
            return report;
        }
    };
    report.scan_complete = true;
    for entry in agents {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                report.scan_complete = false;
                tracing::warn!("MCP secret migration could not read an employee entry");
                continue;
            }
        };
        let name = entry.file_name();
        let Some(agent) = name.to_str() else { continue };
        if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_internal_agent_dir(agent) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {}
            Ok(_) => continue,
            Err(_) => {
                report.failed.insert(agent.to_string());
                tracing::warn!(agent, "MCP secret migration could not inspect employee directory");
                continue;
            }
        }
        let config_path = entry.path().join(".mcp.json");
        let result = crate::mcp_template::with_mcp_config_lock(&config_path, || {
            let profile_path = entry.path().join("agent.toml");
            let profile = match std::fs::read_to_string(profile_path) {
                Ok(profile) => profile,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(_) => return Err("employee profile unavailable".into()),
            };
            let profile: toml::Value = toml::from_str(&profile).map_err(|_| "invalid employee profile".to_string())?;
            if profile.get("agent").and_then(|a| a.get("status")).and_then(toml::Value::as_str) == Some("deleted") {
                return Ok(false);
            }
            duduclaw_security::mcp_secret_store::migrate_agent(home, agent, &config_path)
                .map_err(|_| "MCP secret migration failed; inspect config and retained encrypted backups before retrying".to_string())
        });
        match result {
            Ok(true) => report.migrated += 1,
            Ok(false) => {}
            Err(reason) => {
                report.failed.insert(agent.to_string());
                tracing::warn!(agent, %reason, "MCP secret migration skipped employee");
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn sweep_continues_after_failure_and_skips_deleted_employees() {
        let home = tempfile::tempdir().unwrap();
        for agent in ["broken", "ready", "deleted"] {
            let dir = home.path().join("agents").join(agent);
            std::fs::create_dir_all(&dir).unwrap();
            let status = if agent == "deleted" { "deleted" } else { "active" };
            std::fs::write(dir.join("agent.toml"), format!("[agent]\nstatus = \"{status}\"\n")).unwrap();
            let config = if agent == "broken" { "invalid json" } else {
                r#"{"mcpServers":{"vendor":{"command":"vendor","env":{"TOKEN":"test-secret"}}}}"#
            };
            std::fs::write(dir.join(".mcp.json"), config).unwrap();
        }
        let report = migrate_all(home.path());
        assert_eq!(report.migrated, 1);
        assert!(report.failed.contains("broken"));
        assert_eq!(migrate_all(home.path()).migrated, 0);
        assert_eq!(std::fs::read_to_string(home.path().join("agents/broken/.mcp.json")).unwrap(), "invalid json");
        assert!(!home.path().join("secrets/mcp/deleted.enc").exists());
    }
}
