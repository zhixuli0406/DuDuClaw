//! Background generation jobs: progress record on disk + in-process registry.
//!
//! A staged generation runs 3–10 model calls, longer than one RPC should
//! stay open, so `experts.generate` can start it in the background and the
//! dashboard polls `experts.generate_status`. The record lives in
//! `<draft>/job.json` (atomic temp + rename); the registry holds the abort
//! handle so `experts.generate_cancel` can stop the model calls. A record
//! that says `running` while the registry has no entry belonged to a gateway
//! process that is gone, and is reported as interrupted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    /// Every stage succeeded.
    Done,
    /// Draft written, but some members or the knowledge base fell back.
    Partial,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, JobStatus::Running)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemberProgress {
    pub name: String,
    pub display_name: String,
    pub status: StepStatus,
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobProgress {
    pub roster: StepStatus,
    #[serde(default)]
    pub members: Vec<MemberProgress>,
    pub wiki: StepStatus,
    /// Materialize + validate the draft pack.
    pub assemble: StepStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub status: JobStatus,
    /// `generate` or `revise`.
    pub kind: String,
    pub progress: JobProgress,
    #[serde(default)]
    pub error: String,
    /// Partial-failure notes (members / wiki that fell back).
    #[serde(default)]
    pub notes: Vec<String>,
    pub started_at: String,
    pub updated_at: String,
}

impl JobRecord {
    pub fn new(kind: &str) -> Self {
        let now = crate::expert_admin::now_iso();
        Self {
            status: JobStatus::Running,
            kind: kind.to_string(),
            progress: JobProgress::default(),
            error: String::new(),
            notes: Vec::new(),
            started_at: now.clone(),
            updated_at: now,
        }
    }
}

pub fn job_path(draft_dir: &Path) -> PathBuf {
    draft_dir.join("job.json")
}

pub fn write_job(draft_dir: &Path, rec: &JobRecord) -> Result<(), String> {
    std::fs::create_dir_all(draft_dir).map_err(|e| format!("建立草稿目錄失敗: {e}"))?;
    // Unique temp name: two writers never share (and clobber) one temp file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = draft_dir.join(format!("job.json.{}-{seq}.tmp", std::process::id()));
    let body = serde_json::to_string_pretty(rec).map_err(|e| format!("序列化進度失敗: {e}"))?;
    std::fs::write(&tmp, body).map_err(|e| format!("寫入進度失敗: {e}"))?;
    std::fs::rename(&tmp, job_path(draft_dir)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("寫入進度失敗: {e}")
    })
}

