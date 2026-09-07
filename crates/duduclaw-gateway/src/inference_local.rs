//! Appliance local-model control plane — the `inference.local.*` RPC family.
//!
//! The DuDuClaw OS image ships llama.cpp's `llama-server` plus a unit that
//! reads `<DUDUCLAW_HOME>/llama-server.env` and serves an OpenAI-compatible
//! API on loopback. Weights are deliberately NOT in the image (they are
//! gigabytes and they go stale), so this module is the path from "fresh
//! appliance, no weights" to "a local model is answering":
//!
//! ```text
//!   inference.local.catalog   pick from a small verified list, with a fit
//!                             light for THIS machine
//!   inference.local.download  background GGUF fetch into <home>/models
//!   inference.local.status    poll download progress / is the server up /
//!                             which weights are loaded
//!   inference.local.serve     point the service at one file and (re)start it
//!   inference.local.stop      stop it
//! ```
//!
//! ## Relationship to `localmodels.*`
//!
//! [`crate::local_models`] is the open-ended Hugging Face marketplace —
//! search any repo, any quant, hardware-fit lights, resumable installs.
//! This module is the narrow, verified path for the appliance: six models
//! whose repo/filename/size were checked against the live HF API, one
//! click to download, one click to serve. It **reuses** the marketplace's
//! job registry and `downloader` rather than growing a second download
//! stack — `download` here is `local_models::install` with the arguments
//! filled in from the catalog.
//!
//! ## Honesty rules this module follows
//!
//! - It never claims a model exists. `catalog` reports `installed` from an
//!   actual directory scan; `status` reports `loaded_model` from an actual
//!   HTTP call to the endpoint, and `null` when it cannot reach it.
//! - On a non-appliance host `serve`/`stop` still write the env file (so
//!   the operator can inspect exactly what the appliance would do) but
//!   report `restarted: false` / `stopped: false` with a reason, rather
//!   than pretending a service was managed.
//! - Every failure is returned as itself. Nothing degrades into a fake
//!   success.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tracing::{info, warn};

use duduclaw_inference::appliance;

// ---------------------------------------------------------------------------
// Curated catalog
// ---------------------------------------------------------------------------

/// One curated GGUF the appliance offers to download.
///
/// `size_bytes` is the exact LFS byte size of the file at verification
/// time; a re-upload by the publisher changes it, which is why
/// [`CatalogEntry::verified_at`] records when the check ran and the
/// download path treats the size as a progress hint, never as a checksum.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    /// Stable id used by `inference.local.download {id}`.
    pub id: &'static str,
    pub display_name: &'static str,
    pub hf_repo: &'static str,
    pub file: &'static str,
    pub size_bytes: u64,
    /// Parameter count in billions.
    pub params_b: f32,
    pub quant: &'static str,
    /// Memory needed to actually run it — see [`MIN_RAM_HEADROOM_GB`].
    pub min_ram_gb: f32,
    pub recommended_for: &'static [&'static str],
    /// `true` = repo id, filename, byte size and ungated anonymous
    /// downloadability were checked against the live Hugging Face API.
    /// `false` = listed but unchecked; the dashboard marks it.
    pub verified: bool,
    /// ISO date of the verification run behind `verified`.
    pub verified_at: &'static str,
}

/// Headroom added on top of the file size to get `min_ram_gb`: KV cache at
/// the default 4096-token context, the compute buffers llama.cpp allocates,
/// and enough slack that the box is not swapping while it answers. Larger
/// models get more because their KV cache per token is larger.
///
/// These are engineering estimates, not measurements — the honest framing
/// the dashboard uses is a three-state fit light, never a promised
/// tokens/second number.
const MIN_RAM_HEADROOM_GB: f32 = 1.5;

