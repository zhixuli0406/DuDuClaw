//! WP-G1 — device migration ("汰機搬家"): restore an uploaded backup archive.
//!
//! Three phases, each independently testable:
//!
//! 1. **Extract** ([`extract_tar_gz_safely`]) — an uploaded `.tar.gz` is
//!    unpacked into a staging directory under a pre-parse-style safety gate
//!    modelled on `document_limits.rs`'s discipline: fail-closed, no partial
//!    writes survive a rejection, every ceiling is config-tunable but a `0`
//!    means "use the default", never "unlimited".
//! 2. **Stage** ([`write_marker`]) — once extraction succeeds, a small
//!    marker file records that a restore is pending. The RPC handler
//!    (`handlers.rs`) responds success and tells the caller a restart is
//!    required; nothing destructive has happened yet.
//! 3. **Swap** ([`perform_pending_restore_swap`]) — run once, early in
//!    `server.rs::start_gateway`, before anything else touches `home_dir`'s
//!    files. The device's CURRENT data is moved (never deleted) into a
//!    `restore-backup-<ts>/` directory first, and only then is the staged
//!    content moved into place. A crash between those two moves cannot lose
//!    data: either the old data is still in place (staging untouched) or it
//!    is already safely preserved under `restore-backup-<ts>/`.
//!
//! ## Why the swap target is `home_dir`, not `home_dir.parent()`
//!
//! `device.backup_create` (existing, `device_ops.rs`) archives
//! `home_dir.parent()` — the whole writable data partition — so a
//! from-scratch device rebuild can restore the OS-level layout too. But the
//! part of that archive a device-migration wizard actually cares about
//! (agents, `config.toml`, memory/session databases, …) all lives in the
//! subdirectory matching `home_dir`'s own basename (e.g. `duduclaw/`) inside
//! that tar. [`resolve_restore_root`] finds that subdirectory when present
//! and falls back to the staging root itself otherwise (covers a
//! hand-built or future archive that is already home-rooted) — see its own
//! doc comment.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

// ── Limits: `config.toml [backup]` (restore half) ───────────────────────

pub const DEFAULT_MAX_RESTORE_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024; // 8 GiB
pub const DEFAULT_MAX_RESTORE_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB
pub const DEFAULT_MAX_RESTORE_ENTRIES: u32 = 200_000;

/// Ceilings applied to one restore archive. Same `[backup]` section as
/// [`crate::backup_schedule::BackupScheduleConfig`] — a restore is a backup
/// concept, not a separate feature — read in isolation so a broken
/// unrelated section elsewhere in `config.toml` can never affect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreLimits {
    /// Sum of every entry's declared size (from the tar header — see
    /// [`extract_tar_gz_safely`]'s doc comment on why a tar entry cannot lie
    /// about its own length the way a zip entry can).
    pub max_total_bytes: u64,
    /// Per-entry declared size.
    pub max_entry_bytes: u64,
    /// Total entry count.
    pub max_entries: u32,
}

impl Default for RestoreLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: DEFAULT_MAX_RESTORE_TOTAL_BYTES,
            max_entry_bytes: DEFAULT_MAX_RESTORE_ENTRY_BYTES,
            max_entries: DEFAULT_MAX_RESTORE_ENTRIES,
        }
    }
}

impl RestoreLimits {
    pub fn from_home(home_dir: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
            return Self::default();
        };
        Self::from_toml_str(&content)
    }

    pub fn from_toml_str(content: &str) -> Self {
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        let Some(section) = table.get("backup").and_then(|v| v.as_table()) else {
            return Self::default();
        };
        let d = Self::default();
        Self {
            max_total_bytes: section
                .get("max_restore_total_bytes")
                .and_then(|v| v.as_integer())
                .and_then(|v| u64::try_from(v).ok())
                .unwrap_or(d.max_total_bytes),
            max_entry_bytes: section
                .get("max_restore_entry_bytes")
                .and_then(|v| v.as_integer())
                .and_then(|v| u64::try_from(v).ok())
                .unwrap_or(d.max_entry_bytes),
            max_entries: section
                .get("max_restore_entries")
                .and_then(|v| v.as_integer())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d.max_entries),
        }
        .sanitized()
    }

    /// `0` means "use the default", never "unlimited" — same discipline as
    /// `document_limits::DocumentLimits::sanitized`.
    fn sanitized(mut self) -> Self {
        let d = Self::default();
        if self.max_total_bytes == 0 {
            self.max_total_bytes = d.max_total_bytes;
        }
        if self.max_entry_bytes == 0 {
            self.max_entry_bytes = d.max_entry_bytes;
        }
        if self.max_entries == 0 {
            self.max_entries = d.max_entries;
        }
        self
    }
}

