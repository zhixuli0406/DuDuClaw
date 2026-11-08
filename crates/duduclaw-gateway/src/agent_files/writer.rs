//! The single writer for every human change to a profile file.
//!
//! Inside one cross-process lock on the file: re-read, compare the caller's
//! `base_hash` (optimistic concurrency), re-check the caller's permission
//! (the AI review may have taken seconds, and a binding can be revoked in
//! between), render the new text from the bytes just read, validate the
//! artifact that will take effect, keep the version, replace the file
//! owner-only and atomically, and update the SOUL / IDENTITY / USER / INSTRUCTIONS baseline.
//! After the lock: audit, registry rescan, channel notice, config event.

use std::path::Path;
use std::sync::Arc;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_security::audit::Severity;
use duduclaw_security::soul_guard;
use serde_json::json;
use tokio::sync::RwLock;
use tracing::warn;

use super::history::{self, VersionMeta};
use super::verdict::Verdict;
use super::{AgentFilesConfig, FileKey};

/// What happens to the stored SOUL / IDENTITY / USER / INSTRUCTIONS baseline after the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Baseline {
    /// The person reviewed the whole content: it becomes the baseline.
    Accept,
    /// An automatic edit (rename sync): the baseline follows only when the
    /// content before the write was the approved one, so drift stays drift.
    AcceptIfWasIntact,
    /// The person checked the file exactly as it is now (it had been changed
    /// outside the dashboard) and accepts it: nothing is rewritten, but the
    /// content is kept as a version and becomes the baseline.
    AcceptCurrent,
}

/// Who and why, for the version index and the audit row.
#[derive(Debug, Clone)]
pub(crate) struct CommitMeta {
    pub user_id: String,
    pub source: &'static str,
    pub verdict: Option<Verdict>,
    pub basis: Option<&'static str>,
    pub reason_categories: Vec<String>,
    pub confirm_reason: Option<String>,
    pub approval_id: Option<String>,
    pub decided_by: Option<String>,
    pub severity: Severity,
}

impl CommitMeta {
    pub(crate) fn new(user_id: &str, source: &'static str) -> Self {
        Self {
            user_id: user_id.to_string(),
            source,
            verdict: None,
            basis: None,
            reason_categories: Vec::new(),
            confirm_reason: None,
            approval_id: None,
            decided_by: None,
            severity: Severity::Info,
        }
    }
}

/// One write.
pub(crate) struct Commit<'a> {
    pub home: &'a Path,
    pub agent_id: &'a str,
    pub key: FileKey,
    /// `Some` = the caller's `base_hash`; `None` = render from whatever is
    /// current (internal edits whose mutation runs under the lock).
    pub expected_hash: Option<&'a str>,
    pub meta: CommitMeta,
    pub baseline: Baseline,
}

/// What a write did.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub before: String,
    pub after: String,
    pub before_hash: String,
    pub hash: String,
    pub version_id: Option<String>,
    pub unchanged: bool,
    pub registry_reloaded: bool,
}

/// Why a write did not happen.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CommitError {
    /// The file changed since the caller read it.
    Conflict { current_hash: String },
    /// Refused: no permission (any more), invalid content, product policy.
    Refused(String),
    /// IO and other failures.
    Failed(String),
}

impl CommitError {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Conflict { .. } => "這個檔案在你編輯期間被其他人修改了，請重新載入後再試".into(),
            Self::Refused(m) | Self::Failed(m) => m.clone(),
        }
    }
}

enum Locked {
    Done(Outcome),
    Err(CommitError),
}

/// The locked section (synchronous; see the module doc).
pub(crate) fn commit_locked(
    c: &Commit<'_>,
    render: impl FnOnce(&str) -> Result<String, String>,
    guard: impl FnOnce() -> Result<(), String>,
) -> Result<Outcome, CommitError> {
    commit_locked_with_lifecycle(c, render, guard, false)
}

/// A stop request can require a barrier even if its status was already set.
pub(crate) fn commit_locked_with_lifecycle(
    c: &Commit<'_>,
    render: impl FnOnce(&str) -> Result<String, String>,
    guard: impl FnOnce() -> Result<(), String>,
    force_lifecycle_barrier: bool,
) -> Result<Outcome, CommitError> {
    commit_locked_inner(c, render, guard, force_lifecycle_barrier, None)
}

/// Marker carried by every [`CommitError::Failed`] that means "the file lock
/// was not acquired within the caller's bound — nothing was written".
pub(crate) const LOCK_WAIT_TIMEOUT: &str = "lock_wait_timeout";

