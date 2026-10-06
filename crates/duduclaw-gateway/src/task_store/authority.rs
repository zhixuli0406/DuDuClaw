//! Durable task authority epochs and the exact snapshot bound by decisions.
//!
//! The trigger covers all SQLite writers, including MCP subprocesses and
//! direct lifecycle SQL. A heartbeat or ordinary claim does not change the
//! approved contract; an edit, hand-off or new attempt does.

use super::*;
use sha2::{Digest, Sha256};

/// Persisted contract fields, deliberately excluding display/progress and
/// lease timestamps. Keep the hash projection below aligned with this list.
const AUTHORITY_COLUMNS: &[&str] = &[
    "id",
    "title",
    "description",
    "assigned_to",
    "created_by",
    "created_at",
    "parent_task_id",
    "goal_id",
    "tags",
    "depends_on",
    "max_retries",
    "goal_mode",
    "acceptance_criteria",
    "acceptance_criteria_baseline",
    "deadline_at",
    "risk_boundary",
    "plan_pending",
    "team_spec_json",
    "kind",
    "source_channel",
    "source_chat_id",
    "source_discord_guild_id",
    "discovery_spec_json",
    "discovery_run_id",
    "discovery_approval_id",
    "archived",
    "retry_count",
    "revision_round",
];

/// A single task-row read: revision and hash must never come from different
/// reads. `eligible` is only the common lifecycle gate; each registered resume
/// handler must still enforce its own narrower status and actor permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskAuthoritySnapshot {
    pub task_id: String,
    pub revision: i64,
    pub hash: String,
    pub status: String,
    pub claimed_by: Option<String>,
    pub eligible: bool,
}

impl TaskRow {
    pub fn authority_snapshot_hash(&self) -> String {
        task_snapshot_hash(self)
    }

    /// A parked task may ask a fresh question/approval after its epoch changed.
    /// It cannot reuse the authorization from before it parked. Unknown states,
    /// terminal/archived rows and invalid/elapsed deadlines fail closed.
    pub fn approval_eligible(&self) -> bool {
        self.authority_revision >= 1
            && !self.archived
            && matches!(
                self.status.as_str(),
                "todo"
                    | "pending"
                    | "in_progress"
                    | "review"
                    | "revising"
                    | "needs_human"
                    | "blocked"
                    | "queued"
                    | "pending_approval"
            )
            && self.deadline_at.as_deref().is_none_or(|deadline| {
                DateTime::parse_from_rfc3339(deadline)
                    .is_ok_and(|deadline| Utc::now() < deadline.with_timezone(&Utc))
            })
    }
}

/// Versioned, deterministic SHA-256 over the persisted authority projection.
/// Including the host epoch prevents an edit/revert (ABA) from reviving an old
/// approval. It intentionally excludes operational status and claim acquisition:
/// their lifecycle gate and release/retry transitions are checked separately.
pub fn task_snapshot_hash(task: &TaskRow) -> String {
    let snapshot = serde_json::json!({
        "schema_version": 1,
        "authority_revision": task.authority_revision,
        "id": task.id,
        "title": task.title,
        "description": task.description,
        "assigned_to": task.assigned_to,
        "created_by": task.created_by,
        "created_at": task.created_at,
        "parent_task_id": task.parent_task_id,
        "goal_id": task.goal_id,
        "tags": task.tags,
        "depends_on": task.depends_on,
        "max_retries": task.max_retries,
        "goal_mode": task.goal_mode,
        "acceptance_criteria": task.acceptance_criteria,
        "acceptance_criteria_baseline": task.acceptance_criteria_baseline,
        "deadline_at": task.deadline_at,
        "risk_boundary": task.risk_boundary,
        "plan_pending": task.plan_pending,
        "team_spec_json": task.team_spec_json,
        "kind": task.kind.as_str(),
        "source_channel": task.source_channel,
        "source_chat_id": task.source_chat_id,
        "source_discord_guild_id": task.source_discord_guild_id,
        "discovery_spec_json": task.discovery_spec_json,
        "discovery_run_id": task.discovery_run_id,
        "discovery_approval_id": task.discovery_approval_id,
        "archived": task.archived,
        "retry_count": task.retry_count,
        "revision_round": task.revision_round,
    });
    format!("{:x}", Sha256::digest(snapshot.to_string().as_bytes()))
}

