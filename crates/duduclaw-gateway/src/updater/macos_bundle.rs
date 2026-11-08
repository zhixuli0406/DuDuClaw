//! RM-155: install a verified `duduclaw[-pro]-darwin-<arch>.app.zip` as
//! `DuDuClaw Gateway.app`.
//!
//! Pipeline (every step fails closed; nothing here reads the environment):
//! 1. **Strict extraction** ([`extract_bundle_zip`]) into a private 0700
//!    staging directory next to the target (`<parent>/.duduclaw-update-<uuid>/`,
//!    same volume): every entry under `DuDuClaw Gateway.app/`, UTF-8 names
//!    only, no absolute paths / `..` / backslashes, regular files and
//!    directories only (no symlinks, no setuid), size budgets, exec bit only on
//!    `Contents/MacOS/<exe>`, exactly one executable named after the running
//!    edition.
//! 2. **Code-signature gate** ([`verify_staged_bundle`]): pinned designated
//!    requirement, the installed bundle's own DR (skipped only when that
//!    install is ad-hoc), `Info.plist` identifier + version == the
//!    minisign-verified release identity, edition guard
//!    (`CFBundleExecutable`), release marker inside the executable.
//! 3. **Swap** ([`install_staged_bundle`]): `renamex_np(RENAME_SWAP)` (or
//!    `RENAME_EXCL` for a first install), rename-aside fallback when the
//!    volume does not support it, parents fsynced, rollback on failure.
//! 4. **Post-swap verify** (signature + `version` of the installed path),
//!    swapping back on failure, then cleanup.
//!
//! The pure parts compile and are tested on every host; only the `codesign`,
//! `plutil`, `renamex_np` adapters and the orchestration are macOS-only.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use super::*;
use duduclaw_core::macos_bundle::{BUNDLE_DIR_NAME, BUNDLE_ID, InfoPlistReader, PINNED_DR, STAGING_PREFIX};
use std::io;
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use tracing::{info, warn};

/// Upper bound on zip entries in a bundle archive (a real bundle has < 20).
pub(super) const MAX_BUNDLE_ENTRIES: usize = 4096;
/// Longest accepted entry name, in bytes.
const MAX_ENTRY_NAME: usize = 1024;

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File { executable: bool },
}

#[derive(Debug)]
struct PlannedEntry {
    index: usize,
    /// Components below `DuDuClaw Gateway.app/` (empty = the root itself).
    rel: Vec<String>,
    kind: EntryKind,
}

/// Split a raw zip entry name into validated components. Returns the
/// components and whether the name ends in `/` (directory marker).
fn parse_entry_name(raw: &[u8]) -> Result<(Vec<String>, bool), String> {
    let name = std::str::from_utf8(raw).map_err(|_| "Bundle archive entry name is not UTF-8")?;
    if name.is_empty() || name.len() > MAX_ENTRY_NAME || name.contains('\0') || name.contains('\\') {
        return Err(format!("Bundle archive entry has an invalid name: {}", name.escape_debug()));
    }
    if name.starts_with('/') {
        return Err(format!("Bundle archive entry is absolute: {name}"));
    }
    let dir_marker = name.ends_with('/');
    let body = name.strip_suffix('/').unwrap_or(name);
    let mut parts = Vec::new();
    for part in body.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            return Err(format!("Bundle archive entry has an unsafe path: {name}"));
        }
        parts.push(part.to_owned());
    }
    if parts.first().map(String::as_str) != Some(BUNDLE_DIR_NAME) {
        return Err(format!("Bundle archive entry is outside {BUNDLE_DIR_NAME}/: {name}"));
    }
    parts.remove(0);
    Ok((parts, dir_marker))
}

/// Classify an entry from its unix mode / flags. Symlinks, devices, FIFOs,
/// sockets and setuid/setgid/sticky bits are refused outright.
fn entry_kind(unix_mode: Option<u32>, dir_marker: bool, is_symlink: bool) -> Result<EntryKind, String> {
    if is_symlink {
        return Err("Bundle archive contains a symlink".into());
    }
    let kind = match unix_mode {
        Some(mode) => {
            if mode & 0o7000 != 0 {
                return Err("Bundle archive entry has setuid/setgid/sticky bits".into());
            }
            match mode & S_IFMT {
                S_IFLNK => return Err("Bundle archive contains a symlink".into()),
                S_IFDIR => EntryKind::Dir,
                S_IFREG => EntryKind::File { executable: mode & 0o111 != 0 },
                0 if dir_marker => EntryKind::Dir,
                0 => EntryKind::File { executable: mode & 0o111 != 0 },
                _ => return Err("Bundle archive contains a special file".into()),
            }
        }
        None if dir_marker => EntryKind::Dir,
        None => EntryKind::File { executable: false },
    };
    if (kind == EntryKind::Dir) != dir_marker {
        return Err("Bundle archive entry type does not match its name".into());
    }
    Ok(kind)
}

/// Raw unix modes straight from the central directory (one per entry, in
/// central-directory order, `None` when the entry was not made on a unix host).
/// This does not depend on how the zip crate post-processes `unix_mode()`, so
/// setuid/setgid/sticky bits can never be lost before validation.
fn raw_central_modes(bytes: &[u8]) -> Result<Vec<Option<u32>>, String> {
    const BAD: &str = "Bundle archive has a malformed central directory";
    let u16_at = |o: usize| bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let min = bytes.len().saturating_sub(22 + 65_535);
    let eocd = (min..=bytes.len().saturating_sub(22))
        .rev()
        .find(|&i| bytes.get(i..i + 4) == Some(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or(BAD)?;
    let count = u16_at(eocd + 10).ok_or(BAD)?;
    let cd_size = u32_at(eocd + 12).ok_or(BAD)? as usize;
    let cd_offset = u32_at(eocd + 16).ok_or(BAD)? as usize;
    if count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        return Err("Zip64 bundle archives are not supported".into());
    }
    // Tolerate prepended data: the central directory ends where the EOCD starts.
    let mut pos = eocd.checked_sub(cd_size).ok_or(BAD)?;
    let _ = cd_offset;
    let mut modes = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(pos) != Some(0x0201_4b50) {
            return Err(BAD.into());
        }
        let host = bytes.get(pos + 5).copied().ok_or(BAD)?;
        let attrs = u32_at(pos + 38).ok_or(BAD)?;
        modes.push(if host == 3 { Some(attrs >> 16) } else { None });
        let (n, x, c) = (u16_at(pos + 28).ok_or(BAD)?, u16_at(pos + 30).ok_or(BAD)?, u16_at(pos + 32).ok_or(BAD)?);
        pos = pos.checked_add(46 + n + x + c).ok_or(BAD)?;
    }
    Ok(modes)
}

