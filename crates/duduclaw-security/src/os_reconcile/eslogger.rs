//! macOS `eslogger` observer (stage B of P3-3).
//!
//! `eslogger` (macOS 13+, ships in the base OS) streams Endpoint Security
//! events as JSON. This module splits into two clearly separated concerns:
//!
//!   - [`parse_eslogger_line`] — a **pure** function mapping one line of
//!     `eslogger` JSON to an [`OsEvent`]. Unit-tested against recorded fixtures,
//!     requires no privilege.
//!   - [`EsloggerObserver`] — the live subprocess driver. Actually spawning
//!     `eslogger exec file-write network` requires root / Full-Disk-Access, so
//!     the live capture is intentionally left unwired here (stage B follow-up)
//!     and [`ActionObserver::availability`] reports `Degraded`/`Unsupported`.
//!
//! # ⚠️ Schema provenance (NOT validated against real `eslogger` output)
//!
//! The JSON field paths below are derived from the Endpoint Security
//! `es_message_t` C API (`es_event_write_t { es_file_t *target }`, `es_file_t`
//! serializing with a `path`) and the field layout of the `tstromberg/esl` Go
//! structs (`time`, `process.audit_token.pid`, `event.open.file.path`). Apple
//! explicitly documents `eslogger` as unstable and NOT an API — the emitted
//! structure may change between releases. **These paths have NOT been checked
//! against live `eslogger` output; they must be corrected against a real
//! capture when stage B is wired.** In particular, Endpoint Security has no
//! stable *outbound network connect* NOTIFY event historically, so the
//! `NetConnect` parsing here is speculative and most likely needs replacement
//! with a Network Extension / socket-filter source.

use super::{ActionObserver, Availability, OsEvent, OsEventKind};

/// Parse one line of `eslogger` JSON into an [`OsEvent`].
///
/// Dispatches on the single key inside the top-level `event` object
/// (`write` / `create` / `open` / `exec` / `connect`). Returns `None` for
/// blank lines, malformed JSON, missing required fields (`time`,
/// `process.audit_token.pid`), or event types we do not reconcile.
///
/// See the module-level provenance note: field paths are API-derived, not
/// validated against a real capture.
pub fn parse_eslogger_line(line: &str) -> Option<OsEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;

    let ts = v.get("time").and_then(|t| t.as_str())?.to_string();
    let pid = v
        .get("process")
        .and_then(|p| p.get("audit_token"))
        .and_then(|a| a.get("pid"))
        .and_then(|p| p.as_u64())? as u32;

    let event = v.get("event").and_then(|e| e.as_object())?;
    // The `event` object carries exactly one key naming the event variant.
    let (variant, body) = event.iter().next()?;

    let (kind, path_or_endpoint) = match variant.as_str() {
        // ES_EVENT_TYPE_NOTIFY_WRITE: es_event_write_t { es_file_t *target }.
        "write" => {
            let path = body.get("target").and_then(|t| t.get("path")).and_then(|p| p.as_str())?;
            (OsEventKind::FileWrite, path.to_string())
        }
        // ES_EVENT_TYPE_NOTIFY_CREATE: destination path (best-effort; the union
        // shape varies, so try the common `new_path`/`file` layouts).
        "create" => {
            let path = create_dest_path(body)?;
            (OsEventKind::FileWrite, path)
        }
        // ES_EVENT_TYPE_NOTIFY_OPEN: es_event_open_t { es_file_t *file }.
        "open" => {
            let path = body.get("file").and_then(|f| f.get("path")).and_then(|p| p.as_str())?;
            (OsEventKind::FileRead, path.to_string())
        }
        // ES_EVENT_TYPE_NOTIFY_EXEC: es_event_exec_t { es_process_t *target }.
        "exec" => {
            let path = body
                .get("target")
                .and_then(|t| t.get("executable"))
                .and_then(|e| e.get("path"))
                .and_then(|p| p.as_str())?;
            (OsEventKind::ProcExec, path.to_string())
        }
        // Speculative: no stable ES outbound-connect NOTIFY event exists. Field
        // path is a guess — see module note.
        "connect" | "uipc_connect" => {
            let endpoint = connect_endpoint(body)?;
            (OsEventKind::NetConnect, endpoint)
        }
        _ => return None,
    };

    Some(OsEvent { kind, path_or_endpoint, pid, ts })
}

/// Best-effort extraction of a create event's destination path.
fn create_dest_path(body: &serde_json::Value) -> Option<String> {
    let dest = body.get("destination")?;
    // Union variant `new_path { dir, filename }` or `existing_file { path }`.
    if let Some(existing) =
        dest.get("existing_file").and_then(|f| f.get("path")).and_then(|p| p.as_str())
    {
        return Some(existing.to_string());
    }
    if let Some(new_path) = dest.get("new_path") {
        let dir = new_path
            .get("dir")
            .and_then(|d| d.get("path"))
            .and_then(|p| p.as_str())
            .unwrap_or("");
        let filename = new_path.get("filename").and_then(|f| f.as_str()).unwrap_or("");
        if dir.is_empty() && filename.is_empty() {
            return None;
        }
        return Some(format!("{}/{}", dir.trim_end_matches('/'), filename));
    }
    None
}

