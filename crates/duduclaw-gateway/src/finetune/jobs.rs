//! Training jobs — the `finetune.jobs.*` RPCs and the [`FinetuneBackend`]
//! trait behind them.
//!
//! Three backends ship:
//!
//! | id | Where training happens | Verified against |
//! |---|---|---|
//! | `dry_run` | nowhere — validates the config, writes `train.yaml` + `plan.json` | n/a (local only) |
//! | `remote_gpu_ssh` | a GPU host the user owns, over SSH, via `llamafactory-cli train <yaml>` | LLaMA-Factory CLI/YAML surface; **not** executed against a real GPU host in CI |
//! | `together` | Together AI's fine-tuning API | live OpenAPI at `docs.together.ai/reference/post-fine-tunes`, fetched 2026-09-05 |
//!
//! ## No invented progress
//!
//! Every state transition here is caused by something observed:
//!
//! - `remote_gpu_ssh` — the remote PID is alive (`running`), gone with an
//!   adapter written (`succeeded`), or gone without one (`failed`). The log
//!   the dashboard shows is `tail` of the actual `train.log`.
//! - `together` — the provider's own `status` field, mapped 1:1.
//! - `dry_run` — stays `planned` forever. It never trains anything and the
//!   UI copy says so.
//!
//! There is deliberately no percentage field. LLaMA-Factory's progress lives
//! in the log lines we forward verbatim; Together reports
//! `progress.seconds_remaining` only when `estimate_available` is true, and
//! we pass that through untouched rather than deriving a bar from it.
//!
//! ## Shell-injection boundary
//!
//! `remote_gpu_ssh` composes a `/bin/sh` script that runs on the user's own
//! host. Every value interpolated into it — host, user, workdir, python
//! path, job id, model name — goes through [`validate_shell_safe`] or
//! [`super::validate_id`] first. A value that does not match is a
//! `bad_request`, never a quoted-and-hoped-for string.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    backend_is_remote, io_err, job_dir, jobs_root, new_id, require_data_leaves_device_ack,
    validate_id, FinetuneError, Result,
};

/// Together AI's documented API root (OpenAPI `servers[0]`, verified
/// 2026-09-05).
const TOGETHER_BASE: &str = "https://api.together.ai/v1";

/// Env var holding the Together API key. Never persisted into `job.json`.
const TOGETHER_KEY_ENV: &str = "TOGETHER_API_KEY";

/// How many trailing log lines we carry back to the dashboard.
const LOG_TAIL_LINES: usize = 60;

// ─────────────────────────── config ───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainMethod {
    /// Supervised fine-tuning on `train.jsonl`.
    Sft,
    /// Direct preference optimisation on `preference.jsonl`.
    Dpo,
}

impl TrainMethod {
    pub fn from_str_loose(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sft" | "" => Ok(Self::Sft),
            "dpo" => Ok(Self::Dpo),
            other => Err(FinetuneError::BadRequest(format!(
                "method 需為 sft 或 dpo：{other}"
            ))),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sft => "sft",
            Self::Dpo => "dpo",
        }
    }
    /// LLaMA-Factory dataset name registered by `dataset::dataset_info_json`.
    fn dataset_name(self) -> &'static str {
        match self {
            Self::Sft => "duduclaw_sft",
            Self::Dpo => "duduclaw_dpo",
        }
    }
    /// The dataset file this method trains on.
    pub fn source_file(self) -> &'static str {
        match self {
            Self::Sft => "train.jsonl",
            Self::Dpo => "preference.jsonl",
        }
    }
}

/// The user's own GPU box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteGpuConfig {
    pub host: String,
    pub user: String,
    /// Path to the private key, on THIS machine. Never transmitted.
    pub key_path: String,
    /// Absolute working directory on the remote host.
    pub workdir: String,
    /// Python interpreter on the remote host, e.g.
    /// `/opt/llamafactory/venv/bin/python`. `llamafactory-cli` is looked for
    /// next to it; a bare `python3` falls back to `llamafactory-cli` on PATH.
    pub python: String,
    /// Optional llama.cpp checkout on the remote host. When it contains
    /// `convert_lora_to_gguf.py`, a GGUF conversion is attempted after a
    /// successful run — best effort, and only reported if the file appears.
    #[serde(default)]
    pub llama_cpp_dir: Option<String>,
}

/// Together AI job knobs. The API key is read from the environment at call
/// time and never stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TogetherConfig {
    /// Together model id, e.g. `meta-llama/Meta-Llama-3.1-8B-Instruct-Reference`.
    pub model: String,
    /// Optional suffix for the output model name (API max length 64).
    #[serde(default)]
    pub suffix: Option<String>,
}

/// Everything a run needs. Serialised into `<job dir>/job.json`; contains no
/// secrets (the SSH key is a local path, the Together key never lands here).
/// `id` and `method` carry serde defaults so the struct deserialises
/// straight from the RPC params (`finetune.jobs.create` never sends an id —
/// [`create`] mints one).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobConfig {
    #[serde(default)]
    pub id: String,
    pub dataset_id: String,
    pub backend: String,
    /// Base model. For `remote_gpu_ssh` this is a HF repo id or a remote
    /// path; for `together` it is a Together model id.
    pub base_model: String,
    #[serde(default = "default_method")]
    pub method: TrainMethod,
    /// LLaMA-Factory chat template (`qwen`, `llama3`, `gemma`, …). A wrong
    /// template silently trains nonsense, so it is explicit, not guessed.
    #[serde(default = "default_template")]
    pub template: String,
    #[serde(default = "default_lora_rank")]
    pub lora_rank: u32,
    #[serde(default = "default_lora_alpha")]
    pub lora_alpha: u32,
    #[serde(default = "default_epochs")]
    pub epochs: f64,
    #[serde(default = "default_lr")]
    pub learning_rate: f64,
    #[serde(default = "default_cutoff")]
    pub cutoff_len: u32,
    #[serde(default = "default_batch")]
    pub per_device_batch_size: u32,
    #[serde(default = "default_grad_accum")]
    pub gradient_accumulation_steps: u32,
    #[serde(default)]
    pub remote: Option<RemoteGpuConfig>,
    #[serde(default)]
    pub together: Option<TogetherConfig>,
}

fn default_method() -> TrainMethod {
    TrainMethod::Sft
}
fn default_template() -> String {
    "default".to_string()
}
fn default_lora_rank() -> u32 {
    16
}
fn default_lora_alpha() -> u32 {
    32
}
fn default_epochs() -> f64 {
    3.0
}
fn default_lr() -> f64 {
    5e-5
}
fn default_cutoff() -> u32 {
    2048
}
fn default_batch() -> u32 {
    1
}
fn default_grad_accum() -> u32 {
    8
}

impl JobConfig {
    /// Range-check the numeric knobs so a typo cannot burn GPU hours.
    pub fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        validate_id(&self.dataset_id)?;
        if self.base_model.trim().is_empty() {
            return Err(FinetuneError::BadRequest("base_model 不可空白".into()));
        }
        validate_shell_safe("base_model", &self.base_model)?;
        validate_shell_safe("template", &self.template)?;
        if !(1..=256).contains(&self.lora_rank) {
            return Err(FinetuneError::BadRequest("lora_rank 需在 1–256".into()));
        }
        if !(1..=512).contains(&self.lora_alpha) {
            return Err(FinetuneError::BadRequest("lora_alpha 需在 1–512".into()));
        }
        if !(0.01..=100.0).contains(&self.epochs) {
            return Err(FinetuneError::BadRequest("epochs 需在 0.01–100".into()));
        }
        if !(1e-7..=1e-2).contains(&self.learning_rate) {
            return Err(FinetuneError::BadRequest("learning_rate 需在 1e-7–1e-2".into()));
        }
        if !(128..=131_072).contains(&self.cutoff_len) {
            return Err(FinetuneError::BadRequest("cutoff_len 需在 128–131072".into()));
        }
        if !(1..=64).contains(&self.per_device_batch_size) {
            return Err(FinetuneError::BadRequest("batch size 需在 1–64".into()));
        }
        if !(1..=256).contains(&self.gradient_accumulation_steps) {
            return Err(FinetuneError::BadRequest("gradient accumulation 需在 1–256".into()));
        }
        Ok(())
    }
}

