//! Installer-time Wi-Fi landing (stage 2, `DESIGN-installer-settings-
//! integration-2026-08.md` §5, plan (b)). The live installer's network step
//! only COLLECTS an SSID/passphrase into `LiveInstallState` — it never
//! scans or connects live, because the live image "carries no gateway
//! payload" and deliberately does not rebuild the iwd/D-Bus stack (design
//! doc §5's rejected plan (a)). The installer writes the collected values
//! onto the TARGET disk as `<DUDUCLAW_HOME>/pending-network.json`, and this
//! module is what lands them the first time the TARGET system's own
//! gateway boots — the exact same pending-file-then-land shape
//! [`crate::pending_account`] already established for the account step, so
//! this module mirrors that one's structure deliberately.
//!
//! Unlike the account step, a failed landing here is not a dead end: Wi-Fi
//! hardware may not have finished initializing yet, or the configured
//! network may briefly be out of range at boot. So this module retries in
//! the background for a bounded window (§ below) instead of giving up
//! after one attempt, and — critically — never blocks gateway startup on
//! any of it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{info, warn};

const PENDING_NETWORK_FILE: &str = "pending-network.json";

/// WPA-PSK passphrase length bound, mirrored from
/// `network::validate_psk`/iwd's own `8..=63` rule (see
/// `crates/duduclaw-gateway/src/network/iwd.rs` `connect`'s doc, and
/// `network/mod.rs::validate_psk`) — checked again here so a malformed
/// installer payload is discarded at landing time instead of failing every
/// one of the 15 retries below against `network::wifi_connect` for a
/// reason we could have caught up front.
const PSK_MIN_CHARS: usize = 8;
const PSK_MAX_CHARS: usize = 63;

/// 802.11 SSIDs are a byte string capped at 32 bytes (not 32 characters —
/// a CJK SSID eats that budget three bytes at a time), matching this
/// platform's own `WifiNetwork`/iwd assumptions elsewhere in the network
/// module.
const SSID_MAX_BYTES: usize = 32;

/// Retry cadence for the background landing task: 15 attempts, 20s apart —
/// about 5 minutes total, chosen to comfortably span iwd's own startup and
/// first-scan timing on a cold boot without retrying forever.
const MAX_ATTEMPTS: u32 = 15;
const RETRY_DELAY: Duration = Duration::from_secs(20);

#[derive(serde::Deserialize)]
struct PendingNetwork {
    ssid: String,
    #[serde(default)]
    psk: Option<String>,
}

/// Why a `pending-network.json` was rejected before ever reaching
/// `network::wifi_connect`. Every variant is a permanent, non-retryable
/// defect in the file's own contents (as opposed to a transient I/O or
/// Wi-Fi failure, which stays retryable — see [`spawn_pending_network_landing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Discard {
    /// Not valid JSON, or missing the required `ssid` field.
    Malformed,
    /// Empty after trimming, or over the 32-byte 802.11 SSID cap.
    SsidInvalid,
    /// Present but outside iwd's `8..=63`-character WPA-PSK range.
    PskInvalid,
}

/// Pure parse-and-validate step, split out from the file/IO/retry
/// machinery below specifically so it is unit-testable without touching a
/// filesystem or a Tokio runtime.
///
/// An empty-string `psk` (`{"psk":""}`) is normalized to `None` (open
/// network) rather than rejected — an installer UI that lets the operator
/// clear a passphrase field is a legitimate way to say "no password", not
/// a malformed payload.
fn evaluate_pending_network(raw: &str) -> Result<(String, Option<String>), Discard> {
    let pending: PendingNetwork = serde_json::from_str(raw).map_err(|_| Discard::Malformed)?;

    let ssid = pending.ssid.trim().to_string();
    if ssid.is_empty() || ssid.len() > SSID_MAX_BYTES {
        return Err(Discard::SsidInvalid);
    }

    let psk = match pending.psk {
        None => None,
        Some(p) if p.is_empty() => None,
        Some(p) => {
            let len = p.chars().count();
            if !(PSK_MIN_CHARS..=PSK_MAX_CHARS).contains(&len) {
                return Err(Discard::PskInvalid);
            }
            Some(p)
        }
    };

    Ok((ssid, psk))
}

