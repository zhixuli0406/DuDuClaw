//! Unified audit export + SIEM/webhook sink.
//!
//! DuDuClaw already writes several append-only audit trails
//! (`security_audit.jsonl`, `tool_calls.jsonl`, `channel_failures.jsonl`,
//! `budget_events.jsonl`). Entering a small team / compliance context, the first
//! ask is a *single* structured export that can be streamed to a SIEM. This
//! module does exactly that — it does NOT invent a new source of truth; it
//! aggregates the existing files into a normalized record stream and can POST
//! them to a webhook (Splunk HEC / Elastic / Datadog / generic).
//!
//! Deliberately source-agnostic: each JSONL line is parsed as generic JSON and a
//! best-effort `timestamp` / `agent_id` is lifted out, so new audit files show
//! up in the export by adding one row to [`JSONL_SOURCES`] — no per-schema code.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// The append-only JSONL audit files this exporter aggregates, with the logical
/// `source` label each line is tagged with.
pub const JSONL_SOURCES: &[(&str, &str)] = &[
    ("security_audit.jsonl", "security_audit"),
    ("tool_calls.jsonl", "tool_calls"),
    ("channel_failures.jsonl", "channel_failures"),
    ("budget_events.jsonl", "budget_events"),
];

/// Candidate keys a timestamp might live under, in priority order.
const TS_KEYS: &[&str] = &["timestamp", "ts", "created_at", "time", "at"];
/// Candidate keys an agent id might live under.
const AGENT_KEYS: &[&str] = &["agent_id", "agent", "agent_name"];

/// One normalized audit record.
#[derive(Debug, Clone, Serialize)]
pub struct AuditRecord {
    /// Best-effort RFC3339 timestamp lifted from the line (empty if none).
    pub timestamp: String,
    /// Logical source label (e.g. `"tool_calls"`).
    pub source: String,
    /// Best-effort agent id lifted from the line.
    pub agent_id: Option<String>,
    /// The original JSON line, verbatim.
    pub record: serde_json::Value,
}

fn lift_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Parse an RFC3339 timestamp string, if present and valid.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Read one JSONL audit file into normalized records, filtered to those at or
/// after `since` (when a parseable timestamp exists; records without a
/// timestamp are always included so nothing is silently dropped). Missing file
/// ⇒ empty. Malformed lines are skipped (not fatal).
pub fn read_source(
    home_dir: &Path,
    file: &str,
    source: &str,
    since: Option<DateTime<Utc>>,
) -> Vec<AuditRecord> {
    use std::io::BufRead;
    let path = home_dir.join(file);
    // Stream line-by-line: audit logs can grow to GBs; never load the whole
    // file into memory (OOM risk on large / long-lived deployments).
    let handle = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = std::io::BufReader::new(handle);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ts = lift_str(&value, TS_KEYS).unwrap_or_default();
        if let (Some(since), Some(rec_ts)) = (since, parse_ts(&ts)) {
            if rec_ts < since {
                continue;
            }
        }
        let agent_id = lift_str(&value, AGENT_KEYS);
        out.push(AuditRecord {
            timestamp: ts,
            source: source.to_string(),
            agent_id,
            record: value,
        });
    }
    out
}

/// Collect + normalize every configured audit source, sorted by ABSOLUTE time
/// ascending. Timestamps are compared as parsed UTC instants (NOT as strings) so
/// records with different RFC3339 offsets order correctly; records without a
/// parseable timestamp sort first, then by raw string for stability.
pub fn collect_records(home_dir: &Path, since: Option<DateTime<Utc>>) -> Vec<AuditRecord> {
    let mut all: Vec<AuditRecord> = Vec::new();
    for (file, source) in JSONL_SOURCES {
        all.extend(read_source(home_dir, file, source, since));
    }
    all.sort_by(|a, b| {
        match (parse_ts(&a.timestamp), parse_ts(&b.timestamp)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (None, Some(_)) => std::cmp::Ordering::Less, // timestampless first
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, None) => a.timestamp.cmp(&b.timestamp),
        }
    });
    all
}

/// Serialize records as newline-delimited JSON (one record object per line) —
/// the format most SIEM ingest endpoints expect for bulk push. A record that
/// somehow fails to serialize is logged loudly (audit integrity — never a silent
/// drop) rather than vanishing.
pub fn to_ndjson(records: &[AuditRecord]) -> String {
    let mut out = String::new();
    for r in records {
        match serde_json::to_string(r) {
            Ok(line) => {
                out.push_str(&line);
                out.push('\n');
            }
            Err(e) => tracing::error!(
                source = %r.source,
                "audit record failed to serialize — OMITTED from export: {e}"
            ),
        }
    }
    out
}

/// Wire format for a SIEM push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiemFormat {
    /// One JSON object per line (Splunk HEC raw, Elastic bulk-ish, Datadog).
    Ndjson,
    /// A single JSON array of records.
    JsonArray,
}

/// A configured webhook/SIEM destination.
#[derive(Debug, Clone)]
pub struct SiemSink {
    pub url: String,
    /// Optional `(header_name, header_value)` for auth, e.g.
    /// `("Authorization", "Splunk <token>")` or `("Authorization", "Bearer …")`.
    pub auth_header: Option<(String, String)>,
    pub format: SiemFormat,
}

impl SiemSink {
    /// Serialize `records` in the sink's wire format.
    pub fn body(&self, records: &[AuditRecord]) -> String {
        match self.format {
            SiemFormat::Ndjson => to_ndjson(records),
            SiemFormat::JsonArray => {
                serde_json::to_string(records).unwrap_or_else(|_| "[]".to_string())
            }
        }
    }

