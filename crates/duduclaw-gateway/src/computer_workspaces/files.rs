//! Reading and writing workspace files (design §4.4, §6.3).
//!
//! Every path is relative to `<root>/<id>/data`, validated here, and walked
//! fd-relative with `O_NOFOLLOW` ([`crate::fs_safe::SafeDir`]). A write runs
//! under the workspace's lock ([`super::lock`]): the revision check, quota,
//! temp file, rename and registry update are one unit, so two writers can
//! never both pass the same `expected_revision` or together exceed the
//! quota. The write is bracketed by a registry intent so a crash between
//! the rename and the revision bump is reconciled later.
//!
//! Reading is capped everywhere: one file is at most [`MAX_FILE_BYTES`], a
//! file with several hard links, a special file, a link or a file over the
//! cap is never read or listed (it counts as unprocessable), and listing is
//! a stat-only walk whose hashes come from the ledger ([`super::ledger`]).

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use super::config::WorkspacesConfig;
use super::ledger::LedgerEntry;
use super::store::{Lease, StoreError, WorkspaceStore, WriteIntent};
use crate::fs_safe::{EntryInfo, EntryKind, SafeDir, WriteStep};

/// Largest text a single read or write carries (D3: UTF-8 text only).
pub const MAX_FILE_BYTES: usize = 48 * 1024;
/// Most path segments.
pub const MAX_DEPTH: usize = 4;
/// Most bytes in one segment (after NFC).
pub const MAX_SEGMENT_BYTES: usize = 128;
/// Most entries `list` returns.
pub const LIST_LIMIT: usize = 200;
/// Most directory entries one scan visits (a planted huge tree stops here).
pub const SCAN_LIMIT: usize = 20_000;

/// Why a file operation refused. Closed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    InvalidPath,
    TooLarge,
    NotUtf8,
    FileNotFound,
    /// The path names something the gateway does not handle: a file with
    /// several hard links, a link, a special file, a directory.
    Unprocessable,
    /// Writing would exceed `max_bytes` / `max_files`.
    Quota {
        bytes_used: i64,
        files_used: i64,
    },
    DiskFull,
    /// Another write to this workspace did not finish in time.
    Busy,
    /// The lease was no longer this session's before anything was written.
    LeaseLost,
    /// The file landed, but the lease moved before the registry update (the
    /// registry recorded it anyway, with a `write_landed_after_fence` event).
    LandedAfterFence,
    /// The session was stopped or paused, or the threat level rose, while
    /// the write waited: nothing was renamed into place.
    SessionHalted,
    RevisionMismatch(i64),
    Unavailable,
}

impl From<StoreError> for FileError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::LeaseLost => Self::LeaseLost,
            StoreError::RevisionMismatch(r) => Self::RevisionMismatch(r),
            StoreError::Busy => Self::Busy,
            _ => Self::Unavailable,
        }
    }
}

/// The error a write's fault hook returns to abort before the rename
/// because the session was stopped or paused (mapped to
/// [`FileError::SessionHalted`]).
#[derive(Debug)]
pub struct WriteHalted;

impl std::fmt::Display for WriteHalted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session stopped or paused")
    }
}

impl std::error::Error for WriteHalted {}

/// An `io::Error` carrying [`WriteHalted`].
pub fn write_halted() -> io::Error {
    io::Error::other(WriteHalted)
}

fn is_write_halted(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<WriteHalted>())
}

fn allowed_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '(' | ')' | '（' | '）' | ' ')
}

/// One already-NFC segment passes the path rules.
fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SEGMENT_BYTES
        && !s.starts_with('.')
        && !s.starts_with(' ')
        && !s.ends_with(' ')
        && s.chars().all(allowed_char)
}

/// Validate and NFC-normalize a workspace-relative path. Returns its
/// segments. Refused: empty, absolute, more than [`MAX_DEPTH`] segments, an
/// empty / `.` / `..` / dot-leading segment, a leading or trailing space,
/// and any character outside letters, digits and `-_.()（）` plus inner
/// spaces (so `\`, NUL, control characters and `:` are all out).
pub fn normalize_path(raw: &str) -> Option<Vec<String>> {
    let nfc: String = raw.nfc().collect();
    if nfc.is_empty() || nfc.len() > MAX_DEPTH * (MAX_SEGMENT_BYTES + 1) {
        return None;
    }
    let segments: Vec<String> = nfc.split('/').map(str::to_string).collect();
    if segments.len() > MAX_DEPTH || !segments.iter().all(|s| valid_segment(s)) {
        return None;
    }
    Some(segments)
}

