//! H12d / SFR host producers for protected `skill_from_recording`.
//!
//! Ownership and completion come from HX6's recording job store
//! (`recording_jobs::SqliteRecordingJobs`), installed at gateway boot as
//! H12d's `tool_policy::RecordingJobs` seam ([`StoredRecordings`],
//! [`install_recording_source`]). The bytes come from root's work broker over
//! the employee's own `employee-work` root (`ResourceRoots::read_recording_artifact`,
//! layout `duduclaw_isolation::recording_layout`), each one only as a
//! `recording_jobs::VerifiedArtifact`. Verified means "the helper published
//! exactly these bytes", not "safe": the recorded page chose them. So the host
//! never parses them: [`summarize_recording`] takes a [`VerifiedRecording`]
//! (the type is the tripwire) and hands the framed artifacts to E's
//! `SummarizeRecording` / `RecordingArtifacts`. [`publish_owned`] writes the
//! distilled, E-scanned skill only into the caller's own `agents/<label>/SKILLS`.
use std::{path::Path, sync::Arc};
use duduclaw_isolation::{recording_layout, worker::{ParseError, RecordingSummary}};
use duduclaw_mcp_boundary::{RequestContext, VerificationError,
    recording_jobs::{self, ArtifactError, RecordingJobs as _, RecordingState, VerifiedArtifact},
    resources::{FileRefusal, ResourceRoots}, tool_policy::{AuthorizedToolCall, RecordingJobs, RecordingManifest}};

/// Values the recording collector adds to the helper's result (`ended_by`).
const ENDED_BY: [&str; 5] = ["stop", "revoked", "admin", "run_end", "deadline"];
/// Largest skill this path publishes (`write_host_file`'s cap).
pub const MAX_SKILL_BYTES: usize = 64 * 1024;

/// SFR: H12d's seam over HX6's recording store. A row is found only for the
/// record's own employee, principal (incl. generation) and binding
/// (`RecordingJobs::read`), must be the exact `(run, call)` named by `id`,
/// finished (`complete`), and carry the helper's strict `recorded` result
/// (re-decoded here); the two manifest readers (H12d's `from_result`, HX6's
/// `artifacts_from_result`) must agree.
pub struct StoredRecordings { store: Arc<dyn recording_jobs::RecordingJobs> }
impl StoredRecordings {
    pub fn new(store: Arc<dyn recording_jobs::RecordingJobs>) -> Self { Self { store } }
}
impl RecordingJobs for StoredRecordings {
    fn recorded(&self, context: &RequestContext, id: &str) -> Result<RecordingManifest, VerificationError> {
        let call = recording_jobs::parse_id(id)?;
        // Owner read: another employee's (or another person's) row is not found.
        let row = self.store.read(context, call)?;
        if row.key.call != call || row.key.id() != id || row.employee != context.record().employee {
            return Err(VerificationError::BindingMismatch);
        }
        if row.state != RecordingState::Complete { return Err(VerificationError::Unavailable); }
        let mut result = row.result.ok_or(VerificationError::Invalid)?;
        let fields = result.as_object_mut().ok_or(VerificationError::Invalid)?;
        let ended_by = fields.remove("ended_by").ok_or(VerificationError::Invalid)?;
        if !ended_by.as_str().is_some_and(|by| ENDED_BY.contains(&by)) { return Err(VerificationError::Invalid); }
        // What remains must be exactly the helper's decoded result.
        let bytes = serde_json::to_vec(&result).map_err(|_| VerificationError::Invalid)?;
        duduclaw_execution::recording_helper::decode_result(&bytes).map_err(|_| VerificationError::Invalid)?;
        let manifest = RecordingManifest::from_result(id, &result)?;
        if recording_jobs::artifacts_from_result(&result).map_err(|_| VerificationError::Invalid)? != manifest.digests() {
            return Err(VerificationError::Invalid);
        }
        Ok(manifest)
    }
}

/// Trusted boot (`isolation_mcp::boot::assemble`): make HX6's recording store
/// the source of `skill_from_recording`. One-shot (a second install refuses).
pub fn install_recording_source(store: Arc<dyn recording_jobs::RecordingJobs>) -> Result<(), VerificationError> {
    duduclaw_mcp_boundary::tool_policy::install_recording_jobs(Arc::new(StoredRecordings::new(store)))
}

/// The three verified artifacts of one recording, in layout order. Only
/// [`VerifiedRecording::new`] builds one, and only from `VerifiedArtifact`s
/// (whose only constructor is `recording_jobs::verify_artifact`). No `Clone`.
#[must_use = "the verified artifacts are only reachable through this value"]
pub struct VerifiedRecording { id: String, artifacts: [VerifiedArtifact; 3] }
impl VerifiedRecording {
    pub fn new(id: &str, artifacts: [VerifiedArtifact; 3]) -> Result<Self, ArtifactError> {
        if !recording_layout::valid_id(id)
            || artifacts.iter().zip(recording_layout::ARTIFACTS).any(|(artifact, name)| artifact.name() != name) {
            return Err(ArtifactError::Unknown);
        }
        Ok(Self { id: id.to_owned(), artifacts })
    }
    pub fn id(&self) -> &str { &self.id }
}
impl std::fmt::Debug for VerifiedRecording {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("VerifiedRecording").field("id", &self.id).field("artifacts", &self.artifacts).finish()
    }
}

