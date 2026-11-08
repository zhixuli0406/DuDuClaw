//! HX6 (D8 revision §4, [F8], [F11], [F16]): gateway store of in-run browser
//! recordings. A row is keyed `(run, call)`: the spawnd call id of the
//! recording-slot helper inside the caller's own run. It is a selector bound
//! to (employee, actor principal incl. generation, binding), never bearer
//! authority, and it has no `RunHandle` the run-killing paths accept.
//!
//! The decoded driver result is stored with every artifact's sha256 [F11]:
//! the runtime has RW on `/work` and can rewrite `recordings/<id>/*` at any
//! time, so every later reader (D-H1 work broker consumers, H12d's
//! `skill_from_recording`) must compare the bytes it read against these
//! digests (`verify_artifact`) and refuse `artifact_modified` on mismatch.
#[cfg(feature = "binding-store")]
use std::{path::Path, sync::{Arc, Mutex}};
use duduclaw_isolation::{execution::RunHandle, ids::EmployeeId, ipc::HelperCallId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use crate::{RequestContext, VerificationError};

/// Artifacts published under `/work/recordings/<id>/`, in result order.
pub const ARTIFACT_NAMES: [&str; 3] = ["trace.zip", "actions.json", "network.json"];
/// Per-artifact bound (`recording_helper::MAX_ARTIFACT`).
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
/// The workspace directory holding one directory per recording id.
pub const RECORDINGS_DIR: &str = "recordings";
/// Stored results are bounded like the decoder (`recording_helper::MAX_RESULT`).
const MAX_STORED_RESULT: usize = 64 * 1024 + 1024;

/// `(run, call)` of one in-run recording. `id()` is the user-facing selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingKey { pub run: RunHandle, pub call: HelperCallId }
impl RecordingKey {
    pub fn id(&self) -> String { self.call.hex() }
}

/// `browser_record_stop {id}`: exactly 32 lowercase hex, never all zero.
pub fn parse_id(id: &str) -> Result<HelperCallId, VerificationError> {
    let bytes = crate::execution_jobs::parse_handle(id)?.0.0;
    HelperCallId::new(bytes).map_err(|_| VerificationError::Invalid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState { Recording, StopRequested, Complete, Uncertain }
impl RecordingState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording", Self::StopRequested => "stop_requested",
            Self::Complete => "complete", Self::Uncertain => "uncertain",
        }
    }
    fn parse(text: &str) -> Result<Self, VerificationError> {
        Ok(match text {
            "recording" => Self::Recording, "stop_requested" => Self::StopRequested,
            "complete" => Self::Complete, "uncertain" => Self::Uncertain,
            _ => return Err(VerificationError::Invalid),
        })
    }
    pub fn terminal(self) -> bool { matches!(self, Self::Complete | Self::Uncertain) }
}

