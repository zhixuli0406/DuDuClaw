//! VaultStore — encrypted, TTL-aware, per-agent-keyed mapping store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use duduclaw_security::crypto::CryptoEngine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{RedactionError, Result};
use crate::rules::RestoreScope;
use crate::vault::{key, schema};

/// One row in the vault, decrypted.
#[derive(Debug, Clone)]
pub struct VaultEntry {
    pub token: String,
    pub original: Option<String>, // None if expired (we keep the row but mask the cleartext)
    pub category: String,
    pub rule_id: String,
    pub agent_id: String,
    pub session_id: Option<String>,
    pub restore_scope: RestoreScope,
    pub cross_session: bool,
    pub created_at: i64,
    pub expires_at: i64,
    pub reveal_count: i64,
    pub last_reveal_at: Option<i64>,
    pub expired_marker: bool,
}

/// Stats for dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VaultStats {
    pub total: i64,
    pub active: i64,
    pub expired: i64,
    pub by_category: Vec<(String, i64)>,
}

/// Encrypted mapping store. One process holds a single [`VaultStore`]
/// instance (shared via `Arc`); the inner `Connection` is serialised
/// behind a `Mutex` for write safety. WAL mode is enabled at open.
pub struct VaultStore {
    conn: Mutex<Connection>,
    key_dir: PathBuf,
    engines: Mutex<HashMap<String, CryptoEngine>>,
}

