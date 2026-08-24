// D4b — 聲音 (sound). Grown into a real page by D5 (2026-08-24).
//
// ── What changed, and what deliberately did not ────────────────────────
// D4b shipped this page as an honest "not yet": PipeWire was not in the
// DuDuClaw OS image, so `Availability::Available` was unreachable on a real
// appliance and the page's whole job was to distinguish four states and
// offer controls in none of them. D5 puts `pipewire` + `wireplumber` in the
// image (appliance/mkosi.conf), so that branch is now reachable and grows
// what it always said it would: volume, mute, and an output-device picker.
//
// EVERYTHING ELSE IS UNCHANGED — the probe, the four states, and the copy
// for the other three are exactly as D4b left them, which was the point of
// shipping the probe early.
//
// ── Why this page still runs its OWN probe ─────────────────────────────
// `crate::audio::select_backend` used to fail OPEN to `FakeAudioBackend`
// when `wpctl` could not be reached, because ControlCenter's slider had to
// stay draggable on a dev Mac. D5 removed that fallback ON LINUX (a duty box
// that cannot reach its audio service now gets `AudioBackendKind::
// Unavailable`, see `crate::audio`'s own header comment) — but the fallback
// still exists off Linux, and more importantly the page needs a finer answer
// than the backend can give: "the control tool is installed but nothing is
// running" is a different sentence to the operator than "there is no audio
// support here at all", and only a direct look at the filesystem separates
// them. So the probe stays, and it never constructs a backend, so it can
// never be handed a fake one.
//
// The two checks answer different questions and both are load-bearing:
//   * the PROBE (below) answers "is there an audio stack on this machine",
//     which decides whether controls appear at all;
//   * `audio_ui.backend_kind` answers "what did the last real call actually
//     talk to", which decides whether what is on screen is trustworthy.
// A machine can pass the first and fail the second (wireplumber running but
// wedged), and that combination is rendered as its own honest state rather
// than being collapsed into either neighbour.

use gpui::{prelude::*, px, AnyElement, Context, Div};

use duduclaw_native_gui::theme;

use super::widgets::{self, ButtonWeight, Tone};
use super::{spawn_rpc, Load};
use crate::audio::{self, AudioBackendKind, AudioUiState, OutputDevice};
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

/// How many output devices the list renders before it stops and says how
/// many more there are. Same cap-and-disclose discipline the 網路 page's
/// Wi-Fi list uses, and for the same structural reason: nothing in this
/// directory scrolls (see `settings/mod.rs`'s panel-geometry comment), so a
/// list that could grow without bound has to be built to fit.
const MAX_OUTPUTS_SHOWN: usize = 6;

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
    /// The machine's output devices. Loaded only on the `Available` branch —
    /// enumerating on the other three would mean spawning `wpctl` against a
    /// stack this page has just established is not there.
    ///
    /// An `Ok(vec![])` (PipeWire running, no sound card) and a `Failed` are
    /// rendered differently on purpose; see `AudioBackend::list_outputs`' own
    /// doc comment for why the backend keeps them apart.
    pub(crate) outputs: OutputsLoad,
    /// The device id whose switch is in flight, if any. Blocks a second
    /// switch and dims the list while one is running — the same "ignore
    /// clicks while in flight" discipline `AudioUiState.in_flight` documents
    /// for the volume path (a separate flag because the two are separate
    /// operations and either may be running without the other).
    pub(crate) switching_to: Option<u32>,
}

/// What one device-list load or switch produced, carried from the worker
/// thread back to the view. The error is a `String` (the backend's own
/// reason text) rather than `AudioError` only because it crosses a thread
/// boundary into stored page state; nothing is lost, and the text is what
/// reaches the journal.
type OutputsResult = Result<Vec<OutputDevice>, String>;