/// Does `message` (a [`CommitError::message`]) report a bounded lock wait
/// that ran out (nothing written)?
pub(crate) fn is_lock_wait_timeout(message: &str) -> bool {
    message.contains(LOCK_WAIT_TIMEOUT)
}

fn lock_timeout_error(c: &Commit<'_>, wait: std::time::Duration) -> CommitError {
    CommitError::Failed(format!(
        "{} 正被其他寫入佔用，等待 {} 秒後仍無法取得寫入鎖（{LOCK_WAIT_TIMEOUT}），沒有寫入",
        c.key.file_name(),
        wait.as_secs_f32()
    ))
}

/// The locked section. `lock_wait`: `None` = wait for the file lock as long
/// as it takes; `Some(d)` = give up after `d` (RM-027 round 5 — for a caller
/// that holds the global org lock and must never wait unboundedly).
fn commit_locked_inner(
    c: &Commit<'_>,
    render: impl FnOnce(&str) -> Result<String, String>,
    guard: impl FnOnce() -> Result<(), String>,
    force_lifecycle_barrier: bool,
    lock_wait: Option<std::time::Duration>,
) -> Result<Outcome, CommitError> {
    let path = super::file_path(c.home, c.agent_id, &c.key).map_err(CommitError::Refused)?;
    let keep = AgentFilesConfig::load(c.home).history_keep;
    let anchor = lock_anchor(c, &path);
    let locked_section = || -> std::io::Result<Locked> {
        // A skill is never created here, and its folder is writable by the
        // employee: bind it again under the lock (same existing regular
        // file, same folders) and check that binding once more right before
        // the replace.
        let bound = match skill_binding(c) {
            Ok(b) if b.as_ref().is_none_or(|b| b.path() == path) => b,
            Ok(_) => return Ok(Locked::Err(CommitError::Refused("skill file not found".into()))),
            Err(e) => return Ok(Locked::Err(CommitError::Refused(e))),
        };
        let (current, existed) = match std::fs::read_to_string(&path) {
            Ok(s) => (s, true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !c.key.is_skill() => (String::new(), false),
            Err(e) => return Ok(Locked::Err(CommitError::Failed(format!("cannot read {}: {e}", c.key.file_name())))),
        };
        let before_hash = super::hash_of(&current);
        if let Some(expected) = c.expected_hash
            && expected != before_hash
        {
            return Ok(Locked::Err(CommitError::Conflict { current_hash: before_hash }));
        }
        let new = match render(&current) {
            Ok(n) => n,
            Err(e) => return Ok(Locked::Err(CommitError::Refused(e))),
        };
        let new = if c.key == FileKey::User {
            if new.len() > c.key.max_bytes() || new.chars().count() > super::MAX_USER_CHARS {
                return Ok(Locked::Err(CommitError::Refused("服務對象介紹超過大小限制".into())));
            }
            match duduclaw_security::credential_detect::mask_document(&new) {
                Ok(masked) => masked,
                Err(_) => return Ok(Locked::Err(CommitError::Refused("憑證掃描失敗，未寫入 USER.md".into()))),
            }
        } else { new };
        // Compare fresh bytes under the file lock, before any history or write.
        // The lease also covers external MCP edits and raw profile restoration.
        let _lifecycle = if c.key == FileKey::AgentToml
            && (force_lifecycle_barrier
                || duduclaw_core::autonomy_gate::lifecycle_barrier_required(&current, &new))
        {
            Some(duduclaw_core::autonomy_gate::exclusive_barrier(c.home)?)
        } else {
            None
        };
        // Recheck authority after a potentially long admission drain.
        if let Err(e) = guard() {
            return Ok(Locked::Err(CommitError::Refused(e)));
        }
        let hash = super::hash_of(&new);
        let accept_current = new == current && c.baseline == Baseline::AcceptCurrent && c.key.protected_doc().is_some();
        if existed && new == current && !accept_current {
            return Ok(Locked::Done(Outcome {
                before: current,
                after: new,
                before_hash,
                hash,
                version_id: None,
                unchanged: true,
                registry_reloaded: true,
            }));
        }
        if c.key == FileKey::User && new.chars().count() > super::MAX_USER_CHARS {
            return Ok(Locked::Err(CommitError::Refused("服務對象介紹最多 4,000 個字".into())));
        }
        if new.len() > c.key.max_bytes() {
            return Ok(Locked::Err(CommitError::Refused(format!("內容太長：最多 {} KB", c.key.max_bytes() / 1024))));
        }
        if c.key == FileKey::AgentToml
            && let Err(e) = crate::product_policy::validate_write(c.home, &path, &current, &new)
        {
            return Ok(Locked::Err(CommitError::Refused(e)));
        }
        if c.key == FileKey::AgentToml {
            let table = match new.parse::<toml::Table>() {
                Ok(table) => table,
                Err(_) => return Ok(Locked::Err(CommitError::Refused("invalid agent.toml; connection ownership cannot be verified".into()))),
            };
            if let Err(e) = crate::disconnect_forget::connections::mark_config_connecting(c.home, &table, Some(c.agent_id)) {
                return Ok(Locked::Err(CommitError::Refused(e)));
            }
        }
        // The version first: a change that cannot be undone is not written.
        let meta = VersionMeta {
            user_id: c.meta.user_id.clone(),
            source: c.meta.source.to_string(),
            verdict: c.meta.verdict,
            reason_categories: c.meta.reason_categories.clone(),
            confirm_reason: c.meta.confirm_reason.clone(),
            approval_id: c.meta.approval_id.clone(),
            decided_by: c.meta.decided_by.clone(),
        };
        let version_id = match history::ensure_initial(c.home, c.agent_id, &c.key, &current, keep)
            .and_then(|_| history::record(c.home, c.agent_id, &c.key, &new, &meta, keep))
        {
            Ok(id) => id,
            Err(e) => return Ok(Locked::Err(CommitError::Failed(e))),
        };
        if !accept_current {
            if bound.is_some() && skill_binding(c).ok() != Some(bound.clone()) {
                return Ok(Locked::Err(CommitError::Refused("the skill folder changed while saving; nothing was written".into())));
            }
            crate::handlers::config_commit::replace_owner_only(&path, &new)?;
        }
        update_baseline(c, &before_hash);
        Ok(Locked::Done(Outcome {
            before: current,
            after: new,
            before_hash,
            hash,
            version_id: Some(version_id),
            unchanged: false,
            registry_reloaded: false,
        }))
    };
    let result = match lock_wait {
        None => duduclaw_core::with_file_lock(&anchor, locked_section),
        Some(wait) => match duduclaw_core::with_file_lock_timeout(&anchor, wait, locked_section) {
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(lock_timeout_error(c, wait)),
            other => other,
        },
    };
    match result {
        Ok(Locked::Done(o)) => {
            if !o.unchanged {
                audit_changed(c, &o);
            }
            Ok(o)
        }
        Ok(Locked::Err(e)) => Err(e),
        Err(e) => Err(CommitError::Failed(format!("cannot write {}: {e}", c.key.file_name()))),
    }
}

/// The advisory lock file's anchor. A skill's folder is writable by the
/// employee, so its lock lives outside, next to its versions
/// (`agent_file_history/<agent>/locks/<skill dir>.lock`). Writers that do
/// not use this module (the custom-skill flow, the employee's own skill
/// tools) do not take this lock.
fn lock_anchor(c: &Commit<'_>, path: &Path) -> std::path::PathBuf {
    if c.key.is_skill() {
        return c.home.join("agent_file_history").join(c.agent_id).join("locks").join(c.key.history_dir());
    }
    path.to_path_buf()
}

/// `Some(binding)` for a skill, `None` for every other file.
fn skill_binding(c: &Commit<'_>) -> Result<Option<super::skills::Binding>, String> {
    let FileKey::Skill(p) = &c.key else { return Ok(None) };
    super::skills::bind(&super::agent_dir(c.home, c.agent_id)?, p).map(Some)
}

/// Still under the file lock, so nobody can slip a write in between.
fn update_baseline(c: &Commit<'_>, before_hash: &str) {
    let Some(doc) = c.key.protected_doc() else { return };
    let Ok(dir) = super::agent_dir(c.home, c.agent_id) else { return };
    let result = match c.baseline {
        Baseline::Accept | Baseline::AcceptCurrent => soul_guard::accept_doc_change(doc, c.agent_id, &dir).map(|_| true),
        Baseline::AcceptIfWasIntact => soul_guard::accept_if_was_intact(doc, c.agent_id, &dir, before_hash),
    };
    if let Err(e) = result {
        warn!(agent = c.agent_id, file = c.key.file_name(), "baseline update failed after a write: {e}");
    }
}

fn audit_changed(c: &Commit<'_>, o: &Outcome) {
    let hunks = super::diff::line_diff(&o.before, &o.after);
    let (added, removed) = super::diff::counts(&hunks);
    crate::security_autopilot::audit_and_emit(
        c.home,
        &duduclaw_security::audit::AuditEvent::new(
            "agent_file_changed",
            c.agent_id,
            c.meta.severity.clone(),
            json!({
                "agent_id": c.agent_id,
                "file": c.key.as_str(),
                "user_id": c.meta.user_id,
                "source": c.meta.source,
                "before_sha": o.before_hash,
                "after_sha": o.hash,
                "size_before": o.before.len(),
                "size_after": o.after.len(),
                "lines_added": added,
                "lines_removed": removed,
                "verdict": c.meta.verdict.map(Verdict::as_str),
                "basis": c.meta.basis,
                "reason_categories": c.meta.reason_categories,
                "confirm_reason": c.meta.confirm_reason,
                "approval_id": c.meta.approval_id,
                "decided_by": c.meta.decided_by,
                "version_id": o.version_id,
            }),
        ),
    );
}

/// Re-scan the registry so the next turn sees the change. RELIABLE, not
/// best-effort: the gateway has no periodic rescan, so a skipped one leaves
/// every consumer on the old config until a restart. Retries transient scan
/// errors inline before giving up.
pub(crate) async fn rescan_registry_reliably(registry: &Arc<RwLock<AgentRegistry>>, agent_id: &str) -> bool {
    for attempt in 1..=3u32 {
        let mut reg = registry.write().await;
        match reg.scan().await {
            Ok(()) => return true,
            Err(e) => {
                warn!(agent_id, attempt, error = %e, "registry rescan failed after a profile file write — retrying");
                drop(reg);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    warn!(agent_id, "registry rescan failed 3× — the change is saved but takes effect after the next successful scan");
    false
}

/// Run the blocking part (the advisory file lock, `users.db`, file IO) off
/// the async worker when the runtime allows it (`block_in_place` on a
/// multi-threaded runtime); on a current-thread runtime it runs inline. The
/// closures borrow from the caller, so `spawn_blocking` (which needs
/// `'static`) is not an option here.
pub(crate) fn run_blocking<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// The whole write: per-file in-process lock, [`commit_locked`], then the
/// registry rescan and the channel notice.
pub(crate) async fn commit(
    registry: &Arc<RwLock<AgentRegistry>>,
    c: Commit<'_>,
    render: impl FnOnce(&str) -> Result<String, String>,
    guard: impl FnOnce() -> Result<(), String>,
) -> Result<Outcome, CommitError> {
    commit_within(registry, c, render, guard, None).await
}

/// [`commit`] with an optional hard bound on **both** lock waits (the
/// in-process per-file mutex and the cross-process advisory file lock, one
/// shared budget): `Some(d)` gives up after `d` with a [`LOCK_WAIT_TIMEOUT`]
/// error and nothing written; `None` waits as long as it takes.
/// RM-027 round 5: every `agent.toml` write made while the global org lock
/// is held passes `Some`, so a stuck writer can never pin the org lock.
pub(crate) async fn commit_within(
    registry: &Arc<RwLock<AgentRegistry>>,
    c: Commit<'_>,
    render: impl FnOnce(&str) -> Result<String, String>,
    guard: impl FnOnce() -> Result<(), String>,
    lock_wait: Option<std::time::Duration>,
) -> Result<Outcome, CommitError> {
    let lock = super::file_mutex(c.home, c.agent_id, &c.key);
    let started = std::time::Instant::now();
    let _held = match lock_wait {
        None => lock.lock().await,
        Some(wait) => tokio::time::timeout(wait, lock.lock())
            .await
            .map_err(|_| lock_timeout_error(&c, wait))?,
    };
    // The file lock gets what is left of the same budget (one bound total).
    let file_wait = lock_wait.map(|wait| wait.saturating_sub(started.elapsed()));
    let mut out = run_blocking(|| match file_wait {
        None => commit_locked(&c, render, guard),
        Some(wait) => commit_locked_inner(&c, render, guard, false, Some(wait)),
    })?;
    if out.unchanged {
        return Ok(out);
    }
    out.registry_reloaded = rescan_registry_reliably(registry, c.agent_id).await;
    match c.key {
        FileKey::Contract => crate::pending_agent_notice::mark_contract_changed(c.home, c.agent_id),
        FileKey::AgentToml => {}
        _ => crate::pending_agent_notice::mark_profile_changed(c.home, c.agent_id),
    }
    crate::security_autopilot::emit_config_changed();
    Ok(out)
}
