//! `SysupdateStageApply`: install an OS update only after root has verified
//! it itself.
//!
//! Why: `systemd-sysupdate`'s `Type=regular-file` source does no
//! authentication, so whatever sits in its source directory is installed.
//! The gateway downloads releases into `/data/duduclaw/updates` and checks
//! the signature, but on DuDuClaw OS the gateway's user is also the AI
//! employees' user: anything there can be planted, and the gateway's own
//! check can be forged. So this verb re-does the trust decision as root:
//!
//! 1. read `SHA256SUMS` and `SHA256SUMS.minisig` from the untrusted
//!    [`SOURCE_DIR`] (no links followed, regular files, size caps) and
//!    verify the signature against the OS image key compiled into this
//!    binary ([`crate::release::OS_IMAGE_UPDATE_PUBKEY`], the one key both
//!    sides share);
//! 2. parse the manifest with the gateway's own rules
//!    ([`crate::release::parse_manifest`], [`crate::release::select_release_files`]);
//! 3. empty the root-only [`DEST_DIR`] and copy into it exactly the files
//!    the install needs: the root payload, the one UKI variant whose baked
//!    `root=PARTUUID=` is the destination slot (published under the
//!    canonical `duduclaw-os_<version>.efi` name sysupdate matches), and the
//!    verity hash tree if the release has one. Each is opened without
//!    following links, must be a regular file under a size cap, and is
//!    hashed while it is copied; a mismatch with the signed manifest aborts;
//! 4. read the staged UKI's embedded `IMAGE_VERSION` and require it to equal
//!    the release version, to be at least `--min-rollback-version` (when
//!    set) and to be strictly newer than the running image — an old signed
//!    release cannot be installed under a newer name to get back a
//!    root-running gateway;
//! 5. run `systemd-sysupdate update`, whose transfers (OS side) read from
//!    [`DEST_DIR`].
//!
//! [`DEST_DIR`] is emptied before staging and again afterwards, success or
//! failure, so it never holds anything between calls.
//!
//! A single-UKI (pre per-slot) release whose UKI is not already bound to
//! the destination slot is refused here: the legacy on-device rewrite would
//! change the bytes the manifest signs.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::release::{ManifestEntry, ReleaseFiles};
use crate::rollback_floor::{OsVersion, RollbackFloor};

/// Where the gateway downloads releases (its own home; untrusted).
pub const SOURCE_DIR: &str = crate::release::UPDATE_SOURCE_DIR;
/// Root-only staging directory the OS's sysupdate transfers read.
pub const DEST_DIR: &str = "/data/system/updates";

pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub const MAX_SIGNATURE_BYTES: u64 = 4 * 1024;
pub const MAX_UKI_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_ROOT_BYTES: u64 = 9 * 1024 * 1024 * 1024;
pub const MAX_VERITY_BYTES: u64 = 1024 * 1024 * 1024;
const CHUNK: usize = 4 * 1024 * 1024;

/// Open `path` for reading without following a link, and require a regular
/// file of at most `max` bytes. Returns the file and its length.
pub fn open_regular(path: &Path, max: u64) -> Result<(std::fs::File, u64), String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let len = check_regular(&file, path, max)?;
    Ok((file, len))
}

/// The open file is a regular file of at most `max` bytes; its length.
fn check_regular(file: &std::fs::File, label: &Path, max: u64) -> Result<u64, String> {
    let meta = file.metadata().map_err(|e| format!("cannot stat {}: {e}", label.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", label.display()));
    }
    if meta.len() > max {
        return Err(format!("{} is larger than allowed ({} > {max} bytes)", label.display(), meta.len()));
    }
    Ok(meta.len())
}

fn read_bounded(file: std::fs::File, label: &Path, max: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    file.take(max + 1).read_to_end(&mut out).map_err(|e| format!("cannot read {}: {e}", label.display()))?;
    if out.len() as u64 > max {
        return Err(format!("{} grew past its size limit while being read", label.display()));
    }
    Ok(out)
}

/// Read a small file whole (by path; the source directory uses
/// [`SourceDir::read_small`]).
pub fn read_small(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let (file, _) = open_regular(path, max)?;
    read_bounded(file, path, max)
}

/// Verify a minisign signature over `data` with `pubkey_b64`.
pub fn verify_signature(data: &[u8], sig_text: &str, pubkey_b64: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64).map_err(|e| format!("invalid signing key: {e}"))?;
    let sig = minisign_verify::Signature::decode(sig_text).map_err(|e| format!("signature is malformed: {e}"))?;
    pk.verify(data, &sig, false).map_err(|e| format!("signature verification failed: {e}"))
}

/// Copy `src` to a new `dst` (mode 0600, created exclusively, no links
/// followed), hashing while copying; all-zero chunks are skipped as holes.
/// The copy is removed when the digest is not `sha256_hex` or anything
/// fails.
pub fn copy_verified(src: &Path, dst: &Path, sha256_hex: &str, max: u64) -> Result<u64, String> {
    let (input, len) = open_regular(src, max)?;
    copy_file_verified(input, len, src, dst, sha256_hex, max)
}

