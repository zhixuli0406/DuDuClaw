//! Gateway-owned job records. A handle is a selector, never bearer authority.
#[cfg(feature = "binding-store")]
use std::{sync::{Arc, Mutex}, path::Path};
use duduclaw_isolation::execution::RunHandle;
use serde_json::{Value, json};
use crate::{RequestContext, VerificationError};

pub trait ExecutionJobs: Send + Sync {
    fn insert(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<(), VerificationError>;
    fn read(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<Value, VerificationError>;
    fn finish(&self, run: RunHandle, result: Value) -> Result<(), VerificationError>;
    /// Cooperative stop is persisted before writing to an employee helper.
    /// It does not certify either a flush or cgroup retirement.
    fn request_stop(&self, _context: &RequestContext, _tool: &str, _run: RunHandle) -> Result<(), VerificationError> {
        Err(VerificationError::Unavailable)
    }
    fn stop_requested(&self, _run: RunHandle) -> Result<bool, VerificationError> {
        Err(VerificationError::Unavailable)
    }
    fn cancel_requested(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<(), VerificationError>;
}

pub fn handle_text(run: RunHandle) -> String {
    run.0.0.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub fn parse_handle(value: &str) -> Result<RunHandle, VerificationError> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(VerificationError::Invalid);
    }
    let mut bytes = [0; 16];
    for (i, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let digit = |b: u8| if b.is_ascii_digit() { b - b'0' } else { b - b'a' + 10 };
        bytes[i] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(RunHandle(duduclaw_isolation::ids::RunId(bytes)))
}

/// Open only at trusted gateway boot, before admitting runs. Employee content
/// never supplies this path. Unfinished rows from a prior process remain
/// uncertain; restart is not evidence of either completion or retirement.
#[cfg(feature = "binding-store")]
pub struct SqliteExecutionJobs { connection: Mutex<rusqlite::Connection> }
#[cfg(feature = "binding-store")]
impl SqliteExecutionJobs {
    /// Trusted host control only. The gateway must re-read the authenticated
    /// dashboard Admin role at use time; no MCP/capability impersonation.
    pub fn admin_recording_control(&self, run: RunHandle, stop: bool) -> Result<Value, VerificationError> {
        let mut connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| VerificationError::Unavailable)?;
        let (state, result): (String, Option<String>) = transaction.query_row(
            "SELECT state,result FROM execution_jobs WHERE run=?1 AND tool='browser_record_start'",
            [handle_text(run)], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|_| VerificationError::Invalid)?;
        let state = if stop && state == "pending" {
            transaction.execute("UPDATE execution_jobs SET state='stop_requested',updated=unixepoch() WHERE run=?1 AND tool='browser_record_start' AND state='pending'",
                [handle_text(run)]).map_err(|_| VerificationError::Unavailable)?;
            "stop_requested".to_owned()
        } else { state };
        let result = result.map(|text| serde_json::from_str::<Value>(&text).map_err(|_| VerificationError::Invalid)).transpose()?;
        transaction.commit().map_err(|_| VerificationError::Unavailable)?;
        Ok(json!({"job":handle_text(run),"status":state,"result":result}))
    }
    pub fn open(path: &Path) -> Result<Arc<Self>, VerificationError> {
        let connection = rusqlite::Connection::open(path).map_err(|_| VerificationError::Unavailable)?;
        connection.busy_timeout(std::time::Duration::from_secs(2)).map_err(|_| VerificationError::Unavailable)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS execution_jobs (
              run TEXT PRIMARY KEY, employee BLOB NOT NULL, principal TEXT NOT NULL,
              binding TEXT NOT NULL, tool TEXT NOT NULL, state TEXT NOT NULL,
              result TEXT, updated INTEGER NOT NULL DEFAULT (unixepoch()));
            UPDATE execution_jobs SET state='uncertain' WHERE state IN ('pending','cancel_requested','stop_requested');").map_err(|_| VerificationError::Unavailable)?;
        Ok(Arc::new(Self { connection: Mutex::new(connection) }))
    }
}
#[cfg(feature = "binding-store")]
fn identity(context: &RequestContext) -> Result<(String, String), VerificationError> {
    Ok((serde_json::to_string(&context.record().principal).map_err(|_| VerificationError::Invalid)?,
        serde_json::to_string(&context.record().employee_binding).map_err(|_| VerificationError::Invalid)?))
}
#[cfg(feature = "binding-store")]
impl ExecutionJobs for SqliteExecutionJobs {
    fn insert(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<(), VerificationError> {
        let (principal, binding) = identity(context)?;
        let mut connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| VerificationError::Unavailable)?;
        // This expires retained metadata only; it is never proof of retirement.
        transaction.execute("DELETE FROM execution_jobs WHERE state IN ('complete','uncertain') AND updated < unixepoch()-604800", [])
            .map_err(|_| VerificationError::Unavailable)?;
        let employee_count: u64 = transaction.query_row("SELECT count(*) FROM execution_jobs WHERE employee=?1",
            [context.record().employee.0.as_slice()], |row| row.get(0)).map_err(|_| VerificationError::Unavailable)?;
        if employee_count >= 32 {
            let evicted = transaction.execute("DELETE FROM execution_jobs WHERE run=(SELECT run FROM execution_jobs
                WHERE employee=?1 AND state='complete' ORDER BY updated,run LIMIT 1)",
                [context.record().employee.0.as_slice()]).map_err(|_| VerificationError::Unavailable)?;
            if evicted != 1 { return Err(VerificationError::Unavailable); }
        }
        let count: u64 = transaction.query_row("SELECT count(*) FROM execution_jobs", [], |row| row.get(0))
            .map_err(|_| VerificationError::Unavailable)?;
        if count >= 4096 { return Err(VerificationError::Unavailable); }
        transaction.execute("INSERT INTO execution_jobs(run,employee,principal,binding,tool,state) VALUES (?1,?2,?3,?4,?5,'pending')",
            rusqlite::params![handle_text(run), context.record().employee.0.as_slice(), principal, binding, tool])
            .map_err(|_| VerificationError::Unavailable)?;
        transaction.commit().map_err(|_| VerificationError::Unavailable)
    }
    fn read(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<Value, VerificationError> {
        let (principal, binding) = identity(context)?;
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let (state, result): (String, Option<String>) = connection.query_row(
            "SELECT state,result FROM execution_jobs WHERE run=?1 AND employee=?2 AND principal=?3 AND binding=?4 AND tool=?5",
            rusqlite::params![handle_text(run), context.record().employee.0.as_slice(), principal, binding, tool],
            |row| Ok((row.get(0)?, row.get(1)?))).map_err(|_| VerificationError::Invalid)?;
        let result = result.map(|text| serde_json::from_str::<Value>(&text).map_err(|_| VerificationError::Invalid)).transpose()?;
        Ok(json!({"job":handle_text(run), "status":state, "result":result}))
    }
    fn finish(&self, run: RunHandle, result: Value) -> Result<(), VerificationError> {
        let state = if result["status"] == "uncertain" { "uncertain" } else { "complete" };
        let result = serde_json::to_string(&result).map_err(|_| VerificationError::Invalid)?;
        if result.len() > 1024 * 1024 { return Err(VerificationError::Invalid); }
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        if connection.execute("UPDATE execution_jobs SET state=?3,result=?2,updated=unixepoch() WHERE run=?1 AND state IN ('pending','cancel_requested','stop_requested')",
            rusqlite::params![handle_text(run), result, state]).map_err(|_| VerificationError::Unavailable)? != 1 {
            return Err(VerificationError::Invalid);
        }
        Ok(())
    }
    fn request_stop(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<(), VerificationError> {
        self.read(context, tool, run)?;
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        connection.execute("UPDATE execution_jobs SET state='stop_requested',updated=unixepoch() WHERE run=?1 AND state='pending'",
            [handle_text(run)]).map_err(|_| VerificationError::Unavailable)?;
        Ok(())
    }
    fn stop_requested(&self, run: RunHandle) -> Result<bool, VerificationError> {
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        connection.query_row("SELECT state='stop_requested' FROM execution_jobs WHERE run=?1", [handle_text(run)],
            |row| row.get(0)).map_err(|_| VerificationError::Unavailable)
    }
    fn cancel_requested(&self, context: &RequestContext, tool: &str, run: RunHandle) -> Result<(), VerificationError> {
        self.read(context, tool, run)?;
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        connection.execute("UPDATE execution_jobs SET state='cancel_requested',updated=unixepoch() WHERE run=?1 AND state='pending'",
            [handle_text(run)]).map_err(|_| VerificationError::Unavailable)?;
        Ok(())
    }
}

#[cfg(all(test, feature = "binding-store"))]
mod tests {
    use super::*;
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};

    pub(super) fn context() -> RequestContext {
        RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![Text::new("execute_program".into()).unwrap()]).unwrap(),
            resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
                ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() }]).unwrap(),
            policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
            nonce: [5; 32], jti: [6; 16],
        })
    }

    pub(super) fn run() -> RunHandle { RunHandle(RunId([9; 16])) }

    #[test]
    fn job_handles_are_exact_lowercase_selectors() {
        assert!(parse_handle(&"ab".repeat(16)).is_ok());
        for bad in ["AB".repeat(16), "a".repeat(31), "g".repeat(32), "../job".into()] {
            assert!(parse_handle(&bad).is_err());
        }
        assert_eq!(parse_handle(&handle_text(run())).unwrap(), run());
    }

    #[test]
    fn execution_jobs_handle_does_not_authorize_other_actors_or_tools() {
        let home = tempfile::tempdir().unwrap();
        let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
        let owner = context();
        jobs.insert(&owner, "execute_program", run()).unwrap();
        assert_eq!(jobs.read(&owner, "execute_program", run()).unwrap()["status"], "pending");

        let mut employee = owner.record().clone(); employee.employee = EmployeeId([8; 16]);
        let mut principal = owner.record().clone();
        principal.principal = Principal::Human { user_id: Text::new("other".into()).unwrap(), generation: 1 };
        let mut generation = owner.record().clone();
        generation.principal = Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 2 };
        let mut binding = owner.record().clone();
        binding.employee_binding = EmployeeBinding::Shared(Text::new("owner".into()).unwrap());
        let mut tenant = owner.record().clone();
        tenant.principal = Principal::Background { tenant_id: Text::new("owner".into()).unwrap(), generation: 1 };
        for record in [employee, principal, generation, binding, tenant] {
            let thief = RequestContext::from_verified_record(record);
            assert!(jobs.read(&thief, "execute_program", run()).is_err());
            assert!(jobs.cancel_requested(&thief, "execute_program", run()).is_err());
        }
        assert!(jobs.read(&owner, "office_script", run()).is_err());
        assert!(jobs.cancel_requested(&owner, "office_script", run()).is_err());
        assert_eq!(jobs.read(&owner, "execute_program", run()).unwrap()["status"], "pending");
        // The same actor may retrieve its result in a later authenticated run;
        // the original caller run is provenance, not a bearer or a lease.
        let mut later = owner.record().clone(); later.run = RunId([7; 16]);
        assert!(jobs.read(&RequestContext::from_verified_record(later), "execute_program", run()).is_ok());
    }

    #[test]
    fn execution_jobs_cancel_ack_is_not_completion_and_finish_is_single_use() {
        let home = tempfile::tempdir().unwrap();
        let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
        let owner = context();
        jobs.insert(&owner, "execute_program", run()).unwrap();
        jobs.cancel_requested(&owner, "execute_program", run()).unwrap();
        jobs.cancel_requested(&owner, "execute_program", run()).unwrap();
        let waiting = jobs.read(&owner, "execute_program", run()).unwrap();
        assert_eq!(waiting["status"], "cancel_requested");
        assert_eq!(waiting["result"], Value::Null);
        let result = json!({"status":"cancelled", "outcome":{"kind":"cancelled"}});
        jobs.finish(run(), result.clone()).unwrap();
        let terminal = jobs.read(&owner, "execute_program", run()).unwrap();
        assert_eq!(terminal["status"], "complete");
        assert_eq!(terminal["result"], result);
        assert!(jobs.finish(run(), json!({"status":"replaced"})).is_err());
        jobs.cancel_requested(&owner, "execute_program", run()).unwrap();
        assert_eq!(jobs.read(&owner, "execute_program", run()).unwrap(), terminal);
    }

    #[test]
    fn execution_jobs_restart_preserves_terminal_results_and_marks_unfinished_uncertain() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("jobs.db");
        let owner = context();
        let completed = RunHandle(RunId([10; 16]));
        let cancelling = RunHandle(RunId([11; 16]));
        let result = json!({"status":"exited", "exit_code":0, "stdout":"繁體中文"});
        {
            let jobs = SqliteExecutionJobs::open(&path).unwrap();
            for handle in [run(), completed, cancelling] { jobs.insert(&owner, "execute_program", handle).unwrap(); }
            jobs.finish(completed, result.clone()).unwrap();
            jobs.cancel_requested(&owner, "execute_program", cancelling).unwrap();
        }
        let restarted = SqliteExecutionJobs::open(&path).unwrap();
        let terminal = restarted.read(&owner, "execute_program", completed).unwrap();
        assert_eq!(terminal["status"], "complete");
        assert_eq!(terminal["result"], result);
        for handle in [run(), cancelling] {
            let unknown = restarted.read(&owner, "execute_program", handle).unwrap();
            assert_eq!(unknown["status"], "uncertain");
            assert_eq!(unknown["result"], Value::Null);
            assert!(restarted.finish(handle, json!({"status":"exited"})).is_err());
        }
    }

    #[test]
    fn execution_jobs_admission_bound_only_prunes_expired_completed_rows() {
        let home = tempfile::tempdir().unwrap();
        let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
        let owner = context();
        jobs.insert(&owner, "execute_program", run()).unwrap();
        {
            let connection = jobs.connection.lock().unwrap();
            connection.execute_batch("WITH RECURSIVE numbers(n) AS (
                VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<4095)
                INSERT INTO execution_jobs(run,employee,principal,binding,tool,state)
                SELECT printf('%032x',n),X'00','{}','{}','fixture','pending' FROM numbers;").unwrap();
        }
        let extra = RunHandle(RunId([8; 16]));
        assert!(jobs.insert(&owner, "execute_program", extra).is_err());
        // Aging a pending or uncertain row never makes it safe to evict.
        jobs.connection.lock().unwrap().execute("UPDATE execution_jobs SET updated=0", []).unwrap();
        assert!(jobs.insert(&owner, "execute_program", extra).is_err());
        jobs.finish(run(), json!({"status":"exited"})).unwrap();
        assert!(jobs.insert(&owner, "execute_program", extra).is_err());
        jobs.connection.lock().unwrap().execute("UPDATE execution_jobs SET updated=0 WHERE state='complete'", []).unwrap();
        jobs.insert(&owner, "execute_program", extra).unwrap();
        assert!(jobs.read(&owner, "execute_program", run()).is_err());
        assert_eq!(jobs.read(&owner, "execute_program", extra).unwrap()["status"], "pending");
    }

    #[test]
    fn execution_jobs_storage_errors_and_oversized_results_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        assert!(SqliteExecutionJobs::open(home.path()).is_err());
        let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
        let owner = context();
        assert!(jobs.read(&owner, "execute_program", run()).is_err());
        assert!(jobs.cancel_requested(&owner, "execute_program", run()).is_err());
        assert!(jobs.finish(run(), json!({"status":"exited"})).is_err());
        jobs.insert(&owner, "execute_program", run()).unwrap();
        assert!(jobs.insert(&owner, "execute_program", run()).is_err());
        assert!(jobs.finish(run(), json!({"status":"exited", "stdout":"x".repeat(1024 * 1024)})).is_err());
        assert_eq!(jobs.read(&owner, "execute_program", run()).unwrap()["status"], "pending");
        jobs.finish(run(), json!({"status":"uncertain"})).unwrap();
        assert_eq!(jobs.read(&owner, "execute_program", run()).unwrap()["status"], "uncertain");
        assert!(jobs.finish(run(), json!({"status":"exited"})).is_err());
    }

    #[test]
    fn execution_jobs_employee_quota_does_not_evict_unfinished_work() {
        let home = tempfile::tempdir().unwrap();
        let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
        let owner = context();
        for byte in 1..=32 { jobs.insert(&owner, "execute_program", RunHandle(RunId([byte; 16]))).unwrap(); }
        let extra = RunHandle(RunId([33; 16]));
        assert!(jobs.insert(&owner, "execute_program", extra).is_err());
        jobs.finish(run(), json!({"status":"uncertain"})).unwrap();
        assert!(jobs.insert(&owner, "execute_program", extra).is_err());
        let finished = RunHandle(RunId([1; 16]));
        jobs.finish(finished, json!({"status":"exited"})).unwrap();
        jobs.insert(&owner, "execute_program", extra).unwrap();
        assert!(jobs.read(&owner, "execute_program", finished).is_err());
        assert_eq!(jobs.read(&owner, "execute_program", run()).unwrap()["status"], "uncertain");
        jobs.connection.lock().unwrap().execute("UPDATE execution_jobs SET updated=0 WHERE state='uncertain'", []).unwrap();
        jobs.insert(&owner, "execute_program", RunHandle(RunId([34; 16]))).unwrap();
        assert!(jobs.read(&owner, "execute_program", run()).is_err());
    }
}

