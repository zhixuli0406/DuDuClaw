// Real audio backend — PipeWire's `wpctl` CLI, `#[cfg(target_os = "linux")]`
// only. Shell-S4 (2026-08-22).
//
// ── Why a subprocess, not a wire protocol (unlike `oobe::network::nm`) ────
// `nm.rs` talks NetworkManager over D-Bus directly (`zbus`) because
// NetworkManager exposes a real, documented D-Bus service. PipeWire does
// NOT: its IPC is a bespoke native-socket protocol with no D-Bus surface at
// all, and no maintained pure-Rust client for it existed as of this round's
// own research pass (`research/native-os-2026-08/shell-s4-surfaces-2026-08.md`'s
// control-center section — read that first if touching this file). `wpctl`
// (shipped by WirePlumber, PipeWire's session manager) is the same "shell
// out to the platform's own CLI" tradeoff every `nmcli`-alike tool already
// makes when there's no lower-level API worth hand-rolling — zero new
// dependency, same subprocess pattern `oobe::claim`'s HTTP client and
// `nm.rs`'s own header comment both cite as this crate's established
// "know exactly what's on the wire, don't trust a wrapper" bias.
//
// ── Why `wpctl get-volume` IS the reachability probe (no separate `which
// wpctl`) ────────────────────────────────────────────────────────────────
// `Command::new("wpctl")` failing to spawn at all (`io::ErrorKind::NotFound`)
// is functionally identical to a `which wpctl` miss, and actually reading a
// real volume is a STRONGER "this backend is genuinely usable" signal than
// the binary merely existing on `$PATH` — same "read a real property, don't
// just open the socket" discipline `nm::NmNetworkBackend::probe()`'s own doc
// comment establishes. A machine running PipeWire with no default sink
// configured (headless container, no audio hardware) is still a REAL,
// honest "no working audio" state (`get_volume`/`set_volume`/`toggle_mute`
// all report `Unavailable`), not a reason to silently swap in `FakeAudioBackend`
// mid-session — `select_backend()` (`mod.rs`) only ever makes that swap
// ONCE, at construction time, exactly mirroring `network::select_backend`'s
// own fail-open boundary.
//
// ── Why every method re-reads `get_volume()` after acting ─────────────────
// `wpctl set-volume`/`set-mute` print nothing on success and PipeWire's
// volume/mute change is a synchronous local IPC round-trip (unlike NM's
// `AddAndActivateConnection`, which only ACCEPTS a join request and needs
// `nm.rs`'s own `poll_until_settled` to find out whether it actually
// worked) — there is no async negotiation to poll for here. Re-reading is
// simply the cheapest way to hand the caller the ACTUAL resulting state
// (post-clamp, post-toggle) instead of echoing back what was merely asked
// for, same "never trust the write's own success alone, read the real
// value back" instinct, applied to a call that happens to settle
// synchronously rather than one that needs a poll loop.
//
// ── No explicit subprocess timeout ─────────────────────────────────────
// Every call here is a single, fast local IPC round-trip in the common
// case — same category as `nm.rs`'s own untimed `Settings`/`GetSettings`
// quick calls (only NM's actual multi-second network operations,
// `scan`/`connect`, get an explicit budget there). If `wpctl` itself hangs
// (e.g. wireplumber wedged), this call blocks the BACKGROUND thread
// `overlay::controlcenter::kick_off_audio_call` spawned it on
// indefinitely — never gpui's main thread, which is the one invariant that
// actually matters (see that fn's own header comment). A future round could
// add a kill-after-N budget the same shape `CONNECT_TIMEOUT` gives `nm.rs`
// if this ever proves to matter in practice; not added speculatively here.

use std::process::{Command, Output};

use super::{AudioBackend, AudioError, VolumeState};

/// WirePlumber's own alias for "whatever sink is currently the system
/// default" — resolves without this module ever having to enumerate sink
/// object IDs itself.
const DEFAULT_SINK: &str = "@DEFAULT_AUDIO_SINK@";

pub(crate) struct WpctlAudioBackend;

impl WpctlAudioBackend {
    /// The reachability check `select_backend` (`mod.rs`) uses to decide
    /// Real vs. its Fake fail-open path — see this module's header comment
    /// for why a real `get_volume()` read, not a separate `which wpctl`
    /// call, is the probe.
    pub(crate) fn probe() -> Result<Self, AudioError> {
        let backend = Self;
        backend.get_volume()?;
        Ok(backend)
    }

    fn run(&self, args: &[&str]) -> Result<Output, AudioError> {
        Command::new("wpctl").args(args).output().map_err(|e| AudioError::Unavailable(format!("failed to spawn wpctl: {e}")))
    }

    fn require_success(&self, output: &Output, action: &str) -> Result<(), AudioError> {
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(AudioError::Unavailable(format!("wpctl {action} exited with {:?}: {}", output.status.code(), stderr.trim())))
    }
}

impl AudioBackend for WpctlAudioBackend {
    fn get_volume(&self) -> Result<VolumeState, AudioError> {
        let output = self.run(&["get-volume", DEFAULT_SINK])?;
        self.require_success(&output, "get-volume")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_wpctl_volume(&stdout).ok_or_else(|| AudioError::Unavailable(format!("unparsable wpctl get-volume output: {stdout:?}")))
    }

