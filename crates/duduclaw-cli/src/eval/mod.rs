//! `duduclaw eval` — harness-level agent behavior eval / regression suite.
//!
//! Runs `evals/<suite>/<case>.toml` cases (ADK-evalset / Braintrust
//! eval-action pattern, adapted): each case sends one prompt to an agent
//! through the same CLI harness invocation the gateway uses, parses the
//! stream-json transcript, and checks
//!
//! 1. deterministic `[expect]` assertions (tool_use / final-text signals —
//!    zero LLM cost, replayable offline in CI via `--replay`), and
//! 2. an optional `[judge]` LLM rubric (reuses the RFC-26 fork-judge
//!    `LlmCaller` plumbing, backed by the gateway utility runtime).
//!
//! Exit code is non-zero when any case fails, so CI can gate on it.

mod assertions;
mod case;
mod judge;
mod runner;
mod transcript;

use std::path::{Path, PathBuf};

use console::style;
use duduclaw_fork::judge::LlmCaller;

use assertions::AssertionResult;
use runner::RunMode;

/// Flags from the `duduclaw eval` subcommand.
pub struct EvalOptions {
    /// Case file or suite directory (default `./evals`).
    pub path: Option<PathBuf>,
    /// Only run cases whose `[case] name` contains this substring.
    pub filter: Option<String>,
    /// Replay recorded transcripts instead of live agent runs.
    pub replay: bool,
    /// Record live transcripts next to each case for future `--replay`.
    pub record: bool,
    /// Skip the LLM judge even when a case enables it.
    pub no_judge: bool,
    /// Write a JSON report to this path.
    pub report: Option<PathBuf>,
    /// Precise `EvalCaseRef` selection: the case file's filename stem
    /// (`--case`, repeatable / comma-separated). Unlike `--filter` (a
    /// substring match on the human-readable `[case] name`, which is not
    /// guaranteed unique — B4) this is an exact match against the stable
    /// per-file id. Empty ⇒ no restriction.
    pub case: Vec<String>,
    /// Directory names to exclude from discovery (e.g. `held-out`), matched
    /// against path components relative to the discovery root. Empty ⇒
    /// include everything (current behavior, unchanged).
    pub exclude_dir: Vec<String>,
}

/// Judge outcome attached to a case report.
#[derive(serde::Serialize)]
struct JudgeOutcome {
    passed: bool,
    score: f64,
    min_score: f64,
    rationale: String,
}

/// Full result for one case.
#[derive(serde::Serialize)]
struct CaseReport {
    /// Stable `EvalCaseRef` id — the case file's filename stem (B4).
    id: String,
    name: String,
    path: String,
    passed: bool,
    /// Fatal error before assertions could run (load/spawn/parse failure).
    error: Option<String>,
    assertions: Vec<AssertionResult>,
    judge: Option<JudgeOutcome>,
    /// Observed tool calls (`name {input-preview}`), in order.
    tool_calls: Vec<String>,
    diagnostics: Option<String>,
    duration_ms: u128,
}

/// Entry point for the `Eval` subcommand.
pub async fn cmd_eval(home: &Path, opts: EvalOptions) -> duduclaw_core::error::Result<()> {
    let judge_caller = judge::GatewayJudgeCaller {
        home_dir: home.to_path_buf(),
    };
    let reports = run_eval(home, &opts, &judge_caller).await;
    render(&reports, &opts)?;

    let failed = reports.iter().filter(|r| !r.passed).count();
    if failed > 0 {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "{failed} of {} eval case(s) failed",
            reports.len()
        )));
    }
    Ok(())
}

