//! H13 (D-H3, D8): protected `spawn_ephemeral` children as pending jobs
//! bound to the parent run.
//!
//! In protected mode an ephemeral child is not a new agent: it runs under the
//! parent employee's own identity (uid, storage, binding — D-H3), so it is
//! filed here instead of scaffolding `agents/.ephemeral/<id>` (which would be
//! an unconfirmed employee with its own identity). The job carries the
//! parent's employee id AND incarnation, its binding, the parent run, the
//! requesting human and the record task, all taken from the host's live run
//! association (`delegation_source`), never from arguments; its tool set is
//! the boundary's intersection (`tool_policy::ephemeral_tools`), never more
//! than the parent run's signed snapshot.
//!
//! D8 (HX/D8 revision §1): at most one capability-holding run per employee.
//! The child of the same employee would be a second one while the parent run
//! holds its capability, and the parent waiting for it would be
//! `WouldDeadlock`. So a job is never awaited by the parent: it is `pending`
//! and [`EphemeralJobs::take_startable`] releases it only after the parent
//! run no longer holds the capability ([`ParentRuns`]) and the employee is
//! still the same id and incarnation ([`ParentIdentity`]); anything else
//! keeps it pending (unreadable) or drops it (expired, tombstoned, restored).
//! Release is one-shot. H13b: the dispatcher tick releases jobs with the
//! production liveness (`isolation_ephemeral_runs::LiveRuns`) and starts each
//! as a background self-delegation of the parent employee
//! (`isolation_ephemeral_start`, `isolation_background::ephemeral_child`),
//! whose receipt is captured here while the parent run is still live.
//!
//! H13c: the dispatcher's self-delegation exception consumes the job
//! (`released → started`, once; [`EphemeralJobs::begin_start`]) and the child
//! run is authorized only from a started job. A Personal parent files a child
//! only from a live run its owner started, never from a run that itself came
//! from a background receipt (`same_owner::ephemeral_parent_live`). D8: at
//! most one job per employee is released at a time (see `take_startable`);
//! spawnd's admission coordinator stays the authoritative per-employee slot.
use std::path::Path;
use duduclaw_isolation::{capability::{EmployeeBinding, Lane}, execution::RunHandle, ids::EmployeeId};
use duduclaw_mcp_boundary::{RequestContext, VerificationError};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};

/// Private job store in the DuDuClaw home (`fs_safe::sqlite`).
pub const JOBS_DB: &str = "isolation-ephemeral-jobs.db";
/// Pending children one parent run may hold.
pub const MAX_PENDING_PER_RUN: usize = 4;
/// Pending children overall (release scans at most this many).
pub const MAX_PENDING: usize = 256;
/// A job not started within this time is dropped (same 24 h hard TTL as the
/// unprotected scaffold).
pub const JOB_TTL_SECONDS: u64 = 24 * 60 * 60;
/// Audit event written when a job is queued.
pub const QUEUED_EVENT: &str = "isolation_ephemeral_job_queued";

/// The parent run a child is bound to (host facts only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParentRun {
    pub agent: String,
    pub employee: EmployeeId,
    /// F6b: the parent employee's incarnation pinned at the parent's prepare.
    pub incarnation: u32,
    pub binding: EmployeeBinding,
    pub run: RunHandle,
    pub lane: Lane,
    /// The authenticated human of the parent run; for a Personal parent its
    /// owner (D-H3: only the owner's requests can cause a child).
    pub requester: String,
    /// The parent run's own record task, if any (ownership rechecked live).
    pub task: Option<String>,
}

/// What the child should do; already authorized by the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphemeralRequest {
    pub instruction: String,
    pub context: String,
    pub tier: String,
    /// `tool_policy::ephemeral_tools` (requested ∩ parent run).
    pub tools: Vec<String>,
}

/// One pending child under its parent's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EphemeralJob {
    pub id: String,
    pub parent: ParentRun,
    pub tools: Vec<String>,
    pub tier: String,
    pub instruction: String,
    pub context: String,
    pub created_at: u64,
    pub expires_at: u64,
}

