//! Per-employee work-status counts for the external status endpoint
//! (RM-064, `GET /api/v1/status/employees`). Read-only, and deliberately
//! narrow: counts plus the latest finished task's title and completion time.
//! No description, result, feedback or id leaves the gateway.
//!
//! ## Privacy
//!
//! The counts are computed for one dashboard reader (`user:<id>`), never for
//! "the employee" as a whole: two accounts bound to the same employee must
//! not learn about each other's private work through a number. The SQL
//! applies the same host-stamped A2A owner rule the dashboard task list uses
//! (`a2a_owner IS NULL OR a2a_owner = <principal>`, plus the
//! `task_access_principals` registry), and returns one row per candidate
//! task so the caller can apply the TaskPacket audience — which lives on
//! disk, not in SQL — before anything is counted ([`AgentStatusCounts::tally`]).

use std::collections::HashMap;

use super::*;

/// Work the employee is doing or has queued.
pub(crate) const IN_PROGRESS_STATUSES: &[&str] = &["todo", "pending", "in_progress", "revising"];
/// Work waiting on a person (review, a human decision, an approval).
pub(crate) const AWAITING_STATUSES: &[&str] = &["review", "needs_human", "pending_approval"];
/// Finished tasks older than the 7-day window still considered per employee
/// when looking for the latest *visible* title. If all of them are hidden from
/// the reader the employee simply has no `latest_done`.
pub(crate) const LATEST_SCAN_PER_AGENT: i64 = 32;
/// Candidate rows one snapshot may examine. More is refused (an error, never
/// an undercount) so a single request cannot turn into an unbounded scan of
/// packet directories.
pub(crate) const MAX_STATUS_ROWS: usize = 20_000;

/// One employee's counts. Archived rows are excluded from the open buckets
/// (archiving takes work out of consideration) but still count as done: a
/// task finished this morning and archived at noon was still finished today.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentStatusCounts {
    pub in_progress: i64,
    /// Tasks only; the caller adds pending approvals.
    pub awaiting_tasks: i64,
    pub done_today: i64,
    pub done_7d: i64,
    /// `(title, completed_at)` of the most recently completed task the
    /// reader may see, raw — the caller masks and truncates the title.
    pub latest_done: Option<(String, String)>,
}

/// Which bucket a candidate task falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBucket {
    InProgress,
    Awaiting,
    Done,
}

/// One candidate task for the status counts. `id` is only used to look up
/// the task's audience; it never leaves the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusTaskRow {
    pub id: String,
    pub agent: String,
    pub bucket: StatusBucket,
    pub title: String,
    pub completed_at: Option<String>,
    pub done_today: bool,
    pub done_7d: bool,
}

fn sql_list(statuses: &[&str]) -> String {
    statuses.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",")
}

