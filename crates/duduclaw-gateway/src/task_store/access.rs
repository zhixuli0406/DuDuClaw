//! Host-owned A2A principals survive deletion so retained evidence stays private.
use super::*;

impl TaskStore {
    /// A missing registry is migratable only before any A2A state existed.
    /// Rebuilding from live tasks cannot recover deleted-task owner tombstones.
    pub(super) fn validate_task_access_schema(conn: &Connection) -> Result<(), String> {
        let registry_type: Option<String> = conn.query_row(
            "SELECT type FROM sqlite_master WHERE name = 'task_access_principals'",
            [], |r| r.get(0),
        ).optional().map_err(|e| format!("task access schema inspection: {e}"))?;
        if let Some(kind) = registry_type {
            if kind != "table" {
                return Err("A2A task access registry is not a table".into());
            }
            // Read both columns, even on an empty table, and reject invalid rows.
            let invalid: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM task_access_principals
                 WHERE typeof(task_id) != 'text' OR task_id = ''
                    OR typeof(principal) != 'text' OR principal = '')",
                [], |r| r.get(0),
            ).map_err(|e| format!("A2A task access registry is unreadable: {e}"))?;
            if invalid { return Err("A2A task access registry has invalid rows".into()); }
            return Ok(());
        }
        let has_a2a_schema: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'a2a_owner')
             OR EXISTS (SELECT 1 FROM sqlite_master
                 WHERE name GLOB 'a2a_*' OR name GLOB 'task_a2a_*')",
            [], |r| r.get(0),
        ).map_err(|e| format!("task access original schema: {e}"))?;
        let has_creator: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'created_by')",
            [], |r| r.get(0),
        ).map_err(|e| format!("task access original creator: {e}"))?;
        let has_a2a_rows = if has_creator {
            conn.query_row("SELECT EXISTS (SELECT 1 FROM tasks WHERE created_by GLOB 'a2a:*')",
                [], |r| r.get::<_, bool>(0))
                .map_err(|e| format!("task access original owner: {e}"))?
        } else { false };
        if has_a2a_schema || has_a2a_rows {
            return Err("A2A task access registry is missing; restore it before opening the store".into());
        }
        Ok(())
    }

    pub(super) fn init_task_access_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_access_principals (
                 task_id TEXT PRIMARY KEY,
                 principal TEXT NOT NULL
             );
             UPDATE tasks SET a2a_owner = substr(created_by, 5)
             WHERE a2a_owner IS NULL AND created_by GLOB 'a2a:user:?*';
             WITH RECURSIVE inherited(id, principal) AS (
                 SELECT id, a2a_owner FROM tasks WHERE a2a_owner IS NOT NULL
                 UNION SELECT t.id, i.principal FROM tasks t JOIN inherited i ON t.parent_task_id = i.id
             ) UPDATE tasks SET a2a_owner = (SELECT principal FROM inherited WHERE inherited.id = tasks.id)
               WHERE a2a_owner IS NULL AND id IN (SELECT id FROM inherited);
             INSERT OR IGNORE INTO task_access_principals
                 SELECT id, a2a_owner FROM tasks WHERE a2a_owner IS NOT NULL;
             CREATE TRIGGER IF NOT EXISTS task_a2a_owner_immutable
             BEFORE UPDATE OF a2a_owner ON tasks
             WHEN OLD.a2a_owner IS NOT NULL AND NEW.a2a_owner IS NOT OLD.a2a_owner
             BEGIN SELECT RAISE(ABORT, 'A2A task owner is immutable'); END;
             CREATE TRIGGER IF NOT EXISTS task_a2a_parent_update
             BEFORE UPDATE OF parent_task_id ON tasks
             WHEN (OLD.a2a_owner IS NOT NULL AND NEW.parent_task_id IS NOT OLD.parent_task_id)
                 OR EXISTS (SELECT 1 FROM task_access_principals
                 WHERE task_id = NEW.parent_task_id AND principal IS NOT NEW.a2a_owner)
             BEGIN SELECT RAISE(ABORT, 'A2A parent owner mismatch'); END;
             CREATE TRIGGER IF NOT EXISTS task_a2a_owner_insert
             AFTER INSERT ON tasks
             BEGIN
                 SELECT CASE WHEN EXISTS (SELECT 1 FROM task_access_principals
                     WHERE task_id = NEW.id)
                     THEN RAISE(ABORT, 'A2A task id cannot be reused') END;
                 SELECT CASE WHEN NEW.a2a_owner IS NOT NULL AND NEW.parent_task_id IS NOT NULL
                     AND NOT EXISTS (SELECT 1 FROM task_access_principals
                     WHERE task_id = NEW.parent_task_id AND principal = NEW.a2a_owner)
                     THEN RAISE(ABORT, 'A2A parent owner mismatch') END;
                 UPDATE tasks SET a2a_owner = COALESCE(NEW.a2a_owner,
                     (SELECT principal FROM task_access_principals WHERE task_id = NEW.parent_task_id))
                     WHERE id = NEW.id;
                 INSERT INTO task_access_principals SELECT id, a2a_owner FROM tasks
                     WHERE id = NEW.id AND a2a_owner IS NOT NULL;
             END;",
        ).map_err(|e| format!("task access schema: {e}"))
    }

    /// No Admin override: an operator exception requires a separate audited path.
    /// This read also covers evidence whose task row has been deleted.
    pub fn a2a_principal_allows(home: &Path, task: &str, principal: &str) -> bool {
        Self::a2a_owner_readonly(home, task)
            .is_ok_and(|owner| owner.is_none_or(|owner| owner == principal))
    }

    pub fn a2a_owner_readonly(home: &Path, task: &str) -> Result<Option<String>, String> {
        let mut conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db"))?;
        // Keep schema detection and the owner lookup in one read snapshot.
        // Export/evidence readers must not initialize or migrate the store.
        let tx = conn.transaction().map_err(|e| format!("task access snapshot: {e}"))?;
        let registry_exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'task_access_principals')",
            [], |r| r.get(0),
        ).map_err(|e| format!("task access schema: {e}"))?;
        if !registry_exists {
            // Only a demonstrably pre-A2A schema has no owner restriction.
            // A missing registry in an A2A-capable database remains an error,
            // including when the protected task itself has been deleted.
            let legacy: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tasks')
                 AND NOT EXISTS (SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'a2a_owner')
                 AND NOT EXISTS (SELECT 1 FROM sqlite_master
                     WHERE name GLOB 'a2a_*' OR name GLOB 'task_a2a_*')",
                [], |r| r.get(0),
            ).map_err(|e| format!("task access legacy schema: {e}"))?;
            if !legacy { return Err("A2A task access registry is missing".into()); }
            let has_creator: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'created_by')",
                [], |r| r.get(0),
            ).map_err(|e| format!("task access legacy creator: {e}"))?;
            if has_creator {
                let has_a2a: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM tasks WHERE created_by GLOB 'a2a:*')",
                    [], |r| r.get(0),
                ).map_err(|e| format!("task access legacy owner: {e}"))?;
                if has_a2a { return Err("A2A task access registry is missing".into()); }
            }
            return Ok(None);
        }
        tx.query_row(
            "SELECT principal FROM task_access_principals WHERE task_id = ?1",
            [task], |r| r.get(0),
        ).optional().map_err(|e| format!("task access principal: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_a2a_owner_lookup_is_readonly_and_keeps_ordinary_tasks_visible() {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join("tasks.db")).unwrap();
        db.execute_batch("CREATE TABLE tasks(id TEXT); INSERT INTO tasks VALUES ('ordinary');").unwrap();
        assert_eq!(TaskStore::a2a_owner_readonly(dir.path(), "ordinary").unwrap(), None);
        assert!(TaskStore::a2a_principal_allows(dir.path(), "ordinary", "user:alice"));
        let tables: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0)).unwrap();
        assert_eq!(tables, 1, "readers must not migrate the database");
        db.execute_batch("ALTER TABLE tasks ADD COLUMN created_by TEXT; UPDATE tasks SET created_by='system';").unwrap();
        assert!(TaskStore::a2a_principal_allows(dir.path(), "ordinary", "user:bob"));
        db.execute_batch("UPDATE tasks SET created_by='a2a:user:alice';").unwrap();
        assert!(!TaskStore::a2a_principal_allows(dir.path(), "ordinary", "user:bob"));
    }

    #[test]
    fn missing_or_malformed_a2a_registry_never_uses_legacy_fallback() {
        for schema in [
            "CREATE TABLE tasks(id TEXT, a2a_owner TEXT);",
            "CREATE TABLE tasks(id TEXT); CREATE TABLE a2a_requests(owner TEXT);",
            "CREATE TABLE tasks(id TEXT); CREATE TRIGGER task_a2a_guard AFTER INSERT ON tasks BEGIN SELECT 1; END;",
            "CREATE TABLE tasks(id TEXT); CREATE TABLE task_access_principals(task_id TEXT);",
            "CREATE TABLE tasks(id TEXT); CREATE TABLE task_access_principals(task_id TEXT, principal TEXT); INSERT INTO task_access_principals VALUES ('deleted', NULL);",
            "CREATE TABLE tasks(id TEXT, created_by TEXT); INSERT INTO tasks VALUES ('old-a2a', 'a2a:user:alice');",
            "CREATE TABLE tasks(id TEXT, a2a_owner TEXT); CREATE VIEW task_access_principals AS SELECT id AS task_id, a2a_owner AS principal FROM tasks;",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let db = Connection::open(dir.path().join("tasks.db")).unwrap();
            db.execute_batch(schema).unwrap();
            assert!(TaskStore::a2a_owner_readonly(dir.path(), "deleted").is_err(), "{schema}");
            assert!(!TaskStore::a2a_principal_allows(dir.path(), "deleted", "user:alice"), "{schema}");
            let before: Vec<String> = db.prepare("SELECT sql FROM sqlite_master ORDER BY name").unwrap()
                .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
            assert!(TaskStore::open(dir.path()).is_err(), "must refuse before migration: {schema}");
            let after: Vec<String> = db.prepare("SELECT sql FROM sqlite_master ORDER BY name").unwrap()
                .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
            assert_eq!(after, before, "failed open must not migrate: {schema}");
        }
    }

    #[tokio::test]
    async fn fresh_and_pre_a2a_databases_still_migrate_and_reopen() {
        for legacy in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            if legacy {
                let db = Connection::open(dir.path().join("tasks.db")).unwrap();
                db.execute_batch("CREATE TABLE tasks (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'todo', priority TEXT NOT NULL DEFAULT 'medium',
                    assigned_to TEXT NOT NULL, created_by TEXT NOT NULL DEFAULT 'system',
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, completed_at TEXT,
                    blocked_reason TEXT, parent_task_id TEXT, tags TEXT NOT NULL DEFAULT '', message_id TEXT
                ); INSERT INTO tasks(id,title,assigned_to,created_at,updated_at)
                   VALUES ('ordinary','legacy task','sales','2026-11-05T00:00:00Z','2026-11-05T00:00:00Z');").unwrap();
            }
            drop(TaskStore::open(dir.path()).unwrap());
            let reopened = TaskStore::open(dir.path()).unwrap();
            assert!(TaskStore::a2a_principal_allows(dir.path(), "ordinary", "user:alice"));
            if legacy {
                assert_eq!(reopened.get_task("ordinary").await.unwrap().unwrap().title, "legacy task");
            }
        }
    }

    #[test]
    fn unavailable_owner_database_stays_closed_without_creating_it() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!TaskStore::a2a_principal_allows(dir.path(), "task", "user:alice"));
        assert!(!dir.path().join("tasks.db").exists());
        std::fs::write(dir.path().join("tasks.db"), b"not a SQLite database").unwrap();
        assert!(!TaskStore::a2a_principal_allows(dir.path(), "task", "user:alice"));
    }

    #[tokio::test]
    async fn inherited_owner_allows_content_updates_but_refuses_identity_updates() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaskStore::open(dir.path()).unwrap();
        let mut parent = TaskRow::new("parent".into(), "parent".into(), String::new(),
            "normal".into(), "sales".into(), "a2a:user:alice".into());
        parent.a2a_owner = Some("user:alice".into());
        store.insert_task(&parent).await.unwrap();
        let mut child = TaskRow::new("child".into(), "original".into(), String::new(),
            "normal".into(), "sales".into(), "sales".into());
        child.parent_task_id = Some(parent.id.clone());
        store.insert_task(&child).await.unwrap();
        for fields in [
            serde_json::json!({"parent_task_id": "open-task", "title": "must not land"}),
            serde_json::json!({"parent_task_id": null}),
            serde_json::json!({"a2a_owner": "user:bob"}),
            serde_json::json!({"a2a_owner": null}),
        ] {
            assert!(store.update_task(&child.id, &fields).await.is_err());
        }
        assert_eq!(store.get_task(&child.id).await.unwrap().unwrap().title, "original");
        let updated = store.update_task(&child.id, &serde_json::json!({"title": "updated", "status": "in_progress"}))
            .await.unwrap().unwrap();
        assert_eq!(updated.title, "updated");
        assert_eq!(updated.a2a_owner.as_deref(), Some("user:alice"));
        assert_eq!(updated.parent_task_id.as_deref(), Some("parent"));
        store.remove_task(&parent.id).await.unwrap();
        drop(store);
        let reopened = TaskStore::open(dir.path()).unwrap();
        for id in [&parent.id, &child.id] {
            assert!(TaskStore::a2a_principal_allows(dir.path(), id, "user:alice"));
            assert!(!TaskStore::a2a_principal_allows(dir.path(), id, "user:bob"));
        }
        drop(reopened);
        let db = Connection::open(dir.path().join("tasks.db")).unwrap();
        db.execute_batch("DROP TABLE task_access_principals;").unwrap();
        assert!(!TaskStore::a2a_principal_allows(dir.path(), &parent.id, "user:alice"));
        assert!(TaskStore::open(dir.path()).is_err(), "lost tombstones must not be recreated on restart");
        let registry_exists: bool = db.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name='task_access_principals')",
            [], |r| r.get(0),
        ).unwrap();
        assert!(!registry_exists);
    }
}
