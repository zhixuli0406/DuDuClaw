//! H12b/H12c/H12d: own-skill job tools in protected mode.
//!
//! `skill_hub_install`, `skill_synthesis_run` and `skill_from_recording` act
//! only on the record's own employee: the host fetches/calls a provider/reads
//! brokered artifacts, E parses the untrusted bytes, and the host publishes
//! only into the employee's own `agents/<label>/SKILLS`. No workspace input
//! authorizes a skill (H12a decision), except the recording artifacts, which
//! live in the employee's own brokered `employee-work` root. Identity is the
//! verified record (EmployeeScope + its own `Employee` grant); the agent label
//! is bound by the host, never an argument.
use std::sync::{Arc, RwLock};
use duduclaw_isolation::{bounded::Text, capability::ResourceGrant, recording_layout};
use serde_json::Value;
use crate::{RequestContext, VerificationError};
use super::{AuthorizedToolCall, ResourceResolver, ToolPolicy, own_employee, public_origins_grant, validate_resource};

pub const OWN_SKILL_JOB_TOOLS: &[&str] = &["skill_hub_install", "skill_synthesis_run", "skill_from_recording"];

/// Most lookback days a protected synthesis run may scan (handler cap).
pub const SYNTHESIS_MAX_LOOKBACK_DAYS: u64 = 30;
/// Longest skill-name override accepted for `skill_from_recording`.
pub const RECORDING_NAME_MAX_BYTES: usize = 200;

