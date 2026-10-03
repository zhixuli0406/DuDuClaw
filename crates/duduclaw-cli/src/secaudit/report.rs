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

use duduclaw_core::llm_contract::coverage::RunStatus;

use crate::secaudit::schema::{
    AuditReport, ProfileMode, Severity, SeverityCounts, VerifierIndependence,
};
use crate::secaudit::validator::validate_report;

/// CI exit code for a completed scan. `0` unless at least one finding the
/// gate reads ([`crate::secaudit::schema::Summary::gate_counts`] =
/// `by_severity`: `Candidate` / `Confirmed`, plus `NeedsHuman` under
/// `--fail-on-needs-human`, rule `schema::severity_buckets`) is at or
/// above `fail_on`. Pure function over the already-computed summary —
/// infra-level failures (bad repo path, can't write `--report`, a report
/// that fails validation) are decided by the caller and use `2` directly.
pub fn exit_code(report: &AuditReport, fail_on: Severity) -> i32 {
    if report.summary.gate_counts().count_at_or_above(fail_on) > 0 {
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
                if run.timed_out {
                    format!("逾時：{err}")
                } else {
                    err.clone()
                }
            ));
        } else {
            out.push_str(&format!(
                "  [完成] {} — {} 個發現（{} ms）\n",
                run.engine, run.findings_count, run.duration_ms
            ));
        }
    }
    for missing in &report.engines_missing {
        out.push_str(&format!(
            "  [略過] {} — {}\n",
            missing.engine, missing.reason
        ));
    }
    if report.engines_run.is_empty() && report.engines_missing.is_empty() {
        out.push_str("  （無引擎資訊）\n");
    }
    out.push('\n');

    out.push_str("Findings 統計：\n");
    let (gated, needs_human, inert) = finding_buckets(report);
    if needs_human + inert > 0 {
        out.push_str(&format!(
            "  總計：{}（計入 fail-on {gated}、待人工判斷 {needs_human}、已證偽/已壓制 {inert}）\n",
            report.summary.total_findings
        ));
        out.push_str("  （已證偽/已壓制的發現永遠不計入嚴重度統計與 fail-on 判定）\n");
    } else {
        out.push_str(&format!("  總計：{}\n", report.summary.total_findings));
    }
    out.push_str(&format!(
        "  {}\n",
        severity_row(&report.summary.by_severity)
    ));
    if report.summary.gate_includes_needs_human {
        // Flag on: NeedsHuman is already inside `by_severity`
        // (`schema::severity_buckets`); say so instead of a second row.
        let (with_nh, without_nh) = (
            crate::secaudit::schema::severity_buckets(&report.findings, true).0,
            crate::secaudit::schema::severity_buckets(&report.findings, false).0,
        );
        let nh = with_nh.total().saturating_sub(without_nh.total());
        if nh > 0 {
            out.push_str(&format!(
                "  待人工判斷：{nh} 筆（模型自評、已計入上方統計與 fail-on，--fail-on-needs-human）\n"
            ));
        }
    } else {
        let nh = &report.summary.needs_human_by_severity;
        if nh.total() > 0 {
            out.push_str(&format!(
                "  待人工判斷（模型自評、不計入 fail-on）：{}\n",
                severity_row(nh)
            ));
        }
    }
    if report.summary.carried_from_prior > 0 {
        out.push_str(&format!(
            "  沿用先前報告判定：{}（原始碼未變更，未重新呼叫模型）\n",
            report.summary.carried_from_prior
        ));
    }

    if report.profile.mode == ProfileMode::Deep {
        out.push('\n');
        out.push_str("AI 深度審計／對抗式覆核／PoC（deep profile）：\n");
        out.push_str(&format!(
            "  AI 候選：{}　已證偽：{}（其中前置檢查證偽 {}）　待人工覆核：{}　PoC 已執行：{}\n",
            report.summary.ai_audit_candidates,
            report.summary.ai_audit_refuted,
            report.summary.precheck_refuted,
            report.summary.ai_audit_needs_human,
            report.summary.poc_ran,
        ));
        render_coverage(report, &mut out);
        render_verifier_and_prior(report, &mut out);
    }
    if report.run_status == RunStatus::Incomplete {
        out.push_str(&format!(
            "  執行狀態：未完成（{}）\n",
            report
                .incomplete_reason
                .map(incomplete_reason_zh)
                .unwrap_or("原因未記錄")
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
            report.summary.gate_counts().count_at_or_above(fail_on),
            fail_on.as_str()
        ));
    }
    out
}

