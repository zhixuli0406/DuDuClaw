//! Machine fingerprint generation.
//!
//! macOS uses a versioned UUID-only fingerprint; other platforms retain
//! hostname + MAC. Globally-administered unicast MACs remain hardware binding
//! evidence; instance-unique local MACs support cloud/container bindings, while
//! shared or invalid MACs are weak migration candidates only.
//!
//! ## What changed in v1.66.1
//!
//! Until v1.66.0 the identity component was whatever
//! `mac_address::get_mac_address()` returned — "the first non-loopback
//! interface" in whatever order the OS enumerates them. That is not a machine
//! identity: an interface can report an all-zero address, a shared placeholder,
//! or a multicast address, and which interface enumerates first changes across
//! OS upgrades. When that happens the fingerprint silently collapses to
//! "hostname only" *and* every license issued against the pre-upgrade value
//! stops validating.
//!
//! macOS 26 (Darwin 27) made that the normal case rather than the edge case.
//! Measured on a Mac mini running Darwin 27, 2026-09-29: `/sbin/ifconfig`
//! (Apple-signed) reports real addresses (`en0 = d0:11:e5:db:58:67`,
//! `anpi0 = 42:79:95:f6:e2:b7`), but an ordinary non-entitled process — this
//! binary, and `node`, and a raw ctypes `getifaddrs` probe alike — is handed
//! `02:00:00:00:00:00` for *every* interface. There is nothing left to select,
//! so the fingerprint had no hardware component at all.
//!
//! Three changes answer that:
//!
//! - **macOS binds to `IOPlatformUUID`** instead of a MAC ([`platform_uuid`]),
//!   read via `ioreg` — which an ordinary process *can* read. Other platforms
//!   are untouched.
//! - [`select_primary_mac`] filters the MAC enumeration on every platform, so a
//!   placeholder / all-zero / multicast address never
//!   becomes the selected identity. Real globally-administered unicast MACs
//!   remain preferred over locally-administered instance identities.
//! - [`fingerprint_candidates`] reproduces the values older builds computed on
//!   this machine, retaining their strength classification. Weak historical
//!   matches are isolated until the account holder approves a re-bind.
//!
//! The MAC component keeps the **exact** rendering `mac_address::MacAddress`'s
//! `Display` produces (uppercase, zero-padded). On a host where the first
//! enumerated interface already carried a usable address — the normal Linux /
//! Windows case — the v1.66.1 fingerprint is therefore byte-identical to the
//! v1.66.0 one, and nothing is flagged legacy.

use sha2::{Digest, Sha256};

/// The placeholder MAC address macOS hands to non-entitled processes in place
/// of a real one. Identical on every machine, so it carries zero identifying
/// information.
const PLACEHOLDER_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x00];

/// The MAC string the pre-v1.66.1 algorithm substituted when no MAC could be
/// read at all. Kept only to diagnose licenses requiring an approved re-bind.
const LEGACY_ABSENT_MAC: &str = "00:00:00:00:00:00";

/// Prefix marking a platform-UUID identity component.
///
/// Deliberately not hex- or colon-shaped: it can never collide with a MAC
/// string, so a UUID-derived fingerprint and a MAC-derived one are distinct
/// values even in the impossible case that the two sources agreed.
const UUID_IDENTITY_PREFIX: &str = "ioplatformuuid:";

/// Locally observed identity evidence behind a locally generated fingerprint candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintStrength {
    Hardware,
    /// Locally-administered unicast MAC, excluding known shared placeholders.
    Instance,
    Weak,
}

/// A fingerprint and the strength of the local observation that produced it.
/// Strength is never read from the license or a control-plane response.
/// The observed fingerprint and its strength cannot be replaced by callers.
///
/// ```compile_fail
/// let mut candidate = duduclaw_license::fingerprint_candidates().remove(0);
/// candidate.fingerprint = "arbitrary-binding".into();
/// ```
///
/// ```compile_fail
/// let candidate: duduclaw_license::FingerprintCandidate = "arbitrary-binding".to_owned().into();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintCandidate {
    fingerprint: String,
    strength: FingerprintStrength,
}

impl FingerprintCandidate {
    pub fn fingerprint(&self) -> &str { &self.fingerprint }

    pub fn strength(&self) -> FingerprintStrength { self.strength }

    /// Synthetic evidence for tests only. Never enable test-support in production.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(fingerprint: impl Into<String>, strength: FingerprintStrength) -> Self {
        Self { fingerprint: fingerprint.into(), strength }
    }
}

#[cfg(test)]
impl From<String> for FingerprintCandidate {
    fn from(fingerprint: String) -> Self {
        Self::for_test(fingerprint, FingerprintStrength::Hardware)
    }
}

#[cfg(test)]
impl From<&str> for FingerprintCandidate {
    fn from(fingerprint: &str) -> Self { fingerprint.to_string().into() }
}

impl AsRef<str> for FingerprintCandidate {
    fn as_ref(&self) -> &str { &self.fingerprint }
}

/// Which fingerprint in a candidate list actually matched a license.
///
/// Returned by [`crate::License::validate_any`] so callers can accept a
/// still-valid legacy binding while telling the operator to re-issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintMatch {
    /// The candidate fingerprint the license validated against.
    pub fingerprint: String,
    /// `true` when the match came from a backward-compatibility candidate
    /// rather than the current (strong) fingerprint.
    pub legacy: bool,
}

/// Generate this machine's preferred fingerprint for display or proposals.
///
/// On macOS with a readable UUID, a versioned UUID-only value. Otherwise the
/// historical hostname + MAC value. This string alone does not prove strength;
/// license validation must use the classified candidates. No cached file is
/// trusted as hardware.
pub fn generate_fingerprint() -> String {
    fingerprint_candidates()[0].fingerprint.clone()
}

/// All locally observed fingerprints, strongest first, then every historical
/// hostname-based variant. A historical hostname cannot be recovered from its
/// hash: an old binding after a rename still needs an authorized rebind.
pub fn fingerprint_candidates() -> Vec<FingerprintCandidate> {
    build_fingerprint_candidates(&hostname_string(), identity())
}

