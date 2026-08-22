// Lock screen surface — Shell-S4-lock (2026-08-22).
//
// Visual spec: `commercial/design/duduclaw-os-desktop/LockScreen.dc.html` —
// a full-screen dark-navy surface (clock, a "你離開的 2 小時" duty summary
// card, an unlock hint). Research: `research/native-os-2026-08/
// shell-s4-surfaces-2026-08.md` §3 — the load-bearing findings this module
// implements: (1) every mainstream lockscreen precedent (macOS/Windows/
// GNOME/KDE) treats the lock surface as READ-ONLY — interaction requires
// unlocking first, never a lockscreen-native action; (2) lockscreen
// notification privacy needs at least THREE tiers (完全不顯示/只顯示件數/
// 完整顯示), one step past Windows 11's own two-layer model, because a duty
// machine sits somewhere more visible (living room/office) than a personal
// phone; (3) iOS StandBy's "可遠觀不可互動" (glanceable, not interactive)
// framing is the right posture for the summary card itself.
//
// ── Module split ─────────────────────────────────────────────────────────
// This file (`mod.rs`) is deliberately gpui-FREE — `LockScreenState` and the
// two env-config readers below are plain `&mut self` data, testable without
// a live window, same discipline `surface.rs`'s own header comment
// establishes for `SurfaceState` and `overlay/notifications_feed.rs`
// establishes for `NotificationsFeed`. All gpui rendering + the
// background-thread/`cx.spawn` glue (idle watchdog, the lock/unlock+refresh
// entry points `main.rs`'s action handlers call) live in the sibling
// `render.rs` — same "state machine here, gpui glue in a sibling file"
// split `oobe/mod.rs` (state) / `oobe/render.rs` (rendering) already
// establishes for OOBE.
//
// ── Why NO password on unlock (accepted trade-off, flagged for 拍板) ──────
// Every OS precedent surveyed unlocks via a password/PIN/biometric — this
// surface does NOT: any key or click wakes the screen straight back to Home,
// no credential check. The task brief calls this out explicitly as the
// intended v1 shape for a duty appliance: the screen lock is a PRIVACY/
// glanceability boundary (don't leave duty-summary content lit on a shared
// screen), not an ACCESS-control boundary — the machine itself already sits
// behind whatever physical/network security protects the appliance, and the
// gateway's own account system (OOBE's `AccountCreate` step) is a SEPARATE
// concept guarding the dashboard/API, not this screen. This is a genuine
// product decision, not an oversight: 是否要補密碼解鎖是本輪明確列的拍板
// 項，尚未拍板前維持「任意鍵/點擊喚回」。See `render.rs`'s own header
// comment for the corresponding "no interactive elements" design that falls
// out of this choice.

use std::time::{Duration, Instant};

pub(crate) mod render;

/// `DUDUCLAW_SHELL_LOCK_PRIVACY` env var's three values — task brief's own
/// wording ("完全不顯示/只顯示件數/完整顯示"), defaulting to the MIDDLE tier
/// (`Count`) per the task brief's explicit "預設取中間檔" instruction and the
/// research doc's §3.3 recommendation (a duty machine's screen is more
/// exposed than a personal device, so content-level detail should not be
/// the out-of-the-box default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockPrivacy {
    /// Clock only. No summary card is rendered at all — not even a count.
    None,
    /// The default: show the aggregate pending-approval COUNT ("N 件等你決
    /// 定"), never any individual item's summary text.
    Count,
    /// Show the summary card's real content (the count plus a couple of
    /// real pending-approval summaries) — the same detail an operator would
    /// see once actually unlocked.
    Full,
}

impl LockPrivacy {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "none" => Some(LockPrivacy::None),
            "count" => Some(LockPrivacy::Count),
            "full" => Some(LockPrivacy::Full),
            _ => None,
        }
    }
}

