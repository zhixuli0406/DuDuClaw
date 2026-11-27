//! Explicit-zone automation windows. All runtime comparisons use an injected UTC clock.

use std::path::Path;

use chrono::{DateTime, Datelike, NaiveTime, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::autopilot_store::AutopilotRuleRow;

#[derive(Debug)]
pub struct ActiveWindow {
    pub days: Vec<u8>,
    pub start: NaiveTime,
    pub end: NaiveTime,
    pub timezone: Option<String>,
}

fn parse_time(value: Option<&Value>) -> Result<NaiveTime, String> {
    let text = value.and_then(Value::as_str).ok_or("time must be HH:MM")?;
    let chars: Vec<char> = text.chars().collect();
    if !matches!(chars.as_slice(), [a, b, ':', c, d]
        if a.is_ascii_digit() && b.is_ascii_digit() && c.is_ascii_digit() && d.is_ascii_digit()) {
        return Err("time must be HH:MM".into());
    }
    NaiveTime::parse_from_str(text, "%H:%M").map_err(|_| "invalid time".into())
}

pub fn parse_active_window(v: &Value) -> Result<ActiveWindow, String> {
    let obj = v.as_object().ok_or("window must be an object")?;
    if obj.keys().any(|k| !matches!(k.as_str(), "days" | "start" | "end" | "timezone")) {
        return Err("unknown window key".into());
    }
    let values = obj.get("days").and_then(Value::as_array).ok_or("days must be an array")?;
    if values.is_empty() {
        return Err("days must not be empty".into());
    }
    let mut days = Vec::new();
    for value in values {
        let day = value.as_u64().filter(|d| (1..=7).contains(d)).ok_or("invalid day")? as u8;
        if days.contains(&day) {
            return Err("duplicate day".into());
        }
        days.push(day);
    }
    let start = parse_time(obj.get("start"))?;
    let end = parse_time(obj.get("end"))?;
    if start >= end {
        return Err("start must be before end".into());
    }
    let timezone = match obj.get("timezone") {
        None | Some(Value::Null) => None,
        Some(Value::String(zone)) => {
            zone.parse::<Tz>().map_err(|_| "invalid timezone")?;
            Some(zone.clone())
        }
        _ => return Err("timezone must be a string or null".into()),
    };
    Ok(ActiveWindow { days, start, end, timezone })
}

pub fn parse_run_policy(v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or("policy must be an object")?;
    if obj.len() != 1 || obj.get("outside_window").and_then(Value::as_str) != Some("skip") {
        return Err("only outside_window skip is supported".into());
    }
    Ok(())
}

pub fn validate_window_for_save(window: &Value, company_tz: Option<&str>) -> Result<(), String> {
    let window = parse_active_window(window)?;
    if window.timezone.is_none() {
        let zone = company_tz.filter(|z| !z.trim().is_empty())
            .ok_or("請先在系統設定填寫公司所在時區，或在這條規則指定時區")?;
        zone.parse::<Tz>().map_err(|_| "invalid company timezone")?;
    }
    Ok(())
}

pub fn read_company_timezone(home: &Path) -> Result<Option<Tz>, String> {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot read config.toml".into()),
    };
    let config: toml::Table = text.parse().map_err(|_| "invalid config.toml")?;
    let general = match config.get("general") {
        None => return Ok(None),
        Some(value) => value.as_table().ok_or("general must be a table")?,
    };
    let zone = match general.get("company_timezone") {
        None => return Ok(None),
        Some(value) => value.as_str().ok_or("company_timezone must be a string")?,
    };
    if zone.trim().is_empty() {
        return Ok(None);
    }
    zone.parse::<Tz>().map(Some).map_err(|_| "invalid company timezone".into())
}

#[derive(Debug, PartialEq, Eq)]
pub enum WindowDecision {
    Inside,
    Outside { detail: String },
    ZoneUnresolved { detail: String },
    Invalid { detail: String },
}

fn describe_days(days: &[u8]) -> String {
    let mut sorted = days.to_vec();
    sorted.sort_unstable();
    let names = ["週一", "週二", "週三", "週四", "週五", "週六", "週日"];
    let mut parts = Vec::new();
    let mut iter = sorted.into_iter().peekable();
    while let Some(first) = iter.next() {
        let mut last = first;
        while iter.peek() == Some(&(last + 1)) {
            last = iter.next().expect("peeked day exists");
        }
        let start = names[usize::from(first - 1)];
        parts.push(if first == last {
            start.to_string()
        } else {
            format!("{start}至{}", names[usize::from(last - 1)])
        });
    }
    parts.join("、")
}

