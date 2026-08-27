//! Y10-1: agent→audio gateway bridge — the "耳朵與嘴" agent-body surface
//! (volume/mute/output-device), built on the exact pattern A7c established
//! for `display_bridge.rs` (see this crate's `display_bridge` module doc and
//! `commercial/docs/DESIGN-os-self-drive-2026-08.md` §11 first). Read that
//! module's doc before this one — everything below is either "same shape,
//! restated for audio" or an explicit, justified divergence.
//!
//! ## Why this does NOT touch `duduclaw-comp` at all
//! A7c's display group had to fix a boundary INSIDE the compositor
//! (`shell_control::listener::classify_peer`'s new `PeerAuthority::Agent`
//! tier) because cursor/theme/output-scale are compositor-owned state, only
//! reachable through comp's own socket. Audio is not: `crates/duduclaw-shell/
//! src/audio/wpctl.rs`'s own module doc establishes that PipeWire has no
//! D-Bus surface and no relationship to Wayland — the shell's own volume
//! slider already talks to the `wpctl` CLI as a bare subprocess, entirely
//! independent of `duduclaw-comp`'s `shell_control` socket (which has no
//! audio op today — confirmed by reading `shell_control/protocol.rs`'s
//! closed `ShellControlRequest` enum before writing this module). Routing
//! audio through comp would mean teaching the compositor a THIRD unrelated
//! subsystem (after Wayland surfaces and… nothing else) just to re-shell-out
//! to the exact same `wpctl` binary this module already calls directly —
//! more surface, not less, for zero new capability. So the "minimal safe
//! solution" for audio skips the comp hop entirely: this module IS the
//! agent's audio backend, the same way `duduclaw-shell::audio::wpctl` is the
//! human UI's.
//!
//! ## The one real boundary: which `XDG_RUNTIME_DIR` sees the socket
//! `wpctl` (via libpipewire) resolves the running daemon at
//! `$XDG_RUNTIME_DIR/pipewire-0`. On both shipping images PipeWire/
//! WirePlumber are started BY the kiosk session itself
//! (`duduclaw-kiosk-launch.sh`) with `XDG_RUNTIME_DIR=/run/duduclaw-kiosk`
//! pinned (the same systemd units `display_bridge.rs`'s doc cites for comp's
//! socket) — so a caller with a different or unset `XDG_RUNTIME_DIR` (an
//! agent's CLI subprocess, or this crate's own out-of-process
//! `duduclaw mcp-server`) spawns a `wpctl` that looks in the wrong place and
//! reports the daemon unreachable, restated for a different socket than
//! `os_drive/display.rs`'s original uid-boundary finding.
//!
//! The fix is the SAME "fixed, deployment-known path works from any process"
//! idea `display_bridge.rs` uses, but collapsed into ONE function with an
//! internal two-tier attempt rather than two independently hand-rolled
//! client modules (`os_drive/display.rs` + `display_bridge.rs`): there is no
//! second wire protocol to duplicate here — both attempts are the exact same
//! `wpctl` subprocess invocation, differing only in one env var. See
//! [`run_wpctl`] / [`two_tier`].
//!
//! ## DAC, not `SO_PEERCRED` — and why there is no verb allowlist here
//! comp's socket layers an application-level `SO_PEERCRED` identity check
//! (`classify_peer` → `PeerAuthority`) on top of filesystem access; PipeWire
//! has none. Reaching its socket AT ALL — a plain Unix DAC decision on
//! `/run/duduclaw-kiosk`'s permission bits — is the ENTIRE authorization
//! story, and once reached, any client may call any `wpctl` verb. On the
//! Debian appliance line `duduclaw-kiosk.service`'s `RuntimeDirectory` is
//! mode 0700, owned by `duduclaw-kiosk`; root (today's Yocto gateway — see
//! `display_bridge.rs`'s doc and A7c §11.1's own finding: "the Yocto
//! `duduclaw-gateway.service` currently has no `User=` line") bypasses DAC
//! and reaches it, while a non-root `User=duduclaw` gateway (the Debian
//! line, `postinst.d/20-users-and-units.sh`: `duduclaw` is never added to
//! the `duduclaw-kiosk` group) does not, and this module reports the same
//! honest "connection failed" a genuinely-missing daemon would, never a
//! silent hang or a fabricated reading. This is NOT a regression introduced
//! here — it is the identical disclosed gap A7c's own `classify_peer` doc
//! already carries for comp's socket (the documented forward path is
//! `DUDUCLAW_SHELL_CONTROL_AGENT_UID` once the Debian gateway either gains
//! an explicit grant or the directory story is revisited), restated
//! honestly for a second resource rather than silently worked around.
//!
//! Because PipeWire has no verb-level authorization layer to extend (unlike
//! comp's closed `agent_allowed` allowlist), the scope narrowing for "what
//! an agent may do to this box's audio" lives entirely in which `wpctl`
//! verbs THIS module chooses to expose: get/set volume, mute TOGGLE (not an
//! explicit on/off — matches `duduclaw-shell`'s own `AudioBackend::
//! toggle_mute` precedent, which has no set-to-value verb either), and
//! list/switch the default output. There is no `wpctl` verb omitted here
//! that would let an agent do something more dangerous with audio (no
//! "erase all sound settings" / "delete a device" verb exists to guard
//! against).
//!
//! ## Scope (matches A7a design doc §5's spirit exactly —
//! `requires_approval=false`)
//! Volume/mute/output-device selection are the audio twin of A7c's cursor/
//! theme/output-scale: reversible, low-risk appearance-adjacent preferences,
//! not destructive machine operations. No `ApprovalBroker` gate.

