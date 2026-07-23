//! Read-only "today's calendar events" via JXA (`osascript -l JavaScript`),
//! macOS only.
//!
//! Sensing priority (research doc §②-6, "structured API over pixels"): this
//! queries `Calendar.app`'s scripting bridge (EventKit-backed) for structured
//! event records — never a screenshot/OCR of a calendar view. The JXA script
//! itself is a **fixed literal** (no external/user data is interpolated into
//! it), so there is no script-injection surface; only the *output* (event
//! titles / calendar names, which originate from the user's own calendar data)
//! is treated as untrusted text and codepoint-truncated before it can reach a
//! prompt.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Hard cap on title / calendar-name fields (codepoints, CJK-safe) — same
/// budget as `frontmost::FrontmostInfo` and `notify_native`.
const MAX_FIELD_CHARS: usize = 200;

/// JXA script queries typically resolve fast against a local calendar store,
/// but a cold iCloud sync can stall — bounded generously per the P2-4 spec.
const CALENDAR_TIMEOUT: Duration = Duration::from_secs(10);

/// One calendar event, read-only, already field-truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalendarEvent {
    pub title: String,
    /// ISO-8601 start timestamp, as produced by JXA's `Date#toISOString()`.
    pub start: String,
    /// ISO-8601 end timestamp.
    pub end: String,
    /// Owning calendar's display name (e.g. "Work", "Family").
    pub calendar: String,
}

/// Raw shape returned by the JXA script, before field truncation.
#[derive(Debug, Deserialize)]
struct RawEvent {
    title: String,
    start: String,
    end: String,
    calendar: String,
}

/// Errors reading today's calendar events.
#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
    #[error("calendar reading is only supported on macOS")]
    Unsupported,
    /// Calendar automation permission (TCC) has not been granted. Recognized
    /// from a known `osascript`/EventKit error-text/code marker — see
    /// [`classify_stderr`]. `duduclaw os doctor` surfaces this with a zh-TW
    /// remediation pointer; callers must NOT attempt to bypass it.
    #[error("calendar automation permission denied: {0}")]
    PermissionDenied(String),
    #[error("calendar helper spawn failed: {0}")]
    Spawn(String),
    #[error("calendar helper timed out")]
    Timeout,
    #[error("calendar helper exited with status {code}: {stderr}")]
    Failed { code: i32, stderr: String },
    #[error("failed to parse calendar JSON output: {0}")]
    ParseError(String),
}

/// The JXA (JavaScript for Automation) program, run via `osascript -l
/// JavaScript -e <this>`. Fixed literal — see the module doc for why that
/// means there is no injection surface to sanitize on the way in.
///
/// Reads only `startDate`/`endDate`/`summary` off each calendar's `events`
/// collection, filtered to `[startOfDay, endOfDay)` for "today" in the local
/// timezone, and returns a JSON array via `JSON.stringify`. A calendar whose
/// `events.whose(...)` query throws (e.g. a delegated/unavailable calendar)
/// is skipped rather than failing the whole read.
const JXA_TODAY_EVENTS: &str = r#"
(function () {
  var Calendar = Application('Calendar');
  var now = new Date();
  var startOfDay = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 0, 0, 0);
  var endOfDay = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 0, 0, 0);
  var out = [];
  var cals = Calendar.calendars();
  for (var i = 0; i < cals.length; i++) {
    var cal = cals[i];
    var evts;
    try {
      evts = cal.events.whose({
        _and: [
          { startDate: { _greaterThanEquals: startOfDay } },
          { startDate: { _lessThan: endOfDay } }
        ]
      })();
    } catch (e) {
      continue;
    }
    for (var j = 0; j < evts.length; j++) {
      try {
        out.push({
          title: String(evts[j].summary()),
          start: evts[j].startDate().toISOString(),
          end: evts[j].endDate().toISOString(),
          calendar: String(cal.name())
        });
      } catch (e) {
        // Skip a single unreadable event rather than failing the batch.
      }
    }
  }
  return JSON.stringify(out);
})()
"#;