// ── Violations ────────────────────────────────────────────────────────

/// Why an uploaded archive was refused. Every variant is a hard DENY — no
/// partial extraction survives (see [`extract_tar_gz_safely`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreViolation {
    Unreadable,
    /// The bytes do not start with the gzip magic (`1f 8b`).
    NotTarGz,
    /// The gzip/tar stream could not be parsed.
    Malformed,
    /// A traversal / absolute-path / symlink / hardlink / special-file entry.
    UnsafeEntry { path: String, reason: &'static str },
    TooManyEntries { count: u64, max: u32 },
    EntryTooLarge { entry: String, bytes: u64, max: u64 },
    TotalTooLarge { bytes: u64, max: u64 },
    Io(String),
}

impl std::fmt::Display for RestoreViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable => write!(f, "無法讀取備份檔"),
            Self::NotTarGz => write!(f, "不是有效的 .tar.gz 備份檔"),
            Self::Malformed => write!(f, "備份檔壓縮結構損毀，無法解析"),
            Self::UnsafeEntry { path, reason } => {
                let name = duduclaw_core::truncate_bytes(path, 120);
                write!(f, "備份檔內含不安全的項目「{name}」（{reason}），已拒絕還原")
            }
            Self::TooManyEntries { count, max } => {
                write!(f, "備份檔內含 {count} 個項目，超過安全上限 {max} 個，已拒絕還原")
            }
            Self::EntryTooLarge { entry, bytes, max } => {
                let name = duduclaw_core::truncate_bytes(entry, 120);
                write!(
                    f,
                    "備份檔中的項目「{name}」約 {} MB，超過安全上限 {} MB，已拒絕還原",
                    mib(*bytes),
                    mib(*max)
                )
            }
            Self::TotalTooLarge { bytes, max } => write!(
                f,
                "備份檔解壓後總大小約 {} MB，超過安全上限 {} MB，已拒絕還原",
                mib(*bytes),
                mib(*max)
            ),
            Self::Io(msg) => write!(f, "還原時發生檔案系統錯誤: {msg}"),
        }
    }
}

fn mib(bytes: u64) -> u64 {
    bytes.div_ceil(1024 * 1024).max(1)
}

// ── Extraction ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreExtractReport {
    pub files_written: u64,
    pub dirs_created: u64,
    pub total_bytes: u64,
}

/// Extract `archive_path` (a `.tar.gz`) into `dest_dir`, enforcing
/// [`RestoreLimits`]. `dest_dir` is created if absent. On ANY violation the
/// whole extraction is rolled back (`dest_dir`'s contents removed) — never a
/// partial tree left on disk.
///
/// ### Why checking the tar header's declared size is sufficient
///
/// Unlike a zip entry (independent `compressed_size` / `uncompressed_size`
/// fields, so a "lying header" is a real risk `document_limits.rs` has to
/// caveat), a tar entry's content boundary IS its header's `size` field —
/// the archiver had to actually emit that many content bytes into the
/// stream for the format to be valid, and the tar reader will not read past
/// that boundary into the next header. The declared-size check below is
/// therefore not a heuristic; it is a precise bound on how many bytes THIS
/// entry can cause the gzip decoder to inflate before its content is
/// consumed. A small `.tar.gz` file can still legitimately compress a
/// declared-huge entry (e.g. megabytes of repeated zero bytes) — that is
/// exactly the "gzip bomb" shape this cap exists to catch.
///
/// ### Safety rules (fail-closed, whole-archive reject)
///
/// - No absolute paths, no `..` components (path traversal).
/// - No symlinks, hardlinks, or special files (device/fifo/socket) — only
///   regular files and directories are ever written to disk.
/// - Per-entry and cumulative declared-size ceilings, entry-count ceiling.
pub fn extract_tar_gz_safely(
    archive_path: &Path,
    dest_dir: &Path,
    limits: &RestoreLimits,
) -> Result<RestoreExtractReport, RestoreViolation> {
    let mut f = std::fs::File::open(archive_path).map_err(|_| RestoreViolation::Unreadable)?;
    let mut magic = [0u8; 2];
    let n = f.read(&mut magic).map_err(|_| RestoreViolation::Unreadable)?;
    if n < 2 || magic != [0x1f, 0x8b] {
        return Err(RestoreViolation::NotTarGz);
    }
    drop(f);

    std::fs::create_dir_all(dest_dir).map_err(|e| RestoreViolation::Io(e.to_string()))?;

    let result = extract_inner(archive_path, dest_dir, limits);
    if result.is_err() {
        // Fail-closed: no partial tree survives a rejected archive.
        let _ = std::fs::remove_dir_all(dest_dir);
    }
    result
}

