//! Cleanup of task-sandbox leftovers: containers and
//! `<home>/sandbox/runs/<id>` directories a crashed or killed gateway, a
//! cancelled task or a failed per-task cleanup did not remove. Runs at
//! gateway start and then every [`SWEEP_INTERVAL`] ([`run_periodically`]).
//!
//! Safe next to a second gateway on the same home (or a concurrent task of
//! this one), without a lease:
//! - Only containers labelled with this home ([`super::container::home_label`])
//!   are considered; other homes on a shared daemon are never touched.
//! - A container is removed only when it is no longer running (`exited`,
//!   `dead`) or when its deadline label is more than [`GRACE`] in the past.
//!   The deadline is the absolute time at which the in-container supervisor
//!   has already stopped the CLI, so a live task can never be past it; every
//!   task's container carries one, so there is no unbounded "maximum task
//!   time" to guess. A running or freshly created container within its
//!   deadline is left alone.
//! - A run directory is removed only when no container of this home still
//!   carries its run id AND it was last modified more than [`GRACE`] ago
//!   (a concurrent task creates its directory a moment before its
//!   container). Entries that are not a 32-hex run id are never touched.
//! - When Docker cannot be listed, no directory is removed (a container could
//!   still be using it).
//! - At most [`MAX_REMOVALS_PER_SWEEP`] containers are removed per sweep; the
//!   rest (and their directories) wait for the next sweep, so a large backlog
//!   is drained in bounded batches instead of blocking or being skipped.
//!
//! Failures write `task_sandbox_cleanup_failed` with a reason code only.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::container::{LABEL, client_probe, home_label};

/// Slack after a container's deadline, and the minimum age of a run directory.
pub const GRACE: Duration = Duration::from_secs(600);

/// Upper bound of containers removed in one sweep. Each removal is two
/// bounded `docker` calls; the remainder waits for the next sweep.
pub const MAX_REMOVALS_PER_SWEEP: usize = 256;

/// Time between two sweeps after the one at gateway start.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// Closed set of `task_sandbox_cleanup_failed` reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepFailure {
    HomeUnresolved,
    DockerUnreachable,
    ListInvalid,
    ContainerRemoveFailed,
    DirectoryRemoveFailed,
}

impl SweepFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::HomeUnresolved => "home_unresolved",
            Self::DockerUnreachable => "docker_unreachable",
            Self::ListInvalid => "list_invalid",
            Self::ContainerRemoveFailed => "container_remove_failed",
            Self::DirectoryRemoveFailed => "directory_remove_failed",
        }
    }
}

/// What one sweep did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    pub containers_removed: usize,
    pub directories_removed: usize,
    /// Removable containers left for the next sweep (batch bound reached).
    pub containers_deferred: usize,
    pub failures: Vec<(SweepFailure, usize)>,
}

/// One listed container: `(id, state, run, deadline)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listed {
    pub id: String,
    pub state: String,
    pub run: String,
    pub deadline: Option<u64>,
}

/// Parse the `docker ps` lines (`id|state|run|deadline`). `None` when any
/// line is malformed: an unverifiable listing removes nothing.
pub(super) fn parse_listing(text: &str) -> Option<Vec<Listed>> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut parts = line.split('|');
        let (id, state, run, deadline) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        if !state.bytes().all(|c| c.is_ascii_lowercase()) || !run.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        out.push(Listed {
            id: id.to_string(),
            state: state.to_string(),
            run: run.to_string(),
            deadline: deadline.parse().ok(),
        });
    }
    Some(out)
}

/// Whether the sweep may remove this container at `now` (unix seconds).
pub(super) fn removable(container: &Listed, now: u64) -> bool {
    if matches!(container.state.as_str(), "exited" | "dead") {
        return true;
    }
    // No (or an unreadable) deadline: never guess, leave it.
    container.deadline.is_some_and(|deadline| now > deadline.saturating_add(GRACE.as_secs()))
}

