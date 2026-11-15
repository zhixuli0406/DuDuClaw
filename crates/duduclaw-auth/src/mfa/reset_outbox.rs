//! RM-136 (M5): durable audit outbox of `users.mfa_reset` attempts.
//!
//! Every attempt gets a unique `attempt_id` and exactly one outcome row in
//! `mfa_reset_audit_outbox` (actor, target, proof kind, outcome, reason code,
//! timestamp — never a password, code or token):
//! - a reset (allowed) and every refusal decided inside the reset transaction
//!   is written **in that transaction** ([`UserDb::mfa_admin_reset_recorded`]),
//!   so a committed reset always has its row;
//! - the email-code start writes a **held** row (`challenge_sent`) in the
//!   transaction that mints the code; the gateway releases it with the final
//!   outcome once the mail was (or was not) delivered;
//! - refusals decided before any transaction (gateway gates) use the short
//!   dedicated insert [`UserDb::mfa_reset_outbox_record`].
//!
//! **Exactly once.** The gateway drains rows into the hash-chained security
//! audit through a [`ResetAuditSink`] ([`UserDb::mfa_reset_outbox_drain`]).
//! The two stores cannot commit together, so each row carries a durable
//! delivery marker, `delivery_from`: the audit log position committed on the
//! row *before* its first append. Per row:
//! 1. no marker yet → commit `delivery_from` = the sink's current position;
//! 2. `append_once(row, delivery_from)`: under the audit writer lock, append
//!    unless a record of this `attempt_id` already follows that position
//!    (only that tail is read, never the whole log);
//! 3. delete the row.
//!
//! Rows that existed before the marker column was added get marker 0 in the
//! schema migration: the whole log is checked for them.
//!
//! A crash before 1 or between 1 and 2 leaves no record and the replay
//! appends it; a crash between 2 and 3 (or a failed delete) leaves a marked
//! row whose replay finds the record and only deletes the row. A marked row
//! is frozen: it can no longer be released, so the delivered outcome is the
//! one the row holds. Held rows are delivered only by the start-up drain (a
//! held row at start belongs to a process that died before releasing it),
//! except once marked.
//!
//! **Bound.** At most [`RESET_OUTBOX_MAX_ROWS`] rows. A full outbox refuses
//! the attempt before any proof is checked (the gateway then appends the
//! refusal directly). Attempts are rate-limited per actor upstream, and rows
//! leave the outbox after every attempt, so the bound is only reached while
//! the audit file itself cannot be written.
use super::*;

/// Upper bound of undelivered outbox rows.
pub const RESET_OUTBOX_MAX_ROWS: i64 = 1000;
const MAX_ID_CHARS: usize = 128;
const MAX_LABEL_CHARS: usize = 40;

/// One `users.mfa_reset` attempt outcome (no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetAuditRow {
    /// Unique per attempt (1–64 of `[0-9A-Za-z-]`).
    pub attempt_id: String,
    pub actor_user_id: String,
    pub target_user_id: Option<String>,
    /// Proof kind label (`factor`, `password`, `email_code_begin`, …).
    pub proof: String,
    /// `allowed`, `refused` or `challenge_sent`.
    pub outcome: String,
    /// `allowed`, `challenge_sent` or an `AdminResetRefusal::reason`.
    pub reason: String,
    pub target_admin: Option<bool>,
    /// Held until the gateway releases it (email-code start only).
    pub held: bool,
    /// Unix seconds.
    pub created_at: i64,
}

impl ResetAuditRow {
    /// A row for `attempt_id`, created now. Ids are bounded on write.
    pub fn new(attempt_id: &str, actor: &str, target: Option<&str>, proof: &str, outcome: &str, reason: &str,
        target_admin: Option<bool>) -> Self {
        Self {
            attempt_id: attempt_id.to_owned(), actor_user_id: actor.to_owned(), target_user_id: target.map(str::to_owned),
            proof: proof.to_owned(), outcome: outcome.to_owned(), reason: reason.to_owned(), target_admin,
            held: false, created_at: chrono::Utc::now().timestamp(),
        }
    }
}

fn bounded(id: &str) -> String { id.chars().take(MAX_ID_CHARS).collect() }

