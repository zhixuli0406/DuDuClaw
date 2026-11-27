//! Company timezone reads and creation defaults. Stored schedules keep their own zone.
use std::path::Path;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanyZone {
    Set(Tz),
    Unset,
    Invalid,
}

pub fn read(home: &Path) -> CompanyZone {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return CompanyZone::Unset,
        Err(_) => return CompanyZone::Invalid,
    };
    let Ok(table) = text.parse::<toml::Table>() else { return CompanyZone::Invalid; };
    let general = match table.get("general") {
        None => return CompanyZone::Unset,
        Some(value) => match value.as_table() {
            Some(table) => table,
            None => return CompanyZone::Invalid,
        },
    };
    match general.get("company_timezone") {
        None => CompanyZone::Unset,
        Some(toml::Value::String(name)) if name.trim().is_empty() => CompanyZone::Unset,
        Some(toml::Value::String(name)) => match name.parse::<Tz>() {
            Ok(zone) => CompanyZone::Set(zone),
            Err(_) => CompanyZone::Invalid,
        },
        Some(_) => CompanyZone::Invalid,
    }
}

pub fn read_source(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let table = text.parse::<toml::Table>().ok()?;
    Some(table.get("general")?.as_table()?.get("company_timezone_source")?.as_str()?.to_owned())
}

/// Daily samples over two years (731 days covers a leap year). A zone's UTC
/// offset changes only at a transition and then holds for the whole period
/// until the next one; every real period is far longer than one day (the
/// shortest current one, Africa/Casablanca's Ramadan change, lasts weeks), so
/// any transition inside the window changes at least one daily sample.
const FIXED_OFFSET_SAMPLE_DAYS: i64 = 731;

