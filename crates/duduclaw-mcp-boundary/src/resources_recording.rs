//! H12d: brokered reads of a finished protected recording.
//!
//! The artifacts live in the employee's own `employee-work` root, at the one
//! layout the recording helper publishes (`recording_layout::artifact_path`).
//! They are opened only through root's work broker for the call's live run and
//! employee (never a host path), and must match the job store's manifest
//! (size and SHA-256) before any byte reaches a parser.
use std::io::Read;
use sha2::{Digest, Sha256};
use duduclaw_isolation::recording_layout;
use super::{selected_workspace_grant, verify_brokered, AuthorizedFile, CallBinding, FileRefusal, ResourceRoots,
    VerificationError, WorkFileRequest, WorkRelativePath, BROKERED_FILE_MAX_BYTES, EMPLOYEE_WORK_ROOT};
use crate::tool_policy::{AuthorizedToolCall, RecordingManifest};

impl ResourceRoots {
    /// Read `artifact` of the recording named by this `skill_from_recording`
    /// call. `manifest` must be the job store's evidence for that same id.
    /// Refuses another tool, another id, an artifact outside the manifest, a
    /// call without exactly its own brokered workspace, and any size/digest
    /// mismatch. Root's refusals keep their typed `WorkBrokerError`.
    pub fn read_recording_artifact(&self, call: &AuthorizedToolCall, manifest: &RecordingManifest, artifact: &str)
        -> Result<Vec<u8>, FileRefusal> {
        if call.name() != "skill_from_recording" { return Err(VerificationError::BindingMismatch.into()); }
        let id = call.arguments().get("id").and_then(serde_json::Value::as_str).ok_or(VerificationError::Invalid)?;
        if manifest.id != id { return Err(VerificationError::BindingMismatch.into()); }
        let expected = manifest.artifact(artifact).ok_or(VerificationError::Invalid)?;
        let relative = recording_layout::artifact_path(id, artifact).ok_or(VerificationError::Invalid)?;
        let path = WorkRelativePath::parse(&relative)?;
        // Exactly the record's own selection: own Employee + the brokered root.
        let selection = crate::tool_policy::own_skill_job_selector(call.context(), call.name())?;
        if call.resources() != selection.as_slice() { return Err(VerificationError::BindingMismatch.into()); }
        let grant = selected_workspace_grant(call, EMPLOYEE_WORK_ROOT)?.clone();
        let broker = self.brokered_root(call.context(), EMPLOYEE_WORK_ROOT)?;
        let context = call.context();
        context.verify_live(crate::authority::now_seconds()?)?;
        let record = context.record();
        let request = WorkFileRequest { run: record.run, employee: record.employee, path: &path,
            max_bytes: BROKERED_FILE_MAX_BYTES };
        let opened = broker.open_work_file(&request).map_err(FileRefusal::Broker)?;
        let file = verify_brokered(opened)?;
        // The run may have ended while root was opening the file.
        context.verify_live(crate::authority::now_seconds()?)?;
        let mut input = AuthorizedFile::from_opened(CallBinding::new(call), grant, file)?;
        if input.metadata()?.len() != expected.bytes { return Err(VerificationError::BindingMismatch.into()); }
        let mut bytes = Vec::new();
        // Every read re-checks live authority (AuthorizedFile's Read).
        (&mut input).take(expected.bytes + 1).read_to_end(&mut bytes).map_err(|_| VerificationError::Unavailable)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if bytes.len() as u64 != expected.bytes || digest != expected.sha256 {
            return Err(VerificationError::BindingMismatch.into());
        }
        Ok(bytes)
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "resources_recording_tests.rs"]
mod tests;
