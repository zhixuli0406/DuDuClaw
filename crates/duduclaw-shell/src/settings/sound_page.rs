// D4b — 聲音 (sound).
//
// ── Why this page is mostly an honest "not yet" ────────────────────────
// PipeWire is NOT in the DuDuClaw OS image. That is a fact about the
// appliance, not a gap in this page: audio is work package D5 and has not
// been started. The trap this page exists to avoid is the one
// `crate::audio::select_backend` sets by design — it FAILS OPEN to
// `FakeAudioBackend` when `wpctl` cannot be probed, because ControlCenter's
// slider must stay draggable on a dev Mac. That is right for a quick
// toggle and completely wrong for a settings page: a slider that moves and
// changes nothing is exactly the "假資料" this app forbids.
//
// So this page does its OWN probe and reports what it finds, in four
// distinguishable states, and only offers controls in the one state where
// they would do something. It never constructs an audio backend on the
// not-installed paths, so it can never be handed a fake one.
//
// ── The interface left for D5 ──────────────────────────────────────────
// When PipeWire lands, `Availability::Available` becomes reachable on a real
// appliance and this page grows the output-device picker / volume / mute
// controls in that branch. Everything else here — the probe, the four
// states, the copy for the other three — stays as it is. Nothing about the
// wiring changes, which is the point of shipping the probe now.

use gpui::{prelude::*, Context, Div};

use super::widgets::{self, Tone};
use super::{spawn_rpc, Load};
use crate::audio::{AudioBackendKind, AudioUiState};
use crate::palette::ShellPalette;
use crate::ShellView;

/// The PipeWire client socket, relative to `$XDG_RUNTIME_DIR`. This is the
/// name PipeWire's own default `core.name` produces and what every client
/// (including `wpctl`) connects to.
const PIPEWIRE_SOCKET: &str = "pipewire-0";

/// The WirePlumber CLI `crate::audio::wpctl` shells out to. Its presence is
/// what distinguishes "the audio stack is not installed" from "it is
/// installed but not running".
const WPCTL_BINARY: &str = "wpctl";

/// What the probe found. Four states, because the operator's next action
/// differs for each one — which is the same test `settings/mod.rs`'s honesty
/// contract applies everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Availability {
    /// Not a Linux machine at all (this crate's macOS dev loop).
    NotSupportedHere,
    /// Linux, but neither the control tool nor the socket is present.
    NotInstalled,
    /// The control tool is installed but no PipeWire session is running.
    NotRunning,
    /// Both present — real audio control is possible.
    Available,
}

/// What one probe run observed. Kept as data rather than collapsed straight
/// into `Availability` so the classification below stays pure and testable
/// on a machine where none of these things exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Probe {
    pub(crate) is_linux: bool,
    pub(crate) wpctl_found: bool,
    pub(crate) socket_found: bool,
}

/// Pure: observation -> state.
///
/// Note the socket, not the binary, is what decides `Available`: `wpctl` on
/// `$PATH` with no session running produces a tool that connects to nothing.
pub(crate) fn classify(probe: Probe) -> Availability {
    if !probe.is_linux {
        return Availability::NotSupportedHere;
    }
    match (probe.wpctl_found, probe.socket_found) {
        (true, true) => Availability::Available,
        (false, false) => Availability::NotInstalled,
        // The two mixed cases both mean "half a stack": a tool with no
        // session, or a session we have no tool to drive. Neither may be
        // advertised as available — offering controls that cannot execute is
        // the dishonest half of this decision — and neither is "not
        // installed", because something IS there.
        _ => Availability::NotRunning,
    }
}

/// Runs the real observation. Blocking (two filesystem walks); called from a
/// background thread via `spawn_rpc`, same contract as every other page.
pub(crate) fn probe_now() -> Probe {
    Probe {
        is_linux: cfg!(target_os = "linux"),
        wpctl_found: binary_on_path(WPCTL_BINARY),
        socket_found: pipewire_socket_present(),
    }
}

/// Whether `name` resolves to an existing file on `$PATH`. Deliberately does
/// NOT execute it — running an unknown binary just to learn it exists is a
/// side effect a settings page has no business causing.
fn binary_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

fn pipewire_socket_present() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|dir| std::path::PathBuf::from(dir).join(PIPEWIRE_SOCKET).exists())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SoundPageState {
    /// `Load` for symmetry with every other page, even though the probe
    /// cannot "fail" — it can only observe. `Failed` is unreachable here and
    /// that is fine; a bespoke three-state enum would buy nothing.
    pub(crate) availability: Load<Availability>,
}

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.sound.availability.needs_load() {
        return;
    }
    view.settings_ui.sound.availability = Load::Loading;
    spawn_rpc(
        cx,
        || classify(probe_now()),
        |view, availability, cx| {
            view.settings_ui.sound.availability = Load::Loaded(availability);
            cx.notify();
        },
    );
}

