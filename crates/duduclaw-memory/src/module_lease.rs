//! SQLite adapter for the core ModuleLease spike; not wired into runtime entry points.
use std::{path::{Path, PathBuf}, time::Duration};
use duduclaw_core::module_lease::{unavailable, Inflight, ModuleId, ModuleRecord, ModuleState, ModuleStore, ModuleUnavailable, StoreError, UnavailableReason, MODULE_SPECS};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};

pub struct SqliteModuleStore { path: PathBuf }
impl SqliteModuleStore {
    /// Explicit first-run bootstrap. Refuses to overwrite any existing modules.db.
    /// A partial initialization is left closed for operator recovery.
    pub fn initialize(home: &Path) -> Result<Self, StoreError> {
        let path = home.join("modules.db");
        std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| StoreError(e.to_string()))?;
        let store = Self { path };
        let mut conn = store.connect().map_err(|e| StoreError(e.to_string()))?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| StoreError(e.to_string()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| StoreError(e.to_string()))?;
        tx.execute_batch("CREATE TABLE modules(id TEXT PRIMARY KEY, state TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation >= 0), deadline INTEGER, updated_by TEXT NOT NULL, updated_at INTEGER NOT NULL);
            CREATE TABLE inflight(module TEXT NOT NULL REFERENCES modules(id), work_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind = 'work'), generation INTEGER NOT NULL CHECK(generation >= 0), pid INTEGER NOT NULL CHECK(pid > 0), heartbeat_at INTEGER NOT NULL, slot TEXT PRIMARY KEY);").map_err(|e| StoreError(e.to_string()))?;
        for spec in MODULE_SPECS {
            let row = ModuleRecord::initial(spec.id);
            let (state, deadline) = encode(row.state);
            tx.execute("INSERT INTO modules VALUES (?1, ?2, 0, ?3, 'defaults', 0)", params![spec.id.as_str(), state, deadline]).map_err(|e| StoreError(e.to_string()))?;
        }
        tx.commit().map_err(|e| StoreError(e.to_string()))?;
        Ok(store)
    }
    /// Never creates a missing database. Each operation reopens and rereads the store.
    pub fn open(home: &Path) -> Self { Self { path: home.join("modules.db") } }
    fn connect(&self) -> rusqlite::Result<Connection> {
        let conn = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }
}
fn encode(state: ModuleState) -> (&'static str, Option<i64>) {
    match state { ModuleState::Active => ("active", None), ModuleState::Draining { deadline } => ("draining", Some(deadline)), ModuleState::Disabled => ("disabled", None), ModuleState::Purging => ("purging", None), ModuleState::Purged => ("purged", None) }
}
impl ModuleStore for SqliteModuleStore {
    fn transaction(&self, id: ModuleId, action: &mut dyn FnMut(&mut ModuleRecord) -> Result<(), ModuleUnavailable>) -> Result<(), ModuleUnavailable> {
        let denied = || unavailable(id, None, UnavailableReason::StoreUnreadable);
        let mut conn = self.connect().map_err(|_| denied())?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| denied())?;
        let (state, generation, deadline, updated_by, updated_at): (String, u64, Option<i64>, String, i64) = tx.query_row(
            "SELECT state,generation,deadline,updated_by,updated_at FROM modules WHERE id=?1", [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map_err(|_| denied())?;
        let state = match (state.as_str(), deadline) {
            ("active", None) => ModuleState::Active, ("draining", Some(deadline)) => ModuleState::Draining { deadline },
            ("disabled", None) => ModuleState::Disabled, ("purging", None) => ModuleState::Purging, ("purged", None) => ModuleState::Purged,
            _ => return Err(denied()),
        };
        let mut inflight = Vec::new();
        {
            let mut stmt = tx.prepare("SELECT slot,work_id,generation,pid,heartbeat_at,kind FROM inflight WHERE module=?1").map_err(|_| denied())?;
            let rows = stmt.query_map([id.as_str()], |r| {
                let kind: String = r.get(5)?;
                if kind != "work" { return Err(rusqlite::Error::InvalidQuery); }
                Ok(Inflight { slot: r.get(0)?, work_id: r.get(1)?, generation: r.get(2)?, pid: r.get(3)?, heartbeat_at: r.get(4)? })
            }).map_err(|_| denied())?;
            for row in rows { inflight.push(row.map_err(|_| denied())?); }
        }
        let mut row = ModuleRecord { state, generation, updated_by, updated_at, inflight };
        action(&mut row)?;
        let (state, deadline) = encode(row.state);
        tx.execute("UPDATE modules SET state=?2,generation=?3,deadline=?4,updated_by=?5,updated_at=?6 WHERE id=?1",
            params![id.as_str(), state, row.generation, deadline, row.updated_by, row.updated_at]).map_err(|_| denied())?;
        tx.execute("DELETE FROM inflight WHERE module=?1", [id.as_str()]).map_err(|_| denied())?;
        for slot in row.inflight {
            tx.execute("INSERT INTO inflight(module,work_id,kind,generation,pid,heartbeat_at,slot) VALUES (?1,?2,'work',?3,?4,?5,?6)",
                params![id.as_str(), slot.work_id, slot.generation, slot.pid, slot.heartbeat_at, slot.slot]).map_err(|_| denied())?;
        }
        tx.commit().map_err(|_| denied())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use duduclaw_core::module_lease::{ModuleGate, Voice, LeaseKind};
    #[test]
    fn module_lease_sqlite_cross_handle_and_reopen() {
        let home = tempfile::tempdir().unwrap();
        let first = ModuleGate::new(Arc::new(SqliteModuleStore::initialize(home.path()).unwrap()));
        let second = ModuleGate::new(Arc::new(SqliteModuleStore::open(home.path())));
        first.transition::<Voice>(ModuleState::Active, "test").unwrap();
        let work = first.acquire::<Voice>(LeaseKind::Work { work_id: "turn".into() }).unwrap();
        let resume = second.resume::<Voice>("turn").unwrap();
        first.transition::<Voice>(ModuleState::Draining { deadline: i64::MAX }, "test").unwrap();
        assert!(matches!(second.state::<Voice>().unwrap(), ModuleState::Draining { .. }));
        assert!(second.acquire::<Voice>(LeaseKind::Rpc).is_err());
        drop(work);
        assert!(second.resume::<Voice>("turn").is_ok());
        drop(resume);
        assert!(second.resume::<Voice>("turn").is_err());
    }
    #[test]
    fn module_lease_sqlite_store_errors_deny_even_reads() {
        let home = tempfile::tempdir().unwrap();
        let gate = ModuleGate::new(Arc::new(SqliteModuleStore::open(home.path())));
        assert!(gate.acquire::<Voice>(LeaseKind::ReadAccess).is_err());
        let store = SqliteModuleStore::initialize(home.path()).unwrap();
        let conn = store.connect().unwrap();
        conn.execute("DELETE FROM modules WHERE id='voice'", []).unwrap();
        let gate = ModuleGate::new(Arc::new(store));
        assert!(matches!(gate.state::<Voice>().unwrap_err().reason, UnavailableReason::StoreUnreadable));
        drop(conn);
        std::fs::write(home.path().join("modules.db"), b"broken").unwrap();
        assert!(gate.acquire::<Voice>(LeaseKind::Cleanup).is_err());
    }
}
