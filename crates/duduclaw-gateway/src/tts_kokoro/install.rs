//! Verified, resumable installation and dashboard progress.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicBool};
use serde_json::{Value, json};
use duduclaw_onnx::install::{fetch_verified, verify_path, FileState, InstallLock};
use super::manifest;

#[derive(Default)]
struct Job {
    running: bool,
    progress: Option<Value>,
    error: Option<String>,
}
fn job() -> &'static Mutex<Job> {
    static JOB: OnceLock<Mutex<Job>> = OnceLock::new();
    JOB.get_or_init(Default::default)
}
fn lock() -> std::sync::MutexGuard<'static, Job> {
    job().lock().unwrap_or_else(|p| p.into_inner())
}
pub fn model_dir(home: &Path) -> PathBuf { home.join("models/kokoro-zh") }
fn ort_root(home: &Path) -> PathBuf { home.join("lib/onnxruntime") }
fn missing_files(home: &Path) -> Vec<&'static str> {
    let dir = model_dir(home);
    manifest::ASSETS.iter().filter(|a| {
        std::fs::symlink_metadata(dir.join(a.path))
            .map(|m| !m.is_file() || m.len() != a.size).unwrap_or(true)
    }).map(|a| a.path).collect()
}
fn ort_available(home: &Path) -> bool {
    let Some(path) = duduclaw_onnx::manifest::ort_lib_path(&ort_root(home)) else { return false; };
    let Some((size, _)) = duduclaw_onnx::manifest::ort_lib_pin(duduclaw_onnx::manifest::current_target()) else { return false; };
    std::fs::symlink_metadata(path).map(|m| m.is_file() && m.len() == size).unwrap_or(false)
}
pub fn is_installed(home: &Path) -> bool {
    missing_files(home).is_empty() && ort_available(home)
}
pub fn status(home: &Path) -> Value {
    let missing = missing_files(home);
    let ort_ok = ort_available(home);
    let installed = missing.is_empty() && ort_ok;
    let g = lock();
    let state = if g.running { "downloading" } else if g.error.is_some() { "error" }
        else if installed { "ready" } else { "absent" };
    json!({"installed": installed, "size_bytes": manifest::total_bytes(),
        "missing_files": missing, "ort_ok": ort_ok, "state": state,
        "progress": g.progress, "error": g.error})
}
pub fn install_start(home: &Path) -> Result<Value, String> {
    if duduclaw_onnx::manifest::ort_archive_for_current_platform().is_none() {
        return Err("這個平台沒有可用的 Kokoro ONNX Runtime".into());
    }
    let mut g = lock();
    if g.running { return Ok(json!({"started": false})); }
    // Always verify on a user-requested install, even when sizes match.
    g.running = true;
    g.error = None;
    g.progress = Some(json!({"file": "starting", "done_bytes": 0, "total_bytes": manifest::total_bytes()}));
    let home = home.to_path_buf();
    tokio::spawn(async move {
        let outcome = install(&home).await;
        let mut g = lock();
        g.running = false;
        g.error = outcome.err();
    });
    Ok(json!({"started": true}))
}
async fn install(home: &Path) -> Result<(), String> {
    let dir = model_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _lock = InstallLock::acquire(&dir).map_err(|e| e.to_string())?;
    let client = duduclaw_onnx::install::http_client("duduclaw").map_err(|e| e.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let total = manifest::total_bytes();
    let mut completed = 0;
    for a in manifest::ASSETS {
        let dest = dir.join(a.path);
        std::fs::create_dir_all(dest.parent().ok_or("模型目錄無效")?).map_err(|e| e.to_string())?;
        let check_path = dest.clone();
        let verified = tokio::task::spawn_blocking(move || verify_path(&check_path, a.path, a.size, a.sha256))
            .await.map_err(|e| format!("Kokoro 驗證工作失敗：{e}"))?;
        if verified != FileState::Verified {
            fetch_verified(&client, a.url, &dest, a.size, a.sha256, &cancel, &|bytes| {
                lock().progress = Some(json!({"file": a.path, "done_bytes": completed + bytes, "total_bytes": total}));
            }).await.map_err(|e| e.to_string())?;
        }
        completed += a.size;
        lock().progress = Some(json!({"file": a.path, "done_bytes": completed, "total_bytes": total}));
    }
    let archive = duduclaw_onnx::manifest::ort_archive_for_current_platform().ok_or("平台不支援 ONNX Runtime")?;
    duduclaw_onnx::install::install_ort(&client, &ort_root(home), archive, &cancel, &|bytes| {
        lock().progress = Some(json!({"file": "ONNX Runtime", "done_bytes": completed + bytes, "total_bytes": total}));
    }).await.map_err(|e| e.to_string())?;
    Ok(())
}
