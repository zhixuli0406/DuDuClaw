//! Orphan sweep for computer-use containers (`duduclaw-cu-*`) a crashed or
//! killed gateway left behind. Mirrors the task sandbox's `sweep.rs`:
//!
//! - Only containers labelled with this home's
//!   [`HOME_LABEL`](crate::computer_use_orchestrator::HOME_LABEL) are listed;
//!   other homes on a shared daemon are never touched.
//! - A container is removed when it is no longer running (`exited`, `dead`)
//!   or when its deadline label is more than [`GRACE`] in the past. A
//!   container with no readable deadline is left alone.
//! - When Docker cannot be listed (or the listing does not parse), nothing is
//!   removed.
//!
//! - A container carrying the workspace labels (P2-C) is also removed when
//!   the registry says its lease epoch is no longer the live lease of that
//!   workspace; when the registry cannot be read, that rule removes nothing.
//!   Containers without the workspace label follow the rules above only.
//!   Removal never uses `-v`, so a bind-mounted workspace is never touched.
//!
//! Runs at gateway start and every [`SWEEP_INTERVAL`]; each pass first
//! (only in the gateway holding the home's instance lock; the workspace rule
//! above is gated the same way, the orphan rule is not) reconciles the
//! workspace registry (expired leases, write intents,
//! unfinished creates/deletes, retention). Every Docker call goes through
//! the orchestrator's bounded `docker_output`.

use std::path::Path;
use std::time::Duration;

use crate::computer_use_orchestrator::{
    DEADLINE_LABEL, HOME_LABEL, computer_use_home_label, docker_output,
};
use crate::computer_workspaces::{LEASE_LABEL, WORKSPACE_LABEL};

/// Slack after a container's deadline before it counts as orphaned.
pub const GRACE: Duration = Duration::from_secs(600);
/// Time between two sweeps after the one at gateway start.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// Upper bound on one listing / removal call.
const DOCKER_CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Containers removed per sweep at most; the rest wait for the next one.
pub const MAX_REMOVALS_PER_SWEEP: usize = 64;

/// One listed container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub state: String,
    pub deadline: Option<u64>,
    /// `com.duduclaw.computer-use.workspace` (empty label = `None`).
    pub workspace: Option<String>,
    /// `com.duduclaw.computer-use.lease` (the epoch it was started under).
    pub lease: Option<i64>,
}

/// Parse `id|name|state|deadline` lines. `None` when any line is malformed:
/// an unverifiable listing removes nothing.
pub fn parse_listing(text: &str) -> Option<Vec<Listed>> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut parts = line.split('|');
        let (id, name, state, deadline) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        // Optional workspace + lease labels (both or neither).
        let (workspace, lease) = match (parts.next(), parts.next()) {
            (None, None) => (None, None),
            (Some(w), Some(l)) => (
                Some(w.trim()).filter(|w| !w.is_empty()).map(str::to_string),
                l.trim().parse().ok(),
            ),
            _ => return None,
        };
        if parts.next().is_some() || id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        if !state.bytes().all(|c| c.is_ascii_lowercase()) {
            return None;
        }
        out.push(Listed {
            id: id.to_string(),
            name: name.trim_start_matches('/').to_string(),
            state: state.to_string(),
            deadline: deadline.trim().parse().ok(),
            workspace,
            lease,
        });
    }
    Some(out)
}

/// Whether the sweep may remove this container at `now` (unix seconds).
/// Only computer-use containers (name `duduclaw-cu-<32 hex>`) qualify.
pub fn removable(container: &Listed, now: u64) -> bool {
    let is_cu = container
        .name
        .strip_prefix("duduclaw-cu-")
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|c| c.is_ascii_hexdigit()));
    if !is_cu {
        return false;
    }
    if matches!(container.state.as_str(), "exited" | "dead") {
        return true;
    }
    container.deadline.is_some_and(|deadline| now > deadline.saturating_add(GRACE.as_secs()))
}

/// The workspace rule (design §4.6 step 2): a computer-use container with
/// workspace labels whose epoch is not the live lease of that workspace.
/// `live_epoch(id)` is `Some(Some(epoch))` for a live lease, `Some(None)` for
/// no live lease, `None` when the registry does not know (nothing removed).
pub fn stale_workspace_container(container: &Listed, live_epoch: impl Fn(&str) -> Option<Option<i64>>) -> bool {
    let is_cu = container
        .name
        .strip_prefix("duduclaw-cu-")
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|c| c.is_ascii_hexdigit()));
    let (Some(ws), Some(epoch)) = (&container.workspace, container.lease) else {
        return false;
    };
    if !is_cu || !crate::computer_workspaces::paths::valid_workspace_id(ws) {
        return false;
    }
    match live_epoch(ws) {
        Some(Some(live)) => live != epoch,
        Some(None) => true,
        None => false,
    }
}

/// Whether this process may run the registry maintenance P2-C added
/// (reconciliation and the stale-workspace-container rule): only the gateway
/// holding the home's instance lock, like workflow queue consumption and
/// patrol. When not held, logs once per process and returns false.
///
/// Lease *renewal* for a live session (the reaper's renew in `workspace.rs`)
/// deliberately does NOT use this gate: a session belongs to the gateway
/// that started it, and gating renewal would make a second gateway's
/// sessions lose their leases. The pre-v1.67 orphan-container rule also
/// stays ungated. Request-driven operator actions are unaffected.
fn maintenance_allowed(home: &Path) -> bool {
    static SKIP_LOGGED: std::sync::Once = std::sync::Once::new();
    let held = duduclaw_core::gateway_instance::held(home);
    if !held {
        SKIP_LOGGED.call_once(|| {
            tracing::debug!("computer workspace maintenance skipped: this process does not hold the gateway instance lock");
        });
    }
    held
}