pub(crate) fn render(
    body: Div,
    state: &SoundPageState,
    audio_ui: &AudioUiState,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    let card = widgets::card(palette).child(widgets::card_header("音訊輸出", None, palette));
    let card = match state.availability {
        Load::NotLoaded | Load::Loading => card.child(widgets::notice_static("檢查中…", Tone::Muted, palette)),
        // Unreachable by construction (`classify` is infallible) but handled
        // rather than `unreachable!()` — a panic in a settings page is never
        // the right answer to a surprise.
        Load::Failed(ref e) => card.child(widgets::notice(e.user_message(), Tone::Danger, palette)),
        Load::Loaded(Availability::NotSupportedHere) => card
            .child(widgets::notice_static("這個平台沒有可設定的音訊裝置。", Tone::Muted, palette))
            .child(widgets::notice_static("音訊設定只在 DuDuClaw 值班機上提供。", Tone::Muted, palette)),
        Load::Loaded(Availability::NotInstalled) => card
            .child(widgets::notice_static("音訊服務未安裝。", Tone::Warning, palette))
            .child(widgets::notice_static(
                "這台值班機目前沒有內建音訊支援，因此沒有音量或輸出裝置可以調整。後續版本加入音訊服務後，這裡就會出現對應的設定。",
                Tone::Muted,
                palette,
            )),
        Load::Loaded(Availability::NotRunning) => card
            .child(widgets::notice_static("音訊服務未啟動。", Tone::Warning, palette))
            .child(widgets::notice_static("音訊元件已安裝但沒有在執行，請重新開機；若問題持續，請聯絡支援。", Tone::Muted, palette)),
        Load::Loaded(Availability::Available) => available_body(card, audio_ui, palette),
    };

    body.child(card)
}

/// The one branch where controls would be honest. It still shows only what
/// has genuinely been read: `AudioUiState` starts from a seeded percentage
/// and only begins reflecting the real device once ControlCenter's slider
/// has round-tripped once (see that field's own doc comment), so an
/// un-round-tripped state is reported as "尚未讀取" rather than as 62%.
fn available_body(card: Div, audio_ui: &AudioUiState, palette: ShellPalette) -> Div {
    match audio_ui.backend_kind {
        Some(AudioBackendKind::Real) => card
            .child(widgets::value_row("音量", format!("{}%", audio_ui.pct), palette))
            .child(widgets::value_row("靜音", if audio_ui.muted { "是".to_string() } else { "否".to_string() }, palette))
            .child(widgets::notice_static("音量可從畫面右上角的控制中心直接調整。", Tone::Muted, palette))
            .child(widgets::notice_static("輸出裝置選擇尚未提供。", Tone::Muted, palette)),
        Some(AudioBackendKind::Fake) => card
            .child(widgets::notice_static("目前顯示的是示範音量，並未連上真正的音訊裝置。", Tone::Warning, palette))
            .child(widgets::notice_static("請確認音訊服務是否正常執行。", Tone::Muted, palette)),
        None => card
            .child(widgets::notice_static("尚未讀取目前音量。", Tone::Muted, palette))
            .child(widgets::notice_static("在控制中心調整一次音量後，這裡就會顯示實際數值。", Tone::Muted, palette)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(is_linux: bool, wpctl: bool, socket: bool) -> Probe {
        Probe { is_linux, wpctl_found: wpctl, socket_found: socket }
    }

    #[test]
    fn a_non_linux_host_is_reported_as_unsupported_not_as_broken() {
        assert_eq!(classify(probe(false, true, true)), Availability::NotSupportedHere);
    }

    /// The state this page exists for today: nothing installed at all.
    #[test]
    fn no_tool_and_no_socket_reads_as_not_installed() {
        assert_eq!(classify(probe(true, false, false)), Availability::NotInstalled);
    }

    #[test]
    fn a_tool_without_a_session_reads_as_not_running() {
        assert_eq!(classify(probe(true, true, false)), Availability::NotRunning);
    }

    /// A live socket we have no way to drive must not be advertised as
    /// available — offering controls that cannot execute is the dishonest
    /// half of this decision.
    #[test]
    fn a_session_without_the_control_tool_is_not_advertised_as_available() {
        assert_eq!(classify(probe(true, false, true)), Availability::NotRunning);
    }

    #[test]
    fn both_present_is_the_only_available_state() {
        assert_eq!(classify(probe(true, true, true)), Availability::Available);
    }

    /// The whole point of the local probe: it must not inherit
    /// `audio::select_backend`'s fail-open-to-Fake behaviour.
    #[test]
    fn the_probe_never_reports_available_without_real_evidence() {
        for p in [probe(true, false, false), probe(true, true, false), probe(true, false, true), probe(false, false, false)] {
            assert_ne!(classify(p), Availability::Available, "{p:?} was wrongly treated as a working audio stack");
        }
    }

    #[test]
    fn a_fresh_page_has_probed_nothing() {
        assert!(SoundPageState::default().availability.needs_load());
    }

    /// The real probe must be callable anywhere without panicking, whatever
    /// the host has (this runs on macOS in CI).
    #[test]
    fn probing_the_real_host_does_not_panic_and_is_self_consistent() {
        let observed = probe_now();
        assert_eq!(observed.is_linux, cfg!(target_os = "linux"));
        // Whatever it found, classification must succeed.
        let _ = classify(observed);
    }
}
