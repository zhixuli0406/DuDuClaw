// Retry backoff + log denoise for the Notifications feed's gateway poll —
// WP-A4-4 (2026-08-22), the fix for the appliance VM's live journal storm.
//
// ── What actually happened on the值班機 ─────────────────────────────────
// `journalctl -u duduclaw-kiosk` recorded, six-plus times within the SAME
// second and for many consecutive seconds:
//
//     [notifications] approvals RPC failed: Unreachable("HTTP error: 429 Too
//     Many Requests")
//
// Two independent defects stacked:
//
//   1. `NotificationsFeed::apply_list_err`/`apply_session_err` cleared
//      `busy` and flipped `status` to `Offline` but NEVER advanced
//      `last_refreshed_at` — so `is_stale()` stayed permanently `true` after
//      the first failure, and every single stale check that landed while no
//      fetch happened to be in flight immediately fired another one. There
//      was no delay between attempt N and attempt N+1 beyond however long
//      the failing call itself took, which against a gateway answering 429
//      at the WS upgrade is single-digit milliseconds.
//   2. The stale-check TIMERS were armed once per render pass with no
//      single-arm guard (see `overlay::notifications::schedule_stale_check`
//      and `lockscreen::render`'s own two timers) — so the pending-timer
//      count grew with every repaint and each of those timers, once fired,
//      hit defect 1. That amplification is fixed at those call sites; this
//      module fixes defect 1.
//
// Both fixes are needed: the arm guard alone would still leave a
// failure-retry cadence with no spacing between the retries a single armed
// timer produces over time, and this module alone would still leave an
// unbounded pile of timers burning CPU.
//
// ── Design ──────────────────────────────────────────────────────────────
// Deliberately pure `&mut self`/`Instant`-in mutation with no gpui types and
// no wall-clock reads of its own — same "testable without a live window"
// discipline `notifications_feed`'s own header comment establishes, extended
// one step further (the caller passes `now`, so every delay assertion below
// is exact rather than sleep-and-hope).
//
// 429 gets a materially longer base delay than a generic failure because the
// two mean opposite things: a generic failure ("gateway is down") wants a
// reasonably prompt retry so the panel recovers quickly once it comes back,
// while a 429 means the client itself is the problem — retrying promptly is
// what caused it. The gateway's own limiter is a 60-request-per-minute token
// bucket (`OpType::HttpRequest`), so a first 429 backoff of 15s is roughly
// "wait for a quarter of the bucket to refill", not a number picked for
// feel.

use std::time::{Duration, Instant};

use crate::gateway_client::{GatewayError, RpcError, SessionError};

/// First retry delay after a plain (non-rate-limited) failure.
pub(crate) const BASE_DELAY: Duration = Duration::from_secs(2);

/// First retry delay after an HTTP 429 — see this file's header comment for
/// why this is much longer than [`BASE_DELAY`] rather than the same number.
pub(crate) const RATE_LIMITED_BASE_DELAY: Duration = Duration::from_secs(15);

/// Hard ceiling on the exponential growth, jitter included. Task brief's own
/// number ("上限要有天花板（例如 60s）").
pub(crate) const MAX_DELAY: Duration = Duration::from_secs(60);

/// Fraction of the computed delay that jitter may ADD (never subtract — a
/// negative jitter could undercut [`RATE_LIMITED_BASE_DELAY`] and start
/// nudging back toward the very behaviour this module exists to stop). Same
/// "+ up to 25%, reconnect backoff" shape `duduclaw-gateway`'s own
/// `tick_source_ws` reconnect loop already uses, so this crate isn't
/// inventing a second jitter convention.
const JITTER_FRACTION: f64 = 0.25;

/// `1.0 + JITTER_FRACTION` — the most a jittered delay can exceed its
/// un-jittered curve value. Test-only, and deliberately shared rather than
/// re-derived at each assertion: it lets `notifications_feed`'s tests state
/// an EXACT upper bound instead of an arbitrary slack (see that module's own
/// test-section comment on why no test there is allowed to depend on the
/// test runner's scheduling).
#[cfg(test)]
pub(crate) const JITTER_CEILING_MULTIPLIER: f64 = 1.0 + JITTER_FRACTION;