/// Land an installer-written `pending-network.json`, if one exists, by
/// connecting via the existing cross-platform `network::wifi_connect`
/// (Linux-only under the hood; a non-Linux dev build gets an honest
/// `BackendUnavailable` on every attempt and exhausts the retry budget —
/// there is no separate `#[cfg(target_os = "linux")]` branch needed here
/// because that facade already carries the distinction).
///
/// Call once at gateway startup, after [`crate::pending_account::land_pending_account`]
/// (design doc §5 plan (b) — same "installer writes intent, target-system
/// gateway lands it" shape, just for Wi-Fi instead of the admin account).
///
/// Deliberately NOT `async fn`: the synchronous file-read/validate prefix
/// below never touches the network, so a caller in a plain (non-async)
/// context could reuse it too. The retry loop that actually calls
/// `network::wifi_connect` is spawned onto the caller's Tokio runtime —
/// this function itself returns immediately either way, so it can never
/// delay gateway startup.
pub(crate) fn spawn_pending_network_landing(home_dir: PathBuf) {
    let path = home_dir.join(PENDING_NETWORK_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            // Transient (permissions, etc). Leave the file — worth
            // retrying on the next boot, unlike a validation failure below
            // (which would never succeed no matter how many boots retry
            // it).
            warn!(
                error = %e,
                "failed to read pending-network.json — will retry next boot"
            );
            return;
        }
    };

    let (ssid, psk) = match evaluate_pending_network(&raw) {
        Ok(v) => v,
        Err(discard) => {
            // A retry can never succeed against the same malformed/invalid
            // bytes, and a residual file that may hold a plaintext
            // passphrase is not worth leaving on disk. Discard.
            warn!(
                ?discard,
                "pending-network.json is invalid — discarding (retry would never succeed)"
            );
            remove_pending_file(&path);
            return;
        }
    };

    // Background: Wi-Fi hardware/iwd may not be ready this early in boot,
    // and this must never block gateway startup (design doc §5 risk list —
    // the whole point of landing at the TARGET system's first boot instead
    // of during install is that install has no gateway/network stack at
    // all to test against).
    tokio::spawn(land_pending_network(path, ssid, psk));
}

/// The actual connect-and-retry loop, run as a detached background task.
async fn land_pending_network(path: PathBuf, ssid: String, psk: Option<String>) {
    for attempt in 1..=MAX_ATTEMPTS {
        // Best-effort rescan before every attempt: `network::wifi_connect`
        // (see `network/iwd.rs` `connect`'s doc) only matches against
        // whatever iwd's LAST scan already found — it does not scan
        // internally. On a cold boot iwd may not have scanned yet, so a
        // freshly-provisioned SSID would otherwise fail `NotFound` forever
        // even once it's actually in range. A scan failure here is not
        // fatal — `wifi_connect` below still runs against whatever iwd
        // already has cached.
        let _ = crate::network::wifi_scan(true).await;

        match crate::network::wifi_connect(&ssid, psk.as_deref()).await {
            Ok(()) => {
                info!(ssid = %ssid, "installer pending network landed");
                remove_pending_file(&path);
                return;
            }
            Err(e) => {
                // Never log `psk` — only the closed error code + technical
                // detail (already scrubbed of any passphrase by
                // `network::WifiError`'s own contract) and the SSID, which
                // is not a secret.
                warn!(
                    ssid = %ssid,
                    code = e.code.code(),
                    detail = %e.detail,
                    attempt,
                    max_attempts = MAX_ATTEMPTS,
                    "failed to land installer pending network — will retry"
                );
            }
        }

        if attempt < MAX_ATTEMPTS {
            tokio::time::sleep(RETRY_DELAY).await;
        }
    }

    // Retry budget exhausted (~5 minutes). Keep the file so the NEXT boot
    // tries again from scratch — this is the honest degradation path for,
    // e.g., no wireless hardware at all (a QEMU live-fire test is exactly
    // this case) or a network genuinely out of range. Deleting here would
    // permanently strand the installer's Wi-Fi intent with no retry
    // surface left.
    warn!(
        ssid = %ssid,
        "gave up landing installer pending network after {MAX_ATTEMPTS} attempts — keeping pending-network.json for retry on next boot"
    );
}