fn slug(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

/// The exact protected argument schema of [`OWN_SKILL_JOB_TOOLS`]. Nothing
/// selects another employee, scope, owner, path, URL or execution identity:
/// - `skill_hub_install`: `{hub, skill_name}` (slugs). `scope`/`owner` refuse:
///   the install target is always the caller's own SKILLS.
/// - `skill_synthesis_run`: `{agent_id?, dry_run?: bool, lookback_days?: 1..=30}`;
///   `agent_id` is only the legacy self-reference (the handler requires it to
///   equal the bound employee).
/// - `skill_from_recording`: `{id: 32 lowercase hex, name?}`; `name` is a
///   bounded display override (slugified by the handler), never a path.
pub fn check_own_skill_job_arguments(name: &str, arguments: &Value) -> Result<(), VerificationError> {
    let object = arguments.as_object().ok_or(VerificationError::Invalid)?;
    let allowed: &[&str] = match name {
        "skill_hub_install" => &["hub", "skill_name"],
        "skill_synthesis_run" => &["agent_id", "dry_run", "lookback_days"],
        "skill_from_recording" => &["id", "name"],
        _ => return Err(VerificationError::Invalid),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) { return Err(VerificationError::Invalid); }
    let valid = match name {
        "skill_hub_install" => object.get("hub").is_some_and(slug) && object.get("skill_name").is_some_and(slug),
        "skill_synthesis_run" => object.get("agent_id").is_none_or(slug)
            && object.get("dry_run").is_none_or(Value::is_boolean)
            && object.get("lookback_days").is_none_or(|days| days.as_u64()
                .is_some_and(|days| (1..=SYNTHESIS_MAX_LOOKBACK_DAYS).contains(&days))),
        _ => object.get("id").and_then(Value::as_str).is_some_and(recording_layout::valid_id)
            && object.get("name").is_none_or(|value| value.as_str().is_some_and(|s|
                s.len() <= RECORDING_NAME_MAX_BYTES && !s.chars().any(char::is_control))),
    };
    if valid { Ok(()) } else { Err(VerificationError::Invalid) }
}

/// Record-side selection, shared by discovery (`eligible`) and calls:
/// - all three: the employee's own `Employee` grant under its own EmployeeScope;
/// - `skill_hub_install`: plus the record's signed `PublicOrigins` grant (the
///   admin-enabled protected public web, D-H2); the hub URL is host-selected
///   and re-checked against it before the fetch;
/// - `skill_from_recording`: plus the plain brokered `Workspace{employee-work}`
///   grant, and only while a recording job source is installed (HX6).
pub fn own_skill_job_selector(context: &RequestContext, name: &str) -> Result<Vec<ResourceGrant>, VerificationError> {
    if !OWN_SKILL_JOB_TOOLS.contains(&name) { return Err(VerificationError::Invalid); }
    let record = context.record();
    let own = own_employee(context);
    if !record.employee_scope() || !record.io_resources().any(|grant| grant == &own) {
        return Err(VerificationError::Invalid);
    }
    let mut selection = vec![own];
    match name {
        "skill_hub_install" => selection.push(public_origins_grant(context).ok_or(VerificationError::Invalid)?.clone()),
        "skill_from_recording" => {
            // Fail closed without the recording job store (installed at gateway
            // boot): no artifact manifest could bind the recording to this employee.
            if recording_jobs().is_none() { return Err(VerificationError::Unavailable); }
            let workspace = ResourceGrant::Workspace { handle: Text::new(crate::resources::EMPLOYEE_WORK_ROOT.to_owned())
                .map_err(|_| VerificationError::Invalid)? };
            if record.resources.as_slice().iter().filter(|grant| *grant == &workspace).count() != 1 {
                return Err(VerificationError::Invalid);
            }
            selection.push(workspace);
        }
        _ => {}
    }
    Ok(selection)
}

/// Call path for [`OWN_SKILL_JOB_TOOLS`] (arguments already checked, the tool
/// already eligible). The resolver vouches for live identity (and lists the
/// record's workspace for the recording); it may neither add nor substitute a
/// grant. `PublicOrigins` comes only from the signed record, never the resolver.
pub(super) fn authorize(context: &RequestContext, policy: &'static ToolPolicy, arguments: &Value,
    resolver: &dyn ResourceResolver) -> Result<AuthorizedToolCall, VerificationError> {
    let selection = own_skill_job_selector(context, policy.name)?;
    let expected: Vec<_> = selection.iter().filter(|grant| !matches!(grant, ResourceGrant::PublicOrigins { .. })).collect();
    let resolved = resolver.resolve(context, policy, arguments)?;
    if resolved.len() != expected.len() || !expected.iter().all(|grant| resolved.contains(grant)) {
        return Err(VerificationError::Invalid);
    }
    for grant in &selection {
        crate::resources::require_resource(context, grant)?;
        validate_resource(grant)?;
    }
    Ok(AuthorizedToolCall { context: context.clone(), policy, arguments: arguments.clone(), resources: selection })
}

/// One artifact of a finished protected recording, as the helper reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingArtifact {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: [u8; 32],
}

/// The job store's evidence that recording `id` finished (`recorded`) for the
/// calling employee/principal/binding, with the helper's artifact manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingManifest {
    pub id: String,
    pub artifacts: Vec<RecordingArtifact>,
}
impl RecordingManifest {
    /// Parse the helper result stored with a completed job (the shape of
    /// `duduclaw_execution::recording_helper::decode_result`): status
    /// `recorded`, exactly [`recording_layout::ARTIFACTS`] in order, each
    /// 1..=16 MiB with a lowercase hex SHA-256. Anything else refuses.
    pub fn from_result(id: &str, result: &Value) -> Result<Self, VerificationError> {
        if !recording_layout::valid_id(id) || result["id"].as_str() != Some(id) || result["status"] != "recorded" {
            return Err(VerificationError::Invalid);
        }
        let entries = result["artifacts"].as_array().ok_or(VerificationError::Invalid)?;
        if entries.len() != recording_layout::ARTIFACTS.len() { return Err(VerificationError::Invalid); }
        let mut artifacts = Vec::new();
        for (entry, name) in entries.iter().zip(recording_layout::ARTIFACTS) {
            let bytes = entry["bytes"].as_u64().filter(|n| (1..=recording_layout::MAX_ARTIFACT_BYTES).contains(n))
                .ok_or(VerificationError::Invalid)?;
            let digest = entry["sha256"].as_str().filter(|s| s.len() == 64 && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).ok_or(VerificationError::Invalid)?;
            if entry["name"].as_str() != Some(name) || entry.as_object().is_none_or(|fields| fields.len() != 3) {
                return Err(VerificationError::Invalid);
            }
            let mut sha256 = [0u8; 32];
            for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
                let digit = |b: u8| if b.is_ascii_digit() { b - b'0' } else { b - b'a' + 10 };
                sha256[index] = digit(pair[0]) * 16 + digit(pair[1]);
            }
            artifacts.push(RecordingArtifact { name, bytes, sha256 });
        }
        Ok(Self { id: id.to_owned(), artifacts })
    }

    pub fn artifact(&self, name: &str) -> Option<&RecordingArtifact> {
        self.artifacts.iter().find(|artifact| artifact.name == name)
    }

    /// SFR: the same digests in HX6's form, the input of
    /// `recording_jobs::verify_artifact` (the only way to a `VerifiedArtifact`).
    pub fn digests(&self) -> Vec<crate::recording_jobs::ArtifactDigest> {
        self.artifacts.iter().map(|artifact| crate::recording_jobs::ArtifactDigest { name: artifact.name,
            bytes: artifact.bytes, sha256: artifact.sha256.iter().map(|b| format!("{b:02x}")).collect() }).collect()
    }
}

