//! Intent CAS and per-proposal process locks. No external adapter owns a DB writer.
use super::*;
use std::fs::{File, OpenOptions};

pub(super) fn schema(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS migration_effect_intents (
        id BLOB NOT NULL, revision INTEGER NOT NULL, digest BLOB NOT NULL,
        action INTEGER NOT NULL, token BLOB NOT NULL, PRIMARY KEY(id, revision));")?;
    Ok(())
}

/// Locks live outside SQLite and are never unlinked (unlinking an acquired lock
/// would permit two owners). A crash releases the kernel lock automatically.
/// In-memory stores cannot be reopened by another process and use &mut exclusivity.
pub(super) fn lock(conn: &Connection, binding: &ProposalBinding) -> Result<Option<File>> {
    let Some(path) = conn.path().filter(|p| !p.is_empty()) else { return Ok(None); };
    let mut suffix = String::from("proposal-");
    for byte in binding.operation_id.proposal_id.0 {
        use std::fmt::Write;
        write!(&mut suffix, "{byte:02x}").map_err(|_| ConfirmationError::Storage)?;
    }
    suffix.push_str(".lock");
    let path = std::path::Path::new(path).with_extension(suffix);
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