/// Core loop, judge injected for testability. Cases run sequentially:
/// deterministic order, and live runs must not contend for the operator's
/// account quota.
async fn run_eval(home: &Path, opts: &EvalOptions, judge_caller: &dyn LlmCaller) -> Vec<CaseReport> {
    let root = opts
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from("evals"));
    let mode = if opts.replay {
        RunMode::Replay
    } else {
        RunMode::Live {
            record: opts.record,
        }
    };

    let mut case_paths = match case::discover_cases(&root) {
        Ok(p) => p,
        Err(e) => {
            return vec![CaseReport {
                id: "<discovery>".into(),
                name: "<discovery>".into(),
                path: root.display().to_string(),
                passed: false,
                error: Some(e),
                assertions: Vec::new(),
                judge: None,
                tool_calls: Vec::new(),
                diagnostics: None,
                duration_ms: 0,
            }]
        }
    };

    // `--exclude-dir` (B4): drop any case whose path — relative to the
    // discovery root — has a directory component matching an excluded name
    // (e.g. `held-out`). Applied before the uniqueness check and `--case`
    // filter so excluded cases never participate in either.
    if !opts.exclude_dir.is_empty() {
        case_paths.retain(|p| !path_excluded(&root, p, &opts.exclude_dir));
    }

    // Suite-wide `EvalCaseRef` uniqueness (B4): the filename stem is the
    // stable case id. A collision makes `--case` ambiguous and silently
    // shadows one case's results with another's, so the whole suite fails
    // fast with the offending pair named — never a partial silent run.
    {
        let mut seen: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
        for p in &case_paths {
            let id = case_id(p);
            if let Some(prev) = seen.insert(id.clone(), p.clone()) {
                return vec![CaseReport {
                    id: id.clone(),
                    name: "<duplicate-case-id>".into(),
                    path: root.display().to_string(),
                    passed: false,
                    error: Some(format!(
                        "duplicate case id {id:?} (filename stem must be unique across the suite): {} and {}",
                        prev.display(),
                        p.display()
                    )),
                    assertions: Vec::new(),
                    judge: None,
                    tool_calls: Vec::new(),
                    diagnostics: None,
                    duration_ms: 0,
                }];
            }
        }
    }

    // `--case` (B4): exact `EvalCaseRef` selection, evaluated against the
    // filename stem before any TOML parsing — unlike `--filter` this never
    // needs to load a case to decide whether to run it.
    if !opts.case.is_empty() {
        let wanted: std::collections::HashSet<&str> = opts.case.iter().map(String::as_str).collect();
        case_paths.retain(|p| wanted.contains(case_id(p).as_str()));
    }

    let mut reports = Vec::new();
    for path in case_paths {
        let started = std::time::Instant::now();
        let id = case_id(&path);
        let loaded = case::load_case(&path);
        let case_file = match loaded {
            Ok(c) => c,
            Err(e) => {
                reports.push(CaseReport {
                    id,
                    name: path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("<unnamed>")
                        .to_string(),
                    path: path.display().to_string(),
                    passed: false,
                    error: Some(e),
                    assertions: Vec::new(),
                    judge: None,
                    tool_calls: Vec::new(),
                    diagnostics: None,
                    duration_ms: started.elapsed().as_millis(),
                });
                continue;
            }
        };

        if let Some(f) = &opts.filter {
            if !case_file.case.name.contains(f.as_str()) {
                continue;
            }
        }

        reports.push(run_one(&path, &id, &case_file, home, mode, opts.no_judge, judge_caller).await);
    }
    reports
}

/// The stable `EvalCaseRef` id for a case file: its filename stem. Matches
/// the existing `commercial/evals/` convention (360 shipped cases, each
/// filename already globally unique) — `[case] name` stays the human-readable
/// title, never the identity.
fn case_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("<unnamed>")
        .to_string()
}

/// True when `path` (relative to `root`) has a directory component matching
/// one of `exclude_dirs` by exact name. Falls back to matching against the
/// absolute path's components when `path` doesn't start with `root` (e.g. a
/// caller-supplied absolute path outside the walked root).
fn path_excluded(root: &Path, path: &Path, exclude_dirs: &[String]) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| {
        matches!(c, std::path::Component::Normal(os) if os
            .to_str()
            .map(|s| exclude_dirs.iter().any(|d| d == s))
            .unwrap_or(false))
    })
}