    /// POST the records to the sink. Returns the HTTP status on success.
    /// No-op (returns `Ok(0)`) when there are no records.
    pub async fn send(
        &self,
        http: &reqwest::Client,
        records: &[AuditRecord],
    ) -> Result<u16, String> {
        if records.is_empty() {
            return Ok(0);
        }
        let body = self.body(records);
        let content_type = match self.format {
            SiemFormat::Ndjson => "application/x-ndjson",
            SiemFormat::JsonArray => "application/json",
        };
        let mut req = http
            .post(&self.url)
            .header("Content-Type", content_type)
            .body(body);
        if let Some((name, value)) = &self.auth_header {
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req.send().await.map_err(|e| format!("SIEM POST failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("SIEM sink returned HTTP {status}"));
        }
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(home: &Path, file: &str, lines: &[&str]) {
        std::fs::write(home.join(file), lines.join("\n")).unwrap();
    }

    #[test]
    fn collects_and_normalizes_across_sources() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        write(
            home,
            "tool_calls.jsonl",
            &[r#"{"timestamp":"2026-07-08T10:00:00Z","agent_id":"alice","tool_name":"memory_search","success":true}"#],
        );
        write(
            home,
            "budget_events.jsonl",
            &[r#"{"ts":"2026-07-08T09:00:00Z","agent_id":"bob","event":"budget_breaker_open","scope":"daily"}"#],
        );
        let recs = collect_records(home, None);
        assert_eq!(recs.len(), 2);
        // Sorted ascending by timestamp: bob (09:00) before alice (10:00).
        assert_eq!(recs[0].agent_id.as_deref(), Some("bob"));
        assert_eq!(recs[0].source, "budget_events");
        assert_eq!(recs[1].agent_id.as_deref(), Some("alice"));
        assert_eq!(recs[1].source, "tool_calls");
    }

    #[test]
    fn since_filter_drops_older_but_keeps_timestampless() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        write(
            home,
            "security_audit.jsonl",
            &[
                r#"{"timestamp":"2026-07-01T00:00:00Z","agent":"old"}"#,
                r#"{"timestamp":"2026-07-08T00:00:00Z","agent":"new"}"#,
                r#"{"kind":"no_timestamp","agent":"keep"}"#,
            ],
        );
        let since = parse_ts("2026-07-05T00:00:00Z");
        let recs = read_source(home, "security_audit.jsonl", "security_audit", since);
        let agents: Vec<_> = recs.iter().filter_map(|r| r.agent_id.clone()).collect();
        assert!(agents.contains(&"new".to_string()));
        assert!(agents.contains(&"keep".to_string()), "no-ts record kept");
        assert!(!agents.contains(&"old".to_string()), "older record filtered");
    }

    #[test]
    fn malformed_lines_skipped_missing_file_empty() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        write(home, "tool_calls.jsonl", &["not json", "", r#"{"ts":"x","a":1}"#]);
        let recs = read_source(home, "tool_calls.jsonl", "tool_calls", None);
        assert_eq!(recs.len(), 1, "only the valid JSON line survives");
        // Missing file → empty, no panic.
        assert!(read_source(home, "nope.jsonl", "nope", None).is_empty());
    }

    #[test]
    fn sorts_by_absolute_time_across_offsets() {
        // Two records whose STRING order disagrees with their absolute-time
        // order: "2026-07-08T01:00:00+10:00" (2026-07-07 15:00 UTC) is EARLIER
        // than "2026-07-07T20:00:00Z" despite the later date string.
        let dir = tempdir().unwrap();
        let home = dir.path();
        write(
            home,
            "tool_calls.jsonl",
            &[
                r#"{"timestamp":"2026-07-08T01:00:00+10:00","agent_id":"earlier"}"#,
                r#"{"timestamp":"2026-07-07T20:00:00Z","agent_id":"later"}"#,
            ],
        );
        let recs = collect_records(home, None);
        assert_eq!(recs.len(), 2);
        assert_eq!(
            recs[0].agent_id.as_deref(),
            Some("earlier"),
            "must sort by absolute UTC instant, not timestamp string"
        );
        assert_eq!(recs[1].agent_id.as_deref(), Some("later"));
    }

    #[test]
    fn ndjson_and_array_bodies() {
        let recs = vec![AuditRecord {
            timestamp: "2026-07-08T00:00:00Z".into(),
            source: "tool_calls".into(),
            agent_id: Some("a".into()),
            record: serde_json::json!({"x": 1}),
        }];
        let nd = SiemSink {
            url: "http://x".into(),
            auth_header: None,
            format: SiemFormat::Ndjson,
        }
        .body(&recs);
        assert_eq!(nd.lines().count(), 1);
        assert!(nd.ends_with('\n'));

        let arr = SiemSink {
            url: "http://x".into(),
            auth_header: None,
            format: SiemFormat::JsonArray,
        }
        .body(&recs);
        assert!(arr.starts_with('[') && arr.ends_with(']'));
    }

    #[tokio::test]
    async fn send_empty_is_noop() {
        let sink = SiemSink {
            url: "http://127.0.0.1:1/never".into(),
            auth_header: None,
            format: SiemFormat::Ndjson,
        };
        let http = reqwest::Client::new();
        // No records → no request attempted (would otherwise fail to connect).
        assert_eq!(sink.send(&http, &[]).await.unwrap(), 0);
    }
}
