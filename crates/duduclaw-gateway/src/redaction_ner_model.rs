//! `redaction.model.*` — install / inspect / remove the local NER model.
//!
//! The "AI 智慧偵測" rule set needs a ~945 MB model plus an ONNX Runtime
//! shared library that the release binary deliberately does not carry (see
//! `duduclaw-redaction::ner`). This module is the operator-facing half: a
//! single background download job with progress, and an honest status.
//!
//! Shape follows `local_models.rs` — a process-global job slot, a
//! `std::sync::Mutex` never held across an `.await`, cancel by aborting the
//! task and leaving the `.part` files for the next resume.
//!
//! Honesty rules, same as `inference_local.rs`:
//! * `installed` is answered from the disk, never from "we started a job".
//! * a failed download stays `error` with its reason until something
//!   supersedes it — it never degrades into `absent`, which would read as
//!   "you never tried".
//! * latency numbers come from the real inference telemetry or are `null`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};

use duduclaw_redaction::ner::install::{self, InstallDirs, Progress};
use duduclaw_redaction::ner::{manifest, runtime};

#[derive(Default)]
struct JobSlot {
    running: bool,
    progress: Option<Progress>,
    error: Option<String>,
    handle: Option<tokio::task::JoinHandle<()>>,
    cancel: Option<Arc<AtomicBool>>,
    /// Set when an install or a removal finished and the live redaction
    /// pipeline should be rebuilt. Consumed by [`take_pending_reload`].
    pending_reload: bool,
}

fn slot() -> &'static Mutex<JobSlot> {
    static SLOT: OnceLock<Mutex<JobSlot>> = OnceLock::new();
    SLOT.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, JobSlot> {
    slot().lock().unwrap_or_else(|p| p.into_inner())
}

fn dirs_for(home: &Path) -> InstallDirs {
    InstallDirs::under_home(home)
}

/// `redaction.model.status` — everything the dashboard card renders.
pub fn status(home: &Path) -> Value {
    let dirs = dirs_for(home);
    let problem = install::installed_problem(&dirs);
    let installed = problem.is_none();
    let platform_supported = manifest::ort_archive_for_current_platform().is_some();

    let (running, progress, error) = {
        let g = lock();
        (g.running, g.progress.clone(), g.error.clone())
    };

    // Order matters: a live job outranks everything, then a recorded failure,
    // then the disk, then whether a session is resident.
    let stats = runtime::global_stats().snapshot();
    let state = if running {
        "downloading"
    } else if error.is_some() && !installed {
        "error"
    } else if !installed {
        "absent"
    } else if stats.loaded {
        "loaded"
    } else {
        "ready"
    };

    json!({
        "installed": installed,
        "model_revision": manifest::MODEL_REVISION,
        "ort_version": manifest::ORT_VERSION,
        "size_bytes": install::total_install_bytes(),
        "state": state,
        "progress": progress.map(|p| json!({
            "file": p.file,
            "done_bytes": p.done_bytes,
            "total_bytes": p.total_bytes,
        })),
        "error": error,
        "avg_latency_ms": stats.avg_latency_ms,
        "p50_latency_ms": stats.p50_latency_ms,
        "calls": stats.calls,
        "last_used_at": stats.last_used_at,
        // Beyond the design contract, because the UI cannot render the
        // truth without them: macOS on Intel has no ONNX Runtime build at
        // this version, and "why isn't it installed" is the card's whole job.
        "platform_supported": platform_supported,
        "reason": problem,
    })
}

