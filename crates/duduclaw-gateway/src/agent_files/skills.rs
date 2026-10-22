//! An employee's own skill files (`<home>/agents/<id>/SKILLS/**.md`) as
//! profile files people can edit.
//!
//! Only `.md` files that already exist can be edited: creating and deleting
//! skills stays with the custom-skill flow, and scripts next to a skill are
//! never touched. A skill path is validated twice: as text when it is
//! parsed (every component a single safe name, no `..`, at most
//! [`MAX_COMPONENTS`] deep) and against the disk when it is resolved (every
//! level exists under exactly that name, none is a symlink, the result stays
//! inside the employee's `SKILLS` directory).

use std::path::{Path, PathBuf};

/// The per-employee skills directory (the registry's scan root).
pub(crate) const SKILLS_DIR: &str = "SKILLS";
/// The `FileKey` string prefix.
pub(crate) const KEY_PREFIX: &str = "skill:";
/// `foo.md`, `foo/SKILL.md`, `foo/references/api.md`, one level more.
pub(crate) const MAX_COMPONENTS: usize = 4;
/// Longest relative path accepted.
pub(crate) const MAX_PATH_BYTES: usize = 200;
/// Longest single path component.
const MAX_COMPONENT_BYTES: usize = 128;
/// At most this many skill files are listed for one employee.
pub(crate) const MAX_LISTED: usize = 100;
/// At most this many entries are looked at in one folder, and the walk
/// stops once this many files were found (the folder is employee-writable).
const MAX_DIR_ENTRIES: usize = 500;
const MAX_FOUND: usize = 4 * MAX_LISTED;

/// A syntactically valid skill file path relative to `SKILLS/`, kept as the
/// full key string `skill:<rel>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SkillPath {
    key: String,
}

/// One path component: a single safe name (the skill loader's own rule),
/// printable, and free of characters some file systems cannot store.
fn valid_component(c: &str) -> bool {
    duduclaw_agent::skill_loader::is_safe_skill_name(c)
        && c.len() <= MAX_COMPONENT_BYTES
        && c.trim() == c
        && !c.chars().any(|ch| ch.is_control() || matches!(ch, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

fn is_md(name: &str) -> bool {
    name.len() > 3 && name.get(name.len() - 3..).is_some_and(|ext| ext.eq_ignore_ascii_case(".md"))
}

impl SkillPath {
    /// Validate `rel` (`foo.md`, `foo/SKILL.md`, …) as text.
    pub(crate) fn parse(rel: &str) -> Result<Self, String> {
        const BAD: &str = "invalid skill file path";
        if rel.is_empty() || rel.len() > MAX_PATH_BYTES || rel.contains('\\') {
            return Err(BAD.into());
        }
        let parts: Vec<&str> = rel.split('/').collect();
        if parts.len() > MAX_COMPONENTS || !parts.iter().all(|p| valid_component(p)) {
            return Err(BAD.into());
        }
        if !parts.last().is_some_and(|p| is_md(p)) {
            return Err("only skill text files (.md) can be edited here".into());
        }
        Ok(Self { key: format!("{KEY_PREFIX}{rel}") })
    }

    /// The `skill:<rel>` key string.
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// The path relative to `SKILLS/`.
    pub(crate) fn rel(&self) -> &str {
        self.key.get(KEY_PREFIX.len()..).unwrap_or("")
    }

    /// The skill's name as the loader names it: the folder of a `SKILL.md`,
    /// otherwise the file name without `.md`.
    pub(crate) fn skill_name(&self) -> &str {
        let rel = self.rel();
        let mut parts = rel.rsplit('/');
        let file = parts.next().unwrap_or(rel);
        if file.eq_ignore_ascii_case("SKILL.md")
            && let Some(dir) = parts.next()
        {
            return dir;
        }
        file.get(..file.len().saturating_sub(3)).unwrap_or(file)
    }

    /// The version-history folder name: a fixed-length digest of the path
    /// (the path itself may nest and may be long).
    pub(crate) fn history_dir(&self) -> String {
        let digest = super::hash_of(self.rel());
        format!("skill-{}", duduclaw_core::truncate_bytes(&digest, 24))
    }
}

/// `<agent_dir>/SKILLS`, refused when it is a symlink or not a directory.
fn skills_root(agent_dir: &Path) -> Result<PathBuf, String> {
    let root = agent_dir.join(SKILLS_DIR);
    match std::fs::symlink_metadata(&root) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(root),
        Ok(_) => Err("this employee's skills folder cannot be edited here".into()),
        Err(_) => Err("skill file not found".into()),
    }
}

/// Whether `dir` holds an entry named exactly `name` (not a case-folded
/// match on a case-insensitive disk).
fn has_exact_entry(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|e| e.file_name().to_str() == Some(name)))
        .unwrap_or(false)
}

