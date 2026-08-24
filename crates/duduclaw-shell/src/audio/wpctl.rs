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
// ── Why `wpctl status` IS the reachability probe (D5, 2026-08-24) ────────
// `Command::new("wpctl")` failing to spawn at all (`io::ErrorKind::NotFound`)
// is functionally identical to a `which wpctl` miss, and actually completing
// a round-trip to the daemon is a STRONGER "this backend is genuinely
// usable" signal than the binary merely existing on `$PATH` — same "read a
// real property, don't just open the socket" discipline
// `nm::NmNetworkBackend::probe()`'s own doc comment establishes.
//
// Shell-S4 used `get-volume` for this. D5 moved it to `status` because the
// two failures it used to conflate now have to be told apart, and only
// `status` can:
//   * PipeWire unreachable (daemon not started, socket gone, wpctl missing)
//     — `wpctl status` itself exits non-zero. `select_backend()` turns this
//     into `UnavailableAudioBackend` and the UI disables its controls.
//   * PipeWire running with NO SINK (a box with no sound card, or a VM
//     booted without one) — `wpctl status` succeeds and lists an empty
//     Sinks section, while `get-volume @DEFAULT_AUDIO_SINK@` fails. That is
//     a working audio service attached to no hardware, and 系統設定 › 聲音
//     must say 「沒有可用的輸出裝置」 rather than 「音訊服務未啟動」.
// Probing with `get-volume` made the second case indistinguishable from the
// first, i.e. made the appliance blame its own service for absent hardware.
//
// `select_backend()` (`mod.rs`) makes this decision ONCE, at construction
// time, never mid-session.
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
// `audio::bridge::kick_off_audio_call` (or `settings::spawn_rpc`, for the
// device-list calls) spawned it on
// indefinitely — never gpui's main thread, which is the one invariant that
// actually matters (see that fn's own header comment). A future round could
// add a kill-after-N budget the same shape `CONNECT_TIMEOUT` gives `nm.rs`
// if this ever proves to matter in practice; not added speculatively here.

use std::process::{Command, Output};

use super::{AudioBackend, AudioError, OutputDevice, VolumeState};

/// WirePlumber's own alias for "whatever sink is currently the system
/// default" — resolves without this module ever having to enumerate sink
/// object IDs itself.
const DEFAULT_SINK: &str = "@DEFAULT_AUDIO_SINK@";

pub(crate) struct WpctlAudioBackend;

