//! Durable channel inbox; this is a handoff ledger, not another agent runner.
//! Dispatch may have side effects: expired dispatch leases become uncertain.
//!
//! Status vocabulary (F2):
//! - `ready` / `claimed` / `dispatching`: in flight. A `ready` row may carry
//!   `retry_at` after a transient revalidation failure (back-off).
//! - `completed`: the turn ran and its reply (or late Push) was accepted.
//! - `failed_before_dispatch`: proven not executed (for example the reply
//!   window had passed and `line_late_reply = "fail"`). Plain `retry` allowed.
//! - `undelivered`: the turn ran but its answer was not delivered. Only an
//!   explicit `rerun` with a duplicate-risk confirmation runs it again.
//! - `uncertain`: the turn may have run and no receipt exists. `rerun` only.
//! - `quarantined`: route/authority changed, or could not be read after
//!   repeated back-off. `close` (and `retry` for `revalidation_unavailable`).
//! - `closed`: an operator resolved it.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

pub(crate) mod alerts;
pub mod cli_approval;
pub mod cli_batch;
pub(crate) mod config;
mod maintenance;
mod resolve;
mod schema;
pub(crate) mod snapshot;

pub(crate) use resolve::ResolveRequest;

pub(crate) const LEASE_SECONDS: i64 = 90;
pub(crate) const PAYLOAD_SECONDS: i64 = 86400;
/// The local reply-token window (LINE documents roughly one minute).
pub(crate) const REPLY_TOKEN_SECONDS: i64 = 60;
/// Consecutive "could not read the route/authority" back-offs before an
/// event is quarantined as `revalidation_unavailable`.
pub(crate) const UNAVAILABLE_LIMIT: i64 = 5;
/// One-shot file the device-restore swap writes into the home
/// (`backup_restore::perform_pending_restore_swap`). The inbox consumes it
/// when it opens, before any worker can claim: waiting events from the
/// archive are held as `quarantined` / `restored_from_backup`.
pub const RESTORE_MARKER: &str = "channel_ingress.restored";
/// Reason for events held after a restore.
pub(crate) const RESTORED_REASON: &str = "restored_from_backup";
/// Revision written at acceptance; the snapshot is taken right after commit.
pub(crate) const PENDING_REVISION: &str = "pending";
/// A pending event older than this can no longer be snapshotted "as
/// accepted": it is quarantined as `snapshot_unavailable` (review N1).
pub(crate) const SNAPSHOT_MAX_AGE_SECS: i64 = 300;
/// Statuses that hold back later events of the same conversation.
const BLOCKING: &str = "'ready','claimed','dispatching','uncertain','quarantined'";

/// The acceptance-time authorization marker: binds the event to the
/// credentials whose signature it passed until the full snapshot is stored.
pub(crate) fn pending_authorization(credential_revision: &str) -> String {
    format!("{PENDING_REVISION}:{credential_revision}")
}

/// Back-off before the `n`-th re-read (5 s doubling, capped at 5 minutes).
pub(crate) fn unavailable_backoff(n: i64) -> i64 {
    (5_i64 << n.clamp(0, 6)).min(300)
}

pub(crate) fn digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    hex::encode(h.finalize())
}

