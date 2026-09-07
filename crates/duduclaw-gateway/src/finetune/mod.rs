//! Fine-tuning and post-training — the `finetune.*` RPC family (WP-E of
//! `docs/todo/TODO-ai-runtimes-2026-09.md`, decision 4C).
//!
//! ## The one product claim this module is allowed to make
//!
//! **Curate here, train elsewhere, deploy here.**
//!
//! The appliance target hardware (Intel N305, AMD 8845HS — integrated
//! graphics only) cannot train. Every 2026 post-training toolchain we
//! surveyed (LLaMA-Factory, Unsloth, Axolotl, h2o-llmstudio) assumes CUDA or
//! ROCm; llama.cpp's own finetune path is unmaintained and unstable. So this
//! module never runs a training step locally and the dashboard copy says so
//! in as many words. What it does:
//!
//! 1. **Curate** — [`dataset`] turns the conversations, task results and
//!    approval decisions this box already stores into SFT JSONL (ShareGPT or
//!    Alpaca) and DPO preference pairs.
//! 2. **Train elsewhere** — [`jobs`] ships that dataset to a GPU the user
//!    supplies: their own host over SSH (LLaMA-Factory CLI), a cloud
//!    fine-tuning API, or nowhere at all (`dry_run`, which validates the
//!    config and writes the plan).
//! 3. **Deploy here** — [`import`] copies the resulting GGUF/LoRA into
//!    `<DUDUCLAW_HOME>/models`, the same directory `local_models.rs` scans
//!    and `duduclaw_inference::InferenceConfig::models_dir` points at, so the
//!    artifact shows up in the local-models list with no extra plumbing.
//!
//! ## Privacy gate
//!
//! Steps 1 → 2 move curated *customer* data off this machine. Both
//! `finetune.datasets.export` and `finetune.jobs.create` against a remote
//! backend refuse unless the caller passes
//! `acknowledged_data_leaves_device: true`; the refusal is a structured
//! error ([`FinetuneError::NotAcknowledged`], code
//! `data_leaves_device_not_acknowledged`) the dashboard renders as a consent
//! dialog rather than a red toast. The gate fails closed: an absent field is
//! a refusal, never a default-yes.
//!
//! ## Honesty rules for backends
//!
//! A backend reports only what it actually observed. There is no synthetic
//! percentage anywhere in this module: an SSH job's state comes from the
//! remote PID and the tail of the real training log, a cloud job's state
//! comes from the provider's own status field, and a `dry_run` job never
//! leaves the `planned` state. A backend whose wire format we could not
//! verify against live documentation is compiled out by default and returns
//! [`FinetuneError::UnverifiedBackend`] when asked for.

pub mod dataset;
pub mod import;
pub mod jobs;

use std::path::{Path, PathBuf};

/// Every failure this module can produce, each with a stable machine code.
///
/// The codes are part of the RPC contract — the dashboard branches on them
/// (notably `data_leaves_device_not_acknowledged`, which opens the consent
/// dialog instead of showing an error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinetuneError {
    /// A data-leaves-device operation was attempted without the explicit
    /// acknowledgement. `what` names the operation for the dialog copy.
    NotAcknowledged { what: String },
    /// Malformed or missing parameters.
    BadRequest(String),
    /// No such dataset / job / file.
    NotFound(String),
    /// Filesystem or SQLite failure.
    Io(String),
    /// The backend ran but failed (SSH exit code, HTTP error, …).
    Backend(String),
    /// The backend's wire format is not verified against live provider
    /// documentation, so it is compiled out by default. Fails closed.
    UnverifiedBackend(String),
}

impl FinetuneError {
    /// The stable code the dashboard branches on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAcknowledged { .. } => "data_leaves_device_not_acknowledged",
            Self::BadRequest(_) => "bad_request",
            Self::NotFound(_) => "not_found",
            Self::Io(_) => "io_error",
            Self::Backend(_) => "backend_error",
            Self::UnverifiedBackend(_) => "unverified_backend",
        }
    }

    /// zh-TW operator-facing message. Never carries a credential: the SSH
    /// backend redacts key paths out of transcripts before they reach here.
    pub fn message(&self) -> String {
        match self {
            Self::NotAcknowledged { what } => format!(
                "「{what}」會把整理好的資料送出這台機器。請先確認你了解資料將離開本機，再繼續。"
            ),
            Self::BadRequest(m) => m.clone(),
            Self::NotFound(m) => format!("找不到：{m}"),
            Self::Io(m) => format!("讀寫失敗：{m}"),
            Self::Backend(m) => format!("訓練後端回報失敗：{m}"),
            Self::UnverifiedBackend(m) => format!(
                "這個訓練後端的介面尚未經過實地查證，預設關閉：{m}"
            ),
        }
    }
}

impl std::fmt::Display for FinetuneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for FinetuneError {}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, FinetuneError>;

/// `<DUDUCLAW_HOME>/finetune` — everything this module writes lives under it.
pub fn finetune_root(home: &Path) -> PathBuf {
    home.join("finetune")
}

