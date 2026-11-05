//! Bounded, read-only projections of existing evidence; never infer a sent message.
use super::{Report, Request, Row};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

const MAX_SCAN_BYTES: u64 = 128 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_ROWS: usize = 10_000;
const UNREADABLE: &str = "COMPLIANCE_SOURCE_UNREADABLE";
const TOO_LARGE: &str = "COMPLIANCE_TOO_LARGE";

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn row() -> Row {
    Row { time: String::new(), employee: String::new(), destination: String::new(),
        categories: String::new(), decision: String::new(), approver: String::new(),
        reason: String::new(), kind: String::new(), requester: String::new(), id: String::new() }
}

fn in_range(stamp: &str, request: &Request) -> Result<bool, String> {
    let time = DateTime::parse_from_rfc3339(stamp).map_err(|_| UNREADABLE.to_string())?
        .with_timezone(&Utc);
    Ok(time >= request.from && time < request.until)
}

fn utc_time(stamp: &str) -> Result<String, String> {
    Ok(DateTime::parse_from_rfc3339(stamp).map_err(|_| UNREADABLE.to_string())?
        .with_timezone(&Utc).to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
}

fn categories(value: &Value) -> String {
    value.as_array().map(|values| values.iter().filter_map(Value::as_str)
        .collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

// The approval's notification channel is unrelated to the outbound destination.
// Keep only explicit sink metadata; never export a URL's credentials/path/query.
fn destination(value: &Value) -> String {
    let args = value.get("arguments").unwrap_or(value);
    let channel = text(args, "channel");
    let mut host = String::new();
    for key in ["url", "host"] {
        if let Some(raw) = args.get(key).and_then(Value::as_str) {
            if let Ok(url) = url::Url::parse(raw) {
                host = url.host_str().unwrap_or("").to_string();
                break;
            }
            if key == "host" && !raw.is_empty() && raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')) {
                host = raw.into();
                break;
            }
        }
    }
    [channel, host].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ")
}

fn existing_regular(path: &Path) -> Result<Option<std::fs::Metadata>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(Some(meta)),
        Ok(_) => Err(UNREADABLE.into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(UNREADABLE.into()),
    }
}

fn add_bytes(scanned: &mut u64, bytes: u64) -> Result<(), String> {
    *scanned = scanned.checked_add(bytes).ok_or_else(|| TOO_LARGE.to_string())?;
    if *scanned > MAX_SCAN_BYTES { return Err(TOO_LARGE.into()); }
    Ok(())
}

fn check_rows(report: &Report) -> Result<(), String> {
    if report.outbound.len() + report.approvals.len() + report.accesses.len() + report.reviews.len() > MAX_ROWS {
        return Err(TOO_LARGE.into());
    }
    Ok(())
}

fn approvals(home: &Path, request: &Request, report: &mut Report, scanned: &mut u64) -> Result<(), String> {
    let path = home.join("approvals.db");
    let Some(meta) = existing_regular(&path)? else {
        report.notes.push("approvals_missing".into());
        return Ok(());
    };
    add_bytes(scanned, meta.len())?;
    for suffix in ["approvals.db-wal", "approvals.db-shm"] {
        if let Some(meta) = existing_regular(&home.join(suffix))? { add_bytes(scanned, meta.len())?; }
    }
    let conn = Connection::open_with_flags(&path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW)
        .map_err(|_| UNREADABLE.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(2)).map_err(|_| UNREADABLE.to_string())?;
    // A corrupt timestamp must not silently disappear through SQL's NULL comparisons.
    let invalid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM approvals WHERE julianday(created_at) IS NULL OR (decided_at IS NOT NULL AND julianday(decided_at) IS NULL))", [], |r| r.get(0))
        .map_err(|_| UNREADABLE.to_string())?;
    if invalid { return Err(UNREADABLE.into()); }
    let mut stmt = conn.prepare("SELECT id,agent_id,action_kind,status,created_at,decided_at,decided_by,payload,invalidated_reason,on_behalf_of FROM approvals WHERE (julianday(created_at)>=julianday(?1) AND julianday(created_at)<julianday(?2)) OR (julianday(decided_at)>=julianday(?1) AND julianday(decided_at)<julianday(?2)) ORDER BY created_at,id LIMIT 10001")
        .map_err(|_| UNREADABLE.to_string())?;
    let mut rows = stmt.query(params![request.from.to_rfc3339(), request.until.to_rfc3339()])
        .map_err(|_| UNREADABLE.to_string())?;
    while let Some(record) = rows.next().map_err(|_| UNREADABLE.to_string())? {
        let mut projected = row();
        projected.id = record.get(0).map_err(|_| UNREADABLE.to_string())?;
        projected.employee = record.get(1).map_err(|_| UNREADABLE.to_string())?;
        projected.requester = record.get::<_, Option<String>>(9).map_err(|_| UNREADABLE.to_string())?.unwrap_or_default();
        projected.kind = record.get(2).map_err(|_| UNREADABLE.to_string())?;
        projected.decision = record.get(3).map_err(|_| UNREADABLE.to_string())?;
        let created: String = record.get(4).map_err(|_| UNREADABLE.to_string())?;
        let decided: Option<String> = record.get(5).map_err(|_| UNREADABLE.to_string())?;
        let decision_in_range = decided.as_deref().map(|stamp| in_range(stamp, request)).transpose()?.unwrap_or(false);
        projected.time = utc_time(if decision_in_range { decided.as_deref().unwrap_or(&created) } else { &created })?;
        if decision_in_range {
            projected.approver = record.get::<_, Option<String>>(6).map_err(|_| UNREADABLE.to_string())?.unwrap_or_default();
            projected.reason = record.get::<_, Option<String>>(8).map_err(|_| UNREADABLE.to_string())?.unwrap_or_default();
            if !report.notes.iter().any(|note| note == "approval_status_snapshot") {
                report.notes.push("approval_status_snapshot".into());
            }
        } else {
            // A later decision must not be presented as having happened at creation.
            projected.decision = if decided.is_some() { "decision_outside_range".into() }
                else if projected.decision == "pending" { "pending".into() }
                else { "decision_time_unrecorded".into() };
        }
        let payload: String = record.get(7).map_err(|_| UNREADABLE.to_string())?;
        if payload.len() > MAX_LINE_BYTES { return Err(TOO_LARGE.into()); }
        let payload: Value = serde_json::from_str(&payload).map_err(|_| UNREADABLE.to_string())?;
        if projected.kind == "cli_egress" || payload.get("egress_guard").is_some() {
            let mut outbound = projected.clone();
            outbound.destination = destination(&payload);
            outbound.categories = categories(&payload["egress_guard"]["sources"]);
            if outbound.reason.is_empty() { outbound.reason = text(&payload["egress_guard"], "reason"); }
            report.outbound.push(outbound);
        }
        report.approvals.push(projected);
        check_rows(report)?;
    }
    Ok(())
}