/// sha256 hex of the normalized path (what audit rows carry); the same
/// function the tool-call audit uses.
pub fn path_hash(segments: &[String]) -> String {
    duduclaw_core::workspace_path::workspace_path_hash(&segments.join("/"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One file of a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub path: String,
    pub size: u64,
    /// Empty when the hash is unknown (the file could not be read).
    pub sha256: String,
}

/// A stat-only walk of `data/`.
#[derive(Debug, Default)]
pub struct Scan {
    /// Regular, single-link files within the size cap whose every path
    /// segment passes the path rules, sorted by path.
    pub files: Vec<(String, u64)>,
    /// Everything else that is not a temp file: bad names, links, special
    /// files, hard-linked or oversized files, directories too deep.
    pub unprocessable: usize,
    /// The walk stopped at [`SCAN_LIMIT`].
    pub truncated: bool,
}

fn processable_file(e: &EntryInfo) -> bool {
    e.kind == EntryKind::File && e.nlink == 1 && e.size <= MAX_FILE_BYTES as u64
}

/// Walk `dir` without reading any file.
pub fn scan(dir: &SafeDir) -> io::Result<Scan> {
    let mut out = Scan::default();
    let mut visited = 0usize;
    walk(dir, "", 0, &mut out, &mut visited, true)?;
    out.files.sort();
    Ok(out)
}

fn walk(
    dir: &SafeDir,
    prefix: &str,
    depth: usize,
    out: &mut Scan,
    visited: &mut usize,
    root: bool,
) -> io::Result<()> {
    let entries = match dir.list() {
        Ok(e) => e,
        Err(e) if root => return Err(e),
        Err(_) => {
            out.unprocessable += 1;
            return Ok(());
        }
    };
    for entry in entries {
        *visited += 1;
        if *visited > SCAN_LIMIT {
            out.truncated = true;
            return Ok(());
        }
        // The gateway's own temp files (`.<name>.tmp-…`) are not content.
        if entry.kind == EntryKind::File && entry.name.starts_with('.') {
            continue;
        }
        let nfc: String = entry.name.nfc().collect();
        let named = valid_segment(&entry.name) && nfc == entry.name;
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        match entry.kind {
            EntryKind::File if named && processable_file(&entry) => {
                out.files.push((path, entry.size));
            }
            EntryKind::Dir if named && depth + 1 < MAX_DEPTH => {
                match dir.child_dir(&entry.name, false) {
                    Ok(Some(child)) => walk(&child, &path, depth + 1, out, visited, false)?,
                    _ => out.unprocessable += 1,
                }
            }
            _ => out.unprocessable += 1,
        }
        if out.truncated {
            return Ok(());
        }
    }
    Ok(())
}

/// Hash one listed file (capped, single link). `None` when it cannot be read.
fn hash_listed(dir: &SafeDir, path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let (parents, name) = parts.split_at(parts.len() - 1);
    let parent = if parents.is_empty() {
        None
    } else {
        Some(dir.open_path(parents, false).ok().flatten()?)
    };
    let bytes = parent
        .as_ref()
        .unwrap_or(dir)
        .read_bytes_checked(name[0], MAX_FILE_BYTES as u64, true)
        .ok()
        .flatten()?;
    Some(sha256_hex(&bytes))
}

fn data(home: &Path, id: &str) -> Result<SafeDir, FileError> {
    super::paths::open_data(home, id)
        .map_err(|_| FileError::Unavailable)?
        .ok_or(FileError::Unavailable)
}

/// What `list` returns.
#[derive(Debug, Default)]
pub struct Listing {
    /// At most [`LIST_LIMIT`] files.
    pub entries: Vec<ManifestEntry>,
    /// More files exist than were returned.
    pub more: bool,
    /// Entries the gateway does not handle (never named in the answer).
    pub unprocessable: usize,
}

/// The ledger view of `dir`'s files: the ledger hash when the size still
/// matches (and the hash is known), otherwise the file is hashed on its own.
/// `force` names a path hash that is always re-hashed (reconciliation's
/// target, review M-4). A file the ledger knows but that cannot be read is
/// kept with an empty (unknown) hash; any other unreadable file is `None`.
fn with_hashes<'a>(
    dir: &SafeDir,
    files: impl Iterator<Item = &'a (String, u64)>,
    ledger: &std::collections::HashMap<String, (u64, String)>,
    force: Option<&str>,
) -> Vec<Option<ManifestEntry>> {
    files
        .map(|(path, size)| {
            let forced = force.is_some_and(|h| {
                let segs: Vec<String> = path.split('/').map(str::to_string).collect();
                path_hash(&segs) == h
            });
            let known = ledger.get(path);
            let sha = match known {
                Some((s, sha)) if !forced && s == size && !sha.is_empty() => Some(sha.clone()),
                _ => match hash_listed(dir, path) {
                    Some(h) => Some(h),
                    None if forced || known.is_some() => Some(String::new()),
                    None => None,
                },
            }?;
            Some(ManifestEntry {
                path: path.clone(),
                size: *size,
                sha256: sha,
            })
        })
        .collect()
}

/// The workspace's files (at most [`LIST_LIMIT`]): a stat-only walk, hashes
/// from the ledger. A strange entry is counted, never fails the listing.
pub fn list_files(home: &Path, store: &WorkspaceStore, id: &str) -> Result<Listing, FileError> {
    let dir = data(home, id)?;
    let scanned = scan(&dir).map_err(|_| FileError::Unavailable)?;
    let ledger = store.ledger(id).map_err(FileError::from)?;
    let mut out = Listing {
        more: scanned.files.len() > LIST_LIMIT || scanned.truncated,
        unprocessable: scanned.unprocessable,
        ..Listing::default()
    };
    for entry in with_hashes(&dir, scanned.files.iter().take(LIST_LIMIT), &ledger, None) {
        match entry {
            Some(e) => out.entries.push(e),
            None => out.unprocessable += 1,
        }
    }
    Ok(out)
}

/// Read one text file. Returns `(content, sha256)`.
pub fn read_file(home: &Path, id: &str, rel_path: &str) -> Result<(String, String), FileError> {
    let segments = normalize_path(rel_path).ok_or(FileError::InvalidPath)?;
    let dir = data(home, id)?;
    let (parents, name) = segments.split_at(segments.len() - 1);
    let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
    let parent = if parents.is_empty() {
        dir
    } else {
        match dir.open_path(&parents, false) {
            Ok(Some(d)) => d,
            Ok(None) => return Err(FileError::FileNotFound),
            Err(_) => return Err(FileError::InvalidPath),
        }
    };
    let bytes = match parent.read_bytes_checked(&name[0], MAX_FILE_BYTES as u64, true) {
        Ok(Some(b)) => b,
        Ok(None) => return Err(FileError::FileNotFound),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(FileError::TooLarge),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return Err(FileError::Unprocessable);
        }
        Err(_) => return Err(FileError::InvalidPath),
    };
    let sha = sha256_hex(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| FileError::NotUtf8)?;
    Ok((text, sha))
}