#[cfg(all(test, feature = "binding-store"))]
mod recording_stop_tests {
    use super::*;
    #[test]
    fn execution_jobs_stop_is_durable_owned_and_never_terminal() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("jobs.db");
        let owner = super::tests::context();
        let run = super::tests::run();
        {
            let jobs = SqliteExecutionJobs::open(&path).unwrap();
            jobs.insert(&owner, "browser_record_start", run).unwrap();
            assert!(!jobs.stop_requested(run).unwrap());
            assert!(jobs.request_stop(&owner, "execute_program", run).is_err());
            let mut thief = owner.record().clone();
            thief.employee = duduclaw_isolation::ids::EmployeeId([8; 16]);
            assert!(jobs.request_stop(&RequestContext::from_verified_record(thief), "browser_record_start", run).is_err());
            assert!(!jobs.stop_requested(run).unwrap());
            jobs.request_stop(&owner, "browser_record_start", run).unwrap();
            jobs.request_stop(&owner, "browser_record_start", run).unwrap();
            assert!(jobs.stop_requested(run).unwrap());
            assert_eq!(jobs.read(&owner, "browser_record_start", run).unwrap()["status"], "stop_requested");
        }
        let jobs = SqliteExecutionJobs::open(&path).unwrap();
        assert_eq!(jobs.read(&owner, "browser_record_start", run).unwrap()["status"], "uncertain");
        assert!(!jobs.stop_requested(run).unwrap());
        assert!(jobs.finish(run, json!({"status":"stopped"})).is_err());
    }
}

