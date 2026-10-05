//! P2-A: the global event cursor (E-H1) and the durable notice log (S-M6).

use super::*;

impl TaskStore {
    /// The global event cursor (`None` until the first event subscription).
    pub async fn event_cursor(&self) -> Result<Option<i64>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT last_event_id FROM responsibility_event_cursor WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("event cursor: {e}"))
    }

    /// Seed the cursor if absent (never replays history).
    pub async fn init_event_cursor(
        &self,
        last_event_id: i64,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO responsibility_event_cursor (singleton, last_event_id, updated_at)
             VALUES (1, ?1, ?2)",
            params![last_event_id, resp_ts(now)],
        )
        .map_err(|e| format!("init event cursor: {e}"))?;
        Ok(())
    }

    /// Move the cursor forward only (`MAX(old, new)`); `force` jumps exactly
    /// (event-gap recovery, which is itself forward-only by construction).
    pub async fn advance_event_cursor(
        &self,
        last_event_id: i64,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO responsibility_event_cursor (singleton, last_event_id, updated_at)
             VALUES (1, ?1, ?2)
             ON CONFLICT(singleton) DO UPDATE SET
               last_event_id = MAX(last_event_id, excluded.last_event_id),
               updated_at = excluded.updated_at",
            params![last_event_id, resp_ts(now)],
        )
        .map_err(|e| format!("advance event cursor: {e}"))?;
        Ok(())
    }

    /// E-H1: the driver saw the feature switched off. Recorded once per
    /// transition (not per tick); the next pass with the feature on jumps
    /// the cursor to the tail instead of replaying what happened meanwhile.
    pub async fn mark_event_cursor_paused(&self, now: DateTime<Utc>) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE responsibility_event_cursor SET paused = 1, updated_at = ?1 WHERE singleton = 1",
            params![resp_ts(now)],
        )
        .map_err(|e| format!("pause event cursor: {e}"))?;
        Ok(())
    }

    /// Clear the pause mark; `true` when it was set (the caller then jumps
    /// the cursor to the tail).
    pub async fn take_event_cursor_paused(&self) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE responsibility_event_cursor SET paused = 0 WHERE singleton = 1 AND paused = 1",
                [],
            )
            .map_err(|e| format!("resume event cursor: {e}"))?;
        Ok(n == 1)
    }

    /// S-M6: claim one notice (once per key, ever). `false` ⇒ already handled.
    /// Only the gateway writes this table; nothing an employee can post
    /// (Activity rows included) moves the per-window count.
    pub async fn claim_notice(
        &self,
        responsibility_id: &str,
        notice_key: &str,
        period_key: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO responsibility_notice_log
                   (responsibility_id, notice_key, period_key, outcome, created_at)
                 VALUES (?1, ?2, ?3, 'claimed', ?4)",
                params![responsibility_id, notice_key, period_key, resp_ts(now)],
            )
            .map_err(|e| format!("claim notice: {e}"))?;
        Ok(n == 1)
    }

    pub async fn set_notice_outcome(
        &self,
        responsibility_id: &str,
        notice_key: &str,
        outcome: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE responsibility_notice_log SET outcome = ?3
              WHERE responsibility_id = ?1 AND notice_key = ?2",
            params![responsibility_id, notice_key, outcome],
        )
        .map_err(|e| format!("notice outcome: {e}"))?;
        Ok(())
    }

    /// Pushes made (sent or deferred to after quiet hours) in one window.
    pub async fn pushed_notices_in_window(
        &self,
        responsibility_id: &str,
        period_key: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM responsibility_notice_log
              WHERE responsibility_id = ?1 AND period_key = ?2 AND outcome IN ('pushed','deferred')",
            params![responsibility_id, period_key],
            |r| r.get(0),
        )
        .map_err(|e| format!("pushed notices: {e}"))
    }

    /// E-M4: occurrences in `window` that an event woke.
    pub async fn event_wakes_in_window(
        &self,
        responsibility_id: &str,
        window: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(DISTINCT f.occurrence_task_id) FROM wakeup_fires f
               JOIN responsibility_occurrences o ON o.task_id = f.occurrence_task_id
              WHERE f.responsibility_id = ?1 AND f.reason = 'event' AND f.state = 'consumed'
                AND o.period_key = ?2",
            params![responsibility_id, window],
            |r| r.get(0),
        )
        .map_err(|e| format!("event wakes in window: {e}"))
    }

    /// Drop every pending fact of one reason (`dropped`, with `drop_reason`).
    pub async fn drop_pending_fires(
        &self,
        responsibility_id: &str,
        reason: &str,
        drop_reason: &str,
        now: DateTime<Utc>,
    ) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE wakeup_fires SET state = 'dropped', drop_reason = ?3, settled_at = ?4
              WHERE responsibility_id = ?1 AND reason = ?2 AND state = 'pending'",
            params![responsibility_id, reason, drop_reason, resp_ts(now)],
        )
        .map_err(|e| format!("drop pending fires: {e}"))
    }
}