/// The device list's four states.
///
/// A bespoke enum rather than `super::Load<Vec<OutputDevice>>` because
/// `Load::Failed` carries `client::SettingsRpcError` — the DASHBOARD RPC
/// bridge's error type, which has nothing to do with a `wpctl` subprocess
/// failure. The alternatives were both worse: inventing an RPC error to wrap
/// a subprocess error would put a misleading message in front of the
/// operator, and folding a failure into `Loaded(vec![])` would render 「這台
/// 機器目前沒有可用的輸出裝置」 for a machine whose devices simply could not
/// be read — which is the exact class of lie this page exists to prevent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum OutputsLoad {
    #[default]
    NotLoaded,
    Loading,
    /// A completed enumeration. An empty vector is a real answer.
    Loaded(Vec<OutputDevice>),
    /// The enumeration or the switch could not be performed; carries the
    /// backend's own reason.
    Failed(String),
}

impl OutputsLoad {
    fn needs_load(&self) -> bool {
        matches!(self, OutputsLoad::NotLoaded)
    }
}

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.settings_ui.sound.availability.needs_load() {
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

    // Gated on the probe's answer, not run in parallel with it: enumerating
    // devices means spawning `wpctl`, and there is no point spawning it
    // until the probe has said there is something to spawn it against. The
    // next render after the probe settles is what picks this up — the same
    // "render calls ensure_loaded, ensure_loaded decides" shape every page
    // in this directory uses, rather than a bespoke continuation.
    if view.settings_ui.sound.availability.value() == Some(&Availability::Available) && view.settings_ui.sound.outputs.needs_load() {
        load_outputs(view, cx);
    }

    // The volume reading is shared with ControlCenter (one `audio_ui` on the
    // view), so this only ever dispatches the FIRST read of the process —
    // opening this page on a shell whose ControlCenter has already read the
    // volume reuses that value rather than re-spawning `wpctl`.
    audio::ensure_volume_probed(cx);
}

fn load_outputs(view: &mut ShellView, cx: &mut Context<ShellView>) {
    view.settings_ui.sound.outputs = OutputsLoad::Loading;
    spawn_rpc(
        cx,
        || -> OutputsResult {
            let (backend, _kind) = audio::select_backend();
            backend.list_outputs().map_err(|audio::AudioError::Unavailable(reason)| reason)
        },
        |view, result, cx| {
            apply_outputs(&mut view.settings_ui.sound, result);
            cx.notify();
        },
    );
}

/// Settles a device-list load or switch. Pure over the page state so the
/// state machine is testable without a window — this crate has no headless
/// UI harness (the gap `surface.rs`'s own header comment documents).
pub(crate) fn apply_outputs(state: &mut SoundPageState, result: OutputsResult) {
    // Cleared on BOTH arms: a switch that failed must release the guard, or
    // the row stays stuck on 「切換中…」 with no way out.
    state.switching_to = None;
    state.outputs = match result {
        Ok(devices) => OutputsLoad::Loaded(devices),
        Err(reason) => {
            // The detail goes to the journal; the operator gets one plain
            // line (see `outputs_body`). Same split every other honest
            // failure in this directory uses.
            eprintln!("[settings/sound] enumerating audio outputs failed: {reason}");
            OutputsLoad::Failed(reason)
        }
    };
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
        Load::Loaded(Availability::Available) => available_body(card, state, audio_ui, palette, cx),
    };

    body.child(card)
}