impl AgentStatusCounts {
    /// Fold `rows` (in the order [`TaskStore::status_rows_for_principal`]
    /// returns them: per employee, newest finished first) into per-employee
    /// counts, keeping only rows `visible` admits. A `visible` error aborts
    /// the whole tally — an unreadable ACL is never a zero or a skip.
    pub fn tally(
        rows: &[StatusTaskRow],
        mut visible: impl FnMut(&StatusTaskRow) -> Result<bool, String>,
    ) -> Result<HashMap<String, AgentStatusCounts>, String> {
        let mut out: HashMap<String, AgentStatusCounts> = HashMap::new();
        for row in rows {
            if !visible(row)? {
                continue;
            }
            let counts = out.entry(row.agent.clone()).or_default();
            match row.bucket {
                StatusBucket::InProgress => counts.in_progress += 1,
                StatusBucket::Awaiting => counts.awaiting_tasks += 1,
                StatusBucket::Done => {
                    counts.done_today += i64::from(row.done_today);
                    counts.done_7d += i64::from(row.done_7d);
                    if counts.latest_done.is_none() {
                        if let Some(at) = &row.completed_at {
                            counts.latest_done = Some((row.title.clone(), at.clone()));
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}

impl TaskStore {
    /// Candidate rows for `agents` as seen by `principal` (`user:<id>`):
    /// open and awaiting work (not archived), every task finished since
    /// `window_start`, and the [`LATEST_SCAN_PER_AGENT`] most recently
    /// finished ones before it. Tasks owned by another A2A principal are
    /// never returned. Ordered per employee, newest finished first. More
    /// than [`MAX_COUNTED_AGENTS`] names or [`MAX_STATUS_ROWS`] rows is
    /// refused rather than truncated.
    pub async fn status_rows_for_principal(
        &self,
        agents: &[String],
        today_start: DateTime<Utc>,
        window_start: DateTime<Utc>,
        principal: &str,
    ) -> Result<Vec<StatusTaskRow>, String> {
        if agents.len() > MAX_COUNTED_AGENTS {
            return Err(format!(
                "too many agents to count ({} > {MAX_COUNTED_AGENTS})",
                agents.len()
            ));
        }
        if principal.trim().is_empty() {
            return Err("status counts need a reader principal".into());
        }
        if agents.is_empty() {
            return Ok(Vec::new());
        }
        let names_json = serde_json::to_string(agents).map_err(|e| e.to_string())?;
        // `julianday` normalises the mixed RFC3339 spellings writers use
        // (`Z` / `+00:00`, 0-9 fractional digits); an unparseable stamp is
        // NULL and simply never counts as done.
        let sql = format!(
            "WITH scoped AS (
                 SELECT t.id, t.assigned_to, t.status, t.archived, t.title, t.completed_at,
                        julianday(t.completed_at) AS done_jd
                   FROM tasks t
                  WHERE t.assigned_to IN (SELECT value FROM json_each(?1))
                    AND t.kind IN ('task','goal')
                    AND (t.a2a_owner IS NULL OR t.a2a_owner = ?4)
                    AND NOT EXISTS (SELECT 1 FROM task_access_principals p
                                     WHERE p.task_id = t.id AND p.principal IS NOT ?4)
             ),
             ranked AS (
                 SELECT id, assigned_to, status, archived, title, completed_at, done_jd,
                        ROW_NUMBER() OVER (
                            PARTITION BY assigned_to, (status = 'done' AND done_jd IS NOT NULL)
                            ORDER BY done_jd DESC, id DESC) AS done_rn
                   FROM scoped
             )
             SELECT id, assigned_to, status, title, completed_at,
                    COALESCE(done_jd >= julianday(?2), 0) AS is_today,
                    COALESCE(done_jd >= julianday(?3), 0) AS in_window
               FROM ranked
              WHERE (archived = 0 AND status IN ({open},{awaiting}))
                 OR (status = 'done' AND done_jd IS NOT NULL
                     AND (done_jd >= julianday(?3) OR done_rn <= {scan}))
              ORDER BY assigned_to, done_jd IS NULL, done_jd DESC, id DESC
              LIMIT {limit}",
            open = sql_list(IN_PROGRESS_STATUSES),
            awaiting = sql_list(AWAITING_STATUSES),
            scan = LATEST_SCAN_PER_AGENT,
            limit = MAX_STATUS_ROWS + 1,
        );
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare status counts: {e}"))?;
        let rows = stmt
            .query_map(
                params![
                    names_json,
                    today_start.to_rfc3339(),
                    window_start.to_rfc3339(),
                    principal
                ],
                |row| {
                    let status: String = row.get(2)?;
                    let bucket = if status == "done" {
                        StatusBucket::Done
                    } else if AWAITING_STATUSES.contains(&status.as_str()) {
                        StatusBucket::Awaiting
                    } else {
                        StatusBucket::InProgress
                    };
                    Ok(StatusTaskRow {
                        id: row.get(0)?,
                        agent: row.get(1)?,
                        bucket,
                        title: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        completed_at: row.get(4)?,
                        done_today: row.get::<_, i64>(5)? != 0,
                        done_7d: row.get::<_, i64>(6)? != 0,
                    })
                },
            )
            .map_err(|e| format!("query status counts: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("status count row: {e}"))?);
            if out.len() > MAX_STATUS_ROWS {
                return Err(format!("too many tasks to count (> {MAX_STATUS_ROWS})"));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    async fn put(store: &TaskStore, id: &str, agent: &str, status: &str, completed: Option<DateTime<Utc>>) {
        put_owned(store, id, agent, status, completed, None).await;
    }

    async fn put_owned(
        store: &TaskStore,
        id: &str,
        agent: &str,
        status: &str,
        completed: Option<DateTime<Utc>>,
        a2a_owner: Option<&str>,
    ) {
        let mut row = TaskRow::new(
            id.to_string(),
            format!("title-{id}"),
            "secret description".to_string(),
            "medium".to_string(),
            agent.to_string(),
            "u1".to_string(),
        );
        row.status = status.to_string();
        row.completed_at = completed.map(|t| t.to_rfc3339());
        row.a2a_owner = a2a_owner.map(str::to_string);
        store.insert_task(&row).await.unwrap();
    }

    fn all(_: &StatusTaskRow) -> Result<bool, String> {
        Ok(true)
    }

    #[tokio::test]
    async fn buckets_windows_and_latest_title_per_agent() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        let now = Utc::now();
        let today = now - Duration::hours(1);
        let window = now - Duration::days(7);
        for (id, status) in [("1", "todo"), ("2", "in_progress"), ("3", "pending"), ("4", "revising"),
                             ("5", "review"), ("6", "needs_human"), ("7", "pending_approval"),
                             ("8", "failed"), ("9", "cancelled"), ("10", "blocked")] {
            put(&store, id, "sales", status, None).await;
        }
        put(&store, "d1", "sales", "done", Some(now - Duration::minutes(5))).await;
        put(&store, "d2", "sales", "done", Some(now - Duration::days(2))).await;
        put(&store, "d3", "sales", "done", Some(now - Duration::days(30))).await;
        put(&store, "d4", "hr", "done", Some(now - Duration::days(3))).await;
        put(&store, "x", "finance", "todo", None).await;
        // Archived open work is not open; archived done work still counts.
        put(&store, "a1", "sales", "todo", None).await;
        store.update_task("a1", &serde_json::json!({ "archived": true })).await.unwrap();

        let rows = store
            .status_rows_for_principal(&["sales".into(), "hr".into(), "nobody".into()], today, window, "user:u-alice")
            .await
            .unwrap();
        assert!(rows.iter().all(|r| r.agent != "finance"), "only the agents asked about");
        let counts = AgentStatusCounts::tally(&rows, all).unwrap();
        let sales = &counts["sales"];
        assert_eq!(sales.in_progress, 4);
        assert_eq!(sales.awaiting_tasks, 3);
        assert_eq!(sales.done_today, 1);
        assert_eq!(sales.done_7d, 2);
        assert_eq!(sales.latest_done.as_ref().map(|(t, _)| t.as_str()), Some("title-d1"));
        let hr = &counts["hr"];
        assert_eq!((hr.in_progress, hr.awaiting_tasks, hr.done_today, hr.done_7d), (0, 0, 0, 1));
        assert_eq!(hr.latest_done.as_ref().map(|(t, _)| t.as_str()), Some("title-d4"));
        assert!(!counts.contains_key("nobody"));
        assert!(store.status_rows_for_principal(&[], today, window, "user:u").await.unwrap().is_empty());
        assert!(store.status_rows_for_principal(&["sales".into()], today, window, " ").await.is_err());
        let too_many: Vec<String> = (0..=MAX_COUNTED_AGENTS).map(|i| format!("a{i}")).collect();
        assert!(store.status_rows_for_principal(&too_many, today, window, "user:u").await.is_err());
    }

    #[tokio::test]
    async fn another_principals_a2a_tasks_never_count_or_title() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        let now = Utc::now();
        let (today, window) = (now - Duration::hours(6), now - Duration::days(7));
        put(&store, "mine-open", "sales", "todo", None).await;
        put_owned(&store, "bob-open", "sales", "in_progress", None, Some("user:bob")).await;
        put_owned(&store, "bob-review", "sales", "review", None, Some("user:bob")).await;
        put(&store, "shared-done", "sales", "done", Some(now - Duration::hours(3))).await;
        put_owned(&store, "bob-done", "sales", "done", Some(now - Duration::minutes(1)), Some("user:bob")).await;
        put_owned(&store, "alice-done", "sales", "done", Some(now - Duration::minutes(30)), Some("user:alice")).await;

        let as_alice = store
            .status_rows_for_principal(&["sales".into()], today, window, "user:alice")
            .await
            .unwrap();
        assert!(as_alice.iter().all(|r| !r.id.starts_with("bob-")), "{as_alice:?}");
        let alice = &AgentStatusCounts::tally(&as_alice, all).unwrap()["sales"];
        assert_eq!((alice.in_progress, alice.awaiting_tasks, alice.done_today, alice.done_7d), (1, 0, 2, 2));
        assert_eq!(alice.latest_done.as_ref().map(|(t, _)| t.as_str()), Some("title-alice-done"));

        let as_bob = store
            .status_rows_for_principal(&["sales".into()], today, window, "user:bob")
            .await
            .unwrap();
        let bob = &AgentStatusCounts::tally(&as_bob, all).unwrap()["sales"];
        assert_eq!((bob.in_progress, bob.awaiting_tasks, bob.done_today), (2, 1, 2));
        assert_eq!(bob.latest_done.as_ref().map(|(t, _)| t.as_str()), Some("title-bob-done"));

        let as_carol = store
            .status_rows_for_principal(&["sales".into()], today, window, "user:carol")
            .await
            .unwrap();
        let carol = &AgentStatusCounts::tally(&as_carol, all).unwrap()["sales"];
        assert_eq!((carol.in_progress, carol.done_today), (1, 1));
        assert_eq!(carol.latest_done.as_ref().map(|(t, _)| t.as_str()), Some("title-shared-done"));
    }

    #[tokio::test]
    async fn hidden_rows_are_skipped_for_counts_and_title_and_errors_abort() {
        let home = tempfile::tempdir().unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        let now = Utc::now();
        let (today, window) = (now - Duration::hours(1), now - Duration::days(7));
        put(&store, "open-hidden", "sales", "todo", None).await;
        put(&store, "done-hidden", "sales", "done", Some(now - Duration::minutes(1))).await;
        put(&store, "done-old", "sales", "done", Some(now - Duration::days(40))).await;
        let rows = store
            .status_rows_for_principal(&["sales".into()], today, window, "user:alice")
            .await
            .unwrap();
        let counts = AgentStatusCounts::tally(&rows, |r| Ok(!r.id.ends_with("-hidden"))).unwrap();
        let sales = &counts["sales"];
        assert_eq!((sales.in_progress, sales.done_today, sales.done_7d), (0, 0, 0));
        // The latest *visible* finished task, even outside the window.
        assert_eq!(sales.latest_done.as_ref().map(|(t, _)| t.as_str()), Some("title-done-old"));
        assert!(AgentStatusCounts::tally(&rows, |_| Err("acl unreadable".into())).is_err());
    }
}
