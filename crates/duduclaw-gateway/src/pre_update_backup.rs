//! Pre-update `/data` snapshot (H3d §11.5, item 2).
//!
//! Before an OS update overwrites the destination A/B root slot, snapshot
//! the handful of files under `<DUDUCLAW_HOME>` that actually define this
//! box's *identity* — `config.toml`, `org.toml`, and each agent's own
//! `agent.toml` — into `<DUDUCLAW_HOME>/backups/pre-update-<version>/`.
//!
//! This is deliberately NOT [`crate::device_ops::DeviceOps::backup_create`]
//! (`tar -czf` of the whole home directory, user-triggered, for download):
//!
//! - **Automatic**, fired unconditionally before every `device.update_apply`
//!   — the whole point is that nobody has to remember to click "backup"
//!   before updating.
//! - **Narrow**: config only. No memory databases, no conversation history,
//!   no model weights, no `tool_calls.jsonl` audit log — those are exactly
//!   the "large data" the design doc explicitly excludes, and copying them
//!   on every update would turn a courtesy snapshot into a second full
//!   backup nobody asked for.
//! - **Best-effort**: a failure here is logged (`[pre_update_backup]`) and
//!   MUST NEVER block the update itself. Losing a courtesy snapshot is not
//!   worth failing an update the user explicitly asked for — see
//!   `handlers.rs::handle_device_update_apply`'s call site.
//!
//! Rotation keeps the newest [`RETENTION_COUNT`] snapshot directories (by
//! filesystem mtime) and prunes the rest, so a box that updates every week
//! for a year does not slowly fill `/data` with config snapshots nobody
//! will ever read again.

use std::path::{Path, PathBuf};

use tracing::warn;

/// Subdirectory of `<DUDUCLAW_HOME>` snapshots land in. Shares the
/// `backups/` directory `device.backup_create`'s scheduled backups already
/// use — one place an operator looks for "things that back this box up",
/// distinguished by the `pre-update-` prefix rather than a separate tree.
const BACKUP_SUBDIR: &str = "backups";

/// Prefix of a snapshot directory's name; the remainder is the version
/// about to be installed.
const SNAPSHOT_PREFIX: &str = "pre-update-";

/// Total byte ceiling for one snapshot. Config files are KB-sized in
/// practice — this is a hard stop against an unexpectedly huge
/// `agent.toml` (or an agent count nobody anticipated) rather than a limit
/// this function expects to ever approach.
const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;

/// How many `pre-update-*` snapshot directories to keep. Oldest by
/// filesystem mtime beyond this are pruned after a successful snapshot.
const RETENTION_COUNT: usize = 5;

/// What one snapshot actually did — logged, never surfaced to the end user
/// (this is a courtesy operation, not a user-facing feature with its own
/// UI this round).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotReport {
    pub dir: PathBuf,
    pub files_copied: usize,
    pub bytes_copied: u64,
}

