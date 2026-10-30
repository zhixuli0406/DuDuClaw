//! Offline operator recovery. Never use OPERATOR_CLI_KINDS here: its dashboard
//! decision requires a running gateway, while recovery requires it stopped.
use std::path::{Path, PathBuf};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use super::lease_store::{private_files, Store};

#[derive(Debug, Serialize)]
pub struct LeaseInfo {
    pub lease_id: String,
    pub agent_id: String,
    pub turn_id: String,
    pub phase: String,
    pub acquired_at: i64,
}

#[derive(Debug, Serialize)]
pub struct LeaseList {
    pub leases: Vec<LeaseInfo>,
    pub confirm: String,
    pub lease_tokens: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct ClearResult {
    pub deleted: usize,
    pub backup: PathBuf,
}

fn token(ids: impl IntoIterator<Item = String>) -> String {
    let mut ids: Vec<_> = ids.into_iter().collect();
    ids.sort();
    let mut hash = Sha256::new();
    for id in ids {
        hash.update((id.len() as u64).to_be_bytes());
        hash.update(id.as_bytes());
    }
    hex::encode(hash.finalize()).chars().take(16).collect()
}

fn rows(conn: &Connection) -> Result<LeaseList, String> {
    let mut stmt = conn.prepare("SELECT lease_id, agent_id, turn_id, phase, acquired_at FROM sink_leases ORDER BY lease_id")
        .map_err(|e| e.to_string())?;
    let leases = stmt.query_map([], |row| Ok(LeaseInfo {
        lease_id: row.get(0)?, agent_id: row.get(1)?, turn_id: row.get(2)?,
        phase: row.get(3)?, acquired_at: row.get(4)?,
    })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let confirm = token(leases.iter().map(|row| row.lease_id.clone()));
    let lease_tokens = leases.iter().map(|row| (row.lease_id.clone(), token([row.lease_id.clone()]))).collect();
    Ok(LeaseList { leases, confirm, lease_tokens })
}

fn gateway_stopped(home: &Path) -> Result<Option<std::fs::File>, String> {
    let path = duduclaw_core::gateway_instance::lock_path(home);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot inspect gateway lock: {e}")),
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Err("invalid gateway lock".into()),
        Ok(_) => (),
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC); }
    let file = options.open(path).map_err(|e| format!("cannot open gateway lock: {e}"))?;
    #[cfg(unix)]
    { use std::os::unix::fs::MetadataExt;
      if file.metadata().map_err(|e| e.to_string())?.nlink() != 1 { return Err("gateway lock hard link refused".into()); } }
    if duduclaw_core::gateway_instance::held(home) || duduclaw_core::gateway_instance::locked_elsewhere(home) {
        return Err("stop the gateway before recovering egress leases".into());
    }
    fs2::FileExt::try_lock_exclusive(&file).map_err(|e| format!("gateway lock unavailable: {e}"))?;
    Ok(Some(file))
}

pub fn list(home: &Path) -> Result<LeaseList, String> {
    let _gateway = gateway_stopped(home)?;
    let store = Store::open(home)?;
    rows(&store.conn)
}

pub fn clear(home: &Path, lease: Option<&str>, confirm: &str) -> Result<ClearResult, String> {
    let _gateway = gateway_stopped(home)?;
    let mut store = Store::open(home)?;
    let current = rows(&store.conn)?;
    let ids: Vec<String> = current.leases.iter().filter(|row| lease.is_none_or(|id| row.lease_id == id))
        .map(|row| row.lease_id.clone()).collect();
    if ids.is_empty() { return Err("no matching egress leases".into()); }
    // The exclusive lease owner lock covers checkpoint, backup and deletion.
    let (busy, _, _): (i64, i64, i64) = store.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(|e| e.to_string())?;
    if busy != 0 { return Err("cannot checkpoint egress lease backup".into()); }
    let backup = home.join(format!("egress_leases.backup-{}-{}.db", chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ"), uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC); }
    let mut dest = options.open(&backup).map_err(|e| e.to_string())?;
    private_files(&home.join("egress_leases.db"))?;
    let mut source_options = std::fs::OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; source_options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC); }
    let mut source = source_options.open(home.join("egress_leases.db")).map_err(|e| e.to_string())?;
    std::io::copy(&mut source, &mut dest).map_err(|e| e.to_string())?;
    dest.sync_all().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(home).and_then(|dir| dir.sync_all()).map_err(|e| e.to_string())?;
    let backup_conn = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW).map_err(|e| e.to_string())?;
    if rows(&backup_conn)?.confirm != current.confirm { return Err("egress lease backup does not match current leases".into()); }
    drop(backup_conn);
    if confirm != token(ids.clone()) { return Err(format!("stale confirmation token; backup: {}", backup.display())); }
    let tx = store.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    if rows(&tx)?.confirm != current.confirm { return Err("egress leases changed after backup; nothing deleted".into()); }
    for id in &ids { tx.execute("DELETE FROM sink_leases WHERE lease_id=?1", [id]).map_err(|e| e.to_string())?; }
    tx.commit().map_err(|e| e.to_string())?;
    let event = duduclaw_security::audit::AuditEvent::new("egress_leases_cleared", "", duduclaw_security::audit::Severity::Warning,
        serde_json::json!({"actor": "本機指令列（身分未驗證）", "lease_ids": ids, "backup": backup, "deleted": ids.len()}));
    duduclaw_security::audit::try_append_audit_event(home, &event)
        .map_err(|e| format!("leases cleared and backed up, but security audit failed: {e}"))?;
    Ok(ClearResult { deleted: ids.len(), backup })
}