const STABLE_UUID_PREFIX: &str = "macos-uuid-v2:";

fn build_fingerprint_candidates(hostname: &str, identity: &Identity) -> Vec<FingerprintCandidate> {
    let mut out = Vec::new();
    if let Some(uuid_identity) = identity.candidates.iter()
        .find(|id| is_uuid_identity(id))
    {
        // Both the hash domain and the wire value are versioned. The wire
        // prefix makes this disjoint from all historical 32-hex fingerprints.
        out.push(FingerprintCandidate {
            fingerprint: format!("{STABLE_UUID_PREFIX}{}",
                compute_fingerprint(&format!("duduclaw:machine:v2::{uuid_identity}"))),
            strength: FingerprintStrength::Hardware,
        });
    }
    for id in &identity.candidates {
        let candidate = FingerprintCandidate {
            fingerprint: compute_fingerprint(&format!("{hostname}::{id}")),
            strength: identity_strength(id),
        };
        if !out.iter().any(|existing| existing.fingerprint == candidate.fingerprint) {
            out.push(candidate);
        }
    }
    out
}

fn is_uuid_identity(identity: &str) -> bool {
    identity.strip_prefix(UUID_IDENTITY_PREFIX).is_some_and(is_hardware_uuid)
}

fn identity_strength(identity: &str) -> FingerprintStrength {
    if is_uuid_identity(identity) { return FingerprintStrength::Hardware; }
    identity.parse::<mac_address::MacAddress>()
        .map(|mac| mac_strength(&mac.bytes()))
        .unwrap_or(FingerprintStrength::Weak)
}

/// macOS failed to read its UUID after bounded retries. Do not treat a MAC
/// candidate as evidence of a UUID binding: validation still requires equality.
/// Old untagged UUID hashes cannot be distinguished from old MAC hashes.
pub fn fingerprint_hardware_unavailable() -> bool {
    cfg!(target_os = "macos") && !has_hardware_uuid(identity())
}

fn has_hardware_uuid(identity: &Identity) -> bool {
    identity.candidates.iter().any(|id| is_uuid_identity(id))
}

/// Proposed binding for a refresh. The request must retain the installed
/// binding as the request identity. A proposal takes effect only after the
/// account holder approves the re-bind in the console; a fingerprint is not
/// proof of account ownership.
/// Never propose a Weak candidate or a MAC when the macOS UUID probe failed.
/// Other platforms keep accepted bindings; unbound OEM licenses stay unbound.
pub fn refresh_fingerprint(installed: &str) -> crate::Result<String> {
    refresh_fingerprint_for(installed, cfg!(target_os = "macos"),
        fingerprint_hardware_unavailable(), &fingerprint_candidates())
}

fn refresh_fingerprint_for(installed: &str, macos: bool, unavailable: bool,
    candidates: &[FingerprintCandidate]) -> crate::Result<String> {
    if installed.is_empty() { return Ok(String::new()); }
    if macos && unavailable { return Err(crate::LicenseError::HardwareIdentityUnavailable); }
    // Keep an existing non-macOS binding, including accepted legacy Instance
    // candidates. A weak or foreign binding needs the preferred valid identity.
    let candidate = (!macos).then(|| candidates.iter().find(|c|
        c.fingerprint() == installed && c.strength() != FingerprintStrength::Weak)).flatten()
        .or_else(|| candidates.iter().find(|c| c.strength() != FingerprintStrength::Weak))
        .ok_or(crate::LicenseError::WeakFingerprint)?;
    Ok(candidate.fingerprint.clone())
}

/// `true` when neither a platform UUID nor a usable MAC address could be read,
/// so the fingerprint is effectively "hostname only". Individual historical
/// candidates still require their own binding-strength classification.
///
/// A *reported* degradation, never a silent one: the CLI (`license fingerprint`
/// / `license status`) and the gateway surface it so a weak binding is never
/// mistaken for a strong one.
pub fn fingerprint_is_hostname_only() -> bool {
    !identity().hardware_bound
}

/// Compute fingerprint from a given input string.
/// Exposed for testability.
pub(crate) fn compute_fingerprint(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let hash = hasher.finalize();
    // Truncate to 128 bits (16 bytes) for a compact fingerprint
    hex::encode(&hash[..16])
}

fn hostname_string() -> String {
    gethostname::gethostname().to_string_lossy().to_string()
}

fn push_unique(out: &mut Vec<String>, value: String) {
    if !out.contains(&value) {
        out.push(value);
    }
}

// ── Resolved machine identity (computed once per process) ──────

/// The identity components this host offers, in descending strength.
struct Identity {
    /// Identity strings, strongest first, deduplicated. Never empty.
    candidates: Vec<String>,
    /// `true` when `candidates[0]` has a platform UUID or a usable Hardware or
    /// Instance MAC rather than a placeholder fallback.
    hardware_bound: bool,
}

static IDENTITY: std::sync::OnceLock<Identity> = std::sync::OnceLock::new();

fn identity() -> &'static Identity {
    IDENTITY.get_or_init(resolve_identity)
}

fn resolve_identity() -> Identity {
    let hardware = hardware_identity();
    let selected_mac = selected_mac_string();
    let legacy_mac = legacy_mac_string();
    Identity {
        hardware_bound: hardware.is_some() || selected_mac.is_some(),
        candidates: build_identity_candidates(hardware, selected_mac, legacy_mac),
    }
}

/// Assemble the identity candidate list.
///
/// Pure over its inputs so the ordering + dedup rules are testable on every
/// platform, including the macOS-only UUID branch.
fn build_identity_candidates(
    hardware: Option<String>,
    selected_mac: Option<String>,
    legacy_mac: Option<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for candidate in [hardware, selected_mac, legacy_mac].into_iter().flatten() {
        push_unique(&mut out, candidate);
    }
    // The pre-v1.66.1 fallback when no MAC was readable at all. Always present
    // to report that the license needs an approved re-bind, never to grant access.
    push_unique(&mut out, LEGACY_ABSENT_MAC.to_string());
    out
}

// ── Hardware identity: macOS IOPlatformUUID ───────────────────