impl WpctlAudioBackend {
    /// The reachability check `select_backend` (`mod.rs`) uses to decide
    /// `Real` vs. `Unavailable` — see this module's header comment for why
    /// `status` (a completed round-trip to the daemon), not `get-volume` and
    /// not a separate `which wpctl` call, is the probe.
    pub(crate) fn probe() -> Result<Self, AudioError> {
        let backend = Self;
        let output = backend.run(&["status"])?;
        backend.require_success(&output, "status")?;
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

    fn list_outputs(&self) -> Result<Vec<OutputDevice>, AudioError> {
        let output = self.run(&["status"])?;
        self.require_success(&output, "status")?;
        // Lossy on purpose: sink names come from hardware/firmware strings
        // and a vendor that ships invalid UTF-8 in a product name must
        // degrade to a replacement character in one row, never fail the
        // whole enumeration.
        Ok(parse_wpctl_sinks(&String::from_utf8_lossy(&output.stdout)))
    }

    fn set_default_output(&self, id: u32) -> Result<Vec<OutputDevice>, AudioError> {
        // `wpctl set-default <ID>` — WirePlumber picks the right default
        // *category* (Audio/Sink here) from the object's own media class, so
        // this one verb covers sinks without the shell having to say which
        // kind of default it means.
        let arg = id.to_string();
        let output = self.run(&["set-default", &arg])?;
        self.require_success(&output, "set-default")?;
        // Re-enumerate rather than patching the caller's list locally: the
        // authoritative answer to "which sink is default now" is the one the
        // daemon gives, and a switch that silently did not take must show up
        // as the old device still being marked. Same "read the real value
        // back" discipline this module's header comment documents for volume.
        self.list_outputs()
    }
}

/// Parses the audio `Sinks:` section of `wpctl status`. Pure and
/// unit-testable against captured stdout, same shape `parse_wpctl_volume`
/// below already establishes for the volume reply.
///
/// The shape being parsed is a two-level box-drawing tree. This is a
/// VERBATIM excerpt of a real WirePlumber 0.5.8 / PipeWire 1.4.2 capture
/// taken for this round (`tests::REAL_STATUS_CAPTURE` below is the whole
/// thing, not a paraphrase):
///
/// ```text
/// PipeWire 'pipewire-0' [1.4.2, root@host, cookie:312973461]
///  └─ Clients:
///         32. WirePlumber                      [1.4.2, root@host, pid:1190]
///
/// Audio
///  ├─ Devices:
///  ├─ Sinks:
///  │  *   50. Built-in Audio Analog Stereo     [vol: 0.65]
///  │      53. HDMI / DisplayPort               [vol: 1.00 MUTED]
///  ├─ Sources:
///  └─ Streams:
///
/// Video
///  ├─ Sinks:
/// ```
///
/// TWO nesting levels have to be tracked, and finding that out is what the
/// live capture was for: **`Video` has its own `Sinks:` section**. Matching
/// section headers by name alone — which an earlier draft of this function
/// did — reports a video sink as an audio output the moment a machine has
/// one. So a bare, un-indented word (`Audio` / `Video` / `Settings`, and the
/// `PipeWire '...'` banner) is a CATEGORY boundary that resets the section,
/// and only `Sinks:` inside `Audio` contributes.
///
/// Otherwise deliberately structural rather than regex-driven, and
/// deliberately tolerant, because this is READING another process's
/// human-facing output:
///   - the box-drawing gutter is stripped by character class, so a
///     WirePlumber release that changes which glyphs it draws does not
///     change the result
///   - an ENTRY is recognised before a SECTION HEADER, so a sink whose name
///     ends in `:` cannot be mistaken for the start of a new section
///   - `Sources:` / `Filters:` / `Devices:` / `Streams:` and Settings' own
///     `Default Configured Devices:` list are all skipped by the same
///     mechanism rather than by a per-section special case — including
///     `Filters:`, which is where a `pw-loopback` virtual sink shows up and
///     which really is not an output device this shell should offer
///   - an unparsable line is SKIPPED, never fatal — one malformed row must
///     not hide the other devices
///   - `wpctl`'s RTKit warnings go to stderr, so they never reach this
///     parser; if a future release moved them to stdout they would still be
///     ignored (they are neither entries nor headers)
///
/// An empty result is a legitimate answer (PipeWire running on a box with no
/// sound card) and is what makes the "服務在跑但沒有裝置" state reachable —
/// see this module's header comment on the probe.
fn parse_wpctl_sinks(raw: &str) -> Vec<OutputDevice> {
    let mut devices = Vec::new();
    let mut in_audio = false;
    let mut in_sinks = false;

    for line in raw.lines() {
        let content = strip_tree_gutter(line);
        if content.is_empty() {
            continue;
        }

        // A CATEGORY line: no indentation and no tree gutter at all. Ends
        // whatever section was open, whichever category it names.
        if is_category_line(line) {
            in_audio = content.eq_ignore_ascii_case("Audio");
            in_sinks = false;
            continue;
        }

        // Entry before header — see this fn's own doc comment on why the
        // order matters. An entry is never a section header, whatever it
        // ends with.
        if let Some(device) = parse_sink_entry(content) {
            if in_audio && in_sinks {
                devices.push(device);
            }
            continue;
        }

        if let Some(header) = content.strip_suffix(':') {
            in_sinks = header.trim().eq_ignore_ascii_case("Sinks");
        }
    }

    devices
}

/// Whether `line` is one of `wpctl status`'s top-level category lines
/// (`Audio`, `Video`, `Settings`, and the `PipeWire '...'` banner): flush
/// left, with no indentation and no tree gutter. Everything belonging to a
/// category is drawn indented under it, so "starts at column zero with a
/// non-gutter character" is the whole test.
fn is_category_line(line: &str) -> bool {
    line.chars().next().is_some_and(|c| !c.is_whitespace() && !is_gutter_char(c))
}

/// The box-drawing glyphs WirePlumber draws its tree with. Named as a
/// predicate rather than inlined because both `strip_tree_gutter` and
/// `is_category_line` have to agree on exactly this set.
fn is_gutter_char(c: char) -> bool {
    matches!(c, '│' | '├' | '└' | '┌' | '┐' | '┘' | '┤' | '┬' | '┴' | '┼' | '─')
}

/// Strips WirePlumber's tree-drawing gutter and surrounding whitespace.
/// Character-class based rather than prefix-matched so nesting depth and
/// glyph choice are both irrelevant.
fn strip_tree_gutter(line: &str) -> &str {
    line.trim_matches(|c: char| c.is_whitespace() || is_gutter_char(c))
}

/// One `[*] <id>. <name> [<detail>]` row, with the gutter already stripped.
/// `None` for anything that is not that shape (a section header, the
/// `PipeWire 'pipewire-0' [...]` banner, a blank gutter line).
fn parse_sink_entry(content: &str) -> Option<OutputDevice> {
    // `*` marks WirePlumber's current default. Its ABSENCE is not an error:
    // a machine can genuinely have sinks and no configured default.
    let (is_default, rest) = match content.strip_prefix('*') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, content),
    };

    let (id_text, rest) = rest.split_once('.')?;
    // Whole-token parse (not a prefix scan): a line like `Audio.Sink` must
    // not yield an id, and `parse` rejecting non-digits is what enforces it.
    let id: u32 = id_text.trim().parse().ok()?;

    let mut name = rest.trim();
    // Trailing `[vol: 0.65]` / `[alsa]` detail. Only stripped when the row
    // actually ENDS with the bracket group, so a device literally named
    // "Dock [rear]" keeps its name if nothing follows it... and, when a
    // detail does follow, loses only the last group.
    if name.ends_with(']') {
        if let Some(open) = name.rfind('[') {
            name = name[..open].trim_end();
        }
    }
    if name.is_empty() {
        return None;
    }

    Some(OutputDevice { id, name: name.to_string(), is_default })
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

    // ── `wpctl status` sink enumeration (D5) ────────────────────────────

    /// A VERBATIM `wpctl status` capture (WirePlumber 0.5.8 / PipeWire
    /// 1.4.2, Debian trixie arm64, 2026-08-24), taken by running the real
    /// daemons in a container with two `pw-loopback` virtual sinks attached.
    /// Not written by hand and not tidied up — this is the whole stdout.
    ///
    /// It is in this file because capturing it is what found the `Video`
    /// section bug (an earlier draft matched section headers by name alone
    /// and would have reported video sinks as audio outputs), and because
    /// three other things it shows would otherwise have been guesses:
    ///   * the categories really are flush-left bare words
    ///   * `pw-loopback` virtual sinks land under `Filters:`, NOT `Sinks:`
    ///     — including the one carrying the `*` default marker, which is
    ///     exactly the row a name-only parser would have wrongly offered
    ///   * a `Filters:` section also contains a non-entry `- <name>` label
    ///     line, which must not disturb anything
    const REAL_STATUS_CAPTURE: &str = "\
PipeWire 'pipewire-0' [1.4.2, root@712f7324a341, cookie:312973461]
 └─ Clients:
        32. WirePlumber                         [1.4.2, root@712f7324a341, pid:1190]
        40. WirePlumber [export]                [1.4.2, root@712f7324a341, pid:1190]
        41. pw-loopback                         [1.4.2, root@712f7324a341, pid:1197]
        48. wpctl                               [1.4.2, root@712f7324a341, pid:1202]

