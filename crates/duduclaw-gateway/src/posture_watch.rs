//! OS security line P0 — C1 "producer 乙": drive `duduclaw_security`'s
//! `SecurityPosture` Green/Yellow/Red state machine from a live poll loop,
//! and react to it (design §2 支柱三, `DESIGN-os-security-line-2026-09.md`).
//!
//! ## The gap this closes
//!
//! `duduclaw_security::security_posture` ships a fully-built, unit-tested
//! `PostureTracker` (escalate-fast / decay-slow) and a pure audit-log reader
//! (`posture_from_audit`) — but as of the 2026-09 audit neither had a single
//! production caller. `PostureTracker::observe` was exercised only by its
//! own unit tests, and `os_reconcile::escalation_floor_from_report` (the one
//! other place a posture value is computed) has zero call sites outside its
//! own file. Posture transitions had nowhere to go.
//!
//! This module is the missing caller: a lightweight poll loop, independent
//! of whether the autopilot engine is enabled (this is a core safety
//! mechanism, not an opt-in automation), that re-derives the posture from
//! `security_audit.jsonl` every [`POLL_INTERVAL`] and on every ACTUAL
//! transition:
//!
//!   - emits an `AutopilotEvent::SecurityEvent` (`source: "posture"`) via
//!     [`crate::security_autopilot::emit_security_autopilot_event`] — a
//!     no-op when the autopilot bus isn't wired (autopilot disabled), same
//!     fail-open posture as every other producer in this crate. This is the
//!     C1 "producer 乙" half of the design.
//!   - on a FRESH entry into `Red` (and only when the platform-wide scope
//!     isn't already at/above L2 for some other reason — see
//!     [`decide_action`]): degrades the `"__global__"` failsafe scope to
//!     `L2Restricted` (D2's tier-1 "自動" reaction — record / alert /
//!     capability-tighten, always reversible). Per `channel_reply.rs`'s L1
//!     failsafe gate, `L2Restricted` makes every channel reply return a
//!     canned response instead of calling the AI — a real, meaningful,
//!     REVERSIBLE capability tightening, not a cosmetic flag.
//!   - on the transition back to `Green`, IF AND ONLY IF this loop is the
//!     one that raised the global scope (tracked locally in `we_restricted`,
//!     never inferred from the ambient failsafe level) AND that scope is
//!     STILL exactly at the level this loop set: calls `failsafe.resume()`.
//!     A human `!STOP ALL` (L4) or a circuit-breaker escalation (also
//!     scope-specific, not `__global__`) is never touched by this loop —
//!     recovery from those stays a human/other-mechanism decision (D2's
//!     "永遠人為" boundary). This loop NEVER sets L3/L4 and never lowers a
//!     level it did not itself raise.
//!
//! ## Known limitation (P0 scope, documented rather than solved)
//!
//! `FailsafeManager` has its own independent auto-recovery timer
//! (`killswitch.toml [failsafe] l2_auto_recover_secs`, default 600s). This
//! loop sets `L2Restricted` ONCE on the Red transition; it does not
//! re-affirm it while Red persists. If the posture stays Red for longer
//! than `l2_auto_recover_secs`, `FailsafeManager` will silently auto-recover
//! to `L0Normal` on its own, even though the underlying security situation
//! may be unchanged. A keep-alive re-affirmation is a reasonable P1
//! follow-up; for P0 this fail-open-after-a-timeout behavior is an
//! acceptable (arguably desirable — prevents an eternal degraded state from
//! a stuck bad signal) conservative default, not a silent bug.
//!
//! ## Bounded feedback loop
//!
//! The one audit write this loop performs (`log_failsafe_change`, only on
//! an actual `Restrict`/`Resume` action, so at most once per transition) is
//! always Warning severity (`to_level` is `L2Restricted` or `L0Normal`,
//! neither matches that function's own `L3`/`L4` ⇒ Critical rule) — it
//! cannot, by construction, push the NEXT window's critical count toward
//! `PostureThresholds::default().red_criticals` (3) on its own.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::info;

use duduclaw_security::audit::Severity;
use duduclaw_security::failsafe::{FailsafeLevel, FailsafeManager};
use duduclaw_security::security_posture::{
    PostureThresholds, PostureTracker, SecurityPosture, DEFAULT_WINDOW_SECONDS,
};

/// Platform-wide failsafe scope — matches the `"__global__"` convention
/// `chat_commands.rs`'s `!STOP ALL` / `channel_reply.rs`'s L1 gate already
/// use (`effective_level = max(get_level("__global__"), get_level(session))`).
const GLOBAL_SCOPE: &str = "__global__";