pub fn evaluate_window(window_json: &str, company_tz: Result<Option<Tz>, String>, now: DateTime<Utc>) -> WindowDecision {
    let window = match serde_json::from_str::<Value>(window_json)
        .map_err(|_| "invalid window JSON".to_string()).and_then(|v| parse_active_window(&v)) {
        Ok(window) => window,
        Err(detail) => return WindowDecision::Invalid { detail },
    };
    let zone = match window.timezone.as_deref() {
        Some(zone) => zone.parse::<Tz>().ok(),
        None => company_tz.ok().flatten(),
    };
    let Some(zone) = zone else {
        return WindowDecision::ZoneUnresolved {
            detail: "無法確認規則時區，請檢查規則或公司時區設定，本次不執行".into(),
        };
    };
    let local = now.with_timezone(&zone);
    if window.days.contains(&(local.weekday().number_from_monday() as u8))
        && window.start <= local.time() && local.time() < window.end {
        WindowDecision::Inside
    } else {
        WindowDecision::Outside { detail: format!(
            "{}（{}）不在規則時段（{} {}–{}），本次不執行",
            local.format("%m/%d %H:%M"), zone, describe_days(&window.days),
            window.start.format("%H:%M"), window.end.format("%H:%M"),
        ) }
    }
}

pub fn rule_window_decision(home: &Path, rule: &AutopilotRuleRow, now: DateTime<Utc>) -> WindowDecision {
    let Some(json) = rule.active_window.as_deref() else {
        return WindowDecision::Inside;
    };
    // Invalid windows are reported by evaluate_window without reading config.
    let needs_company = serde_json::from_str::<Value>(json).ok()
        .and_then(|v| parse_active_window(&v).ok())
        .is_some_and(|w| w.timezone.is_none());
    let company = if needs_company { read_company_timezone(home) } else { Ok(None) };
    evaluate_window(json, company, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window(zone: Value) -> Value {
        json!({"days": [1, 2, 3, 4, 5], "start": "09:00", "end": "18:00", "timezone": zone})
    }

    fn at(window: &Value, instant: &str) -> WindowDecision {
        evaluate_window(&window.to_string(), Ok(None), instant.parse().unwrap())
    }

    #[test]
    fn window_taipei_vs_utc_host() {
        assert_eq!(at(&window(json!("Asia/Taipei")), "2026-11-27T01:30:00Z"), WindowDecision::Inside);
        assert!(matches!(at(&window(json!("UTC")), "2026-11-27T01:30:00Z"), WindowDecision::Outside { .. }));
    }

    #[test]
    fn window_weekend() {
        let WindowDecision::Outside { detail } = at(&window(json!("Asia/Taipei")), "2026-11-28T12:15:00Z") else {
            panic!("Saturday must be outside");
        };
        assert_eq!(detail, "11/28 20:15（Asia/Taipei）不在規則時段（週一至週五 09:00–18:00），本次不執行");
        assert_eq!(describe_days(&[6, 1, 3]), "週一、週三、週六");
        assert_eq!(describe_days(&[1, 2, 3, 6, 7]), "週一至週三、週六至週日");
    }

    #[test]
    fn window_boundaries() {
        let w = window(json!("Asia/Taipei"));
        for instant in ["2026-11-27T01:00:00Z", "2026-11-27T09:59:00Z", "2026-11-27T09:59:59Z"] {
            assert_eq!(at(&w, instant), WindowDecision::Inside, "{instant}");
        }
        for instant in ["2026-11-27T10:00:00Z", "2026-11-27T00:59:00Z"] {
            assert!(matches!(at(&w, instant), WindowDecision::Outside { .. }), "{instant}");
        }
    }

    #[test]
    fn dst_boundary_new_york() {
        let mut w = window(json!("America/New_York"));
        w["end"] = json!("17:00");
        for (instant, inside) in [
            ("2026-03-06T13:30:00Z", false),
            ("2026-03-09T13:30:00Z", true),
            ("2026-10-30T13:30:00Z", true),
            ("2026-11-02T13:30:00Z", false),
        ] {
            assert_eq!(matches!(at(&w, instant), WindowDecision::Inside), inside, "{instant}");
        }
        // Both copies of the repeated fall-back hour and the spring gap's
        // surrounding instants remain simple local wall-clock comparisons.
        w["days"] = json!([7]);
        w["start"] = json!("01:00");
        w["end"] = json!("03:00");
        for instant in ["2026-11-01T05:30:00Z", "2026-11-01T06:30:00Z", "2026-03-08T06:30:00Z"] {
            assert_eq!(at(&w, instant), WindowDecision::Inside);
        }
        assert!(matches!(at(&w, "2026-03-08T07:30:00Z"), WindowDecision::Outside { .. }));
    }

    #[test]
    fn window_requires_explicit_zone() {
        let w = window(Value::Null);
        for company in [None, Some(""), Some("  ")] {
            assert_eq!(validate_window_for_save(&w, company).unwrap_err(),
                "請先在系統設定填寫公司所在時區，或在這條規則指定時區");
        }
        assert!(validate_window_for_save(&w, Some("Asia/Taipei")).is_ok());
        assert!(validate_window_for_save(&w, Some("Not/AZone")).is_err());
        assert!(validate_window_for_save(&window(json!("Asia/Taipei")), None).is_ok());
        let now = "2026-11-27T01:30:00Z".parse().unwrap();
        for company in [Ok(None), Err("broken config".into())] {
            assert!(matches!(evaluate_window(&w.to_string(), company, now), WindowDecision::ZoneUnresolved { .. }));
        }
        assert_eq!(evaluate_window(&w.to_string(), Ok(Some(chrono_tz::Asia::Taipei)), now), WindowDecision::Inside);
        assert_eq!(evaluate_window(&window(json!("Asia/Taipei")).to_string(), Err("broken".into()), now), WindowDecision::Inside);
        let mut absent = w;
        absent.as_object_mut().unwrap().remove("timezone");
        assert!(parse_active_window(&absent).unwrap().timezone.is_none());
    }

    #[test]
    fn parse_active_window_rejections() {
        for (key, value) in [
            ("days", json!([])), ("days", json!([0])), ("days", json!([8])),
            ("days", json!([1, 1])), ("days", json!([1.5])), ("days", json!(["1"])),
            ("start", json!("9:00")), ("start", json!("24:00")),
            ("start", json!("09:60")), ("start", json!("０９:00")),
            ("start", json!("18:00")), ("start", json!("19:00")),
            ("end", json!("09:00")), ("unknown", json!(true)),
            ("timezone", json!("Not/AZone")), ("timezone", json!(false)),
        ] {
            let mut w = window(Value::Null);
            w[key] = value;
            assert!(parse_active_window(&w).is_err(), "{w}");
        }
        for key in ["days", "start", "end"] {
            let mut w = window(Value::Null);
            w.as_object_mut().unwrap().remove(key);
            assert!(parse_active_window(&w).is_err());
        }
        assert!(parse_active_window(&Value::Null).is_err());
        assert!(matches!(at(&json!({}), "2026-11-27T01:30:00Z"), WindowDecision::Invalid { .. }));
        assert!(matches!(evaluate_window("{", Ok(None), Utc::now()), WindowDecision::Invalid { .. }));
    }

    #[test]
    fn parse_run_policy_rejections() {
        assert!(parse_run_policy(&json!({"outside_window": "skip"})).is_ok());
        for policy in [json!({}), json!({"outside_window": "defer"}),
            json!({"outside_window": "skip", "extra": true}), json!(null),
            json!({"outside_window": false})] {
            assert!(parse_run_policy(&policy).is_err());
        }
    }

    #[test]
    fn read_company_timezone_cases() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        assert_eq!(read_company_timezone(home.path()).unwrap(), None);
        for (text, expected) in [
            ("", None), ("[general]\n", None),
            ("[general]\ncompany_timezone = ''", None),
            ("[general]\ncompany_timezone = 'Asia/Taipei'", Some(chrono_tz::Asia::Taipei)),
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(read_company_timezone(home.path()).unwrap(), expected);
        }
        for text in ["[general]\ncompany_timezone = 'Not/AZone'", "[general]\ncompany_timezone = 8", "[broken", "general = false"] {
            std::fs::write(&path, text).unwrap();
            assert!(read_company_timezone(home.path()).is_err());
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read_company_timezone(home.path()).is_err());
    }
}
