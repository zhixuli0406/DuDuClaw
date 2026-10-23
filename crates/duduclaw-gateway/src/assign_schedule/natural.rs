//! The `時間:` / `排程:` segment of a chat `/交辦`, read into a
//! [`ScheduleSpec`]. zh-TW natural forms (`每週一 9:00`, `平日 早上9點`,
//! `明天 下午3點`, `10/25 14:00`), a few English ones (`daily 09:00`,
//! `every monday 9:00`, `tomorrow 15:00`), ISO / RFC 3339 instants and
//! `cron:<expr>` (Admins only, checked later). A recurring form without a
//! time means 09:00; the confirmation shows what was understood.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};

use super::spec::{ScheduleSpec, SpecError, Zone, parse_hhmm, parse_naive, parse_utc};

const DEFAULT_TIME: (u8, u8) = (9, 0);

/// Read one schedule phrase.
pub fn parse_natural(
    text: &str,
    now: DateTime<Utc>,
    zone: Zone,
) -> Result<ScheduleSpec, SpecError> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err(SpecError::Invalid);
    }
    if let Some(expr) = strip_ci(raw, "cron:").or_else(|| strip_ci(raw, "cron ")) {
        return Ok(ScheduleSpec::Cron {
            expr: expr.trim().to_string(),
        });
    }
    if let Some(t) = parse_utc(raw) {
        return Ok(ScheduleSpec::Once { at: t.to_rfc3339() });
    }
    if let Some(n) = parse_naive(raw) {
        return once(zone, n);
    }
    let s = normalize(raw);
    parse_english(&s, now, zone)
        .or_else(|| parse_chinese(&s, now, zone))
        .unwrap_or(Err(SpecError::Invalid))
        .map(ScheduleSpec::normalized)
}

fn once(zone: Zone, local: chrono::NaiveDateTime) -> Result<ScheduleSpec, SpecError> {
    zone.to_utc(local)
        .map(|t| ScheduleSpec::Once { at: t.to_rfc3339() })
        .ok_or(SpecError::Invalid)
}

/// ASCII case-insensitive prefix strip, char-boundary safe.
fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Full-width digits and colon to ASCII, the several words for "week" to
/// `週`, whitespace collapsed to single spaces.
fn normalize(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '：' => ':',
            '周' => '週',
            '后' => '後',
            '个' => '個',
            '点' => '點',
            '号' => '號',
            '礼' => '禮',
            _ => c,
        })
        .collect();
    let mapped = ["星期", "禮拜", "每個"].iter().fold(mapped, |acc, w| {
        acc.replace(w, if *w == "每個" { "每" } else { "週" })
    });
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── English ─────────────────────────────────────────────────────

fn parse_english(
    s: &str,
    now: DateTime<Utc>,
    zone: Zone,
) -> Option<Result<ScheduleSpec, SpecError>> {
    let lower = s.to_ascii_lowercase();
    let (head, time) = split_trailing_time(&lower);
    let (hour, minute) = time.unwrap_or(DEFAULT_TIME);
    let spec = match head.trim() {
        "hourly" | "every hour" => ScheduleSpec::Hourly {
            minute: time.map(|(_, m)| m).unwrap_or(0),
        },
        "daily" | "every day" => ScheduleSpec::Daily { hour, minute },
        "weekdays" | "every weekday" => ScheduleSpec::Weekdays { hour, minute },
        "today" | "tomorrow" => {
            let days = i64::from(head.trim() == "tomorrow");
            let date = zone.local_now(now).date() + Duration::days(days);
            return Some(once(zone, date.and_time(hm(hour, minute)?)));
        }
        other => {
            let rest = other
                .strip_prefix("weekly ")
                .or_else(|| other.strip_prefix("every "))?;
            let days = rest
                .split([',', ' ', '/'])
                .filter(|p| !p.is_empty())
                .map(english_day)
                .collect::<Option<Vec<u8>>>()?;
            if days.is_empty() {
                return None;
            }
            ScheduleSpec::Weekly { days, hour, minute }
        }
    };
    Some(Ok(spec))
}