/// Snapshot `home`'s identity-defining config files into
/// `<home>/backups/pre-update-<version>/`, then prune old snapshots.
///
/// `version` becomes a path component, so it is validated with the exact
/// same character class [`crate::os_update::is_version_text`] uses to
/// accept a release version everywhere else in this module family — a
/// value this function accepts can only ever be a plain directory name,
/// never a traversal payload.
///
/// A pre-existing snapshot directory for the same version is replaced
/// (`remove_dir_all` then recreate) rather than merged — a retry of the
/// same apply should not accumulate files from an earlier, possibly
/// different, pre-update state.
pub fn snapshot_before_update(home: &Path, version: &str) -> Result<SnapshotReport, String> {
    if !crate::os_update::is_version_text(version) {
        return Err(format!(
            "refusing to use {version:?} as a snapshot directory name"
        ));
    }
    let backups_root = home.join(BACKUP_SUBDIR);
    let dest = backups_root.join(format!("{SNAPSHOT_PREFIX}{version}"));
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .map_err(|e| format!("cannot clear stale snapshot {}: {e}", dest.display()))?;
    }
    std::fs::create_dir_all(&dest)
        .map_err(|e| format!("cannot create {}: {e}", dest.display()))?;

    let mut files_copied = 0usize;
    let mut bytes_copied = 0u64;

    // Top-level singleton config files.
    for name in ["config.toml", "org.toml"] {
        let src = home.join(name);
        if !src.is_file() {
            continue;
        }
        let remaining = MAX_SNAPSHOT_BYTES.saturating_sub(bytes_copied);
        match copy_capped(&src, &dest.join(name), remaining) {
            Ok(n) => {
                bytes_copied += n;
                files_copied += 1;
            }
            Err(e) => warn!("[pre_update_backup] could not copy {}: {e}", src.display()),
        }
    }

    // Per-agent identity/config only (`agent.toml`) — never SOUL.md, memory
    // databases, sessions or logs. `read_dir` on a missing `agents/`
    // directory (a fresh install with no agents yet) is simply skipped, not
    // an error.
    let agents_dir = home.join("agents");
    if let Ok(rd) = std::fs::read_dir(&agents_dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(agent_id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let src = path.join("agent.toml");
            if !src.is_file() {
                continue;
            }
            let remaining = MAX_SNAPSHOT_BYTES.saturating_sub(bytes_copied);
            if remaining == 0 {
                warn!(
                    "[pre_update_backup] snapshot byte ceiling reached, skipping remaining agent.toml files"
                );
                break;
            }
            let agent_dest_dir = dest.join("agents").join(agent_id);
            if let Err(e) = std::fs::create_dir_all(&agent_dest_dir) {
                warn!(
                    "[pre_update_backup] could not create {}: {e}",
                    agent_dest_dir.display()
                );
                continue;
            }
            match copy_capped(&src, &agent_dest_dir.join("agent.toml"), remaining) {
                Ok(n) => {
                    bytes_copied += n;
                    files_copied += 1;
                }
                Err(e) => warn!("[pre_update_backup] could not copy {}: {e}", src.display()),
            }
        }
    }

    prune_old_snapshots(&backups_root);

    Ok(SnapshotReport {
        dir: dest,
        files_copied,
        bytes_copied,
    })
}

/// Copy `src` to `dest`, refusing (leaving `dest` unwritten) rather than
/// silently truncating when `src` is larger than `remaining_budget`. A
/// config file that legitimately exceeds the remaining snapshot budget is
/// more useful skipped-with-a-warning than partially copied — a truncated
/// `agent.toml` is a landmine for whoever eventually restores from it.
fn copy_capped(src: &Path, dest: &Path, remaining_budget: u64) -> Result<u64, String> {
    let meta =
        std::fs::metadata(src).map_err(|e| format!("cannot stat {}: {e}", src.display()))?;
    if meta.len() > remaining_budget {
        return Err(format!(
            "{} is {} bytes, over the remaining {remaining_budget}-byte snapshot budget",
            src.display(),
            meta.len()
        ));
    }
    std::fs::copy(src, dest)
        .map_err(|e| format!("cannot copy {} to {}: {e}", src.display(), dest.display()))?;
    Ok(meta.len())
}