/// The skill file on disk. It must already exist as a regular file, every
/// level must exist under exactly the given name and none may be a symlink,
/// and the canonical result must stay inside `SKILLS/`.
pub(crate) fn resolve(agent_dir: &Path, skill: &SkillPath) -> Result<PathBuf, String> {
    const NOT_FOUND: &str = "skill file not found";
    let root = skills_root(agent_dir)?;
    let parts: Vec<&str> = skill.rel().split('/').collect();
    let mut at = root.clone();
    for (i, part) in parts.iter().enumerate() {
        if !has_exact_entry(&at, part) {
            return Err(NOT_FOUND.into());
        }
        at = at.join(part);
        let m = std::fs::symlink_metadata(&at).map_err(|_| NOT_FOUND.to_string())?;
        if m.file_type().is_symlink() {
            return Err("this skill file cannot be edited here".into());
        }
        let last = i + 1 == parts.len();
        if (last && !m.is_file()) || (!last && !m.is_dir()) {
            return Err(NOT_FOUND.into());
        }
    }
    let canon_root = std::fs::canonicalize(&root).map_err(|_| NOT_FOUND.to_string())?;
    let canon = std::fs::canonicalize(&at).map_err(|_| NOT_FOUND.to_string())?;
    if !canon.starts_with(&canon_root) {
        return Err("this skill file resolves outside the skills folder".into());
    }
    Ok(at)
}

