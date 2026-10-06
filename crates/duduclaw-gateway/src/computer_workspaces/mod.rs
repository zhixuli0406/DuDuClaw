//! Durable computer-use workspaces (P2-C, design
//! `P2C-design-freeze.md`, Appendix B wins).
//!
//! A workspace is a gateway-owned directory `<home>/computer_workspaces/<id>/data`
//! that outlives any computer-use container. The gateway is the only writer
//! (`computer_workspace_write`); a session that attaches it has it
//! read-only at `/workspace/files`, under a root-only tmpfs at `/workspace`
//! that the browser's user cannot enter. Browser state never lands here: the container
//! root is read-only, `/tmp` is a tmpfs, downloads stay blocked.
//!
//! - [`store`]: the registry (`computer_workspaces.db`), state machine and
//!   lease CAS operations.
//! - [`paths`]: derived paths and the mount-source checks.
//! - [`files`]: path rules, quota, atomic writes, crash reconciliation.
//! - [`config`]: `config.toml [computer_use.workspaces]` (default off).
//! - [`lock`]: one writer / creator / reconciler per workspace, across
//!   processes; [`ledger`]: per-file sizes and hashes.
//!
//! Unix only: on other platforms every entry point refuses (fail closed) but
//! the module still compiles.

pub mod cli_approval;
pub mod config;
pub mod doctor;
pub mod files;
mod intents;
pub mod ledger;
pub mod lock;
pub mod owner_cred;
pub mod paths;
mod retention;
pub mod shared;
pub mod state;
pub mod store;

#[cfg(test)]
mod tests;

use std::path::Path;

use sha2::{Digest, Sha256};

pub use config::WorkspacesConfig;
pub use state::WorkspaceState;
pub use store::{Lease, StoreError, WorkspaceRow, WorkspaceStore};

/// How long a lease lives without renewal (the reaper renews every 15 s).
pub const LEASE_TTL_SECS: i64 = 90;

/// Container label naming the attached workspace.
pub const WORKSPACE_LABEL: &str = "com.duduclaw.computer-use.workspace";
/// Container label naming the lease epoch the container was started under.
pub const LEASE_LABEL: &str = "com.duduclaw.computer-use.lease";

pub fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `local-docker:` + the first 32 hex of sha256(home label ‖ daemon id).
pub fn runner_id_from(home_label: &str, daemon_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(home_label.as_bytes());
    h.update(b"\0");
    h.update(daemon_id.as_bytes());
    format!("local-docker:{}", &hex::encode(h.finalize())[..32])
}

/// The runner id of this home's Docker daemon (`docker info --format
/// '{{.ID}}'`). `None` when Docker does not answer with a usable id: a
/// workspace start then fails (never "any runner").
pub async fn current_runner_id(home: &Path) -> Option<String> {
    let out = crate::computer_use_orchestrator::docker_output(
        &["info", "--format", "{{.ID}}"],
        std::time::Duration::from_secs(10),
        "Docker info",
    )
    .await
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8(out.stdout).ok()?;
    let id = id.trim();
    let usable = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b':'));
    usable.then(|| {
        runner_id_from(
            &crate::computer_use_orchestrator::computer_use_home_label(home),
            id,
        )
    })
}

/// Parse a `_trash` stamp (`%Y%m%d%H%M%S`, UTC) to unix seconds.
fn trash_stamp(stamp: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d%H%M%S")
        .ok()
        .map(|t| t.and_utc().timestamp())
}

/// Whether the owner of a workspace created at `created_at` is gone
/// (Appendix B.2): `agents/<owner>/agent.toml` is missing, or `_trash` holds
/// an entry for this id stamped at or after `created_at` (an unparsable
/// stamp, or a `_trash` that cannot be listed, counts as gone — fail closed).
pub fn owner_removed(home: &Path, owner: &str, created_at: i64) -> bool {
    if !duduclaw_core::is_valid_agent_id(owner) {
        return true;
    }
    if !home.join("agents").join(owner).join("agent.toml").is_file() {
        return true;
    }
    let trash = home
        .join("agents")
        .join(duduclaw_core::agent_trash::AGENT_TRASH_DIR);
    let entries = match std::fs::read_dir(&trash) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    for entry in entries {
        let Ok(entry) = entry else { return true };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if duduclaw_core::agent_trash::trash_entry_agent_id(name) != Some(owner) {
            continue;
        }
        let stamp = name.get(name.len().saturating_sub(14)..).unwrap_or("");
        match trash_stamp(stamp) {
            Some(at) if at < created_at => {}
            _ => return true,
        }
    }
    false
}