/// The platform-UUID identity component for this host, if any.
fn hardware_identity() -> Option<String> {
    hardware_identity_with_retries(read_platform_uuid_source, 3)
}

// Retry failures, missing keys and malformed UUIDs alike. Identity is retained
// only in process memory after a live read. A mode-0600 file can still be forged
// by the same uid, so it must never supply a hardware validation candidate.
fn hardware_identity_with_retries(mut provider: impl FnMut() -> Option<String>, attempts: usize) -> Option<String> {
    for _ in 0..attempts {
        if let Some(identity) = hardware_identity_from(&mut provider) {
            return Some(identity);
        }
    }
    None
}

/// Build the identity component from an arbitrary `ioreg`-output provider.
///
/// Pure over the provider, so the macOS branch is unit-testable on any
/// platform without shelling out.
fn hardware_identity_from(provider: impl FnOnce() -> Option<String>) -> Option<String> {
    let output = provider()?;
    let uuid = parse_ioplatform_uuid(&output)?;
    Some(format!("{UUID_IDENTITY_PREFIX}{uuid}"))
}

#[cfg(target_os = "macos")]
fn read_platform_uuid_source() -> Option<String> {
    platform_uuid_source()
}

#[cfg(not(target_os = "macos"))]
fn read_platform_uuid_source() -> Option<String> {
    None
}

/// Extract `IOPlatformUUID` from `ioreg -rd1 -c IOPlatformExpertDevice` output.
///
/// Returns the UUID uppercased, or `None` when the key is absent or its value
/// is not a canonical 8-4-4-4-12 hex UUID — a malformed value is refused rather
/// than folded into a machine identity. The key name is matched
/// case-insensitively; the value is trimmed of whitespace and quotes.
pub fn parse_ioplatform_uuid(output: &str) -> Option<String> {
    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').trim();
        if !key.eq_ignore_ascii_case("IOPlatformUUID") {
            continue;
        }
        let value = value.trim().trim_matches('"').trim();
        if is_canonical_uuid(value) && is_hardware_uuid(value) {
            return Some(value.to_ascii_uppercase());
        }
    }
    None
}

/// Canonical 8-4-4-4-12 hex UUID, exactly 36 characters.
fn is_canonical_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    value.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// Reject shared firmware defaults in both dashed and compact spelling.
/// A syntactically valid UUID alone does not establish hardware evidence.
fn is_hardware_uuid(value: &str) -> bool {
    if !is_canonical_uuid(value)
        && !(value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return false;
    }
    let compact = value.replace('-', "").to_ascii_lowercase();
    // Repeated digits include nil, all-F, all-1 and other common fill values.
    if compact.bytes().all(|b| b == compact.as_bytes()[0]) {
        return false;
    }
    !matches!(compact.as_str(),
        "03000200040005000006000700080009"
        | "00020003000400050006000700080009"
        | "12345678123456781234567812345678"
        | "0123456789abcdef0123456789abcdef")
}

/// Read `ioreg` and return its stdout.
///
/// Hard-bounded on purpose: absolute binary path (never `PATH`), stdout capped
/// at 64 KiB, and a 5-second read deadline per attempt after which the child is
/// killed and reaped. Resolution retries at most three times (15 seconds of
/// read deadlines). No disk cache or license-provided UUID is trusted.
#[cfg(target_os = "macos")]
fn platform_uuid_source() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    /// `ioreg -rd1 -c IOPlatformExpertDevice` measures ~1.9 KB; 64 KiB is ample
    /// headroom while still bounding a pathological response.
    const MAX_IOREG_BYTES: u64 = 64 * 1024;
    const IOREG_TIMEOUT: Duration = Duration::from_secs(5);

    let mut child = Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(MAX_IOREG_BYTES).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let result = rx.recv_timeout(IOREG_TIMEOUT).ok();
    // Reap unconditionally: on the timeout path the child is still running, and
    // on the success path it may be blocked writing past the 64 KiB cap.
    let _ = child.kill();
    let _ = child.wait();

    result.map(|buf| String::from_utf8_lossy(&buf).into_owned())
}

// ── Hardware identity: MAC addresses ──────────────────────────

/// Prefer the first Hardware MAC, then the first Instance MAC. Shared,
/// all-zero, broadcast and multicast values never become the primary identity.
pub fn select_primary_mac(candidates: impl IntoIterator<Item = [u8; 6]>) -> Option<[u8; 6]> {
    let mut instance = None;
    for mac in candidates {
        match mac_strength(&mac) {
            FingerprintStrength::Hardware => return Some(mac),
            FingerprintStrength::Instance => { instance.get_or_insert(mac); }
            FingerprintStrength::Weak => {}
        }
    }
    instance
}

fn mac_strength(mac: &[u8; 6]) -> FingerprintStrength {
    if mac.iter().all(|b| *b == 0) || *mac == PLACEHOLDER_MAC || mac[0] & 0x01 != 0 {
        return FingerprintStrength::Weak;
    }
    if mac[0] & 0x02 != 0 { FingerprintStrength::Instance } else { FingerprintStrength::Hardware }
}

fn is_usable_mac(mac: &[u8; 6]) -> bool {
    mac_strength(mac) != FingerprintStrength::Weak
}

/// Render a MAC exactly as `mac_address::MacAddress`'s `Display` does.
///
/// Delegating rather than hand-rolling the format is deliberate: it makes a
/// selected MAC byte-identical to the legacy probe's string by construction, so
/// a host whose first interface was already usable keeps its v1.66.0
/// fingerprint to the byte.
fn format_mac(mac: &[u8; 6]) -> String {
    mac_address::MacAddress::new(*mac).to_string()
}

/// The MAC this host offers as an identity: the first *usable* address in the
/// full enumeration, or — when the enumeration itself is unavailable — the
/// legacy single-address probe, but only if that address is usable.
fn selected_mac_string() -> Option<String> {
    if let Some(mac) = mac_address::MacAddressIterator::new()
        .ok()
        .and_then(|iter| select_primary_mac(iter.map(|mac| mac.bytes())))
    {
        return Some(format_mac(&mac));
    }

    mac_address::get_mac_address()
        .ok()
        .flatten()
        .map(|mac| mac.bytes())
        .filter(is_usable_mac)
        .map(|mac| format_mac(&mac))
}