fn extract_inner(
    archive_path: &Path,
    dest_dir: &Path,
    limits: &RestoreLimits,
) -> Result<RestoreExtractReport, RestoreViolation> {
    let f = std::fs::File::open(archive_path).map_err(|_| RestoreViolation::Unreadable)?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(gz);
    let entries = archive.entries().map_err(|_| RestoreViolation::Malformed)?;

    let mut total: u64 = 0;
    let mut count: u64 = 0;
    let mut files_written: u64 = 0;
    let mut dirs_created: u64 = 0;

    for entry in entries {
        let mut entry = entry.map_err(|_| RestoreViolation::Malformed)?;
        count += 1;
        if count > limits.max_entries as u64 {
            return Err(RestoreViolation::TooManyEntries { count, max: limits.max_entries });
        }

        let entry_type = entry.header().entry_type();
        let raw_path = entry.path().map_err(|_| RestoreViolation::Malformed)?.into_owned();
        let path_str = raw_path.to_string_lossy().to_string();

        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(RestoreViolation::UnsafeEntry { path: path_str, reason: "symlink_or_hardlink" });
        }
        if !matches!(entry_type, tar::EntryType::Regular | tar::EntryType::Directory) {
            return Err(RestoreViolation::UnsafeEntry { path: path_str, reason: "special_file" });
        }
        if raw_path.is_absolute()
            || raw_path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
        {
            return Err(RestoreViolation::UnsafeEntry { path: path_str, reason: "path_traversal" });
        }

        let size = entry.header().size().unwrap_or(0);
        if size > limits.max_entry_bytes {
            return Err(RestoreViolation::EntryTooLarge { entry: path_str, bytes: size, max: limits.max_entry_bytes });
        }
        total = total.saturating_add(size);
        if total > limits.max_total_bytes {
            return Err(RestoreViolation::TotalTooLarge { bytes: total, max: limits.max_total_bytes });
        }

        let dest_path = dest_dir.join(&raw_path);
        if entry_type.is_dir() {
            std::fs::create_dir_all(&dest_path).map_err(|e| RestoreViolation::Io(e.to_string()))?;
            dirs_created += 1;
            continue;
        }
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RestoreViolation::Io(e.to_string()))?;
        }
        let mut out = std::fs::File::create(&dest_path).map_err(|e| RestoreViolation::Io(e.to_string()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| RestoreViolation::Io(e.to_string()))?;
        files_written += 1;
    }

    Ok(RestoreExtractReport { files_written, dirs_created, total_bytes: total })
}

// ── Upload staging ────────────────────────────────────────────────────
//
// Mirrors `expert_admin`'s upload-fencing pattern (`upload_dir` +
// `sanitize_upload_name` + `staged_upload_path`): `POST
// /api/device/backup-upload` (`server.rs`) writes the uploaded bytes here;
// `device.backup_restore` (`handlers.rs`) is only ever handed the resulting
// server-local path, fail-closed re-checked by [`is_within_upload_dir`]
// before anything reads it.

/// HTTP body-size guard for the upload endpoint. The real content-shape
/// ceiling is [`RestoreLimits`], enforced during extraction — this only
/// bounds how large a single upload request the gateway will buffer.
pub const MAX_BACKUP_UPLOAD_BYTES: usize = 8 * 1024 * 1024 * 1024; // 8 GiB

/// `<home>/tmp/backup-uploads` — staging area for the device-migration
/// restore upload.
pub fn upload_dir(home: &Path) -> PathBuf {
    home.join("tmp").join("backup-uploads")
}