async fn run_one(
    path: &Path,
    id: &str,
    case_file: &case::EvalCaseFile,
    home: &Path,
    mode: RunMode,
    no_judge: bool,
    judge_caller: &dyn LlmCaller,
) -> CaseReport {
    let started = std::time::Instant::now();
    let mut report = CaseReport {
        id: id.to_string(),
        name: case_file.case.name.clone(),
        path: path.display().to_string(),
        passed: false,
        error: None,
        assertions: Vec::new(),
        judge: None,
        tool_calls: Vec::new(),
        diagnostics: None,
        duration_ms: 0,
    };

    let transcript = match runner::obtain_transcript(path, case_file, home, mode).await {
        Ok(t) => t,
        Err(e) => {
            report.error = Some(e);
            report.duration_ms = started.elapsed().as_millis();
            return report;
        }
    };
    report.diagnostics = Some(transcript.diagnostics());
    report.tool_calls = transcript
        .tool_uses
        .iter()
        .map(|u| {
            format!(
                "{} {}",
                u.name,
                duduclaw_core::truncate_chars(&u.input.to_string(), 120)
            )
        })
        .collect();

    report.assertions = assertions::run_assertions(&case_file.expect, &transcript);
    let assertions_ok = report.assertions.iter().all(|a| a.passed);

    // Judge only when configured, enabled, and not suppressed. A judge
    // failure (LLM down, garbage response) fails the case — fail closed.
    if let Some(spec) = case_file.judge.as_ref().filter(|j| j.enabled && !no_judge) {
        match judge::judge_output(
            judge_caller,
            &spec.rubric,
            &case_file.case.prompt,
            &transcript.final_text,
        )
        .await
        {
            Ok(verdict) => {
                report.judge = Some(JudgeOutcome {
                    passed: verdict.score >= spec.min_score,
                    score: verdict.score,
                    min_score: spec.min_score,
                    rationale: verdict.rationale,
                });
            }
            Err(e) => {
                report.judge = Some(JudgeOutcome {
                    passed: false,
                    score: 0.0,
                    min_score: spec.min_score,
                    rationale: format!("judge error (fail closed): {e}"),
                });
            }
        }
    }

    let judge_ok = report.judge.as_ref().map(|j| j.passed).unwrap_or(true);
    report.passed = assertions_ok && judge_ok;
    report.duration_ms = started.elapsed().as_millis();
    report
}

/// Console + optional JSON report rendering (style mirrors `duduclaw test`).
fn render(reports: &[CaseReport], opts: &EvalOptions) -> duduclaw_core::error::Result<()> {
    println!();
    println!("  {} {}", style("🧪").bold(), style("Agent Behavior Eval").bold());
    println!();

    for r in reports {
        let icon = if r.passed {
            style("PASS").green().bold()
        } else {
            style("FAIL").red().bold()
        };
        println!(
            "  [{icon}] {}  {}",
            r.name,
            style(format!("({} ms)", r.duration_ms)).dim()
        );
        println!("         {}", style(&r.path).dim());
        if let Some(e) = &r.error {
            println!("         {} {e}", style("error:").red());
        }
        for a in &r.assertions {
            let mark = if a.passed { style("ok").green() } else { style("FAIL").red() };
            println!("         [{mark}] {} — {}", a.name, a.detail);
        }
        if let Some(j) = &r.judge {
            let mark = if j.passed { style("ok").green() } else { style("FAIL").red() };
            println!(
                "         [{mark}] judge score {:.2} (min {:.2}) — {}",
                j.score, j.min_score, j.rationale
            );
        }
        if !r.passed && !r.tool_calls.is_empty() {
            println!("         {}", style("tool calls:").dim());
            for tc in &r.tool_calls {
                println!("           {}", style(tc).dim());
            }
        }
        if let Some(d) = &r.diagnostics {
            println!("         {}", style(d).dim());
        }
        println!();
    }

    let total = reports.len();
    let passed = reports.iter().filter(|r| r.passed).count();
    println!("  {}", style("─".repeat(50)).dim());
    println!(
        "  Results: {} passed, {} failed (out of {})",
        style(passed).green().bold(),
        style(total - passed).red().bold(),
        total,
    );
    println!();

    // ── MAST failure-taxonomy breakdown (R3) ─────────────────
    // Attribute every failure deterministically onto a MAST mode / infra /
    // unclassified label (arXiv:2503.13657). Only rendered when failures
    // exist so a green run stays quiet.
    let mast_breakdown = mast_breakdown(reports);
    if !mast_breakdown.is_empty() {
        println!("  {}", style("MAST failure breakdown").bold());
        for (label, count) in &mast_breakdown {
            println!("    {} × {}", style(count).yellow().bold(), label);
        }
        println!();
    }

    if let Some(report_path) = &opts.report {
        let suite = opts
            .path
            .as_deref()
            .unwrap_or_else(|| Path::new("evals"))
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("evals")
            .to_string();
        let json = serde_json::json!({
            // WP2.2 machine contract (`duduclaw-gateway::eval_runner::EvalReport`):
            // {suite, total, passed, per_case: [{id, passed, failed_assertions, ...}]}.
            // `suite`/`per_case` are additive to the pre-existing human/CI
            // fields below — no existing consumer's keys are removed.
            "suite": suite,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "mode": if opts.replay { "replay" } else { "live" },
            "total": total,
            "passed": passed,
            "failed": total - passed,
            "mast_breakdown": mast_breakdown.iter().map(|(label, count)| serde_json::json!({
                "label": label,
                "count": count,
            })).collect::<Vec<_>>(),
            "per_case": per_case_json(reports),
            "cases": reports,
        });
        std::fs::write(report_path, serde_json::to_string_pretty(&json)? + "\n")?;
        println!("  Report written to {}", report_path.display());
        println!();
    }
    Ok(())
}

