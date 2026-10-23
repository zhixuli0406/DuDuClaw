//! Moving an earlier import out of the way before its replacement installs,
//! and moving it back when the replacement fails.
//!
//! Everything is a `rename` inside the home (one file system), recorded in
//! order, so a rollback is the same list reversed. Nothing is deleted:
//! what a failed replacement already wrote is moved into the backup's
//! `failed-new/` (same layout as the home). The org entries of the
//! employees moved away are kept, and put back on a rollback.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde_json::json;

use duduclaw_core::agent_trash::AGENT_TRASH_DIR;
use duduclaw_core::error::Result;

use super::ReplaceTarget;
use crate::expert::import_error::import_err;
use super::ReplaceState;
use crate::expert::experts_dir;
use duduclaw_core::org_store::{self, OrgEntry};

/// Where the non-employee parts of a replaced import are kept.
pub const REPLACED_DIR: &str = "_replaced";

/// The moves made by [`retire`], in order.
#[derive(Debug, Default)]
pub struct RetireLog {
    home: PathBuf,
    slug: String,
    moves: Vec<(PathBuf, PathBuf)>,
    /// Org entries of the moved employees before anything changed
    /// (`None`: there was none).
    org_before: Vec<(String, Option<OrgEntry>)>,
    /// Employee ids that were moved to the trash.
    pub agents: Vec<String>,
    /// `experts/_replaced/<slug>_<stamp>` (may not exist when only
    /// employees moved).
    pub backup: PathBuf,
}

impl RetireLog {
    /// Where the earlier import's record (and its approval state) went.
    pub fn record_backup(&self) -> PathBuf {
        self.backup.join("record")
    }

    /// Where a failed replacement's own record went on rollback.
    pub fn failed_record(&self) -> PathBuf {
        self.aside_path(&experts_dir(&self.home).join(&self.slug))
    }

    fn aside_path(&self, p: &Path) -> PathBuf {
        let rel = p.strip_prefix(&self.home).unwrap_or(p);
        let rel: PathBuf = rel.components().filter(|c| matches!(c, Component::Normal(_))).collect();
        self.backup.join("failed-new").join(rel)
    }
}

/// Move the earlier import of `target` out of the way. On any failure the
/// moves already made are undone and the error says so.
pub fn retire(home: &Path, target: &ReplaceTarget) -> Result<RetireLog> {
    let slug = &target.slug;
    let now = chrono::Utc::now();
    let stamp = now.format("%Y%m%d%H%M%S").to_string();
    let org = org_store::load(home);
    // A fresh folder even when an earlier replace (or a failed one) used
    // the same second.
    let replaced = experts_dir(home).join(REPLACED_DIR);
    let backup = std::iter::once(format!("{slug}_{stamp}"))
        .chain((2..100).map(|n| format!("{slug}_{stamp}-{n}")))
        .map(|name| replaced.join(name))
        .find(|p| p.symlink_metadata().is_err())
        .ok_or_else(|| import_err("import.replace_failed", json!({ "slug": slug }), "備份資料夾名稱已用完，請稍後再試"))?;
    let mut log = RetireLog {
        home: home.to_path_buf(),
        slug: slug.clone(),
        backup,
        org_before: target.agents.iter().map(|id| (id.clone(), org.get(id).cloned())).collect(),
        ..Default::default()
    };
    match plan_moves(home, target, now, &log.backup) {
        Ok(moves) => {
            for (from, to, agent) in moves {
                if let Err(e) = move_path(&from, &to) {
                    rollback(&log, &ReplaceState::default());
                    return Err(import_err(
                        "import.replace_failed",
                        json!({ "slug": slug }),
                        format!("無法把之前的版本移到備份（{}）：{e}；已還原，什麼都沒改", from.display()),
                    ));
                }
                if let Some(id) = agent {
                    log.agents.push(id);
                }
                log.moves.push((from, to));
            }
            Ok(log)
        }
        Err(msg) => Err(import_err("import.replace_failed", json!({ "slug": slug }), msg)),
    }
}

/// Undo [`retire`] after the replacement failed. First everything the
/// replacement wrote (`state`) is moved into `<backup>/failed-new/`, then
/// every move is reversed (newest first), then the org entries of the
/// employees involved are put back as they were. Best effort — a failed
/// step is printed so the operator can finish it by hand.
pub fn rollback(log: &RetireLog, state: &ReplaceState) {
    let originals: BTreeSet<&PathBuf> = log.moves.iter().map(|(from, _)| from).collect();
    for p in state.written_paths.iter().rev() {
        if !originals.contains(p) && p.symlink_metadata().is_ok() {
            move_aside(log, p);
        }
    }
    for (from, to) in log.moves.iter().rev() {
        if from.symlink_metadata().is_ok() && !move_aside(log, from) {
            continue;
        }
        if let Err(e) = std::fs::rename(to, from) {
            eprintln!(
                "  ✗ 還原失敗：{} → {}：{e}（請手動搬回）",
                to.display(),
                from.display()
            );
        }
    }
    restore_org(log, &state.written_agents);
    // Only removes the backup folder when nothing is left in it.
    for sub in ["skills", "wiki", ""] {
        let _ = std::fs::remove_dir(log.backup.join(sub));
    }
}