/// The one branch where controls are honest — real as of D5.
///
/// Three groups, in the order an operator reaches for them: the current
/// volume/mute reading, the output-device picker, and a refresh. Each one
/// still states plainly when it has nothing real to show; nothing here is
/// seeded.
fn available_body(card: Div, state: &SoundPageState, audio_ui: &AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let card = match audio_ui.backend_kind {
        // The backend this page's probe implies, and the normal case on an
        // appliance.
        Some(AudioBackendKind::Real) => volume_rows(card, audio_ui, palette, cx),
        // The probe found a real stack but the last call went through the
        // demo backend — only reachable via `DUDUCLAW_SHELL_FAKE_AUDIO=1`,
        // and worth saying out loud rather than silently showing simulated
        // numbers on a settings page.
        Some(AudioBackendKind::Fake) => card
            .child(widgets::notice_static("目前顯示的是示範音量，並未連上真正的音訊裝置。", Tone::Warning, palette))
            .child(widgets::notice_static("這台機器有音訊服務，但目前的音量顯示來自示範模式。", Tone::Muted, palette)),
        // The contradiction case: the tool and the socket are both there,
        // yet the call could not be completed. Naming it exactly is more
        // useful than folding it into 「未啟動」, which the probe has already
        // ruled out.
        //
        // The copy deliberately does NOT say the service is RUNNING, which
        // an earlier draft did. The probe only sees that a socket FILE
        // exists, and a daemon killed with SIGKILL leaves its socket behind
        // — verified in the D5 VM round, where `pkill -9` on pipewire left
        // all four `pipewire-0*` files in $XDG_RUNTIME_DIR and this exact
        // branch rendered. "There is a service here and it is not
        // answering" is what was actually observed; "it is running" was an
        // over-claim by one inference.
        Some(AudioBackendKind::Unavailable) => card
            .child(widgets::notice_static("這台機器上有音訊服務，但它沒有回應。", Tone::Danger, palette))
            .child(widgets::notice_static("請重新開機；若問題持續，請聯絡支援。", Tone::Muted, palette)),
        None => card.child(widgets::notice_static("正在讀取目前音量…", Tone::Muted, palette)),
    };

    card.child(widgets::card_header("輸出裝置", None, palette)).child(outputs_body(state, palette, cx)).child(refresh_row(state, audio_ui, palette, cx))
}

fn volume_rows(card: Div, audio_ui: &AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    if !audio_ui.has_reading() {
        // `Real` with no value means the read came back an error — in
        // practice, no default sink. Say that rather than printing 0%; the
        // device list right below this then confirms it.
        return card.child(widgets::notice_static("讀不到音量，這台機器可能沒有接上輸出裝置。", Tone::Warning, palette));
    }

    let muted = audio_ui.muted;
    let mute_click = cx.listener(|view, _ev, _window, cx| {
        audio::kick_off_audio_call(view, cx, None, |backend| backend.toggle_mute());
    });

    let card = card.child(widgets::value_row("音量", format!("{}%", audio_ui.pct), palette)).child(widgets::control_row(
        "靜音",
        if muted { "目前為靜音".to_string() } else { "目前有聲音".to_string() },
        widgets::toggle_pill("sound-mute", muted, !audio_ui.in_flight, palette, mute_click).into_any_element(),
        palette,
    ));

    let card = card.child(widgets::notice_static("音量也可以從畫面右上角的控制中心調整。", Tone::Muted, palette));
    if audio_ui.last_call_failed {
        return card.child(widgets::notice_static("最後一次調整沒有成功，畫面上是上一次讀到的數值。", Tone::Warning, palette));
    }
    card
}

fn outputs_body(state: &SoundPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    match state.outputs {
        OutputsLoad::NotLoaded | OutputsLoad::Loading => widgets::notice_static("讀取輸出裝置中…", Tone::Muted, palette),
        // "I could not ask" — distinct from "I asked and there are none"
        // below. The backend's own reason text is NOT shown: it is a `wpctl`
        // stderr line, which belongs in the journal, not on an operator's
        // screen (settings/mod.rs's 使用者視角 rule).
        OutputsLoad::Failed(_) => widgets::notice_static("讀不到輸出裝置清單，請按「重新整理」再試一次。", Tone::Danger, palette),
        OutputsLoad::Loaded(ref devices) if devices.is_empty() => {
            // A REAL answer, not a failure: the audio service is running and
            // this machine has no sound output attached. Common on a
            // headless duty box and on a VM booted without a sound card.
            widgets::notice_static("這台機器目前沒有可用的輸出裝置。", Tone::Warning, palette)
        }
        OutputsLoad::Loaded(ref devices) => device_list(devices, state.switching_to, palette, cx),
    }
}

