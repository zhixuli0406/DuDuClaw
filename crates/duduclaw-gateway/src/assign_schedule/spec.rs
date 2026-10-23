//! When a scheduled assignment fires: a one-shot instant or a repeat
//! (hourly, daily, weekdays, weekly; a cron expression for Admins), read in
//! an IANA zone or the host's local zone. Pure: no I/O.

use std::str::FromStr;

use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, TimeZone, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};

/// How often a scheduled assignment fires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpec {
    /// Once, at an instant (RFC 3339, UTC).
    Once { at: String },
    /// Every hour at `minute`.
    Hourly { minute: u8 },
    /// Every day at `hour:minute`.
    Daily { hour: u8, minute: u8 },
    /// Monday to Friday at `hour:minute`.
    Weekdays { hour: u8, minute: u8 },
    /// On `days` (ISO numbering, 1 = Monday … 7 = Sunday) at `hour:minute`.
    Weekly { days: Vec<u8>, hour: u8, minute: u8 },
    /// A cron expression (5, 6 or 7 fields). Admins only.
    Cron { expr: String },
}

/// Why a schedule was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    /// Not a schedule this module can read.
    Invalid,
    /// An unknown timezone name.
    BadTimezone,
    /// A cron expression from someone who is not an Admin.
    CronAdminOnly,
    /// A one-shot time that has passed (or is under a minute away).
    InThePast,
    /// A one-shot time more than a year ahead.
    TooFar,
    /// Fires more often than `min_minutes` allows.
    TooFrequent { min_minutes: i64 },
    /// A repeat that never fires.
    NeverFires,
}

impl SpecError {
    /// Machine code for RPC errors and audit lines.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_schedule",
            Self::BadTimezone => "invalid_timezone",
            Self::CronAdminOnly => "cron_admin_only",
            Self::InThePast => "time_passed",
            Self::TooFar => "too_far_ahead",
            Self::TooFrequent { .. } => "too_frequent",
            Self::NeverFires => "never_fires",
        }
    }
}

/// The zone a schedule's wall-clock times are read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Named(chrono_tz::Tz),
    /// The host's local zone (`TZ` / `/etc/localtime`).
    Local,
}

impl Zone {
    /// `None` / blank = the host's zone; an unknown name is an error.
    pub fn parse(name: Option<&str>) -> Result<Self, SpecError> {
        match name.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(Self::Local),
            Some(n) => duduclaw_core::parse_timezone(n)
                .map(Self::Named)
                .ok_or(SpecError::BadTimezone),
        }
    }

    /// A wall-clock time in this zone as UTC (the earlier instant when it is
    /// ambiguous, the hour after when it falls in a DST gap).
    pub fn to_utc(self, local: NaiveDateTime) -> Option<DateTime<Utc>> {
        match self {
            Self::Named(tz) => resolve(&tz, local),
            Self::Local => resolve(&chrono::Local, local),
        }
    }

    /// `now` as a wall-clock time in this zone.
    pub fn local_now(self, now: DateTime<Utc>) -> NaiveDateTime {
        match self {
            Self::Named(tz) => now.with_timezone(&tz).naive_local(),
            Self::Local => now.with_timezone(&chrono::Local).naive_local(),
        }
    }
}

fn resolve<T: TimeZone>(tz: &T, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&local) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        LocalResult::Ambiguous(a, _) => Some(a.with_timezone(&Utc)),
        LocalResult::None => match tz.from_local_datetime(&(local + Duration::hours(1))) {
            LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => Some(t.with_timezone(&Utc)),
            LocalResult::None => None,
        },
    }
}

const WEEKDAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// Shortest gap Admins may schedule (cron included).
pub const ADMIN_MIN_INTERVAL_MINUTES: i64 = 5;
/// How far ahead a one-shot may be set.
pub const ONCE_MAX_AHEAD_DAYS: i64 = 366;

impl ScheduleSpec {
    /// Whether only an Admin may create (and keep) this schedule.
    pub fn admin_only(&self) -> bool {
        matches!(self, Self::Cron { .. })
    }