/// Free bytes on the file system holding `path` (`None` when unknown).
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let st = nix::sys::statvfs::statvfs(path).ok()?;
    Some((st.blocks_available() as u64).saturating_mul(st.fragment_size() as u64))
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Result of a successful write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    pub data_revision: i64,
    pub sha256: String,
    pub bytes_used: i64,
    pub files_used: i64,
}

/// Inputs of one write.
pub struct WriteRequest<'a> {
    pub lease: &'a Lease,
    pub rel_path: &'a str,
    pub content: &'a str,
    pub expected_revision: Option<i64>,
    pub now: i64,
}

fn io_to_file_error(e: &io::Error) -> FileError {
    if e.raw_os_error() == Some(28) {
        FileError::DiskFull
    } else {
        FileError::Unavailable
    }
}

/// Write one file (design §4.4). Nothing on disk changes when a check
/// refuses; any I/O failure leaves the old file byte-identical.
pub fn write_file(
    home: &Path,
    store: &WorkspaceStore,
    cfg: &WorkspacesConfig,
    req: &WriteRequest<'_>,
    fault: &dyn Fn(WriteStep) -> io::Result<()>,
) -> Result<WriteOutcome, FileError> {
    write_file_inner(home, store, cfg, req, fault, true)
}

/// The size of the file at `name` in `parent` (`None` when absent);
/// something the gateway does not handle there is refused.
fn existing_size(parent: Option<&SafeDir>, name: &str) -> Result<Option<u64>, FileError> {
    let Some(parent) = parent else {
        return Ok(None);
    };
    match parent.entry(name).map_err(|_| FileError::InvalidPath)? {
        None => Ok(None),
        Some(e) if processable_file(&e) => Ok(Some(e.size)),
        Some(_) => Err(FileError::Unprocessable),
    }
}