/// Reads `DUDUCLAW_SHELL_LOCK_PRIVACY` fresh every call — same "read live at
/// the point of use, never cached at boot" convention `audio::select_backend`
/// / `oobe::network::select_backend` already establish for their own env
/// overrides (`DUDUCLAW_SHELL_FAKE_AUDIO`/`DUDUCLAW_SHELL_FAKE_NET`), so an
/// operator can flip this without restarting the shell. Unset, empty, or an
/// unrecognized value all fall back to `Count` (never a panic) — an
/// unrecognized value is logged, matching `main.rs`'s own
/// `DUDUCLAW_SHELL_DEBUG_SURFACE` handling of a typo'd value.
pub(crate) fn privacy_from_env() -> LockPrivacy {
    match std::env::var("DUDUCLAW_SHELL_LOCK_PRIVACY") {
        Ok(raw) if raw.trim().is_empty() => LockPrivacy::Count,
        Ok(raw) => LockPrivacy::parse(raw.trim()).unwrap_or_else(|| {
            eprintln!("[lockscreen] DUDUCLAW_SHELL_LOCK_PRIVACY={raw:?} unrecognized, using default \"count\"");
            LockPrivacy::Count
        }),
        Err(_) => LockPrivacy::Count,
    }
}

/// Default idle-to-lock threshold when `DUDUCLAW_SHELL_LOCK_IDLE_MINS` is
/// unset — task brief's own default.
const DEFAULT_IDLE_MINS: u64 = 10;

/// Reads `DUDUCLAW_SHELL_LOCK_IDLE_MINS` fresh every call (same live-read
/// convention as `privacy_from_env`). `0` explicitly disables idle auto-lock
/// (task brief: "0=關") — returned as `None` so callers (`render::
/// maybe_auto_lock`) have a single unambiguous "don't even check" signal
/// rather than comparing against a zero `Duration` at every call site. An
/// unparseable value falls back to the default rather than disabling
/// auto-lock outright — a typo'd config should degrade to a safe default,
/// not silently turn off a privacy feature.
pub(crate) fn idle_after_from_env() -> Option<Duration> {
    let default = Some(Duration::from_secs(DEFAULT_IDLE_MINS * 60));
    match std::env::var("DUDUCLAW_SHELL_LOCK_IDLE_MINS") {
        Ok(raw) if raw.trim().is_empty() => default,
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(mins) => Some(Duration::from_secs(mins * 60)),
            Err(_) => {
                eprintln!(
                    "[lockscreen] DUDUCLAW_SHELL_LOCK_IDLE_MINS={raw:?} is not a valid non-negative integer, using default {DEFAULT_IDLE_MINS} minutes"
                );
                default
            }
        },
        Err(_) => default,
    }
}

/// Runtime lock-screen state — lives on `ShellView` as `lockscreen` (see
/// `main.rs`'s own field doc comment). Deliberately plain data (no gpui
/// types) — see this file's header comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LockScreenState {
    locked: bool,
    /// `Some` only while `locked` — when the lock STARTED, feeding the
    /// summary card's dynamic "你離開的 {duration}" title
    /// (`render::away_duration_text`). Not simply `locked`'s own toggle
    /// timestamp: `lock()` is idempotent (see its own doc comment), so this
    /// only ever gets a FRESH value on the transition from unlocked ->
    /// locked, never on a redundant re-lock call.
    locked_at: Option<Instant>,
    /// Last observed key/mouse activity — the idle-auto-lock clock. Reset by
    /// `note_input()` (every key/mouse event while unlocked, wired in
    /// `main.rs`'s `Render::render`) and by `unlock()` itself (unlocking IS
    /// activity — without this, a machine that auto-locked, then got
    /// immediately unlocked by one keystroke, would still read as
    /// instantly-idle again and could re-lock on the very next watchdog
    /// tick).
    last_input_at: Instant,
}

impl Default for LockScreenState {
    fn default() -> Self {
        Self { locked: false, locked_at: None, last_input_at: Instant::now() }
    }
}

