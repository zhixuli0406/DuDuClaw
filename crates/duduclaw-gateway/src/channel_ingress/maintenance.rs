//! Lease recovery, payload expiry, retention and the "stuck" query.
//!
//! One maintenance task per home runs these (see `line::ingress::worker`);
//! dispatch workers no longer do.

use super::{BLOCKING, IngressStore};
use rusqlite::{TransactionBehavior, params};

/// Rows that changed state during one recovery pass, for alerts.
#[derive(Debug, Default)]
pub(crate) struct RecoveryReport {
    /// Dispatch leases that expired: the turn may have run.
    pub newly_uncertain: Vec<String>,
    /// Waiting rows whose payload retention ran out.
    pub newly_quarantined: Vec<String>,
}

/// A conversation whose oldest waiting message has waited too long.
#[derive(Debug, Clone)]
pub(crate) struct StuckConversation {
    /// Digest prefix of (account, conversation); never the LINE id itself.
    pub conversation_ref: String,
    pub waiting: i64,
    pub oldest_wait_secs: i64,
    /// The earliest row ahead of it that is not `ready`, if any.
    pub blocker: Option<(String, String)>,
}

fn ids(tx: &rusqlite::Transaction<'_>, sql: &str, now: i64) -> Result<Vec<String>, String> {
    let mut stmt = tx.prepare(sql).map_err(|e| e.to_string())?;
    stmt.query_map([now], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

impl IngressStore {
    pub(crate) async fn recover_and_purge(&self, now: i64) -> Result<RecoveryReport, String> {
        let mut report = RecoveryReport::default();
        let mut conn = self.connection().lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let reclaimed = tx
            .execute(
                "UPDATE ingress SET status='ready',lease_id=NULL,lease_until=NULL,reason='pre_dispatch_lease_expired'
                    WHERE status='claimed' AND lease_until<=?1",
                [now],
            )
            .map_err(|e| e.to_string())?;
        report.newly_uncertain = ids(
            &tx,
            "SELECT id FROM ingress WHERE status='dispatching' AND lease_until<=?1",
            now,
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO ingress_attempts(operation_id,ingress_id,ordinal,status,reason,finished_at,
                retry_authorization_id) SELECT lease_id,id,attempt,'uncertain','dispatch_receipt_missing',?1,
                i.run_authorization_id FROM ingress i WHERE status='dispatching' AND lease_until<=?1",
            [now],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE ingress SET status='uncertain',lease_id=NULL,lease_until=NULL,reason='dispatch_receipt_missing'
                WHERE status='dispatching' AND lease_until<=?1",
            [now],
        )
        .map_err(|e| e.to_string())?;
        report.newly_quarantined = ids(
            &tx,
            "SELECT id FROM ingress WHERE status IN ('ready','claimed') AND id IN (SELECT id FROM ingress_payload
                WHERE expires_at<=?1)",
            now,
        )?;
        tx.execute(
            "UPDATE ingress SET status='quarantined',reason='payload_retention_expired',lease_id=NULL,
                lease_until=NULL WHERE status IN ('ready','claimed') AND id IN (SELECT id FROM ingress_payload
                WHERE expires_at<=?1)",
            [now],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM ingress_payload WHERE expires_at<=?1 AND id NOT IN (SELECT id FROM ingress
                WHERE status='dispatching')",
            [now],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        // Compact erased payload pages out of the WAL.
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(|e| e.to_string())?;
        drop(conn);
        if reclaimed > 0 {
            self.work_signal().notify_waiters();
        }
        Ok(report)
    }

    /// Delete finished events (`completed`, `closed`) received more than
    /// `retention_days` ago, with their attempts, receipts, run
    /// authorizations and resolutions (review I-MEDIUM-6). A LINE redelivery
    /// of a purged event would be accepted again; LINE redelivers within
    /// hours, not months. `undelivered` and `failed_before_dispatch` past
    /// retention are closed first (`retention_closed`) and go with them;
    /// `uncertain` and `quarantined` hold their conversation and are kept
    /// until someone acts.
    pub(crate) async fn purge_finished(
        &self,
        now: i64,
        retention_days: i64,
    ) -> Result<usize, String> {
        let cutoff = now - retention_days.max(1) * 86_400;
        let mut conn = self.connection().lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        // Review L13: an event that ran and was not delivered, or never ran,
        // does not hold its conversation and nobody is forced to close it.
        // Past retention it is closed by the system (its payload went after
        // 24 hours, so it could no longer run again) and purged with the rest.
        tx.execute(
            "UPDATE ingress SET status='closed',reason='retention_closed',lease_id=NULL,lease_until=NULL
                WHERE status IN ('undelivered','failed_before_dispatch') AND received_at<?1",
            [cutoff],
        )
        .map_err(|e| e.to_string())?;
        let gone = ids(
            &tx,
            "SELECT id FROM ingress WHERE status IN ('completed','closed') AND received_at<?1 LIMIT 2000",
            cutoff,
        )?;
        for id in &gone {
            tx.execute("DELETE FROM ingress_payload WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
            // The row goes first: the attempt delete guard only allows
            // removing attempts of an event that no longer exists.
            tx.execute("DELETE FROM ingress WHERE id=?1", [id])
                .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM ingress_attempt_receipts WHERE operation_id IN (SELECT operation_id
                    FROM ingress_attempts WHERE ingress_id=?1)",
                [id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM ingress_attempts WHERE ingress_id=?1", [id])
                .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM ingress_run_confirmations WHERE authorization_id IN (SELECT authorization_id
                    FROM ingress_retry_authorizations WHERE ingress_id=?1)",
                [id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM ingress_retry_authorizations WHERE ingress_id=?1",
                [id],
            )
            .map_err(|e| e.to_string())?;
            tx.execute("DELETE FROM ingress_resolutions WHERE ingress_id=?1", [id])
                .map_err(|e| e.to_string())?;
        }
        tx.execute("DELETE FROM ingress_alerts WHERE at<?1", [cutoff])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(gone.len())
    }

    /// Conversations whose oldest `ready` message has waited longer than
    /// `minutes`, with the row holding them back.
    pub(crate) async fn stuck_conversations(
        &self,
        now: i64,
        minutes: i64,
    ) -> Result<Vec<StuckConversation>, String> {
        if minutes <= 0 {
            return Ok(Vec::new());
        }
        let cutoff = now - minutes * 60;
        let conn = self.connection().lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT channel,account,conversation,count(*),MIN(received_at),MIN(seq) FROM ingress
                    WHERE status='ready' AND decision_fastlane=0 GROUP BY channel,account,conversation
                    HAVING MIN(received_at)<?1",
            )
            .map_err(|e| e.to_string())?;
        let groups = stmt
            .query_map([cutoff], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for (channel, account, conversation, waiting, oldest, first_seq) in groups {
            let blocker = conn
                .query_row(
                    &format!(
                        "SELECT id,status FROM ingress WHERE channel=?1 AND account=?2 AND conversation=?3
                            AND seq<?4 AND decision_fastlane=0 AND status IN ({BLOCKING}) AND status!='ready'
                            ORDER BY seq LIMIT 1"
                    ),
                    params![channel, account, conversation, first_seq],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .ok();
            let full = super::digest(&[&channel, &account, &conversation]);
            out.push(StuckConversation {
                conversation_ref: full.chars().take(12).collect(),
                waiting,
                oldest_wait_secs: now - oldest,
                blocker,
            });
        }
        Ok(out)
    }

    /// Record that an alert with this key was raised. `false` when it already
    /// was, so each condition is announced once.
    pub(crate) async fn claim_alert(&self, key: &str, now: i64) -> Result<bool, String> {
        self.connection()
            .lock()
            .await
            .execute(
                "INSERT OR IGNORE INTO ingress_alerts VALUES (?1,?2)",
                params![key, now],
            )
            .map(|n| n == 1)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_ingress::AcceptedEvent;

    fn event(id: &str, conversation: &str) -> AcceptedEvent {
        AcceptedEvent {
            decision_fastlane: false,
            decision_binding: None,
            event_id: id.into(),
            account: "account".into(),
            revision: "r1".into(),
            authorization_revision: "a1".into(),
            conversation: conversation.into(),
            payload: "{}".into(),
        }
    }

    #[tokio::test]
    async fn retention_purges_finished_history_but_keeps_waiting_events() {
        let dir = tempfile::tempdir().unwrap();
        let s = IngressStore::open(dir.path()).unwrap();
        s.append(&[event("done", "a"), event("waiting", "b")], 10)
            .await
            .unwrap();
        for _ in 0..2 {
            let row = s.claim(10).await.unwrap().unwrap();
            s.transition(&row, "claimed", "dispatching", None)
                .await
                .unwrap();
            let to = if row.event_id == "done" {
                "completed"
            } else {
                "uncertain"
            };
            s.transition(&row, "dispatching", to, None).await.unwrap();
        }
        assert!(s.claim_alert("k", 10).await.unwrap());
        assert!(!s.claim_alert("k", 11).await.unwrap());
        assert_eq!(s.purge_finished(10 + 89 * 86_400, 90).await.unwrap(), 0);
        assert_eq!(s.purge_finished(11 + 90 * 86_400, 90).await.unwrap(), 1);
        let rows = s.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, "waiting");
        let conn = s.connection().lock().await;
        let attempts: i64 = conn
            .query_row("SELECT count(*) FROM ingress_attempts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts, 1, "only the purged event's attempt is gone");
        // While the event exists its attempts stay immutable.
        assert!(conn.execute("DELETE FROM ingress_attempts", []).is_err());
        let alerts: i64 = conn
            .query_row("SELECT count(*) FROM ingress_alerts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(alerts, 0);
    }

    #[tokio::test]
    async fn undelivered_and_never_run_events_are_closed_after_retention() {
        let dir = tempfile::tempdir().unwrap();
        let s = IngressStore::open(dir.path()).unwrap();
        s.append(
            &[event("lost", "a"), event("late", "b"), event("held", "c")],
            10,
        )
        .await
        .unwrap();
        for _ in 0..3 {
            let row = s.claim(10).await.unwrap().unwrap();
            match row.event_id.as_str() {
                "late" => {
                    s.transition(
                        &row,
                        "claimed",
                        "failed_before_dispatch",
                        Some("late_reply_expired"),
                    )
                    .await
                    .unwrap();
                }
                other => {
                    s.transition(&row, "claimed", "dispatching", None)
                        .await
                        .unwrap();
                    let to = if other == "lost" {
                        "undelivered"
                    } else {
                        "uncertain"
                    };
                    s.transition(&row, "dispatching", to, None).await.unwrap();
                }
            }
        }
        assert_eq!(s.purge_finished(10 + 89 * 86_400, 90).await.unwrap(), 0);
        assert_eq!(s.list().await.unwrap().len(), 3);
        assert_eq!(s.purge_finished(11 + 90 * 86_400, 90).await.unwrap(), 2);
        let rows = s.list().await.unwrap();
        assert_eq!(rows.len(), 1, "an uncertain event still waits for a person");
        assert_eq!(rows[0].event_id, "held");
    }

    #[tokio::test]
    async fn stuck_conversation_names_its_blocker_without_the_line_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = IngressStore::open(dir.path()).unwrap();
        s.append(&[event("first", "Uuser"), event("second", "Uuser")], 10)
            .await
            .unwrap();
        let first = s.claim(10).await.unwrap().unwrap();
        s.transition(&first, "claimed", "dispatching", None)
            .await
            .unwrap();
        s.transition(&first, "dispatching", "uncertain", Some("x"))
            .await
            .unwrap();
        assert!(s.stuck_conversations(10 + 60, 15).await.unwrap().is_empty());
        let stuck = s.stuck_conversations(10 + 16 * 60, 15).await.unwrap();
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0].waiting, 1);
        assert_eq!(
            stuck[0].blocker.as_ref().map(|b| b.1.as_str()),
            Some("uncertain")
        );
        assert!(!stuck[0].conversation_ref.contains("Uuser"));
    }
}
