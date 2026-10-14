//! Rollback floor (`--min-rollback-version <version>`): refuse to boot an
//! OS entry older than the first release whose gateway runs unprivileged.
//!
//! Why it lives here and checks the *target*, not the caller: on DuDuClaw
//! OS an AI employee with a shell runs as the gateway's uid, so it can read
//! the gateway's keys and forge any request the gateway would make. Only a
//! check on the root side that does not depend on who asks can stop
//! `update_rollback` from booting the previous slot, which after the first
//! non-root upgrade is an image whose gateway (and so the agent) still ran
//! as root.
//!
//! Where the target's version comes from: the `.osrel` section embedded in
//! the entry's unified kernel image on the ESP (the image's own
//! `os-release`, `IMAGE_VERSION=`), cross-checked against the version in
//! the entry's filename (`duduclaw-os_<version>[+counter].efi`, the name
//! `systemd-sysupdate` gives it). The ESP is writable by root only, and the
//! UKI is the file the firmware actually boots, so neither can be chosen by
//! the caller. A missing or unreadable `.osrel`, no `IMAGE_VERSION`, a
//! version that does not parse, or a filename that disagrees with the
//! embedded version all count as "cannot determine": refused.
//!
//! Every ESP entry other than the running one is checked, exhausted ones
//! included (sd-boot still boots an exhausted entry when nothing else
//! works); when the running entry cannot be identified, every entry is.
//!
//! Only Type #2 entries (UKIs under `EFI/Linux`) can be verified this way.
//! A Type #1 entry (`loader/entries/*.conf`) names a kernel and initrd
//! whose version is not bound to anything sysd can check as strictly, so
//! with a floor set, any `.conf` entry on the ESP refuses the rollback
//! ([`type1_entries_present`]). DuDuClaw OS ships Type #2 only.

use std::cmp::Ordering;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

/// A parsed OS version: up to four numeric components, and whether a
/// pre-release suffix (`-rc1`) follows. `0.4.0-rc1` < `0.4.0` < `0.4.1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsVersion {
    nums: [u64; 4],
    pre: bool,
}

impl OsVersion {
    /// `MAJOR[.MINOR[.PATCH[.BUILD]]]` with an optional `-pre` or `+meta`
    /// suffix (metadata is ignored, a pre-release ranks below the release).
    /// Anything else is `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().trim_start_matches(['v', 'V']);
        if raw.is_empty() || raw.len() > 32 {
            return None;
        }
        let (core, pre) = match raw.find(['-', '+']) {
            Some(i) => {
                let rest = &raw[i + 1..];
                if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
                    return None;
                }
                (&raw[..i], raw.as_bytes()[i] == b'-')
            }
            None => (raw, false),
        };
        let parts: Vec<&str> = core.split('.').collect();
        if parts.is_empty() || parts.len() > 4 {
            return None;
        }
        let mut nums = [0u64; 4];
        for (slot, part) in nums.iter_mut().zip(&parts) {
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *slot = part.parse().ok()?;
        }
        Some(Self { nums, pre })
    }
}

impl Ord for OsVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nums.cmp(&other.nums).then_with(|| other.pre.cmp(&self.pre))
    }
}

impl PartialOrd for OsVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The configured floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackFloor {
    /// No flag: behaviour unchanged (older OS images).
    Off,
    /// Refuse targets older than this.
    Min(OsVersion),
    /// The flag was given but did not parse: refuse every rollback.
    Invalid(String),
}

impl RollbackFloor {
    pub fn from_flag(flag: Option<&str>) -> Self {
        match flag {
            None => Self::Off,
            Some(raw) => match OsVersion::parse(raw) {
                Some(v) => Self::Min(v),
                None => Self::Invalid(raw.to_string()),
            },
        }
    }
}

static FLOOR: OnceLock<RollbackFloor> = OnceLock::new();

/// Set once by `main.rs` from `--min-rollback-version`.
pub fn set_floor(floor: RollbackFloor) {
    let _ = FLOOR.set(floor);
}

pub fn floor() -> &'static RollbackFloor {
    FLOOR.get().unwrap_or(&RollbackFloor::Off)
}