/// Who asked the recording to end before its own deadline (`ended_by`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopBy { Stop, Revoked, Admin }
impl StopBy {
    pub fn as_str(self) -> &'static str {
        match self { Self::Stop => "stop", Self::Revoked => "revoked", Self::Admin => "admin" }
    }
    fn parse(text: &str) -> Result<Self, VerificationError> {
        Ok(match text { "stop" => Self::Stop, "revoked" => Self::Revoked, "admin" => Self::Admin,
            _ => return Err(VerificationError::Invalid) })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecordingRow {
    pub key: RecordingKey,
    pub employee: EmployeeId,
    pub state: RecordingState,
    pub result: Option<Value>,
}
impl RecordingRow {
    /// What a stop/status caller sees: id, state and the stored result.
    pub fn view(&self) -> Value {
        json!({"id":self.key.id(), "status":self.state.as_str(), "result":self.result})
    }
}

/// One stored artifact digest (from the strictly decoded driver result).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDigest { pub name: &'static str, pub bytes: u64, pub sha256: String }

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    /// No stored digest for this name / recording (never published, or uncertain).
    #[error("artifact unknown")] Unknown,
    /// The bytes read now differ from the digest recorded at publication.
    #[error("artifact modified")] Modified,
}
impl ArtifactError {
    pub fn code(self) -> &'static str {
        match self { Self::Unknown => "artifact_unknown", Self::Modified => "artifact_modified" }
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The three artifact digests of a stored `recorded` result. Any other shape
/// (not recorded, uncertain, forged names/order/sizes/digests) has none.
pub fn artifacts_from_result(result: &Value) -> Result<Vec<ArtifactDigest>, ArtifactError> {
    if result["status"] != "recorded" { return Err(ArtifactError::Unknown); }
    let list = result["artifacts"].as_array().ok_or(ArtifactError::Unknown)?;
    if list.len() != ARTIFACT_NAMES.len() { return Err(ArtifactError::Unknown); }
    list.iter().zip(ARTIFACT_NAMES).map(|(artifact, name)| {
        let bytes = artifact["bytes"].as_u64().filter(|n| *n > 0 && *n <= MAX_ARTIFACT_BYTES);
        let sha256 = artifact["sha256"].as_str().filter(|value| valid_digest(value));
        match (artifact["name"].as_str() == Some(name), bytes, sha256) {
            (true, Some(bytes), Some(sha256)) => Ok(ArtifactDigest { name, bytes, sha256: sha256.to_owned() }),
            _ => Err(ArtifactError::Unknown),
        }
    }).collect()
}

/// [F11] Check bytes read back from `recordings/<id>/<name>` against the
/// stored digest. Size and sha256 must both match; anything else is
/// `artifact_modified` (the runtime can destroy, never forge, its artifacts).
pub fn verify_artifact(digests: &[ArtifactDigest], name: &str, bytes: &[u8]) -> Result<(), ArtifactError> {
    let digest = digests.iter().find(|digest| digest.name == name).ok_or(ArtifactError::Unknown)?;
    let actual: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    if bytes.len() as u64 != digest.bytes || actual != digest.sha256 { return Err(ArtifactError::Modified); }
    Ok(())
}

/// The gateway-owned recording store. Every actor-facing method re-checks
/// ownership inside its own transaction; host methods take no actor.
pub trait RecordingJobs: Send + Sync {
    /// New live row for a started recording-slot call of `context`'s run.
    fn insert(&self, context: &RequestContext, key: RecordingKey) -> Result<(), VerificationError>;
    /// Owner read by id: same employee, principal (incl. generation) and binding.
    fn read(&self, context: &RequestContext, call: HelperCallId) -> Result<RecordingRow, VerificationError>;
    /// Persist the actor's stop before `HelperStop` (live authority re-checked
    /// under the store lock). A terminal row is left unchanged.
    fn request_stop(&self, context: &RequestContext, key: RecordingKey) -> Result<(), VerificationError>;
    /// Host-side stop marks (revocation [F16], Admin RPC). No actor check.
    fn mark_stop(&self, key: RecordingKey, by: StopBy) -> Result<(), VerificationError>;
    /// Who asked the live recording to end, if anyone (collector, `ended_by`).
    fn stop_by(&self, key: RecordingKey) -> Result<Option<StopBy>, VerificationError>;
    /// Collector only, exactly once per row.
    fn finish(&self, key: RecordingKey, result: Value) -> Result<(), VerificationError>;
    /// Trusted host read (Admin RPC; the caller re-checked the Admin role).
    fn admin_read(&self, call: HelperCallId) -> Result<RecordingRow, VerificationError>;
    /// [F11] Stored digests of `employee`'s recording `call`.
    fn artifacts(&self, employee: EmployeeId, call: HelperCallId) -> Result<Vec<ArtifactDigest>, VerificationError> {
        let row = self.admin_read(call)?;
        if row.employee != employee || row.state != RecordingState::Complete { return Err(VerificationError::Invalid); }
        artifacts_from_result(row.result.as_ref().ok_or(VerificationError::Invalid)?).map_err(|_| VerificationError::Invalid)
    }
}

/// Opened once at gateway boot on `isolation-execution-jobs.db` (separate
/// table, separate connection). Unfinished rows of a previous gateway process
/// become uncertain: spawnd ended that run when our control connection closed.
#[cfg(feature = "binding-store")]
pub struct SqliteRecordingJobs { connection: Mutex<rusqlite::Connection> }

#[cfg(feature = "binding-store")]
fn identity(context: &RequestContext) -> Result<(String, String), VerificationError> {
    Ok((serde_json::to_string(&context.record().principal).map_err(|_| VerificationError::Invalid)?,
        serde_json::to_string(&context.record().employee_binding).map_err(|_| VerificationError::Invalid)?))
}

#[cfg(feature = "binding-store")]
impl SqliteRecordingJobs {
    pub fn open(path: &Path) -> Result<Arc<Self>, VerificationError> {
        let connection = rusqlite::Connection::open(path).map_err(|_| VerificationError::Unavailable)?;
        connection.busy_timeout(std::time::Duration::from_secs(2)).map_err(|_| VerificationError::Unavailable)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS recording_jobs (
              call TEXT PRIMARY KEY, run TEXT NOT NULL, employee BLOB NOT NULL, principal TEXT NOT NULL,
              binding TEXT NOT NULL,
              state TEXT NOT NULL CHECK (state IN ('recording','stop_requested','complete','uncertain')),
              stop_by TEXT CHECK (stop_by IS NULL OR stop_by IN ('stop','revoked','admin')),
              result TEXT, updated INTEGER NOT NULL DEFAULT (unixepoch()));
            UPDATE recording_jobs SET state='uncertain',
              result='{\"status\":\"uncertain\",\"reason\":\"gateway_restarted\"}', updated=unixepoch()
              WHERE state IN ('recording','stop_requested');").map_err(|_| VerificationError::Unavailable)?;
        Ok(Arc::new(Self { connection: Mutex::new(connection) }))
    }

