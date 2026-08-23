//! Appliance identity/time-and-date data for the `device.about` and
//! `device.timedate`/`device.timedate_set` dashboard RPCs (dispatch + admin/
//! appliance gating lives in `handlers.rs`, same split as `device.rs`).
//!
//! Same discipline as `device.rs`: every "read a live value" function here is
//! a thin OS-facing collector (reads a file / shells out, not unit-tested
//! beyond "doesn't panic") feeding a pure parsing function (unit-tested with
//! synthetic input on any host, including this repo's macOS dev machines).
//! Every collector degrades to `None`/`available: false` rather than failing
//! to build — or panicking — off-Linux.

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

// ── device.about ─────────────────────────────────────────────────────────

/// `device.about` response shape.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeviceAbout {
    pub os_pretty_name: Option<String>,
    pub os_version_id: Option<String>,
    pub os_image_id: Option<String>,
    pub os_build_id: Option<String>,
    pub kernel: Option<String>,
    pub hostname: Option<String>,
    pub gateway_version: String,
    /// First 16 lowercase hex chars of `sha256("duduclaw-device-id:" ||
    /// machine_id)` — NEVER the raw `/etc/machine-id` value (systemd's own
    /// docs treat it as confidential; see [`derive_device_id`]).
    pub device_id: Option<String>,
    pub is_appliance: bool,
}

/// Parse an `/etc/os-release`-shaped body: `KEY="quoted value"` / `KEY=bare`
/// lines, `#`-comments, and blank lines. Pure — unit-tested with synthetic
/// input on every host this crate compiles for, since the real file only
/// exists on Linux.
pub fn parse_os_release(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        map.insert(key.to_string(), unquote_os_release_value(raw_value.trim()));
    }
    map
}

/// Strip one matching pair of surrounding `"`/`'` quotes, if present.
/// `strip_prefix`/`strip_suffix` on `char` patterns are always char-boundary
/// safe (never a raw byte-index slice), so this can't panic on a CJK/emoji
/// value even though no os-release field is expected to carry one.
fn unquote_os_release_value(raw: &str) -> String {
    for q in ['"', '\''] {
        if let Some(stripped) = raw.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return stripped.to_string();
        }
    }
    raw.to_string()
}

fn read_os_release() -> Option<BTreeMap<String, String>> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    Some(parse_os_release(&text))
}

/// Pure: `/proc/sys/kernel/osrelease` is a single trimmed line (e.g.
/// `"6.12.0-duduclaw\n"`). Empty after trimming degrades to `None` rather
/// than an empty-string field.
fn non_empty_trim(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_kernel_osrelease() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .and_then(|t| non_empty_trim(&t))
}

fn read_hostname_file() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .and_then(|t| non_empty_trim(&t))
}

/// `/etc/hostname` first (matches the appliance image's own source of
/// truth), falling back to the already-depended-on `hostname` crate (used
/// elsewhere in this crate for mDNS advertising) — no new dependency added
/// for this, per the task brief.
fn read_hostname() -> Option<String> {
    read_hostname_file().or_else(|| {
        hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .filter(|s| !s.trim().is_empty())
    })
}

fn read_machine_id() -> Option<String> {
    std::fs::read_to_string("/etc/machine-id")
        .ok()
        .and_then(|t| non_empty_trim(&t))
}

/// Derive a stable, non-reversible-in-practice device id from
/// `/etc/machine-id` WITHOUT ever exposing the raw value — systemd's own
/// `machine-id(5)` docs describe it as confidential ("should not be exposed
/// … to untrusted clients"). `sha256("duduclaw-device-id:" || machine_id)`,
/// first 16 lowercase hex chars (64 bits — plenty to disambiguate devices in
/// a dashboard/support context without being a raw credential).
pub fn derive_device_id(machine_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"duduclaw-device-id:");
    hasher.update(machine_id.as_bytes());
    let full_hex = hex::encode(hasher.finalize());
    full_hex.chars().take(16).collect()
}

/// Assemble [`DeviceAbout`]. `gateway_version` is passed in (from
/// `crate::updater::current_version()`) rather than read here, keeping this
/// function a pure-ish orchestration of already-isolated collectors.
pub fn collect_device_about(gateway_version: &str) -> DeviceAbout {
    let os_release = read_os_release().unwrap_or_default();
    let get = |k: &str| os_release.get(k).cloned().filter(|s| !s.is_empty());
    DeviceAbout {
        os_pretty_name: get("PRETTY_NAME"),
        os_version_id: get("VERSION_ID"),
        os_image_id: get("IMAGE_ID"),
        os_build_id: get("BUILD_ID"),
        kernel: read_kernel_osrelease(),
        hostname: read_hostname(),
        gateway_version: gateway_version.to_string(),
        device_id: read_machine_id().map(|id| derive_device_id(&id)),
        is_appliance: duduclaw_core::is_appliance(),
    }
}

// ── device.timedate / device.timedate_set ──────────────────────────────────

