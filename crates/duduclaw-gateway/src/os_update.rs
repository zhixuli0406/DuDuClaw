//! OS image update staging — download, verify, bind, then hand to sysupdate (H3d).
//!
//! Full design: `commercial/docs/DESIGN-ab-update-rollback-2026-08.md` §5.
//!
//! ## Why the gateway does the verifying
//!
//! `systemd-sysupdate`'s source for this appliance is
//! `[Source] Type=regular-file` — a local staging directory — and
//! `sysupdate.d(5)` v257 is explicit that for that resource type "no
//! integrity or authentication verification is done". There is no switch to
//! turn it on. So whatever puts bytes into the staging directory **is** the
//! authenticity gate, and that is this module.
//!
//! The chain is the one already shipping for app binaries
//! ([`crate::updater::verify_archive_with_pubkey`]) with two deliberate
//! differences:
//!
//! 1. **A separate keypair** ([`OS_IMAGE_UPDATE_PUBKEY`]). Not cryptography
//!    — blast radius. An app-binary update written wrong is one service to
//!    reinstall; an OS image written wrong is a box that does not come back
//!    and cannot be reached remotely. Two risk levels must not share a key
//!    that can impersonate the other.
//! 2. **One signature over a manifest, not per-file signatures.** A release
//!    is a root payload *and* its UKI, and they must be from the same build:
//!    signing each file independently would let an attacker (or a mirror
//!    bug) serve a legitimately-signed root from one version beside a
//!    legitimately-signed UKI from another. `SHA256SUMS` binds them, and the
//!    detached `SHA256SUMS.minisig` is the only signature.
//!
//! ## Order of operations (each step fails closed)
//!
//! ```text
//! source_url unset                        -> NotConfigured, sysupdate never runs
//! fetch SHA256SUMS + SHA256SUMS.minisig
//! verify signature against the pinned key -> any failure aborts, nothing downloaded
//! parse manifest -> exactly one root payload for this arch + one UKI, same version
//! running version == payload version      -> UpToDate, 5 GiB never downloaded
//! (free space is logged, never a refusal: sparse staging routinely
//!  stores far fewer blocks than a payload's apparent size)
//! download each file, hashing as it lands -> digest mismatch deletes and aborts
//! resolve the destination slot from the live GPT
//! rewrite the UKI's root=PARTUUID= to that slot   (see crate::uki_patch)
//! atomically move both files into the staging dir
//! ```
//!
//! Only then does the caller invoke `systemd-sysupdate update`, which finds
//! a staging directory containing exactly one verified version.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::uki_patch;

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

/// Byte ceilings. Each refuses an absurd response *before* it is written to a
/// small appliance disk, and each is far above any legitimate value: a UKI is
/// ~150 MiB, a root slot is 5 GiB, the manifest is a few hundred bytes.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 4 * 1024;
const MAX_UKI_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ROOT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Streaming chunk size, and the granularity at which an all-zero run is
/// turned into a filesystem hole (see [`write_sparse_chunk`]).
const CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Where systemd-boot's discoverable-partition symlinks live. Both trees are
/// world-readable udev output, which is why the unprivileged gateway can
/// resolve the A/B layout without opening a block device (it is not in the
/// `disk` group, and must not be).
const BY_PARTLABEL_DIR: &str = "/dev/disk/by-partlabel";
const BY_PARTUUID_DIR: &str = "/dev/disk/by-partuuid";

/// The partition label systemd-sysupdate reserves for "this slot is free".
const EMPTY_SLOT_LABEL: &str = "_empty";
/// Prefix of an occupied slot's label; the remainder is the image version.
const SLOT_LABEL_PREFIX: &str = "duduclaw-os_";

// ---------------------------------------------------------------------------
// configuration
// ---------------------------------------------------------------------------

/// `config.toml [os_update]`.
///
/// ```toml
/// [os_update]
/// # Base URL of a release directory holding SHA256SUMS, SHA256SUMS.minisig
/// # and the payload files. Empty (the default) means "no update source
/// # configured" — an honest refusal, never a hardcoded vendor URL.
/// source_url = "https://updates.example.com/duduclaw-os/stable/"
/// # Optional override; defaults to <DUDUCLAW_HOME>/updates.
/// staging_dir = "/data/duduclaw/updates"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
pub struct OsUpdateConfig {
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub staging_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigRoot {
    #[serde(default)]
    os_update: Option<OsUpdateConfig>,
}

impl OsUpdateConfig {
    /// Same defensive convention as `TickConfig::from_home` /
    /// `GoalLoopConfig::from_home`: a missing or malformed `config.toml`, or
    /// a missing/malformed `[os_update]` section, resolves to the default
    /// (no source configured) instead of failing the caller.
    pub fn from_home(home: &Path) -> Self {
        let path = home.join("config.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<ConfigRoot>(&text) {
            Ok(root) => root.os_update.unwrap_or_default(),
            Err(e) => {
                warn!("[os_update] config.toml did not parse, using defaults: {e}");
                Self::default()
            }
        }
    }

    /// Resolved staging directory. Default is `<home>/updates`, NOT
    /// `/var/lib/duduclaw/updates` as the design doc originally said: `/var`
    /// lives on the 5 GiB root slot, which cannot hold a 5 GiB root payload,
    /// while `<home>` is on `/data` (the partition that grows to fill the
    /// disk) and is already owned by the unprivileged gateway user. The
    /// sysupdate transfer's `[Source] Path=` tracks this value.
    pub fn staging_dir(&self, home: &Path) -> PathBuf {
        match self.staging_dir.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => home.join("updates"),
        }
    }
}

// ---------------------------------------------------------------------------
// errors / report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageError {
    /// No `[os_update] source_url`. The honest state of a fresh appliance —
    /// never dressed up as a failure, and never a reason to run sysupdate
    /// against whatever happens to be lying in the staging directory.
    NotConfigured,
    /// The source's newest version is the one already running.
    UpToDate(String),
    /// A verification or validation gate said no. Terminal: never retried,
    /// because an integrity gate a retry loop can wear down is not a gate.
    Rejected(String),
    /// Transport failure — the only class where retrying later is sensible.
    Network(String),
    /// Local filesystem / environment failure.
    Io(String),
}