/// Per-case machine contract for `--report` (WP2.2's `EvalRunner` parses
/// exactly this shape). Every field is sourced from data the case run
/// already produced — nothing here is fabricated. `failed_assertions` names
/// each failed `[expect]` check (`AssertionResult::name`, already a
/// descriptive string e.g. `"must_use_tools: tasks_create"`); a case that
/// died before assertions ran (spawn/parse/replay failure) reports its
/// `error` as the sole entry so `failed_assertions` is never empty on a
/// failure. `mast_class` is `None` for a passing case.
fn per_case_json(reports: &[CaseReport]) -> Vec<serde_json::Value> {
    use duduclaw_gateway::mast;
    reports
        .iter()
        .map(|r| {
            let failed_assertions: Vec<String> = if !r.assertions.is_empty() {
                r.assertions
                    .iter()
                    .filter(|a| !a.passed)
                    .map(|a| a.name.clone())
                    .collect()
            } else if let Some(e) = &r.error {
                vec![format!("error: {e}")]
            } else {
                Vec::new()
            };
            let judge_score = r.judge.as_ref().map(|j| j.score);
            let mast_class = if r.passed {
                None
            } else if let Some(e) = &r.error {
                Some(mast::classify_eval_error(e).display())
            } else {
                let first_failed = r
                    .assertions
                    .iter()
                    .find(|a| !a.passed)
                    .map(|a| mast::classify_eval_assertion(&a.name).display());
                Some(first_failed.unwrap_or_else(|| mast::MastLabel::Unclassified.display()))
            };
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "passed": r.passed,
                "failed_assertions": failed_assertions,
                "judge_score": judge_score,
                "mast_class": mast_class,
            })
        })
        .collect()
}

