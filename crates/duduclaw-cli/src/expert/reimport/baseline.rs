//! What each imported employee looked like right after the import, so a
//! later "replace" can tell whether someone edited it since.
//!
//! Stored next to the install record as `experts/<slug>/baseline.json`:
//! one content hash per file under each employee directory. Files the
//! gateway maintains itself (hidden paths such as `.mcp.json` and
//! `.claude/`, `*.lock`, the wiki index / log / database) are left out:
//! they change at boot and while the employee works, so they would make
//! every employee look edited.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Files hashed per employee before the fingerprint gives up (and the
/// employee counts as "cannot tell" — treated as edited).
const MAX_FILES: usize = 5_000;
/// Larger files are compared by size and modification time only.
const MAX_HASHED_BYTES: u64 = 64 * 1024 * 1024;

const BASELINE_FILE: &str = "baseline.json";

/// `relative path → content digest` for one employee directory.
pub type Fingerprint = BTreeMap<String, String>;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Baseline {
    #[serde(default)]
    pub agents: BTreeMap<String, Fingerprint>,
}

/// Hash every regular file under `dir` (links skipped). `None` when the
/// tree is too large to fingerprint.
pub fn fingerprint(dir: &Path) -> Option<Fingerprint> {
    let mut out = Fingerprint::new();
    walk(dir, dir, &mut out)?;
    Some(out)
}

fn walk(base: &Path, dir: &Path, out: &mut Fingerprint) -> Option<()> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Some(());
    };
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(base) else { continue };
        let rel_str = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if is_machine_managed(&rel_str) {
            continue;
        }
        if ft.is_dir() {
            walk(base, &path, out)?;
        } else if ft.is_file() {
            if out.len() >= MAX_FILES {
                return None;
            }
            out.insert(rel_str, digest(&path));
        }
    }
    Some(())
}

/// Files DuDuClaw itself writes into an employee directory while it runs:
/// any hidden component (`.mcp.json`, `.claude/`, `wiki/.delivery_fence.*`),
/// lock files (`agent.toml.lock`, `wiki/_index.lock`) and the wiki's own
/// index, log and database. They change without anyone editing the
/// employee, so they never count as an edit.
fn is_machine_managed(rel: &str) -> bool {
    let hidden = rel.split('/').any(|c| c.starts_with('.'));
    let lock = rel.ends_with(".lock");
    let wiki_system = matches!(rel, "wiki/_index.md" | "wiki/_log.md") || rel.starts_with("wiki/_wiki.db");
    hidden || lock || wiki_system
}

fn digest(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    if len > MAX_HASHED_BYTES {
        let mtime = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return format!("size:{len}:mtime:{mtime}");
    }
    match std::fs::read(path) {
        Ok(bytes) => crate::expert::registry::sha256_hex(&bytes),
        Err(_) => "unreadable".to_string(),
    }
}

/// Write the baseline for `agents` of the import `slug` (after a real run).
pub fn write(home: &Path, slug: &str, agents: &[String]) -> Result<(), String> {
    let mut baseline = Baseline::default();
    for id in agents {
        if let Some(fp) = fingerprint(&home.join("agents").join(id)) {
            baseline.agents.insert(id.clone(), fp);
        }
    }
    let dir = crate::expert::experts_dir(home).join(slug);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建立 {} 失敗: {e}", dir.display()))?;
    let body = serde_json::to_string_pretty(&baseline).map_err(|e| format!("序列化失敗: {e}"))?;
    std::fs::write(dir.join(BASELINE_FILE), body).map_err(|e| format!("寫入匯入基準失敗: {e}"))
}

