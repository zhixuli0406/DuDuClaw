//! The signed OS release format, shared by the gateway (which downloads
//! and checks a release) and `duduclaw-sysd` (which, as root, checks it
//! again before anything is installed: on DuDuClaw OS the gateway's user
//! is also the AI employees' user, so its check alone is not a boundary).
//!
//! Moved here from `duduclaw-gateway/src/os_update.rs` (2026-10) so the
//! pinned key, the manifest grammar, the release-file selection and the
//! A/B slot resolution exist once; the gateway re-exports them.

use std::path::{Path, PathBuf};

/// The directory the gateway downloads OS releases into on DuDuClaw OS
/// (its own home, so untrusted from root's point of view);
/// `staged_update` reads it, the gateway's verified-install layout writes it.
pub const UPDATE_SOURCE_DIR: &str = "/data/duduclaw/updates";

/// True when `s` is a canonical hyphenated UUID (8-4-4-4-12 hex).
pub fn is_uuid_text(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let mut parts = s.split('-');
    for want in [8usize, 4, 4, 4, 12] {
        match parts.next() {
            Some(p) if p.len() == want && p.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

/// Public half of the **OS image** signing key (`~/.minisign/duduclaw-os-release.key`
/// on the release host, `MINISIGN_OS_SECRET_KEY` in CI). Deliberately NOT
/// `crate::updater::UPDATE_PUBKEY` — see the module doc.
///
/// Pinned in the binary on purpose: an image-resident keyring file lives on a
/// writable root and can be replaced; a constant cannot.
pub const OS_IMAGE_UPDATE_PUBKEY: &str = "RWQyI00ugZ/+WVisQ2ZnKeTqFs8Ze8h2X11FO9Z8le0YubFMXYTwQD7n";

/// Name of the signed manifest inside a release directory.
pub const MANIFEST_NAME: &str = "SHA256SUMS";
/// Name of its detached minisign signature.
pub const SIGNATURE_NAME: &str = "SHA256SUMS.minisig";


/// Where systemd-boot's discoverable-partition symlinks live. Both trees are
/// world-readable udev output, which is why the unprivileged gateway can
/// resolve the A/B layout without opening a block device (it is not in the
/// `disk` group, and must not be).
pub const BY_PARTLABEL_DIR: &str = "/dev/disk/by-partlabel";
pub const BY_PARTUUID_DIR: &str = "/dev/disk/by-partuuid";

/// The partition label systemd-sysupdate reserves for "this slot is free".
pub const EMPTY_SLOT_LABEL: &str = "_empty";
/// Prefix of an occupied slot's label; the remainder is the image version.
pub const SLOT_LABEL_PREFIX: &str = "duduclaw-os_";


/// One `<sha256>  <name>` row of the signed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub sha256: String,
    pub name: String,
}

/// Parse a `shasum`-style manifest.
///
/// Every filename is validated to be a bare basename: no separator, no `..`,
/// no absolute path, no NUL. The manifest is signed, but a signed manifest
/// from a *legitimate* release must still never be able to name a path
/// outside the staging directory — the signature proves origin, not intent.
pub fn parse_manifest(text: &str) -> Result<Vec<ManifestEntry>, String> {
    let mut out = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let digest = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or_default().trim();
        // `shasum` writes "<hash>  <name>" for text mode and "<hash> *<name>"
        // for binary mode; strip the binary marker rather than making it part
        // of the filename.
        let name = rest.strip_prefix('*').unwrap_or(rest);
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "manifest line {}: not a SHA-256 digest",
                lineno + 1
            ));
        }
        if !is_safe_basename(name) {
            return Err(format!(
                "manifest line {}: {name:?} is not a plain filename",
                lineno + 1
            ));
        }
        out.push(ManifestEntry {
            sha256: digest.to_ascii_lowercase(),
            name: name.to_string(),
        });
    }
    if out.is_empty() {
        return Err("manifest lists no files".to_string());
    }
    Ok(out)
}

pub fn is_safe_basename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

