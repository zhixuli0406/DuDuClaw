//! Write intents of the registry (design §4.4): a write is recorded before
//! the temp file is made and settled after the rename, so a crash between
//! the two is reconciled at boot instead of leaving a silent revision gap.

use rusqlite::params;
use serde_json::json;

use super::ledger::{self, LedgerEntry};
use super::store::{Lease, StoreError, WorkspaceStore, WriteIntent, db, get_row, record_event};

/// What [`WorkspaceStore::finish_write`] committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    pub revision: i64,
    /// The lease was no longer this session's when the write was recorded.
    pub lease_lost: bool,
}

impl WorkspaceStore {
    /// Record a write that is about to happen. Refused unless `lease` is the
    /// live lease and (when given) `expected_revision` matches.
    pub fn begin_write(
        &self,
        lease: &Lease,
        intent: &WriteIntent,
        expected_revision: Option<i64>,
        now: i64,
    ) -> Result<(), StoreError> {
        self.tx(|c| {
            let row = get_row(c, &lease.workspace_id)?.ok_or(StoreError::NotFound)?;
            if !row.lease_active(now)
                || row.lease_epoch != lease.epoch
                || row.lease_holder.as_deref() != Some(&lease.holder)
                || row.permission_revision != lease.permission_revision
            {
                return Err(StoreError::LeaseLost);
            }
            if let Some(expected) = expected_revision
                && expected != row.data_revision
            {
                return Err(StoreError::RevisionMismatch(row.data_revision));
            }
            c.execute(
                "INSERT INTO workspace_write_intents (intent_id, workspace_id, rel_path_hash, \
                 temp_name, target_sha256, lease_epoch, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    intent.intent_id,
                    intent.workspace_id,
                    intent.rel_path_hash,
                    intent.temp_name,
                    intent.target_sha256,
                    intent.lease_epoch,
                    intent.created_at
                ],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    /// The file landed: drop the intent, record `entry` in the ledger, set
    /// usage to `usage` `(bytes, files)` and bump the revision. Refused
    /// (nothing changes) when the intent is no longer there: reconciliation
    /// already settled it, so bumping again would count the write twice.
    ///
    /// When the lease moved meanwhile the write still counts (it is the
    /// owner's own file): the same transaction commits the revision, the
    /// ledger row and a `write_landed_after_fence` event, and the answer says
    /// [`Finished::lease_lost`]. It is never an `Err` (review M-1: an `Err`
    /// rolled the whole transaction back, so the event was never written and
    /// `expected_revision` could not see the write).
    pub fn finish_write(
        &self,
        lease: &Lease,
        intent: &WriteIntent,
        entry: &LedgerEntry,
        usage: (i64, i64),
        now: i64,
    ) -> Result<Finished, StoreError> {
        self.tx(|c| {
            let row = get_row(c, &lease.workspace_id)?.ok_or(StoreError::NotFound)?;
            let lost = !row.lease_active(now)
                || row.lease_epoch != lease.epoch
                || row.lease_holder.as_deref() != Some(&lease.holder);
            let dropped = c
                .execute(
                    "DELETE FROM workspace_write_intents WHERE intent_id = ?1",
                    params![intent.intent_id],
                )
                .map_err(db)?;
            if dropped != 1 {
                return Err(StoreError::Unavailable(
                    "write intent already settled".into(),
                ));
            }
            ledger::upsert(c, &lease.workspace_id, entry)?;
            let manifest = ledger::manifest_hash(c, &lease.workspace_id)?;
            let (bytes, files) = usage;
            c.execute(
                "UPDATE workspaces SET data_revision = data_revision + 1, manifest_hash = ?2, \
                 bytes_used = ?3, files_used = ?4 WHERE workspace_id = ?1",
                params![lease.workspace_id, manifest, bytes, files],
            )
            .map_err(db)?;
            let row = get_row(c, &lease.workspace_id)?.ok_or(StoreError::NotFound)?;
            let kind = if lost {
                "write_landed_after_fence"
            } else {
                "file_written"
            };
            record_event(
                c,
                &lease.workspace_id,
                kind,
                &format!("agent:{}", row.owner_agent_id),
                Some(&row),
                json!({"path_hash": intent.rel_path_hash, "sha256": intent.target_sha256}),
            )?;
            Ok(Finished {
                revision: row.data_revision,
                lease_lost: lost,
            })
        })
    }

    /// Whether workspace `id` has an event `kind` at or after `since`.
    pub fn has_event_since(&self, id: &str, kind: &str, since: i64) -> Result<bool, StoreError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.prepare(
            "SELECT 1 FROM workspace_events WHERE workspace_id = ?1 AND kind = ?2 AND at >= ?3 LIMIT 1",
        )
        .map_err(db)?
        .exists(params![id, kind, since])
        .map_err(db)
    }

    /// Intents of one workspace (settled under that workspace's lock).
    pub fn list_intents_for(&self, id: &str) -> Result<Vec<WriteIntent>, StoreError> {
        Ok(self
            .list_intents()?
            .into_iter()
            .filter(|i| i.workspace_id == id)
            .collect())
    }

    /// Drop an intent whose write failed (nothing landed).
    pub fn abandon_write(&self, intent_id: &str) -> Result<(), StoreError> {
        self.tx(|c| {
            c.execute(
                "DELETE FROM workspace_write_intents WHERE intent_id = ?1",
                params![intent_id],
            )
            .map_err(db)?;
            Ok(())
        })
    }

    pub fn list_intents(&self) -> Result<Vec<WriteIntent>, StoreError> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = conn
            .prepare("SELECT intent_id, workspace_id, rel_path_hash, temp_name, target_sha256, lease_epoch, created_at FROM workspace_write_intents ORDER BY created_at")
            .map_err(db)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WriteIntent {
                    intent_id: r.get(0)?,
                    workspace_id: r.get(1)?,
                    rel_path_hash: r.get(2)?,
                    temp_name: r.get(3)?,
                    target_sha256: r.get(4)?,
                    lease_epoch: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })
            .map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)
    }

    /// Boot reconciliation of one intent: usage recomputed from disk,
    /// `revision_bump` when the target hash matched, event `kind`.
    pub fn reconcile_intent(
        &self,
        intent: &WriteIntent,
        usage: (&str, i64, i64),
        revision_bump: bool,
        kind: &str,
    ) -> Result<(), StoreError> {
        self.tx(|c| {
            let (manifest, bytes, files) = usage;
            c.execute(
                "UPDATE workspaces SET data_revision = data_revision + ?2, manifest_hash = ?3, \
                 bytes_used = ?4, files_used = ?5 WHERE workspace_id = ?1",
                params![
                    intent.workspace_id,
                    i64::from(revision_bump),
                    manifest,
                    bytes,
                    files
                ],
            )
            .map_err(db)?;
            c.execute(
                "DELETE FROM workspace_write_intents WHERE intent_id = ?1",
                params![intent.intent_id],
            )
            .map_err(db)?;
            let row = get_row(c, &intent.workspace_id)?;
            record_event(
                c,
                &intent.workspace_id,
                kind,
                "system:boot",
                row.as_ref(),
                json!({"path_hash": intent.rel_path_hash}),
            )
        })
    }
}
