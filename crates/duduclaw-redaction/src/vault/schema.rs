//! Vault SQLite schema + migration.

use rusqlite::Connection;

use crate::error::Result;

/// Current schema version. Bump and add a migration branch in
/// [`migrate`] whenever the table layout changes.
pub const SCHEMA_VERSION: i32 = 1;

const INIT_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS redaction_mappings (
    token            TEXT PRIMARY KEY,
    original_enc     BLOB NOT NULL,
    category         TEXT NOT NULL,
    rule_id          TEXT NOT NULL,
    agent_id         TEXT NOT NULL,
    session_id       TEXT,
    restore_scope    TEXT NOT NULL,
    cross_session    INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    reveal_count     INTEGER NOT NULL DEFAULT 0,
    last_reveal_at   INTEGER,
    expired_marker   INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_session ON redaction_mappings(agent_id, session_id);
CREATE INDEX IF NOT EXISTS idx_expires ON redaction_mappings(expires_at);
CREATE INDEX IF NOT EXISTS idx_category ON redaction_mappings(category);

CREATE TABLE IF NOT EXISTS vault_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Initialise (or upgrade) a vault connection.
pub fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(INIT_SQL)?;
    let now = chrono::Utc::now().timestamp().to_string();
    let version = SCHEMA_VERSION.to_string();
    conn.execute(
        "INSERT OR IGNORE INTO vault_meta(key, value) VALUES (?1, ?2)",
        rusqlite::params!["schema_version", &version],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO vault_meta(key, value) VALUES (?1, ?2)",
        rusqlite::params!["created_at", &now],
    )?;
    Ok(())
}

/// Read the current schema version from the meta table. Returns 0 for a
/// freshly created file with no meta rows.
pub fn current_version(conn: &Connection) -> Result<i32> {
    let v: rusqlite::Result<String> = conn.query_row(
        "SELECT value FROM vault_meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    );
    match v {
        Ok(s) => Ok(s.parse().unwrap_or(0)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

/// Run all pending migrations. `init_schema` is idempotent
/// (`CREATE ... IF NOT EXISTS`) so we always run it first; that way a
/// freshly created DB (no `vault_meta` table) doesn't trip the version
/// query.
pub fn migrate(conn: &Connection) -> Result<()> {
    init_schema(conn)?;
    let _ = current_version(conn)?;
    // Future migrations: branch on current version.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();

        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('redaction_mappings','vault_meta')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cnt, 2);
    }

    #[test]
    fn version_meta_is_set() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }
}
