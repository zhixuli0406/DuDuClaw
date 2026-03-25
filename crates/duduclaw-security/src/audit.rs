//! Security audit event log — append-only JSONL file.
//!
//! [C-2b] All security events (drift, injection, quarantine) are persisted
//! to `~/.duduclaw/security_audit.jsonl` for forensic review.

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Severity level of a security event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// A single security audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: String,
    pub event_type: String,
    pub agent_id: String,
    pub severity: Severity,
    pub details: serde_json::Value,
}

impl AuditEvent {
    /// Create a new audit event with the current timestamp.
    pub fn new(
        event_type: impl Into<String>,
        agent_id: impl Into<String>,
        severity: Severity,
        details: serde_json::Value,
    ) -> Self {
        Self {
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.into(),
            agent_id: agent_id.into(),
            severity,
            details,
        }
    }
}

/// Append an audit event to the security log file.
///
/// The log is stored at `<home_dir>/security_audit.jsonl`.
/// This function is synchronous (blocking I/O) and suitable for
/// calling from both sync and async contexts via `spawn_blocking`.
pub fn append_audit_event(home_dir: &Path, event: &AuditEvent) {
    let path = home_dir.join("security_audit.jsonl");
    let json = match serde_json::to_string(event) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize audit event: {e}");
            return;
        }
    };

    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            // Use advisory file lock to prevent multi-process write corruption (MW-H2)
            #[cfg(unix)]
            {
                use std::os::unix::io::AsRawFd;
                if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
                    warn!("flock failed on audit log: {}", std::io::Error::last_os_error());
                }
            }
            if let Err(e) = writeln!(f, "{json}") {
                warn!("Failed to write audit event: {e}");
            }
            // Lock automatically released when file is dropped
        }
        Err(e) => {
            warn!("Failed to open audit log {}: {e}", path.display());
        }
    }
}

/// Read recent audit events (last N entries).
///
/// Simplified: collect all lines, then slice the tail (MW-L2).
/// For very large files, consider using a reverse-line reader crate.
pub fn read_recent_events(home_dir: &Path, limit: usize) -> Vec<AuditEvent> {
    let path = home_dir.join("security_audit.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(limit);

    lines[start..]
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Count events by severity since a given timestamp.
///
/// Uses proper ISO 8601 DateTime parsing instead of string prefix
/// comparison to avoid incorrect ordering (MW-M3).
pub fn count_events_since(
    home_dir: &Path,
    since: &str,
) -> (usize, usize, usize) {
    let path = home_dir.join("security_audit.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return (0, 0, 0),
    };

    let since_dt = chrono::DateTime::parse_from_rfc3339(since)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now() - chrono::Duration::hours(24));

    let mut info = 0usize;
    let mut warning = 0usize;
    let mut critical = 0usize;

    for line in content.lines() {
        if let Ok(event) = serde_json::from_str::<AuditEvent>(line) {
            let event_time = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .ok();
            if event_time.is_some_and(|t| t >= since_dt) {
                match event.severity {
                    Severity::Info => info += 1,
                    Severity::Warning => warning += 1,
                    Severity::Critical => critical += 1,
                }
            }
        }
    }

    (info, warning, critical)
}

// ── Convenience constructors for common events ──────────────

/// Log a SOUL.md drift detection event.
pub fn log_soul_drift(home_dir: &Path, agent_id: &str, expected: &str, actual: &str) {
    let event = AuditEvent::new(
        "soul_drift",
        agent_id,
        Severity::Critical,
        serde_json::json!({
            "expected_hash": expected,
            "actual_hash": actual,
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log a prompt injection detection event.
pub fn log_injection_detected(
    home_dir: &Path,
    agent_id: &str,
    risk_score: u32,
    matched_rules: &[String],
    blocked: bool,
) {
    let severity = if blocked {
        Severity::Critical
    } else {
        Severity::Warning
    };
    let event = AuditEvent::new(
        "prompt_injection",
        agent_id,
        severity,
        serde_json::json!({
            "risk_score": risk_score,
            "matched_rules": matched_rules,
            "blocked": blocked,
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log a skill quarantine event.
pub fn log_skill_quarantined(home_dir: &Path, agent_id: &str, skill_name: &str, reason: &str) {
    let event = AuditEvent::new(
        "skill_quarantined",
        agent_id,
        Severity::Warning,
        serde_json::json!({
            "skill_name": skill_name,
            "reason": reason,
        }),
    );
    append_audit_event(home_dir, &event);
}