/// Every editable skill file of the employee, sorted, at most
/// [`MAX_LISTED`]. Symlinks, hidden entries, names that would not parse and
/// anything deeper than [`MAX_COMPONENTS`] are skipped.
pub(crate) fn list(agent_dir: &Path) -> Vec<SkillPath> {
    let Ok(root) = skills_root(agent_dir) else { return Vec::new() };
    let mut out: Vec<SkillPath> = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, String, usize)> = [(root, String::new(), 1)].into();
    while let Some((dir, prefix, depth)) = queue.pop_front() {
        if out.len() >= MAX_FOUND {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().take(MAX_DIR_ENTRIES).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            let Ok(ft) = e.file_type() else { continue };
            if name.starts_with('.') || ft.is_symlink() {
                continue;
            }
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if ft.is_dir() && depth < MAX_COMPONENTS && valid_component(&name) {
                queue.push_back((e.path(), rel, depth + 1));
            } else if ft.is_file()
                && let Ok(p) = SkillPath::parse(&rel)
            {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| a.rel().cmp(b.rel()));
    out.truncate(MAX_LISTED);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_strict() {
        for ok in ["notes.md", "notes/SKILL.md", "notes/references/api.md", "a/b/c/d.md", "報價/SKILL.md", "x.MD"] {
            assert!(SkillPath::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "../SOUL.md",
            "notes/../../SOUL.md",
            "/etc/passwd.md",
            "notes//SKILL.md",
            "notes\\SKILL.md",
            ".hidden.md",
            "notes/.git/x.md",
            "notes/run.py",
            "notes",
            "a/b/c/d/e.md",
            "a:b.md",
            " notes.md",
            "no\u{7}tes.md",
            &format!("{}.md", "x".repeat(200)),
        ] {
            assert!(SkillPath::parse(bad).is_err(), "{bad:?}");
        }
        let p = SkillPath::parse("notes/SKILL.md").unwrap();
        assert_eq!((p.key(), p.rel(), p.skill_name()), ("skill:notes/SKILL.md", "notes/SKILL.md", "notes"));
        assert_eq!(SkillPath::parse("quote.md").unwrap().skill_name(), "quote");
        let dir = p.history_dir();
        assert!(dir.starts_with("skill-") && dir.len() == 30 && !dir.contains('/'), "{dir}");
        assert_ne!(dir, SkillPath::parse("notes.md").unwrap().history_dir());
    }

    fn agent() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().canonicalize().unwrap().join("a1");
        std::fs::create_dir_all(dir.join("SKILLS/quote/references")).unwrap();
        std::fs::write(dir.join("SKILLS/quote/SKILL.md"), "---\nname: quote\n---\nbody").unwrap();
        std::fs::write(dir.join("SKILLS/quote/references/prices.md"), "ref").unwrap();
        std::fs::write(dir.join("SKILLS/quote/run.py"), "print(1)").unwrap();
        std::fs::write(dir.join("SKILLS/flat.md"), "flat").unwrap();
        std::fs::write(dir.join("SKILLS/.hidden.md"), "x").unwrap();
        std::fs::write(dir.join("SKILLS/flat.md.lock"), "").unwrap();
        (t, dir)
    }

    #[test]
    fn lists_only_existing_markdown() {
        let (_t, dir) = agent();
        let rels: Vec<String> = list(&dir).iter().map(|p| p.rel().to_string()).collect();
        assert_eq!(rels, vec!["flat.md", "quote/SKILL.md", "quote/references/prices.md"]);
        assert!(list(&dir.join("missing")).is_empty());
    }

    #[test]
    fn resolve_refuses_missing_wrong_case_and_symlinks() {
        let (t, dir) = agent();
        assert!(resolve(&dir, &SkillPath::parse("quote/SKILL.md").unwrap()).is_ok());
        assert!(resolve(&dir, &SkillPath::parse("missing.md").unwrap()).is_err(), "new files are not created here");
        assert!(resolve(&dir, &SkillPath::parse("QUOTE/SKILL.md").unwrap()).is_err(), "exact name only");
        #[cfg(unix)]
        {
            let outside = t.path().join("outside.md");
            std::fs::write(&outside, "secret").unwrap();
            std::os::unix::fs::symlink(&outside, dir.join("SKILLS/link.md")).unwrap();
            assert!(resolve(&dir, &SkillPath::parse("link.md").unwrap()).is_err(), "symlinked file");
            std::fs::create_dir_all(t.path().join("elsewhere")).unwrap();
            std::fs::write(t.path().join("elsewhere/SKILL.md"), "x").unwrap();
            std::os::unix::fs::symlink(t.path().join("elsewhere"), dir.join("SKILLS/linked")).unwrap();
            assert!(resolve(&dir, &SkillPath::parse("linked/SKILL.md").unwrap()).is_err(), "symlinked folder");
            let rels: Vec<String> = list(&dir).iter().map(|p| p.rel().to_string()).collect();
            assert!(!rels.iter().any(|r| r.starts_with("link")), "{rels:?}");
            // A symlinked SKILLS folder is refused as a whole.
            let other = t.path().join("a2");
            std::fs::create_dir_all(&other).unwrap();
            std::os::unix::fs::symlink(dir.join("SKILLS"), other.join("SKILLS")).unwrap();
            assert!(resolve(&other, &SkillPath::parse("flat.md").unwrap()).is_err());
            assert!(list(&other).is_empty());
        }
    }
}
