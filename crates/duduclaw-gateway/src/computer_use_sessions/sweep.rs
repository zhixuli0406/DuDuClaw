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
//! Runs at gateway start and every [`SWEEP_INTERVAL`]. Every Docker call goes
//! through the orchestrator's bounded `docker_output`.

use std::path::Path;
use std::time::Duration;

use crate::computer_use_orchestrator::{
    DEADLINE_LABEL, HOME_LABEL, computer_use_home_label, docker_output,
};

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
}

/// Parse `id|name|state|deadline` lines. `None` when any line is malformed:
/// an unverifiable listing removes nothing.
pub fn parse_listing(text: &str) -> Option<Vec<Listed>> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut parts = line.split('|');
        let (id, name, state, deadline) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
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

/// Sweep at start and then every [`SWEEP_INTERVAL`], forever (spawned).
/// Each pass also deletes browser-audit screenshots older than the
/// retention period ([`super::AUDIT_RETENTION_DAYS`] days).
pub async fn run_periodically(home: std::path::PathBuf) {
    let mut ticks = tokio::time::interval(SWEEP_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
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

/// One sweep. Returns how many containers were removed.
pub async fn sweep_once(home: &Path) -> usize {
    let label = computer_use_home_label(home);
    let format = format!("{{{{.ID}}}}|{{{{.Names}}}}|{{{{.State}}}}|{{{{.Label \"{DEADLINE_LABEL}\"}}}}");
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
    let mut removed = 0;
    for container in containers.iter().filter(|c| removable(c, now)).take(MAX_REMOVALS_PER_SWEEP) {
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
