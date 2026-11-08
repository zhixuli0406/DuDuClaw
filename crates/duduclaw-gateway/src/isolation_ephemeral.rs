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
//! Release is one-shot. Starting a released job as a run is the consumer's
//! job (see the H13 follow-up in the work breakdown).
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

/// Lifecycle state of a stored job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState { Pending, Released, Dropped }

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

    pub fn from_connection(db: Connection) -> Result<Self, VerificationError> {
        db.busy_timeout(std::time::Duration::from_millis(500)).map_err(store_error)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS ephemeral_jobs(
                id TEXT PRIMARY KEY, employee TEXT NOT NULL, run TEXT NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('pending','released','dropped')),
                job TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS ephemeral_jobs_state ON ephemeral_jobs(state, created_at);")
            .map_err(store_error)?;
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
            "dropped" => Ok(JobState::Dropped), _ => Err(VerificationError::Unavailable),
        }).transpose()
    }

    /// Release (once) every pending job whose parent run no longer holds the
    /// capability and whose employee is still the same id and incarnation.
    /// Expired, corrupt or re-identified jobs are dropped; unreadable
    /// liveness/identity keeps a job pending.
    pub fn take_startable(&mut self, runs: &dyn ParentRuns, identity: &dyn ParentIdentity, now: u64)
        -> Result<Vec<EphemeralJob>, VerificationError> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
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
                Ok(job) => match runs.holds_capability(job.parent.employee, job.parent.run) {
                    Ok(false) => match identity.current(&job.parent.agent) {
                        Ok(current) if current == (job.parent.employee, job.parent.incarnation) => {
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
pub async fn queue_for_parent(home: &Path, sender: &str, context: &RequestContext, request: EphemeralRequest)
    -> Result<EphemeralJob, VerificationError> {
    if !duduclaw_core::execution_gate::isolation_required() { return Err(VerificationError::Invalid); }
    #[cfg(target_os = "linux")]
    {
        let (home, sender, context) = (home.to_owned(), sender.to_owned(), context.clone());
        duduclaw_mcp_boundary::authority::verify_blocking(move || {
            use duduclaw_isolation::capability::Principal;
            let source = crate::isolation_mcp::delegation_source(&home, &sender, &context)?;
            let record = context.record();
            if source.employee != record.employee || source.run != RunHandle(record.run)
                || source.binding != record.employee_binding {
                return Err(VerificationError::BindingMismatch);
            }
            let Principal::Human { user_id, .. } = &record.principal else { return Err(VerificationError::Invalid); };
            let parent = ParentRun { agent: source.agent, employee: source.employee, incarnation: source.incarnation,
                binding: source.binding, run: source.run, lane: source.lane, requester: user_id.as_str().to_owned(),
                task: record.task.as_ref().map(|task| task.as_str().to_owned()) };
            let job = EphemeralJob::new(parent, request, duduclaw_mcp_boundary::authority::now_seconds()?)?;
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