/// Deterministically attribute each failure onto a MAST label
/// (arXiv:2503.13657), returning `(display, count)` pairs sorted by count
/// desc then label. A case that died before assertions ran is `infra`; each
/// failed deterministic assertion maps via `mast::classify_eval_assertion`.
/// A judge-only failure (assertions all passed) is `unclassified` — the LLM
/// rubric is semantic, outside the deterministic table.
fn mast_breakdown(reports: &[CaseReport]) -> Vec<(String, usize)> {
    use duduclaw_gateway::mast;
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for r in reports.iter().filter(|r| !r.passed) {
        if let Some(err) = &r.error {
            let label = mast::classify_eval_error(err);
            *counts.entry(label.display()).or_insert(0) += 1;
            continue;
        }
        let mut attributed = false;
        for a in r.assertions.iter().filter(|a| !a.passed) {
            let label = mast::classify_eval_assertion(&a.name);
            *counts.entry(label.display()).or_insert(0) += 1;
            attributed = true;
        }
        if !attributed {
            // Failed with all deterministic assertions passing ⇒ judge failure.
            *counts
                .entry(mast::MastLabel::Unclassified.display())
                .or_insert(0) += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    fn report(name: &str, passed: bool, error: Option<&str>, assertions: Vec<(&str, bool)>) -> CaseReport {
        CaseReport {
            id: name.into(),
            name: name.into(),
            path: "p".into(),
            passed,
            error: error.map(String::from),
            assertions: assertions
                .into_iter()
                .map(|(n, p)| AssertionResult {
                    name: n.into(),
                    passed: p,
                    detail: String::new(),
                })
                .collect(),
            judge: None,
            tool_calls: vec![],
            diagnostics: None,
            duration_ms: 0,
        }
    }

    #[test]
    fn mast_breakdown_attributes_failures() {
        let reports = vec![
            report("ok", true, None, vec![("must_use_tools: x", true)]),
            report("spec-fail", false, None, vec![("must_use_tools: tasks_create", false)]),
            report("spec-fail-2", false, None, vec![("output_contains: \"x\"", false)]),
            report("infra", false, Some("spawn failed"), vec![]),
            report("judge-only", false, None, vec![("output_contains: \"x\"", true)]),
        ];
        let bd = mast_breakdown(&reports);
        // Two FM-1.1 spec failures, one infra, one unclassified (judge).
        let map: std::collections::HashMap<_, _> = bd.into_iter().collect();
        assert_eq!(map.get("FM-1.1 Disobey Task Specification"), Some(&2));
        assert_eq!(map.get("infra (outside MAST scope)"), Some(&1));
        assert_eq!(map.get("unclassified"), Some(&1));
    }

    #[test]
    fn mast_breakdown_empty_when_all_pass() {
        let reports = vec![report("ok", true, None, vec![])];
        assert!(mast_breakdown(&reports).is_empty());
    }

    struct StubJudge(&'static str);
    #[async_trait]
    impl LlmCaller for StubJudge {
        async fn complete(&self, _prompt: &str) -> duduclaw_fork::Result<String> {
            Ok(self.0.to_string())
        }
    }

    const TRANSCRIPT: &str = concat!(
        "{\"type\":\"assistant\",\"message\":{\"content\":[",
        "{\"type\":\"tool_use\",\"name\":\"mcp__duduclaw__tasks_create\",\"input\":{}}]}}\n",
        "{\"type\":\"assistant\",\"message\":{\"content\":[",
        "{\"type\":\"text\",\"text\":\"Refund approved for order #1234.\"}]}}\n",
        "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}\n",
    );

    fn write_suite(dir: &Path, expect: &str, judge: &str) -> PathBuf {
        let case = format!(
            "[case]\nname = \"refund-flow\"\nagent = \"support-bot\"\nprompt = \"refund order 1234\"\n\n{expect}{judge}"
        );
        std::fs::write(dir.join("refund-flow.toml"), case).unwrap();
        std::fs::write(dir.join("refund-flow.transcript.jsonl"), TRANSCRIPT).unwrap();
        dir.to_path_buf()
    }

    fn opts(root: &Path) -> EvalOptions {
        EvalOptions {
            path: Some(root.to_path_buf()),
            filter: None,
            replay: true,
            record: false,
            no_judge: false,
            report: None,
            case: Vec::new(),
            exclude_dir: Vec::new(),
        }
    }

    #[tokio::test]
    async fn replay_suite_passes_end_to_end_with_judge() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\nmust_use_tools = [\"tasks_create\"]\nmust_not_use_tools = [\"Bash\"]\noutput_contains = [\"order #1234\"]\n\n",
            "[judge]\nrubric = \"acknowledges the refund\"\nmin_score = 0.5\n",
        );
        let judge = StubJudge("{\"score\": 0.9, \"rationale\": \"ok\"}");
        let reports = run_eval(dir.path(), &opts(&root), &judge).await;
        assert_eq!(reports.len(), 1);
        assert!(reports[0].passed, "error={:?}", reports[0].error);
        assert!(reports[0].judge.as_ref().unwrap().passed);
    }

    #[tokio::test]
    async fn failing_assertion_fails_case() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\nmust_use_tools = [\"Bash\"]\n\n",
            "",
        );
        let judge = StubJudge("unused");
        let reports = run_eval(dir.path(), &opts(&root), &judge).await;
        assert!(!reports[0].passed);
        assert!(reports[0].error.is_none());
    }

    #[tokio::test]
    async fn judge_below_min_score_fails_case_and_no_judge_skips() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "[judge]\nrubric = \"perfect\"\nmin_score = 0.95\n",
        );
        let judge = StubJudge("{\"score\": 0.4, \"rationale\": \"weak\"}");
        let reports = run_eval(dir.path(), &opts(&root), &judge).await;
        assert!(!reports[0].passed);

        let mut o = opts(&root);
        o.no_judge = true;
        let reports = run_eval(dir.path(), &o, &judge).await;
        assert!(reports[0].passed);
        assert!(reports[0].judge.is_none());
    }

    #[tokio::test]
    async fn garbage_judge_response_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "",
            "[judge]\nrubric = \"anything\"\nmin_score = 0.1\n",
        );
        let judge = StubJudge("i refuse to emit json");
        let reports = run_eval(dir.path(), &opts(&root), &judge).await;
        assert!(!reports[0].passed);
        let j = reports[0].judge.as_ref().unwrap();
        assert!(!j.passed);
        assert!(j.rationale.contains("fail closed"));
    }

    #[tokio::test]
    async fn filter_skips_nonmatching_cases_and_bad_case_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(dir.path(), "[expect]\noutput_contains = [\"x\"]\n\n", "");
        std::fs::write(root.join("broken.toml"), "not valid toml [").unwrap();

        let judge = StubJudge("unused");
        let mut o = opts(&root);
        o.filter = Some("no-such-case".into());
        let reports = run_eval(dir.path(), &o, &judge).await;
        // The broken file fails at load (before name filtering) and must
        // surface — a corrupt suite should never silently pass CI.
        assert_eq!(reports.len(), 1);
        assert!(!reports[0].passed);
        assert!(reports[0].error.as_ref().unwrap().contains("parse"));
    }

    /// The shipped `evals/examples/` files must always load, and the
    /// replay-ready examples must pass end-to-end offline.
    #[tokio::test]
    async fn shipped_examples_stay_valid_and_replayable() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../evals/examples");

        case::load_case(&examples.join("refund-flow.toml")).unwrap();
        case::load_case(&examples.join("greeting-replay.toml")).unwrap();
        case::load_case(&examples.join("grounded-replay.toml")).unwrap();

        let judge = StubJudge("unused");
        let o = EvalOptions {
            path: Some(examples.join("greeting-replay.toml")),
            filter: None,
            replay: true,
            record: false,
            no_judge: true,
            report: None,
            case: Vec::new(),
            exclude_dir: Vec::new(),
        };
        let reports = run_eval(&examples, &o, &judge).await;
        assert_eq!(reports.len(), 1);
        assert!(
            reports[0].passed,
            "shipped replay example failed: error={:?} assertions={:?}",
            reports[0].error, reports[0].assertions
        );
        assert_eq!(reports[0].tool_calls.len(), 1);
    }

    /// WP4 GroundEval: the shipped `grounded-replay` example must pass its
    /// `[[expect.grounded]]` assertion end-to-end offline (regression guard
    /// for the transcript pairing + assertion logic together, not just each
    /// in isolation).
    #[tokio::test]
    async fn shipped_grounded_example_stays_replayable() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../evals/examples");
        let judge = StubJudge("unused");
        let o = EvalOptions {
            path: Some(examples.join("grounded-replay.toml")),
            filter: None,
            replay: true,
            record: false,
            no_judge: true,
            report: None,
            case: Vec::new(),
            exclude_dir: Vec::new(),
        };
        let reports = run_eval(&examples, &o, &judge).await;
        assert_eq!(reports.len(), 1);
        assert!(
            reports[0].passed,
            "shipped grounded example failed: error={:?} assertions={:?}",
            reports[0].error, reports[0].assertions
        );
        assert!(reports[0]
            .assertions
            .iter()
            .any(|a| a.name.starts_with("grounded:") && a.passed));
    }

    #[tokio::test]
    async fn discovery_failure_is_one_failed_report() {
        let dir = tempfile::tempdir().unwrap();
        let judge = StubJudge("unused");
        let o = EvalOptions {
            path: Some(dir.path().join("missing")),
            filter: None,
            replay: true,
            record: false,
            no_judge: true,
            report: None,
            case: Vec::new(),
            exclude_dir: Vec::new(),
        };
        let reports = run_eval(dir.path(), &o, &judge).await;
        assert_eq!(reports.len(), 1);
        assert!(!reports[0].passed);
    }

    // ── B4: EvalCaseRef (filename-stem ids), --case, --exclude-dir ─────

    /// Add a second, independent case to a suite directory already seeded by
    /// `write_suite` (its case stays `refund-flow`).
    fn add_case(dir: &Path, stem: &str) {
        let case = format!(
            "[case]\nname = \"{stem}-name\"\nagent = \"support-bot\"\nprompt = \"hi\"\n\n[expect]\noutput_contains = [\"Refund approved\"]\n"
        );
        std::fs::write(dir.join(format!("{stem}.toml")), case).unwrap();
        std::fs::write(dir.join(format!("{stem}.transcript.jsonl")), TRANSCRIPT).unwrap();
    }

    #[tokio::test]
    async fn case_flag_selects_by_filename_stem_not_display_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(dir.path(), "[expect]\noutput_contains = [\"order #1234\"]\n\n", "");
        add_case(dir.path(), "upsell-001");

        let mut o = opts(&root);
        o.case = vec!["upsell-001".to_string()];
        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        assert_eq!(reports.len(), 1, "only the requested case id runs");
        assert_eq!(reports[0].id, "upsell-001");
        // `--case` matches the filename stem, not `[case] name` (which is
        // "upsell-001-name" here) — proves it's id-based, not name-based.
        assert_eq!(reports[0].name, "upsell-001-name");
    }

    #[tokio::test]
    async fn case_flag_accepts_multiple_ids() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(dir.path(), "[expect]\noutput_contains = [\"order #1234\"]\n\n", "");
        add_case(dir.path(), "upsell-001");
        add_case(dir.path(), "upsell-002");

        let mut o = opts(&root);
        o.case = vec!["refund-flow".to_string(), "upsell-002".to_string()];
        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        let mut ids: Vec<&str> = reports.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["refund-flow", "upsell-002"]);
    }

    #[tokio::test]
    async fn exclude_dir_skips_matching_subdirectory_default_includes_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(dir.path(), "[expect]\noutput_contains = [\"order #1234\"]\n\n", "");
        let held_out = dir.path().join("held-out");
        std::fs::create_dir(&held_out).unwrap();
        add_case(&held_out, "heldout-001");

        // Default (no --exclude-dir): both cases run — current behavior unchanged.
        let reports = run_eval(dir.path(), &opts(&root), &StubJudge("unused")).await;
        assert_eq!(reports.len(), 2, "held-out is discovered by default");

        // --exclude-dir held-out: only the top-level case runs.
        let mut o = opts(&root);
        o.exclude_dir = vec!["held-out".to_string()];
        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].id, "refund-flow");
    }

    #[tokio::test]
    async fn duplicate_filename_stem_fails_the_whole_suite() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(dir.path(), "[expect]\noutput_contains = [\"order #1234\"]\n\n", "");
        // A same-named case nested under a subdirectory: same stable id
        // (`refund-flow`) as the top-level case — must be rejected, not
        // silently shadow one result with the other's.
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        add_case(&nested, "refund-flow");

        let reports = run_eval(dir.path(), &opts(&root), &StubJudge("unused")).await;
        assert_eq!(reports.len(), 1, "suite fails fast as a single report, no partial run");
        assert!(!reports[0].passed);
        let err = reports[0].error.as_ref().expect("error must be set");
        assert!(err.contains("duplicate case id"), "unexpected error: {err}");
        assert!(err.contains("refund-flow"), "error should name the colliding id: {err}");
    }

    #[tokio::test]
    async fn report_json_carries_suite_and_per_case_machine_contract() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\nmust_use_tools = [\"Bash\"]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
        let report_path = dir.path().join("report.json");
        let mut o = opts(&root);
        o.report = Some(report_path.clone());

        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        assert!(!reports[0].passed, "must_use_tools = Bash never fires in TRANSCRIPT");
        render(&reports, &o).unwrap();

        let raw = std::fs::read_to_string(&report_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(json["suite"], serde_json::json!(root.file_name().unwrap().to_str().unwrap()));
        assert_eq!(json["total"], serde_json::json!(1));
        assert_eq!(json["passed"], serde_json::json!(0));

        let per_case = json["per_case"].as_array().unwrap();
        assert_eq!(per_case.len(), 1);
        let case = &per_case[0];
        assert_eq!(case["id"], serde_json::json!("refund-flow"));
        assert_eq!(case["name"], serde_json::json!("refund-flow"));
        assert_eq!(case["passed"], serde_json::json!(false));
        let failed = case["failed_assertions"].as_array().unwrap();
        assert!(
            failed.iter().any(|v| v.as_str().unwrap().contains("must_use_tools")),
            "failed_assertions should name the failing check: {failed:?}"
        );
        assert!(case["mast_class"].is_string(), "a failed case must carry a mast_class");
        assert!(case["judge_score"].is_null(), "no [judge] configured for this case");
    }
}
