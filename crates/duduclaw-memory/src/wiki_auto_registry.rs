//! Gateway-owned registry of auto pages (v1.84.0, RM-200 review H1 item 3).
//!
//! An auto page (`agents/<id>/wiki/auto/**`) carries host-owned front matter
//! (`host_sources`, `host_promoted`) that decides which conversation may see
//! it. Those lines are only text in a file inside the employee's directory,
//! so their syntactic presence proves nothing: a file planted there by any
//! process that can write the directory could carry them too. This registry
//! binds them to something an employee process cannot produce: a record
//! kept OUTSIDE every employee directory, at
//! `<home>/state/wiki-auto/<agent-id>.json` (directory 0700, file 0600; under
//! `<home>/state/` next to the gateway's other state, refused to employees by
//! the agent-file-guard's `<home>`-state rule), mapping each auto page's
//! wiki-relative path to the sha256 of the bytes the gateway wrote and the
//! host fields it wrote with them:
//!
//! ```json
//! { "version": 1,
//!   "pages": { "auto/sop/refunds.md": {
//!       "sha256": "<64 hex>",
//!       "host_sources": ["conversation:telegram:A:m:1"],
//!       "host_promoted": false } } }
//! ```
//!
//! Writers (all in the gateway process or the operator's CLI, never through
//! an employee tool): the auto writer (`auto_wiki_page::write_auto_page`),
//! the dashboard promotion (`auto_wiki_page::promote_page`), and the
//! maintenance rewrites of an existing page (janitor auto-correct tagging,
//! trust snapshot sync — [`rerecord_if_current`], which only moves an entry
//! that matched the bytes being rewritten). Deleting or archiving an auto
//! page through [`crate::WikiStore`] drops its entry.
//!
//! A reader loads the registry once per call ([`AutoPageRegistry::load`]) and
//! asks [`AutoPageRegistry::current`] per page. A missing, unreadable or
//! malformed registry is an empty one: every auto page is hidden from every
//! conversation (fail closed) until re-confirmed from the dashboard.
//!
//! The per-agent auto-page write lock lives beside the other gateway locks,
//! at `<home>/locks/wiki-auto/<agent-id>.lock` ([`lock_auto_pages`], review
//! M1), not in the employee's wiki directory.

use std::collections::{BTreeMap, BTreeSet};
use duduclaw_core::wiki_auto_namespace::canonical_page_id;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::warn;

use duduclaw_core::fs_lock::FileLockGuard;

/// `<home>/<STATE_DIR>/<REGISTRY_DIR>/<agent-id>.json`.
pub const STATE_DIR: &str = "state";
/// Directory of the per-agent registries under [`STATE_DIR`].
pub const REGISTRY_DIR: &str = "wiki-auto";
/// `<home>/<LOCKS_DIR>/<LOCK_DIR>/<agent-id>.lock` (review M1).
pub const LOCKS_DIR: &str = "locks";
/// Directory of the per-agent auto-page locks under [`LOCKS_DIR`].
pub const LOCK_DIR: &str = "wiki-auto";
/// Format version this build reads and writes. Another value reads as empty.
pub const REGISTRY_VERSION: u32 = 1;
/// Upper bound on waiting for the auto-page lock. Fail closed on expiry.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// What the gateway recorded for one auto page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoPageRecord {
    /// Lowercase hex sha256 of the page bytes exactly as the gateway wrote them.
    pub sha256: String,
    /// The `host_sources` written with those bytes.
    #[serde(default)]
    pub host_sources: Vec<String>,
    /// The `host_promoted` mark written with those bytes.
    #[serde(default)]
    pub host_promoted: bool,
}

/// The registry of one agent's auto pages, keyed by wiki-relative path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoPageRegistry {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub pages: BTreeMap<String, AutoPageRecord>,
}

impl AutoPageRegistry {
    /// Load the registry of `agent_id` under `home`. Missing, unreadable,
    /// malformed or of another version → empty (every auto page hidden).
    pub fn load(home: &Path, agent_id: &str) -> Self {
        let Some(path) = registry_path(home, agent_id) else {
            return Self::default();
        };
        match read_registry(&path) {
            Ok(reg) => reg,
            Err(e) => {
                warn!(agent = agent_id, "auto page registry unreadable, treated as empty: {e}");
                Self::default()
            }
        }
    }

