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
//     `toggle_mute()` (Shell-S4's three verbs) plus `list_outputs()` /
//     `set_default_output(id)` (D5's output-device picker).
//   - `FakeAudioBackend` (`fake.rs`) — every platform, always compiled, an
//     in-memory (process-global atomics, see that module's own header
//     comment for why) volume/mute pair. It owns no devices and says so.
//   - `WpctlAudioBackend` (`wpctl.rs`) — `#[cfg(target_os = "linux")]` only,
//     drives PipeWire's `wpctl` CLI as a subprocess (PipeWire has no D-Bus
//     surface `zbus` could reach the way `oobe::network::nm` reaches
//     NetworkManager — see that module's own header comment for the
//     citation).
//   - `UnavailableAudioBackend` (`unavailable.rs`) — Linux with no reachable
//     PipeWire. Every verb returns the real probe error; see the D5 section
//     below for why this replaced a fall-through to `FakeAudioBackend`.
//   - `select_backend()` — the one decision point:
//     `DUDUCLAW_SHELL_FAKE_AUDIO=1` override > Linux `wpctl` probe
//     (success -> Real, failure -> Unavailable) > non-Linux Fake.
//   - `bridge.rs` — the gpui plumbing (background thread + poll) both
//     ControlCenter and 系統設定 › 聲音 dispatch through.
//
// ── Why every call still goes through a background thread, even though a
// volume change is sub-millisecond local IPC (unlike Wi-Fi's real seconds-
// scale network I/O) ────────────────────────────────────────────────────
// gpui's main thread must never block on ANY subprocess spawn, however fast
// it usually returns — a hung/missing `wpctl` binary (wireplumber wedged,
// container with no PipeWire at all) must degrade to "the slider stops
// responding for one backend timeout", never "the whole compositor UI
// thread stalls". Same background-thread + `std::sync::mpsc` + `cx.spawn`
// poll bridge `steps::network`'s click handlers established, now living in
// `bridge.rs` (that module's own header comment has the exact shape and why
// it's ONE shared helper for every verb rather than a copy per call site).
//
// ── D5 (2026-08-24): PipeWire is in the image, so the fail-open changed ──
// Shell-S4 shipped against an image with NO audio stack at all, so a Linux
// probe failure fell back to `FakeAudioBackend` — a draggable slider that
// changed nothing. Now that `pipewire`/`wireplumber` are in the image
// (appliance/mkosi.conf) that fallback would be a lie on the one platform
// where it matters: a duty box whose audio daemon is missing or wedged must
// SAY SO, not simulate. So Linux now fails to `UnavailableAudioBackend`
// (`unavailable.rs`) — every verb returns the real probe error, and the UI
// renders a disabled control with the reason. `FakeAudioBackend` survives
// for exactly two callers: a non-Linux host (this crate's macOS dev loop,
// where there is no `wpctl` module compiled at all) and the explicit
// `DUDUCLAW_SHELL_FAKE_AUDIO=1` override.
//
// This round also grew the trait past volume: `list_outputs()` /
// `set_default_output()` back 系統設定 › 聲音's output-device picker, which
// D4b shipped as an honest "尚未提供" placeholder waiting for exactly this.

mod bridge;
mod fake;
// Compiled on every platform (so its tests run in this crate's macOS dev
// loop) but only CONSTRUCTED by `select_backend`'s Linux branch — the same
// cross-platform-construction shape `AudioError::Unavailable` documents just
// below, and the reason the whole module carries the lint allowance rather
// than being `#[cfg(target_os = "linux")]`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod unavailable;
#[cfg(target_os = "linux")]
mod wpctl;

pub(crate) use bridge::{ensure_volume_probed, kick_off_audio_call};
pub(crate) use fake::FakeAudioBackend;
#[cfg(target_os = "linux")]
use unavailable::UnavailableAudioBackend;

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
/// `AudioUiState::settle`'s `Err` arm (log the detail, leave the slider at
/// its last known value — see that fn's own doc comment). The reason STRING
/// is what carries the difference for a support session reading the journal;
/// it is deliberately never rendered on an operator-facing surface (it is
/// `wpctl` stderr, not copy).
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