/// The finished recording of this call, every artifact read through root's
/// broker and verified against the job store's digests. Refuses (fail
/// closed) without an installed job store, for a job that is not this
/// employee's finished recording, and for any artifact that is not the
/// recorded one (`FileRefusal::Artifact`).
pub fn recording_artifacts(call: &AuthorizedToolCall, roots: &ResourceRoots)
    -> Result<(RecordingManifest, VerifiedRecording), FileRefusal> {
    if call.name() != "skill_from_recording" { return Err(VerificationError::BindingMismatch.into()); }
    let jobs = duduclaw_mcp_boundary::tool_policy::recording_jobs().ok_or(VerificationError::Unavailable)?;
    let id = call.arguments().get("id").and_then(serde_json::Value::as_str).ok_or(VerificationError::Invalid)?;
    call.context().verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
    let manifest = jobs.recorded(call.context(), id)?;
    if manifest.id != id { return Err(VerificationError::BindingMismatch.into()); }
    let [trace, actions, network] = recording_layout::ARTIFACTS;
    let artifacts = [roots.read_recording_artifact(call, &manifest, trace)?,
        roots.read_recording_artifact(call, &manifest, actions)?,
        roots.read_recording_artifact(call, &manifest, network)?];
    let recording = VerifiedRecording::new(id, artifacts).map_err(FileRefusal::Artifact)?;
    Ok((manifest, recording))
}

/// The distill step's input is a [`VerifiedRecording`]: the artifacts are
/// framed (never parsed) here and parsed only by E — the trace archive with
/// entry/size/ratio caps and no traversal or symlink entries. Blocking.
pub fn summarize_recording(recording: VerifiedRecording, slug: &str) -> Result<RecordingSummary, ParseError> {
    let [trace, actions, network] = &recording.artifacts;
    let bundle = recording_layout::bundle([trace.bytes(), actions.bytes(), network.bytes()])
        .ok_or(ParseError::LimitExceeded)?;
    duduclaw_parse_worker::adapters::summarize_recording_artifacts(&bundle, slug)
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.len() <= 64
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Publish the distilled skill into the caller's own SKILLS only (H12a/H12c
/// rules): exact protected arguments, the capability-bound label (checked by
/// the CLI against the current isolated call), E's `ScanSkill` on these exact
/// bytes right before the write (High/Critical refuse), live authority, and
/// a no-clobber `write_host_file` into `agents/<label>/SKILLS/<slug>.md`
/// under the call's captured authority. Never the global bank, never another
/// employee, never an overwrite. Returns E's risk verdict of the published bytes.
pub async fn publish_owned(home: &Path, call: &AuthorizedToolCall, agent: &str, slug: &str, markdown: &str)
    -> Result<String, String> {
    subject(call, agent, slug, markdown)?;
    let bytes = markdown.as_bytes().to_vec();
    let (passed, risk, _) = tokio::task::spawn_blocking(move || duduclaw_parse_worker::adapters::scan_skill_bytes(&bytes)).await
        .map_err(|_| "隔離安全掃描失敗，技能未發布。".to_string())?
        .map_err(|_| "隔離安全掃描失敗，技能未發布。".to_string())?;
    crate::skill_lifecycle::hub_install_owned::scan_admits(passed, &risk)
        .map_err(|_| format!("蒸餾出的技能未通過隔離安全掃描（風險 {risk}），已擋下不發布。"))?;
    write_owned(home, call, agent, slug, markdown).await?;
    Ok(risk)
}

/// The publication subject: this tool's exact protected call, a valid own
/// label, a slug (never a path) and bounded content.
fn subject(call: &AuthorizedToolCall, agent: &str, slug: &str, markdown: &str) -> Result<(), String> {
    let refused = || "無法確認這次技能發布的授權，已拒絕發布。".to_string();
    if call.name() != "skill_from_recording" || !duduclaw_core::is_valid_agent_id(agent)
        || duduclaw_core::is_reserved_agent_id(agent) {
        return Err(refused());
    }
    duduclaw_mcp_boundary::tool_policy::check_own_skill_job_arguments(call.name(), call.arguments()).map_err(|_| refused())?;
    if !valid_slug(slug) { return Err("無效的技能名稱（僅限 a-z、0-9、-、_）。".into()); }
    if markdown.is_empty() || markdown.len() > MAX_SKILL_BYTES { return Err("蒸餾出的技能超過發布上限。".into()); }
    Ok(())
}

/// The write half of [`publish_owned`], after E admitted these exact bytes.
async fn write_owned(home: &Path, call: &AuthorizedToolCall, agent: &str, slug: &str, markdown: &str)
    -> Result<(), String> {
    subject(call, agent, slug, markdown)?;
    let live = |context: &RequestContext| duduclaw_mcp_boundary::authority::now_seconds()
        .and_then(|now| context.verify_live(now)).map_err(|_| "這次執行的授權已失效，技能未發布。".to_string());
    live(call.context())?;
    let authority = duduclaw_mcp_boundary::io_authority::scope(call.context().clone(),
        async { duduclaw_mcp_boundary::io_authority::capture() }).await;
    let (home, agent, file, context, bytes) = (home.to_path_buf(), agent.to_owned(), format!("{slug}.md"),
        call.context().clone(), markdown.as_bytes().to_vec());
    tokio::task::spawn_blocking(move || {
        live(&context)?;
        authority.run(|| crate::isolation_worker_plans::write_host_file(&home, &["agents", agent.as_str(), "SKILLS"],
            &file, &bytes)).map_err(|_| "技能發布尚未確認；既有技能不會覆寫。".to_string())
    }).await.map_err(|_| "技能發布工作失敗。".to_string())?
}

#[cfg(all(test, target_os = "linux"))]
#[path = "isolation_skill_jobs_tests.rs"]
mod tests;
