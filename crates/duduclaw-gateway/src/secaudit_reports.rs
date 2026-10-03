//! Dashboard reads/writes for `duduclaw secaudit --save` reports
//! (DESIGN-code-security-audit-2026-08 §3.1 "Dashboard").
//!
//! ## Deliberate decoupling from the CLI crate
//!
//! `crates/duduclaw-cli/src/secaudit/schema.rs` owns the authoritative
//! `AuditReport` / `Finding` Rust types and writes them to
//! `<home>/secaudit/reports/<UTC ISO8601 basic>.json` via `--save`. This
//! module does NOT import those types — `duduclaw-cli` already depends on
//! `duduclaw-gateway` (for the embedded dashboard + `duduclaw-gateway`
//! feature), so the reverse dependency would be circular. Instead this
//! module treats the report as `serde_json::Value` against the documented
//! stable snake_case JSON contract: as long as the CLI writer's shape stays
//! additive-only (schema.rs's own stated invariant), the dashboard reads it
//! without needing to duplicate or import the struct definitions.
//!
//! ## Containment
//!
//! Every read/write is confined to `<home>/secaudit/reports/`, checked
//! **after** canonicalisation (same discipline as
//! `duduclaw-security::secret_manager::file`): a `file` param is validated as
//! a bare basename first (no separators, no `..`), then the resolved real
//! path must still land inside the canonicalised reports directory. A single
//! report is capped at [`MAX_REPORT_BYTES`] so a malformed or hostile file
//! can't be read into memory unbounded.
//!
//! ## Fail-open listing, fail-closed everything else
//!
//! `list_reports` never lets one broken file break the whole list — a
//! corrupt/oversized/unreadable report becomes a row with `parse_error` set
//! and every summary field `None`, per the project convention "壞檔標
//! parse_error 不炸整列". Reading a single full report and mutating a
//! finding's status are both fail-closed: any validation or I/O failure
//! returns `Err` and touches nothing on disk.

use std::path::{Path, PathBuf};

use duduclaw_core::with_file_lock;
use serde::Serialize;
use serde_json::Value;

/// Reports directory, relative to `<home>`. Matches the CLI's `--save`
/// contract exactly (see module doc).
pub const REPORTS_SUBDIR: &str = "secaudit/reports";

/// Hard cap on a single report file (task spec: "單檔 16MB 上限").
pub const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;

/// `secaudit.finding_status` status values an operator write may set — the
/// three human-review outcomes. Matches
/// `duduclaw_cli::secaudit::schema::FindingStatus`'s snake_case wire form;
/// `candidate` / `needs_human` are machine-set only and deliberately not
/// writable here (an operator confirms/suppresses/refutes, never manufactures
/// a fresh "unreviewed" or "ambiguous" state).
const ALLOWED_STATUSES: &[&str] = &["confirmed", "suppressed", "refuted"];

pub fn reports_dir(home_dir: &Path) -> PathBuf {
    home_dir.join(REPORTS_SUBDIR)
}

/// One row of `secaudit.reports` — shallow summary fields only (never the
/// `findings` array), tolerant of a broken file.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReportListRow {
    pub file: String,
    /// RFC3339, filesystem mtime.
    pub mtime: String,
    pub repo: Option<String>,
    pub started_at: Option<String>,
    pub profile_mode: Option<String>,
    pub total_findings: Option<u64>,
    pub by_severity: Option<Value>,
    pub engines_run_count: Option<u64>,
    pub engines_missing_count: Option<u64>,
    /// Set (all fields above `None`) when the file could not be read or
    /// parsed as JSON — the row still appears, just honestly empty.
    pub parse_error: Option<String>,
}

/// List every `*.json` report, newest mtime first. A missing directory is
/// not an error — an operator who has never run `secaudit --save` sees an
/// empty list, not a broken page.
pub fn list_reports(home_dir: &Path) -> Vec<ReportListRow> {
    let dir = reports_dir(home_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Symlinks are skipped, not followed — a report directory should
        // only ever contain plain files the CLI itself wrote.
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        let name = name.to_string();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
            .unwrap_or_default();

        if meta.len() > MAX_REPORT_BYTES {
            rows.push(ReportListRow {
                file: name,
                mtime,
                parse_error: Some(format!(
                    "報告檔案過大（{} bytes，上限 {MAX_REPORT_BYTES} bytes）",
                    meta.len()
                )),
                ..Default::default()
            });
            continue;
        }

        match std::fs::read(&path) {
            Ok(raw) => rows.push(summarize_row(name, mtime, &raw)),
            Err(e) => rows.push(ReportListRow {
                file: name,
                mtime,
                parse_error: Some(format!("讀取失敗：{e}")),
                ..Default::default()
            }),
        }
    }

    rows.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    rows
}