/// `IMAGE_VERSION=` from an os-release text.
pub fn image_version_from_osrel(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let v = line.trim().strip_prefix("IMAGE_VERSION=")?;
        let v = v.trim().trim_matches('"').trim_matches('\'');
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// The version in an entry filename: `duduclaw-os_<version>[+counter].efi`.
pub fn version_from_entry_name(name: &str) -> Option<String> {
    let stem = crate::dispatch::entry_stem(name)?;
    stem.strip_prefix("duduclaw-os_").filter(|v| !v.is_empty()).map(str::to_string)
}

const PE_HEADER_READ: usize = 64 * 1024;
const MAX_SECTIONS: usize = 96;
const MAX_OSREL_BYTES: u64 = 64 * 1024;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// File offset and length of the PE section `name` (at most 8 bytes), from
/// the first bytes of the image. `None` for anything that is not a PE
/// image with that section inside `header`.
pub fn pe_section(header: &[u8], name: &str) -> Option<(u64, u64)> {
    if header.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(header, 0x3c)? as usize;
    if header.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let sections = u16_at(header, coff + 2)? as usize;
    let optional = u16_at(header, coff + 16)? as usize;
    if sections > MAX_SECTIONS {
        return None;
    }
    let table = coff.checked_add(20)?.checked_add(optional)?;
    let mut want = [0u8; 8];
    let n = name.as_bytes();
    if n.len() > 8 {
        return None;
    }
    want[..n.len()].copy_from_slice(n);
    for i in 0..sections {
        let at = table.checked_add(i.checked_mul(40)?)?;
        let entry = header.get(at..at + 40)?;
        if entry[..8] != want {
            continue;
        }
        let virtual_size = u32_at(entry, 8)? as u64;
        let raw_size = u32_at(entry, 16)? as u64;
        let raw_ptr = u32_at(entry, 20)? as u64;
        let len = if virtual_size > 0 { virtual_size.min(raw_size) } else { raw_size };
        return Some((raw_ptr, len));
    }
    None
}

/// `IMAGE_VERSION` embedded in the UKI at `path` (its `.osrel` section).
pub fn uki_image_version(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut header = Vec::with_capacity(PE_HEADER_READ);
    (&mut file).take(PE_HEADER_READ as u64).read_to_end(&mut header).map_err(|e| e.to_string())?;
    let (offset, len) =
        pe_section(&header, ".osrel").ok_or_else(|| format!("{} has no readable .osrel section", path.display()))?;
    if len == 0 || len > MAX_OSREL_BYTES {
        return Err(format!("{}: .osrel section has an unusable size", path.display()));
    }
    file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut osrel = Vec::new();
    file.take(len).read_to_end(&mut osrel).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&osrel);
    image_version_from_osrel(&text).ok_or_else(|| format!("{}: .osrel carries no IMAGE_VERSION", path.display()))
}

/// The version of one ESP entry: the embedded one, which must agree with
/// the filename.
pub fn entry_version(dir: &Path, name: &str) -> Result<OsVersion, String> {
    let embedded = uki_image_version(&dir.join(name))?;
    let embedded_v = OsVersion::parse(&embedded).ok_or_else(|| format!("{name}: version {embedded:?} does not parse"))?;
    let named = version_from_entry_name(name).and_then(|v| OsVersion::parse(&v));
    if named.as_ref() != Some(&embedded_v) {
        return Err(format!("{name}: filename and embedded version {embedded:?} disagree"));
    }
    Ok(embedded_v)
}

/// Whether the ESP at `esp_root` holds any Type #1 boot entry
/// (`loader/entries/*.conf`). A missing directory is `Ok(false)`; a
/// directory that cannot be read is an error (the caller refuses).
pub fn type1_entries_present(esp_root: &Path) -> Result<bool, String> {
    let dir = esp_root.join("loader").join("entries");
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("cannot list {}: {e}", dir.display())),
    };
    for entry in rd {
        let entry = entry.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() > ".conf".len() && name.to_ascii_lowercase().ends_with(".conf") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The ESP root for an `EFI/Linux` entries directory.
pub fn esp_root_of(entries_dir: &Path) -> Option<&Path> {
    entries_dir.parent()?.parent()
}

/// The entries a rollback could end up booting: every entry but the
/// running one (all of them when the running one is unknown).
pub fn rollback_candidates<'a>(entries: &'a [String], running: Option<&str>) -> Vec<&'a String> {
    let running_stem = running.and_then(crate::dispatch::entry_stem);
    entries
        .iter()
        .filter(|e| running_stem.is_none() || crate::dispatch::entry_stem(e) != running_stem)
        .collect()
}