/// [`copy_verified`] from an already opened (and checked) file.
fn copy_file_verified(
    mut input: std::fs::File,
    len: u64,
    src: &Path,
    dst: &Path,
    sha256_hex: &str,
    max: u64,
) -> Result<u64, String> {
    let result = (|| {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dst)
            .map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut copied: u64 = 0;
        loop {
            let n = input.read(&mut buf).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
            if n == 0 {
                break;
            }
            copied += n as u64;
            if copied > max {
                return Err(format!("{} grew past its size limit while being copied", src.display()));
            }
            let chunk = &buf[..n];
            hasher.update(chunk);
            if chunk.iter().all(|b| *b == 0) {
                use std::io::Seek;
                out.seek(std::io::SeekFrom::Current(n as i64)).map_err(|e| e.to_string())?;
            } else {
                out.write_all(chunk).map_err(|e| format!("cannot write {}: {e}", dst.display()))?;
            }
        }
        out.set_len(copied).map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        if copied != len {
            return Err(format!("{} changed size while being copied", src.display()));
        }
        let got = format!("{:x}", hasher.finalize());
        if !got.eq_ignore_ascii_case(sha256_hex) {
            return Err(format!("{} does not match the signed manifest", src.display()));
        }
        Ok(copied)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(dst);
    }
    result
}

/// `root=PARTUUID=<uuid>` from a UKI's `.cmdline` section (bounded read).
pub fn uki_root_partuuid(path: &Path) -> Result<String, String> {
    use std::io::{Seek, SeekFrom};
    let (mut file, _) = open_regular(path, MAX_UKI_BYTES)?;
    let mut header = Vec::new();
    (&mut file).take(64 * 1024).read_to_end(&mut header).map_err(|e| e.to_string())?;
    let (offset, len) = crate::rollback_floor::pe_section(&header, ".cmdline")
        .ok_or_else(|| format!("{} has no .cmdline section", path.display()))?;
    if len == 0 || len > 16 * 1024 {
        return Err(format!("{}: .cmdline has an unusable size", path.display()));
    }
    file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut cmdline = Vec::new();
    file.take(len).read_to_end(&mut cmdline).map_err(|e| e.to_string())?;
    cmdline_root_partuuid(&String::from_utf8_lossy(&cmdline))
        .ok_or_else(|| format!("{}: no root=PARTUUID= on the kernel command line", path.display()))
}

/// The value of the whole token `root=PARTUUID=<uuid>` on a command line.
pub fn cmdline_root_partuuid(cmdline: &str) -> Option<String> {
    cmdline
        .split(|c: char| c.is_whitespace() || c == '\0')
        .find_map(|t| t.strip_prefix("root=PARTUUID="))
        .filter(|v| crate::release::is_uuid_text(v))
        .map(str::to_ascii_lowercase)
}

/// Which UKI to stage, from each variant's baked PARTUUID (`None` = could
/// not be read) and the destination slot's PARTUUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UkiChoice {
    Canonical,
    SlotB,
}

pub fn choose_uki(canonical: Option<&str>, slot_b: Option<Option<&str>>, dest: &str) -> Result<UkiChoice, String> {
    let matches = |v: Option<&str>| v.is_some_and(|v| v.eq_ignore_ascii_case(dest));
    match slot_b {
        None if matches(canonical) => Ok(UkiChoice::Canonical),
        None => Err("this release's only kernel image is not bound to the destination slot".into()),
        Some(b) => match (matches(canonical), matches(b)) {
            (true, false) => Ok(UkiChoice::Canonical),
            (false, true) => Ok(UkiChoice::SlotB),
            (true, true) => Err("both kernel images claim the destination slot".into()),
            (false, false) => Err("neither kernel image is bound to the destination slot".into()),
        },
    }
}

/// The version gate (pure): the staged image's embedded version must equal
/// the release version, be at least the floor, and be newer than what runs.
pub fn check_versions(
    embedded: Option<&str>,
    release: &str,
    floor: &RollbackFloor,
    running: Option<&str>,
) -> Result<(), String> {
    let embedded = embedded.ok_or("the staged kernel image carries no IMAGE_VERSION")?;
    let staged = OsVersion::parse(embedded).ok_or_else(|| format!("version {embedded:?} does not parse"))?;
    let named = OsVersion::parse(release).ok_or_else(|| format!("release version {release:?} does not parse"))?;
    if staged != named {
        return Err(format!("the kernel image says {embedded} but the release is {release}"));
    }
    match floor {
        RollbackFloor::Off => {}
        RollbackFloor::Invalid(raw) => return Err(format!("--min-rollback-version {raw:?} is not a version")),
        RollbackFloor::Min(min) if &staged < min => {
            return Err(format!("{embedded} is older than the minimum allowed version"));
        }
        RollbackFloor::Min(_) => {}
    }
    let running = running.ok_or("the running image version cannot be determined")?;
    let current = OsVersion::parse(running).ok_or_else(|| format!("running version {running:?} does not parse"))?;
    if staged <= current {
        return Err(format!("{embedded} is not newer than the running {running}"));
    }
    Ok(())
}