/// HX6 seam: the recording job store. `recorded` returns the manifest only for
/// a finished recording owned by the record's employee, principal and binding
/// (the job handle is a selector, never bearer authority); anything else is a
/// refusal. Without one `skill_from_recording` is not eligible. SFR: gateway
/// boot installs `isolation_skill_jobs::StoredRecordings` over HX6's
/// `SqliteRecordingJobs` (`install_recording_source`).
pub trait RecordingJobs: Send + Sync {
    fn recorded(&self, context: &RequestContext, id: &str) -> Result<RecordingManifest, VerificationError>;
}

static RECORDING_JOBS: RwLock<Option<Arc<dyn RecordingJobs>>> = RwLock::new(None);

/// Trusted boot installs the job store once (HX6). A second install refuses.
pub fn install_recording_jobs(source: Arc<dyn RecordingJobs>) -> Result<(), VerificationError> {
    let mut slot = RECORDING_JOBS.write().map_err(|_| VerificationError::Unavailable)?;
    if slot.is_some() { return Err(VerificationError::Invalid); }
    *slot = Some(source);
    Ok(())
}

/// The installed recording job store, if any (a poisoned lock reads as none).
pub fn recording_jobs() -> Option<Arc<dyn RecordingJobs>> {
    test_recording_jobs().or_else(|| RECORDING_JOBS.read().ok().and_then(|slot| slot.clone()))
}

#[cfg(any(test, feature = "test-support"))]
fn test_recording_jobs() -> Option<Arc<dyn RecordingJobs>> { TEST_RECORDING_JOBS.with(|slot| slot.borrow().clone()) }
#[cfg(not(any(test, feature = "test-support")))]
fn test_recording_jobs() -> Option<Arc<dyn RecordingJobs>> { None }

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_RECORDING_JOBS: std::cell::RefCell<Option<Arc<dyn RecordingJobs>>> = const { std::cell::RefCell::new(None) };
}

/// Test-only: a per-thread recording job store, restored when dropped. Never
/// affects other threads (or the production slot).
#[cfg(any(test, feature = "test-support"))]
pub struct TestRecordingJobs(Option<Arc<dyn RecordingJobs>>);
#[cfg(any(test, feature = "test-support"))]
impl TestRecordingJobs {
    pub fn install(source: Option<Arc<dyn RecordingJobs>>) -> Self {
        Self(TEST_RECORDING_JOBS.with(|slot| slot.replace(source)))
    }
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for TestRecordingJobs {
    fn drop(&mut self) {
        let previous = self.0.take();
        TEST_RECORDING_JOBS.with(|slot| { slot.replace(previous); });
    }
}

#[cfg(test)]
#[path = "tool_policy_skill_jobs_tests.rs"]
mod tests;
