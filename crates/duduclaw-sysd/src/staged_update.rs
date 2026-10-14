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
    let meta = file.metadata().map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > max {
        return Err(format!("{} is larger than allowed ({} > {max} bytes)", path.display(), meta.len()));
    }
    Ok((file, meta.len()))
}

/// Read a small file whole (manifest, signature).
pub fn read_small(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let (file, _) = open_regular(path, max)?;
    let mut out = Vec::new();
    file.take(max + 1).read_to_end(&mut out).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if out.len() as u64 > max {
        return Err(format!("{} grew past its size limit while being read", path.display()));
    }
    Ok(out)
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
    let result = (|| {
        let (mut input, len) = open_regular(src, max)?;
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

/// Everything before `systemd-sysupdate update`: verify, select, copy,
/// check versions. `dest_partuuid` is the destination slot's PARTUUID,
/// `running` the running image version. Leaves the staged files in `dest`
/// on success; the caller empties `dest` afterwards either way.
pub fn stage(
    source: &Path,
    dest: &Path,
    pubkey: &str,
    arch: &str,
    dest_partuuid: &str,
    floor: &RollbackFloor,
    running: Option<&str>,
) -> Result<ReleaseFiles, String> {
    let manifest = read_small(&source.join(crate::release::MANIFEST_NAME), MAX_MANIFEST_BYTES)?;
    let sig = read_small(&source.join(crate::release::SIGNATURE_NAME), MAX_SIGNATURE_BYTES)?;
    let sig = String::from_utf8(sig).map_err(|_| "the signature file is not text".to_string())?;
    verify_signature(&manifest, &sig, pubkey)?;
    let text = String::from_utf8(manifest).map_err(|_| "the manifest is not text".to_string())?;
    let entries = crate::release::parse_manifest(&text)?;
    let release = crate::release::select_release_files(&entries, arch)?;

    reset_dir(dest)?;
    let copy = |entry: &ManifestEntry, as_name: &str, max: u64| {
        copy_verified(&source.join(&entry.name), &dest.join(as_name), &entry.sha256, max)
    };
    // Kernel images first (small), into temporary names, to choose one.
    let canonical_tmp = format!(".candidate-{}", release.uki.name);
    copy(&release.uki, &canonical_tmp, MAX_UKI_BYTES)?;
    let slot_b_tmp = release.uki_slot_b.as_ref().map(|e| format!(".candidate-{}", e.name));
    if let (Some(entry), Some(tmp)) = (&release.uki_slot_b, &slot_b_tmp) {
        copy(entry, tmp, MAX_UKI_BYTES)?;
    }
    let canonical_uuid = uki_root_partuuid(&dest.join(&canonical_tmp)).ok();
    let slot_b_uuid = slot_b_tmp.as_ref().map(|tmp| uki_root_partuuid(&dest.join(tmp)).ok());
    let choice = choose_uki(canonical_uuid.as_deref(), slot_b_uuid.as_ref().map(|o| o.as_deref()), dest_partuuid)?;
    let chosen_tmp = match choice {
        UkiChoice::Canonical => canonical_tmp.clone(),
        UkiChoice::SlotB => slot_b_tmp.clone().ok_or("no slot-B kernel image")?,
    };
    let uki_final = dest.join(&release.uki.name);
    std::fs::rename(dest.join(&chosen_tmp), &uki_final).map_err(|e| e.to_string())?;
    for tmp in [Some(canonical_tmp), slot_b_tmp].into_iter().flatten() {
        let _ = std::fs::remove_file(dest.join(tmp));
    }
    let embedded = crate::rollback_floor::uki_image_version(&uki_final).ok();
    check_versions(embedded.as_deref(), &release.version, floor, running)?;

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
        assert!(dest.join("leftover").exists(), "a refused release does not touch the destination");
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
}
