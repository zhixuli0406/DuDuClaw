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

/// Normalise a cron expression for the `cron` crate. Two jobs in one pass:
///
/// 1. **Field count** — a 5-field crontab (`m h dom mon dow`) gains a
///    leading `0` seconds field (the crate wants 6 or 7 fields).
/// 2. **Day-of-week convention** — numeric day-of-week values are translated
///    from the classic Unix crontab convention (`0`–`7`, both `0` and `7`
///    meaning Sunday, so `1-5` = Monday–Friday) into the crate's
///    Quartz-flavoured ordinals (`1` = Sunday … `7` = Saturday). Without the
///    translation every numerically-written weekday schedule fires one day
///    early: `* * 1-5` silently meant Sun–Thu — phantom Sunday runs plus
///    silently skipped Fridays (the 2026-08-16 LWM incident).
///
/// Name forms (`MON`, `mon-fri`) are convention-free in the crate and pass
/// through untouched, as do `*`, `?` and `*/step` (stepping from Sunday
/// yields the same weekday set in both conventions). Numeric ranges are
/// expanded to explicit lists so a translated range can never come out
/// reversed (`6-7` = Sat–Sun → `1,7`, never the invalid `7-1`).
///
/// Anything else — wrong field counts, out-of-range numbers, malformed
/// items — passes through unchanged so the crate's own parse error remains
/// the single authority on validity (fail-visible, never silently
/// reinterpreted).
pub fn normalise_cron(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    // Day-of-week position: last field in 5/6-field forms, second-to-last
    // in the 7-field (trailing year) form.
    let (dow_idx, prepend_seconds) = match fields.len() {
        5 => (4, true),
        6 => (5, false),
        7 => (5, false),
        _ => return expr.to_string(),
    };
    let mut out: Vec<String> = fields.iter().map(|s| s.to_string()).collect();
    out[dow_idx] = translate_dow_field(fields[dow_idx]);
    if prepend_seconds {
        out.insert(0, "0".to_string());
    }
    out.join(" ")
}

/// Translate one comma-separated day-of-week field from Unix crontab
/// numbering to the `cron` crate's Quartz numbering. See [`normalise_cron`].
fn translate_dow_field(field: &str) -> String {
    field
        .split(',')
        .map(translate_dow_item)
        .collect::<Vec<_>>()
        .join(",")
}

fn translate_dow_item(item: &str) -> String {
    let (base, step) = match item.split_once('/') {
        Some((b, s)) => (b, Some(s)),
        None => (item, None),
    };
    // `*`/`?` bases are convention-free; `*/step` steps from Sunday in both
    // conventions, so the resulting weekday set is identical either way.
    if base == "*" || base == "?" {
        return item.to_string();
    }
    let translated = if let Some((lo, hi)) = base.split_once('-') {
        match (parse_unix_dow(lo), parse_unix_dow(hi)) {
            (Some(lo), Some(hi)) if lo <= hi => {
                let step: usize = match step {
                    Some(s) => match s.parse() {
                        Ok(n) if n >= 1 => n,
                        _ => return item.to_string(),
                    },
                    None => 1,
                };
                // Expand instead of translating endpoints: `6-7` (Sat–Sun)
                // would otherwise become the reversed range `7-1`.
                let days: std::collections::BTreeSet<u32> =
                    (lo..=hi).step_by(step).map(unix_to_quartz_dow).collect();
                Some(
                    days.iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                )
            }
            _ => None,
        }
    } else {
        match (parse_unix_dow(base), step) {
            // Bare `N/step` is not Vixie-cron grammar and its wrapped
            // translation would be a guess — leave it to the crate's parser.
            (Some(_), Some(_)) => None,
            (Some(n), None) => Some(unix_to_quartz_dow(n).to_string()),
            (None, _) => None,
        }
    };
    translated.unwrap_or_else(|| item.to_string())
}

/// Parse a Unix-crontab day-of-week ordinal (`0`–`7`; `0` and `7` are both
/// Sunday). Names and out-of-range numbers return `None` (left untouched).
fn parse_unix_dow(s: &str) -> Option<u32> {
    let n: u32 = s.parse().ok()?;
    (n <= 7).then_some(n)
}