/// `finish = false` stops after the rename (a simulated crash: the intent
/// stays for reconciliation).
pub(crate) fn write_file_inner(
    home: &Path,
    store: &WorkspaceStore,
    cfg: &WorkspacesConfig,
    req: &WriteRequest<'_>,
    fault: &dyn Fn(WriteStep) -> io::Result<()>,
    finish: bool,
) -> Result<WriteOutcome, FileError> {
    let segments = normalize_path(req.rel_path).ok_or(FileError::InvalidPath)?;
    if req.content.len() > MAX_FILE_BYTES {
        return Err(FileError::TooLarge);
    }
    let id = req.lease.workspace_id.as_str();
    // From here to the registry update this workspace has one writer.
    let _lock = super::lock::lock_for_change(home, id)?;
    let row = store.get(id)?.ok_or(FileError::Unavailable)?;
    let dir = data(home, id)?;
    let (parents, name) = segments.split_at(segments.len() - 1);
    let name = &name[0];
    let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
    let parent_now = if parents.is_empty() {
        Some(data(home, id)?)
    } else {
        dir.open_path(&parents, false)
            .map_err(|_| FileError::InvalidPath)?
    };
    // Refuses a target the gateway does not handle (hard link, special…).
    existing_size(parent_now.as_ref(), name)?;
    // Usage is the ledger's: a file written outside the gateway is not in
    // it, so replacing it counts as a new file (review L-2).
    let joined = segments.join("/");
    let old = store.ledger_entry(id, &joined)?.map(|(size, _)| size);
    let len = req.content.len() as i64;
    let new_bytes = (row.bytes_used - old.map_or(0, |s| s as i64)).max(0) + len;
    let new_files = row.files_used + i64::from(old.is_none());
    if new_bytes > cfg.max_bytes as i64 || new_files > i64::from(cfg.max_files) {
        return Err(FileError::Quota {
            bytes_used: row.bytes_used,
            files_used: row.files_used,
        });
    }
    let data_path = super::paths::data_dir(
        &super::paths::canonical_home(home).map_err(|_| FileError::Unavailable)?,
        id,
    );
    match free_bytes(&data_path) {
        Some(free) if free >= req.content.len() as u64 + cfg.min_free_bytes => {}
        _ => return Err(FileError::DiskFull),
    }
    let tmp = crate::fs_safe::tmp_name(name);
    let mut temp_rel = parents.clone();
    temp_rel.push(&tmp);
    let sha = sha256_hex(req.content.as_bytes());
    let intent = WriteIntent {
        intent_id: uuid::Uuid::new_v4().as_simple().to_string(),
        workspace_id: id.to_string(),
        rel_path_hash: path_hash(&segments),
        temp_name: temp_rel.join("/"),
        target_sha256: Some(sha.clone()),
        lease_epoch: req.lease.epoch,
        created_at: req.now,
    };
    store.begin_write(req.lease, &intent, req.expected_revision, req.now)?;
    let parent = match parent_now {
        Some(p) => Ok(p),
        None => dir
            .open_path(&parents, true)
            .map_err(|_| FileError::InvalidPath)?
            .ok_or(FileError::InvalidPath),
    };
    let written = parent.and_then(|p| {
        p.write_bytes_atomic_with(name, req.content.as_bytes(), &tmp, fault)
            .map_err(|e| {
                if is_write_halted(&e) {
                    FileError::SessionHalted
                } else if e.kind() == io::ErrorKind::PermissionDenied {
                    FileError::InvalidPath
                } else {
                    io_to_file_error(&e)
                }
            })
    });
    if let Err(e) = written {
        let _ = store.abandon_write(&intent.intent_id);
        return Err(e);
    }
    if !finish {
        return Err(FileError::Unavailable);
    }
    let entry = LedgerEntry {
        path: joined,
        size: req.content.len() as u64,
        sha256: sha.clone(),
    };
    let finished =
        store.finish_write(req.lease, &intent, &entry, (new_bytes, new_files), req.now)?;
    if finished.lease_lost {
        // Committed (revision, ledger, event) before this answer.
        return Err(FileError::LandedAfterFence);
    }
    Ok(WriteOutcome {
        data_revision: finished.revision,
        sha256: sha,
        bytes_used: new_bytes,
        files_used: new_files,
    })
}