/// Registry reconciliation, then the Activity Feed notice for each
/// workspace whose retention just ran out. Off the async runtime.
/// Runs only in the lock-holding gateway.
pub async fn reconcile_workspaces(home: &Path) {
    if !maintenance_allowed(home) {
        return;
    }
    reconcile_workspaces_unchecked(home).await;
}

/// [`reconcile_workspaces`] without the lock gate (callers decide).
pub async fn reconcile_workspaces_unchecked(home: &Path) {
    let h = home.to_path_buf();
    let expired = tokio::task::spawn_blocking(move || crate::computer_workspaces::reconcile_registry(&h)).await;
    let expired = match expired {
        Ok(Ok(expired)) => expired,
        Ok(Err(e)) => {
            tracing::debug!(?e, "computer workspace reconciliation skipped");
            return;
        }
        Err(_) => return,
    };
    let Ok(store) = crate::task_store::TaskStore::open(home) else { return };
    for (id, owner) in expired {
        let row = crate::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "workspace_expired".to_string(),
            agent_id: owner,
            task_id: None,
            summary: "電腦操作工作區已超過保留期限，轉為唯讀（資料沒有刪除）；需要時請管理員延長或刪除。".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: Some(serde_json::json!({"workspace_id": id}).to_string()),
        };
        if let Err(e) = store.append_activity(&row).await {
            tracing::warn!("workspace expiry activity append failed: {e}");
        }
    }
}

/// Sweep at start and then every [`SWEEP_INTERVAL`], forever (spawned).
/// Each pass also deletes browser-audit screenshots older than the
/// retention period ([`super::AUDIT_RETENTION_DAYS`] days).
pub async fn run_periodically(home: std::path::PathBuf) {
    let mut ticks = tokio::time::interval(SWEEP_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        reconcile_workspaces(&home).await;
        sweep_once(&home).await;
        cleanup_screenshots(&home).await;
    }
}

/// Apply the screenshot retention, off the async runtime. Returns how many
/// files were deleted.
pub async fn cleanup_screenshots(home: &Path) -> u32 {
    let home = home.to_path_buf();
    let cleaned = tokio::task::spawn_blocking(move || {
        crate::screenshot_audit::BrowserAuditLog::new(&home, super::AUDIT_RETENTION_DAYS).cleanup_expired()
    })
    .await;
    match cleaned {
        Ok(Ok(removed)) => removed,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "computer-use sweep: screenshot retention failed");
            0
        }
        Err(e) => {
            tracing::warn!(error = %e, "computer-use sweep: screenshot retention task failed");
            0
        }
    }
}

/// The containers one pass removes. The orphan rule always applies; the
/// workspace stale-epoch rule only when `maintain` (lock held).
pub fn select_removable<'a>(
    containers: &'a [Listed],
    now: u64,
    maintain: bool,
    live_epoch: impl Fn(&str) -> Option<Option<i64>>,
) -> Vec<&'a Listed> {
    containers
        .iter()
        .filter(|c| removable(c, now) || (maintain && stale_workspace_container(c, &live_epoch)))
        .take(MAX_REMOVALS_PER_SWEEP)
        .collect()
}

/// One sweep. Returns how many containers were removed. The workspace
/// stale-epoch rule runs only in the lock-holding gateway.
pub async fn sweep_once(home: &Path) -> usize {
    let maintain = maintenance_allowed(home);
    sweep_once_with(home, maintain).await
}

/// [`sweep_once`] with the gate decided by the caller.
pub async fn sweep_once_with(home: &Path, maintain: bool) -> usize {
    let label = computer_use_home_label(home);
    let format = format!(
        "{{{{.ID}}}}|{{{{.Names}}}}|{{{{.State}}}}|{{{{.Label \"{DEADLINE_LABEL}\"}}}}|{{{{.Label \"{WORKSPACE_LABEL}\"}}}}|{{{{.Label \"{LEASE_LABEL}\"}}}}"
    );
    let filter = format!("label={HOME_LABEL}={label}");
    let args = ["ps", "--all", "--no-trunc", "--filter", filter.as_str(), "--format", format.as_str()];
    let listing = match docker_output(&args, DOCKER_CALL_TIMEOUT, "Computer-use sweep list").await {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout).ok(),
        _ => None,
    };
    let Some(text) = listing else {
        tracing::debug!("computer-use sweep: Docker not listable, nothing removed");
        return 0;
    };
    let Some(containers) = parse_listing(&text) else {
        tracing::warn!("computer-use sweep: unexpected docker ps output, nothing removed");
        return 0;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Live lease epochs from the registry; `None` when it cannot be read.
    let rows = if maintain && containers.iter().any(|c| c.workspace.is_some()) {
        crate::computer_workspaces::shared::shared_async(home)
            .await
            .and_then(|s| s.list_all())
            .ok()
    } else {
        None
    };
    let now_i = now as i64;
    let live_epoch = |id: &str| -> Option<Option<i64>> {
        let rows = rows.as_ref()?;
        Some(
            rows.iter()
                .find(|r| r.workspace_id == id)
                .filter(|r| r.lease_active(now_i))
                .map(|r| r.lease_epoch),
        )
    };
    let mut removed = 0;
    for container in select_removable(&containers, now, maintain, live_epoch) {
        let rm = ["rm", "--force", container.id.as_str()];
        match docker_output(&rm, DOCKER_CALL_TIMEOUT, "Computer-use sweep remove").await {
            Ok(out) if out.status.success() => removed += 1,
            _ => tracing::warn!(container = %container.name, "computer-use sweep: removal failed"),
        }
    }
    if removed > 0 {
        tracing::info!(removed, "computer-use sweep: removed leftover containers");
    }
    removed
}
