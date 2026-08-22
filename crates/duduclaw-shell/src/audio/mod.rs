// Audio backend abstraction — Shell-S4 (2026-08-22, "控制中心音量真值").
//
// Shell-S3 gave this crate its canonical "real backend on Linux,
// deterministic fallback everywhere else, ONE `select_backend()` decision
// point" shape (`oobe/network/mod.rs` — read that module's own header
// comment first, this one repeats it verbatim for audio) to replace
// `ControlCenter`'s Wi-Fi row's static snapshot. `overlay/controlcenter.rs`'s
// OWN header comment flagged the volume/brightness sliders as the next
// static-snapshot debt this same round pays off — for VOLUME only;
// brightness stays static this round (see that file's header comment on
// why: backlight is a different backend, out of scope here).
//
//   - `AudioBackend` trait — `get_volume()` / `set_volume(pct)` /
//     `toggle_mute()`, task brief's exact three verbs.
//   - `FakeAudioBackend` (`fake.rs`) — every platform, always compiled, an
//     in-memory (process-global atomics, see that module's own header
//     comment for why) volume/mute pair seeded from `fake_data::
//     SLIDER_ROWS[0].pct`.
//   - `WpctlAudioBackend` (`wpctl.rs`) — `#[cfg(target_os = "linux")]` only,
//     drives PipeWire's `wpctl` CLI as a subprocess (PipeWire has no D-Bus
//     surface `zbus` could reach the way `oobe::network::nm` reaches
//     NetworkManager — see that module's own header comment for the
//     citation).
//   - `select_backend()` — the one decision point, same three-tier
//     priority `network::select_backend` established:
//     `DUDUCLAW_SHELL_FAKE_AUDIO=1` override > Linux `wpctl` probe > Fake.
//
// ── Why every call still goes through a background thread, even though a
// volume change is sub-millisecond local IPC (unlike Wi-Fi's real seconds-
// scale network I/O) ────────────────────────────────────────────────────
// gpui's main thread must never block on ANY subprocess spawn, however fast
// it usually returns — a hung/missing `wpctl` binary (wireplumber wedged,
// container with no PipeWire at all) must degrade to "the slider stops
// responding for one backend timeout", never "the whole compositor UI
// thread stalls". Same background-thread + `std::sync::mpsc` + `cx.spawn`
// poll bridge `steps::network`'s click handlers established, wired up in
// `overlay::controlcenter::kick_off_audio_call` (that fn's own header
// comment has the exact shape and why it's ONE shared helper for both
// `set_volume` and `toggle_mute` rather than two near-identical copies).

mod fake;
#[cfg(target_os = "linux")]
mod wpctl;

pub(crate) use fake::FakeAudioBackend;

/// One backend's read of the current output volume — task brief:
/// "get_volume()（0-100＋muted）".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VolumeState {
    pub(crate) pct: u8,
    pub(crate) muted: bool,
}

/// Every failure mode either backend can report. Deliberately ONE variant,
/// unlike `oobe::network::NetError`'s several: Wi-Fi has operator-visible
/// distinctions worth different UI copy (wrong password vs. unreachable AP
/// vs. a timed-out join). A volume backend has no such distinctions this
/// round's UI needs to act on differently — `wpctl` missing, a device with
/// no default sink, a malformed reply, and a non-zero exit code are all
/// "this backend call didn't work", surfaced the same honest way in
/// `overlay::controlcenter::AudioUiState::settle`'s `Err` arm (log the
/// detail, leave the slider at its last known value — see that fn's own
/// doc comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudioError {
    // Only ever constructed by `WpctlAudioBackend` (`wpctl.rs`,
    // `#[cfg(target_os = "linux")]`) in production — `FakeAudioBackend`
    // never fails. Same cross-platform-construction shape `oobe::network::
    // NetError`'s own header comment documents for its per-backend
    // variants: not dead code on Linux (the platform that matters), just
    // unreachable-by-construction on this crate's Mac dev-loop host.
    // `#[cfg(test)]` code (`mod.rs`'s own `settle_err_...` test) also
    // constructs this on every platform, but the dead-code lint only looks
    // at non-test code.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unavailable(String),
}

/// The volume control surface `overlay::controlcenter` drives. `Send` (not
/// `Send + Sync`), same reasoning `oobe::network::NetworkBackend`'s own doc
/// comment gives: every call happens from inside exactly one
/// `std::thread::spawn` closure at a time.
pub(crate) trait AudioBackend: Send {
    fn get_volume(&self) -> Result<VolumeState, AudioError>;
    /// `pct` is clamped to `0..=100` by the implementation, never rejected
    /// — see `WpctlAudioBackend::set_volume`'s own doc comment for why a
    /// drag that computes a slightly out-of-range target from a fast mouse
    /// move should never surface as an error.
    fn set_volume(&self, pct: u8) -> Result<VolumeState, AudioError>;
    fn toggle_mute(&self) -> Result<VolumeState, AudioError>;
}

