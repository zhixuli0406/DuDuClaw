//! SQLite persistence for approval records.
//! Moved verbatim out of `approval.rs` (file-size split).

use super::*;

impl ApprovalStore {
    /// Open (or create) the store at `<home>/approvals.db`.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        // SQLite NOFOLLOW rejects symlinked parent components too (e.g. macOS
        // /var -> /private/var). Resolve the trusted home, while still refusing
        // a symlink at the actual database or sidecar path.
        let db_path = std::fs::canonicalize(home_dir)
            .map_err(|e| format!("resolve approval home: {e}"))?
            .join("approvals.db");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            for suffix in ["-wal", "-shm"] {
                let path = PathBuf::from(format!("{}{suffix}", db_path.display()));
                if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                    return Err("approval sidecar symlink refused".into());
                }
                if path.exists() {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                        .map_err(|e| e.to_string())?;
                }
            }
            if std::fs::symlink_metadata(&db_path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err("approval database symlink refused".into());
            }
            // Never open-and-close an existing database file: closing any fd
            // on it drops every POSIX lock this process holds there, so other
            // live connections lose their WAL locks and another process may
            // checkpoint and remove the WAL under them (stale reads, then
            // "disk I/O error"; seen in workflow::resume_tests). Only create.
            if !db_path.exists() {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&db_path)
                {
                    Ok(_) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(format!("private approval store: {e}")),
                }
            }
            std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        let conn = Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|e| format!("open approvals store: {e}"))?;
        Self::init_schema(&conn)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for suffix in ["-wal", "-shm"] {
                let p = PathBuf::from(format!("{}{suffix}", db_path.display()));
                if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
                    return Err("approval sidecar symlink refused".into());
                }
                if p.exists() {
                    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        info!(?db_path, "ApprovalStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: Some(db_path),
        })
    }

    /// In-memory store for tests (no file, no WAL persistence).
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: None,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA secure_delete=ON;
             PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS approvals (
                 id           TEXT PRIMARY KEY,
                 agent_id     TEXT NOT NULL,
                 action_kind  TEXT NOT NULL,
                 summary      TEXT NOT NULL,
                 payload      TEXT NOT NULL DEFAULT '{}',
                 status       TEXT NOT NULL DEFAULT 'pending',
                 created_at   TEXT NOT NULL,
                 decided_at   TEXT,
                 decided_by   TEXT,
                 ttl_seconds  INTEGER NOT NULL DEFAULT 3600
             );

             CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(status);
             CREATE INDEX IF NOT EXISTS idx_approvals_agent  ON approvals(agent_id);
             ",
        )
        .map_err(|e| format!("init approvals schema: {e}"))?;
        Self::migrate(conn)?;
        Self::init_operations(conn)?;
        Self::migrate_workflow_authority(conn)?;
        Ok(())
    }

    /// Idempotent additive migration (same shape as `task_store`): every column
    /// is nullable, so an old `approvals.db` upgrades in place and a downgrade
    /// still reads every pre-existing column.
    fn migrate(conn: &Connection) -> Result<(), String> {
        let existing: HashSet<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(approvals)")
                .map_err(|e| format!("pragma approvals: {e}"))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| format!("pragma query: {e}"))?;
            rows.filter_map(Result::ok).collect()
        };
        // WP20: channel push bookkeeping.
        let migrations: &[(&str, &str)] = &[
            ("notify_channel", "notify_channel TEXT"),
            ("notify_chat_id", "notify_chat_id TEXT"),
            ("reminded_at", "reminded_at TEXT"),
            // D1: ActionGuard simulation narrative (JSON text, NULL when absent).
            ("simulation", "simulation TEXT"),
            (
                "request_kind",
                "request_kind TEXT NOT NULL DEFAULT 'approval'",
            ),
            ("binding_json", "binding_json TEXT"),
            ("answer_json", "answer_json TEXT"),
            ("invalidated_reason", "invalidated_reason TEXT"),
        ];
        for (col, ddl) in migrations {
            if !existing.contains(*col) {
                conn.execute(&format!("ALTER TABLE approvals ADD COLUMN {ddl}"), [])
                    .map_err(|e| format!("add column {col}: {e}"))?;
            }
        }
        Ok(())
    }

    pub(super) async fn insert(&self, rec: &ApprovalRecord) -> Result<(), String> {
        self.insert_row(rec, false).await.map(|_| ())
    }

    /// Insert unless a row with the same id exists. Returns true when inserted.
    pub(super) async fn insert_if_absent(&self, rec: &ApprovalRecord) -> Result<bool, String> {
        self.insert_row(rec, true).await
    }

    async fn insert_row(&self, rec: &ApprovalRecord, if_absent: bool) -> Result<bool, String> {
        let payload_text = rec.payload.to_string();
        let simulation_text = rec.simulation.as_ref().map(|v| v.to_string());
        let conn = self.conn.lock().await;
        let sql = format!(
            "INSERT INTO approvals
                (id, agent_id, action_kind, summary, payload, status,
                 created_at, decided_at, decided_by, ttl_seconds,
                 notify_channel, notify_chat_id, reminded_at, simulation, request_kind, binding_json, answer_json,
                invalidated_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18){}",
            if if_absent { " ON CONFLICT(id) DO NOTHING" } else { "" }
        );
        conn.execute(
            &sql,
            params![
                rec.id.as_str(),
                rec.agent_id,
                rec.action_kind,
                rec.summary,
                payload_text,
                rec.status.as_str(),
                rec.created_at,
                rec.decided_at,
                rec.decided_by,
                rec.ttl_seconds,
                rec.notify_channel,
                rec.notify_chat_id,
                rec.reminded_at,
                simulation_text,
                rec.request_kind.as_str(),
                rec.binding.as_ref().map(|v| serde_json::to_string(v).unwrap()),
                rec.answer.as_ref().map(Value::to_string),
                rec.invalidated_reason,
            ],
        )
        .map(|n| n == 1)
        .map_err(|e| format!("insert approval: {e}"))
    }

    /// WP20: record where the pending-approval push landed. Best-effort
    /// bookkeeping — never gates the approval itself.
    pub(super) async fn set_notify_target(
        &self,
        id: &ApprovalId,
        channel: &str,
        chat_id: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE approvals SET notify_channel = ?1, notify_chat_id = ?2 WHERE id = ?3",
            params![channel, chat_id, id.as_str()],
        )
        .map_err(|e| format!("set notify target: {e}"))?;
        Ok(())
    }

    /// WP20: claim the once-only reminder slot. The `reminded_at IS NULL AND
    /// status = 'pending'` guard makes this the race winner — two concurrent
    /// pollers (gateway sweep + a blocked MCP process) cannot both send.
    /// Returns `true` when THIS caller won and should actually push.
    pub(super) async fn claim_reminder(&self, id: &ApprovalId, at: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE approvals SET reminded_at = ?1
                 WHERE id = ?2 AND reminded_at IS NULL AND status = 'pending'",
                params![at, id.as_str()],
            )
            .map_err(|e| format!("claim reminder: {e}"))?;
        Ok(n > 0)
    }

    pub(super) async fn get(&self, id: &ApprovalId) -> Result<Option<ApprovalRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, agent_id, action_kind, summary, payload, status,
                    created_at, decided_at, decided_by, ttl_seconds,
                    notify_channel, notify_chat_id, reminded_at, simulation,
                    request_kind, binding_json, answer_json, invalidated_reason
             FROM approvals WHERE id = ?1",
            params![id.as_str()],
            row_to_record,
        )
        .optional()
        .map_err(|e| format!("get approval: {e}"))
    }

    /// Transition a pending row to a terminal status. The
    /// `WHERE status = 'pending'` guard makes this idempotent-safe and
    /// closes the two-decider race — returns rows affected (0 = not
    /// pending / not found).
    pub(super) async fn decide_if_pending(
        &self,
        id: &ApprovalId,
        status: ApprovalStatus,
        decided_by: &str,
        decided_at: &str,
    ) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE approvals
             SET status = ?1, decided_by = ?2, decided_at = ?3
             WHERE id = ?4 AND status = 'pending'
             AND (?1 NOT IN ('approved','denied','answered') OR
                  (julianday(created_at) IS NOT NULL
                   AND julianday(?3) < julianday(created_at) + ttl_seconds / 86400.0))",
            params![status.as_str(), decided_by, decided_at, id.as_str()],
        )
        .map_err(|e| format!("decide approval: {e}"))
    }

    /// Every approval of one `action_kind`, any status (GDPR scrub).
    pub(super) async fn list_by_kind(&self, kind: &str) -> Result<Vec<ApprovalRecord>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, action_kind, summary, payload, status,
                        created_at, decided_at, decided_by, ttl_seconds,
                        notify_channel, notify_chat_id, reminded_at, simulation,
                        request_kind, binding_json, answer_json, invalidated_reason
                 FROM approvals WHERE action_kind = ?1 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("prepare list_by_kind: {e}"))?;
        let rows = stmt
            .query_map(params![kind], row_to_record)
            .map_err(|e| format!("query list_by_kind: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list_by_kind: {e}"))?;
        Ok(rows)
    }

    /// Replace an approval's summary and payload (GDPR scrub). Status,
    /// decision and timestamps are untouched.
    pub(super) async fn replace_text(
        &self,
        id: &ApprovalId,
        summary: &str,
        payload: &Value,
    ) -> Result<usize, String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE approvals SET summary = ?1, payload = ?2, simulation = NULL,
             status = CASE WHEN binding_json IS NOT NULL THEN 'invalidated' ELSE status END,
             invalidated_reason = CASE WHEN binding_json IS NOT NULL THEN 'payload_scrubbed' ELSE invalidated_reason
                END WHERE id = ?3",
            params![summary, payload.to_string(), id.as_str()],
        )
        .map_err(|e| format!("scrub approval: {e}"))
    }

    pub(super) async fn list_pending(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<ApprovalRecord>, String> {
        let conn = self.conn.lock().await;
        match agent_id {
            Some(aid) => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, agent_id, action_kind, summary, payload, status,
                                created_at, decided_at, decided_by, ttl_seconds,
                                notify_channel, notify_chat_id, reminded_at, simulation,
                                request_kind, binding_json, answer_json, invalidated_reason
                         FROM approvals
                         WHERE status = 'pending' AND agent_id = ?1
                         ORDER BY created_at ASC",
                    )
                    .map_err(|e| format!("prepare list_pending: {e}"))?;
                let rows = stmt
                    .query_map(params![aid], row_to_record)
                    .map_err(|e| format!("query list_pending: {e}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("collect list_pending: {e}"))?;
                Ok(rows)
            }
            None => {
                let mut stmt = conn
                    .prepare(
                        "SELECT id, agent_id, action_kind, summary, payload, status,
                                created_at, decided_at, decided_by, ttl_seconds,
                                notify_channel, notify_chat_id, reminded_at, simulation,
                                request_kind, binding_json, answer_json, invalidated_reason
                         FROM approvals
                         WHERE status = 'pending'
                         ORDER BY created_at ASC",
                    )
                    .map_err(|e| format!("prepare list_pending: {e}"))?;
                let rows = stmt
                    .query_map([], row_to_record)
                    .map_err(|e| format!("query list_pending: {e}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("collect list_pending: {e}"))?;
                Ok(rows)
            }
        }
    }
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<ApprovalRecord> {
    let payload_text: String = row.get(4)?;
    let payload: Value = serde_json::from_str(&payload_text).unwrap_or(Value::Null);
    let status_text: String = row.get(5)?;
    let simulation_text: Option<String> = row.get(13)?;
    let simulation = simulation_text.and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let binding = row
        .get::<_, Option<String>>(15)?
        .map(|s| {
            serde_json::from_str::<ExecutionBinding>(&s).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    15,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .transpose()?;
    let action_kind: String = row.get(2)?;
    if action_kind.starts_with("bound_") && binding.is_none() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(ApprovalRecord {
        id: ApprovalId::from(row.get::<_, String>(0)?),
        agent_id: row.get(1)?,
        action_kind: row.get(2)?,
        summary: row.get(3)?,
        payload,
        status: ApprovalStatus::from_db(&status_text),
        created_at: row.get(6)?,
        decided_at: row.get(7)?,
        decided_by: row.get(8)?,
        ttl_seconds: row.get(9)?,
        notify_channel: row.get(10)?,
        notify_chat_id: row.get(11)?,
        reminded_at: row.get(12)?,
        simulation,
        request_kind: RequestKind::from_db(&row.get::<_, String>(14)?),
        binding,
        answer: row
            .get::<_, Option<String>>(16)?
            .and_then(|s| serde_json::from_str(&s).ok()),
        invalidated_reason: row.get(17)?,
    })
}

// ── Broker ──────────────────────────────────────────────────