/// Turn a workspace whose owner is gone into `orphaned` (lazy, at every
/// attach / read / write). Returns whether it did.
/// The owner is gone when [`owner_removed`] says so **or** the employee now
/// carrying the owner's name does not hold this workspace's credential
/// ([`owner_cred::matches`], review M-5); either one makes it `orphaned`.
pub fn orphan_if_owner_removed(home: &Path, store: &WorkspaceStore, row: &WorkspaceRow) -> bool {
    if !matches!(
        row.state,
        WorkspaceState::Ready | WorkspaceState::Expired | WorkspaceState::Revoked
    ) {
        return false;
    }
    let removed = owner_removed(home, &row.owner_agent_id, row.created_at);
    let credential_ok = owner_cred::matches(home, row);
    if !removed && credential_ok {
        return false;
    }
    store
        .transition(
            &row.workspace_id,
            &[
                WorkspaceState::Ready,
                WorkspaceState::Expired,
                WorkspaceState::Revoked,
            ],
            WorkspaceState::Orphaned,
            "system:owner_check",
            "orphaned",
            Some(if removed {
                "owner_removed"
            } else {
                "owner_credential_mismatch"
            }),
        )
        .is_ok()
}

/// Boot / periodic registry reconciliation that needs no Docker (design
/// §4.6 steps 1, 3, 4): expired leases, write intents, unfinished
/// `creating` / `failed_create` / `deleting` rows, retention, the event
/// cap. Rows and intents are only touched while their workspace lock is
/// free (taken without waiting), so a create or write in progress in any
/// process is left alone. Returns the `(id, owner)` pairs that just expired
/// (for the Activity Feed).
pub fn reconcile_registry(home: &Path) -> Result<Vec<(String, String)>, StoreError> {
    let enabled = config::load(home).is_ok_and(|c| c.enabled);
    // Never create the registry on an install that has never used the
    // feature (review: the sweep used to create it everywhere).
    if !enabled && !home.join(paths::DB_FILE).exists() {
        return Ok(Vec::new());
    }
    let store = shared::shared_blocking(home)?;
    let now = unix_now();
    store.expire_stale_leases(now)?;
    files::reconcile_intents(home, &store);
    for row in store.list_all()? {
        if !matches!(
            row.state,
            WorkspaceState::Creating | WorkspaceState::FailedCreate | WorkspaceState::Deleting
        ) {
            continue;
        }
        let Some(_lock) = lock::try_lock_idle(home, &row.workspace_id) else {
            continue;
        };
        // Re-read under the lock: the holder may have just finished.
        let Ok(Some(row)) = store.get(&row.workspace_id) else {
            continue;
        };
        match row.state {
            // A `creating` row with no live creator: nothing was handed out.
            WorkspaceState::Creating => {
                let _ = paths::remove_workspace_dir(home, &row.workspace_id);
                let _ = store.transition(
                    &row.workspace_id,
                    &[WorkspaceState::Creating],
                    WorkspaceState::FailedCreate,
                    "system:boot",
                    "failed_create",
                    Some("interrupted"),
                );
            }
            WorkspaceState::FailedCreate => {
                let _ = paths::remove_workspace_dir(home, &row.workspace_id);
            }
            WorkspaceState::Deleting => {
                if paths::remove_workspace_dir(home, &row.workspace_id).is_ok() {
                    let _ = store.transition(
                        &row.workspace_id,
                        &[WorkspaceState::Deleting],
                        WorkspaceState::Deleted,
                        "system:boot",
                        "deleted",
                        None,
                    );
                }
            }
            _ => {}
        }
    }
    let _ = store.prune_events(now);
    match config::load(home) {
        Ok(cfg) if cfg.enabled && cfg.retention_days > 0 => store.expire_retention(now),
        _ => Ok(Vec::new()),
    }
}
