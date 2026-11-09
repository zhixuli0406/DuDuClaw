//! Protected discovery delivery is host record control, not an execution plan.
//! It consumes the checkpoint's durable host manifest and opens exact files
//! through host-owned directory descriptors. It never scans an output tree.
use super::*;
use super::super::super::artifact_manifest::ArtifactManifest;

pub(super) async fn deliver(home: &Path, caller: &TrustedCaller, run: &str,
    selected: Option<&str>) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    { let _ = (home, caller, run, selected); Err("isolated artifact delivery requires Linux".into()) }
    #[cfg(target_os = "linux")]
    {
        let tasks = TaskStore::open(home)?;
        let task = authorized_task(home, &tasks, caller, run).await?;
        let (cell, manifest) = checkpoint(home, &task, run)?;
        let expected = manifest.digest();
        let input_home = home.to_owned();
        let input_run = run.to_owned();
        let input_cell = cell.clone();
        let selected = selected.map(str::to_owned);
        let result = tokio::task::spawn_blocking(move || read_checkpoint(
            &input_home, &input_run, &input_cell, &manifest, selected.as_deref()))
            .await.map_err(|_| "artifact read unavailable")??;
        // Recheck the current target/creator ACL and durable run/checkpoint
        // binding after file I/O. A yielded request cannot retain old access.
        let current = authorized_task(home, &tasks, caller, run).await?;
        let (current_cell, current_manifest) = checkpoint(home, &current, run)?;
        if current.id != task.id || current_cell != cell || current_manifest.digest() != expected {
            return Err("artifact checkpoint binding changed".into());
        }
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
fn checkpoint(home: &Path, task: &TaskRow, run_id: &str) -> Result<(String, ArtifactManifest), String> {
    super::super::super::maintenance::check_clean(home)?;
    let store = DiscoveryStore::open(home).map_err(|_| "discovery unavailable")?;
    let run = store.load_run(run_id).map_err(|_| "discovery unavailable")?.ok_or("discovery not found")?;
    validate_binding(task, &run)?;
    if !matches!(run.status.as_str(), "complete" | "degraded" | "budget_exhausted" | "rate_limited") {
        return Err("verified artifact is not available".into());
    }
    let cell = run.best_cell_id.ok_or("verified artifact is not available")?;
    valid_id(&cell)?;
    let manifest = store.load_artifact_manifest(run_id, &cell).map_err(|_| "artifact manifest unavailable")?
        .ok_or("this checkpoint has no published artifact manifest")?;
    Ok((cell, manifest))
}

#[cfg(target_os = "linux")]
fn read_checkpoint(home: &Path, run: &str, cell: &str, manifest: &ArtifactManifest,
    selected: Option<&str>) -> Result<Value, String> {
    use super::super::super::artifact_manifest::{AuthorizedArtifact, MAX_DOWNLOAD};
    let artifact = AuthorizedArtifact::open(home, run, cell)?;
    if let Some(selected) = selected {
        if selected.len() != 69 || !selected.strip_prefix("file-").is_some_and(|digest|
            digest.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))) {
            return Err("invalid artifact file identity".into());
        }
        let (path, expected) = manifest.files().find(|(path, _)| file_id(Path::new(path)) == selected)
            .ok_or("artifact file not found")?;
        let file = artifact.open_file(path)?;
        if file.metadata().map_err(|_| "artifact file unavailable")?.len() > MAX_DOWNLOAD {
            return Err("artifact exceeds 2 MiB protected download limit".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_DOWNLOAD + 1).read_to_end(&mut bytes).map_err(|_| "artifact read failed")?;
        if bytes.len() as u64 > MAX_DOWNLOAD || format!("{:x}", Sha256::digest(&bytes)) != expected {
            return Err("verified artifact changed".into());
        }
        Ok(json!({"run_id":run, "cell_id":cell, "file_id":selected,
            "name":safe_name(Path::new(path))?, "size_bytes":bytes.len(),
            "verified":true, "content_base64":STANDARD.encode(bytes)}))
    } else {
        let mut files = Vec::new();
        for (path, expected) in manifest.files() {
            let file = artifact.open_file(path)?;
            files.push(json!({"file_id":file_id(Path::new(path)), "name":safe_name(Path::new(path))?,
                "size_bytes":file.metadata().map_err(|_| "artifact file unavailable")?.len(), "sha256":expected}));
        }
        // The manifest is bound to the completed checkpoint. Listing only stats
        // files; only a selected download claims current content verification.
        Ok(json!({"run_id":run, "cell_id":cell, "verified":false, "manifest_verified":true,
            "content_verification":"on_download", "files":files}))
    }
}