pub(crate) struct AcceptedEvent {
    pub decision_fastlane: bool,
    pub decision_binding: Option<String>,
    pub event_id: String,
    pub account: String,
    pub revision: String,
    pub authorization_revision: String,
    pub conversation: String,
    pub payload: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct IngressRow {
    pub id: String,
    pub run_id: String,
    pub run_authorization_id: Option<String>,
    pub decision_fastlane: bool,
    pub decision_binding: Option<String>,
    pub seq: i64,
    pub channel: String,
    pub account: String,
    pub event_id: String,
    pub revision: String,
    pub authorization_revision: String,
    pub conversation: String,
    pub status: String,
    pub attempt: i64,
    pub received_at: i64,
    pub reason: Option<String>,
    pub lease_id: Option<String>,
    pub retry_at: Option<i64>,
    pub unavailable_count: i64,
    pub run_started_at: Option<i64>,
    #[serde(skip)]
    pub payload: Option<String>,
}

impl IngressRow {
    /// Still waiting for its post-commit route/authority snapshot.
    #[cfg(test)]
    pub(crate) fn snapshot_pending(&self) -> bool {
        self.revision == PENDING_REVISION
    }
}

/// What a finished dispatch attempt learned besides its status.
#[derive(Default, Clone, Debug)]
pub(crate) struct AttemptReceipt {
    pub provider_receipt: Option<String>,
    pub delivered_via: Option<String>,
    pub progress_note: Option<String>,
}

/// Outcome of a transient revalidation failure.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Deferred {
    /// Back to `ready` with `retry_at`.
    Retry(i64),
    /// The limit was reached: `quarantined` / `revalidation_unavailable`.
    Quarantined,
    /// The lease was no longer ours; nothing changed.
    Lost,
}

pub(crate) struct IngressStore {
    conn: Mutex<Connection>,
    work: tokio::sync::Notify,
    /// Ids held at open because of a restore marker, not yet announced.
    restored: std::sync::Mutex<Vec<String>>,
}