impl StageError {
    /// Stable machine-readable error code — the SAME string both
    /// `handlers.rs`'s `device.update_check`/`device.update_apply` RPCs and
    /// `mcp_os_ops.rs`'s `os_check_update` agent tool return in their
    /// `{"code": ...}` failure shape. Factored out (Y5-3, agent-body update
    /// vertical slice) so a third call site doesn't re-derive a THIRD copy of
    /// this match — the two RPC handlers previously each hand-wrote an
    /// identical `match e { StageError::NotConfigured => "not_configured", ... }`.
    pub fn code(&self) -> &'static str {
        match self {
            StageError::NotConfigured => "not_configured",
            StageError::UpToDate(_) => "up_to_date",
            StageError::Rejected(_) => "verification_failed",
            StageError::Network(_) => "network_error",
            StageError::Io(_) => "io_error",
        }
    }

    /// zh-TW copy for the dashboard. Internal nouns (sysupdate, PARTUUID,
    /// slot numbers) stay out of the user-facing half; the technical detail
    /// rides along only where it is the actual diagnosis.
    pub fn user_message(&self) -> String {
        match self {
            StageError::NotConfigured => "尚未設定更新來源，因此沒有可安裝的更新。\
                 請先在設定中填入更新來源網址。"
                .to_string(),
            StageError::UpToDate(v) => format!("目前已是最新版本（{v}），沒有需要安裝的更新。"),
            StageError::Rejected(why) => {
                format!("更新檔驗證未通過，已拒絕安裝（裝置未被更動）：{why}")
            }
            StageError::Network(why) => format!("無法取得更新檔：{why}"),
            StageError::Io(why) => format!("準備更新檔時發生錯誤：{why}"),
        }
    }
}

/// What staging actually did, for the audit log and the caller's response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageReport {
    pub version: String,
    pub arch: String,
    pub root_payload: PathBuf,
    pub uki_payload: PathBuf,
    pub destination_partuuid: String,
    pub template_partuuid: String,
    pub bytes_downloaded: u64,
}

// ---------------------------------------------------------------------------
// manifest
// ---------------------------------------------------------------------------

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
            return Err(format!("manifest line {}: not a SHA-256 digest", lineno + 1));
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

fn is_safe_basename(name: &str) -> bool {
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

/// The two payload files a release must contain, already matched to this
/// machine's architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFiles {
    pub version: String,
    pub arch: String,
    pub root: ManifestEntry,
    pub uki: ManifestEntry,
}

