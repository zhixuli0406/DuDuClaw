//! Removing a skill from an employee's own layer without deleting it.
//!
//! "Remove" moves the skill's files out of `<agent_dir>/SKILLS/` into
//! `<home>/skill_archive/<agent_id>/<archive_id>/`, next to a `manifest.json`
//! that records what was moved and by whom. [`restore`] moves them back.
//! Only the employee's own layer can be removed; company-wide and department
//! skills are shared with other employees and can only be switched off per
//! employee (see [`crate::skill_state`]).
//!
//! Locating a skill's files follows the loader's naming rules exactly
//! (`<name>/SKILL.md` anywhere below the root, or a top-level `<name>.md`, with
//! names compared case-insensitively like the loader's de-duplication), so
//! what gets moved is what the loader would have loaded under that name.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::skill_state::validate_skill_name;

/// Directory under `<home>` holding removed skills, one subdirectory per employee.
pub const SKILL_ARCHIVE_DIR: &str = "skill_archive";
const MANIFEST: &str = "manifest.json";
const MAX_DEPTH: usize = 8;
/// Upper bound on archived entries listed per employee.
pub const MAX_ARCHIVE_LISTED: usize = 500;
/// Upper bound on directory entries walked while locating one skill.
const MAX_SCANNED_ENTRIES: usize = 20_000;

/// `path` must be a directory that is not itself a symbolic link.
fn require_real_dir(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err("the skills folder is a link; refusing to move files through it".into()),
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err("the skills folder is not a folder".into()),
        Err(e) => Err(format!("read the skills folder: {e}")),
    }
}

/// Every existing directory between `root` and `rel` (exclusive of `rel`
/// itself) must be a real directory, not a symbolic link, so a move can
/// never be redirected outside the skills folder. Checked again right
/// before each move.
fn require_no_linked_parents(root: &Path, rel: &Path) -> Result<(), String> {
    require_real_dir(root)?;
    let mut cur = root.to_path_buf();
    let parents: Vec<_> = rel.parent().map(|p| p.components().collect()).unwrap_or_default();
    for comp in parents {
        cur.push(comp.as_os_str());
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(format!("'{}' is a link or not a folder; refusing to move files through it", rel.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("read '{}': {e}", rel.display())),
        }
    }
    Ok(())
}

/// Create the missing directories above `rel` one level at a time, refusing
/// links, so restoring never writes through a link an employee planted.
fn create_parents(root: &Path, rel: &Path) -> Result<(), String> {
    let mut cur = root.to_path_buf();
    let parents: Vec<_> = rel.parent().map(|p| p.components().collect()).unwrap_or_default();
    for comp in parents {
        cur.push(comp.as_os_str());
        match std::fs::create_dir(&cur) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create a folder for '{}': {e}", rel.display())),
        }
    }
    require_no_linked_parents(root, rel)
}

/// What one archive holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchiveManifest {
    pub archive_id: String,
    pub agent_id: String,
    pub skill_name: String,
    pub archived_at: String,
    pub archived_by: String,
    /// File or directory names moved out of `SKILLS/`, relative to it.
    pub entries: Vec<String>,
}

/// `<home>/skill_archive/<agent_id>/`.
pub fn agent_archive_dir(home: &Path, agent_id: &str) -> Result<PathBuf, String> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Err(format!("invalid agent id '{agent_id}'"));
    }
    Ok(home.join(SKILL_ARCHIVE_DIR).join(agent_id))
}