/// Rebuild the ledger of `id` from disk (reuses ledger hashes whose size
/// still matches) and return `(manifest, bytes, files, entries)`.
fn rebuild_ledger(
    store: &WorkspaceStore,
    dir: &SafeDir,
    id: &str,
    force: Option<&str>,
) -> Option<(String, i64, i64, Vec<LedgerEntry>)> {
    let scanned = scan(dir).ok()?;
    let ledger = store.ledger(id).ok()?;
    let entries: Vec<LedgerEntry> = with_hashes(dir, scanned.files.iter(), &ledger, force)
        .into_iter()
        .flatten()
        .map(|e| LedgerEntry {
            path: e.path,
            size: e.size,
            sha256: e.sha256,
        })
        .collect();
    let bytes = entries.iter().map(|e| e.size as i64).sum();
    let files = entries.len() as i64;
    let manifest = store.replace_ledger(id, &entries).ok()?;
    Some((manifest, bytes, files, entries))
}

/// Settle one intent of a workspace whose lock is held.
fn settle_intent(home: &Path, store: &WorkspaceStore, intent: &WriteIntent) -> bool {
    let row = store.get(&intent.workspace_id).ok().flatten();
    let gone = row.as_ref().is_none_or(|r| {
        matches!(
            r.state,
            super::WorkspaceState::Deleted | super::WorkspaceState::FailedCreate
        )
    });
    if gone {
        // Nothing left to reconcile against: drop the intent only.
        return store.abandon_write(&intent.intent_id).is_ok();
    }
    let Ok(Some(dir)) = super::paths::open_data(home, &intent.workspace_id) else {
        // The data directory cannot be read right now (review L-1): keep the
        // intent, the usage and the ledger as they are, and say so once a
        // day so `duduclaw doctor` shows it.
        let day_ago = super::unix_now() - 86_400;
        if !store
            .has_event_since(&intent.workspace_id, "data_unreadable", day_ago)
            .unwrap_or(true)
        {
            let _ = store.note(
                &intent.workspace_id,
                "data_unreadable",
                "system:reconcile",
                serde_json::json!({"path_hash": intent.rel_path_hash}),
            );
        }
        return false;
    };
    let parts: Vec<&str> = intent.temp_name.split('/').collect();
    let (parents, tmp) = parts.split_at(parts.len() - 1);
    let parent = if parents.is_empty() {
        super::paths::open_data(home, &intent.workspace_id)
            .ok()
            .flatten()
    } else {
        dir.open_path(parents, false).ok().flatten()
    };
    let temp_present = parent.as_ref().is_some_and(|p| {
        tmp[0].starts_with('.')
            && p.entry(tmp[0])
                .ok()
                .flatten()
                .is_some_and(|e| e.kind == EntryKind::File)
    });
    if temp_present && let Some(p) = &parent {
        let _ = p.remove_file(tmp[0]);
    }
    // The target is always re-hashed: a same-size replacement must not keep
    // the old hash (review M-4).
    let Some((manifest, bytes, files, entries)) = rebuild_ledger(
        store,
        &dir,
        &intent.workspace_id,
        Some(&intent.rel_path_hash),
    ) else {
        return false;
    };
    let landed = !temp_present
        && entries.iter().any(|e| {
            let segs: Vec<String> = e.path.split('/').map(str::to_string).collect();
            path_hash(&segs) == intent.rel_path_hash
                && intent.target_sha256.as_deref() == Some(e.sha256.as_str())
        });
    let kind = if temp_present {
        "write_abandoned_after_crash"
    } else if landed {
        "reconciled_after_crash"
    } else {
        "write_outcome_unknown"
    };
    store
        .reconcile_intent(intent, (&manifest, bytes, files), landed, kind)
        .is_ok()
}

/// Reconciliation of write intents (design §4.4). Only a workspace whose
/// lock is free is touched: a held lock means a live writer owns its
/// intents. Returns how many were settled.
pub fn reconcile_intents(home: &Path, store: &WorkspaceStore) -> usize {
    let Ok(intents) = store.list_intents() else {
        return 0;
    };
    let ids: BTreeSet<String> = intents.into_iter().map(|i| i.workspace_id).collect();
    let mut settled = 0;
    for id in ids {
        let Some(_lock) = super::lock::try_lock_idle(home, &id) else {
            continue;
        };
        // Re-read under the lock: a writer may have finished meanwhile.
        let Ok(intents) = store.list_intents_for(&id) else {
            continue;
        };
        for intent in intents {
            if settle_intent(home, store, &intent) {
                settled += 1;
            }
        }
    }
    settled
}
