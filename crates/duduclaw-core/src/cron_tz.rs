//! Timezone-aware cron evaluation shared by the heartbeat scheduler and the
//! cron task scheduler.
//!
//! Cron expressions in DuDuClaw default to UTC (the pre-v1.8.23 behaviour);
//! callers that want wall-clock semantics pass an IANA timezone name like
//! `"Asia/Taipei"`. Invalid names fall back to UTC with a warn-level log so
//! a typo can never crash the scheduler — the cron just behaves exactly like
//! a pre-v1.8.23 UTC cron until the user fixes the name.
//!
//! ## Why the fall-back is UTC instead of system-local
//!
//! DuDuClaw runs inside Docker containers that default to UTC, on launchd
//! processes that sometimes have `TZ` unset, and on headless servers where
//! "system local" is rarely what the user actually wants. UTC is the only
//! timezone with identical behaviour across every deployment surface, so
//! we require the user to name their intended timezone explicitly.
//!
//! ## Semantics of `should_fire_in_tz`
//!
//! Given a cron `Schedule`, the last-run instant (in UTC), the current
//! instant (in UTC), and an optional timezone name:
//!
//! - Convert both instants to the target timezone.
//! - Anchor the schedule iterator at `last_run` (or `now - 1h` when the
//!   task has never run — a wide-enough back-scan to catch missed ticks
//!   after a restart without re-firing stale history).
//! - Fire when the next scheduled instant is ≤ `now_in_tz`.
//!
//! Because a single physical instant maps to the same UTC offset regardless
//! of the viewing timezone, comparisons remain correct across DST
//! transitions — the `cron::Schedule` implementation handles fold / gap
//! hours conservatively.

use chrono::{DateTime, Duration, Utc};
use cron::Schedule;

/// Parse an IANA timezone name. Empty strings and unrecognised names both
/// return `None`; callers then fall back to UTC evaluation. A `None`
/// parse is silent — callers log once at config-load time so the scheduler
/// hot loop doesn't spam identical warnings every tick.
pub fn parse_timezone(name: &str) -> Option<chrono_tz::Tz> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<chrono_tz::Tz>().ok()
}