    fn set_volume(&self, pct: u8) -> Result<VolumeState, AudioError> {
        // `wpctl set-volume <ID> <VOL>[%]` — the `%` suffix form takes an
        // integer percentage directly, sidestepping any locale-dependent
        // decimal-point/comma float formatting entirely (this module never
        // has to `format!("{:.2}", ...)` a fraction).
        let clamped = pct.min(100);
        let arg = format!("{clamped}%");
        let output = self.run(&["set-volume", DEFAULT_SINK, &arg])?;
        self.require_success(&output, "set-volume")?;
        self.get_volume()
    }

    fn toggle_mute(&self) -> Result<VolumeState, AudioError> {
        let output = self.run(&["set-mute", DEFAULT_SINK, "toggle"])?;
        self.require_success(&output, "set-mute")?;
        self.get_volume()
    }
}

/// Parses `wpctl get-volume @DEFAULT_AUDIO_SINK@`'s stdout. Pure and
/// unit-testable without a live `wpctl` process — same "hand-rolled parser
/// as its own free function, exercised by a table of raw strings" shape
/// `nm.rs`'s `strength_to_bars`/`ssid_to_string` already establish.
///
/// Documented real output shape: `Volume: 0.75\n` (unmuted) or
/// `Volume: 0.75 [MUTED]\n` (muted). Resilient beyond that exact shape —
/// task brief: "壞輸出 fail-open" — because this is READING an external
/// process's stdout, not a value this crate controls:
///   - tolerates leading/trailing whitespace and blank lines (`trim()`)
///   - tolerates a missing/reworded `Volume:` prefix (falls back to
///     scanning the first whitespace-delimited token for a float)
///   - tolerates a comma decimal separator (some locales), normalized to
///     `.` before parsing
///   - clamps a boosted volume (`wpctl` allows `> 100%`, e.g. `1.50`) down
///     to this trait's `0..=100` contract rather than erroring
///   - rejects empty input, a non-numeric first token, and a negative
///     fraction outright (`None`) rather than guessing
fn parse_wpctl_volume(raw: &str) -> Option<VolumeState> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let muted = trimmed.to_ascii_uppercase().contains("MUTED");
    let after_prefix = trimmed.strip_prefix("Volume:").unwrap_or(trimmed).trim();
    let first_token = after_prefix.split_whitespace().next()?;
    let normalized = first_token.replace(',', ".");
    let frac: f64 = normalized.parse().ok()?;
    if !frac.is_finite() || frac < 0.0 {
        return None;
    }
    let pct = (frac * 100.0).round();
    let pct_u8 = if pct > 100.0 { 100 } else { pct as u8 };
    Some(VolumeState { pct: pct_u8, muted })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_volume_without_mute() {
        let state = parse_wpctl_volume("Volume: 0.75\n").unwrap();
        assert_eq!(state.pct, 75);
        assert!(!state.muted);
    }

    #[test]
    fn parses_muted_volume_with_bracket_suffix() {
        let state = parse_wpctl_volume("Volume: 0.75 [MUTED]\n").unwrap();
        assert_eq!(state.pct, 75);
        assert!(state.muted);
    }

    #[test]
    fn parses_full_volume_as_100_percent() {
        let state = parse_wpctl_volume("Volume: 1.00\n").unwrap();
        assert_eq!(state.pct, 100);
    }

    #[test]
    fn parses_zero_volume() {
        let state = parse_wpctl_volume("Volume: 0.00\n").unwrap();
        assert_eq!(state.pct, 0);
        assert!(!state.muted);
    }

    #[test]
    fn clamps_boosted_volume_above_100_percent() {
        let state = parse_wpctl_volume("Volume: 1.50\n").unwrap();
        assert_eq!(state.pct, 100);
    }

    #[test]
    fn tolerates_extra_whitespace_and_blank_trailing_lines() {
        let state = parse_wpctl_volume("  Volume:   0.42   \n\n").unwrap();
        assert_eq!(state.pct, 42);
    }

    #[test]
    fn tolerates_comma_decimal_separator() {
        let state = parse_wpctl_volume("Volume: 0,33\n").unwrap();
        assert_eq!(state.pct, 33);
    }

    #[test]
    fn tolerates_a_missing_volume_prefix() {
        // Resilience beyond the documented exact shape — see this fn's own
        // doc comment.
        let state = parse_wpctl_volume("0.60 [MUTED]\n").unwrap();
        assert_eq!(state.pct, 60);
        assert!(state.muted);
    }

    #[test]
    fn mute_detection_is_case_insensitive() {
        let state = parse_wpctl_volume("Volume: 0.10 [muted]\n").unwrap();
        assert!(state.muted);
    }

    #[test]
    fn rejects_empty_output() {
        assert_eq!(parse_wpctl_volume(""), None);
        assert_eq!(parse_wpctl_volume("   \n"), None);
    }

    #[test]
    fn rejects_garbage_output() {
        assert_eq!(parse_wpctl_volume("not a volume at all"), None);
    }

    #[test]
    fn rejects_a_prefix_with_no_numeric_token_after_it() {
        assert_eq!(parse_wpctl_volume("Volume:\n"), None);
    }

    #[test]
    fn rejects_negative_volume() {
        assert_eq!(parse_wpctl_volume("Volume: -0.10\n"), None);
    }

    #[test]
    fn rejects_non_finite_tokens() {
        assert_eq!(parse_wpctl_volume("Volume: nan\n"), None);
        assert_eq!(parse_wpctl_volume("Volume: inf\n"), None);
    }
}
