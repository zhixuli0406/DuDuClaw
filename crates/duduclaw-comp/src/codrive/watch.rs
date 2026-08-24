//! CD-3 codrive — watch-mode idle auto-pause (DESIGN-codrive-desktop-
//! 2026-08.md §3.4 "Watch mode（共識⑥）：敏感情境…觸發後，該軌跡剩餘全程要求
//! 人在場…人離開即自動暫停"). Task brief item 3's comp-side half:
//! `{"op":"watch","enable":true|false}` toggles idle-based supervision for
//! the rest of a session; `codrive_check_watch_idle` (called once per redraw
//! from `winit_backend.rs`, the same hook `highlight.rs`'s expiry check and
//! `shadow.rs`'s PiP render already use) auto-freezes the seat once the
//! human seat has been silent for too long, and `on_human_input`
//! (`mod.rs`) auto-lifts that specific pause the instant any human input
//! proves presence again — see `codrive_try_watch_resume` below for why
//! that's a DIFFERENT recovery shape than every other frozen state here.
//!
//! ## Design assumptions this round made (DESIGN doesn't spell these out —
//! same "MVP + document assumptions" convention `shadow.rs`'s module doc
//! established)
//! - **Idle threshold is an env var, not a CLI flag or config file.** The
//!   task brief offered either ("`--codrive-watch-idle-secs` 或 config"). This
//!   crate has no config-file reader at all (comp is Linux-only Rust with a
//!   deliberately thin dependency list — see Cargo.toml's workspace-detach
//!   comment) and `main.rs`'s only existing CLI parsing is the ad hoc
//!   `-c/--command` scan. `DUDUCLAW_CODRIVE_WATCH_IDLE_SECS` follows the
//!   exact precedent `DUDUCLAW_CODRIVE_DEBUG_STDIN` already set for a comp-
//!   level tunable — read once (cached in a `OnceLock`), default
//!   [`DEFAULT_WATCH_IDLE_SECS`] (60s, the task brief's own conservative
//!   fallback value) if unset or unparseable.
//! - **Turning watch ON resets the idle clock.** Without this, enabling
//!   supervision right after a long quiet stretch (nobody touched input
//!   because nobody needed to, not because they left) would immediately
//!   auto-pause — a surprising false trigger the moment supervision starts.
//! - **Shadow work needs no special-casing here at all.** A watch-idle pause
//!   sets the exact same `self.codrive.frozen` flag a human touch would —
//!   which the EXISTING WP-CD2-freeze-scope mechanism
//!   (`shadow::is_freeze_bypass_eligible`) already lets shadow-confined
//!   commands bypass regardless of freeze cause (takeover is the one
//!   exception, and a watch-idle pause is not a takeover). So "shadow
//!   session 不受 watch 暫停" (task brief item 3's closing clause) falls out
//!   for free — this file never reads `codrive_shadow_active`.
//! - **Disabling watch releases a stale pause.** If `{"op":"watch",
//!   "enable":false}` arrives while `codrive_watch_paused` is true, leaving
//!   it set would strand the agent forever (its only other clearing path is
//!   human input, which the whole point of a "nobody's watching" pause is
//!   that none may arrive for a while) — see `codrive_set_watch`'s `false`
//!   branch.

use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::state::DuduclawComp;

/// Task brief's own fallback value ("無明文則 60s 保守值").
const DEFAULT_WATCH_IDLE_SECS: u64 = 60;
/// Floor on the env-var override — a threshold near zero would make watch
/// mode fire on essentially every redraw tick, which is really "always
/// paused" wearing an idle-timeout costume; reject that shape rather than
/// silently accepting it.
///
/// E1a-1a reuses this as the human-seat synthesis quiet window
/// (`human_seat::HUMAN_ACTIVE_WINDOW`) — see that constant's doc for why the
/// semantics carry over rather than a fresh number being invented.
pub(super) const MIN_WATCH_IDLE_SECS: u64 = 5;