impl VaultStore {
    /// Open a vault DB file. Creates and migrates if missing. `key_dir`
    /// is where per-agent keys live.
    pub fn open<P: AsRef<Path>>(db_path: P, key_dir: P) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let key_dir = key_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&key_dir)?;

        let conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        schema::migrate(&conn)?;

        Ok(VaultStore {
            conn: Mutex::new(conn),
            key_dir,
            engines: Mutex::new(HashMap::new()),
        })
    }

    /// In-memory vault for tests.
    #[cfg(test)]
    pub fn in_memory(key_dir: PathBuf) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::migrate(&conn)?;
        std::fs::create_dir_all(&key_dir)?;
        Ok(VaultStore {
            conn: Mutex::new(conn),
            key_dir,
            engines: Mutex::new(HashMap::new()),
        })
    }

    fn with_engine<R>(&self, agent_id: &str, f: impl FnOnce(&CryptoEngine) -> Result<R>) -> Result<R> {
        // Hot path: try to read existing engine.
        {
            let engines = self.engines.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
            if let Some(eng) = engines.get(agent_id) {
                return f(eng);
            }
        }
        // Slow path: load key, create engine, insert.
        let key_bytes = key::load_or_generate(agent_id, &self.key_dir)?;
        let engine = CryptoEngine::new(&key_bytes)
            .map_err(|e| RedactionError::crypto(format!("CryptoEngine init failed: {e}")))?;
        let mut engines = self.engines.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
        let entry = engines.entry(agent_id.to_string()).or_insert(engine);
        f(entry)
    }

    /// Insert a new (token → original) mapping. Returns `Err` and writes
    /// nothing on any failure — pipeline relies on this for fail-closed.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_mapping(
        &self,
        token: &str,
        original: &str,
        agent_id: &str,
        session_id: Option<&str>,
        category: &str,
        rule_id: &str,
        restore_scope: &RestoreScope,
        cross_session: bool,
        ttl_hours: i64,
    ) -> Result<()> {
        let encrypted = self.with_engine(agent_id, |eng| {
            eng.encrypt(original.as_bytes())
                .map_err(|e| RedactionError::crypto(format!("encrypt failed: {e}")))
        })?;

        let now = Utc::now().timestamp();
        let expires_at = now + ttl_hours * 3600;
        let scope_wire = serde_json::to_string(restore_scope)?;

        let conn = self.conn.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO redaction_mappings
                (token, original_enc, category, rule_id, agent_id, session_id,
                 restore_scope, cross_session, created_at, expires_at,
                 reveal_count, last_reveal_at, expired_marker)
             VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, NULL, 0)",
            params![
                token,
                encrypted,
                category,
                rule_id,
                agent_id,
                session_id,
                scope_wire,
                cross_session as i64,
                now,
                expires_at,
            ],
        )?;
        Ok(())
    }

    /// Look up a token. Returns `None` if not found OR if the token exists
    /// but the `(agent_id, session_id)` doesn't match (unless the entry
    /// is `cross_session`).
    ///
    /// On expired entries, returns `Some(VaultEntry { original: None, expired_marker: true, .. })`
    /// — the caller can render a "[expired PII]" placeholder.
    pub fn lookup_mapping(
        &self,
        token: &str,
        agent_id: &str,
        session_id: Option<&str>,
    ) -> Result<Option<VaultEntry>> {
        let row_opt = {
            let conn = self.conn.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
            let mut stmt = conn.prepare(
                "SELECT token, original_enc, category, rule_id, agent_id, session_id,
                        restore_scope, cross_session, created_at, expires_at,
                        reveal_count, last_reveal_at, expired_marker
                 FROM redaction_mappings WHERE token = ?1",
            )?;

            stmt.query_row(params![token], |row| {
                Ok(RawRow {
                    token: row.get(0)?,
                    original_enc: row.get(1)?,
                    category: row.get(2)?,
                    rule_id: row.get(3)?,
                    agent_id: row.get(4)?,
                    session_id: row.get(5)?,
                    restore_scope: row.get(6)?,
                    cross_session: row.get::<_, i64>(7)? != 0,
                    created_at: row.get(8)?,
                    expires_at: row.get(9)?,
                    reveal_count: row.get(10)?,
                    last_reveal_at: row.get(11)?,
                    expired_marker: row.get::<_, i64>(12)? != 0,
                })
            })
            .optional()?
        };

        let Some(row) = row_opt else { return Ok(None) };

        // Cross-session check.
        if row.agent_id != agent_id {
            return Ok(None);
        }
        if !row.cross_session && row.session_id.as_deref() != session_id {
            return Ok(None);
        }

        let restore_scope: RestoreScope = serde_json::from_str(&row.restore_scope)?;

        // Expired entries are returned with `original = None`.
        let now = Utc::now().timestamp();
        let is_expired = row.expired_marker || row.expires_at <= now;
        let original = if is_expired {
            None
        } else {
            let decrypted = self.with_engine(agent_id, |eng| {
                eng.decrypt(&row.original_enc)
                    .map_err(|e| RedactionError::crypto(format!("decrypt failed: {e}")))
            })?;
            Some(String::from_utf8(decrypted)
                .map_err(|e| RedactionError::crypto(format!("invalid utf-8: {e}")))?)
        };

        Ok(Some(VaultEntry {
            token: row.token,
            original,
            category: row.category,
            rule_id: row.rule_id,
            agent_id: row.agent_id,
            session_id: row.session_id,
            restore_scope,
            cross_session: row.cross_session,
            created_at: row.created_at,
            expires_at: row.expires_at,
            reveal_count: row.reveal_count,
            last_reveal_at: row.last_reveal_at,
            expired_marker: is_expired,
        }))
    }

    /// Bump the reveal counter — called after a successful restore.
    pub fn record_reveal(&self, token: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        let conn = self.conn.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
        conn.execute(
            "UPDATE redaction_mappings
                SET reveal_count = reveal_count + 1, last_reveal_at = ?1
              WHERE token = ?2",
            params![now, token],
        )?;
        Ok(())
    }

    /// Mark all entries whose TTL has passed as `expired_marker = 1`.
    /// Returns the number of rows updated. Idempotent.
    pub fn mark_expired(&self) -> Result<usize> {
        let now = Utc::now().timestamp();
        let conn = self.conn.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
        let n = conn.execute(
            "UPDATE redaction_mappings
                SET expired_marker = 1
              WHERE expired_marker = 0 AND expires_at <= ?1",
            params![now],
        )?;
        Ok(n)
    }

    /// Permanently delete entries that have been expired for at least
    /// `after_days` days. Returns the number deleted.
    pub fn purge_expired(&self, after_days: u32) -> Result<usize> {
        let cutoff = Utc::now().timestamp() - (after_days as i64) * 86_400;
        let conn = self.conn.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
        let n = conn.execute(
            "DELETE FROM redaction_mappings WHERE expired_marker = 1 AND expires_at <= ?1",
            params![cutoff],
        )?;
        Ok(n)
    }

    /// Aggregate stats for the dashboard.
    pub fn stats(&self) -> Result<VaultStats> {
        let now = Utc::now().timestamp();
        let conn = self.conn.lock().map_err(|e| RedactionError::vault(e.to_string()))?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM redaction_mappings",
            [],
            |row| row.get(0),
        )?;
        let expired: i64 = conn.query_row(
            "SELECT COUNT(*) FROM redaction_mappings WHERE expired_marker = 1 OR expires_at <= ?1",
            params![now],
            |row| row.get(0),
        )?;
        let active = total - expired;

        let mut by_cat = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT category, COUNT(*) FROM redaction_mappings GROUP BY category",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            by_cat.push((row.get::<_, String>(0)?, row.get::<_, i64>(1)?));
        }
        Ok(VaultStats { total, active, expired, by_category: by_cat })
    }
}