    /// The record of `path`, whatever the file holds now.
    pub fn record(&self, path: &str) -> Option<&AutoPageRecord> {
        let key = canonical_page_id(path)?;
        let mut found = None;
        for (stored, record) in &self.pages {
            if canonical_page_id(stored).as_deref() == Some(key.as_str()) {
                if found.is_some_and(|previous| previous != record) {
                    return None; // Conflicting aliases never grant visibility.
                }
                found = Some(record);
            }
        }
        found
    }

    /// The record of `path` when `raw` is exactly the bytes it was recorded
    /// for; `None` for a page the gateway did not write as it is now.
    pub fn current(&self, path: &str, raw: &str) -> Option<&AutoPageRecord> {
        self.record(path).filter(|r| r.sha256 == page_digest(raw))
    }

    /// Without a conversation identity, only a current host-promoted page
    /// is readable. Require the exact frontmatter mark as conversation readers
    /// do, even if a malformed registry claims otherwise.
    pub fn current_promoted(&self, path: &str, raw: &str) -> bool {
        self.current(path, raw).is_some_and(|record| record.host_promoted)
            && page_host_promoted(raw)
    }

}

/// Shared interpretation of the host-owned promotion mark.
pub fn page_host_promoted(content: &str) -> bool {
    let Some(rest) = content.strip_prefix("---\n").or_else(|| content.strip_prefix("---\r\n")) else {
        return false;
    };
    let Some((fm, _)) = rest.split_once("\n---") else { return false; };
    fm.lines()
        .find(|line| line.strip_prefix("host_promoted")
            .is_some_and(|rest| rest.trim_start().starts_with(':')))
        .and_then(|line| line.split_once(':'))
        .is_some_and(|(_, value)| value.trim() == "true")
}