/// The decision (pure): with a floor, every candidate's version must be
/// known and at least the floor.
pub fn check_floor(floor: &RollbackFloor, candidates: &[(&str, Result<OsVersion, String>)]) -> Result<(), String> {
    let min = match floor {
        RollbackFloor::Off => return Ok(()),
        RollbackFloor::Invalid(raw) => {
            return Err(format!("--min-rollback-version {raw:?} is not a version; refusing every rollback"));
        }
        RollbackFloor::Min(min) => min,
    };
    for (name, version) in candidates {
        match version {
            Ok(v) if v >= min => {}
            Ok(_) => {
                return Err(format!(
                    "refusing to roll back: {name} is older than the minimum allowed version (its system ran \
                     the gateway with root privileges)"
                ));
            }
            Err(why) => return Err(format!("refusing to roll back: cannot determine the version of {name} ({why})")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> OsVersion {
        OsVersion::parse(s).unwrap()
    }

    #[test]
    fn versions_compare_numerically_with_prereleases_below_releases() {
        assert!(v("0.4.0") > v("0.3.9"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert_eq!(v("0.4"), v("0.4.0"));
        assert!(v("0.4.0-rc1") < v("0.4.0"));
        assert!(v("0.4.0-rc1") > v("0.3.0"));
        assert_eq!(v("0.4.0+build7"), v("0.4.0"));
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        for bad in ["", "abc", "1..2", "1.2.3.4.5", "1.2.x", "1.2.3-", "-1", "1.2.3 4", &"9".repeat(40)] {
            assert!(OsVersion::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn floor_flag_parsing_fails_closed() {
        assert_eq!(RollbackFloor::from_flag(None), RollbackFloor::Off);
        assert_eq!(RollbackFloor::from_flag(Some("0.4.0")), RollbackFloor::Min(v("0.4.0")));
        assert!(matches!(RollbackFloor::from_flag(Some("latest")), RollbackFloor::Invalid(_)));
    }

    #[test]
    fn osrel_and_entry_names_yield_versions() {
        let osrel = "NAME=\"DuDuClaw OS\"\nVERSION_ID=0.4\nIMAGE_VERSION=\"0.4.0\"\n";
        assert_eq!(image_version_from_osrel(osrel).as_deref(), Some("0.4.0"));
        assert_eq!(image_version_from_osrel("NAME=x\n"), None);
        assert_eq!(version_from_entry_name("duduclaw-os_0.4.0.efi").as_deref(), Some("0.4.0"));
        assert_eq!(version_from_entry_name("duduclaw-os_0.3.0+0-1.efi").as_deref(), Some("0.3.0"));
        assert_eq!(version_from_entry_name("other_0.4.0.efi"), None);
    }

    /// A minimal PE image: DOS header, PE signature, COFF header, no
    /// optional header, one section table, then the sections' data.
    fn fake_uki(sections: &[(&str, &[u8])]) -> Vec<u8> {
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
            out[at + 16..at + 20].copy_from_slice(&((data.len() as u32 + 511) & !511).to_le_bytes());
            out[at + 20..at + 24].copy_from_slice(&(ptr as u32).to_le_bytes());
            ptr += data.len();
        }
        for (_, data) in sections {
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn the_embedded_osrel_is_read_and_cross_checked_with_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let osrel = b"ID=duduclaw\nIMAGE_VERSION=0.3.0\n";
        std::fs::write(dir.path().join("duduclaw-os_0.3.0+0-1.efi"), fake_uki(&[(".linux", b"kernel"), (".osrel", osrel)]))
            .unwrap();
        assert_eq!(uki_image_version(&dir.path().join("duduclaw-os_0.3.0+0-1.efi")).unwrap(), "0.3.0");
        assert_eq!(entry_version(dir.path(), "duduclaw-os_0.3.0+0-1.efi").unwrap(), v("0.3.0"));

        // Renamed to look newer: the embedded version wins and they disagree.
        std::fs::write(dir.path().join("duduclaw-os_0.9.0.efi"), fake_uki(&[(".osrel", osrel)])).unwrap();
        assert!(entry_version(dir.path(), "duduclaw-os_0.9.0.efi").unwrap_err().contains("disagree"));
        // No .osrel, not a PE image, no IMAGE_VERSION: cannot determine.
        std::fs::write(dir.path().join("duduclaw-os_0.5.0.efi"), fake_uki(&[(".linux", b"k")])).unwrap();
        assert!(entry_version(dir.path(), "duduclaw-os_0.5.0.efi").is_err());
        std::fs::write(dir.path().join("duduclaw-os_0.6.0.efi"), b"not a pe image").unwrap();
        assert!(entry_version(dir.path(), "duduclaw-os_0.6.0.efi").is_err());
        std::fs::write(dir.path().join("duduclaw-os_0.7.0.efi"), fake_uki(&[(".osrel", b"ID=x\n")])).unwrap();
        assert!(entry_version(dir.path(), "duduclaw-os_0.7.0.efi").is_err());
        assert!(entry_version(dir.path(), "duduclaw-os_0.8.0.efi").is_err(), "missing file");
    }

    #[test]
    fn candidates_are_every_entry_but_the_running_one() {
        let entries: Vec<String> =
            ["duduclaw-os_0.4.0+2-1.efi", "duduclaw-os_0.3.0.efi", "duduclaw-os_0.2.0+0-1.efi"].map(String::from).to_vec();
        let c = rollback_candidates(&entries, Some("duduclaw-os_0.4.0.efi"));
        assert_eq!(c, vec![&entries[1], &entries[2]], "exhausted entries count too");
        assert_eq!(rollback_candidates(&entries, None).len(), 3, "unknown running entry: check all");
    }

    #[test]
    fn type1_entries_are_detected_and_unreadable_listings_fail() {
        let esp = tempfile::tempdir().unwrap();
        assert_eq!(type1_entries_present(esp.path()), Ok(false), "no loader/entries");
        let entries = esp.path().join("loader").join("entries");
        std::fs::create_dir_all(&entries).unwrap();
        std::fs::write(entries.join("README"), "x").unwrap();
        assert_eq!(type1_entries_present(esp.path()), Ok(false));
        std::fs::write(entries.join("old-rootful.CONF"), "linux /vmlinuz\n").unwrap();
        assert_eq!(type1_entries_present(esp.path()), Ok(true));
        // A file where the directory should be cannot be listed: refuse.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join("loader")).unwrap();
        std::fs::write(other.path().join("loader").join("entries"), "not a dir").unwrap();
        assert!(type1_entries_present(other.path()).is_err());
        let linux = esp.path().join("EFI").join("Linux");
        assert_eq!(esp_root_of(&linux), Some(esp.path()));
    }

    #[test]
    fn the_floor_refuses_older_or_unknown_targets() {
        let min = RollbackFloor::Min(v("0.4.0"));
        assert!(check_floor(&RollbackFloor::Off, &[("a", Err("x".into()))]).is_ok(), "no flag: unchanged");
        assert!(check_floor(&min, &[("a", Ok(v("0.4.0"))), ("b", Ok(v("0.5.1")))]).is_ok());
        assert!(check_floor(&min, &[("a", Ok(v("0.3.9")))]).unwrap_err().contains("older"));
        assert!(check_floor(&min, &[("a", Ok(v("0.4.0-rc2")))]).is_err(), "a pre-release of the floor is older");
        assert!(check_floor(&min, &[("a", Ok(v("0.5.0"))), ("b", Err("no .osrel".into()))]).unwrap_err().contains("cannot determine"));
        assert!(check_floor(&RollbackFloor::Invalid("x".into()), &[]).is_err());
        assert!(check_floor(&min, &[]).is_ok(), "nothing to boot into is refused elsewhere");
    }
}
