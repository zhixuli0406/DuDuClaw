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
// ── Password unlock (WP-lock-pw, 2026-08-22 — REVERSES the S4-lock MVP) ──
// Shell-S4-lock originally shipped with NO credential check at all (any key
// or click woke the screen straight back to Home) — a deliberate v1 choice,
// explicitly flagged in this file's own header as pending a 拍板 decision.
// That decision landed 2026-08-22: **any-key-unlock is REVOKED**. The first
// key/click after locking now only REVEALS a password field (clock/summary
// card stay exactly where they were, per the original spec's own "read-only
// surface" framing — see `render.rs`'s header comment); actually unlocking
// requires the operator to type the `admin@local` password and press Enter,
// verified against the real gateway's `POST /api/login`
// (`gateway_client::verify_password` — see that module's own header
// comment). The `LockPrivacy`/idle-lock machinery below is UNCHANGED by
// this round; only the unlock PATH gained a credential gate.
//
// ── Offline fail-safe semantics (the load-bearing design decision here) ──
// A password can only be VERIFIED by asking the gateway — if the gateway is
// unreachable, there is no way to know whether a typed password is correct,
// so this surface's only honest choice is to STAY LOCKED and say so
// (`UnlockFailureKind::Unreachable` -> `Key::LockOfflineError`, "本機服務未
// 回應"), never to fail open. This is a conscious trade-off, not an
// oversight: a duty machine that unlocked itself the moment its own gateway
// process happened to be down would defeat the entire point of a lock
// screen. The escape hatch for a genuinely stuck machine (gateway crashed,
// won't restart, operator physically needs in) is the SAME one every
// server/appliance already has — a local serial/TTY console with its own
// OS-level login, entirely outside this gpui surface's control — not
// something this module needs to (or should) reimplement. Once the gateway
// comes back, the very next Enter press against the SAME typed password
// (still sitting in the field — see `UnlockFailureKind::Unreachable`'s own
// handling in `render.rs`) verifies normally; no special recovery flow
// exists because none is needed. `DUDUCLAW_SHELL_LOCK_NO_PASSWORD=1`
// (`password_required_from_env` below) is the SEPARATE, explicit opt-out
// for dev/headless/unattended-kiosk deployments where a password prompt
// would strand an operator with no keyboard at all — it is not a substitute
// for the offline fail-safe above (a machine that opted OUT of passwords
// entirely still just unlocks on any key, offline or not, same as before
// this round).

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

/// `DUDUCLAW_SHELL_LOCK_NO_PASSWORD` — WP-lock-pw's dev/headless escape
/// hatch (task brief: "開發/無人值守場景明確 opt-out，預設密碼開"). When
/// set to exactly `"1"`, the lockscreen reproduces the ORIGINAL Shell-S4-lock
/// behavior verbatim: any key or click unlocks immediately, no password
/// prompt is ever revealed, no gateway round trip — for headless smoke runs
/// and unattended-kiosk deployments where a password prompt would strand
/// the operator with no keyboard (see this file's header comment on why
/// this is a SEPARATE concern from the offline fail-safe). Read live (not
/// cached at boot), same convention `privacy_from_env`/`idle_after_from_env`
/// establish. Any other value — including unset, empty, or a typo — means
/// "password required": the safe default, so a malformed value can never
/// accidentally disable the credential gate.
pub(crate) fn password_required_from_env() -> bool {
    !std::env::var("DUDUCLAW_SHELL_LOCK_NO_PASSWORD").is_ok_and(|v| v.trim() == "1")
}

/// Consecutive failed verify attempts (since the lock started, or since the
/// last one settled) before the client-side throttle kicks in — task
/// brief: "3 次後 2 秒節流防爆破".
const FAILS_BEFORE_THROTTLE: u32 = 3;
/// How long a submit is refused once the throttle is active — task brief's
/// own number. NOT a lockout: the very next Enter press after this window
/// elapses is accepted normally, and the typed password is never cleared
/// just because a submit was refused (see `UnlockPrompt`'s own doc comment).
const THROTTLE_DURATION: Duration = Duration::from_secs(2);