impl LockScreenState {
    pub(crate) fn is_locked(&self) -> bool {
        self.locked
    }

    pub(crate) fn locked_at(&self) -> Option<Instant> {
        self.locked_at
    }

    /// Idempotent: re-locking an already-locked screen (e.g. the idle
    /// watchdog ticks again before the operator has unlocked) does NOT reset
    /// `locked_at` — the "away for N minutes" duration keeps counting from
    /// the FIRST lock, not the most recent no-op call.
    pub(crate) fn lock(&mut self) {
        if self.locked {
            return;
        }
        self.locked = true;
        self.locked_at = Some(Instant::now());
    }

    /// See `last_input_at`'s own doc comment for why this also resets the
    /// idle clock.
    pub(crate) fn unlock(&mut self) {
        self.locked = false;
        self.locked_at = None;
        self.last_input_at = Instant::now();
    }

    pub(crate) fn note_input(&mut self) {
        self.last_input_at = Instant::now();
    }

    /// Pure predicate the idle watchdog (`render::maybe_auto_lock`) consults
    /// every tick: true when currently UNLOCKED and idle for at least
    /// `idle_after`. Already-locked never reports true — re-arming a lock
    /// that's already locked is `lock()`'s own job (a no-op), not this
    /// predicate's; callers only call this at all when `idle_after_from_env`
    /// returned `Some` (0 = disabled is handled entirely by that fn
    /// returning `None`, this predicate has no opinion on "disabled").
    pub(crate) fn is_idle_past(&self, idle_after: Duration) -> bool {
        !self.locked && self.last_input_at.elapsed() >= idle_after
    }
}