/// Sanitize a client-supplied filename into a safe `.tar.gz` basename:
/// strips any directory components (zip-slip via `../evil.tar.gz` or an
/// absolute path), keeps only `[A-Za-z0-9._-]`, guarantees a non-empty
/// `.tar.gz`/`.tgz` name.
pub fn sanitize_upload_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches('.');
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    let lower = cleaned.to_ascii_lowercase();
    if cleaned.is_empty() {
        "backup.tar.gz".to_string()
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        cleaned
    } else {
        format!("{cleaned}.tar.gz")
    }
}

/// Build the on-disk staging path for an upload: always inside
/// [`upload_dir`], always a fresh uuid-prefixed name (no overwrite races,
/// no traversal — the client name only contributes a sanitized suffix).
pub fn staged_upload_path(home: &Path, client_name: &str) -> PathBuf {
    let name = sanitize_upload_name(client_name);
    upload_dir(home).join(format!("{}-{}", uuid::Uuid::new_v4(), name))
}

/// `true` when `path` resolves, after canonicalization, inside `home`'s
/// upload staging dir. The fail-closed check `device.backup_restore` runs
/// on a caller-supplied `path` param before touching it at all — a path
/// pointing anywhere else (including one that merely looks plausible but
/// was never actually written by this gateway's own upload endpoint) is
/// refused.
pub fn is_within_upload_dir(home: &Path, path: &Path) -> bool {
    let dir = upload_dir(home);
    let (Ok(canon_dir), Ok(canon_path)) = (std::fs::canonicalize(&dir), std::fs::canonicalize(path)) else {
        return false;
    };
    canon_path.starts_with(&canon_dir)
}

// ── Marker / staging paths ───────────────────────────────────────────

pub const RESTORE_STAGING_DIRNAME: &str = "restore-staging";
/// Prefix for the "old data preserved" directory `perform_pending_restore_swap`
/// creates under `home_dir` — full name is `{RESTORE_BACKUP_PREFIX}<now_tag>`.
/// Exported so `backup_schedule.rs` can build a `restore-backup-*` tar
/// exclusion glob without hand-duplicating this literal.
pub const RESTORE_BACKUP_PREFIX: &str = "restore-backup-";
const RESTORE_MARKER_FILE: &str = "restore-pending.json";

/// `<home>/restore-staging` — extraction target for an uploaded backup, and
/// the source [`perform_pending_restore_swap`] swaps in from at boot.
pub fn staging_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(RESTORE_STAGING_DIRNAME)
}

