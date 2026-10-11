//! Bounds for retained files and in-memory download buffers.
use super::*;
use fs2::FileExt;
pub(super) const MAX_RETAINED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_READY_GLOBAL: u64 = 100;
const MAX_READY_PER_OWNER: u64 = 20;
pub(super) fn retained_capacity(
    conn: &rusqlite::Connection,
    owner: &str,
    new_bytes: u64,
) -> ExportResult<()> {
    let (count, bytes): (u64, u64) = conn
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(size_bytes),0) FROM exports WHERE state='ready'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| "EXPORT_IO".to_string())?;
    let own: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM exports WHERE owner=?1 AND state='ready'",
            params![owner],
            |row| row.get(0),
        )
        .map_err(|_| "EXPORT_IO".to_string())?;
    if count >= MAX_READY_GLOBAL
        || own >= MAX_READY_PER_OWNER
        || bytes.saturating_add(new_bytes) > MAX_RETAINED_BYTES
    {
        return Err("EXPORT_STORAGE_FULL".into());
    }
    Ok(())
}
/// A cross-process file lock held by the HTTP response body.
#[derive(Debug)]
pub struct DownloadPermit {
    _file: std::fs::File,
}
pub(super) fn download_permit(home: &Path) -> ExportResult<DownloadPermit> {
    let root = private_directory(&home.join("data_exports"))?;
    let locks = private_directory(&root.join(".locks"))?;
    for slot in 0..2 {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(locks.join(format!("download-{slot}")))
            .map_err(|_| "EXPORT_IO".to_string())?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(DownloadPermit { _file: file }),
            Err(e) if worker_lease::is_lock_contention(&e) => {}
            Err(_) => return Err("EXPORT_IO".into()),
        }
    }
    Err("EXPORT_BUSY".into())
}
