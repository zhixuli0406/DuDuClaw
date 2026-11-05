//! Durable A2A request identities and insertion-ordered, caller-owned pages.

use super::*;

const MESSAGE_CONFLICT: &str = "A2A messageId already used with a different payload";

impl TaskStore {
    pub(super) fn init_a2a_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS a2a_requests (
                owner TEXT NOT NULL,
                message_id TEXT NOT NULL,
                payload_sha256 TEXT NOT NULL,
                task_id TEXT NOT NULL UNIQUE,
                PRIMARY KEY(owner, message_id)
             );
             CREATE TABLE IF NOT EXISTS a2a_task_sequence (
                owner TEXT NOT NULL,
                seq INTEGER NOT NULL,
                task_id TEXT NOT NULL UNIQUE,
                PRIMARY KEY(owner, seq)
             );
             CREATE TRIGGER IF NOT EXISTS a2a_task_sequence_insert
             AFTER INSERT ON tasks WHEN NEW.a2a_owner IS NOT NULL AND NEW.created_via = 'a2a'
             BEGIN
                INSERT INTO a2a_task_sequence(owner,seq,task_id)
                VALUES(NEW.a2a_owner, (SELECT COALESCE(MAX(seq),0)+1 FROM a2a_task_sequence WHERE owner=NEW.a2a_owner), NEW.id);
             END;
             INSERT INTO a2a_task_sequence(owner,seq,task_id)
                SELECT a2a_owner,
                    COALESCE((SELECT MAX(seq) FROM a2a_task_sequence WHERE owner=tasks.a2a_owner),0)
                        + ROW_NUMBER() OVER (PARTITION BY a2a_owner ORDER BY created_at,id), id
                FROM tasks WHERE a2a_owner IS NOT NULL AND created_via = 'a2a'
                    AND NOT EXISTS (SELECT 1 FROM a2a_task_sequence WHERE task_id=tasks.id);"
        ).map_err(|e| format!("init A2A request schema: {e}"))
    }

    pub(crate) async fn a2a_existing_request(
        &self, owner: &str, message_id: &str, payload_sha256: &str,
    ) -> Result<Option<TaskRow>, String> {
        let conn = self.conn.lock().await;
        existing_request(&conn, owner, message_id, payload_sha256)
    }

    /// The request identity and goal are one commit. Receipts survive removal,
    /// so a retry of a deleted task cannot create another execution.
    pub(crate) async fn insert_a2a_task(
        &self, task: &TaskRow, message_id: &str, payload_sha256: &str,
    ) -> Result<Option<TaskRow>, String> {
        let owner = task.a2a_owner.as_deref().filter(|s| !s.is_empty())
            .ok_or_else(|| "A2A owner is required".to_string())?;
        if task.created_via.as_deref() != Some("a2a") || message_id.trim().is_empty()
            || message_id.len() > 256 || payload_sha256.len() != 64 {
            return Err("invalid A2A request identity".into());
        }
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("begin A2A request: {e}"))?;
        if let Some(existing) = existing_request(&tx, owner, message_id, payload_sha256)? {
            return Ok(Some(existing));
        }
        super::tasks::insert_task_tx(&tx, task)?;
        tx.execute("INSERT INTO a2a_requests(owner,message_id,payload_sha256,task_id) VALUES(?1,?2,?3,?4)",
            params![owner, message_id, payload_sha256, task.id])
            .map_err(|e| format!("record A2A request: {e}"))?;
        tx.commit().map_err(|e| format!("commit A2A request: {e}"))?;
        Ok(None)
    }

    pub(crate) async fn a2a_snapshot(&self, owner: &str) -> Result<i64, String> {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT COALESCE(MAX(seq),0) FROM a2a_task_sequence WHERE owner=?1", params![owner], |r| r.get(0))
            .map_err(|e| format!("A2A snapshot: {e}"))
    }

    pub(crate) async fn a2a_tasks_after(
        &self, owner: &str, after: i64, snapshot: i64, limit: usize,
    ) -> Result<Vec<(i64, TaskRow)>, String> {
        if owner.trim().is_empty() || after < 0 || snapshot < after {
            return Err("invalid A2A page boundary".into());
        }
        let conn = self.conn.lock().await;
        let mut statement = conn.prepare(&format!(
            "SELECT {TASK_COLUMNS}, (SELECT seq FROM a2a_task_sequence s WHERE s.task_id = tasks.id) \
             FROM tasks WHERE a2a_owner = ?1 AND id IN \
             (SELECT task_id FROM a2a_task_sequence WHERE owner = ?1 AND seq > ?2 AND seq <= ?3) \
             ORDER BY (SELECT seq FROM a2a_task_sequence s WHERE s.task_id = tasks.id) LIMIT ?4"
        )).map_err(|e| format!("prepare A2A task page: {e}"))?;
        statement.query_map(params![owner, after, snapshot, limit.min(1001) as i64], |row| {
            Ok((row.get(row.as_ref().column_count() - 1)?, row_to_task(row)?))
        }).map_err(|e| format!("query A2A task page: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect A2A task page: {e}"))
    }
}