    fn cron_expr(&self) -> Option<String> {
        let at = |h: u8, m: u8| format!("0 {m} {h}");
        match self {
            Self::Once { .. } => None,
            Self::Hourly { minute } => Some(format!("0 {minute} * * * *")),
            Self::Daily { hour, minute } => Some(format!("{} * * *", at(*hour, *minute))),
            Self::Weekdays { hour, minute } => Some(format!("{} * * Mon-Fri", at(*hour, *minute))),
            Self::Weekly { days, hour, minute } => {
                let names: Vec<&str> = days
                    .iter()
                    .filter_map(|d| WEEKDAY_NAMES.get(usize::from(*d).checked_sub(1)?).copied())
                    .collect();
                Some(format!("{} * * {}", at(*hour, *minute), names.join(",")))
            }
            Self::Cron { expr } => Some(duduclaw_core::cron_tz::normalise_cron(expr)),
        }
    }

    fn schedule(&self) -> Result<Option<Schedule>, SpecError> {
        match self.cron_expr() {
            None => Ok(None),
            Some(e) => Schedule::from_str(&e)
                .map(Some)
                .map_err(|_| SpecError::Invalid),
        }
    }

    /// The first fire strictly after `after`; `None` when there is none.
    pub fn next_after(&self, after: DateTime<Utc>, zone: Zone) -> Option<DateTime<Utc>> {
        if let Self::Once { at } = self {
            let at = parse_utc(at)?;
            return (at > after).then_some(at);
        }
        let schedule = self.schedule().ok()??;
        match zone {
            Zone::Named(tz) => schedule
                .after(&after.with_timezone(&tz))
                .next()
                .map(|t| t.with_timezone(&Utc)),
            Zone::Local => schedule
                .after(&after.with_timezone(&chrono::Local))
                .next()
                .map(|t| t.with_timezone(&Utc)),
        }
    }

    /// Shortest gap between the next dozen fires (`None` for a one-shot).
    fn min_gap(&self, now: DateTime<Utc>, zone: Zone) -> Option<Duration> {
        if matches!(self, Self::Once { .. }) {
            return None;
        }
        let mut prev = self.next_after(now, zone)?;
        let mut best: Option<Duration> = None;
        for _ in 0..12 {
            let Some(next) = self.next_after(prev, zone) else {
                break;
            };
            let gap = next - prev;
            best = Some(best.map_or(gap, |b| b.min(gap)));
            prev = next;
        }
        best
    }

    /// Check the shape, who may use it and how often it fires; returns the
    /// first fire. `min_minutes` is the non-Admin floor.
    pub fn validate(
        &self,
        is_admin: bool,
        now: DateTime<Utc>,
        zone: Zone,
        min_minutes: i64,
    ) -> Result<DateTime<Utc>, SpecError> {
        self.validate_policy(is_admin, now, zone, min_minutes)?;
        if let Self::Once { at } = self {
            let at = parse_utc(at).ok_or(SpecError::Invalid)?;
            if at < now + Duration::minutes(1) {
                return Err(SpecError::InThePast);
            }
            if at > now + Duration::days(ONCE_MAX_AHEAD_DAYS) {
                return Err(SpecError::TooFar);
            }
            return Ok(at);
        }
        let first = self.next_after(now, zone).ok_or(SpecError::NeverFires)?;
        Ok(first)
    }

    /// Recheck the current policy at firing/resume without applying the
    /// creation-only one-shot lead time to a slot that is already due.
    pub(crate) fn validate_policy(
        &self,
        is_admin: bool,
        now: DateTime<Utc>,
        zone: Zone,
        min_minutes: i64,
    ) -> Result<(), SpecError> {
        self.check_shape()?;
        if self.admin_only() && !is_admin {
            return Err(SpecError::CronAdminOnly);
        }
        let floor = if is_admin {
            ADMIN_MIN_INTERVAL_MINUTES
        } else {
            min_minutes
        };
        if let Some(gap) = self.min_gap(now, zone) {
            if gap < Duration::minutes(floor) {
                return Err(SpecError::TooFrequent { min_minutes: floor });
            }
        }
        Ok(())
    }