/// One audio output (a PipeWire *sink*) as the operator sees it in 系統設定
/// › 聲音. `id` is the backend's own object id, opaque to the UI and only
/// ever handed straight back to `set_default_output` — the shell never
/// parses or renders it as anything but a key.
///
/// Owned `String` rather than `&'static str` because these names come from
/// the running machine's hardware, and `PartialEq`/`Eq` because the page's
/// `settings::sound_page::OutputsLoad` state has to be comparable (this
/// crate derives `PartialEq` on every page state so a test can assert one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputDevice {
    pub(crate) id: u32,
    pub(crate) name: String,
    /// Whether this is the sink the system currently routes to. Exactly one
    /// device should carry it, but the UI never assumes that — a backend
    /// that reports none (or, pathologically, two) still renders, because a
    /// wrong assumption here would mean an empty screen instead of a list.
    pub(crate) is_default: bool,
}

/// The audio control surface `overlay::controlcenter` (volume/mute) and
/// `settings::sound_page` (output devices) drive. `Send` (not `Send + Sync`),
/// same reasoning `oobe::network::NetworkBackend`'s own doc comment gives:
/// every call happens from inside exactly one `std::thread::spawn` closure
/// at a time.
pub(crate) trait AudioBackend: Send {
    fn get_volume(&self) -> Result<VolumeState, AudioError>;
    /// `pct` is clamped to `0..=100` by the implementation, never rejected
    /// — see `WpctlAudioBackend::set_volume`'s own doc comment for why a
    /// drag that computes a slightly out-of-range target from a fast mouse
    /// move should never surface as an error.
    fn set_volume(&self, pct: u8) -> Result<VolumeState, AudioError>;
    fn toggle_mute(&self) -> Result<VolumeState, AudioError>;

    /// Every output this machine can route to. An `Ok(vec![])` is a REAL
    /// answer ("I enumerated successfully and this machine has no sinks"),
    /// distinct from an `Err` ("I could not ask") — the settings page renders
    /// those two differently, which is the whole reason this returns a
    /// `Result<Vec<_>>` rather than a bare `Vec`.
    fn list_outputs(&self) -> Result<Vec<OutputDevice>, AudioError>;

    /// Makes `id` the system default sink. Returns the list as it stands
    /// AFTER the change — re-read rather than patched locally, the same
    /// "never trust the write's own success alone, read the real value back"
    /// discipline `WpctlAudioBackend`'s header comment documents for volume.
    fn set_default_output(&self, id: u32) -> Result<Vec<OutputDevice>, AudioError>;
}

/// Which concrete backend a given process run actually got — same
/// operator-facing honesty contract `oobe::network::NetBackendKind`'s own
/// doc comment establishes ("在 UI 誠實標示（不可假裝連線成功）"), applied
/// here to audio: `overlay::controlcenter` renders a demo-mode notice
/// whenever the LATEST settled call used `Fake`, and a DISABLED control with
/// a reason whenever it used `Unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioBackendKind {
    // Only constructed inside `select_backend`'s own `#[cfg(target_os =
    // "linux")]` block below — same cross-platform-construction shape
    // `oobe::network`'s own `NetBackendKind::Real` documents.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Real,
    Fake,
    /// Linux with no reachable PipeWire. NOT a fallback that pretends: every
    /// call through this backend fails with the real probe error, and the UI
    /// disables its controls. See this module's header comment (D5) for why
    /// this replaced the old fall-through-to-`Fake` on Linux.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unavailable,
}

impl AudioBackendKind {
    /// Whether a control backed by this backend should accept interaction at
    /// all. `Fake` counts as usable on purpose: it is the macOS dev loop's
    /// backend and its slider genuinely moves its own (simulated) device —
    /// it is labelled, not disabled. Only `Unavailable` can perform nothing.
    pub(crate) fn is_usable(self) -> bool {
        !matches!(self, AudioBackendKind::Unavailable)
    }
}