/// How many consecutive failures the exponent is allowed to grow over
/// before it stops mattering — `MAX_DELAY` clamps the result anyway, this
/// just keeps the intermediate `2^n` from overflowing on a machine that has
/// been offline for a very long time.
const MAX_EXPONENT: u32 = 16;

/// Why a fetch failed, reduced to the only distinction the retry policy
/// actually acts on. Deliberately NOT a mirror of `GatewayError`'s full
/// shape: the panel already collapses every failure to one Offline banner
/// (see `notifications_feed::FeedStatus::Offline`), and the retry policy
/// only needs "was this us calling too fast, or something else".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    /// HTTP 429 from the gateway's token bucket, on either the session
    /// bootstrap (`SessionError::Http(429)`) or the WS upgrade
    /// (`RpcError::RateLimited`).
    RateLimited,
    /// Everything else — gateway down, auth rejected, malformed response,
    /// timeout. All one bucket on purpose (see this enum's own doc comment).
    Other,
}

impl FailureKind {
    /// Typed classification — matches on the error VARIANT and, for the
    /// session path, on its typed numeric status. No string matching
    /// anywhere (this crate's coding convention 2: no unanchored `contains`
    /// for a routing decision).
    pub(crate) fn classify(err: &GatewayError) -> Self {
        match err {
            GatewayError::Rpc(RpcError::RateLimited) => FailureKind::RateLimited,
            GatewayError::Session(SessionError::Http(429)) => FailureKind::RateLimited,
            _ => FailureKind::Other,
        }
    }
}

/// What the caller should do with its stderr diagnostic for this failure —
/// see `RefreshBackoff::record_failure`'s own doc comment for the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogVerdict {
    /// Print one line. `suppressed` is how many failures were swallowed
    /// since the last printed line, so the printed line can say so instead
    /// of quietly lying about the real failure count.
    Emit { suppressed: u32 },
    /// Say nothing — this is the same failure as the last one that was
    /// already reported.
    Suppress,
}

/// Per-feed retry state. One instance lives inside `NotificationsFeed`; the
/// lockscreen and the Notifications panel share it because they share that
/// one feed (see `notifications_feed`'s own header comment on why there is
/// exactly one model).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct RefreshBackoff {
    consecutive_failures: u32,
    /// `None` = no backoff window is open, attempt whenever the normal
    /// staleness cadence says so.
    retry_not_before: Option<Instant>,
    last_kind: Option<FailureKind>,
    /// Failures swallowed since the last `Emit` — folded into the next
    /// `Emit`'s own count so nothing is silently lost.
    suppressed_since_log: u32,
}

impl RefreshBackoff {
    /// `true` while a backoff window is still open — the caller must NOT
    /// attempt a fetch. Fails toward "attempt" (returns `false`) when no
    /// window is set, so a bug here can never wedge the feed permanently
    /// offline.
    pub(crate) fn is_open(&self, now: Instant) -> bool {
        match self.retry_not_before {
            None => false,
            Some(t) => now < t,
        }
    }

