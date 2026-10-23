use super::*;

// 2026-10-23 10:00 Taipei (a Friday).
fn now() -> DateTime<Utc> {
    "2026-10-23T02:00:00Z".parse().unwrap()
}

fn tp() -> Zone {
    Zone::parse(Some("Asia/Taipei")).unwrap()
}

fn p(s: &str) -> ScheduleSpec {
    parse_natural(s, now(), tp()).unwrap_or_else(|e| panic!("{s}: {e:?}"))
}

fn once_at(s: &str) -> ScheduleSpec {
    ScheduleSpec::Once { at: s.into() }
}

#[test]
fn weekly_forms() {
    let mon9 = ScheduleSpec::Weekly {
        days: vec![1],
        hour: 9,
        minute: 0,
    };
    assert_eq!(p("每週一 9:00"), mon9);
    assert_eq!(p("每周一 9:00"), mon9);
    assert_eq!(p("每星期一 早上9點"), mon9);
    assert_eq!(p("每個禮拜一 09:00"), mon9);
    assert_eq!(p("每週一"), mon9, "no time means 09:00");
    assert_eq!(
        p("每週一、三、五 下午2點半"),
        ScheduleSpec::Weekly {
            days: vec![1, 3, 5],
            hour: 14,
            minute: 30
        }
    );
    assert_eq!(
        p("每週三週一 10:00"),
        ScheduleSpec::Weekly {
            days: vec![1, 3],
            hour: 10,
            minute: 0
        }
    );
    assert_eq!(
        p("每週日 晚上8點"),
        ScheduleSpec::Weekly {
            days: vec![7],
            hour: 20,
            minute: 0
        }
    );
    assert_eq!(p("every monday 9:00"), mon9);
    assert_eq!(
        p("weekly mon,wed 9:00"),
        ScheduleSpec::Weekly {
            days: vec![1, 3],
            hour: 9,
            minute: 0
        }
    );
}

#[test]
fn daily_weekdays_and_hourly_forms() {
    assert_eq!(p("每天 9:00"), ScheduleSpec::Daily { hour: 9, minute: 0 });
    assert_eq!(
        p("每日早上8點15分"),
        ScheduleSpec::Daily {
            hour: 8,
            minute: 15
        }
    );
    assert_eq!(
        p("daily 18:30"),
        ScheduleSpec::Daily {
            hour: 18,
            minute: 30
        }
    );
    let wd = ScheduleSpec::Weekdays { hour: 9, minute: 0 };
    assert_eq!(p("平日 9:00"), wd);
    assert_eq!(p("工作日 9點"), wd);
    assert_eq!(p("週一到週五 9:00"), wd);
    assert_eq!(p("每週一至五 9:00"), wd);
    assert_eq!(p("weekdays 09:00"), wd);
    assert_eq!(p("每小時"), ScheduleSpec::Hourly { minute: 0 });
    assert_eq!(p("每小時30分"), ScheduleSpec::Hourly { minute: 30 });
    assert_eq!(p("hourly"), ScheduleSpec::Hourly { minute: 0 });
}

#[test]
fn one_shot_forms() {
    assert_eq!(p("明天 15:00"), once_at("2026-10-24T07:00:00+00:00"));
    assert_eq!(p("明天下午3點"), once_at("2026-10-24T07:00:00+00:00"));
    assert_eq!(p("後天 上午9點"), once_at("2026-10-25T01:00:00+00:00"));
    assert_eq!(p("今天 17:30"), once_at("2026-10-23T09:30:00+00:00"));
    assert_eq!(p("tomorrow 15:00"), once_at("2026-10-24T07:00:00+00:00"));
    assert_eq!(p("10/25 14:00"), once_at("2026-10-25T06:00:00+00:00"));
    assert_eq!(p("10月25日 下午2點"), once_at("2026-10-25T06:00:00+00:00"));
    assert_eq!(p("2026/11/02 09:00"), once_at("2026-11-02T01:00:00+00:00"));
    assert_eq!(p("2026年11月2號 9點"), once_at("2026-11-02T01:00:00+00:00"));
    assert_eq!(
        p("1/5 9:00"),
        once_at("2027-01-05T01:00:00+00:00"),
        "a passed date means next year"
    );
    assert_eq!(p("2026-10-25 14:00"), once_at("2026-10-25T06:00:00+00:00"));
    assert_eq!(
        p("2026-10-25T14:00:00+09:00"),
        once_at("2026-10-25T05:00:00+00:00")
    );
    assert_eq!(
        p("２０２６-１０-２５ １４：００"),
        once_at("2026-10-25T06:00:00+00:00")
    );
}

#[test]
fn cron_is_read_and_left_for_validation() {
    assert_eq!(
        p("cron:0 9 * * 1"),
        ScheduleSpec::Cron {
            expr: "0 9 * * 1".into()
        }
    );
    assert_eq!(
        p("CRON 0 9 * * 1"),
        ScheduleSpec::Cron {
            expr: "0 9 * * 1".into()
        }
    );
}

#[test]
fn nonsense_is_refused() {
    for s in [
        "",
        "下禮拜找時間",
        "每週 9:00",
        "每天 25:00",
        "明天 下午",
        "每小時99分",
        "13/40 9:00",
        "every blursday",
    ] {
        assert!(
            parse_natural(s, now(), tp()).is_err(),
            "{s} should be refused"
        );
    }
}