fn move_aside(log: &RetireLog, p: &Path) -> bool {
    let mut aside = log.aside_path(p);
    let mut n = 2;
    while aside.symlink_metadata().is_ok() {
        aside.set_file_name(format!(
            "{}-{n}",
            p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        n += 1;
    }
    match move_path(p, &aside) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("  ✗ 還原失敗：{} 無法移開：{e}（請手動處理）", p.display());
            false
        }
    }
}

/// The org entries as they were before the replace: earlier employees get
/// theirs back; employees only the failed replacement created lose theirs.
fn restore_org(log: &RetireLog, written: &[String]) {
    for (id, before) in &log.org_before {
        let res = match before {
            Some(entry) => org_store::upsert(&log.home, id, entry.clone()).map(|_| ()),
            None => org_store::remove(&log.home, id).map(|_| ()),
        };
        if let Err(e) = res {
            eprintln!("  ✗ 組織資料還原失敗（{id}）：{e}（請手動檢查 org.toml）");
        }
    }
    for id in written {
        if log.org_before.iter().any(|(old, _)| old == id) || !crate::is_valid_agent_id(id) {
            continue;
        }
        if let Err(e) = org_store::remove(&log.home, id) {
            eprintln!("  ✗ 組織資料還原失敗（{id}）：{e}（請手動檢查 org.toml）");
        }
    }
}

type PlannedMove = (PathBuf, PathBuf, Option<String>);

fn plan_moves(
    home: &Path,
    target: &ReplaceTarget,
    now: chrono::DateTime<chrono::Utc>,
    backup: &Path,
) -> std::result::Result<Vec<PlannedMove>, String> {
    let rec = &target.record;
    let mut moves: Vec<PlannedMove> = Vec::new();
    let trash = home.join("agents").join(AGENT_TRASH_DIR);
    for id in &target.agents {
        if !crate::is_valid_agent_id(id) {
            continue;
        }
        let dir = home.join("agents").join(id);
        if dir.exists() {
            // The trash names entries `<id>_<14-digit stamp>`: a later second
            // when this one is already used for the id.
            let to = (0..120)
                .map(|k| now + chrono::Duration::seconds(k))
                .map(|t| trash.join(format!("{id}_{}", t.format("%Y%m%d%H%M%S"))))
                .find(|p| p.symlink_metadata().is_err())
                .ok_or_else(|| format!("垃圾桶裡 {id} 的備份名稱已用完，請稍後再試"))?;
            moves.push((dir, to, Some(id.clone())));
        }
    }
    for skill in &rec.global_skills {
        if duduclaw_agent::skill_loader::is_safe_skill_name(skill) {
            let src = home.join("skills").join(skill);
            if src.exists() {
                moves.push((src, backup.join("skills").join(skill), None));
            }
        }
    }
    let wiki_root = home.join("shared").join("wiki");
    for rel in &rec.wiki_files {
        let rel_path = Path::new(rel);
        if !rel_path.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!("匯入紀錄裡的知識頁路徑不安全：{}", rel.escape_debug()));
        }
        let src = wiki_root.join(rel_path);
        if src.is_file() {
            moves.push((src, backup.join("wiki").join(rel_path), None));
        }
    }
    let archive = home.join("imported").join(&target.slug);
    if archive.exists() {
        moves.push((archive, backup.join("imported"), None));
    }
    let record_dir = experts_dir(home).join(&target.slug);
    if record_dir.exists() {
        moves.push((record_dir, backup.join("record"), None));
    }
    Ok(moves)
}

fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "備份位置已存在"));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to)
}

/// After the replacement installed: drop the org records of replaced
/// employees that the new version did not bring back (their directories
/// are in the trash; a dangling record would otherwise linger).
pub fn finish(home: &Path, log: &RetireLog, new_agents: &[String]) {
    for id in &log.agents {
        if !new_agents.contains(id)
            && let Err(e) = org_store::remove(home, id)
        {
            eprintln!("  ⚠ 組織資料移除失敗（{id}）：{e}");
        }
    }
}

/// Withdraw the approvals still pending for an import that is out of use
/// (its record folder is `record_dir`): the skills and hooks requests it
/// filed. A request already decided is left as it is. Returns how many
/// were withdrawn.
pub async fn withdraw_pending(home: &Path, record_dir: &Path, reason: &str) -> usize {
    let ids: Vec<String> = ["skills-state.json", "hooks-state.json"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(record_dir.join(f)).ok())
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .filter_map(|v| v.get("approval_id").and_then(|i| i.as_str()).map(str::to_string))
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        return 0;
    }
    let broker = match duduclaw_gateway::approval::ApprovalBroker::open(home) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("  ⚠ 無法撤回舊的審核單（{e}），請到「待辦」手動拒絕");
            return 0;
        }
    };
    let mut n = 0;
    for id in ids {
        match broker.withdraw(&duduclaw_gateway::approval::ApprovalId::from(id.clone()), reason).await {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => eprintln!("  ⚠ 撤回審核單 {id} 失敗：{e}"),
        }
    }
    n
}
