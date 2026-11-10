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
    IsolationUnavailable,
    Unreadable,
    /// The bytes do not start with the gzip magic (`1f 8b`).
    NotTarGz,
    /// The gzip/tar stream could not be parsed.
    Malformed,
    /// A traversal / absolute-path / symlink / hardlink / special-file entry.
    UnsafeEntry {
        path: String,
        reason: &'static str,
    },
    TooManyEntries {
        count: u64,
        max: u32,
    },
    EntryTooLarge {
        entry: String,
        bytes: u64,
        max: u64,
    },
    TotalTooLarge {
        bytes: u64,
        max: u64,
    },
    Io(String),
}

impl std::fmt::Display for RestoreViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IsolationUnavailable => write!(f, "隔離模式下請使用受保護的資料匯入流程，已拒絕舊版備份解析"),
            Self::Unreadable => write!(f, "無法讀取備份檔"),
            Self::NotTarGz => write!(f, "不是有效的 .tar.gz 備份檔"),
            Self::Malformed => write!(f, "備份檔壓縮結構損毀，無法解析"),
            Self::UnsafeEntry { path, reason } => {
                let name = duduclaw_core::truncate_bytes(path, 120);
                write!(
                    f,
                    "備份檔內含不安全的項目「{name}」（{reason}），已拒絕還原"
                )
            }
            Self::TooManyEntries { count, max } => {
                write!(
                    f,
                    "備份檔內含 {count} 個項目，超過安全上限 {max} 個，已拒絕還原"
                )
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
/// [`RestoreLimits`]. `dest_dir` must not exist; its parent must exist. On
/// any extraction violation only this newly created directory is removed.
/// Existing directories are refused and never changed.
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
    extract_tar_gz_with_isolation(archive_path, dest_dir, limits, duduclaw_parse_worker::client::required())
}

fn extract_tar_gz_with_isolation(
    archive_path: &Path,
    dest_dir: &Path,
    limits: &RestoreLimits,
    isolation_required: bool,
) -> Result<RestoreExtractReport, RestoreViolation> {
    // Package G owns the bounded migration importer and its authority review.
    // A parse-worker archive summary cannot authorize a legacy database restore.
    if isolation_required { return Err(RestoreViolation::IsolationUnavailable); }
    let mut f = std::fs::File::open(archive_path).map_err(|_| RestoreViolation::Unreadable)?;
    let mut magic = [0u8; 2];
    let n = f
        .read(&mut magic)
        .map_err(|_| RestoreViolation::Unreadable)?;
    if n < 2 || magic != [0x1f, 0x8b] {
        return Err(RestoreViolation::NotTarGz);
    }
    drop(f);

    // This function owns only a directory it created itself. Never roll
    // back another request's published staging (even for direct callers).
    std::fs::create_dir(dest_dir).map_err(|e| RestoreViolation::Io(e.to_string()))?;

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
            return Err(RestoreViolation::TooManyEntries {
                count,
                max: limits.max_entries,
            });
        }

        let entry_type = entry.header().entry_type();
        let raw_path = entry
            .path()
            .map_err(|_| RestoreViolation::Malformed)?
            .into_owned();
        let path_str = raw_path.to_string_lossy().to_string();

        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(RestoreViolation::UnsafeEntry {
                path: path_str,
                reason: "symlink_or_hardlink",
            });
        }
        if !matches!(
            entry_type,
            tar::EntryType::Regular | tar::EntryType::Directory
        ) {
            return Err(RestoreViolation::UnsafeEntry {
                path: path_str,
                reason: "special_file",
            });
        }
        if raw_path.is_absolute()
            || raw_path.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(RestoreViolation::UnsafeEntry {
                path: path_str,
                reason: "path_traversal",
            });
        }

        let size = entry.header().size().unwrap_or(0);
        if size > limits.max_entry_bytes {
            return Err(RestoreViolation::EntryTooLarge {
                entry: path_str,
                bytes: size,
                max: limits.max_entry_bytes,
            });
        }
        total = total.saturating_add(size);
        if total > limits.max_total_bytes {
            return Err(RestoreViolation::TotalTooLarge {
                bytes: total,
                max: limits.max_total_bytes,
            });
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
        let mut out =
            std::fs::File::create(&dest_path).map_err(|e| RestoreViolation::Io(e.to_string()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| RestoreViolation::Io(e.to_string()))?;
        files_written += 1;
    }

    Ok(RestoreExtractReport {
        files_written,
        dirs_created,
        total_bytes: total,
    })
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
    let (Ok(canon_dir), Ok(canon_path)) =
        (std::fs::canonicalize(&dir), std::fs::canonicalize(path))
    else {
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

#[derive(Debug)]
pub struct RestoreStageReport {
    pub extraction: RestoreExtractReport,
    pub mfa_notice: bool,
}

/// Serialize staging and boot swaps on a stable inode under the live locks/.
fn with_restore_lock<T>(home: &Path, f: impl FnOnce() -> T) -> std::io::Result<T> {
    duduclaw_core::with_file_lock(&home.join("locks/restore"), || Ok(f()))
}

fn path_occupied(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Stage one restore without ever extracting into, or cleaning, published data.
/// A marker, including an unreadable or malformed one, blocks all later stages.
/// The request's private temp directory is removed on every pre-publication
/// failure; a published directory is retained if marker publication fails.
pub fn stage_restore(home: &Path, source: &Path) -> Result<RestoreStageReport, String> {
    stage_restore_with_protection(home, source, protection())
}

/// Native Linux protected mode (v1.81, SR2 1c-u2): a whole-home restore is
/// refused outright, before any lock, read, temp directory or marker.
///
/// Why: the archive replaces every authority store and tree at once
/// (`users.db`, `migration-authority/apply.db`, `migration-confirmations.db`,
/// `approvals.db`, `agents/`). Restoring a backup taken before an Admin
/// tombstoned a Personal employee would undo that tombstone — the confirmed
/// row comes back live, the users.db mirror and owner binding return and the
/// directory reappears under `agents/<label>` (outside
/// `isolation-quarantine/`) — and it could equally revive proposals, approvals
/// or credentials that were since revoked. Protected installations use the
/// bounded migration import instead, whose every item is an Admin-approved
/// proposal.
pub const PROTECTED_RESTORE_REFUSED: &str = "受保護模式下不能整包還原資料目錄：整包還原會一次覆寫 AI 員工的隔離配置、\
    核准紀錄與帳號資料，可能讓已交接（停用並移入隔離區）的個人專屬員工或已撤銷的授權復原。請改用受保護的資料匯入流程，\
    由管理員逐項核准。";

/// L3: a protected single-binary host refuses an archive that carries
/// isolation state (see `backup_restore_isolation.rs`): restored there, those
/// records would no longer be enforced by the isolation gates.
pub const PROTECTED_RESTORE_ISOLATION_STATE: &str = "受保護模式下不能還原這份備份：備份內含隔離架構的 AI 員工狀態\
    （已確認的員工紀錄、個人專屬員工的擁有者紀錄、隔離區資料或遷移核准紀錄）。這台主機是單一執行檔的受保護安裝，\
    還原後這些紀錄將不再受到隔離規則保護。請在原本的隔離安裝上改用受保護的資料匯入流程，由管理員逐項核准。";
/// L3: the same check could not rule isolation state out (fail closed).
pub const PROTECTED_RESTORE_ISOLATION_UNKNOWN: &str = "受保護模式下不能還原這份備份：無法確認備份是否含有隔離架構的 \
    AI 員工狀態（相關的資料庫或目錄無法讀取或格式不符），為安全起見已拒絕。請確認備份的來源與完整性，\
    或改用受保護的資料匯入流程。";

#[path = "backup_restore_isolation.rs"]
mod isolation_state;

/// How a restore is protected on this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreProtection {
    /// Not protected: restore as before.
    Off,
    /// Protected single-binary host (protected mode, isolation not
    /// installed): restore as in v1.80, except an archive carrying isolation
    /// state ([`PROTECTED_RESTORE_ISOLATION_STATE`]).
    SingleBinary,
    /// Isolation installed: every whole-home restore is refused
    /// ([`PROTECTED_RESTORE_REFUSED`]).
    Isolation,
}

/// Isolation: native Linux protected mode on a host where the isolation
/// architecture is installed (the same switch the boot path uses to require
/// isolation). The refusal protects isolation state (tombstones, proposals,
/// quarantine) and points to the isolation import flow. SingleBinary:
/// protected mode alone.
fn protection() -> RestoreProtection {
    if crate::runtime_readiness::isolation_active() { RestoreProtection::Isolation }
    else if crate::runtime_readiness::is_protected_linux() { RestoreProtection::SingleBinary }
    else { RestoreProtection::Off }
}

/// L3: the archive's restore root carries no isolation state, or the refusal.
fn archive_isolation_refusal(root: &Path) -> Result<(), &'static str> {
    match isolation_state::inspect(root) {
        isolation_state::ArchiveIsolation::Absent => Ok(()),
        isolation_state::ArchiveIsolation::Present => Err(PROTECTED_RESTORE_ISOLATION_STATE),
        isolation_state::ArchiveIsolation::Unknown => Err(PROTECTED_RESTORE_ISOLATION_UNKNOWN),
    }
}

fn stage_restore_with_protection(home: &Path, source: &Path, protection: RestoreProtection)
    -> Result<RestoreStageReport, String> {
    if protection == RestoreProtection::Isolation { return Err(PROTECTED_RESTORE_REFUSED.into()); }
    let single_binary = protection == RestoreProtection::SingleBinary;
    with_restore_lock(home, || stage_restore_locked(home, source, single_binary))
        .map_err(|e| format!("無法鎖定還原作業：{e}"))?
}

fn stage_restore_locked(home: &Path, source: &Path, single_binary: bool) -> Result<RestoreStageReport, String> {
    if path_occupied(&marker_path(home)).map_err(|e| e.to_string())? {
        return Err("已有待套用的還原，請先重新啟動閘道套用，再上傳新的還原。".into());
    }
    let request = tempfile::Builder::new().prefix(".restore-request-")
        .tempdir_in(home).map_err(|e| format!("建立還原暫存目錄失敗：{e}"))?;
    let extracted = request.path().join("data");
    let extraction = extract_tar_gz_safely(source, &extracted, &RestoreLimits::from_home(home))
        .map_err(|e| e.to_string())?;
    let root = resolve_restore_root(&extracted, home);
    // L3: inspected in the private request directory, before any validation
    // reads or publication; the request directory is removed on refusal.
    if single_binary {
        archive_isolation_refusal(&root)?;
    }
    let read_config = |path: &Path| -> Result<String, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("無法讀取還原設定：{e}")),
        }
    };
    let restored_config = read_config(&root.join("config.toml"))?;
    let current_config = home.join("config.toml");
    crate::product_policy::validate_write(
        home, &current_config, &read_config(&current_config)?, &restored_config,
    )?;
    let mfa_notice = crate::mfa::restore_policy_differs(home, &restored_config);
    if mfa_notice {
        // Validate policy preservation here; boot rechecks against the live policy.
        crate::mfa::preserve_restore_policy(&restored_config, crate::mfa::restore_policy(home))?;
    }
    let staging = staging_dir(home);
    if path_occupied(&staging).map_err(|e| e.to_string())? {
        return Err("已有未完成的還原暫存資料，為保留資料已拒絕覆寫；請由管理員檢查。".into());
    }
    let marker = RestoreMarker {
        staged_at: Utc::now(),
        source_filename: source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    publish_restore_with_sync(home, &extracted, &marker, &mut sync_restore_path)
        .map_err(|e| format!("發布還原資料或標記失敗（已發布的暫存資料會保留）：{e}"))?;
    Ok(RestoreStageReport { extraction, mfa_notice })
}