/// Resolves + constructs the backend for one backend call — see this
/// module's header comment for why callers only ever invoke this from a
/// background thread, and `oobe::network::select_backend`'s own doc comment
/// for the three-tier priority this repeats (with D5's one deliberate
/// divergence, spelled out below):
///   1. `DUDUCLAW_SHELL_FAKE_AUDIO=1` — explicit dev/test override, any
///      platform, and only in a build carrying the `debug-affordances`
///      feature (see `fake_override_requested` below). Same shape as
///      `DUDUCLAW_SHELL_FAKE_NET`.
///   2. `#[cfg(target_os = "linux")]` — try `WpctlAudioBackend::probe()`.
///      On success this run gets `Real`. On failure (no `wpctl` on `$PATH`,
///      PipeWire not running, the daemon unreachable) it gets `Unavailable`
///      carrying that exact error — NOT `Fake`. Note a machine with a
///      running daemon and NO SINK passes the probe and lands on `Real`: the
///      absence of a sink is a fact about the hardware, not a broken
///      service, and `wpctl.rs`'s header comment covers why the probe was
///      changed so the two stop being conflated. This is the
///      divergence from `network::select_backend`, and from this module's
///      own Shell-S4 behaviour: PipeWire ships in the image now, so on the
///      appliance a probe failure is a real fault to report, not a gap to
///      paper over. (Wi-Fi's fake fallback is unaffected and stays as it is
///      — a different feature with a different image story.)
///   3. Non-Linux — `FakeAudioBackend`, the macOS dev loop.
pub(crate) fn select_backend() -> (Box<dyn AudioBackend>, AudioBackendKind) {
    select_backend_with(fake_override_requested())
}

/// The demo-backend override's env var name — one spelling, so the gate and
/// its test cannot drift apart.
const FAKE_AUDIO_ENV: &str = "DUDUCLAW_SHELL_FAKE_AUDIO";

/// Whether this run was asked for the demo backend.
///
/// Q1 (2026-08-24): behind the shipping gate — a shipping image must never
/// present an invented volume control that changes nothing, and the kiosk
/// launcher sources an operator-writable env file into the whole session
/// tree, so an env var alone is not a trustworthy opt-in. See
/// `crate::shipping`.
fn fake_override_requested() -> bool {
    crate::shipping::debug_env_is_one(FAKE_AUDIO_ENV)
}

