//! Host-only delivery handles bound to a durable pre-delivery checkpoint.
//! Public transports apply owner/manager ACLs before requesting this handle.
use std::path::{Path, PathBuf};
use super::{maintenance, store::DiscoveryStore, workspace};

#[derive(Debug, Clone)]
pub struct VerifiedArtifact {
    pub run_id: String,
    pub cell_id: String,
    pub root: PathBuf,
    pub expected_sha256: String,
}
impl VerifiedArtifact {
    pub fn verify(&self) -> Result<(), String> {
        if workspace::canonical_real_directory(&self.root).map_err(|error| error.to_string())? != self.root {
            return Err("artifact path changed".into());
        }
        let actual = workspace::directory_sha256(&self.root).map_err(|error| error.to_string())?;
        if actual != self.expected_sha256 { return Err("verified artifact changed".into()); }
        Ok(())
    }
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub fn load_verified_artifact(home: &Path, run_id: &str) -> Result<Option<VerifiedArtifact>, String> {
    if !safe_id(run_id) { return Err("invalid artifact run ID".into()); }
    maintenance::check_clean(home)?;
    let home = workspace::canonical_real_directory(home).map_err(|error| error.to_string())?;
    let store = DiscoveryStore::open(&home).map_err(|error| error.to_string())?;
    let Some(run) = store.load_run(run_id).map_err(|error| error.to_string())? else { return Ok(None); };
    if !matches!(run.status.as_str(), "complete" | "degraded" | "budget_exhausted" | "rate_limited") {
        return Ok(None);
    }
    let Some(cell_id) = run.best_cell_id else { return Ok(None); };
    if !safe_id(&cell_id) { return Err("corrupt artifact cell ID".into()); }
    let expected_sha256 = store.load_artifact_hash(run_id, &cell_id)
        .map_err(|error| error.to_string())?.ok_or("missing durable checkpoint digest")?;
    let root = home.join("discovery/artifacts").join(run_id).join(&cell_id).join("ws");
    let artifact = VerifiedArtifact { run_id: run_id.into(), cell_id, root, expected_sha256 };
    artifact.verify()?;
    Ok(Some(artifact))
}
