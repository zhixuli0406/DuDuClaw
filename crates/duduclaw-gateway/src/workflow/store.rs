//! Shared workflow storage. All mutations use short immediate transactions.
use super::schema::{ApprovedWorkflowRevision, StepEvidence, WorkflowRun};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use rusqlite::{OptionalExtension, params};
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

pub struct WorkflowStore {
    connection: Mutex<Connection>,
    path: Option<PathBuf>,
}
impl WorkflowStore {
    pub fn open(home: &Path) -> Result<Self, String> {
        let home = std::fs::canonicalize(home).map_err(|e| e.to_string())?;
        let path = home.join("workflow.db");
        for suffix in ["", "-wal", "-shm"] {
            let target = PathBuf::from(format!("{}{suffix}", path.display()));
            if std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err("workflow database symlink refused".into());
            }
            #[cfg(unix)]
            if target.exists() {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| e.to_string())?;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            // Create only: closing an fd on a live database drops this
            // process's POSIX locks on it (see approval/store.rs).
            if !path.exists() {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                {
                    Ok(_) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(e.to_string()),
                }
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        let mut connection = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|e| e.to_string())?;
        Self::initialize(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            path: Some(path),
        })
    }
    pub fn open_in_memory() -> Result<Self, String> {
        let mut connection = Connection::open_in_memory().map_err(|e| e.to_string())?;
        Self::initialize(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            path: None,
        })
    }
    fn initialize(connection: &mut Connection) -> Result<(), String> {
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA secure_delete=ON;" ,)
            .map_err(|e| e.to_string())?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS workflow_schema_meta (owner TEXT PRIMARY KEY,
            version INTEGER NOT NULL);
            INSERT OR IGNORE INTO workflow_schema_meta VALUES('foundation',1);
            CREATE TABLE IF NOT EXISTS workflow_revisions (workflow_id TEXT NOT NULL,revision INTEGER NOT NULL,
            revision_hash TEXT NOT NULL,record_json TEXT NOT NULL,PRIMARY KEY(workflow_id,revision));
            CREATE TRIGGER IF NOT EXISTS workflow_revision_immutable BEFORE UPDATE ON workflow_revisions BEGIN SELECT
            RAISE(ABORT,'workflow revision immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_revision_no_delete BEFORE DELETE ON workflow_revisions BEGIN SELECT
            RAISE(ABORT,'workflow revision retained'); END;
            CREATE TABLE IF NOT EXISTS workflow_candidate_revisions(workflow_id TEXT NOT NULL,
            revision INTEGER NOT NULL,revision_hash TEXT NOT NULL,record_json TEXT NOT NULL,PRIMARY KEY(workflow_id,
            revision));
            CREATE TRIGGER IF NOT EXISTS workflow_candidate_immutable BEFORE UPDATE ON workflow_candidate_revisions
            BEGIN SELECT RAISE(ABORT,'workflow candidate immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_candidate_retained BEFORE DELETE ON workflow_candidate_revisions
            BEGIN SELECT RAISE(ABORT,'workflow candidate retained'); END;
            CREATE TABLE IF NOT EXISTS workflow_runs (run_id TEXT PRIMARY KEY,trigger_key TEXT NOT NULL UNIQUE,
            workflow_id TEXT NOT NULL,revision INTEGER NOT NULL,workflow_hash TEXT NOT NULL,record_json TEXT NOT NULL,
            status TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS workflow_steps (run_id TEXT NOT NULL,step_id TEXT NOT NULL,
            position INTEGER NOT NULL,status TEXT NOT NULL,record_json TEXT NOT NULL,PRIMARY KEY(run_id,step_id));
            CREATE TRIGGER IF NOT EXISTS workflow_run_authority_immutable BEFORE UPDATE ON workflow_runs
            WHEN json_extract(NEW.record_json,'$.run_id') IS NOT json_extract(OLD.record_json,'$.run_id')
            OR json_extract(NEW.record_json,'$.trigger_key') IS NOT json_extract(OLD.record_json,'$.trigger_key')
            OR json_extract(NEW.record_json,'$.trigger') IS NOT json_extract(OLD.record_json,'$.trigger')
            OR json_extract(NEW.record_json,'$.workflow_id') IS NOT json_extract(OLD.record_json,'$.workflow_id')
            OR json_extract(NEW.record_json,'$.revision') IS NOT json_extract(OLD.record_json,'$.revision')
            OR json_extract(NEW.record_json,'$.workflow_hash') IS NOT json_extract(OLD.record_json,'$.workflow_hash')
            OR json_extract(NEW.record_json,'$.skill_hash') IS NOT json_extract(OLD.record_json,'$.skill_hash')
            OR json_extract(NEW.record_json,'$.actor') IS NOT json_extract(OLD.record_json,'$.actor')
            OR json_extract(NEW.record_json,'$.creator_grant') IS NOT json_extract(OLD.record_json,'$.creator_grant')
            OR json_extract(NEW.record_json,'$.audience') IS NOT json_extract(OLD.record_json,'$.audience')
            OR json_extract(NEW.record_json,'$.task') IS NOT json_extract(OLD.record_json,'$.task')
            OR json_extract(NEW.record_json,'$.input') IS NOT json_extract(OLD.record_json,'$.input')
            OR json_extract(NEW.record_json,'$.input_hash') IS NOT json_extract(OLD.record_json,'$.input_hash')
            OR json_extract(NEW.record_json,'$.input_observed_at') IS NOT json_extract(OLD.record_json,
            '$.input_observed_at') OR json_extract(NEW.record_json,
            '$.policy_revision') IS NOT json_extract(OLD.record_json,'$.policy_revision')
            OR json_extract(NEW.record_json,'$.environment_hash') IS NOT json_extract(OLD.record_json,
            '$.environment_hash') OR json_extract(NEW.record_json,'$.grant') IS NOT json_extract(OLD.record_json,
            '$.grant') OR json_extract(NEW.record_json,'$.activation_id') IS NOT json_extract(OLD.record_json,
            '$.activation_id') OR json_extract(NEW.record_json,'$.deadline_at') IS NOT json_extract(OLD.record_json,
            '$.deadline_at') OR json_extract(NEW.record_json,'$.budget') IS NOT json_extract(OLD.record_json,
            '$.budget') OR json_extract(NEW.record_json,'$.created_at') IS NOT json_extract(OLD.record_json,
            '$.created_at') OR json_extract(NEW.record_json,'$.decision_context') IS NOT json_extract(OLD.record_json,
            '$.decision_context') BEGIN SELECT RAISE(ABORT,'workflow run authority immutable'); END;
            CREATE TABLE IF NOT EXISTS workflow_run_leases(run_id TEXT PRIMARY KEY,token TEXT NOT NULL,
            lease_until INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS workflow_fixture_run_requests(run_id TEXT PRIMARY KEY,
            request_json TEXT NOT NULL,evidence_json TEXT);
            CREATE TRIGGER IF NOT EXISTS workflow_fixture_final_immutable BEFORE UPDATE
            ON workflow_fixture_run_requests WHEN OLD.evidence_json IS NOT NULL BEGIN SELECT RAISE(ABORT,
            'fixture execution evidence immutable'); END;
            CREATE TABLE IF NOT EXISTS workflow_artifact_commits(record_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,
            step_id TEXT NOT NULL,content_hash TEXT NOT NULL,content_json TEXT NOT NULL,audience_json TEXT NOT NULL,
            receipt_json TEXT NOT NULL);
            CREATE TRIGGER IF NOT EXISTS workflow_artifact_immutable BEFORE UPDATE ON workflow_artifact_commits BEGIN
            SELECT RAISE(ABORT,'workflow artifact immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_artifact_retained BEFORE DELETE ON workflow_artifact_commits BEGIN
            SELECT RAISE(ABORT,'workflow artifact retained'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_completed_step_immutable BEFORE UPDATE ON workflow_steps
            WHEN OLD.status='succeeded' BEGIN SELECT RAISE(ABORT,'workflow completed step immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_step_retained BEFORE DELETE ON workflow_steps BEGIN SELECT
            RAISE(ABORT,'workflow step retained'); END;

            CREATE TABLE IF NOT EXISTS workflow_activations (activation_id TEXT PRIMARY KEY,workflow_id TEXT NOT NULL,
            revision INTEGER NOT NULL,material_hash TEXT NOT NULL,acceptance_id TEXT NOT NULL,grant_id TEXT,
            grant_epoch INTEGER,state TEXT NOT NULL,record_json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS workflow_outbox (outbox_id TEXT PRIMARY KEY,kind TEXT NOT NULL,
            entity_id TEXT NOT NULL,payload_json TEXT NOT NULL,delivered INTEGER NOT NULL DEFAULT 0);
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_status ON workflow_runs(status);
            CREATE TABLE IF NOT EXISTS workflow_failure_resets(reset_id TEXT PRIMARY KEY,
            activation_id TEXT NOT NULL,reset_at TEXT NOT NULL,reset_by TEXT NOT NULL,reason TEXT NOT NULL);
            CREATE TRIGGER IF NOT EXISTS workflow_failure_reset_immutable BEFORE UPDATE ON workflow_failure_resets
            BEGIN SELECT RAISE(ABORT,'workflow failure reset immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_failure_reset_retained BEFORE DELETE ON workflow_failure_resets
            BEGIN SELECT RAISE(ABORT,'workflow failure reset retained'); END;
            CREATE TABLE IF NOT EXISTS workflow_cost_entries(entry_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,
            workflow_id TEXT NOT NULL,formal INTEGER NOT NULL,month TEXT NOT NULL,step_id TEXT NOT NULL,
            attempt INTEGER NOT NULL,kind TEXT NOT NULL,amount_micros INTEGER NOT NULL CHECK(amount_micros>=0),
            basis TEXT NOT NULL,created_at TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS idx_workflow_cost_run ON workflow_cost_entries(run_id);
            CREATE INDEX IF NOT EXISTS idx_workflow_outbox_undelivered ON workflow_outbox(delivered)
            WHERE delivered=0;
            CREATE INDEX IF NOT EXISTS idx_workflow_cost_month ON workflow_cost_entries(workflow_id,month);
            CREATE TRIGGER IF NOT EXISTS workflow_cost_immutable BEFORE UPDATE ON workflow_cost_entries
            BEGIN SELECT RAISE(ABORT,'workflow cost entry immutable'); END;
            CREATE TRIGGER IF NOT EXISTS workflow_cost_retained BEFORE DELETE ON workflow_cost_entries
            BEGIN SELECT RAISE(ABORT,'workflow cost entry retained'); END;")
            .map_err(|e| e.to_string())?;
        let version: i64 = tx
            .query_row(
                "SELECT version FROM workflow_schema_meta WHERE owner='foundation'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if version != 1 {
            return Err("unsupported workflow foundation schema version".into());
        }
        super::draft_store::init_schema(&tx)?;
        tx.commit().map_err(|e| e.to_string())
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub async fn get_run(&self, id: &str) -> Result<Option<WorkflowRun>, String> {
        self.with_connection(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json FROM workflow_runs WHERE run_id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                .transpose()
        })
        .await
    }
    pub async fn get_step(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<StepEvidence>, String> {
        self.with_connection(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json FROM workflow_steps WHERE run_id=?1 AND step_id=?2",
                    params![run_id, step_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                .transpose()
        })
        .await
    }
    /// Newest activated runs of one accepted workflow revision.
    pub async fn list_runs(
        &self,
        workflow_id: &str,
        revision: i64,
        limit: usize,
    ) -> Result<Vec<WorkflowRun>, String> {
        self.with_connection(|c| {
            let mut q = c
                .prepare(
                    "SELECT record_json FROM workflow_runs WHERE workflow_id=?1 AND revision=?2
                        AND json_extract(record_json,'$.activation_id') IS NOT NULL
                        ORDER BY created_at DESC,run_id DESC LIMIT ?3",
                )
                .map_err(|e| e.to_string())?;
            let raw: Vec<String> = q
                .query_map(params![workflow_id, revision, limit.clamp(1, 100) as i64], |r| {
                    r.get(0)
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            raw.iter()
                .map(|s| serde_json::from_str(s).map_err(|e| e.to_string()))
                .collect()
        })
        .await
    }
    /// Every step checkpoint of one run in definition order.
    pub async fn get_steps(&self, run_id: &str) -> Result<Vec<StepEvidence>, String> {
        self.with_connection(|c| {
            let mut q = c
                .prepare("SELECT record_json FROM workflow_steps WHERE run_id=?1 ORDER BY position")
                .map_err(|e| e.to_string())?;
            let raw: Vec<String> = q
                .query_map(params![run_id], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            raw.iter()
                .map(|s| serde_json::from_str(s).map_err(|e| e.to_string()))
                .collect()
        })
        .await
    }
    pub async fn get_revision(
        &self,
        id: &str,
        revision: i64,
    ) -> Result<Option<ApprovedWorkflowRevision>, String> {
        self.with_connection(|c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json,0 AS preference FROM workflow_revisions WHERE workflow_id=?1
                        AND revision=?2 UNION ALL
                        SELECT record_json,1 AS preference FROM workflow_candidate_revisions WHERE workflow_id=?1
                        AND revision=?2 ORDER BY preference LIMIT 1",
                    params![id, revision],
                    |r| r.get(0)
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                .transpose()
        })
        .await
    }
    /// Owners initialize their additive schemas through this same transaction seam.
    /// No provider await or process execution may occur inside the closure.
    pub async fn with_transaction<T>(
        &self,
        body: impl FnOnce(&Transaction<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut connection = self.connection.lock().await;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let value = body(&tx)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(value)
    }
    /// Read closure; callers must not mutate through this inspection API.
    pub async fn with_connection<T>(
        &self,
        body: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let connection = self.connection.lock().await;
        body(&connection)
    }
}