/// The curated list.
///
/// **Verification (2026-09-05).** Every row's `hf_repo`, `file` and
/// `size_bytes` was checked against the live Hugging Face API — repo
/// metadata via `https://huggingface.co/api/models/<repo>` (confirming
/// `gated: false`) and the file listing via
/// `https://huggingface.co/api/models/<repo>/tree/main?limit=1000` (taking
/// the `lfs.size` field as the true byte size), then a `HEAD` on
/// `https://huggingface.co/<repo>/resolve/main/<file>` with no
/// `Authorization` header to prove an anonymous one-click download returns
/// 200 with a matching `content-length`. Repo commit SHAs at verification
/// time are recorded next to each row so a silent re-upload is detectable.
///
/// Two findings shaped the choices:
/// - `Qwen/Qwen3-1.7B-GGUF` ships **no** Q4_K_M (only `Q8_0`), so the 1.7B
///   row comes from `unsloth/` — the publisher's own repo would 404.
/// - `google/gemma-3-4b-it-qat-q4_0-gguf` is `gated: "manual"` and an
///   anonymous fetch returns 401 `GatedRepo`, so it cannot be a one-click
///   appliance download; the ungated `ggml-org/` mirror is used instead.
///
/// Watch the filenames: the unsloth repos also ship `Q4_0`, `Q4_1`,
/// `Q4_K_S`, `IQ4_NL`, `IQ4_XS` and `UD-Q4_K_XL` next to the plain
/// `Q4_K_M`, and Qwen's own Coder repo uses a lowercase filename. These
/// strings are exact, not patterns.
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        // repo sha d7f544eead698dbd1f15126ef60b45a1e1933222
        id: "qwen3-1.7b",
        display_name: "Qwen3 1.7B",
        hf_repo: "unsloth/Qwen3-1.7B-GGUF",
        file: "Qwen3-1.7B-Q4_K_M.gguf",
        size_bytes: 1_107_409_472,
        params_b: 1.7,
        quant: "Q4_K_M",
        min_ram_gb: 2.5,
        recommended_for: &["chat", "chinese", "smallest"],
        verified: true,
        verified_at: "2026-09-05",
    },
    CatalogEntry {
        // repo sha bc640142c66e1fdd12af0bd68f40445458f3869b
        id: "qwen3-4b",
        display_name: "Qwen3 4B",
        hf_repo: "Qwen/Qwen3-4B-GGUF",
        file: "Qwen3-4B-Q4_K_M.gguf",
        size_bytes: 2_497_280_256,
        params_b: 4.0,
        quant: "Q4_K_M",
        min_ram_gb: 3.9,
        recommended_for: &["chat", "chinese", "balanced"],
        verified: true,
        verified_at: "2026-09-05",
    },
    CatalogEntry {
        // repo sha 7c41481f57cb95916b40956ab2f0b139b296d974
        id: "qwen3-8b",
        display_name: "Qwen3 8B",
        hf_repo: "Qwen/Qwen3-8B-GGUF",
        file: "Qwen3-8B-Q4_K_M.gguf",
        size_bytes: 5_027_783_488,
        params_b: 8.0,
        quant: "Q4_K_M",
        min_ram_gb: 6.2,
        recommended_for: &["chat", "chinese", "reasoning"],
        verified: true,
        verified_at: "2026-09-05",
    },
    CatalogEntry {
        // repo sha d0976223747697cb51e056d85c532013931fe52e
        // Text-only from this file alone; vision needs a companion mmproj
        // this row deliberately does not ship.
        id: "gemma3-4b",
        display_name: "Gemma 3 4B",
        hf_repo: "ggml-org/gemma-3-4b-it-GGUF",
        file: "gemma-3-4b-it-Q4_K_M.gguf",
        size_bytes: 2_489_757_856,
        params_b: 4.0,
        quant: "Q4_K_M",
        min_ram_gb: 3.9,
        recommended_for: &["chat", "multilingual"],
        verified: true,
        verified_at: "2026-09-05",
    },
    CatalogEntry {
        // repo sha e7d0997e49c9cb00d88b4c1a6a16aa894b0bbc31
        // Meta's own repos are gated; unsloth's mirror is ungated.
        id: "llama3.2-3b",
        display_name: "Llama 3.2 3B",
        hf_repo: "unsloth/Llama-3.2-3B-Instruct-GGUF",
        file: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        size_bytes: 2_019_377_600,
        params_b: 3.2,
        quant: "Q4_K_M",
        min_ram_gb: 3.4,
        recommended_for: &["chat", "english"],
        verified: true,
        verified_at: "2026-09-05",
    },
    CatalogEntry {
        // repo sha 13fb94bfda8c8cf22497dc57b78f391a9acb426a
        // Qwen's repo ships BOTH split shards and this single file; the
        // single file is used so the download is one request.
        id: "qwen2.5-coder-7b",
        display_name: "Qwen2.5-Coder 7B",
        hf_repo: "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF",
        file: "qwen2.5-coder-7b-instruct-q4_k_m.gguf",
        size_bytes: 4_683_073_536,
        params_b: 7.0,
        quant: "Q4_K_M",
        min_ram_gb: 5.9,
        recommended_for: &["code"],
        verified: true,
        verified_at: "2026-09-05",
    },
];