fn device_list(devices: &[OutputDevice], switching_to: Option<u32>, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let mut list = gpui::div().flex().flex_col().gap(px(8.));
    for (index, device) in devices.iter().take(MAX_OUTPUTS_SHOWN).enumerate() {
        list = list.child(device_row(index, device, switching_to, palette, cx));
    }
    if devices.len() > MAX_OUTPUTS_SHOWN {
        // Honest overflow rather than a silently clipped list — see
        // `MAX_OUTPUTS_SHOWN`'s own doc comment.
        list = list.child(widgets::notice(format!("還有 {} 個輸出裝置未顯示。", devices.len() - MAX_OUTPUTS_SHOWN), Tone::Muted, palette));
    }
    list
}

fn device_row(index: usize, device: &OutputDevice, switching_to: Option<u32>, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let id = device.id;
    let busy = switching_to.is_some();
    let subtitle = if device.is_default { "目前使用中".to_string() } else { "點「使用這個」切換到這個裝置".to_string() };

    let trailing: AnyElement = if device.is_default {
        widgets::status_pill("使用中".to_string(), Tone::Success, palette).into_any_element()
    } else {
        let label = if switching_to == Some(id) { "切換中…".to_string() } else { "使用這個".to_string() };
        let on_click = cx.listener(move |view, _ev, _window, cx| switch_output(view, id, cx));
        widgets::button(SWITCH_BUTTON_IDS[index.min(SWITCH_BUTTON_IDS.len() - 1)], label, ButtonWeight::Secondary, !busy, palette, on_click)
            .into_any_element()
    };

    // The name is the machine's own string; rendered as a value row rather
    // than a `control_row` title because `control_row` takes a `&'static str`
    // and a hardware name is anything but static.
    gpui::div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(palette.foreground, 1.0))
                        .child(device.name.clone()),
                )
                .child(gpui::div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.text_faint, 1.0)).child(subtitle)),
        )
        .child(trailing)
}

/// gpui element ids are `&'static str`, and a device list is dynamic — so the
/// rows draw from a fixed table sized to [`MAX_OUTPUTS_SHOWN`] rather than
/// leaking a `String` per frame. The `.min()` at the call site keeps this
/// total even if the cap and this table ever disagree.
const SWITCH_BUTTON_IDS: [&str; MAX_OUTPUTS_SHOWN] =
    ["sound-out-0", "sound-out-1", "sound-out-2", "sound-out-3", "sound-out-4", "sound-out-5"];

fn switch_output(view: &mut ShellView, id: u32, cx: &mut Context<ShellView>) {
    if view.settings_ui.sound.switching_to.is_some() {
        return;
    }
    view.settings_ui.sound.switching_to = Some(id);
    cx.notify();
    spawn_rpc(
        cx,
        move || -> OutputsResult {
            let (backend, _kind) = audio::select_backend();
            backend.set_default_output(id).map_err(|audio::AudioError::Unavailable(reason)| reason)
        },
        |view, result, cx| {
            apply_outputs(&mut view.settings_ui.sound, result);
            cx.notify();
        },
    );
}