    /// Remaining backoff, or `None` when the window is already closed.
    pub(crate) fn remaining(&self, now: Instant) -> Option<Duration> {
        match self.retry_not_before {
            Some(t) if now < t => Some(t - now),
            _ => None,
        }
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// A fetch succeeded — clears the whole backoff. Returns `Some(n)` when
    /// this success ENDED a failure streak of `n`, so the caller can print
    /// exactly one recovery line (and nothing at all in the overwhelmingly
    /// common "it was already fine" case).
    pub(crate) fn record_success(&mut self) -> Option<u32> {
        let recovered_from = self.consecutive_failures;
        self.consecutive_failures = 0;
        self.retry_not_before = None;
        self.last_kind = None;
        self.suppressed_since_log = 0;
        (recovered_from > 0).then_some(recovered_from)
    }

    /// A fetch failed — grows the backoff window and decides whether this
    /// failure is worth a log line.
    ///
    /// Log policy (the "連續同類失敗不得每次都 warn 一行" requirement): a
    /// line is emitted for the FIRST failure of a streak, whenever the
    /// failure KIND changes mid-streak (offline -> rate-limited is real
    /// news), and thereafter only on power-of-two failure counts (2, 4, 8,
    /// 16 …) so a machine that stays offline overnight produces a handful of
    /// lines rather than one per retry. Every suppressed failure is counted
    /// and reported by the next emitted line — suppression here means "don't
    /// repeat yourself", never "hide it".
    ///
    /// `jitter` must be in `[0.0, 1.0]`; it is the caller's random draw,
    /// injected rather than sampled here so every test below can assert an
    /// exact delay. Out-of-range values are clamped rather than trusted.
    pub(crate) fn record_failure(&mut self, kind: FailureKind, now: Instant, jitter: f64) -> LogVerdict {
        let kind_changed = self.last_kind.is_some_and(|prev| prev != kind);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_kind = Some(kind);
        self.retry_not_before = Some(now + delay_for(kind, self.consecutive_failures, jitter));

        let first_of_streak = self.consecutive_failures == 1;
        if first_of_streak || kind_changed || self.consecutive_failures.is_power_of_two() {
            let suppressed = self.suppressed_since_log;
            self.suppressed_since_log = 0;
            LogVerdict::Emit { suppressed }
        } else {
            self.suppressed_since_log = self.suppressed_since_log.saturating_add(1);
            LogVerdict::Suppress
        }
    }
}

/// Pure delay curve: `base * 2^(failures-1)`, capped at [`MAX_DELAY`], then
/// grown by up to [`JITTER_FRACTION`] and capped again (so the ceiling is a
/// real ceiling, jitter included — a jittered value must never exceed the
/// number this module advertises as its maximum).
pub(crate) fn delay_for(kind: FailureKind, consecutive_failures: u32, jitter: f64) -> Duration {
    let base = match kind {
        FailureKind::RateLimited => RATE_LIMITED_BASE_DELAY,
        FailureKind::Other => BASE_DELAY,
    };
    let exponent = consecutive_failures.saturating_sub(1).min(MAX_EXPONENT);
    let scaled = base.saturating_mul(1u32 << exponent);
    let capped = scaled.min(MAX_DELAY);
    let jitter = jitter.clamp(0.0, 1.0);
    let jittered = capped.mul_f64(1.0 + JITTER_FRACTION * jitter);
    jittered.min(MAX_DELAY)
}

/// The production jitter draw. No `rand` dependency is added for this (the
/// crate has none today, and one nanosecond-resolution clock read is
/// entirely adequate for spreading retries — this is a thundering-herd
/// smoother, not a security primitive; the honest limitation is that it is
/// NOT a uniform random draw and must never be used as one).
pub(crate) fn sample_jitter() -> f64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero jitter everywhere below unless a test is specifically about
    /// jitter — the delay curve itself is what most assertions are locking.
    const NO_JITTER: f64 = 0.0;

    #[test]
    fn a_fresh_backoff_is_closed_and_lets_the_first_attempt_through() {
        let b = RefreshBackoff::default();
        let now = Instant::now();
        assert!(!b.is_open(now));
        assert_eq!(b.remaining(now), None);
        assert_eq!(b.consecutive_failures(), 0);
    }

    #[test]
    fn a_429_backs_off_much_longer_than_a_generic_failure_on_the_very_first_try() {
        let rate_limited = delay_for(FailureKind::RateLimited, 1, NO_JITTER);
        let other = delay_for(FailureKind::Other, 1, NO_JITTER);
        assert_eq!(rate_limited, RATE_LIMITED_BASE_DELAY);
        assert_eq!(other, BASE_DELAY);
        assert!(rate_limited > other, "429 means 'you are calling too fast' — it must back off harder than a generic failure");
    }

    #[test]
    fn consecutive_failures_double_the_delay_until_the_ceiling() {
        assert_eq!(delay_for(FailureKind::Other, 1, NO_JITTER), Duration::from_secs(2));
        assert_eq!(delay_for(FailureKind::Other, 2, NO_JITTER), Duration::from_secs(4));
        assert_eq!(delay_for(FailureKind::Other, 3, NO_JITTER), Duration::from_secs(8));
        assert_eq!(delay_for(FailureKind::Other, 4, NO_JITTER), Duration::from_secs(16));
        assert_eq!(delay_for(FailureKind::Other, 5, NO_JITTER), Duration::from_secs(32));
        // 64s would exceed the ceiling.
        assert_eq!(delay_for(FailureKind::Other, 6, NO_JITTER), MAX_DELAY);
    }

