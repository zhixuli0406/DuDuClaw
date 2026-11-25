//! Intent CAS and per-proposal process locks. No external adapter owns a DB writer.
use super::*;
use std::fs::{File, OpenOptions};

pub(super) fn schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS migration_effect_intents (
        id BLOB NOT NULL, revision INTEGER NOT NULL, digest BLOB NOT NULL,
        action INTEGER NOT NULL, token BLOB NOT NULL, PRIMARY KEY(id, revision));")?;
    Ok(())
}

/// A commit lock, separate from the operation lock that serializes remote
/// effects. Seat writers acquire admission BEFORE calling `run`. Lock files
/// are never unlinked; process death releases both locks. In-memory stores use
/// the caller's exclusive `&mut ConfirmationStore` instead.
pub struct ProposalLock { path: Option<std::path::PathBuf> }
impl ProposalLock {
    pub(super) fn new(conn: &Connection, binding: &ProposalBinding) -> Self {
        let path = conn.path().filter(|p| !p.is_empty()).map(|path| {
            let mut suffix = String::from("proposal-");
            for byte in binding.operation_id.proposal_id.0 {
                use std::fmt::Write;
                write!(&mut suffix, "{byte:02x}").expect("write to string");
            }
            suffix.push_str(".lock");
            std::path::Path::new(path).with_extension(suffix)
        });
        Self { path }
    }
    pub fn run<T>(&self, run: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.acquire()?;
        run()
    }
    pub(super) fn acquire(&self) -> Result<Option<File>> {
        let Some(path) = &self.path else { return Ok(None); };
        lock_path(path)
    }
    pub(super) fn operation(&self) -> Result<Option<File>> {
        let Some(path) = &self.path else { return Ok(None); };
        // Nonblocking exclusion only: no seat writer waits on this lock. Keep
        // it across remote calls so apply and compensation cannot interleave.
        lock_path(&path.with_extension("operation.lock"))
    }
}

fn lock_path(path: &std::path::Path) -> Result<Option<File>> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|_| ConfirmationError::Storage)?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| ConfirmationError::Storage)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { return Err(ConfirmationError::Storage); }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata().map_err(|_| ConfirmationError::Storage)?;
        if opened.nlink() != 1 || opened.ino() != metadata.ino() || opened.dev() != metadata.dev()
            || opened.mode() & 0o077 != 0 { return Err(ConfirmationError::Storage); }
    }
    file.try_lock().map_err(|_| ConfirmationError::Stale)?;
    Ok(Some(file))
}

pub(super) fn intent(conn: &Connection, item: &ConfirmationItem, action: i64) -> Result<[u8; 16]> {
    let token = rand::random::<[u8; 16]>();
    let binding = &item.binding;
    let count = conn.execute("INSERT INTO migration_effect_intents VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(id,revision) DO UPDATE SET action=excluded.action,token=excluded.token
        WHERE digest=excluded.digest",
        params![binding.operation_id.proposal_id.0.as_slice(), binding.operation_id.revision,
            binding.proposal_digest.0.as_slice(), action, token.as_slice()])?;
    if count != 1 { return Err(ConfirmationError::Stale); }
    audit(conn, item, "effect_intent", None)?;
    Ok(token)
}

pub(super) fn consume(conn: &Connection, item: &ConfirmationItem, action: i64, token: &[u8; 16]) -> Result<()> {
    let binding = &item.binding;
    if required(conn, binding)?.status != item.status { return Err(ConfirmationError::Stale); }
    let count = conn.execute("DELETE FROM migration_effect_intents
        WHERE id=?1 AND revision=?2 AND digest=?3 AND action=?4 AND token=?5",
        params![binding.operation_id.proposal_id.0.as_slice(), binding.operation_id.revision,
            binding.proposal_digest.0.as_slice(), action, token.as_slice()])?;
    if count != 1 { return Err(ConfirmationError::Stale); }
    Ok(())
}