fn is_run_id(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn audit_failure(home: &Path, failure: SweepFailure, count: usize) {
    super::audit(home, "task_sandbox_cleanup_failed", "system", serde_json::json!({
        "reason": failure.code(), "count": count,
    }));
}

/// Sweep at gateway start and then every [`SWEEP_INTERVAL`], forever (the
/// caller spawns it). Each sweep is cheap when the sandbox never ran: no
/// `<home>/sandbox` ⇒ no Docker call at all.
pub async fn run_periodically(home: std::path::PathBuf) {
    let mut ticks = tokio::time::interval(SWEEP_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        sweep_once(&home).await;
    }
}

/// One sweep. A home where the sandbox never ran (no `<home>/sandbox`) is a
/// no-op that does not even contact Docker.
pub async fn sweep_once(home: &Path) -> SweepReport {
    let mut report = SweepReport::default();
    if !home.join("sandbox").is_dir() {
        return report;
    }
    let Ok(canonical) = crate::discovery::workspace::canonical_real_directory(home) else {
        report.failures.push((SweepFailure::HomeUnresolved, 1));
        audit_failure(home, SweepFailure::HomeUnresolved, 1);
        return report;
    };
    let runs_dir = canonical.join("sandbox").join("runs");
    let run_dirs: Vec<String> = std::fs::read_dir(&runs_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| is_run_id(n))
                .collect()
        })
        .unwrap_or_default();
    let label = home_label(&canonical);
    let listing = client_probe(&[
        "ps", "--all", "--no-trunc",
        "--filter", &format!("label={LABEL}=1"),
        "--filter", &format!("label={LABEL}.home={label}"),
        "--format", &format!("{{{{.ID}}}}|{{{{.State}}}}|{{{{.Label \"{LABEL}.run\"}}}}|{{{{.Label \"{LABEL}.deadline\"}}}}"),
    ])
    .await;
    let Some(text) = listing else {
        // Docker gone or not running: nothing provable about the directories.
        if !run_dirs.is_empty() {
            report.failures.push((SweepFailure::DockerUnreachable, run_dirs.len()));
            audit_failure(home, SweepFailure::DockerUnreachable, run_dirs.len());
        }
        return report;
    };
    let Some(containers) = parse_listing(&text) else {
        report.failures.push((SweepFailure::ListInvalid, 1));
        audit_failure(home, SweepFailure::ListInvalid, 1);
        return report;
    };
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut in_use: BTreeSet<String> = BTreeSet::new();
    let mut remove_failed = 0usize;
    for container in &containers {
        if !removable(container, now) {
            in_use.insert(container.run.clone());
            continue;
        }
        if report.containers_removed + remove_failed >= MAX_REMOVALS_PER_SWEEP {
            // Batch bound reached: next sweep. Its directory stays with it.
            report.containers_deferred += 1;
            in_use.insert(container.run.clone());
            continue;
        }
        let removed = client_probe(&["rm", "--force", &container.id]).await.is_some();
        let gone = removed
            && client_probe(&["ps", "--all", "--quiet", "--no-trunc", "--filter", &format!("id={}", container.id)])
                .await
                .is_some_and(|out| out.trim().is_empty());
        if gone {
            report.containers_removed += 1;
        } else {
            remove_failed += 1;
            in_use.insert(container.run.clone());
        }
    }
    if remove_failed > 0 {
        report.failures.push((SweepFailure::ContainerRemoveFailed, remove_failed));
        audit_failure(home, SweepFailure::ContainerRemoveFailed, remove_failed);
    }
    let mut dir_failed = 0usize;
    for run in run_dirs.iter().filter(|r| !in_use.contains(*r)) {
        let path = runs_dir.join(run);
        let old_enough = std::fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_dir() && !m.file_type().is_symlink())
            .and_then(|m| m.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > GRACE);
        if !old_enough {
            continue;
        }
        match super::container::remove_run_dir(&path) {
            Ok(()) => report.directories_removed += 1,
            Err(_) => dir_failed += 1,
        }
    }
    if dir_failed > 0 {
        report.failures.push((SweepFailure::DirectoryRemoveFailed, dir_failed));
        audit_failure(home, SweepFailure::DirectoryRemoveFailed, dir_failed);
    }
    if report.containers_removed + report.directories_removed + report.containers_deferred > 0 {
        tracing::info!(
            containers = report.containers_removed,
            directories = report.directories_removed,
            deferred = report.containers_deferred,
            "task sandbox: removed leftover containers / run directories"
        );
    }
    report
}