/// The stored baseline, `None` for imports made before baselines existed.
pub fn read(home: &Path, slug: &str) -> Option<Baseline> {
    let raw = std::fs::read_to_string(crate::expert::experts_dir(home).join(slug).join(BASELINE_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// How one file differs from the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Modified,
    Added,
    Removed,
    /// No baseline: changed after the import time (by modification time).
    ChangedAfterImport,
    /// Nothing could be compared (unreadable or too large a tree).
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileChange {
    pub path: String,
    pub change: ChangeKind,
}

/// Files that differ between `before` and `now`, sorted by path.
/// Machine-managed paths are skipped on both sides, so a baseline written
/// with an older exclusion list does not report them as removed.
pub fn diff(before: &Fingerprint, now: &Fingerprint) -> Vec<FileChange> {
    let mut out = Vec::new();
    let before: Fingerprint = before
        .iter()
        .filter(|(p, _)| !is_machine_managed(p))
        .map(|(p, d)| (p.clone(), d.clone()))
        .collect();
    for (path, digest) in now.iter().filter(|(p, _)| !is_machine_managed(p)) {
        match before.get(path) {
            None => out.push(FileChange { path: path.clone(), change: ChangeKind::Added }),
            Some(d) if d != digest => out.push(FileChange { path: path.clone(), change: ChangeKind::Modified }),
            Some(_) => {}
        }
    }
    for path in before.keys() {
        if !now.contains_key(path) {
            out.push(FileChange { path: path.clone(), change: ChangeKind::Removed });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// No-baseline fallback: files under `dir` modified more than a grace
/// period after `installed_at` (RFC 3339). An unparsable time fails closed
/// with a single `Unknown` line.
pub fn changed_after(dir: &Path, installed_at: &str) -> Vec<FileChange> {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(installed_at) else {
        return vec![FileChange { path: String::new(), change: ChangeKind::Unknown }];
    };
    // The import itself keeps writing for a moment after the record time.
    let cutoff = at.timestamp().saturating_add(120);
    let Some(fp) = fingerprint(dir) else {
        return vec![FileChange { path: String::new(), change: ChangeKind::Unknown }];
    };
    let mut out = Vec::new();
    for path in fp.keys() {
        let modified = std::fs::metadata(dir.join(path))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(i64::MAX);
        if modified > cutoff {
            out.push(FileChange { path: path.clone(), change: ChangeKind::ChangedAfterImport });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_skips_runtime_dotfiles_and_diff_classifies_changes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("SOUL.md"), "a").unwrap();
        std::fs::write(d.path().join(".mcp.json"), "{}").unwrap();
        std::fs::create_dir_all(d.path().join(".claude")).unwrap();
        std::fs::write(d.path().join(".claude/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(d.path().join("wiki")).unwrap();
        std::fs::write(d.path().join("wiki/a.md"), "x").unwrap();
        // What the gateway writes while it runs (seen in a live test).
        for f in ["agent.toml.lock", "wiki/_index.lock", "wiki/.delivery_fence.epoch", "wiki/_index.md", "wiki/_log.md", "wiki/_wiki.db"] {
            std::fs::write(d.path().join(f), "runtime").unwrap();
        }
        let before = fingerprint(d.path()).unwrap();
        assert_eq!(before.keys().cloned().collect::<Vec<_>>(), ["SOUL.md", "wiki/a.md"]);

        std::fs::write(d.path().join("SOUL.md"), "b").unwrap();
        std::fs::write(d.path().join(".mcp.json"), "{\"x\":1}").unwrap();
        std::fs::remove_file(d.path().join("wiki/a.md")).unwrap();
        std::fs::write(d.path().join("notes.md"), "n").unwrap();
        let now = fingerprint(d.path()).unwrap();
        // A baseline taken before a path was known to be machine-managed
        // does not report it as removed.
        let mut older = before.clone();
        older.insert("wiki/_index.md".into(), "x".into());
        assert_eq!(diff(&older, &before), vec![]);
        let changes = diff(&before, &now);
        assert_eq!(
            changes,
            vec![
                FileChange { path: "SOUL.md".into(), change: ChangeKind::Modified },
                FileChange { path: "notes.md".into(), change: ChangeKind::Added },
                FileChange { path: "wiki/a.md".into(), change: ChangeKind::Removed },
            ]
        );
    }

    #[test]
    fn no_baseline_falls_back_to_modification_time() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("SOUL.md"), "a").unwrap();
        let long_ago = "2020-01-01T00:00:00+00:00";
        assert_eq!(changed_after(d.path(), long_ago).len(), 1);
        let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        assert!(changed_after(d.path(), &future).is_empty());
        let bad = changed_after(d.path(), "not a time");
        assert_eq!(bad[0].change, ChangeKind::Unknown);
    }
}
