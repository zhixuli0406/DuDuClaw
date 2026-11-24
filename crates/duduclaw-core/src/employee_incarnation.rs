//! Gateway-owned employee nonces. Never derive authority from filesystem times.
use std::{io, path::Path};
use rusqlite::{Connection, OptionalExtension, params};
use crate::private_sqlite::{self, Access};

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS google_employee_incarnations (
        agent_id TEXT PRIMARY KEY NOT NULL, nonce TEXT NOT NULL);")
}

pub fn read(conn: &Connection, agent: &str) -> rusqlite::Result<Option<String>> {
    if revoked(conn, agent)? { return Ok(None); }
    conn.query_row("SELECT nonce FROM google_employee_incarnations WHERE agent_id=?1",
        [agent], |r| r.get(0)).optional()
}

/// Only employee creation or an authorized first attestation may call this.
pub fn ensure(conn: &Connection, agent: &str) -> rusqlite::Result<String> {
    migrate(conn)?;
    if revoked(conn, agent)? { return Err(rusqlite::Error::InvalidQuery); }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    conn.execute("INSERT OR IGNORE INTO google_employee_incarnations(agent_id,nonce) VALUES (?1,?2)",
        params![agent, nonce])?;
    read(conn, agent)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

pub fn retire(conn: &Connection, agent: &str) -> rusqlite::Result<()> {
    migrate(conn)?;
    conn.execute("DELETE FROM google_employee_incarnations WHERE agent_id=?1", [agent])?;
    Ok(())
}

// A separate durable tombstone keeps a failed SQLite rotation revoked even
// after the database becomes readable again or the gateway restarts.
fn tombstone(home: &Path, agent: &str) -> std::path::PathBuf {
    let key: String = agent.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    home.join(".employee-nonce-revocations").join(key)
}

fn revoked(conn: &Connection, agent: &str) -> rusqlite::Result<bool> {
    let Some(path) = conn.path().filter(|p| !p.is_empty()) else { return Ok(false); };
    let Some(home) = Path::new(path).parent() else { return Err(rusqlite::Error::InvalidQuery); };
    for path in [tombstone(home, agent), home.join(".employee-nonce-revocations/all")] {
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {},
            Err(_) => return Err(rusqlite::Error::InvalidQuery),
        }
    }
    Ok(false)
}

