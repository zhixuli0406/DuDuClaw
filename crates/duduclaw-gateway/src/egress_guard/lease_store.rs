//! Write-ahead sink fences. Only the gateway opens this database.
use std::path::{Path, PathBuf};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use super::{MAX_SINK_LEASES, MAX_TOTAL_SINK_LEASES};

pub(super) struct Store {
    conn: Connection,
    // Drop the connection before releasing exclusive ledger ownership.
    _owner: std::fs::File,
}

pub(super) struct Row {
    pub agent: String,
    pub turn: String,
    pub id: String,
    pub acquired_at: i64,
}

impl Store {
    pub fn open(home: &Path) -> Result<Self, String> {
        // Canonicalize the trusted home (macOS /var is a symlink), but never
        // follow a database/sidecar symlink. Match ApprovalStore's private files.
        let home = std::fs::canonicalize(home).map_err(|e| e.to_string())?;
        // A second gateway may still serve routes after losing gateway.lock.
        // SQLite transactions alone cannot keep two in-memory ledgers coherent.
        // Hold this lock for the connection's lifetime, including idle periods.
        let owner = owner_lock(&home.join("egress_leases.lock"))?;
        let path = home.join("egress_leases.db");
        private_files(&path)?;
        let conn = Connection::open_with_flags(&path, OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(1)).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;").map_err(|e| e.to_string())?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !(0..=1).contains(&version) { return Err("unsupported egress lease schema".into()); }
        if version == 0 {
            conn.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS sink_leases (
                    lease_id TEXT PRIMARY KEY,
                    agent_id TEXT NOT NULL,
                    turn_id TEXT NOT NULL,
                    phase TEXT NOT NULL CHECK(phase IN ('NotDispatched', 'Dispatched')),
                    acquired_at INTEGER NOT NULL CHECK(acquired_at >= 0)
                );
                CREATE INDEX IF NOT EXISTS sink_leases_agent ON sink_leases(agent_id);
                PRAGMA user_version=1;
                COMMIT;").map_err(|e| e.to_string())?;
        }
        private_files(&path)?;
        Ok(Self { conn, _owner: owner })
    }

