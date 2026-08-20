//! Report rendering (zh-TW human summary) + JSON file output + CI exit code.
//!
//! Mirrors `duduclaw eval`'s CI-gate convention: a human-readable summary is
//! always printed; `--report <path>` additionally writes the full JSON
//! report to disk. Exit code follows the task spec exactly: `0` when no
//! finding meets `--fail-on` (this also covers the "zero findings at all"
//! and "all scanners missing" cases — see `cmd_secaudit`'s dogfood
//! requirement), `1` when at least one does, `2` reserved for infra errors
//! that prevent `secaudit` itself from completing (invalid repo path,
//! `--report` file write failure) — never for "a scanner was unavailable",
//! which is an ordinary, honestly-reported outcome.

use std::path::{Path, PathBuf};

use crate::secaudit::schema::{AuditReport, ProfileMode, Severity};

/// CI exit code for a completed scan. `0` unless at least one finding is at
/// or above `fail_on`. Pure function over the already-computed summary —
/// infra-level failures (bad repo path, can't write `--report`) are decided
/// by the caller before a report even exists, and use `2` directly.
pub fn exit_code(report: &AuditReport, fail_on: Severity) -> i32 {
    if report.summary.by_severity.count_at_or_above(fail_on) > 0 {
        1
    } else {
        0
    }
}

/// zh-TW human-readable summary. Pure function (returns a `String`) so it's
/// testable without capturing stdout.
pub fn render_summary(report: &AuditReport, fail_on: Severity) -> String {
    let mut out = String::new();
    out.push_str("代碼安全審計（secaudit）\n");
    out.push_str(&format!("  掃描目標：{}\n", report.repo));
    out.push_str(&format!(
        "  掃描模式：{}\n",
        match report.profile.mode {
            crate::secaudit::schema::ProfileMode::Quick => "quick",
            crate::secaudit::schema::ProfileMode::Deep => "deep",
        }
    ));
    out.push_str(&format!("  開始時間：{}\n", report.started_at));
    out.push('\n');

    out.push_str("引擎執行狀況：\n");
    for run in &report.engines_run {
        if let Some(err) = &run.parse_error {
            out.push_str(&format!(
                "  [異常] {} — {}\n",
                run.engine,
                if run.timed_out { format!("逾時：{err}") } else { err.clone() }
            ));
        } else {
            out.push_str(&format!(
                "  [完成] {} — {} 個發現（{} ms）\n",
                run.engine, run.findings_count, run.duration_ms
            ));
        }
    }
    for missing in &report.engines_missing {
        out.push_str(&format!("  [略過] {} — {}\n", missing.engine, missing.reason));
    }
    if report.engines_run.is_empty() && report.engines_missing.is_empty() {
        out.push_str("  （無引擎資訊）\n");
    }
    out.push('\n');

    out.push_str("Findings 統計：\n");
    let excluded = report
        .summary
        .total_findings
        .saturating_sub(report.summary.by_severity.total());
    if excluded > 0 {
        out.push_str(&format!(
            "  總計：{}（可行動 {}、已證偽/已壓制 {}——不計入嚴重度統計與 fail-on 判定）\n",
            report.summary.total_findings,
            report.summary.by_severity.total(),
            excluded
        ));
    } else {
        out.push_str(&format!("  總計：{}\n", report.summary.total_findings));
    }
    out.push_str(&format!(
        "  Critical: {}  High: {}  Medium: {}  Low: {}  Info: {}\n",
        report.summary.by_severity.critical,
        report.summary.by_severity.high,
        report.summary.by_severity.medium,
        report.summary.by_severity.low,
        report.summary.by_severity.info,
    ));

    if report.profile.mode == ProfileMode::Deep {
        out.push('\n');
        out.push_str("AI 深度審計／對抗式覆核／PoC（deep profile）：\n");
        out.push_str(&format!(
            "  AI 候選：{}　已證偽：{}　待人工覆核：{}　PoC 已執行：{}\n",
            report.summary.ai_audit_candidates,
            report.summary.ai_audit_refuted,
            report.summary.ai_audit_needs_human,
            report.summary.poc_ran,
        ));
    }

    if let Some(intake) = &report.profile.intake {
        out.push('\n');
        out.push_str("Intake / 威脅建模（deep profile）：\n");
        match &intake.git_history {
            crate::secaudit::intake::GitHistoryStatus::Available { hotspots } => {
                if hotspots.is_empty() {
                    out.push_str("  git 熱點：近一年內無安全關鍵詞相關 commit\n");
                } else {
                    out.push_str("  git 熱點（近一年，安全關鍵詞 commit 觸及的檔案）：\n");
                    for h in hotspots.iter().take(10) {
                        out.push_str(&format!(
                            "    {} — 安全相關異動 {} 次（總異動 {} 次）\n",
                            h.file, h.security_touches, h.total_touches
                        ));
                    }
                }
            }
            crate::secaudit::intake::GitHistoryStatus::Unavailable { reason } => {
                out.push_str(&format!("  git 熱點：不可用（{reason}）\n"));
            }
        }
        if !intake.entry_points.is_empty() {
            out.push_str(&format!(
                "  偵測到的進入點：{}\n",
                intake.entry_points.join(", ")
            ));
        }
        if !intake.language_census.is_empty() {
            let top: Vec<String> = intake
                .language_census
                .iter()
                .take(5)
                .map(|s| format!("{}×{}", s.extension, s.file_count))
                .collect();
            out.push_str(&format!("  語言統計（前 5）：{}\n", top.join("、")));
        }
    }

    out.push('\n');
    let code = exit_code(report, fail_on);
    if code == 0 {
        out.push_str(&format!(
            "結論：未達 --fail-on 門檻（{}），視為通過。\n",
            fail_on.as_str()
        ));
    } else {
        out.push_str(&format!(
            "結論：發現 {} 項 {} 以上等級的問題，未通過。\n",
            report.summary.by_severity.count_at_or_above(fail_on),
            fail_on.as_str()
        ));
    }
    out
}