/// `<DUDUCLAW_HOME>/finetune/datasets`.
pub fn datasets_root(home: &Path) -> PathBuf {
    finetune_root(home).join("datasets")
}

/// `<DUDUCLAW_HOME>/finetune/datasets/<id>` — id must already be validated.
pub fn dataset_dir(home: &Path, id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    Ok(datasets_root(home).join(id))
}

/// `<DUDUCLAW_HOME>/finetune/jobs`.
pub fn jobs_root(home: &Path) -> PathBuf {
    finetune_root(home).join("jobs")
}

/// `<DUDUCLAW_HOME>/finetune/jobs/<id>`.
pub fn job_dir(home: &Path, id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    Ok(jobs_root(home).join(id))
}

/// `<DUDUCLAW_HOME>/models` — the SAME directory `local_models.rs` scans and
/// `duduclaw_inference::config::InferenceConfig::models_dir` defaults to
/// (`~/.duduclaw/models`; the appliance overrides it to
/// `/data/duduclaw/models`, which is that install's `DUDUCLAW_HOME/models`).
/// Imported artifacts land here so they appear in the local-models list
/// without a second registry.
pub fn models_dir(home: &Path) -> PathBuf {
    home.join("models")
}

/// Dataset and job ids: lowercase-safe slug, no traversal, no shell
/// metacharacters (job ids end up inside a remote shell command, so this is
/// a security boundary, not just tidiness).
pub fn validate_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        && !id.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(FinetuneError::BadRequest(format!(
            "id 只能用英數字、`-`、`_`，最長 64 字：{id}"
        )))
    }
}

/// Generate a fresh id from a timestamp plus a short random suffix.
pub fn new_id(prefix: &str) -> String {
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let rand = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{ts}-{}", &rand[..6])
}

/// Does this backend move data off the machine? Drives the privacy gate on
/// `finetune.jobs.create`.
///
/// Unknown backend names are treated as remote — fail closed, so a future
/// backend added without touching this function still gets the consent gate.
pub fn backend_is_remote(backend: &str) -> bool {
    !matches!(backend, "dry_run")
}

/// The privacy gate. `acknowledged` comes straight from the RPC's
/// `acknowledged_data_leaves_device` field — absent deserialises to `false`,
/// which is a refusal.
pub fn require_data_leaves_device_ack(acknowledged: bool, what: &str) -> Result<()> {
    if acknowledged {
        Ok(())
    } else {
        Err(FinetuneError::NotAcknowledged { what: what.to_string() })
    }
}

/// `std::fs` error → [`FinetuneError::Io`], with the path for context.
pub(crate) fn io_err(path: &Path, e: impl std::fmt::Display) -> FinetuneError {
    FinetuneError::Io(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_id_rejects_traversal_and_shell_metacharacters() {
        assert!(validate_id("ds-20260905-abc123").is_ok());
        assert!(validate_id("Data_Set_1").is_ok());
        for bad in [
            "",
            "..",
            "../etc",
            "a/b",
            "a b",
            "a;rm -rf /",
            "a$(id)",
            "a`id`",
            "-rf",
            "a'b",
        ] {
            assert!(validate_id(bad).is_err(), "should reject {bad:?}");
        }
        assert!(validate_id(&"x".repeat(65)).is_err());
    }

    #[test]
    fn privacy_gate_fails_closed() {
        // Absent / false acknowledgement refuses, and carries the stable code
        // the dashboard branches on to open the consent dialog.
        let err = require_data_leaves_device_ack(false, "匯出資料集").unwrap_err();
        assert_eq!(err.code(), "data_leaves_device_not_acknowledged");
        assert!(err.message().contains("匯出資料集"));
        assert!(require_data_leaves_device_ack(true, "匯出資料集").is_ok());
    }

    #[test]
    fn only_dry_run_is_local() {
        assert!(!backend_is_remote("dry_run"));
        assert!(backend_is_remote("remote_gpu_ssh"));
        assert!(backend_is_remote("together"));
        // Unknown names fail closed — a backend added later still gets gated.
        assert!(backend_is_remote("some_future_cloud"));
    }

    #[test]
    fn paths_hang_off_home_and_share_models_dir_with_local_models() {
        let home = Path::new("/data/duduclaw");
        assert_eq!(finetune_root(home), Path::new("/data/duduclaw/finetune"));
        assert_eq!(
            dataset_dir(home, "ds1").unwrap(),
            Path::new("/data/duduclaw/finetune/datasets/ds1")
        );
        assert_eq!(job_dir(home, "j1").unwrap(), Path::new("/data/duduclaw/finetune/jobs/j1"));
        // Must match `local_models::installed`'s `home_dir.join("models")`.
        assert_eq!(models_dir(home), Path::new("/data/duduclaw/models"));
        assert!(dataset_dir(home, "../escape").is_err());
    }

    #[test]
    fn new_id_is_a_valid_id() {
        let id = new_id("ds");
        assert!(validate_id(&id).is_ok(), "{id}");
        assert!(id.starts_with("ds-"));
    }
}