    #[test]
    fn the_ceiling_holds_for_absurd_failure_counts_without_overflowing() {
        for n in [7u32, 32, 1_000, u32::MAX] {
            assert_eq!(delay_for(FailureKind::Other, n, NO_JITTER), MAX_DELAY, "failure #{n} must clamp, not overflow");
            assert_eq!(delay_for(FailureKind::RateLimited, n, 1.0), MAX_DELAY);
        }
    }

    #[test]
    fn jitter_only_ever_adds_and_never_pushes_past_the_ceiling() {
        let plain = delay_for(FailureKind::Other, 1, NO_JITTER);
        let jittered = delay_for(FailureKind::Other, 1, 1.0);
        assert!(jittered > plain, "jitter must actually spread retries apart");
        assert_eq!(jittered, BASE_DELAY.mul_f64(1.25));
        // At the ceiling, jitter must not create a delay above MAX_DELAY.
        assert_eq!(delay_for(FailureKind::Other, 20, 1.0), MAX_DELAY);
    }

    #[test]
    fn an_out_of_range_jitter_draw_is_clamped_not_trusted() {
        assert_eq!(delay_for(FailureKind::Other, 1, -5.0), BASE_DELAY);
        assert_eq!(delay_for(FailureKind::Other, 1, 42.0), BASE_DELAY.mul_f64(1.25));
    }

    #[test]
    fn recording_a_failure_opens_a_window_that_blocks_the_next_attempt() {
        let mut b = RefreshBackoff::default();
        let t0 = Instant::now();
        b.record_failure(FailureKind::Other, t0, NO_JITTER);
        assert!(b.is_open(t0), "an attempt at the very instant of failure must be refused");
        assert!(b.is_open(t0 + Duration::from_millis(1999)));
        assert!(!b.is_open(t0 + BASE_DELAY), "the window must actually close");
        assert_eq!(b.remaining(t0), Some(BASE_DELAY));
    }

    #[test]
    fn a_429_streak_grows_the_window_and_stops_at_the_ceiling() {
        let mut b = RefreshBackoff::default();
        let t0 = Instant::now();
        b.record_failure(FailureKind::RateLimited, t0, NO_JITTER);
        assert_eq!(b.remaining(t0), Some(RATE_LIMITED_BASE_DELAY));
        b.record_failure(FailureKind::RateLimited, t0, NO_JITTER);
        assert_eq!(b.remaining(t0), Some(Duration::from_secs(30)));
        b.record_failure(FailureKind::RateLimited, t0, NO_JITTER);
        assert_eq!(b.remaining(t0), Some(MAX_DELAY), "60s ceiling");
        b.record_failure(FailureKind::RateLimited, t0, NO_JITTER);
        assert_eq!(b.remaining(t0), Some(MAX_DELAY), "and it stays at the ceiling, never beyond");
        assert_eq!(b.consecutive_failures(), 4);
    }

    #[test]
    fn success_resets_the_backoff_completely() {
        let mut b = RefreshBackoff::default();
        let t0 = Instant::now();
        b.record_failure(FailureKind::RateLimited, t0, NO_JITTER);
        b.record_failure(FailureKind::RateLimited, t0, NO_JITTER);
        assert!(b.is_open(t0));

        assert_eq!(b.record_success(), Some(2), "a recovery must report the streak it ended");
        assert!(!b.is_open(t0), "after a success the very next staleness check must be allowed through");
        assert_eq!(b.consecutive_failures(), 0);
        assert_eq!(b.remaining(t0), None);

        // And the next failure starts the curve over from the base, not from
        // wherever the previous streak left off.
        b.record_failure(FailureKind::RateLimited, t0, NO_JITTER);
        assert_eq!(b.remaining(t0), Some(RATE_LIMITED_BASE_DELAY));
    }