/// Write the full JSON report to `path` (pretty-printed, matching `duduclaw
/// eval`'s `--report` convention). Any I/O failure here is an infra error
/// (exit 2) at the call site.
pub fn write_json_report(report: &AuditReport, path: &Path) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| std::io::Error::other(format!("serialize report: {e}")))?;
    std::fs::write(path, json + "\n")
}

/// UTC ISO 8601 "basic" form (no separators — filesystem-safe, sorts
/// lexically = chronologically), e.g. `20260818T093000Z`. Matches the
/// filename contract `duduclaw-gateway::secaudit_reports` already reads
/// from (see that module's doc comment).
pub fn save_timestamp_basic(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

/// `--save`: write a copy of the report to
/// `<home>/secaudit/reports/<UTC-ISO8601-basic>.json`, creating the
/// directory if needed, with owner-only (0600) permissions. Field names are
/// exactly the same as `--report`'s JSON (this just calls
/// [`write_json_report`] at a different path) — the dashboard RPC line
/// (`duduclaw-gateway::secaudit_reports`) reads this directory and depends
/// on the shape never changing, only growing. Returns the path written.
pub fn save_report(home_dir: &Path, report: &AuditReport) -> std::io::Result<PathBuf> {
    let dir = home_dir.join("secaudit").join("reports");
    std::fs::create_dir_all(&dir)?;
    let filename = format!("{}.json", save_timestamp_basic(chrono::Utc::now()));
    let path = dir.join(filename);
    write_json_report(report, &path)?;
    // Best-effort: a permission-set failure shouldn't un-write an
    // already-saved report (the write already succeeded above).
    let _ = duduclaw_core::platform::set_owner_only(&path);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secaudit::intake::{GitHistoryStatus, HotspotFile, LanguageStat, RepoProfile};
    use crate::secaudit::schema::{
        EngineMissing, EngineRun, Finding, FindingKind, ProfileMode, ScanProfile, Summary,
    };

    fn base_report(findings: Vec<Finding>) -> AuditReport {
        AuditReport {
            repo: "/tmp/repo".to_string(),
            started_at: "2026-08-17T00:00:00Z".to_string(),
            profile: ScanProfile {
                mode: ProfileMode::Quick,
                intake: None,
            },
            engines_run: vec![EngineRun {
                engine: "gitleaks".to_string(),
                findings_count: findings.len(),
                duration_ms: 10,
                parse_error: None,
                timed_out: false,
            }],
            engines_missing: vec![EngineMissing {
                engine: "osv-scanner".to_string(),
                reason: "requires network access".to_string(),
            }],
            summary: Summary::from_findings(&findings, 1, 1),
            findings,
        }
    }

    // ── exit_code ─────────────────────────────────────────────────────

    #[test]
    fn exit_code_zero_when_no_findings_at_all() {
        let report = base_report(vec![]);
        assert_eq!(exit_code(&report, Severity::High), 0);
    }

    #[test]
    fn exit_code_zero_when_findings_all_below_threshold() {
        let findings = vec![Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::Low,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        )];
        let report = base_report(findings);
        assert_eq!(exit_code(&report, Severity::High), 0);
    }

    #[test]
    fn exit_code_one_when_a_finding_meets_threshold() {
        let findings = vec![Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        )];
        let report = base_report(findings);
        assert_eq!(exit_code(&report, Severity::High), 1);
    }

    #[test]
    fn exit_code_one_when_a_finding_exceeds_threshold() {
        let findings = vec![Finding::candidate(
            "gitleaks",
            FindingKind::Secret,
            Severity::Critical,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        )];
        let report = base_report(findings);
        assert_eq!(exit_code(&report, Severity::Medium), 1);
    }

    #[test]
    fn exit_code_zero_on_machine_with_all_engines_missing() {
        // Dogfood requirement: a machine with zero scanners installed must
        // still produce exit 0 with an honest, non-empty engines_missing list.
        let report = AuditReport {
            repo: "/tmp/repo".to_string(),
            started_at: "2026-08-17T00:00:00Z".to_string(),
            profile: ScanProfile {
                mode: ProfileMode::Quick,
                intake: None,
            },
            engines_run: vec![],
            engines_missing: vec![
                EngineMissing { engine: "semgrep".to_string(), reason: "not installed".to_string() },
                EngineMissing { engine: "gitleaks".to_string(), reason: "not installed".to_string() },
                EngineMissing { engine: "osv-scanner".to_string(), reason: "network policy".to_string() },
                EngineMissing { engine: "cargo-audit".to_string(), reason: "not applicable".to_string() },
            ],
            findings: vec![],
            summary: Summary::from_findings(&[], 0, 4),
        };
        assert_eq!(exit_code(&report, Severity::High), 0);
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("semgrep"));
        assert!(text.contains("gitleaks"));
        assert!(text.contains("osv-scanner"));
        assert!(text.contains("cargo-audit"));
        assert!(text.contains("通過"));
    }

    // ── render_summary ───────────────────────────────────────────────

    #[test]
    fn render_summary_includes_repo_and_counts() {
        let findings = vec![Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        )];
        let report = base_report(findings);
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("/tmp/repo"));
        assert!(text.contains("總計：1"));
        assert!(text.contains("未通過"));
    }

    #[test]
    fn render_summary_shows_engine_run_and_missing_entries() {
        let report = base_report(vec![]);
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("gitleaks"));
        assert!(text.contains("osv-scanner"));
        assert!(text.contains("requires network access"));
    }

    #[test]
    fn render_summary_includes_deep_profile_intake_data() {
        let mut report = base_report(vec![]);
        report.profile.mode = ProfileMode::Deep;
        report.profile.intake = Some(RepoProfile {
            language_census: vec![LanguageStat { extension: "rs".to_string(), file_count: 42 }],
            entry_points: vec!["src/main.rs".to_string()],
            git_history: GitHistoryStatus::Available {
                hotspots: vec![HotspotFile {
                    file: "src/auth.rs".to_string(),
                    total_touches: 5,
                    security_touches: 3,
                }],
            },
        });
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("src/auth.rs"));
        assert!(text.contains("src/main.rs"));
        assert!(text.contains("rs×42"));
    }

    #[test]
    fn render_summary_reports_unavailable_git_history_honestly() {
        let mut report = base_report(vec![]);
        report.profile.mode = ProfileMode::Deep;
        report.profile.intake = Some(RepoProfile {
            language_census: vec![],
            entry_points: vec![],
            git_history: GitHistoryStatus::Unavailable {
                reason: "not a git repository".to_string(),
            },
        });
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("不可用"));
        assert!(text.contains("not a git repository"));
    }

    #[test]
    fn render_summary_marks_engine_parse_error() {
        let mut report = base_report(vec![]);
        report.engines_run.push(EngineRun {
            engine: "cargo-audit".to_string(),
            findings_count: 0,
            duration_ms: 5,
            parse_error: Some("JSON parse failed: unexpected token".to_string()),
            timed_out: false,
        });
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("[異常] cargo-audit"));
        assert!(text.contains("unexpected token"));
    }

    // ── write_json_report ────────────────────────────────────────────

    #[test]
    fn write_json_report_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let report = base_report(vec![]);
        write_json_report(&report, &path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: AuditReport = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.repo, "/tmp/repo");
    }

    #[test]
    fn write_json_report_fails_on_unwritable_path() {
        let report = base_report(vec![]);
        let bad_path = Path::new("/nonexistent-dir-xyz/report.json");
        assert!(write_json_report(&report, bad_path).is_err());
    }

    // ── deep-profile AI summary block ────────────────────────────────

    #[test]
    fn render_summary_shows_ai_audit_block_only_in_deep_profile() {
        let mut deep = base_report(vec![]);
        deep.profile.mode = ProfileMode::Deep;
        deep.summary.ai_audit_candidates = 3;
        deep.summary.ai_audit_refuted = 1;
        deep.summary.ai_audit_needs_human = 2;
        deep.summary.poc_ran = 1;
        let text = render_summary(&deep, Severity::High);
        assert!(text.contains("AI 深度審計"));
        assert!(text.contains("AI 候選：3"));
        assert!(text.contains("已證偽：1"));
        assert!(text.contains("待人工覆核：2"));
        assert!(text.contains("PoC 已執行：1"));

        let quick = base_report(vec![]); // mode defaults to Quick in base_report
        let text = render_summary(&quick, Severity::High);
        assert!(!text.contains("AI 深度審計"));
    }

    // ── save_timestamp_basic / save_report ───────────────────────────

    #[test]
    fn save_timestamp_basic_has_no_separators() {
        let ts = chrono::DateTime::parse_from_rfc3339("2026-08-18T09:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(save_timestamp_basic(ts), "20260818T093000Z");
    }

    #[test]
    fn save_report_creates_dir_and_writes_readable_json() {
        let home = tempfile::tempdir().unwrap();
        let report = base_report(vec![]);
        let path = save_report(home.path(), &report).unwrap();
        assert!(path.starts_with(home.path().join("secaudit").join("reports")));
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: AuditReport = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.repo, "/tmp/repo");
    }

    #[cfg(unix)]
    #[test]
    fn save_report_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let report = base_report(vec![]);
        let path = save_report(home.path(), &report).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