fn summarize_row(file: String, mtime: String, raw: &[u8]) -> ReportListRow {
    let parsed: Result<Value, _> = serde_json::from_slice(raw);
    match parsed {
        Ok(v) => {
            let summary = v.get("summary");
            ReportListRow {
                file,
                mtime,
                repo: v.get("repo").and_then(Value::as_str).map(str::to_string),
                started_at: v
                    .get("started_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                profile_mode: v
                    .get("profile")
                    .and_then(|p| p.get("mode"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                total_findings: summary
                    .and_then(|s| s.get("total_findings"))
                    .and_then(Value::as_u64),
                by_severity: summary.and_then(|s| s.get("by_severity")).cloned(),
                engines_run_count: summary
                    .and_then(|s| s.get("engines_run_count"))
                    .and_then(Value::as_u64),
                engines_missing_count: summary
                    .and_then(|s| s.get("engines_missing_count"))
                    .and_then(Value::as_u64),
                parse_error: None,
            }
        }
        Err(e) => ReportListRow {
            file,
            mtime,
            parse_error: Some(format!("JSON 解析失敗：{e}")),
            ..Default::default()
        },
    }
}

/// Validate `name` is a bare basename — no path separators, no `..`, must
/// end in `.json`. This is the first gate before any filesystem touch; the
/// canonicalize+containment check in [`vet_report_path`] is the second,
/// symlink-safe gate.
fn validate_basename(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("file 不可為空".to_string());
    }
    if name.len() > 256 {
        return Err("file 檔名過長".to_string());
    }
    if name.contains(['/', '\\']) || name.contains('\0') {
        return Err("file 不可包含路徑分隔符".to_string());
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err("file 不可包含 ..".to_string());
    }
    if !name.ends_with(".json") {
        return Err("file 必須是 .json 檔".to_string());
    }
    Ok(())
}

/// Resolve `basename` to a real file confined to `<home>/secaudit/reports/`.
/// Canonicalises first (proves existence, resolves symlinks) then checks
/// containment against the canonicalised reports directory — the same order
/// `secret_manager::file::vet_path` uses, for the same reason: an
/// uncanonicalised prefix test can be fooled by a symlink.
fn vet_report_path(home_dir: &Path, basename: &str) -> Result<PathBuf, String> {
    validate_basename(basename)?;
    let dir = reports_dir(home_dir);
    let candidate = dir.join(basename);
    let real = candidate
        .canonicalize()
        .map_err(|e| format!("找不到報告 {basename}：{e}"))?;
    let real_dir = dir.canonicalize().unwrap_or(dir);
    if !real.starts_with(&real_dir) {
        return Err(format!("report path escapes reports directory: {basename}"));
    }
    let meta = std::fs::metadata(&real).map_err(|e| format!("讀取報告中繼資料失敗：{e}"))?;
    if !meta.is_file() {
        return Err("report path 不是一般檔案".to_string());
    }
    if meta.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "報告檔案過大（{} bytes，上限 {MAX_REPORT_BYTES} bytes）",
            meta.len()
        ));
    }
    Ok(real)
}

/// Read one full report as raw JSON (`secaudit.report`). Fail-closed: any
/// validation or parse failure returns `Err`, nothing is fabricated.
pub fn read_report(home_dir: &Path, basename: &str) -> Result<Value, String> {
    let path = vet_report_path(home_dir, basename)?;
    let raw = std::fs::read(&path).map_err(|e| format!("讀取報告失敗：{e}"))?;
    serde_json::from_slice::<Value>(&raw).map_err(|e| format!("報告 JSON 格式錯誤：{e}"))
}

