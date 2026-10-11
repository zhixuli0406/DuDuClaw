//! Private storage paths, interruption recovery and content expiration.
use super::*;

pub(super) fn private_directory(path: &Path) -> ExportResult<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err("EXPORT_IO".into());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("EXPORT_IO".into()),
            }
            if std::fs::symlink_metadata(path)
                .map_err(|_| "EXPORT_IO".to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("EXPORT_IO".into());
            }
        }
        Err(_) => return Err("EXPORT_IO".into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "EXPORT_IO".to_string())?;
    }
    Ok(path.into())
}
pub(super) fn private_file(path: &Path) -> ExportResult<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|_| "EXPORT_IO".to_string())
}
pub(super) fn job_dir(home: &Path, id: &str) -> ExportResult<PathBuf> {
    if !valid_id(id) {
        return Err("EXPORT_NOT_FOUND".into());
    }
    let root = private_directory(&home.join("data_exports"))?;
    private_directory(&root.join(id))
}
/// Call once at gateway boot, before accepting requests. Never resumes stale work.
fn recover(home: &Path) -> ExportResult<()> {
    let conn = store::db(home)?;
    let mut stmt = conn
        .prepare("SELECT id FROM exports WHERE worker_active=1 OR state IN ('queued','running')")
        .map_err(|_| "EXPORT_IO".to_string())?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| "EXPORT_IO".to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "EXPORT_IO".to_string())?;
    for id in ids {
        if let Some(mut lease) = worker_lease::try_lease(home, &id)? {
            lease.activate();
            let (_, _, status, _) = store::status(&conn, &id)?;
            if matches!(status.state.as_str(), "queued" | "running") {
                store::fail(home, &id, "EXPORT_INTERRUPTED");
            }
        }
    }
    Ok(())
}
pub fn initialize(home: &Path) -> ExportResult<()> {
    sweep(home)
}
/// Run at boot and periodically. Metadata/audit survives; content does not.
pub fn sweep(home: &Path) -> ExportResult<()> {
    recover(home)?;
    let conn = store::db(home)?;
    let now = Utc::now().to_rfc3339();
    conn.execute("UPDATE exports SET state='expired',error_code='EXPORT_EXPIRED',manifest='[]' WHERE julianday(expires_at)<=julianday(?1) AND state NOT IN ('cancelled','failed','expired')",params![now]).map_err(|_|"EXPORT_IO".to_string())?;
    let mut stmt = conn
        .prepare("SELECT id FROM exports WHERE state IN ('cancelled','failed','expired')")
        .map_err(|_| "EXPORT_IO".to_string())?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| "EXPORT_IO".to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "EXPORT_IO".to_string())?;
    for id in ids {
        store::remove_outputs(home, &id)?;
    }
    // Keep only ninety days of metadata/audit. No unbounded customer-content cache.
    let cutoff = (Utc::now() - chrono::Duration::days(90)).to_rfc3339();
    conn.execute(
        "DELETE FROM export_audit WHERE timestamp<?1",
        params![cutoff],
    )
    .map_err(|_| "EXPORT_IO".to_string())?;
    let stale = {
        let mut stmt = conn.prepare("SELECT id FROM exports WHERE worker_active=0 AND julianday(expires_at)<julianday(?1) AND state IN ('cancelled','failed','expired')").map_err(|_| "EXPORT_IO".to_string())?;
        stmt.query_map(params![cutoff], |row| row.get::<_, String>(0))
            .map_err(|_| "EXPORT_IO".to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| "EXPORT_IO".to_string())?
    };
    for id in stale {
        if let Some(_lease) = worker_lease::try_existing_lease(home, &id)? {
            let removed = conn.execute("DELETE FROM exports WHERE id=?1 AND worker_active=0 AND julianday(expires_at)<julianday(?2) AND state IN ('cancelled','failed','expired')",params![id,cutoff]).map_err(|_| "EXPORT_IO".to_string())?;
            if removed == 1 {
                // No live worker can hold this inode; its durable identity is gone.
                match std::fs::remove_file(home.join("data_exports/.locks").join(&id)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err("EXPORT_IO".into()),
                }
            }
        }
    }
    Ok(())
}