/// Files and directories (relative to `skills_dir`) that make up the skill
/// called `name`. Empty when the employee's own layer has no such skill.
pub fn locate(skills_dir: &Path, name: &str) -> Result<Vec<PathBuf>, String> {
    validate_skill_name(name)?;
    match std::fs::symlink_metadata(skills_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        _ => require_real_dir(skills_dir)?,
    }
    let root = std::fs::canonicalize(skills_dir).map_err(|e| format!("read the skills folder: {e}"))?;
    let mut found: Vec<PathBuf> = Vec::new();
    let mut seen_entries = 0usize;
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(root.clone(), 0)]);
    while let Some((dir, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            seen_entries += 1;
            if seen_entries > MAX_SCANNED_ENTRIES {
                return Err("the skills folder holds too many files to search".into());
            }
            let file_name = entry.file_name();
            let Some(fname) = file_name.to_str() else { continue };
            if fname.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.is_dir() {
                let Ok(resolved) = std::fs::canonicalize(&path) else { continue };
                if resolved.starts_with(&root) {
                    queue.push_back((path, depth + 1));
                }
                continue;
            }
            if let Some(hit) = skill_entry_for(&root, &dir, &path, fname, depth, name)
                && !found.contains(&hit)
            {
                found.push(hit);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The entry to move when `path` is a skill body named `name`.
fn skill_entry_for(
    root: &Path,
    dir: &Path,
    path: &Path,
    fname: &str,
    depth: usize,
    name: &str,
) -> Option<PathBuf> {
    let is_md = Path::new(fname)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md"));
    if !is_md {
        return None;
    }
    if fname.eq_ignore_ascii_case("SKILL.md") {
        // A SKILL.md directly in the root has no skill directory to move.
        if dir == root {
            return None;
        }
        let dir_name = dir.file_name()?.to_str()?;
        if !dir_name.eq_ignore_ascii_case(name) {
            return None;
        }
        return dir.strip_prefix(root).ok().map(Path::to_path_buf);
    }
    if depth == 0 {
        let stem = path.file_stem()?.to_str()?;
        if stem.eq_ignore_ascii_case(name) {
            return Some(PathBuf::from(fname));
        }
    }
    None
}

/// Whether `dir` holds another skill's `SKILL.md` below its top level.
fn holds_nested_skill(dir: &Path) -> bool {
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(dir.to_path_buf(), 0)]);
    while let Some((d, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                queue.push_back((path, depth + 1));
            } else if depth > 0
                && entry.file_name().to_str().is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"))
            {
                return true;
            }
        }
    }
    false
}

/// Move the skill `name` out of `skills_dir` into a new archive for
/// `agent_id`. Errors when the employee's own layer has no such skill.
pub fn archive(
    home: &Path,
    agent_id: &str,
    skills_dir: &Path,
    name: &str,
    archived_by: &str,
) -> Result<ArchiveManifest, String> {
    let entries = locate(skills_dir, name)?;
    if entries.is_empty() {
        return Err(format!("'{name}' is not one of this employee's own skills"));
    }
    for rel in &entries {
        require_no_linked_parents(skills_dir, rel)?;
        let abs = skills_dir.join(rel);
        if abs.is_dir() && holds_nested_skill(&abs) {
            return Err(format!("'{name}' contains other skills; move them out first"));
        }
    }
    let archive_id = new_archive_id();
    let dest = agent_archive_dir(home, agent_id)?.join(&archive_id);
    std::fs::create_dir_all(&dest).map_err(|e| format!("create archive folder: {e}"))?;
    let manifest = ArchiveManifest {
        archive_id,
        agent_id: agent_id.to_string(),
        skill_name: name.to_string(),
        archived_at: chrono::Utc::now().to_rfc3339(),
        archived_by: archived_by.to_string(),
        entries: entries.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
    };
    // The manifest goes first: an interrupted move still leaves a listed
    // archive instead of files nobody can find.
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::write(dest.join(MANIFEST), text) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(format!("write archive manifest: {e}"));
    }
    let mut moved: Vec<String> = Vec::new();
    for (i, rel) in entries.iter().enumerate() {
        let target_name = format!("{i}-{}", flatten(rel));
        let moved_ok = require_no_linked_parents(skills_dir, rel)
            .and_then(|()| {
                std::fs::rename(skills_dir.join(rel), dest.join(&target_name))
                    .map_err(|e| format!("move '{}' to the archive: {e}", rel.display()))
            });
        if let Err(e) = moved_ok {
            rollback(skills_dir, &dest, &entries, &moved);
            return Err(e);
        }
        moved.push(target_name);
    }
    Ok(manifest)
}

fn rollback(skills_dir: &Path, dest: &Path, entries: &[PathBuf], moved: &[String]) {
    for (rel, stored) in entries.iter().zip(moved) {
        let _ = std::fs::rename(dest.join(stored), skills_dir.join(rel));
    }
    let _ = std::fs::remove_dir_all(dest);
}