/// The password prompt's own sub-state (WP-lock-pw, 2026-08-22) — only ever
/// meaningful while the parent `LockScreenState.locked` is `true`;
/// `LockScreenState::unlock()` resets this to `default()` unconditionally,
/// so no error/attempt-count/visibility ever survives past a successful
/// unlock into the NEXT lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct UnlockPrompt {
    /// Hidden until the first key/click after locking reveals it (task
    /// brief: "按任意鍵/點擊 → 出現密碼輸入框") — see
    /// `LockScreenState::reveal_prompt`.
    visible: bool,
    phase: UnlockPhase,
    /// Consecutive failed verify attempts since this lock started (or since
    /// the last successful unlock) — feeds `FAILS_BEFORE_THROTTLE` above.
    /// Deliberately NOT reset by a `Failed` -> re-shown transition, only by
    /// `unlock()` — "不鎖死" (task brief) means the operator can always
    /// keep trying, not that the throttle forgets a run of recent failures.
    fail_count: u32,
    /// When the most recent attempt SETTLED as a FAILURE (a success instead
    /// resets the whole struct via `unlock()`, so this field never needs to
    /// distinguish "settled successfully") — the throttle clock
    /// `LockScreenState::can_submit` reads against `THROTTLE_DURATION`.
    last_attempt_at: Option<Instant>,
}

/// The password field's current activity — drives `render.rs`'s own
/// verifying-spinner/error-line choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UnlockPhase {
    #[default]
    Idle,
    InFlight,
    Failed(UnlockFailureKind),
}

/// Why the most recent verify attempt failed — `render.rs`'s own mapping to
/// `Key::LockPasswordWrongError`/`Key::LockOfflineError` collapses
/// `gateway_client::LoginError`'s finer-grained variants down to exactly
/// these two honest UI states (see that module's own doc comment on why a
/// rare 429 belongs in `Unreachable` rather than a third state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnlockFailureKind {
    /// A real password mismatch — the gateway is reachable and said so.
    WrongPassword,
    /// Every other failure — offline, timeout, unexpected HTTP status,
    /// malformed response, the gateway's own rate limit — one honest
    /// fail-safe message. See this file's header comment ("Offline
    /// fail-safe semantics") for why this surface stays locked rather than
    /// guessing.
    Unreachable,
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
    /// WP-lock-pw (2026-08-22) — the password prompt's own sub-state. See
    /// `UnlockPrompt`'s own doc comment.
    unlock_prompt: UnlockPrompt,
}

impl Default for LockScreenState {
    fn default() -> Self {
        Self { locked: false, locked_at: None, last_input_at: Instant::now(), unlock_prompt: UnlockPrompt::default() }
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
        // Defensive reset — the only reachable path here is from an
        // unlocked state, where `unlock_prompt` should already sit at its
        // default (nothing else ever sets it while unlocked), but a fresh
        // lock starting from a guaranteed-clean slate costs nothing and
        // removes any need to trust that invariant.
        self.unlock_prompt = UnlockPrompt::default();
    }

    /// See `last_input_at`'s own doc comment for why this also resets the
    /// idle clock. Also resets `unlock_prompt` unconditionally — see that
    /// type's own doc comment: no error/attempt-count/visibility from THIS
    /// lock survives into the next one.
    pub(crate) fn unlock(&mut self) {
        self.locked = false;
        self.locked_at = None;
        self.last_input_at = Instant::now();
        self.unlock_prompt = UnlockPrompt::default();
    }

    pub(crate) fn note_input(&mut self) {
        self.last_input_at = Instant::now();
    }

    pub(crate) fn prompt_visible(&self) -> bool {
        self.unlock_prompt.visible
    }

    pub(crate) fn unlock_phase(&self) -> UnlockPhase {
        self.unlock_prompt.phase
    }

    /// Reveals the password prompt — idempotent, same "a redundant call
    /// costs nothing" discipline `lock()` establishes. Returns `true`
    /// exactly on the Hidden -> visible EDGE (never on a call that finds it
    /// already visible), so callers (`render::note_input_or_reveal`) know
    /// precisely when to also move keyboard focus onto the freshly-rendered
    /// field — focusing on every redundant call would steal focus back from
    /// wherever the operator's cursor already is (e.g. mid-typing inside
    /// the field itself, since every keystroke that isn't a bound action
    /// also reaches this same catch-all — see `render.rs`'s own header
    /// comment on that bubble order).
    pub(crate) fn reveal_prompt(&mut self) -> bool {
        if self.unlock_prompt.visible {
            return false;
        }
        self.unlock_prompt.visible = true;
        true
    }