/// Formats an elapsed `Duration` into the summary card's "你離開的 {}" tail
/// — plain zh-TW literal composition, deliberately NOT routed through
/// `crate::i18n` (only the surrounding "你離開的 {}" template itself is a
/// `Key`, see `Key::LockAwaySummaryTitle`'s own doc comment): this crate's
/// established boundary keeps DYNAMIC, server/clock-derived content
/// hardcoded zh-TW even where the surrounding chrome is i18n'd — same split
/// `home.rs::ticker_text`'s own doc comment documents for its "等你批准："
/// prefix. Minutes below 1 read as "剛剛" (just now) rather than "0 分鐘";
/// hours drop the remainder minutes (matching the design board's own simple
/// "2 小時" precision, not a stopwatch).
pub(crate) fn format_away_duration(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    if minutes < 1 {
        "剛剛".to_string()
    } else if minutes < 60 {
        format!("{minutes} 分鐘")
    } else {
        format!("{} 小時", minutes / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same `ENV_LOCK`-guarded discipline `audio::mod`/`oobe::network::mod`
    // already establish for their own process-global env var tests.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env<T>(key: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        let result = f();
        unsafe {
            match &prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        result
    }

    #[test]
    fn starts_unlocked_with_no_locked_at() {
        let state = LockScreenState::default();
        assert!(!state.is_locked());
        assert_eq!(state.locked_at(), None);
    }

    #[test]
    fn lock_sets_locked_and_locked_at() {
        let mut state = LockScreenState::default();
        state.lock();
        assert!(state.is_locked());
        assert!(state.locked_at().is_some());
    }

    #[test]
    fn relocking_an_already_locked_screen_does_not_reset_locked_at() {
        let mut state = LockScreenState::default();
        state.lock();
        let first = state.locked_at();
        state.lock();
        assert_eq!(state.locked_at(), first, "a redundant lock() call must not restart the away-duration clock");
    }

    #[test]
    fn unlock_clears_locked_and_locked_at_and_refreshes_last_input() {
        let mut state = LockScreenState::default();
        state.lock();
        state.unlock();
        assert!(!state.is_locked());
        assert_eq!(state.locked_at(), None);
        assert!(!state.is_idle_past(Duration::from_secs(60)), "unlocking must count as fresh input");
    }

    #[test]
    fn is_idle_past_is_false_while_locked_regardless_of_elapsed_time() {
        let mut state = LockScreenState::default();
        state.lock();
        assert!(!state.is_idle_past(Duration::from_secs(0)), "an already-locked screen never reports idle-past");
    }

    #[test]
    fn is_idle_past_is_false_immediately_after_note_input_against_a_real_threshold() {
        let mut state = LockScreenState::default();
        state.note_input();
        assert!(!state.is_idle_past(Duration::from_secs(60)));
    }

    #[test]
    fn is_idle_past_becomes_true_once_a_zero_threshold_has_any_elapsed_time() {
        let mut state = LockScreenState::default();
        state.note_input();
        std::thread::sleep(Duration::from_millis(5));
        assert!(state.is_idle_past(Duration::from_millis(1)));
    }

    #[test]
    fn privacy_env_default_is_count_when_unset() {
        with_env("DUDUCLAW_SHELL_LOCK_PRIVACY", None, || {
            assert_eq!(privacy_from_env(), LockPrivacy::Count);
        });
    }

    #[test]
    fn privacy_env_parses_all_three_documented_values() {
        with_env("DUDUCLAW_SHELL_LOCK_PRIVACY", Some("none"), || assert_eq!(privacy_from_env(), LockPrivacy::None));
        with_env("DUDUCLAW_SHELL_LOCK_PRIVACY", Some("count"), || assert_eq!(privacy_from_env(), LockPrivacy::Count));
        with_env("DUDUCLAW_SHELL_LOCK_PRIVACY", Some("full"), || assert_eq!(privacy_from_env(), LockPrivacy::Full));
    }

    #[test]
    fn privacy_env_falls_back_to_count_on_an_unrecognized_value() {
        with_env("DUDUCLAW_SHELL_LOCK_PRIVACY", Some("bogus"), || {
            assert_eq!(privacy_from_env(), LockPrivacy::Count);
        });
    }

    #[test]
    fn idle_env_default_is_ten_minutes_when_unset() {
        with_env("DUDUCLAW_SHELL_LOCK_IDLE_MINS", None, || {
            assert_eq!(idle_after_from_env(), Some(Duration::from_secs(10 * 60)));
        });
    }

    #[test]
    fn idle_env_zero_disables_auto_lock() {
        with_env("DUDUCLAW_SHELL_LOCK_IDLE_MINS", Some("0"), || {
            assert_eq!(idle_after_from_env(), None);
        });
    }

    #[test]
    fn idle_env_parses_a_custom_value() {
        with_env("DUDUCLAW_SHELL_LOCK_IDLE_MINS", Some("3"), || {
            assert_eq!(idle_after_from_env(), Some(Duration::from_secs(3 * 60)));
        });
    }

    #[test]
    fn idle_env_falls_back_to_default_on_an_unparseable_value() {
        with_env("DUDUCLAW_SHELL_LOCK_IDLE_MINS", Some("not-a-number"), || {
            assert_eq!(idle_after_from_env(), Some(Duration::from_secs(10 * 60)));
        });
    }

    #[test]
    fn format_away_duration_under_a_minute_reads_as_just_now() {
        assert_eq!(format_away_duration(Duration::from_secs(30)), "剛剛");
    }

    #[test]
    fn format_away_duration_reports_whole_minutes_under_an_hour() {
        assert_eq!(format_away_duration(Duration::from_secs(5 * 60)), "5 分鐘");
        assert_eq!(format_away_duration(Duration::from_secs(59 * 60)), "59 分鐘");
    }

    #[test]
    fn format_away_duration_reports_whole_hours_dropping_remainder_minutes() {
        assert_eq!(format_away_duration(Duration::from_secs(2 * 3600 + 45 * 60)), "2 小時");
        assert_eq!(format_away_duration(Duration::from_secs(3600)), "1 小時");
    }
}