fn existing_request(conn: &Connection, owner: &str, message_id: &str, payload_sha256: &str) -> Result<Option<TaskRow>, String> {
    let receipt: Option<(String, String)> = conn.query_row(
        "SELECT payload_sha256,task_id FROM a2a_requests WHERE owner=?1 AND message_id=?2",
        params![owner, message_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional().map_err(|e| format!("read A2A request: {e}"))?;
    let Some((digest, id)) = receipt else { return Ok(None); };
    if digest != payload_sha256 { return Err(MESSAGE_CONFLICT.into()); }
    let task = conn.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id=?1 AND a2a_owner=?2"),
        params![id, owner], row_to_task).optional().map_err(|e| format!("read A2A retry task: {e}"))?;
    task.map(Some).ok_or_else(|| "A2A original task is no longer available".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, owner: &str) -> TaskRow {
        let mut row = TaskRow::new(id.into(), "A2A task".into(), String::new(), "medium".into(),
            "sales".into(), format!("a2a:{owner}"));
        row.a2a_owner = Some(owner.into());
        row.created_via = Some("a2a".into());
        row
    }

    #[tokio::test]
    async fn a2a_page_filters_owner_and_keeps_fixed_insertion_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        for (id, owner) in [("z", "user:alice"), ("b", "user:bob"), ("c", "user:alice"), ("a", "user:alice")] {
            store.insert_task(&row(id, owner)).await.unwrap();
        }
        let snapshot = store.a2a_snapshot("user:alice").await.unwrap();
        let first = store.a2a_tasks_after("user:alice", 0, snapshot, 1).await.unwrap();
        assert_eq!(first[0].1.id, "z");
        store.insert_task(&row("0-new", "user:alice")).await.unwrap();
        store.conn.lock().await.execute("UPDATE tasks SET pinned=1,updated_at='2099' WHERE id='z'", []).unwrap();
        let next = store.a2a_tasks_after("user:alice", first[0].0, snapshot, 10).await.unwrap();
        assert_eq!(next.iter().map(|(_, r)| r.id.as_str()).collect::<Vec<_>>(), ["c", "a"]);
        let fresh = store.a2a_tasks_after("user:alice", 0, store.a2a_snapshot("user:alice").await.unwrap(), 10).await.unwrap();
        assert_eq!(fresh.last().unwrap().1.id, "0-new");
        assert!(store.a2a_tasks_after("", 0, snapshot, 10).await.is_err());
        assert!(store.a2a_tasks_after("user:alice-extra", 0, snapshot, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a2a_message_id_is_atomic_durable_and_payload_bound() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let other = TaskStore::open(dir.path()).unwrap();
        let digest = "a".repeat(64);
        let one = row("one", "user:alice");
        let two = row("two", "user:alice");
        let (a, b) = tokio::join!(store.insert_a2a_task(&one, "message", &digest),
            other.insert_a2a_task(&two, "message", &digest));
        let results = [a.unwrap(), b.unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_none()).count(), 1);
        let winner = results.iter().find_map(Option::as_ref).unwrap();
        assert_eq!(store.a2a_tasks_after("user:alice", 0, i64::MAX, 10).await.unwrap().len(), 1);
        drop(other);
        drop(store);
        let reopened = TaskStore::open(dir.path()).unwrap();
        assert_eq!(reopened.a2a_existing_request("user:alice", "message", &digest).await.unwrap().unwrap().id, winner.id);
        assert_eq!(reopened.insert_a2a_task(&row("changed", "user:alice"), "message", &"b".repeat(64)).await.unwrap_err(), MESSAGE_CONFLICT);
        assert!(reopened.insert_a2a_task(&row("bob", "user:bob"), "message", &digest).await.unwrap().is_none());
        reopened.conn.lock().await.execute("DELETE FROM tasks WHERE id=?1", params![winner.id]).unwrap();
        assert!(reopened.insert_a2a_task(&row("retry", "user:alice"), "message", &digest).await.is_err());
        assert!(reopened.get_task("retry").await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a2a_concurrent_connections_create_exactly_one_task() {
        let dir = tempfile::tempdir().unwrap();
        let left = TaskStore::open(dir.path()).unwrap();
        let right = TaskStore::open(dir.path()).unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut jobs = Vec::new();
        for (store, id) in [(left, "left"), (right, "right")] {
            let barrier = barrier.clone();
            jobs.push(tokio::spawn(async move {
                barrier.wait().await;
                store.insert_a2a_task(&row(id, "user:alice"), "same-message", &"a".repeat(64)).await
            }));
        }
        let mut inserted = 0;
        for job in jobs {
            if job.await.unwrap().unwrap().is_none() { inserted += 1; }
        }
        assert_eq!(inserted, 1);
        let reopened = TaskStore::open(dir.path()).unwrap();
        assert_eq!(reopened.a2a_tasks_after("user:alice", 0, i64::MAX, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a2a_failed_insert_does_not_consume_message_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut invalid = row("invalid", "user:alice");
        invalid.timeout_secs = Some(1);
        let digest = "a".repeat(64);
        assert!(store.insert_a2a_task(&invalid, "message", &digest).await.is_err());
        assert!(store.a2a_existing_request("user:alice", "message", &digest).await.unwrap().is_none());
        assert!(store.insert_a2a_task(&row("valid", "user:alice"), "message", &digest).await.unwrap().is_none());
    }
}
