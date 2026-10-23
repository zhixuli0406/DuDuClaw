use super::*;
use serde_json::json;

fn utc(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn taipei() -> Zone {
    Zone::parse(Some("Asia/Taipei")).unwrap()
}

#[test]
fn weekly_fires_on_the_named_days_in_the_zone() {
    let spec = ScheduleSpec::Weekly {
        days: vec![1, 3],
        hour: 9,
        minute: 0,
    };
    // 2026-10-23 is a Friday; 09:00 Taipei = 01:00 UTC.
    let now = utc("2026-10-23T02:00:00Z");
    let first = spec.next_after(now, taipei()).unwrap();
    assert_eq!(first, utc("2026-10-26T01:00:00Z"), "Monday 09:00 Taipei");
    assert_eq!(
        spec.next_after(first, taipei()).unwrap(),
        utc("2026-10-28T01:00:00Z")
    );
}

#[test]
fn weekdays_skip_the_weekend_and_daily_does_not() {
    let fri_evening = utc("2026-10-23T12:00:00Z");
    let wd = ScheduleSpec::Weekdays {
        hour: 9,
        minute: 30,
    };
    assert_eq!(
        wd.next_after(fri_evening, taipei()).unwrap(),
        utc("2026-10-26T01:30:00Z")
    );
    let daily = ScheduleSpec::Daily {
        hour: 9,
        minute: 30,
    };
    assert_eq!(
        daily.next_after(fri_evening, taipei()).unwrap(),
        utc("2026-10-24T01:30:00Z")
    );
}

#[test]
fn non_admins_get_no_cron_and_an_hourly_floor() {
    let now = utc("2026-10-23T02:00:00Z");
    let cron = ScheduleSpec::Cron {
        expr: "0 9 * * 1".into(),
    };
    assert_eq!(
        cron.validate(false, now, taipei(), 60),
        Err(SpecError::CronAdminOnly)
    );
    assert!(cron.validate(true, now, taipei(), 60).is_ok());
    let every_minute = ScheduleSpec::Cron {
        expr: "* * * * *".into(),
    };
    assert_eq!(
        every_minute.validate(true, now, taipei(), 60),
        Err(SpecError::TooFrequent {
            min_minutes: ADMIN_MIN_INTERVAL_MINUTES
        })
    );
    let hourly = ScheduleSpec::Hourly { minute: 0 };
    assert!(hourly.validate(false, now, taipei(), 60).is_ok());
    assert_eq!(
        hourly.validate(false, now, taipei(), 120),
        Err(SpecError::TooFrequent { min_minutes: 120 })
    );
}

#[test]
fn a_one_shot_must_be_ahead_and_within_a_year() {
    let now = utc("2026-10-23T02:00:00Z");
    let at = |s: &str| ScheduleSpec::Once { at: s.into() };
    assert_eq!(
        at("2026-10-23T01:00:00Z").validate(false, now, Zone::Local, 60),
        Err(SpecError::InThePast)
    );
    assert_eq!(
        at("2026-10-23T02:00:30Z").validate(false, now, Zone::Local, 60),
        Err(SpecError::InThePast)
    );
    assert_eq!(
        at("2028-01-01T00:00:00Z").validate(false, now, Zone::Local, 60),
        Err(SpecError::TooFar)
    );
    assert_eq!(
        at("2026-10-23T03:00:00Z").validate(false, now, Zone::Local, 60),
        Ok(utc("2026-10-23T03:00:00Z"))
    );
    assert_eq!(
        at("2026-10-23T03:00:00Z").next_after(utc("2026-10-23T03:00:00Z"), Zone::Local),
        None
    );
}

#[test]
fn bad_shapes_are_refused() {
    let now = utc("2026-10-23T02:00:00Z");
    for spec in [
        ScheduleSpec::Daily {
            hour: 24,
            minute: 0,
        },
        ScheduleSpec::Hourly { minute: 60 },
        ScheduleSpec::Weekly {
            days: vec![],
            hour: 9,
            minute: 0,
        },
        ScheduleSpec::Weekly {
            days: vec![8],
            hour: 9,
            minute: 0,
        },
        ScheduleSpec::Cron {
            expr: "not a cron".into(),
        },
        ScheduleSpec::Once {
            at: "tomorrow".into(),
        },
    ] {
        assert_eq!(
            spec.validate(true, now, Zone::Local, 60),
            Err(SpecError::Invalid),
            "{spec:?}"
        );
    }
    assert_eq!(Zone::parse(Some("Mars/Base")), Err(SpecError::BadTimezone));
    assert_eq!(Zone::parse(Some("  ")), Ok(Zone::Local));
}

#[test]
fn the_dashboard_shape_is_read_in_the_given_zone() {
    let z = taipei();
    assert_eq!(
        from_json(&json!({"kind": "once", "at": "2026-10-25T14:00"}), z).unwrap(),
        ScheduleSpec::Once {
            at: "2026-10-25T06:00:00+00:00".into()
        }
    );
    assert_eq!(
        from_json(
            &json!({"kind": "once", "at": "2026-10-25T14:00:00+09:00"}),
            z
        )
        .unwrap(),
        ScheduleSpec::Once {
            at: "2026-10-25T05:00:00+00:00".into()
        }
    );
    assert_eq!(
        from_json(
            &json!({"kind": "weekly", "days": [5, 1, 5], "time": "08:15"}),
            z
        )
        .unwrap(),
        ScheduleSpec::Weekly {
            days: vec![1, 5],
            hour: 8,
            minute: 15
        }
    );
    assert_eq!(
        from_json(&json!({"kind": "weekdays", "time": "9:00"}), z).unwrap(),
        ScheduleSpec::Weekdays { hour: 9, minute: 0 }
    );
    assert_eq!(
        from_json(&json!({"kind": "hourly"}), z).unwrap(),
        ScheduleSpec::Hourly { minute: 0 }
    );
    assert!(
        from_json(&json!({"kind": "daily"}), z).is_err(),
        "a daily schedule needs a time"
    );
    assert!(from_json(&json!({"kind": "monthly", "time": "09:00"}), z).is_err());
}

#[test]
fn the_stored_form_round_trips() {
    let spec = ScheduleSpec::Weekly {
        days: vec![1, 3],
        hour: 9,
        minute: 0,
    };
    let text = serde_json::to_string(&spec).unwrap();
    assert_eq!(
        text,
        r#"{"kind":"weekly","days":[1,3],"hour":9,"minute":0}"#
    );
    assert_eq!(serde_json::from_str::<ScheduleSpec>(&text).unwrap(), spec);
}