/// Remove every entry of `dir` and recreate it 0700 (missing is fine). A
/// symlink at `dir` is removed, never followed.
pub fn reset_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            std::fs::remove_file(dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
        }
        Ok(_) => std::fs::remove_dir_all(dir).map_err(|e| format!("cannot clear {}: {e}", dir.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect {}: {e}", dir.display())),
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    Ok(())
}

/// The untrusted source directory, opened once with `O_DIRECTORY |
/// O_NOFOLLOW` (a symlink swapped in for the directory, or any component
/// under it, is refused); every file is then opened relative to that
/// descriptor with `openat(..., O_NOFOLLOW)`, and only plain basenames are
/// accepted.
pub struct SourceDir {
    fd: std::os::fd::OwnedFd,
    path: std::path::PathBuf,
}

impl SourceDir {
    pub fn open(path: &Path) -> Result<Self, String> {
        use std::os::fd::FromRawFd;
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| "source path contains NUL".to_string())?;
        // SAFETY: `c` is a valid NUL-terminated path for the call's duration.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("cannot open {} as a real directory: {}", path.display(), std::io::Error::last_os_error()));
        }
        // SAFETY: a fresh descriptor this function owns.
        Ok(Self { fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }, path: path.to_path_buf() })
    }

    /// Open `name` (a plain basename) in this directory: no link followed,
    /// regular file, at most `max` bytes.
    pub fn open_file(&self, name: &str, max: u64) -> Result<(std::fs::File, u64), String> {
        use std::os::fd::{AsRawFd, FromRawFd};
        if !crate::release::is_safe_basename(name) {
            return Err(format!("{name:?} is not a plain file name"));
        }
        let c = std::ffi::CString::new(name).map_err(|_| "file name contains NUL".to_string())?;
        // SAFETY: valid directory descriptor and NUL-terminated name.
        let fd = unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        let label = self.path.join(name);
        if fd < 0 {
            return Err(format!("cannot open {}: {}", label.display(), std::io::Error::last_os_error()));
        }
        // SAFETY: a fresh descriptor this function owns.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let len = check_regular(&file, &label, max)?;
        Ok((file, len))
    }

    pub fn read_small(&self, name: &str, max: u64) -> Result<Vec<u8>, String> {
        let (file, _) = self.open_file(name, max)?;
        read_bounded(file, &self.path.join(name), max)
    }
}

