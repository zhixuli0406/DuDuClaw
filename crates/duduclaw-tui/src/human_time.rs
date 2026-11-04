//! Human-readable local timestamps at the presentation boundary.
use chrono::{DateTime, FixedOffset, Local, TimeDelta, TimeZone, Utc};
use serde_json::Value;

pub const UNKNOWN: &str = "時間不明";

/// Only RFC3339 is accepted; malformed or hostile input is never echoed.
pub fn format(input: &str) -> String {
    #[cfg(test)]
    if let Some(now) = TEST_CLOCK.with(|clock| *clock.borrow()) { return format_at(input, now); }
    format_at(input, Local::now())
}

fn format_at<Tz: TimeZone>(input: &str, now: DateTime<Tz>) -> String {
    if input.len() > 128 { return UNKNOWN.into(); }
    match DateTime::parse_from_rfc3339(input) {
        Ok(time) => relative(time, now),
        Err(_) => UNKNOWN.into(),
    }
}

fn relative<Tz: TimeZone>(time: DateTime<FixedOffset>, now: DateTime<Tz>) -> String {
    let delta = time.signed_duration_since(&now);
    let future = delta > TimeDelta::zero();
    let elapsed = delta.abs();
    if elapsed < TimeDelta::seconds(60) {
        return if future { "即將" } else { "剛剛" }.into();
    }
    if elapsed < TimeDelta::hours(1) {
        return format!("{} 分鐘{}", elapsed.num_minutes(), if future { "後" } else { "前" });
    }
    let local = time.with_timezone(&now.timezone());
    let pattern = if local.date_naive() == now.date_naive() { "%H:%M" } else { "%m/%d %H:%M" };
    // Naive local time avoids imposing a Display bound on the timezone offset.
    local.naive_local().format(pattern).to_string()
}

/// Typed metadata fields, not arbitrary user content, opt into time formatting.
pub(crate) fn is_time_field(key: &str) -> bool {
    matches!(key, "timestamp" | "ts" | "created_at" | "updated_at" | "started_at"
        | "finished_at" | "completed_at" | "produced_at" | "expires_at" | "deadline_at"
        | "stop_at" | "next_due_at" | "observed_at" | "dispatched_at" | "opened_at"
        | "checked_at" | "generated_at" | "since" | "last_run" | "last_run_at" | "next_run")
}

pub(crate) fn value(value: &Value) -> String {
    match value {
        Value::String(input) => format(input),
        Value::Number(number) => number.as_i64().and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
            .map(|time| format(&time.to_rfc3339())).unwrap_or_else(|| UNKNOWN.into()),
        _ => UNKNOWN.into(),
    }
}

/// Missing last-run metadata means no run has occurred, not an invalid clock.
pub(crate) fn field(key: &str, value: &Value) -> String {
    if matches!(key, "last_run" | "last_run_at")
        && (value.is_null() || value.as_str().is_some_and(|s| s.trim().is_empty())) {
        "尚未執行".into()
    } else { self::value(value) }
}

pub(crate) fn elapsed_minutes(input: &str) -> Option<i64> {
    let start = DateTime::parse_from_rfc3339(input).ok()?;
    #[cfg(test)]
    if let Some(now) = TEST_CLOCK.with(|clock| *clock.borrow()) {
        return Some(now.signed_duration_since(start).num_minutes().max(0));
    }
    Some(Local::now().signed_duration_since(start).num_minutes().max(0))
}

/// Preserve nested request payloads verbatim for review; only top-level metadata
/// is changed on this display-only copy. Identity and decision data are untouched.
pub(crate) fn metadata_json(value: &Value) -> Value {
    let mut displayed = value.clone();
    if let Some(fields) = displayed.as_object_mut() {
        for (key, entry) in fields {
            if is_time_field(key) { *entry = Value::String(self::field(key, entry)); }
        }
    }
    displayed
}