/// `(gated, needs_human, inert)` counted from the findings: refuted +
/// suppressed are inert; needs_human is its own bucket only when the
/// `--fail-on-needs-human` flag is off (otherwise it is gated, matching
/// `schema::severity_buckets`); everything else is gated.
fn finding_buckets(report: &AuditReport) -> (usize, usize, usize) {
    use crate::secaudit::schema::FindingStatus;
    let include_nh = report.summary.gate_includes_needs_human;
    let (mut gated, mut needs_human, mut inert) = (0, 0, 0);
    for f in &report.findings {
        match f.status {
            FindingStatus::Refuted | FindingStatus::Suppressed => inert += 1,
            FindingStatus::NeedsHuman if !include_nh => needs_human += 1,
            _ => gated += 1,
        }
    }
    (gated, needs_human, inert)
}

fn severity_row(c: &SeverityCounts) -> String {
    format!(
        "Critical: {}  High: {}  Medium: {}  Low: {}  Info: {}",
        c.critical, c.high, c.medium, c.low, c.info
    )
}

fn incomplete_reason_zh(
    r: duduclaw_core::llm_contract::coverage::IncompleteReason,
) -> &'static str {
    use duduclaw_core::llm_contract::coverage::IncompleteReason as R;
    match r {
        R::EngineUnavailable => "AI 引擎無法使用",
        R::ValidationBudgetExhausted => "候選數已達全程上限，部分候選或模組未處理",
        R::BudgetCannotFundReserves => "預算不足以保留覆核額度",
        R::CriticBudgetExhausted => "覆核預算用盡",
        R::Interrupted => "執行被中斷",
    }
}

fn render_coverage(report: &AuditReport, out: &mut String) {
    let c = &report.summary.coverage;
    out.push_str(&format!(
        "  模組覆蓋：共 {}　已審 {}（無候選 {}、有候選 {}）　延後 {}　失敗 {}\n",
        c.modules_total,
        c.covered + c.candidate,
        c.covered,
        c.candidate,
        c.deferred,
        c.failed
    ));
    if c.partial {
        out.push_str(&format!("  部分覆蓋：{} 模組未審\n", c.deferred + c.failed));
    }
}

fn render_verifier_and_prior(report: &AuditReport, out: &mut String) {
    let v = &report.verifier;
    let who = |a: &Option<String>| a.clone().unwrap_or_else(|| "全域 runtime".to_string());
    let line = match v.independence {
        VerifierIndependence::NotRun => "未執行覆核".to_string(),
        VerifierIndependence::SameAgent => {
            format!("同一 agent 覆核（{}）", who(&v.verifier_agent))
        }
        VerifierIndependence::DifferentAgent => format!(
            "獨立 agent 覆核（審計：{}；覆核：{}）",
            who(&v.audit_agent),
            who(&v.verifier_agent)
        ),
    };
    out.push_str(&format!("  覆核獨立性：{line}\n"));
    match &report.prior_run {
        Some(p) => out.push_str(&format!(
            "  先前報告承接：{}　沿用已壓制 {}、沿用已證偽 {}、重新覆核先前確認 {}、原始碼已變更 {}\n",
            p.report_file,
            p.carried_suppressed,
            p.carried_refuted,
            p.revalidated_prior_confirmed,
            p.changed_source
        )),
        None => out.push_str("  先前報告承接：無（沒有可承接的先前報告，或已用 --no-prior 停用）\n"),
    }
}