impl EphemeralJob {
    /// A child of `parent`. Refuses a Personal parent whose requester is not
    /// its owner, an empty/oversized/duplicated tool set and empty texts.
    pub fn new(parent: ParentRun, request: EphemeralRequest, now: u64) -> Result<Self, VerificationError> {
        if let EmployeeBinding::Personal(owner) = &parent.binding {
            if owner.as_str() != parent.requester { return Err(VerificationError::BindingMismatch); }
        }
        let tools = &request.tools;
        if tools.is_empty() || tools.len() > duduclaw_mcp_boundary::tool_policy::spawn::EPHEMERAL_MAX_TOOLS
            || tools.iter().enumerate().any(|(index, tool)| tool.is_empty() || tools[..index].contains(tool))
            || request.instruction.trim().is_empty() || request.context.trim().is_empty()
            || parent.requester.is_empty() || !duduclaw_core::is_valid_agent_id(&parent.agent) {
            return Err(VerificationError::Invalid);
        }
        Ok(Self { id: uuid::Uuid::new_v4().to_string(), parent, tools: request.tools, tier: request.tier,
            instruction: request.instruction, context: request.context, created_at: now,
            expires_at: now.checked_add(JOB_TTL_SECONDS).ok_or(VerificationError::InvalidTime)? })
    }
}

/// Lifecycle state of a stored job: `pending → released` (parent exited,
/// [`EphemeralJobs::take_startable`], one-shot) `→ started` (H13c: the
/// dispatcher gate admitted its self-delegation, [`EphemeralJobs::begin_start`],
/// once) `→ finished` (H13d: terminal, the child's run ended; no second run
/// from the stored receipt), or `→ dropped`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState { Pending, Released, Started, Finished, Dropped }

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ephemeral_jobs(
        id TEXT PRIMARY KEY, employee TEXT NOT NULL, run TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('pending','released','started','finished','dropped')),
        job TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);";
const INDEX: &str = "CREATE INDEX IF NOT EXISTS ephemeral_jobs_state ON ephemeral_jobs(state, created_at);";

/// Does the parent run still hold the employee's one capability-holding run
/// (D8)? An error is treated as "still holds" (the job keeps waiting).
pub trait ParentRuns {
    fn holds_capability(&self, employee: EmployeeId, run: RunHandle) -> Result<bool, VerificationError>;
}

/// The employee currently holding the parent's label: `(id, incarnation)`.
/// `Unavailable` keeps the job pending; any other error drops it.
pub trait ParentIdentity {
    fn current(&self, agent: &str) -> Result<(EmployeeId, u32), VerificationError>;
}

/// Production [`ParentIdentity`]: the A2 lifecycle read (root ReadBack).
pub struct LiveLifecycle<'a> { pub home: &'a Path }
impl ParentIdentity for LiveLifecycle<'_> {
    fn current(&self, agent: &str) -> Result<(EmployeeId, u32), VerificationError> {
        crate::isolation_lifecycle::read_lifecycle(self.home, agent)
            .map(|lifecycle| (lifecycle.employee, lifecycle.incarnation))
            .map_err(|error| match error {
                crate::isolation_lifecycle::LifecycleError::Unavailable => VerificationError::Unavailable,
                _ => VerificationError::Revoked,
            })
    }
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn store_error<E>(_: E) -> VerificationError { VerificationError::Unavailable }