/// Unix (`0`/`7` = Sunday) → Quartz (`1` = Sunday … `7` = Saturday).
fn unix_to_quartz_dow(n: u32) -> u32 {
    (n % 7) + 1
}

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
        normalise_cron(expr).parse().unwrap()
    }

    #[test]
    fn normalise_prepends_seconds_to_5_field() {
        assert_eq!(normalise_cron("30 8 * * *"), "0 30 8 * * *");
        assert_eq!(normalise_cron("0 9 * * *"), "0 0 9 * * *");
    }

    #[test]
    fn normalise_translates_unix_dow_to_quartz() {
        // The headline case: crontab Mon–Fri, not the crate's Sun–Thu.
        assert_eq!(normalise_cron("45 8 * * 1-5"), "0 45 8 * * 2,3,4,5,6");
        // Both Unix spellings of Sunday.
        assert_eq!(normalise_cron("0 9 * * 0"), "0 0 9 * * 1");
        assert_eq!(normalise_cron("0 9 * * 7"), "0 0 9 * * 1");
        // Friday 17:00 — the built-in weekly-report template shape.
        assert_eq!(normalise_cron("0 17 * * 5"), "0 0 17 * * 6");
        // Sat–Sun expands to a list instead of a reversed range.
        assert_eq!(normalise_cron("0 9 * * 6-7"), "0 0 9 * * 1,7");
        assert_eq!(normalise_cron("0 9 * * 5,6"), "0 0 9 * * 6,7");
        // Range with step keeps crontab stepping semantics (1,3,5 → Mon/Wed/Fri).
        assert_eq!(normalise_cron("0 9 * * 1-5/2"), "0 0 9 * * 2,4,6");
    }

    #[test]
    fn normalise_translates_dow_in_6_and_7_field_forms() {
        assert_eq!(normalise_cron("0 45 8 * * 1-5"), "0 45 8 * * 2,3,4,5,6");
        assert_eq!(
            normalise_cron("0 45 8 * * 1-5 2026"),
            "0 45 8 * * 2,3,4,5,6 2026"
        );
    }

    #[test]
    fn normalise_leaves_convention_free_forms_alone() {
        // Names are convention-free in the crate.
        assert_eq!(normalise_cron("0 9 * * MON-FRI"), "0 0 9 * * MON-FRI");
        // `*` and `*/step` yield the same weekday set in both conventions.
        assert_eq!(normalise_cron("0 9 * * *"), "0 0 9 * * *");
        assert_eq!(normalise_cron("0 9 * * */2"), "0 0 9 * * */2");
        assert_eq!(normalise_cron("0 9 * * ?"), "0 0 9 * * ?");
    }

    #[test]
    fn normalise_passes_garbage_through_for_the_crate_to_reject() {
        // Out-of-range / malformed day-of-week stays verbatim so the crate's
        // parse error remains the single validity authority.
        assert_eq!(normalise_cron("0 9 * * 8"), "0 0 9 * * 8");
        assert_eq!(normalise_cron("0 9 * * 5-1"), "0 0 9 * * 5-1");
        assert_eq!(normalise_cron("not a cron"), "not a cron");
        assert_eq!(normalise_cron("0 9 * * 3/2"), "0 0 9 * * 3/2");
    }

    /// The oracle is the real scheduler: `1-5` written in crontab convention
    /// must fire Mon–Fri — Friday included, Sunday excluded. This is the
    /// exact 2026-08-16 LWM failure shape (Sunday phantom run, Friday skip).
    #[test]
    fn crontab_mon_fri_fires_mon_fri_in_the_crate() {
        use chrono::Datelike;
        let s = sched("45 8 * * 1-5");
        for (day, want) in [
            (14, 1), // 2026-08-14 Fri
            (15, 0), // Sat
            (16, 0), // Sun
            (17, 1), // Mon
        ] {
            let start = Utc.with_ymd_and_hms(2026, 8, day, 0, 0, 0).unwrap();
            let end = Utc.with_ymd_and_hms(2026, 8, day, 23, 59, 59).unwrap();
            let fired = s.after(&start).take_while(|t| *t <= end).count();
            assert_eq!(
                fired,
                want,
                "2026-08-{day:02} ({:?})",
                start.date_naive().weekday()
            );
        }
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