/// Header-only validation of the whole archive before anything is written.
fn plan_bundle_zip<R: io::Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    exe_name: &str,
    raw_modes: &[Option<u32>],
) -> Result<Vec<PlannedEntry>, String> {
    if archive.len() > MAX_BUNDLE_ENTRIES {
        return Err(format!("Bundle archive has too many entries ({})", archive.len()));
    }
    if raw_modes.len() != archive.len() {
        return Err("Bundle archive central directory is inconsistent".into());
    }
    let mut planned = Vec::with_capacity(archive.len());
    let mut seen = std::collections::BTreeSet::new();
    let mut declared = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| format!("Bundle archive entry error: {e}"))?;
        let (rel, dir_marker) = parse_entry_name(entry.name_raw())?;
        // Refuse setuid/setgid/sticky on files AND directories, using both the
        // crate's view and the raw central-directory attributes.
        if raw_modes[index].is_some_and(|m| m & 0o7000 != 0) {
            return Err("Bundle archive entry has setuid/setgid/sticky bits".into());
        }
        let kind = entry_kind(entry.unix_mode().or(raw_modes[index]), dir_marker || entry.is_dir(), entry.is_symlink())?;
        if rel.is_empty() && kind != EntryKind::Dir {
            return Err(format!("{BUNDLE_DIR_NAME} must be a directory"));
        }
        // APFS/HFS+ are case-insensitive by default: two spellings of one
        // path would silently overwrite each other.
        if !seen.insert(rel.join("/").to_lowercase()) {
            return Err(format!("Bundle archive has a duplicate entry: {}", rel.join("/")));
        }
        if let EntryKind::File { .. } = kind {
            check_size(entry.size(), MAX_BINARY_BYTES, "Bundle archive entry")?;
            declared = declared.checked_add(entry.size()).ok_or("Bundle archive size overflow")?;
            check_size(declared, MAX_ARCHIVE_BYTES, "Decompressed bundle archive")?;
        }
        planned.push(PlannedEntry { index, rel, kind });
    }

    let main_exe = ["Contents", "MacOS", exe_name];
    let files: std::collections::BTreeSet<String> = planned.iter()
        .filter(|e| matches!(e.kind, EntryKind::File { .. }))
        .map(|e| e.rel.join("/").to_lowercase()).collect();
    let mut executables = 0;
    let mut has_info_plist = false;
    for entry in &planned {
        // A file may not also be used as a directory by another entry.
        for depth in 1..entry.rel.len() {
            if files.contains(&entry.rel[..depth].join("/").to_lowercase()) {
                return Err(format!("Bundle archive uses a file as a directory: {}", entry.rel.join("/")));
            }
        }
        let in_macos_dir = entry.rel.len() > 2 && entry.rel[0] == "Contents" && entry.rel[1] == "MacOS";
        let is_main = entry.rel.iter().map(String::as_str).eq(main_exe.iter().copied());
        match entry.kind {
            EntryKind::File { executable: true } if is_main => executables += 1,
            EntryKind::File { executable: true } => {
                return Err(format!("Only Contents/MacOS/{exe_name} may be executable (found {})", entry.rel.join("/")));
            }
            _ if in_macos_dir => {
                return Err(format!("Contents/MacOS/ may only hold {exe_name} (found {})", entry.rel.join("/")));
            }
            EntryKind::File { executable: false } if entry.rel == ["Contents", "Info.plist"] => has_info_plist = true,
            _ => {}
        }
    }
    if executables != 1 {
        return Err(format!("Bundle archive must contain exactly one executable, Contents/MacOS/{exe_name}"));
    }
    if !has_info_plist {
        return Err("Bundle archive has no Contents/Info.plist".into());
    }
    Ok(planned)
}

fn create_dir_private(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(mode).create(path)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::create_dir(path)
    }
}

/// Create every missing directory from `root` (exclusive) down to `dir`.
fn create_dirs_under(root: &Path, rel: &[String]) -> io::Result<()> {
    let mut path = root.to_path_buf();
    for part in rel {
        path.push(part);
        match create_dir_private(&path, 0o755) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !std::fs::symlink_metadata(&path)?.is_dir() {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "path component is not a directory"));
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Strictly extract `archive_bytes` into `staging_dir` (fresh, private) and
/// return the staged `DuDuClaw Gateway.app` path. `exe_name` is
/// [`archive_binary_name`] of the running executable (edition guard).
pub(super) fn extract_bundle_zip(archive_bytes: &[u8], staging_dir: &Path, exe_name: &str) -> Result<PathBuf, String> {
    use std::io::Read;
    check_size(archive_bytes.len() as u64, MAX_DOWNLOAD_BYTES, "Compressed bundle archive")?;
    let mut archive = zip::ZipArchive::new(io::Cursor::new(archive_bytes))
        .map_err(|e| format!("Invalid bundle zip: {e}"))?;
    let raw_modes = raw_central_modes(archive_bytes)?;
    let planned = plan_bundle_zip(&mut archive, exe_name, &raw_modes)?;
    let root = staging_dir.join(BUNDLE_DIR_NAME);
    create_dir_private(&root, 0o755).map_err(|e| format!("Cannot create staged bundle: {e}"))?;
    let mut remaining = MAX_ARCHIVE_BYTES;
    for entry in &planned {
        let io_err = |e: io::Error| format!("Bundle extraction failed at {}: {e}", entry.rel.join("/"));
        match entry.kind {
            EntryKind::Dir => create_dirs_under(&root, &entry.rel).map_err(io_err)?,
            EntryKind::File { executable } => {
                let (name, parents) = entry.rel.split_last().ok_or("Bundle root cannot be a file")?;
                create_dirs_under(&root, parents).map_err(io_err)?;
                let path = parents.iter().fold(root.clone(), |p, c| p.join(c)).join(name);
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(if executable { 0o755 } else { 0o644 });
                }
                #[cfg(not(unix))]
                let _ = executable;
                let mut out = options.open(&path).map_err(io_err)?;
                let mut file = archive.by_index(entry.index).map_err(|e| format!("Bundle archive entry error: {e}"))?;
                let limit = MAX_BINARY_BYTES.min(remaining);
                let written = io::copy(&mut (&mut file).take(limit.saturating_add(1)), &mut out)
                    .map_err(|e| format!("Bundle extraction failed at {}: {e}", entry.rel.join("/")))?;
                if written > limit {
                    return Err("Decompressed bundle archive exceeds size limit".into());
                }
                remaining -= written;
                out.sync_all().map_err(io_err)?;
            }
        }
    }
    sync_dir(&root).map_err(|e| format!("Cannot sync staged bundle: {e}"))?;
    Ok(root)
}