fn english_day(w: &str) -> Option<u8> {
    let w = w.trim().trim_end_matches('s');
    let days = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    days.iter()
        .position(|d| w.len() >= 3 && w.starts_with(d))
        .and_then(|i| u8::try_from(i + 1).ok())
}

/// `"<head> HH:MM"` → `("<head>", Some((h, m)))`.
fn split_trailing_time(s: &str) -> (&str, Option<(u8, u8)>) {
    match s.rsplit_once(' ') {
        Some((head, tail)) => match parse_hhmm(tail) {
            Some(t) => (head, Some(t)),
            None => (s, None),
        },
        None => (s, None),
    }
}

fn hm(h: u8, m: u8) -> Option<NaiveTime> {
    NaiveTime::from_hms_opt(u32::from(h), u32::from(m), 0)
}

// ── zh-TW ───────────────────────────────────────────────────────

fn parse_chinese(
    s: &str,
    now: DateTime<Utc>,
    zone: Zone,
) -> Option<Result<ScheduleSpec, SpecError>> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(rest) = compact.strip_prefix("每小時") {
        let minute = if rest.is_empty() {
            0
        } else {
            leading_number(rest.trim_start_matches(':'))
                .filter(|(n, _)| *n < 60)?
                .0
        };
        return Some(Ok(ScheduleSpec::Hourly { minute }));
    }
    for (prefix, weekdays) in [
        ("每天", false),
        ("每日", false),
        ("天天", false),
        ("平日", true),
        ("工作日", true),
        ("每工作日", true),
        ("每工作天", true),
        ("週一到週五", true),
        ("週一至週五", true),
        ("每週一到五", true),
        ("每週一至五", true),
        ("每週一到週五", true),
        ("每週一至週五", true),
    ] {
        if let Some(rest) = compact.strip_prefix(prefix) {
            let (hour, minute) = match time_or_default(rest) {
                Some(t) => t,
                None => return Some(Err(SpecError::Invalid)),
            };
            return Some(Ok(if weekdays {
                ScheduleSpec::Weekdays { hour, minute }
            } else {
                ScheduleSpec::Daily { hour, minute }
            }));
        }
    }
    if let Some(rest) = compact.strip_prefix("每週") {
        return Some(weekly(rest));
    }
    for (prefix, days) in [("今天", 0), ("明天", 1), ("後天", 2)] {
        if let Some(rest) = compact.strip_prefix(prefix) {
            let date = zone.local_now(now).date() + Duration::days(days);
            return Some(at_date(zone, date, rest));
        }
    }
    // Dates keep their spaces: `10/25 14:00` must not run together.
    let (date, rest) = leading_date(s, zone.local_now(now).date())?;
    Some(at_date(zone, date, rest))
}

fn at_date(zone: Zone, date: NaiveDate, rest: &str) -> Result<ScheduleSpec, SpecError> {
    let (h, m) = time_or_default(rest).ok_or(SpecError::Invalid)?;
    once(zone, date.and_time(hm(h, m).ok_or(SpecError::Invalid)?))
}

/// `一、三、五 9:00`, `一三五`, `一和三`.
fn weekly(rest: &str) -> Result<ScheduleSpec, SpecError> {
    let mut days = Vec::new();
    let mut tail = rest;
    loop {
        let mut chars = tail.chars();
        let Some(c) = chars.next() else { break };
        if let Some(d) = zh_day(c) {
            days.push(d);
            tail = chars.as_str();
            tail = tail.strip_prefix('週').unwrap_or(tail);
        } else if matches!(c, '、' | ',' | '，' | '/' | '和' | '及') {
            tail = chars.as_str();
            tail = tail.strip_prefix('週').unwrap_or(tail);
        } else {
            break;
        }
    }
    if days.is_empty() {
        return Err(SpecError::Invalid);
    }
    let (hour, minute) = time_or_default(tail).ok_or(SpecError::Invalid)?;
    Ok(ScheduleSpec::Weekly { days, hour, minute })
}