/// `group/foo` → `group__foo`, so nested entries keep a unique flat name.
fn flatten(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("__")
}

fn new_archive_id() -> String {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ");
    let rand = uuid::Uuid::new_v4().simple().to_string();
    format!("{stamp}-{}", &rand[..8])
}

fn valid_archive_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Archived skills of one employee, newest first.
pub fn list(home: &Path, agent_id: &str) -> Result<Vec<ArchiveManifest>, String> {
    let dir = agent_archive_dir(home, agent_id)?;
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
    let mut out: Vec<ArchiveManifest> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|id| valid_archive_id(id))
        .filter_map(|id| read_manifest(&dir, &id).ok())
        .collect();
    out.sort_by(|a, b| b.archive_id.cmp(&a.archive_id));
    out.truncate(MAX_ARCHIVE_LISTED);
    Ok(out)
}

fn read_manifest(dir: &Path, archive_id: &str) -> Result<ArchiveManifest, String> {
    let text = std::fs::read_to_string(dir.join(archive_id).join(MANIFEST))
        .map_err(|e| format!("read archive manifest: {e}"))?;
    let m: ArchiveManifest =
        serde_json::from_str(&text).map_err(|e| format!("archive manifest is not valid: {e}"))?;
    if m.archive_id != archive_id {
        return Err("archive manifest does not match its directory".into());
    }
    Ok(m)
}

/// Move an archived skill back into `skills_dir`. Refuses when any of its
/// files would overwrite something already there.
pub fn restore(
    home: &Path,
    agent_id: &str,
    skills_dir: &Path,
    archive_id: &str,
) -> Result<ArchiveManifest, String> {
    if !valid_archive_id(archive_id) {
        return Err("invalid archive id".into());
    }
    let dir = agent_archive_dir(home, agent_id)?;
    let manifest = read_manifest(&dir, archive_id)?;
    if manifest.agent_id != agent_id {
        return Err("archive belongs to another employee".into());
    }
    let src = dir.join(archive_id);
    match std::fs::symlink_metadata(skills_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(skills_dir).map_err(|e| format!("create the skills folder: {e}"))?;
        }
        _ => require_real_dir(skills_dir)?,
    }
    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (i, rel) in manifest.entries.iter().enumerate() {
        let rel_path = safe_relative(rel)?;
        let stored = src.join(format!("{i}-{}", flatten(&rel_path)));
        if std::fs::symlink_metadata(&stored).is_err() {
            return Err(format!("the archive is missing '{}'", rel_path.display()));
        }
        require_no_linked_parents(skills_dir, &rel_path)?;
        if std::fs::symlink_metadata(skills_dir.join(&rel_path)).is_ok() {
            return Err(format!("'{}' already exists; rename or remove it first", rel_path.display()));
        }
        pairs.push((stored, rel_path));
    }
    let mut done: Vec<usize> = Vec::new();
    for (i, (stored, rel)) in pairs.iter().enumerate() {
        let target = skills_dir.join(rel);
        let step = create_parents(skills_dir, rel).and_then(|()| {
            if std::fs::symlink_metadata(&target).is_ok() {
                return Err(format!("'{}' already exists; rename or remove it first", rel.display()));
            }
            std::fs::rename(stored, &target).map_err(|e| format!("move '{}' back: {e}", rel.display()))
        });
        if let Err(e) = step {
            // Put back what was already restored so the archive stays whole.
            for &j in done.iter().rev() {
                let (s, r) = &pairs[j];
                let _ = std::fs::rename(skills_dir.join(r), s);
            }
            return Err(e);
        }
        done.push(i);
    }
    std::fs::remove_dir_all(&src).map_err(|e| format!("clean up archive: {e}"))?;
    Ok(manifest)
}

/// A manifest entry as a relative path with no `..`, root or prefix parts.
fn safe_relative(rel: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(rel);
    let ok = !rel.is_empty()
        && p.components().all(|c| matches!(c, std::path::Component::Normal(_)));
    if ok { Ok(p) } else { Err(format!("archive entry '{rel}' is not a safe relative path")) }
}

#[cfg(test)]
#[path = "skill_archive_tests.rs"]
mod tests;