fn registry_key(path: &str) -> io::Result<String> {
    canonical_page_id(path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ambiguous wiki page id"))
}

/// Lowercase hex sha256 of a page's bytes.
pub fn page_digest(raw: &str) -> String {
    crate::lineage::sha256_hex(raw.as_bytes())
}

/// `<home>/state/wiki-auto/<agent-id>.json`, or `None` for an invalid id.
pub fn registry_path(home: &Path, agent_id: &str) -> Option<PathBuf> {
    duduclaw_core::is_valid_agent_id(agent_id)
        .then(|| home.join(STATE_DIR).join(REGISTRY_DIR).join(format!("{agent_id}.json")))
}

/// `<home>/locks/wiki-auto/<agent-id>.lock`, or `None` for an invalid id.
pub fn lock_path(home: &Path, agent_id: &str) -> Option<PathBuf> {
    duduclaw_core::is_valid_agent_id(agent_id)
        .then(|| home.join(LOCKS_DIR).join(LOCK_DIR).join(format!("{agent_id}.lock")))
}

/// `(home, agent_id)` of an employee wiki laid out as
/// `<home>/agents/<agent-id>/wiki`; `None` for any other directory (the
/// shared wiki, a test directory, an ephemeral scaffold).
pub fn owner_of_wiki(wiki_dir: &Path) -> Option<(PathBuf, String)> {
    if wiki_dir.file_name()? != "wiki" {
        return None;
    }
    let agent_dir = wiki_dir.parent()?;
    let agent_id = agent_dir.file_name()?.to_str()?.to_string();
    let agents = agent_dir.parent()?;
    if agents.file_name()? != "agents" || !duduclaw_core::is_valid_agent_id(&agent_id) {
        return None;
    }
    Some((agents.parent()?.to_path_buf(), agent_id))
}

/// Whether a wiki-relative path is in the auto namespace (first segment as a
/// file system opens it, see `duduclaw_core::wiki_auto_namespace`).
pub fn is_auto_rel(path: &str) -> bool {
    duduclaw_core::wiki_auto_namespace::is_auto_page_path(path)
}

/// Record (create or replace) the entry of `rel`: `written` is the exact page
/// content the gateway just wrote, with these host fields.
pub fn record_page(
    home: &Path,
    agent_id: &str,
    rel: &str,
    written: &str,
    host_sources: &[String],
    host_promoted: bool,
) -> io::Result<()> {
    let rel = registry_key(rel)?;
    let record = AutoPageRecord {
        sha256: page_digest(written),
        host_sources: host_sources.to_vec(),
        host_promoted,
    };
    update(home, agent_id, true, |reg| {
        reg.pages.insert(rel, record);
        true
    })
    .map(|_| ())
}

/// A maintenance rewrite of `rel` (janitor tagging, trust snapshot sync):
/// when the entry matched `old_raw` — the bytes the rewrite was derived from
/// — it now records `new_written` (host fields unchanged: the rewrite carries
/// them over). A page whose bytes the registry did not match stays
/// unrecorded, so a rewrite never legitimises content the gateway did not
/// write. Returns whether the entry moved.
pub fn rerecord_if_current(
    home: &Path,
    agent_id: &str,
    rel: &str,
    old_raw: &str,
    new_written: &str,
) -> io::Result<bool> {
    let rel = registry_key(rel)?;
    let old = page_digest(old_raw);
    let new = page_digest(new_written);
    update(home, agent_id, false, |reg| match reg.pages.get_mut(&rel) {
        Some(entry) if entry.sha256 == old => {
            entry.sha256 = new;
            true
        }
        _ => false,
    })
}

/// Drop the entry of `rel` (the page was deleted or archived). No registry
/// file is created for this.
pub fn forget_page(home: &Path, agent_id: &str, rel: &str) -> io::Result<()> {
    let rel = registry_key(rel)?;
    update(home, agent_id, false, |reg| {
        let before = reg.pages.len();
        // Revocation is conservative even for case-insensitive filesystem
        // aliases. Never leave a lowercase grant behind after deleting via
        // an uppercase spelling; unlike lookup, removing extra grants is safe.
        let folded: Vec<String> = rel.split(['/', '\\'])
            .map(duduclaw_core::wiki_auto_namespace::fold_segment).collect();
        reg.pages.retain(|path, _| {
            let key: Vec<String> = path.split(['/', '\\'])
                .map(duduclaw_core::wiki_auto_namespace::fold_segment).collect();
            key != folded
        });
        reg.pages.len() != before
    }).map(|_| ())
}

/// [`rerecord_if_current`] for the wiki at `wiki_dir`, when it is an
/// employee wiki ([`owner_of_wiki`]) and `rel` an auto page; a no-op
/// `Ok(false)` otherwise.
pub fn rerecord_in_wiki(wiki_dir: &Path, rel: &str, old_raw: &str, new_written: &str) -> io::Result<bool> {
    match owner_of_wiki(wiki_dir) {
        Some((home, agent)) if is_auto_rel(rel) => rerecord_if_current(&home, &agent, rel, old_raw, new_written),
        _ => Ok(false),
    }
}

/// [`forget_page`] for the wiki at `wiki_dir` (see [`rerecord_in_wiki`]).
pub fn forget_in_wiki(wiki_dir: &Path, rel: &str) -> io::Result<()> {
    match owner_of_wiki(wiki_dir) {
        Some((home, agent)) if is_auto_rel(rel) => forget_page(&home, &agent, rel),
        _ => Ok(()),
    }
}

/// Read-modify-write the registry under its file lock
/// (`<agent-id>.json.lock` beside it). `f` returns whether it changed
/// anything; nothing is written otherwise. `create = false` leaves an absent
/// registry absent.
fn update(
    home: &Path,
    agent_id: &str,
    create: bool,
    f: impl FnOnce(&mut AutoPageRegistry) -> bool,
) -> io::Result<bool> {
    let path = registry_path(home, agent_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid agent id"))?;
    if !create && std::fs::symlink_metadata(&path).is_err() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir)?;
    }
    duduclaw_core::with_file_lock(&path, || {
        let mut reg = match read_registry(&path) {
            Ok(reg) => reg,
            Err(e) => {
                // Unreadable entries are hidden either way; rebuilding from
                // what this write knows keeps the rest fail-closed.
                warn!(agent = agent_id, "auto page registry unreadable, rebuilt: {e}");
                AutoPageRegistry::default()
            }
        };
        if !f(&mut reg) {
            return Ok(false);
        }
        reg.version = REGISTRY_VERSION;
        write_registry(&path, &reg)?;
        Ok(true)
    })
}