/// Best-effort extraction of a connect event's endpoint (speculative schema).
fn connect_endpoint(body: &serde_json::Value) -> Option<String> {
    // Try a few plausible shapes; fall back to any `address`/`path` string.
    if let Some(addr) = body.get("address").and_then(|a| a.as_str()) {
        return Some(addr.to_string());
    }
    if let Some(path) = body.get("path").and_then(|p| p.as_str()) {
        return Some(path.to_string());
    }
    None
}

/// Live macOS `eslogger` observer.
///
/// Constructing it is cheap and side-effect-free. Live capture is not wired in
/// stage B: [`ActionObserver::collect_since`] returns an explanatory error and
/// [`ActionObserver::availability`] reports the degraded/unsupported status so
/// callers fall back to a tool-calls-only self-consistency check.
#[derive(Debug, Default)]
pub struct EsloggerObserver;

impl EsloggerObserver {
    pub fn new() -> Self {
        Self
    }
}

impl ActionObserver for EsloggerObserver {
    fn collect_since(&self, _pid: u32, _since: &str) -> Result<Vec<OsEvent>, String> {
        Err(
            "eslogger live capture requires root/Full-Disk-Access and is not wired \
             (P3-3 stage B follow-up); the JSON parser is available via \
             parse_eslogger_line()"
                .to_string(),
        )
    }

    fn availability(&self) -> Availability {
        #[cfg(target_os = "macos")]
        {
            Availability::Degraded(
                "eslogger requires root/Full-Disk-Access; live capture not yet wired".to_string(),
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            Availability::Unsupported
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded-style fixture. Field paths are API-derived, NOT validated against
    // real eslogger output (see module note).
    fn write_fixture() -> &'static str {
        r#"{"schema_version":1,"time":"2026-07-06T12:00:00Z","event_type":6,"process":{"audit_token":{"pid":4321},"executable":{"path":"/usr/bin/claude"}},"event":{"write":{"target":{"path":"/etc/passwd"}}}}"#
    }

    #[test]
    fn parses_write_event() {
        let ev = parse_eslogger_line(write_fixture()).expect("should parse");
        assert_eq!(ev.kind, OsEventKind::FileWrite);
        assert_eq!(ev.path_or_endpoint, "/etc/passwd");
        assert_eq!(ev.pid, 4321);
        assert_eq!(ev.ts, "2026-07-06T12:00:00Z");
    }

    #[test]
    fn parses_exec_event() {
        let line = r#"{"time":"2026-07-06T12:00:01Z","process":{"audit_token":{"pid":10}},"event":{"exec":{"target":{"executable":{"path":"/bin/sh"}}}}}"#;
        let ev = parse_eslogger_line(line).unwrap();
        assert_eq!(ev.kind, OsEventKind::ProcExec);
        assert_eq!(ev.path_or_endpoint, "/bin/sh");
        assert_eq!(ev.pid, 10);
    }

    #[test]
    fn parses_open_as_file_read() {
        let line = r#"{"time":"2026-07-06T12:00:02Z","process":{"audit_token":{"pid":11}},"event":{"open":{"file":{"path":"/tmp/secret"}}}}"#;
        let ev = parse_eslogger_line(line).unwrap();
        assert_eq!(ev.kind, OsEventKind::FileRead);
        assert_eq!(ev.path_or_endpoint, "/tmp/secret");
    }

    #[test]
    fn blank_and_malformed_lines_return_none() {
        assert!(parse_eslogger_line("").is_none());
        assert!(parse_eslogger_line("   ").is_none());
        assert!(parse_eslogger_line("{not json").is_none());
        assert!(parse_eslogger_line("{}").is_none());
    }

    #[test]
    fn missing_pid_returns_none() {
        let line = r#"{"time":"2026-07-06T12:00:00Z","process":{"audit_token":{}},"event":{"write":{"target":{"path":"/x"}}}}"#;
        assert!(parse_eslogger_line(line).is_none());
    }

    #[test]
    fn unknown_event_variant_returns_none() {
        let line = r#"{"time":"2026-07-06T12:00:00Z","process":{"audit_token":{"pid":1}},"event":{"lookup":{"x":1}}}"#;
        assert!(parse_eslogger_line(line).is_none());
    }

    #[test]
    fn availability_is_not_enforcing() {
        // Whether Degraded (macOS) or Unsupported (elsewhere), it is never
        // Enforcing until live capture is wired.
        assert_ne!(EsloggerObserver::new().availability(), Availability::Enforcing);
    }

    #[test]
    fn collect_since_is_not_wired() {
        assert!(EsloggerObserver::new().collect_since(1, "2026-07-06T12:00:00Z").is_err());
    }
}
