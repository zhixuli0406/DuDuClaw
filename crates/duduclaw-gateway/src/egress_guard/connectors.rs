//! Connection stamps are attribution hints, never egress authority.

use std::path::Path;
use duduclaw_core::google_identity::{Effective, Fallback};

fn table(path: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

pub(super) fn declared_allowed(home: &Path, agent: &str, reference: &str) -> bool {
    if !duduclaw_core::is_valid_agent_id(agent) || !super::valid_source(reference) { return false; }
    let dir = crate::ephemeral::resolve_agent_dir(home, agent)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let resolved = duduclaw_core::preset::agent_resolved_path(home, agent);
    let path = if resolved.exists() { resolved } else { dir.join("agent.toml") };
    let Some(employee) = table(&path) else { return false; };
    if let Some(source) = reference.strip_prefix("db:") {
        let Some(config) = table(&home.join("config.toml")) else { return false; };
        return duduclaw_db::parse_db_sources(&config).get(source).is_some()
            && employee.get("capabilities").and_then(|v| v.get("db_sources"))
                .and_then(toml::Value::as_array)
                .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(source)));
    }
    let effective = duduclaw_core::google_identity::effective_for(home, Some(agent));
    if !crate::google_workspace::integration_enabled(home) { return false; }
    if reference == "google:home" {
        let granted = matches!(&effective.effective, Effective::Home)
            || (matches!(&effective.effective, Effective::Delegate { .. } | Effective::Personal { .. })
                && effective.fallback == Fallback::Home);
        if !granted { return false; }
        let Some(raw) = std::fs::read_to_string(home.join("config.toml")).ok() else { return false; };
        return crate::google_service_account::parse_config(&raw, home).ok().flatten().is_some()
            || crate::google_apps_script::parse_bridge_section(&raw).ok().flatten().is_some()
            || crate::mcp_oauth::get_stored_token(home, "google").is_some();
    }
    if reference == "google:delegation" {
        return matches!(&effective.effective, Effective::Delegate { .. })
            && crate::google_identity::delegation_key::info(home).ok().flatten().is_some();
    }
    let Some(user_id) = reference.strip_prefix("google:person:") else { return false; };
    let Effective::Personal { selector } = &effective.effective else { return false; };
    if !crate::team_edition::team_grants_enabled(home) || !home.join("users.db").is_file() { return false; }
    let Ok(db) = duduclaw_auth::UserDb::new(&home.join("users.db")) else { return false; };
    let Ok(user) = crate::google_personal::access::select_user(&db, agent, selector) else { return false; };
    if user.id != user_id || user.status != duduclaw_auth::UserStatus::Active
        || db.google_consent(user_id, agent).ok().flatten() != Some(true) { return false; }
    crate::person_credentials::get(home, user_id).ok().flatten().is_some_and(|record|
        record.metadata.status == crate::person_credentials::Status::Connected)
}

pub(super) fn remote_reference(home: &Path, agent: &str, server: &str) -> Option<String> {
    crate::remote_mcp::store::validate_ids(agent, server).ok()?;
    let record = crate::remote_mcp::store::get(home, agent, server).ok()??;
    (record.status == crate::remote_mcp::store::ConnStatus::Connected)
        .then(|| format!("remote_mcp:{agent}/{server}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_require_existing_source_and_exact_employee_grant() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("config.toml"), "[db_sources.sales]\ndriver='sqlite'\nurl='/tmp/sales.db'\nallowed_tables=['orders']\n").unwrap();
        let path = home.path().join("agents/bot/agent.toml");
        std::fs::write(&path, "[capabilities]\ndb_sources=['sales']\n").unwrap();
        assert!(declared_allowed(home.path(), "bot", "db:sales"));
        for rejected in ["db:absent", "channel:telegram", "remote_mcp:bot/server", "unknown"] {
            assert!(!declared_allowed(home.path(), "bot", rejected));
        }
        std::fs::write(&path, "[capabilities]\ndb_sources=[]\n").unwrap();
        assert!(!declared_allowed(home.path(), "bot", "db:sales"));
        std::fs::write(&path, "[").unwrap();
        assert!(!declared_allowed(home.path(), "bot", "db:sales"));
    }
}
