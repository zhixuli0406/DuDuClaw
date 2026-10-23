//! Scheduled assignments: work a person hands an AI employee for later or on
//! a repeat (`crate::assign_schedule`). One row per schedule; each fire
//! creates an ordinary task or goal stamped with the creator as
//! `on_behalf_of`. A new table only, so an old `tasks.db` gains it in place.
//!
//! The creator is always a dashboard account id written by the gateway from
//! the authenticated identity, never from request input.

use super::*;

const ASSIGN_SCHEDULE_SQL: &str = "
CREATE TABLE IF NOT EXISTS assign_schedules (
  id                   TEXT PRIMARY KEY,
  creator_user_id      TEXT NOT NULL,
  agent_id             TEXT NOT NULL,
  kind                 TEXT NOT NULL CHECK(kind IN ('task','goal')),
  title                TEXT NOT NULL,
  payload_json         TEXT NOT NULL,
  schedule_json        TEXT NOT NULL,
  timezone             TEXT,
  status               TEXT NOT NULL CHECK(status IN ('active','paused','done','removed')),
  pause_reason         TEXT,
  created_via          TEXT NOT NULL,
  source_channel       TEXT,
  source_chat_id       TEXT,
  next_fire_at         TEXT,
  last_fire_at         TEXT,
  last_task_id         TEXT,
  last_outcome         TEXT,
  fire_count           INTEGER NOT NULL DEFAULT 0,
  skip_count           INTEGER NOT NULL DEFAULT 0,
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  created_at           TEXT NOT NULL,
  updated_at           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_assign_sched_creator ON assign_schedules(creator_user_id, status);
CREATE INDEX IF NOT EXISTS idx_assign_sched_due ON assign_schedules(status, next_fire_at);
";

const COLUMNS: &str = "id, creator_user_id, agent_id, kind, title, payload_json, schedule_json, \
     timezone, status, pause_reason, created_via, source_channel, source_chat_id, next_fire_at, \
     last_fire_at, last_task_id, last_outcome, fire_count, skip_count, consecutive_failures, \
     created_at, updated_at";

/// Statuses that still hold a slot in the per-creator cap (a paused schedule
/// can be resumed, so it counts).
pub const ASSIGN_SCHEDULE_LIVE_STATUSES: &[&str] = &["active", "paused"];

/// One scheduled assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssignScheduleRow {
    pub id: String,
    /// Dashboard account id of the person who created it.
    pub creator_user_id: String,
    pub agent_id: String,
    /// `task` (a board task) or `goal` (an autonomous goal).
    pub kind: String,
    /// Short display title (the task title / the goal text, truncated).
    pub title: String,
    /// The create request each fire replays (`assign_schedule::AssignPayload`).
    pub payload_json: String,
    /// The schedule (`assign_schedule::ScheduleSpec`).
    pub schedule_json: String,
    /// IANA zone the schedule is read in; `None` = the host's local zone.
    pub timezone: Option<String>,
    /// `active` | `paused` | `done` | `removed`.
    pub status: String,
    /// Why a schedule is paused: `by_user`, `access_lost`,
    /// `agent_unavailable`, `create_failed`, `schedule_restricted`.
    pub pause_reason: Option<String>,
    /// `dashboard`, or the chat channel it was created from.
    pub created_via: String,
    /// The conversation progress goes back to (chat-created schedules).
    pub source_channel: Option<String>,
    pub source_chat_id: Option<String>,
    /// Next due instant (RFC 3339, UTC); `None` once done.
    pub next_fire_at: Option<String>,
    pub last_fire_at: Option<String>,
    pub last_task_id: Option<String>,
    /// `created`, `skipped:<reason>`, `failed`.
    pub last_outcome: Option<String>,
    pub fire_count: i64,
    pub skip_count: i64,
    pub consecutive_failures: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// What one fire did, for [`TaskStore::advance_assign_schedule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleFireOutcome {
    /// A task or goal was created.
    Created { task_id: String },
    /// The slot was skipped (`missed`, `previous_open`); nothing was created.
    Skipped { reason: String },
    /// Creating the task failed.
    Failed,
}

/// Host-owned slot fence carried to the final task insertion, including
/// after an asynchronous plan-first call. Never supplied by a client.
pub(crate) struct AssignScheduleSlot<'a> {
    pub id: &'a str,
    pub slot: &'a str,
    pub next: Option<&'a str>,
    pub now: DateTime<Utc>,
}