/// Suffix of the slot-B pre-signed UKI variant (T4, 2026-09-02 — see the
/// module doc). Checked BEFORE the generic `.efi` suffix below, since a
/// slot-B filename is itself a (more specific) `.efi` filename and would
/// otherwise be misparsed as a canonical UKI with `.slot-b` glued onto its
/// version string.
pub const UKI_SLOT_B_SUFFIX: &str = ".slot-b.efi";

/// The payload files a release must contain, already matched to this
/// machine's architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFiles {
    pub version: String,
    pub arch: String,
    pub root: ManifestEntry,
    /// The canonical UKI — `duduclaw-os_<version>.efi`. Always present:
    /// this is what a legacy (pre-T4) release ships alone, and what every
    /// release stages under (`clear_stale_payloads`, sysupdate's own
    /// `MatchPattern=`), whether or not it was the variant actually
    /// selected for this device's destination slot.
    pub uki: ManifestEntry,
    /// The slot-B pre-signed UKI variant (T4) — `duduclaw-os_<version>.slot-b.efi`.
    /// `None` for a release that ships only the canonical UKI (pre-T4, or a
    /// release line that never adopts per-slot UKIs); [`bind_uki_to_slot`]
    /// reads this to decide between selection and the legacy device-side
    /// rewrite.
    pub uki_slot_b: Option<ManifestEntry>,
    /// The dm-verity hash-tree artifact (VER-V wave) —
    /// `duduclaw-os_<version>.verity-<arch>.raw`. One per release,
    /// slot-independent (see the module doc's "VER-V" section). `None` for
    /// a release built before dm-verity landed — a legitimate, non-error
    /// state.
    pub verity: Option<ManifestEntry>,
    /// The signed companion carrying [`Self::verity`]'s root hash as 64 hex
    /// characters — `duduclaw-os_<version>.verity-<arch>.roothash`. Always
    /// present exactly when `verity` is; [`select_release_files`] refuses a
    /// release that ships one without the other.
    pub verity_roothash: Option<ManifestEntry>,
}