use serde_json::{json, Value};

/// Same fixed path `display_bridge::KIOSK_RUNTIME_DIR` uses — see this
/// module's doc for why audio needs the identical fallback. Duplicated
/// rather than shared: two independent, small modules each owning one
/// literal is cheaper than a third "kiosk paths" module coordinating both.
const KIOSK_RUNTIME_DIR: &str = "/run/duduclaw-kiosk";

/// WirePlumber's alias for "whatever sink is currently the system default"
/// — same literal `duduclaw-shell::audio::wpctl::DEFAULT_SINK` uses,
/// duplicated for the same "don't depend on the gpui-heavy shell crate"
/// reason `display_bridge.rs`'s doc gives for not depending on
/// `duduclaw-comp`.
const DEFAULT_SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// The `wpctl` binary name in production. A constant (not a literal at each
/// call site) so [`run_wpctl_bin_with_env`]'s test-only binary-name override
/// is obviously the ONLY place a different value is ever used.
const WPCTL_BIN: &str = "wpctl";

/// One output device (a PipeWire *sink*), the audio twin of
/// `duduclaw-shell::audio::OutputDevice`. `id` is `wpctl`'s own object id,
/// opaque to a caller and only ever handed back to `set_default_output`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioOutput {
    id: u32,
    name: String,
    is_default: bool,
}

impl AudioOutput {
    fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "is_default": self.is_default })
    }
}

// ── Subprocess plumbing ────────────────────────────────────────────────────