fn tool_record(value: &Value, report: &mut Report) {
    let mut projected = row();
    projected.time = text(value, "timestamp");
    projected.employee = text(value, "agent_id");
    projected.kind = text(value, "tool_name");
    projected.id = text(value, "call_id");
    projected.categories = categories(&value["sources"]);
    if value.get("egress_guard").is_some() || value.get("error_class").and_then(Value::as_str) == Some("egress_guard") {
        let mut outbound = projected.clone();
        outbound.decision = text(value, "egress_guard");
        if outbound.decision.is_empty() { outbound.decision = "denied".into(); }
        report.outbound.push(outbound);
    }
    if value.get("action_review").is_some() {
        projected.decision = text(value, "action_review");
        projected.reason = text(value, "action_review_mode");
        report.reviews.push(projected.clone());
    }
    if matches!(projected.kind.as_str(), "db_select" | "db_query" | "db_tables") {
        projected.decision = match value.get("success").and_then(Value::as_bool) {
            Some(true) => "recorded_success", Some(false) => "recorded_failure", None => "",
        }.into();
        projected.reason.clear();
        report.accesses.push(projected);
    }
}

fn redaction_record(value: &Value, report: &mut Report) {
    let mut projected = row();
    projected.time = text(value, "ts");
    projected.employee = text(value, "agent_id");
    projected.kind = text(value, "event");
    match projected.kind.as_str() {
        "redact" => {
            projected.categories = text(value, "category");
            projected.destination = text(value, "source_category");
            projected.decision = "redacted".into();
            report.accesses.push(projected);
        }
        "egress_allow" | "egress_deny" => {
            projected.decision = projected.kind.clone();
            projected.kind = text(value, "tool");
            projected.reason = text(value, "reason");
            report.outbound.push(projected);
        }
        _ => (),
    }
}