fn marker_path(home_dir: &Path) -> PathBuf {
    home_dir.join(RESTORE_MARKER_FILE)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreMarker {
    pub staged_at: DateTime<Utc>,
    /// The uploaded file's original client-supplied name — audit trail only,
    /// never used to build a path.
    pub source_filename: String,
}

/// Atomic (temp + rename) write. Called only after [`extract_tar_gz_safely`]
/// has already succeeded — the marker's presence is the sole signal
/// `perform_pending_restore_swap` acts on, so it must never be written
/// before the staged content is actually safe to swap in.
pub fn write_marker(home_dir: &Path, marker: &RestoreMarker) -> std::io::Result<()> {
    let path = marker_path(home_dir);
    let body = serde_json::to_string_pretty(marker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body.as_bytes())?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

pub fn read_marker(home_dir: &Path) -> Option<RestoreMarker> {
    std::fs::read_to_string(marker_path(home_dir))
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
}

pub fn clear_marker(home_dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(marker_path(home_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Find the directory inside `staging` whose content is the actual
/// home-directory replacement.
///
/// `device.backup_create` archives `home_dir.parent()`, so the tar's root
/// mirrors the data partition, not `home_dir` directly — after extraction,
/// staging typically looks like `restore-staging/<home_basename>/...`
/// (e.g. `restore-staging/duduclaw/config.toml`). This prefers that nested
/// directory when present; falls back to the staging root itself when it is
/// not (a hand-built archive, a test fixture, or a future backup format
/// that is already home-rooted) — never errors, since a fallback that still
/// finds SOME content is safer than refusing an otherwise-valid restore.
pub fn resolve_restore_root(staging: &Path, home_dir: &Path) -> PathBuf {
    if let Some(name) = home_dir.file_name() {
        let nested = staging.join(name);
        if nested.is_dir() {
            return nested;
        }
    }
    staging.to_path_buf()
}

// ── Swap-in (run once, at boot) ──────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSwapReport {
    pub preserved_dir: PathBuf,
    pub entries_swapped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreSwapError {
    /// The marker was present but the staging directory was missing or
    /// empty — a corrupt/interrupted stage. The marker is cleared (so the
    /// gateway does not retry the same broken swap forever) but nothing is
    /// touched — fail-closed, no data movement on an already-suspect state.
    StagingMissingOrEmpty,
    Io(String),
}

/// Detect and apply a pending restore. `Ok(None)` — the overwhelming common
/// case — means no marker was present; this is a cheap check safe to run on
/// every boot regardless of appliance mode.
///
/// `now_tag` is an already-formatted, filesystem-safe timestamp (the caller
/// supplies it so this function stays pure/testable — see the tests below
/// for the exact format `server.rs` uses).
///
/// ## Ordering (why this can never destroy data)
///
/// 1. Resolve the restore root; bail (marker cleared, no move) if it is
///    missing or has nothing in it.
/// 2. Move every CURRENT top-level entry of `home_dir` (except the staging
///    dir and the marker itself) into a fresh `restore-backup-<now_tag>/`
///    subdirectory. This step ONLY ever moves data into a new, empty
///    directory — it cannot lose anything.
/// 3. Only after step 2 fully succeeds: move every entry from the restore
///    root up into `home_dir`.
/// 4. Remove the now-empty staging tree and the marker.
///
/// A crash or error between steps is always recoverable by hand: either
/// step 2 never completed (old data still exactly where it was) or it did
/// (old data sits under `restore-backup-<now_tag>/`, inspectable). Step 3
/// failing partway leaves some new entries in place and some old ones under
/// the preserved dir — never a mix that loses information, and the error is
/// returned rather than silently swallowed so the boot log carries it.
pub fn perform_pending_restore_swap(
    home_dir: &Path,
    now_tag: &str,
) -> Result<Option<RestoreSwapReport>, RestoreSwapError> {
    if read_marker(home_dir).is_none() {
        return Ok(None);
    }

    let staging = staging_dir(home_dir);
    let restore_root = resolve_restore_root(&staging, home_dir);
    let has_content = std::fs::read_dir(&restore_root)
        .map(|mut rd| rd.next().is_some())
        .unwrap_or(false);
    if !has_content {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = clear_marker(home_dir);
        return Err(RestoreSwapError::StagingMissingOrEmpty);
    }

    let preserved_dir = home_dir.join(format!("{RESTORE_BACKUP_PREFIX}{now_tag}"));
    std::fs::create_dir_all(&preserved_dir).map_err(|e| RestoreSwapError::Io(e.to_string()))?;
    let preserved_name = preserved_dir.file_name().map(|n| n.to_os_string());

    for entry in std::fs::read_dir(home_dir).map_err(|e| RestoreSwapError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| RestoreSwapError::Io(e.to_string()))?;
        let name = entry.file_name();
        if name == RESTORE_STAGING_DIRNAME
            || name == RESTORE_MARKER_FILE
            || Some(&name) == preserved_name.as_ref()
        {
            continue;
        }
        let dest = preserved_dir.join(&name);
        std::fs::rename(entry.path(), &dest).map_err(|e| RestoreSwapError::Io(e.to_string()))?;
    }

    let mut swapped = 0usize;
    for entry in std::fs::read_dir(&restore_root).map_err(|e| RestoreSwapError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| RestoreSwapError::Io(e.to_string()))?;
        let dest = home_dir.join(entry.file_name());
        std::fs::rename(entry.path(), &dest).map_err(|e| RestoreSwapError::Io(e.to_string()))?;
        swapped += 1;
    }

    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = clear_marker(home_dir) {
        warn!(error = %e, "backup-restore: 換入完成但清除標記檔失敗（下次開機會偵測到殘留 staging 為空而略過，不會重複換入）");
    }

    Ok(Some(RestoreSwapReport { preserved_dir, entries_swapped: swapped }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── RestoreLimits config ────────────────────────────────────

    #[test]
    fn limits_default_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(RestoreLimits::from_home(dir.path()), RestoreLimits::default());
    }

    #[test]
    fn limits_zero_means_default_not_unlimited() {
        let cfg = RestoreLimits::from_toml_str(
            "[backup]\nmax_restore_total_bytes = 0\nmax_restore_entry_bytes = 0\nmax_restore_entries = 0\n",
        );
        assert_eq!(cfg, RestoreLimits::default());
    }

    #[test]
    fn limits_read_custom_values() {
        let cfg = RestoreLimits::from_toml_str(
            "[backup]\nmax_restore_total_bytes = 1000\nmax_restore_entry_bytes = 100\nmax_restore_entries = 5\n",
        );
        assert_eq!(cfg.max_total_bytes, 1000);
        assert_eq!(cfg.max_entry_bytes, 100);
        assert_eq!(cfg.max_entries, 5);
    }

    // ── extract_tar_gz_safely ────────────────────────────────────

    /// Build a `.tar.gz` in memory from `(path, contents)` pairs — plain
    /// files only (the safe-path helper below covers unsafe entries).
    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *content).unwrap();
        }
        let tar_bytes = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    /// Build a `.tar.gz` with one entry whose raw header bytes are exactly
    /// `name_bytes` / `link_bytes` — bypassing `Header::set_path`'s own
    /// traversal/absolute-path validation (which would refuse to construct
    /// the very fixtures these tests need). Writing straight into the raw
    /// name/linkname fields is what a real malicious (non-Rust-authored)
    /// archive looks like on the wire, so this is a faithful attack fixture,
    /// not a workaround.
    fn tar_gz_with_raw_header(name_bytes: &[u8], entry_type: tar::EntryType, link_bytes: Option<&[u8]>) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_entry_type(entry_type);
        {
            let old = header.as_old_mut();
            let n = name_bytes.len().min(old.name.len());
            old.name[..n].copy_from_slice(&name_bytes[..n]);
            if let Some(link) = link_bytes {
                let n = link.len().min(old.linkname.len());
                old.linkname[..n].copy_from_slice(&link[..n]);
            }
        }
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    fn write_archive(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn extracts_a_normal_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = write_archive(
            tmp.path(),
            "backup.tar.gz",
            &build_tar_gz(&[("duduclaw/config.toml", b"[general]\n"), ("duduclaw/agents/.keep", b"")]),
        );
        let dest = tmp.path().join("staging");
        let report = extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()).unwrap();
        assert_eq!(report.files_written, 2);
        assert!(dest.join("duduclaw/config.toml").is_file());
        assert_eq!(std::fs::read(dest.join("duduclaw/config.toml")).unwrap(), b"[general]\n");
    }

    #[test]
    fn rejects_non_gzip_input() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = write_archive(tmp.path(), "fake.tar.gz", b"not a gzip file at all");
        let dest = tmp.path().join("staging");
        assert_eq!(
            extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()),
            Err(RestoreViolation::NotTarGz)
        );
        assert!(!dest.exists(), "no partial tree on rejection");
    }

    #[test]
    fn rejects_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("staging");
        assert_eq!(
            extract_tar_gz_safely(&tmp.path().join("nope.tar.gz"), &dest, &RestoreLimits::default()),
            Err(RestoreViolation::Unreadable)
        );
    }

    #[test]
    fn rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = tar_gz_with_raw_header(b"../../etc/passwd", tar::EntryType::Regular, None);
        let archive = write_archive(tmp.path(), "evil.tar.gz", &bytes);
        let dest = tmp.path().join("staging");
        let err = extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()).unwrap_err();
        assert!(matches!(err, RestoreViolation::UnsafeEntry { reason: "path_traversal", .. }), "{err:?}");
        assert!(!dest.exists());
    }

    #[test]
    fn rejects_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        // The raw header name field accepts this literal; `Path::is_absolute`
        // catches it on the read side regardless of how it got there.
        let bytes = tar_gz_with_raw_header(b"/etc/passwd", tar::EntryType::Regular, None);
        let archive = write_archive(tmp.path(), "evil2.tar.gz", &bytes);
        let dest = tmp.path().join("staging");
        let err = extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()).unwrap_err();
        assert!(matches!(err, RestoreViolation::UnsafeEntry { reason: "path_traversal", .. }), "{err:?}");
    }

    #[test]
    fn rejects_symlink_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = tar_gz_with_raw_header(b"link", tar::EntryType::Symlink, Some(b"/etc/passwd"));
        let archive = write_archive(tmp.path(), "evil3.tar.gz", &bytes);
        let dest = tmp.path().join("staging");
        let err = extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()).unwrap_err();
        assert!(
            matches!(err, RestoreViolation::UnsafeEntry { reason: "symlink_or_hardlink", .. }),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_entry_over_the_per_entry_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = write_archive(
            tmp.path(),
            "big.tar.gz",
            &build_tar_gz(&[("duduclaw/big.bin", &vec![0u8; 4096])]),
        );
        let dest = tmp.path().join("staging");
        let limits = RestoreLimits { max_entry_bytes: 1024, ..Default::default() };
        let err = extract_tar_gz_safely(&archive, &dest, &limits).unwrap_err();
        assert!(matches!(err, RestoreViolation::EntryTooLarge { .. }), "{err:?}");
        assert!(!dest.exists());
    }

    #[test]
    fn rejects_total_over_the_cumulative_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = write_archive(
            tmp.path(),
            "multi.tar.gz",
            &build_tar_gz(&[
                ("duduclaw/a.bin", &vec![0u8; 600]),
                ("duduclaw/b.bin", &vec![0u8; 600]),
            ]),
        );
        let dest = tmp.path().join("staging");
        // Each entry individually clears the per-entry cap; together they
        // blow the cumulative budget.
        let limits = RestoreLimits { max_entry_bytes: 1000, max_total_bytes: 1000, ..Default::default() };
        let err = extract_tar_gz_safely(&archive, &dest, &limits).unwrap_err();
        assert!(matches!(err, RestoreViolation::TotalTooLarge { .. }), "{err:?}");
    }

    #[test]
    fn rejects_too_many_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let files: Vec<(String, &[u8])> = (0..10).map(|i| (format!("duduclaw/f{i}.txt"), b"x".as_slice())).collect();
        let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let archive = write_archive(tmp.path(), "many.tar.gz", &build_tar_gz(&refs));
        let dest = tmp.path().join("staging");
        let limits = RestoreLimits { max_entries: 5, ..Default::default() };
        let err = extract_tar_gz_safely(&archive, &dest, &limits).unwrap_err();
        assert!(matches!(err, RestoreViolation::TooManyEntries { max: 5, .. }), "{err:?}");
    }

    // ── upload staging ────────────────────────────────────────────

    #[test]
    fn sanitize_upload_name_strips_traversal_and_forces_extension() {
        assert_eq!(sanitize_upload_name("my-device.tar.gz"), "my-device.tar.gz");
        assert_eq!(sanitize_upload_name("../../etc/passwd"), "passwd.tar.gz");
        assert_eq!(sanitize_upload_name("C:\\evil\\..\\x.tar.gz"), "x.tar.gz");
        assert_eq!(sanitize_upload_name(""), "backup.tar.gz");
        assert_eq!(sanitize_upload_name("weird name!.tgz"), "weird_name_.tgz");
    }

    #[test]
    fn staged_upload_path_always_inside_upload_dir() {
        let home = Path::new("/home/x");
        let p = staged_upload_path(home, "../../evil.tar.gz");
        assert!(p.starts_with(upload_dir(home)));
        assert!(p.to_string_lossy().ends_with("evil.tar.gz"));
    }

    #[test]
    fn is_within_upload_dir_accepts_only_real_staged_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = upload_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("abc-backup.tar.gz");
        std::fs::write(&staged, b"x").unwrap();
        assert!(is_within_upload_dir(home, &staged));

        // A file outside the upload dir must be refused even if it exists.
        let outside = home.join("secret.tar.gz");
        std::fs::write(&outside, b"x").unwrap();
        assert!(!is_within_upload_dir(home, &outside));

        // A non-existent path (canonicalize fails) is refused, not treated
        // as vacuously "inside".
        assert!(!is_within_upload_dir(home, &dir.join("nope.tar.gz")));
    }

    // ── marker round-trip ────────────────────────────────────────

    #[test]
    fn marker_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_marker(tmp.path()).is_none());
        let marker = RestoreMarker { staged_at: Utc::now(), source_filename: "old-device.tar.gz".into() };
        write_marker(tmp.path(), &marker).unwrap();
        let read = read_marker(tmp.path()).unwrap();
        assert_eq!(read.source_filename, "old-device.tar.gz");
        clear_marker(tmp.path()).unwrap();
        assert!(read_marker(tmp.path()).is_none());
        // Clearing an already-absent marker is not an error (idempotent).
        assert!(clear_marker(tmp.path()).is_ok());
    }

    // ── resolve_restore_root ──────────────────────────────────────

    #[test]
    fn resolve_root_prefers_the_nested_home_basename_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("duduclaw");
        let staging = tmp.path().join("restore-staging");
        std::fs::create_dir_all(staging.join("duduclaw")).unwrap();
        std::fs::write(staging.join("duduclaw/config.toml"), b"x").unwrap();
        // A sibling entry at the staging root that is NOT the nested dir —
        // must not be picked.
        std::fs::write(staging.join("other.txt"), b"y").unwrap();

        let root = resolve_restore_root(&staging, &home);
        assert_eq!(root, staging.join("duduclaw"));
    }

    #[test]
    fn resolve_root_falls_back_to_staging_root_when_not_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("duduclaw");
        let staging = tmp.path().join("restore-staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("config.toml"), b"x").unwrap();

        let root = resolve_restore_root(&staging, &home);
        assert_eq!(root, staging);
    }

    // ── perform_pending_restore_swap ────────────────────────────

    #[test]
    fn no_marker_is_a_cheap_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(perform_pending_restore_swap(tmp.path(), "20260101T000000Z"), Ok(None));
    }

    #[test]
    fn marker_present_but_staging_empty_clears_marker_and_errors_without_touching_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), b"[general]\n").unwrap();
        write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename: "x.tar.gz".into() }).unwrap();
        // No staging dir at all (interrupted upload).

        let result = perform_pending_restore_swap(home, "20260101T000000Z");
        assert_eq!(result, Err(RestoreSwapError::StagingMissingOrEmpty));
        assert!(read_marker(home).is_none(), "marker must be cleared so boot does not loop forever");
        assert!(home.join("config.toml").is_file(), "existing data must be untouched");
        assert!(!home.join("restore-backup-20260101T000000Z").exists());
    }

    #[test]
    fn swap_preserves_old_data_and_moves_staged_content_into_place() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Old (current) data.
        std::fs::write(home.join("config.toml"), b"old-config").unwrap();
        std::fs::create_dir_all(home.join("agents/kiki")).unwrap();
        std::fs::write(home.join("agents/kiki/SOUL.md"), b"old-soul").unwrap();

        // Staged new data, nested under the home basename (the realistic
        // shape `device.backup_create` produces).
        let staging = staging_dir(home);
        let nested = staging.join(home.file_name().unwrap());
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("config.toml"), b"new-config").unwrap();
        std::fs::create_dir_all(nested.join("agents/miki")).unwrap();
        std::fs::write(nested.join("agents/miki/SOUL.md"), b"new-soul").unwrap();

        write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename: "old-device.tar.gz".into() }).unwrap();

        let report = perform_pending_restore_swap(home, "20260101T000000Z").unwrap().unwrap();
        assert_eq!(report.preserved_dir, home.join("restore-backup-20260101T000000Z"));

        // New data is now live.
        assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), b"new-config");
        assert_eq!(std::fs::read(home.join("agents/miki/SOUL.md")).unwrap(), b"new-soul");

        // Old data preserved, never deleted.
        let preserved = home.join("restore-backup-20260101T000000Z");
        assert_eq!(std::fs::read(preserved.join("config.toml")).unwrap(), b"old-config");
        assert_eq!(std::fs::read(preserved.join("agents/kiki/SOUL.md")).unwrap(), b"old-soul");

        // Staging and marker are gone.
        assert!(!staging.exists());
        assert!(read_marker(home).is_none());
    }

    #[test]
    fn swap_falls_back_to_the_staging_root_when_not_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), b"old").unwrap();

        let staging = staging_dir(home);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("config.toml"), b"new").unwrap();

        write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename: "x.tar.gz".into() }).unwrap();
        perform_pending_restore_swap(home, "ts1").unwrap();
        assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), b"new");
    }

    #[test]
    fn a_second_boot_with_no_new_marker_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), b"old").unwrap();
        let staging = staging_dir(home);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("config.toml"), b"new").unwrap();
        write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename: "x.tar.gz".into() }).unwrap();

        assert!(perform_pending_restore_swap(home, "ts1").unwrap().is_some());
        // Marker is gone now — a second call must be a pure no-op, never
        // re-preserve/re-swap.
        assert_eq!(perform_pending_restore_swap(home, "ts2"), Ok(None));
        assert!(!home.join("restore-backup-ts2").exists());
    }
}
