//! One-time company-account backfill. Config, marker and notice publish together.
use std::path::Path;

use duduclaw_odoo::{AgentOdooConfig, OdooConfig, access_policy::OdooAccessConfig};
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde::Serialize;
use serde_json::json;

use crate::handlers::config_commit;

const MARKER: &str = "1.86.0";

#[derive(Debug, PartialEq, Eq)]
enum OwnAccount {
    Absent,
    Complete,
    Partial,
}

fn classify_account(odoo: Option<&AgentOdooConfig>, raw_odoo: Option<&toml::Value>) -> OwnAccount {
    let Some(raw_odoo) = raw_odoo else { return OwnAccount::Absent; };
    let Some(table) = raw_odoo.as_table() else { return OwnAccount::Partial; };
    const POLICY_KEYS: &[&str] = &[
        "profile", "allowed_models", "allowed_actions", "company_ids",
        "unblock_models", "permission_override", "act_as_requester",
    ];
    let policy_only = table.keys().all(|key| POLICY_KEYS.contains(&key.as_str()));
    // Unknown or legacy account keys must never grant the company account.
    if table.keys().any(|key| !POLICY_KEYS.contains(&key.as_str())
        && !matches!(key.as_str(), "username" | "api_key_enc" | "password_enc" | "url" | "db")) {
        return OwnAccount::Partial;
    }
    let Some(odoo) = odoo else {
        return if policy_only { OwnAccount::Absent } else { OwnAccount::Partial };
    };
    let populated = |value: &Option<String>| value.as_ref().is_some_and(|s| !s.trim().is_empty());
    if populated(&odoo.username) && (populated(&odoo.api_key_enc) || populated(&odoo.password_enc)) {
        OwnAccount::Complete
    } else if [&odoo.username, &odoo.api_key_enc, &odoo.password_enc, &odoo.url, &odoo.db]
        .iter().any(|value| value.is_some()) {
        OwnAccount::Partial
    } else if policy_only {
        OwnAccount::Absent
    } else {
        OwnAccount::Partial
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Employee {
    pub id: String,
    pub display_name: Option<String>,
    pub has_own_account: bool,
    #[serde(skip)]
    own_account: OwnAccount,
}

/// Only immediate regular employee directories, never the ephemeral namespace.
/// Unreadable/malformed employee credentials abort rather than grant a fallback.
pub(crate) fn employees(home: &Path) -> Result<Vec<Employee>, String> {
    let entries = match std::fs::read_dir(home.join("agents")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("無法讀取 AI 員工清單。".into()),
    };
    let mut rows = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| "無法讀取 AI 員工清單。")?;
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else { continue; };
        if !duduclaw_core::is_valid_agent_id(&id) || crate::ephemeral::is_ephemeral_id(&id) {
            continue;
        }
        let kind = entry.file_type().map_err(|_| "無法讀取 AI 員工目錄。")?;
        if kind.is_symlink() { return Err("AI 員工目錄不能是符號連結。".into()); }
        if !kind.is_dir() { continue; }
        let path = entry.path().join("agent.toml");
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("無法讀取 AI 員工設定。".into()),
        };
        if !metadata.is_file() { return Err("AI 員工設定必須是一般檔案。".into()); }
        let raw = std::fs::read_to_string(path).map_err(|_| "無法讀取 AI 員工設定。")?;
        let table: toml::Table = raw.parse().map_err(|_| "AI 員工設定格式有誤。")?;
        let raw_odoo = table.get("odoo");
        let odoo = if raw_odoo.is_some_and(|value| !value.is_table()) {
            None
        } else {
            AgentOdooConfig::parse_agent_toml(&raw)?
        };
        let own_account = classify_account(odoo.as_ref(), raw_odoo);
        let has_own_account = own_account == OwnAccount::Complete;
        let agent = table.get("agent").and_then(toml::Value::as_table);
        let display_name = agent.and_then(|a| a.get("display_name").or_else(|| a.get("name")))
            .and_then(toml::Value::as_str).map(str::to_owned);
        rows.push(Employee { id, display_name, has_own_account, own_account });
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.dedup_by(|a, b| a.id == b.id);
    Ok(rows)
}