fn jsonl(path: &Path, request: &Request, report: &mut Report, scanned: &mut u64, redaction: bool) -> Result<bool, String> {
    let Some(meta) = existing_regular(path)? else { return Ok(false); };
    if meta.len() > MAX_SCAN_BYTES.saturating_sub(*scanned) { return Err(TOO_LARGE.into()); }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| UNREADABLE.to_string())?;
    if !file.metadata().map_err(|_| UNREADABLE.to_string())?.is_file() { return Err(UNREADABLE.into()); }
    // Capture once under the family lock; verify and project these exact bytes.
    let mut snapshot = Vec::new();
    file.take(MAX_SCAN_BYTES.saturating_sub(*scanned) + 1).read_to_end(&mut snapshot)
        .map_err(|_| UNREADABLE.to_string())?;
    if snapshot.len() as u64 > MAX_SCAN_BYTES.saturating_sub(*scanned) { return Err(TOO_LARGE.into()); }
    // Check line bounds before the verifier allocates parsed JSON objects.
    if snapshot.split_inclusive(|b| *b == b'\n').any(|line| line.len() > MAX_LINE_BYTES) {
        return Err(TOO_LARGE.into());
    }
    if !redaction {
        let content = std::str::from_utf8(&snapshot).map_err(|_| UNREADABLE.to_string())?;
        if !duduclaw_security::audit_chain::verify_chain_content(content).is_intact() {
            return Err(UNREADABLE.into());
        }
    }
    let mut reader = BufReader::new(snapshot.as_slice());
    loop {
        let mut bytes = Vec::new();
        let count = Read::by_ref(&mut reader).take((MAX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes).map_err(|_| UNREADABLE.to_string())?;
        if count == 0 { break; }
        if count > MAX_LINE_BYTES { return Err(TOO_LARGE.into()); }
        add_bytes(scanned, count as u64)?;
        if bytes.iter().all(u8::is_ascii_whitespace) { continue; }
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| UNREADABLE.to_string())?;
        if !value.is_object() { return Err(UNREADABLE.into()); }
        let stamp = text(&value, if redaction { "ts" } else { "timestamp" });
        if !in_range(&stamp, request)? { continue; }
        value[if redaction { "ts" } else { "timestamp" }] = utc_time(&stamp)?.into();
        if redaction { redaction_record(&value, report); } else { tool_record(&value, report); }
        check_rows(report)?;
    }
    Ok(true)
}

// Both generations are read while writers cannot rotate or append. The lock
// key is always the live filename, never an inode that rotation can replace.
fn jsonl_family(path: &Path, backup: &Path, request: &Request, report: &mut Report,
    scanned: &mut u64, redaction: bool) -> Result<bool, String> {
    jsonl_family_between(path, backup, request, report, scanned, redaction, || {})
}

fn jsonl_family_between(path: &Path, backup: &Path, request: &Request, report: &mut Report,
    scanned: &mut u64, redaction: bool, between: impl FnOnce()) -> Result<bool, String> {
    // Do not create an unused source directory just to acquire its lock.
    if let Some(parent) = path.parent() {
        match std::fs::symlink_metadata(parent) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            _ => return Err(UNREADABLE.into()),
        }
    }
    duduclaw_core::with_file_lock(path, || {
        Ok((|| {
            let old = jsonl(backup, request, report, scanned, redaction)?;
            between();
            let current = jsonl(path, request, report, scanned, redaction)?;
            Ok(old || current)
        })())
    }).map_err(|_| UNREADABLE.to_string())?
}