/// Read today's calendar events (local timezone), read-only.
pub async fn today_events() -> Result<Vec<CalendarEvent>, CalendarError> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err(CalendarError::Unsupported);
    }

    #[cfg(target_os = "macos")]
    {
        let fut = tokio::process::Command::new("osascript")
            .args(["-l", "JavaScript", "-e", JXA_TODAY_EVENTS])
            .output();
        let output = match tokio::time::timeout(CALENDAR_TIMEOUT, fut).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(CalendarError::Spawn(format!("osascript: {e}"))),
            Err(_) => return Err(CalendarError::Timeout),
        };
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr =
                duduclaw_core::truncate_chars(String::from_utf8_lossy(&output.stderr).trim(), 240);
            return Err(classify_stderr(code, stderr));
        }
        parse_calendar_json(&String::from_utf8_lossy(&output.stdout))
    }
}

/// Parse the JXA script's `JSON.stringify`d stdout into truncated
/// [`CalendarEvent`]s. Pure function — directly unit-testable with a canned
/// stdout sample, no live Calendar.app / macOS required.
fn parse_calendar_json(stdout: &str) -> Result<Vec<CalendarEvent>, CalendarError> {
    let raw: Vec<RawEvent> = serde_json::from_str(stdout.trim())
        .map_err(|e| CalendarError::ParseError(e.to_string()))?;
    Ok(raw
        .into_iter()
        .map(|r| CalendarEvent {
            title: duduclaw_core::truncate_chars(&r.title, MAX_FIELD_CHARS),
            start: r.start,
            end: r.end,
            calendar: duduclaw_core::truncate_chars(&r.calendar, MAX_FIELD_CHARS),
        })
        .collect())
}

/// Classify a failed helper's exit code + stderr into a [`CalendarError`],
/// recognizing the macOS Calendar automation-permission (TCC) denial. As with
/// `frontmost::classify_stderr`, this is a diagnostic/UX classification (which
/// hint text `duduclaw os doctor` shows), not a security gate, so a plain
/// case-insensitive substring match is acceptable here (project convention #2
/// governs security/routing decisions, not error-message triage).
fn classify_stderr(code: i32, stderr: String) -> CalendarError {
    let lower = stderr.to_ascii_lowercase();
    let is_permission_denied = lower.contains("not authorized")
        || lower.contains("-1743")
        || lower.contains("(-25211)")
        || lower.contains("calendar got an error");
    if is_permission_denied {
        CalendarError::PermissionDenied(stderr)
    } else {
        CalendarError::Failed { code, stderr }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_calendar_json_happy_path() {
        let stdout = r#"[
            {"title":"Standup","start":"2026-07-23T01:00:00.000Z","end":"2026-07-23T01:30:00.000Z","calendar":"Work"},
            {"title":"午餐","start":"2026-07-23T04:00:00.000Z","end":"2026-07-23T05:00:00.000Z","calendar":"個人"}
        ]"#;
        let events = parse_calendar_json(stdout).expect("should parse");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].title, "Standup");
        assert_eq!(events[0].calendar, "Work");
        assert_eq!(events[1].title, "午餐");
        assert_eq!(events[1].calendar, "個人");
    }

    #[test]
    fn parse_calendar_json_empty_array() {
        let events = parse_calendar_json("[]").expect("should parse");
        assert!(events.is_empty());
    }

    #[test]
    fn parse_calendar_json_truncates_long_cjk_titles() {
        let long_title: String = "測".repeat(250);
        let stdout = format!(
            r#"[{{"title":"{long_title}","start":"2026-07-23T00:00:00.000Z","end":"2026-07-23T01:00:00.000Z","calendar":"Work"}}]"#
        );
        let events = parse_calendar_json(&stdout).expect("should parse");
        assert_eq!(events[0].title.chars().count(), MAX_FIELD_CHARS);
    }

    #[test]
    fn parse_calendar_json_malformed_is_parse_error() {
        let r = parse_calendar_json("not json");
        assert!(matches!(r, Err(CalendarError::ParseError(_))));
    }

    #[test]
    fn classify_stderr_detects_tcc_denial_markers() {
        for sample in [
            "execution error: Calendar got an error: Not authorized to send Apple events to Calendar. (-1743)",
            "osascript error (-25211)",
        ] {
            let e = classify_stderr(1, sample.to_string());
            assert!(
                matches!(e, CalendarError::PermissionDenied(_)),
                "expected PermissionDenied for {sample:?}, got {e:?}"
            );
        }
    }

    #[test]
    fn classify_stderr_generic_failure_is_not_permission_denied() {
        let e = classify_stderr(1, "syntax error near token".into());
        assert!(matches!(e, CalendarError::Failed { code: 1, .. }));
    }
}
