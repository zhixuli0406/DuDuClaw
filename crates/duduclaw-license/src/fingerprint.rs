//! Machine fingerprint generation.
//!
//! Computes a deterministic SHA-256 hash of the machine's hostname combined
//! with a hardware identity component, truncated to 128 bits and hex-encoded.
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
//!   placeholder / all-zero / multicast address never becomes an identity.
//! - [`fingerprint_candidates`] reproduces the values older builds computed on
//!   this machine, so an already-issued license keeps validating (see
//!   [`crate::License::validate_any`]) while the operator is told to re-issue.
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
/// read at all. Kept as the last candidate so licenses issued on such a machine
/// keep validating.
const LEGACY_ABSENT_MAC: &str = "00:00:00:00:00:00";

/// Prefix marking a platform-UUID identity component.
///
/// Deliberately not hex- or colon-shaped: it can never collide with a MAC
/// string, so a UUID-derived fingerprint and a MAC-derived one are distinct
/// values even in the impossible case that the two sources agreed.
const UUID_IDENTITY_PREFIX: &str = "ioplatformuuid:";

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

/// Generate this machine's **strong** fingerprint.
///
/// A hex-encoded 128-bit truncation of `SHA-256(hostname::identity)`, where
/// `identity` is the strongest source available on this host:
///
/// 1. `ioplatformuuid:<UUID>` — macOS only, from `IOPlatformUUID`;
/// 2. a usable MAC address selected out of the full interface enumeration;
/// 3. the raw first-enumerated MAC (pre-v1.66.1 behaviour), even if it is a
///    placeholder — degraded, but never worse than the old value;
/// 4. `00:00:00:00:00:00` when nothing at all is readable.
pub fn generate_fingerprint() -> String {
    let hostname = hostname_string();
    compute_fingerprint(&format!("{}::{}", hostname, identity().candidates[0]))
}

/// All fingerprints this machine may legitimately present, strongest first.
///
/// `[0]` is always [`generate_fingerprint`] — the value `duduclaw license
/// fingerprint` prints and the only one that should be used for a *new*
/// issuance. The remaining entries reproduce what older builds would have
/// computed here, so upgrading DuDuClaw (or the OS) does not invalidate an
/// already-issued license. Duplicates are removed, so on a machine where every
/// source agrees this returns a single entry.
pub fn fingerprint_candidates() -> Vec<String> {
    let hostname = hostname_string();
    identity()
        .candidates
        .iter()
        .map(|id| compute_fingerprint(&format!("{}::{}", hostname, id)))
        .collect()
}

/// `true` when neither a platform UUID nor a usable MAC address could be read,
/// so the fingerprint is effectively "hostname only" and offers no hardware
/// binding.
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
    /// `true` when `candidates[0]` is a real hardware identity (platform UUID
    /// or a usable MAC) rather than a placeholder fallback.
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
    // so a license issued on such a machine keeps validating.
    push_unique(&mut out, LEGACY_ABSENT_MAC.to_string());
    out
}

// ── Hardware identity: macOS IOPlatformUUID ───────────────────

/// The platform-UUID identity component for this host, if any.
fn hardware_identity() -> Option<String> {
    hardware_identity_from(read_platform_uuid_source)
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
        if is_canonical_uuid(value) {
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

/// Read `ioreg` and return its stdout.
///
/// Hard-bounded on purpose: absolute binary path (never `PATH`), stdout capped
/// at 64 KiB, and a 2-second wall-clock ceiling after which the child is
/// killed. Every failure mode returns `None`, which degrades to the MAC path —
/// a missing UUID must never be able to stall or crash license loading.
#[cfg(target_os = "macos")]
fn platform_uuid_source() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    /// `ioreg -rd1 -c IOPlatformExpertDevice` measures ~1.9 KB; 64 KiB is ample
    /// headroom while still bounding a pathological response.
    const MAX_IOREG_BYTES: u64 = 64 * 1024;
    const IOREG_TIMEOUT: Duration = Duration::from_secs(2);

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

/// Pick the first *usable* MAC address out of an interface enumeration.
///
/// A MAC is unusable when it carries no identifying information:
///
/// - all-zero (`00:00:00:00:00:00`) — the "no address" placeholder;
/// - `02:00:00:00:00:00` — the placeholder macOS hands to non-entitled
///   processes, identical on every machine running that OS;
/// - multicast (low bit of the first octet set) — never a real interface
///   identity, and some virtual adapters expose one.
///
/// Returns `None` when every candidate is unusable, so the caller can decide
/// how to degrade rather than silently adopting a shared constant.
pub fn select_primary_mac(candidates: impl IntoIterator<Item = [u8; 6]>) -> Option<[u8; 6]> {
    candidates.into_iter().find(is_usable_mac)
}

fn is_usable_mac(mac: &[u8; 6]) -> bool {
    if mac.iter().all(|b| *b == 0) {
        return false;
    }
    if *mac == PLACEHOLDER_MAC {
        return false;
    }
    // Multicast bit — the least significant bit of the first octet.
    if mac[0] & 0x01 != 0 {
        return false;
    }
    true
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

    #[test]
    fn fingerprint_is_32_hex_chars() {
        let fp = generate_fingerprint();
        assert_eq!(fp.len(), 32, "fingerprint should be 32 hex characters");
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
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        ]);
        assert_eq!(picked, Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
    }

    #[test]
    fn select_primary_mac_takes_first_usable() {
        let picked = select_primary_mac([
            [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            [0xd0, 0x11, 0xe5, 0xdb, 0x58, 0x67],
        ]);
        assert_eq!(picked, Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]));
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
        // new code selects a real address — both must remain acceptable.
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
            candidates[0],
            generate_fingerprint(),
            "candidate[0] must be the strong fingerprint (what `license fingerprint` prints)"
        );
    }

    #[test]
    fn candidates_include_the_no_mac_legacy_value() {
        // The pre-v1.66.1 fallback binding must stay accepted.
        let hostname = hostname_string();
        let legacy = compute_fingerprint(&format!("{hostname}::{LEGACY_ABSENT_MAC}"));
        assert!(
            fingerprint_candidates().contains(&legacy),
            "the no-MAC legacy fingerprint must remain a candidate"
        );
    }

    #[test]
    fn candidates_are_all_32_hex_chars() {
        for fp in fingerprint_candidates() {
            assert_eq!(fp.len(), 32, "{fp} is not 32 chars");
            assert!(fp.chars().all(|c| c.is_ascii_hexdigit()), "{fp} is not hex");
        }
    }

    #[test]
    fn hostname_only_flag_agrees_with_the_resolved_identity() {
        // Never claim strength we did not measure: the flag must be false
        // exactly when candidate[0] is a real hardware identity.
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