fn remove_pending_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(
                error = %e,
                path = %path.display(),
                "failed to remove pending-network.json"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── evaluate_pending_network ─────────────────────────────────────

    #[test]
    fn malformed_json_is_rejected() {
        assert_eq!(evaluate_pending_network("{ not json"), Err(Discard::Malformed));
    }

    #[test]
    fn missing_ssid_field_is_rejected_as_malformed() {
        assert_eq!(
            evaluate_pending_network(r#"{"psk":"correct-horse-battery"}"#),
            Err(Discard::Malformed)
        );
    }

    #[test]
    fn empty_ssid_is_rejected() {
        assert_eq!(
            evaluate_pending_network(r#"{"ssid":"   ","psk":null}"#),
            Err(Discard::SsidInvalid)
        );
    }

    #[test]
    fn ssid_over_32_bytes_is_rejected() {
        let ssid = "a".repeat(33);
        let raw = serde_json::json!({ "ssid": ssid, "psk": null }).to_string();
        assert_eq!(evaluate_pending_network(&raw), Err(Discard::SsidInvalid));
    }

    #[test]
    fn cjk_ssid_within_32_bytes_is_accepted() {
        // 10 CJK codepoints * 3 bytes (UTF-8) = 30 bytes, under the cap.
        let ssid = "咖啡廳無線網路測試站";
        assert_eq!(ssid.len(), 30);
        let raw = serde_json::json!({ "ssid": ssid, "psk": null }).to_string();
        assert_eq!(
            evaluate_pending_network(&raw),
            Ok((ssid.to_string(), None))
        );
    }

    #[test]
    fn psk_below_minimum_is_rejected() {
        let raw = serde_json::json!({ "ssid": "HomeWifi", "psk": "short12" }).to_string();
        assert_eq!(evaluate_pending_network(&raw), Err(Discard::PskInvalid));
    }

    #[test]
    fn psk_over_maximum_is_rejected() {
        let raw =
            serde_json::json!({ "ssid": "HomeWifi", "psk": "a".repeat(64) }).to_string();
        assert_eq!(evaluate_pending_network(&raw), Err(Discard::PskInvalid));
    }

    #[test]
    fn psk_at_minimum_boundary_is_accepted() {
        let psk = "a".repeat(8);
        let raw = serde_json::json!({ "ssid": "HomeWifi", "psk": psk }).to_string();
        assert_eq!(
            evaluate_pending_network(&raw),
            Ok(("HomeWifi".to_string(), Some(psk)))
        );
    }

    #[test]
    fn psk_at_maximum_boundary_is_accepted() {
        let psk = "a".repeat(63);
        let raw = serde_json::json!({ "ssid": "HomeWifi", "psk": psk }).to_string();
        assert_eq!(
            evaluate_pending_network(&raw),
            Ok(("HomeWifi".to_string(), Some(psk)))
        );
    }

    #[test]
    fn null_psk_is_an_open_network() {
        let raw = serde_json::json!({ "ssid": "OpenGuestWifi", "psk": null }).to_string();
        assert_eq!(
            evaluate_pending_network(&raw),
            Ok(("OpenGuestWifi".to_string(), None))
        );
    }

    #[test]
    fn empty_string_psk_is_normalized_to_open_network() {
        let raw = serde_json::json!({ "ssid": "OpenGuestWifi", "psk": "" }).to_string();
        assert_eq!(
            evaluate_pending_network(&raw),
            Ok(("OpenGuestWifi".to_string(), None))
        );
    }

    // ── spawn_pending_network_landing ────────────────────────────────
    //
    // Only the synchronous pre-spawn dispatch (file-existence gate,
    // validation, discard-and-delete) is exercised here. The legitimate-
    // file path spawns a background task that calls the REAL
    // `network::wifi_connect`/`wifi_scan` facade, which talks to iwd over
    // D-Bus on Linux and returns an honest `BackendUnavailable` everywhere
    // else — neither a D-Bus daemon nor real Wi-Fi hardware exists in this
    // test environment, and asserting on that task's outcome (or even just
    // "did it run yet") would be inherently racy against a 20s retry
    // cadence. Testing the pure `evaluate_pending_network` step above plus
    // these two dispatch-only paths is the least-flaky split: it proves
    // every byte on disk is validated correctly before anything ever
    // reaches the network, without pinning down timing of an I/O-bound
    // background task this module deliberately doesn't control tightly.

    #[test]
    fn no_pending_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        // Must not panic, and must not require a Tokio runtime at all
        // (the NotFound branch returns before any `tokio::spawn`).
        spawn_pending_network_landing(dir.path().to_path_buf());
    }

    #[test]
    fn malformed_pending_file_is_discarded_without_a_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let pending_path = dir.path().join(PENDING_NETWORK_FILE);
        std::fs::write(&pending_path, "{ not json").unwrap();

        // The Malformed branch also returns before any `tokio::spawn`, so
        // this needs no Tokio runtime either — a plain `#[test]` is
        // deliberately used instead of `#[tokio::test]` to prove that.
        spawn_pending_network_landing(dir.path().to_path_buf());

        assert!(!pending_path.exists());
    }

    #[test]
    fn ssid_invalid_pending_file_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let pending_path = dir.path().join(PENDING_NETWORK_FILE);
        std::fs::write(&pending_path, r#"{"ssid":"","psk":null}"#).unwrap();

        spawn_pending_network_landing(dir.path().to_path_buf());

        assert!(!pending_path.exists());
    }
}