impl TaskStore {
    pub async fn authority_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<TaskAuthoritySnapshot>, String> {
        Ok(self.get_task(id).await?.map(|task| TaskAuthoritySnapshot {
            task_id: task.id.clone(),
            revision: task.authority_revision,
            hash: task.authority_snapshot_hash(),
            status: task.status.clone(),
            claimed_by: task.claimed_by.clone(),
            eligible: task.approval_eligible(),
        }))
    }

    pub(super) fn init_authority_schema(conn: &Connection) -> Result<(), String> {
        let mut watched = AUTHORITY_COLUMNS.to_vec();
        watched.extend(["status", "claimed_by"]);
        let edits = AUTHORITY_COLUMNS
            .iter()
            .map(|column| format!("OLD.{column} IS NOT NEW.{column}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        // All identifiers above and state tokens below are host constants.
        // The trigger's internal epoch UPDATE cannot recursively match its own
        // UPDATE OF list, even if recursive_triggers is enabled by a caller.
        let ddl = format!(
            "DROP TRIGGER IF EXISTS task_authority_revision_v1;
             CREATE TRIGGER task_authority_revision_v1
             AFTER UPDATE OF {} ON tasks
             WHEN {edits}
                OR (OLD.claimed_by IS NOT NULL AND OLD.claimed_by IS NOT NEW.claimed_by)
                OR (OLD.status IS NOT NEW.status AND NOT (
                    (OLD.status IN ('todo','pending','revising','queued','pending_approval')
                        AND NEW.status = 'in_progress')
                    OR (OLD.status = 'in_progress' AND NEW.status = 'review')
                ))
             BEGIN
                 UPDATE tasks SET authority_revision = MAX(OLD.authority_revision + 1,
                     COALESCE((SELECT revision_floor FROM task_authority_epochs WHERE task_id=NEW.id),1))
                     WHERE id = NEW.id;
             END;",
            watched.join(","),
        );
        // Epoch tombstones outlive task deletion and every task purge. Never GC
        // them while any old authorization can be presented. The whole migration
        // and trigger replacement commit together, including existing-row floors.
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS task_authority_epochs (
            task_id TEXT PRIMARY KEY, revision_floor INTEGER NOT NULL CHECK(revision_floor>=1));
            INSERT INTO task_authority_epochs(task_id,revision_floor)
                SELECT id,authority_revision FROM tasks WHERE 1
                ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            CREATE TRIGGER IF NOT EXISTS task_epoch_no_delete BEFORE DELETE ON task_authority_epochs
                BEGIN SELECT RAISE(ABORT,'task authority tombstones cannot be deleted'); END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_no_rewind BEFORE UPDATE ON task_authority_epochs
                WHEN NEW.task_id IS NOT OLD.task_id OR NEW.revision_floor<OLD.revision_floor
                BEGIN SELECT RAISE(ABORT,'task authority epoch cannot rewind'); END;
            CREATE TRIGGER IF NOT EXISTS task_revision_no_rewind BEFORE UPDATE OF authority_revision ON tasks
                WHEN NEW.authority_revision<OLD.authority_revision
                BEGIN SELECT RAISE(ABORT,'task authority revision cannot rewind'); END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_insert AFTER INSERT ON tasks BEGIN
                UPDATE tasks SET authority_revision=MAX(NEW.authority_revision,
                    COALESCE((SELECT revision_floor FROM task_authority_epochs WHERE task_id=NEW.id),1))
                    WHERE id=NEW.id;
                INSERT INTO task_authority_epochs(task_id,revision_floor)
                    SELECT id,authority_revision FROM tasks WHERE id=NEW.id
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_update AFTER UPDATE OF authority_revision ON tasks BEGIN
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(NEW.id,NEW.authority_revision)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_delete AFTER DELETE ON tasks BEGIN
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(OLD.id,OLD.authority_revision+1)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;
            CREATE TRIGGER IF NOT EXISTS task_epoch_rename AFTER UPDATE OF id ON tasks
                WHEN OLD.id IS NOT NEW.id BEGIN
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(OLD.id,OLD.authority_revision+1)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
                INSERT INTO task_authority_epochs(task_id,revision_floor) VALUES(NEW.id,OLD.authority_revision+1)
                    ON CONFLICT(task_id) DO UPDATE SET revision_floor=MAX(revision_floor,excluded.revision_floor);
            END;")
            .map_err(|e| format!("init task authority epochs: {e}"))?;
        tx.execute_batch(&ddl)
            .map_err(|e| format!("init task authority schema: {e}"))?;
        tx.commit()
            .map_err(|e| format!("commit task authority schema: {e}"))
    }
}