fn valid_attempt_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn valid_label(label: &str) -> bool {
    !label.is_empty() && label.len() <= MAX_LABEL_CHARS && label.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// A fresh random attempt id (32 hexadecimal characters).
pub fn new_attempt_id() -> Result<String> { random_hex(16) }

/// Where outbox rows are delivered (the gateway: the hash-chained security
/// audit). See the module docs for the exactly-once protocol.
pub trait ResetAuditSink {
    /// A position at or before the start of anything appended from now on
    /// (the audit log length).
    fn position(&mut self) -> Result<u64>;
    /// Append `row` unless a record of `row.attempt_id` already follows
    /// position `since`; check and append under the sink's writer lock.
    /// `Ok(true)` appended, `Ok(false)` already there (nothing written).
    fn append_once(&mut self, row: &ResetAuditRow, since: u64) -> Result<bool>;
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let failed = |e: rusqlite::Error| format!("MFA reset outbox migration failed: {e}");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mfa_reset_audit_outbox (
            attempt_id TEXT PRIMARY KEY,
            actor_user_id TEXT NOT NULL,
            target_user_id TEXT,
            proof TEXT NOT NULL,
            outcome TEXT NOT NULL,
            reason TEXT NOT NULL,
            target_admin INTEGER CHECK(target_admin IS NULL OR target_admin IN (0,1)),
            held INTEGER NOT NULL DEFAULT 0 CHECK(held IN (0,1)),
            created_at INTEGER NOT NULL,
            delivery_from INTEGER CHECK(delivery_from IS NULL OR delivery_from>=0));",
    ).map_err(failed)?;
    // Tables created before the delivery marker existed. Their rows were
    // written by code without the marker, which may already have appended a
    // row's record and crashed before deleting it, at any earlier position.
    // So every such row gets marker 0: its first drain checks the whole
    // audit log for its `attempt_id` and appends only when none is found.
    // Rows created later stay unmarked until their first drain (module
    // docs). This runs inside the caller's migration transaction, so the
    // column never exists without these markers. Cost: the security audit log
    // is not rotated and the once-check has no window bound, so the worst case
    // is one full sequential read of it, under the audit writer lock, per
    // pre-marker row (at most [`RESET_OUTBOX_MAX_ROWS`]) per drain, until the
    // row is deleted after its delivery (normally the first drain). Held
    // pre-marker rows are marked too: they belong to a process that is gone,
    // so they are frozen and delivered as interrupted.
    let marked: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('mfa_reset_audit_outbox')
        WHERE name='delivery_from')", [], |r| r.get(0)).map_err(failed)?;
    if !marked {
        conn.execute_batch("ALTER TABLE mfa_reset_audit_outbox ADD COLUMN delivery_from INTEGER
            CHECK(delivery_from IS NULL OR delivery_from>=0);
            UPDATE mfa_reset_audit_outbox SET delivery_from=0;").map_err(failed)?;
    }
    Ok(())
}

const ROW_COLUMNS: &str = "attempt_id,actor_user_id,target_user_id,proof,outcome,reason,target_admin,held,created_at,delivery_from";

/// One row of [`ROW_COLUMNS`] with its delivery marker.
fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(ResetAuditRow, Option<i64>)> {
    Ok((ResetAuditRow {
        attempt_id: r.get(0)?, actor_user_id: r.get(1)?, target_user_id: r.get(2)?, proof: r.get(3)?,
        outcome: r.get(4)?, reason: r.get(5)?, target_admin: r.get(6)?, held: r.get(7)?, created_at: r.get(8)?,
    }, r.get(9)?))
}

fn position_of(marker: i64) -> Result<u64> {
    u64::try_from(marker).map_err(|_| "MFA reset audit delivery marker invalid".to_string())
}

/// Whether one more row fits under [`RESET_OUTBOX_MAX_ROWS`].
pub(super) fn has_room(conn: &Connection) -> Result<bool> {
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM mfa_reset_audit_outbox", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(rows < RESET_OUTBOX_MAX_ROWS)
}

/// Insert `row` on `conn` (the caller's transaction). Refused when the
/// outbox is full or the row is malformed.
pub(super) fn insert(conn: &Connection, row: &ResetAuditRow) -> Result<()> {
    if !valid_attempt_id(&row.attempt_id) || !valid_label(&row.proof) || !valid_label(&row.outcome)
        || !valid_label(&row.reason) {
        return Err("MFA reset audit row invalid".into());
    }
    if !has_room(conn)? { return Err("MFA reset audit outbox full".into()); }
    conn.execute("INSERT INTO mfa_reset_audit_outbox(attempt_id,actor_user_id,target_user_id,proof,outcome,reason,
        target_admin,held,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![row.attempt_id, bounded(&row.actor_user_id), row.target_user_id.as_deref().map(bounded), row.proof,
            row.outcome, row.reason, row.target_admin, row.held, row.created_at]).map_err(|e| e.to_string())?;
    Ok(())
}