fn row_to_schedule(r: &rusqlite::Row<'_>) -> rusqlite::Result<AssignScheduleRow> {
    Ok(AssignScheduleRow {
        id: r.get(0)?,
        creator_user_id: r.get(1)?,
        agent_id: r.get(2)?,
        kind: r.get(3)?,
        title: r.get(4)?,
        payload_json: r.get(5)?,
        schedule_json: r.get(6)?,
        timezone: r.get(7)?,
        status: r.get(8)?,
        pause_reason: r.get(9)?,
        created_via: r.get(10)?,
        source_channel: r.get(11)?,
        source_chat_id: r.get(12)?,
        next_fire_at: r.get(13)?,
        last_fire_at: r.get(14)?,
        last_task_id: r.get(15)?,
        last_outcome: r.get(16)?,
        fire_count: r.get(17)?,
        skip_count: r.get(18)?,
        consecutive_failures: r.get(19)?,
        created_at: r.get(20)?,
        updated_at: r.get(21)?,
    })
}

fn live_status_list() -> String {
    ASSIGN_SCHEDULE_LIVE_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",")
}

impl TaskStore {
    /// Serialize the slot claim, task insertion and slot consumption with
    /// pause/remove across all connections. If control won first, no task
    /// exists; if insertion won first, management acts on the next slot.
    pub(crate) async fn insert_assign_schedule_task(
        &self,
        row: &TaskRow,
        slot: &AssignScheduleSlot<'_>,
    ) -> Result<(), String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("schedule insert: begin: {e}"))?;
        let changed = tx
            .execute(
                "UPDATE assign_schedules SET next_fire_at = ?3, \
               status = CASE WHEN ?3 IS NULL THEN 'done' ELSE status END, \
               last_fire_at = ?4, last_task_id = ?5, last_outcome = 'created', \
               fire_count = fire_count + 1, consecutive_failures = 0, updated_at = ?4 \
             WHERE id = ?1 AND status = 'active' AND next_fire_at = ?2",
                params![slot.id, slot.slot, slot.next, slot.now.to_rfc3339(), row.id],
            )
            .map_err(|e| format!("schedule insert: claim: {e}"))?;
        if changed != 1 {
            return Err("scheduled assignment slot no longer active".into());
        }
        super::tasks::insert_task_tx(&tx, row)?;
        tx.commit()
            .map_err(|e| format!("schedule insert: commit: {e}"))
    }

    pub(super) fn init_assign_schedule_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(ASSIGN_SCHEDULE_SQL)
            .map_err(|e| format!("init assign schedule schema: {e}"))
    }

    /// Insert a new schedule, refusing when the creator already holds
    /// `max_live` active or paused ones (counted in the same transaction, so
    /// two creates at once cannot both slip under the cap). `Ok(false)` =
    /// over the cap.
    pub async fn insert_assign_schedule(
        &self,
        row: &AssignScheduleRow,
        max_live: i64,
    ) -> Result<bool, String> {
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("insert schedule: begin: {e}"))?;
        let live: i64 = tx
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM assign_schedules WHERE creator_user_id = ?1 AND status IN ({})",
                    live_status_list()
                ),
                params![row.creator_user_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("insert schedule: count: {e}"))?;
        if live >= max_live {
            return Ok(false);
        }
        tx.execute(
            &format!("INSERT INTO assign_schedules ({COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)"),
            params![
                row.id,
                row.creator_user_id,
                row.agent_id,
                row.kind,
                row.title,
                row.payload_json,
                row.schedule_json,
                row.timezone,
                row.status,
                row.pause_reason,
                row.created_via,
                row.source_channel,
                row.source_chat_id,
                row.next_fire_at,
                row.last_fire_at,
                row.last_task_id,
                row.last_outcome,
                row.fire_count,
                row.skip_count,
                row.consecutive_failures,
                row.created_at,
                row.updated_at,
            ],
        )
        .map_err(|e| format!("insert schedule: {e}"))?;
        tx.commit()
            .map_err(|e| format!("insert schedule: commit: {e}"))?;
        Ok(true)
    }

    pub async fn get_assign_schedule(&self, id: &str) -> Result<Option<AssignScheduleRow>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM assign_schedules WHERE id = ?1"),
            params![id],
            row_to_schedule,
        )
        .optional()
        .map_err(|e| format!("get schedule: {e}"))
    }

    /// Schedules that are not removed, active/paused first and newest within
    /// each group, so completed history cannot hide the narrowing controls.
    /// One creator's
    /// (`Some(user_id)`) or everyone's (`None`, for Admins). Done one-shot
    /// schedules are listed too (they show what ran). `limit` is clamped to
    /// `[1, 500]`.
    pub async fn list_assign_schedules(
        &self,
        creator_user_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AssignScheduleRow>, String> {
        let limit = limit.clamp(1, 500);
        let conn = self.conn.lock().await;
        let (sql, bind) = match creator_user_id {
            Some(u) => (
                format!(
                    "SELECT {COLUMNS} FROM assign_schedules WHERE creator_user_id = ?1 AND status <> 'removed' \
                     ORDER BY CASE WHEN status IN ({}) THEN 0 ELSE 1 END, created_at DESC LIMIT {limit}",
                    live_status_list()
                ),
                Some(u.to_string()),
            ),
            None => (
                format!(
                    "SELECT {COLUMNS} FROM assign_schedules WHERE status <> 'removed' \
                     ORDER BY CASE WHEN status IN ({}) THEN 0 ELSE 1 END, created_at DESC LIMIT {limit}",
                    live_status_list()
                ),
                None,
            ),
        };
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("list schedules: {e}"))?;
        let rows = match &bind {
            Some(u) => stmt.query_map(params![u], row_to_schedule),
            None => stmt.query_map([], row_to_schedule),
        }
        .map_err(|e| format!("list schedules: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("list schedules: {e}"))?;
        Ok(rows)
    }

    /// Active schedules whose next fire is at or before `now`, oldest first
    /// (at most `limit`).
    pub async fn due_assign_schedules(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<AssignScheduleRow>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {COLUMNS} FROM assign_schedules WHERE status = 'active' AND next_fire_at IS NOT NULL \
                 AND julianday(next_fire_at) <= julianday(?1) ORDER BY next_fire_at LIMIT {}",
                limit.clamp(1, 500)
            ))
            .map_err(|e| format!("due schedules: {e}"))?;
        let rows = stmt
            .query_map(params![now.to_rfc3339()], row_to_schedule)
            .map_err(|e| format!("due schedules: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("due schedules: {e}"))?;
        Ok(rows)
    }

    /// Record one fire of the slot `slot` and move to `next` (`None` = the
    /// schedule is finished). Compare-and-set on the slot and the active
    /// status: `Ok(false)` when someone else already moved it (a pause, a
    /// remove, a second gateway), and nothing is written.
    pub async fn advance_assign_schedule(
        &self,
        id: &str,
        slot: &str,
        next: Option<&str>,
        outcome: &ScheduleFireOutcome,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let now_s = now.to_rfc3339();
        let (task_id, label, fired, skipped, failed): (Option<&str>, String, i64, i64, bool) =
            match outcome {
                ScheduleFireOutcome::Created { task_id } => {
                    (Some(task_id.as_str()), "created".into(), 1, 0, false)
                }
                ScheduleFireOutcome::Skipped { reason } => {
                    (None, format!("skipped:{reason}"), 0, 1, false)
                }
                ScheduleFireOutcome::Failed => (None, "failed".into(), 0, 0, true),
            };
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE assign_schedules SET \
                   next_fire_at = ?3, \
                   status = CASE WHEN ?3 IS NULL THEN 'done' ELSE status END, \
                   last_fire_at = ?4, \
                   last_task_id = COALESCE(?5, last_task_id), \
                   last_outcome = ?6, \
                   fire_count = fire_count + ?7, \
                   skip_count = skip_count + ?8, \
                   consecutive_failures = CASE WHEN ?9 THEN consecutive_failures + 1 ELSE 0 END, \
                   updated_at = ?4 \
                 WHERE id = ?1 AND status = 'active' AND next_fire_at = ?2",
                params![
                    id, slot, next, now_s, task_id, label, fired, skipped, failed
                ],
            )
            .map_err(|e| format!("advance schedule: {e}"))?;
        Ok(changed == 1)
    }

    /// Pause an active schedule with a reason. `Ok(false)` when it was not
    /// active.
    pub async fn pause_assign_schedule(
        &self,
        id: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE assign_schedules SET status = 'paused', pause_reason = ?2, updated_at = ?3 \
                 WHERE id = ?1 AND status = 'active'",
                params![id, reason, now.to_rfc3339()],
            )
            .map_err(|e| format!("pause schedule: {e}"))?;
        Ok(changed == 1)
    }

    /// Resume a paused schedule at `next_fire_at`. `Ok(false)` when it was
    /// not paused.
    pub async fn resume_assign_schedule(
        &self,
        id: &str,
        next_fire_at: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE assign_schedules SET status = 'active', pause_reason = NULL, next_fire_at = ?2, \
                 consecutive_failures = 0, updated_at = ?3 WHERE id = ?1 AND status = 'paused'",
                params![id, next_fire_at, now.to_rfc3339()],
            )
            .map_err(|e| format!("resume schedule: {e}"))?;
        Ok(changed == 1)
    }

    /// Remove a schedule (kept as `removed` for the record; never listed or
    /// fired again). `Ok(false)` when it was already removed or missing.
    pub async fn remove_assign_schedule(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE assign_schedules SET status = 'removed', next_fire_at = NULL, updated_at = ?2 \
                 WHERE id = ?1 AND status <> 'removed'",
                params![id, now.to_rfc3339()],
            )
            .map_err(|e| format!("remove schedule: {e}"))?;
        Ok(changed == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, creator: &str, next: &str) -> AssignScheduleRow {
        let now = Utc::now().to_rfc3339();
        AssignScheduleRow {
            id: id.into(),
            creator_user_id: creator.into(),
            agent_id: "sales".into(),
            kind: "goal".into(),
            title: "weekly report".into(),
            payload_json: "{}".into(),
            schedule_json: "{}".into(),
            timezone: None,
            status: "active".into(),
            pause_reason: None,
            created_via: "dashboard".into(),
            source_channel: None,
            source_chat_id: None,
            next_fire_at: Some(next.into()),
            last_fire_at: None,
            last_task_id: None,
            last_outcome: None,
            fire_count: 0,
            skip_count: 0,
            consecutive_failures: 0,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn completed_history_never_hides_active_or_paused_controls() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        for (id, status) in [("old-active", "active"), ("old-paused", "paused")] {
            let mut schedule = row(id, "u1", "2026-10-24T01:00:00+00:00");
            schedule.created_at = "2025-01-01T00:00:00Z".into();
            schedule.status = status.into();
            store.insert_assign_schedule(&schedule, 200).await.unwrap();
        }
        for n in 0..201 {
            let mut schedule = row(&format!("done-{n}"), "u1", "2026-10-24T01:00:00+00:00");
            schedule.status = "done".into();
            schedule.created_at = "2026-01-01T00:00:00Z".into();
            schedule.next_fire_at = None;
            store.insert_assign_schedule(&schedule, 200).await.unwrap();
        }
        for owner in [Some("u1"), None] {
            let rows = store.list_assign_schedules(owner, 200).await.unwrap();
            assert_eq!(rows.len(), 200);
            assert!(rows[..2].iter().any(|r| r.id == "old-active"));
            assert!(rows[..2].iter().any(|r| r.id == "old-paused"));
        }
    }

    #[tokio::test]
    async fn a_failed_task_insert_rolls_back_the_slot_claim() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let slot = "2026-10-24T01:00:00+00:00";
        store
            .insert_assign_schedule(&row("schedule", "u1", slot), 20)
            .await
            .unwrap();
        let task = TaskRow::new(
            "duplicate".into(),
            "task".into(),
            "".into(),
            "medium".into(),
            "sales".into(),
            "u1".into(),
        );
        store.insert_task(&task).await.unwrap();
        let fence = AssignScheduleSlot {
            id: "schedule",
            slot,
            next: None,
            now: Utc::now(),
        };
        assert!(
            store
                .insert_assign_schedule_task(&task, &fence)
                .await
                .is_err()
        );
        let saved = store
            .get_assign_schedule("schedule")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.status, "active");
        assert_eq!(saved.next_fire_at.as_deref(), Some(slot));
        assert_eq!(saved.fire_count, 0);
        assert!(saved.last_task_id.is_none());
    }

    #[tokio::test]
    async fn the_cap_counts_active_and_paused_rows_of_one_creator() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        assert!(
            store
                .insert_assign_schedule(&row("a", "u1", "2026-10-24T01:00:00+00:00"), 2)
                .await
                .unwrap()
        );
        assert!(
            store
                .insert_assign_schedule(&row("b", "u1", "2026-10-24T01:00:00+00:00"), 2)
                .await
                .unwrap()
        );
        assert!(
            !store
                .insert_assign_schedule(&row("c", "u1", "2026-10-24T01:00:00+00:00"), 2)
                .await
                .unwrap()
        );
        assert!(
            store
                .insert_assign_schedule(&row("d", "u2", "2026-10-24T01:00:00+00:00"), 2)
                .await
                .unwrap()
        );
        assert!(
            store
                .pause_assign_schedule("a", "by_user", Utc::now())
                .await
                .unwrap()
        );
        assert!(
            !store
                .insert_assign_schedule(&row("e", "u1", "2026-10-24T01:00:00+00:00"), 2)
                .await
                .unwrap()
        );
        assert!(store.remove_assign_schedule("a", Utc::now()).await.unwrap());
        assert!(
            store
                .insert_assign_schedule(&row("f", "u1", "2026-10-24T01:00:00+00:00"), 2)
                .await
                .unwrap()
        );
        let mine = store.list_assign_schedules(Some("u1"), 50).await.unwrap();
        assert_eq!(mine.len(), 2, "removed rows are not listed");
        assert_eq!(
            store.list_assign_schedules(None, 50).await.unwrap().len(),
            3
        );
    }

    #[tokio::test]
    async fn advance_is_compare_and_set_and_a_last_slot_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let slot = "2026-10-24T01:00:00+00:00";
        store
            .insert_assign_schedule(&row("a", "u1", slot), 20)
            .await
            .unwrap();
        let due = store
            .due_assign_schedules("2026-10-24T01:00:30Z".parse().unwrap(), 10)
            .await
            .unwrap();
        assert_eq!(due.len(), 1);
        let created = ScheduleFireOutcome::Created {
            task_id: "t1".into(),
        };
        let next = "2026-10-31T01:00:00+00:00";
        assert!(
            store
                .advance_assign_schedule("a", slot, Some(next), &created, Utc::now())
                .await
                .unwrap()
        );
        assert!(
            !store
                .advance_assign_schedule("a", slot, Some(next), &created, Utc::now())
                .await
                .unwrap(),
            "the same slot cannot be advanced twice"
        );
        let r = store.get_assign_schedule("a").await.unwrap().unwrap();
        assert_eq!((r.fire_count, r.last_task_id.as_deref()), (1, Some("t1")));
        assert_eq!(r.next_fire_at.as_deref(), Some(next));
        let skipped = ScheduleFireOutcome::Skipped {
            reason: "previous_open".into(),
        };
        assert!(
            store
                .advance_assign_schedule("a", next, None, &skipped, Utc::now())
                .await
                .unwrap()
        );
        let r = store.get_assign_schedule("a").await.unwrap().unwrap();
        assert_eq!(r.status, "done");
        assert_eq!(r.skip_count, 1);
        assert_eq!(
            r.last_task_id.as_deref(),
            Some("t1"),
            "a skip keeps the last task"
        );
        assert!(
            store
                .due_assign_schedules(Utc::now(), 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_paused_schedule_is_not_due_and_resume_sets_the_next_slot() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        store
            .insert_assign_schedule(&row("a", "u1", "2026-10-24T01:00:00+00:00"), 20)
            .await
            .unwrap();
        assert!(
            store
                .pause_assign_schedule("a", "access_lost", Utc::now())
                .await
                .unwrap()
        );
        assert!(
            !store
                .pause_assign_schedule("a", "by_user", Utc::now())
                .await
                .unwrap()
        );
        assert!(
            store
                .due_assign_schedules("2026-10-25T00:00:00Z".parse().unwrap(), 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !store
                .advance_assign_schedule(
                    "a",
                    "2026-10-24T01:00:00+00:00",
                    None,
                    &ScheduleFireOutcome::Failed,
                    Utc::now()
                )
                .await
                .unwrap()
        );
        assert!(
            store
                .resume_assign_schedule("a", "2026-10-26T01:00:00+00:00", Utc::now())
                .await
                .unwrap()
        );
        let r = store.get_assign_schedule("a").await.unwrap().unwrap();
        assert_eq!((r.status.as_str(), r.pause_reason), ("active", None));
        assert_eq!(r.next_fire_at.as_deref(), Some("2026-10-26T01:00:00+00:00"));
    }
}
