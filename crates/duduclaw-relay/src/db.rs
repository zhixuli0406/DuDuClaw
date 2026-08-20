//! SQLite-backed device registry (WAL mode).
//!
//! A single small table because its two concerns always change together for
//! a given device: (1) the registered Ed25519 public key used for
//! WebSocket challenge-response auth, and (2) the device's last-known
//! network position (self-reported LAN IP + the public IP observed on its
//! WebSocket connection), used only to answer `/v1/find`.
//!
//! Uses a small pool of blocking `rusqlite::Connection`s behind
//! `std::sync::Mutex` (same shape as `duduclaw-auth::db::UserDb`) — every
//! query here is a handful of single-row reads/writes, fast enough to run
//! synchronously inside an async handler without a dedicated blocking-task
//! pool.

use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::RelayError;

const POOL_SIZE: usize = 4;

pub struct RelayDb {
    pool: Vec<Mutex<Connection>>,
}

/// Outcome of a registration attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// A brand-new `device_id` was created.
    Created,
    /// `device_id` already existed with the *same* public key — treated as
    /// a successful, idempotent re-registration (name is updated if given).
    AlreadyRegistered,
    /// `device_id` already existed with a *different* public key — refused.
    /// Prevents an attacker from squatting/hijacking someone else's
    /// device_id by re-registering it with a key they control.
    Conflict,
}

/// Minimal device info surfaced on the `/v1/find` page. Deliberately
/// excludes the public key and `device_id` — nothing that could be misused
/// if the page were shared or scraped.
#[derive(Debug, Clone)]
pub struct DeviceSummary {
    pub name: String,
    pub lan_ip: String,
}