    /// True once a submit is allowed: no request currently in flight, and
    /// — once `fail_count` has crossed `FAILS_BEFORE_THROTTLE` — the
    /// `THROTTLE_DURATION` cooldown since the last failure has elapsed.
    /// Locked-status itself is NOT checked here (same division of concerns
    /// `is_idle_past` establishes for the idle watchdog) — every real call
    /// site only ever reaches this while already locked.
    pub(crate) fn can_submit(&self, now: Instant) -> bool {
        if self.unlock_prompt.phase == UnlockPhase::InFlight {
            return false;
        }
        if self.unlock_prompt.fail_count < FAILS_BEFORE_THROTTLE {
            return true;
        }
        match self.unlock_prompt.last_attempt_at {
            Some(at) => now.saturating_duration_since(at) >= THROTTLE_DURATION,
            None => true,
        }
    }

    /// True while `fail_count` has crossed the throttle threshold AND the
    /// cooldown since the last failure hasn't elapsed yet — purely a
    /// RENDER-time signal (`render.rs`'s own `Key::LockThrottledLabel`
    /// hint), distinct from `can_submit` above (which also returns `false`
    /// while a request is simply `InFlight`, a different reason with its
    /// own `Key::LockVerifyingLabel` message).
    pub(crate) fn is_throttled(&self, now: Instant) -> bool {
        self.unlock_prompt.fail_count >= FAILS_BEFORE_THROTTLE
            && self.unlock_prompt.phase != UnlockPhase::InFlight
            && self.unlock_prompt.last_attempt_at.is_some_and(|at| now.saturating_duration_since(at) < THROTTLE_DURATION)
    }

    /// Marks a verify request as dispatched — guarded by `can_submit`, so
    /// this never overwrites an in-flight or still-throttled attempt; a
    /// call site that skips the `can_submit` check first gets a defensive
    /// no-op (returns `false`) instead of a double-dispatch.
    pub(crate) fn begin_verify(&mut self, now: Instant) -> bool {
        if !self.can_submit(now) {
            return false;
        }
        self.unlock_prompt.phase = UnlockPhase::InFlight;
        true
    }

