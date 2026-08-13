//! Local-model marketplace backend — the `localmodels.*` RPC family.
//!
//! Thin gateway front over `duduclaw_inference::model_registry::market`
//! (intent + hardware-fit + MoE-aware HF sweep) and `::downloader`
//! (resumable, shard-aware GGUF fetch). Owns the in-process install-job
//! registry so the dashboard can poll progress and cancel.
//!
//! Design doc: commercial/docs/DESIGN-local-model-marketplace-2026-08-13.md.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

use duduclaw_inference::model_registry::downloader;
use duduclaw_inference::model_registry::market::{self, Intent};
use duduclaw_inference::types::HardwareInfo;

/// One install job's dashboard-visible snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct InstallJob {
    pub id: u64,
    pub repo: String,
    pub filename: String,
    /// queued | downloading | completed | failed | cancelled
    pub state: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
}

struct JobSlot {
    job: InstallJob,
    handle: Option<tokio::task::JoinHandle<()>>,
}

type Registry = Arc<Mutex<HashMap<u64, JobSlot>>>;

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(Default::default)
}

fn next_job_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Detected hardware, cached for the process lifetime (detection shells out
/// to system tools; the numbers don't move enough mid-session to matter).
pub async fn hardware() -> HardwareInfo {
    static HW: tokio::sync::OnceCell<HardwareInfo> = tokio::sync::OnceCell::const_new();
    HW.get_or_init(|| async { duduclaw_inference::hardware::detect_hardware().await })
        .await
        .clone()
}

/// `localmodels.search` — one intent sweep fitted to this machine.
pub async fn search(intent_raw: &str, home_dir: &Path) -> Result<serde_json::Value, String> {
    let intent = Intent::from_str_loose(intent_raw)
        .ok_or_else(|| "intent 需為 chat | code | long_context | chinese".to_string())?;
    let hw = hardware().await;
    let models = market::market_search(intent, &hw, home_dir).await;
    Ok(serde_json::json!({
        "models": models,
        "hardware": hardware_summary(&hw),
    }))
}

/// `localmodels.quants` — full quant listing for one repo.
pub async fn quants(repo: &str, home_dir: &Path) -> Result<serde_json::Value, String> {
    let hw = hardware().await;
    let model = market::market_quants(repo, &hw, home_dir)
        .await
        .ok_or_else(|| format!("查無此 repo 或無法連線 Hugging Face：{repo}"))?;
    Ok(serde_json::json!({ "model": model }))
}

/// Dashboard hardware banner payload.
pub fn hardware_summary(hw: &HardwareInfo) -> serde_json::Value {
    serde_json::json!({
        "gpu_name": hw.gpu_name,
        "gpu_type": format!("{:?}", hw.gpu_type),
        "vram_available_mb": hw.vram_available_mb,
        "ram_total_mb": hw.ram_total_mb,
        "ram_available_mb": hw.ram_available_mb,
    })
}

/// `localmodels.installed` — the models directory scan (files are the
/// registry; install = drop a .gguf here, exactly like `models.list`).
pub async fn installed(home_dir: &Path) -> serde_json::Value {
    let models_dir = home_dir.join("models");
    let mut files: Vec<serde_json::Value> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&models_dir).await {
        while let Ok(Some(e)) = entries.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".gguf") {
                continue;
            }
            let size = e.metadata().await.map(|m| m.len()).unwrap_or(0);
            files.push(serde_json::json!({ "filename": name, "size_bytes": size }));
        }
    }
    files.sort_by(|a, b| a["filename"].as_str().cmp(&b["filename"].as_str()));
    serde_json::json!({ "models": files })
}