/// Deserialise `finetune.jobs.create` params into a [`JobConfig`], turning a
/// serde failure into a `bad_request` the dashboard can render instead of a
/// 500. Extra keys (`acknowledged_data_leaves_device`) are ignored here — the
/// caller reads that one separately.
pub fn config_from_params(params: &Value) -> Result<JobConfig> {
    serde_json::from_value(params.clone())
        .map_err(|e| FinetuneError::BadRequest(format!("訓練參數不正確：{e}")))
}

/// Values interpolated into the remote `/bin/sh` script or an `ssh` argv
/// must contain nothing a shell would act on. Conservative allowlist.
pub fn validate_shell_safe(field: &str, value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 512
        && !value.starts_with('-')
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '@' | '+')
        });
    if ok {
        Ok(())
    } else {
        Err(FinetuneError::BadRequest(format!(
            "{field} 含有不允許的字元（只接受英數字與 . _ - / : @ +）：{value}"
        )))
    }
}

impl RemoteGpuConfig {
    fn validate(&self) -> Result<()> {
        validate_shell_safe("host", &self.host)?;
        validate_shell_safe("user", &self.user)?;
        validate_shell_safe("key_path", &self.key_path)?;
        validate_shell_safe("workdir", &self.workdir)?;
        validate_shell_safe("python", &self.python)?;
        if let Some(d) = &self.llama_cpp_dir {
            validate_shell_safe("llama_cpp_dir", d)?;
        }
        if !self.workdir.starts_with('/') {
            return Err(FinetuneError::BadRequest("workdir 必須是絕對路徑".into()));
        }
        Ok(())
    }

    /// `llamafactory-cli` next to the configured interpreter, so a venv is
    /// used without needing it on PATH.
    pub fn llamafactory_cli(&self) -> String {
        match Path::new(&self.python).parent() {
            Some(p) if !p.as_os_str().is_empty() && self.python.contains('/') => {
                p.join("llamafactory-cli").display().to_string()
            }
            _ => "llamafactory-cli".to_string(),
        }
    }

    fn target(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// `ssh` argv prefix. `BatchMode=yes` matters: without it a host with a
    /// passphrase-protected key hangs the gateway on a password prompt
    /// instead of failing with a message the operator can act on.
    pub fn ssh_argv(&self) -> Vec<String> {
        vec![
            "-i".into(),
            self.key_path.clone(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            "ConnectTimeout=15".into(),
            "-n".into(),
            self.target(),
        ]
    }

    /// The `-e` transport string rsync uses, mirroring `ssh_argv`'s options.
    pub fn rsync_transport(&self) -> String {
        format!(
            "ssh -i {} -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=15",
            self.key_path
        )
    }

    fn remote_job_dir(&self, job_id: &str) -> String {
        format!("{}/{}", self.workdir.trim_end_matches('/'), job_id)
    }
}

// ─────────────────────────── YAML generation ───────────────────────────

/// Render the LLaMA-Factory training config.
///
/// Pure function of the job config and the two remote paths — the SSH
/// backend writes the result verbatim, and `dry_run` writes exactly the same
/// bytes without shipping them anywhere, which is what makes a dry run worth
/// running.
pub fn render_llamafactory_yaml(cfg: &JobConfig, dataset_dir: &str, output_dir: &str) -> String {
    let stage = cfg.method.as_str();
    let mut y = String::new();
    y.push_str("# Generated by DuDuClaw finetune — do not edit by hand.\n");
    y.push_str(&format!("# job: {}\n", cfg.id));
    y.push_str(&format!("# dataset: {}\n\n", cfg.dataset_id));

    y.push_str("### model\n");
    y.push_str(&format!("model_name_or_path: {}\n", cfg.base_model));
    y.push_str("trust_remote_code: true\n\n");

    y.push_str("### method\n");
    y.push_str(&format!("stage: {stage}\n"));
    y.push_str("do_train: true\n");
    y.push_str("finetuning_type: lora\n");
    y.push_str("lora_target: all\n");
    y.push_str(&format!("lora_rank: {}\n", cfg.lora_rank));
    y.push_str(&format!("lora_alpha: {}\n", cfg.lora_alpha));
    if cfg.method == TrainMethod::Dpo {
        y.push_str("pref_beta: 0.1\n");
        y.push_str("pref_loss: sigmoid\n");
    }
    y.push('\n');

    y.push_str("### dataset\n");
    y.push_str(&format!("dataset: {}\n", cfg.method.dataset_name()));
    y.push_str(&format!("dataset_dir: {dataset_dir}\n"));
    y.push_str(&format!("template: {}\n", cfg.template));
    y.push_str(&format!("cutoff_len: {}\n", cfg.cutoff_len));
    y.push_str("overwrite_cache: true\n");
    y.push_str("preprocessing_num_workers: 4\n\n");

    y.push_str("### output\n");
    y.push_str(&format!("output_dir: {output_dir}\n"));
    y.push_str("logging_steps: 5\n");
    y.push_str("save_steps: 500\n");
    y.push_str("plot_loss: true\n");
    y.push_str("overwrite_output_dir: true\n\n");

    y.push_str("### train\n");
    y.push_str(&format!(
        "per_device_train_batch_size: {}\n",
        cfg.per_device_batch_size
    ));
    y.push_str(&format!(
        "gradient_accumulation_steps: {}\n",
        cfg.gradient_accumulation_steps
    ));
    y.push_str(&format!("learning_rate: {}\n", cfg.learning_rate));
    y.push_str(&format!("num_train_epochs: {}\n", cfg.epochs));
    y.push_str("lr_scheduler_type: cosine\n");
    y.push_str("warmup_ratio: 0.1\n");
    y.push_str("bf16: true\n");
    y
}

// ─────────────────────────── job record ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobArtifact {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
}

/// `planned` | `preparing` | `running` | `succeeded` | `failed` | `cancelled`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub config: JobConfig,
    pub state: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    /// Provider-side id (`ft-…`) or the remote PID, whichever applies.
    #[serde(default)]
    pub remote_ref: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<JobArtifact>,
    /// Verbatim tail of the real training log. Never synthesised.
    #[serde(default)]
    pub log_tail: Vec<String>,
}

