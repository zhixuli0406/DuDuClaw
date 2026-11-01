//! Moving an earlier import out of the way before its replacement installs,
//! and moving it back when the replacement fails.
//!
//! Everything is a `rename` inside the home (one file system). Before the
//! first move a journal is written (`duduclaw_gateway::expert_replace_journal`):
//! the planned moves, the org entries of the employees moved, and every
//! path the new version then starts creating. A rollback here and the
//! recovery of a replace that was stopped half way (gateway start, next
//! import) both undo from that journal. Nothing is deleted: what a failed
//! replacement already wrote is moved into the backup's `failed-new/`.

use std::path::{Component, Path, PathBuf};

use serde_json::json;

use duduclaw_core::agent_trash::AGENT_TRASH_DIR;
use duduclaw_core::error::Result;
use duduclaw_core::org_store;
use duduclaw_gateway::expert_replace_journal::{self as journal, REPLACED_DIR, ReplaceJournal};

use super::ReplaceTarget;
use crate::expert::experts_dir;
use crate::expert::import_error::import_err;

pub(crate) use journal::withdraw_pending;

/// A replace in progress: its journal (kept on disk) and what moved.
#[derive(Debug)]
pub struct RetireLog {
    home: PathBuf,
    pub(crate) journal: ReplaceJournal,
    /// Employee ids that were moved to the trash.
    pub agents: Vec<String>,
    /// `experts/_replaced/<slug>_<stamp>`.
    pub backup: PathBuf,
}

impl RetireLog {
    /// Where the earlier import's record (and its approval state) went.
    pub fn record_backup(&self) -> PathBuf {
        self.backup.join("record")
    }

    /// The new version is about to create employee `id`.
    pub fn note_agent(&mut self, id: &str) {
        self.journal.note_agent(&self.home, id);
        self.save();
    }

    /// The new version is about to create `path` (appended to the
    /// journal's sidecar, not a full rewrite).
    pub fn note_path(&mut self, path: &Path) {
        if let Err(e) = self.journal.append_written(&self.home, path) {
            eprintln!("  ⚠ 取代紀錄（journal）更新失敗：{e}");
        }
    }

    fn save(&self) {
        if let Err(e) = self.journal.save(&self.home) {
            eprintln!("  ⚠ 取代紀錄（journal）更新失敗：{e}");
        }
    }

    /// The replace is over (installed, or rolled back).
    pub fn close(&self) {
        self.journal.remove(&self.home);
    }
}

/// Move the earlier import of `target` out of the way. The journal is
/// written before anything moves; on any failure what moved is put back.
pub fn retire(home: &Path, target: &ReplaceTarget) -> Result<RetireLog> {
    let slug = &target.slug;
    let now = chrono::Utc::now();
    let stamp = now.format("%Y%m%d%H%M%S").to_string();
    // A fresh folder even when an earlier replace (or a failed one) used
    // the same second.
    let replaced = experts_dir(home).join(REPLACED_DIR);
    let backup = std::iter::once(format!("{slug}_{stamp}"))
        .chain((2..100).map(|n| format!("{slug}_{stamp}-{n}")))
        .map(|name| replaced.join(name))
        .find(|p| p.symlink_metadata().is_err() && !ReplaceJournal::new(home, slug, p).path(home).exists())
        .ok_or_else(|| import_err("import.replace_failed", json!({ "slug": slug }), "備份資料夾名稱已用完，請稍後再試"))?;
    let failed = |msg: String| import_err("import.replace_failed", json!({ "slug": slug }), msg);

    let barrier = duduclaw_core::autonomy_gate::exclusive_barrier(home)
        .map_err(|error| failed(error.to_string()))?;
    let moves = plan_moves(home, target, now, &backup).map_err(failed)?;
    let mut j = ReplaceJournal::new(home, slug, &backup);
    let org = org_store::load(home);
    for id in &target.agents {
        j.keep_org(id, org.get(id));
    }
    for (from, to, _) in &moves {
        j.plan_move(home, from, to);
    }
    // The new version writes these whether or not the earlier one had them.
    j.note_path(home, &home.join("imported").join(slug));
    j.note_path(home, &experts_dir(home).join(slug));
    j.save(home)
        .map_err(|e| failed(format!("無法寫入取代紀錄（journal）：{e}；什麼都沒改")))?;

    let mut log = RetireLog { home: home.to_path_buf(), journal: j, agents: Vec::new(), backup };
    for (from, to, agent) in moves {
        if let Err(e) = move_path(&from, &to) {
            // Rollback acquires its own barrier; never nest exclusive leases.
            drop(barrier);
            rollback(&log);
            return Err(failed(format!(
                "無法把之前的版本移到備份（{}）：{e}；已嘗試還原，失敗項目會保留紀錄",
                from.display()
            )));
        }
        if let Some(id) = agent {
            log.agents.push(id);
        }
    }
    Ok(log)
}

/// Undo the replace after the new version failed (see
/// [`journal::undo`]), print what could not be undone, close the journal.
/// Returns where the new version's record folder went (its approvals).
pub fn rollback(log: &RetireLog) -> Option<PathBuf> {
    let out = journal::undo(&log.home, &log.journal);
    for f in &out.failures {
        eprintln!("  ✗ 還原失敗：{f}（請手動處理）");
    }
    if out.failures.is_empty() {
        log.close();
    }
    out.failed_record
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