/// Same total UTC offset at `now` and at every daily sample up to two years ahead.
pub fn fixed_offset_zone(tz: chrono_tz::Tz, now: chrono::DateTime<Utc>) -> bool {
    use chrono::{Offset, TimeZone};
    let offset = |day: i64| tz.offset_from_utc_datetime(&(now + chrono::Duration::days(day)).naive_utc()).fix();
    let first = offset(0);
    (1..=FIXED_OFFSET_SAMPLE_DAYS).all(|day| offset(day) == first)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneSource {
    Company,
    UtcUnset,
    UtcInvalid,
    UtcMemberDst,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneChoice {
    pub zone: String,
    pub source: ZoneSource,
}

pub fn default_for_new_schedule(
    company: &CompanyZone,
    human_origin: bool,
    now: DateTime<Utc>,
) -> ZoneChoice {
    let source = match company {
        CompanyZone::Set(zone) if human_origin && !fixed_offset_zone(*zone, now) => ZoneSource::UtcMemberDst,
        CompanyZone::Set(zone) => return ZoneChoice { zone: zone.to_string(), source: ZoneSource::Company },
        CompanyZone::Unset => ZoneSource::UtcUnset,
        CompanyZone::Invalid => ZoneSource::UtcInvalid,
    };
    ZoneChoice { zone: "UTC".into(), source }
}

pub fn zone_note_zh(source: &ZoneSource) -> Option<&'static str> {
    match source {
        ZoneSource::Company => None,
        ZoneSource::UtcUnset => Some("公司時區尚未設定，這個排程以 UTC 計時。"),
        ZoneSource::UtcInvalid => Some("公司時區設定無效，這個排程以 UTC 計時。"),
        ZoneSource::UtcMemberDst => Some("公司時區有夏令時間，成員自建的例行工作以 UTC 計時；要依當地時間請管理員建立。"),
    }
}

pub fn format_local(company: &CompanyZone, at: DateTime<Utc>, fmt: &str) -> String {
    match company {
        CompanyZone::Set(zone) => {
            let local = at.with_timezone(zone);
            format!("{} {}", local.format(fmt), local.format("%Z"))
        }
        CompanyZone::Unset | CompanyZone::Invalid => format!("{} UTC", at.format(fmt)),
    }
}

/// Detect the host system's IANA timezone name (e.g. `"Asia/Taipei"`,
/// `"America/New_York"`, `"UTC"`).
///
/// Uses `iana_time_zone::get_timezone()` which reads `/etc/localtime` on
/// Unix / the registry on Windows. Validated against `chrono-tz` so we
/// never return a name the scheduler would reject — defensive because
/// `iana-time-zone` is allowed to surface historical aliases that
/// `chrono-tz`'s database has dropped.
///
/// Returns `None` when the host has no discoverable TZ (extremely rare
/// on real machines — typical in minimal Docker images with no
/// `/etc/localtime`). Callers should fall back to UTC.
///
/// Used only for the one-time company timezone initialization at upgrade.
pub fn detect_host_zone() -> Option<String> {
    let name = iana_time_zone::get_timezone().ok()?;
    // Round-trip through chrono-tz so we only ever hand back names the
    // scheduler's `crate::parse_timezone` will accept.
    if crate::parse_timezone(&name).is_some() {
        Some(name)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitDecision {
    AlreadySet,
    AlreadyDone,
    Write(String),
    Unavailable,
}

pub fn upgrade_init_decision(
    config_has_key: bool,
    marker_exists: bool,
    detected: Option<&str>,
) -> InitDecision {
    if marker_exists { return InitDecision::AlreadyDone; }
    if config_has_key { return InitDecision::AlreadySet; }
    match detected.and_then(crate::parse_timezone) {
        Some(zone) => InitDecision::Write(zone.to_string()),
        None => InitDecision::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 27, 1, 30, 0).unwrap()
    }

    #[test]
    fn read_cases() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        assert_eq!(read(home.path()), CompanyZone::Unset);
        for (text, expected) in [
            ("", CompanyZone::Unset),
            ("[other]\nvalue = 1", CompanyZone::Unset),
            ("[general]", CompanyZone::Unset),
            ("[general]\ncompany_timezone = ''", CompanyZone::Unset),
            ("[general]\ncompany_timezone = '   '", CompanyZone::Unset),
            ("[general]\ncompany_timezone = 'Asia/Taipei'", CompanyZone::Set(chrono_tz::Asia::Taipei)),
            ("[general]\ncompany_timezone = 'Not/AZone'", CompanyZone::Invalid),
            ("[general]\ncompany_timezone = 8", CompanyZone::Invalid),
            ("[general]\ncompany_timezone = false", CompanyZone::Invalid),
            ("general = false", CompanyZone::Invalid),
            ("[broken", CompanyZone::Invalid),
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(read(home.path()), expected, "{text}");
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(read(home.path()), CompanyZone::Invalid);
    }

    #[test]
    fn read_source_cases() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        assert_eq!(read_source(home.path()), None);
        for text in ["", "[general]", "general = false", "[broken",
            "[general]\ncompany_timezone_source = 8"] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(read_source(home.path()), None, "{text}");
        }
        std::fs::write(&path, "[general]\ncompany_timezone_source = 'detected_at_upgrade'").unwrap();
        assert_eq!(read_source(home.path()).as_deref(), Some("detected_at_upgrade"));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(read_source(home.path()), None);
    }

    #[test]
    fn fixed_offset_zone_cases() {
        for name in ["UTC", "Etc/UTC", "Etc/GMT", "GMT", "Asia/Taipei", "Asia/Tokyo",
            "Asia/Kolkata", "America/Phoenix", "Etc/GMT-8"] {
            assert!(fixed_offset_zone(name.parse().unwrap(), now()), "{name}");
        }
        for name in ["America/New_York", "Europe/London", "Australia/Sydney", "Africa/Casablanca"] {
            assert!(!fixed_offset_zone(name.parse().unwrap(), now()), "{name}");
        }
    }

    #[test]
    fn default_for_new_schedule_truth_table() {
        for human_origin in [false, true] {
            for (company, zone, source) in [
                (CompanyZone::Set(chrono_tz::Asia::Taipei), "Asia/Taipei", ZoneSource::Company),
                (CompanyZone::Set(chrono_tz::UTC), "UTC", ZoneSource::Company),
                (CompanyZone::Unset, "UTC", ZoneSource::UtcUnset),
                (CompanyZone::Invalid, "UTC", ZoneSource::UtcInvalid),
                (CompanyZone::Set(chrono_tz::America::New_York),
                    if human_origin { "UTC" } else { "America/New_York" },
                    if human_origin { ZoneSource::UtcMemberDst } else { ZoneSource::Company }),
            ] {
                assert_eq!(default_for_new_schedule(&company, human_origin, now()),
                    ZoneChoice { zone: zone.into(), source });
            }
        }
    }

    #[test]
    fn zone_note_zh_cases() {
        assert_eq!(zone_note_zh(&ZoneSource::Company), None);
        assert_eq!(zone_note_zh(&ZoneSource::UtcUnset), Some("公司時區尚未設定，這個排程以 UTC 計時。"));
        assert_eq!(zone_note_zh(&ZoneSource::UtcInvalid), Some("公司時區設定無效，這個排程以 UTC 計時。"));
        assert_eq!(zone_note_zh(&ZoneSource::UtcMemberDst), Some("公司時區有夏令時間，成員自建的例行工作以 UTC 計時；要依當地時間請管理員建立。"));
    }

    #[test]
    fn format_local_labels_zone() {
        let fmt = "%Y-%m-%d %H:%M";
        assert_eq!(format_local(&CompanyZone::Set(chrono_tz::Asia::Taipei), now(), fmt), "2026-11-27 09:30 CST");
        assert_eq!(format_local(&CompanyZone::Set(chrono_tz::America::New_York), now(), fmt), "2026-11-26 20:30 EST");
        for company in [CompanyZone::Unset, CompanyZone::Invalid] {
            assert_eq!(format_local(&company, now(), fmt), "2026-11-27 01:30 UTC");
        }
    }

    #[test]
    fn detect_host_zone_returns_valid_iana_name() {
        // No particular host zone or detection success is required.
        if let Some(name) = detect_host_zone() {
            assert!(!name.is_empty());
            assert!(crate::parse_timezone(&name).is_some());
        }
    }

    #[test]
    fn upgrade_init_decision_cases() {
        for detected in [None, Some("Not/AZone"), Some(""), Some("Asia/Taipei")] {
            for config_has_key in [false, true] {
                assert_eq!(upgrade_init_decision(config_has_key, true, detected), InitDecision::AlreadyDone);
            }
            assert_eq!(upgrade_init_decision(true, false, detected), InitDecision::AlreadySet);
        }
        assert_eq!(upgrade_init_decision(false, false, Some("Asia/Taipei")), InitDecision::Write("Asia/Taipei".into()));
        for detected in [None, Some("Not/AZone"), Some(""), Some("   ")] {
            assert_eq!(upgrade_init_decision(false, false, detected), InitDecision::Unavailable);
        }
    }
}