pub(super) fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Create `<parent>/.duduclaw-update-<uuid>/` with mode 0700, owned by this
/// user, on the same volume as `parent`.
pub(super) fn create_staging_dir(parent: &Path) -> Result<PathBuf, String> {
    let dir = parent.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4()));
    create_dir_private(&dir, 0o700).map_err(|e| format!("Cannot create update staging directory: {e}"))?;
    let check = || -> Result<(), String> {
        let meta = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
        if !meta.is_dir() {
            return Err("Update staging path is not a directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let parent_meta = std::fs::metadata(parent).map_err(|e| e.to_string())?;
            if meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o077 != 0 {
                return Err("Update staging directory must be private to this user".into());
            }
            if meta.dev() != parent_meta.dev() {
                return Err("Update staging directory is on a different volume".into());
            }
        }
        Ok(())
    };
    if let Err(e) = check() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(dir)
}

// ---------------------------------------------------------------------------
// T7c: code-signature gate
// ---------------------------------------------------------------------------

/// What `codesign -d` reports about an installed bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SignatureInfo {
    pub adhoc: bool,
    pub designated: Option<String>,
}

/// `codesign` behind a trait so the gate's decisions are unit-testable.
pub(super) trait CodeSign: Sync {
    /// `codesign --verify --strict --deep -R "=<requirement>" <bundle>`.
    fn verify(&self, bundle: &Path, requirement: &str) -> Result<(), String>;
    /// `codesign -d --verbose=2 -r- <bundle>` (designated requirement + ad-hoc flag).
    fn signature_info(&self, bundle: &Path) -> Result<SignatureInfo, String>;
}

/// The `designated => …` line of `codesign -d -r-` output.
pub(super) fn parse_designated(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("designated => "))
        .map(|dr| dr.trim().to_owned())
        .filter(|dr| !dr.is_empty())
}

/// Whether `codesign -dv` output describes an ad-hoc signature.
pub(super) fn parse_adhoc(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line == "Signature=adhoc"
            || line.strip_prefix("CodeDirectory ").is_some_and(|rest| {
                rest.split_whitespace().any(|f| f.starts_with("flags=") && f.contains("(adhoc"))
            })
    })
}

/// Verify a staged bundle before it may replace anything.
///
/// `current_bundle` is the installed gateway bundle (update) or `None`
/// (first install / migration from a bare binary, whose identity differs by
/// design). Its DR is enforced on the new bundle unless it is ad-hoc.
pub(super) fn verify_staged_bundle(
    cs: &dyn CodeSign,
    plist: &dyn InfoPlistReader,
    staged: &Path,
    current_bundle: Option<&Path>,
    exe_name: &str,
    expected_version: &str,
) -> Result<(), String> {
    cs.verify(staged, PINNED_DR)
        .map_err(|e| format!("Bundle signature does not satisfy the pinned requirement: {e}"))?;
    if let Some(current) = current_bundle {
        let info = cs.signature_info(current)
            .map_err(|e| format!("Cannot read the installed bundle's signature: {e}"))?;
        if info.adhoc {
            warn!(bundle = %current.display(),
                "Installed DuDuClaw Gateway.app is ad-hoc signed; skipping its designated-requirement check");
        } else {
            let dr = info.designated.ok_or("Installed bundle has no designated requirement")?;
            cs.verify(staged, &dr)
                .map_err(|e| format!("Bundle signature does not satisfy the installed bundle's requirement: {e}"))?;
        }
    }
    let id = plist.bundle_identifier(staged)?;
    if id != BUNDLE_ID {
        return Err(format!("Bundle identifier mismatch — expected {BUNDLE_ID}, found {id}"));
    }
    let version = plist.read_key(staged, "CFBundleShortVersionString")?;
    if version != expected_version {
        return Err(format!("Bundle version mismatch — expected {expected_version}, found {version}"));
    }
    let staged_exe = plist.read_key(staged, "CFBundleExecutable")?;
    if staged_exe != exe_name {
        return Err(format!("Bundle edition mismatch — expected {exe_name}, found {staged_exe}"));
    }
    if let Some(current) = current_bundle {
        let current_exe = plist.read_key(current, "CFBundleExecutable")?;
        if current_exe != staged_exe {
            return Err(format!("Bundle edition mismatch — installed {current_exe}, update {staged_exe}"));
        }
    }
    let exe = duduclaw_core::macos_bundle::bundle_executable(staged, exe_name);
    let mut file = std::fs::File::open(&exe).map_err(|e| format!("Cannot open staged executable: {e}"))?;
    release_identity::verify_binary_version(&mut file, expected_version)
}

#[cfg(target_os = "macos")]
pub(super) struct SystemCodeSign;

#[cfg(target_os = "macos")]
impl CodeSign for SystemCodeSign {
    fn verify(&self, bundle: &Path, requirement: &str) -> Result<(), String> {
        let output = std::process::Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--deep", "-R"])
            .arg(format!("={requirement}"))
            .arg(bundle)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("/usr/bin/codesign: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().chars().take(1024).collect())
        }
    }

    fn signature_info(&self, bundle: &Path) -> Result<SignatureInfo, String> {
        let output = std::process::Command::new("/usr/bin/codesign")
            .args(["-d", "--verbose=2", "-r-"])
            .arg(bundle)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("/usr/bin/codesign: {e}"))?;
        let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if !output.status.success() {
            return Err(text.trim().chars().take(1024).collect());
        }
        Ok(SignatureInfo { adhoc: parse_adhoc(&text), designated: parse_designated(&text) })
    }
}

// ---------------------------------------------------------------------------
// T7d: swap / rollback / cleanup
// ---------------------------------------------------------------------------

/// Directory-rename primitives, behind a trait for fault injection.
pub(super) trait SwapOps {
    /// Atomically exchange `a` and `b` (`renamex_np(RENAME_SWAP)`).
    fn swap(&self, a: &Path, b: &Path) -> io::Result<()>;
    /// Rename refusing to replace an existing `to` (`renamex_np(RENAME_EXCL)`).
    fn rename_excl(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Plain `rename(2)`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
}

pub(super) struct SystemSwap;

#[cfg(target_os = "macos")]
fn renamex(from: &Path, to: &Path, flags: libc::c_uint) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

impl SwapOps for SystemSwap {
    fn swap(&self, a: &Path, b: &Path) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            renamex(a, b, libc::RENAME_SWAP)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (a, b);
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }

    fn rename_excl(&self, from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            renamex(from, to, libc::RENAME_EXCL)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (from, to);
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }
}

