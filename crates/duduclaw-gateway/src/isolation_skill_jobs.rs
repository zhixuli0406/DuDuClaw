//! H12d host producers for protected `skill_from_recording`.
//!
//! Ownership and completion come from the recording job store (HX6 seam,
//! `tool_policy::RecordingJobs`), the bytes from root's work broker over the
//! employee's own `employee-work` root (`ResourceRoots::read_recording_artifact`,
//! layout `duduclaw_isolation::recording_layout`). The host never parses the
//! recording: the artifact text goes to E's `SummarizeRecording` unchanged.
use std::sync::Arc;
use duduclaw_mcp_boundary::{RequestContext, VerificationError, execution_jobs::{parse_handle, ExecutionJobs},
    resources::{FileRefusal, ResourceRoots}, tool_policy::{AuthorizedToolCall, RecordingJobs, RecordingManifest}};
use serde_json::json;

/// The tool whose finished jobs are recordings.
const RECORDING_TOOL: &str = "browser_record_start";
/// The protected helper records no HAR (its `network.json` holds only response
/// origins/status codes and is not given to the summarizer). A fixed empty
/// HAR keeps E's `{har, actions, events}` input shape without host parsing.
const EMPTY_HAR: &str = r#"{"log":{"entries":[]}}"#;
/// E's input cap is 64 MiB and JSON escaping can expand text sixfold.
const MAX_ACTIONS_TEXT_BYTES: usize = (64 * 1024 * 1024 - 1024) / 6;

/// Adapter from the gateway execution job store to the recording seam. HX6
/// decides whether this is the production store (and installs it with
/// `tool_policy::install_recording_jobs`); it is not installed today.
/// Assumption to confirm in HX6: a finished recording job's `result` is the
/// helper's `recording_helper::decode_result` value.
pub struct ExecutionJobRecordings { jobs: Arc<dyn ExecutionJobs> }
impl ExecutionJobRecordings {
    pub fn new(jobs: Arc<dyn ExecutionJobs>) -> Self { Self { jobs } }
}
impl RecordingJobs for ExecutionJobRecordings {
    fn recorded(&self, context: &RequestContext, id: &str) -> Result<RecordingManifest, VerificationError> {
        // `read` is keyed by employee, principal, binding and tool: another
        // employee's (or another person's) job is simply not found.
        let job = self.jobs.read(context, RECORDING_TOOL, parse_handle(id)?)?;
        if job["status"] != "complete" { return Err(VerificationError::Unavailable); }
        let result = &job["result"];
        let bytes = serde_json::to_vec(result).map_err(|_| VerificationError::Invalid)?;
        duduclaw_execution::recording_helper::decode_result(&bytes).map_err(|_| VerificationError::Invalid)?;
        RecordingManifest::from_result(id, result)
    }
}

/// The finished recording of this call and E's summarizer input for it.
/// Refuses (fail closed) without an installed job store, for a job that is
/// not this employee's finished recording, or for artifacts that do not match
/// the job's manifest.
pub fn recording_summary_input(call: &AuthorizedToolCall, roots: &ResourceRoots)
    -> Result<(RecordingManifest, Vec<u8>), FileRefusal> {
    if call.name() != "skill_from_recording" { return Err(VerificationError::BindingMismatch.into()); }
    let jobs = duduclaw_mcp_boundary::tool_policy::recording_jobs().ok_or(VerificationError::Unavailable)?;
    let id = call.arguments().get("id").and_then(serde_json::Value::as_str).ok_or(VerificationError::Invalid)?;
    call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
    let manifest = jobs.recorded(call.context(), id)?;
    if manifest.id != id { return Err(VerificationError::BindingMismatch.into()); }
    let actions = roots.read_recording_artifact(call, &manifest, "actions.json")?;
    if actions.len() > MAX_ACTIONS_TEXT_BYTES { return Err(VerificationError::Invalid.into()); }
    let actions = String::from_utf8(actions).map_err(|_| VerificationError::Invalid)?;
    let bytes = serde_json::to_vec(&json!({"har": EMPTY_HAR, "actions": actions, "events": ""}))
        .map_err(|_| VerificationError::Invalid)?;
    Ok((manifest, bytes))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "isolation_skill_jobs_tests.rs"]
mod tests;
