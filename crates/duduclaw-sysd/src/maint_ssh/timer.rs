//! The PID 1 fallback timer (R7, design §4.5): a transient
//! `duduclaw-maint-ssh-expire.timer` that runs the OS's
//! `/usr/libexec/duduclaw/maint-ssh-close` if sysd itself is not around to
//! close the window. Every command is a fixed argv; the only computed parts
//! are two numbers rendered by this module.

use std::time::Duration;

use super::host::argv;

/// Transient unit name (systemd adds `.timer` / `.service`).
pub const FALLBACK_UNIT: &str = "duduclaw-maint-ssh-expire";
pub const FALLBACK_TIMER: &str = "duduclaw-maint-ssh-expire.timer";
pub const FALLBACK_SERVICE: &str = "duduclaw-maint-ssh-expire.service";
/// The OS-side close script (DuDuClaw-OS PR O-2).
pub const FALLBACK_SCRIPT: &str = "/usr/libexec/duduclaw/maint-ssh-close";
/// The fallback fires this long after sysd's own deadline.
pub const FALLBACK_GRACE_SECS: u64 = 60;

/// Clear a failed leftover so `systemd-run --unit=` can reuse the name
/// (R2-L4). Its result is ignored: the units usually do not exist.
pub fn reset_failed_argv() -> Vec<String> {
    argv(&["systemctl", "reset-failed", FALLBACK_TIMER, FALLBACK_SERVICE])
}

/// Arm the timer: a monotonic trigger (`--on-active`, paused during suspend)
/// and an absolute UTC trigger (`OnCalendar`, covers suspend), whichever
/// comes first. The calendar time comes from the wall clock: if that is
/// wrong (no RTC, before NTP sync) and later steps forward, the timer fires
/// early, which only closes the window sooner.
pub fn arm_argv(ttl_secs: u64, realtime_now: Duration) -> Vec<String> {
    let delay = ttl_secs + FALLBACK_GRACE_SECS;
    vec![
        "systemd-run".to_string(),
        format!("--unit={FALLBACK_UNIT}"),
        format!("--on-active={delay}s"),
        format!("--timer-property=OnCalendar={}", utc_calendar(realtime_now.as_secs() + delay)),
        "--timer-property=AccuracySec=5s".to_string(),
        // Unload the timer once it has fired, so a later window can reuse the
        // unit name without a leftover elapsed timer.
        "--timer-property=RemainAfterElapse=no".to_string(),
        "--property=Type=oneshot".to_string(),
        "--property=CollectMode=inactive-or-failed".to_string(),
        FALLBACK_SCRIPT.to_string(),
    ]
}

/// Cancel: stop the timer (the service, if it is running right now, is left
/// to finish: it only closes what sysd is closing anyway).
pub fn stop_argv() -> Vec<String> {
    argv(&["systemctl", "stop", FALLBACK_TIMER])
}

pub fn is_active_argv() -> Vec<String> {
    argv(&["systemctl", "is-active", FALLBACK_TIMER])
}

/// `YYYY-MM-DD HH:MM:SS UTC` for a Unix time, the form systemd's calendar
/// parser accepts.
pub fn utc_calendar(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian
/// (H. Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_rendering_matches_known_dates() {
        assert_eq!(utc_calendar(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_calendar(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_calendar(1_760_600_000), "2025-10-16 07:33:20 UTC");
        assert_eq!(utc_calendar(1_798_761_599), "2026-12-31 23:59:59 UTC");
    }

    #[test]
    fn arm_argv_has_both_triggers_and_the_cleanup_properties() {
        let a = arm_argv(3600, Duration::from_secs(1_760_600_000));
        assert_eq!(a[0], "systemd-run");
        assert!(a.contains(&"--unit=duduclaw-maint-ssh-expire".to_string()));
        assert!(a.contains(&"--on-active=3660s".to_string()));
        assert!(a.contains(&"--timer-property=OnCalendar=2025-10-16 08:34:20 UTC".to_string()));
        assert!(a.contains(&"--property=CollectMode=inactive-or-failed".to_string()));
        assert!(a.contains(&"--property=Type=oneshot".to_string()));
        assert!(a.contains(&"--timer-property=AccuracySec=5s".to_string()));
        assert!(a.contains(&"--timer-property=RemainAfterElapse=no".to_string()));
        assert_eq!(a.last().unwrap(), FALLBACK_SCRIPT);
        assert_eq!(reset_failed_argv(), ["systemctl", "reset-failed", FALLBACK_TIMER, FALLBACK_SERVICE]);
        assert_eq!(stop_argv(), ["systemctl", "stop", FALLBACK_TIMER]);
    }
}