/// The volume cannot do `renamex_np` (non-APFS/HFS+, SMB, …).
fn unsupported(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EINVAL))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Where the previous bundle went, so a failed post-swap check can undo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Installed {
    /// Atomic exchange: the old bundle now sits at the staged path.
    Swapped { old: PathBuf },
    /// Fallback: the old bundle was renamed aside to `old`.
    MovedAside { old: PathBuf },
    /// First install: nothing was replaced.
    Fresh,
}

/// Put `staged` (inside `staging_dir`) at `target`.
pub(super) fn install_staged_bundle(
    ops: &dyn SwapOps,
    staged: &Path,
    staging_dir: &Path,
    target: &Path,
) -> Result<Installed, String> {
    let parent = target.parent().ok_or("Bundle target has no parent directory")?;
    let sync = || {
        sync_dir(parent).and_then(|()| sync_dir(staging_dir))
            .map_err(|e| format!("Failed to sync bundle directories: {e}"))
    };
    let exists = match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.is_dir() => true,
        Ok(_) => return Err(format!("{} exists and is not a directory", target.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(format!("Cannot inspect {}: {e}", target.display())),
    };
    if !exists {
        match ops.rename_excl(staged, target) {
            Ok(()) => {}
            Err(e) if unsupported(&e) => {
                if std::fs::symlink_metadata(target).is_ok() {
                    return Err(format!("{} appeared during install", target.display()));
                }
                ops.rename(staged, target).map_err(|e| format!("Failed to install bundle: {e}"))?;
            }
            Err(e) => return Err(format!("Failed to install bundle: {e}")),
        }
        sync()?;
        return Ok(Installed::Fresh);
    }
    match ops.swap(staged, target) {
        Ok(()) => {
            sync()?;
            Ok(Installed::Swapped { old: staged.to_path_buf() })
        }
        Err(e) if unsupported(&e) => {
            let aside = staging_dir.join("previous.app");
            ops.rename(target, &aside).map_err(|e| format!("Failed to move the installed bundle aside: {e}"))?;
            if let Err(e) = ops.rename(staged, target) {
                return match ops.rename(&aside, target) {
                    Ok(()) => Err(format!("Failed to install bundle (rolled back): {e}")),
                    Err(rb) => Err(format!("Failed to install bundle: {e}; rollback failed: {rb}. Previous bundle: {}", aside.display())),
                };
            }
            sync()?;
            Ok(Installed::MovedAside { old: aside })
        }
        Err(e) => Err(format!("Failed to swap bundle: {e}")),
    }
}

/// Undo [`install_staged_bundle`] after a failed post-swap check. The
/// rejected bundle ends up inside `staging_dir` for cleanup.
pub(super) fn restore_previous(
    ops: &dyn SwapOps,
    installed: &Installed,
    staging_dir: &Path,
    target: &Path,
) -> Result<(), String> {
    let rejected = staging_dir.join("rejected.app");
    let result = match installed {
        Installed::Swapped { old } => ops.swap(old, target),
        Installed::MovedAside { old } => ops.rename(target, &rejected).and_then(|()| ops.rename(old, target)),
        Installed::Fresh => ops.rename(target, &rejected),
    };
    result.map_err(|e| format!("Failed to restore the previous bundle: {e}"))?;
    if let Some(parent) = target.parent() {
        let _ = sync_dir(parent);
    }
    let _ = sync_dir(staging_dir);
    Ok(())
}

/// Boot cleanup: remove `<parent>/.duduclaw-update-*` directories left by an
/// interrupted update — only real directories owned by this user, older than
/// `min_age` (never a concurrent update's), and not containing `running_exe`.
/// Bare-binary staging *files* with the same prefix are left alone.
pub(super) fn cleanup_stale_staging(parent: &Path, running_exe: &Path, min_age: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(parent) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_str().is_some_and(|n| n.starts_with(STAGING_PREFIX)) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.is_dir() || running_exe.starts_with(&path) {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != unsafe { libc::getuid() } {
                continue;
            }
        }
        let old_enough = meta.modified().ok()
            .map(|m| m.elapsed().unwrap_or_default())
            .is_some_and(|age| age >= min_age);
        if old_enough && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// T8: orchestration (macOS)
// ---------------------------------------------------------------------------

/// Shown once after the first install of the bundle (dashboard + CLI).
pub const FOLDER_ACCESS_NOTICE: &str = "DuDuClaw Gateway.app 已安裝。macOS 會針對新的 App 詢問一次資料夾存取權限（之後的更新不會再詢問）。\
macOS will ask once for folder access for DuDuClaw Gateway.app; later updates keep the grant.";

/// Install path chosen for this bundle archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BundleInstallMode {
    /// Running from (or a gateway bundle already exists at) `target`.
    Update { target: PathBuf },
    /// Bare binary → first bundle install at `target`.
    Migrate { target: PathBuf },
}

/// Replace the old bare executable with a symlink to the bundle executable
/// atomically (temp symlink in the same directory + rename over the file).
/// The running process keeps its inode; CLI shims and old `.mcp.json`
/// commands keep resolving.
#[cfg(unix)]
pub(super) fn replace_with_symlink(bare: &Path, bundle_exe: &Path) -> Result<(), String> {
    let dir = bare.parent().ok_or("Bare executable has no parent directory")?;
    let meta = std::fs::symlink_metadata(bare).map_err(|e| format!("Cannot inspect {}: {e}", bare.display()))?;
    if !(meta.is_file() || meta.file_type().is_symlink()) {
        return Err(format!("{} is not a file", bare.display()));
    }
    let tmp = dir.join(format!(".duduclaw-link-{}", uuid::Uuid::new_v4()));
    std::os::unix::fs::symlink(bundle_exe, &tmp).map_err(|e| format!("Cannot create CLI symlink: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp, bare) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Cannot replace {} with a symlink: {e}", bare.display()));
    }
    sync_dir(dir).map_err(|e| format!("Cannot sync {}: {e}", dir.display()))
}