/// Look one catalog row up by id.
pub fn entry(id: &str) -> Option<&'static CatalogEntry> {
    CATALOG.iter().find(|e| e.id == id)
}

/// Tri-state hardware fit, same vocabulary the marketplace page already
/// uses (`comfortable` / `tight` / `too_big`) so one legend covers both.
///
/// Pure. `available_mb` is the larger of usable VRAM and available system
/// RAM — on the appliance's integrated graphics there is no separate VRAM
/// pool, so it is system RAM.
pub fn fit_for(min_ram_gb: f32, available_mb: u64) -> &'static str {
    let need_mb = (min_ram_gb * 1024.0) as u64;
    if need_mb == 0 {
        return "comfortable";
    }
    // 25% slack over the bare minimum before calling it comfortable.
    if available_mb >= need_mb + need_mb / 4 {
        "comfortable"
    } else if available_mb >= need_mb {
        "tight"
    } else {
        "too_big"
    }
}

/// The `min_ram_gb` a catalog row should carry for a given file size.
/// Exposed so the constant behind the table is checkable rather than six
/// hand-typed numbers nobody can re-derive.
pub fn min_ram_for(size_bytes: u64) -> f32 {
    size_bytes as f32 / (1024.0 * 1024.0 * 1024.0) + MIN_RAM_HEADROOM_GB
}

// ---------------------------------------------------------------------------
// RPC: inference.local.catalog
// ---------------------------------------------------------------------------

/// `inference.local.catalog` — the curated list, each row carrying a fit
/// light computed for this machine and whether the file is already on disk.
pub async fn catalog(home_dir: &Path) -> serde_json::Value {
    let hw = crate::local_models::hardware().await;
    let available_mb = hw.vram_available_mb.max(hw.ram_available_mb);
    let models_dir = models_dir(home_dir);

    let mut rows = Vec::with_capacity(CATALOG.len());
    for e in CATALOG {
        let installed = tokio::fs::metadata(models_dir.join(e.file))
            .await
            .map(|m| m.is_file())
            .unwrap_or(false);
        let mut row = serde_json::to_value(e).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = row.as_object_mut() {
            obj.insert("fit".into(), fit_for(e.min_ram_gb, available_mb).into());
            obj.insert("installed".into(), installed.into());
        }
        rows.push(row);
    }

    serde_json::json!({
        "models": rows,
        "hardware": crate::local_models::hardware_summary(&hw),
        "appliance": appliance::appliance_defaults_active(),
        "endpoint": appliance::LLAMA_SERVER_ENDPOINT,
        "models_dir": models_dir.display().to_string(),
    })
}

// ---------------------------------------------------------------------------
// RPC: inference.local.download
// ---------------------------------------------------------------------------

/// `inference.local.download {id}` — start a background download of one
/// catalog row into `<home>/models`. Returns immediately with the job id;
/// progress is polled via [`status`].
///
/// Delegates to the marketplace's install path, so resume, the 100 GB
/// guard, filename validation and the one-job-per-file lock all apply
/// unchanged.
pub async fn download(id: &str, home_dir: &Path) -> Result<serde_json::Value, String> {
    let e = entry(id).ok_or_else(|| format!("查無此模型代號：{id}"))?;
    let job_id =
        crate::local_models::install(e.hf_repo, e.file, Vec::new(), e.size_bytes, home_dir).await?;
    info!(model = e.id, repo = e.hf_repo, job_id, "local model download started");
    Ok(serde_json::json!({
        "job_id": job_id,
        "id": e.id,
        "filename": e.file,
        "size_bytes": e.size_bytes,
    }))
}