// Sync children before their parent; never follow links or accept special files.
fn sync_restore_tree(
    path: &Path,
    sync: &mut impl FnMut(&Path, bool) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if kind.is_dir() {
        for entry in std::fs::read_dir(path)? {
            sync_restore_tree(&entry?.path(), sync)?;
        }
        sync(path, true)
    } else if kind.is_file() {
        sync(path, false)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "restore tree contains a link or special file"))
    }
}

fn sync_restore_path(path: &Path, directory: bool) -> std::io::Result<()> {
    // Windows std does not support directory fsync; rely on file sync_all there.
    #[cfg(windows)]
    if directory {
        return Ok(());
    }
    #[cfg(windows)]
    { std::fs::OpenOptions::new().write(true).open(path)?.sync_all() }
    #[cfg(not(windows))]
    {
        let _ = directory;
        std::fs::File::open(path)?.sync_all()
    }
}

fn publish_restore_with_sync(
    home: &Path,
    extracted: &Path,
    marker: &RestoreMarker,
    sync: &mut impl FnMut(&Path, bool) -> std::io::Result<()>,
) -> std::io::Result<()> {
    sync_restore_tree(extracted, sync)?;
    std::fs::rename(extracted, staging_dir(home))?;
    sync(home, true)?;
    write_marker_with_sync(home, marker, sync)
}

/// Atomic (temp + rename) write. Called only after [`extract_tar_gz_safely`]
/// has already succeeded — the marker's presence is the sole signal
/// `perform_pending_restore_swap` acts on, so it must never be written
/// before the staged content is actually safe to swap in.
pub fn write_marker(home_dir: &Path, marker: &RestoreMarker) -> std::io::Result<()> {
    write_marker_with_sync(home_dir, marker, &mut sync_restore_path)
}