/// `device.timedate` response shape.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TimedateStatus {
    pub timezone: Option<String>,
    /// RFC3339, always populated (from `chrono` in-process — never from the
    /// `timedatectl` shell-out) regardless of whether `timedatectl` itself
    /// is reachable.
    pub local_time: Option<String>,
    pub utc_time: Option<String>,
    pub ntp_enabled: Option<bool>,
    pub ntp_synchronized: Option<bool>,
    /// `false` when `timedatectl` could not be run at all (missing binary,
    /// off-Linux, non-zero exit) — `timezone`/`ntp_enabled`/
    /// `ntp_synchronized` are `None` in that case, never fabricated.
    pub available: bool,
}

/// Parse `timedatectl show --property=... [--property=...]` output —
/// `Key=Value` lines, one per requested property, in whatever order
/// `timedatectl` emits them (this parser doesn't rely on order — see the
/// task brief's "PARSE IT PURELY" ask). Pure, no comment-skipping needed
/// (`timedatectl show` never emits `#` lines) but blank lines are tolerated
/// defensively.
pub fn parse_timedatectl_show(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if !key.is_empty() {
                map.insert(key.to_string(), value.trim().to_string());
            }
        }
    }
    map
}

/// `timedatectl`'s boolean properties (`NTP`, `NTPSynchronized`) render as
/// the literal strings `"yes"`/`"no"` — anything else (a future systemd
/// version, a malformed capture) is honestly `None`, never guessed.
pub fn yes_no_to_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