Audio
 ├─ Devices:
 │
 ├─ Sinks:
 │
 ├─ Sources:
 │
 ├─ Filters:
 │    - loopback-1197-19
 │      42. output.pw-loopback-1197                                      [Stream/Output/Audio]
 │  *   43. duduclaw_test_sink                                           [Audio/Sink]
 │
 └─ Streams:

Video
 ├─ Devices:
 │
 ├─ Sinks:
 │
 ├─ Sources:
 │
 ├─ Filters:
 │
 └─ Streams:

Settings
 └─ Default Configured Devices:
";

    /// A machine WITH real hardware sinks, in the shape the capture above
    /// establishes. Hand-composed (the capture container had no sound card)
    /// but structurally identical to it — same gutter, same category
    /// nesting, same `*` marker column, same trailing detail group.
    const STATUS_SAMPLE: &str = "\
PipeWire 'pipewire-0' [1.4.2, root@duduclaw-os, cookie:1516884236]
 └─ Clients:
        31. WirePlumber                         [1.4.2, root@duduclaw-os, pid:512]
        44. wpctl                               [1.4.2, root@duduclaw-os, pid:901]

Audio
 ├─ Devices:
 │      45. Built-in Audio                      [alsa]
 │
 ├─ Sinks:
 │  *   50. Built-in Audio Analog Stereo        [vol: 0.65]
 │      53. HDMI / DisplayPort                  [vol: 1.00 MUTED]
 │
 ├─ Sink endpoints:
 │
 ├─ Sources:
 │  *   52. Built-in Audio Analog Stereo        [vol: 1.00]
 │
 ├─ Source endpoints:
 │
 └─ Streams:

