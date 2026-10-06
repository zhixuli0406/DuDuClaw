//! Per-file ledger of a workspace (review H3): `(path, size, sha256)` of
//! every file the gateway wrote or reconciled, in `workspace_files`.
//!
//! The manifest hash and the hashes `list` reports come from here, so no
//! operation re-reads and re-hashes the whole tree. A file whose on-disk
//! size no longer matches its ledger row (changed outside the gateway) is
//! hashed again on its own, capped at [`super::files::MAX_FILE_BYTES`].
//! An empty `sha256` means "hash unknown": reconciliation could not read
//! the file (review M-4); `list` reports it as such.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use super::store::{StoreError, WorkspaceStore, db};

/// One ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Manifest hash over the ledger rows of `id`, in path order: for each
/// row the path length, the path and its sha256. No rows → sha256 of
/// nothing ([`super::store::EMPTY_MANIFEST`]).
pub(crate) fn manifest_hash(c: &Connection, id: &str) -> Result<String, StoreError> {
    let mut stmt = c
        .prepare("SELECT path, sha256 FROM workspace_files WHERE workspace_id = ?1 ORDER BY path")
        .map_err(db)?;
    let rows = stmt
        .query_map(params![id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db)?;
    let mut hash = Sha256::new();
    for row in rows {
        let (path, sha) = row.map_err(db)?;
        hash.update((path.len() as u64).to_be_bytes());
        hash.update(path.as_bytes());
        hash.update(sha.as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Insert or replace one row inside the caller's transaction.
pub(crate) fn upsert(c: &Connection, id: &str, entry: &LedgerEntry) -> Result<(), StoreError> {
    c.execute(
        "INSERT INTO workspace_files (workspace_id, path, size, sha256) VALUES (?1,?2,?3,?4) \
         ON CONFLICT(workspace_id, path) DO UPDATE SET size = excluded.size, sha256 = excluded.sha256",
        params![id, entry.path, entry.size as i64, entry.sha256],
    )
    .map_err(db)?;
    Ok(())
}

impl WorkspaceStore {
    /// Every ledger row of `id`, keyed by path.
    pub fn ledger(&self, id: &str) -> Result<HashMap<String, (u64, String)>, StoreError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = conn
            .prepare("SELECT path, size, sha256 FROM workspace_files WHERE workspace_id = ?1")
            .map_err(db)?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get::<_, i64>(1)?.max(0) as u64, r.get::<_, String>(2)?),
                ))
            })
            .map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)
    }

    /// The ledger row of one path, if any.
    pub fn ledger_entry(&self, id: &str, path: &str) -> Result<Option<(u64, String)>, StoreError> {
        use rusqlite::OptionalExtension;
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT size, sha256 FROM workspace_files WHERE workspace_id = ?1 AND path = ?2",
            params![id, path],
            |r| Ok((r.get::<_, i64>(0)?.max(0) as u64, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(db)
    }

    /// Replace the whole ledger of `id` with `entries` (reconciliation) and
    /// return the new manifest hash.
    pub fn replace_ledger(&self, id: &str, entries: &[LedgerEntry]) -> Result<String, StoreError> {
        self.tx(|c| {
            c.execute(
                "DELETE FROM workspace_files WHERE workspace_id = ?1",
                params![id],
            )
            .map_err(db)?;
            for e in entries {
                upsert(c, id, e)?;
            }
            manifest_hash(c, id)
        })
    }
}