impl JobRecord {
    fn is_terminal(&self) -> bool {
        matches!(self.state.as_str(), "succeeded" | "failed" | "cancelled")
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn record_path(home: &Path, id: &str) -> Result<PathBuf> {
    Ok(job_dir(home, id)?.join("job.json"))
}

pub fn read_record(home: &Path, id: &str) -> Result<JobRecord> {
    let path = record_path(home, id)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| FinetuneError::NotFound(format!("訓練工作 {id}")))?;
    serde_json::from_str(&raw).map_err(|e| io_err(&path, e))
}

fn write_record(home: &Path, rec: &JobRecord) -> Result<()> {
    let path = record_path(home, &rec.id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let body = serde_json::to_string_pretty(rec).map_err(|e| io_err(&path, e))?;
    std::fs::write(&path, body).map_err(|e| io_err(&path, e))
}

// ─────────────────────────── backend trait ───────────────────────────

/// Paths a backend may write into.
pub struct JobPaths {
    /// `<home>/finetune/jobs/<id>`.
    pub job_dir: PathBuf,
    /// `<home>/finetune/datasets/<dataset_id>`.
    pub dataset_dir: PathBuf,
}

#[async_trait]
pub trait FinetuneBackend: Send + Sync {
    fn id(&self) -> &'static str;

    /// Does using this backend move data off the machine? Drives the privacy
    /// gate in [`create`].
    fn is_remote(&self) -> bool {
        backend_is_remote(self.id())
    }

    /// Config check that touches neither network nor disk.
    fn validate(&self, cfg: &JobConfig) -> Result<()>;

    /// Kick the run off. Mutates `rec` in place with whatever was actually
    /// observed.
    async fn start(&self, cfg: &JobConfig, paths: &JobPaths, rec: &mut JobRecord) -> Result<()>;

    /// Refresh from the real world. Must not advance the state on a
    /// transport failure — an unreachable host is a *poll* failure, not a
    /// training failure.
    async fn poll(&self, cfg: &JobConfig, paths: &JobPaths, rec: &mut JobRecord) -> Result<()>;

    async fn cancel(&self, cfg: &JobConfig, paths: &JobPaths, rec: &mut JobRecord) -> Result<()>;
}

/// Resolve a backend id to its implementation.
pub fn backend_for(id: &str) -> Result<Box<dyn FinetuneBackend>> {
    match id {
        "dry_run" => Ok(Box::new(DryRunBackend)),
        "remote_gpu_ssh" => Ok(Box::new(RemoteGpuSshBackend)),
        "together" => Ok(Box::new(TogetherBackend)),
        other => Err(FinetuneError::BadRequest(format!(
            "未知的訓練後端：{other}（可用：dry_run、remote_gpu_ssh、together）"
        ))),
    }
}

// ─────────────────────────── dry run ───────────────────────────

/// Validates, writes `train.yaml` and `plan.json`, and stops. The point is
/// that everything except the GPU is exercised: the same YAML bytes, the
/// same command string, the same remote paths.
pub struct DryRunBackend;

#[async_trait]
impl FinetuneBackend for DryRunBackend {
    fn id(&self) -> &'static str {
        "dry_run"
    }

    fn validate(&self, cfg: &JobConfig) -> Result<()> {
        cfg.validate()
    }

    async fn start(&self, cfg: &JobConfig, paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        let (dataset_dir, output_dir, command) = match &cfg.remote {
            // With a remote configured, plan the exact remote invocation.
            Some(r) => {
                r.validate()?;
                let d = r.remote_job_dir(&cfg.id);
                (
                    format!("{d}/data"),
                    format!("{d}/output"),
                    format!("cd {d} && {} train train.yaml", r.llamafactory_cli()),
                )
            }
            // Without one, plan against local paths so the YAML is still real.
            None => (
                paths.dataset_dir.display().to_string(),
                paths.job_dir.join("output").display().to_string(),
                "llamafactory-cli train train.yaml".to_string(),
            ),
        };
        let yaml = render_llamafactory_yaml(cfg, &dataset_dir, &output_dir);
        write_yaml(&paths.job_dir, &yaml)?;

        let source = paths.dataset_dir.join(cfg.method.source_file());
        let rows = count_jsonl_rows(&source);
        let plan = json!({
            "backend": "dry_run",
            "trained": false,
            "note": "乾跑：只驗證設定並產生 train.yaml，沒有任何訓練發生，也沒有資料離開本機。",
            "command": command,
            "dataset_file": source.display().to_string(),
            "dataset_rows": rows,
            "remote_dataset_dir": dataset_dir,
            "remote_output_dir": output_dir,
        });
        let plan_path = paths.job_dir.join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::to_string_pretty(&plan).map_err(|e| io_err(&plan_path, e))?,
        )
        .map_err(|e| io_err(&plan_path, e))?;

        rec.state = "planned".into();
        rec.detail = Some("乾跑完成：設定有效，已產生 train.yaml 與 plan.json。沒有進行訓練。".into());
        rec.artifacts = vec![
            artifact(&paths.job_dir.join("train.yaml")),
            artifact(&plan_path),
        ]
        .into_iter()
        .flatten()
        .collect();
        if rows == 0 {
            rec.detail = Some(format!(
                "乾跑完成，但 {} 沒有任何資料列——請先建構資料集。",
                cfg.method.source_file()
            ));
        }
        Ok(())
    }

    async fn poll(&self, _cfg: &JobConfig, _paths: &JobPaths, _rec: &mut JobRecord) -> Result<()> {
        // A dry run has nothing to observe. Staying at `planned` is the
        // honest answer; inventing a "completed" here would be the exact
        // failure mode this module exists to avoid.
        Ok(())
    }

    async fn cancel(&self, _cfg: &JobConfig, _paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        rec.state = "cancelled".into();
        rec.finished_at = Some(now());
        Ok(())
    }
}

fn write_yaml(job_dir: &Path, yaml: &str) -> Result<()> {
    std::fs::create_dir_all(job_dir).map_err(|e| io_err(job_dir, e))?;
    let p = job_dir.join("train.yaml");
    std::fs::write(&p, yaml).map_err(|e| io_err(&p, e))
}

fn artifact(p: &Path) -> Option<JobArtifact> {
    let md = std::fs::metadata(p).ok()?;
    Some(JobArtifact {
        name: p.file_name()?.to_string_lossy().to_string(),
        path: p.display().to_string(),
        size_bytes: md.len(),
    })
}

fn count_jsonl_rows(p: &Path) -> usize {
    std::fs::read_to_string(p)
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

// ─────────────────────────── remote GPU over SSH ───────────────────────────

pub struct RemoteGpuSshBackend;

/// The launch script. Kept as a pure function so its shape is testable
/// without an SSH host.
pub fn remote_launch_script(cfg: &JobConfig, r: &RemoteGpuConfig) -> String {
    let d = r.remote_job_dir(&cfg.id);
    let cli = r.llamafactory_cli();
    format!(
        // `command -v` first so a missing / mistyped interpreter path fails
        // with a message the operator can act on, instead of an empty
        // train.log and a job that looks like a training failure.
        "set -eu\n\
         d={d}\n\
         mkdir -p \"$d/output\"\n\
         cd \"$d\"\n\
         command -v {cli} >/dev/null 2>&1 || {{ echo 'DUDUCLAW_ERROR=llamafactory-cli not found: {cli}' >&2; exit 127; }}\n\
         nohup {cli} train train.yaml > train.log 2>&1 &\n\
         echo $! > train.pid\n\
         echo \"DUDUCLAW_PID=$(cat train.pid)\"\n"
    )
}

/// The poll script: PID liveness, adapter presence, optional GGUF, log tail.
/// Emits marker lines so parsing never depends on log formatting.
pub fn remote_poll_script(cfg: &JobConfig, r: &RemoteGpuConfig) -> String {
    let d = r.remote_job_dir(&cfg.id);
    format!(
        "d={d}\n\
         pid=$(cat \"$d/train.pid\" 2>/dev/null || echo)\n\
         if [ -n \"$pid\" ] && kill -0 \"$pid\" 2>/dev/null; then echo DUDUCLAW_ALIVE=yes; else echo DUDUCLAW_ALIVE=no; fi\n\
         if [ -f \"$d/output/adapter_model.safetensors\" ] || [ -f \"$d/output/adapter_model.bin\" ]; then echo DUDUCLAW_ADAPTER=yes; else echo DUDUCLAW_ADAPTER=no; fi\n\
         echo DUDUCLAW_LOG_BEGIN\n\
         tail -n {LOG_TAIL_LINES} \"$d/train.log\" 2>/dev/null || true\n"
    )
}

/// Best-effort GGUF conversion on the remote host, run only when the user
/// pointed us at a llama.cpp checkout that actually has the script.
pub fn remote_convert_script(cfg: &JobConfig, r: &RemoteGpuConfig) -> Option<String> {
    let llama = r.llama_cpp_dir.as_ref()?;
    let d = r.remote_job_dir(&cfg.id);
    Some(format!(
        "d={d}\n\
         s={llama}/convert_lora_to_gguf.py\n\
         if [ -f \"$s\" ]; then\n\
         \x20 {py} \"$s\" \"$d/output\" --base {base} --outfile \"$d/output/{id}-lora.gguf\" >> \"$d/convert.log\" 2>&1 || echo DUDUCLAW_CONVERT=failed\n\
         else\n\
         \x20 echo DUDUCLAW_CONVERT=absent\n\
         fi\n\
         if [ -f \"$d/output/{id}-lora.gguf\" ]; then echo DUDUCLAW_GGUF=yes; else echo DUDUCLAW_GGUF=no; fi\n",
        py = r.python,
        base = cfg.base_model,
        id = cfg.id,
    ))
}

/// Parsed result of one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatus {
    pub alive: bool,
    pub adapter: bool,
    pub log_tail: Vec<String>,
}

/// Parse the marker-delimited poll output. Unknown/garbled output yields
/// `alive: false, adapter: false` — but the caller treats a *transport*
/// failure separately, so a network blip never reads as "training failed".
pub fn parse_remote_status(stdout: &str) -> RemoteStatus {
    let mut alive = false;
    let mut adapter = false;
    let mut log = Vec::new();
    let mut in_log = false;
    for line in stdout.lines() {
        if in_log {
            log.push(line.to_string());
            continue;
        }
        match line.trim() {
            "DUDUCLAW_ALIVE=yes" => alive = true,
            "DUDUCLAW_ADAPTER=yes" => adapter = true,
            "DUDUCLAW_LOG_BEGIN" => in_log = true,
            _ => {}
        }
    }
    if log.len() > LOG_TAIL_LINES {
        log = log.split_off(log.len() - LOG_TAIL_LINES);
    }
    RemoteStatus { alive, adapter, log_tail: log }
}

async fn run(program: &str, args: &[String]) -> Result<(bool, String, String)> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| FinetuneError::Backend(format!("無法執行 {program}：{e}")))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

