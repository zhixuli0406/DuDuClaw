//! Fresh metadata recheck after a trusted host attests credential possession.
//! A digest alone never authenticates an HTTP request.

use chrono::{DateTime, Utc};
use duduclaw_core::google_identity::{CredentialKind, CredentialRef, native_google_scope};
use sha2::Digest;
use std::path::Path;

const REFUSED: &str = "Google 專用金鑰已失效、權限不足，或不屬於這位 AI 員工。";

pub(crate) fn authorize(
    home: &Path,
    reference: &CredentialRef,
    agent: &str,
    tool: &str,
) -> Result<(), &'static str> {
    let required = native_google_scope(tool).ok_or(REFUSED)?.grant();
    if !reference.is_well_formed()
        || !duduclaw_core::is_valid_agent_id(agent)
        || agent == crate::mcp_internal_key::INTERNAL_CLIENT_ID
        || !home.join("agents").join(agent).join("agent.toml").is_file()
    {
        return Err(REFUSED);
    }
    match reference.kind {
        CredentialKind::Legacy => legacy(home, reference, agent, required),
        CredentialKind::Refresh => refresh(home, reference, agent, required),
    }
}

fn legacy(
    home: &Path,
    reference: &CredentialRef,
    agent: &str,
    required: &str,
) -> Result<(), &'static str> {
    let path = home.join("config.toml");
    if std::fs::metadata(&path).map_err(|_| REFUSED)?.len() > 4 * 1024 * 1024 {
        return Err(REFUSED);
    }
    let raw = std::fs::read_to_string(path).map_err(|_| REFUSED)?;
    let doc: toml::Value = toml::from_str(&raw).map_err(|_| REFUSED)?;
    let keys = doc
        .get("mcp_keys")
        .and_then(toml::Value::as_table)
        .ok_or(REFUSED)?;
    let (key, value) = keys
        .iter()
        .find(|(key, _)| format!("{:x}", sha2::Sha256::digest(key.as_bytes())) == reference.sha256)
        .ok_or(REFUSED)?;
    let suffix = ["ddc_prod_", "ddc_staging_", "ddc_dev_"]
        .iter()
        .find_map(|prefix| key.strip_prefix(prefix))
        .ok_or(REFUSED)?;
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(REFUSED);
    }
    let table = value.as_table().ok_or(REFUSED)?;
    if table.get("client_id").and_then(toml::Value::as_str) != Some(agent) {
        return Err(REFUSED);
    }
    if let Some(external) = table.get("is_external") {
        external.as_bool().ok_or(REFUSED)?;
    }
    let created = table
        .get("created_at")
        .and_then(toml::Value::as_str)
        .ok_or(REFUSED)?;
    let created = DateTime::parse_from_rfc3339(created).map_err(|_| REFUSED)?;
    if Utc::now().signed_duration_since(created).num_days() > 30 {
        return Err(REFUSED);
    }
    let scopes = table
        .get("scopes")
        .and_then(toml::Value::as_array)
        .ok_or(REFUSED)?;
    if scopes.iter().any(|scope| scope.as_str().is_none())
        || !scopes.iter().any(|scope| scope.as_str() == Some(required))
    {
        return Err(REFUSED);
    }
    Ok(())
}

fn refresh(
    home: &Path,
    reference: &CredentialRef,
    agent: &str,
    required: &str,
) -> Result<(), &'static str> {
    use rusqlite::{Connection, OpenFlags};
    let db =
        Connection::open_with_flags(home.join("mcp_tokens.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| REFUSED)?;
    db.busy_timeout(std::time::Duration::from_millis(500))
        .map_err(|_| REFUSED)?;
    // SQL NULL is mandatory. An invalid, non-NULL revocation timestamp can
    // never be mistaken for an absent revocation after timestamp parsing.
    let (client, scopes, external, issued, expires): (String, String, i64, i64, i64) = db.query_row(
        "SELECT client_id, scopes, is_external, issued_at, expires_at FROM refresh_tokens WHERE token_hash=?1 AND revoked_at IS NULL",
        [&reference.sha256], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).map_err(|_| REFUSED)?;
    if client != agent
        || ![0, 1].contains(&external)
        || DateTime::<Utc>::from_timestamp(issued, 0).is_none()
        || DateTime::<Utc>::from_timestamp(expires, 0).is_none()
        || expires <= issued
        || Utc::now().timestamp() >= expires
        || !scopes.split(',').any(|scope| scope.trim() == required)
    {
        return Err(REFUSED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
        std::fs::write(
            home.path().join("agents/employee-a/agent.toml"),
            "name='employee-a'\n",
        )
        .unwrap();
        home
    }
    #[test]
    fn legacy_rechecks_scope_expiry_owner_and_removal_without_admin_substitution() {
        let home = home();
        let key = "ddc_dev_0123456789abcdef0123456789abcdef";
        let reference = CredentialRef {
            kind: CredentialKind::Legacy,
            sha256: format!("{:x}", sha2::Sha256::digest(key.as_bytes())),
        };
        let write = |agent: &str, scopes: &str, age: i64| {
            std::fs::write(home.path().join("config.toml"), format!(
            "[mcp_keys.{key}]\nclient_id='{agent}'\nscopes=[{scopes}]\nis_external=true\ncreated_at='{}'\n",
            (Utc::now() - chrono::Duration::days(age)).to_rfc3339())).unwrap()
        };
        write("employee-a", "'google:read'", 0);
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_ok());
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_create_draft").is_err());
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search_extra").is_err());
        write("employee-a", "'admin'", 0);
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
        write("employee-b", "'google:read'", 0);
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
        write("employee-a", "'google:read'", 31);
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
        std::fs::write(home.path().join("config.toml"), "[mcp_keys]\n").unwrap();
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
    }
    #[test]
    fn refresh_rechecks_live_sql_and_rejects_every_non_null_revocation() {
        let home = home();
        let reference = CredentialRef {
            kind: CredentialKind::Refresh,
            sha256: "a".repeat(64),
        };
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
        assert!(!home.path().join("mcp_tokens.db").exists());
        let db = rusqlite::Connection::open(home.path().join("mcp_tokens.db")).unwrap();
        db.execute_batch("CREATE TABLE refresh_tokens(token_hash TEXT,client_id TEXT,scopes TEXT,is_external INTEGER,issued_at INTEGER,expires_at INTEGER,revoked_at INTEGER);").unwrap();
        let now = Utc::now().timestamp();
        db.execute(
            "INSERT INTO refresh_tokens VALUES(?1,'employee-a','google:read',1,?2,?3,NULL)",
            rusqlite::params![reference.sha256, now, now + 3600],
        )
        .unwrap();
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_ok());
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_create_draft").is_err());
        for revoked in [0_i64, now, i64::MAX] {
            db.execute("UPDATE refresh_tokens SET revoked_at=?1", [revoked])
                .unwrap();
            assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
        }
        db.execute_batch("UPDATE refresh_tokens SET revoked_at='malformed';")
            .unwrap();
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
        db.execute(
            "UPDATE refresh_tokens SET revoked_at=NULL,expires_at=?1",
            [now - 1],
        )
        .unwrap();
        assert!(authorize(home.path(), &reference, "employee-a", "gmail_search").is_err());
    }
}