/// Severity keys of the summary tables, in the CLI's `SeverityCounts` order.
const SEVERITY_KEYS: &[&str] = &["critical", "high", "medium", "low", "info"];

/// Normalize a finding's `severity` string to a [`SEVERITY_KEYS`] entry.
/// `informational` reads as `info`; anything else unknown is `None` (the
/// finding is then left out of the tables, without error).
fn severity_key(raw: &str) -> Option<&'static str> {
    match raw {
        "informational" => Some("info"),
        other => SEVERITY_KEYS.iter().copied().find(|k| *k == other),
    }
}

/// Record an operator decision on one finding: overwrite `status`, set
/// `severity_basis` to `operator`, and append an `operator_review` evidence
/// item (creating the `evidence` array if absent or not an array).
fn apply_operator_review(
    obj: &mut serde_json::Map<String, Value>,
    status: &str,
    recorded_at: &str,
) {
    obj.insert("status".to_string(), Value::String(status.to_string()));
    obj.insert(
        "severity_basis".to_string(),
        Value::String("operator".to_string()),
    );
    let item = serde_json::json!({
        "kind": "operator_review",
        "source": "dashboard",
        "detail": format!("operator_decision: {status}"),
        "recorded_at": recorded_at,
    });
    match obj.get_mut("evidence").and_then(Value::as_array_mut) {
        Some(evidence) => evidence.push(item),
        None => {
            obj.insert("evidence".to_string(), Value::Array(vec![item]));
        }
    }
}

/// Recompute `summary.by_severity` and `summary.needs_human_by_severity`
/// from all findings (secaudit v2 §3.3, D1=B). `refuted` / `suppressed`
/// count in neither table; `needs_human` counts in `needs_human_by_severity`
/// unless `summary.gate_includes_needs_human` is `true`, in which case it
/// counts in `by_severity`; every other status counts in `by_severity`.
/// Only those two keys are written. A report without a `summary` object
/// (v1) is left untouched.
fn recompute_severity_tables(doc: &mut Value) {
    let gate_includes_needs_human = doc
        .get("summary")
        .and_then(|s| s.get("gate_includes_needs_human"))
        .and_then(Value::as_bool)
        == Some(true);
    if !doc.get("summary").is_some_and(Value::is_object) {
        return;
    }
    let mut gated = [0u64; 5];
    let mut needs_human = [0u64; 5];
    if let Some(findings) = doc.get("findings").and_then(Value::as_array) {
        for f in findings {
            let Some(sev) = f
                .get("severity")
                .and_then(Value::as_str)
                .and_then(severity_key)
            else {
                continue;
            };
            let Some(idx) = SEVERITY_KEYS.iter().position(|k| *k == sev) else {
                continue;
            };
            match f.get("status").and_then(Value::as_str) {
                Some("refuted") | Some("suppressed") => {}
                Some("needs_human") if !gate_includes_needs_human => needs_human[idx] += 1,
                _ => gated[idx] += 1,
            }
        }
    }
    let table = |counts: &[u64; 5]| {
        let mut m = serde_json::Map::new();
        for (k, n) in SEVERITY_KEYS.iter().zip(counts.iter()) {
            m.insert((*k).to_string(), Value::from(*n));
        }
        Value::Object(m)
    };
    if let Some(summary) = doc.get_mut("summary").and_then(Value::as_object_mut) {
        summary.insert("by_severity".to_string(), table(&gated));
        summary.insert("needs_human_by_severity".to_string(), table(&needs_human));
    }
    recompute_ai_audit_counters(doc);
}

/// Recompute `summary.ai_audit_refuted` / `summary.ai_audit_needs_human`
/// from all findings (`source_engine == "ai_audit"` with that status). Only
/// those two keys are written (`ai_audit_candidates` and the rest stay as
/// they are); a report without a `summary` object is left untouched.
fn recompute_ai_audit_counters(doc: &mut Value) {
    if !doc.get("summary").is_some_and(Value::is_object) {
        return;
    }
    let mut refuted = 0u64;
    let mut needs_human = 0u64;
    if let Some(findings) = doc.get("findings").and_then(Value::as_array) {
        for f in findings {
            if f.get("source_engine").and_then(Value::as_str) != Some("ai_audit") {
                continue;
            }
            match f.get("status").and_then(Value::as_str) {
                Some("refuted") => refuted += 1,
                Some("needs_human") => needs_human += 1,
                _ => {}
            }
        }
    }
    if let Some(summary) = doc.get_mut("summary").and_then(Value::as_object_mut) {
        summary.insert("ai_audit_refuted".to_string(), Value::from(refuted));
        summary.insert("ai_audit_needs_human".to_string(), Value::from(needs_human));
    }
}