/// `ssh <opts> <target> <script>` — the script goes in as one argv element,
/// so nothing in it is re-split by the local shell (there is no local shell:
/// we exec `ssh` directly).
async fn ssh_exec(r: &RemoteGpuConfig, script: &str) -> Result<(bool, String, String)> {
    let mut args = r.ssh_argv();
    args.push(script.to_string());
    run("ssh", &args).await
}

#[async_trait]
impl FinetuneBackend for RemoteGpuSshBackend {
    fn id(&self) -> &'static str {
        "remote_gpu_ssh"
    }

    fn validate(&self, cfg: &JobConfig) -> Result<()> {
        cfg.validate()?;
        let r = cfg.remote.as_ref().ok_or_else(|| {
            FinetuneError::BadRequest("remote_gpu_ssh 需要 remote{host,user,key_path,workdir,python}".into())
        })?;
        r.validate()?;
        if !Path::new(&r.key_path).exists() {
            return Err(FinetuneError::BadRequest(format!(
                "找不到 SSH 金鑰：{}",
                r.key_path
            )));
        }
        Ok(())
    }

    async fn start(&self, cfg: &JobConfig, paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        let r = cfg.remote.as_ref().expect("validated");
        let remote_dir = r.remote_job_dir(&cfg.id);

        let source = paths.dataset_dir.join(cfg.method.source_file());
        if count_jsonl_rows(&source) == 0 {
            return Err(FinetuneError::BadRequest(format!(
                "{} 沒有資料列，先建構資料集再送訓練",
                cfg.method.source_file()
            )));
        }

        rec.state = "preparing".into();
        rec.started_at = Some(now());

        let yaml = render_llamafactory_yaml(
            cfg,
            &format!("{remote_dir}/data"),
            &format!("{remote_dir}/output"),
        );
        write_yaml(&paths.job_dir, &yaml)?;

        // 1. remote dirs
        let (ok, _o, e) = ssh_exec(r, &format!("mkdir -p {remote_dir}/data {remote_dir}/output")).await?;
        if !ok {
            return Err(FinetuneError::Backend(format!("建立遠端目錄失敗：{}", e.trim())));
        }

        // 2. dataset + generated YAML up
        let mut src = paths.dataset_dir.display().to_string();
        src.push('/');
        let (ok, _o, e) = run(
            "rsync",
            &[
                "-az".into(),
                "-e".into(),
                r.rsync_transport(),
                src,
                format!("{}:{remote_dir}/data/", r.target()),
            ],
        )
        .await?;
        if !ok {
            return Err(FinetuneError::Backend(format!("上傳資料集失敗：{}", e.trim())));
        }
        let (ok, _o, e) = run(
            "rsync",
            &[
                "-az".into(),
                "-e".into(),
                r.rsync_transport(),
                paths.job_dir.join("train.yaml").display().to_string(),
                format!("{}:{remote_dir}/train.yaml", r.target()),
            ],
        )
        .await?;
        if !ok {
            return Err(FinetuneError::Backend(format!("上傳訓練設定失敗：{}", e.trim())));
        }

        // 3. launch
        let (ok, out, e) = ssh_exec(r, &remote_launch_script(cfg, r)).await?;
        if !ok {
            return Err(FinetuneError::Backend(format!("啟動訓練失敗：{}", e.trim())));
        }
        let pid = out
            .lines()
            .find_map(|l| l.trim().strip_prefix("DUDUCLAW_PID=").map(str::to_string));
        rec.remote_ref = pid.clone();
        rec.state = "running".into();
        rec.detail = Some(format!(
            "已在 {}@{} 啟動 LLaMA-Factory（PID {}）。訓練在那台機器上跑，不在這台。",
            r.user,
            r.host,
            pid.as_deref().unwrap_or("?")
        ));
        Ok(())
    }

    async fn poll(&self, cfg: &JobConfig, paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        if rec.is_terminal() {
            return Ok(());
        }
        let r = cfg.remote.as_ref().ok_or_else(|| {
            FinetuneError::BadRequest("這個工作缺少 remote 設定，無法查詢狀態".into())
        })?;
        let (ok, out, err) = ssh_exec(r, &remote_poll_script(cfg, r)).await?;
        if !ok {
            // Transport failure. Report it, keep the state — the run may well
            // still be going on a host we momentarily cannot reach.
            rec.detail = Some(format!("暫時連不上訓練主機：{}", err.trim()));
            return Ok(());
        }
        let st = parse_remote_status(&out);
        rec.log_tail = st.log_tail;
        if st.alive {
            rec.state = "running".into();
            rec.detail = Some("訓練進行中（狀態來自遠端行程與訓練日誌）。".into());
            return Ok(());
        }
        if st.adapter {
            // Optional GGUF conversion, then fetch.
            if let Some(script) = remote_convert_script(cfg, r) {
                let (_ok, cout, _e) = ssh_exec(r, &script).await.unwrap_or((false, String::new(), String::new()));
                if cout.contains("DUDUCLAW_GGUF=yes") {
                    rec.detail = Some("訓練完成，遠端已轉出 GGUF。".into());
                } else if cout.contains("DUDUCLAW_CONVERT=absent") {
                    rec.detail =
                        Some("訓練完成。遠端沒有 llama.cpp 轉檔腳本，只取回 LoRA adapter。".into());
                } else {
                    rec.detail = Some("訓練完成。GGUF 轉檔沒有成功，只取回 LoRA adapter。".into());
                }
            } else {
                rec.detail = Some("訓練完成，已取回 LoRA adapter。".into());
            }
            let dest = paths.job_dir.join("artifacts");
            std::fs::create_dir_all(&dest).map_err(|e| io_err(&dest, e))?;
            let (ok, _o, e) = run(
                "rsync",
                &[
                    "-az".into(),
                    "-e".into(),
                    r.rsync_transport(),
                    format!("{}:{}/output/", r.target(), r.remote_job_dir(&cfg.id)),
                    format!("{}/", dest.display()),
                ],
            )
            .await?;
            if !ok {
                rec.state = "failed".into();
                rec.error = Some(format!("訓練完成但取回產物失敗：{}", e.trim()));
                rec.finished_at = Some(now());
                return Ok(());
            }
            rec.artifacts = collect_artifacts(&dest);
            rec.state = "succeeded".into();
            rec.finished_at = Some(now());
            return Ok(());
        }
        rec.state = "failed".into();
        rec.error = Some("遠端訓練行程已結束但沒有產出 adapter，請看下方日誌。".into());
        rec.finished_at = Some(now());
        Ok(())
    }

    async fn cancel(&self, cfg: &JobConfig, _paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        if let (Some(r), Some(pid)) = (cfg.remote.as_ref(), rec.remote_ref.as_deref()) {
            if pid.chars().all(|c| c.is_ascii_digit()) && !pid.is_empty() {
                let _ = ssh_exec(r, &format!("kill {pid} 2>/dev/null || true")).await;
            }
        }
        rec.state = "cancelled".into();
        rec.finished_at = Some(now());
        rec.detail = Some("已送出停止指令給訓練主機。".into());
        Ok(())
    }
}

fn collect_artifacts(dir: &Path) -> Vec<JobArtifact> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Some(a) = artifact(&e.path()) {
                out.push(a);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// ─────────────────────────── Together AI ───────────────────────────

/// Together AI fine-tuning.
///
/// Wire shapes verified 2026-09-05 against the live OpenAPI documents at
/// `docs.together.ai/reference/{post-fine-tunes,get-fine-tunes-id,
/// post-fine-tunes-id-cancel,upload-file}`:
///
/// - `POST /v1/files/upload` — multipart with `file`, `file_name`,
///   `purpose=fine-tune`; returns `{id, …}`.
/// - `POST /v1/fine-tunes` — `{model, training_file, n_epochs,
///   learning_rate, suffix?, training_type:{type:"Lora",lora_r,lora_alpha},
///   training_method:{…}}`; returns `{id: "ft-…", status, …}`.
/// - `GET /v1/fine-tunes/{id}` / `POST /v1/fine-tunes/{id}/cancel`.
/// - Status enum: pending, queued, running, compressing, uploading,
///   cancel_requested, cancelled, error, completed.
///
/// **Unverified** (documented here rather than papered over): the flat
/// SDK-style `training_method: "dpo"` string — the OpenAPI schema says the
/// field is an object, so this client sends the object form; and
/// `GET /v1/fine-tunes` has no documented pagination parameters, so listing
/// is not used (we track jobs by id ourselves).
pub struct TogetherBackend;

/// Convert one JSONL corpus from our LLaMA-Factory shape into Together's.
///
/// Together does not accept ShareGPT: SFT wants
/// `{"messages":[{"role","content"}]}` and DPO wants
/// `{"input":{"messages":[…]},"preferred_output":[…],"non_preferred_output":[…]}`.
/// A line we cannot map is dropped rather than uploaded malformed.
pub fn to_together_jsonl(body: &str, method: TrainMethod) -> Result<String> {
    let mut out = String::new();
    let mut kept = 0usize;
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mapped = match method {
            TrainMethod::Sft => sft_line_to_together(&v),
            TrainMethod::Dpo => dpo_line_to_together(&v),
        };
        if let Some(m) = mapped {
            out.push_str(&m.to_string());
            out.push('\n');
            kept += 1;
        }
    }
    if kept == 0 {
        return Err(FinetuneError::BadRequest(
            "資料集裡沒有可以轉成 Together 格式的資料列".into(),
        ));
    }
    Ok(out)
}