/// How often to re-derive posture from the audit log. Independent of
/// `DEFAULT_WINDOW_SECONDS` (60s, the SIZE of the sliding window each poll
/// evaluates) — this is how often the window is re-evaluated.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// Spawn the poll loop. Runs unconditionally (posture monitoring is a core
/// safety mechanism, not gated on the autopilot engine being enabled) —
/// `failsafe` is threaded through as `Option` only because `ReplyContext`'s
/// field is typed that way; `ReplyContext::new` always constructs one in
/// practice, so `None` here is a defensive fallback (posture is still
/// tracked and still emits `SecurityEvent`s; the capability-tightening half
/// of C1 is simply inert).
pub fn spawn(
    home_dir: std::path::PathBuf,
    failsafe: Option<Arc<FailsafeManager>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tracker = PostureTracker::new(PostureThresholds::default());
        let mut we_restricted = false;
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            we_restricted = poll_once(&home_dir, failsafe.as_ref(), &mut tracker, we_restricted)
                .await
                .1;
        }
    })
}

/// One poll iteration — pulled out of the infinite loop so tests can drive
/// it directly without waiting on [`POLL_INTERVAL`]. Returns
/// `(current_posture, we_restricted)` for the caller to fold back in.
async fn poll_once(
    home_dir: &Path,
    failsafe: Option<&Arc<FailsafeManager>>,
    tracker: &mut PostureTracker,
    we_restricted: bool,
) -> (SecurityPosture, bool) {
    let prev = tracker.current();

    // Blocking file I/O (the audit log can grow large under sustained
    // incidents) — offloaded so a slow read never stalls the tokio runtime.
    let home = home_dir.to_path_buf();
    let (warning, critical) = tokio::task::spawn_blocking(move || {
        let since =
            (chrono::Utc::now() - chrono::Duration::seconds(DEFAULT_WINDOW_SECONDS)).to_rfc3339();
        let (_info, warning, critical) =
            duduclaw_security::audit::count_events_since(&home, &since);
        (warning, critical)
    })
    .await
    .unwrap_or((0, 0));

    let new_posture = tracker.observe(warning, critical, None);
    if new_posture == prev {
        return (new_posture, we_restricted);
    }

    // Producer 乙: mirror the transition onto the autopilot bus. `Info`
    // (only possible for a Green→Green no-op, already short-circuited
    // above, or — n/a here since Green is the lowest state) never applies;
    // Yellow ⇒ Warning, Red ⇒ Critical.
    emit_posture_event(new_posture);

    let Some(fs) = failsafe else {
        return (new_posture, we_restricted);
    };
    let current_level = fs.get_level(GLOBAL_SCOPE).await;
    match decide_action(new_posture, we_restricted, current_level) {
        PostureAction::Restrict => {
            let reason = format!(
                "security posture escalated to RED ({warning} warning + {critical} critical \
                 security_audit.jsonl events in the last {DEFAULT_WINDOW_SECONDS}s)"
            );
            fs.set_level(GLOBAL_SCOPE, FailsafeLevel::L2Restricted, &reason)
                .await;
            duduclaw_security::audit::log_failsafe_change(
                home_dir,
                "system",
                GLOBAL_SCOPE,
                &current_level.to_string(),
                "L2Restricted",
                &reason,
            );
            info!(
                warning,
                critical, "posture_watch: RED — degraded {GLOBAL_SCOPE} failsafe to L2Restricted"
            );
            (new_posture, true)
        }
        PostureAction::Resume => {
            fs.resume(GLOBAL_SCOPE).await;
            duduclaw_security::audit::log_failsafe_change(
                home_dir,
                "system",
                GLOBAL_SCOPE,
                &current_level.to_string(),
                "L0Normal",
                "security posture recovered to GREEN",
            );
            info!("posture_watch: GREEN — restored {GLOBAL_SCOPE} failsafe to L0Normal");
            (new_posture, false)
        }
        PostureAction::None => {
            // Clear our own bookkeeping the moment posture is Green, even if
            // we didn't (or couldn't) resume anything ourselves — a later
            // Red re-entry must start a fresh episode, never carry over a
            // stale "we caused this" flag.
            let still_restricted = we_restricted && new_posture != SecurityPosture::Green;
            (new_posture, still_restricted)
        }
    }
}

fn posture_severity(p: SecurityPosture) -> Severity {
    match p {
        SecurityPosture::Green => Severity::Info,
        SecurityPosture::Yellow => Severity::Warning,
        SecurityPosture::Red => Severity::Critical,
    }
}

fn emit_posture_event(posture: SecurityPosture) {
    crate::security_autopilot::emit_security_autopilot_event(
        &posture_severity(posture),
        "security_posture_change",
        None,
        "posture",
    );
}