/// The stored job `id` if its row is in `state` (`None`: any state). A row
/// whose stored job disagrees with its keys is `Revoked`.
fn read_job(db: &Connection, id: &str, state: Option<&str>) -> Result<Option<EphemeralJob>, VerificationError> {
    use rusqlite::OptionalExtension;
    let row: Option<(String, String, String)> = db.query_row(
        "SELECT employee,run,job FROM ephemeral_jobs WHERE id=?1 AND (?2 IS NULL OR state=?2)",
        params![id, state], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(store_error)?;
    let Some((employee, run, encoded)) = row else { return Ok(None) };
    let job: EphemeralJob = serde_json::from_str(&encoded).map_err(|_| VerificationError::Revoked)?;
    if job.id != id || hex(&job.parent.employee.0) != employee || hex(&(job.parent.run.0).0) != run {
        return Err(VerificationError::Revoked);
    }
    Ok(Some(job))
}

/// The job store. Blocking SQLite: call from the blocking pool.
pub struct EphemeralJobs { db: Connection }

impl EphemeralJobs {
    /// The private store in `home` (created 0600 if missing).
    pub fn open(home: &Path) -> Result<Self, VerificationError> {
        #[cfg(unix)]
        { Self::from_connection(crate::fs_safe::sqlite::open(home, JOBS_DB).map_err(store_error)?) }
        #[cfg(not(unix))]
        { let _ = home; Err(VerificationError::Unavailable) }
    }

    pub fn from_connection(mut db: Connection) -> Result<Self, VerificationError> {
        use rusqlite::OptionalExtension;
        db.busy_timeout(std::time::Duration::from_millis(500)).map_err(store_error)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let existing: Option<String> = tx.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='ephemeral_jobs'", [], |row| row.get(0))
            .optional().map_err(store_error)?;
        // H13c/H13d: a store created before the `started`/`finished` states
        // keeps its rows (same columns) under the widened CHECK constraint.
        if existing.as_deref().is_some_and(|sql| !sql.contains("'finished'")) {
            tx.execute_batch("ALTER TABLE ephemeral_jobs RENAME TO ephemeral_jobs_h13b;").map_err(store_error)?;
            tx.execute_batch(SCHEMA).map_err(store_error)?;
            tx.execute_batch("INSERT INTO ephemeral_jobs(id,employee,run,state,job,created_at,expires_at)
                SELECT id,employee,run,state,job,created_at,expires_at FROM ephemeral_jobs_h13b;
                DROP TABLE ephemeral_jobs_h13b;").map_err(store_error)?;
        }
        tx.execute_batch(SCHEMA).map_err(store_error)?;
        tx.execute_batch(INDEX).map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(Self { db })
    }

    /// File `job` as pending. Bounded per parent run and overall.
    pub fn queue(&mut self, job: &EphemeralJob) -> Result<(), VerificationError> {
        let encoded = serde_json::to_string(job).map_err(|_| VerificationError::Invalid)?;
        let (employee, run) = (hex(&job.parent.employee.0), hex(&(job.parent.run.0).0));
        let created = i64::try_from(job.created_at).map_err(|_| VerificationError::InvalidTime)?;
        let expires = i64::try_from(job.expires_at).map_err(|_| VerificationError::InvalidTime)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let per_run: i64 = tx.query_row("SELECT COUNT(*) FROM ephemeral_jobs WHERE state='pending' AND run=?1",
            [&run], |row| row.get(0)).map_err(store_error)?;
        let total: i64 = tx.query_row("SELECT COUNT(*) FROM ephemeral_jobs WHERE state='pending'", [],
            |row| row.get(0)).map_err(store_error)?;
        if per_run >= MAX_PENDING_PER_RUN as i64 || total >= MAX_PENDING as i64 {
            return Err(VerificationError::Unavailable);
        }
        tx.execute("INSERT INTO ephemeral_jobs(id,employee,run,state,job,created_at,expires_at)
            VALUES(?1,?2,?3,'pending',?4,?5,?6)", params![job.id, employee, run, encoded, created, expires])
            .map_err(store_error)?;
        tx.commit().map_err(store_error)
    }

    pub fn state(&self, id: &str) -> Result<Option<JobState>, VerificationError> {
        use rusqlite::OptionalExtension;
        let state: Option<String> = self.db.query_row("SELECT state FROM ephemeral_jobs WHERE id=?1", [id],
            |row| row.get(0)).optional().map_err(store_error)?;
        state.map(|state| match state.as_str() {
            "pending" => Ok(JobState::Pending), "released" => Ok(JobState::Released),
            "started" => Ok(JobState::Started), "finished" => Ok(JobState::Finished),
            "dropped" => Ok(JobState::Dropped),
            _ => Err(VerificationError::Unavailable),
        }).transpose()
    }

    fn job_in(&self, id: &str, state: Option<&str>) -> Result<Option<EphemeralJob>, VerificationError> {
        read_job(&self.db, id, state)
    }

    /// H13b: the job `id` if it was released and its self-delegation has not
    /// been admitted yet.
    pub fn released(&self, id: &str) -> Result<Option<EphemeralJob>, VerificationError> {
        self.job_in(id, Some("released"))
    }

    /// H13c: the job `id` if the dispatcher gate admitted it (the only state a
    /// child run is authorized from, `authorize_current`).
    pub fn started(&self, id: &str) -> Result<Option<EphemeralJob>, VerificationError> {
        self.job_in(id, Some("started"))
    }

    /// H13c: the job `id` in any state (its tool digest is bound into the
    /// child's receipt material, `ephemeral_child::bind_job_tools`).
    pub fn job(&self, id: &str) -> Result<Option<EphemeralJob>, VerificationError> {
        self.job_in(id, None)
    }

    /// H13c: consume the dispatcher's self-delegation exception of job `id`:
    /// `released → started`, once. `check` sees the released job first and
    /// may refuse (nothing changes then). The transition itself is one
    /// conditional UPDATE, so of two concurrent callers at most one gets the
    /// job. `None`: the job is not (or no longer) released. H13d: a job with
    /// `now >= expires_at` is dropped and never started.
    pub fn begin_start(&self, id: &str, now: u64, check: impl FnOnce(&EphemeralJob) -> Result<(), VerificationError>)
        -> Result<Option<EphemeralJob>, VerificationError> {
        let Some(job) = self.released(id)? else { return Ok(None) };
        if now >= job.expires_at {
            self.drop_released(id)?;
            return Ok(None);
        }
        check(&job)?;
        let changed = self.db.execute("UPDATE ephemeral_jobs SET state='started' WHERE id=?1 AND state='released'", [id])
            .map_err(store_error)?;
        Ok((changed == 1).then_some(job))
    }

    /// A released job whose start failed (its queue row could not be
    /// written): dropped, so it never holds its employee's slot (D8).
    pub fn drop_released(&self, id: &str) -> Result<(), VerificationError> {
        self.db.execute("UPDATE ephemeral_jobs SET state='dropped' WHERE id=?1 AND state='released'", [id])
            .map(|_| ()).map_err(store_error)
    }

    /// H13d: the child's run ended: `started → finished` (terminal, idempotent;
    /// any other state is untouched). A finished job authorizes no run and
    /// no longer holds its employee's slot.
    pub fn finish(&self, id: &str) -> Result<(), VerificationError> {
        self.db.execute("UPDATE ephemeral_jobs SET state='finished' WHERE id=?1 AND state='started'", [id])
            .map(|_| ()).map_err(store_error)
    }

    /// Release (once) pending jobs whose parent run no longer holds the
    /// capability and whose employee is still the same id and incarnation.
    /// Expired, corrupt or re-identified jobs are dropped; unreadable
    /// liveness/identity keeps a job pending.
    ///
    /// H13c (D8): at most ONE job per employee is released per call, and none
    /// while another job of that employee is still `released` (enqueued, its
    /// run not admitted yet) or `started` (admitted, its run not bound or not
    /// ended yet; H13d: `finish` clears it when the run ends) and unexpired —
    /// such a child counts as holding the employee's slot even before its run
    /// is reserved/bound. spawnd's
    /// admission coordinator remains the authoritative D8 enforcement: a
    /// second run of the same employee waits behind the first
    /// (`deferred_until_current_run_ends`), whatever the gateway releases.
    pub fn take_startable(&mut self, runs: &dyn ParentRuns, identity: &dyn ParentIdentity, now: u64)
        -> Result<Vec<EphemeralJob>, VerificationError> {
        let now_db = i64::try_from(now).map_err(|_| VerificationError::InvalidTime)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let mut busy: std::collections::BTreeSet<String> = {
            let mut statement = tx.prepare("SELECT DISTINCT employee FROM ephemeral_jobs
                WHERE state IN ('released','started') AND expires_at > ?1").map_err(store_error)?;
            let employees = statement.query_map([now_db], |row| row.get::<_, String>(0)).map_err(store_error)?
                .collect::<Result<std::collections::BTreeSet<String>, _>>().map_err(store_error)?;
            employees
        };
        let mut statement = tx.prepare("SELECT id,employee,run,job FROM ephemeral_jobs WHERE state='pending'
            ORDER BY created_at, id LIMIT ?1").map_err(store_error)?;
        let rows: Vec<(String, String, String, String)> = statement
            .query_map([MAX_PENDING as i64], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .map_err(store_error)?.collect::<Result<_, _>>().map_err(store_error)?;
        drop(statement);
        let mut released = Vec::new();
        for (id, employee, run, encoded) in rows {
            let decision = match serde_json::from_str::<EphemeralJob>(&encoded) {
                Err(_) => Some(JobState::Dropped),
                Ok(job) if job.id != id || hex(&job.parent.employee.0) != employee
                    || hex(&(job.parent.run.0).0) != run || now >= job.expires_at => Some(JobState::Dropped),
                // D8: one child of this employee is already on its way.
                Ok(_) if busy.contains(&employee) => None,
                Ok(job) => match runs.holds_capability(job.parent.employee, job.parent.run) {
                    Ok(false) => match identity.current(&job.parent.agent) {
                        Ok(current) if current == (job.parent.employee, job.parent.incarnation) => {
                            busy.insert(employee.clone());
                            released.push(job);
                            Some(JobState::Released)
                        }
                        Err(VerificationError::Unavailable) => None,
                        _ => Some(JobState::Dropped),
                    },
                    // D8: the parent still holds the capability (or liveness
                    // is unreadable): keep waiting.
                    Ok(true) | Err(_) => None,
                },
            };
            if let Some(state) = decision {
                let state = if state == JobState::Released { "released" } else { "dropped" };
                tx.execute("UPDATE ephemeral_jobs SET state=?1 WHERE id=?2 AND state='pending'", params![state, id])
                    .map_err(store_error)?;
            }
        }
        tx.commit().map_err(store_error)?;
        Ok(released)
    }
}

/// Protected `spawn_ephemeral`, after `tool_policy::authorize`: file a pending
/// child of the CALLING run under its own identity. The parent is the live
/// run association of `context` (`delegation_source`: running, same
/// employee, binding and incarnation as prepared); the sender must be its
/// agent. Never awaits the child (D8).
///
/// H13b: while the parent is live, the child's background authority is
/// captured as a self-delegation receipt (`ephemeral_child::record_child`:
/// fresh lifecycle + users.db reads; a Personal parent only when its
/// lifecycle owner personally started this run). A child never files a child.
pub async fn queue_for_parent(home: &Path, sender: &str, context: &RequestContext, request: EphemeralRequest)
    -> Result<EphemeralJob, VerificationError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Err(VerificationError::Invalid); }
    #[cfg(target_os = "linux")]
    {
        let (blocking_home, blocking_sender, blocking_context) = (home.to_owned(), sender.to_owned(), context.clone());
        let job = duduclaw_mcp_boundary::authority::verify_blocking(move || {
            use duduclaw_isolation::capability::Principal;
            let (home, sender, context) = (blocking_home, blocking_sender, blocking_context);
            let source = crate::isolation_mcp::delegation_source(&home, &sender, &context)?;
            let record = context.record();
            if source.employee != record.employee || source.run != RunHandle(record.run)
                || source.binding != record.employee_binding {
                return Err(VerificationError::BindingMismatch);
            }
            // Depth 1: the run of an ephemeral child never files another child.
            if source.parent.as_ref().is_some_and(crate::isolation_background::is_ephemeral_child) {
                return Err(VerificationError::Invalid);
            }
            // H13c: a Personal parent only from a live run its owner started —
            // never from a run that itself came from a background receipt.
            crate::isolation_background::same_owner::ephemeral_parent_live(&source.binding, source.parent.as_ref())?;
            let Principal::Human { user_id, .. } = &record.principal else { return Err(VerificationError::Invalid); };
            let parent = ParentRun { agent: source.agent, employee: source.employee, incarnation: source.incarnation,
                binding: source.binding, run: source.run, lane: source.lane, requester: user_id.as_str().to_owned(),
                task: record.task.as_ref().map(|task| task.as_str().to_owned()) };
            EphemeralJob::new(parent, request, duduclaw_mcp_boundary::authority::now_seconds()?)
        }).await?;
        crate::isolation_background::ephemeral_child::record_child(home, sender, context, &job).await?;
        let home = home.to_owned();
        duduclaw_mcp_boundary::authority::verify_blocking(move || {
            EphemeralJobs::open(&home)?.queue(&job)?;
            duduclaw_security::audit::append_audit_event(&home, &duduclaw_security::audit::AuditEvent::new(
                QUEUED_EVENT, &job.parent.agent, duduclaw_security::audit::Severity::Info,
                serde_json::json!({"job": job.id, "employee": hex(&job.parent.employee.0),
                    "incarnation": job.parent.incarnation, "tools": job.tools.len(), "stage": "pending_parent_exit"})));
            Ok(job)
        }).await
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = (home, sender, context, request); Err(VerificationError::Unavailable) }
}

#[cfg(test)]
#[path = "isolation_ephemeral_tests.rs"]
mod tests;