fn msg(role: &str, content: &str) -> Value {
    json!({ "role": role, "content": content })
}

fn sft_line_to_together(v: &Value) -> Option<Value> {
    let mut messages = Vec::new();
    if let Some(sys) = v.get("system").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        messages.push(msg("system", sys));
    }
    if let Some(convs) = v.get("conversations").and_then(Value::as_array) {
        // ShareGPT
        for c in convs {
            let from = c.get("from").and_then(Value::as_str)?;
            let value = c.get("value").and_then(Value::as_str)?;
            let role = match from {
                "human" => "user",
                "gpt" => "assistant",
                _ => continue,
            };
            messages.push(msg(role, value));
        }
    } else {
        // Alpaca
        for pair in v.get("history").and_then(Value::as_array).into_iter().flatten() {
            let q = pair.get(0).and_then(Value::as_str)?;
            let a = pair.get(1).and_then(Value::as_str)?;
            messages.push(msg("user", q));
            messages.push(msg("assistant", a));
        }
        let instruction = v.get("instruction").and_then(Value::as_str)?;
        let input = v.get("input").and_then(Value::as_str).unwrap_or("");
        let user = if input.trim().is_empty() {
            instruction.to_string()
        } else {
            format!("{instruction}\n\n{input}")
        };
        messages.push(msg("user", &user));
        messages.push(msg("assistant", v.get("output").and_then(Value::as_str)?));
    }
    if messages.iter().all(|m| m["role"] != "assistant") {
        return None;
    }
    Some(json!({ "messages": messages }))
}

fn dpo_line_to_together(v: &Value) -> Option<Value> {
    let (prompt, chosen, rejected) = if let Some(convs) = v.get("conversations").and_then(Value::as_array) {
        (
            convs.first()?.get("value").and_then(Value::as_str)?.to_string(),
            v.get("chosen")?.get("value").and_then(Value::as_str)?.to_string(),
            v.get("rejected")?.get("value").and_then(Value::as_str)?.to_string(),
        )
    } else {
        (
            v.get("instruction").and_then(Value::as_str)?.to_string(),
            v.get("chosen").and_then(Value::as_str)?.to_string(),
            v.get("rejected").and_then(Value::as_str)?.to_string(),
        )
    };
    Some(json!({
        "input": { "messages": [ msg("user", &prompt) ] },
        "preferred_output": [ msg("assistant", &chosen) ],
        "non_preferred_output": [ msg("assistant", &rejected) ],
    }))
}

/// The `POST /v1/fine-tunes` body. Pure so the shape is testable without a
/// network call or an API key.
pub fn together_create_body(cfg: &JobConfig, training_file_id: &str) -> Value {
    let t = cfg.together.as_ref();
    let mut body = json!({
        "model": t.map(|t| t.model.clone()).unwrap_or_else(|| cfg.base_model.clone()),
        "training_file": training_file_id,
        "n_epochs": cfg.epochs.round().max(1.0) as u64,
        "learning_rate": cfg.learning_rate,
        // OpenAPI: `training_type` is an object; `lora_r` and `lora_alpha`
        // live inside it, NOT at the top level.
        "training_type": {
            "type": "Lora",
            "lora_r": cfg.lora_rank,
            "lora_alpha": cfg.lora_alpha,
        },
        // OpenAPI: `training_method` is a oneOf object, not a string.
        "training_method": match cfg.method {
            TrainMethod::Sft => json!({ "method": "sft", "train_on_inputs": "auto" }),
            TrainMethod::Dpo => json!({ "method": "dpo", "dpo_beta": 0.1 }),
        },
    });
    if let Some(suffix) = t.and_then(|t| t.suffix.as_deref()).filter(|s| !s.trim().is_empty()) {
        // API maxLength is 64.
        let s: String = suffix.chars().take(64).collect();
        body["suffix"] = json!(s);
    }
    body
}

/// Map Together's nine documented statuses onto our six states.
pub fn map_together_status(status: &str) -> &'static str {
    match status {
        "pending" | "queued" | "uploading" => "preparing",
        "running" | "compressing" => "running",
        "completed" => "succeeded",
        "error" => "failed",
        "cancelled" | "cancel_requested" => "cancelled",
        // An unrecognised status is not silently treated as success.
        _ => "running",
    }
}

fn together_key() -> Result<String> {
    std::env::var(TOGETHER_KEY_ENV)
        .ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            FinetuneError::BadRequest(format!(
                "未設定 {TOGETHER_KEY_ENV}——請先在 gateway 環境設定 Together API 金鑰"
            ))
        })
}