/// Which concrete backend a given process run actually got — same
/// operator-facing honesty contract `oobe::network::NetBackendKind`'s own
/// doc comment establishes ("在 UI 誠實標示（不可假裝連線成功）"), applied
/// here to volume: `overlay::controlcenter`'s demo-mode notice renders
/// whenever the LATEST settled call used `Fake`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioBackendKind {
    // Only constructed inside `select_backend`'s own `#[cfg(target_os =
    // "linux")]` block below — same cross-platform-construction shape
    // `oobe::network`'s own `NetBackendKind::Real` documents.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Real,
    Fake,
}

/// Resolves + constructs the backend for one `get_volume`/`set_volume`/
/// `toggle_mute` call — see this module's header comment for why callers
/// only ever invoke this from a background thread, and `oobe::network::
/// select_backend`'s own doc comment for the identical three-tier priority
/// this repeats:
///   1. `DUDUCLAW_SHELL_FAKE_AUDIO=1` — explicit dev/test override, any
///      platform. Same shape as `DUDUCLAW_SHELL_FAKE_NET`.
///   2. `#[cfg(target_os = "linux")]` — try `WpctlAudioBackend::probe()`.
///      On success this run gets `Real`. On failure (no `wpctl` on `$PATH`,
///      no default sink configured, PipeWire not running) this is the
///      fail-open path: fall through to Fake rather than leaving the
///      slider unusable.
///   3. Otherwise (non-Linux, or the Linux probe failed) — `FakeAudioBackend`.
pub(crate) fn select_backend() -> (Box<dyn AudioBackend>, AudioBackendKind) {
    if std::env::var("DUDUCLAW_SHELL_FAKE_AUDIO").ok().as_deref() == Some("1") {
        return (Box::new(FakeAudioBackend::new()), AudioBackendKind::Fake);
    }

    #[cfg(target_os = "linux")]
    {
        match wpctl::WpctlAudioBackend::probe() {
            Ok(backend) => return (Box::new(backend), AudioBackendKind::Real),
            Err(e) => {
                eprintln!("[overlay/audio] wpctl unreachable, falling back to demo volume: {e:?}");
            }
        }
    }

    (Box::new(FakeAudioBackend::new()), AudioBackendKind::Fake)
}

/// Runtime-mutable ControlCenter volume-slider state — lives on `ShellView`
/// as `audio_ui` (see `main.rs`'s own field doc comment), read by
/// `overlay::controlcenter::render` and mutated by its click/drag/mute
/// handlers. Plain data, no gpui types — same "no gpui anywhere in the
/// state/backend layer" discipline `oobe::network`'s own header comment
/// holds itself to (this struct is UI STATE, not a backend, but the same
/// reasoning applies: it has to be constructible and testable without a
/// window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioUiState {
    /// Displayed volume percentage. Starts at `fake::SEED_PCT` (`fake_data::
    /// SLIDER_ROWS[0]`'s own static snapshot, 62%) — this crate's
    /// established "I/O is always click-triggered, never a render-time side
    /// effect" convention (`steps::network`'s own header comment) means
    /// ControlCenter does NOT eagerly `get_volume()` the moment the panel
    /// opens, so this field only starts reflecting the REAL backend once
    /// the operator's first interaction (click/drag/mute) round-trips.
    /// Deliberate, documented scope boundary for this round, same shape
    /// `oobe::network`'s own `NeverScanned` state accepts (nothing shows
    /// real data until the first click) — see `overlay::controlcenter`'s
    /// header comment for the full accounting of what's covered.
    pub(crate) pct: u8,
    pub(crate) muted: bool,
    /// `None` until the first real backend call settles — see `pct`'s own
    /// doc comment. Once `Some`, drives the same "示範模式" honest notice
    /// `steps::network`'s `NetBackendKind` check already established.
    pub(crate) backend_kind: Option<AudioBackendKind>,
    /// Guards against overlapping backend calls — a fast drag can fire many
    /// mouse-move ticks faster than one `wpctl` round-trip settles; while
    /// `true`, `overlay::controlcenter::kick_off_audio_call` drops new
    /// interactions rather than queuing them (same "ignore clicks while in
    /// flight" discipline `steps::network`'s connect/cancel handlers use —
    /// see `kick_off_connect`'s own doc comment for why that guard, not a
    /// visual-only one, is the authoritative source of truth). In practice
    /// this only ever drops a handful of intermediate drag targets during
    /// one sub-10ms local IPC call, landing on whichever target the mouse
    /// was at once the in-flight call settles and the next event fires —
    /// not a felt lag.
    pub(crate) in_flight: bool,
}