#[cfg(target_os = "macos")]
fn bundle_mode(current_exe: &Path) -> Result<BundleInstallMode, String> {
    use duduclaw_core::macos_bundle::{BundleKind, PlutilReader, classify, migration_target_root};
    if let Some(root) = duduclaw_core::macos_bundle::running_gateway_bundle() {
        return Ok(BundleInstallMode::Update { target: root });
    }
    if duduclaw_core::macos_bundle::bundle_root_of(current_exe).is_some() {
        return Err("Running from an app bundle that is not DuDuClaw Gateway.app — refusing to touch it".into());
    }
    match detect_install_method() {
        InstallMethod::Standalone | InstallMethod::Npm | InstallMethod::Pro => {}
        other => return Err(format!("This install ({other:?}) cannot migrate to DuDuClaw Gateway.app")),
    }
    let home = dirs::home_dir().filter(|h| h.is_absolute()).ok_or("Cannot determine home directory")?;
    let target = migration_target_root(current_exe, &home);
    match std::fs::symlink_metadata(&target) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BundleInstallMode::Migrate { target }),
        Ok(meta) if meta.is_dir() => {
            let exe = duduclaw_core::macos_bundle::bundle_executable(&target, archive_binary_name(current_exe));
            match classify(&exe, &PlutilReader) {
                // e.g. install.sh already placed the bundle, but the gateway
                // still runs bare: update that bundle (its DR is enforced).
                BundleKind::Gateway(_) => Ok(BundleInstallMode::Update { target }),
                _ => Err(format!("{} exists and is not DuDuClaw Gateway.app — refusing to replace it", target.display())),
            }
        }
        Ok(_) => Err(format!("{} exists and is not a directory", target.display())),
        Err(e) => Err(format!("Cannot inspect {}: {e}", target.display())),
    }
}