/// Keep the newest [`RETENTION_COUNT`] `pre-update-*` directories under
/// `backups_root` (by filesystem mtime), remove the rest. Best-effort: a
/// missing `backups_root`, or a failure removing one stale directory, is
/// logged and never propagated — pruning is housekeeping, not part of the
/// snapshot's own success/failure.
fn prune_old_snapshots(backups_root: &Path) {
    let Ok(rd) = std::fs::read_dir(backups_root) else {
        return;
    };
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let is_ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(SNAPSHOT_PREFIX));
        if !is_ours {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        candidates.push((mtime, path));
    }
    if candidates.len() <= RETENTION_COUNT {
        return;
    }
    // Oldest first, so the prefix taken below is exactly the excess.
    candidates.sort_by_key(|(t, _)| *t);
    let excess = candidates.len() - RETENTION_COUNT;
    for (_, path) in candidates.into_iter().take(excess) {
        if let Err(e) = std::fs::remove_dir_all(&path) {
            warn!(
                "[pre_update_backup] could not prune old snapshot {}: {e}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_copies_top_level_config_and_per_agent_toml_only() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), b"[gateway]\n").unwrap();
        std::fs::write(home.path().join("org.toml"), b"[org]\n").unwrap();

        let agent_a = home.path().join("agents/a");
        std::fs::create_dir_all(&agent_a).unwrap();
        std::fs::write(agent_a.join("agent.toml"), b"[agent]\nid=\"a\"\n").unwrap();
        // A file that must NOT be copied — the whole point of "narrow".
        std::fs::write(agent_a.join("memory.db"), b"not config").unwrap();
        std::fs::write(agent_a.join("SOUL.md"), b"# soul").unwrap();

        let report = snapshot_before_update(home.path(), "0.2.0").unwrap();

        assert_eq!(report.files_copied, 3, "config.toml + org.toml + a/agent.toml");
        let dest = home.path().join("backups/pre-update-0.2.0");
        assert_eq!(report.dir, dest);
        assert!(dest.join("config.toml").is_file());
        assert!(dest.join("org.toml").is_file());
        assert!(dest.join("agents/a/agent.toml").is_file());
        assert!(
            !dest.join("agents/a/memory.db").exists(),
            "memory.db must never be snapshotted"
        );
        assert!(
            !dest.join("agents/a/SOUL.md").exists(),
            "SOUL.md must never be snapshotted — this is config-only"
        );
    }

    #[test]
    fn snapshot_on_a_bare_home_with_no_config_at_all_is_still_success_with_zero_files() {
        let home = tempfile::tempdir().unwrap();
        let report = snapshot_before_update(home.path(), "0.1.0").unwrap();
        assert_eq!(report.files_copied, 0);
        assert_eq!(report.bytes_copied, 0);
        assert!(report.dir.is_dir(), "the (empty) snapshot dir must still exist");
    }

    #[test]
    fn snapshot_refuses_a_version_that_is_not_a_plain_path_component() {
        let home = tempfile::tempdir().unwrap();
        for bad in ["../../etc", "0.2.0/../../evil", "", "with space"] {
            let err = snapshot_before_update(home.path(), bad).unwrap_err();
            assert!(err.contains("refusing"), "must refuse {bad:?}, got: {err}");
        }
        // Confirm nothing was created for the traversal attempt.
        assert!(!home.path().join("backups").exists());
    }

    #[test]
    fn snapshot_replaces_a_stale_directory_for_the_same_version_rather_than_merging() {
        let home = tempfile::tempdir().unwrap();
        let dest = home.path().join("backups/pre-update-0.2.0");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("leftover-from-a-crashed-attempt.txt"), b"stale").unwrap();

        let report = snapshot_before_update(home.path(), "0.2.0").unwrap();
        assert!(
            !report.dir.join("leftover-from-a-crashed-attempt.txt").exists(),
            "a stale prior attempt must be cleared, not merged into"
        );
    }

    #[test]
    fn copy_capped_refuses_rather_than_truncates_an_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("big.toml");
        std::fs::write(&src, vec![0u8; 100]).unwrap();
        let dest = dir.path().join("out.toml");

        let err = copy_capped(&src, &dest, 50).unwrap_err();
        assert!(err.contains("over the remaining"));
        assert!(!dest.exists(), "an over-budget file must not be partially written");

        // Exactly at budget still succeeds.
        let n = copy_capped(&src, &dest, 100).unwrap();
        assert_eq!(n, 100);
        assert!(dest.exists());
    }

    #[test]
    fn prune_keeps_only_the_newest_retention_count_snapshots() {
        let home = tempfile::tempdir().unwrap();
        let backups_root = home.path().join("backups");
        std::fs::create_dir_all(&backups_root).unwrap();

        // Create more than RETENTION_COUNT snapshot dirs with distinct
        // mtimes (filesystem mtime resolution can be coarse — set it
        // explicitly via `File::set_modified` rather than relying on
        // real-time sleeps between creations).
        for i in 0..(RETENTION_COUNT + 3) {
            let p = backups_root.join(format!("pre-update-0.{i}.0"));
            std::fs::create_dir_all(&p).unwrap();
            let mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(i as u64 * 100);
            std::fs::File::open(&p).unwrap().set_modified(mtime).unwrap();
        }
        // A non-`pre-update-` directory must survive pruning untouched —
        // this function only ever touches its own naming shape.
        let foreign = backups_root.join("scheduled-2026-08-24");
        std::fs::create_dir_all(&foreign).unwrap();

        prune_old_snapshots(&backups_root);

        let remaining: Vec<_> = std::fs::read_dir(&backups_root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            remaining.iter().filter(|n| n.starts_with(SNAPSHOT_PREFIX)).count(),
            RETENTION_COUNT
        );
        // The newest ones (highest index) must be the survivors.
        for i in 3..(RETENTION_COUNT + 3) {
            assert!(
                remaining.contains(&format!("pre-update-0.{i}.0")),
                "newest snapshot 0.{i}.0 must survive pruning: {remaining:?}"
            );
        }
        assert!(foreign.exists(), "a foreign directory must never be pruned");
    }
}