    /// Records a settled failure — advances `fail_count` (feeding the
    /// throttle) and stamps `last_attempt_at` (the throttle clock), same
    /// timestamp used for both so a caller can't pass two different
    /// "now"s that disagree.
    pub(crate) fn record_failure(&mut self, kind: UnlockFailureKind, now: Instant) {
        self.unlock_prompt.phase = UnlockPhase::Failed(kind);
        self.unlock_prompt.fail_count += 1;
        self.unlock_prompt.last_attempt_at = Some(now);
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

    // ── WP-lock-pw (2026-08-22): password prompt sub-state ────────────────

    #[test]
    fn no_password_env_default_is_password_required_when_unset() {
        with_env("DUDUCLAW_SHELL_LOCK_NO_PASSWORD", None, || {
            assert!(password_required_from_env());
        });
    }

    #[test]
    fn no_password_env_exactly_one_disables_the_password_requirement() {
        with_env("DUDUCLAW_SHELL_LOCK_NO_PASSWORD", Some("1"), || {
            assert!(!password_required_from_env());
        });
    }

    #[test]
    fn no_password_env_any_other_value_still_requires_a_password() {
        // A typo'd value must degrade to the SAFE default (password
        // required), not silently disable the credential gate.
        with_env("DUDUCLAW_SHELL_LOCK_NO_PASSWORD", Some("true"), || {
            assert!(password_required_from_env());
        });
        with_env("DUDUCLAW_SHELL_LOCK_NO_PASSWORD", Some("0"), || {
            assert!(password_required_from_env());
        });
    }

    #[test]
    fn prompt_starts_hidden_on_a_fresh_lock() {
        let mut state = LockScreenState::default();
        state.lock();
        assert!(!state.prompt_visible());
        assert_eq!(state.unlock_phase(), UnlockPhase::Idle);
    }

    #[test]
    fn reveal_prompt_returns_true_exactly_on_the_hidden_to_visible_edge() {
        let mut state = LockScreenState::default();
        state.lock();
        assert!(state.reveal_prompt(), "first reveal must report the edge");
        assert!(state.prompt_visible());
        assert!(!state.reveal_prompt(), "a redundant reveal while already visible must report no edge");
        assert!(state.prompt_visible(), "still visible after the redundant call");
    }

    #[test]
    fn unlock_resets_the_prompt_back_to_hidden_with_no_leftover_error() {
        let mut state = LockScreenState::default();
        state.lock();
        state.reveal_prompt();
        state.record_failure(UnlockFailureKind::WrongPassword, Instant::now());
        state.unlock();
        assert!(!state.is_locked());
        state.lock();
        assert!(!state.prompt_visible(), "a fresh lock must not inherit the previous lock's revealed prompt");
        assert_eq!(state.unlock_phase(), UnlockPhase::Idle, "a fresh lock must not inherit the previous lock's error");
    }

    #[test]
    fn can_submit_is_true_before_any_attempt() {
        let mut state = LockScreenState::default();
        state.lock();
        assert!(state.can_submit(Instant::now()));
    }

    #[test]
    fn begin_verify_sets_in_flight_and_blocks_a_second_concurrent_submit() {
        let mut state = LockScreenState::default();
        state.lock();
        let now = Instant::now();
        assert!(state.begin_verify(now), "first begin_verify must succeed");
        assert_eq!(state.unlock_phase(), UnlockPhase::InFlight);
        assert!(!state.can_submit(now), "a request already in flight must block a second submit");
        assert!(!state.begin_verify(now), "begin_verify itself must refuse while already in flight");
    }

    #[test]
    fn fewer_than_three_failures_never_throttles() {
        let mut state = LockScreenState::default();
        state.lock();
        let now = Instant::now();
        state.record_failure(UnlockFailureKind::WrongPassword, now);
        assert!(state.can_submit(now), "1st failure must not throttle");
        assert!(!state.is_throttled(now));
        state.record_failure(UnlockFailureKind::WrongPassword, now);
        assert!(state.can_submit(now), "2nd failure must not throttle");
        assert!(!state.is_throttled(now));
    }

    #[test]
    fn the_third_consecutive_failure_throttles_for_the_cooldown_window() {
        let mut state = LockScreenState::default();
        state.lock();
        let now = Instant::now();
        state.record_failure(UnlockFailureKind::WrongPassword, now);
        state.record_failure(UnlockFailureKind::WrongPassword, now);
        state.record_failure(UnlockFailureKind::WrongPassword, now);
        assert!(!state.can_submit(now), "3rd consecutive failure must throttle immediately");
        assert!(state.is_throttled(now));
        assert_eq!(state.unlock_phase(), UnlockPhase::Failed(UnlockFailureKind::WrongPassword));

        let after_cooldown = now + THROTTLE_DURATION;
        assert!(state.can_submit(after_cooldown), "submit must be allowed again once the cooldown elapses");
        assert!(!state.is_throttled(after_cooldown));
    }

    #[test]
    fn throttle_is_not_a_hard_lockout_repeated_failures_keep_allowing_retries_after_each_cooldown() {
        // Task brief: "錯誤次數不鎖死但 3 次後 2 秒節流防爆破" — never a
        // permanent block, just a forced pause between attempts once the
        // threshold is crossed.
        let mut state = LockScreenState::default();
        state.lock();
        let mut now = Instant::now();
        for _ in 0..3 {
            state.record_failure(UnlockFailureKind::WrongPassword, now);
        }
        assert!(!state.can_submit(now));
        now += THROTTLE_DURATION;
        assert!(state.can_submit(now), "must be submittable again after the cooldown");
        state.record_failure(UnlockFailureKind::WrongPassword, now);
        assert!(!state.can_submit(now), "a further failure re-arms the throttle");
        now += THROTTLE_DURATION;
        assert!(state.can_submit(now), "and it clears again after another cooldown — never a permanent lockout");
    }

    #[test]
    fn record_failure_overwrites_an_in_flight_phase() {
        let mut state = LockScreenState::default();
        state.lock();
        let now = Instant::now();
        state.begin_verify(now);
        state.record_failure(UnlockFailureKind::Unreachable, now);
        assert_eq!(state.unlock_phase(), UnlockPhase::Failed(UnlockFailureKind::Unreachable));
        assert!(state.can_submit(now), "below the throttle threshold, settling out of InFlight must immediately allow a retry");
    }
}