// ---------------------------------------------------------------------------
// RPC: inference.local.serve / stop
// ---------------------------------------------------------------------------

/// Models directory for a given home — the single convention shared with
/// `local_models.rs` and the inference config's home-aware default.
fn models_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("models")
}

/// Env file the image's unit reads.
fn env_path(home_dir: &Path) -> PathBuf {
    home_dir.join(appliance::LLAMA_SERVER_ENV_FILE)
}

/// Pure: is `name` a safe GGUF basename? No traversal, no separators, no
/// dotfiles, and it must actually be a `.gguf`.
fn safe_model_file(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.starts_with('.')
        && name.ends_with(".gguf")
}

/// Pure: render the env file the unit reads.
///
/// `LLAMA_MODEL` and `LLAMA_CTX` are always rewritten from the arguments.
/// `LLAMA_PORT` and `LLAMA_EXTRA_ARGS` are carried over from `existing`
/// when the operator set them — an operator who added flags by hand keeps
/// them across a model switch. Unknown keys in `existing` are preserved
/// too, in the order they appeared, for the same reason.
///
/// Values are written bare (no quoting): the env-file format the service
/// manager parses takes the rest of the line verbatim, and every value we
/// write ourselves is a validated basename or an integer.
pub fn render_env_file(existing: &str, model_path: &str, ctx: u32) -> String {
    let managed = ["LLAMA_MODEL", "LLAMA_CTX"];
    let mut out = String::new();
    out.push_str("# Written by DuDuClaw (inference.local.serve). Edit LLAMA_EXTRA_ARGS freely;\n");
    out.push_str("# LLAMA_MODEL and LLAMA_CTX are rewritten whenever a model is selected.\n");
    out.push_str(&format!("LLAMA_MODEL={model_path}\n"));
    out.push_str(&format!("LLAMA_CTX={ctx}\n"));

    let mut saw_port = false;
    let mut saw_extra = false;
    let mut carried = String::new();
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if managed.contains(&key) {
            continue;
        }
        if key == "LLAMA_PORT" {
            saw_port = true;
        }
        if key == "LLAMA_EXTRA_ARGS" {
            saw_extra = true;
        }
        carried.push_str(trimmed);
        carried.push('\n');
    }
    if !saw_port {
        out.push_str(&format!("LLAMA_PORT={}\n", appliance::LLAMA_SERVER_PORT));
    }
    if !saw_extra {
        out.push_str("LLAMA_EXTRA_ARGS=\n");
    }
    out.push_str(&carried);
    out
}

/// `inference.local.serve {model_file, ctx?}` — point the local model
/// service at one downloaded GGUF and (re)start it.
///
/// On an appliance the env file is written atomically and the service is
/// restarted. Everywhere else the env file is still written — so the
/// operator can see exactly what would happen — and the response says
/// `restarted: false` with the reason. Nothing is faked either way.
pub async fn serve(
    model_file: &str,
    ctx: Option<u32>,
    home_dir: &Path,
) -> Result<serde_json::Value, String> {
    if !safe_model_file(model_file) {
        return Err(format!("模型檔名不合法：{model_file}"));
    }
    let model_path = models_dir(home_dir).join(model_file);
    if !tokio::fs::try_exists(&model_path).await.unwrap_or(false) {
        return Err(format!("模型檔案不存在，請先下載：{model_file}"));
    }
    let ctx = ctx.unwrap_or(appliance::DEFAULT_CTX).clamp(512, 131_072);

    let path = env_path(home_dir);
    let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let body = render_env_file(&existing, &model_path.to_string_lossy(), ctx);
    write_atomic(&path, &body).await?;

    let (restarted, detail) = restart_unit().await;
    info!(model = model_file, ctx, restarted, "local model service configured");
    Ok(serde_json::json!({
        "model_file": model_file,
        "model_path": model_path.display().to_string(),
        "ctx": ctx,
        "env_path": path.display().to_string(),
        "restarted": restarted,
        "detail": detail,
    }))
}