/// Pick this machine's payload pair out of a manifest.
///
/// Names must be exactly `duduclaw-os_<version>.root-<arch>.raw` and
/// `duduclaw-os_<version>.efi` — the same shapes
/// `mkosi.extra/etc/sysupdate.d/*.transfer` match on, so a name this
/// function accepts is a name sysupdate will find. Anything else in the
/// manifest (a checksum of the manifest itself, a second architecture, a
/// release note) is ignored rather than treated as an error; anything
/// *ambiguous* (two roots for this arch, two UKIs, mismatched versions) is
/// refused, because guessing which one was meant is exactly how a machine
/// ends up with a kernel from one build and a root from another.
pub fn select_release_files(
    entries: &[ManifestEntry],
    arch: &str,
) -> Result<ReleaseFiles, String> {
    let root_suffix = format!(".root-{arch}.raw");
    let mut roots: Vec<(&ManifestEntry, String)> = Vec::new();
    let mut ukis: Vec<(&ManifestEntry, String)> = Vec::new();

    for e in entries {
        let Some(rest) = e.name.strip_prefix(SLOT_LABEL_PREFIX) else {
            continue;
        };
        if let Some(v) = rest.strip_suffix(&root_suffix) {
            if is_version_text(v) {
                roots.push((e, v.to_string()));
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
    Ok(ReleaseFiles {
        version: root_ver,
        arch: arch.to_string(),
        root: root.clone(),
        uki: uki.clone(),
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

/// `IMAGE_VERSION=` out of an os-release file. This is the same field
/// sysupdate's `ProtectVersion=%A` reads, so "what version am I running" has
/// one answer across the whole update chain.
pub fn parse_image_version(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("IMAGE_VERSION=") {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Running image version, or `None` off an appliance image.
pub fn running_image_version() -> Option<String> {
    for path in ["/usr/lib/os-release", "/etc/os-release"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(v) = parse_image_version(&text) {
                return Some(v);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// source URL
// ---------------------------------------------------------------------------

/// A validated update source: either an HTTP(S) base URL or a local
/// directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Http(String),
    Dir(PathBuf),
}

/// Validate and normalise `[os_update] source_url`.
///
/// Accepted:
/// - `https://...` — always.
/// - `http://...` — only for a loopback / private / link-local host. Not a
///   weakening of the trust model: the pinned signature is what makes a
///   payload installable, so plain HTTP costs confidentiality, not
///   integrity, and an on-site LAN mirror is a real appliance deployment.
///   A *public* plaintext host is still refused, because there the operator
///   almost certainly meant https.
/// - `file:///abs/path` — an operator-mounted USB or an offline mirror. It
///   goes through the identical verification path; "offline" is not an
///   excuse for an unverified install.
pub fn parse_source(raw: &str) -> Result<Source, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err("no update source configured".to_string());
    }
    if s.len() > 512 {
        return Err("update source URL is implausibly long".to_string());
    }
    if s.contains(char::is_whitespace) || s.contains("..") {
        return Err("update source URL contains an illegal sequence".to_string());
    }
    if let Some(path) = s.strip_prefix("file://") {
        let path = if path.is_empty() { "/" } else { path };
        if !path.starts_with('/') {
            return Err("file:// update source must be an absolute path".to_string());
        }
        return Ok(Source::Dir(PathBuf::from(path)));
    }
    let host = if let Some(rest) = s.strip_prefix("https://") {
        rest
    } else if let Some(rest) = s.strip_prefix("http://") {
        let host = host_of(rest);
        if !is_local_host(host) {
            return Err(format!(
                "plain http:// is only accepted for a private/loopback update mirror, \
                 and {host:?} is not one — use https://"
            ));
        }
        rest
    } else {
        return Err("update source must start with https://, http:// or file://".to_string());
    };
    if host_of(host).is_empty() {
        return Err("update source URL has no host".to_string());
    }
    Ok(Source::Http(s.trim_end_matches('/').to_string()))
}

fn host_of(after_scheme: &str) -> &str {
    let end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..end];
    // Strip userinfo, then the port. Bracketed IPv6 keeps its brackets,
    // which `is_local_host` handles.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    authority.split(':').next().unwrap_or(authority)
}

/// Loopback, RFC1918, CGNAT, or link-local — the address ranges an on-site
/// update mirror actually lives on.
fn is_local_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            v6.is_loopback()
                // fc00::/7 unique-local, fe80::/10 link-local.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
        Err(_) => false,
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
fn read_slot_entries(dir: &Path) -> Result<Vec<SlotEntry>, String> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
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
fn partuuid_of(dir: &Path, device: &Path) -> Result<String, String> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for entry in rd.flatten() {
        let Ok(target) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if target == device {
            let uuid = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if uki_patch::is_uuid_text(&uuid) {
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

fn current_root_device() -> Result<PathBuf, String> {
    let mounts = std::fs::read_to_string("/proc/mounts")
        .map_err(|e| format!("cannot read /proc/mounts: {e}"))?;
    let dev = parse_root_device(&mounts).ok_or("no device is mounted at /")?;
    std::fs::canonicalize(&dev).map_err(|e| format!("cannot resolve {dev}: {e}"))
}

// ---------------------------------------------------------------------------
// free space
// ---------------------------------------------------------------------------

/// Free bytes from one `df -Pk <dir>` table. POSIX `-P` guarantees the
/// one-line-per-filesystem layout that makes this parseable at all.
pub fn parse_df_avail_kb(text: &str) -> Option<u64> {
    let line = text.lines().nth(1)?;
    line.split_whitespace().nth(3)?.parse::<u64>().ok()
}

async fn free_bytes(dir: &Path) -> Option<u64> {
    let out = tokio::process::Command::new("df")
        .arg("-Pk")
        .arg(dir)
        .output()
        .await
        .ok()?;
    parse_df_avail_kb(&String::from_utf8_lossy(&out.stdout)).map(|kb| kb * 1024)
}

// ---------------------------------------------------------------------------
// sparse write
// ---------------------------------------------------------------------------

/// Write one chunk, turning an all-zero chunk into a filesystem hole.
///
/// A root payload is a whole 5 GiB partition image whose tail is mostly
/// zeroes. Writing those zeroes would need 5 GiB of `/data` on a box whose
/// `/data` is 4 GiB at the factory. Hashing still covers every byte
/// including the holes, so this cannot weaken verification — it only changes
/// how many blocks the same content occupies.
///
/// Async (`tokio::fs`) rather than `std::fs` on purpose: a 5 GiB staging
/// write on a runtime worker thread would stall every other gateway task for
/// the duration.
async fn write_sparse_chunk(f: &mut tokio::fs::File, chunk: &[u8]) -> std::io::Result<()> {
    if chunk.iter().all(|b| *b == 0) {
        f.seek(SeekFrom::Current(chunk.len() as i64)).await?;
        Ok(())
    } else {
        f.write_all(chunk).await
    }
}

// ---------------------------------------------------------------------------
// staging
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Fetched {
    bytes: Vec<u8>,
}

async fn fetch_small(source: &Source, name: &str, max: u64) -> Result<Fetched, StageError> {
    match source {
        Source::Http(base) => {
            let url = format!("{base}/{name}");
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|e| StageError::Network(e.to_string()))?;
            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| StageError::Network(format!("{name}: {e}")))?;
            if !resp.status().is_success() {
                // A 4xx on the manifest or its signature is not a transport
                // hiccup — the release genuinely does not have one, and a
                // retry loop against an unsigned release is exactly the
                // wrong behaviour. 5xx and connection failures stay
                // Network, where retrying later is sensible.
                return Err(if resp.status().is_client_error() {
                    StageError::Rejected(format!(
                        "{name} is missing from the release ({}) — an update without a \
                         signed manifest is never installed",
                        resp.status()
                    ))
                } else {
                    StageError::Network(format!("{name}: server answered {}", resp.status()))
                });
            }
            if let Some(len) = resp.content_length() {
                if len > max {
                    return Err(StageError::Rejected(format!(
                        "{name} is {len} bytes, over the {max}-byte ceiling"
                    )));
                }
            }
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| StageError::Network(format!("{name}: {e}")))?;
            if bytes.len() as u64 > max {
                return Err(StageError::Rejected(format!("{name} is over its size ceiling")));
            }
            Ok(Fetched {
                bytes: bytes.to_vec(),
            })
        }
        Source::Dir(dir) => {
            let path = dir.join(name);
            let meta = tokio::fs::metadata(&path).await.map_err(|e| {
                // Same reasoning as the 4xx branch above: an offline release
                // that simply has no signature file is refused, not retried.
                if e.kind() == std::io::ErrorKind::NotFound {
                    StageError::Rejected(format!(
                        "{name} is missing from the release — an update without a signed \
                         manifest is never installed"
                    ))
                } else {
                    StageError::Network(format!("{name}: {e}"))
                }
            })?;
            if meta.len() > max {
                return Err(StageError::Rejected(format!("{name} is over its size ceiling")));
            }
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| StageError::Io(format!("{name}: {e}")))?;
            Ok(Fetched { bytes })
        }
    }
}

/// Stream one payload file to `dest`, hashing as it lands, and verify the
/// digest before returning. A mismatch removes the file: a half-verified
/// payload must never survive in the staging directory where sysupdate
/// could find it.
async fn fetch_payload(
    source: &Source,
    entry: &ManifestEntry,
    dest: &Path,
    max: u64,
) -> Result<u64, StageError> {
    let outcome = fetch_payload_inner(source, entry, dest, max).await;
    if outcome.is_err() {
        // A payload that failed for any reason — transport, ceiling,
        // checksum — must not be left where sysupdate could find it.
        let _ = tokio::fs::remove_file(dest).await;
    }
    outcome
}

async fn fetch_payload_inner(
    source: &Source,
    entry: &ManifestEntry,
    dest: &Path,
    max: u64,
) -> Result<u64, StageError> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| StageError::Io(format!("cannot create {}: {e}", dest.display())))?;
    let mut total: u64 = 0;

    macro_rules! absorb {
        ($chunk:expr) => {{
            let chunk: &[u8] = $chunk;
            total += chunk.len() as u64;
            if total > max {
                return Err(StageError::Rejected(format!(
                    "{} exceeded its {max}-byte ceiling mid-transfer",
                    entry.name
                )));
            }
            hasher.update(chunk);
            write_sparse_chunk(&mut file, chunk)
                .await
                .map_err(|e| StageError::Io(format!("writing {}: {e}", dest.display())))?;
        }};
    }

    match source {
        Source::Http(base) => {
            let url = format!("{base}/{}", entry.name);
            let client = reqwest::Client::builder()
                // No overall timeout: a multi-gigabyte payload on a slow
                // link is not a stuck request. The connect timeout still
                // bounds a dead server.
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| StageError::Network(e.to_string()))?;
            let mut resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| StageError::Network(format!("{}: {e}", entry.name)))?;
            if !resp.status().is_success() {
                return Err(StageError::Network(format!(
                    "{}: server answered {}",
                    entry.name,
                    resp.status()
                )));
            }
            if let (Some(len), Some(free)) = (
                resp.content_length(),
                free_bytes(dest.parent().unwrap_or(Path::new("."))).await,
            ) {
                // Advisory only. Sparse staging routinely stores far fewer
                // blocks than the payload's apparent size, so refusing here
                // would reject installs that fit comfortably; an actual
                // ENOSPC is still reported honestly by the write below.
                if len > free {
                    warn!(
                        "[os_update] {} is {len} bytes and the staging filesystem has {free} free \
                         — relying on sparse staging to fit",
                        entry.name
                    );
                }
            }
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| StageError::Network(format!("{}: {e}", entry.name)))?
            {
                absorb!(&chunk);
            }
        }
        Source::Dir(dir) => {
            use tokio::io::AsyncReadExt;
            let path = dir.join(&entry.name);
            let mut src = tokio::fs::File::open(&path)
                .await
                .map_err(|e| StageError::Network(format!("{}: {e}", entry.name)))?;
            let mut buf = vec![0u8; CHUNK_BYTES];
            loop {
                let n = src
                    .read(&mut buf)
                    .await
                    .map_err(|e| StageError::Io(format!("{}: {e}", entry.name)))?;
                if n == 0 {
                    break;
                }
                absorb!(&buf[..n]);
            }
        }
    }

    // A trailing hole is only materialised by set_len: seeking past the end
    // of a file does not extend it.
    file.set_len(total)
        .await
        .map_err(|e| StageError::Io(format!("finalising {}: {e}", dest.display())))?;
    file.sync_all()
        .await
        .map_err(|e| StageError::Io(format!("syncing {}: {e}", dest.display())))?;

    let got = format!("{:x}", hasher.finalize());
    if got != entry.sha256 {
        return Err(StageError::Rejected(format!(
            "{} failed its checksum (expected {}, got {got})",
            entry.name, entry.sha256
        )));
    }
    Ok(total)
}

/// Fetch, verify and parse a release's signed manifest into the matched
/// (root, uki) pair for this machine's architecture — the network+trust
/// prefix shared by [`check_update_with`] (stops here: cheap, no payload
/// download) and [`stage_update_with`] (goes on to download and bind the
/// payloads). One place decides "is this manifest legitimate and does it
/// name a usable release", so the two callers can never drift on what
/// counts as verified.
async fn fetch_verified_release(source: &Source) -> Result<ReleaseFiles, StageError> {
    let manifest = fetch_small(source, MANIFEST_NAME, MAX_MANIFEST_BYTES).await?;
    let signature = fetch_small(source, SIGNATURE_NAME, MAX_SIGNATURE_BYTES).await?;
    let sig_text = String::from_utf8_lossy(&signature.bytes).into_owned();
    crate::updater::verify_minisign_signature_with_pubkey(
        &manifest.bytes,
        &sig_text,
        OS_IMAGE_UPDATE_PUBKEY,
    )
    .map_err(StageError::Rejected)?;
    info!("[os_update] release manifest signature verified against the pinned OS key");

    let manifest_text = String::from_utf8_lossy(&manifest.bytes).into_owned();
    let entries = parse_manifest(&manifest_text).map_err(StageError::Rejected)?;
    let arch = machine_arch();
    select_release_files(&entries, arch).map_err(StageError::Rejected)
}

/// What a cheap "check for updates" call found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckReport {
    pub current_version: String,
    pub latest_version: String,
    pub available: bool,
}