struct RawRow {
    token: String,
    original_enc: Vec<u8>,
    category: String,
    rule_id: String,
    agent_id: String,
    session_id: Option<String>,
    restore_scope: String,
    cross_session: bool,
    created_at: i64,
    expires_at: i64,
    reveal_count: i64,
    last_reveal_at: Option<i64>,
    expired_marker: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_store() -> (VaultStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = VaultStore::in_memory(tmp.path().to_path_buf()).unwrap();
        (store, tmp)
    }

    #[test]
    fn round_trip_insert_lookup() {
        let (store, _tmp) = fresh_store();
        store
            .insert_mapping(
                "<REDACT:EMAIL:abcdef01>",
                "alice@acme.com",
                "agnes",
                Some("s1"),
                "EMAIL",
                "email_rule",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();

        let entry = store
            .lookup_mapping("<REDACT:EMAIL:abcdef01>", "agnes", Some("s1"))
            .unwrap()
            .unwrap();
        assert_eq!(entry.original.as_deref(), Some("alice@acme.com"));
        assert_eq!(entry.category, "EMAIL");
        assert!(!entry.expired_marker);
    }

    #[test]
    fn lookup_misses_across_sessions() {
        let (store, _tmp) = fresh_store();
        store
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "alice@a.com",
                "agnes",
                Some("session-A"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();

        let entry = store
            .lookup_mapping("<REDACT:E:abcdef01>", "agnes", Some("session-B"))
            .unwrap();
        assert!(entry.is_none(), "cross-session lookup must miss by default");
    }

    #[test]
    fn cross_session_flag_allows_lookup() {
        let (store, _tmp) = fresh_store();
        store
            .insert_mapping(
                "<REDACT:CODE:abcdef01>",
                "Project Falcon",
                "agnes",
                Some("session-A"),
                "CODE",
                "r",
                &RestoreScope::Owner,
                true,
                24,
            )
            .unwrap();

        let entry = store
            .lookup_mapping("<REDACT:CODE:abcdef01>", "agnes", Some("session-B"))
            .unwrap()
            .unwrap();
        assert_eq!(entry.original.as_deref(), Some("Project Falcon"));
    }

    #[test]
    fn lookup_misses_across_agents() {
        let (store, _tmp) = fresh_store();
        store
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "x",
                "agnes",
                Some("s1"),
                "E",
                "r",
                &RestoreScope::Owner,
                true,
                24,
            )
            .unwrap();
        let entry = store
            .lookup_mapping("<REDACT:E:abcdef01>", "bobby", Some("s1"))
            .unwrap();
        assert!(entry.is_none());
    }

    #[test]
    fn mark_expired_then_lookup_returns_expired() {
        let (store, _tmp) = fresh_store();
        // TTL 0 → already expired by the time mark_expired runs.
        store
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "x",
                "agnes",
                Some("s1"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                0,
            )
            .unwrap();

        // Sleep a hair so `expires_at <= now` is unambiguous.
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let n = store.mark_expired().unwrap();
        assert!(n >= 1);

        let entry = store
            .lookup_mapping("<REDACT:E:abcdef01>", "agnes", Some("s1"))
            .unwrap()
            .unwrap();
        assert!(entry.expired_marker);
        assert!(entry.original.is_none());
    }

    #[test]
    fn purge_removes_old_expired_entries() {
        let (store, _tmp) = fresh_store();
        store
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "x",
                "agnes",
                Some("s1"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                0,
            )
            .unwrap();
        // Hand-set expires_at to 60 days ago.
        {
            let conn = store.conn.lock().unwrap();
            let sixty_days_ago = Utc::now().timestamp() - 60 * 86_400;
            conn.execute(
                "UPDATE redaction_mappings SET expires_at = ?1, expired_marker = 1",
                params![sixty_days_ago],
            )
            .unwrap();
        }

        let deleted = store.purge_expired(30).unwrap();
        assert_eq!(deleted, 1);

        let entry = store
            .lookup_mapping("<REDACT:E:abcdef01>", "agnes", Some("s1"))
            .unwrap();
        assert!(entry.is_none());
    }

    #[test]
    fn stats_aggregates_correctly() {
        let (store, _tmp) = fresh_store();
        for (tok, cat) in [
            ("<REDACT:E:11111111>", "EMAIL"),
            ("<REDACT:E:22222222>", "EMAIL"),
            ("<REDACT:P:33333333>", "PHONE"),
        ] {
            store
                .insert_mapping(
                    tok,
                    "x",
                    "agnes",
                    Some("s1"),
                    cat,
                    "r",
                    &RestoreScope::Owner,
                    false,
                    24,
                )
                .unwrap();
        }
        let s = store.stats().unwrap();
        assert_eq!(s.total, 3);
        assert_eq!(s.active, 3);
        assert_eq!(s.expired, 0);
        let email_count = s
            .by_category
            .iter()
            .find(|(c, _)| c == "EMAIL")
            .map(|(_, n)| *n);
        assert_eq!(email_count, Some(2));
    }

    #[test]
    fn record_reveal_bumps_counter() {
        let (store, _tmp) = fresh_store();
        store
            .insert_mapping(
                "<REDACT:E:abcdef01>",
                "x",
                "agnes",
                Some("s1"),
                "E",
                "r",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();
        store.record_reveal("<REDACT:E:abcdef01>").unwrap();
        store.record_reveal("<REDACT:E:abcdef01>").unwrap();

        let e = store
            .lookup_mapping("<REDACT:E:abcdef01>", "agnes", Some("s1"))
            .unwrap()
            .unwrap();
        assert_eq!(e.reveal_count, 2);
        assert!(e.last_reveal_at.is_some());
    }

    #[test]
    fn ciphertext_does_not_contain_plaintext() {
        let (store, _tmp) = fresh_store();
        let secret = "SUPER_SECRET_TOKEN_xyz123";
        store
            .insert_mapping(
                "<REDACT:S:abcdef01>",
                secret,
                "agnes",
                Some("s1"),
                "S",
                "r",
                &RestoreScope::Owner,
                false,
                24,
            )
            .unwrap();
        let conn = store.conn.lock().unwrap();
        let blob: Vec<u8> = conn
            .query_row(
                "SELECT original_enc FROM redaction_mappings WHERE token = ?1",
                params!["<REDACT:S:abcdef01>"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!blob.windows(secret.len()).any(|w| w == secret.as_bytes()));
    }
}