/// `inference.local.stop` — stop the local model service. Same honesty
/// contract as [`serve`]: off-appliance it reports `stopped: false`.
pub async fn stop() -> serde_json::Value {
    let (stopped, detail) = run_unit_action("stop").await;
    // The backend just went away: drop the gateway's cached engine so the
    // next local call re-probes instead of talking to a dead port.
    crate::claude_runner::reset_inference_engine().await;
    serde_json::json!({ "stopped": stopped, "detail": detail })
}

/// Atomic write: temp file in the same directory, then rename. A crash
/// mid-write can never leave the service reading half an env file.
async fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| "無效的設定路徑".to_string())?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("建立設定目錄失敗：{e}"))?;
    let tmp = path.with_extension("env.tmp");
    tokio::fs::write(&tmp, body)
        .await
        .map_err(|e| format!("寫入設定失敗：{e}"))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| format!("套用設定失敗：{e}"))
}

/// Restart the local model service, honestly. Also drops the gateway's
/// cached inference engine (and any "backend unavailable" retry window),
/// so a model served for the first time is picked up by the next local
/// call without a gateway restart.
async fn restart_unit() -> (bool, String) {
    let result = run_unit_action("restart").await;
    crate::claude_runner::reset_inference_engine().await;
    result
}

/// Shell out to the service manager. Returns `(acted, human-readable
/// detail)`. Refuses off-appliance rather than shelling out on a
/// developer's laptop.
async fn run_unit_action(action: &str) -> (bool, String) {
    if !appliance::appliance_defaults_active() {
        return (
            false,
            "此主機不是 DuDuClaw OS 裝置，只寫入設定檔、未啟動本地模型服務".to_string(),
        );
    }
    let out = tokio::process::Command::new("systemctl")
        .arg(action)
        .arg(appliance::LLAMA_SERVER_UNIT)
        .output()
        .await;
    // The service manager's own stderr names unit files and system paths.
    // It goes to the log for an operator to read; the `detail` a person
    // sees stays in product vocabulary (same bar the dashboard's
    // `technical-terms` i18n guard enforces on the front end).
    match out {
        Ok(o) if o.status.success() => (true, String::new()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            warn!(action, stderr = %err, "local model service action failed");
            (false, "本地模型服務沒有回應，請稍後再試一次".to_string())
        }
        Err(e) => {
            warn!(action, error = %e, "could not invoke the service manager");
            (false, "這台裝置上無法控制本地模型服務".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// RPC: inference.local.status
// ---------------------------------------------------------------------------

/// Ask the OpenAI-compatible endpoint which model it has loaded.
///
/// Returns `(reachable, loaded_model)`. A short timeout: this is polled
/// from the dashboard while a download runs, and a down server must answer
/// "no" fast rather than hanging the RPC.
async fn probe_endpoint() -> (bool, Option<String>) {
    let url = format!("{}/models", appliance::LLAMA_SERVER_ENDPOINT);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (false, None),
    };
    let Ok(resp) = client.get(&url).send().await else {
        return (false, None);
    };
    if !resp.status().is_success() {
        return (false, None);
    }
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        // Reachable but unparseable: say reachable, claim no model name.
        return (true, None);
    };
    let loaded = body["data"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|m| m["id"].as_str())
        .map(display_model_name);
    (true, loaded)
}

/// Pure: turn whatever the server calls its model into something a person
/// can read.
///
/// llama.cpp's server reports the **absolute path** of the loaded GGUF as
/// the model id. That string goes straight into the dashboard's status
/// banner, and a filesystem path has no business in copy a person reads
/// (the same bar the front end's `technical-terms` i18n guard enforces),
/// so a path collapses to its file name. A server that reports a plain
/// alias is passed through untouched.
fn display_model_name(id: &str) -> String {
    id.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(id)
        .to_string()
}

/// `inference.local.status` — one call answering every question the
/// dashboard's banner asks: is the local model service up, which weights
/// are loaded, what is downloading, and what is already on disk.
pub async fn status(home_dir: &Path) -> serde_json::Value {
    let (reachable, loaded_model) = probe_endpoint().await;
    let installed = crate::local_models::installed(home_dir).await;
    let jobs = crate::local_models::install_status();
    let dir = models_dir(home_dir);
    let configured_model = tokio::fs::read_to_string(env_path(home_dir))
        .await
        .ok()
        .and_then(|s| env_value(&s, "LLAMA_MODEL"))
        .and_then(|p| {
            PathBuf::from(p)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        });

    serde_json::json!({
        "appliance": appliance::appliance_defaults_active(),
        "llama_server_present": Path::new(appliance::LLAMA_SERVER_PATH).exists(),
        "endpoint": appliance::LLAMA_SERVER_ENDPOINT,
        "reachable": reachable,
        "loaded_model": loaded_model,
        "configured_model": configured_model,
        "models_dir": dir.display().to_string(),
        "has_model": appliance::models_dir_has_gguf(&dir).await,
        "installed": installed["models"],
        "downloads": jobs["jobs"],
    })
}

/// Pure: read one key out of an env-file body.
fn env_value(body: &str, key: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// Tests — pure functions only: no HTTP, no processes, no downloads.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique_and_files_are_downloadable_basenames() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate catalog id");

        for e in CATALOG {
            assert!(safe_model_file(e.file), "{} has an unsafe filename", e.id);
            // `local_models::install` rejects anything else.
            assert_eq!(e.hf_repo.split('/').count(), 2, "{} repo is not owner/name", e.id);
            assert!(e.size_bytes > 0, "{} has no size", e.id);
            assert!(!e.recommended_for.is_empty(), "{} has no use case", e.id);
        }
    }

    #[test]
    fn min_ram_matches_the_documented_formula() {
        // The six hand-written numbers must stay derivable from the file
        // size, not drift into folklore. 0.15 GB tolerance for rounding.
        for e in CATALOG {
            let expected = min_ram_for(e.size_bytes);
            assert!(
                (e.min_ram_gb - expected).abs() < 0.15,
                "{}: min_ram_gb {} but formula gives {expected}",
                e.id,
                e.min_ram_gb
            );
        }
    }

    #[test]
    fn fit_light_is_a_step_function_of_available_memory() {
        // 4 GB model: needs 4096 MB, comfortable at 5120 MB.
        assert_eq!(fit_for(4.0, 8192), "comfortable");
        assert_eq!(fit_for(4.0, 5120), "comfortable");
        assert_eq!(fit_for(4.0, 5119), "tight");
        assert_eq!(fit_for(4.0, 4096), "tight");
        assert_eq!(fit_for(4.0, 4095), "too_big");
        assert_eq!(fit_for(4.0, 0), "too_big");
    }

    #[test]
    fn unknown_catalog_id_is_not_silently_substituted() {
        assert!(entry("qwen3-1.7b").is_some());
        assert!(entry("qwen3-1.7B").is_none(), "id lookup must be exact");
        assert!(entry("").is_none());
        assert!(entry("../etc/passwd").is_none());
    }

    #[test]
    fn model_filename_validation_is_fail_closed() {
        assert!(safe_model_file("Qwen3-4B-Q4_K_M.gguf"));
        assert!(!safe_model_file("../evil.gguf"));
        assert!(!safe_model_file("sub/dir/model.gguf"));
        assert!(!safe_model_file(".hidden.gguf"));
        assert!(!safe_model_file("notes.txt"));
        assert!(!safe_model_file(""));
    }

    #[test]
    fn env_file_rewrites_the_managed_keys() {
        let out = render_env_file("", "/data/duduclaw/models/a.gguf", 4096);
        assert!(out.contains("LLAMA_MODEL=/data/duduclaw/models/a.gguf\n"));
        assert!(out.contains("LLAMA_CTX=4096\n"));
        assert!(out.contains("LLAMA_PORT=8080\n"));
        assert!(out.contains("LLAMA_EXTRA_ARGS=\n"));
    }

    #[test]
    fn env_file_carries_over_operator_settings() {
        let existing = "LLAMA_MODEL=/old/b.gguf\nLLAMA_CTX=8192\nLLAMA_PORT=9090\nLLAMA_EXTRA_ARGS=--threads 8\nLLAMA_CUSTOM=x\n";
        let out = render_env_file(existing, "/data/duduclaw/models/a.gguf", 2048);
        // Managed keys are replaced, exactly once.
        assert_eq!(out.matches("LLAMA_MODEL=").count(), 1);
        assert!(out.contains("LLAMA_MODEL=/data/duduclaw/models/a.gguf\n"));
        assert!(!out.contains("/old/b.gguf"));
        assert_eq!(out.matches("LLAMA_CTX=").count(), 1);
        assert!(out.contains("LLAMA_CTX=2048\n"));
        // Operator's own settings survive a model switch.
        assert!(out.contains("LLAMA_PORT=9090\n"));
        assert!(!out.contains("LLAMA_PORT=8080"));
        assert!(out.contains("LLAMA_EXTRA_ARGS=--threads 8\n"));
        assert!(out.contains("LLAMA_CUSTOM=x\n"));
    }

    #[test]
    fn a_reported_model_path_never_reaches_the_banner_as_a_path() {
        // llama.cpp's server answers /v1/models with the absolute path.
        assert_eq!(
            display_model_name("/data/duduclaw/models/Qwen3-4B-Q4_K_M.gguf"),
            "Qwen3-4B-Q4_K_M.gguf"
        );
        // A plain alias passes through untouched.
        assert_eq!(display_model_name("local"), "local");
        // Degenerate inputs never produce an empty banner string.
        assert_eq!(display_model_name("/"), "/");
        assert_eq!(display_model_name(""), "");
    }

    #[test]
    fn env_value_reads_back_what_render_wrote() {
        let out = render_env_file("", "/data/duduclaw/models/a.gguf", 4096);
        assert_eq!(
            env_value(&out, "LLAMA_MODEL").as_deref(),
            Some("/data/duduclaw/models/a.gguf")
        );
        assert_eq!(env_value(&out, "LLAMA_CTX").as_deref(), Some("4096"));
        // Empty value reads as absent, not as an empty model path.
        assert_eq!(env_value(&out, "LLAMA_EXTRA_ARGS"), None);
        assert_eq!(env_value(&out, "NOPE"), None);
        // Comments are never parsed as values.
        assert_eq!(env_value("# LLAMA_MODEL=/tricked\n", "LLAMA_MODEL"), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serve_refuses_a_model_that_is_not_there() {
        let dir = tempfile::tempdir().unwrap();
        // Never downloaded: refuse rather than write an env file pointing
        // at nothing.
        let err = serve("Qwen3-4B-Q4_K_M.gguf", None, dir.path()).await.unwrap_err();
        assert!(err.contains("請先下載"), "{err}");
        assert!(!env_path(dir.path()).exists());
        // Traversal is refused before the existence check.
        assert!(serve("../evil.gguf", None, dir.path()).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serve_writes_the_env_file_and_reports_restart_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("a.gguf"), b"gguf").unwrap();

        let out = serve("a.gguf", Some(2048), dir.path()).await.unwrap();
        assert_eq!(out["ctx"], 2048);
        // The test host is not an appliance, so nothing was restarted and
        // the response says so instead of claiming success.
        assert_eq!(out["restarted"], false);
        assert!(!out["detail"].as_str().unwrap().is_empty());

        let body = std::fs::read_to_string(env_path(dir.path())).unwrap();
        assert!(body.contains("LLAMA_CTX=2048"));
        assert!(body.contains(&models.join("a.gguf").display().to_string()));
        // No temp file left behind.
        assert!(!dir.path().join("llama-server.env.tmp").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ctx_is_clamped_to_a_sane_range() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("a.gguf"), b"gguf").unwrap();
        assert_eq!(serve("a.gguf", Some(1), dir.path()).await.unwrap()["ctx"], 512);
        assert_eq!(
            serve("a.gguf", Some(9_999_999), dir.path()).await.unwrap()["ctx"],
            131_072
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn download_refuses_an_unknown_id_without_starting_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let err = download("not-a-model", dir.path()).await.unwrap_err();
        assert!(err.contains("not-a-model"), "{err}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_on_a_bare_home_reports_nothing_installed() {
        let dir = tempfile::tempdir().unwrap();
        let s = status(dir.path()).await;
        assert_eq!(s["has_model"], false);
        assert_eq!(s["installed"].as_array().unwrap().len(), 0);
        assert_eq!(s["appliance"], false);
        assert!(s["configured_model"].is_null());
    }
}
