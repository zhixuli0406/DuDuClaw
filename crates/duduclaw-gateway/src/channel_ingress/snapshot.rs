//! Events waiting for their route/authority snapshot (review N1).
//!
//! A webhook stores each event with [`super::PENDING_REVISION`] and the
//! credentials digest its signature was checked with; the snapshot is
//! taken right after the commit. Until it is stored, no worker can claim the
//! event (`claim_lane` skips pending rows). A snapshot that cannot be read
//! backs off (5, 10, 20, 40 s); after [`super::UNAVAILABLE_LIMIT`] attempts,
//! or once the event is older than [`super::SNAPSHOT_MAX_AGE_SECS`] (for
//! example the gateway was down in between), the event is quarantined as
//! `snapshot_unavailable`: the state "as accepted" can no longer be
//! established, so a later configuration is never adopted silently. Such an
//! event never ran and can take a plain `retry`.

use super::{
    Deferred, IngressRow, IngressStore, PENDING_REVISION, ROW_SELECT, SNAPSHOT_MAX_AGE_SECS,
    UNAVAILABLE_LIMIT, row_from_sql, unavailable_backoff,
};
use rusqlite::params;

/// Reason for a pending event whose snapshot could not be taken.
pub(crate) const SNAPSHOT_UNAVAILABLE: &str = "snapshot_unavailable";

impl IngressStore {
    /// Pending rows due for a snapshot attempt (oldest first).
    pub(crate) async fn pending_due(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<IngressRow>, String> {
        let conn = self.connection().lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "{ROW_SELECT} WHERE i.status='ready' AND i.revision=?1
                    AND (i.retry_at IS NULL OR i.retry_at<=?2) ORDER BY i.seq LIMIT ?3"
            ))
            .map_err(|e| e.to_string())?;
        stmt.query_map(params![PENDING_REVISION, now, limit], row_from_sql)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Whether `row` is too old to be snapshotted "as accepted".
    pub(crate) fn snapshot_too_old(row: &IngressRow, now: i64) -> bool {
        now - row.received_at > SNAPSHOT_MAX_AGE_SECS
    }

    /// The snapshot could not be read now: back off, or quarantine at the
    /// limit. CAS on the pending marker and the attempt count, so two
    /// concurrent attempts count once.
    pub(crate) async fn defer_snapshot(
        &self,
        row: &IngressRow,
        reason: &str,
        now: i64,
    ) -> Result<Deferred, String> {
        let count = row.unavailable_count + 1;
        let (status, retry_at, why) = if count >= UNAVAILABLE_LIMIT {
            ("quarantined", None, SNAPSHOT_UNAVAILABLE)
        } else {
            ("ready", Some(now + unavailable_backoff(count - 1)), reason)
        };
        let n = self
            .connection()
            .lock()
            .await
            .execute(
                "UPDATE ingress SET status=?4,reason=?5,retry_at=?6,unavailable_count=?3 WHERE id=?1
                    AND revision=?2 AND status='ready' AND unavailable_count=?7",
                params![
                    row.id,
                    PENDING_REVISION,
                    count,
                    status,
                    why,
                    retry_at,
                    row.unavailable_count
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(match (n, retry_at) {
            (0, _) => Deferred::Lost,
            (_, Some(at)) => Deferred::Retry(at),
            (_, None) => Deferred::Quarantined,
        })
    }

    /// Quarantine a pending event without a snapshot (too old, or the
    /// credentials it was accepted with are gone).
    pub(crate) async fn quarantine_pending(
        &self,
        row: &IngressRow,
        reason: &str,
    ) -> Result<bool, String> {
        self.connection()
            .lock()
            .await
            .execute(
                "UPDATE ingress SET status='quarantined',reason=?3,retry_at=NULL WHERE id=?1
                    AND revision=?2 AND status='ready'",
                params![row.id, PENDING_REVISION, reason],
            )
            .map(|n| n == 1)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_ingress::AcceptedEvent;

    async fn pending(dir: &std::path::Path) -> IngressStore {
        let s = IngressStore::open(dir).unwrap();
        s.append(
            &[AcceptedEvent {
                decision_fastlane: false,
                decision_binding: None,
                event_id: "p".into(),
                account: "a".into(),
                revision: PENDING_REVISION.into(),
                authorization_revision: "pending:cred".into(),
                conversation: "c".into(),
                payload: "{}".into(),
            }],
            10,
        )
        .await
        .unwrap();
        s
    }

    #[tokio::test]
    async fn pending_events_are_not_claimable_until_their_snapshot_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let s = pending(dir.path()).await;
        assert!(s.claim(11).await.unwrap().is_none());
        let row = s.pending_due(11, 10).await.unwrap().remove(0);
        assert!(
            s.store_snapshot(&row.id, "pending:cred", "r", "a")
                .await
                .unwrap()
        );
        assert_eq!(s.claim(12).await.unwrap().unwrap().id, row.id);
    }

    #[tokio::test]
    async fn unreadable_snapshot_backs_off_then_quarantines_retryably() {
        let dir = tempfile::tempdir().unwrap();
        let s = pending(dir.path()).await;
        let mut now = 11;
        for _ in 1..UNAVAILABLE_LIMIT {
            let row = s.pending_due(now, 10).await.unwrap().remove(0);
            let Deferred::Retry(at) = s
                .defer_snapshot(&row, "config_unreadable", now)
                .await
                .unwrap()
            else {
                panic!("expected back-off");
            };
            assert!(
                s.pending_due(now, 10).await.unwrap().is_empty(),
                "not due yet"
            );
            assert!(s.claim(now).await.unwrap().is_none());
            now = at;
        }
        let row = s.pending_due(now, 10).await.unwrap().remove(0);
        assert_eq!(
            s.defer_snapshot(&row, "config_unreadable", now)
                .await
                .unwrap(),
            Deferred::Quarantined
        );
        let held = s.get(&row.id).await.unwrap().unwrap();
        assert_eq!(held.status, "quarantined");
        assert_eq!(held.reason.as_deref(), Some(SNAPSHOT_UNAVAILABLE));
        assert!(super::super::resolve::retry_allowed(
            &held.status,
            held.reason.as_deref()
        ));
        // A stale copy of the row cannot count twice.
        assert_eq!(
            s.defer_snapshot(&row, "config_unreadable", now)
                .await
                .unwrap(),
            Deferred::Lost
        );
    }
}