    fn row(connection: &rusqlite::Connection, call: HelperCallId, owner: Option<(&RequestContext, &str, &str)>)
        -> Result<RecordingRow, VerificationError> {
        let mapped = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, Vec<u8>, String, Option<String>)> {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        };
        let (run, employee, state, result) = match owner {
            Some((context, principal, binding)) => connection.query_row(
                "SELECT run,employee,state,result FROM recording_jobs WHERE call=?1 AND employee=?2 AND principal=?3 AND binding=?4",
                rusqlite::params![call.hex(), context.record().employee.0.as_slice(), principal, binding], mapped),
            None => connection.query_row("SELECT run,employee,state,result FROM recording_jobs WHERE call=?1",
                [call.hex()], mapped),
        }.map_err(|_| VerificationError::Invalid)?;
        let employee: [u8; 16] = employee.try_into().map_err(|_| VerificationError::Invalid)?;
        let result = result.map(|text| serde_json::from_str::<Value>(&text).map_err(|_| VerificationError::Invalid)).transpose()?;
        Ok(RecordingRow { key: RecordingKey { run: crate::execution_jobs::parse_handle(&run)?, call },
            employee: EmployeeId(employee), state: RecordingState::parse(&state)?, result })
    }
}

#[cfg(feature = "binding-store")]
impl RecordingJobs for SqliteRecordingJobs {
    fn insert(&self, context: &RequestContext, key: RecordingKey) -> Result<(), VerificationError> {
        // In-run only: the row's run is the capability's own run.
        if key.run.0 != context.record().run { return Err(VerificationError::BindingMismatch); }
        let (principal, binding) = identity(context)?;
        let employee = context.record().employee;
        let mut connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| VerificationError::Unavailable)?;
        // Retained metadata only; never evidence of retirement.
        transaction.execute("DELETE FROM recording_jobs WHERE state IN ('complete','uncertain') AND updated < unixepoch()-604800", [])
            .map_err(|_| VerificationError::Unavailable)?;
        let per_employee: u64 = transaction.query_row("SELECT count(*) FROM recording_jobs WHERE employee=?1",
            [employee.0.as_slice()], |row| row.get(0)).map_err(|_| VerificationError::Unavailable)?;
        if per_employee >= 32 {
            let evicted = transaction.execute("DELETE FROM recording_jobs WHERE call=(SELECT call FROM recording_jobs
                WHERE employee=?1 AND state IN ('complete','uncertain') ORDER BY updated,call LIMIT 1)",
                [employee.0.as_slice()]).map_err(|_| VerificationError::Unavailable)?;
            if evicted != 1 { return Err(VerificationError::Unavailable); }
        }
        let total: u64 = transaction.query_row("SELECT count(*) FROM recording_jobs", [], |row| row.get(0))
            .map_err(|_| VerificationError::Unavailable)?;
        if total >= 4096 { return Err(VerificationError::Unavailable); }
        transaction.execute("INSERT INTO recording_jobs(call,run,employee,principal,binding,state) VALUES (?1,?2,?3,?4,?5,'recording')",
            rusqlite::params![key.call.hex(), crate::execution_jobs::handle_text(key.run), employee.0.as_slice(), principal, binding])
            .map_err(|_| VerificationError::Unavailable)?;
        transaction.commit().map_err(|_| VerificationError::Unavailable)
    }
    fn read(&self, context: &RequestContext, call: HelperCallId) -> Result<RecordingRow, VerificationError> {
        let (principal, binding) = identity(context)?;
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        Self::row(&connection, call, Some((context, principal.as_str(), binding.as_str())))
    }
    fn request_stop(&self, context: &RequestContext, key: RecordingKey) -> Result<(), VerificationError> {
        let (principal, binding) = identity(context)?;
        let mut connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| VerificationError::Unavailable)?;
        let row = Self::row(&transaction, key.call, Some((context, principal.as_str(), binding.as_str())))?;
        if row.key != key { return Err(VerificationError::Invalid); }
        // A blocking task can outlive its caller while waiting for the lock.
        context.verify_live(crate::authority::now_seconds()?)?;
        transaction.execute("UPDATE recording_jobs SET state='stop_requested',stop_by=coalesce(stop_by,'stop'),updated=unixepoch()
            WHERE call=?1 AND state IN ('recording','stop_requested')", [key.call.hex()]).map_err(|_| VerificationError::Unavailable)?;
        transaction.commit().map_err(|_| VerificationError::Unavailable)
    }
    fn mark_stop(&self, key: RecordingKey, by: StopBy) -> Result<(), VerificationError> {
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        connection.execute("UPDATE recording_jobs SET state='stop_requested',stop_by=coalesce(stop_by,?3),updated=unixepoch()
            WHERE call=?1 AND run=?2 AND state IN ('recording','stop_requested')",
            rusqlite::params![key.call.hex(), crate::execution_jobs::handle_text(key.run), by.as_str()])
            .map_err(|_| VerificationError::Unavailable)?;
        Ok(())
    }
    fn stop_by(&self, key: RecordingKey) -> Result<Option<StopBy>, VerificationError> {
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let value: Option<String> = connection.query_row("SELECT stop_by FROM recording_jobs WHERE call=?1 AND run=?2",
            rusqlite::params![key.call.hex(), crate::execution_jobs::handle_text(key.run)], |row| row.get(0))
            .map_err(|_| VerificationError::Unavailable)?;
        value.as_deref().map(StopBy::parse).transpose()
    }
    fn finish(&self, key: RecordingKey, result: Value) -> Result<(), VerificationError> {
        let state = if result["status"] == "uncertain" { "uncertain" } else { "complete" };
        let text = serde_json::to_string(&result).map_err(|_| VerificationError::Invalid)?;
        if text.len() > MAX_STORED_RESULT { return Err(VerificationError::Invalid); }
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        if connection.execute("UPDATE recording_jobs SET state=?3,result=?2,updated=unixepoch()
            WHERE call=?1 AND run=?4 AND state IN ('recording','stop_requested')",
            rusqlite::params![key.call.hex(), text, state, crate::execution_jobs::handle_text(key.run)])
            .map_err(|_| VerificationError::Unavailable)? != 1 {
            return Err(VerificationError::Invalid);
        }
        Ok(())
    }
    fn admin_read(&self, call: HelperCallId) -> Result<RecordingRow, VerificationError> {
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        Self::row(&connection, call, None)
    }
}

#[cfg(test)]
#[path = "recording_jobs_tests.rs"]
mod tests;
