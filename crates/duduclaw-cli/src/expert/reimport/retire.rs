//! Moving an earlier import out of the way before its replacement installs,
//! and moving it back when the replacement fails.
//!
//! Everything is a `rename` inside the home (one file system), recorded in
//! order, so a rollback is the same list reversed. Nothing is deleted.

use std::path::{Component, Path, PathBuf};

use serde_json::json;

use duduclaw_core::agent_trash::AGENT_TRASH_DIR;
use duduclaw_core::error::Result;

use super::ReplaceTarget;
use crate::expert::import_error::import_err;
use crate::expert::{InstallRecord, experts_dir};

/// Where the non-employee parts of a replaced import are kept.
pub const REPLACED_DIR: &str = "_replaced";

/// The moves made by [`retire`], in order.
#[derive(Debug, Default)]
pub struct RetireLog {
    moves: Vec<(PathBuf, PathBuf)>,
    /// Employee ids that were moved to the trash.
    pub agents: Vec<String>,
    /// `experts/_replaced/<slug>_<stamp>` (may not exist when only
    /// employees moved).
    pub backup: PathBuf,
}

/// Move the earlier import of `target` out of the way. On any failure the
/// moves already made are undone and the error says so.
pub fn retire(home: &Path, target: &ReplaceTarget) -> Result<RetireLog> {
    let rec = &target.record;
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    let mut log = RetireLog {
        backup: experts_dir(home).join(REPLACED_DIR).join(format!("{}_{stamp}", rec.slug)),
        ..Default::default()
    };
    match plan_moves(home, rec, &stamp, &log.backup) {
        Ok(moves) => {
            for (from, to, agent) in moves {
                if let Err(e) = move_path(&from, &to) {
                    rollback(&log);
                    return Err(import_err(
                        "import.replace_failed",
                        json!({ "slug": rec.slug }),
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
        Err(msg) => Err(import_err("import.replace_failed", json!({ "slug": rec.slug }), msg)),
    }
}

/// Undo [`retire`]: every move reversed, newest first. Whatever the failed
/// replacement already put at an original path is moved aside into
/// `<backup>/failed-new/` first (never deleted). Best effort — a failed
/// move-back is printed so the operator can finish it by hand.
pub fn rollback(log: &RetireLog) {
    for (n, (from, to)) in log.moves.iter().enumerate().rev() {
        if from.exists() {
            let aside = log.backup.join("failed-new").join(n.to_string());
            if let Err(e) = move_path(from, &aside) {
                eprintln!("  ✗ 還原失敗：{} 無法移開：{e}（請手動處理）", from.display());
                continue;
            }
        }
        if let Err(e) = std::fs::rename(to, from) {
            eprintln!(
                "  ✗ 還原失敗：{} → {}：{e}（請手動搬回）",
                to.display(),
                from.display()
            );
        }
    }
    // Only removes the backup folder when nothing is left in it.
    for sub in ["skills", "wiki", ""] {
        let _ = std::fs::remove_dir(log.backup.join(sub));
    }
}

type PlannedMove = (PathBuf, PathBuf, Option<String>);

fn plan_moves(home: &Path, rec: &InstallRecord, stamp: &str, backup: &Path) -> std::result::Result<Vec<PlannedMove>, String> {
    let mut moves: Vec<PlannedMove> = Vec::new();
    let trash = home.join("agents").join(AGENT_TRASH_DIR);
    for id in &rec.agents {
        if !crate::is_valid_agent_id(id) {
            continue;
        }
        let dir = home.join("agents").join(id);
        if dir.exists() {
            moves.push((dir, trash.join(format!("{id}_{stamp}")), Some(id.clone())));
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
    let archive = home.join("imported").join(&rec.slug);
    if archive.exists() {
        moves.push((archive, backup.join("imported"), None));
    }
    let record_dir = experts_dir(home).join(&rec.slug);
    if record_dir.exists() {
        moves.push((record_dir, backup.join("record"), None));
    }
    Ok(moves)
}

fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
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
            && let Err(e) = duduclaw_core::org_store::remove(home, id)
        {
            eprintln!("  ⚠ 組織資料移除失敗（{id}）：{e}");
        }
    }
}
