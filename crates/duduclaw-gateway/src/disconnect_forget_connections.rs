//! Conservative connection-state readback before planning and applying.

use std::path::Path;
use duduclaw_memory::SqliteMemoryEngine;

/// Fence deletion before publishing a newly connected credential/configuration.
/// SQLite serializes this with apply's ownership check and deletion transaction.
pub(crate) fn mark_connecting(home: &Path, reference: &str) -> Result<(), String> {
    let path = home.join("memory.db");
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("cannot verify memory ownership before connecting".into()),
        Ok(_) => {},
    }
    let conn = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| "cannot fence memory ownership before connecting")?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| "cannot fence memory ownership before connecting")?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='disconnected_connectors')",
        [], |r| r.get(0),
    ).map_err(|_| "cannot verify memory ownership before connecting")?;
    if exists {
        conn.execute("DELETE FROM disconnected_connectors WHERE connector_ref=?1", [reference])
            .map_err(|_| "cannot fence memory ownership before connecting")?;
    }
    Ok(())
}

pub(crate) fn mark_config_connecting(home: &Path, config: &toml::Table, agent: Option<&str>) -> Result<(), String> {
    if let Some(channels) = config.get("channels").and_then(toml::Value::as_table) {
        for (kind, field) in [
            ("line", "line_channel_token"), ("telegram", "telegram_bot_token"),
            ("discord", "discord_bot_token"), ("slack", "slack_bot_token"),
            ("googlechat", "googlechat_service_account_json"), ("whatsapp", "whatsapp_access_token"),
            ("feishu", "feishu_app_id"), ("teams", "teams_app_password"),
            ("wecom", "wecom_corp_secret"), ("dingtalk", "dingtalk_app_secret"),
        ] {
            if let Some(agent) = agent {
                if channels.contains_key(kind) { mark_connecting(home, &format!("channel:{kind}:{agent}"))?; }
            } else if channels.contains_key(field) || channels.contains_key(&format!("{field}_enc")) {
                mark_connecting(home, &format!("channel:{kind}"))?;
            }
        }
    }
    if agent.is_none() {
        if let Some(sources) = config.get("db_sources").and_then(toml::Value::as_table) {
            for name in sources.keys() { mark_connecting(home, &format!("db:{name}"))?; }
        }
        if config.get("integrations").and_then(toml::Value::as_table).is_some_and(|integrations|
            integrations.contains_key("google_service_account") || integrations.contains_key("google_apps_script")) {
            mark_connecting(home, "google:home")?;
        }
    }
    Ok(())
}

fn table(path: &Path) -> Option<toml::Table> {
    std::fs::read_to_string(path).ok()?.parse().ok()
}

/// Only absence in a readable authoritative store proves a disconnect.
/// Disabled, expired or unreadable credentials still retain ownership.
fn confirmed_disconnected(home: &Path, reference: &str) -> bool {
    if let Some(name) = reference.strip_prefix("db:") {
        let Some(config) = table(&home.join("config.toml")) else { return false; };
        return match config.get("db_sources") {
            None => true,
            Some(value) => value.as_table().is_some_and(|sources| !sources.contains_key(name)),
        };
    }
    if reference == "google:home" {
        let Some(config) = table(&home.join("config.toml")) else { return false; };
        if let Some(value) = config.get("integrations") {
            let Some(integrations) = value.as_table() else { return false; };
            if integrations.contains_key("google_service_account") || integrations.contains_key("google_apps_script") {
                return false;
            }
        }
        return crate::mcp_oauth::home_store::closed_readonly_control(home).is_ok_and(|control|
            control.is_null() || (control.get("credential").is_some_and(serde_json::Value::is_null)
                && control.get("source_generation").is_some()));
    }
    if reference == "google:delegation" {
        return crate::google_identity::delegation_key::info(home).is_ok_and(|record| record.is_none());
    }
    if let Some(user) = reference.strip_prefix("google:person:") {
        return crate::person_credentials::get(home, user).is_ok_and(|record| record.is_none());
    }
    if let Some(remote) = reference.strip_prefix("remote_mcp:") {
        let Some((agent, server)) = remote.split_once('/') else { return false; };
        if crate::remote_mcp::store::validate_ids(agent, server).is_err() { return false; }
        return crate::remote_mcp::store::get(home, agent, server).is_ok_and(|record|
            record.is_none_or(|record| record.status == crate::remote_mcp::store::ConnStatus::NotConnected));
    }
    if let Some(channel) = reference.strip_prefix("channel:") {
        if let Some((kind, agent)) = channel.split_once(':') {
            if !duduclaw_core::is_valid_agent_id(agent) { return false; }
            let dir = crate::ephemeral::resolve_agent_dir(home, agent)
                .unwrap_or_else(|| home.join("agents").join(agent));
            let Some(config) = table(&dir.join("agent.toml")) else { return false; };
            return match config.get("channels") {
                None => true,
                Some(value) => value.as_table().is_some_and(|channels| !channels.contains_key(kind)),
            };
        }
        let field = match channel {
            "line" => "line_channel_token", "telegram" => "telegram_bot_token",
            "discord" => "discord_bot_token", "slack" => "slack_bot_token",
            "googlechat" => "googlechat_service_account_json",
            "whatsapp" => "whatsapp_access_token", "feishu" => "feishu_app_id",
            "teams" => "teams_app_password", "wecom" => "wecom_corp_secret",
            "dingtalk" => "dingtalk_app_secret", _ => return false,
        };
        let Some(config) = table(&home.join("config.toml")) else { return false; };
        return match config.get("channels") {
            None => true,
            Some(value) => value.as_table().is_some_and(|channels|
                !channels.contains_key(field) && !channels.contains_key(&format!("{field}_enc"))),
        };
    }
    false
}

pub(super) async fn refresh(home: &Path, engine: &SqliteMemoryEngine, selected: &str) -> Result<(), String> {
    let refs = engine.disconnected_connector_refs().await.map_err(|e| e.to_string())?;
    let confirmed: Vec<(String, String)> = refs.into_iter()
        .filter(|(reference, _)| confirmed_disconnected(home, reference)).collect();
    engine.retain_disconnected_connectors(&confirmed).await.map_err(|e| e.to_string())?;
    if !confirmed_disconnected(home, selected) {
        return Err("connection is active or its state cannot be verified".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Added regression tests; not run in this change.
    #[tokio::test]
    async fn reconnected_or_unreadable_source_loses_disconnect_evidence() {
        let home = tempfile::tempdir().unwrap();
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        engine.record_connector_disconnect("db:shared").await.unwrap();
        std::fs::write(home.path().join("config.toml"), "[db_sources.shared]\ndriver='sqlite'\n").unwrap();
        refresh(home.path(), &engine, "db:removed").await.unwrap();
        assert!(engine.disconnected_connector_refs().await.unwrap().is_empty());
        assert!(refresh(home.path(), &engine, "db:shared").await.is_err());
        engine.record_connector_disconnect("db:shared").await.unwrap();
        std::fs::write(home.path().join("config.toml"), "[").unwrap();
        assert!(refresh(home.path(), &engine, "db:removed").await.is_err());
        assert!(engine.disconnected_connector_refs().await.unwrap().is_empty());
    }
}