impl Default for AudioUiState {
    fn default() -> Self {
        Self { pct: fake::SEED_PCT, muted: false, backend_kind: None, in_flight: false }
    }
}

impl AudioUiState {
    /// Called right before dispatching a background backend call. Marks
    /// busy and, for a volume SET (`Some(target_pct)`), optimistically
    /// shows the target percentage immediately — so a drag feels
    /// responsive rather than waiting for the round-trip to visually move
    /// the fill — mirroring `oobe::network`'s own `set_net_connecting`
    /// transitioning the UI to `Connecting` before the real result lands.
    /// `None` (a mute toggle, which has no target percentage to preview)
    /// leaves `pct` untouched.
    pub(crate) fn begin(&mut self, optimistic_pct: Option<u8>) {
        self.in_flight = true;
        if let Some(pct) = optimistic_pct {
            self.pct = pct;
        }
    }

    /// Applies a settled backend result. Always resolves `in_flight` back
    /// to `false` regardless of outcome, so a failed call never leaves the
    /// slider permanently unresponsive — an `Err` deliberately leaves
    /// `pct`/`muted` at their last known (optimistic or previously-settled)
    /// value rather than inventing a rollback target, the same "no
    /// authoritative correction available, don't guess one" reasoning
    /// `steps::network::apply_connect_result`'s own `Err` arm applies
    /// (it renders a failure message instead of reverting the selection).
    pub(crate) fn settle(&mut self, kind: AudioBackendKind, result: Result<VolumeState, AudioError>) {
        self.in_flight = false;
        self.backend_kind = Some(kind);
        match result {
            Ok(state) => {
                self.pct = state.pct;
                self.muted = state.muted;
            }
            Err(e) => {
                eprintln!("[overlay/audio] backend call failed: {e:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Same discipline `oobe/network/mod.rs`'s own `ENV_LOCK` establishes —
    // `DUDUCLAW_SHELL_FAKE_AUDIO` is process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn fake_audio_env_override_forces_fake_kind_regardless_of_platform() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_SHELL_FAKE_AUDIO").ok();
        unsafe { std::env::set_var("DUDUCLAW_SHELL_FAKE_AUDIO", "1") };
        let (_, kind) = select_backend();
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("DUDUCLAW_SHELL_FAKE_AUDIO", v),
                None => std::env::remove_var("DUDUCLAW_SHELL_FAKE_AUDIO"),
            }
        }
        assert_eq!(kind, AudioBackendKind::Fake);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn non_linux_default_is_fake_without_needing_the_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_SHELL_FAKE_AUDIO").ok();
        unsafe { std::env::remove_var("DUDUCLAW_SHELL_FAKE_AUDIO") };
        let (_, kind) = select_backend();
        unsafe {
            if let Some(v) = &prev {
                std::env::set_var("DUDUCLAW_SHELL_FAKE_AUDIO", v);
            }
        }
        assert_eq!(kind, AudioBackendKind::Fake);
    }

    #[test]
    fn default_ui_state_matches_the_fake_backend_seed() {
        assert_eq!(AudioUiState::default().pct, fake::SEED_PCT);
        assert!(!AudioUiState::default().muted);
        assert_eq!(AudioUiState::default().backend_kind, None);
        assert!(!AudioUiState::default().in_flight);
    }

    #[test]
    fn begin_with_target_pct_sets_busy_and_previews_the_value() {
        let mut ui = AudioUiState::default();
        ui.begin(Some(80));
        assert!(ui.in_flight);
        assert_eq!(ui.pct, 80);
    }

    #[test]
    fn begin_without_target_pct_sets_busy_but_leaves_pct_untouched() {
        let mut ui = AudioUiState::default();
        let before = ui.pct;
        ui.begin(None);
        assert!(ui.in_flight);
        assert_eq!(ui.pct, before);
    }

    #[test]
    fn settle_ok_clears_busy_and_adopts_the_authoritative_state() {
        let mut ui = AudioUiState::default();
        ui.begin(Some(80));
        ui.settle(AudioBackendKind::Fake, Ok(VolumeState { pct: 77, muted: true }));
        assert!(!ui.in_flight);
        assert_eq!(ui.pct, 77);
        assert!(ui.muted);
        assert_eq!(ui.backend_kind, Some(AudioBackendKind::Fake));
    }

    #[test]
    fn settle_err_clears_busy_but_keeps_the_last_known_value() {
        let mut ui = AudioUiState::default();
        ui.begin(Some(80));
        ui.settle(AudioBackendKind::Fake, Err(AudioError::Unavailable("boom".to_string())));
        assert!(!ui.in_flight);
        assert_eq!(ui.pct, 80, "the optimistic preview must survive an Err settle");
        assert_eq!(ui.backend_kind, Some(AudioBackendKind::Fake), "kind is recorded even on failure");
    }
}