/// H3d §11.5 (item 1): read the REAL update source, not local staging.
///
/// Before this existed, `device.update_status` (`systemd-sysupdate list
/// --json=short`) only ever reflected the LOCAL staging directory — empty
/// until a `device.update_apply` call had actually downloaded something —
/// so pressing "check for updates" reported "nothing new" regardless of
/// what the configured source actually offered ("使用者按「檢查更新」永遠
/// 顯示最新版" in the design doc). This fetches the same two small files
/// [`stage_update_with`] verifies before ANY payload byte is downloaded
/// ([`fetch_verified_release`]) and compares versions — the cost is bytes
/// in the hundreds, not gigabytes, and the trust chain is byte-identical to
/// the one that gates an actual install.
///
/// Every failure mode mirrors [`stage_update_with`]'s and stays a distinct
/// [`StageError`] variant — never collapsed into a generic "no update"
/// result. A network failure while checking must never be reported as "you
/// are up to date"; that is exactly the silent-false-success shape this
/// whole module exists to refuse.
pub async fn check_update(home: &Path) -> Result<UpdateCheckReport, StageError> {
    let cfg = OsUpdateConfig::from_home(home);
    check_update_with(&cfg).await
}

pub async fn check_update_with(cfg: &OsUpdateConfig) -> Result<UpdateCheckReport, StageError> {
    if cfg.source_url.trim().is_empty() {
        return Err(StageError::NotConfigured);
    }
    let source = parse_source(&cfg.source_url).map_err(StageError::Rejected)?;
    let release = fetch_verified_release(&source).await?;

    // Honest fallback rather than a panic or a guessed version: an
    // off-image dev/test host (or a corrupted os-release) has no
    // IMAGE_VERSION at all, and "unknown" still lets the caller show "an
    // update is available" truthfully (unknown != release.version).
    let current = running_image_version().unwrap_or_else(|| "unknown".to_string());
    let available = current != release.version;
    Ok(UpdateCheckReport {
        current_version: current,
        latest_version: release.version,
        available,
    })
}