    fn check_shape(&self) -> Result<(), SpecError> {
        let hm = |h: &u8, m: &u8| *h < 24 && *m < 60;
        let ok = match self {
            Self::Once { at } => parse_utc(at).is_some(),
            Self::Hourly { minute } => *minute < 60,
            Self::Daily { hour, minute } | Self::Weekdays { hour, minute } => hm(hour, minute),
            Self::Weekly { days, hour, minute } => {
                hm(hour, minute) && !days.is_empty() && days.iter().all(|d| (1..=7).contains(d))
            }
            Self::Cron { expr } => !expr.trim().is_empty() && expr.len() <= 200,
        };
        if !ok {
            return Err(SpecError::Invalid);
        }
        self.schedule().map(|_| ())
    }

    /// The same schedule with weekly days sorted and de-duplicated.
    pub fn normalized(self) -> Self {
        match self {
            Self::Weekly {
                mut days,
                hour,
                minute,
            } => {
                days.sort_unstable();
                days.dedup();
                Self::Weekly { days, hour, minute }
            }
            other => other,
        }
    }
}

/// An RFC 3339 instant as UTC.
pub fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `HH:MM` (also `H:MM`, `HH：MM`).
pub fn parse_hhmm(s: &str) -> Option<(u8, u8)> {
    let s = s.trim().replace('：', ":");
    let (h, m) = s.split_once(':')?;
    let h: u8 = h.trim().parse().ok()?;
    let m: u8 = m.trim().parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

/// Read the dashboard's structured schedule:
/// `{kind:"once", at}` (RFC 3339, or a wall-clock `YYYY-MM-DDTHH:MM` in
/// `zone`), `{kind:"hourly", minute}`, `{kind:"daily"|"weekdays", time}`,
/// `{kind:"weekly", days:[1..7], time}`, `{kind:"cron", expr}`.
pub fn from_json(v: &serde_json::Value, zone: Zone) -> Result<ScheduleSpec, SpecError> {
    let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    let time = || {
        v.get("time")
            .and_then(|t| t.as_str())
            .and_then(parse_hhmm)
            .ok_or(SpecError::Invalid)
    };
    let spec = match kind {
        "once" => {
            let at = v
                .get("at")
                .and_then(|a| a.as_str())
                .ok_or(SpecError::Invalid)?;
            let utc = match parse_utc(at) {
                Some(t) => t,
                None => {
                    let naive = parse_naive(at).ok_or(SpecError::Invalid)?;
                    zone.to_utc(naive).ok_or(SpecError::Invalid)?
                }
            };
            ScheduleSpec::Once {
                at: utc.to_rfc3339(),
            }
        }
        "hourly" => {
            let minute = v.get("minute").and_then(|m| m.as_u64()).unwrap_or(0);
            ScheduleSpec::Hourly {
                minute: u8::try_from(minute).map_err(|_| SpecError::Invalid)?,
            }
        }
        "daily" => {
            let (hour, minute) = time()?;
            ScheduleSpec::Daily { hour, minute }
        }
        "weekdays" => {
            let (hour, minute) = time()?;
            ScheduleSpec::Weekdays { hour, minute }
        }
        "weekly" => {
            let (hour, minute) = time()?;
            let days = v
                .get("days")
                .and_then(|d| d.as_array())
                .ok_or(SpecError::Invalid)?
                .iter()
                .map(|d| {
                    d.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or(SpecError::Invalid)
                })
                .collect::<Result<Vec<u8>, _>>()?;
            ScheduleSpec::Weekly { days, hour, minute }
        }
        "cron" => ScheduleSpec::Cron {
            expr: v
                .get("expr")
                .and_then(|e| e.as_str())
                .map(|e| e.trim().to_string())
                .ok_or(SpecError::Invalid)?,
        },
        _ => return Err(SpecError::Invalid),
    };
    Ok(spec.normalized())
}

/// `YYYY-MM-DDTHH:MM[:SS]` or `YYYY-MM-DD HH:MM[:SS]` (no offset).
pub fn parse_naive(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().replace('T', " ");
    ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y/%m/%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(&s, f).ok())
}

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