fn zh_day(c: char) -> Option<u8> {
    Some(match c {
        '一' => 1,
        '二' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '日' | '天' => 7,
        _ => return None,
    })
}

/// `2026/10/25`, `2026-10-25`, `10/25`, `10月25日`, `2026年10月25號`. A date
/// without a year that has passed this year means next year.
fn leading_date(s: &str, today: NaiveDate) -> Option<(NaiveDate, &str)> {
    let mut nums: Vec<u32> = Vec::new();
    let mut rest = s;
    loop {
        let n = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if n == 0 || n > 4 {
            return None;
        }
        let (digits, r) = rest.split_at(n);
        nums.push(digits.parse().ok()?);
        rest = r;
        if nums.len() == 3 {
            break;
        }
        match rest.chars().next() {
            Some(c @ ('/' | '-' | '年' | '月')) => rest = &rest[c.len_utf8()..],
            _ => break,
        }
    }
    let rest = rest
        .strip_prefix('日')
        .or_else(|| rest.strip_prefix('號'))
        .unwrap_or(rest);
    let date = match nums.as_slice() {
        [y, m, d] if *y >= 2000 => NaiveDate::from_ymd_opt(i32::try_from(*y).ok()?, *m, *d)?,
        [m, d] => {
            let this_year = NaiveDate::from_ymd_opt(today.year(), *m, *d)?;
            if this_year < today {
                NaiveDate::from_ymd_opt(today.year() + 1, *m, *d)?
            } else {
                this_year
            }
        }
        _ => return None,
    };
    Some((date, rest))
}

/// Leading ASCII digits (one or two) as a number, and the rest.
fn leading_number(s: &str) -> Option<(u8, &str)> {
    let n = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if n == 0 || n > 2 {
        return None;
    }
    let (digits, rest) = s.split_at(n);
    Some((digits.parse().ok()?, rest))
}

/// A time of day, or 09:00 when the text is empty.
fn time_or_default(s: &str) -> Option<(u8, u8)> {
    let s = s.trim().trim_start_matches(['的', ' ']);
    if s.is_empty() {
        return Some(DEFAULT_TIME);
    }
    parse_zh_time(s)
}

/// `9:00`, `09:30`, `9點`, `9點半`, `9點15分`, `9時`, with an optional
/// `上午` / `早上` / `凌晨` / `中午` / `下午` / `傍晚` / `晚上` in front.
fn parse_zh_time(s: &str) -> Option<(u8, u8)> {
    let periods = [
        ("上午", 0u8),
        ("早上", 0),
        ("凌晨", 0),
        ("中午", 1),
        ("下午", 2),
        ("傍晚", 2),
        ("晚上", 2),
    ];
    let (period, body) = periods
        .iter()
        .find_map(|(p, k)| s.strip_prefix(p).map(|rest| (Some(*k), rest)))
        .unwrap_or((None, s));
    let (mut hour, minute) = if let Some(t) = parse_hhmm(body) {
        t
    } else {
        let (h, rest) = leading_number(body)?;
        let rest = rest
            .strip_prefix('點')
            .or_else(|| rest.strip_prefix('時'))?;
        let minute = if rest.is_empty() || rest == "整" {
            0
        } else if rest == "半" {
            30
        } else {
            let (m, tail) = leading_number(rest)?;
            if !(tail.is_empty() || tail == "分") {
                return None;
            }
            m
        };
        (h, minute)
    };
    match period {
        Some(2) if hour < 12 => hour += 12,
        Some(1) if hour < 11 => hour += 12,
        Some(0) if hour == 12 => hour = 0,
        _ => {}
    }
    (hour < 24 && minute < 60).then_some((hour, minute))
}

#[cfg(test)]
#[path = "natural_tests.rs"]
mod tests;