fn write_marker_with_sync(
    home_dir: &Path,
    marker: &RestoreMarker,
    sync: &mut impl FnMut(&Path, bool) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let path = marker_path(home_dir);
    let body = serde_json::to_string_pretty(marker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut tmp = tempfile::Builder::new().prefix(".restore-marker-").tempfile_in(home_dir)?;
    std::io::Write::write_all(&mut tmp, body.as_bytes())?;
    sync(tmp.path(), false)?;
    tmp.persist(&path).map_err(|e| e.error)?;
    sync(home_dir, true)
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
    GatewayRunning(String),
    /// Native Linux protected mode: a marker staged before protection was
    /// enabled is never swapped in ([`PROTECTED_RESTORE_REFUSED`]; on a
    /// single-binary host only an archive with isolation state,
    /// [`PROTECTED_RESTORE_ISOLATION_STATE`] / `_UNKNOWN`). Nothing
    /// is moved; the staging tree and marker stay for an Admin to inspect.
    ProtectedMode(&'static str),
    Io(String),
}

// The singleton inode must stay at its original path on every platform.
// Ignore Windows aliases too, even when this restore runs on Unix.
fn is_gateway_lock_directory(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.split(':').next().unwrap_or(name)
            .trim_end_matches(['.', ' ']).to_uppercase().to_lowercase() == "locks"
    })
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
/// 1. Acquire the gateway singleton lock before any restore mutation, then
///    resolve the restore root; bail (marker cleared, no move) if it is
///    missing or has nothing in it.
/// 2. Move every CURRENT top-level entry of `home_dir` (except the staging
///    dir, markers and live `locks/` directory) into a fresh `restore-backup-<now_tag>/`
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
    perform_pending_restore_swap_with_protection(home_dir, now_tag, protection())
}

fn perform_pending_restore_swap_with_protection(
    home_dir: &Path,
    now_tag: &str,
    protection: RestoreProtection,
) -> Result<Option<RestoreSwapReport>, RestoreSwapError> {
    if read_marker(home_dir).is_none() {
        return Ok(None);
    }
    // SR2 1c-u2: see `PROTECTED_RESTORE_REFUSED` (a restore could undo tombstones).
    if protection == RestoreProtection::Isolation {
        return Err(RestoreSwapError::ProtectedMode(PROTECTED_RESTORE_REFUSED));
    }
    // L3: a restore staged before protection was enabled (or by an older
    // build) is inspected again; nothing moves, staging and marker stay.
    if protection == RestoreProtection::SingleBinary {
        archive_isolation_refusal(&resolve_restore_root(&staging_dir(home_dir), home_dir))
            .map_err(RestoreSwapError::ProtectedMode)?;
    }

    // Hold through policy reads, staging rewrites, the entire swap and boot.
    // A second gateway must not complete an Admin toggle during this window.
    if !duduclaw_core::gateway_instance::held(home_dir) {
        duduclaw_core::gateway_instance::acquire_for_restore(home_dir).map_err(|error| {
            RestoreSwapError::GatewayRunning(format!(
                "無法還原：其他閘道程序正在使用此資料目錄，請先停止該程序後再重試。{error}"
            ))
        })?;
    }

    with_restore_lock(home_dir, || perform_pending_restore_swap_locked(home_dir, now_tag))
        .map_err(|e| RestoreSwapError::Io(e.to_string()))?
}

