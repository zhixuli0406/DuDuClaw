//! Durable invalidation before product policy writes, including A -> B -> A.
//! The existing users.db owns the epoch. No database or schema is created here.
use std::{io, path::Path, time::Duration};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

pub const POLICY_EPOCH_KEY: &str = "isolation_policy_epoch";

fn unavailable(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("isolation policy fence unavailable: {error}"))
}

/// Durably invalidate existing run authority before changing a policy source.
/// A failed later file write may invalidate extra runs, but must never revive
/// old authority. Call under the policy writer's own lock, before publication.
/// A home without users.db cannot have a database-backed live capability yet.
pub fn fence_change(home: &Path) -> io::Result<()> {
    let path = home.join("users.db");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_file() || metadata.permissions().readonly() => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "isolation authority database is not writable"));
        }
        Ok(_) => {}
    }
    // SQLite NOFOLLOW also rejects symlinked ancestors (macOS /var is one).
    // Resolve the trusted home, never the leaf database that must stay nofollow.
    let path = std::fs::canonicalize(home)?.join("users.db");
    let mut connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map_err(unavailable)?;
    connection.busy_timeout(Duration::from_secs(2)).map_err(unavailable)?;
    connection.pragma_update(None, "synchronous", "FULL").map_err(unavailable)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
    let changed = tx.execute("UPDATE auth_meta SET value=value+1 WHERE key=?1
        AND typeof(value)='integer' AND value>0 AND value<9223372036854775807", [POLICY_EPOCH_KEY])
        .map_err(unavailable)?;
    if changed != 1 { return Err(unavailable("missing, corrupt or exhausted policy epoch")); }
    tx.commit().map_err(unavailable)
}

#[cfg(test)]
#[path = "isolation_policy_tests.rs"]
mod tests;