pub(super) fn collect(home: &Path, request: &Request) -> Result<Report, String> {
    let home = std::fs::canonicalize(home).map_err(|_| UNREADABLE.to_string())?;
    let mut report = Report::default();
    let mut scanned = 0;
    approvals(&home, request, &mut report, &mut scanned)?;
    let tools_found = jsonl_family(&home.join("tool_calls.jsonl"), &home.join("tool_calls.jsonl.old"),
        request, &mut report, &mut scanned, false)?;
    if !tools_found { report.notes.push("tool_audit_missing".into()); }
    let redaction = home.join("redaction");
    if std::fs::symlink_metadata(&redaction).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(UNREADABLE.into());
    }
    let redaction_found = jsonl_family(&redaction.join("audit.jsonl"), &redaction.join("audit.jsonl.1"),
        request, &mut report, &mut scanned, true)?;
    if !redaction_found { report.notes.push("redaction_audit_missing".into()); }
    for rows in [&mut report.outbound, &mut report.approvals, &mut report.accesses, &mut report.reviews] {
        rows.sort_by(|a, b| DateTime::parse_from_rfc3339(&a.time).ok()
            .cmp(&DateTime::parse_from_rfc3339(&b.time).ok()).then(a.id.cmp(&b.id)));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Request {
        serde_json::from_value(serde_json::json!({
            "from":"2026-11-01T00:00:00Z", "until":"2026-11-02T00:00:00Z",
            "format":"csv", "language":"zh-TW"
        })).unwrap()
    }

    #[test]
    fn compliance_sources_verify_both_chain_segments_even_outside_range() {
        use duduclaw_security::audit_chain::append_chained_line;
        for name in ["tool_calls.jsonl.old", "tool_calls.jsonl"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(name);
            for decision in ["allow", "block"] {
                append_chained_line(&path, serde_json::json!({
                    "timestamp":"2026-10-01T00:00:00Z", "action_review":decision
                }).as_object().unwrap().clone(), None).unwrap();
            }
            assert!(collect(dir.path(), &request()).is_ok());
            let body = std::fs::read_to_string(&path).unwrap();
            let tampered = body.replacen("allow", "other", 1);
            std::fs::write(&path, tampered).unwrap();
            assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
            let mut invalid_utf8 = body.as_bytes().to_vec();
            invalid_utf8.push(0xff);
            std::fs::write(&path, invalid_utf8).unwrap();
            assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
            for tail in ["{broken", "null", r#"{"timestamp":"2026-11-01T00:00:00Z"}"#,
                r#"{"_prev_hash":false}"#, r#"{"_prev_hash":"wrong"}"#] {
                std::fs::write(&path, format!("{body}{tail}\n")).unwrap();
                assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
            }
        }
    }

    #[test]
    fn compliance_sources_independent_genesis_segments_and_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["tool_calls.jsonl.old", "tool_calls.jsonl"] {
            let path = dir.path().join(name);
            duduclaw_security::audit_chain::append_chained_line(&path, serde_json::json!({
                "timestamp":"2026-11-01T00:00:00Z", "action_review":"allow"
            }).as_object().unwrap().clone(), None).unwrap();
        }
        assert_eq!(collect(dir.path(), &request()).unwrap().reviews.len(), 2);
        std::fs::remove_file(dir.path().join("tool_calls.jsonl.old")).unwrap();
        std::fs::create_dir(dir.path().join("tool_calls.jsonl.old")).unwrap();
        assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
        std::fs::remove_dir(dir.path().join("tool_calls.jsonl.old")).unwrap();
        std::fs::write(dir.path().join("redaction"), "not a directory").unwrap();
        assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
    }

    #[test]
    fn compliance_sources_rotation_between_generations_cannot_omit_live_rows() {
        for redaction in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (live, old, record) = if redaction {
                ("audit.jsonl", "audit.jsonl.1", "{\"ts\":\"2026-11-01T00:00:00Z\",\"event\":\"egress_deny\"}\n")
            } else {
                ("tool_calls.jsonl", "tool_calls.jsonl.old", "{\"timestamp\":\"2026-11-01T00:00:00Z\",\"action_review\":\"block\"}\n")
            };
            let path = dir.path().join(live);
            let backup = dir.path().join(old);
            std::fs::write(&path, record).unwrap();
            std::fs::write(&backup, record).unwrap();
            let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let (go_tx, go_rx) = std::sync::mpsc::channel();
            let mut report = Report::default();
            std::thread::scope(|scope| {
                let path = &path;
                let backup = &backup;
                let writer = scope.spawn(move || {
                    go_rx.recv().unwrap();
                    attempt_tx.send(()).unwrap();
                    duduclaw_core::with_file_lock(path, || {
                        std::fs::rename(path, backup)?;
                        std::fs::write(path, record)?;
                        done_tx.send(()).unwrap();
                        Ok(())
                    }).unwrap();
                });
                assert!(jsonl_family_between(path, backup, &request(), &mut report, &mut 0,
                    redaction, || {
                        go_tx.send(()).unwrap();
                        attempt_rx.recv().unwrap();
                        assert!(done_rx.recv_timeout(std::time::Duration::from_millis(100)).is_err());
                    }).unwrap());
                writer.join().unwrap();
            });
            assert_eq!(if redaction { report.outbound.len() } else { report.reviews.len() }, 2);
            done_rx.recv().unwrap();
        }
    }

    #[test]
    fn compliance_sources_project_existing_evidence_and_utc_range() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("approvals.db")).unwrap();
        conn.execute_batch("CREATE TABLE approvals(id TEXT,agent_id TEXT,action_kind TEXT,status TEXT,created_at TEXT,decided_at TEXT,decided_by TEXT,payload TEXT,invalidated_reason TEXT,on_behalf_of TEXT);").unwrap();
        conn.execute("INSERT INTO approvals VALUES('a','sales','cli_egress','approved','2026-10-30T00:00:00Z','2026-11-01T03:00:00Z','dashboard:owner',?1,NULL,'user:owner')",
            [r#"{"arguments":{"url":"https://alice:password@example.com/path?secret=token","channel":"email"},"egress_guard":{"sources":["memory_read"],"reason":"egress_guard"}}"#]).unwrap();
        std::fs::write(dir.path().join("tool_calls.jsonl.old"), concat!(
            "{\"timestamp\":\"2026-11-01T08:00:00+08:00\",\"agent_id\":\"sales\",\"tool_name\":\"send_message\",\"egress_guard\":\"would_ask\",\"sources\":[\"memory_read\"]}\n",
            "{\"timestamp\":\"2026-11-02T00:00:00Z\",\"action_review\":\"block\"}\n"
        )).unwrap();
        std::fs::create_dir(dir.path().join("redaction")).unwrap();
        std::fs::write(dir.path().join("redaction/audit.jsonl"), "{\"ts\":\"2026-11-01T01:00:00Z\",\"event\":\"redact\",\"agent_id\":\"sales\",\"source_category\":\"tool_result\",\"category\":\"EMAIL\",\"token\":\"must-not-export\"}\n").unwrap();
        let report = collect(dir.path(), &request()).unwrap();
        assert_eq!(report.approvals.len(), 1);
        assert_eq!(report.outbound.len(), 2);
        assert_eq!(report.accesses.len(), 1);
        assert!(report.reviews.is_empty());
        let approval = report.outbound.iter().find(|r| r.id == "a").unwrap();
        assert_eq!(approval.destination, "email / example.com");
        assert_eq!(approval.categories, "memory_read");
        assert_eq!(approval.time, "2026-11-01T03:00:00Z");
        assert_eq!(approval.requester, "user:owner");
        assert_eq!(report.outbound[0].time, "2026-11-01T00:00:00Z");
    }

    #[test]
    fn compliance_sources_do_not_backdate_later_approval_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("approvals.db")).unwrap();
        conn.execute_batch("CREATE TABLE approvals(id TEXT,agent_id TEXT,action_kind TEXT,status TEXT,created_at TEXT,decided_at TEXT,decided_by TEXT,payload TEXT,invalidated_reason TEXT,on_behalf_of TEXT);
            INSERT INTO approvals VALUES('future','sales','mcp_call','approved','2026-11-01T01:00:00Z','2026-11-03T01:00:00Z','dashboard:owner','{}','later reason',NULL);").unwrap();
        let report = collect(dir.path(), &request()).unwrap();
        assert_eq!(report.approvals.len(), 1);
        let approval = &report.approvals[0];
        assert_eq!(approval.decision, "decision_outside_range");
        assert_eq!(approval.time, "2026-11-01T01:00:00Z");
        assert!(approval.approver.is_empty());
        assert!(approval.reason.is_empty());
        assert!(approval.requester.is_empty());
    }

    #[test]
    fn compliance_sources_sort_by_instant_including_fractional_seconds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool_calls.jsonl"), concat!(
            "{\"timestamp\":\"2026-11-01T00:00:00.1Z\",\"action_review\":\"block\"}\n",
            "{\"timestamp\":\"2026-11-01T08:00:00+08:00\",\"action_review\":\"allow\"}\n"
        )).unwrap();
        let report = collect(dir.path(), &request()).unwrap();
        assert_eq!(report.reviews.len(), 2);
        assert_eq!(report.reviews[0].time, "2026-11-01T00:00:00Z");
        assert_eq!(report.reviews[1].time, "2026-11-01T00:00:00.100Z");
    }

    #[test]
    fn compliance_sources_database_calls_export_only_recorded_metadata() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tool_calls.jsonl"), concat!(
            "{\"timestamp\":\"2026-11-01T00:00:00Z\",\"agent_id\":\"sales\",\"tool_name\":\"db_select\",\"success\":true,\"input\":\"SELECT secret FROM customers\",\"result_text\":\"sensitive value\"}\n",
            "{\"timestamp\":\"2026-11-01T00:00:01Z\",\"tool_name\":\"db_query\",\"success\":false}\n",
            "{\"timestamp\":\"2026-11-01T00:00:02Z\",\"tool_name\":\"db_tables\"}\n",
            "{\"timestamp\":\"2026-11-01T00:00:03Z\",\"tool_name\":\"db_select_extra\",\"success\":true}\n"
        )).unwrap();
        let report = collect(dir.path(), &request()).unwrap();
        assert_eq!(report.accesses.len(), 3);
        assert_eq!(report.accesses[0].decision, "recorded_success");
        assert_eq!(report.accesses[1].decision, "recorded_failure");
        assert!(report.accesses[2].decision.is_empty());
        for access in report.accesses {
            assert!(access.destination.is_empty());
            assert!(access.categories.is_empty());
            assert!(access.reason.is_empty());
            assert!(access.approver.is_empty());
            assert!(access.requester.is_empty());
        }
    }

    #[test]
    fn compliance_sources_missing_are_not_created_and_corruption_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let report = collect(dir.path(), &request()).unwrap();
        assert_eq!(report.notes.len(), 3);
        assert!(!dir.path().join("approvals.db").exists());
        std::fs::write(dir.path().join("tool_calls.jsonl"), "broken\n").unwrap();
        assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
        std::fs::write(dir.path().join("tool_calls.jsonl"), "{\"timestamp\":\"invalid\"}\n").unwrap();
        assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
    }

    #[test]
    fn compliance_sources_row_and_line_limits_refuse_instead_of_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let record = "{\"timestamp\":\"2026-11-01T00:00:00Z\",\"action_review\":\"allow\"}\n";
        std::fs::write(dir.path().join("tool_calls.jsonl"), record.repeat(MAX_ROWS + 1)).unwrap();
        assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(TOO_LARGE));
        std::fs::write(dir.path().join("tool_calls.jsonl"), "x".repeat(MAX_LINE_BYTES + 1)).unwrap();
        assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(TOO_LARGE));
    }

    #[cfg(unix)]
    #[test]
    fn compliance_sources_refuse_symlinked_evidence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("elsewhere"), "{}").unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join("approvals.db")).unwrap();
        assert_eq!(collect(dir.path(), &request()).err().as_deref(), Some(UNREADABLE));
    }
}