/// `redaction.model.install` — start the background download.
///
/// Idempotent: already installed ⇒ `{ started: false }`, already downloading
/// ⇒ `{ started: false }`. Neither is an error; both are what a double-click
/// means.
pub fn install_start(home: &Path) -> Result<Value, String> {
    let dirs = dirs_for(home);
    if manifest::ort_archive_for_current_platform().is_none() {
        return Err(format!(
            "AI 智慧偵測在這個平台（{}）沒有可用的執行環境，無法安裝",
            manifest::current_target()
        ));
    }
    if install::is_installed(&dirs) {
        return Ok(json!({ "started": false, "installed": true }));
    }

    let mut g = lock();
    if g.running {
        return Ok(json!({ "started": false, "installed": false }));
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_task = cancel.clone();
    let dirs_task = dirs.clone();
    let progress_fn: install::ProgressFn = Arc::new(|p: Progress| {
        let mut g = lock();
        g.progress = Some(p);
    });

    let handle = tokio::spawn(async move {
        let outcome = install::install(&dirs_task, progress_fn, cancel_task).await;
        let mut g = lock();
        g.running = false;
        g.handle = None;
        g.cancel = None;
        match outcome {
            Ok(()) => {
                g.error = None;
                g.pending_reload = true;
                tracing::info!("NER model installed");
            }
            Err(e) => {
                // Never logged with a path+token; the install code only ever
                // produces operator-safe text.
                tracing::warn!(error = %e, "NER model install failed");
                g.error = Some(e.to_string());
            }
        }
    });

    g.running = true;
    g.error = None;
    g.progress = Some(Progress {
        file: "starting".to_string(),
        done_bytes: 0,
        total_bytes: install::total_install_bytes(),
    });
    g.handle = Some(handle);
    g.cancel = Some(cancel);
    Ok(json!({ "started": true, "installed": false }))
}

/// `redaction.model.cancel` — stop a running download.
///
/// The partially downloaded `.part` files stay on disk so a later install
/// resumes instead of restarting a 945 MB transfer.
pub fn cancel() -> Result<Value, String> {
    let mut g = lock();
    if !g.running {
        return Ok(json!({ "cancelled": false }));
    }
    if let Some(c) = g.cancel.take() {
        c.store(true, Ordering::Relaxed);
    }
    if let Some(h) = g.handle.take() {
        h.abort();
    }
    g.running = false;
    g.error = Some("下載已取消".to_string());
    Ok(json!({ "cancelled": true }))
}

/// `redaction.model.remove` — delete the model files.
///
/// The ONNX Runtime library is left in place on purpose: it is tens of MB,
/// version-shared, and not what an operator reclaiming ~945 MB is after.
/// Refuses while a download is running, so the two never race on the same
/// files.
pub fn remove(home: &Path) -> Result<Value, String> {
    if lock().running {
        return Err("正在下載中，請先取消再移除".to_string());
    }
    let dirs = dirs_for(home);
    // Free the session first — on Windows an open mapping would block the
    // delete outright, and everywhere else it would keep 1.7 GB resident
    // against files that no longer exist. `unload_registered` acts only on an
    // engine that already exists: opening one here would register it under a
    // default `[redaction.ner]` config that the next rule compile would then
    // reuse in place of the operator's.
    runtime::unload_registered(&dirs.model_dir);
    install::remove_model(&dirs.model_dir).map_err(|e| format!("移除模型失敗：{e}"))?;
    let mut g = lock();
    g.error = None;
    g.progress = None;
    g.pending_reload = true;
    Ok(json!({ "removed": true }))
}

/// Take the "the live pipeline should be rebuilt" flag, clearing it.
///
/// Set when an install or a removal changed whether a `type = "ner"` rule can
/// compile. The rebuild itself needs `&MethodHandler`, which a detached task
/// does not have, so the flag is consumed by the next `redaction.model.*` RPC
/// — the dashboard polls status every two seconds while a download runs, so
/// the reload lands within one poll of the download finishing.
pub fn take_pending_reload() -> bool {
    let mut g = lock();
    std::mem::replace(&mut g.pending_reload, false)
}

/// Absolute model directory, for log lines and operator messages.
pub fn model_dir(home: &Path) -> PathBuf {
    dirs_for(home).model_dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Tests share the process-global slot; reset it so order cannot matter.
    fn reset() {
        let mut g = lock();
        *g = JobSlot::default();
    }

    #[test]
    fn status_on_a_clean_home_is_absent_with_a_reason() {
        reset();
        let tmp = TempDir::new().unwrap();
        let s = status(tmp.path());
        assert_eq!(s["installed"], json!(false));
        assert_eq!(s["state"], json!("absent"));
        assert!(s["reason"].is_string(), "{s}");
        assert_eq!(s["model_revision"], json!(manifest::MODEL_REVISION));
        assert_eq!(s["ort_version"], json!(manifest::ORT_VERSION));
        assert!(s["size_bytes"].as_u64().unwrap() > 900_000_000);
        assert_eq!(s["progress"], Value::Null);
        assert_eq!(s["error"], Value::Null);
    }

    #[test]
    fn status_carries_the_telemetry_contract_fields() {
        reset();
        let tmp = TempDir::new().unwrap();
        let s = status(tmp.path());
        for key in [
            "installed",
            "model_revision",
            "ort_version",
            "size_bytes",
            "state",
            "progress",
            "error",
            "avg_latency_ms",
            "p50_latency_ms",
            "calls",
            "last_used_at",
        ] {
            assert!(s.get(key).is_some(), "status is missing `{key}`: {s}");
        }
    }

    #[test]
    fn a_recorded_failure_shows_as_error_not_absent() {
        reset();
        let tmp = TempDir::new().unwrap();
        lock().error = Some("下載中斷".into());
        let s = status(tmp.path());
        assert_eq!(s["state"], json!("error"));
        assert_eq!(s["error"], json!("下載中斷"));
        reset();
    }

    #[test]
    fn a_running_job_outranks_a_stale_error() {
        reset();
        let tmp = TempDir::new().unwrap();
        {
            let mut g = lock();
            g.running = true;
            g.error = Some("舊的失敗".into());
            g.progress = Some(Progress {
                file: "onnx/model_q4.onnx_data".into(),
                done_bytes: 10,
                total_bytes: 100,
            });
        }
        let s = status(tmp.path());
        assert_eq!(s["state"], json!("downloading"));
        assert_eq!(s["progress"]["done_bytes"], json!(10));
        assert_eq!(s["progress"]["file"], json!("onnx/model_q4.onnx_data"));
        reset();
    }

    #[test]
    fn cancel_with_no_job_is_a_no_op_not_an_error() {
        reset();
        let r = cancel().unwrap();
        assert_eq!(r["cancelled"], json!(false));
    }

    #[test]
    fn cancel_marks_the_slot_and_records_why() {
        reset();
        {
            let mut g = lock();
            g.running = true;
        }
        let r = cancel().unwrap();
        assert_eq!(r["cancelled"], json!(true));
        let g = lock();
        assert!(!g.running);
        assert!(g.error.as_deref().unwrap().contains("取消"));
        drop(g);
        reset();
    }

    #[test]
    fn remove_is_refused_while_a_download_runs() {
        reset();
        let tmp = TempDir::new().unwrap();
        {
            let mut g = lock();
            g.running = true;
        }
        let err = remove(tmp.path()).unwrap_err();
        assert!(err.contains("下載中"), "{err}");
        reset();
    }

    #[test]
    fn remove_on_a_clean_home_succeeds_and_asks_for_a_reload() {
        reset();
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(model_dir(tmp.path())).unwrap();
        let r = remove(tmp.path()).unwrap();
        assert_eq!(r["removed"], json!(true));
        assert!(take_pending_reload(), "a removal must trigger a rebuild");
        assert!(!take_pending_reload(), "the flag must be consumed once");
        reset();
    }

    #[tokio::test]
    async fn install_is_idempotent_while_a_job_is_live() {
        reset();
        let tmp = TempDir::new().unwrap();
        {
            let mut g = lock();
            g.running = true;
        }
        let r = install_start(tmp.path()).unwrap();
        assert_eq!(r["started"], json!(false), "must not start a second job");
        reset();
    }

    #[test]
    fn model_dir_is_under_models_not_redaction() {
        let p = model_dir(Path::new("/h"));
        assert!(p.ends_with("models/privacy-filter"), "{p:?}");
    }
}