/// Read-only and available while the gateway owns the lease lock.
pub fn health(home: &Path) -> Result<(usize, Option<i64>), String> {
    let path = home.join("egress_leases.db");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, None)),
        Err(e) => return Err(e.to_string()),
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Err("invalid egress lease database".into()),
        Ok(_) => (),
    }
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match std::fs::symlink_metadata(PathBuf::from(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !suffix.is_empty() => (),
            Err(e) => return Err(e.to_string()),
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() { return Err("invalid egress lease file".into()); }
                #[cfg(unix)]
                { use std::os::unix::fs::MetadataExt;
                  if meta.nlink() != 1 { return Err("egress lease hard link refused".into()); } }
            }
        }
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW).map_err(|e| e.to_string())?;
    let (count, oldest): (usize, Option<i64>) = conn.query_row("SELECT COUNT(*), MIN(acquired_at) FROM sink_leases", [], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|e| e.to_string())?;
    Ok((count, oldest.map(|at| chrono::Utc::now().timestamp_millis().saturating_sub(at).max(0) / 1000)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(home: &Path) {
        let mut store = Store::open(home).unwrap();
        store.insert("bot", "T", "lease-a", 10).unwrap();
    }

    #[test]
    fn held_lease_lock_refuses_recovery() {
        let home = tempfile::tempdir().unwrap();
        let _owner = Store::open(home.path()).unwrap();
        assert!(list(home.path()).is_err());
        assert!(clear(home.path(), None, "anything").is_err());
    }

    #[test]
    fn stale_token_keeps_rows_and_makes_backup_before_comparison() {
        let home = tempfile::tempdir().unwrap();
        seeded(home.path());
        let first = list(home.path()).unwrap();
        {
            let mut store = Store::open(home.path()).unwrap();
            store.insert("bot", "T", "lease-b", 20).unwrap();
        }
        assert!(clear(home.path(), None, &first.confirm).unwrap_err().contains("stale"));
        assert_eq!(list(home.path()).unwrap().leases.len(), 2);
        let backups: Vec<_> = std::fs::read_dir(home.path()).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.file_name().unwrap().to_string_lossy().starts_with("egress_leases.backup-")).collect();
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(&backups[0]).unwrap();
        assert_eq!(rows(&backup).unwrap().leases.len(), 2);
    }

    #[test]
    fn successful_clear_preserves_backup_and_writes_security_audit() {
        let home = tempfile::tempdir().unwrap();
        seeded(home.path());
        let current = list(home.path()).unwrap();
        let result = clear(home.path(), Some("lease-a"), &current.lease_tokens["lease-a"]).unwrap();
        assert_eq!(result.deleted, 1);
        assert_eq!(rows(&Connection::open(&result.backup).unwrap()).unwrap().leases.len(), 1);
        assert!(list(home.path()).unwrap().leases.is_empty());
        let events = duduclaw_security::audit::read_recent_events(home.path(), 10);
        assert!(events.iter().any(|event| event.event_type == "egress_leases_cleared" && event.details["deleted"] == 1));
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt;
          assert_eq!(std::fs::metadata(result.backup).unwrap().permissions().mode() & 0o777, 0o600); }
    }

    #[test]
    fn token_is_order_independent_and_unambiguous() {
        assert_eq!(token(["a".into(), "b".into()]), token(["b".into(), "a".into()]));
        assert_ne!(token(["ab".into(), "c".into()]), token(["a".into(), "bc".into()]));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_symlinks_and_hard_links() {
        for name in ["egress_leases.db", "egress_leases.lock", "locks/gateway.lock"] {
            for hard in [false, true] {
                let home = tempfile::tempdir().unwrap();
                std::fs::create_dir(home.path().join("locks")).unwrap();
                let target = home.path().join("target");
                std::fs::write(&target, "keep").unwrap();
                if hard { std::fs::hard_link(&target, home.path().join(name)).unwrap(); }
                else { std::os::unix::fs::symlink(&target, home.path().join(name)).unwrap(); }
                assert!(list(home.path()).is_err(), "{name}");
                assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
            }
        }
    }

    #[test]
    fn held_gateway_lock_refuses_recovery() {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        assert!(list(home.path()).is_err());
    }
}