/// True iff the cron schedule should fire by `now_utc`, evaluated in the
/// named timezone (or UTC when `timezone` is `None` / invalid).
///
/// `last_run_utc` is the last confirmed firing of this task, or `None`
/// when the task has never run (e.g. freshly loaded from DB). When `None`,
/// the anchor is `now_utc - 1h` so we pick up at most one catch-up fire
/// after a restart.
pub fn should_fire_in_tz(
    schedule: &Schedule,
    last_run_utc: Option<DateTime<Utc>>,
    now_utc: DateTime<Utc>,
    timezone: Option<chrono_tz::Tz>,
) -> bool {
    match timezone {
        Some(tz) => {
            let now_tz = now_utc.with_timezone(&tz);
            let anchor = last_run_utc
                .map(|t| t.with_timezone(&tz))
                .unwrap_or_else(|| now_tz - Duration::hours(1));
            schedule
                .after(&anchor)
                .next()
                .map(|next| next <= now_tz)
                .unwrap_or(false)
        }
        None => {
            let anchor =
                last_run_utc.unwrap_or_else(|| now_utc - Duration::hours(1));
            schedule
                .after(&anchor)
                .next()
                .map(|next| next <= now_utc)
                .unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sched(expr: &str) -> Schedule {
        let fields = expr.split_whitespace().count();
        let normalised = if fields == 5 {
            format!("0 {expr}")
        } else {
            expr.to_string()
        };
        normalised.parse().unwrap()
    }

    #[test]
    fn parse_timezone_accepts_iana() {
        assert!(parse_timezone("Asia/Taipei").is_some());
        assert!(parse_timezone("America/New_York").is_some());
        assert!(parse_timezone("UTC").is_some());
    }

    #[test]
    fn parse_timezone_rejects_garbage() {
        assert!(parse_timezone("").is_none());
        assert!(parse_timezone("   ").is_none());
        assert!(parse_timezone("Mars/Olympus").is_none());
        assert!(parse_timezone("UTC+8").is_none());
    }

    #[test]
    fn parse_timezone_trims_whitespace() {
        assert!(parse_timezone("  Asia/Taipei  ").is_some());
    }

    #[test]
    fn fires_in_taipei_at_local_0900() {
        // Cron "0 9 * * *" in Asia/Taipei means 09:00 Taipei = 01:00 UTC.
        let schedule = sched("0 9 * * *");
        let tz = parse_timezone("Asia/Taipei").unwrap();

        // 00:59 UTC on 2026-04-22 = 08:59 Taipei → should NOT fire yet.
        let before = Utc.with_ymd_and_hms(2026, 4, 22, 0, 59, 0).unwrap();
        assert!(!should_fire_in_tz(&schedule, None, before, Some(tz)));

        // 01:00 UTC = 09:00 Taipei → should fire.
        let at = Utc.with_ymd_and_hms(2026, 4, 22, 1, 0, 0).unwrap();
        assert!(should_fire_in_tz(&schedule, None, at, Some(tz)));

        // After firing at 01:00 UTC, should NOT fire again at 02:00 UTC
        // (last_run is anchored at 01:00 UTC).
        let last = at;
        let later = Utc.with_ymd_and_hms(2026, 4, 22, 2, 0, 0).unwrap();
        assert!(!should_fire_in_tz(
            &schedule, Some(last), later, Some(tz)
        ));

        // Next day 01:00 UTC → should fire again.
        let next_day = Utc.with_ymd_and_hms(2026, 4, 23, 1, 0, 0).unwrap();
        assert!(should_fire_in_tz(
            &schedule, Some(last), next_day, Some(tz)
        ));
    }

    #[test]
    fn utc_fallback_preserves_legacy_behaviour() {
        // "0 9 * * *" with no timezone fires at 09:00 UTC, same as pre-v1.8.23.
        let schedule = sched("0 9 * * *");

        // 08:59 UTC → no fire.
        let before = Utc.with_ymd_and_hms(2026, 4, 22, 8, 59, 0).unwrap();
        assert!(!should_fire_in_tz(&schedule, None, before, None));

        // 09:00 UTC → fire.
        let at = Utc.with_ymd_and_hms(2026, 4, 22, 9, 0, 0).unwrap();
        assert!(should_fire_in_tz(&schedule, None, at, None));
    }

    #[test]
    fn invalid_timezone_name_falls_back_to_utc() {
        // The helper uses whatever timezone the caller passed. An invalid
        // name becomes `None` via parse_timezone, which is UTC semantics.
        let schedule = sched("0 9 * * *");
        let tz = parse_timezone("Mars/Olympus"); // → None
        assert!(tz.is_none());

        let at = Utc.with_ymd_and_hms(2026, 4, 22, 9, 0, 0).unwrap();
        assert!(should_fire_in_tz(&schedule, None, at, tz));
    }

    #[test]
    fn every_five_minutes_independent_of_timezone() {
        // "*/5 * * * *" — every 5 minutes on the minute, same in every TZ
        // because no hour/day anchor is involved.
        let schedule = sched("*/5 * * * *");
        let taipei = parse_timezone("Asia/Taipei");
        let utc = None::<chrono_tz::Tz>;

        let t = Utc.with_ymd_and_hms(2026, 4, 22, 1, 5, 0).unwrap();
        assert!(should_fire_in_tz(&schedule, None, t, taipei));
        assert!(should_fire_in_tz(&schedule, None, t, utc));
    }

    #[test]
    fn new_york_east_coast_morning() {
        // "0 8 * * *" in America/New_York = 12:00 UTC (during EST, UTC-5)
        // or 13:00 UTC (during EDT, UTC-4). 2026-04-22 is in EDT.
        let schedule = sched("0 8 * * *");
        let tz = parse_timezone("America/New_York").unwrap();

        // 11:59 UTC on 2026-04-22 = 07:59 EDT → no fire.
        let before = Utc.with_ymd_and_hms(2026, 4, 22, 11, 59, 0).unwrap();
        assert!(!should_fire_in_tz(&schedule, None, before, Some(tz)));

        // 12:00 UTC = 08:00 EDT → fire.
        let at = Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap();
        assert!(should_fire_in_tz(&schedule, None, at, Some(tz)));
    }
}