#[async_trait]
impl FinetuneBackend for TogetherBackend {
    fn id(&self) -> &'static str {
        "together"
    }

    fn validate(&self, cfg: &JobConfig) -> Result<()> {
        cfg.validate()?;
        let t = cfg.together.as_ref().ok_or_else(|| {
            FinetuneError::BadRequest("together 後端需要 together{model}".into())
        })?;
        if t.model.trim().is_empty() {
            return Err(FinetuneError::BadRequest("together.model 不可空白".into()));
        }
        Ok(())
    }

    async fn start(&self, cfg: &JobConfig, paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        let key = together_key()?;
        let source = paths.dataset_dir.join(cfg.method.source_file());
        let body = std::fs::read_to_string(&source)
            .map_err(|_| FinetuneError::BadRequest(format!(
                "{} 不存在，先建構資料集再送訓練",
                cfg.method.source_file()
            )))?;
        let converted = to_together_jsonl(&body, cfg.method)?;
        // Keep exactly what we uploaded, so "what did it train on" is
        // answerable after the fact.
        let upload_path = paths.job_dir.join("together_upload.jsonl");
        std::fs::create_dir_all(&paths.job_dir).map_err(|e| io_err(&paths.job_dir, e))?;
        std::fs::write(&upload_path, &converted).map_err(|e| io_err(&upload_path, e))?;

        rec.state = "preparing".into();
        rec.started_at = Some(now());

        let client = reqwest::Client::new();
        let form = reqwest::multipart::Form::new()
            .text("purpose", "fine-tune")
            .text("file_name", format!("{}.jsonl", cfg.id))
            .text("file_type", "jsonl")
            .part(
                "file",
                reqwest::multipart::Part::bytes(converted.into_bytes())
                    .file_name(format!("{}.jsonl", cfg.id)),
            );
        let resp = client
            .post(format!("{TOGETHER_BASE}/files/upload"))
            .bearer_auth(&key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| FinetuneError::Backend(format!("上傳資料集失敗：{e}")))?;
        let up: Value = json_or_error(resp, "檔案上傳").await?;
        let file_id = up
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| FinetuneError::Backend("Together 回應沒有 file id".into()))?
            .to_string();

        let resp = client
            .post(format!("{TOGETHER_BASE}/fine-tunes"))
            .bearer_auth(&key)
            .json(&together_create_body(cfg, &file_id))
            .send()
            .await
            .map_err(|e| FinetuneError::Backend(format!("建立訓練工作失敗：{e}")))?;
        let created: Value = json_or_error(resp, "建立訓練工作").await?;
        let job_id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| FinetuneError::Backend("Together 回應沒有 job id".into()))?
            .to_string();
        let status = created.get("status").and_then(Value::as_str).unwrap_or("pending");
        rec.remote_ref = Some(job_id.clone());
        rec.state = map_together_status(status).to_string();
        rec.detail = Some(format!(
            "已送到 Together（工作 {job_id}，狀態 {status}）。訓練在他們的 GPU 上進行，資料已離開這台機器。"
        ));
        Ok(())
    }

    async fn poll(&self, _cfg: &JobConfig, _paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        if rec.is_terminal() {
            return Ok(());
        }
        let Some(job_id) = rec.remote_ref.clone() else {
            return Ok(());
        };
        let key = together_key()?;
        let resp = reqwest::Client::new()
            .get(format!("{TOGETHER_BASE}/fine-tunes/{job_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .map_err(|e| FinetuneError::Backend(format!("查詢狀態失敗：{e}")))?;
        let v: Value = json_or_error(resp, "查詢狀態").await?;
        let status = v.get("status").and_then(Value::as_str).unwrap_or("running");
        rec.state = map_together_status(status).to_string();
        rec.detail = Some(format!("Together 回報狀態：{status}"));
        if let Some(name) = v.get("model_output_name").and_then(Value::as_str) {
            rec.detail = Some(format!("Together 回報狀態：{status}（輸出模型 {name}）"));
        }
        // Pass the provider's own estimate through untouched; never turn it
        // into a percentage we did not measure.
        if v.get("progress").and_then(|p| p.get("estimate_available")) == Some(&json!(true)) {
            if let Some(sec) = v["progress"].get("seconds_remaining").and_then(Value::as_i64) {
                rec.log_tail = vec![format!("Together 估計剩餘 {sec} 秒")];
            }
        }
        if rec.is_terminal() {
            rec.finished_at = Some(now());
        }
        if status == "error" {
            rec.error = Some("Together 回報訓練失敗，請到 Together 主控台看詳細事件。".into());
        }
        Ok(())
    }

    async fn cancel(&self, _cfg: &JobConfig, _paths: &JobPaths, rec: &mut JobRecord) -> Result<()> {
        let Some(job_id) = rec.remote_ref.clone() else {
            rec.state = "cancelled".into();
            rec.finished_at = Some(now());
            return Ok(());
        };
        let key = together_key()?;
        let resp = reqwest::Client::new()
            .post(format!("{TOGETHER_BASE}/fine-tunes/{job_id}/cancel"))
            .bearer_auth(&key)
            .send()
            .await
            .map_err(|e| FinetuneError::Backend(format!("取消失敗：{e}")))?;
        let _ = json_or_error(resp, "取消").await?;
        rec.state = "cancelled".into();
        rec.finished_at = Some(now());
        Ok(())
    }
}

/// Together error bodies are `{"error":{"message","type",…}}`; surface the
/// message rather than a bare status code.
async fn json_or_error(resp: reqwest::Response, what: &str) -> Result<Value> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| FinetuneError::Backend(format!("{what}：讀取回應失敗 {e}")))?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(v);
    }
    let msg = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| text.trim())
        .chars()
        .take(400)
        .collect::<String>();
    Err(FinetuneError::Backend(format!("{what} 失敗（HTTP {status}）：{msg}")))
}

// ─────────────────────────── RPC surface ───────────────────────────

fn paths_for(home: &Path, cfg: &JobConfig) -> Result<JobPaths> {
    Ok(JobPaths {
        job_dir: job_dir(home, &cfg.id)?,
        dataset_dir: super::dataset_dir(home, &cfg.dataset_id)?,
    })
}