    pub fn restore(&mut self) -> Result<Vec<Row>, String> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        let mut per_agent = std::collections::HashMap::new();
        {
            let mut stmt = tx.prepare("SELECT agent_id, turn_id, lease_id, acquired_at, phase FROM sink_leases").map_err(|e| e.to_string())?;
            let mut cursor = stmt.query([]).map_err(|e| e.to_string())?;
            while let Some(row) = cursor.next().map_err(|e| e.to_string())? {
                let entry = Row {
                    agent: row.get(0).map_err(|e| e.to_string())?,
                    turn: row.get(1).map_err(|e| e.to_string())?,
                    id: row.get(2).map_err(|e| e.to_string())?,
                    acquired_at: row.get(3).map_err(|e| e.to_string())?,
                };
                let phase: String = row.get(4).map_err(|e| e.to_string())?;
                if !duduclaw_core::is_valid_agent_id(&entry.agent)
                    || !(entry.turn.is_empty() || crate::computer_use_sessions::turns::valid_turn_id(&entry.turn))
                    || entry.id.is_empty() || entry.id.len() > 128 || entry.acquired_at < 0
                    || !matches!(phase.as_str(), "NotDispatched" | "Dispatched") {
                    return Err("invalid egress lease row".into());
                }
                let count = per_agent.entry(entry.agent.clone()).or_insert(0usize);
                *count += 1;
                if *count > MAX_SINK_LEASES { return Err("egress agent lease store over capacity".into()); }
                rows.push(entry);
                if rows.len() > MAX_TOTAL_SINK_LEASES { return Err("egress lease store over capacity".into()); }
            }
        }
        // Wall clocks cannot prove that a sink has stopped. Every restored row
        // gets a fresh monotonic fence in the ledger, even after a forward jump.
        tx.execute("UPDATE sink_leases SET phase = 'Dispatched'", [])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(rows)
    }

    pub fn insert(&mut self, agent: &str, turn: &str, id: &str, wall: i64) -> Result<(), String> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let agent_count: i64 = tx.query_row("SELECT COUNT(*) FROM sink_leases WHERE agent_id=?1", [agent], |r| r.get(0)).map_err(|e| e.to_string())?;
        let total: i64 = tx.query_row("SELECT COUNT(*) FROM sink_leases", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if agent_count >= MAX_SINK_LEASES as i64 || total >= MAX_TOTAL_SINK_LEASES as i64 {
            return Err("egress lease store at capacity".into());
        }
        // Granting a lease authorizes an immediate, possibly partial write in
        // another process. Arm the durable phase BEFORE replying; waiting for a
        // dispatched notification would create a second crash window. The local
        // transport still distinguishes NotDispatched for safe cancellation.
        tx.execute("INSERT INTO sink_leases (lease_id, agent_id, turn_id, phase, acquired_at)
            VALUES (?1, ?2, ?3, 'Dispatched', ?4)", params![id, agent, turn, wall]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn delete_batch(&mut self, leases: &[(String, super::Key)]) -> Result<(), String> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        {
            let mut stmt = tx.prepare("DELETE FROM sink_leases WHERE lease_id=?1 AND agent_id=?2 AND turn_id=?3").map_err(|e| e.to_string())?;
            for (agent, (turn, id)) in leases {
                stmt.execute(params![id, agent, turn]).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn delete(&self, agent: &str, turn: &str, id: &str) -> Result<(), String> {
        self.conn.execute("DELETE FROM sink_leases WHERE lease_id=?1 AND agent_id=?2 AND turn_id=?3", params![id, agent, turn])
            .map(|_| ()).map_err(|e| e.to_string())
    }
}

fn owner_lock(path: &Path) -> Result<std::fs::File, String> {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_file() || meta.file_type().is_symlink()) {
        return Err("egress lease lock is not a regular file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if file.metadata().map_err(|e| e.to_string())?.nlink() != 1 {
            return Err("egress lease lock hard link refused".into());
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    fs2::FileExt::try_lock_exclusive(&file).map_err(|e| format!("egress lease owner unavailable: {e}"))?;
    // Never unlink this file: another owner must lock the same inode.
    Ok(file)
}

fn private_files(path: &Path) -> Result<(), String> {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let file = PathBuf::from(name);
        if let Ok(meta) = std::fs::symlink_metadata(&file) {
            if !meta.is_file() || meta.file_type().is_symlink() { return Err("egress lease file is not a regular file".into()); }
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                if meta.nlink() != 1 { return Err("egress lease hard link refused".into()); }
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
            }
        } else if suffix.is_empty() {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            match options.open(&file) {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn egress_lease_files_are_private_under_non_utf8_home() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(std::ffi::OsString::from_vec(vec![b'h', 0xff]));
        if let Err(error) = std::fs::create_dir(&home) {
            if error.raw_os_error() == Some(libc::EILSEQ)
                || error.kind() == std::io::ErrorKind::InvalidFilename {
                eprintln!("skipping non-UTF-8 home test: filesystem rejects the filename: {error}");
                return;
            }
            panic!("creating non-UTF-8 test home: {error}");
        }
        let mut store = Store::open(&home).unwrap();
        store.insert("bot", "T", "lease", chrono::Utc::now().timestamp_millis()).unwrap();
        for name in ["egress_leases.db", "egress_leases.db-wal", "egress_leases.db-shm", "egress_leases.lock"] {
            assert_eq!(std::fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn egress_restore_counts_old_rows_against_agent_and_global_caps() {
        for (count, group_size, expected) in [
            (MAX_SINK_LEASES + 1, MAX_SINK_LEASES + 1, "egress agent lease store over capacity"),
            (MAX_TOTAL_SINK_LEASES + 1, MAX_SINK_LEASES, "egress lease store over capacity"),
        ] {
            let home = tempfile::tempdir().unwrap();
            let mut store = Store::open(home.path()).unwrap();
            // Simulate an over-capacity existing store with ancient timestamps.
            store.conn.execute(
                "WITH RECURSIVE numbers(n) AS (SELECT 0 UNION ALL SELECT n+1 FROM numbers WHERE n+1 < ?1)
                 INSERT INTO sink_leases SELECT CAST(n AS TEXT), 'bot-' || (n / ?2), 'T', 'NotDispatched', 0 FROM numbers",
                params![count as i64, group_size as i64],
            ).unwrap();
            assert_eq!(store.restore().err().unwrap(), expected);
            assert_eq!(store.conn.query_row("SELECT COUNT(*) FROM sink_leases", [], |r| r.get::<_, usize>(0)).unwrap(), count);
            assert!(store.insert("bot-0", "T", "overflow", 0).is_err());
        }
    }

    #[test]
    fn egress_owner_lock_refuses_links_without_changing_target() {
        for symlink in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let target = home.path().join("unrelated-file");
            std::fs::write(&target, "keep").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
            let lock = home.path().join("egress_leases.lock");
            if symlink { std::os::unix::fs::symlink(&target, &lock).unwrap(); }
            else { std::fs::hard_link(&target, &lock).unwrap(); }
            assert!(Store::open(home.path()).is_err());
            assert!(!home.path().join("egress_leases.db").exists());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
            assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o640);
        }
    }
}