/// `localmodels.install` — start a background download of `filename` (or the
/// whole shard group) from `repo` into `<home>/models/`. Returns the job id
/// immediately; progress via `localmodels.install_status`.
pub async fn install(
    repo: &str,
    filename: &str,
    shards: Vec<String>,
    total_bytes: u64,
    home_dir: &Path,
) -> Result<u64, String> {
    if repo.split('/').count() != 2
        || !repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err(format!("repo id 不合法：{repo}"));
    }
    // One live job per (repo, filename) — double-clicks must not race two
    // downloads onto the same .part file.
    {
        let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        if reg.values().any(|s| {
            s.job.repo == repo
                && s.job.filename == filename
                && matches!(s.job.state.as_str(), "queued" | "downloading")
        }) {
            return Err("這個模型已經在下載中".to_string());
        }
    }

    let id = next_job_id();
    let job = InstallJob {
        id,
        repo: repo.to_string(),
        filename: filename.to_string(),
        state: "downloading".to_string(),
        downloaded_bytes: 0,
        total_bytes,
        error: None,
        dest: None,
    };

    let dest_dir: PathBuf = home_dir.join("models");
    let repo_owned = repo.to_string();
    let file_owned = filename.to_string();
    let job_id = id;
    let handle = tokio::spawn(async move {
        let update = |f: &dyn Fn(&mut InstallJob)| {
            let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
            if let Some(slot) = reg.get_mut(&job_id) {
                f(&mut slot.job);
            }
        };
        let progress: downloader::ProgressCallback = Box::new(move |p| {
            let mut reg = registry().lock().unwrap_or_else(|p2| p2.into_inner());
            if let Some(slot) = reg.get_mut(&job_id) {
                slot.job.downloaded_bytes = p.downloaded_bytes;
                if p.total_bytes > 0 {
                    slot.job.total_bytes = p.total_bytes;
                }
            }
        });

        let result = if shards.len() > 1 {
            // Shard group: (url, mirror, filename) triples over the resolve CDN.
            let triples: Vec<(String, String, String)> = shards
                .iter()
                .filter_map(|s| {
                    let base = s.rsplit('/').next()?.to_string();
                    Some((
                        format!("https://huggingface.co/{repo_owned}/resolve/main/{s}"),
                        String::new(),
                        base,
                    ))
                })
                .collect();
            downloader::download_model_shards(&triples, &dest_dir, Some(progress)).await
        } else {
            let url = format!("https://huggingface.co/{repo_owned}/resolve/main/{file_owned}");
            let base = file_owned.rsplit('/').next().unwrap_or(&file_owned).to_string();
            downloader::download_model(&url, "", &dest_dir, &base, Some(progress)).await
        };

        match result {
            Ok(path) => update(&|j: &mut InstallJob| {
                j.state = "completed".to_string();
                j.dest = Some(path.display().to_string());
                if j.total_bytes > 0 {
                    j.downloaded_bytes = j.total_bytes;
                }
            }),
            Err(e) => update(&|j: &mut InstallJob| {
                // A cancel aborts the task before this point in the normal
                // case; an error after cancel keeps the cancelled state.
                if j.state != "cancelled" {
                    j.state = "failed".to_string();
                    j.error = Some(e.to_string());
                }
            }),
        }
    });

    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(id, JobSlot { job, handle: Some(handle) });
    Ok(id)
}

/// `localmodels.install_status` — all recent jobs (newest first).
pub fn install_status() -> serde_json::Value {
    let reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let mut jobs: Vec<InstallJob> = reg.values().map(|s| s.job.clone()).collect();
    jobs.sort_by(|a, b| b.id.cmp(&a.id));
    serde_json::json!({ "jobs": jobs })
}

/// `localmodels.cancel` — abort a running job. The partial `.part` file is
/// left in place so a re-install resumes from where it stopped.
pub fn cancel(job_id: u64) -> Result<(), String> {
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let slot = reg.get_mut(&job_id).ok_or_else(|| "查無此下載任務".to_string())?;
    if let Some(handle) = slot.handle.take() {
        handle.abort();
    }
    if matches!(slot.job.state.as_str(), "queued" | "downloading") {
        slot.job.state = "cancelled".to_string();
    }
    Ok(())
}

/// `localmodels.remove` — delete one installed .gguf (basename only, no
/// traversal; the file IS the registry so deletion is the whole operation).
pub async fn remove(filename: &str, home_dir: &Path) -> Result<(), String> {
    let safe = !filename.is_empty()
        && !filename.contains('/')
        && !filename.contains('\\')
        && !filename.contains("..")
        && filename.ends_with(".gguf");
    if !safe {
        return Err(format!("檔名不合法：{filename}"));
    }
    let path = home_dir.join("models").join(filename);
    tokio::fs::remove_file(&path).await.map_err(|e| format!("刪除失敗：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn installed_lists_only_gguf_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("b-model.gguf"), b"x").unwrap();
        std::fs::write(models.join("a-model.gguf"), b"xy").unwrap();
        std::fs::write(models.join("notes.txt"), b"n").unwrap();
        let out = installed(dir.path()).await;
        let list = out["models"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["filename"], "a-model.gguf");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remove_rejects_traversal_and_non_gguf() {
        let dir = tempfile::tempdir().unwrap();
        assert!(remove("../evil.gguf", dir.path()).await.is_err());
        assert!(remove("notes.txt", dir.path()).await.is_err());
        assert!(remove("", dir.path()).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn install_rejects_bad_repo_and_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        assert!(install("no-slash", "f.gguf", vec![], 0, dir.path()).await.is_err());
        // A queued job blocks a duplicate for the same (repo, file). The
        // spawned download will fail fast (fake repo) — that's fine, the
        // dedup check happens against the registered state first.
        let id = install("org/fake-repo-x", "f.gguf", vec![], 10, dir.path()).await.unwrap();
        let dup = install("org/fake-repo-x", "f.gguf", vec![], 10, dir.path()).await;
        assert!(dup.is_err());
        let _ = cancel(id);
        let status = install_status();
        assert!(status["jobs"].as_array().unwrap().iter().any(|j| j["id"] == id));
    }

    #[test]
    fn cancel_unknown_job_errors() {
        assert!(cancel(999_999).is_err());
    }
}