/// Pure parse+clamp, unit-testable without touching the environment.
/// Missing or unparseable input, or a value under [`MIN_WATCH_IDLE_SECS`],
/// all fail open to [`DEFAULT_WATCH_IDLE_SECS`] — an operator typo in this
/// env var should never turn into "watch mode never pauses" or "watch mode
/// panics at startup."
fn parse_watch_idle_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&secs| secs >= MIN_WATCH_IDLE_SECS)
        .unwrap_or(DEFAULT_WATCH_IDLE_SECS)
}

/// Read once per process (an operator changing the env var mid-run has no
/// effect — matches `DUDUCLAW_CODRIVE_DEBUG_STDIN`'s own "checked once at
/// startup" shape) via `std::env::var` rather than plumbing a value through
/// `codrive::init`'s signature.
fn watch_idle_threshold() -> Duration {
    static THRESHOLD: OnceLock<Duration> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        let raw = std::env::var("DUDUCLAW_CODRIVE_WATCH_IDLE_SECS").ok();
        Duration::from_secs(parse_watch_idle_secs(raw.as_deref()))
    })
}

impl DuduclawComp {
    /// `{"op":"watch","enable":true|false}` (this file's module doc). Runs
    /// on the calloop main thread (`codrive::handle_agent_inject`'s
    /// `InjectCmd::Watch` arm — same routing as `Shadow`/`TakeOver`).
    /// Idempotent re-assertion mirrors `codrive_set_shadow`'s own shape.
    pub fn codrive_set_watch(&mut self, enable: bool) {
        if self.codrive_watch_active == enable {
            self.codrive.record(
                if enable { "watch_enabled" } else { "watch_disabled" },
                Some("watch"),
                None,
                None,
                Some("no-op — already in the requested state".into()),
            );
            return;
        }
        self.codrive_watch_active = enable;
        // A2: lockstep mirror, so the socket thread can answer `status`'s
        // `watch_active` field without a main-thread round trip (see
        // `CodriveShared::watch_active`).
        self.codrive.watch_active.store(enable, Ordering::SeqCst);
        if enable {
            // Module doc: avoid an immediate false-trigger right when
            // supervision starts.
            self.codrive_last_human_activity = Instant::now();
        } else {
            // Module doc: never leave a watch-idle pause stranded with no
            // path back — `also_unfreeze: true` since disabling supervision
            // should release the human-facing effect of the pause too, not
            // just the bookkeeping.
            self.codrive_end_watch_pause("watch_disabled", true);
        }
        self.codrive.record(if enable { "watch_enabled" } else { "watch_disabled" }, Some("watch"), None, None, None);
    }