/// Scheduling has a meaningful absence distinct from an invalid timestamp.
pub(crate) fn schedule(input: &crate::sanitize::TerminalText) -> String {
    if input.is(crate::strings::NOT_SCHEDULED) { crate::strings::NOT_SCHEDULED.into() }
    else { format(&input.plain()) }
}

#[cfg(test)]
thread_local! {
    static TEST_CLOCK: std::cell::RefCell<Option<DateTime<FixedOffset>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_test_clock<R>(now: &str, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<DateTime<FixedOffset>>);
    impl Drop for Restore {
        fn drop(&mut self) { TEST_CLOCK.with(|clock| *clock.borrow_mut() = self.0); }
    }
    let previous = TEST_CLOCK.with(|clock| clock.replace(Some(DateTime::parse_from_rfc3339(now).unwrap())));
    let _restore = Restore(previous);
    run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-11-04T12:00:00+08:00").unwrap()
    }

    #[test]
    fn relative_boundaries_are_symmetric_and_subseconds_do_not_round_up() {
        let now = reference();
        for (millis, past, future) in [
            (0, "剛剛", "剛剛"),
            (1, "剛剛", "即將"),
            (59_999, "剛剛", "即將"),
            (60_000, "1 分鐘前", "1 分鐘後"),
            (119_999, "1 分鐘前", "1 分鐘後"),
            (3_599_999, "59 分鐘前", "59 分鐘後"),
            (3_600_000, "11:00", "13:00"),
        ] {
            assert_eq!(format_at(&(now - TimeDelta::milliseconds(millis)).to_rfc3339(), now), past);
            assert_eq!(format_at(&(now + TimeDelta::milliseconds(millis)).to_rfc3339(), now), future);
        }
    }

    #[test]
    fn calendar_and_clock_use_the_local_timezone_including_day_boundaries() {
        let now = reference();
        assert_eq!(format_at("2026-11-03T23:00:00Z", now), "07:00");
        assert_eq!(format_at("2026-11-03T15:59:00Z", now), "11/03 23:59");
        assert_eq!(format_at("2026-11-04T16:00:00Z", now), "11/05 00:00");
        let midnight = DateTime::parse_from_rfc3339("2027-01-01T00:00:10+08:00").unwrap();
        assert_eq!(format_at("2026-12-31T23:59:40+08:00", midnight), "剛剛");
        assert_eq!(format_at("2026-12-31T22:00:00+08:00", midnight), "12/31 22:00");
    }

    #[test]
    fn invalid_and_oversized_values_never_reach_output() {
        for input in ["", "10:00", "revision", "2026-11-04T99:00:00Z", "\x1b]52;c;secret\x07"] {
            assert_eq!(format_at(input, reference()), UNKNOWN);
        }
        assert_eq!(format_at(&"x".repeat(129), reference()), UNKNOWN);
        assert_eq!(value(&Value::Null), UNKNOWN);
        assert_eq!(value(&serde_json::json!(u64::MAX)), UNKNOWN);
    }

    #[test]
    fn local_timezone_uses_the_target_dates_daylight_saving_offset() {
        let zone = chrono_tz::America::New_York;
        let now = zone.with_ymd_and_hms(2026, 11, 4, 12, 0, 0).unwrap();
        assert_eq!(format_at("2026-07-01T12:00:00Z", now), "07/01 08:00");
        assert_eq!(format_at("2026-11-03T12:00:00Z", now), "11/03 07:00");
    }

    #[test]
    fn timestamp_metadata_is_display_only_and_does_not_rewrite_payloads() {
        with_test_clock("2026-11-04T12:00:00+08:00", || {
            let row = serde_json::json!({"created_at":"2026-11-04T03:59:50Z", "payload":{"created_at":"raw"}});
            let output = metadata_json(&row);
            assert_eq!(output["created_at"], "剛剛");
            assert_eq!(output["payload"], row["payload"]);
            assert_eq!(row["created_at"], "2026-11-04T03:59:50Z");
            assert_eq!(value(&serde_json::json!(reference().timestamp())), "剛剛");
            assert_eq!(format("2026-11-03T15:59:00Z"), "11/03 23:59");
        });
    }
}