/// Where a staging run builds its files: a fresh private sibling of `dest`
/// (same filesystem, so the final rename is atomic).
fn private_staging_dir(dest: &Path) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let parent = dest.parent().ok_or("the staging directory has no parent")?;
    let base = dest.file_name().and_then(|n| n.to_str()).ok_or("the staging directory has no name")?;
    let dir = parent.join(format!(".{base}-staging-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    std::fs::create_dir(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Remove what an interrupted run (power cut, sysd killed mid-copy) left
/// next to `dest`: directories directly in `dest`'s parent named exactly
/// `.<dest name>-staging-<32 hex>` or `.<dest name>-old-<32 hex>`, that are
/// real directories (never a symlink, never followed) owned by `owner_uid`
/// (root in production: sysd's own euid). Anything else is left alone.
/// Returns how many were removed. Called at the start of each staging run,
/// under the update lock.
pub fn remove_staging_leftovers(dest: &Path, owner_uid: u32) -> usize {
    use std::os::unix::fs::MetadataExt;
    let (Some(parent), Some(base)) = (dest.parent(), dest.file_name().and_then(|n| n.to_str())) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(parent) else { return 0 };
    let shape = |name: &str| {
        [format!(".{base}-staging-"), format!(".{base}-old-")].iter().any(|prefix| {
            name.strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
        })
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !shape(name) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != owner_uid {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!("could not remove leftover {}: {e}", path.display()),
        }
    }
    removed
}

/// Put a fully verified staging directory in place as `dest`: the old
/// `dest` (or a link planted there) is moved aside, the staging directory is
/// renamed in, the old one removed. `dest` never holds a partial or
/// unverified set.
fn place(staging: &Path, dest: &Path) -> Result<(), String> {
    let parent = dest.parent().ok_or("the staging directory has no parent")?;
    let base = dest.file_name().and_then(|n| n.to_str()).ok_or("the staging directory has no name")?;
    let old = parent.join(format!(".{base}-old-{}", uuid::Uuid::new_v4().simple()));
    let had_old = match std::fs::symlink_metadata(dest) {
        Ok(_) => {
            std::fs::rename(dest, &old).map_err(|e| format!("cannot move {} aside: {e}", dest.display()))?;
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(format!("cannot inspect {}: {e}", dest.display())),
    };
    std::fs::rename(staging, dest).map_err(|e| format!("cannot put the staged update in place: {e}"))?;
    if had_old {
        let removed = match std::fs::symlink_metadata(&old) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => std::fs::remove_dir_all(&old),
            _ => std::fs::remove_file(&old),
        };
        if let Err(e) = removed {
            tracing::warn!("could not remove the previous staging directory {}: {e}", old.display());
        }
    }
    Ok(())
}

/// Everything before `systemd-sysupdate update`. All work happens in a
/// private staging directory next to `dest`; every check (signature,
/// hashes, slot-bound UKI, embedded version equal to the release, newer
/// than running, at least the floor) passes there first, and only then is
/// the directory renamed into place as `dest`, so sysupdate's source never
/// holds an unverified or too-old image even briefly. On any failure the
/// staging directory is removed and `dest` is emptied.
///
/// `pubkey` is a parameter only so tests can sign with a throwaway key; the
/// one production caller (`dispatch`) passes the compiled-in
/// [`crate::release::OS_IMAGE_UPDATE_PUBKEY`]. There is no runtime override.
/// Callers serialise update operations (`dispatch`'s update lock).
pub fn stage(
    source: &Path,
    dest: &Path,
    pubkey: &str,
    arch: &str,
    dest_partuuid: &str,
    floor: &RollbackFloor,
    running: Option<&str>,
) -> Result<ReleaseFiles, String> {
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    let cleared = remove_staging_leftovers(dest, me);
    if cleared > 0 {
        tracing::info!("removed {cleared} leftover staging director(y/ies) from an interrupted update");
    }
    let result = private_staging_dir(dest).and_then(|staging| {
        let staged = stage_inner(source, &staging, pubkey, arch, dest_partuuid, floor, running)
            .and_then(|release| place(&staging, dest).map(|()| release));
        if staged.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        staged
    });
    if result.is_err() {
        if let Err(e) = reset_dir(dest) {
            tracing::warn!("could not empty {}: {e}", dest.display());
        }
    }
    result
}

fn stage_inner(
    source: &Path,
    staging: &Path,
    pubkey: &str,
    arch: &str,
    dest_partuuid: &str,
    floor: &RollbackFloor,
    running: Option<&str>,
) -> Result<ReleaseFiles, String> {
    let src = SourceDir::open(source)?;
    let manifest = src.read_small(crate::release::MANIFEST_NAME, MAX_MANIFEST_BYTES)?;
    let sig = src.read_small(crate::release::SIGNATURE_NAME, MAX_SIGNATURE_BYTES)?;
    let sig = String::from_utf8(sig).map_err(|_| "the signature file is not text".to_string())?;
    verify_signature(&manifest, &sig, pubkey)?;
    let text = String::from_utf8(manifest).map_err(|_| "the manifest is not text".to_string())?;
    let entries = crate::release::parse_manifest(&text)?;
    let release = crate::release::select_release_files(&entries, arch)?;

    let copy = |entry: &ManifestEntry, as_name: &str, max: u64| {
        let (file, len) = src.open_file(&entry.name, max)?;
        copy_file_verified(file, len, &src.path.join(&entry.name), &staging.join(as_name), &entry.sha256, max)
    };
    // Kernel images first (small), into temporary names, to choose one.
    let canonical_tmp = format!(".candidate-{}", release.uki.name);
    copy(&release.uki, &canonical_tmp, MAX_UKI_BYTES)?;
    let slot_b_tmp = release.uki_slot_b.as_ref().map(|e| format!(".candidate-{}", e.name));
    if let (Some(entry), Some(tmp)) = (&release.uki_slot_b, &slot_b_tmp) {
        copy(entry, tmp, MAX_UKI_BYTES)?;
    }
    let canonical_uuid = uki_root_partuuid(&staging.join(&canonical_tmp)).ok();
    let slot_b_uuid = slot_b_tmp.as_ref().map(|tmp| uki_root_partuuid(&staging.join(tmp)).ok());
    let choice = choose_uki(canonical_uuid.as_deref(), slot_b_uuid.as_ref().map(|o| o.as_deref()), dest_partuuid)?;
    let chosen_tmp = match choice {
        UkiChoice::Canonical => canonical_tmp.clone(),
        UkiChoice::SlotB => slot_b_tmp.clone().ok_or("no slot-B kernel image")?,
    };
    // Version check on the chosen file before it gets its sysupdate name.
    let embedded = crate::rollback_floor::uki_image_version(&staging.join(&chosen_tmp)).ok();
    check_versions(embedded.as_deref(), &release.version, floor, running)?;
    std::fs::rename(staging.join(&chosen_tmp), staging.join(&release.uki.name)).map_err(|e| e.to_string())?;
    for tmp in [Some(canonical_tmp), slot_b_tmp].into_iter().flatten() {
        let _ = std::fs::remove_file(staging.join(tmp));
    }

    copy(&release.root, &release.root.name, MAX_ROOT_BYTES)?;
    if let Some(verity) = &release.verity {
        copy(verity, &verity.name, MAX_VERITY_BYTES)?;
    }
    Ok(release)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gateway's app-update key and a real signature over b"FAKE_BINARY"
    /// made with it (same fixture as `duduclaw-gateway/src/updater.rs`).
    const TEST_PUBKEY: &str = "RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh";
    const TEST_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RUTh5pOpk0YmdButYZG9tlcekzhpZGpz83+E62dF1rtB5NtAoxXBenf0jZrluKejtL1R5cHtL60+75EzvH/EorwGHcc2aZlwkwQ=\n\
trusted comment: duduclaw test\n\
mRGk2RUiXNVr9zzLXu6BI9+0URr0xlBwS3rMMXD3smkK7rcMrajd/tMz7jhWxQkiPzmWe4pdxFiIG1Vx2JdFAQ==\n";

    #[test]
    fn signatures_verify_only_for_the_signed_bytes_and_key() {
        assert!(verify_signature(b"FAKE_BINARY", TEST_SIG, TEST_PUBKEY).is_ok());
        assert!(verify_signature(b"FAKE_BINARY!", TEST_SIG, TEST_PUBKEY).is_err(), "other bytes");
        assert!(verify_signature(b"FAKE_BINARY", TEST_SIG, crate::release::OS_IMAGE_UPDATE_PUBKEY).is_err(), "other key");
        assert!(verify_signature(b"FAKE_BINARY", "garbage", TEST_PUBKEY).is_err(), "malformed");
        assert!(verify_signature(b"FAKE_BINARY", "", TEST_PUBKEY).is_err(), "missing");
    }

    #[test]
    fn a_missing_or_unsigned_release_is_refused_before_anything_is_copied() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let dest = dst.path().join("updates");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("leftover"), "x").unwrap();
        let err = stage(src.path(), &dest, TEST_PUBKEY, "x86-64", "x", &RollbackFloor::Off, Some("0.1.0")).unwrap_err();
        assert!(err.contains("SHA256SUMS"), "{err}");
        std::fs::write(src.path().join("SHA256SUMS"), "FAKE_BINARY").unwrap();
        std::fs::write(src.path().join("SHA256SUMS.minisig"), "bad").unwrap();
        assert!(stage(src.path(), &dest, TEST_PUBKEY, "x86-64", "x", &RollbackFloor::Off, Some("0.1.0")).is_err());
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0, "a refused release leaves the destination empty");
        // Correctly signed, but not a manifest: refused at parsing.
        std::fs::write(src.path().join("SHA256SUMS"), "FAKE_BINARY").unwrap();
        std::fs::write(src.path().join("SHA256SUMS.minisig"), TEST_SIG).unwrap();
        let err = stage(src.path(), &dest, TEST_PUBKEY, "x86-64", "x", &RollbackFloor::Off, Some("0.1.0")).unwrap_err();
        assert!(err.contains("SHA-256") || err.contains("manifest"), "{err}");
    }

    #[test]
    fn manifest_names_follow_the_shared_rules() {
        let sha = "a".repeat(64);
        assert!(crate::release::parse_manifest(&format!("{sha}  duduclaw-os_0.4.0.efi\n")).is_ok());
        for bad in ["../etc/passwd", "/abs", ".hidden", "a/b", "a b", "a\\b"] {
            assert!(crate::release::parse_manifest(&format!("{sha}  {bad}\n")).is_err(), "{bad}");
        }
    }

    #[test]
    fn copies_refuse_links_and_non_regular_files_and_hash_mismatches() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("payload");
        std::fs::write(&good, b"hello\0\0\0world").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"hello\0\0\0world"));
        let out = dir.path().join("out");
        assert_eq!(copy_verified(&good, &out, &sha, 1024).unwrap(), 13);
        assert_eq!(std::fs::read(&out).unwrap(), b"hello\0\0\0world");

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        assert!(copy_verified(&link, &dir.path().join("o2"), &sha, 1024).is_err(), "symlink source");
        assert!(copy_verified(dir.path(), &dir.path().join("o3"), &sha, 1024).is_err(), "directory source");
        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(copy_verified(&fifo, &dir.path().join("o4"), &sha, 1024).is_err(), "fifo source");
        assert!(copy_verified(&good, &dir.path().join("o5"), &"0".repeat(64), 1024).is_err(), "hash mismatch");
        assert!(!dir.path().join("o5").exists(), "a mismatched copy is removed");
        assert!(copy_verified(&good, &dir.path().join("o6"), &sha, 4).is_err(), "size cap");
        // An existing destination (or a link planted there) is never written through.
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join("o7")).unwrap();
        assert!(copy_verified(&good, &dir.path().join("o7"), &sha, 1024).is_err());
        assert!(!dir.path().join("elsewhere").exists());
    }

    #[test]
    fn uki_choice_follows_the_destination_slot() {
        let a = "11111111-1111-1111-1111-111111111111";
        let b = "22222222-2222-2222-2222-222222222222";
        assert_eq!(choose_uki(Some(a), Some(Some(b)), a), Ok(UkiChoice::Canonical));
        assert_eq!(choose_uki(Some(a), Some(Some(b)), b), Ok(UkiChoice::SlotB));
        assert!(choose_uki(Some(a), Some(Some(a)), a).is_err(), "ambiguous");
        assert!(choose_uki(Some(a), Some(Some(b)), "33333333-3333-3333-3333-333333333333").is_err());
        assert_eq!(choose_uki(Some(a), None, a), Ok(UkiChoice::Canonical));
        assert!(choose_uki(Some(b), None, a).is_err(), "legacy single UKI bound elsewhere");
        assert!(choose_uki(None, Some(None), a).is_err(), "unreadable");
        assert_eq!(cmdline_root_partuuid(&format!("quiet root=PARTUUID={} ro", a.to_uppercase())).as_deref(), Some(a));
        assert_eq!(cmdline_root_partuuid("xroot=PARTUUID=11111111-1111-1111-1111-111111111111"), None);
    }

    #[test]
    fn versions_must_match_clear_the_floor_and_move_forward() {
        let floor = RollbackFloor::Min(OsVersion::parse("0.4.0").unwrap());
        assert!(check_versions(Some("0.5.0"), "0.5.0", &floor, Some("0.4.0")).is_ok());
        assert!(check_versions(Some("0.5.0"), "0.6.0", &floor, Some("0.4.0")).unwrap_err().contains("says"));
        assert!(check_versions(Some("0.3.0"), "0.3.0", &floor, Some("0.2.0")).unwrap_err().contains("older"));
        assert!(check_versions(Some("0.4.0"), "0.4.0", &floor, Some("0.4.0")).unwrap_err().contains("not newer"));
        assert!(check_versions(Some("0.4.1"), "0.4.1", &floor, Some("0.5.0")).is_err(), "downgrade");
        assert!(check_versions(None, "0.5.0", &floor, Some("0.4.0")).is_err());
        assert!(check_versions(Some("0.5.0"), "0.5.0", &floor, None).is_err(), "unknown running version");
        assert!(check_versions(Some("0.5.0"), "0.5.0", &RollbackFloor::Invalid("x".into()), Some("0.4.0")).is_err());
        assert!(check_versions(Some("0.2.0"), "0.2.0", &RollbackFloor::Off, Some("0.1.0")).is_ok(), "no floor");
    }

    #[test]
    fn the_destination_is_emptied_and_recreated_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("updates");
        std::fs::create_dir_all(dest.join("sub")).unwrap();
        std::fs::write(dest.join("x"), "x").unwrap();
        reset_dir(&dest).unwrap();
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
        assert_eq!(std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o700);
        // A symlink planted at the destination is removed, not followed.
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), "k").unwrap();
        let planted = dir.path().join("planted");
        std::os::unix::fs::symlink(&outside, &planted).unwrap();
        reset_dir(&planted).unwrap();
        assert!(outside.join("keep").exists());
        assert!(std::fs::symlink_metadata(&planted).unwrap().is_dir());
    }
    // ── end to end, with a throwaway signing key ──────────────────────────

    const PART_A: &str = "11111111-2222-3333-4444-555555555555";
    const PART_B: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

    /// A minimal PE image with the given sections (enough for the `.osrel`
    /// and `.cmdline` readers).
    fn pe(sections: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let pe_at = 0x40usize;
        let table = pe_at + 4 + 20;
        let data_start = table + 40 * sections.len();
        let mut out = vec![0u8; data_start];
        out[0..2].copy_from_slice(b"MZ");
        out[0x3c..0x40].copy_from_slice(&(pe_at as u32).to_le_bytes());
        out[pe_at..pe_at + 4].copy_from_slice(b"PE\0\0");
        out[pe_at + 6..pe_at + 8].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        let mut ptr = data_start;
        for (i, (name, data)) in sections.iter().enumerate() {
            let at = table + i * 40;
            out[at..at + name.len()].copy_from_slice(name.as_bytes());
            out[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_le_bytes());
            out[at + 16..at + 20].copy_from_slice(&(data.len() as u32).to_le_bytes());
            out[at + 20..at + 24].copy_from_slice(&(ptr as u32).to_le_bytes());
            ptr += data.len();
        }
        for (_, data) in sections {
            out.extend_from_slice(data);
        }
        out
    }

    fn uki(version: &str, partuuid: &str) -> Vec<u8> {
        pe(&[
            (".osrel", format!("ID=duduclaw\nIMAGE_VERSION={version}\n").into_bytes()),
            (".cmdline", format!("rootwait root=PARTUUID={partuuid} ro").into_bytes()),
            (".linux", b"kernel".to_vec()),
        ])
    }

    struct Release {
        _dir: tempfile::TempDir,
        source: std::path::PathBuf,
        dest: std::path::PathBuf,
        pubkey: String,
        root_name: String,
    }

    /// A signed release `version` in a temp source dir: root payload (with a
    /// hole), canonical UKI bound to slot A, slot-B UKI bound to slot B.
    fn release(version: &str) -> Release {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        std::fs::create_dir(&source).unwrap();
        let root_name = format!("duduclaw-os_{version}.root-x86-64.raw");
        let mut root = b"root-fs-start".to_vec();
        root.extend(std::iter::repeat_n(0u8, 5 * 1024 * 1024));
        root.extend_from_slice(b"root-fs-end");
        let files: Vec<(String, Vec<u8>)> = vec![
            (root_name.clone(), root),
            (format!("duduclaw-os_{version}.efi"), uki(version, PART_A)),
            (format!("duduclaw-os_{version}.slot-b.efi"), uki(version, PART_B)),
        ];
        let mut manifest = String::new();
        for (name, bytes) in &files {
            std::fs::write(source.join(name), bytes).unwrap();
            manifest.push_str(&format!("{:x}  {name}\n", Sha256::digest(bytes)));
        }
        std::fs::write(source.join("SHA256SUMS"), &manifest).unwrap();
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let sig = minisign::sign(Some(&kp.pk), &kp.sk, std::io::Cursor::new(manifest.as_bytes()), Some("test"), None)
            .unwrap()
            .into_string();
        std::fs::write(source.join("SHA256SUMS.minisig"), sig).unwrap();
        Release { pubkey: kp.pk.to_base64(), source, dest, root_name, _dir: dir }
    }

    fn run(r: &Release, dest_partuuid: &str, floor: &RollbackFloor, running: &str) -> Result<ReleaseFiles, String> {
        stage(&r.source, &r.dest, &r.pubkey, "x86-64", dest_partuuid, floor, Some(running))
    }

    fn assert_refused_and_empty(r: &Release, result: Result<ReleaseFiles, String>, what: &str) {
        let err = result.expect_err(what);
        assert!(!err.is_empty(), "{what}");
        let left: Vec<_> = std::fs::read_dir(&r.dest).map(|d| d.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
        assert!(left.is_empty(), "{what}: destination not emptied: {left:?}");
    }

    #[test]
    fn a_signed_release_is_staged_with_the_uki_for_the_destination_slot() {
        let r = release("0.5.0");
        let floor = RollbackFloor::Min(OsVersion::parse("0.4.0").unwrap());
        let staged = run(&r, PART_B, &floor, "0.4.0").unwrap();
        assert_eq!(staged.version, "0.5.0");
        let mut names: Vec<String> =
            std::fs::read_dir(&r.dest).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        let mut want = vec![r.root_name.clone(), "duduclaw-os_0.5.0.efi".to_string()];
        want.sort();
        assert_eq!(names, want, "only what sysupdate needs");
        let siblings: Vec<_> = std::fs::read_dir(r.dest.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(siblings.is_empty(), "no staging leftovers: {siblings:?}");
        // The canonical name holds the slot-B variant's exact signed bytes.
        assert_eq!(std::fs::read(r.dest.join("duduclaw-os_0.5.0.efi")).unwrap(), uki("0.5.0", PART_B));
        assert_eq!(std::fs::read(r.dest.join(&r.root_name)).unwrap(), std::fs::read(r.source.join(&r.root_name)).unwrap());
        // Slot A gets the canonical variant.
        let staged = run(&r, PART_A, &floor, "0.4.0").unwrap();
        assert_eq!(staged.version, "0.5.0");
        assert_eq!(std::fs::read(r.dest.join("duduclaw-os_0.5.0.efi")).unwrap(), uki("0.5.0", PART_A));
    }

    #[test]
    fn tampered_unsigned_or_misplaced_releases_are_refused_and_nothing_is_left() {
        let off = RollbackFloor::Off;
        // Tampered payload after signing.
        let r = release("0.5.0");
        std::fs::write(r.source.join(&r.root_name), b"evil root").unwrap();
        assert_refused_and_empty(&r, run(&r, PART_A, &off, "0.4.0"), "tampered payload");
        // Wrong key.
        let r = release("0.5.0");
        let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap().pk.to_base64();
        let res = stage(&r.source, &r.dest, &other, "x86-64", PART_A, &off, Some("0.4.0"));
        assert_refused_and_empty(&r, res, "wrong key");
        // Missing signature.
        let r = release("0.5.0");
        std::fs::remove_file(r.source.join("SHA256SUMS.minisig")).unwrap();
        assert_refused_and_empty(&r, run(&r, PART_A, &off, "0.4.0"), "missing .minisig");
        // A payload replaced by a symlink to an identical file elsewhere.
        let r = release("0.5.0");
        let elsewhere = r.source.parent().unwrap().join("elsewhere.raw");
        std::fs::rename(r.source.join(&r.root_name), &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, r.source.join(&r.root_name)).unwrap();
        assert_refused_and_empty(&r, run(&r, PART_A, &off, "0.4.0"), "symlinked payload");
        // No UKI bound to the destination slot.
        let r = release("0.5.0");
        assert_refused_and_empty(&r, run(&r, "deadbeef-0000-0000-0000-000000000000", &off, "0.4.0"), "unbound slot");
    }

    #[test]
    fn old_equal_or_below_floor_releases_are_refused_and_nothing_is_left() {
        let r = release("0.5.0");
        assert_refused_and_empty(&r, run(&r, PART_A, &RollbackFloor::Off, "0.5.0"), "equal version");
        assert_refused_and_empty(&r, run(&r, PART_A, &RollbackFloor::Off, "0.6.0"), "older than running");
        let floor = RollbackFloor::Min(OsVersion::parse("0.6.0").unwrap());
        assert_refused_and_empty(&r, run(&r, PART_A, &floor, "0.4.0"), "below the floor");
        // A UKI whose embedded version disagrees with its signed name (an old
        // image published under a newer name) is refused too.
        let r = release("0.9.0");
        let old = uki("0.3.0", PART_A);
        std::fs::write(r.source.join("duduclaw-os_0.9.0.efi"), &old).unwrap();
        let manifest = std::fs::read_to_string(r.source.join("SHA256SUMS")).unwrap();
        let fixed: String = manifest
            .lines()
            .map(|l| {
                if l.ends_with("  duduclaw-os_0.9.0.efi") {
                    format!("{:x}  duduclaw-os_0.9.0.efi\n", Sha256::digest(&old))
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        std::fs::write(r.source.join("SHA256SUMS"), &fixed).unwrap();
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let sig = minisign::sign(Some(&kp.pk), &kp.sk, std::io::Cursor::new(fixed.as_bytes()), None, None).unwrap();
        std::fs::write(r.source.join("SHA256SUMS.minisig"), sig.into_string()).unwrap();
        let res = stage(&r.source, &r.dest, &kp.pk.to_base64(), "x86-64", PART_A, &RollbackFloor::Off, Some("0.4.0"));
        assert_refused_and_empty(&r, res, "embedded version differs from the release");
    }

    /// H1: work happens in a private sibling; `dest` is replaced only after
    /// every check passed, and a refused release (here: older than running)
    /// never reaches `dest` under its sysupdate name.
    #[test]
    fn checks_finish_before_anything_is_placed_where_sysupdate_reads() {
        let r = release("0.5.0");
        std::fs::create_dir_all(&r.dest).unwrap();
        std::fs::write(r.dest.join("previous"), "x").unwrap();
        // stage_inner never touches `dest`, whether it succeeds or fails.
        let scratch = r.dest.parent().unwrap().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        assert!(stage_inner(&r.source, &scratch, &r.pubkey, "x86-64", PART_A, &RollbackFloor::Off, Some("0.6.0")).is_err());
        assert!(r.dest.join("previous").exists());
        assert!(!scratch.join("duduclaw-os_0.5.0.efi").exists(), "a too-old UKI never gets its sysupdate name");
        let scratch2 = r.dest.parent().unwrap().join("scratch2");
        std::fs::create_dir(&scratch2).unwrap();
        assert!(stage_inner(&r.source, &scratch2, &r.pubkey, "x86-64", PART_A, &RollbackFloor::Off, Some("0.4.0")).is_ok());
        assert!(r.dest.join("previous").exists(), "only stage() places, after the checks");
        // stage() replaces `dest` as a whole on success.
        run(&r, PART_A, &RollbackFloor::Off, "0.4.0").unwrap();
        assert!(!r.dest.join("previous").exists());
        assert!(r.dest.join("duduclaw-os_0.5.0.efi").exists());
    }

    /// L1: a source directory that is itself a symlink is refused.
    #[test]
    fn a_symlinked_source_directory_is_refused() {
        let r = release("0.5.0");
        let link = r.source.parent().unwrap().join("source-link");
        std::os::unix::fs::symlink(&r.source, &link).unwrap();
        let res = stage(&link, &r.dest, &r.pubkey, "x86-64", PART_A, &RollbackFloor::Off, Some("0.4.0"));
        assert_refused_and_empty(&r, res, "symlinked source directory");
        assert!(SourceDir::open(&link).is_err());
        let dir = SourceDir::open(&r.source).unwrap();
        assert!(dir.open_file("../SHA256SUMS", 1024).is_err(), "only plain names");
        assert!(dir.read_small("SHA256SUMS", 64 * 1024).is_ok());
    }

    /// Leftovers of an interrupted run are removed: exact names only, real
    /// directories only, owned by the expected uid only, links never
    /// followed.
    #[test]
    fn leftovers_of_an_interrupted_run_are_removed_and_nothing_else() {
        // SAFETY: getuid has no preconditions.
        let me = unsafe { libc::getuid() };
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("updates");
        let hex = "0123456789abcdef0123456789abcdef";
        let staging = dir.path().join(format!(".updates-staging-{hex}"));
        let old = dir.path().join(format!(".updates-old-{hex}"));
        for d in [&staging, &old] {
            std::fs::create_dir_all(d.join("sub")).unwrap();
            std::fs::write(d.join("sub/big.raw"), "x").unwrap();
        }
        // Not ours: other names, a file with our shape, a symlink with our shape.
        let other = dir.path().join(".updates-staging-short");
        std::fs::create_dir(&other).unwrap();
        let unrelated = dir.path().join(".old-0123456789abcdef0123456789abcdef");
        std::fs::create_dir(&unrelated).unwrap();
        let file = dir.path().join(format!(".updates-old-{}", "f".repeat(32)));
        std::fs::write(&file, "x").unwrap();
        let target = dir.path().join("keep");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("precious"), "x").unwrap();
        let link = dir.path().join(format!(".updates-staging-{}", "e".repeat(32)));
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert_eq!(remove_staging_leftovers(&dest, me.wrapping_add(1)), 0, "wrong owner: untouched");
        assert!(staging.exists() && old.exists());
        assert_eq!(remove_staging_leftovers(&dest, me), 2);
        assert!(!staging.exists() && !old.exists());
        assert!(other.exists() && unrelated.exists() && file.exists());
        assert!(std::fs::symlink_metadata(&link).is_ok(), "a link is not removed or followed");
        assert!(target.join("precious").exists());
    }

    /// stage() clears leftovers before it starts.
    #[test]
    fn staging_starts_by_clearing_leftovers() {
        let r = release("0.5.0");
        let leftover = r.dest.parent().unwrap().join(format!(".dest-staging-{}", "a".repeat(32)));
        std::fs::create_dir(&leftover).unwrap();
        run(&r, PART_A, &RollbackFloor::Off, "0.4.0").unwrap();
        assert!(!leftover.exists());
    }
}