pub(crate) async fn run(home: &Path) -> Result<bool, String> {
    let home = home.to_path_buf();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let path = home.join("config.toml");
        let raw = config_commit::read_text_or_empty(&path)?;
        let mut table: toml::Table = raw.parse().map_err(|_| "config.toml is invalid")?;
        let access = OdooAccessConfig::from_table(&table)?;
        if !access.company_key_agents_migrated.is_empty() { return Ok(false); }
        let document = toml::Value::Table(table.clone());
        match crate::odoo_identity::server::company_server_from_document(&document) {
            Ok(Some(_)) => {}
            Ok(None) => {
                tracing::warn!("Skipping Odoo company-account backfill: company server is not configured");
                return Ok(false);
            }
            Err(_) => {
                tracing::warn!("Skipping Odoo company-account backfill: invalid company server configuration");
                return Ok(false);
            }
        }
        let typed: Result<OdooConfig, _> = document.get("odoo").expect("validated company section")
            .clone().try_into();
        if typed.is_err() {
            tracing::warn!("Skipping Odoo company-account backfill: invalid typed Odoo configuration");
            return Ok(false);
        }
        let mut ids = Vec::new();
        let mut partial_agents = Vec::new();
        for employee in employees(&home)? {
            match employee.own_account {
                OwnAccount::Absent => ids.push(toml::Value::String(employee.id)),
                OwnAccount::Complete => {}
                OwnAccount::Partial => partial_agents.push(employee.id),
            }
        }
        // Aborting all backfill would disconnect every AI after upgrade. Excluding
        // only ambiguous accounts is the narrow fail-closed choice; admins must
        // explicitly assign these employees before they use the company account.
        if !partial_agents.is_empty() {
            tracing::warn!(?partial_agents, "Excluded incomplete own Odoo accounts from company-account backfill");
        }
        let count = ids.len();
        let section = config_commit::table_at_mut(&mut table, &["odoo_access"])?;
        section.insert("company_key_agents".into(), toml::Value::Array(ids));
        section.insert("company_key_agents_migrated".into(), toml::Value::String(MARKER.into()));
        section.insert("upgrade_notice_pending".into(), toml::Value::Boolean(true));
        if let Some(member) = table.get_mut("member_api_keys") {
            member.as_table_mut().ok_or("member_api_keys is invalid")?
                .entry("allow_conversation_content").or_insert(toml::Value::Boolean(false));
        }
        OdooAccessConfig::from_table(&table)?;
        runtime.block_on(config_commit::commit_table_locked_with_guard(
            &path, config_commit::content_hash(&raw), &table, move || {
                try_append_audit_event(&home, &AuditEvent::new(
                    "odoo_company_key_agents_backfilled", "", Severity::Info,
                    json!({"count":count,"marker":MARKER,"phase":"before_commit","partial_agents":partial_agents}),
                )).map_err(|_| "Odoo migration audit failed".to_owned())
            },
        ))?;
        Ok(true)
    }).await.map_err(|e| format!("Odoo migration task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(home: &Path, rel: &str, text: &str) {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn read(home: &Path) -> String { std::fs::read_to_string(home.join("config.toml")).unwrap() }
    const COMPANY: &str = "# keep comment\n[odoo]\nurl='https://erp.example.test'\ndb='company'\n";

    #[test]
    fn classifies_raw_odoo_account_keys() {
        for (raw, expected) in [
            ("", OwnAccount::Absent),
            ("[odoo]\n", OwnAccount::Absent),
            ("[odoo]\nlogin='alice'\napi_key='secret'\n", OwnAccount::Partial),
            ("[odoo]\nprofile='default'\nallowed_models=[]\nallowed_actions=[]\ncompany_ids=[]\nunblock_models=[]\npermission_override='inherit'\nact_as_requester=false\n", OwnAccount::Absent),
            ("odoo='x'\n", OwnAccount::Partial),
            ("[odoo]\nusername='alice'\napi_key_enc='encrypted'\n", OwnAccount::Complete),
            ("[odoo]\nusername='alice'\napi_key_enc='encrypted'\nlogin='legacy'\n", OwnAccount::Partial),
        ] {
            let table: toml::Table = raw.parse().unwrap();
            let odoo = AgentOdooConfig::from_agent_toml(raw);
            assert_eq!(classify_account(odoo.as_ref(), table.get("odoo")), expected, "{raw}");
        }
        for key in ["login", "api_key", "password", "user", "key", "server", "unknown"] {
            let raw = format!("[odoo]\n{key}='value'\n");
            let table: toml::Table = raw.parse().unwrap();
            let odoo = AgentOdooConfig::parse_agent_toml(&raw).unwrap();
            assert_eq!(classify_account(odoo.as_ref(), table.get("odoo")), OwnAccount::Partial, "{key}");
        }
    }

    #[tokio::test]
    async fn backfills_all_regular_employees_once_without_changing_mode() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        write(h, "config.toml", &format!("{COMPANY}[member_api_keys]\nenabled=true\n[odoo_access]\npermission_mode='engine'\n"));
        for id in ["c", "a", "b"] {
            write(h, &format!("agents/{id}/agent.toml"), "[agent]\nname='Employee'\n");
        }
        write(h, "agents/own/agent.toml", "[odoo]\nusername='own'\napi_key_enc='encrypted'\n");
        write(h, "agents/.ephemeral/eph-test/agent.toml", "[agent]\nname='Temp'\n");
        write(h, "agents/eph-other/agent.toml", "[agent]\nname='Temp'\n");
        assert!(run(h).await.unwrap());
        let raw = read(h);
        let config = OdooAccessConfig::parse_config_toml(&raw).unwrap();
        assert_eq!(config.company_key_agents, vec!["a", "b", "c"]);
        assert_eq!(config.company_key_agents_migrated, MARKER);
        assert!(config.upgrade_notice_pending);
        let mut table: toml::Table = raw.parse().unwrap();
        assert_eq!(table["odoo_access"]["permission_mode"].as_str(), Some("engine"));
        assert!(table["odoo_access"].get("permission_mode_ack").is_none());
        assert_eq!(table["member_api_keys"]["allow_conversation_content"].as_bool(), Some(false));
        assert!(raw.contains("# keep comment"));
        let audit = std::fs::read_to_string(h.join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("odoo_company_key_agents_backfilled"));
        assert!(!run(h).await.unwrap());
        assert_eq!(read(h), raw);
        assert_eq!(std::fs::read_to_string(h.join("security_audit.jsonl")).unwrap(), audit);
        table.get_mut("odoo_access").unwrap().as_table_mut().unwrap()
            .insert("company_key_agents".into(), toml::Value::Array(vec![]));
        write(h, "config.toml", &toml::to_string(&table).unwrap());
        assert!(!run(h).await.unwrap());
        assert!(OdooAccessConfig::parse_config_toml(&read(h)).unwrap().company_key_agents.is_empty());
    }

    #[tokio::test]
    async fn excludes_partial_accounts_and_audits_their_ids() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        write(h, "config.toml", COMPANY);
        for (id, raw) in [
            ("absent", "[agent]\nname='Absent'\n"),
            ("policy", "[odoo]\nallowed_models=['res.partner']\nallowed_actions=['read']\nprofile='default'\ncompany_ids=[1]\nunblock_models=[]\npermission_override='read_only'\nact_as_requester=true\n"),
            ("complete", "[odoo]\nusername='own'\napi_key_enc='encrypted'\n"),
            ("password", "[odoo]\nusername='own'\npassword_enc='encrypted'\n"),
            ("username", "[odoo]\nusername='own'\n"),
            ("url", "[odoo]\nurl='https://erp.example.test'\n"),
            ("db", "[odoo]\ndb='other'\n"),
            ("empty", "[odoo]\nusername=''\napi_key_enc='  '\n"),
            ("empty-url", "[odoo]\nurl=''\n"),
            ("key", "[odoo]\napi_key_enc='encrypted'\n"),
            ("legacy", "[odoo]\nlogin='alice'\napi_key='secret'\n"),
            ("non-table", "odoo='x'\n"),
        ] {
            write(h, &format!("agents/{id}/agent.toml"), raw);
        }
        assert!(run(h).await.unwrap());
        let config = OdooAccessConfig::parse_config_toml(&read(h)).unwrap();
        assert_eq!(config.company_key_agents, vec!["absent", "policy"]);
        assert_eq!(config.company_key_agents_migrated, MARKER);
        let audit = std::fs::read_to_string(h.join("security_audit.jsonl")).unwrap();
        let events: Vec<serde_json::Value> = audit.lines()
            .map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["details"]["partial_agents"],
            json!(["db", "empty", "empty-url", "key", "legacy", "non-table", "url", "username"]));
        assert_eq!(events[0]["details"]["count"], 2);
    }

    #[tokio::test]
    async fn invalid_company_is_untouched_and_later_valid_start_backfills() {
        for raw in [
            "[odoo]\nurl='ftp://x'\ndb='company'\n",
            "[odoo]\nurl='https://erp.example.test/?query=1'\ndb='company'\n",
            "[odoo]\nurl='https://erp.example.test/#fragment'\ndb='company'\n",
            "[odoo]\nurl='https://erp.example.test'\ndb='invalid db'\n",
            "[odoo]\nurl='https://erp.example.test'\ndb='company'\npoll_enabled='yes'\n",
            "[odoo]\nurl='https://erp.example.test'\ndb='company'\nusername=5\n",
        ] {
            let home = tempfile::tempdir().unwrap();
            let h = home.path();
            write(h, "config.toml", raw);
            write(h, "agents/absent/agent.toml", "[agent]\nname='Absent'\n");
            assert!(!run(h).await.unwrap(), "{raw}");
            assert_eq!(read(h), raw);
            let config = OdooAccessConfig::parse_config_toml(&read(h)).unwrap();
            assert!(config.company_key_agents.is_empty());
            assert!(config.company_key_agents_migrated.is_empty());
            assert!(!config.upgrade_notice_pending);
            assert!(!h.join("security_audit.jsonl").exists());
            write(h, "config.toml", COMPANY);
            assert!(run(h).await.unwrap());
            let config = OdooAccessConfig::parse_config_toml(&read(h)).unwrap();
            assert_eq!(config.company_key_agents, vec!["absent"]);
            assert_eq!(config.company_key_agents_migrated, MARKER);
            assert!(h.join("security_audit.jsonl").is_file());
        }
    }

    #[tokio::test]
    async fn missing_company_and_invalid_access_are_untouched() {
        for raw in ["", "[odoo]\nurl='https://erp.test'\n", "[odoo_access]\nattachment_max_mb=0\n", "[odoo_access]\nunknown=true\n"] {
            let home = tempfile::tempdir().unwrap();
            write(home.path(), "config.toml", raw);
            let _ = run(home.path()).await;
            assert_eq!(read(home.path()), raw);
        }
        let home = tempfile::tempdir().unwrap();
        let raw = format!("{COMPANY}[odoo_access]\nattachment_max_mb=26\n");
        write(home.path(), "config.toml", &raw);
        assert!(run(home.path()).await.is_err());
        assert_eq!(read(home.path()), raw);
    }

    #[tokio::test]
    async fn preserves_explicit_settings_and_never_adds_permission_mode() {
        for mode in ["", "permission_mode='engine'\n", "permission_mode='follow_odoo_account'\npermission_mode_ack={by='admin',at='2026-11-27T00:00:00Z'}\n"] {
            let home = tempfile::tempdir().unwrap();
            let raw = format!("{COMPANY}[member_api_keys]\nallow_conversation_content=true\n[odoo_access]\n{mode}");
            write(home.path(), "config.toml", &raw);
            let before: toml::Table = raw.parse().unwrap();
            assert!(run(home.path()).await.unwrap());
            let after: toml::Table = read(home.path()).parse().unwrap();
            for key in ["permission_mode", "permission_mode_ack"] {
                assert_eq!(before["odoo_access"].get(key), after["odoo_access"].get(key));
            }
            assert_eq!(after["member_api_keys"]["allow_conversation_content"].as_bool(), Some(true));
        }
    }

    #[tokio::test]
    async fn audit_failure_and_invalid_employee_abort_without_marker() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), "config.toml", COMPANY);
        std::fs::create_dir(home.path().join("security_audit.jsonl")).unwrap();
        assert!(run(home.path()).await.is_err());
        assert_eq!(read(home.path()), COMPANY);
        let home = tempfile::tempdir().unwrap();
        write(home.path(), "config.toml", COMPANY);
        write(home.path(), "agents/a/agent.toml", "[odoo]\nusername=5\n");
        assert!(run(home.path()).await.is_err());
        assert_eq!(read(home.path()), COMPANY);
    }
}