fn perform_pending_restore_swap_locked(
    home_dir: &Path,
    now_tag: &str,
) -> Result<Option<RestoreSwapReport>, RestoreSwapError> {
    if read_marker(home_dir).is_none() {
        return Ok(None);
    }
    let staging = staging_dir(home_dir);
    let restore_root = resolve_restore_root(&staging, home_dir);
    let has_content = match std::fs::read_dir(&restore_root) {
        Ok(mut entries) => entries.next().transpose()
            .map_err(|e| RestoreSwapError::Io(e.to_string()))?.is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(RestoreSwapError::Io(e.to_string())),
    };
    if !has_content {
        let _ = std::fs::remove_dir_all(&staging);
        let _ = clear_marker(home_dir);
        return Err(RestoreSwapError::StagingMissingOrEmpty);
    }

    // A dashboard restore must not replace the installation-owned product policy.
    // Validate before markers or data movement, leaving both trees intact on refusal.
    let current_config = home_dir.join("config.toml");
    let read_config = |path: &Path| -> Result<String, RestoreSwapError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(RestoreSwapError::Io(error.to_string())),
        }
    };
    crate::product_policy::validate_write(
        home_dir,
        &current_config,
        &read_config(&current_config)?,
        &read_config(&restore_root.join("config.toml"))?,
    ).map_err(RestoreSwapError::Io)?;

    let kept_mfa = crate::mfa::restore_policy(home_dir);
    let restored_config_path = restore_root.join("config.toml");
    let restored_config = read_config(&restored_config_path)?;
    if crate::mfa::restore_policy_differs(home_dir, &restored_config) {
        let updated = crate::mfa::preserve_restore_policy(&restored_config, kept_mfa)
            .map_err(RestoreSwapError::Io)?;
        std::fs::write(&restored_config_path, updated)
            .map_err(|e| RestoreSwapError::Io(e.to_string()))?;
    }

    // LINE inbox (F2/F5): events restored from an archive may already have
    // been handled on the device the archive came from. This one-shot marker
    // makes the inbox hold them for an operator instead of running them
    // again. It is written before anything moves; if it cannot be written
    // the restore stops here (data untouched, pending marker kept, so the
    // next boot tries again) rather than bringing the backlog back live.
    std::fs::write(
        home_dir.join(crate::channel_ingress::RESTORE_MARKER),
        now_tag.as_bytes(),
    )
    .map_err(|e| {
        RestoreSwapError::Io(format!(
            "could not write the LINE inbox restore marker, restore not applied: {e}"
        ))
    })?;

    let preserved_dir = home_dir.join(format!("{RESTORE_BACKUP_PREFIX}{now_tag}"));
    std::fs::create_dir_all(&preserved_dir).map_err(|e| RestoreSwapError::Io(e.to_string()))?;
    let preserved_name = preserved_dir.file_name().map(|n| n.to_os_string());

    for entry in std::fs::read_dir(home_dir).map_err(|e| RestoreSwapError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| RestoreSwapError::Io(e.to_string()))?;
        let name = entry.file_name();
        if name == RESTORE_STAGING_DIRNAME
            || name == RESTORE_MARKER_FILE
            || name == crate::channel_ingress::RESTORE_MARKER
            || is_gateway_lock_directory(&name)
            || Some(&name) == preserved_name.as_ref()
        {
            continue;
        }
        let dest = preserved_dir.join(&name);
        std::fs::rename(entry.path(), &dest).map_err(|e| RestoreSwapError::Io(e.to_string()))?;
    }

    let mut swapped = 0usize;
    for entry in
        std::fs::read_dir(&restore_root).map_err(|e| RestoreSwapError::Io(e.to_string()))?
    {
        let entry = entry.map_err(|e| RestoreSwapError::Io(e.to_string()))?;
        if is_gateway_lock_directory(&entry.file_name()) {
            continue;
        }
        let dest = home_dir.join(entry.file_name());
        std::fs::rename(entry.path(), &dest).map_err(|e| RestoreSwapError::Io(e.to_string()))?;
        swapped += 1;
    }

    let _ = std::fs::remove_dir_all(&staging);
    if let Err(e) = clear_marker(home_dir) {
        warn!(error = %e, "backup-restore: 換入完成但清除標記檔失敗（下次開機會偵測到殘留 staging 為空而略過，不會重複換入）");
    }

    crate::security_autopilot::audit_and_emit(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "backup_restore_mfa_policy_preserved", "gateway",
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({"key":"auth.mfa_enforced", "kept":kept_mfa,
                "notice":crate::mfa::RESTORE_POLICY_NOTICE}),
        ),
    );
    tracing::info!(mfa_enforced = kept_mfa, "{}", crate::mfa::RESTORE_POLICY_NOTICE);

    Ok(Some(RestoreSwapReport {
        preserved_dir,
        entries_swapped: swapped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── RestoreLimits config ────────────────────────────────────

    #[test]
    fn limits_default_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            RestoreLimits::from_home(dir.path()),
            RestoreLimits::default()
        );
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
    fn tar_gz_with_raw_header(
        name_bytes: &[u8],
        entry_type: tar::EntryType,
        link_bytes: Option<&[u8]>,
    ) -> Vec<u8> {
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
            &build_tar_gz(&[
                ("duduclaw/config.toml", b"[general]\n"),
                ("duduclaw/agents/.keep", b""),
            ]),
        );
        let dest = tmp.path().join("staging");
        let report = extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()).unwrap();
        assert_eq!(report.files_written, 2);
        assert!(dest.join("duduclaw/config.toml").is_file());
        assert_eq!(
            std::fs::read(dest.join("duduclaw/config.toml")).unwrap(),
            b"[general]\n"
        );
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
            extract_tar_gz_safely(
                &tmp.path().join("nope.tar.gz"),
                &dest,
                &RestoreLimits::default()
            ),
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
        assert!(
            matches!(
                err,
                RestoreViolation::UnsafeEntry {
                    reason: "path_traversal",
                    ..
                }
            ),
            "{err:?}"
        );
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
        assert!(
            matches!(
                err,
                RestoreViolation::UnsafeEntry {
                    reason: "path_traversal",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_symlink_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = tar_gz_with_raw_header(b"link", tar::EntryType::Symlink, Some(b"/etc/passwd"));
        let archive = write_archive(tmp.path(), "evil3.tar.gz", &bytes);
        let dest = tmp.path().join("staging");
        let err = extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()).unwrap_err();
        assert!(
            matches!(
                err,
                RestoreViolation::UnsafeEntry {
                    reason: "symlink_or_hardlink",
                    ..
                }
            ),
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
        let limits = RestoreLimits {
            max_entry_bytes: 1024,
            ..Default::default()
        };
        let err = extract_tar_gz_safely(&archive, &dest, &limits).unwrap_err();
        assert!(
            matches!(err, RestoreViolation::EntryTooLarge { .. }),
            "{err:?}"
        );
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
        let limits = RestoreLimits {
            max_entry_bytes: 1000,
            max_total_bytes: 1000,
            ..Default::default()
        };
        let err = extract_tar_gz_safely(&archive, &dest, &limits).unwrap_err();
        assert!(
            matches!(err, RestoreViolation::TotalTooLarge { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn rejects_too_many_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let files: Vec<(String, &[u8])> = (0..10)
            .map(|i| (format!("duduclaw/f{i}.txt"), b"x".as_slice()))
            .collect();
        let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let archive = write_archive(tmp.path(), "many.tar.gz", &build_tar_gz(&refs));
        let dest = tmp.path().join("staging");
        let limits = RestoreLimits {
            max_entries: 5,
            ..Default::default()
        };
        let err = extract_tar_gz_safely(&archive, &dest, &limits).unwrap_err();
        assert!(
            matches!(err, RestoreViolation::TooManyEntries { max: 5, .. }),
            "{err:?}"
        );
    }

    fn assert_no_request_residue(home: &Path) {
        for entry in std::fs::read_dir(home).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(!name.to_string_lossy().starts_with(".restore-request-"), "{name:?}");
            assert!(!name.to_string_lossy().starts_with(".restore-marker-"), "{name:?}");
        }
    }

    #[test]
    fn staging_second_request_refuses_pending_and_keeps_first_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let a = write_archive(tmp.path(), "a.tar.gz", &build_tar_gz(&[("kept.txt", b"A")]));
        let b = write_archive(tmp.path(), "b.tar.gz", &build_tar_gz(&[("new.txt", b"B")]));
        stage_restore(&home, &a).unwrap();
        let marker = std::fs::read(marker_path(&home)).unwrap();
        let error = stage_restore(&home, &b).unwrap_err();
        assert!(error.contains("已有待套用的還原"), "{error}");
        assert_eq!(std::fs::read(staging_dir(&home).join("kept.txt")).unwrap(), b"A");
        assert!(!staging_dir(&home).join("new.txt").exists());
        assert_eq!(std::fs::read(marker_path(&home)).unwrap(), marker);
        assert_no_request_residue(&home);
    }

    #[test]
    fn staging_failure_cleans_own_partial_tree_and_preserves_existing_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("config.toml"), "[backup]\nmax_restore_entries=1\n").unwrap();
        let archive = write_archive(tmp.path(), "bad.tar.gz", &build_tar_gz(&[
            ("first.txt", b"partial"), ("second.txt", b"rejected"),
        ]));
        // Check both a fresh stage and a previous publication without a marker.
        for existing in [false, true] {
            if existing {
                std::fs::create_dir(staging_dir(&home)).unwrap();
                std::fs::write(staging_dir(&home).join("kept.txt"), b"A").unwrap();
            }
            let error = stage_restore(&home, &archive).unwrap_err();
            assert!(error.contains("超過安全上限"), "{error}");
            assert!(!marker_path(&home).exists());
            assert!(!staging_dir(&home).join("first.txt").exists());
            assert_no_request_residue(&home);
            if existing {
                assert_eq!(std::fs::read(staging_dir(&home).join("kept.txt")).unwrap(), b"A");
            } else {
                assert!(!staging_dir(&home).exists());
            }
        }
    }

    #[test]
    fn staging_refuses_existing_unmarked_publication_without_overwriting_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(staging_dir(&home)).unwrap();
        std::fs::write(staging_dir(&home).join("kept.txt"), b"A").unwrap();
        let archive = write_archive(tmp.path(), "b.tar.gz", &build_tar_gz(&[("new.txt", b"B")]));
        let error = stage_restore(&home, &archive).unwrap_err();
        assert!(error.contains("拒絕覆寫"), "{error}");
        assert_eq!(std::fs::read(staging_dir(&home).join("kept.txt")).unwrap(), b"A");
        assert!(!staging_dir(&home).join("new.txt").exists());
        assert!(!marker_path(&home).exists());
        assert_no_request_residue(&home);
    }

    #[test]
    fn staging_refuses_malformed_pending_marker() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(marker_path(tmp.path()), b"broken marker").unwrap();
        let error = stage_restore(tmp.path(), &tmp.path().join("unused.tar.gz")).unwrap_err();
        assert!(error.contains("已有待套用的還原"), "{error}");
        assert_eq!(std::fs::read(marker_path(tmp.path())).unwrap(), b"broken marker");
        assert_no_request_residue(tmp.path());
    }

    #[test]
    fn extraction_never_cleans_an_existing_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("staging");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("kept.txt"), b"A").unwrap();
        let archive = write_archive(tmp.path(), "b.tar.gz", &build_tar_gz(&[("new.txt", b"B")]));
        assert!(extract_tar_gz_safely(&archive, &dest, &RestoreLimits::default()).is_err());
        assert_eq!(std::fs::read(dest.join("kept.txt")).unwrap(), b"A");
        assert!(!dest.join("new.txt").exists());
    }

    #[test]
    fn concurrent_staging_publishes_exactly_one_complete_request() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let a = write_archive(tmp.path(), "a.tar.gz", &build_tar_gz(&[("a.txt", b"A")]));
        let b = write_archive(tmp.path(), "b.tar.gz", &build_tar_gz(&[("b.txt", b"B")]));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [a, b].into_iter().map(|source| {
            let home = home.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                stage_restore(&home, &source)
            })
        }).collect();
        let successes = handles.into_iter()
            .map(|handle| handle.join().unwrap()).filter(Result::is_ok).count();
        assert_eq!(successes, 1);
        let marker = read_marker(&home).unwrap();
        let (kept, absent, bytes) = if marker.source_filename == "a.tar.gz" {
            ("a.txt", "b.txt", b"A")
        } else {
            ("b.txt", "a.txt", b"B")
        };
        assert_eq!(std::fs::read(staging_dir(&home).join(kept)).unwrap(), bytes);
        assert!(!staging_dir(&home).join(absent).exists());
        assert_no_request_residue(&home);
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

    #[test]
    fn publish_syncs_tree_then_rename_then_marker_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let extracted = home.join("request/data");
        let nested = extracted.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("payload");
        std::fs::write(&file, b"durable").unwrap();
        let marker = RestoreMarker { staged_at: Utc::now(), source_filename: "test.tar.gz".into() };
        let mut steps = Vec::new();
        publish_restore_with_sync(home, &extracted, &marker, &mut |path, directory| {
            match steps.len() {
                0 => assert_eq!((path, directory), (file.as_path(), false)),
                1 => assert_eq!((path, directory), (nested.as_path(), true)),
                2 => assert_eq!((path, directory), (extracted.as_path(), true)),
                3 => {
                    assert_eq!((path, directory), (home, true));
                    assert!(!extracted.exists());
                    assert!(staging_dir(home).join("nested/payload").is_file());
                    assert!(!marker_path(home).exists());
                }
                4 => {
                    assert!(!directory);
                    assert!(path.file_name().unwrap().to_string_lossy().starts_with(".restore-marker-"));
                    assert!(!marker_path(home).exists());
                    assert!(serde_json::from_slice::<RestoreMarker>(&std::fs::read(path)?).is_ok());
                }
                5 => {
                    assert_eq!((path, directory), (home, true));
                    assert!(read_marker(home).is_some());
                }
                _ => panic!("unexpected sync"),
            }
            steps.push(path.to_path_buf());
            sync_restore_path(path, directory)
        }).unwrap();
        assert_eq!(steps.len(), 6);
    }

    #[test]
    fn failed_publish_sync_retains_data_and_any_persisted_marker() {
        for fail_at in 0..5 {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            let extracted = home.join("data");
            std::fs::create_dir(&extracted).unwrap();
            std::fs::write(extracted.join("payload"), b"keep").unwrap();
            let marker = RestoreMarker { staged_at: Utc::now(), source_filename: "test.tar.gz".into() };
            let mut calls = 0;
            let result = publish_restore_with_sync(home, &extracted, &marker, &mut |_, _| {
                let current = calls;
                calls += 1;
                if current == fail_at { Err(std::io::Error::other("injected sync failure")) }
                else { Ok(()) }
            });
            assert!(result.is_err());
            assert_eq!(marker_path(home).exists(), fail_at == 4);
            assert!(extracted.join("payload").exists() || staging_dir(home).join("payload").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn publish_refuses_symlinks_without_syncing_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let extracted = tmp.path().join("data");
        std::fs::create_dir(&extracted).unwrap();
        std::fs::write(tmp.path().join("target"), b"keep").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("target"), extracted.join("link")).unwrap();
        let marker = RestoreMarker { staged_at: Utc::now(), source_filename: "test.tar.gz".into() };
        assert!(publish_restore_with_sync(tmp.path(), &extracted, &marker, &mut |_, _| {
            panic!("must refuse the link before syncing")
        }).is_err());
        assert!(!staging_dir(tmp.path()).exists());
        assert!(!marker_path(tmp.path()).exists());
    }

    // ── marker round-trip ────────────────────────────────────────

    #[test]
    fn marker_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_marker(tmp.path()).is_none());
        let marker = RestoreMarker {
            staged_at: Utc::now(),
            source_filename: "old-device.tar.gz".into(),
        };
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
    fn restore_refuses_a_live_gateway_without_changing_any_files() {
        use fs2::FileExt;

        fn snapshot(root: &Path, dir: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
            let mut files = std::collections::BTreeMap::new();
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    files.insert(relative, None);
                    files.extend(snapshot(root, &path));
                } else {
                    // Windows forbids reading the locked byte range through
                    // another handle; the empty lock itself is never written.
                    let content = if relative == Path::new("locks/gateway.lock") {
                        Vec::new()
                    } else {
                        std::fs::read(&path).unwrap()
                    };
                    files.insert(relative, Some(content));
                }
            }
            files
        }

        for with_staging in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            std::fs::create_dir_all(home.join("locks")).unwrap();
            let holder = std::fs::OpenOptions::new().create(true).truncate(false)
                .read(true).write(true)
                .open(duduclaw_core::gateway_instance::lock_path(home)).unwrap();
            holder.try_lock_exclusive().unwrap();
            std::fs::write(home.join("locks/gateway.lock.holder"), "other gateway").unwrap();
            std::fs::write(home.join("config.toml"), "[auth]\nmfa_enforced=true\n").unwrap();
            std::fs::write(home.join("users.db"), "live identity").unwrap();
            if with_staging {
                let staging = staging_dir(home);
                std::fs::create_dir(&staging).unwrap();
                std::fs::write(staging.join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
                std::fs::write(staging.join("users.db"), "archived identity").unwrap();
            }
            write_marker(home, &RestoreMarker {
                staged_at: Utc::now(), source_filename: "pending.tar.gz".into(),
            }).unwrap();
            let before = snapshot(home, home);
            let error = perform_pending_restore_swap(home, "blocked").unwrap_err();
            assert!(matches!(error, RestoreSwapError::GatewayRunning(ref text)
                if text.contains("其他閘道程序正在使用此資料目錄")));
            assert_eq!(snapshot(home, home), before);
            assert!(!duduclaw_core::gateway_instance::held(home));
        }
    }

    #[test]
    fn restore_preserves_the_singleton_inode_and_skips_archived_lock_aliases() {
        use fs2::FileExt;

        assert!(is_gateway_lock_directory(std::ffi::OsStr::new("locks::$INDEX_ALLOCATION")));
        for archived_name in ["locks", "LOCKS", "locks.", "locks ", "lockſ", "locKs"] {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            duduclaw_core::gateway_instance::acquire_for_restore(home).unwrap();
            let lock = duduclaw_core::gateway_instance::lock_path(home);
            #[cfg(unix)]
            let original_inode = {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&lock).unwrap().ino()
            };
            let holder_before = std::fs::read(duduclaw_core::gateway_instance::holder_path(home)).unwrap();
            std::fs::write(home.join("locks/keep"), b"live lock data").unwrap();
            let staging = staging_dir(home);
            std::fs::create_dir_all(&staging).unwrap();
            // Preserve literal trailing dots/spaces instead of Win32 final-component normalization.
            #[cfg(windows)]
            let staging = staging.canonicalize().unwrap();
            std::fs::create_dir_all(staging.join(archived_name)).unwrap();
            std::fs::write(staging.join(archived_name).join("gateway.lock"), b"archived lock").unwrap();
            assert!(std::fs::read_dir(&staging).unwrap().any(|entry| entry.unwrap().file_name() == archived_name));
            std::fs::write(staging.join("config.toml"), "[auth]\nmfa_enforced=true\n").unwrap();
            write_marker(home, &RestoreMarker {
                staged_at: Utc::now(), source_filename: "locks.tar.gz".into(),
            }).unwrap();

            let report = perform_pending_restore_swap(home, "stable-lock").unwrap().unwrap();
            assert_eq!(report.entries_swapped, 1);
            assert!(!staging.exists(), "skipped lock aliases must not prevent staging cleanup");
            assert!(read_marker(home).is_none());
            assert!(!report.preserved_dir.join("locks").exists());
            assert_eq!(std::fs::read(home.join("locks/keep")).unwrap(), b"live lock data");
            assert_eq!(std::fs::read(duduclaw_core::gateway_instance::holder_path(home)).unwrap(), holder_before);
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert_eq!(std::fs::metadata(&lock).unwrap().ino(), original_inode);
            }
            let other = std::fs::OpenOptions::new().read(true).write(true).open(&lock).unwrap();
            assert!(other.try_lock_exclusive().is_err(), "a second gateway must remain excluded");
            assert!(duduclaw_core::gateway_instance::held(home));
        }
    }

    #[test]
    fn mfa_restore_keeps_current_policy_and_restores_all_data_once() {
        for (current, restored, kept) in [
            ("[auth]\nmfa_enforced=true\n", "[auth]\nmfa_enforced=false # keep comment\n", true),
            ("[auth]\nmfa_enforced=false\n", "[auth]\nmfa_enforced=true # keep comment\n", false),
            ("[auth]\nmfa_enforced=true\n", "", true),
            ("[auth]\nmfa_enforced='invalid'\n", "[auth]\nmfa_enforced=false\n", true),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            // These cases represent an existing install, including an Admin disable.
            drop(duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap());
            std::fs::write(home.join("config.toml"), current).unwrap();
            std::fs::write(home.join("old.txt"), "old").unwrap();
            let staging = staging_dir(home);
            std::fs::create_dir_all(&staging).unwrap();
            let backup_config = format!("# backup\n{restored}\n[general]\nlog_level = 'debug' # untouched\n");
            std::fs::write(staging.join("config.toml"), &backup_config).unwrap();
            std::fs::write(staging.join("customer-data.txt"), "restored").unwrap();
            write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename:"test.tar.gz".into() }).unwrap();
            let report = perform_pending_restore_swap(home, "mfa-test").unwrap().unwrap();
            let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
            let parsed: toml::Table = config.parse().unwrap();
            assert_eq!(parsed["auth"]["mfa_enforced"].as_bool(), Some(kept));
            assert!(config.starts_with("# backup\n"));
            assert!(config.contains("log_level = 'debug' # untouched"));
            if restored.contains("# keep comment") { assert!(config.contains("# keep comment")); }
            assert_eq!(std::fs::read_to_string(home.join("customer-data.txt")).unwrap(), "restored");
            assert_eq!(std::fs::read_to_string(report.preserved_dir.join("config.toml")).unwrap(), current);
            assert!(report.preserved_dir.join("old.txt").exists());
            assert!(!home.join("old.txt").exists());
            assert!(!staging.exists());
            assert!(read_marker(home).is_none());
            let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
            assert!(audit.contains("backup_restore_mfa_policy_preserved"));
            assert_eq!(perform_pending_restore_swap(home, "second-boot"), Ok(None));
            assert!(!home.join("restore-backup-second-boot").exists());
            assert_eq!(std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap(), audit);
        }
    }

    #[test]
    fn mfa_empty_home_restore_enforces_policy_after_personal_provisioning() {
        for personal in [true, false] {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            let staging = staging_dir(home);
            std::fs::create_dir_all(&staging).unwrap();
            std::fs::write(staging.join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
            drop(duduclaw_auth::UserDb::new(&staging.join("users.db")).unwrap());
            // Include a dormant true in the archived store to check both outputs.
            let mut conn = rusqlite::Connection::open(staging.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
            drop(conn);
            write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename: "normal.tar.gz".into() }).unwrap();
            perform_pending_restore_swap(home, "empty-mfa").unwrap().unwrap();
            let read_config = || std::fs::read_to_string(home.join("config.toml")).unwrap().parse::<toml::Table>().unwrap()["auth"]["mfa_enforced"].as_bool().unwrap();
            assert!(read_config()); // Edition is still unknown.
            crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
                home, "127.0.0.1:18789".parse().unwrap(), personal, None,
            ).unwrap();
            crate::mfa::reload_policy(home);
            assert!(read_config());
            assert!(crate::mfa::boot_policy(home, false).unwrap());
            assert!(crate::mfa::enforced(home, personal).unwrap());
            let conn = rusqlite::Connection::open(home.join("users.db")).unwrap();
            assert!(duduclaw_auth::mfa::read_enforcement(&conn).unwrap());
        }
    }

    #[test]
    fn mfa_restore_before_first_boot_cannot_import_a_disabled_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
        assert!(!home.join("users.db").exists());
        let staging = staging_dir(home);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
        {
            let _db = duduclaw_auth::UserDb::new(&staging.join("users.db")).unwrap();
            let mut conn = rusqlite::Connection::open(staging.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, false, 123).unwrap();
        }
        write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename: "old.tar.gz".into() }).unwrap();
        perform_pending_restore_swap(home, "before-first-boot").unwrap().unwrap();
        crate::mfa::reload_policy(home);
        assert!(crate::mfa::enforced(home, false).unwrap());
        assert!(crate::mfa::boot_policy(home, false).unwrap());
        // Restore keeps the persisted and runtime policy strict in every edition.
        let config: toml::Table = std::fs::read_to_string(home.join("config.toml")).unwrap().parse().unwrap();
        assert_eq!(config["auth"]["mfa_enforced"].as_bool(), Some(true));
        assert!(crate::mfa::enforced(home, true).unwrap());
    }

    #[test]
    fn mfa_restore_preserves_fresh_team_default_over_archived_false() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
            home, "127.0.0.1:18789".parse().unwrap(), false, None,
        ).unwrap();
        let staging = staging_dir(home);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("config.toml"), "[auth]\nmfa_enforced=false\n").unwrap();
        {
            let _db = duduclaw_auth::UserDb::new(&staging.join("users.db")).unwrap();
            let mut conn = rusqlite::Connection::open(staging.join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, false, 123).unwrap();
        }
        write_marker(home, &RestoreMarker { staged_at: Utc::now(), source_filename: "fresh-test.tar.gz".into() }).unwrap();
        perform_pending_restore_swap(home, "fresh-mfa-test").unwrap().unwrap();
        crate::mfa::reload_policy(home);
        assert!(crate::mfa::enforced(home, false).unwrap());
        assert!(crate::mfa::boot_policy(home, false).unwrap());
        assert_eq!(std::fs::read_to_string(home.join("config.toml")).unwrap()
            .parse::<toml::Table>().unwrap()["auth"]["mfa_enforced"].as_bool(), Some(true));
    }

    #[test]
    fn restore_cannot_remove_or_replace_installation_profile() {
        for restored in ["", "[product]\nprofile='standard'\n"] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path();
            let original = "[product]\nprofile='restricted'\n";
            std::fs::write(home.join("config.toml"), original).unwrap();
            let staging = staging_dir(home);
            std::fs::create_dir_all(&staging).unwrap();
            if !restored.is_empty() {
                std::fs::write(staging.join("config.toml"), restored).unwrap();
            }
            std::fs::write(staging.join("customer-data.txt"), "preserve me").unwrap();
            write_marker(home, &RestoreMarker {
                staged_at: Utc::now(), source_filename: "isolated-test.tar.gz".into(),
            }).unwrap();
            let result = perform_pending_restore_swap(home, "profile-test");
            assert!(matches!(result, Err(RestoreSwapError::Io(ref error)) if error.contains("installation-owned")));
            assert_eq!(std::fs::read_to_string(home.join("config.toml")).unwrap(), original);
            assert!(staging.join("customer-data.txt").exists());
            assert!(!home.join("restore-backup-profile-test").exists());
        }
    }

    #[test]
    fn no_marker_is_a_cheap_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            perform_pending_restore_swap(tmp.path(), "20260101T000000Z"),
            Ok(None)
        );
    }

    #[test]
    fn marker_present_but_staging_empty_clears_marker_and_errors_without_touching_home() {
        for empty_directory in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            std::fs::write(home.join("config.toml"), b"[general]\n").unwrap();
            write_marker(
                home,
                &RestoreMarker {
                    staged_at: Utc::now(),
                    source_filename: "x.tar.gz".into(),
                },
            )
            .unwrap();
            if empty_directory {
                std::fs::create_dir(staging_dir(home)).unwrap();
            }

            let result = perform_pending_restore_swap(home, "20260101T000000Z");
            assert_eq!(result, Err(RestoreSwapError::StagingMissingOrEmpty));
            assert!(
                read_marker(home).is_none(),
                "marker must be cleared so boot does not loop forever"
            );
            assert!(
                home.join("config.toml").is_file(),
                "existing data must be untouched"
            );
            assert!(!home.join("restore-backup-20260101T000000Z").exists());
        }
    }

    #[test]
    fn swap_preserves_old_data_and_moves_staged_content_into_place() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // This is an existing installation; keep the byte-preservation assertion.
        drop(duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap());
        // Old (current) data.
        std::fs::write(home.join("config.toml"), b"# old-config\n").unwrap();
        std::fs::create_dir_all(home.join("agents/kiki")).unwrap();
        std::fs::write(home.join("agents/kiki/SOUL.md"), b"old-soul").unwrap();

        // Staged new data, nested under the home basename (the realistic
        // shape `device.backup_create` produces).
        let staging = staging_dir(home);
        let nested = staging.join(home.file_name().unwrap());
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("config.toml"), b"# new-config\n").unwrap();
        std::fs::create_dir_all(nested.join("agents/miki")).unwrap();
        std::fs::write(nested.join("agents/miki/SOUL.md"), b"new-soul").unwrap();

        write_marker(
            home,
            &RestoreMarker {
                staged_at: Utc::now(),
                source_filename: "old-device.tar.gz".into(),
            },
        )
        .unwrap();

        let report = perform_pending_restore_swap(home, "20260101T000000Z")
            .unwrap()
            .unwrap();
        assert_eq!(
            report.preserved_dir,
            home.join("restore-backup-20260101T000000Z")
        );

        // New data is now live.
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap(),
            b"# new-config\n"
        );
        assert_eq!(
            std::fs::read(home.join("agents/miki/SOUL.md")).unwrap(),
            b"new-soul"
        );

        // Old data preserved, never deleted.
        let preserved = home.join("restore-backup-20260101T000000Z");
        assert_eq!(
            std::fs::read(preserved.join("config.toml")).unwrap(),
            b"# old-config\n"
        );
        assert_eq!(
            std::fs::read(preserved.join("agents/kiki/SOUL.md")).unwrap(),
            b"old-soul"
        );

        // Staging and marker are gone.
        assert!(!staging.exists());
        assert!(read_marker(home).is_none());
        // The LINE inbox is told the data came from an archive (F2).
        assert!(home.join(crate::channel_ingress::RESTORE_MARKER).exists());
    }

    /// F5 L5: no inbox marker, no restore. Nothing moves, the pending
    /// restore stays for the next boot.
    #[test]
    fn swap_stops_before_moving_anything_when_the_inbox_marker_cannot_be_written() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), b"# old-config\n").unwrap();
        // A directory where the marker file should go makes the write fail.
        std::fs::create_dir_all(home.join(crate::channel_ingress::RESTORE_MARKER)).unwrap();
        let staging = staging_dir(home);
        let nested = staging.join(home.file_name().unwrap());
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("config.toml"), b"# new-config\n").unwrap();
        write_marker(
            home,
            &RestoreMarker {
                staged_at: Utc::now(),
                source_filename: "old-device.tar.gz".into(),
            },
        )
        .unwrap();
        let err = perform_pending_restore_swap(home, "20260101T000000Z").unwrap_err();
        assert!(matches!(err, RestoreSwapError::Io(ref m) if m.contains("restore marker")));
        assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), b"# old-config\n");
        assert!(!home.join("restore-backup-20260101T000000Z").exists());
        assert!(read_marker(home).is_some(), "pending restore kept for the next boot");
        assert!(nested.join("config.toml").exists());
    }

    #[test]
    fn swap_falls_back_to_the_staging_root_when_not_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // This is an existing installation, not the strict empty-home restore.
        drop(duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap());
        std::fs::write(home.join("config.toml"), b"# old\n").unwrap();

        let staging = staging_dir(home);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("config.toml"), b"# new\n").unwrap();

        write_marker(
            home,
            &RestoreMarker {
                staged_at: Utc::now(),
                source_filename: "x.tar.gz".into(),
            },
        )
        .unwrap();
        perform_pending_restore_swap(home, "ts1").unwrap();
        assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), b"# new\n");
    }

    #[test]
    fn a_second_boot_with_no_new_marker_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), b"# old\n").unwrap();
        let staging = staging_dir(home);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("config.toml"), b"# new\n").unwrap();
        write_marker(
            home,
            &RestoreMarker {
                staged_at: Utc::now(),
                source_filename: "x.tar.gz".into(),
            },
        )
        .unwrap();

        assert!(perform_pending_restore_swap(home, "ts1").unwrap().is_some());
        // Marker is gone now — a second call must be a pure no-op, never
        // re-preserve/re-swap.
        assert_eq!(perform_pending_restore_swap(home, "ts2"), Ok(None));
        assert!(!home.join("restore-backup-ts2").exists());
    }
}