/// What, if anything, to do to the `__global__` failsafe scope on THIS
/// posture transition. Pure — no I/O, fully unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostureAction {
    None,
    /// Raise `__global__` to `L2Restricted`.
    Restrict,
    /// Resume `__global__` to `L0Normal`.
    Resume,
}

/// Decide the failsafe action for a posture transition already known to have
/// occurred (`new != prev` is the caller's responsibility — this function
/// doesn't need `prev` at all, only `new`).
///
/// Never raises past `L2Restricted` and never lowers a level this loop did
/// not itself raise:
///   - `Red` only triggers `Restrict` when the CURRENT global level is below
///     `L2Restricted` — if a human already halted everything (`L3`/`L4`) or
///     some other mechanism already restricted it, this loop stays out of
///     the way entirely.
///   - `Green` only triggers `Resume` when `we_restricted` (this loop's own
///     bookkeeping — never inferred from the ambient level) is true AND the
///     current level is STILL exactly what this loop set. If it has since
///     moved (e.g. a human escalated further, or something else already
///     resumed it), this loop does not touch it.
fn decide_action(
    new: SecurityPosture,
    we_restricted: bool,
    current_level: FailsafeLevel,
) -> PostureAction {
    match new {
        SecurityPosture::Red => {
            if current_level < FailsafeLevel::L2Restricted {
                PostureAction::Restrict
            } else {
                PostureAction::None
            }
        }
        SecurityPosture::Green => {
            if we_restricted && current_level == FailsafeLevel::L2Restricted {
                PostureAction::Resume
            } else {
                PostureAction::None
            }
        }
        SecurityPosture::Yellow => PostureAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_security::killswitch::FailsafeConfig;

    // ── Pure decision logic ─────────────────────────────────────────────

    #[test]
    fn red_restricts_only_below_l2() {
        assert_eq!(
            decide_action(SecurityPosture::Red, false, FailsafeLevel::L0Normal),
            PostureAction::Restrict
        );
        assert_eq!(
            decide_action(SecurityPosture::Red, false, FailsafeLevel::L1Degraded),
            PostureAction::Restrict
        );
        assert_eq!(
            decide_action(SecurityPosture::Red, false, FailsafeLevel::L2Restricted),
            PostureAction::None,
            "already at L2 — nothing new to do"
        );
        assert_eq!(
            decide_action(SecurityPosture::Red, false, FailsafeLevel::L4Halted),
            PostureAction::None,
            "must NEVER downgrade a human L4 halt to L2"
        );
    }

    #[test]
    fn green_resumes_only_if_we_restricted_and_still_at_l2() {
        assert_eq!(
            decide_action(SecurityPosture::Green, true, FailsafeLevel::L2Restricted),
            PostureAction::Resume
        );
        assert_eq!(
            decide_action(SecurityPosture::Green, false, FailsafeLevel::L2Restricted),
            PostureAction::None,
            "we never raised it — not ours to lower"
        );
        assert_eq!(
            decide_action(SecurityPosture::Green, true, FailsafeLevel::L4Halted),
            PostureAction::None,
            "level has since moved to something more severe — leave it alone"
        );
        assert_eq!(
            decide_action(SecurityPosture::Green, true, FailsafeLevel::L0Normal),
            PostureAction::None,
            "already back to normal (e.g. auto-recovered) — nothing to do"
        );
    }

    #[test]
    fn yellow_never_touches_failsafe() {
        for level in [
            FailsafeLevel::L0Normal,
            FailsafeLevel::L2Restricted,
            FailsafeLevel::L4Halted,
        ] {
            assert_eq!(
                decide_action(SecurityPosture::Yellow, true, level),
                PostureAction::None
            );
            assert_eq!(
                decide_action(SecurityPosture::Yellow, false, level),
                PostureAction::None
            );
        }
    }

    #[test]
    fn posture_severity_mapping() {
        assert!(matches!(
            posture_severity(SecurityPosture::Green),
            Severity::Info
        ));
        assert!(matches!(
            posture_severity(SecurityPosture::Yellow),
            Severity::Warning
        ));
        assert!(matches!(
            posture_severity(SecurityPosture::Red),
            Severity::Critical
        ));
    }

    // ── poll_once integration (real audit log + real FailsafeManager) ──

    fn test_failsafe() -> Arc<FailsafeManager> {
        Arc::new(FailsafeManager::new(FailsafeConfig::default()))
    }

    #[tokio::test]
    async fn poll_once_is_a_noop_on_a_clean_log() {
        let home = tempfile::tempdir().unwrap();
        let fs = test_failsafe();
        let mut tracker = PostureTracker::new(PostureThresholds::default());
        let (posture, restricted) = poll_once(home.path(), Some(&fs), &mut tracker, false).await;
        assert_eq!(posture, SecurityPosture::Green);
        assert!(!restricted);
        assert_eq!(fs.get_level(GLOBAL_SCOPE).await, FailsafeLevel::L0Normal);
    }

    #[tokio::test]
    async fn three_criticals_degrade_global_failsafe_to_restricted() {
        let home = tempfile::tempdir().unwrap();
        for _ in 0..3 {
            duduclaw_security::audit::log_injection_detected(
                home.path(),
                "agent-x",
                99,
                &["x".into()],
                true, // blocked ⇒ Critical
            );
        }
        let fs = test_failsafe();
        let mut tracker = PostureTracker::new(PostureThresholds::default());
        let (posture, restricted) = poll_once(home.path(), Some(&fs), &mut tracker, false).await;
        assert_eq!(posture, SecurityPosture::Red);
        assert!(restricted);
        assert_eq!(
            fs.get_level(GLOBAL_SCOPE).await,
            FailsafeLevel::L2Restricted
        );

        // A second poll on the SAME (still-Red) window must not re-fire —
        // posture hasn't changed, so `poll_once` short-circuits before
        // touching failsafe again.
        let (posture2, restricted2) =
            poll_once(home.path(), Some(&fs), &mut tracker, restricted).await;
        assert_eq!(posture2, SecurityPosture::Red);
        assert!(restricted2);
    }

    #[tokio::test]
    async fn a_human_halt_is_never_downgraded_by_a_red_posture() {
        let home = tempfile::tempdir().unwrap();
        for _ in 0..3 {
            duduclaw_security::audit::log_injection_detected(
                home.path(),
                "agent-x",
                99,
                &["x".into()],
                true,
            );
        }
        let fs = test_failsafe();
        fs.force_halt(GLOBAL_SCOPE, "operator: !STOP ALL").await;
        assert_eq!(fs.get_level(GLOBAL_SCOPE).await, FailsafeLevel::L4Halted);

        let mut tracker = PostureTracker::new(PostureThresholds::default());
        let (posture, restricted) = poll_once(home.path(), Some(&fs), &mut tracker, false).await;
        assert_eq!(posture, SecurityPosture::Red);
        assert!(
            !restricted,
            "this loop did not cause the halt, so it must not claim it"
        );
        assert_eq!(
            fs.get_level(GLOBAL_SCOPE).await,
            FailsafeLevel::L4Halted,
            "a human halt must survive a Red posture observation untouched"
        );
    }

    #[tokio::test]
    async fn recovery_to_green_resumes_only_what_this_loop_set() {
        let home = tempfile::tempdir().unwrap();
        let fs = test_failsafe();
        // Simulate: this loop previously restricted due to Red.
        fs.set_level(GLOBAL_SCOPE, FailsafeLevel::L2Restricted, "posture: RED")
            .await;
        let mut tracker = PostureTracker::new(PostureThresholds::default());
        // Manually drive the tracker to Red so the next observation (a
        // clean window) is a genuine Red→Yellow→Green decay, exercising the
        // SAME state machine `poll_once` itself drives via `tracker.observe`.
        tracker.observe(0, 3, None); // Red
        assert_eq!(tracker.current(), SecurityPosture::Red);

        // Poll #1: clean window ⇒ decays Red→Yellow. No failsafe change yet.
        let (p1, r1) = poll_once(home.path(), Some(&fs), &mut tracker, true).await;
        assert_eq!(p1, SecurityPosture::Yellow);
        assert!(r1, "still restricted mid-decay");
        assert_eq!(
            fs.get_level(GLOBAL_SCOPE).await,
            FailsafeLevel::L2Restricted
        );

        // Poll #2: another clean window ⇒ decays Yellow→Green ⇒ resumes.
        let (p2, r2) = poll_once(home.path(), Some(&fs), &mut tracker, r1).await;
        assert_eq!(p2, SecurityPosture::Green);
        assert!(!r2);
        assert_eq!(fs.get_level(GLOBAL_SCOPE).await, FailsafeLevel::L0Normal);
    }

    #[tokio::test]
    async fn no_failsafe_manager_still_tracks_posture() {
        let home = tempfile::tempdir().unwrap();
        for _ in 0..3 {
            duduclaw_security::audit::log_injection_detected(
                home.path(),
                "agent-x",
                99,
                &["x".into()],
                true,
            );
        }
        let mut tracker = PostureTracker::new(PostureThresholds::default());
        let (posture, restricted) = poll_once(home.path(), None, &mut tracker, false).await;
        assert_eq!(
            posture,
            SecurityPosture::Red,
            "posture tracking is independent of failsafe wiring"
        );
        assert!(!restricted, "nothing to restrict without a FailsafeManager");
    }
}