impl IngressStore {
    pub(crate) fn from_connection(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
            work: tokio::sync::Notify::new(),
            restored: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Ids held at open because the data was restored from a backup; each
    /// list is handed out once (for the alert).
    pub(crate) fn take_restored(&self) -> Vec<String> {
        self.restored
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default()
    }

    /// Wakes idle workers after an append or after a row returns to `ready`.
    pub(crate) fn work_signal(&self) -> &tokio::sync::Notify {
        &self.work
    }

    /// One transaction for the whole signed envelope. Success is the ACK boundary.
    pub(crate) async fn append(&self, events: &[AcceptedEvent], now: i64) -> Result<(), String> {
        {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            for e in events {
                let id = digest(&["line", &e.account, &e.event_id]);
                let inserted = tx
                    .execute(
                        "INSERT OR IGNORE INTO ingress(id,channel,account,event_id,revision,authorization_revision,
                            conversation,received_at,run_id,decision_fastlane,decision_binding) VALUES (?1,'line',?2,?3,
                            ?4,?5,?6,?7,?1,?8,?9)",
                        params![
                            id,
                            e.account,
                            e.event_id,
                            e.revision,
                            e.authorization_revision,
                            e.conversation,
                            now,
                            e.decision_fastlane,
                            e.decision_binding
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                // A redelivery must never refresh a token or resurrect purged payload.
                if inserted == 1 {
                    tx.execute(
                        "INSERT INTO ingress_payload VALUES (?1,?2,?3)",
                        params![id, e.payload, now + PAYLOAD_SECONDS],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        self.work.notify_waiters();
        Ok(())
    }

    /// Store the post-commit snapshot of a row accepted with
    /// [`PENDING_REVISION`]. Only replaces the pending marker it was given,
    /// so a later snapshot never overwrites an earlier one.
    pub(crate) async fn store_snapshot(
        &self,
        id: &str,
        pending_authorization: &str,
        revision: &str,
        authorization_revision: &str,
    ) -> Result<bool, String> {
        self.conn
            .lock()
            .await
            .execute(
                "UPDATE ingress SET revision=?3,authorization_revision=?4,retry_at=NULL,unavailable_count=0,
                    reason=NULL WHERE id=?1 AND revision=?5 AND authorization_revision=?2
                    AND status='ready'",
                params![
                    id,
                    pending_authorization,
                    revision,
                    authorization_revision,
                    PENDING_REVISION
                ],
            )
            .map(|n| n == 1)
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn claim(&self, now: i64) -> Result<Option<IngressRow>, String> {
        self.claim_lane(now, false).await
    }
    pub(crate) async fn claim_decision(&self, now: i64) -> Result<Option<IngressRow>, String> {
        self.claim_lane(now, true).await
    }
    async fn claim_lane(&self, now: i64, decision: bool) -> Result<Option<IngressRow>, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let id: Option<String> = tx
            .query_row(
                &format!(
                    "SELECT i.id FROM ingress i WHERE status='ready' AND decision_fastlane=?1
                      AND i.revision!='{PENDING_REVISION}'
                      AND (i.retry_at IS NULL OR i.retry_at<=?2)
                      AND (?1=1 OR NOT EXISTS (SELECT 1 FROM ingress p WHERE p.decision_fastlane=0
                        AND p.channel=i.channel AND p.account=i.account AND p.conversation=i.conversation
                        AND p.seq<i.seq AND p.status IN ({BLOCKING})))
                      ORDER BY seq LIMIT 1"
                ),
                params![decision, now],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(id) = id else { return Ok(None) };
        let lease = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "UPDATE ingress SET status='claimed',attempt=attempt+1,lease_id=?2,lease_until=?3,retry_at=NULL
                WHERE id=?1 AND status='ready'",
            params![id, lease, now + LEASE_SECONDS],
        )
        .map_err(|e| e.to_string())?;
        let row = tx
            .query_row(ROW_SELECT_BY_ID, [&id], row_from_sql)
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Some(row))
    }

    pub(crate) async fn transition(
        &self,
        row: &IngressRow,
        from: &str,
        to: &str,
        reason: Option<&str>,
    ) -> Result<bool, String> {
        self.transition_with(row, from, to, reason, None).await
    }

    /// CAS on (id, lease, status). Leaving `dispatching` writes one immutable
    /// attempt row (plus its receipt side row). Entering `dispatching` stamps
    /// the run start; `completed` drops the payload at once (review I-LOW-3).
    pub(crate) async fn transition_with(
        &self,
        row: &IngressRow,
        from: &str,
        to: &str,
        reason: Option<&str>,
        receipt: Option<&AttemptReceipt>,
    ) -> Result<bool, String> {
        let n = {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            if from == "dispatching" && matches!(to, "completed" | "undelivered" | "uncertain") {
                let inserted = tx
                    .execute(
                        "INSERT OR IGNORE INTO ingress_attempts(operation_id,ingress_id,ordinal,status,reason,
                            finished_at,retry_authorization_id) SELECT lease_id,id,attempt,?4,?5,strftime('%s','now'),
                            i.run_authorization_id FROM ingress i WHERE id=?1 AND lease_id=?2 AND status=?3",
                        params![row.id, row.lease_id, from, to, reason],
                    )
                    .map_err(|e| e.to_string())?;
                if inserted == 1 {
                    if let Some(r) = receipt {
                        tx.execute(
                            "INSERT OR IGNORE INTO ingress_attempt_receipts VALUES (?1,?2,?3,?4)",
                            params![
                                row.lease_id,
                                r.provider_receipt,
                                r.delivered_via,
                                r.progress_note
                            ],
                        )
                        .map_err(|e| e.to_string())?;
                    }
                }
            }
            let n = tx
                .execute(
                    "UPDATE ingress SET status=?4,reason=?5,
                        run_started_at=CASE WHEN ?4='dispatching' THEN CAST(strftime('%s','now') AS INTEGER)
                            ELSE run_started_at END,
                        lease_id=CASE WHEN ?4='dispatching' THEN lease_id ELSE NULL END,
                        lease_until=CASE WHEN ?4='dispatching' THEN lease_until ELSE NULL END
                        WHERE id=?1 AND lease_id=?2 AND status=?3",
                    params![row.id, row.lease_id, from, to, reason],
                )
                .map_err(|e| e.to_string())?;
            if n == 1 && to == "completed" {
                tx.execute("DELETE FROM ingress_payload WHERE id=?1", [&row.id])
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            n
        };
        if n == 1 && to == "ready" {
            self.work.notify_waiters();
        }
        Ok(n == 1)
    }

    /// The route/authority could not be read (not "read and different").
    /// Back off and retry; after [`UNAVAILABLE_LIMIT`] consecutive failures
    /// quarantine it (review I-HIGH-3). Never crosses the dispatch boundary.
    pub(crate) async fn defer_unavailable(
        &self,
        row: &IngressRow,
        reason: &str,
        now: i64,
    ) -> Result<Deferred, String> {
        let count = row.unavailable_count + 1;
        let (status, retry_at, why) = if count >= UNAVAILABLE_LIMIT {
            ("quarantined", None, "revalidation_unavailable")
        } else {
            ("ready", Some(now + unavailable_backoff(count - 1)), reason)
        };
        let n = self
            .conn
            .lock()
            .await
            .execute(
                "UPDATE ingress SET status=?4,reason=?5,retry_at=?6,unavailable_count=?3,lease_id=NULL,
                    lease_until=NULL WHERE id=?1 AND lease_id=?2 AND status='claimed'",
                params![row.id, row.lease_id, count, status, why, retry_at],
            )
            .map_err(|e| e.to_string())?;
        Ok(match (n, retry_at) {
            (0, _) => Deferred::Lost,
            (_, Some(at)) => Deferred::Retry(at),
            (_, None) => Deferred::Quarantined,
        })
    }

    /// `Ok(true)`: renewed. `Ok(false)`: the lease is definitely gone (another
    /// run owns the row, or it expired). `Err`: could not tell (database busy
    /// or I/O); the caller retries within the remaining lease (I-MEDIUM-5).
    pub(crate) async fn renew(&self, row: &IngressRow, now: i64) -> Result<bool, String> {
        self.conn
            .lock()
            .await
            .execute(
                "UPDATE ingress SET lease_until=?3 WHERE id=?1 AND lease_id=?2 AND status IN ('claimed','dispatching')
                    AND lease_until>?4",
                params![row.id, row.lease_id, now + LEASE_SECONDS, now],
            )
            .map(|n| n == 1)
            .map_err(|e| e.to_string())
    }

    #[cfg(test)]
    pub(crate) async fn simulate_full(&self) {
        self.conn
            .lock()
            .await
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);
                PRAGMA max_page_count=8;")
            .unwrap();
    }

    /// Every write on this connection fails as on a read-only volume.
    #[cfg(test)]
    pub(crate) async fn simulate_read_only(&self) {
        self.conn
            .lock()
            .await
            .execute_batch("PRAGMA query_only=ON;")
            .unwrap();
    }

    /// Shorten this connection's busy wait so a held write lock surfaces fast.
    #[cfg(test)]
    pub(crate) async fn simulate_busy_timeout(&self, ms: u64) {
        self.conn
            .lock()
            .await
            .busy_timeout(std::time::Duration::from_millis(ms))
            .unwrap();
    }

    pub(crate) async fn get(&self, id: &str) -> Result<Option<IngressRow>, String> {
        self.conn
            .lock()
            .await
            .query_row(ROW_SELECT_BY_ID, [id], row_from_sql)
            .optional()
            .map_err(|e| e.to_string())
    }

    #[cfg(test)]
    pub(crate) async fn list(&self) -> Result<Vec<IngressRow>, String> {
        self.list_page(None).await
    }

    pub(crate) async fn list_page(
        &self,
        before_seq: Option<i64>,
    ) -> Result<Vec<IngressRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "{} WHERE (?1 IS NULL OR i.seq<?1) ORDER BY i.seq DESC LIMIT 200",
                ROW_SELECT
            ))
            .map_err(|e| e.to_string())?;
        stmt.query_map([before_seq], row_from_sql)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
    pub(crate) async fn inspect(
        &self,
        id: &str,
        before_attempt: Option<i64>,
        before_authorization: Option<i64>,
    ) -> Result<Option<serde_json::Value>, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|e| e.to_string())?;
        let row = tx
            .query_row(ROW_SELECT_BY_ID, [id], row_from_sql)
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(row) = row else { return Ok(None) };
        let mut stmt = tx
            .prepare("SELECT a.operation_id,a.ordinal,a.status,a.reason,a.finished_at,a.retry_authorization_id,
                r.provider_receipt,r.delivered_via,r.progress_note FROM ingress_attempts a
                LEFT JOIN ingress_attempt_receipts r ON r.operation_id=a.operation_id
                WHERE a.ingress_id=?1 AND (?2 IS NULL OR a.ordinal<?2) ORDER BY a.ordinal DESC LIMIT 201")
            .map_err(|e| e.to_string())?;
        let mut attempts = stmt.query_map(params![id,before_attempt], |r| Ok(serde_json::json!({
            "operation_id": r.get::<_,String>(0)?,
            "ordinal": r.get::<_,i64>(1)?,
            "status": r.get::<_,String>(2)?,
            "reason": r.get::<_,Option<String>>(3)?,
            "finished_at": r.get::<_,i64>(4)?,
            "retry_authorization_id": r.get::<_,Option<String>>(5)?,
            "provider_receipt": r.get::<_,Option<String>>(6)?,
            "delivered_via": r.get::<_,Option<String>>(7)?,
            "progress": r.get::<_,Option<String>>(8)?
        }))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        let more_attempts = attempts.len() > 200;
        attempts.truncate(200);
        let mut stmt = tx
            .prepare("SELECT a.rowid,a.authorization_id,a.predecessor_operation_id,a.actor,a.note,a.at,
                c.action,c.confirmed_duplicate_risk,c.provider_receipt FROM ingress_retry_authorizations a
                LEFT JOIN ingress_run_confirmations c ON c.authorization_id=a.authorization_id
                WHERE a.ingress_id=?1 AND (?2 IS NULL OR a.rowid<?2) ORDER BY a.rowid DESC LIMIT 201")
            .map_err(|e| e.to_string())?;
        let mut authorizations = stmt.query_map(params![id,before_authorization], |r| Ok(serde_json::json!({
            "sequence": r.get::<_,i64>(0)?,
            "authorization_id": r.get::<_,String>(1)?,
            "predecessor_operation_id": r.get::<_,String>(2)?,
            "actor": r.get::<_,String>(3)?,
            "note": r.get::<_,String>(4)?,
            "at": r.get::<_,i64>(5)?,
            "action": r.get::<_,Option<String>>(6)?.unwrap_or_else(|| "retry".into()),
            "confirmed_duplicate_risk": r.get::<_,Option<bool>>(7)?,
            "provider_receipt": r.get::<_,Option<String>>(8)?
        }))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        let more_authorizations = authorizations.len() > 200;
        authorizations.truncate(200);
        Ok(Some(serde_json::json!({
            "event":row,
            "next_before_attempt": if more_attempts {
                attempts.last().and_then(|a| a["ordinal"].as_i64())
            } else {
                None
            },
            "next_before_authorization": if more_authorizations {
                authorizations.last().and_then(|a| a["sequence"].as_i64())
            } else {
                None
            },
            "attempts":attempts,"retry_authorizations":authorizations,
        })))
    }

    pub(crate) async fn summary(&self) -> Result<serde_json::Value, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT status,count(*) FROM ingress GROUP BY status")
            .map_err(|e| e.to_string())?;
        let counts = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
            .map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT ingress_id,actor,action,note,at,provider_receipt,confirmed_duplicate_risk
                FROM ingress_resolutions ORDER BY id DESC LIMIT 200")
            .map_err(|e| e.to_string())?;
        let resolutions=stmt.query_map([],|r|Ok(serde_json::json!({
            "ingress_id": r.get::<_,String>(0)?,
            "actor": r.get::<_,String>(1)?,
            "action": r.get::<_,String>(2)?,
            "note": r.get::<_,String>(3)?,
            "at": r.get::<_,i64>(4)?,
            "provider_receipt": r.get::<_,Option<String>>(5)?,
            "confirmed_duplicate_risk": r.get::<_,Option<bool>>(6)?
        }))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT operation_id,ingress_id,ordinal,status,reason,retry_authorization_id
                FROM ingress_attempts ORDER BY finished_at DESC,rowid DESC LIMIT 200")
            .map_err(|e| e.to_string())?;
        let attempts=stmt.query_map([],|r|Ok(serde_json::json!({
            "operation_id": r.get::<_,String>(0)?,
            "ingress_id": r.get::<_,String>(1)?,
            "ordinal": r.get::<_,i64>(2)?,
            "status": r.get::<_,String>(3)?,
            "reason": r.get::<_,Option<String>>(4)?,
            "retry_authorization_id": r.get::<_,Option<String>>(5)?
        }))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT authorization_id,ingress_id,predecessor_operation_id,actor,note,at
                FROM ingress_retry_authorizations ORDER BY at DESC,rowid DESC LIMIT 200")
            .map_err(|e| e.to_string())?;
        let authorizations=stmt.query_map([],|r|Ok(serde_json::json!({
            "authorization_id": r.get::<_,String>(0)?,
            "ingress_id": r.get::<_,String>(1)?,
            "predecessor_operation_id": r.get::<_,String>(2)?,
            "actor": r.get::<_,String>(3)?,
            "note": r.get::<_,String>(4)?,
            "at": r.get::<_,i64>(5)?
        }))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        Ok(
            serde_json::json!({
                "counts": counts,
                "resolutions": resolutions,
                "attempts": attempts,
                "retry_authorizations": authorizations
            }),
        )
    }
    /// `Ok(false)`: the dispatch lease is definitely not ours any more.
    /// `Err`: the inbox could not be read (the caller retries, review N3).
    pub(crate) async fn lease_active(&self, row: &IngressRow, now: i64) -> Result<bool, String> {
        self.conn
            .lock()
            .await
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ingress WHERE id=?1 AND lease_id=?2 AND status='dispatching'
                    AND lease_until>?3)",
                params![row.id, row.lease_id, now],
                |r| r.get::<_, bool>(0),
            )
            .map_err(|e| e.to_string())
    }

    pub(crate) fn connection(&self) -> &Mutex<Connection> {
        &self.conn
    }
}

const ROW_SELECT: &str = "SELECT i.id,i.channel,i.account,i.event_id,i.revision,i.conversation,i.status,i.attempt,
    i.received_at,i.reason,i.lease_id,p.payload,i.authorization_revision,i.seq,i.run_id,i.run_authorization_id,
    i.decision_fastlane,i.decision_binding,i.retry_at,i.unavailable_count,i.run_started_at
    FROM ingress i LEFT JOIN ingress_payload p ON p.id=i.id";
const ROW_SELECT_BY_ID: &str = "SELECT i.id,i.channel,i.account,i.event_id,i.revision,i.conversation,i.status,
    i.attempt,i.received_at,i.reason,i.lease_id,p.payload,i.authorization_revision,i.seq,i.run_id,
    i.run_authorization_id,i.decision_fastlane,i.decision_binding,i.retry_at,i.unavailable_count,i.run_started_at
    FROM ingress i LEFT JOIN ingress_payload p ON p.id=i.id WHERE i.id=?1";
fn row_from_sql(r: &rusqlite::Row<'_>) -> rusqlite::Result<IngressRow> {
    Ok(IngressRow {
        id: r.get(0)?,
        channel: r.get(1)?,
        account: r.get(2)?,
        event_id: r.get(3)?,
        revision: r.get(4)?,
        conversation: r.get(5)?,
        status: r.get(6)?,
        attempt: r.get(7)?,
        received_at: r.get(8)?,
        reason: r.get(9)?,
        lease_id: r.get(10)?,
        payload: r.get(11)?,
        authorization_revision: r.get(12)?,
        seq: r.get(13)?,
        run_id: r.get(14)?,
        run_authorization_id: r.get(15)?,
        decision_fastlane: r.get(16)?,
        decision_binding: r.get(17)?,
        retry_at: r.get(18)?,
        unavailable_count: r.get(19)?,
        run_started_at: r.get(20)?,
    })
}

#[cfg(test)]
mod tests;