    #[test]
    fn a_success_with_no_prior_failure_reports_nothing_to_log() {
        let mut b = RefreshBackoff::default();
        assert_eq!(b.record_success(), None, "the quiet path must stay completely silent");
    }

    #[test]
    fn the_first_failure_of_a_streak_is_always_logged() {
        let mut b = RefreshBackoff::default();
        let t0 = Instant::now();
        assert_eq!(b.record_failure(FailureKind::Other, t0, NO_JITTER), LogVerdict::Emit { suppressed: 0 });
    }

    #[test]
    fn a_long_identical_streak_logs_on_powers_of_two_and_suppresses_the_rest() {
        let mut b = RefreshBackoff::default();
        let t0 = Instant::now();
        let mut emitted = Vec::new();
        for n in 1..=16u32 {
            if let LogVerdict::Emit { suppressed } = b.record_failure(FailureKind::Other, t0, NO_JITTER) {
                emitted.push((n, suppressed));
            }
        }
        assert_eq!(emitted.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![1, 2, 4, 8, 16]);
        // Every suppressed failure is accounted for by the NEXT emitted
        // line — nothing is silently dropped (5.誠實回報: suppression is
        // "don't repeat", not "hide").
        let total_suppressed: u32 = emitted.iter().map(|(_, s)| *s).sum();
        assert_eq!(total_suppressed + emitted.len() as u32, 16);
    }

    #[test]
    fn a_kind_change_mid_streak_is_always_worth_a_line() {
        let mut b = RefreshBackoff::default();
        let t0 = Instant::now();
        b.record_failure(FailureKind::Other, t0, NO_JITTER); // #1 Emit (first of streak)
        b.record_failure(FailureKind::Other, t0, NO_JITTER); // #2 Emit (power of two)
        assert_eq!(b.record_failure(FailureKind::Other, t0, NO_JITTER), LogVerdict::Suppress); // #3
        b.record_failure(FailureKind::Other, t0, NO_JITTER); // #4 Emit (power of two), carries suppressed:1
        // #5 is neither the first of the streak nor a power of two, so
        // WITHOUT the kind-change rule it would be suppressed.
        assert_eq!(
            b.record_failure(FailureKind::RateLimited, t0, NO_JITTER),
            LogVerdict::Emit { suppressed: 0 },
            "offline -> rate-limited is real news, not a repeat"
        );
        // And the switch really did change the retry policy, not just the log.
        assert_eq!(b.remaining(t0), Some(MAX_DELAY.min(RATE_LIMITED_BASE_DELAY.saturating_mul(1 << 4))));
    }

    #[test]
    fn classify_maps_both_429_paths_and_nothing_else() {
        assert_eq!(FailureKind::classify(&GatewayError::Rpc(RpcError::RateLimited)), FailureKind::RateLimited);
        assert_eq!(FailureKind::classify(&GatewayError::Session(SessionError::Http(429))), FailureKind::RateLimited);

        assert_eq!(FailureKind::classify(&GatewayError::Session(SessionError::Http(503))), FailureKind::Other);
        assert_eq!(FailureKind::classify(&GatewayError::Session(SessionError::Refused)), FailureKind::Other);
        assert_eq!(FailureKind::classify(&GatewayError::Session(SessionError::NonLoopback)), FailureKind::Other);
        assert_eq!(FailureKind::classify(&GatewayError::Rpc(RpcError::Timeout)), FailureKind::Other);
        assert_eq!(FailureKind::classify(&GatewayError::Rpc(RpcError::AuthRejected)), FailureKind::Other);
        // The pre-fix shape of the very failure this whole module exists for
        // — a 429 that reached the UI as an untyped `Unreachable` string —
        // must NOT be pattern-matched back out of the text.
        assert_eq!(
            FailureKind::classify(&GatewayError::Rpc(RpcError::Unreachable("HTTP error: 429 Too Many Requests".to_string()))),
            FailureKind::Other
        );
    }

    #[test]
    fn sample_jitter_stays_inside_the_range_delay_for_expects() {
        for _ in 0..64 {
            let j = sample_jitter();
            assert!((0.0..1.0).contains(&j), "jitter draw {j} escaped [0,1)");
        }
    }
}