/// Read-modify-write one finding's review decision (`secaudit.finding_status`
/// — the operator confirm/suppress/refute action). Besides `status`, it sets
/// the finding's `severity_basis` to `operator`, appends an
/// `operator_review` evidence item, and recomputes the summary's
/// `by_severity` / `needs_human_by_severity` tables, so a reviewed v2 report
/// still passes `duduclaw secaudit-validate`. Locked (cross-process safe) +
/// atomic temp-file-then-rename, mirroring `working_state::persist`.
///
/// Returns the updated finding object on success.
pub fn set_finding_status(
    home_dir: &Path,
    basename: &str,
    finding_id: &str,
    status: &str,
) -> Result<Value, String> {
    if !ALLOWED_STATUSES.contains(&status) {
        return Err(format!(
            "status 必須是 {} 其中之一（收到：{status}）",
            ALLOWED_STATUSES.join(" / ")
        ));
    }
    if finding_id.is_empty() || finding_id.len() > 200 {
        return Err("finding_id 不合法".to_string());
    }
    let path = vet_report_path(home_dir, basename)?;

    with_file_lock(&path, || {
        let raw = std::fs::read(&path)?;
        let mut doc: Value = serde_json::from_slice(&raw).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("報告 JSON 格式錯誤：{e}"),
            )
        })?;

        let recorded_at = chrono::Utc::now().to_rfc3339();
        let updated = {
            let Some(findings) = doc.get_mut("findings").and_then(Value::as_array_mut) else {
                return Ok(Err("報告缺少 findings 陣列".to_string()));
            };
            let Some(finding) = findings
                .iter_mut()
                .find(|f| f.get("id").and_then(Value::as_str) == Some(finding_id))
            else {
                return Ok(Err(format!("finding_id 不存在：{finding_id}")));
            };
            let Some(obj) = finding.as_object_mut() else {
                return Ok(Err("finding 格式錯誤（非 JSON object）".to_string()));
            };
            apply_operator_review(obj, status, &recorded_at);
            finding.clone()
        };
        recompute_severity_tables(&mut doc);

        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&doc).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("序列化失敗：{e}"))
        })?;
        std::fs::write(&tmp, json + "\n")?;
        std::fs::rename(&tmp, &path)?;

        Ok(Ok(updated))
    })
    .map_err(|e: std::io::Error| format!("更新 finding 狀態失敗：{e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    struct TempHome(PathBuf);
    impl TempHome {
        fn new() -> Self {
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "duduclaw-secaudit-reports-test-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(reports_dir(&p)).unwrap();
            Self(p)
        }
        fn write_report(&self, name: &str, body: &str) -> PathBuf {
            let p = reports_dir(&self.0).join(name);
            std::fs::write(&p, body).unwrap();
            p
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample_report(findings: &str) -> String {
        format!(
            r#"{{
                "repo": "/tmp/repo",
                "started_at": "2026-08-17T00:00:00Z",
                "profile": {{ "mode": "quick", "intake": null }},
                "engines_run": [],
                "engines_missing": [],
                "findings": [{findings}],
                "summary": {{
                    "total_findings": 1,
                    "by_severity": {{ "critical": 0, "high": 1, "medium": 0, "low": 0, "info": 0 }},
                    "engines_run_count": 1,
                    "engines_missing_count": 0
                }}
            }}"#
        )
    }

    fn sample_finding(id: &str, status: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "source_engine": "semgrep",
                "kind": "static_analysis",
                "severity": "high",
                "title": "t",
                "file": "src/main.rs",
                "line": 10,
                "snippet": "s",
                "rule_id": "r",
                "evidence": [],
                "status": "{status}"
            }}"#
        )
    }

    // ── list_reports ─────────────────────────────────────────────────

    #[test]
    fn list_reports_on_missing_directory_returns_empty_not_error() {
        let home = std::env::temp_dir().join(format!(
            "duduclaw-secaudit-reports-nodir-{}",
            std::process::id()
        ));
        assert!(list_reports(&home).is_empty());
    }

    #[test]
    fn list_reports_summarizes_shallow_fields_newest_first() {
        let home = TempHome::new();
        home.write_report(
            "20260817T000000Z.json",
            &sample_report(&sample_finding("semgrep-aaa", "candidate")),
        );
        // Ensure a distinguishable mtime ordering isn't flaky on fast
        // filesystems: write the second file after a tiny sleep.
        std::thread::sleep(std::time::Duration::from_millis(20));
        home.write_report(
            "20260818T000000Z.json",
            &sample_report(&sample_finding("semgrep-bbb", "candidate")),
        );
        let rows = list_reports(&home.0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].file, "20260818T000000Z.json");
        assert_eq!(rows[1].file, "20260817T000000Z.json");
        assert_eq!(rows[0].repo.as_deref(), Some("/tmp/repo"));
        assert_eq!(rows[0].total_findings, Some(1));
        assert!(rows[0].parse_error.is_none());
    }

    #[test]
    fn list_reports_tolerates_a_broken_file_without_dropping_the_rest() {
        let home = TempHome::new();
        home.write_report(
            "20260817T000000Z.json",
            &sample_report(&sample_finding("semgrep-aaa", "candidate")),
        );
        home.write_report("20260818T000000Z.json", "{ not valid json");
        let rows = list_reports(&home.0);
        assert_eq!(rows.len(), 2);
        let broken = rows
            .iter()
            .find(|r| r.file == "20260818T000000Z.json")
            .unwrap();
        assert!(broken.parse_error.is_some());
        assert!(broken.repo.is_none());
        let ok = rows
            .iter()
            .find(|r| r.file == "20260817T000000Z.json")
            .unwrap();
        assert!(ok.parse_error.is_none());
    }

    #[test]
    fn list_reports_ignores_non_json_files() {
        let home = TempHome::new();
        std::fs::write(reports_dir(&home.0).join("notes.txt"), "hello").unwrap();
        assert!(list_reports(&home.0).is_empty());
    }

    // ── basename validation ─────────────────────────────────────────

    #[test]
    fn validate_basename_rejects_path_traversal_and_separators() {
        assert!(validate_basename("../etc/passwd").is_err());
        assert!(validate_basename("a/b.json").is_err());
        assert!(validate_basename("a\\b.json").is_err());
        assert!(validate_basename("..").is_err());
        assert!(validate_basename("").is_err());
        assert!(validate_basename("report.txt").is_err());
        assert!(validate_basename("20260817T000000Z.json").is_ok());
    }

    #[test]
    fn read_report_rejects_traversal_even_when_target_exists() {
        let home = TempHome::new();
        // A real file that exists, but reached via a traversal-shaped name.
        std::fs::write(home.0.join("secret.json"), "{}").unwrap();
        let err = read_report(&home.0, "../secret.json").unwrap_err();
        assert!(err.contains("不可包含"), "{err}");
    }

    #[test]
    fn read_report_rejects_missing_file() {
        let home = TempHome::new();
        assert!(read_report(&home.0, "nope.json").is_err());
    }

    #[test]
    fn read_report_round_trips_a_valid_report() {
        let home = TempHome::new();
        home.write_report(
            "r.json",
            &sample_report(&sample_finding("semgrep-aaa", "candidate")),
        );
        let doc = read_report(&home.0, "r.json").unwrap();
        assert_eq!(doc["repo"], "/tmp/repo");
        assert_eq!(doc["findings"][0]["id"], "semgrep-aaa");
    }

    // ── set_finding_status ──────────────────────────────────────────

    #[test]
    fn set_finding_status_rejects_unknown_status() {
        let home = TempHome::new();
        home.write_report(
            "r.json",
            &sample_report(&sample_finding("semgrep-aaa", "candidate")),
        );
        let err = set_finding_status(&home.0, "r.json", "semgrep-aaa", "deleted").unwrap_err();
        assert!(err.contains("status"), "{err}");
    }

    #[test]
    fn set_finding_status_rejects_unknown_finding_id() {
        let home = TempHome::new();
        home.write_report(
            "r.json",
            &sample_report(&sample_finding("semgrep-aaa", "candidate")),
        );
        let err = set_finding_status(&home.0, "r.json", "does-not-exist", "confirmed").unwrap_err();
        assert!(err.contains("finding_id"), "{err}");
    }

    #[test]
    fn set_finding_status_round_trips_read_modify_write() {
        let home = TempHome::new();
        home.write_report(
            "r.json",
            &sample_report(&sample_finding("semgrep-aaa", "candidate")),
        );
        let updated = set_finding_status(&home.0, "r.json", "semgrep-aaa", "confirmed").unwrap();
        assert_eq!(updated["status"], "confirmed");

        // Persisted to disk, not just the in-memory return value.
        let doc = read_report(&home.0, "r.json").unwrap();
        assert_eq!(doc["findings"][0]["status"], "confirmed");
        // Everything else in the report is untouched.
        assert_eq!(doc["repo"], "/tmp/repo");
    }

    #[test]
    fn set_finding_status_accepts_all_three_reviewer_outcomes() {
        let home = TempHome::new();
        for (i, status) in ["confirmed", "suppressed", "refuted"].iter().enumerate() {
            let file = format!("r{i}.json");
            home.write_report(&file, &sample_report(&sample_finding("f-1", "candidate")));
            let updated = set_finding_status(&home.0, &file, "f-1", status).unwrap();
            assert_eq!(updated["status"], *status);
        }
    }

    fn finding(id: &str, severity: &str, status: &str) -> String {
        format!(
            r#"{{ "id": "{id}", "source_engine": "ai_audit", "kind": "ai_audit",
                 "severity": "{severity}", "title": "t", "file": "src/a.rs",
                 "line": 1, "snippet": "s", "rule_id": null, "evidence": [],
                 "status": "{status}", "severity_basis": "model_self_reported" }}"#
        )
    }

    fn v2_report(findings: &[String], gate_includes_needs_human: bool) -> String {
        format!(
            r#"{{
                "schema_version": 2,
                "repo": "/tmp/repo",
                "findings": [{}],
                "summary": {{
                    "total_findings": {},
                    "by_severity": {{ "critical": 9, "high": 9, "medium": 9, "low": 9, "info": 9 }},
                    "needs_human_by_severity": {{ "critical": 9, "high": 9, "medium": 9, "low": 9, "info": 9 }},
                    "gate_includes_needs_human": {gate_includes_needs_human},
                    "engines_run_count": 3,
                    "precheck_refuted": 7,
                    "ai_audit_candidates": 5,
                    "ai_audit_refuted": 99,
                    "ai_audit_needs_human": 99
                }}
            }}"#,
            findings.join(","),
            findings.len()
        )
    }

    fn mixed_findings() -> Vec<String> {
        vec![
            finding("f-crit", "critical", "candidate"),
            finding("f-high", "high", "confirmed"),
            finding("f-med", "medium", "needs_human"),
            finding("f-low", "low", "refuted"),
            finding("f-info", "informational", "suppressed"),
            finding("f-info2", "informational", "needs_human"),
            finding("f-bogus", "bogus", "candidate"),
            // Scanner finding: counts in no ai_audit counter (and its unknown
            // severity keeps it out of the tables).
            finding("f-scan", "bogus", "refuted").replacen("\"ai_audit\"", "\"semgrep\"", 1),
        ]
    }

    fn counts(v: &Value) -> [u64; 5] {
        let mut out = [0u64; 5];
        for (i, k) in SEVERITY_KEYS.iter().enumerate() {
            out[i] = v[*k].as_u64().unwrap_or(u64::MAX);
        }
        out
    }

    #[test]
    fn set_finding_status_appends_operator_evidence_and_basis() {
        let home = TempHome::new();
        home.write_report("r.json", &v2_report(&mixed_findings(), false));
        let updated = set_finding_status(&home.0, "r.json", "f-crit", "confirmed").unwrap();
        assert_eq!(updated["status"], "confirmed");
        assert_eq!(updated["severity_basis"], "operator");
        let ev = updated["evidence"].as_array().unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["kind"], "operator_review");
        assert_eq!(ev[0]["source"], "dashboard");
        assert_eq!(ev[0]["detail"], "operator_decision: confirmed");
        let at = ev[0]["recorded_at"].as_str().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(at).is_ok(), "{at}");

        // Persisted, and a second decision appends rather than replaces.
        set_finding_status(&home.0, "r.json", "f-crit", "refuted").unwrap();
        let doc = read_report(&home.0, "r.json").unwrap();
        let f = &doc["findings"][0];
        assert_eq!(f["evidence"].as_array().unwrap().len(), 2);
        assert_eq!(f["evidence"][1]["detail"], "operator_decision: refuted");
        // Other findings keep their own basis.
        assert_eq!(doc["findings"][1]["severity_basis"], "model_self_reported");
    }

    #[test]
    fn set_finding_status_creates_missing_evidence_array() {
        let home = TempHome::new();
        let body =
            r#"{ "findings": [ { "id": "f-1", "severity": "low", "status": "candidate" } ] }"#;
        home.write_report("r.json", body);
        let updated = set_finding_status(&home.0, "r.json", "f-1", "suppressed").unwrap();
        assert_eq!(updated["evidence"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn set_finding_status_recomputes_tables_gate_off() {
        let home = TempHome::new();
        home.write_report("r.json", &v2_report(&mixed_findings(), false));
        set_finding_status(&home.0, "r.json", "f-high", "refuted").unwrap();
        let doc = read_report(&home.0, "r.json").unwrap();
        let s = &doc["summary"];
        // critical candidate counts; high now refuted; low refuted and info
        // suppressed count nowhere; bogus severity ignored.
        assert_eq!(counts(&s["by_severity"]), [1, 0, 0, 0, 0]);
        assert_eq!(counts(&s["needs_human_by_severity"]), [0, 0, 1, 0, 1]);
        // ai_audit counters: f-high + f-low refuted; f-med + f-info2 pending.
        assert_eq!(s["ai_audit_refuted"], 2);
        assert_eq!(s["ai_audit_needs_human"], 2);
        assert_eq!(s["ai_audit_candidates"], 5, "left untouched");
        // Other summary fields untouched.
        assert_eq!(s["total_findings"], 8);
        assert_eq!(s["engines_run_count"], 3);
        assert_eq!(s["precheck_refuted"], 7);
        assert_eq!(s["gate_includes_needs_human"], false);
    }

    #[test]
    fn set_finding_status_recomputes_tables_gate_on() {
        let home = TempHome::new();
        home.write_report("r.json", &v2_report(&mixed_findings(), true));
        set_finding_status(&home.0, "r.json", "f-crit", "confirmed").unwrap();
        let doc = read_report(&home.0, "r.json").unwrap();
        let s = &doc["summary"];
        // needs_human findings move into by_severity when the gate includes them.
        assert_eq!(counts(&s["by_severity"]), [1, 1, 1, 0, 1]);
        assert_eq!(counts(&s["needs_human_by_severity"]), [0, 0, 0, 0, 0]);
        // Counters do not depend on the gate flag.
        assert_eq!(s["ai_audit_refuted"], 1);
        assert_eq!(s["ai_audit_needs_human"], 2);
        assert_eq!(s["ai_audit_candidates"], 5, "left untouched");
    }

    #[test]
    fn set_finding_status_v1_report_without_summary_still_works() {
        let home = TempHome::new();
        let body = format!(
            r#"{{ "repo": "/tmp/repo", "findings": [{}] }}"#,
            sample_finding("semgrep-aaa", "candidate")
        );
        home.write_report("r.json", &body);
        let updated = set_finding_status(&home.0, "r.json", "semgrep-aaa", "confirmed").unwrap();
        assert_eq!(updated["status"], "confirmed");
        let doc = read_report(&home.0, "r.json").unwrap();
        assert!(
            doc.get("summary").is_none(),
            "no summary is invented for v1"
        );
        assert_eq!(doc["findings"][0]["severity_basis"], "operator");
    }

    #[test]
    fn set_finding_status_rejects_traversal_basename() {
        let home = TempHome::new();
        let err = set_finding_status(&home.0, "../x.json", "f-1", "confirmed").unwrap_err();
        assert!(err.contains("..") || err.contains("路徑"), "{err}");
    }
}