#[cfg(test)]
mod isolation_tests {
    #[test]
    fn parse_isolation_restore_refuses_before_read_or_staging() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("staging");
        assert_eq!(super::extract_tar_gz_with_isolation(
            &dir.path().join("missing"), &dest, &super::RestoreLimits::default(), true,
        ), Err(super::RestoreViolation::IsolationUnavailable));
        assert!(!dest.exists());
    }

    /// SR2 1c-u2: in native Linux protected mode a whole-home restore is
    /// refused with a plain-language reason (it could undo tombstones),
    /// both when staging and at the boot swap of an older staged restore.
    #[test]
    fn backup_restore_refused_in_protected_mode() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // Staging: refused before any lock, read, temp directory or marker.
        let error = super::stage_restore_with_protection(home, &home.join("missing.tar.gz"),
            super::RestoreProtection::Isolation).unwrap_err();
        assert_eq!(error, super::PROTECTED_RESTORE_REFUSED);
        assert!(error.contains("受保護模式") && error.contains("個人專屬員工"), "{error}");
        assert!(std::fs::read_dir(home).unwrap().next().is_none(), "nothing was written");
        // Boot: a restore staged before protection was enabled never swaps
        // in; nothing moves and the staged data and marker stay for review.
        let staging = super::staging_dir(home);
        std::fs::create_dir_all(staging.join("agents/alice-helper")).unwrap();
        std::fs::create_dir_all(home.join("agents")).unwrap();
        std::fs::write(home.join("users.db"), b"current").unwrap();
        super::write_marker(home, &super::RestoreMarker {
            staged_at: chrono::Utc::now(), source_filename: "before-tombstone.tar.gz".into() }).unwrap();
        assert_eq!(super::perform_pending_restore_swap_with_protection(home, "ts", super::RestoreProtection::Isolation),
            Err(super::RestoreSwapError::ProtectedMode(super::PROTECTED_RESTORE_REFUSED)));
        assert!(super::read_marker(home).is_some());
        assert!(staging.join("agents/alice-helper").is_dir());
        assert!(!home.join("agents/alice-helper").exists());
        assert_eq!(std::fs::read(home.join("users.db")).unwrap(), b"current");
        assert!(!home.join("restore-backup-ts").exists());
    }

    /// Build a whole-home archive (`duduclaw/...`, as `device.backup_create`
    /// writes it) from files under a scratch tree.
    fn archive_of(tree: &std::path::Path, out: &std::path::Path) {
        let file = std::fs::File::create(out).unwrap();
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        builder.append_dir_all("duduclaw", tree).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn home_with_name() -> (tempfile::TempDir, std::path::PathBuf) {
        let parent = tempfile::tempdir().unwrap();
        let home = parent.path().join("duduclaw");
        std::fs::create_dir(&home).unwrap();
        (parent, home)
    }

    fn sqlite(path: &std::path::Path, sql: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        rusqlite::Connection::open(path).unwrap().execute_batch(sql).unwrap();
    }

    /// L3: on a protected single-binary host a backup carrying isolation
    /// state (here: a confirmed employee and a Personal mirror row, as taken
    /// on an isolation host) is refused at staging, before publication: no
    /// staging tree, no marker, no request directory left behind. An
    /// unreadable store is refused too (fail closed). A plain v1.80 backup
    /// still stages, and an unprotected host is unchanged.
    #[test]
    fn l3_single_binary_restore_refuses_archive_with_isolation_state() {
        use super::RestoreProtection::{Off, SingleBinary};
        let scratch = tempfile::tempdir().unwrap();
        let isolation_tree = scratch.path().join("isolation");
        std::fs::create_dir_all(&isolation_tree).unwrap();
        std::fs::write(isolation_tree.join("config.toml"), b"").unwrap();
        sqlite(&isolation_tree.join("migration-authority/apply.db"),
            "CREATE TABLE confirmed_employees(label TEXT); INSERT INTO confirmed_employees VALUES('alice-personal');");
        sqlite(&isolation_tree.join("users.db"),
            "CREATE TABLE isolation_personal_agents(agent_name TEXT, owner_user_id TEXT);
             INSERT INTO isolation_personal_agents VALUES('alice-personal', 'u1');");
        let isolation_archive = scratch.path().join("isolation.tar.gz");
        archive_of(&isolation_tree, &isolation_archive);

        let unreadable_tree = scratch.path().join("unreadable");
        std::fs::create_dir_all(&unreadable_tree).unwrap();
        std::fs::write(unreadable_tree.join("migration-confirmations.db"), b"not a SQLite database").unwrap();
        let unreadable_archive = scratch.path().join("unreadable.tar.gz");
        archive_of(&unreadable_tree, &unreadable_archive);

        let plain_tree = scratch.path().join("plain");
        std::fs::create_dir_all(plain_tree.join("agents/dudu")).unwrap();
        std::fs::write(plain_tree.join("config.toml"), b"").unwrap();
        sqlite(&plain_tree.join("users.db"), "CREATE TABLE users(id TEXT); INSERT INTO users VALUES('u1');");
        let plain_archive = scratch.path().join("plain.tar.gz");
        archive_of(&plain_tree, &plain_archive);

        for (archive, refusal) in [(&isolation_archive, super::PROTECTED_RESTORE_ISOLATION_STATE),
            (&unreadable_archive, super::PROTECTED_RESTORE_ISOLATION_UNKNOWN)] {
            let (_parent, home) = home_with_name();
            let error = super::stage_restore_with_protection(&home, archive, SingleBinary).unwrap_err();
            assert_eq!(error, refusal);
            assert!(error.contains("受保護模式"), "{error}");
            assert!(super::read_marker(&home).is_none());
            assert!(!super::staging_dir(&home).exists());
            let leftovers: Vec<_> = std::fs::read_dir(&home).unwrap().map(|e| e.unwrap().file_name())
                .filter(|name| name.to_string_lossy().starts_with(".restore-request-")).collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
        }
        // Not protected: the same isolation archive stages as before.
        let (_parent, home) = home_with_name();
        assert!(super::stage_restore_with_protection(&home, &isolation_archive, Off).is_ok());
        // Single-binary host, plain v1.80 backup: stages.
        let (_parent, home) = home_with_name();
        assert!(super::stage_restore_with_protection(&home, &plain_archive, SingleBinary).is_ok());
        assert!(super::read_marker(&home).is_some());
    }

    /// L3 at the boot swap: an isolation-state archive staged earlier (before
    /// protection, or by an older build) never swaps in on a protected
    /// single-binary host; nothing moves and staging + marker stay.
    #[test]
    fn l3_single_binary_swap_refuses_staged_isolation_state() {
        let (_parent, home) = home_with_name();
        let root = super::staging_dir(&home).join("duduclaw");
        std::fs::create_dir_all(root.join("isolation-quarantine/0123-0-alice")).unwrap();
        std::fs::write(home.join("users.db"), b"current").unwrap();
        super::write_marker(&home, &super::RestoreMarker {
            staged_at: chrono::Utc::now(), source_filename: "isolation-host.tar.gz".into() }).unwrap();
        assert_eq!(super::perform_pending_restore_swap_with_protection(&home, "ts", super::RestoreProtection::SingleBinary),
            Err(super::RestoreSwapError::ProtectedMode(super::PROTECTED_RESTORE_ISOLATION_STATE)));
        assert!(super::read_marker(&home).is_some());
        assert!(root.join("isolation-quarantine").is_dir());
        assert!(!home.join("isolation-quarantine").exists());
        assert_eq!(std::fs::read(home.join("users.db")).unwrap(), b"current");
        assert!(!home.join("restore-backup-ts").exists());
    }
}