/// Download, verify and bind a release, leaving the staging directory
/// holding exactly one version that `systemd-sysupdate update` can install.
///
/// See the module doc for the ordered gate list. Every early return leaves
/// the machine exactly as it was.
pub async fn stage_update(home: &Path) -> Result<StageReport, StageError> {
    let cfg = OsUpdateConfig::from_home(home);
    stage_update_with(home, &cfg).await
}

pub async fn stage_update_with(
    home: &Path,
    cfg: &OsUpdateConfig,
) -> Result<StageReport, StageError> {
    if cfg.source_url.trim().is_empty() {
        return Err(StageError::NotConfigured);
    }
    let source = parse_source(&cfg.source_url).map_err(StageError::Rejected)?;
    let staging = cfg.staging_dir(home);

    // ---- 1. manifest + signature, verified before anything is downloaded
    let release = fetch_verified_release(&source).await?;

    // ---- 2. already running it? (before spending 5 GiB of transfer)
    if let Some(running) = running_image_version() {
        if running == release.version {
            return Err(StageError::UpToDate(running));
        }
    }

    // ---- 3. destination slot, resolved from the live GPT
    let current_root = current_root_device().map_err(StageError::Io)?;
    let slots = read_slot_entries(Path::new(BY_PARTLABEL_DIR)).map_err(StageError::Io)?;
    let destination = pick_destination_slot(&slots, &current_root).map_err(StageError::Io)?;
    let dest_partuuid =
        partuuid_of(Path::new(BY_PARTUUID_DIR), &destination.device).map_err(StageError::Io)?;
    info!(
        "[os_update] staging {} for slot {} (label {}, PARTUUID {dest_partuuid})",
        release.version,
        destination.device.display(),
        destination.label
    );

    // ---- 4. somewhere to land
    //
    // Everything lands in `.incoming/` first and is only renamed into the
    // staging directory itself after it has been verified AND bound to a
    // slot. sysupdate lists the staging directory and does not descend, so a
    // download interrupted by a power cut is invisible to it — which is what
    // makes an interrupted update a non-event rather than a half-installed
    // one. The leftovers of such a run are cleared here rather than left to
    // accumulate on a small /data.
    let incoming = staging.join(".incoming");
    if incoming.exists() {
        if let Err(e) = std::fs::remove_dir_all(&incoming) {
            warn!("[os_update] could not clear {}: {e}", incoming.display());
        }
    }
    std::fs::create_dir_all(&incoming)
        .map_err(|e| StageError::Io(format!("cannot create {}: {e}", incoming.display())))?;
    if let Some(free) = free_bytes(&incoming).await {
        info!(
            "[os_update] staging into {} ({free} bytes free)",
            incoming.display()
        );
    }

    // ---- 5. payloads, hashed as they land
    let root_tmp = incoming.join(&release.root.name);
    let uki_tmp = incoming.join(&release.uki.name);
    let mut bytes_downloaded = fetch_payload(&source, &release.root, &root_tmp, MAX_ROOT_BYTES)
        .await
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&uki_tmp);
        })?;
    bytes_downloaded += fetch_payload(&source, &release.uki, &uki_tmp, MAX_UKI_BYTES)
        .await
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&root_tmp);
        })?;
    info!("[os_update] both payload files verified against the signed manifest");

    // ---- 6. bind the UKI to the destination slot
    let mut uki_bytes = std::fs::read(&uki_tmp)
        .map_err(|e| StageError::Io(format!("cannot re-read the staged kernel image: {e}")))?;
    let template_partuuid = uki_patch::rewrite_root_partuuid(&mut uki_bytes, &dest_partuuid)
        .map_err(StageError::Rejected)?;
    std::fs::write(&uki_tmp, &uki_bytes)
        .map_err(|e| StageError::Io(format!("cannot write the bound kernel image: {e}")))?;
    info!(
        "[os_update] kernel image bound to {dest_partuuid} (payload was built against {template_partuuid})"
    );

    // ---- 7. publish: clear stale versions, then move into place
    clear_stale_payloads(&staging, &release);
    let root_final = staging.join(&release.root.name);
    let uki_final = staging.join(&release.uki.name);
    std::fs::rename(&root_tmp, &root_final)
        .map_err(|e| StageError::Io(format!("cannot publish the root payload: {e}")))?;
    std::fs::rename(&uki_tmp, &uki_final)
        .map_err(|e| StageError::Io(format!("cannot publish the kernel image: {e}")))?;
    let _ = std::fs::remove_dir(&incoming);

    Ok(StageReport {
        version: release.version,
        arch: release.arch,
        root_payload: root_final,
        uki_payload: uki_final,
        destination_partuuid: dest_partuuid,
        template_partuuid,
        bytes_downloaded,
    })
}

/// After sysupdate has run: confirm it wrote the slot we bound the kernel to.
///
/// This is the one thing that could still go wrong silently. We choose the
/// destination independently of sysupdate (it has no way to tell us), and
/// the two choices agree because with exactly two slots and
/// `ProtectVersion=%A` there is only one candidate. But if `IMAGE_VERSION`
/// and the running slot's label ever drift apart, `ProtectVersion=` stops
/// protecting anything and sysupdate could pick the *running* slot — and
/// then the ESP would hold a kernel pointing at a root that belongs to the
/// other version. On a headless box that is the expensive failure. So it is
/// checked from the live GPT rather than assumed, and a mismatch is reported
/// loudly instead of being discovered at the next boot.
/// Seconds to wait for udev to publish the relabelled slot before calling it
/// a mismatch. sysupdate rewrites the GPT and udev then republishes
/// `/dev/disk/by-partlabel/`; measured on the appliance the symlink appeared
/// within the same second the check first ran, which was enough to make an
/// unwaited check report a perfectly good install as a mismatch.
const SLOT_CONFIRM_TIMEOUT_SECS: u64 = 30;