/// The DECISION half of [`select_backend`], split from the env read so it is
/// testable without touching the process environment.
///
/// This split is not cosmetic. `std::env::set_var` is process-global and
/// unsynchronised, so a test that sets a variable, calls production code and
/// asserts on the resulting backend races every other test in the binary
/// that touches the environment. Worse, the test that used to do exactly
/// that PASSED FOR THE WRONG REASON: the Linux fallback used to be `Fake`
/// too, so "the override worked" and "the override was ignored" produced the
/// identical answer. D5 changed that fallback to `Unavailable` and the stale
/// assertion failed honestly on Linux — the fix is to assert on a decision
/// with no ambient input, not to re-run until it passes.
fn select_backend_with(force_fake: bool) -> (Box<dyn AudioBackend>, AudioBackendKind) {
    if force_fake {
        return (Box::new(FakeAudioBackend::new()), AudioBackendKind::Fake);
    }

    #[cfg(target_os = "linux")]
    {
        match wpctl::WpctlAudioBackend::probe() {
            Ok(backend) => return (Box::new(backend), AudioBackendKind::Real),
            Err(e) => {
                let AudioError::Unavailable(reason) = e;
                eprintln!("[audio] PipeWire unreachable — audio controls will be disabled: {reason}");
                return (Box::new(UnavailableAudioBackend::new(reason)), AudioBackendKind::Unavailable);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    (Box::new(FakeAudioBackend::new()), AudioBackendKind::Fake)
}

/// Runtime-mutable volume/mute state — lives on `ShellView` as `audio_ui`
/// (see `main.rs`'s own field doc comment), read by
/// `overlay::controlcenter::render` and, since D5, by
/// `settings::sound_page::render` too. ONE shared value on purpose: the two
/// surfaces show the same machine's volume, and a second copy would let them
/// disagree. It also means opening 系統設定 › 聲音 after ControlCenter has
/// already read the volume costs no extra `wpctl` spawn (see
/// `bridge::ensure_volume_probed`'s once-per-process latch).
///
/// The DEVICE LIST deliberately does NOT live here — it belongs to the
/// settings page alone (`sound_page::OutputsLoad`), which is also what keeps
/// this struct `Copy`.
///
/// Plain data, no gpui types — same "no gpui anywhere in the
/// state/backend layer" discipline `oobe::network`'s own header comment
/// holds itself to (this struct is UI STATE, not a backend, but the same
/// reasoning applies: it has to be constructible and testable without a
/// window).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AudioUiState {
    /// Last volume percentage a backend actually reported. MEANINGLESS
    /// until `backend_kind` is `Some` — read it through that, never on its
    /// own.
    ///
    /// D5 (2026-08-24) changed its initial value from `fake::SEED_PCT` (62%,
    /// copied from `fake_data::SLIDER_ROWS[0]`) to 0, and paired that with
    /// an eager first read (`bridge::ensure_volume_probed`, dispatched the
    /// first time a surface that shows volume renders). Shell-S4 showed the
    /// seeded 62% until the operator's first drag round-tripped, which is
    /// precisely the 假資料 this shell forbids everywhere else — a number
    /// that looks like a reading and is not one. There is now no invented
    /// value to display at any point: before the first settle the control
    /// renders as not-yet-read.
    pub(crate) pct: u8,
    pub(crate) muted: bool,
    /// `None` until the first backend call settles — the "nothing has been
    /// read yet" state. Once `Some`, it drives which honest treatment the
    /// control gets: plain (`Real`), labelled 示範模式 (`Fake`), or disabled
    /// with a reason (`Unavailable`).
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
    /// Whether the eager first read has been DISPATCHED (not whether it
    /// succeeded). Set once by `bridge::ensure_volume_probed` so a render
    /// pass cannot re-dispatch it every frame — the same job
    /// `settings::Load::needs_load` does for the settings pages, kept as a
    /// separate flag here because `backend_kind` alone would re-fire forever
    /// while a backend keeps failing.
    pub(crate) probe_started: bool,
    /// Whether the LAST settled call failed. Drives one honest line next to
    /// an otherwise-working control ("最後一次調整沒有成功"); deliberately a
    /// bool rather than the error text so this struct stays `Copy` — the
    /// detail goes to stderr in `settle`, where a support session can read
    /// it, and the operator-facing UI has nothing to do with the difference
    /// between one `wpctl` failure mode and another (see `AudioError`'s own
    /// single-variant doc comment for the same argument).
    pub(crate) last_call_failed: bool,
    /// Whether any call has ever come back `Ok`, i.e. whether `pct`/`muted`
    /// hold a value a backend actually reported. Separate from
    /// `backend_kind.is_some()` because a settled FAILURE also sets the kind
    /// — an `Unavailable` backend settles immediately with an error, and
    /// treating that as "we have a reading of 0%" is exactly the invented
    /// number this round removed. Separate from `!last_call_failed` too: a
    /// successful read followed by a failed write leaves a stale-but-real
    /// value that is still the honest thing to show.
    pub(crate) has_value: bool,
}

impl AudioUiState {
    /// Whether a real reading exists to display. Everything that renders a
    /// number goes through this — the alternative (checking `pct != 0`)
    /// would silently treat a genuine 0% as "not read".
    pub(crate) fn has_reading(&self) -> bool {
        self.has_value
    }

    /// Whether the control should accept interaction. Requires BOTH a usable
    /// backend and a value that backend actually reported:
    ///   * before the first read settles, nothing is known and there is
    ///     nothing to drag from;
    ///   * an `Unavailable` backend can perform nothing at all;
    ///   * a `Real` backend that has never produced a value has, in
    ///     practice, no default sink — `wpctl set-volume` against
    ///     `@DEFAULT_AUDIO_SINK@` would fail for the same reason the read
    ///     did, and offering a control that is known to fail is exactly what
    ///     `settings/mod.rs`'s honesty contract forbids.
    /// A backend that read once and then failed STAYS interactive — that is
    /// a plausibly transient fault, and the operator retrying is reasonable.
    pub(crate) fn is_interactive(&self) -> bool {
        self.has_value && self.backend_kind.is_some_and(AudioBackendKind::is_usable)
    }

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
                self.last_call_failed = false;
                self.has_value = true;
            }
            Err(AudioError::Unavailable(reason)) => {
                self.last_call_failed = true;
                eprintln!("[audio] backend call failed: {reason}");
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

    /// Q1 (2026-08-24): the override is behind the compile-time shipping
    /// gate, so this asserts a DIFFERENT (and equally real) thing in each
    /// build rather than being `#[cfg]`-skipped in one of them — see
    /// `crate::shipping`'s header comment.
    ///
    /// On a non-Linux host both branches land on `Fake` anyway (there is no
    /// `wpctl` module compiled at all), so the gate is only observable on
    /// Linux — which is exactly the platform where a simulated volume slider
    /// on a duty machine would be the lie D5 removed the Linux fake fallback
    /// to prevent.
    #[test]
    fn fake_audio_env_override_forces_fake_kind_only_in_a_debug_build() {
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
        if crate::shipping::debug_affordances_available() || !cfg!(target_os = "linux") {
            assert_eq!(kind, AudioBackendKind::Fake);
        } else {
            assert_ne!(
                kind,
                AudioBackendKind::Fake,
                "a shipping build on Linux must report the real audio state, never simulate it"
            );
        }
    }

    /// The same decision with NO ambient input. The test above has to read
    /// the process environment to exercise the gate, which makes it race
    /// every other test in this binary that calls `set_var`; these two assert
    /// the decision itself, so the D5 invariant is pinned deterministically
    /// whichever way that race falls and in BOTH cargo feature
    /// configurations (the gated test's Linux branch only checks this
    /// invariant in a shipping build).
    #[test]
    fn an_override_in_effect_selects_the_demo_backend_on_every_platform() {
        let (_, kind) = select_backend_with(true);
        assert_eq!(kind, AudioBackendKind::Fake);
    }

    /// D5's headline decision: on the ONE platform whose image ships
    /// PipeWire, a probe failure is REPORTED, never simulated. Whether this
    /// run lands on `Real` or `Unavailable` depends on whether the machine
    /// running the tests has a working PipeWire — either is correct, and
    /// `Fake` is the answer that must be unreachable.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_linux_host_never_falls_back_to_the_demo_backend() {
        let (_, kind) = select_backend_with(false);
        assert_ne!(kind, AudioBackendKind::Fake, "a Linux box must report a broken audio stack, not paper over it");
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

    /// D5's headline behaviour change: a fresh state carries NO number that
    /// could be mistaken for a reading. Before this round it opened at 62%
    /// (`fake::SEED_PCT`, copied from a static fake-data table) and rendered
    /// that as if it were the machine's volume.
    #[test]
    fn a_fresh_ui_state_has_read_nothing_and_invents_no_percentage() {
        let ui = AudioUiState::default();
        assert!(!ui.has_reading(), "nothing has been read yet");
        assert!(!ui.is_interactive(), "a control with no reading must not be draggable");
        assert_eq!(ui.backend_kind, None);
        assert!(!ui.in_flight);
        assert!(!ui.probe_started);
        assert!(!ui.last_call_failed);
        assert_ne!(ui.pct, fake::SEED_PCT, "the demo backend's seed must not be the UI's opening value");
    }

    /// `Unavailable` settles like any other backend (kind recorded, busy
    /// cleared) but must never look like a successful reading of 0%.
    #[test]
    fn an_unavailable_backend_settles_without_producing_a_reading() {
        let mut ui = AudioUiState::default();
        ui.begin(None);
        ui.settle(AudioBackendKind::Unavailable, Err(AudioError::Unavailable("no pipewire".to_string())));
        assert!(!ui.in_flight);
        assert_eq!(ui.backend_kind, Some(AudioBackendKind::Unavailable));
        assert!(!ui.has_reading(), "a failed call is not a reading");
        assert!(!ui.is_interactive(), "an unavailable backend can perform nothing");
        assert!(ui.last_call_failed);
    }

    /// A real backend whose LATEST call failed keeps the last real value and
    /// stays interactive — a transient `wpctl` hiccup must not brick the
    /// slider, and the stale-but-real number is the honest thing to show.
    #[test]
    fn a_failed_call_after_a_good_read_keeps_the_reading_and_the_control() {
        let mut ui = AudioUiState::default();
        ui.settle(AudioBackendKind::Real, Ok(VolumeState { pct: 40, muted: false }));
        ui.begin(Some(70));
        ui.settle(AudioBackendKind::Real, Err(AudioError::Unavailable("boom".to_string())));
        assert!(ui.has_reading());
        assert!(ui.is_interactive());
        assert!(ui.last_call_failed);
    }

    #[test]
    fn a_later_success_clears_the_failure_flag() {
        let mut ui = AudioUiState::default();
        ui.settle(AudioBackendKind::Real, Err(AudioError::Unavailable("boom".to_string())));
        assert!(ui.last_call_failed);
        ui.settle(AudioBackendKind::Real, Ok(VolumeState { pct: 10, muted: true }));
        assert!(!ui.last_call_failed);
        assert!(ui.has_reading());
    }

    #[test]
    fn only_the_unavailable_kind_is_unusable() {
        assert!(AudioBackendKind::Real.is_usable());
        assert!(AudioBackendKind::Fake.is_usable(), "the demo backend is labelled, not disabled");
        assert!(!AudioBackendKind::Unavailable.is_usable());
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
