//! The single writer for every human change to a profile file.
//!
//! Inside one cross-process lock on the file: re-read, compare the caller's
//! `base_hash` (optimistic concurrency), re-check the caller's permission
//! (the AI review may have taken seconds, and a binding can be revoked in
//! between), render the new text from the bytes just read, validate the
//! artifact that will take effect, keep the version, replace the file
//! owner-only and atomically, and update the SOUL / INSTRUCTIONS baseline.
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

/// What happens to the stored SOUL / INSTRUCTIONS baseline after the write.
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
    let path = super::file_path(c.home, c.agent_id, &c.key).map_err(CommitError::Refused)?;
    let keep = AgentFilesConfig::load(c.home).history_keep;
    let result = duduclaw_core::with_file_lock(&path, || {
        // A skill is never created here, and its folder is writable by the
        // employee: resolve it again under the lock and refuse anything that
        // no longer is the same existing regular file.
        if c.key.is_skill() && super::file_path(c.home, c.agent_id, &c.key).as_ref() != Ok(&path) {
            return Ok(Locked::Err(CommitError::Refused("skill file not found".into())));
        }
        let current = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !c.key.is_skill() => String::new(),
            Err(e) => return Ok(Locked::Err(CommitError::Failed(format!("cannot read {}: {e}", c.key.file_name())))),
        };
        let before_hash = super::hash_of(&current);
        if let Some(expected) = c.expected_hash
            && expected != before_hash
        {
            return Ok(Locked::Err(CommitError::Conflict { current_hash: before_hash }));
        }
        if let Err(e) = guard() {
            return Ok(Locked::Err(CommitError::Refused(e)));
        }
        let new = match render(&current) {
            Ok(n) => n,
            Err(e) => return Ok(Locked::Err(CommitError::Refused(e))),
        };
        let hash = super::hash_of(&new);
        let accept_current = new == current && c.baseline == Baseline::AcceptCurrent && c.key.protected_doc().is_some();
        if new == current && !accept_current {
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
        if new.len() > c.key.max_bytes() {
            return Ok(Locked::Err(CommitError::Refused(format!("內容太長：最多 {} KB", c.key.max_bytes() / 1024))));
        }
        if c.key == FileKey::AgentToml
            && let Err(e) = crate::product_policy::validate_write(c.home, &path, &current, &new)
        {
            return Ok(Locked::Err(CommitError::Refused(e)));
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
    });
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
    let lock = super::file_mutex(c.home, c.agent_id, &c.key);
    let _held = lock.lock().await;
    let mut out = run_blocking(|| commit_locked(&c, render, guard))?;
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