async fn read_timedatectl() -> Option<String> {
    let output = tokio::process::Command::new("timedatectl")
        .args(["show", "--property=Timezone", "--property=NTP", "--property=NTPSynchronized"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Collect [`TimedateStatus`]. `local_time`/`utc_time` are always populated
/// (genuinely known in-process via `chrono`, independent of `timedatectl`'s
/// availability); every other field degrades to `None` + `available: false`
/// together, per the task brief's "never fabricate" rule.
pub async fn collect_timedate() -> TimedateStatus {
    let local_time = Some(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
    let utc_time = Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    match read_timedatectl().await {
        Some(text) => {
            let map = parse_timedatectl_show(&text);
            let timezone = map.get("Timezone").cloned().filter(|s| !s.is_empty());
            let ntp_enabled = map.get("NTP").and_then(|s| yes_no_to_bool(s));
            let ntp_synchronized = map.get("NTPSynchronized").and_then(|s| yes_no_to_bool(s));
            TimedateStatus {
                timezone,
                local_time,
                utc_time,
                ntp_enabled,
                ntp_synchronized,
                available: true,
            }
        }
        None => TimedateStatus {
            timezone: None,
            local_time,
            utc_time,
            ntp_enabled: None,
            ntp_synchronized: None,
            available: false,
        },
    }
}

/// Defense-in-depth shape check for `device.timedate_set`'s `timezone`
/// param — the sysd side validates again (doctrine: gateway AND sysd both
/// check, neither trusts the other alone). ASCII, `[A-Za-z0-9+._/-]`, no
/// `..`, no leading/trailing `/`, `<=64` bytes. Deliberately does not
/// validate against the IANA tzdata database (that would need a bundled
/// tzdata copy) — this is a shape check, not a "does this zone exist" check.
pub fn validate_timezone_shape(tz: &str) -> bool {
    if tz.is_empty() || tz.len() > 64 {
        return false;
    }
    if !tz.is_ascii() {
        return false;
    }
    if tz.contains("..") || tz.starts_with('/') || tz.ends_with('/') {
        return false;
    }
    tz.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '_' | '/' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_os_release ───────────────────────────────────────────────

    #[test]
    fn parses_typical_debian_os_release() {
        let text = "PRETTY_NAME=\"DuDuClaw OS 0.1.0\"\n\
                     NAME=\"DuDuClaw OS\"\n\
                     VERSION_ID=\"0.1.0\"\n\
                     ID=duduclaw\n\
                     # a comment\n\
                     \n\
                     HOME_URL='https://duduclaw.example'\n";
        let map = parse_os_release(text);
        assert_eq!(map.get("PRETTY_NAME").map(String::as_str), Some("DuDuClaw OS 0.1.0"));
        assert_eq!(map.get("VERSION_ID").map(String::as_str), Some("0.1.0"));
        assert_eq!(map.get("ID").map(String::as_str), Some("duduclaw"));
        assert_eq!(map.get("HOME_URL").map(String::as_str), Some("https://duduclaw.example"));
        assert!(!map.contains_key("a comment"));
    }

    #[test]
    fn missing_fields_are_absent_not_empty_string() {
        let map = parse_os_release("ID=duduclaw\n");
        assert!(!map.contains_key("PRETTY_NAME"));
    }

    #[test]
    fn empty_or_garbage_os_release_is_empty_map() {
        assert!(parse_os_release("").is_empty());
        assert!(parse_os_release("not a key value line\n\n# just comments\n").is_empty());
    }

    #[test]
    fn unquote_handles_both_quote_styles_and_bare_values() {
        assert_eq!(unquote_os_release_value("\"quoted\""), "quoted");
        assert_eq!(unquote_os_release_value("'quoted'"), "quoted");
        assert_eq!(unquote_os_release_value("bare"), "bare");
        // Mismatched quotes must not be stripped.
        assert_eq!(unquote_os_release_value("\"mismatched'"), "\"mismatched'");
    }

    // ── derive_device_id ─────────────────────────────────────────────────

    #[test]
    fn derive_device_id_is_deterministic_and_16_lowercase_hex_chars() {
        let id = derive_device_id("abcd1234abcd1234abcd1234abcd1234");
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(id, derive_device_id("abcd1234abcd1234abcd1234abcd1234"));
    }

    #[test]
    fn derive_device_id_differs_for_different_input() {
        assert_ne!(derive_device_id("machine-id-a"), derive_device_id("machine-id-b"));
    }

    #[test]
    fn derive_device_id_never_echoes_the_raw_machine_id() {
        let raw = "super-secret-machine-id-value";
        let id = derive_device_id(raw);
        assert!(!id.contains(raw));
    }

    // ── DeviceAbout orchestration smoke test ─────────────────────────────

    #[test]
    fn collect_device_about_never_panics_and_carries_gateway_version() {
        let about = collect_device_about("1.61.0");
        assert_eq!(about.gateway_version, "1.61.0");
        assert_eq!(about.is_appliance, duduclaw_core::is_appliance());
        // Every OS-identity field is legitimately `None` off-Linux — that's
        // honest, not a bug.
    }

    // ── parse_timedatectl_show ────────────────────────────────────────────

    #[test]
    fn parses_typical_timedatectl_show_output() {
        let text = "Timezone=Asia/Taipei\nNTP=yes\nNTPSynchronized=yes\n";
        let map = parse_timedatectl_show(text);
        assert_eq!(map.get("Timezone").map(String::as_str), Some("Asia/Taipei"));
        assert_eq!(map.get("NTP").map(String::as_str), Some("yes"));
        assert_eq!(map.get("NTPSynchronized").map(String::as_str), Some("yes"));
    }

    #[test]
    fn parse_timedatectl_show_tolerates_any_property_order() {
        let text = "NTPSynchronized=no\nTimezone=UTC\nNTP=no\n";
        let map = parse_timedatectl_show(text);
        assert_eq!(map.get("Timezone").map(String::as_str), Some("UTC"));
        assert_eq!(map.get("NTP").map(String::as_str), Some("no"));
    }

    #[test]
    fn parse_timedatectl_show_garbage_is_empty_map() {
        assert!(parse_timedatectl_show("").is_empty());
        assert!(parse_timedatectl_show("not a kv line\n\n").is_empty());
    }

    // ── yes_no_to_bool ─────────────────────────────────────────────────

    #[test]
    fn yes_no_to_bool_maps_exact_tokens() {
        assert_eq!(yes_no_to_bool("yes"), Some(true));
        assert_eq!(yes_no_to_bool("no"), Some(false));
        assert_eq!(yes_no_to_bool(" yes "), Some(true));
    }

    #[test]
    fn yes_no_to_bool_unrecognized_is_none() {
        assert_eq!(yes_no_to_bool("true"), None);
        assert_eq!(yes_no_to_bool(""), None);
        assert_eq!(yes_no_to_bool("Yes"), None);
    }

    // ── collect_timedate orchestration smoke test ────────────────────────

    #[tokio::test]
    async fn collect_timedate_always_carries_chrono_times_never_panics() {
        let status = collect_timedate().await;
        assert!(status.local_time.is_some());
        assert!(status.utc_time.is_some());
        if !status.available {
            assert!(status.timezone.is_none());
            assert!(status.ntp_enabled.is_none());
            assert!(status.ntp_synchronized.is_none());
        }
    }

    // ── validate_timezone_shape ───────────────────────────────────────────

    #[test]
    fn validate_timezone_shape_accepts_realistic_zones() {
        assert!(validate_timezone_shape("Asia/Taipei"));
        assert!(validate_timezone_shape("UTC"));
        assert!(validate_timezone_shape("America/Argentina/Buenos_Aires"));
        assert!(validate_timezone_shape("Etc/GMT+8"));
    }

    #[test]
    fn validate_timezone_shape_rejects_bad_shapes() {
        assert!(!validate_timezone_shape(""));
        assert!(!validate_timezone_shape(&"A".repeat(65)));
        assert!(!validate_timezone_shape("../../etc/passwd"));
        assert!(!validate_timezone_shape("/Asia/Taipei"));
        assert!(!validate_timezone_shape("Asia/Taipei/"));
        assert!(!validate_timezone_shape("Asia Taipei"));
        assert!(!validate_timezone_shape("測試/時區"));
        assert!(!validate_timezone_shape("Asia;rm -rf /"));
    }
}