Video
 ├─ Devices:
 │      47. Integrated Camera                   [v4l2]
 │

Settings
 └─ Default Configured Devices:
        0. Audio/Sink    alsa_output.pci-0000_00_1f.3.analog-stereo
";

    #[test]
    fn parses_the_sinks_section_of_a_real_status_capture() {
        let sinks = parse_wpctl_sinks(STATUS_SAMPLE);
        assert_eq!(sinks.len(), 2, "exactly the two Sinks rows, not Sources/Devices/Settings");
        assert_eq!(sinks[0], OutputDevice { id: 50, name: "Built-in Audio Analog Stereo".to_string(), is_default: true });
        assert_eq!(sinks[1], OutputDevice { id: 53, name: "HDMI / DisplayPort".to_string(), is_default: false });
    }

    /// The bug this ordering exists to prevent: `Sources:` uses the same row
    /// shape and also carries a `*`, so a parser that kept reading past the
    /// section boundary would report a microphone as an output device.
    #[test]
    fn sources_are_never_reported_as_outputs() {
        let sinks = parse_wpctl_sinks(STATUS_SAMPLE);
        assert!(sinks.iter().all(|d| d.id != 52), "id 52 is a SOURCE (a microphone), not an output");
    }

    /// Settings' `Default Configured Devices:` rows parse as entries by
    /// shape (`0. Audio/Sink ...`) and must be excluded by section, not by
    /// a special case.
    #[test]
    fn the_settings_block_is_not_mistaken_for_devices() {
        let sinks = parse_wpctl_sinks(STATUS_SAMPLE);
        assert!(sinks.iter().all(|d| d.id != 0));
    }

    /// PipeWire up, no sound card: a legitimate, distinct answer — an empty
    /// list, not an error. This is the state that makes 「沒有可用的輸出裝置」
    /// reachable instead of the box blaming its own audio service.
    #[test]
    fn a_running_daemon_with_no_sinks_yields_an_empty_list_not_a_failure() {
        let raw = "\
PipeWire 'pipewire-0' [1.4.2, root@duduclaw-os, cookie:1]
 └─ Clients:
        31. WirePlumber                         [1.4.2, root@duduclaw-os, pid:512]

Audio
 ├─ Devices:
 │
 ├─ Sinks:
 │
 ├─ Sources:
 │
 └─ Streams:
";
        assert!(parse_wpctl_sinks(raw).is_empty());
    }

    #[test]
    fn a_machine_with_no_configured_default_still_lists_its_sinks() {
        let raw = "Audio\n ├─ Sinks:\n │      50. Only Sink                          [vol: 0.40]\n";
        let sinks = parse_wpctl_sinks(raw);
        assert_eq!(sinks.len(), 1);
        assert!(!sinks[0].is_default);
    }

    /// A device name that ends in a colon must not be read as the start of a
    /// new section — the reason entries are matched before headers.
    #[test]
    fn a_sink_named_with_a_trailing_colon_does_not_end_the_section() {
        let raw = "Audio\n ├─ Sinks:\n │  *   50. Dock:                               [vol: 0.50]\n │      51. Speakers                            [vol: 0.30]\n";
        let sinks = parse_wpctl_sinks(raw);
        assert_eq!(sinks.len(), 2, "the colon-suffixed name must not close the Sinks section");
        assert_eq!(sinks[0].name, "Dock:");
    }

    #[test]
    fn only_the_trailing_detail_group_is_stripped_from_a_name() {
        let raw = "Audio\n ├─ Sinks:\n │      50. Dock [rear] output                   [vol: 0.50]\n";
        assert_eq!(parse_wpctl_sinks(raw)[0].name, "Dock [rear] output");
    }

    #[test]
    fn a_name_with_no_detail_group_survives_intact() {
        let raw = "Audio\n ├─ Sinks:\n │      50. Speakers\n";
        assert_eq!(parse_wpctl_sinks(raw)[0].name, "Speakers");
    }

    /// One malformed row must cost one row, not the whole enumeration.
    #[test]
    fn an_unparsable_row_is_skipped_and_the_rest_survive() {
        let raw = "Audio\n ├─ Sinks:\n │      not-an-id. Broken                        [vol: 0.50]\n │      51. Good                               [vol: 0.30]\n";
        let sinks = parse_wpctl_sinks(raw);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].id, 51);
    }

    #[test]
    fn empty_and_garbage_input_produce_no_devices() {
        assert!(parse_wpctl_sinks("").is_empty());
        assert!(parse_wpctl_sinks("Could not connect to PipeWire\n").is_empty());
    }

    /// Section matching is case-insensitive but still a WHOLE-word match:
    /// `Sink endpoints:` is a different section and must not turn the parser
    /// on (unanchored `contains("Sink")` is exactly the class of bug this
    /// project's coding convention 2 forbids).
    #[test]
    fn sink_endpoints_is_a_different_section() {
        let raw = " ├─ Sink endpoints:\n │      99. Endpoint                            [vol: 1.00]\n";
        assert!(parse_wpctl_sinks(raw).is_empty());
    }

    // ── against the verbatim live capture (D5, 2026-08-24) ──────────────

    /// The capture's machine genuinely has no hardware sink — its only
    /// audio-ish nodes are `pw-loopback` filters. Reporting zero is the
    /// correct answer, and it is the answer an earlier draft got WRONG:
    /// matching `Sinks:` by name alone, and treating a `Filters:` row as a
    /// sink, would both have produced phantom devices here.
    #[test]
    fn the_live_capture_reports_no_hardware_sinks() {
        assert_eq!(parse_wpctl_sinks(REAL_STATUS_CAPTURE), Vec::new());
    }

    /// The `*`-marked `43. duduclaw_test_sink [Audio/Sink]` row in the
    /// capture is a `pw-loopback` FILTER, not an output. It carries both the
    /// default marker and an `Audio/Sink` media class, so it is the single
    /// most convincing-looking wrong answer in the whole file.
    #[test]
    fn a_loopback_filter_is_not_offered_as_an_output_even_when_it_is_default() {
        let ids: Vec<u32> = parse_wpctl_sinks(REAL_STATUS_CAPTURE).into_iter().map(|d| d.id).collect();
        assert!(!ids.contains(&43), "id 43 is a Filters row, not a Sinks row");
        assert!(!ids.contains(&42));
    }

    /// THE bug the live capture found. `Video` has its own `Sinks:` section,
    /// so a parser that matches section headers by name alone starts
    /// collecting again after the audio tree has ended.
    #[test]
    fn a_video_sink_is_never_reported_as_an_audio_output() {
        let raw = "\
Audio
 ├─ Sinks:
 │  *   50. Real Speakers                       [vol: 0.40]
 └─ Streams:

Video
 ├─ Sinks:
 │      77. Some Video Sink                     [v4l2]
 └─ Streams:
";
        let sinks = parse_wpctl_sinks(raw);
        assert_eq!(sinks.len(), 1, "only the Audio category's sinks count");
        assert_eq!(sinks[0].id, 50);
    }

    /// Sinks listed before any category line appears (a truncated capture,
    /// or a future format that drops the banner) are NOT attributed to
    /// audio. Fail-closed: an unattributable device is not offered.
    #[test]
    fn sinks_outside_any_audio_category_are_not_collected() {
        let raw = " ├─ Sinks:\n │      50. Orphan                              [vol: 0.40]\n";
        assert!(parse_wpctl_sinks(raw).is_empty(), "a Sinks section with no owning category must not be assumed to be audio");
    }

    #[test]
    fn category_lines_are_exactly_the_flush_left_ones() {
        assert!(is_category_line("Audio"));
        assert!(is_category_line("Video"));
        assert!(is_category_line("Settings"));
        assert!(is_category_line("PipeWire 'pipewire-0' [1.4.2, root@host, cookie:1]"));
        assert!(!is_category_line(" ├─ Sinks:"));
        assert!(!is_category_line(" │  *   43. duduclaw_test_sink"));
        assert!(!is_category_line("        32. WirePlumber"));
        assert!(!is_category_line(""));
    }

    /// The label line inside `Filters:` (`- loopback-1197-19`) is neither an
    /// entry nor a header and must simply pass through.
    #[test]
    fn a_filters_label_line_disturbs_nothing() {
        let raw = "\
Audio
 ├─ Sinks:
 │  *   50. Real Speakers                       [vol: 0.40]
 ├─ Filters:
 │    - loopback-1197-19
 │      42. output.pw-loopback-1197             [Stream/Output/Audio]
";
        let sinks = parse_wpctl_sinks(raw);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].id, 50);
    }
}
