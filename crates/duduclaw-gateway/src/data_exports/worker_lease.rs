//! Cross-process worker lifetime. Cancellation does not release CPU/IO quota.
use super::*;
use fs2::FileExt;

pub(super) fn is_lock_contention(error: &std::io::Error) -> bool {
    // Windows reports ERROR_LOCK_VIOLATION, which need not be WouldBlock.
    // Only fs2's known OS error is contention; unknown failures stay closed.
    error
        .raw_os_error()
        .is_some_and(|code| Some(code) == fs2::lock_contended_error().raw_os_error())
}

#[derive(Debug)]
pub(super) struct WorkerLease {
    pub id: String,
    home: PathBuf,
    _file: std::fs::File,
    active: bool,
}
impl WorkerLease {
    pub(super) fn activate(&mut self) {
        self.active = true;
    }
}
impl Drop for WorkerLease {
    fn drop(&mut self) {
        if self.active {
            if let Ok(conn) = store::db(&self.home) {
                let _ = conn.execute(
                    "UPDATE exports SET worker_active=0 WHERE id=?1",
                    params![self.id],
                );
            }
        }
        // The file and its OS lock live until after the durable quota release.
    }
}
pub(super) fn try_lease(home: &Path, id: &str) -> ExportResult<Option<WorkerLease>> {
    lease(home, id, true)
}
pub(super) fn try_existing_lease(home: &Path, id: &str) -> ExportResult<Option<WorkerLease>> {
    lease(home, id, false)
}
fn lease(home: &Path, id: &str, create: bool) -> ExportResult<Option<WorkerLease>> {
    if !valid_id(id) {
        return Err("EXPORT_NOT_FOUND".into());
    }
    let root = private_directory(&home.join("data_exports"))?;
    let locks = private_directory(&root.join(".locks"))?;
    let path = locks.join(id);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("EXPORT_IO".into()),
    };
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(WorkerLease {
            id: id.into(),
            home: home.into(),
            _file: file,
            active: false,
        })),
        Err(e) if is_lock_contention(&e) => Ok(None),
        Err(_) => Err("EXPORT_IO".into()),
    }
}