/// Missing → empty; a link, a non-file, bad JSON or another version → error.
fn read_registry(path: &Path) -> io::Result<AutoPageRegistry> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AutoPageRegistry::default()),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "registry is not a regular file"));
    }
    let raw = std::fs::read_to_string(path)?;
    let reg: AutoPageRegistry = serde_json::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    if reg.version != REGISTRY_VERSION {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("registry version {}", reg.version)));
    }
    // Upgrade old Windows keys in memory before any read or mutation. Drop
    // every member of a conflicting alias set, never pick a permissive one.
    let mut pages = BTreeMap::new();
    let mut conflicts = BTreeSet::new();
    for (path, record) in reg.pages {
        let Some(key) = canonical_page_id(&path) else { continue; };
        if conflicts.contains(&key) { continue; }
        if pages.get(&key).is_some_and(|old| old != &record) {
            pages.remove(&key);
            conflicts.insert(key);
        } else {
            pages.insert(key, record);
        }
    }
    Ok(AutoPageRegistry { version: reg.version, pages })
}

/// Atomic replace (O_EXCL temp file + rename, never through a planted link),
/// then owner-only permissions.
fn write_registry(path: &Path, reg: &AutoPageRegistry) -> io::Result<()> {
    let json = serde_json::to_string_pretty(reg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    crate::wiki_path::atomic_write_nofollow(path, &json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Create `dir` (and its parents) and make it owner-only. A link at `dir`
/// is refused.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "not a private directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Take the per-agent auto-page write lock (review M1):
/// `<home>/locks/wiki-auto/<agent-id>.lock`, directory 0700, opened without
/// following a link and checked to be a regular file, then locked with the
/// shared primitive (`duduclaw_core::fs_lock::lock_handle_within`, bounded
/// wait). After the wait the locked handle must still be the file at the
/// path, otherwise the lock is taken again. Every failure is an error
/// (callers fail closed).
pub fn lock_auto_pages(home: &Path, agent_id: &str) -> io::Result<FileLockGuard> {
    let path = lock_path(home, agent_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid agent id"))?;
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir)?;
    }
    for _ in 0..3 {
        let file = open_lock_nofollow(&path)?;
        let guard = duduclaw_core::fs_lock::lock_handle_within(file, LOCK_WAIT)?;
        if still_linked(guard.file(), &path)? {
            return Ok(guard);
        }
    }
    Err(io::Error::other("auto page lock file keeps being replaced"))
}

fn open_lock_nofollow(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "auto page lock is not a regular file"));
    }
    Ok(file)
}