impl UserDb {
    /// Short dedicated insert of one outcome row (refusals decided before
    /// any reset transaction).
    pub fn mfa_reset_outbox_record(&self, row: &ResetAuditRow) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        insert(&tx, row)?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Release a held row with its final outcome. `Ok(false)` when no held,
    /// unmarked row of `attempt_id` exists: a start-up drain already began
    /// delivering it, and that single record stands. An error leaves the
    /// held row as the attempt's one durable state (retry the release; never
    /// record the outcome a second way).
    pub fn mfa_reset_outbox_release(&self, attempt_id: &str, outcome: &str, reason: &str) -> Result<bool> {
        if !valid_label(outcome) || !valid_label(reason) { return Err("MFA reset audit row invalid".into()); }
        let changed = self.conn().execute("UPDATE mfa_reset_audit_outbox SET held=0,outcome=?2,reason=?3
            WHERE attempt_id=?1 AND held=1 AND delivery_from IS NULL", params![attempt_id, outcome, reason])
            .map_err(|e| e.to_string())?;
        Ok(changed == 1)
    }

    /// Undelivered rows, oldest first (unmarked held rows only with
    /// `include_held`).
    pub fn mfa_reset_outbox_pending(&self, include_held: bool) -> Result<Vec<ResetAuditRow>> {
        Ok(self.outbox_rows(include_held)?.into_iter().map(|(row, _)| row).collect())
    }

    fn outbox_rows(&self, include_held: bool) -> Result<Vec<(ResetAuditRow, Option<i64>)>> {
        let conn = self.conn();
        let mut q = conn.prepare(&format!("SELECT {ROW_COLUMNS} FROM mfa_reset_audit_outbox
            WHERE held=0 OR delivery_from IS NOT NULL OR ?1 ORDER BY created_at,rowid LIMIT ?2"))
            .map_err(|e| e.to_string())?;
        q.query_map(params![include_held, RESET_OUTBOX_MAX_ROWS], read_row).map_err(|e| e.to_string())?
            .collect::<std::result::Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Step 1 of the module protocol: commit `position` as the delivery
    /// marker of an unmarked row (an earlier marker wins) and return the row
    /// as it now stands, frozen, with its marker. `None` when the row is gone.
    fn mark(&self, attempt_id: &str, position: u64) -> Result<Option<(ResetAuditRow, u64)>> {
        let marker = i64::try_from(position).map_err(|_| "audit log position out of range".to_string())?;
        let conn = self.conn();
        conn.execute("UPDATE mfa_reset_audit_outbox SET delivery_from=?2 WHERE attempt_id=?1 AND delivery_from IS NULL",
            params![attempt_id, marker]).map_err(|e| e.to_string())?;
        let row = conn.query_row(&format!("SELECT {ROW_COLUMNS} FROM mfa_reset_audit_outbox WHERE attempt_id=?1"),
            [attempt_id], read_row).optional().map_err(|e| e.to_string())?;
        match row {
            Some((row, Some(marker))) => Ok(Some((row, position_of(marker)?))),
            Some((_, None)) => Err("MFA reset audit delivery marker not stored".into()),
            None => Ok(None),
        }
    }

    /// Deliver undelivered rows to `sink`, oldest first, exactly once each
    /// (module docs): mark, `append_once`, delete. Stops at the first error
    /// (that row and the later ones stay for the next drain) and returns it.
    /// `Ok(n)` = rows delivered and removed, including rows whose record an
    /// interrupted earlier drain had already appended.
    pub fn mfa_reset_outbox_drain(&self, include_held: bool, sink: &mut dyn ResetAuditSink) -> Result<usize> {
        let mut delivered = 0;
        for (row, marker) in self.outbox_rows(include_held)? {
            let (row, since) = match marker {
                // Includes marker 0 of rows from before the marker column.
                Some(marker) => (row, position_of(marker)?),
                None => match self.mark(&row.attempt_id, sink.position()?)? {
                    Some(marked) => marked,
                    None => continue,
                },
            };
            sink.append_once(&row, since)?;
            self.conn().execute("DELETE FROM mfa_reset_audit_outbox WHERE attempt_id=?1", [&row.attempt_id])
                .map_err(|e| e.to_string())?;
            delivered += 1;
        }
        Ok(delivered)
    }
}
