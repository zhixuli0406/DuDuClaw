// Deterministic in-memory audio backend — every platform, always compiled.
// See `mod.rs`'s header comment for the three situations that select this:
// Mac dev loop (no `wpctl` module compiled at all), `DUDUCLAW_SHELL_FAKE_AUDIO=1`
// forced override, and Linux with `wpctl` unreachable.
//
// Unlike `oobe::network::FakeNetworkBackend` (which is stateless — every
// call reads straight from a `const` table, see that struct's own header
// comment on why `status()` can always answer `Disconnected` without
// tracking anything), a fake VOLUME has to remember what it was last set
// to: `ControlCenter`'s slider is meaningless if every `set_volume` is
// forgotten the instant the call returns. `select_backend()` (`mod.rs`,
// mirroring `oobe::network::select_backend`) constructs a brand new
// `FakeAudioBackend` instance on EVERY call — same "fresh handle per call,
// no persistent connection pool" simplicity that module's own header
// comment documents for the real Wi-Fi backend — so the state can't live on
// `Self`; it lives in two process-wide atomics instead. This is still an
// honest simulation of a real device (a real audio sink's volume also
// outlives any one process handle to it), just backed by `AtomicU8`/
// `AtomicBool` rather than a kernel mixer.
//
// `Ordering::Relaxed` throughout: these atomics coordinate ControlCenter's
// own single-threaded UI logic with exactly one short-lived background
// thread at a time (`overlay::controlcenter`'s `kick_off_audio_call`), never
// two threads racing to observe an ordering-sensitive side effect on
// anything else — the same "operations serialize by construction, not by
// atomic ordering" shape `duduclaw-shell` has no prior art for, but matches
// how e.g. `main.rs`'s `diag_enabled()` `OnceLock<bool>` reasons about its
// own one-writer-many-readers shape.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::{AudioBackend, AudioError, OutputDevice, VolumeState};

/// Seed volume, percent — mirrors `fake_data::SLIDER_ROWS[0].pct` (`0.62`),
/// the exact static number ControlCenter showed before this round. Kept as
/// a literal `const` (not computed from `fake_data` at `static` init time,
/// which would need a `const fn` float `.round()` this workspace's MSRV
/// doesn't guarantee) — `tests::seed_pct_matches_fake_data_slider_row`
/// below is the drift guard that keeps the two numbers honest instead.
pub(super) const SEED_PCT: u8 = 62;

static VOLUME_PCT: AtomicU8 = AtomicU8::new(SEED_PCT);
static MUTED: AtomicBool = AtomicBool::new(false);

pub(crate) struct FakeAudioBackend;

impl FakeAudioBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl AudioBackend for FakeAudioBackend {
    fn get_volume(&self) -> Result<VolumeState, AudioError> {
        Ok(VolumeState { pct: VOLUME_PCT.load(Ordering::Relaxed), muted: MUTED.load(Ordering::Relaxed) })
    }

    fn set_volume(&self, pct: u8) -> Result<VolumeState, AudioError> {
        // `pct` is already `u8` (never negative) — only the upper bound
        // needs clamping, same "accept the caller's intent, clamp rather
        // than reject" choice `WpctlAudioBackend::set_volume` makes for the
        // real backend (see that fn's own doc comment), so a slider drag
        // that briefly computes 100+ from a fast mouse move never surfaces
        // as an error either backend.
        let clamped = pct.min(100);
        VOLUME_PCT.store(clamped, Ordering::Relaxed);
        self.get_volume()
    }

    fn toggle_mute(&self) -> Result<VolumeState, AudioError> {
        MUTED.fetch_xor(true, Ordering::Relaxed);
        self.get_volume()
    }

    /// Successfully enumerates ZERO devices (D5, 2026-08-24) — and that is
    /// the honest answer, not a stub.
    ///
    /// This backend simulates a volume knob so the macOS dev loop's
    /// ControlCenter slider stays draggable; it does not simulate hardware.
    /// Inventing a "示範輸出裝置" row would put fabricated device names on a
    /// settings page whose entire contract is that it never does that
    /// (`settings/mod.rs`'s honesty section). An empty list renders as
    /// 「沒有可用的輸出裝置」, which is exactly true of a machine whose audio
    /// backend is a pair of atomics.
    ///
    /// It is unreachable from the settings page in practice anyway: that
    /// page runs its own probe and only reaches its device list on
    /// `Availability::Available`, which requires a real `wpctl` and a real
    /// PipeWire socket. Implemented honestly rather than with `unreachable!()`
    /// because a panic in a settings page is never the right answer to a
    /// surprise (same reasoning that page's own `Load::Failed` arm gives).
    fn list_outputs(&self) -> Result<Vec<OutputDevice>, AudioError> {
        Ok(Vec::new())
    }

