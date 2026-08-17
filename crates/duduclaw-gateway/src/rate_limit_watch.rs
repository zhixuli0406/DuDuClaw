//! Quota early-warning telemetry from `claude` CLI `rate_limit_event` frames.
//!
//! The CLI emits these on the stream-json channel as an **advisory** — the
//! run continues and finishes normally:
//!
//! ```json
//! {"type":"rate_limit_event",
//!  "rate_limit_info":{"status":"allowed_warning","rateLimitType":"seven_day",
//!                     "utilization":0.92,"resetsAt":1787083200,
//!                     "isUsingOverage":false,"surpassedThreshold":0.75}}
//! ```
//!
//! Before 2026-08-17 (TODO-rate-limit-warning-misread-as-failure) both
//! stream parsers dropped the frame on the floor, AND the raw frame text
//! could reach failure-diagnostic strings, where `is_rate_limit_error`'s
//! substring match (`rateLimitType` → "ratelimittype" ⊃ "ratelimit")
//! classified a healthy account as rate-limited — cooling it down and
//! re-spending quota precisely when the subscription was near its ceiling.
//!
//! This module is the telemetry sink: parsers call [`record_frame`]; the
//! latest observation is readable via [`latest`] (surfaced on the
//! `system.status` RPC) and logged with a utilization-delta throttle so a
//! 92%-quota afternoon doesn't produce one warn line per dispatch.
//! It never influences success/failure classification — that is the point.

use serde::Serialize;
use std::sync::Mutex;

/// One observed quota advisory. `utilization` is 0.0–1.0 as reported.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaWarning {
    /// `allowed` / `allowed_warning` / whatever the CLI reports.
    pub status: String,
    /// e.g. `five_hour`, `seven_day`.
    pub rate_limit_type: String,
    pub utilization: f64,
    /// Unix seconds when the window resets, when reported.
    pub resets_at: Option<i64>,
    /// The threshold that was crossed, when reported.
    pub surpassed_threshold: Option<f64>,
    /// Unix seconds when we observed the frame.
    pub observed_at: i64,
}

static LATEST: Mutex<Option<QuotaWarning>> = Mutex::new(None);
/// Utilization (percent, rounded) at the last warn-level log, per process —
/// log again only when it moves by ≥1 point or the window type changes.
static LAST_LOGGED: Mutex<Option<(String, i64)>> = Mutex::new(None);

/// Whether a parsed stream-json event is a `rate_limit_event` frame.
pub fn is_rate_limit_frame(event: &serde_json::Value) -> bool {
    event.get("type").and_then(|t| t.as_str()) == Some("rate_limit_event")
}

/// Cheap line-level guard so failure diagnostics (`last_line` in
/// `StreamDiagnostics`) never embed the frame's raw text — that embedding is
/// exactly what turned an advisory into a misclassified rate-limit failure.
pub fn line_is_rate_limit_frame(line: &str) -> bool {
    line.contains("\"rate_limit_event\"")
}

/// Parse and record a `rate_limit_event` frame. Returns `true` when the
/// frame was recognized (callers use this only for tests — recording never
/// affects the run's outcome).
pub fn record_frame(event: &serde_json::Value) -> bool {
    if !is_rate_limit_frame(event) {
        return false;
    }
    let info = event.get("rate_limit_info").unwrap_or(event);
    let warning = QuotaWarning {
        status: info
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        rate_limit_type: info
            .get("rateLimitType")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        utilization: info
            .get("utilization")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        resets_at: info.get("resetsAt").and_then(|v| v.as_i64()),
        surpassed_threshold: info.get("surpassedThreshold").and_then(|v| v.as_f64()),
        observed_at: chrono::Utc::now().timestamp(),
    };

    let pct = (warning.utilization * 100.0).round() as i64;
    let should_log = {
        let mut last = LAST_LOGGED.lock().unwrap_or_else(|p| p.into_inner());
        let changed = last
            .as_ref()
            .is_none_or(|(t, p)| *t != warning.rate_limit_type || *p != pct);
        if changed {
            *last = Some((warning.rate_limit_type.clone(), pct));
        }
        changed
    };
    if should_log {
        tracing::warn!(
            rate_limit_type = %warning.rate_limit_type,
            status = %warning.status,
            utilization_pct = pct,
            resets_at = ?warning.resets_at,
            "CLI quota advisory (telemetry, NOT a failure): subscription window \
             utilization reported by claude CLI"
        );
    } else {
        tracing::debug!(
            rate_limit_type = %warning.rate_limit_type,
            utilization_pct = pct,
            "CLI quota advisory (unchanged)"
        );
    }

    *LATEST.lock().unwrap_or_else(|p| p.into_inner()) = Some(warning);
    true
}

/// The most recent quota advisory observed by any stream parser in this
/// process, if any. Surfaced on `system.status` for the dashboard.
pub fn latest() -> Option<QuotaWarning> {
    LATEST.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: &str = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"seven_day","utilization":0.92,"resetsAt":1787083200,"isUsingOverage":false,"surpassedThreshold":0.75}}"#;

    #[test]
    fn records_the_observed_frame_shape() {
        let event: serde_json::Value = serde_json::from_str(FRAME).unwrap();
        assert!(record_frame(&event));
        let latest = latest().expect("frame must be recorded");
        assert_eq!(latest.status, "allowed_warning");
        assert_eq!(latest.rate_limit_type, "seven_day");
        assert!((latest.utilization - 0.92).abs() < 1e-9);
        assert_eq!(latest.resets_at, Some(1787083200));
        assert_eq!(latest.surpassed_threshold, Some(0.75));
    }

    #[test]
    fn non_frame_events_are_ignored() {
        let event: serde_json::Value =
            serde_json::from_str(r#"{"type":"result","result":"PONG"}"#).unwrap();
        assert!(!record_frame(&event));
    }

    #[test]
    fn line_guard_matches_only_frame_lines() {
        assert!(line_is_rate_limit_frame(FRAME));
        assert!(!line_is_rate_limit_frame(r#"{"type":"result","result":"PONG"}"#));
    }
}
