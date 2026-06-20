//! Vault SQLite schema + migration.

use rusqlite::Connection;

use crate::error::Result;

/// Current schema version. Bump and add a migration branch in
/// [`migrate`] whenever the table layout changes.
///
/// v2: the primary key became the composite `(token, agent_id, session_id)`
/// so that distinct `(agent, session)` entries can never clobber one another
/// via `INSERT OR REPLACE` — even if two values produce a colliding token
/// hash. `session_id` is `NOT NULL DEFAULT ''` because SQLite treats NULLs
/// as distinct in a PRIMARY KEY (which would allow duplicate cross-session
/// rows); the store normalises `None` ↔ `''` at its boundary.
pub const SCHEMA_VERSION: i32 = 2;

const INIT_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS redaction_mappings (
    token            TEXT NOT NULL,
    original_enc     BLOB NOT NULL,
    category         TEXT NOT NULL,
    rule_id          TEXT NOT NULL,
    agent_id         TEXT NOT NULL,
    session_id       TEXT NOT NULL DEFAULT '',
    restore_scope    TEXT NOT NULL,
    cross_session    INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    reveal_count     INTEGER NOT NULL DEFAULT 0,
    last_reveal_at   INTEGER,
    expired_marker   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (token, agent_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_session ON redaction_mappings(agent_id, session_id);
CREATE INDEX IF NOT EXISTS idx_token ON redaction_mappings(token);
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

/// Run all pending migrations.
///
/// The v1 schema used `token` alone as the primary key, so two distinct
/// `(agent, session)` entries that produced a colliding token hash could
/// clobber each other via `INSERT OR REPLACE`. v2 widens the primary key to
/// `(token, agent_id, session_id)`. Because `CREATE TABLE IF NOT EXISTS`
/// cannot alter an existing table's primary key, and there is no row-level
/// migration framework here, an existing v1 table is rebuilt: vault rows are
/// ephemeral, TTL-bounded token↔PII mappings, so dropping and recreating the
/// table (losing in-flight mappings, which would simply be re-created on the
/// next redaction) is the safe, simple choice.
pub fn migrate(conn: &Connection) -> Result<()> {
    let existing = table_exists(conn, "redaction_mappings")?;
    let version = if existing { current_version(conn)? } else { 0 };

    // Pre-v2 table with the legacy single-column primary key: rebuild it.
    if existing && version < 2 {
        conn.execute_batch(
            "DROP TABLE IF EXISTS redaction_mappings;
             DELETE FROM vault_meta WHERE key = 'schema_version';",
        )?;
    }

    init_schema(conn)?;
    let _ = current_version(conn)?;
    // Future migrations: branch on current version.
    Ok(())
}

/// Whether a table of the given name exists in the connection.
fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
        rusqlite::params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
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

    #[test]
    fn migrate_rebuilds_legacy_v1_table() {
        // Simulate a pre-v2 DB: single-column `token` primary key + v1 meta.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE redaction_mappings (
                token TEXT PRIMARY KEY,
                original_enc BLOB NOT NULL,
                category TEXT NOT NULL,
                rule_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT,
                restore_scope TEXT NOT NULL,
                cross_session INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                reveal_count INTEGER NOT NULL DEFAULT 0,
                last_reveal_at INTEGER,
                expired_marker INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE vault_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO vault_meta(key, value) VALUES ('schema_version', '1');
            "#,
        )
        .unwrap();

        migrate(&conn).unwrap();

        // Version is now v2 and the primary key is the composite form.
        assert_eq!(current_version(&conn).unwrap(), 2);
        let pk_cols: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('redaction_mappings') WHERE pk > 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pk_cols, 3, "composite PK must span 3 columns");
    }
}