    /// Called once per redraw (`winit_backend.rs`, alongside `codrive_
    /// highlight_elements`'s own per-frame expiry check) — cheap `Instant`
    /// reads only, no allocation, safe for this crate's tight unthrottled
    /// redraw loop (see `shadow.rs`'s module doc for that established
    /// characterization). No-op unless watch mode is on and the seat isn't
    /// already frozen for some other reason.
    pub fn codrive_check_watch_idle(&mut self, now: Instant) {
        if !self.codrive_watch_active {
            return;
        }
        let elapsed = now.saturating_duration_since(self.codrive_last_human_activity);
        if elapsed < watch_idle_threshold() {
            return;
        }
        // A2: recorded BEFORE the freeze, so `codrive_sync_mode` below has
        // the trigger available the moment the derived mode becomes
        // `handover`. Never inferred afterwards from flag shapes — a
        // watch-idle pause and a human touch leave identical flags.
        self.codrive_note_handover_reason(super::mode::HandoverReason::WatchIdle);
        let was_frozen = self.codrive.frozen.swap(true, Ordering::SeqCst);
        if was_frozen {
            return; // already frozen for some other reason — nothing to do
        }
        self.codrive_watch_paused = true;
        self.codrive.watch_paused.store(true, Ordering::SeqCst);
        tracing::info!(
            idle_secs = elapsed.as_secs(),
            "codrive: watch-mode idle timeout — auto-pausing the agent seat"
        );
        self.codrive.record(
            "watch_paused",
            None,
            None,
            None,
            Some(format!(
                "idle_for={}s (threshold={}s)",
                elapsed.as_secs(),
                watch_idle_threshold().as_secs()
            )),
        );
        self.codrive.push_event(r#"{"event":"watch_paused"}"#);
        // A2: an idle auto-pause IS a `codrive -> handover` transition.
        self.codrive_sync_mode();
    }

    /// Called from the TOP of `on_human_input` (`mod.rs`), before the
    /// ordinary freeze-on-touch logic — module doc's central claim: a
    /// watch-idle pause is lifted by mere PRESENCE (any human input event),
    /// not an explicit "交還" gesture, because the pause existed only to
    /// answer "is anyone watching" and this event already answers it.
    /// Returns `true` iff it handled the event (caller must return early
    /// instead of falling through to the ordinary touch-freezes-it path —
    /// otherwise the very next line would immediately re-freeze the seat
    /// for the "human touched it" reason, defeating the auto-resume).
    pub fn codrive_try_watch_resume(&mut self) -> bool {
        if !self.codrive_watch_paused {
            return false;
        }
        self.codrive_end_watch_pause("human_input", true);
        true
    }

    /// Shared "a watch-idle pause ends" transition, reused by three
    /// callers: `codrive_try_watch_resume` above (`also_unfreeze: true` —
    /// this call IS what ends the pause), and `mod.rs`'s `human_resume`/
    /// `emergency_stop` (`also_unfreeze: false` — those already clear/set
    /// `frozen` themselves; this just clears the `codrive_watch_paused`
    /// bookkeeping flag and gives it an audited terminal transition so it
    /// can never dangle `true` after `frozen` has already changed for an
    /// unrelated reason). No-op (no audit line) if no watch-idle pause was
    /// active — same "nothing to undo" shape as `codrive_end_takeover_if_
    /// active`.
    pub(super) fn codrive_end_watch_pause(&mut self, trigger: &'static str, also_unfreeze: bool) {
        if !self.codrive_watch_paused {
            return;
        }
        self.codrive_watch_paused = false;
        self.codrive.watch_paused.store(false, Ordering::SeqCst);
        if also_unfreeze {
            self.codrive.frozen.store(false, Ordering::SeqCst);
        }
        tracing::info!(trigger, "codrive: watch-idle pause ended");
        self.codrive.record("watch_resumed", None, None, None, Some(trigger.to_string()));
        self.codrive.push_event(r#"{"event":"watch_resumed"}"#);
        // A2: `also_unfreeze` makes this a `handover -> codrive` transition;
        // the other two callers (`human_resume`/`emergency_stop`) have
        // already changed `frozen`/`terminated` themselves, and this call
        // simply observes whatever they produced. A no-op when nothing
        // actually moved.
        self.codrive_sync_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_watch_idle_secs_defaults_when_unset() {
        assert_eq!(parse_watch_idle_secs(None), DEFAULT_WATCH_IDLE_SECS);
    }

    #[test]
    fn parse_watch_idle_secs_defaults_on_garbage() {
        assert_eq!(parse_watch_idle_secs(Some("not-a-number")), DEFAULT_WATCH_IDLE_SECS);
        assert_eq!(parse_watch_idle_secs(Some("")), DEFAULT_WATCH_IDLE_SECS);
        assert_eq!(parse_watch_idle_secs(Some("-5")), DEFAULT_WATCH_IDLE_SECS);
    }

    #[test]
    fn parse_watch_idle_secs_defaults_below_floor() {
        assert_eq!(parse_watch_idle_secs(Some("1")), DEFAULT_WATCH_IDLE_SECS);
        assert_eq!(parse_watch_idle_secs(Some("4")), DEFAULT_WATCH_IDLE_SECS);
    }

    #[test]
    fn parse_watch_idle_secs_accepts_valid_override() {
        assert_eq!(parse_watch_idle_secs(Some("120")), 120);
        assert_eq!(parse_watch_idle_secs(Some(" 30 ")), 30, "surrounding whitespace must be trimmed");
    }

    #[test]
    fn parse_watch_idle_secs_accepts_the_floor_itself() {
        assert_eq!(parse_watch_idle_secs(Some("5")), 5);
    }
}