/// Run `<exe> version` with the updater's timeout and check the version.
#[cfg(target_os = "macos")]
async fn run_version_check(exe: &Path, expected: &str) -> Result<(), String> {
    let exe = exe.to_path_buf();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(VERIFY_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(&exe).arg("version")
                .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).output()
        }),
    ).await
    .map_err(|_| format!("Bundle verification timed out after {VERIFY_TIMEOUT_SECS}s"))?
    .map_err(|e| format!("Verification task panicked: {e}"))?
    .map_err(|e| format!("Cannot execute installed bundle: {e}"))?;
    if !output.status.success() {
        return Err(format!("Installed bundle verification failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    release_identity::verify_binary_version_output(&String::from_utf8_lossy(&output.stdout), expected)
}

/// Install a minisign-verified `.app.zip` (identity already checked against
/// these bytes by the caller). Returns the result and the installed
/// executable path (used for the relaunch).
#[cfg(target_os = "macos")]
pub(super) async fn install_bundle_archive(
    archive_bytes: &[u8],
    current_exe: &Path,
    identity: &VerifiedReleaseIdentity,
) -> Result<(ApplyResult, PathBuf), String> {
    let exe_name = archive_binary_name(current_exe);
    let mode = bundle_mode(current_exe)?;
    // The running process is bare: after the install, point the old entry
    // points (CLI path, LaunchAgent) at the bundle — also when adopting a
    // bundle install.sh already placed.
    let migrating = duduclaw_core::macos_bundle::running_gateway_bundle().is_none();
    let (target, current_bundle) = match &mode {
        BundleInstallMode::Update { target } => (target.clone(), Some(target.clone())),
        BundleInstallMode::Migrate { target } => (target.clone(), None),
    };
    let parent = target.parent().ok_or("Bundle target has no parent directory")?.to_path_buf();
    if current_bundle.is_none() && !parent.exists() {
        std::fs::create_dir_all(&parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    if duduclaw_core::platform::is_unsafe_update_dir(&parent) {
        return Err(format!("Bundle directory {} is writable by non-admin users — refusing update for safety",
            parent.display()));
    }
    let staging = create_staging_dir(&parent)?;
    let result = install_into(archive_bytes, &staging, &target, current_bundle, exe_name, identity.version()).await;
    let _ = std::fs::remove_dir_all(&staging);
    let installed_exe = result?;

    let mut message = String::from("Update installed. The service will restart automatically.");
    if migrating {
        // The bundle is in place and verified; the remaining steps only
        // point existing entry points at it and never undo the install.
        if let Err(e) = replace_with_symlink(current_exe, &installed_exe) {
            warn!(error = %e, "Bundle installed but the old executable could not be replaced with a symlink");
        }
        let _ = tokio::task::spawn_blocking({
            let exe = installed_exe.clone();
            move || duduclaw_core::autostart::migrate_launchd_plists_to(&exe, Some(BUNDLE_ID))
        }).await;
        mark_folder_access_notice();
        message = format!("{message} {FOLDER_ACCESS_NOTICE}");
    }
    info!(installed = %installed_exe.display(), migrating, "DuDuClaw Gateway.app installed");
    Ok((ApplyResult { success: true, message, needs_restart: true, restart_hint: macos_launchd_restart_hint() },
        installed_exe))
}

#[cfg(target_os = "macos")]
async fn install_into(
    archive_bytes: &[u8],
    staging: &Path,
    target: &Path,
    current_bundle: Option<PathBuf>,
    exe_name: &'static str,
    version: &str,
) -> Result<PathBuf, String> {
    let bytes = archive_bytes.to_vec();
    let staging_dir = staging.to_path_buf();
    let version_s = version.to_owned();
    let staged = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        let staged = extract_bundle_zip(&bytes, &staging_dir, exe_name)
            .map_err(|e| format!("Update extraction failed: {e}"))?;
        verify_staged_bundle(&SystemCodeSign, &duduclaw_core::macos_bundle::PlutilReader, &staged,
            current_bundle.as_deref(), exe_name, &version_s)
            .map_err(|e| format!("Update verification failed: {e}"))?;
        Ok(staged)
    }).await.map_err(|e| format!("Bundle staging task panicked: {e}"))??;

    let (staging_dir, target_p) = (staging.to_path_buf(), target.to_path_buf());
    let installed = tokio::task::spawn_blocking(move || install_staged_bundle(&SystemSwap, &staged, &staging_dir, &target_p))
        .await.map_err(|e| format!("Bundle swap task panicked: {e}"))??;

    let installed_exe = duduclaw_core::macos_bundle::bundle_executable(target, exe_name);
    let post = async {
        let t = target.to_path_buf();
        tokio::task::spawn_blocking(move || SystemCodeSign.verify(&t, PINNED_DR))
            .await.map_err(|e| format!("Verification task panicked: {e}"))?
            .map_err(|e| format!("Installed bundle signature check failed: {e}"))?;
        run_version_check(&installed_exe, version).await
    }.await;
    if let Err(e) = post {
        let (staging_dir, target_p) = (staging.to_path_buf(), target.to_path_buf());
        let restored = tokio::task::spawn_blocking(move || restore_previous(&SystemSwap, &installed, &staging_dir, &target_p))
            .await.map_err(|e| format!("Bundle restore task panicked: {e}"))?;
        return Err(match restored {
            Ok(()) => format!("Update verification failed (previous bundle restored): {e}"),
            Err(rb) => format!("Update verification failed: {e}; {rb}"),
        });
    }
    Ok(installed_exe)
}

// ── one-time folder-access notice ───────────────────────────────────────────

fn notice_marker() -> PathBuf {
    duduclaw_core::platform::duduclaw_home().join("state").join("macos_bundle_notice_pending")
}

fn mark_folder_access_notice() {
    let path = notice_marker();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, b"1");
}

/// The folder-access notice, returned exactly once after a bundle migration
/// (the marker is consumed). `None` otherwise and off macOS.
pub fn take_folder_access_notice() -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    std::fs::remove_file(notice_marker()).ok().map(|()| FOLDER_ACCESS_NOTICE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    const ROOT: &str = "DuDuClaw Gateway.app";

    enum E<'a> { Dir(&'a str), File(&'a str, u32, &'a [u8]), Link(&'a str, &'a str), Raw(&'a str, u32, &'a [u8]) }

    fn zip_of(entries: &[E]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for e in entries {
            match e {
                E::Dir(n) => w.add_directory(*n, SimpleFileOptions::default().unix_permissions(0o755)).unwrap(),
                E::File(n, mode, data) => {
                    w.start_file(*n, SimpleFileOptions::default().unix_permissions(*mode)).unwrap();
                    w.write_all(data).unwrap();
                }
                E::Raw(n, attrs, data) => {
                    w.start_file(*n, SimpleFileOptions::default().external_attributes(attrs << 16)).unwrap();
                    w.write_all(data).unwrap();
                }
                E::Link(n, t) => w.add_symlink(*n, *t, SimpleFileOptions::default()).unwrap(),
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn good<'a>(exe: &'a str) -> Vec<E<'a>> {
        vec![
            E::Dir("DuDuClaw Gateway.app/"),
            E::Dir("DuDuClaw Gateway.app/Contents/"),
            E::File("DuDuClaw Gateway.app/Contents/Info.plist", 0o644, b"<plist/>"),
            E::Dir("DuDuClaw Gateway.app/Contents/MacOS/"),
            E::File(exe, 0o755, b"binary"),
            E::File("DuDuClaw Gateway.app/Contents/_CodeSignature/CodeResources", 0o644, b"sig"),
            E::File("DuDuClaw Gateway.app/Contents/Resources/AppIcon.icns", 0o644, b"icon"),
        ]
    }

    const EXE: &str = "DuDuClaw Gateway.app/Contents/MacOS/duduclaw";

    #[test]
    fn strict_extractor_accepts_a_signed_bundle_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = extract_bundle_zip(&zip_of(&good(EXE)), dir.path(), "duduclaw").unwrap();
        assert_eq!(root, dir.path().join(ROOT));
        assert_eq!(std::fs::read(root.join("Contents/MacOS/duduclaw")).unwrap(), b"binary");
        assert_eq!(std::fs::read(root.join("Contents/Resources/AppIcon.icns")).unwrap(), b"icon");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &str| std::fs::metadata(root.join(p)).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode("Contents/MacOS/duduclaw"), 0o755);
            assert_eq!(mode("Contents/Info.plist"), 0o644);
        }
    }

    #[test]
    fn strict_extractor_refuses_unsafe_archives_without_writing_them() {
        let pro = "DuDuClaw Gateway.app/Contents/MacOS/duduclaw-pro";
        let cases: Vec<(&str, Vec<E>)> = vec![
            ("symlink", { let mut g = good(EXE); g.push(E::Link("DuDuClaw Gateway.app/Contents/Resources/x", "/etc/passwd")); g }),
            ("symlink mode", { let mut g = good(EXE); g.push(E::Raw("DuDuClaw Gateway.app/Contents/Resources/y", 0o120777, b"/etc")); g }),
            ("setuid", { let mut g = good(EXE); g.push(E::Raw("DuDuClaw Gateway.app/Contents/Resources/z", 0o104644, b"z")); g }),
            ("fifo", { let mut g = good(EXE); g.push(E::Raw("DuDuClaw Gateway.app/Contents/Resources/f", 0o010644, b"")); g }),
            ("outside root", { let mut g = good(EXE); g.push(E::File("Other.app/Contents/Info.plist", 0o644, b"x")); g }),
            ("parent dir", { let mut g = good(EXE); g.push(E::File("DuDuClaw Gateway.app/../evil", 0o644, b"x")); g }),
            ("extra executable", { let mut g = good(EXE); g.push(E::File("DuDuClaw Gateway.app/Contents/Resources/run.sh", 0o755, b"x")); g }),
            ("second macos file", { let mut g = good(EXE); g.push(E::File("DuDuClaw Gateway.app/Contents/MacOS/helper", 0o644, b"x")); g }),
            ("wrong edition", good(pro)),
            ("not executable", { let mut g = good(EXE); g.retain(|e| !matches!(e, E::File(n, _, _) if *n == EXE)); g.push(E::File(EXE, 0o644, b"b")); g }),
            ("no info plist", { let mut g = good(EXE); g.retain(|e| !matches!(e, E::File(n, _, _) if n.ends_with("Info.plist"))); g }),
            ("case duplicate", { let mut g = good(EXE); g.push(E::File("DuDuClaw Gateway.app/contents/info.plist", 0o644, b"x")); g }),
            ("file as dir", { let mut g = good(EXE); g.push(E::File("DuDuClaw Gateway.app/Contents/Info.plist/x", 0o644, b"x")); g }),
            ("root file", vec![E::File("DuDuClaw Gateway.app", 0o644, b"x")]),
        ];
        for (what, entries) in cases {
            let dir = tempfile::tempdir().unwrap();
            let error = extract_bundle_zip(&zip_of(&entries), dir.path(), "duduclaw");
            assert!(error.is_err(), "{what} must be refused");
            assert!(!dir.path().join(ROOT).exists(), "{what}: nothing may be written");
        }
        // Pro edition: its own executable name is required.
        let dir = tempfile::tempdir().unwrap();
        assert!(extract_bundle_zip(&zip_of(&good(pro)), dir.path(), "duduclaw-pro").is_ok());
        assert!(extract_bundle_zip(b"not a zip", tempfile::tempdir().unwrap().path(), "duduclaw").is_err());
    }

    #[test]
    fn entry_names_and_modes_are_classified_strictly() {
        assert_eq!(parse_entry_name(b"DuDuClaw Gateway.app/").unwrap(), (vec![], true));
        assert_eq!(parse_entry_name(b"DuDuClaw Gateway.app/Contents/Info.plist").unwrap().0,
            vec!["Contents".to_string(), "Info.plist".to_string()]);
        for bad in [&b"DuDuClaw Gateway.app//x"[..], b"DuDuClaw Gateway.app/a:b", b"\xff", b"",
            b"DuDuClaw Gateway.app/x\0", b"__MACOSX/DuDuClaw Gateway.app/._Info.plist",
            b"/DuDuClaw Gateway.app/evil", b"DuDuClaw Gateway.app/./evil", b"DuDuClaw Gateway.app\\evil",
            b"DuDuClaw Gateway.app/../evil", b"DuDuClaw Gateway.app/Contents/..", b"Other.app/Contents/Info.plist"] {
            assert!(parse_entry_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(entry_kind(Some(0o100755), false, false).unwrap(), EntryKind::File { executable: true });
        assert_eq!(entry_kind(Some(0o040755), true, false).unwrap(), EntryKind::Dir);
        assert_eq!(entry_kind(None, false, false).unwrap(), EntryKind::File { executable: false });
        assert!(entry_kind(Some(0o120777), false, false).is_err());
        assert!(entry_kind(Some(0o100644), false, true).is_err());
        assert!(entry_kind(Some(0o102755), false, false).is_err());
        assert!(entry_kind(Some(0o060644), false, false).is_err());
        assert!(entry_kind(Some(0o040755), false, false).is_err(), "dir mode on a file name");
    }

    #[test]
    fn staging_dir_is_private_and_unique() {
        let parent = tempfile::tempdir().unwrap();
        let a = create_staging_dir(parent.path()).unwrap();
        let b = create_staging_dir(parent.path()).unwrap();
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_str().unwrap().starts_with(STAGING_PREFIX));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }

    // ── codesign gate ────────────────────────────────────────────────────

    struct FakeSign { pinned_ok: bool, current: Result<SignatureInfo, String>, old_dr_ok: bool }
    impl CodeSign for FakeSign {
        fn verify(&self, _: &Path, requirement: &str) -> Result<(), String> {
            let ok = if requirement == PINNED_DR { self.pinned_ok } else { self.old_dr_ok };
            if ok { Ok(()) } else { Err(format!("fails {requirement}")) }
        }
        fn signature_info(&self, _: &Path) -> Result<SignatureInfo, String> { self.current.clone() }
    }
    struct Plists(HashMap<(PathBuf, &'static str), String>);
    impl InfoPlistReader for Plists {
        fn read_key(&self, bundle: &Path, key: &str) -> Result<String, String> {
            self.0.iter().find(|((b, k), _)| b == bundle && *k == key).map(|(_, v)| v.clone()).ok_or_else(|| "missing".into())
        }
    }

    fn staged_with_marker(version: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join(ROOT);
        std::fs::create_dir_all(staged.join("Contents/MacOS")).unwrap();
        let marker = [b"\0DUDUCLAW_RELEASE_ID:V1:".as_slice(), version.as_bytes(), b"\0DUDUCLAW_RELEASE_END\0"].concat();
        std::fs::write(staged.join("Contents/MacOS/duduclaw"), marker).unwrap();
        (dir, staged)
    }

    fn plists(staged: &Path, current: &Path, id: &str, version: &str, exe: &str, current_exe: &str) -> Plists {
        Plists(HashMap::from([
            ((staged.to_path_buf(), "CFBundleIdentifier"), id.to_string()),
            ((staged.to_path_buf(), "CFBundleShortVersionString"), version.to_string()),
            ((staged.to_path_buf(), "CFBundleExecutable"), exe.to_string()),
            ((current.to_path_buf(), "CFBundleExecutable"), current_exe.to_string()),
        ]))
    }

    #[test]
    fn codesign_gate_requires_pinned_and_installed_requirements() {
        let (_d, staged) = staged_with_marker("1.81.0");
        let current = PathBuf::from("/Users/k/Applications/DuDuClaw Gateway.app");
        let ok_plists = plists(&staged, &current, BUNDLE_ID, "1.81.0", "duduclaw", "duduclaw");
        let developer = Ok(SignatureInfo { adhoc: false, designated: Some("identifier \"ai.duduclaw.gateway\" and anchor apple generic".into()) });
        let gate = |cs: &FakeSign, p: &Plists, cur: Option<&Path>| verify_staged_bundle(cs, p, &staged, cur, "duduclaw", "1.81.0");

        assert!(gate(&FakeSign { pinned_ok: true, current: developer.clone(), old_dr_ok: true }, &ok_plists, Some(&current)).is_ok());
        // Pinned DR failure is fatal, always.
        assert!(gate(&FakeSign { pinned_ok: false, current: developer.clone(), old_dr_ok: true }, &ok_plists, None).is_err());
        // Installed bundle's DR must also hold.
        assert!(gate(&FakeSign { pinned_ok: true, current: developer.clone(), old_dr_ok: false }, &ok_plists, Some(&current)).is_err());
        // Unreadable installed signature fails closed; missing DR too.
        assert!(gate(&FakeSign { pinned_ok: true, current: Err("not signed".into()), old_dr_ok: true }, &ok_plists, Some(&current)).is_err());
        assert!(gate(&FakeSign { pinned_ok: true, current: Ok(SignatureInfo { adhoc: false, designated: None }), old_dr_ok: true }, &ok_plists, Some(&current)).is_err());
        // Ad-hoc installed bundle: its DR check is skipped (warned), nothing else is.
        let adhoc = Ok(SignatureInfo { adhoc: true, designated: Some("cdhash H\"00\"".into()) });
        assert!(gate(&FakeSign { pinned_ok: true, current: adhoc.clone(), old_dr_ok: false }, &ok_plists, Some(&current)).is_ok());
        assert!(gate(&FakeSign { pinned_ok: false, current: adhoc, old_dr_ok: true }, &ok_plists, Some(&current)).is_err());
        // Migration (no current bundle) never consults the old DR.
        assert!(gate(&FakeSign { pinned_ok: true, current: Err("unused".into()), old_dr_ok: false }, &ok_plists, None).is_ok());

        let cs = FakeSign { pinned_ok: true, current: developer, old_dr_ok: true };
        for bad in [
            plists(&staged, &current, "ai.duduclaw.desktop", "1.81.0", "duduclaw", "duduclaw"),
            plists(&staged, &current, BUNDLE_ID, "1.80.0", "duduclaw", "duduclaw"),
            plists(&staged, &current, BUNDLE_ID, "1.81.0", "duduclaw-pro", "duduclaw"),
            plists(&staged, &current, BUNDLE_ID, "1.81.0", "duduclaw", "duduclaw-pro"),
        ] {
            assert!(gate(&cs, &bad, Some(&current)).is_err());
        }
        // Release marker inside the executable must match too.
        let (_d2, old) = staged_with_marker("1.80.0");
        let p = plists(&old, &current, BUNDLE_ID, "1.81.0", "duduclaw", "duduclaw");
        assert!(verify_staged_bundle(&cs, &p, &old, Some(&current), "duduclaw", "1.81.0").unwrap_err().contains("version mismatch"));
    }

    #[test]
    fn codesign_output_parsing() {
        let text = "Executable=/A/DuDuClaw Gateway.app/Contents/MacOS/duduclaw\nIdentifier=ai.duduclaw.gateway\n\
CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+7 location=embedded\nAuthority=Developer ID Application\n\
# designated => identifier \"x\"\ndesignated => identifier \"ai.duduclaw.gateway\" and anchor apple generic\n";
        assert_eq!(parse_designated(text).unwrap(), "identifier \"ai.duduclaw.gateway\" and anchor apple generic");
        assert!(!parse_adhoc(text));
        assert!(parse_adhoc("CodeDirectory v=20400 size=1 flags=0x20002(adhoc,linker-signed) hashes=1+0\n"));
        assert!(parse_adhoc("Signature=adhoc\n"));
        assert_eq!(parse_designated("no requirement"), None);
    }

    // ── swap / rollback / cleanup ────────────────────────────────────────

    /// Simulates RENAME_SWAP with three renames (tests only).
    struct EmulatedSwap;
    impl SwapOps for EmulatedSwap {
        fn swap(&self, a: &Path, b: &Path) -> io::Result<()> {
            let tmp = a.with_extension("swap-tmp");
            std::fs::rename(a, &tmp)?;
            std::fs::rename(b, a)?;
            std::fs::rename(&tmp, b)
        }
        fn rename_excl(&self, from: &Path, to: &Path) -> io::Result<()> {
            if to.exists() { return Err(io::Error::from(io::ErrorKind::AlreadyExists)); }
            std::fs::rename(from, to)
        }
    }
    struct NoRenamex;
    impl SwapOps for NoRenamex {
        fn swap(&self, _: &Path, _: &Path) -> io::Result<()> { Err(io::Error::from(io::ErrorKind::Unsupported)) }
        fn rename_excl(&self, _: &Path, _: &Path) -> io::Result<()> { Err(io::Error::from(io::ErrorKind::Unsupported)) }
    }

    fn bundle(dir: &Path, tag: &str) {
        std::fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
        std::fs::write(dir.join("Contents/MacOS/duduclaw"), tag).unwrap();
    }
    fn tag(dir: &Path) -> String { std::fs::read_to_string(dir.join("Contents/MacOS/duduclaw")).unwrap() }

    #[test]
    fn swap_installs_and_restores_with_and_without_renamex() {
        let ops: [&dyn SwapOps; 3] = [&SystemSwap, &EmulatedSwap, &NoRenamex];
        for ops in ops {
            let parent = tempfile::tempdir().unwrap();
            let target = parent.path().join(ROOT);
            bundle(&target, "old");
            let staging = create_staging_dir(parent.path()).unwrap();
            let staged = staging.join(ROOT);
            bundle(&staged, "new");
            let installed = install_staged_bundle(ops, &staged, &staging, &target).unwrap();
            assert_eq!(tag(&target), "new");
            restore_previous(ops, &installed, &staging, &target).unwrap();
            assert_eq!(tag(&target), "old");
        }
    }

    #[test]
    fn first_install_never_replaces_and_rolls_back_cleanly() {
        let ops: [&dyn SwapOps; 3] = [&SystemSwap, &EmulatedSwap, &NoRenamex];
        for ops in ops {
            let parent = tempfile::tempdir().unwrap();
            let target = parent.path().join(ROOT);
            let staging = create_staging_dir(parent.path()).unwrap();
            let staged = staging.join(ROOT);
            bundle(&staged, "new");
            let installed = install_staged_bundle(ops, &staged, &staging, &target).unwrap();
            assert_eq!(installed, Installed::Fresh);
            assert_eq!(tag(&target), "new");
            restore_previous(ops, &installed, &staging, &target).unwrap();
            assert!(!target.exists());
        }
        // A file squatting on the target is never replaced.
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join(ROOT);
        std::fs::write(&target, b"squatter").unwrap();
        let staging = create_staging_dir(parent.path()).unwrap();
        bundle(&staging.join(ROOT), "new");
        assert!(install_staged_bundle(&EmulatedSwap, &staging.join(ROOT), &staging, &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"squatter");
    }

    #[test]
    fn fallback_failure_restores_the_installed_bundle() {
        struct FailSecondRename(std::cell::Cell<u32>);
        impl SwapOps for FailSecondRename {
            fn swap(&self, _: &Path, _: &Path) -> io::Result<()> { Err(io::Error::from(io::ErrorKind::Unsupported)) }
            fn rename_excl(&self, _: &Path, _: &Path) -> io::Result<()> { Err(io::Error::from(io::ErrorKind::Unsupported)) }
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                self.0.set(self.0.get() + 1);
                if self.0.get() == 2 { return Err(io::Error::other("injected")); }
                std::fs::rename(from, to)
            }
        }
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join(ROOT);
        bundle(&target, "old");
        let staging = create_staging_dir(parent.path()).unwrap();
        bundle(&staging.join(ROOT), "new");
        let error = install_staged_bundle(&FailSecondRename(Default::default()), &staging.join(ROOT), &staging, &target).unwrap_err();
        assert!(error.contains("rolled back"), "{error}");
        assert_eq!(tag(&target), "old");
    }

    #[test]
    fn boot_cleanup_removes_only_stale_private_staging_dirs() {
        let parent = tempfile::tempdir().unwrap();
        let stale = create_staging_dir(parent.path()).unwrap();
        bundle(&stale.join(ROOT), "x");
        let running = create_staging_dir(parent.path()).unwrap();
        bundle(&running.join(ROOT), "y");
        let running_exe = running.join(ROOT).join("Contents/MacOS/duduclaw");
        // Bare-binary staging file with the same prefix: not ours to remove.
        let file = parent.path().join(format!("{STAGING_PREFIX}abc"));
        std::fs::write(&file, b"tmp").unwrap();
        let other = parent.path().join("keep-me");
        std::fs::create_dir(&other).unwrap();
        // Too young: nothing removed.
        assert_eq!(cleanup_stale_staging(parent.path(), &running_exe, std::time::Duration::from_secs(3600)), 0);
        assert_eq!(cleanup_stale_staging(parent.path(), &running_exe, std::time::Duration::ZERO), 1);
        assert!(!stale.exists());
        assert!(running.exists() && file.exists() && other.exists());
    }

    #[cfg(unix)]
    #[test]
    fn bare_binary_becomes_a_symlink_to_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("duduclaw");
        std::fs::write(&bare, b"bare").unwrap();
        let exe = dir.path().join(ROOT).join("Contents/MacOS/duduclaw");
        bundle(&dir.path().join(ROOT), "bundle");
        replace_with_symlink(&bare, &exe).unwrap();
        assert_eq!(std::fs::read_link(&bare).unwrap(), exe);
        assert_eq!(std::fs::read(&bare).unwrap(), b"bundle");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
