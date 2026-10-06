//! Time-driven registry changes: lease expiry, retention expiry and renewal,
//! and the size cap of the event table (design §3.5, §4.6; review M4, L9).

use rusqlite::params;
use serde_json::json;

use super::state::WorkspaceState;
use super::store::{StoreError, WorkspaceRow, WorkspaceStore, db, get_row, record_event};

/// Events older than this are deleted by reconciliation.
pub const EVENT_RETENTION_SECS: i64 = 90 * 86_400;
/// At most this many events are kept (the newest).
pub const EVENT_MAX_ROWS: i64 = 100_000;

/// "No live lease at `?now`", in SQL (mirrors [`WorkspaceRow::lease_active`]).
const NO_LIVE_LEASE: &str = "(lease_holder IS NULL OR lease_until IS NULL OR lease_until <= ?2)";

impl WorkspaceStore {
    /// Every lease past its `lease_until`: cleared, epoch +1 (boot / sweep).
    pub fn expire_stale_leases(&self, now: i64) -> Result<usize, StoreError> {
        self.tx(|c| {
            let ids: Vec<String> = {
                let mut stmt = c
                    .prepare("SELECT workspace_id FROM workspaces WHERE lease_holder IS NOT NULL AND (lease_until IS NULL OR lease_until <= ?1)")
                    .map_err(db)?;
                let ids = stmt.query_map(params![now], |r| r.get(0)).map_err(db)?;
                ids.collect::<Result<_, _>>().map_err(db)?
            };
            for id in &ids {
                c.execute(
                    "UPDATE workspaces SET lease_epoch = lease_epoch + 1, lease_holder = NULL, \
                     lease_instance = NULL, lease_until = NULL WHERE workspace_id = ?1 \
                     AND lease_holder IS NOT NULL AND (lease_until IS NULL OR lease_until <= ?2)",
                    params![id, now],
                )
                .map_err(db)?;
                let row = get_row(c, id)?;
                record_event(c, id, "lease_expired", "system:sweep", row.as_ref(), json!({}))?;
            }
            Ok(ids.len())
        })
    }

    /// `ready` rows past `expires_at` with no live lease → `expired`, in one
    /// transaction whose UPDATE re-checks both conditions, so a lease taken
    /// between the scan and the change keeps the workspace `ready`. Nothing
    /// is deleted. Returns `(id, owner)` of each.
    pub fn expire_retention(&self, now: i64) -> Result<Vec<(String, String)>, StoreError> {
        self.tx(|c| {
            let due: Vec<(String, String)> = {
                let sql = format!(
                    "SELECT workspace_id, owner_agent_id FROM workspaces WHERE state = 'ready' \
                     AND expires_at IS NOT NULL AND expires_at <= ?1 AND {}",
                    NO_LIVE_LEASE.replace("?2", "?1")
                );
                let mut stmt = c.prepare(&sql).map_err(db)?;
                let rows = stmt
                    .query_map(params![now], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(db)?;
                rows.collect::<Result<_, _>>().map_err(db)?
            };
            let update = format!(
                "UPDATE workspaces SET state = 'expired', state_reason = 'retention', \
                 lease_epoch = lease_epoch + 1, lease_holder = NULL, lease_instance = NULL, \
                 lease_until = NULL WHERE workspace_id = ?1 AND state = 'ready' \
                 AND expires_at IS NOT NULL AND expires_at <= ?2 AND {NO_LIVE_LEASE}"
            );
            let mut out = Vec::new();
            for (id, owner) in due {
                if c.execute(&update, params![id, now]).map_err(db)? != 1 {
                    continue;
                }
                let row = get_row(c, &id)?;
                record_event(
                    c,
                    &id,
                    "expired",
                    "system:retention",
                    row.as_ref(),
                    json!({"from": ["ready"], "to": "expired", "reason": "retention"}),
                )?;
                out.push((id, owner));
            }
            Ok(out)
        })
    }

    /// Operator: extend retention; `expired` → `ready`. One transaction, so
    /// a retention sweep cannot expire the workspace again between the state
    /// change and the new `expires_at` (review L-4). Leaving `expired` moves
    /// the lease epoch (as every transition into `ready` from another state).
    pub fn renew_retention(
        &self,
        id: &str,
        actor: &str,
        now: i64,
        retention_days: u32,
    ) -> Result<WorkspaceRow, StoreError> {
        let expires = (retention_days > 0).then(|| now + i64::from(retention_days) * 86_400);
        self.tx(|c| {
            let row = get_row(c, id)?.ok_or(StoreError::NotFound)?;
            if !matches!(row.state, WorkspaceState::Ready | WorkspaceState::Expired) {
                return Err(StoreError::State(row.state));
            }
            let fence = i64::from(row.state != WorkspaceState::Ready);
            c.execute(
                "UPDATE workspaces SET state = 'ready', state_reason = NULL, expires_at = ?2, \
                 lease_epoch = lease_epoch + ?3, \
                 lease_holder = CASE WHEN ?3 = 1 THEN NULL ELSE lease_holder END, \
                 lease_instance = CASE WHEN ?3 = 1 THEN NULL ELSE lease_instance END, \
                 lease_until = CASE WHEN ?3 = 1 THEN NULL ELSE lease_until END \
                 WHERE workspace_id = ?1",
                params![id, expires, fence],
            )
            .map_err(db)?;
            let after = get_row(c, id)?.ok_or(StoreError::NotFound)?;
            record_event(
                c,
                id,
                "renewed",
                actor,
                Some(&after),
                json!({"from": [row.state.as_str()], "to": "ready", "reason": null}),
            )?;
            Ok(after)
        })
    }

    /// Cap the event table: drop events older than [`EVENT_RETENTION_SECS`]
    /// and everything beyond the newest [`EVENT_MAX_ROWS`]. Returns how many
    /// rows went.
    pub fn prune_events(&self, now: i64) -> Result<usize, StoreError> {
        self.prune_events_with(now, EVENT_RETENTION_SECS, EVENT_MAX_ROWS)
    }

    pub(crate) fn prune_events_with(
        &self,
        now: i64,
        max_age_secs: i64,
        max_rows: i64,
    ) -> Result<usize, StoreError> {
        self.tx(|c| {
            let old = c
                .execute(
                    "DELETE FROM workspace_events WHERE at < ?1",
                    params![now - max_age_secs],
                )
                .map_err(db)?;
            let over = c
                .execute(
                    "DELETE FROM workspace_events WHERE id <= (SELECT id FROM workspace_events \
                     ORDER BY id DESC LIMIT 1 OFFSET ?1)",
                    params![max_rows],
                )
                .map_err(db)?;
            Ok(old + over)
        })
    }
}