impl RelayDb {
    pub fn open(path: &Path) -> Result<Self, RelayError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| RelayError::Internal(format!("create db dir: {e}")))?;
            }
        }
        let mut pool = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let conn = Connection::open(path)
                .map_err(|e| RelayError::Internal(format!("open db: {e}")))?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
            )
            .map_err(|e| RelayError::Internal(format!("set pragmas: {e}")))?;
            pool.push(Mutex::new(conn));
        }
        let db = Self { pool };
        db.init_schema()?;
        Ok(db)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        for m in &self.pool {
            if let Ok(guard) = m.try_lock() {
                return guard;
            }
        }
        // Fallback: block on the first slot. Recover from a poisoned lock
        // instead of panicking — one panicked guard must not turn into a
        // full outage of the device registry.
        self.pool[0].lock().unwrap_or_else(|e| e.into_inner())
    }

    fn init_schema(&self) -> Result<(), RelayError> {
        let conn = self.conn();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS devices (
                device_id TEXT PRIMARY KEY,
                pubkey_b64 TEXT NOT NULL,
                name TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                last_seen_at TEXT,
                last_lan_ip TEXT,
                last_public_ip TEXT,
                online INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_devices_public_ip ON devices(last_public_ip, online);",
        )
        .map_err(|e| RelayError::Internal(format!("init schema: {e}")))?;
        Ok(())
    }

    /// Register (or idempotently re-register) a device's public key.
    pub fn register_device(
        &self,
        device_id: &str,
        pubkey_b64: &str,
        name: Option<&str>,
    ) -> Result<RegisterOutcome, RelayError> {
        let conn = self.conn();
        let existing: Option<String> = conn
            .query_row(
                "SELECT pubkey_b64 FROM devices WHERE device_id = ?1",
                params![device_id],
                |row| row.get(0),
            )
            .optional()?;

        match existing {
            None => {
                conn.execute(
                    "INSERT INTO devices (device_id, pubkey_b64, name, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![device_id, pubkey_b64, name.unwrap_or(""), Utc::now().to_rfc3339()],
                )?;
                Ok(RegisterOutcome::Created)
            }
            Some(existing_key) if existing_key == pubkey_b64 => {
                if let Some(n) = name {
                    conn.execute(
                        "UPDATE devices SET name = ?1 WHERE device_id = ?2",
                        params![n, device_id],
                    )?;
                }
                Ok(RegisterOutcome::AlreadyRegistered)
            }
            Some(_) => Ok(RegisterOutcome::Conflict),
        }
    }

    pub fn device_exists(&self, device_id: &str) -> Result<bool, RelayError> {
        let conn = self.conn();
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM devices WHERE device_id = ?1",
                params![device_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    pub fn get_pubkey(&self, device_id: &str) -> Result<Option<String>, RelayError> {
        let conn = self.conn();
        let key: Option<String> = conn
            .query_row(
                "SELECT pubkey_b64 FROM devices WHERE device_id = ?1",
                params![device_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(key)
    }

    pub fn mark_online(&self, device_id: &str, public_ip: Option<&str>) -> Result<(), RelayError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE devices SET online = 1, last_seen_at = ?1, last_public_ip = COALESCE(?2, last_public_ip) WHERE device_id = ?3",
            params![Utc::now().to_rfc3339(), public_ip, device_id],
        )?;
        Ok(())
    }

    pub fn mark_offline(&self, device_id: &str) -> Result<(), RelayError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE devices SET online = 0, last_seen_at = ?1 WHERE device_id = ?2",
            params![Utc::now().to_rfc3339(), device_id],
        )?;
        Ok(())
    }

    pub fn update_lan_ip(&self, device_id: &str, lan_ip: &str) -> Result<(), RelayError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE devices SET last_lan_ip = ?1, last_seen_at = ?2 WHERE device_id = ?3",
            params![lan_ip, Utc::now().to_rfc3339(), device_id],
        )?;
        Ok(())
    }

    /// Devices currently online whose last-observed public IP matches
    /// `public_ip`, ordered by name. Backing query for `/v1/find`.
    pub fn devices_by_public_ip(&self, public_ip: &str) -> Result<Vec<DeviceSummary>, RelayError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT name, last_lan_ip FROM devices \
             WHERE last_public_ip = ?1 AND online = 1 AND last_lan_ip IS NOT NULL \
             ORDER BY name",
        )?;
        let rows = stmt
            .query_map(params![public_ip], |row| {
                Ok(DeviceSummary {
                    name: row.get(0)?,
                    lan_ip: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> (RelayDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = RelayDb::open(&dir.path().join("relay.db")).unwrap();
        (db, dir)
    }

    #[test]
    fn register_new_device_creates_row() {
        let (db, _dir) = open_temp();
        let outcome = db.register_device("dev-1", "pk1", Some("盒子")).unwrap();
        assert_eq!(outcome, RegisterOutcome::Created);
        assert!(db.device_exists("dev-1").unwrap());
        assert_eq!(db.get_pubkey("dev-1").unwrap().as_deref(), Some("pk1"));
    }

    #[test]
    fn reregister_same_key_is_idempotent() {
        let (db, _dir) = open_temp();
        db.register_device("dev-1", "pk1", Some("A")).unwrap();
        let outcome = db.register_device("dev-1", "pk1", Some("B")).unwrap();
        assert_eq!(outcome, RegisterOutcome::AlreadyRegistered);
    }

    #[test]
    fn reregister_different_key_is_conflict() {
        let (db, _dir) = open_temp();
        db.register_device("dev-1", "pk1", None).unwrap();
        let outcome = db.register_device("dev-1", "pk2", None).unwrap();
        assert_eq!(outcome, RegisterOutcome::Conflict);
        // The original key must be unchanged.
        assert_eq!(db.get_pubkey("dev-1").unwrap().as_deref(), Some("pk1"));
    }

    #[test]
    fn unknown_device_lookups_return_none_not_error() {
        let (db, _dir) = open_temp();
        assert!(!db.device_exists("ghost").unwrap());
        assert_eq!(db.get_pubkey("ghost").unwrap(), None);
    }

    #[test]
    fn online_offline_and_lan_ip_roundtrip() {
        let (db, _dir) = open_temp();
        db.register_device("dev-1", "pk1", None).unwrap();
        db.mark_online("dev-1", Some("203.0.113.5")).unwrap();
        db.update_lan_ip("dev-1", "192.168.1.9").unwrap();

        let found = db.devices_by_public_ip("203.0.113.5").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lan_ip, "192.168.1.9");

        db.mark_offline("dev-1").unwrap();
        let found_after_offline = db.devices_by_public_ip("203.0.113.5").unwrap();
        assert!(found_after_offline.is_empty());
    }

    #[test]
    fn devices_by_public_ip_excludes_other_ips() {
        let (db, _dir) = open_temp();
        db.register_device("dev-1", "pk1", Some("A")).unwrap();
        db.mark_online("dev-1", Some("203.0.113.5")).unwrap();
        db.update_lan_ip("dev-1", "192.168.1.9").unwrap();

        db.register_device("dev-2", "pk2", Some("B")).unwrap();
        db.mark_online("dev-2", Some("198.51.100.1")).unwrap();
        db.update_lan_ip("dev-2", "192.168.1.10").unwrap();

        let found = db.devices_by_public_ip("203.0.113.5").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "A");
    }

    #[test]
    fn mark_online_preserves_previous_public_ip_when_none_given() {
        let (db, _dir) = open_temp();
        db.register_device("dev-1", "pk1", None).unwrap();
        db.mark_online("dev-1", Some("203.0.113.5")).unwrap();
        db.mark_online("dev-1", None).unwrap();
        db.update_lan_ip("dev-1", "192.168.1.9").unwrap();
        let found = db.devices_by_public_ip("203.0.113.5").unwrap();
        assert_eq!(found.len(), 1);
    }
}