/// The MAC string the pre-v1.66.1 algorithm used: the first enumerated
/// non-loopback address, unfiltered — placeholder values included, because
/// reproducing them is exactly the point of the compatibility candidate.
fn legacy_mac_string() -> Option<String> {
    mac_address::get_mac_address()
        .ok()
        .flatten()
        .map(|mac| mac.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim shape of `ioreg -rd1 -c IOPlatformExpertDevice` output.
    const IOREG_SAMPLE: &str = r#"
+-o Macmini9,1  <class IOPlatformExpertDevice, id 0x100000267, registered>
    {
      "IOPolledInterface" = "AppleARMWatchdogTimerHibernateHandler is not serializable"
      "IOPlatformSerialNumber" = "C07XXXXXXXXX"
      "IOPlatformUUID" = "E9E128F2-D6CD-56BD-ACF6-BD2542EE2175"
      "IOBusyInterest" = "IOCommand is not serializable"
    }
"#;

    fn sample_identity(uuid: bool) -> Identity {
        Identity {
            candidates: build_identity_candidates(
                uuid.then(|| hardware_identity_from(|| Some(IOREG_SAMPLE.into())).unwrap()),
                Some("D0:11:E5:DB:58:67".into()),
                Some("02:00:00:00:00:00".into()),
            ),
            hardware_bound: true,
        }
    }

    #[test]
    fn uuid_fingerprint_survives_hostname_changes_and_keeps_hardware_legacy_binding() {
        let identity = sample_identity(true);
        let old = build_fingerprint_candidates("Mac-mini.local", &identity);
        let renamed = build_fingerprint_candidates("Mac-mini-2.local", &identity);
        assert_eq!(old[0], renamed[0]);
        assert!(old[0].fingerprint.strip_prefix(STABLE_UUID_PREFIX).is_some());
        for id in &identity.candidates {
            let legacy = compute_fingerprint(&format!("Mac-mini.local::{id}"));
            let license = crate::License::new("sub", "customer", crate::LicenseTier::SelfHostPro,
                &legacy, chrono::Duration::days(30), "test");
            if identity_strength(id) == FingerprintStrength::Weak {
                assert!(matches!(license.validate_any(&old, 7, 14), Err(crate::LicenseError::WeakFingerprint)));
            } else {
                let matched = license.validate_any(&old, 7, 14).unwrap();
                assert!(matched.legacy);
                assert_eq!(matched.fingerprint, legacy);
            }
        }
        let license = crate::License::new("sub", "customer", crate::LicenseTier::SelfHostPro,
            &old[0].fingerprint, chrono::Duration::days(30), "test");
        assert!(!license.validate_any(&renamed, 7, 14).unwrap().legacy);
        let absent = build_fingerprint_candidates("Mac-mini.local", &sample_identity(false));
        assert!(matches!(license.validate_any(&absent, 7, 14), Err(crate::LicenseError::InvalidFingerprint)));
        let old_uuid_license = crate::License::new("sub", "customer", crate::LicenseTier::SelfHostPro,
            &old[1].fingerprint, chrono::Duration::days(30), "test");
        assert!(matches!(old_uuid_license.validate_any(&absent, 7, 14), Err(crate::LicenseError::InvalidFingerprint)));
    }

    #[test]
    fn weak_legacy_bindings_cannot_activate_on_another_machine_with_the_same_hostname() {
        let owner = sample_identity(true);
        let mut copied = sample_identity(true);
        copied.candidates[0] = "ioplatformuuid:11111111-2222-3333-4444-555555555555".into();
        let owner_candidates = build_fingerprint_candidates("same-host", &owner);
        let copied_candidates = build_fingerprint_candidates("same-host", &copied);
        assert_ne!(owner_candidates[0], copied_candidates[0]);
        for weak_mac in [LEGACY_ABSENT_MAC, "02:00:00:00:00:00"] {
            let weak_fp = compute_fingerprint(&format!("same-host::{weak_mac}"));
            let license = crate::License::new("sub", "customer", crate::LicenseTier::SelfHostPro,
                weak_fp, chrono::Duration::days(30), "test");
            for candidates in [&owner_candidates, &copied_candidates] {
                assert!(matches!(license.validate_any(candidates, 7, 14),
                    Err(crate::LicenseError::WeakFingerprint)));
            }
        }
    }

    #[test]
    fn absent_mac_and_unusable_first_candidates_are_weak() {
        for legacy in [None, Some(LEGACY_ABSENT_MAC), Some("02:00:00:00:00:00"),
            Some("FF:FF:FF:FF:FF:FF"), Some("01:00:5E:00:00:01")]
        {
            let identity = Identity {
                candidates: build_identity_candidates(None, None, legacy.map(str::to_string)),
                hardware_bound: false,
            };
            let candidates = build_fingerprint_candidates("host", &identity);
            assert!(candidates.iter().all(|candidate| candidate.strength == FingerprintStrength::Weak));
            let license = crate::License::new("sub", "customer", crate::LicenseTier::SelfHostPro,
                &candidates[0].fingerprint, chrono::Duration::days(30), "test");
            assert!(matches!(license.validate_any(&candidates, 7, 14),
                Err(crate::LicenseError::WeakFingerprint)));
        }
    }

    #[test]
    fn weak_macs_cannot_bind_on_another_host() {
        for mac in [LEGACY_ABSENT_MAC, "02:00:00:00:00:00",
            "FF:FF:FF:FF:FF:FF", "01:00:5E:00:00:01"] {
            let owner = Identity {
                candidates: build_identity_candidates(
                    Some("ioplatformuuid:11111111-2222-3333-4444-555555555555".into()),
                    Some(mac.into()), Some(mac.into())),
                hardware_bound: true,
            };
            let copied = Identity {
                candidates: build_identity_candidates(
                    Some("ioplatformuuid:AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE".into()),
                    Some(mac.into()), Some(mac.into())),
                hardware_bound: true,
            };
            let mac_only = Identity {
                candidates: build_identity_candidates(None, Some(mac.into()), Some(mac.into())),
                hardware_bound: true,
            };
            let owner_candidates = build_fingerprint_candidates("same-host", &owner);
            let copied_candidates = build_fingerprint_candidates("same-host", &copied);
            let mac_only_candidates = build_fingerprint_candidates("same-host", &mac_only);
            assert_ne!(owner_candidates[0], copied_candidates[0]);
            let fingerprint = compute_fingerprint(&format!("same-host::{mac}"));
            for tier in [crate::LicenseTier::Hobby, crate::LicenseTier::Solo,
                crate::LicenseTier::Studio, crate::LicenseTier::Business,
                crate::LicenseTier::Partner, crate::LicenseTier::PersonalProSelfHost,
                crate::LicenseTier::SelfHostPro, crate::LicenseTier::Oem]
            {
                let license = crate::License::new("sub", "customer", tier,
                    &fingerprint, chrono::Duration::days(30), "test");
                for candidates in [&owner_candidates, &copied_candidates, &mac_only_candidates] {
                    let matched = candidates.iter().find(|c| c.fingerprint == fingerprint).unwrap();
                    assert_eq!(matched.strength, FingerprintStrength::Weak);
                    assert!(matches!(license.validate_any(candidates, 7, 14),
                        Err(crate::LicenseError::WeakFingerprint)));
                }
            }
        }
    }

    #[test]
    fn cloud_macs_validate_paid_bindings_and_preserve_legacy_candidates() {
        for hostname in ["linux-cloud", "WINDOWS-VM"] {
            for mac in ["42:01:0A:80:00:02", "02:11:22:33:44:55",
                "06:11:22:33:44:55", "0A:11:22:33:44:55", "02:42:AC:11:00:02"]
            {
                let bytes = mac.parse::<mac_address::MacAddress>().unwrap().bytes();
                assert_eq!(select_primary_mac([bytes]), Some(bytes));
                // A newly selected instance identity keeps the old hash byte-for-byte.
                let identity = Identity {
                    candidates: build_identity_candidates(None, Some(mac.into()), Some(mac.into())),
                    hardware_bound: true,
                };
                let candidates = build_fingerprint_candidates(hostname, &identity);
                assert_eq!(candidates[0].strength(), FingerprintStrength::Instance);
                let fingerprint = compute_fingerprint(&format!("{hostname}::{mac}"));
                assert_eq!(candidates[0].fingerprint(), fingerprint);
                // A later hardware NIC must not invalidate the historical instance binding.
                let upgraded = Identity {
                    candidates: build_identity_candidates(None, Some("D0:11:E5:DB:58:67".into()), Some(mac.into())),
                    hardware_bound: true,
                };
                let upgraded = build_fingerprint_candidates(hostname, &upgraded);
                assert_eq!(upgraded[0].strength(), FingerprintStrength::Hardware);
                assert_eq!(upgraded[1].strength(), FingerprintStrength::Instance);
                assert_eq!(upgraded[2].strength(), FingerprintStrength::Weak);
                for tier in [crate::LicenseTier::Hobby, crate::LicenseTier::Solo,
                    crate::LicenseTier::Studio, crate::LicenseTier::Business,
                    crate::LicenseTier::Partner, crate::LicenseTier::PersonalProSelfHost,
                    crate::LicenseTier::SelfHostPro, crate::LicenseTier::Oem]
                {
                    let license = crate::License::new("sub", "customer", tier,
                        &fingerprint, chrono::Duration::days(30), "test");
                    assert!(!license.validate_any(&candidates, 7, 14).unwrap().legacy);
                    assert!(license.validate_any(&upgraded, 7, 14).unwrap().legacy);
                }
            }
        }
    }

    #[test]
    fn real_mac_paid_bindings_validate_without_uuid_on_linux_and_windows() {
        for hostname in ["linux-host", "WINDOWS-PC"] {
            for mac in ["D0:11:E5:DB:58:67", "00:11:22:33:44:55"] {
                let identity = Identity {
                    candidates: build_identity_candidates(None, Some(mac.into()), Some(mac.into())),
                    hardware_bound: true,
                };
                let candidates = build_fingerprint_candidates(hostname, &identity);
                let fingerprint = compute_fingerprint(&format!("{hostname}::{mac}"));
                assert_eq!(candidates[0].fingerprint(), fingerprint);
                assert_eq!(candidates[0].strength(), FingerprintStrength::Hardware);
                assert!(!has_hardware_uuid(&identity));
                assert!(candidates.iter().all(|c| !c.fingerprint().starts_with(STABLE_UUID_PREFIX)));
                for tier in [crate::LicenseTier::Hobby, crate::LicenseTier::Solo,
                    crate::LicenseTier::Studio, crate::LicenseTier::Business,
                    crate::LicenseTier::Partner, crate::LicenseTier::PersonalProSelfHost,
                    crate::LicenseTier::SelfHostPro, crate::LicenseTier::Oem]
                {
                    let license = crate::License::new("sub", "customer", tier,
                        &fingerprint, chrono::Duration::days(30), "test");
                    let matched = license.validate_any(&candidates, 7, 14).unwrap();
                    assert_eq!(matched.fingerprint, fingerprint);
                    assert!(!matched.legacy);
                }
            }
        }
    }

    #[test]
    fn macos_uuid_failure_never_proposes_mac_even_when_mac_is_hardware() {
        let identity = sample_identity(false);
        assert_eq!(identity_strength(&identity.candidates[0]), FingerprintStrength::Hardware);
        assert!(!has_hardware_uuid(&identity));
        assert!(matches!(refresh_fingerprint_for("installed", true,
            !has_hardware_uuid(&identity), &build_fingerprint_candidates("host", &identity)),
            Err(crate::LicenseError::HardwareIdentityUnavailable)));
        assert!(has_hardware_uuid(&sample_identity(true)));
    }

    #[test]
    fn refresh_proposes_only_valid_candidates_and_preserves_unbound() {
        let hardware = FingerprintCandidate::for_test("hardware", FingerprintStrength::Hardware);
        let instance = FingerprintCandidate::for_test("instance", FingerprintStrength::Instance);
        let weak = FingerprintCandidate::for_test("weak", FingerprintStrength::Weak);
        for candidates in [vec![instance.clone(), weak.clone()], vec![hardware.clone(), instance.clone(), weak.clone()]] {
            for installed in ["old", "weak"] {
                let proposed = refresh_fingerprint_for(installed, false, false, &candidates).unwrap();
                assert_eq!(proposed, candidates[0].fingerprint());
            }
            assert_eq!(refresh_fingerprint_for("instance", false, false, &candidates).unwrap(), "instance");
        }
        assert_eq!(refresh_fingerprint_for("old", true, false, &[hardware]).unwrap(), "hardware");
        assert!(matches!(refresh_fingerprint_for("old", true, true, &[instance]),
            Err(crate::LicenseError::HardwareIdentityUnavailable)));
        assert!(matches!(refresh_fingerprint_for("weak", false, false, &[weak]),
            Err(crate::LicenseError::WeakFingerprint)));
        assert!(matches!(refresh_fingerprint_for("old", false, false, &[]),
            Err(crate::LicenseError::WeakFingerprint)));
        assert_eq!(refresh_fingerprint_for("", true, true, &[]).unwrap(), "");
    }

    #[test]
    fn without_uuid_mac_hostname_semantics_are_unchanged() {
        let identity = sample_identity(false);
        let candidates = build_fingerprint_candidates("host", &identity);
        assert_eq!(candidates[0].fingerprint, compute_fingerprint("host::D0:11:E5:DB:58:67"));
        assert_ne!(candidates[0], build_fingerprint_candidates("renamed", &identity)[0]);
        assert_eq!(candidates.len(), identity.candidates.len());
    }

    #[test]
    fn uuid_read_retries_failures_and_invalid_output_but_stops_after_success() {
        let mut calls = 0;
        let result = hardware_identity_with_retries(|| {
            calls += 1;
            match calls { 1 => None, 2 => Some("invalid".into()), _ => Some(IOREG_SAMPLE.into()) }
        }, 3);
        assert!(result.is_some());
        assert_eq!(calls, 3);
        let mut calls = 0;
        assert!(hardware_identity_with_retries(|| { calls += 1; None }, 3).is_none());
        assert_eq!(calls, 3);
        let mut calls = 0;
        assert!(hardware_identity_with_retries(|| { calls += 1; Some(IOREG_SAMPLE.into()) }, 3).is_some());
        assert_eq!(calls, 1);
    }

    #[test]
    fn fingerprint_has_versioned_or_legacy_shape() {
        let fp = generate_fingerprint();
        let fp = fp.strip_prefix(STABLE_UUID_PREFIX).unwrap_or(&fp);
        assert_eq!(fp.len(), 32, "fingerprint digest should be 32 hex characters");
        assert!(
            fp.chars().all(|c| c.is_ascii_hexdigit()),
            "fingerprint should contain only hex characters"
        );
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let fp1 = compute_fingerprint("test-host::AA:BB:CC:DD:EE:FF");
        let fp2 = compute_fingerprint("test-host::AA:BB:CC:DD:EE:FF");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn different_inputs_produce_different_fingerprints() {
        let fp1 = compute_fingerprint("host-a::AA:BB:CC:DD:EE:FF");
        let fp2 = compute_fingerprint("host-b::AA:BB:CC:DD:EE:FF");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn different_macs_produce_different_fingerprints() {
        let fp1 = compute_fingerprint("same-host::AA:BB:CC:DD:EE:FF");
        let fp2 = compute_fingerprint("same-host::11:22:33:44:55:66");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn mac_address_included_in_fingerprint() {
        // Same hostname but different MAC should yield different fingerprints
        let fp_with_mac = compute_fingerprint("hello::AA:BB:CC:DD:EE:FF");
        let fp_hostname_only = compute_fingerprint("hello");
        assert_ne!(fp_with_mac, fp_hostname_only);
    }

    #[test]
    fn selected_mac_string_is_17_chars_when_present() {
        // On any real machine with a usable NIC this is Some; in CI/containers
        // and on macOS 26 it may be None — that's acceptable.
        if let Some(addr) = selected_mac_string() {
            assert_eq!(addr.len(), 17, "MAC address should be 17 characters");
        }
    }

    // ── MAC selection ───────────────────────────────────────────

    #[test]
    fn select_primary_mac_skips_all_zero() {
        let picked = select_primary_mac([[0, 0, 0, 0, 0, 0], [0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67]]);
        assert_eq!(picked, Some([0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67]));
    }

    #[test]
    fn select_primary_mac_skips_the_shared_placeholder() {
        let picked = select_primary_mac([
            PLACEHOLDER_MAC,
            PLACEHOLDER_MAC,
            PLACEHOLDER_MAC,
            [0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67],
        ]);
        assert_eq!(picked, Some([0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67]));
    }

    #[test]
    fn select_primary_mac_skips_multicast() {
        // Low bit of the first octet set ⇒ multicast, never an interface identity.
        let picked = select_primary_mac([
            [0x01, 0x00, 0x5e, 0x00, 0x00, 0x01],
            [0xa8, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        ]);
        assert_eq!(picked, Some([0xa8, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
    }

    #[test]
    fn select_primary_mac_classifies_local_unicast_and_prefers_hardware() {
        for first in (0u8..=255).filter(|first| first & 0x02 != 0) {
            let local = [first, 0x11, 0x22, 0x33, 0x44, 0x55];
            let unicast = first & 0x01 == 0;
            assert_eq!(select_primary_mac([local]), unicast.then_some(local));
            assert_eq!(identity_strength(&format_mac(&local)),
                if unicast { FingerprintStrength::Instance } else { FingerprintStrength::Weak });
            let global = [0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67];
            assert_eq!(select_primary_mac([local, global]), Some(global));
        }
    }

    #[test]
    fn select_primary_mac_keeps_first_instance_when_no_hardware_exists() {
        let gcp = [0x42, 0x01, 0x0a, 0x80, 0x00, 0x02];
        let docker = [0x02, 0x42, 0xac, 0x11, 0x00, 0x02];
        assert_eq!(select_primary_mac([PLACEHOLDER_MAC, gcp, docker]), Some(gcp));
        assert_eq!(select_primary_mac([docker, gcp, PLACEHOLDER_MAC]), Some(docker));
    }

    #[test]
    fn select_primary_mac_takes_first_usable() {
        let picked = select_primary_mac([
            [0xa8, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            [0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67],
        ]);
        assert_eq!(picked, Some([0xa8, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
    }

    #[test]
    fn select_primary_mac_returns_none_when_all_unusable() {
        let picked = select_primary_mac([
            [0, 0, 0, 0, 0, 0],
            PLACEHOLDER_MAC,
            [0x01, 0x00, 0x5e, 0x00, 0x00, 0x01],
        ]);
        assert_eq!(picked, None);
    }

    #[test]
    fn select_primary_mac_empty_is_none() {
        assert_eq!(select_primary_mac([]), None);
    }

    #[test]
    fn format_mac_matches_the_crate_display_exactly() {
        // The byte-identical guarantee for Linux/Windows hosts: a selected MAC
        // must render exactly as the legacy probe's string did (uppercase,
        // zero-padded), so their fingerprints collapse to one candidate.
        let bytes = [0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67];
        assert_eq!(format_mac(&bytes), "D0:11:E5:DB:58:67");
        assert_eq!(
            format_mac(&bytes),
            mac_address::MacAddress::new(bytes).to_string()
        );
        // Zero-padding must survive for low octets.
        assert_eq!(format_mac(&PLACEHOLDER_MAC), "02:00:00:00:00:00");
    }

    // ── ioreg / IOPlatformUUID parsing ──────────────────────────

    #[test]
    fn placeholder_uuids_never_supply_hardware_evidence() {
        for value in ["00000000-0000-0000-0000-000000000000",
            "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF",
            "11111111-1111-1111-1111-111111111111",
            "03000200-0400-0500-0006-000700080009",
            "00020003-0004-0005-0006-000700080009",
            "12345678-1234-5678-1234-567812345678",
            "01234567-89AB-CDEF-0123-456789ABCDEF"]
        {
            for spelling in [value.to_string(), value.to_ascii_lowercase(),
                value.replace('-', ""), value.replace('-', "").to_ascii_lowercase()]
            {
                let output = format!("IOPlatformUUID=\"{spelling}\"");
                assert_eq!(parse_ioplatform_uuid(&output), None, "{spelling}");
                assert!(hardware_identity_from(|| Some(output.clone())).is_none());
                // Defense in depth: even a bypassed parser cannot promote a placeholder.
                let id = format!("{UUID_IDENTITY_PREFIX}{spelling}");
                assert_eq!(identity_strength(&id), FingerprintStrength::Weak);
                let identity = Identity { candidates: vec![id], hardware_bound: false };
                let candidates = build_fingerprint_candidates("shared-host", &identity);
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].strength(), FingerprintStrength::Weak);
                assert!(!candidates[0].fingerprint.starts_with(STABLE_UUID_PREFIX));
                let license = crate::License::new("sub", "customer", crate::LicenseTier::SelfHostPro,
                    &candidates[0].fingerprint, chrono::Duration::days(30), "v1");
                assert!(matches!(license.validate_any(&candidates, 7, 14), Err(crate::LicenseError::WeakFingerprint)));
            }
        }
    }

    #[test]
    fn parse_ioplatform_uuid_reads_a_real_sample() {
        assert_eq!(
            parse_ioplatform_uuid(IOREG_SAMPLE).as_deref(),
            Some("E9E128F2-D6CD-56BD-ACF6-BD2542EE2175")
        );
    }

    #[test]
    fn parse_ioplatform_uuid_returns_none_when_the_key_is_absent() {
        let without = IOREG_SAMPLE
            .lines()
            .filter(|l| !l.contains("IOPlatformUUID"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_ioplatform_uuid(&without), None);
    }

    #[test]
    fn parse_ioplatform_uuid_refuses_a_malformed_value() {
        // Refused rather than folded into an identity: short, non-hex, wrong
        // dash layout, and empty all fail closed.
        for bad in [
            r#"  "IOPlatformUUID" = "E9E128F2-D6CD-56BD-ACF6""#,
            r#"  "IOPlatformUUID" = "ZZZZZZZZ-D6CD-56BD-ACF6-BD2542EE2175""#,
            r#"  "IOPlatformUUID" = "E9E128F2D6CD56BDACF6BD2542EE21750""#,
            r#"  "IOPlatformUUID" = """#,
            r#"  "IOPlatformUUID" = "not a uuid at all, padded to 36 ch""#,
        ] {
            assert_eq!(parse_ioplatform_uuid(bad), None, "must refuse: {bad}");
        }
    }

    #[test]
    fn parse_ioplatform_uuid_matches_the_key_case_insensitively() {
        let lowered = r#"  "ioplatformuuid" = "e9e128f2-d6cd-56bd-acf6-bd2542ee2175""#;
        assert_eq!(
            parse_ioplatform_uuid(lowered).as_deref(),
            Some("E9E128F2-D6CD-56BD-ACF6-BD2542EE2175"),
            "key matched case-insensitively, value uppercased"
        );
    }

    #[test]
    fn parse_ioplatform_uuid_tolerates_missing_quotes_and_spacing() {
        let bare = "IOPlatformUUID=E9E128F2-D6CD-56BD-ACF6-BD2542EE2175";
        assert_eq!(
            parse_ioplatform_uuid(bare).as_deref(),
            Some("E9E128F2-D6CD-56BD-ACF6-BD2542EE2175")
        );
    }

    // ── Injectable macOS identity path ──────────────────────────

    #[test]
    fn hardware_identity_from_builds_a_prefixed_uuid_identity() {
        assert_eq!(
            hardware_identity_from(|| Some(IOREG_SAMPLE.to_string())).as_deref(),
            Some("ioplatformuuid:E9E128F2-D6CD-56BD-ACF6-BD2542EE2175")
        );
    }

    #[test]
    fn hardware_identity_from_is_none_when_the_source_is_unavailable() {
        assert_eq!(hardware_identity_from(|| None), None);
        assert_eq!(hardware_identity_from(|| Some(String::new())), None);
    }

    #[test]
    fn uuid_identity_can_never_collide_with_a_mac_string() {
        let uuid_identity = hardware_identity_from(|| Some(IOREG_SAMPLE.to_string())).unwrap();
        assert!(uuid_identity.starts_with(UUID_IDENTITY_PREFIX));
        assert_ne!(uuid_identity, format_mac(&[0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67]));
        assert_ne!(uuid_identity, LEGACY_ABSENT_MAC);
    }

    // ── Candidate ordering + dedup ──────────────────────────────

    #[test]
    fn candidates_put_the_uuid_identity_first_when_available() {
        // macOS 26 shape: UUID readable, every MAC is the shared placeholder.
        let candidates = build_identity_candidates(
            Some("ioplatformuuid:E9E128F2-D6CD-56BD-ACF6-BD2542EE2175".into()),
            None,
            Some("02:00:00:00:00:00".into()),
        );
        assert_eq!(
            candidates,
            vec![
                "ioplatformuuid:E9E128F2-D6CD-56BD-ACF6-BD2542EE2175".to_string(),
                "02:00:00:00:00:00".to_string(),
                "00:00:00:00:00:00".to_string(),
            ]
        );
    }

    #[test]
    fn candidates_fall_back_to_the_mac_when_no_uuid_is_available() {
        // Non-macOS shape: no UUID source, first interface already usable ⇒ the
        // selected and legacy strings are identical and collapse to one entry,
        // so a v1.66.0 fingerprint stays candidate[0] byte-for-byte.
        let candidates = build_identity_candidates(
            None,
            Some("D0:11:E5:DB:58:67".into()),
            Some("D0:11:E5:DB:58:67".into()),
        );
        assert_eq!(
            candidates,
            vec![
                "D0:11:E5:DB:58:67".to_string(),
                "00:00:00:00:00:00".to_string(),
            ]
        );
    }

    #[test]
    fn candidates_keep_the_legacy_raw_mac_when_selection_moved() {
        // OS upgrade shape: the old code would have taken the placeholder, the
        // new code selects a real address; the placeholder remains diagnostic only.
        let candidates = build_identity_candidates(
            None,
            Some("D0:11:E5:DB:58:67".into()),
            Some("02:00:00:00:00:00".into()),
        );
        assert_eq!(
            candidates,
            vec![
                "D0:11:E5:DB:58:67".to_string(),
                "02:00:00:00:00:00".to_string(),
                "00:00:00:00:00:00".to_string(),
            ]
        );
    }

    #[test]
    fn candidates_are_never_empty_and_never_duplicated() {
        let empty_host = build_identity_candidates(None, None, None);
        assert_eq!(empty_host, vec!["00:00:00:00:00:00".to_string()]);

        for list in [
            build_identity_candidates(None, None, Some("00:00:00:00:00:00".into())),
            build_identity_candidates(
                Some("ioplatformuuid:E9E128F2-D6CD-56BD-ACF6-BD2542EE2175".into()),
                Some("D0:11:E5:DB:58:67".into()),
                Some("D0:11:E5:DB:58:67".into()),
            ),
            identity().candidates.clone(),
        ] {
            let mut seen = list.clone();
            seen.sort();
            seen.dedup();
            assert_eq!(seen.len(), list.len(), "duplicated candidates: {list:?}");
            assert!(!list.is_empty());
        }
    }

    #[test]
    fn candidates_lead_with_the_strong_fingerprint() {
        let candidates = fingerprint_candidates();
        assert!(!candidates.is_empty());
        assert_eq!(
            candidates[0].fingerprint,
            generate_fingerprint(),
            "candidate[0] must be the strong fingerprint (what `license fingerprint` prints)"
        );
    }

    #[test]
    fn candidates_include_the_no_mac_legacy_value() {
        // The pre-v1.66.1 fallback stays detectable, but cannot grant paid access.
        let hostname = hostname_string();
        let legacy = compute_fingerprint(&format!("{hostname}::{LEGACY_ABSENT_MAC}"));
        assert!(
            fingerprint_candidates().iter().any(|candidate| candidate.fingerprint == legacy
                && candidate.strength == FingerprintStrength::Weak),
            "the no-MAC legacy fingerprint must remain a candidate"
        );
    }

    #[test]
    fn candidates_are_all_32_hex_chars() {
        for fp in fingerprint_candidates() {
            let fp = fp.fingerprint.strip_prefix(STABLE_UUID_PREFIX).unwrap_or(&fp.fingerprint);
            assert_eq!(fp.len(), 32, "{fp} is not 32 chars");
            assert!(fp.chars().all(|c| c.is_ascii_hexdigit()), "{fp} is not hex");
        }
    }

    #[test]
    fn hostname_only_flag_agrees_with_the_resolved_identity() {
        // This flag reports the preferred identity. Historical candidates
        // may still be weak even when a usable UUID or MAC is present.
        let resolved = identity();
        let first_is_hardware = resolved.candidates[0].starts_with(UUID_IDENTITY_PREFIX)
            || mac_address::MacAddress::try_from(resolved.candidates[0].as_str())
                .ok()
                .is_some_and(|m| is_usable_mac(&m.bytes()));
        assert_eq!(fingerprint_is_hostname_only(), !first_is_hardware);
        assert_eq!(resolved.hardware_bound, first_is_hardware);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reads_a_platform_uuid_on_a_real_machine() {
        // On a real Mac this must succeed — it is the whole point of v1.66.1.
        // Kept honest: if ioreg is unavailable the identity degrades rather
        // than the test lying, so we assert the degradation is consistent.
        match platform_uuid_source() {
            Some(output) => {
                let uuid = parse_ioplatform_uuid(&output);
                assert!(uuid.is_some(), "ioreg ran but no usable IOPlatformUUID");
                assert!(!fingerprint_is_hostname_only());
            }
            None => assert!(
                selected_mac_string().is_some() || fingerprint_is_hostname_only(),
                "no UUID and no usable MAC must report hostname-only"
            ),
        }
    }
}