/// Pick this machine's payload set out of a manifest.
///
/// Names must be exactly `duduclaw-os_<version>.root-<arch>.raw`,
/// `duduclaw-os_<version>.efi`, and (optionally, T4)
/// `duduclaw-os_<version>.slot-b.efi` — the same shapes
/// `mkosi.extra/etc/sysupdate.d/*.transfer` / `recipes-duduclaw/`
/// `duduclaw-ab-update/files/*.transfer` match on, so a name this function
/// accepts is a name sysupdate will find. Anything else in the manifest (a
/// checksum of the manifest itself, a second architecture, a release note)
/// is ignored rather than treated as an error; anything *ambiguous* (two
/// roots for this arch, two canonical UKIs, two slot-B UKIs, mismatched
/// versions) is refused, because guessing which one was meant is exactly how
/// a machine ends up with a kernel from one build and a root from another.
pub fn select_release_files(entries: &[ManifestEntry], arch: &str) -> Result<ReleaseFiles, String> {
    let root_suffix = format!(".root-{arch}.raw");
    // VER-V: checked BEFORE `root_suffix` below for the same reason
    // `UKI_SLOT_B_SUFFIX` is checked before the generic `.efi` match —
    // `.verity-<arch>.raw` is itself a (more specific) `.raw` filename and
    // would otherwise be misparsed as a root payload.
    let verity_suffix = format!(".verity-{arch}.raw");
    let verity_roothash_suffix = format!(".verity-{arch}.roothash");
    let mut roots: Vec<(&ManifestEntry, String)> = Vec::new();
    let mut ukis: Vec<(&ManifestEntry, String)> = Vec::new();
    let mut uki_slot_bs: Vec<(&ManifestEntry, String)> = Vec::new();
    let mut veritys: Vec<(&ManifestEntry, String)> = Vec::new();
    let mut verity_roothashes: Vec<(&ManifestEntry, String)> = Vec::new();

    for e in entries {
        let Some(rest) = e.name.strip_prefix(SLOT_LABEL_PREFIX) else {
            continue;
        };
        if let Some(v) = rest.strip_suffix(&verity_suffix) {
            if is_version_text(v) {
                veritys.push((e, v.to_string()));
            }
        } else if let Some(v) = rest.strip_suffix(&verity_roothash_suffix) {
            if is_version_text(v) {
                verity_roothashes.push((e, v.to_string()));
            }
        } else if let Some(v) = rest.strip_suffix(&root_suffix) {
            if is_version_text(v) {
                roots.push((e, v.to_string()));
            }
        } else if let Some(v) = rest.strip_suffix(UKI_SLOT_B_SUFFIX) {
            if is_version_text(v) {
                uki_slot_bs.push((e, v.to_string()));
            }
        } else if let Some(v) = rest.strip_suffix(".efi") {
            if is_version_text(v) {
                ukis.push((e, v.to_string()));
            }
        }
    }

    let (root, root_ver) = match roots.len() {
        1 => roots.remove(0),
        0 => {
            return Err(format!(
                "the release contains no root payload for this machine's architecture ({arch})"
            ));
        }
        n => return Err(format!("the release contains {n} root payloads for {arch}")),
    };
    let (uki, uki_ver) = match ukis.len() {
        1 => ukis.remove(0),
        0 => return Err("the release contains no kernel image".to_string()),
        n => return Err(format!("the release contains {n} kernel images")),
    };
    if root_ver != uki_ver {
        return Err(format!(
            "the release is inconsistent: root payload is {root_ver} but the kernel image is {uki_ver}"
        ));
    }
    let uki_slot_b = match uki_slot_bs.len() {
        0 => None,
        1 => {
            let (entry, v) = uki_slot_bs.remove(0);
            if v != root_ver {
                return Err(format!(
                    "the release is inconsistent: root payload is {root_ver} but the slot-B \
                     kernel image is {v}"
                ));
            }
            Some(entry.clone())
        }
        n => return Err(format!("the release contains {n} slot-B kernel images")),
    };
    let verity = match veritys.len() {
        0 => None,
        1 => {
            let (entry, v) = veritys.remove(0);
            if v != root_ver {
                return Err(format!(
                    "the release is inconsistent: root payload is {root_ver} but the verity \
                     hash tree is {v}"
                ));
            }
            Some(entry.clone())
        }
        n => {
            return Err(format!(
                "the release contains {n} verity hash trees for {arch}"
            ));
        }
    };
    let verity_roothash = match verity_roothashes.len() {
        0 => None,
        1 => {
            let (entry, v) = verity_roothashes.remove(0);
            if v != root_ver {
                return Err(format!(
                    "the release is inconsistent: root payload is {root_ver} but the verity \
                     roothash companion is {v}"
                ));
            }
            Some(entry.clone())
        }
        n => {
            return Err(format!(
                "the release contains {n} verity roothash companions for {arch}"
            ));
        }
    };
    // VER-V: a hash tree without its signed roothash companion (or vice
    // versa) is a half-shipped release — refused rather than guessed, the
    // same discipline every other ambiguous/incomplete pairing above gets.
    if verity.is_some() != verity_roothash.is_some() {
        return Err(
            "the release ships a verity hash tree without its roothash companion, or vice \
             versa — both or neither must be present"
                .to_string(),
        );
    }
    Ok(ReleaseFiles {
        version: root_ver,
        arch: arch.to_string(),
        root: root.clone(),
        uki: uki.clone(),
        uki_slot_b,
        verity,
        verity_roothash,
    })
}

/// A version token as it may appear in a filename and a GPT partition label.
/// Deliberately narrow — this string becomes part of a path and a partition
/// label, so it may not carry a separator, a space or a shell metacharacter.
pub fn is_version_text(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 32
        && v.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}

/// systemd's architecture specifier for this build — the same spelling the
/// GPT type table and the `%a` in the sysupdate transfers use.
pub fn machine_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x86-64"
    } else {
        // Honest rather than a wrong guess: no payload will match, and the
        // caller reports "no root payload for this architecture".
        "unsupported"
    }
}


// ---------------------------------------------------------------------------
// A/B slot resolution
// ---------------------------------------------------------------------------

/// One `/dev/disk/by-partlabel/` entry, resolved to its device node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotEntry {
    pub label: String,
    pub device: PathBuf,
}