pub fn read_job(draft_dir: &Path) -> Option<JobRecord> {
    let raw = std::fs::read_to_string(job_path(draft_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Shared, persisted progress for one running job.
pub struct ProgressSink {
    draft_dir: PathBuf,
    record: Mutex<JobRecord>,
}

impl ProgressSink {
    pub fn new(draft_dir: PathBuf, record: JobRecord) -> Self {
        Self {
            draft_dir,
            record: Mutex::new(record),
        }
    }

    /// A sink that never touches disk (unit tests).
    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::new(PathBuf::new(), JobRecord::new("generate"))
    }

    /// Apply `f` to the record and persist it (best-effort: a failed write
    /// is logged, never fatal — progress is a display aid, not state the
    /// draft depends on).
    pub fn update(&self, f: impl FnOnce(&mut JobRecord)) {
        let snapshot = {
            let mut guard = match self.record.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            f(&mut guard);
            guard.updated_at = crate::expert_admin::now_iso();
            guard.clone()
        };
        if self.draft_dir.as_os_str().is_empty() {
            return;
        }
        if let Err(e) = write_job(&self.draft_dir, &snapshot) {
            tracing::warn!(error = %e, "expert generation: progress write failed");
        }
    }

    pub fn snapshot(&self) -> JobRecord {
        match self.record.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn set_member(&self, idx: usize, status: StepStatus, error: &str) {
        self.update(|r| {
            if let Some(m) = r.progress.members.get_mut(idx) {
                m.status = status;
                m.error = duduclaw_core::truncate_chars(error, 200);
            }
        });
    }
}

// ─────────────────────────── Registry ───────────────────────────

/// Jobs (generation or install) allowed at once across all drafts — a cost
/// breaker: one generation is up to ~10 model calls.
pub const MAX_CONCURRENT_JOBS: usize = if cfg!(test) { 64 } else { 3 };

struct Entry {
    token: u64,
    handle: Option<tokio::task::AbortHandle>,
    cancelled: bool,
}

fn registry() -> &'static Mutex<HashMap<String, Entry>> {
    static REG: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_registry<T>(f: impl FnOnce(&mut HashMap<String, Entry>) -> T) -> T {
    let mut guard = match registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut guard)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveError {
    /// This draft already has a job.
    Busy,
    /// [`MAX_CONCURRENT_JOBS`] reached.
    TooMany,
}

impl ReserveError {
    pub fn message(self) -> &'static str {
        match self {
            ReserveError::Busy => "這份草稿正在生成或安裝中，請等目前這一輪完成",
            ReserveError::TooMany => "同時進行的團隊生成太多，請等其他生成完成後再試",
        }
    }
}

/// The reservation of one draft's slot. Dropping it — normal return, task
/// abort (tokio drops the future) or panic unwinding — frees the slot, and
/// if the job record on disk still says `running`, finalizes it as
/// `cancelled` (after [`abort`]) or `failed`. Only the job that holds the
/// slot ever writes a terminal status, so cancel and completion cannot race.
pub struct Slot {
    draft_id: String,
    token: u64,
    draft_dir: Option<PathBuf>,
}

impl Slot {
    /// Finalize `<draft_dir>/job.json` on drop (generation jobs only).
    pub fn finalize_record_in(mut self, draft_dir: PathBuf) -> Self {
        self.draft_dir = Some(draft_dir);
        self
    }

    /// Hand the task's abort handle to the registry. A cancel that arrived
    /// before this point aborts the task right away.
    pub fn attach(&self, handle: tokio::task::AbortHandle) {
        with_registry(|m| match m.get_mut(&self.draft_id) {
            Some(e) if e.token == self.token => {
                if e.cancelled {
                    handle.abort();
                }
                e.handle = Some(handle);
            }
            _ => handle.abort(),
        });
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let cancelled = with_registry(|m| match m.get(&self.draft_id) {
            Some(e) if e.token == self.token => m.remove(&self.draft_id).is_some_and(|e| e.cancelled),
            _ => false,
        });
        let Some(dir) = &self.draft_dir else { return };
        let Some(rec) = read_job(dir) else { return };
        if rec.status != JobStatus::Running {
            return;
        }
        let rec = if cancelled {
            JobRecord {
                status: JobStatus::Cancelled,
                updated_at: crate::expert_admin::now_iso(),
                ..rec
            }
        } else {
            JobRecord {
                status: JobStatus::Failed,
                error: "生成意外中止，請重新生成".to_string(),
                updated_at: crate::expert_admin::now_iso(),
                ..rec
            }
        };
        if let Err(e) = write_job(dir, &rec) {
            tracing::warn!(error = %e, "expert generation: could not finalize job record");
        }
    }
}

/// Reserve the slot for `draft_id`.
pub fn reserve(draft_id: &str) -> Result<Slot, ReserveError> {
    static TOKENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    with_registry(|m| {
        if m.contains_key(draft_id) {
            return Err(ReserveError::Busy);
        }
        if m.len() >= MAX_CONCURRENT_JOBS {
            return Err(ReserveError::TooMany);
        }
        let token = TOKENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        m.insert(
            draft_id.to_string(),
            Entry {
                token,
                handle: None,
                cancelled: false,
            },
        );
        Ok(Slot {
            draft_id: draft_id.to_string(),
            token,
            draft_dir: None,
        })
    })
}

pub fn is_registered(draft_id: &str) -> bool {
    with_registry(|m| m.contains_key(draft_id))
}

/// Request cancellation of a running job. `false` ⇒ nothing was running in
/// this process. The job's [`Slot`] records the `cancelled` status when the
/// aborted task is dropped.
pub fn abort(draft_id: &str) -> bool {
    with_registry(|m| match m.get_mut(draft_id) {
        Some(e) => {
            e.cancelled = true;
            if let Some(h) = &e.handle {
                h.abort();
            }
            true
        }
        None => false,
    })
}

/// Read a job record, converting an orphaned `running` record (its process
/// is gone) into `failed` so the dashboard stops waiting.
pub fn effective_job(draft_dir: &Path, draft_id: &str) -> Option<JobRecord> {
    let rec = read_job(draft_dir)?;
    if rec.status == JobStatus::Running && !is_registered(draft_id) {
        return Some(JobRecord {
            status: JobStatus::Failed,
            error: "生成中斷（服務曾重新啟動），請重新生成".to_string(),
            ..rec
        });
    }
    Some(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_reserve_is_exclusive_and_drop_frees() {
        let id = format!("test-{}", uuid::Uuid::new_v4());
        let slot = reserve(&id).unwrap();
        assert_eq!(reserve(&id).err(), Some(ReserveError::Busy));
        assert!(is_registered(&id));
        drop(slot);
        assert!(!is_registered(&id));
        assert!(!abort(&id), "nothing to abort after release");
    }

    #[test]
    fn dropped_slot_finalizes_a_running_record() {
        let dir = tempfile::tempdir().unwrap();
        let id = format!("test-{}", uuid::Uuid::new_v4());
        write_job(dir.path(), &JobRecord::new("generate")).unwrap();
        let slot = reserve(&id).unwrap().finalize_record_in(dir.path().to_path_buf());
        assert!(abort(&id));
        drop(slot);
        assert_eq!(read_job(dir.path()).unwrap().status, JobStatus::Cancelled);

        // Not cancelled (e.g. a panic) ⇒ failed; a finished record is kept.
        write_job(dir.path(), &JobRecord::new("revise")).unwrap();
        drop(reserve(&id).unwrap().finalize_record_in(dir.path().to_path_buf()));
        assert_eq!(read_job(dir.path()).unwrap().status, JobStatus::Failed);
        let done = JobRecord { status: JobStatus::Done, ..JobRecord::new("revise") };
        write_job(dir.path(), &done).unwrap();
        drop(reserve(&id).unwrap().finalize_record_in(dir.path().to_path_buf()));
        assert_eq!(read_job(dir.path()).unwrap().status, JobStatus::Done);
    }

    #[tokio::test]
    async fn cancel_before_attach_still_aborts_the_task() {
        let id = format!("test-{}", uuid::Uuid::new_v4());
        let slot = reserve(&id).unwrap();
        assert!(abort(&id));
        let task = tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(30)).await });
        slot.attach(task.abort_handle());
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn orphaned_running_record_reads_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let id = format!("test-{}", uuid::Uuid::new_v4());
        write_job(dir.path(), &JobRecord::new("generate")).unwrap();
        let rec = effective_job(dir.path(), &id).unwrap();
        assert_eq!(rec.status, JobStatus::Failed);
        assert!(rec.error.contains("中斷"));
        // While registered it still reads as running.
        let slot = reserve(&id).unwrap();
        assert_eq!(effective_job(dir.path(), &id).unwrap().status, JobStatus::Running);
        drop(slot);
    }

    #[test]
    fn sink_persists_member_progress() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = JobRecord::new("generate");
        rec.progress.members.push(MemberProgress {
            name: "a".into(),
            display_name: "A".into(),
            ..Default::default()
        });
        let sink = ProgressSink::new(dir.path().to_path_buf(), rec);
        sink.set_member(0, StepStatus::Failed, "boom");
        let on_disk = read_job(dir.path()).unwrap();
        assert_eq!(on_disk.progress.members[0].status, StepStatus::Failed);
        assert_eq!(on_disk.progress.members[0].error, "boom");
    }
}