#[cfg(all(test, feature = "binding-store"))]
mod recording_admin_tests {
    use super::*;
    #[test]
    fn execution_jobs_host_control_can_stop_without_employee_run_and_never_other_job_types() {
        let home = tempfile::tempdir().unwrap();
        let jobs = SqliteExecutionJobs::open(&home.path().join("jobs.db")).unwrap();
        let run = super::tests::run();
        let owner = super::tests::context();
        jobs.insert(&owner, "browser_record_start", run).unwrap();
        assert_eq!(jobs.admin_recording_control(run, false).unwrap()["status"], "pending");
        assert_eq!(jobs.admin_recording_control(run, true).unwrap()["status"], "stop_requested");
        assert!(jobs.stop_requested(run).unwrap());
        jobs.finish(run, json!({"status":"recorded"})).unwrap();
        assert_eq!(jobs.admin_recording_control(run, true).unwrap()["status"], "complete");
        let program = RunHandle(duduclaw_isolation::ids::RunId([10; 16]));
        jobs.insert(&owner, "execute_program", program).unwrap();
        assert!(jobs.admin_recording_control(program, false).is_err());
        assert!(jobs.admin_recording_control(program, true).is_err());
        assert_eq!(jobs.read(&owner, "execute_program", program).unwrap()["status"], "pending");
    }
}