fn refresh_row(state: &SoundPageState, audio_ui: &AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // `audio_ui.in_flight` is part of "busy" and not an afterthought: the
    // click below re-arms the volume probe, and `kick_off_audio_call` DROPS a
    // dispatch while another call is in flight (see its in-flight guard). A
    // refresh accepted during a volume round-trip would therefore clear the
    // latch, get dropped, and leave the page showing a stale value with no
    // pending read — a button that silently did nothing.
    let busy = state.switching_to.is_some() || matches!(state.outputs, OutputsLoad::Loading) || audio_ui.in_flight;
    let on_click = cx.listener(|view, _ev, _window, cx| {
        // Re-reads BOTH halves: the device list, and the volume (by clearing
        // the once-per-process probe latch so the next render dispatches a
        // fresh read). A refresh button that only refreshed half the page
        // would be its own small lie.
        view.audio_ui.probe_started = false;
        load_outputs(view, cx);
        cx.notify();
    });
    gpui::div().flex().justify_end().child(widgets::button("sound-refresh", "重新整理".to_string(), ButtonWeight::Secondary, !busy, palette, on_click))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(is_linux: bool, wpctl: bool, socket: bool) -> Probe {
        Probe { is_linux, wpctl_found: wpctl, socket_found: socket }
    }

    fn device(id: u32, name: &str, is_default: bool) -> OutputDevice {
        OutputDevice { id, name: name.to_string(), is_default }
    }

    #[test]
    fn a_non_linux_host_is_reported_as_unsupported_not_as_broken() {
        assert_eq!(classify(probe(false, true, true)), Availability::NotSupportedHere);
    }

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
    /// `audio::select_backend`'s off-Linux fail-open-to-Fake behaviour.
    #[test]
    fn the_probe_never_reports_available_without_real_evidence() {
        for p in [probe(true, false, false), probe(true, true, false), probe(true, false, true), probe(false, false, false)] {
            assert_ne!(classify(p), Availability::Available, "{p:?} was wrongly treated as a working audio stack");
        }
    }

    #[test]
    fn a_fresh_page_has_probed_nothing() {
        let state = SoundPageState::default();
        assert!(state.availability.needs_load());
        assert!(state.outputs.needs_load());
        assert_eq!(state.switching_to, None);
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

    // ── device list state machine (D5) ──────────────────────────────────

    #[test]
    fn a_successful_load_stores_the_devices_and_clears_any_switch() {
        let mut state = SoundPageState { switching_to: Some(50), ..SoundPageState::default() };
        apply_outputs(&mut state, Ok(vec![device(50, "Speakers", true)]));
        assert_eq!(state.outputs, OutputsLoad::Loaded(vec![device(50, "Speakers", true)]));
        assert_eq!(state.switching_to, None, "a settled switch must release the guard");
    }

    /// PipeWire up with no sound card is a real, distinct answer — the page
    /// renders it as 「沒有可用的輸出裝置」, not as a failure and not as a
    /// spinner that never resolves.
    #[test]
    fn an_empty_device_list_settles_rather_than_hanging() {
        let mut state = SoundPageState::default();
        apply_outputs(&mut state, Ok(Vec::new()));
        assert_eq!(state.outputs, OutputsLoad::Loaded(Vec::new()));
        assert!(!state.outputs.needs_load(), "an empty answer is still an answer");
    }

    /// The distinction the whole `OutputsLoad` enum exists for: "I could not
    /// ask" must never settle into the same state as "I asked and there are
    /// none", because the two render as different sentences.
    #[test]
    fn a_failure_to_enumerate_is_not_the_same_as_an_empty_machine() {
        let mut failed = SoundPageState::default();
        apply_outputs(&mut failed, Err("failed to spawn wpctl".to_string()));
        let mut empty = SoundPageState::default();
        apply_outputs(&mut empty, Ok(Vec::new()));
        assert_ne!(failed.outputs, empty.outputs);
        assert!(matches!(failed.outputs, OutputsLoad::Failed(_)));
    }

    /// A failed switch must not leave the button stuck on 「切換中…」 forever.
    #[test]
    fn a_failed_switch_releases_the_guard() {
        let mut state = SoundPageState { switching_to: Some(53), ..SoundPageState::default() };
        apply_outputs(&mut state, Err("wpctl set-default exited with Some(1)".to_string()));
        assert_eq!(state.switching_to, None);
    }

    /// The cap and the id table must agree, or a long list would reuse one
    /// gpui element id across rows.
    #[test]
    fn there_is_one_button_id_per_rendered_row() {
        assert_eq!(SWITCH_BUTTON_IDS.len(), MAX_OUTPUTS_SHOWN);
        let mut seen = SWITCH_BUTTON_IDS.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), SWITCH_BUTTON_IDS.len(), "duplicate element ids");
    }
}