/// Write the full JSON report to `path` (pretty-printed, matching `duduclaw
/// eval`'s `--report` convention). The report validator runs first: a
/// report with any violation is not written and the error lists every
/// violation, one per line. Any failure here is an infra error (exit 2) at
/// the call site.
pub fn write_json_report(report: &AuditReport, path: &Path) -> std::io::Result<()> {
    let violations = validate_report(report);
    if !violations.is_empty() {
        let lines: Vec<String> = violations.iter().map(|v| v.to_string()).collect();
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("report failed validation:\n{}", lines.join("\n")),
        ));
    }
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
    // Validate before creating anything (write_json_report validates again;
    // this keeps a bad report from leaving an empty reports directory).
    let violations = validate_report(report);
    if !violations.is_empty() {
        let lines: Vec<String> = violations.iter().map(|v| v.to_string()).collect();
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("report failed validation:\n{}", lines.join("\n")),
        ));
    }
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
        CURRENT_SCHEMA_VERSION, CoverageSummary, EngineMissing, EngineRun, Finding, FindingKind,
        FindingStatus, GatePolicy, PriorRunInfo, ProfileMode, ScanProfile, Summary, VerifierInfo,
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
            summary: Summary::from_findings(&findings, 1, 1, GatePolicy::default(), &[]),
            findings,
            schema_version: CURRENT_SCHEMA_VERSION,
            run_status: RunStatus::Complete,
            incomplete_reason: None,
            coverage: vec![],
            prior_run: None,
            verifier: VerifierInfo::default(),
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
                EngineMissing {
                    engine: "semgrep".to_string(),
                    reason: "not installed".to_string(),
                },
                EngineMissing {
                    engine: "gitleaks".to_string(),
                    reason: "not installed".to_string(),
                },
                EngineMissing {
                    engine: "osv-scanner".to_string(),
                    reason: "network policy".to_string(),
                },
                EngineMissing {
                    engine: "cargo-audit".to_string(),
                    reason: "not applicable".to_string(),
                },
            ],
            findings: vec![],
            summary: Summary::from_findings(&[], 0, 4, GatePolicy::default(), &[]),
            schema_version: CURRENT_SCHEMA_VERSION,
            run_status: RunStatus::Complete,
            incomplete_reason: None,
            coverage: vec![],
            prior_run: None,
            verifier: VerifierInfo::default(),
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
            language_census: vec![LanguageStat {
                extension: "rs".to_string(),
                file_count: 42,
            }],
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
        assert!(text.contains("已證偽：1（其中前置檢查證偽 0）"));
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

    // ── v2 sections ──────────────────────────────────────────────────

    fn needs_human_high() -> Finding {
        let mut f = Finding::candidate(
            "ai_audit",
            FindingKind::Other,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        f.status = FindingStatus::NeedsHuman;
        f
    }

    #[test]
    fn needs_human_is_shown_but_not_gated_by_default() {
        let mut report = base_report(vec![]);
        report.summary =
            Summary::from_findings(&[needs_human_high()], 1, 1, GatePolicy::default(), &[]);
        assert_eq!(exit_code(&report, Severity::High), 0);
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("待人工判斷（模型自評、不計入 fail-on）"));
        assert!(text.contains("通過"));
    }

    #[test]
    fn fail_on_needs_human_gates_needs_human() {
        let mut report = base_report(vec![needs_human_high()]);
        report.summary = Summary::from_findings(
            &[needs_human_high()],
            1,
            1,
            GatePolicy {
                include_needs_human: true,
            },
            &[],
        );
        assert_eq!(exit_code(&report, Severity::High), 1);
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("待人工判斷：1 筆"));
        assert!(text.contains("已計入上方統計與 fail-on"));
        assert!(text.contains("High: 1"));
        assert!(text.contains("未通過"));
    }

    #[test]
    fn render_summary_shows_coverage_verifier_and_prior_in_deep_profile() {
        let mut report = base_report(vec![]);
        report.profile.mode = ProfileMode::Deep;
        report.summary.coverage = CoverageSummary {
            modules_total: 7,
            covered: 2,
            candidate: 1,
            deferred: 3,
            failed: 1,
            partial: true,
        };
        report.verifier = VerifierInfo {
            independence: VerifierIndependence::DifferentAgent,
            audit_agent: Some("auditor".into()),
            verifier_agent: Some("checker".into()),
        };
        report.prior_run = Some(PriorRunInfo {
            report_file: "20261001T000000Z.json".into(),
            carried_suppressed: 1,
            carried_refuted: 2,
            revalidated_prior_confirmed: 0,
            changed_source: 4,
        });
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("模組覆蓋：共 7"));
        assert!(text.contains("部分覆蓋：4 模組未審"));
        assert!(text.contains("獨立 agent 覆核（審計：auditor；覆核：checker）"));
        assert!(text.contains("20261001T000000Z.json"));
        assert!(text.contains("原始碼已變更 4"));

        report.prior_run = None;
        report.verifier = VerifierInfo::default();
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("先前報告承接：無"));
        assert!(text.contains("未執行覆核"));
    }

    #[test]
    fn render_summary_marks_incomplete_runs() {
        let mut report = base_report(vec![]);
        report.run_status = RunStatus::Incomplete;
        report.incomplete_reason =
            Some(duduclaw_core::llm_contract::coverage::IncompleteReason::EngineUnavailable);
        let text = render_summary(&report, Severity::High);
        assert!(text.contains("執行狀態：未完成（AI 引擎無法使用）"));
    }

    #[test]
    fn write_and_save_refuse_an_invalid_report() {
        let dir = tempfile::tempdir().unwrap();
        let mut report = base_report(vec![]);
        report.schema_version = 1;
        let path = dir.path().join("r.json");
        let err = write_json_report(&report, &path).unwrap_err();
        assert!(err.to_string().contains("schema_version is 1"));
        assert!(!path.exists());
        let home = tempfile::tempdir().unwrap();
        assert!(save_report(home.path(), &report).is_err());
        assert!(!home.path().join("secaudit").exists());
    }

    #[test]
    fn total_line_shows_three_buckets_from_the_findings() {
        let mut refuted = needs_human_high();
        refuted.status = FindingStatus::Refuted;
        refuted.title = "other".into();
        refuted.file = "g".into();
        let scanner_hit = Finding::candidate(
            "semgrep",
            FindingKind::StaticAnalysis,
            Severity::Low,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        let findings = vec![needs_human_high(), needs_human_high(), refuted, scanner_hit];
        let mut report = base_report(findings.clone());
        let text = render_summary(&report, Severity::High);
        assert!(
            text.contains("總計：4（計入 fail-on 1、待人工判斷 2、已證偽/已壓制 1）"),
            "{text}"
        );
        assert!(text.contains("永遠不計入嚴重度統計與 fail-on 判定"));

        // Live-run regression: 2 NeedsHuman, nothing refuted.
        let only_nh = base_report(vec![needs_human_high(), needs_human_high()]);
        let text = render_summary(&only_nh, Severity::High);
        assert!(
            text.contains("總計：2（計入 fail-on 0、待人工判斷 2、已證偽/已壓制 0）"),
            "{text}"
        );

        // Flag on: NeedsHuman is gated.
        report.summary = Summary::from_findings(
            &findings,
            1,
            1,
            GatePolicy {
                include_needs_human: true,
            },
            &[],
        );
        let text = render_summary(&report, Severity::High);
        assert!(
            text.contains("總計：4（計入 fail-on 3、待人工判斷 0、已證偽/已壓制 1）"),
            "{text}"
        );
    }
}