fn invalidate(home: &Path, agent: Option<&str>) -> io::Result<std::path::PathBuf> {
    let path = agent.map(|a| tombstone(home, a))
        .unwrap_or_else(|| home.join(".employee-nonce-revocations/all"));
    let parent = path.parent().unwrap();
    let mut directory = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    match directory.create(parent) {
        Ok(()) => {},
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {},
        Err(e) => return Err(e),
    }
    if !std::fs::symlink_metadata(parent)?.file_type().is_dir() {
        return Err(io::Error::other("nonce revocation directory is not a directory"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(&path) {
        Ok(file) => file.sync_all()?,
        // Any existing marker already denies use, including a planted link.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {},
        Err(e) => return Err(e),
    }
    #[cfg(unix)]
    {
        std::fs::File::open(parent)?.sync_all()?;
        std::fs::File::open(home)?.sync_all()?;
    }
    Ok(path)
}

fn update(home: &Path, agent: &str, create: bool) -> io::Result<()> {
    // Never manufacture an incomplete auth database in a CLI-only home.
    match std::fs::symlink_metadata(home.join("users.db")) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        _ => {},
    }
    let marker = tombstone(home, agent);
    let mut retired = false;
    let result = (|| -> io::Result<()> {
        let Some(conn) = private_sqlite::open(home, "users.db", Access::ReadWrite)
            .map_err(|e| io::Error::other(e.to_string()))? else { return Ok(()); };
        migrate(&conn).map_err(io::Error::other)?;
        // Commit retirement separately: a failed insertion cannot roll back
        // to the previous employee's nonce.
        retire(&conn, agent).map_err(io::Error::other)?;
        retired = true;
        if create {
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            conn.execute("INSERT INTO google_employee_incarnations(agent_id,nonce) VALUES (?1,?2)",
                params![agent, nonce]).map_err(io::Error::other)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            // A leftover tombstone is a safe refusal, not an org-write error.
            if let Err(error) = std::fs::remove_file(marker) {
                if error.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(%agent, %error, "employee nonce remains unavailable");
                }
            }
        },
        Err(error) => {
            if let Err(invalidation) = invalidate(home, Some(agent)) {
                // No old nonce remains after a committed retirement. If both
                // independent stores failed before retirement, refuse rather
                // than publish a replacement carrying the previous authority.
                if !retired { return Err(invalidation); }
                tracing::warn!(%agent, %invalidation, "nonce retired but revocation marker unavailable");
            }
            tracing::warn!(%agent, %error, "employee nonce update failed; nonce invalidated");
        },
    }
    Ok(())
}

/// Best-effort rotation; no auth database is created. Database failures leave
/// a durable tombstone, so neither attestation nor use can reuse the old nonce.
pub fn created(home: &Path, agent: &str) -> io::Result<()> {
    update(home, agent, true)
}

pub fn deleted(home: &Path, agent: &str) -> io::Result<()> {
    update(home, agent, false)
}

/// Scan persisted labels without making registry availability depend on SQLite.
pub fn reconcile(home: &Path) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
        let Some(conn) = private_sqlite::open(home, "users.db", Access::ReadWrite)
            .map_err(|e| io::Error::other(e.to_string()))? else { return Ok(()); };
        migrate(&conn).map_err(io::Error::other)?;
        let labels = {
            let mut stmt = conn.prepare("SELECT agent_id FROM google_employee_incarnations").map_err(io::Error::other)?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(io::Error::other)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(io::Error::other)?
        };
        let global = home.join(".employee-nonce-revocations/all");
        let invalidate_all = match std::fs::symlink_metadata(&global) {
            Ok(_) => true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        };
        for agent in labels {
            let valid = !agent.is_empty() && !agent.contains(['/', '\\']) && agent != "." && agent != "..";
            let present = valid && std::fs::symlink_metadata(home.join("agents").join(&agent))
                .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
            if invalidate_all || !present { deleted(home, &agent)?; }
        }
        // Recovery retires every pre-error row before lifting the global
        // refusal; failed individual retirements retain their own tombstones.
        if invalidate_all { std::fs::remove_file(global)?; }
        Ok(())
    })();
    if let Err(error) = result {
        invalidate(home, None)?;
        tracing::warn!(%error, "employee nonce reconciliation failed; delegation refused");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_without_users_database_does_not_create_it() {
        let home = tempfile::tempdir().unwrap();
        created(home.path(), "bot").unwrap();
        deleted(home.path(), "bot").unwrap();
        reconcile(home.path()).unwrap();
        assert!(!home.path().join("users.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_scan_is_best_effort_and_recovery_retires_pre_error_nonces() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        let conn = private_sqlite::open(home.path(), "users.db", Access::Create).unwrap().unwrap();
        created(home.path(), "bot").unwrap();
        let old = read(&conn, "bot").unwrap().unwrap();
        let path = home.path().join("users.db");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        reconcile(home.path()).unwrap();
        assert!(read(&conn, "bot").unwrap().is_none());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        reconcile(home.path()).unwrap();
        assert!(read(&conn, "bot").unwrap().is_none());
        assert_ne!(ensure(&conn, "bot").unwrap(), old);
    }

    #[test]
    fn nonce_schema_is_independent_and_failed_rotation_never_restores_old_nonce() {
        let home = tempfile::tempdir().unwrap();
        let conn = private_sqlite::open(home.path(), "users.db", Access::Create).unwrap().unwrap();
        created(home.path(), "bot").unwrap();
        let old = read(&conn, "bot").unwrap().unwrap();
        assert!(conn.prepare("SELECT * FROM auth_meta").is_err());
        conn.execute_batch("CREATE TRIGGER refuse_nonce_insert BEFORE INSERT ON google_employee_incarnations
            BEGIN SELECT RAISE(FAIL, 'injected insertion failure'); END;").unwrap();
        created(home.path(), "bot").unwrap();
        assert!(read(&conn, "bot").unwrap().is_none());
        assert!(ensure(&conn, "bot").is_err());
        conn.execute_batch("DROP TRIGGER refuse_nonce_insert").unwrap();
        created(home.path(), "bot").unwrap();
        assert_ne!(read(&conn, "bot").unwrap().unwrap(), old);
        conn.execute_batch("CREATE TRIGGER refuse_nonce_delete BEFORE DELETE ON google_employee_incarnations
            BEGIN SELECT RAISE(FAIL, 'injected deletion failure'); END;").unwrap();
        deleted(home.path(), "bot").unwrap();
        drop(conn);
        let conn = private_sqlite::open(home.path(), "users.db", Access::ReadOnly).unwrap().unwrap();
        assert!(read(&conn, "bot").unwrap().is_none(), "revoked across reopen despite retained SQL row");
    }
}
