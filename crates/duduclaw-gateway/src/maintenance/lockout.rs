//! Password re-entry lockout for `maintenance.enable` (design §6.1, L5,
//! R2-L11): five consecutive wrong passwords for one account lock opening
//! maintenance mode for that account for fifteen minutes. The counter lives in
//! `maintenance.db` so a gateway restart does not reset it.
//!
//! The caller checks [`MaintenanceStore::reauth_locked_until`] BEFORE it calls
//! `verify_password`, and does not call it at all while locked, so a locked
//! account gives no signal (result or timing) about whether a guess was right.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::MaintenanceStore;

/// Consecutive failures that lock the account.
pub const REAUTH_MAX_FAILURES: u32 = 5;
/// How long the lock lasts.
pub const REAUTH_LOCK_MINUTES: i64 = 15;

/// Result of recording one wrong password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReauthFailure {
    /// Not locked yet; this many tries are left before the lock.
    Remaining(u32),
    /// This failure locked the account until the given time.
    LockedNow(DateTime<Utc>),
}

impl MaintenanceStore {
    /// When the account's lock ends, if it is locked at `now`. A read error is
    /// an `Err`, which the caller treats as "refuse" (a gate that cannot read
    /// its state denies), never as "not locked".
    pub async fn reauth_locked_until(
        &self,
        user_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let conn = self.conn.lock().await;
        let raw: Option<Option<String>> = conn
            .query_row(
                "SELECT locked_until FROM maintenance_reauth WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("read maintenance re-auth lock: {e}"))?;
        let Some(Some(text)) = raw else {
            return Ok(None);
        };
        let until = DateTime::parse_from_rfc3339(&text)
            .map_err(|e| format!("unreadable maintenance re-auth lock time: {e}"))?
            .with_timezone(&Utc);
        Ok((until > now).then_some(until))
    }

    /// Count one wrong password. The fifth in a row sets the lock and resets
    /// the counter, so after the lock ends the account again has five tries.
    pub async fn record_reauth_failure(
        &self,
        user_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ReauthFailure, String> {
        let conn = self.conn.lock().await;
        let failures: u32 = conn
            .query_row(
                "SELECT consecutive_failures FROM maintenance_reauth WHERE user_id = ?1",
                params![user_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| format!("read maintenance re-auth failures: {e}"))?
            .map(|n| n.clamp(0, i64::from(u32::MAX)) as u32)
            .unwrap_or(0)
            .saturating_add(1);
        if failures >= REAUTH_MAX_FAILURES {
            let until = now + chrono::Duration::minutes(REAUTH_LOCK_MINUTES);
            conn.execute(
                "INSERT INTO maintenance_reauth (user_id, consecutive_failures, locked_until)
                 VALUES (?1, 0, ?2)
                 ON CONFLICT(user_id) DO UPDATE SET consecutive_failures = 0, locked_until = ?2",
                params![user_id, until.to_rfc3339()],
            )
            .map_err(|e| format!("write maintenance re-auth lock: {e}"))?;
            return Ok(ReauthFailure::LockedNow(until));
        }
        conn.execute(
            "INSERT INTO maintenance_reauth (user_id, consecutive_failures, locked_until)
             VALUES (?1, ?2, NULL)
             ON CONFLICT(user_id) DO UPDATE SET consecutive_failures = ?2",
            params![user_id, failures],
        )
        .map_err(|e| format!("write maintenance re-auth failures: {e}"))?;
        Ok(ReauthFailure::Remaining(REAUTH_MAX_FAILURES - failures))
    }

    /// A correct password resets the run of failures.
    pub async fn clear_reauth_failures(&self, user_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE maintenance_reauth SET consecutive_failures = 0 WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(|e| format!("reset maintenance re-auth failures: {e}"))?;
        Ok(())
    }

    /// Remember a mirrored sysd event; `true` the first time a cursor is seen
    /// for a window (the gateway's dedup for the audit mirror, design §6.2).
    pub async fn mark_ssh_event_seen(&self, window_id: &str, cursor: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO maintenance_ssh_events (window_id, cursor) VALUES (?1, ?2)",
                params![window_id, cursor],
            )
            .map_err(|e| format!("record mirrored SSH event: {e}"))?;
        Ok(n == 1)
    }

    /// Drop the dedup rows of windows other than `keep` (sysd's event buffer
    /// starts empty for every window, so older rows are never needed again).
    pub async fn forget_ssh_events_except(&self, keep: Option<&str>) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM maintenance_ssh_events
              WHERE (?1 IS NULL OR window_id <> ?1) AND window_id <> '__closes__'",
            params![keep],
        )
        .map_err(|e| format!("prune mirrored SSH events: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn five_failures_lock_for_fifteen_minutes_and_reset_the_counter() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let now = Utc::now();
        for left in (1..REAUTH_MAX_FAILURES).rev() {
            assert_eq!(
                store.record_reauth_failure("u1", now).await.unwrap(),
                ReauthFailure::Remaining(left)
            );
            assert_eq!(store.reauth_locked_until("u1", now).await.unwrap(), None);
        }
        let ReauthFailure::LockedNow(until) = store.record_reauth_failure("u1", now).await.unwrap()
        else {
            panic!("fifth failure must lock");
        };
        assert_eq!(until, now + chrono::Duration::minutes(REAUTH_LOCK_MINUTES));
        assert_eq!(
            store.reauth_locked_until("u1", now).await.unwrap(),
            Some(until)
        );
        // Another account is unaffected.
        assert_eq!(store.reauth_locked_until("u2", now).await.unwrap(), None);
        // After the lock ends the account has five tries again.
        let later = until + chrono::Duration::seconds(1);
        assert_eq!(store.reauth_locked_until("u1", later).await.unwrap(), None);
        assert_eq!(
            store.record_reauth_failure("u1", later).await.unwrap(),
            ReauthFailure::Remaining(REAUTH_MAX_FAILURES - 1)
        );
    }

    #[tokio::test]
    async fn a_success_resets_the_run_of_failures() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        let now = Utc::now();
        for _ in 0..4 {
            store.record_reauth_failure("u1", now).await.unwrap();
        }
        store.clear_reauth_failures("u1").await.unwrap();
        assert_eq!(
            store.record_reauth_failure("u1", now).await.unwrap(),
            ReauthFailure::Remaining(REAUTH_MAX_FAILURES - 1)
        );
    }

    #[tokio::test]
    async fn the_lock_survives_reopening_the_database() {
        let home = tempfile::tempdir().unwrap();
        let now = Utc::now();
        {
            let store = MaintenanceStore::open(home.path()).unwrap();
            for _ in 0..REAUTH_MAX_FAILURES {
                store.record_reauth_failure("u1", now).await.unwrap();
            }
        }
        let store = MaintenanceStore::open(home.path()).unwrap();
        assert!(
            store
                .reauth_locked_until("u1", now)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn event_dedup_is_per_window_and_prunable() {
        let store = MaintenanceStore::open_in_memory().unwrap();
        assert!(store.mark_ssh_event_seen("w1", "c1").await.unwrap());
        assert!(!store.mark_ssh_event_seen("w1", "c1").await.unwrap());
        assert!(store.mark_ssh_event_seen("w2", "c1").await.unwrap());
        store.forget_ssh_events_except(Some("w2")).await.unwrap();
        assert!(store.mark_ssh_event_seen("w1", "c1").await.unwrap());
        assert!(!store.mark_ssh_event_seen("w2", "c1").await.unwrap());
    }
}