/// Runs `<bin> <args>` with an optional `XDG_RUNTIME_DIR` override. The ONE
/// point where a real subprocess is spawned — `bin` is parameterized purely
/// for testability (deterministic spawn-failure / non-zero-exit / success
/// coverage using real, always-present binaries like `false`/`echo`, with no
/// dependency on `wpctl` — or PipeWire — actually being installed on the
/// machine running `cargo test`, which `display_bridge.rs`'s stub-socket
/// tests achieve for comp's protocol but has no subprocess equivalent here).
/// Production call sites always go through [`run_wpctl_with_env`], which
/// hardcodes [`WPCTL_BIN`].
async fn run_wpctl_bin_with_env(
    bin: &str,
    args: &[&str],
    xdg_runtime_override: Option<&str>,
) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args);
    if let Some(dir) = xdg_runtime_override {
        cmd.env("XDG_RUNTIME_DIR", dir);
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to spawn {bin}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{bin} {args:?} exited with {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_wpctl_with_env(args: &[&str], xdg_runtime_override: Option<&str>) -> Result<String, String> {
    run_wpctl_bin_with_env(WPCTL_BIN, args, xdg_runtime_override).await
}

/// Generic two-tier retry: adopt `primary` if it succeeded, otherwise await
/// `fallback` and adopt (or fail with) that instead. Pure control flow, no
/// subprocess awareness — mirrors `duduclaw-cli`'s own
/// `os_drive::with_gateway_display_fallback` shape exactly so it is testable
/// with hand-written `Ok`/`Err` values, no real `wpctl` required.
///
/// **Deliberate divergence from `with_gateway_display_fallback`**: that
/// function surfaces the PRIMARY error when both attempts fail, because it
/// exists to preserve `os_drive/display.rs`'s already-shipped, human-facing
/// uid-boundary error text over a bridge error a human operator rarely
/// needs to see. There is no equivalent shipped CLI surface here to
/// preserve — this module is the FIRST implementation of agent-reachable
/// audio, used identically by both the CLI and the MCP tool — so the
/// FALLBACK's error (the fixed kiosk path) is the one that surfaces: it
/// matches this module's documented real deployment story (an agent/
/// mcp-server caller has no correct ambient `XDG_RUNTIME_DIR` to begin
/// with, so the ambient attempt's error is nearly always the uninformative
/// "wrong/no runtime dir" case, not a useful diagnostic).
async fn two_tier<F, Fut>(primary: Result<String, String>, fallback: F) -> Result<String, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let primary_err = match primary {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    match fallback().await {
        Ok(v) => Ok(v),
        Err(fallback_err) => {
            tracing::debug!(
                ambient_error = %primary_err,
                fixed_path_error = %fallback_err,
                "audio_bridge: both the ambient-environment and fixed kiosk-path wpctl \
                 attempts failed — surfacing the fixed-path error (see two_tier's own doc \
                 comment for why that one, not the ambient one, is more actionable here)"
            );
            Err(fallback_err)
        }
    }
}

async fn run_wpctl(args: &[&str]) -> Result<String, String> {
    let ambient = run_wpctl_with_env(args, None).await;
    two_tier(ambient, || run_wpctl_with_env(args, Some(KIOSK_RUNTIME_DIR))).await
}

// ── Pure output parsing (ported from `duduclaw-shell::audio::wpctl` — same
//    wire format, same edge cases, duplicated rather than imported for the
//    same "don't pull the gpui-heavy shell crate into gateway" reason
//    `display_bridge.rs`'s doc gives for not depending on `duduclaw-comp`) ──

/// Parses `wpctl get-volume @DEFAULT_AUDIO_SINK@`'s stdout
/// (`"Volume: 0.75\n"` / `"Volume: 0.75 [MUTED]\n"`). See
/// `duduclaw-shell::audio::wpctl::parse_wpctl_volume`'s doc comment for the
/// full resilience contract this ports verbatim (tolerant of a missing
/// prefix, comma decimals, extra whitespace; clamps a boosted volume to
/// `0..=100`; rejects empty/negative/non-finite input).
fn parse_volume(raw: &str) -> Option<(u8, bool)> {
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
    Some((pct_u8, muted))
}

/// Parses the `Audio` › `Sinks:` section of `wpctl status`. Ports
/// `duduclaw-shell::audio::wpctl::parse_wpctl_sinks` verbatim — see that
/// function's doc comment (and its `REAL_STATUS_CAPTURE` test fixture, also
/// ported below) for the full box-drawing-tree parsing rationale: category
/// boundaries reset the section (so `Video`'s own `Sinks:` is never
/// conflated with `Audio`'s), entries are matched before headers (so a sink
/// literally named with a trailing colon cannot masquerade as closing the
/// section), and an unparsable row is skipped rather than fatal.
fn parse_sinks(raw: &str) -> Vec<AudioOutput> {
    let mut devices = Vec::new();
    let mut in_audio = false;
    let mut in_sinks = false;

    for line in raw.lines() {
        let content = strip_tree_gutter(line);
        if content.is_empty() {
            continue;
        }

        if is_category_line(line) {
            in_audio = content.eq_ignore_ascii_case("Audio");
            in_sinks = false;
            continue;
        }

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

fn is_category_line(line: &str) -> bool {
    line.chars().next().is_some_and(|c| !c.is_whitespace() && !is_gutter_char(c))
}

fn is_gutter_char(c: char) -> bool {
    matches!(c, '│' | '├' | '└' | '┌' | '┐' | '┘' | '┤' | '┬' | '┴' | '┼' | '─')
}

fn strip_tree_gutter(line: &str) -> &str {
    line.trim_matches(|c: char| c.is_whitespace() || is_gutter_char(c))
}

fn parse_sink_entry(content: &str) -> Option<AudioOutput> {
    let (is_default, rest) = match content.strip_prefix('*') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, content),
    };

    let (id_text, rest) = rest.split_once('.')?;
    let id: u32 = id_text.trim().parse().ok()?;

    let mut name = rest.trim();
    if name.ends_with(']') {
        if let Some(open) = name.rfind('[') {
            name = name[..open].trim_end();
        }
    }
    if name.is_empty() {
        return None;
    }

    Some(AudioOutput { id, name: name.to_string(), is_default })
}

// ── Granular ops (mirror `duduclaw-shell::audio::AudioBackend`'s verbs) ────

async fn get_volume() -> Result<(u8, bool), String> {
    let raw = run_wpctl(&["get-volume", DEFAULT_SINK]).await?;
    parse_volume(&raw).ok_or_else(|| format!("unparsable wpctl get-volume output: {raw:?}"))
}

async fn set_volume(pct: u8) -> Result<(u8, bool), String> {
    let clamped = pct.min(100);
    let arg = format!("{clamped}%");
    run_wpctl(&["set-volume", DEFAULT_SINK, &arg]).await?;
    get_volume().await
}

/// `wpctl set-mute … toggle` — TOGGLE only, no explicit on/off. Matches
/// `duduclaw-shell::audio::AudioBackend::toggle_mute`'s own precedent
/// exactly (that trait has no set-to-value mute verb either) and the task
/// brief's own framing ("靜音 toggle"). An agent that needs a specific
/// target state reads it first via [`audio_get`] and only calls this when
/// the current state differs from what it wants — the same "read, then act"
/// dialogue shape the A7c display flow already establishes.
async fn toggle_mute() -> Result<(u8, bool), String> {
    run_wpctl(&["set-mute", DEFAULT_SINK, "toggle"]).await?;
    get_volume().await
}

async fn list_outputs() -> Result<Vec<AudioOutput>, String> {
    let raw = run_wpctl(&["status"]).await?;
    Ok(parse_sinks(&raw))
}

/// Re-enumerates rather than patching the caller's list locally — same
/// "never trust the write's own success alone, read the real value back"
/// discipline `duduclaw-shell::audio::wpctl::WpctlAudioBackend::
/// set_default_output`'s own doc comment documents.
async fn set_default_output(id: u32) -> Result<Vec<AudioOutput>, String> {
    let arg = id.to_string();
    run_wpctl(&["set-default", &arg]).await?;
    list_outputs().await
}

// ── The `os_audio_get`/`os_audio_set` surface (MCP tool + CLI shape) ──────

/// Closed set of fields `audio_set` accepts — never a free-form `wpctl`
/// verb name, same "closed set, refuse rather than pass through" policy
/// `display_bridge::DISPLAY_SET_FIELDS` establishes.
pub const AUDIO_SET_FIELDS: [&str; 3] = ["volume", "mute", "output"];

/// `os_audio_get` — everything `audio_set` can change: current volume/mute,
/// and every output device (with which one is default) — what "現況：音量
/// 40%" needs to answer before an agent decides how much to turn it up.
pub async fn audio_get() -> Result<Value, String> {
    let (pct, muted) = get_volume().await?;
    let outputs = list_outputs().await?;
    Ok(json!({
        "volume_pct": pct,
        "muted": muted,
        "outputs": outputs.iter().map(AudioOutput::to_json).collect::<Vec<_>>(),
    }))
}

/// `os_audio_set` — dispatches on `field` (one of [`AUDIO_SET_FIELDS`])
/// against `value` (always a string on the wire; parsed per-field). Unknown
/// fields and unparseable/out-of-range values are refused HERE, before any
/// subprocess spawn — defense in depth, same "fail closed on the
/// caller-visible side too" convention `display_bridge::display_set`
/// follows (coding convention #4).
pub async fn audio_set(field: &str, value: &str) -> Result<Value, String> {
    match field {
        "volume" => {
            let pct: u8 = value
                .trim()
                .parse()
                .map_err(|_| format!("volume 必須是 0-100 的整數，收到：{value:?}"))?;
            if pct > 100 {
                return Err(format!("volume 必須介於 0-100，收到：{pct}"));
            }
            let (pct, muted) = set_volume(pct).await?;
            Ok(json!({ "volume_pct": pct, "muted": muted }))
        }
        "mute" => {
            if value.trim() != "toggle" {
                return Err(format!(
                    "mute 目前只支援 \"toggle\"（讀取目前狀態請用 os_audio_get），收到：{value:?}"
                ));
            }
            let (pct, muted) = toggle_mute().await?;
            Ok(json!({ "volume_pct": pct, "muted": muted }))
        }
        "output" => {
            let id: u32 = value
                .trim()
                .parse()
                .map_err(|_| format!("output 必須是裝置 id（整數，來自 os_audio_get 的 outputs[].id），收到：{value:?}"))?;
            let outputs = set_default_output(id).await?;
            Ok(json!({ "outputs": outputs.iter().map(AudioOutput::to_json).collect::<Vec<_>>() }))
        }
        other => Err(format!(
            "未知的 audio 欄位：{other:?}（合法值：{}）",
            AUDIO_SET_FIELDS.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── `two_tier` retry logic — pure, no subprocess ────────────────────

    #[tokio::test]
    async fn a_successful_primary_never_polls_the_fallback() {
        let result = two_tier(Ok("ambient-ok".to_string()), || async {
            panic!("fallback must not be polled when the ambient attempt already succeeded")
        })
        .await;
        assert_eq!(result, Ok("ambient-ok".to_string()));
    }

    #[tokio::test]
    async fn a_failed_primary_falls_back_and_succeeds() {
        let result = two_tier(Err("ambient failed".to_string()), || async {
            Ok("fixed-path-ok".to_string())
        })
        .await;
        assert_eq!(result, Ok("fixed-path-ok".to_string()));
    }

    /// Deliberately the OPPOSITE assertion direction from
    /// `os_drive::with_gateway_display_fallback`'s equivalent test — see
    /// `two_tier`'s own doc comment for why this module surfaces the
    /// FALLBACK's error, not the primary's, when both fail.
    #[tokio::test]
    async fn both_failing_surfaces_the_fallbacks_error_not_the_primarys() {
        let result = two_tier(Err("ambient: wrong XDG_RUNTIME_DIR".to_string()), || async {
            Err("fixed path: pipewire unreachable".to_string())
        })
        .await;
        let err = result.unwrap_err();
        assert!(err.contains("pipewire unreachable"), "unexpected: {err}");
        assert!(!err.contains("wrong XDG_RUNTIME_DIR"), "unexpected: {err}");
    }

    // ── `run_wpctl_bin_with_env` — real subprocess, deterministic binaries ──
    // No dependency on `wpctl`/PipeWire actually being installed: `false`/
    // `echo` exist on every Unix `cargo test` runs on (macOS dev loop,
    // Linux CI), which is what makes these portable and deterministic.

    #[tokio::test]
    #[cfg(unix)]
    async fn a_missing_binary_is_an_honest_spawn_error_not_a_panic() {
        let err = run_wpctl_bin_with_env("duduclaw-definitely-not-a-real-binary-xyz", &["status"], None)
            .await
            .unwrap_err();
        assert!(err.contains("failed to spawn"), "unexpected: {err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_nonzero_exit_is_captured_as_an_error_with_the_exit_code() {
        // `false` always exits 1 and prints nothing — a real, deterministic
        // stand-in for "wpctl ran but the daemon refused the request".
        let err = run_wpctl_bin_with_env("false", &["status"], None).await.unwrap_err();
        assert!(err.contains("exited with"), "unexpected: {err}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_successful_run_returns_captured_stdout() {
        let out = run_wpctl_bin_with_env("echo", &["hello-audio-bridge"], None).await.unwrap();
        assert!(out.contains("hello-audio-bridge"), "unexpected: {out:?}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn the_xdg_runtime_dir_override_is_actually_passed_to_the_child() {
        // `env` (coreutils) prints the child's own environment — proves the
        // override reaches the subprocess rather than being silently dropped.
        let out = run_wpctl_bin_with_env("env", &[], Some("/run/duduclaw-kiosk")).await.unwrap();
        assert!(
            out.contains("XDG_RUNTIME_DIR=/run/duduclaw-kiosk"),
            "unexpected env dump: {out:?}"
        );
    }

    // ── `parse_volume` — ported from duduclaw-shell::audio::wpctl's own
    //    exhaustive test table ──────────────────────────────────────────

    #[test]
    fn parses_plain_volume_without_mute() {
        assert_eq!(parse_volume("Volume: 0.75\n"), Some((75, false)));
    }

    #[test]
    fn parses_muted_volume_with_bracket_suffix() {
        assert_eq!(parse_volume("Volume: 0.75 [MUTED]\n"), Some((75, true)));
    }

    #[test]
    fn clamps_boosted_volume_above_100_percent() {
        assert_eq!(parse_volume("Volume: 1.50\n"), Some((100, false)));
    }

    #[test]
    fn tolerates_comma_decimal_separator_and_missing_prefix() {
        assert_eq!(parse_volume("Volume: 0,33\n"), Some((33, false)));
        assert_eq!(parse_volume("0.60 [MUTED]\n"), Some((60, true)));
    }

    #[test]
    fn rejects_empty_negative_and_non_finite_input() {
        assert_eq!(parse_volume(""), None);
        assert_eq!(parse_volume("Volume: -0.10\n"), None);
        assert_eq!(parse_volume("Volume: nan\n"), None);
        assert_eq!(parse_volume("not a volume at all"), None);
    }

    // ── `parse_sinks` — ported from duduclaw-shell::audio::wpctl's real
    //    WirePlumber 0.5.8 / PipeWire 1.4.2 capture ──────────────────────

    const STATUS_SAMPLE: &str = "\
PipeWire 'pipewire-0' [1.4.2, root@duduclaw-os, cookie:1516884236]
 └─ Clients:
        31. WirePlumber                         [1.4.2, root@duduclaw-os, pid:512]

Audio
 ├─ Devices:
 │      45. Built-in Audio                      [alsa]
 │
 ├─ Sinks:
 │  *   50. Built-in Audio Analog Stereo        [vol: 0.65]
 │      53. HDMI / DisplayPort                  [vol: 1.00 MUTED]
 │
 ├─ Sources:
 │  *   52. Built-in Audio Analog Stereo        [vol: 1.00]
 │
 └─ Streams:

Video
 ├─ Devices:
 │      47. Integrated Camera                   [v4l2]

Settings
 └─ Default Configured Devices:
        0. Audio/Sink    alsa_output.pci-0000_00_1f.3.analog-stereo
";

    #[test]
    fn parses_the_sinks_section_of_a_real_status_capture() {
        let sinks = parse_sinks(STATUS_SAMPLE);
        assert_eq!(sinks.len(), 2, "exactly the two Sinks rows, not Sources/Devices/Settings");
        assert_eq!(sinks[0], AudioOutput { id: 50, name: "Built-in Audio Analog Stereo".to_string(), is_default: true });
        assert_eq!(sinks[1], AudioOutput { id: 53, name: "HDMI / DisplayPort".to_string(), is_default: false });
    }

    #[test]
    fn sources_are_never_reported_as_outputs() {
        assert!(parse_sinks(STATUS_SAMPLE).iter().all(|d| d.id != 52), "id 52 is a SOURCE, not an output");
    }

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
        let sinks = parse_sinks(raw);
        assert_eq!(sinks.len(), 1, "only the Audio category's sinks count");
        assert_eq!(sinks[0].id, 50);
    }

    #[test]
    fn a_running_daemon_with_no_sinks_yields_an_empty_list_not_a_failure() {
        let raw = "Audio\n ├─ Devices:\n ├─ Sinks:\n ├─ Sources:\n └─ Streams:\n";
        assert!(parse_sinks(raw).is_empty());
    }

    #[test]
    fn an_unparsable_row_is_skipped_and_the_rest_survive() {
        let raw = "Audio\n ├─ Sinks:\n │      not-an-id. Broken                        [vol: 0.50]\n │      51. Good                               [vol: 0.30]\n";
        let sinks = parse_sinks(raw);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].id, 51);
    }

    #[test]
    fn empty_and_garbage_input_produce_no_devices() {
        assert!(parse_sinks("").is_empty());
        assert!(parse_sinks("Could not connect to PipeWire\n").is_empty());
    }

    // ── `audio_set` — field/value validation before any subprocess ─────

    #[tokio::test]
    async fn audio_set_rejects_an_unknown_field_before_touching_any_subprocess() {
        let err = audio_set("not_a_real_field", "1").await.unwrap_err();
        assert!(err.contains("未知的 audio 欄位"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn audio_set_rejects_a_non_integer_volume_before_touching_any_subprocess() {
        let err = audio_set("volume", "loud").await.unwrap_err();
        assert!(err.contains("volume"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn audio_set_rejects_an_out_of_range_volume_before_touching_any_subprocess() {
        let err = audio_set("volume", "150").await.unwrap_err();
        assert!(err.contains("0-100"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn audio_set_rejects_a_negative_volume_before_touching_any_subprocess() {
        let err = audio_set("volume", "-5").await.unwrap_err();
        assert!(err.contains("volume"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn audio_set_rejects_a_mute_value_other_than_toggle_before_touching_any_subprocess() {
        let err = audio_set("mute", "true").await.unwrap_err();
        assert!(err.contains("toggle"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn audio_set_rejects_a_non_integer_output_id_before_touching_any_subprocess() {
        let err = audio_set("output", "speakers").await.unwrap_err();
        assert!(err.contains("output"), "unexpected: {err}");
    }
}