    /// There is nothing to switch to — see `list_outputs` above. An `Ok`
    /// here would report a successful switch that did not happen.
    fn set_default_output(&self, _id: u32) -> Result<Vec<OutputDevice>, AudioError> {
        Err(AudioError::Unavailable("the demo audio backend has no output devices to switch between".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Same rationale `oobe/network/mod.rs`'s own `ENV_LOCK` documents:
    // `VOLUME_PCT`/`MUTED` are process-global, and `cargo test` runs tests
    // in parallel threads within one process by default — every test here
    // resets BOTH atomics to a value it knows explicitly (never assumes
    // "the pristine default", which a previous test running first would
    // have already clobbered) before asserting on the OUTCOME of its own
    // actions.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() {
        VOLUME_PCT.store(SEED_PCT, Ordering::Relaxed);
        MUTED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn get_volume_reads_back_the_seed_before_any_set() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        let backend = FakeAudioBackend::new();
        let state = backend.get_volume().unwrap();
        assert_eq!(state.pct, SEED_PCT);
        assert!(!state.muted);
    }

    #[test]
    fn set_volume_persists_across_a_fresh_backend_handle() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        FakeAudioBackend::new().set_volume(40).unwrap();
        // A NEW instance — proves the state lives in the shared statics,
        // not on `Self` (which `select_backend()` never reuses across
        // calls, see this module's own header comment).
        let state = FakeAudioBackend::new().get_volume().unwrap();
        assert_eq!(state.pct, 40);
    }

    #[test]
    fn set_volume_clamps_above_100_to_100() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        let backend = FakeAudioBackend::new();
        let state = backend.set_volume(255).unwrap();
        assert_eq!(state.pct, 100);
    }

    #[test]
    fn set_volume_to_zero_is_honored_exactly() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        let backend = FakeAudioBackend::new();
        let state = backend.set_volume(0).unwrap();
        assert_eq!(state.pct, 0);
    }

    #[test]
    fn toggle_mute_flips_and_flips_back_without_touching_volume() {
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        let backend = FakeAudioBackend::new();
        backend.set_volume(55).unwrap();
        let after_first = backend.toggle_mute().unwrap();
        assert!(after_first.muted);
        assert_eq!(after_first.pct, 55);
        let after_second = backend.toggle_mute().unwrap();
        assert!(!after_second.muted);
        assert_eq!(after_second.pct, 55);
    }

    #[test]
    fn set_volume_never_clears_an_existing_mute() {
        // A volume drag while muted (e.g. dragging to preview a level
        // before un-muting) shouldn't silently un-mute — same "each
        // operation only touches its own field" contract `toggle_mute`'s
        // own test above pins from the other direction.
        let _g = TEST_LOCK.lock().unwrap();
        reset();
        let backend = FakeAudioBackend::new();
        backend.toggle_mute().unwrap();
        let state = backend.set_volume(20).unwrap();
        assert!(state.muted);
        assert_eq!(state.pct, 20);
    }

    #[test]
    fn seed_pct_matches_fake_data_slider_row() {
        // Drift guard promised by `SEED_PCT`'s own doc comment — compares
        // the literal against a fresh computation from `fake_data`'s own
        // number, never touches the mutable atomics above, so it's safe
        // under test parallelism regardless of `TEST_LOCK` contention.
        let from_fake_data = (crate::fake_data::SLIDER_ROWS[0].pct * 100.0).round() as u8;
        assert_eq!(SEED_PCT, from_fake_data);
    }

    /// D5: the demo backend enumerates no hardware and says so, rather than
    /// inventing a device row for a settings page that forbids fake data.
    #[test]
    fn the_demo_backend_owns_no_output_devices() {
        let backend = FakeAudioBackend::new();
        assert_eq!(backend.list_outputs().unwrap(), Vec::new());
        assert!(backend.set_default_output(1).is_err(), "switching to a device that does not exist must not report success");
    }
}