/// Whether the locked `file` is still the directory entry at `path`.
fn still_linked(file: &std::fs::File, path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata()?;
        match std::fs::symlink_metadata(path) {
            Ok(now) => Ok(now.is_file() && now.dev() == held.dev() && now.ino() == held.ino()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_matches_legacy_separator_keys_and_refuses_conflicts() {
        let home = tempfile::tempdir().unwrap();
        let file = registry_path(home.path(), "agnes").unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let record = AutoPageRecord {
            sha256: page_digest("v1"), host_sources: vec!["conversation:telegram:A:m:1".into()], host_promoted: false,
        };
        let mut legacy = AutoPageRegistry { version: REGISTRY_VERSION, pages: BTreeMap::new() };
        legacy.pages.insert(r"auto\sop\x.md".into(), record.clone());
        for path in ["auto/sop/x.md", r"auto\sop\x.md", r"auto/sop\x.md", r".\auto\sop\x.md", r"auto\\sop\x.md"] {
            assert_eq!(legacy.current(path, "v1"), Some(&record), "{path}");
        }
        for path in [r"a\..\auto\sop\x.md", r"AUTO\sop\x.md", "auto./sop/x.md", "auto:stream/sop/x.md"] {
            assert!(legacy.current(path, "v1").is_none(), "{path}");
        }
        std::fs::write(&file, serde_json::to_string(&legacy).unwrap()).unwrap();
        assert!(rerecord_if_current(home.path(), "agnes", "auto/sop/x.md", "v1", "v2").unwrap());
        let reg = AutoPageRegistry::load(home.path(), "agnes");
        assert!(reg.pages.contains_key("auto/sop/x.md"));
        assert!(reg.current(r"auto\sop\x.md", "v2").is_some());
        forget_page(home.path(), "agnes", r"AUTO\SOP\X.MD").unwrap();
        assert!(AutoPageRegistry::load(home.path(), "agnes").pages.is_empty());
        let mut conflicting = record.clone();
        conflicting.host_promoted = true;
        legacy.pages.insert("auto/sop/x.md".into(), conflicting);
        assert!(legacy.current("auto/sop/x.md", "v1").is_none());
        std::fs::write(&file, serde_json::to_string(&legacy).unwrap()).unwrap();
        assert!(AutoPageRegistry::load(home.path(), "agnes").pages.is_empty());
    }

    #[test]
    fn registry_lives_outside_every_employee_directory() {
        let home = tempfile::tempdir().unwrap();
        let reg = registry_path(home.path(), "agnes").unwrap();
        let lock = lock_path(home.path(), "agnes").unwrap();
        assert_eq!(reg, home.path().join("state/wiki-auto/agnes.json"));
        assert_eq!(lock, home.path().join("locks/wiki-auto/agnes.lock"));
        for p in [&reg, &lock] {
            assert!(!p.starts_with(home.path().join("agents")), "{}", p.display());
        }
        assert!(registry_path(home.path(), "../x").is_none());
        assert!(lock_path(home.path(), "").is_none());
    }

    #[test]
    fn record_rerecord_and_forget() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert_eq!(AutoPageRegistry::load(h, "agnes"), AutoPageRegistry::default());
        record_page(h, "agnes", "auto/sop/x.md", "v1", &["conversation:telegram:A:m:1".into()], false).unwrap();
        let reg = AutoPageRegistry::load(h, "agnes");
        assert!(reg.current("auto/sop/x.md", "v1").is_some());
        assert!(reg.current("auto/sop/x.md", "v1 edited").is_none());
        assert!(reg.current("auto/sop/y.md", "v1").is_none());
        // A rewrite of bytes the registry did not record moves nothing.
        assert!(!rerecord_if_current(h, "agnes", "auto/sop/x.md", "forged", "forged 2").unwrap());
        assert!(rerecord_if_current(h, "agnes", "auto/sop/x.md", "v1", "v2").unwrap());
        let reg = AutoPageRegistry::load(h, "agnes");
        let rec = reg.current("auto/sop/x.md", "v2").unwrap();
        assert_eq!(rec.host_sources, vec!["conversation:telegram:A:m:1".to_string()]);
        forget_page(h, "agnes", "auto/sop/x.md").unwrap();
        assert!(AutoPageRegistry::load(h, "agnes").record("auto/sop/x.md").is_none());
        // Forgetting never creates a registry.
        forget_page(h, "betty", "auto/sop/x.md").unwrap();
        assert!(!registry_path(h, "betty").unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn registry_and_lock_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        record_page(home.path(), "agnes", "auto/sop/x.md", "v1", &[], true).unwrap();
        let reg = registry_path(home.path(), "agnes").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&reg), 0o600);
        assert_eq!(mode(reg.parent().unwrap()), 0o700);
        let guard = lock_auto_pages(home.path(), "agnes").unwrap();
        let lock = lock_path(home.path(), "agnes").unwrap();
        assert_eq!(mode(lock.parent().unwrap()), 0o700);
        drop(guard);
        // A link planted at the lock path is refused, not followed.
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(home.path().join("elsewhere"), &lock).unwrap();
        assert!(lock_auto_pages(home.path(), "agnes").is_err());
        assert!(!home.path().join("elsewhere").exists());
    }

    #[test]
    fn malformed_or_foreign_version_reads_as_empty() {
        let home = tempfile::tempdir().unwrap();
        let path = registry_path(home.path(), "agnes").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for body in ["not json", r#"{"version": 2, "pages": {}}"#] {
            std::fs::write(&path, body).unwrap();
            assert_eq!(AutoPageRegistry::load(home.path(), "agnes"), AutoPageRegistry::default());
        }
    }

    #[test]
    fn owner_of_wiki_requires_the_employee_layout() {
        let home = Path::new("/srv/dd");
        assert_eq!(
            owner_of_wiki(&home.join("agents/agnes/wiki")),
            Some((home.to_path_buf(), "agnes".to_string()))
        );
        for other in ["shared/wiki", "agents/agnes/notes", "x/agnes/wiki", "agents/.ephemeral/wiki"] {
            assert_eq!(owner_of_wiki(&home.join(other)), None, "{other}");
        }
        assert!(is_auto_rel("auto/sop/x.md") && is_auto_rel("./AUTO/x.md"));
        assert!(!is_auto_rel("automation/x.md") && is_auto_rel("auto") && !is_auto_rel("entities/auto/x.md"));
    }
}