pub async fn confirm_installed_slot(report: &StageReport) -> Result<(), String> {
    let want_label = format!("{SLOT_LABEL_PREFIX}{}", report.version);
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(SLOT_CONFIRM_TIMEOUT_SECS);
    let mut last = format!("no partition is labelled {want_label} after the install");

    loop {
        // A label that resolves to the WRONG device is fatal immediately —
        // waiting cannot turn it right, and this is the failure that must be
        // loud. Only "not published yet" is worth retrying.
        match read_slot_entries(Path::new(BY_PARTLABEL_DIR)) {
            Ok(slots) => match slots.iter().find(|s| s.label == want_label) {
                Some(entry) => match partuuid_of(Path::new(BY_PARTUUID_DIR), &entry.device) {
                    Ok(got) if got == report.destination_partuuid => return Ok(()),
                    Ok(got) => {
                        return Err(format!(
                            "the new version was written to {} (PARTUUID {got}) but the kernel \
                             image was bound to {} — do NOT reboot; the two would not match",
                            entry.device.display(),
                            report.destination_partuuid
                        ));
                    }
                    // by-partuuid is published by the same udev pass as
                    // by-partlabel, so this too can simply be early.
                    Err(e) => last = e,
                },
                None => {
                    last = format!("no partition is labelled {want_label} after the install")
                }
            },
            Err(e) => last = e,
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("{last} (waited {SLOT_CONFIRM_TIMEOUT_SECS}s for udev)"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Delete the staged payload once it has been installed.
///
/// The partition label is the version ledger from here on, so keeping ~4 GiB
/// of already-installed payload on a small `/data` buys nothing and would
/// also make sysupdate keep offering a version that is already in a slot.
/// Best-effort: a failure here is logged, never turned into a failed update.
pub fn cleanup_staged(report: &StageReport) {
    for path in [&report.root_payload, &report.uki_payload] {
        if let Err(e) = std::fs::remove_file(path) {
            warn!("[os_update] could not remove staged {}: {e}", path.display());
        }
    }
}

/// Remove payload files of *other* versions left in the staging directory.
///
/// sysupdate offers every version it finds; leaving an older half-superseded
/// release beside the new one turns "install the update" into a version
/// choice nobody made. Only files matching our own naming shape are touched
/// — never an unrelated file an operator put there.
fn clear_stale_payloads(staging: &Path, keep: &ReleaseFiles) {
    let Ok(rd) = std::fs::read_dir(staging) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == keep.root.name || name == keep.uki.name {
            continue;
        }
        let is_ours = name
            .strip_prefix(SLOT_LABEL_PREFIX)
            .is_some_and(|rest| rest.ends_with(".raw") || rest.ends_with(".efi"));
        if is_ours {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                warn!("[os_update] could not remove stale payload {name}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: &str, name: &str) -> ManifestEntry {
        ManifestEntry {
            sha256: sha.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn manifest_parses_shasum_and_binary_marker_rows() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let text = format!(
            "# a comment\n{a}  duduclaw-os_0.2.0.root-arm64.raw\n{b} *duduclaw-os_0.2.0.efi\n\n"
        );
        let got = parse_manifest(&text).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "duduclaw-os_0.2.0.root-arm64.raw");
        assert_eq!(got[1].name, "duduclaw-os_0.2.0.efi");
        assert_eq!(got[1].sha256, b);
    }

    #[test]
    fn manifest_refuses_paths_and_bad_digests() {
        let a = "a".repeat(64);
        for bad in [
            format!("{a}  ../../etc/passwd"),
            format!("{a}  /etc/passwd"),
            format!("{a}  sub/dir/file.raw"),
            format!("{a}  .hidden"),
            "deadbeef  duduclaw-os_0.2.0.efi".to_string(),
            format!("{}  duduclaw-os_0.2.0.efi", "z".repeat(64)),
        ] {
            assert!(parse_manifest(&bad).is_err(), "must reject {bad:?}");
        }
        assert!(parse_manifest("").is_err(), "an empty manifest is not a release");
    }

    #[test]
    fn release_selection_pairs_root_and_uki_for_this_arch() {
        let entries = vec![
            entry(&"a".repeat(64), "duduclaw-os_0.2.0.root-arm64.raw"),
            entry(&"b".repeat(64), "duduclaw-os_0.2.0.root-x86-64.raw"),
            entry(&"c".repeat(64), "duduclaw-os_0.2.0.efi"),
            entry(&"d".repeat(64), "RELEASE_NOTES.txt"),
        ];
        let got = select_release_files(&entries, "arm64").unwrap();
        assert_eq!(got.version, "0.2.0");
        assert_eq!(got.root.name, "duduclaw-os_0.2.0.root-arm64.raw");
        assert_eq!(got.uki.name, "duduclaw-os_0.2.0.efi");

        let x86 = select_release_files(&entries, "x86-64").unwrap();
        assert_eq!(x86.root.name, "duduclaw-os_0.2.0.root-x86-64.raw");
    }

    #[test]
    fn release_selection_refuses_mixed_versions_and_ambiguity() {
        let mixed = vec![
            entry(&"a".repeat(64), "duduclaw-os_0.2.0.root-arm64.raw"),
            entry(&"c".repeat(64), "duduclaw-os_0.3.0.efi"),
        ];
        assert!(select_release_files(&mixed, "arm64").is_err());

        let two_roots = vec![
            entry(&"a".repeat(64), "duduclaw-os_0.2.0.root-arm64.raw"),
            entry(&"b".repeat(64), "duduclaw-os_0.3.0.root-arm64.raw"),
            entry(&"c".repeat(64), "duduclaw-os_0.2.0.efi"),
        ];
        assert!(select_release_files(&two_roots, "arm64").is_err());

        let no_uki = vec![entry(&"a".repeat(64), "duduclaw-os_0.2.0.root-arm64.raw")];
        assert!(select_release_files(&no_uki, "arm64").is_err());

        let wrong_arch = vec![
            entry(&"a".repeat(64), "duduclaw-os_0.2.0.root-x86-64.raw"),
            entry(&"c".repeat(64), "duduclaw-os_0.2.0.efi"),
        ];
        assert!(select_release_files(&wrong_arch, "arm64").is_err());
    }

    #[test]
    fn source_accepts_https_everywhere_and_http_only_on_a_private_host() {
        assert!(matches!(
            parse_source("https://updates.example.com/os/"),
            Ok(Source::Http(_))
        ));
        for local in [
            "http://127.0.0.1:8099/os",
            "http://localhost:8099/os",
            "http://10.0.2.2:8099/os",
            "http://192.168.1.9/os",
            "http://[fe80::1]/os",
        ] {
            assert!(parse_source(local).is_ok(), "must accept {local}");
        }
        for public in ["http://updates.example.com/os", "http://8.8.8.8/os"] {
            assert!(parse_source(public).is_err(), "must refuse {public}");
        }
    }

    #[test]
    fn source_refuses_junk() {
        for bad in [
            "",
            "   ",
            "ftp://example.com/os",
            "updates.example.com/os",
            "https://example.com/../../etc",
            "file://relative/path",
            "https://exa mple.com/os",
        ] {
            assert!(parse_source(bad).is_err(), "must refuse {bad:?}");
        }
        assert_eq!(
            parse_source("file:///mnt/usb/release/").unwrap(),
            Source::Dir(PathBuf::from("/mnt/usb/release/"))
        );
    }

    #[test]
    fn destination_slot_is_the_one_not_currently_mounted() {
        let entries = vec![
            SlotEntry {
                label: "duduclaw-os_0.1.0".into(),
                device: PathBuf::from("/dev/vda2"),
            },
            SlotEntry {
                label: "_empty".into(),
                device: PathBuf::from("/dev/vda3"),
            },
            SlotEntry {
                label: "duduclaw-data".into(),
                device: PathBuf::from("/dev/vda4"),
            },
        ];
        let got = pick_destination_slot(&entries, Path::new("/dev/vda2")).unwrap();
        assert_eq!(got.device, PathBuf::from("/dev/vda3"));

        // After one update both slots carry versions; the running one is
        // still excluded, with no version comparison involved.
        let after = vec![
            SlotEntry {
                label: "duduclaw-os_0.1.0".into(),
                device: PathBuf::from("/dev/vda2"),
            },
            SlotEntry {
                label: "duduclaw-os_0.2.0".into(),
                device: PathBuf::from("/dev/vda3"),
            },
        ];
        let got = pick_destination_slot(&after, Path::new("/dev/vda3")).unwrap();
        assert_eq!(got.device, PathBuf::from("/dev/vda2"));
    }

    #[test]
    fn destination_slot_fails_closed_on_an_unexpected_layout() {
        let one = vec![SlotEntry {
            label: "duduclaw-os_0.1.0".into(),
            device: PathBuf::from("/dev/vda2"),
        }];
        assert!(pick_destination_slot(&one, Path::new("/dev/vda2")).is_err());

        let three = vec![
            SlotEntry {
                label: "duduclaw-os_0.1.0".into(),
                device: PathBuf::from("/dev/vda2"),
            },
            SlotEntry {
                label: "duduclaw-os_0.2.0".into(),
                device: PathBuf::from("/dev/vda3"),
            },
            SlotEntry {
                label: "_empty".into(),
                device: PathBuf::from("/dev/vda5"),
            },
        ];
        assert!(pick_destination_slot(&three, Path::new("/dev/vda2")).is_err());

        // Running from something that is not a slot at all (a rescue USB,
        // a dev box) must never pick a slot to overwrite.
        let two = vec![
            SlotEntry {
                label: "duduclaw-os_0.1.0".into(),
                device: PathBuf::from("/dev/vda2"),
            },
            SlotEntry {
                label: "_empty".into(),
                device: PathBuf::from("/dev/vda3"),
            },
        ];
        assert!(pick_destination_slot(&two, Path::new("/dev/sdb1")).is_err());
    }

    #[test]
    fn slot_label_shape_is_anchored() {
        assert!(is_root_slot_label("_empty"));
        assert!(is_root_slot_label("duduclaw-os_0.1.0"));
        assert!(!is_root_slot_label("duduclaw-data"));
        assert!(!is_root_slot_label("xduduclaw-os_0.1.0"));
        assert!(!is_root_slot_label("duduclaw-os_"));
        assert!(!is_root_slot_label("duduclaw-os_../../evil"));
        assert!(!is_root_slot_label("_empty2"));
    }

    #[test]
    fn root_device_and_image_version_parsers() {
        let mounts = "sysfs /sys sysfs rw 0 0\n/dev/vda2 / ext4 rw,relatime 0 0\n\
                      /dev/vda1 /boot vfat rw 0 0\n";
        assert_eq!(parse_root_device(mounts).as_deref(), Some("/dev/vda2"));
        assert_eq!(parse_root_device("sysfs /sys sysfs rw 0 0\n"), None);

        let osrel = "ID=duduclaw-os\nIMAGE_ID=duduclaw-os\nIMAGE_VERSION=\"0.2.0\"\n";
        assert_eq!(parse_image_version(osrel).as_deref(), Some("0.2.0"));
        assert_eq!(parse_image_version("ID=debian\n"), None);
    }

    #[test]
    fn df_parser_reads_the_available_column() {
        let text = "Filesystem 1024-blocks  Used Available Capacity Mounted on\n\
                    /dev/vda4     4061608 12345   3800000       1% /data\n";
        assert_eq!(parse_df_avail_kb(text), Some(3_800_000));
        assert_eq!(parse_df_avail_kb("header only\n"), None);
    }

    #[test]
    fn config_defaults_to_no_source_and_home_relative_staging() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = OsUpdateConfig::from_home(dir.path());
        assert!(cfg.source_url.is_empty());
        assert_eq!(cfg.staging_dir(dir.path()), dir.path().join("updates"));

        std::fs::write(
            dir.path().join("config.toml"),
            "[os_update]\nsource_url = \"https://x.example/os/\"\nstaging_dir = \"/data/u\"\n",
        )
        .unwrap();
        let cfg = OsUpdateConfig::from_home(dir.path());
        assert_eq!(cfg.source_url, "https://x.example/os/");
        assert_eq!(cfg.staging_dir(dir.path()), PathBuf::from("/data/u"));
    }

    #[test]
    fn malformed_config_falls_back_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[os_update\nnot toml").unwrap();
        assert!(OsUpdateConfig::from_home(dir.path()).source_url.is_empty());
    }

    #[tokio::test]
    async fn a_release_with_no_signature_is_rejected_not_retried() {
        // Classification matters here, not just the refusal: `Network` means
        // "try again later", and a retry loop against an unsigned release is
        // the wrong behaviour to build in.
        let release = tempfile::tempdir().unwrap();
        std::fs::write(release.path().join(MANIFEST_NAME), b"whatever").unwrap();
        let src = Source::Dir(release.path().to_path_buf());
        let err = fetch_small(&src, SIGNATURE_NAME, MAX_SIGNATURE_BYTES)
            .await
            .unwrap_err();
        assert!(
            matches!(err, StageError::Rejected(_)),
            "a missing signature must be Rejected, got {err:?}"
        );
    }

    #[tokio::test]
    async fn unconfigured_source_never_touches_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let err = stage_update(dir.path()).await.unwrap_err();
        assert_eq!(err, StageError::NotConfigured);
        assert!(!dir.path().join("updates").exists());
    }

    #[tokio::test]
    async fn sparse_write_produces_the_same_bytes_as_a_dense_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.raw");
        let mut f = tokio::fs::File::create(&path).await.unwrap();
        let zeros = vec![0u8; 8192];
        let data = vec![7u8; 512];
        write_sparse_chunk(&mut f, &data).await.unwrap();
        write_sparse_chunk(&mut f, &zeros).await.unwrap();
        write_sparse_chunk(&mut f, &data).await.unwrap();
        write_sparse_chunk(&mut f, &zeros).await.unwrap();
        let total = (data.len() * 2 + zeros.len() * 2) as u64;
        f.set_len(total).await.unwrap();
        drop(f);

        let got = std::fs::read(&path).unwrap();
        let mut want = Vec::new();
        want.extend_from_slice(&data);
        want.extend_from_slice(&zeros);
        want.extend_from_slice(&data);
        want.extend_from_slice(&zeros);
        assert_eq!(got, want, "holes must read back as zeroes");
        assert_eq!(got.len() as u64, total);
    }

    #[tokio::test]
    async fn file_source_end_to_end_verifies_and_rejects_tampering() {
        use sha2::{Digest, Sha256};
        let release = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();

        let payload = vec![9u8; 1024];
        let name = "duduclaw-os_9.9.9.root-arm64.raw";
        std::fs::write(release.path().join(name), &payload).unwrap();
        let good = ManifestEntry {
            sha256: format!("{:x}", Sha256::digest(&payload)),
            name: name.to_string(),
        };
        let src = Source::Dir(release.path().to_path_buf());

        let dest = staging.path().join(name);
        let n = fetch_payload(&src, &good, &dest, MAX_ROOT_BYTES).await.unwrap();
        assert_eq!(n, 1024);
        assert_eq!(std::fs::read(&dest).unwrap(), payload);

        let bad = ManifestEntry {
            sha256: "f".repeat(64),
            name: name.to_string(),
        };
        let err = fetch_payload(&src, &bad, &dest, MAX_ROOT_BYTES)
            .await
            .unwrap_err();
        assert!(matches!(err, StageError::Rejected(_)));
        assert!(
            !dest.exists(),
            "a payload that failed its checksum must not survive in the staging dir"
        );
    }

    #[test]
    fn stale_payloads_of_other_versions_are_cleared_but_foreign_files_are_not() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "duduclaw-os_0.1.0.root-arm64.raw",
            "duduclaw-os_0.1.0.efi",
            "duduclaw-os_0.2.0.root-arm64.raw",
            "duduclaw-os_0.2.0.efi",
            "operator-notes.txt",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let keep = ReleaseFiles {
            version: "0.2.0".into(),
            arch: "arm64".into(),
            root: entry(&"a".repeat(64), "duduclaw-os_0.2.0.root-arm64.raw"),
            uki: entry(&"b".repeat(64), "duduclaw-os_0.2.0.efi"),
        };
        clear_stale_payloads(dir.path(), &keep);

        assert!(dir.path().join("duduclaw-os_0.2.0.root-arm64.raw").exists());
        assert!(dir.path().join("duduclaw-os_0.2.0.efi").exists());
        assert!(!dir.path().join("duduclaw-os_0.1.0.root-arm64.raw").exists());
        assert!(!dir.path().join("duduclaw-os_0.1.0.efi").exists());
        assert!(
            dir.path().join("operator-notes.txt").exists(),
            "only our own naming shape may be removed"
        );
    }

    #[test]
    fn the_os_key_is_not_the_app_key() {
        assert_ne!(
            OS_IMAGE_UPDATE_PUBKEY,
            crate::updater::UPDATE_PUBKEY,
            "the OS image channel must pin its own key — see the module doc"
        );
        assert!(minisign_verify::PublicKey::from_base64(OS_IMAGE_UPDATE_PUBKEY).is_ok());
    }

    // --- check_update_with (H3d §11.5 item 1) -----------------------------

    #[tokio::test]
    async fn check_update_unconfigured_source_never_touches_the_network() {
        let cfg = OsUpdateConfig::default();
        let err = check_update_with(&cfg).await.unwrap_err();
        assert_eq!(err, StageError::NotConfigured);
    }

    #[tokio::test]
    async fn check_update_reports_a_missing_manifest_honestly_not_as_up_to_date() {
        // A `file://` source pointed at a directory with no manifest at
        // all — this must surface as `Rejected`, never as a fabricated
        // "you are up to date" answer. Mirrors
        // `a_release_with_no_signature_is_rejected_not_retried` but through
        // the check path instead of the stage path.
        let dir = tempfile::tempdir().unwrap();
        let cfg = OsUpdateConfig {
            source_url: format!("file://{}", dir.path().display()),
            staging_dir: None,
        };
        let err = check_update_with(&cfg).await.unwrap_err();
        assert!(
            matches!(err, StageError::Rejected(_)),
            "a missing manifest must be Rejected, not silently reported up-to-date: {err:?}"
        );
    }

    #[test]
    fn error_copy_is_user_facing_and_leaks_no_internals() {
        for e in [
            StageError::NotConfigured,
            StageError::UpToDate("0.1.0".into()),
            StageError::Network("connection refused".into()),
        ] {
            let msg = e.user_message();
            assert!(!msg.is_empty());
            for internal in ["sysupdate", "PARTUUID", "minisign", "/dev/vda"] {
                assert!(
                    !msg.contains(internal),
                    "{msg:?} leaks the internal term {internal}"
                );
            }
        }
    }
}