/// `finetune.jobs.list` — newest first.
pub fn list(home: &Path) -> Result<Vec<JobRecord>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(jobs_root(home)) else {
        return Ok(out);
    };
    for e in entries.flatten() {
        let Some(id) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if validate_id(&id).is_err() {
            continue;
        }
        if let Ok(r) = read_record(home, &id) {
            out.push(r);
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// `finetune.jobs.create`.
///
/// Order matters: the privacy gate runs before validation and before any
/// network call, so an un-acknowledged remote job never so much as opens a
/// socket.
pub async fn create(home: &Path, mut cfg: JobConfig, acknowledged: bool) -> Result<JobRecord> {
    let backend = backend_for(&cfg.backend)?;
    if backend.is_remote() {
        require_data_leaves_device_ack(
            acknowledged,
            &format!("送到「{}」訓練", cfg.backend),
        )?;
    }
    if cfg.id.is_empty() {
        cfg.id = new_id("job");
    }
    backend.validate(&cfg)?;

    // The dataset must exist and must have been built.
    let ds_dir = super::dataset_dir(home, &cfg.dataset_id)?;
    if !ds_dir.join("train.jsonl").exists() {
        return Err(FinetuneError::NotFound(format!(
            "資料集 {}（或它還沒建構過）",
            cfg.dataset_id
        )));
    }

    let paths = paths_for(home, &cfg)?;
    std::fs::create_dir_all(&paths.job_dir).map_err(|e| io_err(&paths.job_dir, e))?;
    let mut rec = JobRecord {
        id: cfg.id.clone(),
        config: cfg.clone(),
        state: "preparing".into(),
        detail: None,
        created_at: now(),
        started_at: None,
        finished_at: None,
        remote_ref: None,
        error: None,
        artifacts: Vec::new(),
        log_tail: Vec::new(),
    };
    write_record(home, &rec)?;

    match backend.start(&cfg, &paths, &mut rec).await {
        Ok(()) => {}
        Err(e) => {
            rec.state = "failed".into();
            rec.error = Some(e.message());
            rec.finished_at = Some(now());
            write_record(home, &rec)?;
            return Err(e);
        }
    }
    write_record(home, &rec)?;
    Ok(rec)
}

/// `finetune.jobs.status` — refreshes from the backend, then persists.
pub async fn status(home: &Path, id: &str) -> Result<JobRecord> {
    let mut rec = read_record(home, id)?;
    let backend = backend_for(&rec.config.backend)?;
    let paths = paths_for(home, &rec.config)?;
    let cfg = rec.config.clone();
    backend.poll(&cfg, &paths, &mut rec).await?;
    write_record(home, &rec)?;
    Ok(rec)
}

/// `finetune.jobs.cancel`.
pub async fn cancel(home: &Path, id: &str) -> Result<JobRecord> {
    let mut rec = read_record(home, id)?;
    if rec.is_terminal() {
        return Ok(rec);
    }
    let backend = backend_for(&rec.config.backend)?;
    let paths = paths_for(home, &rec.config)?;
    let cfg = rec.config.clone();
    backend.cancel(&cfg, &paths, &mut rec).await?;
    write_record(home, &rec)?;
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finetune::dataset;

    fn cfg(backend: &str, method: TrainMethod) -> JobConfig {
        JobConfig {
            id: "job-test-1".into(),
            dataset_id: "ds-test-1".into(),
            backend: backend.into(),
            base_model: "Qwen/Qwen3-4B-Instruct".into(),
            method,
            template: "qwen".into(),
            lora_rank: 16,
            lora_alpha: 32,
            epochs: 3.0,
            learning_rate: 5e-5,
            cutoff_len: 2048,
            per_device_batch_size: 1,
            gradient_accumulation_steps: 8,
            remote: None,
            together: None,
        }
    }

    fn remote() -> RemoteGpuConfig {
        RemoteGpuConfig {
            host: "gpu.example.com".into(),
            user: "trainer".into(),
            key_path: "/home/kai/.ssh/gpu_ed25519".into(),
            workdir: "/srv/duduclaw-train".into(),
            python: "/opt/llamafactory/venv/bin/python".into(),
            llama_cpp_dir: Some("/opt/llama.cpp".into()),
        }
    }

    // ── YAML generation ──

    #[test]
    fn yaml_carries_sft_stage_lora_knobs_and_the_registered_dataset_name() {
        let y = render_llamafactory_yaml(&cfg("dry_run", TrainMethod::Sft), "/r/data", "/r/output");
        assert!(y.contains("stage: sft"));
        assert!(y.contains("finetuning_type: lora"));
        assert!(y.contains("lora_rank: 16"));
        assert!(y.contains("lora_alpha: 32"));
        assert!(y.contains("dataset: duduclaw_sft"));
        assert!(y.contains("dataset_dir: /r/data"));
        assert!(y.contains("output_dir: /r/output"));
        assert!(y.contains("template: qwen"));
        assert!(y.contains("model_name_or_path: Qwen/Qwen3-4B-Instruct"));
        assert!(y.contains("num_train_epochs: 3"));
        assert!(y.contains("cutoff_len: 2048"));
        // SFT must not carry DPO-only knobs.
        assert!(!y.contains("pref_beta"));
    }

    #[test]
    fn yaml_switches_to_dpo_stage_dataset_and_preference_knobs() {
        let y = render_llamafactory_yaml(&cfg("dry_run", TrainMethod::Dpo), "/r/data", "/r/output");
        assert!(y.contains("stage: dpo"));
        assert!(y.contains("dataset: duduclaw_dpo"));
        assert!(y.contains("pref_beta: 0.1"));
        assert!(y.contains("pref_loss: sigmoid"));
    }

    #[test]
    fn method_selects_the_matching_dataset_file() {
        assert_eq!(TrainMethod::Sft.source_file(), "train.jsonl");
        assert_eq!(TrainMethod::Dpo.source_file(), "preference.jsonl");
        assert_eq!(TrainMethod::from_str_loose("DPO").unwrap(), TrainMethod::Dpo);
        assert!(TrainMethod::from_str_loose("ppo").is_err());
    }

    // ── config validation ──

    #[test]
    fn config_validation_rejects_out_of_range_knobs() {
        let base = cfg("dry_run", TrainMethod::Sft);
        assert!(base.validate().is_ok());
        assert!(JobConfig { lora_rank: 0, ..base.clone() }.validate().is_err());
        assert!(JobConfig { lora_rank: 1000, ..base.clone() }.validate().is_err());
        assert!(JobConfig { epochs: 0.0, ..base.clone() }.validate().is_err());
        assert!(JobConfig { learning_rate: 1.0, ..base.clone() }.validate().is_err());
        assert!(JobConfig { cutoff_len: 8, ..base.clone() }.validate().is_err());
        assert!(JobConfig { per_device_batch_size: 0, ..base.clone() }.validate().is_err());
        assert!(JobConfig { base_model: "  ".into(), ..base.clone() }.validate().is_err());
        assert!(JobConfig { dataset_id: "../x".into(), ..base }.validate().is_err());
    }

    #[test]
    fn shell_safety_rejects_every_injection_shape() {
        assert!(validate_shell_safe("host", "gpu.example.com").is_ok());
        assert!(validate_shell_safe("workdir", "/srv/train_1").is_ok());
        assert!(validate_shell_safe("model", "Qwen/Qwen3-4B-Instruct").is_ok());
        for bad in [
            "a;rm -rf /",
            "a$(id)",
            "a`id`",
            "a|b",
            "a b",
            "a&b",
            "a>b",
            "a\nb",
            "a'b",
            "a\"b",
            "-oProxyCommand=x",
            "",
        ] {
            assert!(validate_shell_safe("f", bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn remote_config_requires_absolute_workdir_and_clean_fields() {
        assert!(remote().validate().is_ok());
        assert!(RemoteGpuConfig { workdir: "relative/dir".into(), ..remote() }.validate().is_err());
        assert!(RemoteGpuConfig { host: "h; rm -rf /".into(), ..remote() }.validate().is_err());
        assert!(RemoteGpuConfig { user: "$(whoami)".into(), ..remote() }.validate().is_err());
    }

    #[test]
    fn llamafactory_cli_is_taken_from_the_venv_next_to_python() {
        assert_eq!(
            remote().llamafactory_cli(),
            "/opt/llamafactory/venv/bin/llamafactory-cli"
        );
        let bare = RemoteGpuConfig { python: "python3".into(), ..remote() };
        assert_eq!(bare.llamafactory_cli(), "llamafactory-cli");
    }

    #[test]
    fn ssh_argv_forces_batch_mode_so_a_prompt_cannot_hang_the_gateway() {
        let a = remote().ssh_argv();
        assert!(a.windows(2).any(|w| w[0] == "-o" && w[1] == "BatchMode=yes"));
        assert_eq!(a.last().unwrap(), "trainer@gpu.example.com");
        assert!(remote().rsync_transport().contains("BatchMode=yes"));
    }

    #[test]
    fn remote_scripts_target_the_job_directory_and_the_venv_cli() {
        let c = cfg("remote_gpu_ssh", TrainMethod::Sft);
        let launch = remote_launch_script(&c, &remote());
        assert!(launch.contains("d=/srv/duduclaw-train/job-test-1"));
        assert!(launch.contains("/opt/llamafactory/venv/bin/llamafactory-cli train train.yaml"));
        assert!(launch.contains("echo $! > train.pid"));
        let poll = remote_poll_script(&c, &remote());
        assert!(poll.contains("DUDUCLAW_ALIVE"));
        assert!(poll.contains("adapter_model.safetensors"));
        assert!(poll.contains("DUDUCLAW_LOG_BEGIN"));
        let conv = remote_convert_script(&c, &remote()).unwrap();
        assert!(conv.contains("/opt/llama.cpp/convert_lora_to_gguf.py"));
        assert!(conv.contains("DUDUCLAW_GGUF"));
        // No llama.cpp configured → no conversion attempted at all.
        let no_llama = RemoteGpuConfig { llama_cpp_dir: None, ..remote() };
        assert!(remote_convert_script(&c, &no_llama).is_none());
    }

    #[test]
    fn poll_output_parsing_reads_markers_not_log_text() {
        let st = parse_remote_status(
            "DUDUCLAW_ALIVE=yes\nDUDUCLAW_ADAPTER=no\nDUDUCLAW_LOG_BEGIN\n{'loss': 1.2}\n{'loss': 0.9}\n",
        );
        assert!(st.alive && !st.adapter);
        assert_eq!(st.log_tail, vec!["{'loss': 1.2}", "{'loss': 0.9}"]);

        let done = parse_remote_status("DUDUCLAW_ALIVE=no\nDUDUCLAW_ADAPTER=yes\nDUDUCLAW_LOG_BEGIN\ndone\n");
        assert!(!done.alive && done.adapter);

        // Garbage never reads as "finished successfully".
        let junk = parse_remote_status("connection reset");
        assert!(!junk.alive && !junk.adapter && junk.log_tail.is_empty());

        // A log line that happens to contain a marker word is still log text.
        let spoof = parse_remote_status("DUDUCLAW_ALIVE=no\nDUDUCLAW_LOG_BEGIN\nDUDUCLAW_ADAPTER=yes\n");
        assert!(!spoof.adapter);
    }

    // ── Together wire shapes (verified against the live OpenAPI) ──

    #[test]
    fn together_body_nests_lora_and_method_as_objects() {
        let mut c = cfg("together", TrainMethod::Sft);
        c.together = Some(TogetherConfig {
            model: "meta-llama/Meta-Llama-3.1-8B-Instruct-Reference".into(),
            suffix: Some("duduclaw".into()),
        });
        let b = together_create_body(&c, "file-abc");
        assert_eq!(b["model"], "meta-llama/Meta-Llama-3.1-8B-Instruct-Reference");
        assert_eq!(b["training_file"], "file-abc");
        assert_eq!(b["n_epochs"], 3);
        assert_eq!(b["suffix"], "duduclaw");
        // lora_r / lora_alpha are NOT top-level on the wire.
        assert!(b.get("lora_r").is_none());
        assert_eq!(b["training_type"]["type"], "Lora");
        assert_eq!(b["training_type"]["lora_r"], 16);
        assert_eq!(b["training_type"]["lora_alpha"], 32);
        // training_method is an object, not a string.
        assert_eq!(b["training_method"]["method"], "sft");
        assert_eq!(b["training_method"]["train_on_inputs"], "auto");

        let mut d = cfg("together", TrainMethod::Dpo);
        d.together = c.together.clone();
        let bd = together_create_body(&d, "file-abc");
        assert_eq!(bd["training_method"]["method"], "dpo");
        assert_eq!(bd["training_method"]["dpo_beta"], 0.1);
    }

    #[test]
    fn together_suffix_is_capped_at_the_documented_64_chars() {
        let mut c = cfg("together", TrainMethod::Sft);
        c.together = Some(TogetherConfig { model: "m".into(), suffix: Some("x".repeat(200)) });
        let b = together_create_body(&c, "f");
        assert_eq!(b["suffix"].as_str().unwrap().chars().count(), 64);
    }

    #[test]
    fn together_status_map_covers_all_nine_documented_values() {
        for (from, to) in [
            ("pending", "preparing"),
            ("queued", "preparing"),
            ("uploading", "preparing"),
            ("running", "running"),
            ("compressing", "running"),
            ("completed", "succeeded"),
            ("error", "failed"),
            ("cancelled", "cancelled"),
            ("cancel_requested", "cancelled"),
        ] {
            assert_eq!(map_together_status(from), to, "{from}");
        }
        // An unknown status is never optimistically read as success.
        assert_eq!(map_together_status("who_knows"), "running");
    }

    #[test]
    fn sharegpt_and_alpaca_convert_to_togethers_own_jsonl_shapes() {
        let sg = r#"{"conversations":[{"from":"human","value":"問"},{"from":"gpt","value":"答"}],"system":"你是助理"}"#;
        let out = to_together_jsonl(sg, TrainMethod::Sft).unwrap();
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        let m = v["messages"].as_array().unwrap();
        assert_eq!(m[0]["role"], "system");
        assert_eq!(m[1]["role"], "user");
        assert_eq!(m[2]["role"], "assistant");

        let al = r#"{"instruction":"對帳","input":"八月","output":"完成","history":[["前問","前答"]]}"#;
        let out = to_together_jsonl(al, TrainMethod::Sft).unwrap();
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        let m = v["messages"].as_array().unwrap();
        assert_eq!(m[0]["content"], "前問");
        assert_eq!(m[2]["content"], "對帳\n\n八月");
        assert_eq!(m[3]["content"], "完成");

        let dpo = r#"{"conversations":[{"from":"human","value":"對帳"}],"chosen":{"from":"gpt","value":"好"},"rejected":{"from":"gpt","value":"壞"}}"#;
        let out = to_together_jsonl(dpo, TrainMethod::Dpo).unwrap();
        let v: Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["input"]["messages"][0]["content"], "對帳");
        assert_eq!(v["preferred_output"][0]["content"], "好");
        assert_eq!(v["non_preferred_output"][0]["content"], "壞");
        assert_eq!(v["preferred_output"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn conversion_refuses_rather_than_uploading_an_empty_file() {
        assert!(to_together_jsonl("", TrainMethod::Sft).is_err());
        assert!(to_together_jsonl("not json\n", TrainMethod::Sft).is_err());
        // A conversation with no assistant turn cannot supervise anything.
        let no_answer = r#"{"conversations":[{"from":"human","value":"問"}]}"#;
        assert!(to_together_jsonl(no_answer, TrainMethod::Sft).is_err());
    }

    // ── privacy gate + dry run, end to end ──

    fn seeded_home() -> (tempfile::TempDir, String) {
        let home = tempfile::tempdir().unwrap();
        let meta = dataset::create(home.path(), "測試資料集", "sharegpt").unwrap();
        // Build against empty stores, then drop two real rows in so the dry
        // run has something to count.
        dataset::build(
            home.path(),
            &meta.id,
            dataset::DatasetSources::default(),
            "sharegpt",
            true,
        )
        .unwrap();
        let dir = crate::finetune::dataset_dir(home.path(), &meta.id).unwrap();
        std::fs::write(
            dir.join("train.jsonl"),
            "{\"conversations\":[{\"from\":\"human\",\"value\":\"a\"},{\"from\":\"gpt\",\"value\":\"b\"}]}\n",
        )
        .unwrap();
        let id = meta.id;
        (home, id)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_job_without_acknowledgement_is_refused_before_anything_happens() {
        let (home, ds) = seeded_home();
        let mut c = cfg("remote_gpu_ssh", TrainMethod::Sft);
        c.id = String::new();
        c.dataset_id = ds;
        // Deliberately unreachable host: the gate must fire first, so this
        // test cannot touch the network even if the gate regressed to a
        // warning.
        c.remote = Some(RemoteGpuConfig {
            host: "gpu.invalid".into(),
            key_path: "/nonexistent/key".into(),
            ..remote()
        });
        let err = create(home.path(), c, false).await.unwrap_err();
        assert_eq!(err.code(), "data_leaves_device_not_acknowledged");
        // Nothing was written.
        assert!(list(home.path()).unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn together_job_is_gated_too() {
        let (home, ds) = seeded_home();
        let mut c = cfg("together", TrainMethod::Sft);
        c.id = String::new();
        c.dataset_id = ds;
        c.together = Some(TogetherConfig { model: "m".into(), suffix: None });
        let err = create(home.path(), c, false).await.unwrap_err();
        assert_eq!(err.code(), "data_leaves_device_not_acknowledged");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dry_run_needs_no_acknowledgement_because_nothing_leaves() {
        let (home, ds) = seeded_home();
        let mut c = cfg("dry_run", TrainMethod::Sft);
        c.id = String::new();
        c.dataset_id = ds;
        c.remote = Some(remote());
        let rec = create(home.path(), c, false).await.unwrap();

        assert_eq!(rec.state, "planned");
        assert!(rec.detail.as_deref().unwrap().contains("沒有進行訓練"));
        let dir = job_dir(home.path(), &rec.id).unwrap();
        let yaml = std::fs::read_to_string(dir.join("train.yaml")).unwrap();
        assert!(yaml.contains("stage: sft"));
        assert!(yaml.contains("dataset_dir: /srv/duduclaw-train/"));
        let plan: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("plan.json")).unwrap()).unwrap();
        assert_eq!(plan["trained"], false);
        assert_eq!(plan["dataset_rows"], 1);
        assert!(plan["command"].as_str().unwrap().contains("llamafactory-cli train train.yaml"));

        // Polling a dry run never invents a finished state.
        let after = status(home.path(), &rec.id).await.unwrap();
        assert_eq!(after.state, "planned");

        assert_eq!(list(home.path()).unwrap().len(), 1);
        let cancelled = cancel(home.path(), &rec.id).await.unwrap();
        assert_eq!(cancelled.state, "cancelled");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dry_run_reports_an_unbuilt_preference_file_instead_of_pretending() {
        let (home, ds) = seeded_home();
        let mut c = cfg("dry_run", TrainMethod::Dpo);
        c.id = String::new();
        c.dataset_id = ds.clone();
        // The seeded dataset has an empty preference.jsonl.
        let rec = create(home.path(), c, false).await.unwrap();
        assert_eq!(rec.state, "planned");
        assert!(rec.detail.as_deref().unwrap().contains("沒有任何資料列"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_refuses_an_unknown_backend_or_unbuilt_dataset() {
        let (home, ds) = seeded_home();
        let mut bad = cfg("magic_cloud", TrainMethod::Sft);
        bad.dataset_id = ds.clone();
        assert_eq!(
            create(home.path(), bad, true).await.unwrap_err().code(),
            "bad_request"
        );

        let mut missing = cfg("dry_run", TrainMethod::Sft);
        missing.id = String::new();
        missing.dataset_id = "ds-does-not-exist".into();
        assert_eq!(
            create(home.path(), missing, true).await.unwrap_err().code(),
            "not_found"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn status_and_cancel_report_not_found_for_unknown_jobs() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(status(home.path(), "job-nope").await.unwrap_err().code(), "not_found");
        assert_eq!(cancel(home.path(), "job-nope").await.unwrap_err().code(), "not_found");
        assert!(list(home.path()).unwrap().is_empty());
    }

    #[test]
    fn backend_registry_is_closed() {
        assert_eq!(backend_for("dry_run").unwrap().id(), "dry_run");
        assert_eq!(backend_for("remote_gpu_ssh").unwrap().id(), "remote_gpu_ssh");
        assert_eq!(backend_for("together").unwrap().id(), "together");
        assert!(backend_for("unsloth_cloud").is_err());
        // Only dry_run keeps data on the box.
        assert!(!backend_for("dry_run").unwrap().is_remote());
        assert!(backend_for("remote_gpu_ssh").unwrap().is_remote());
        assert!(backend_for("together").unwrap().is_remote());
    }

    #[test]
    fn ssh_backend_validation_needs_a_real_key_file() {
        let mut c = cfg("remote_gpu_ssh", TrainMethod::Sft);
        c.remote = Some(remote());
        // The fixture key path does not exist on this machine.
        assert!(RemoteGpuSshBackend.validate(&c).is_err());
        let key = tempfile::NamedTempFile::new().unwrap();
        c.remote = Some(RemoteGpuConfig {
            key_path: key.path().display().to_string(),
            ..remote()
        });
        // A tempfile path contains only safe characters on every supported
        // platform, so this exercises the happy path.
        if validate_shell_safe("key_path", &key.path().display().to_string()).is_ok() {
            assert!(RemoteGpuSshBackend.validate(&c).is_ok());
        }
        // Missing remote block at all.
        let mut none = cfg("remote_gpu_ssh", TrainMethod::Sft);
        none.remote = None;
        assert!(RemoteGpuSshBackend.validate(&none).is_err());
    }
}