/// True for a label that names one of this image's two root slots: either
/// systemd-sysupdate's reserved `_empty`, or `duduclaw-os_<version>`.
///
/// The prefix test is anchored and its remainder is shape-checked (project
/// convention 2: no unanchored `starts_with` for a routing decision) —
/// `duduclaw-os_evil/../..` is not a slot label.
pub fn is_root_slot_label(label: &str) -> bool {
    label == EMPTY_SLOT_LABEL
        || label
            .strip_prefix(SLOT_LABEL_PREFIX)
            .is_some_and(is_version_text)
}

/// Choose the slot an update will be written into: the root slot that is not
/// the one currently mounted at `/`.
///
/// With exactly two fixed slots this is unambiguous and needs no arithmetic
/// — which is the point. systemd-sysupdate makes the same choice by its own
/// rules (free `_empty` slot first, else the oldest non-`ProtectVersion=`
/// instance); resolving it here as well is what lets the UKI be bound to the
/// right root *before* sysupdate runs, since sysupdate has no conditional
/// logic and cannot pick a payload by destination.
///
/// Fails closed on anything other than exactly one candidate: a machine with
/// a third root-labelled partition, or none, is not a layout this code has
/// ever been tested against, and writing to the wrong partition on a
/// headless box is the failure mode this whole package exists to avoid.
pub fn pick_destination_slot(
    entries: &[SlotEntry],
    current_root: &Path,
) -> Result<SlotEntry, String> {
    let slots: Vec<&SlotEntry> = entries
        .iter()
        .filter(|e| is_root_slot_label(&e.label))
        .collect();
    if slots.len() != 2 {
        return Err(format!(
            "expected exactly 2 root slots on this disk, found {} ({:?})",
            slots.len(),
            slots.iter().map(|s| &s.label).collect::<Vec<_>>()
        ));
    }
    if !slots.iter().any(|s| s.device == current_root) {
        return Err(format!(
            "the running root {} is not one of the two A/B slots ({:?}) — \
             refusing to guess a destination",
            current_root.display(),
            slots.iter().map(|s| &s.device).collect::<Vec<_>>()
        ));
    }
    let candidates: Vec<&&SlotEntry> = slots.iter().filter(|s| s.device != current_root).collect();
    match candidates.len() {
        1 => Ok((*candidates[0]).clone()),
        n => Err(format!(
            "expected exactly 1 destination slot, found {n} — the two slots resolve \
             to the same device node"
        )),
    }
}

/// Read `/dev/disk/by-partlabel/`, resolving every symlink to its device.
pub fn read_slot_entries(dir: &Path) -> Result<Vec<SlotEntry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let label = entry.file_name().to_string_lossy().into_owned();
        // udev percent-escapes characters that are illegal in a device name;
        // our labels use none of them, so a label that needed escaping is
        // simply not one of ours.
        let Ok(device) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        out.push(SlotEntry { label, device });
    }
    Ok(out)
}

/// PARTUUID of `device`, via the world-readable `/dev/disk/by-partuuid` tree.
pub fn partuuid_of(dir: &Path, device: &Path) -> Result<String, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for entry in rd.flatten() {
        let Ok(target) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if target == device {
            let uuid = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if is_uuid_text(&uuid) {
                return Ok(uuid);
            }
            return Err(format!("{uuid:?} is not a canonical PARTUUID"));
        }
    }
    Err(format!("no PARTUUID found for {}", device.display()))
}

/// Device currently mounted at `/`, from a `/proc/mounts`-shaped table.
pub fn parse_root_device(mounts: &str) -> Option<String> {
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let source = f.next()?;
        let target = f.next().unwrap_or_default();
        if target == "/" && source.starts_with('/') {
            return Some(source.to_string());
        }
    }
    None
}

pub fn current_root_device() -> Result<PathBuf, String> {
    let mounts = std::fs::read_to_string("/proc/mounts")
        .map_err(|e| format!("cannot read /proc/mounts: {e}"))?;
    let dev = parse_root_device(&mounts).ok_or("no device is mounted at /")?;
    std::fs::canonicalize(&dev).map_err(|e| format!("cannot resolve {dev}: {e}"))
}

