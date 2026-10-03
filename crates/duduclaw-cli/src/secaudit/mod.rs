//! `duduclaw secaudit` — deterministic intake + OSS scanner orchestration,
//! AI deep audit, adversarial re-verification, and (opt-in) sandboxed PoC
//! for DESIGN-code-security-audit-2026-08 §3.2 (all six steps except §3.2
//! step 6 "規則沉澱", a later wave).
//!
//! Pipeline for one invocation:
//! 1. Validate + canonicalize the repo path (bad path ⇒ exit 2, infra error).
//! 2. Run every applicable OSS scanner ([`scanners::run_all`]) — always,
//!    regardless of `--profile`.
//! 3. `--profile deep` additionally runs, in order:
//!    - a: the intake/threat-model pass ([`intake::build_profile`]) —
//!      language census, entry points, git hotspots;
//!    - b: AI deep audit ([`ai_audit::run_ai_audit`]) — up to `--max-modules`
//!      (default 5) module-level LLM calls producing `Candidate` findings;
//!    - c: adversarial re-verification ([`adversarial::review_all`]) — one
//!      zero-shared-context LLM call per ai_audit candidate, settling each
//!      to `Refuted` or `NeedsHuman` (never auto-`Confirmed`);
//!    - d: with `--poc`, sandboxed PoC generation ([`poc::maybe_run_poc`])
//!      for every `NeedsHuman` finding at severity ≥ High (拍板 D3).
//!
//!    `--profile quick` (default) skips all of 3a-3d.
//! 4. Assemble an [`schema::AuditReport`], print a zh-TW summary, optionally
//!    write the full JSON to `--report <path>` and/or a timestamped copy
//!    under `<home>/secaudit/reports/` via `--save`.
//! 5. Exit code: `0` no finding meets `--fail-on` (default `high`) — this is
//!    also what a machine with every scanner/LLM missing reports, honestly,
//!    per the task's dogfood requirement; `1` at least one does; `2` an
//!    infra error prevented the scan itself from completing (never "a
//!    scanner/the LLM wasn't available", which is an ordinary,
//!    honestly-reported outcome).
//!
//! v2 (DESIGN-llm-contract-secaudit-v2 §3): every LLM reply is parsed with
//! `llm_contract::strict_json` (contract violation ⇒ discarded, never
//! repaired); ai_audit candidates carry a threat model, a trace and
//! conditions and go through a zero-LLM pre-check (`precheck.rs`); every
//! ranked module gets a coverage entry (`coverage.rs`); statuses on
//! unchanged source are carried from the newest prior report
//! (`prior_run.rs`, before adversarial review); the verifier can run under a
//! different agent (`--verifier-agent`, recorded as `verifier.independence`);
//! `NeedsHuman` findings no longer count toward `--fail-on` unless
//! `--fail-on-needs-human`; the run ends `complete` or `incomplete` with a
//! reason; and the report validator (`validator.rs`) runs before anything is
//! written (violation ⇒ exit 2).
//!
//! AI runtime routing (拍板 D2): no model is ever hardcoded. `--agent <id>`
//! routes steps 3b-3d through that agent's `[runtime]` config
//! ([`llm_util::resolve_agent_dir`] → `run_utility_prompt`'s `agent_dir`
//! path); omitting it uses the global `config.toml [runtime]` utility
//! provider/model — the same choke-point every other internal LLM caller in
//! this codebase uses.

mod adversarial;
mod ai_audit;
mod coverage;
mod intake;
mod llm_util;
mod poc;
mod precheck;
mod prior_run;
mod prompts;
mod report;
mod scanners;
mod schema;
mod validator;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use duduclaw_core::llm_contract::coverage::{IncompleteReason, RunStatus};

use schema::{
    AuditReport, CURRENT_SCHEMA_VERSION, EngineMissing, GatePolicy, ModuleCoverage, ProfileMode,
    ScanProfile, Severity, Summary, VerifierIndependence, VerifierInfo,
};

/// Flags from the `duduclaw secaudit` subcommand.
pub struct SecauditOptions {
    pub repo_path: PathBuf,
    /// Raw `--profile` value (`"quick"` | `"deep"`), parsed inside
    /// `cmd_secaudit` so an invalid value maps to exit 2 with a clear message.
    pub profile: String,
    pub report: Option<PathBuf>,
    /// Raw `--fail-on` value (`"critical"|"high"|"medium"|"low"|"info"`).
    pub fail_on: String,
    /// `--agent <id>`: follow this agent's `[runtime]` config for steps
    /// 3b-3d (拍板 D2). `None` ⇒ global `config.toml [runtime]`.
    pub agent: Option<String>,
    /// `--max-modules`: how many of the ranked modules step 3b (AI deep
    /// audit) sends to the model — the primary cost guard. The rest are
    /// recorded as `deferred` coverage.
    pub max_modules: usize,
    /// `--poc`: explicitly enable step 3d (sandboxed PoC generation +
    /// execution). Off by default (拍板 D3).
    pub poc: bool,
    /// `--save`: additionally write a timestamped copy of the report under
    /// `<home>/secaudit/reports/`.
    pub save: bool,
}

/// v2 flags (DESIGN-llm-contract-secaudit-v2 §3.4 items 7, 8, 10). Kept out
/// of [`SecauditOptions`] so existing struct literals stay source
/// compatible.
///
/// Intended clap shape on `MaintenanceCommands::Secaudit`:
///
/// ```text
/// /// Also count NeedsHuman findings (model self-reported severity) toward --fail-on.
/// #[arg(long)]
/// fail_on_needs_human: bool,
/// /// Run the adversarial verifier and PoC under this agent's [runtime]
/// /// (default: the --agent one). Recorded as verifier.independence.
/// #[arg(long, value_name = "ID")]
/// verifier_agent: Option<String>,
/// /// Do not carry statuses from the newest prior saved report.
/// #[arg(long)]
/// no_prior: bool,
/// ```
#[derive(Debug, Clone, Default)]
pub struct SecauditV2Flags {
    /// `--fail-on-needs-human` (D1=B).
    pub fail_on_needs_human: bool,
    /// `--verifier-agent <id>` (D2=B). `None` ⇒ the `--agent` one.
    pub verifier_agent: Option<String>,
    /// `--no-prior`: skip prior-report carry-over.
    pub no_prior: bool,
}

/// Entry point kept for existing callers: [`cmd_secaudit_v2`] with default
/// v2 flags. The CLI dispatch calls `cmd_secaudit_v2` directly, so outside
/// tests this wrapper has no caller.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn cmd_secaudit(home_dir: &Path, opts: SecauditOptions) -> i32 {
    cmd_secaudit_v2(home_dir, opts, SecauditV2Flags::default()).await
}

/// Agents the AI steps run under (resolved once, so the "agent not found"
/// warning prints at most once per id).
struct AgentSelection {
    audit_dir: Option<PathBuf>,
    verifier_dir: Option<PathBuf>,
    audit_id: Option<String>,
    verifier_id: Option<String>,
}

fn resolve_agents(
    home_dir: &Path,
    opts: &SecauditOptions,
    flags: &SecauditV2Flags,
) -> AgentSelection {
    let audit_dir = llm_util::resolve_agent_dir(home_dir, opts.agent.as_deref());
    let audit_id = audit_dir.as_ref().and(opts.agent.clone());
    let (verifier_dir, verifier_id) = match flags.verifier_agent.as_deref() {
        Some(v) if Some(v) != opts.agent.as_deref() => {
            let dir = llm_util::resolve_agent_dir(home_dir, Some(v));
            let id = dir.as_ref().map(|_| v.to_string());
            (dir, id)
        }
        _ => (audit_dir.clone(), audit_id.clone()),
    };
    AgentSelection {
        audit_dir,
        verifier_dir,
        audit_id,
        verifier_id,
    }
}

/// Entry point. Returns the process exit code directly (rather than
/// `Result<(), Error>`) because the task spec requires a specific 0/1/2
/// three-way contract that the generic "any Err ⇒ exit 1" wrapper in
/// `entry_point()` can't express — same pattern already used by
/// `ToolingCommands::DesktopRecordWorker` in lib.rs.
pub async fn cmd_secaudit_v2(
    home_dir: &Path,
    opts: SecauditOptions,
    flags: SecauditV2Flags,
) -> i32 {
    let profile_mode: ProfileMode = match opts.profile.parse() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[secaudit] 無效的 --profile：{e}");
            return 2;
        }
    };
    let fail_on: Severity = match opts.fail_on.parse() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[secaudit] 無效的 --fail-on：{e}");
            return 2;
        }
    };
    if opts.poc && profile_mode != ProfileMode::Deep {
        eprintln!(
            "[secaudit] 提示：--poc 需搭配 --profile deep 才會有作用（PoC 僅套用於 AI 深度審計＋對抗式覆核判定為 plausible 的候選）。"
        );
    }
    if flags.verifier_agent.is_some() && profile_mode != ProfileMode::Deep {
        eprintln!("[secaudit] 提示：--verifier-agent 需搭配 --profile deep 才會有作用。");
    }

    let repo_root = match std::fs::canonicalize(&opts.repo_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "[secaudit] 無法解析掃描目標路徑 {}：{e}",
                opts.repo_path.display()
            );
            return 2;
        }
    };
    if !repo_root.is_dir() {
        eprintln!("[secaudit] 掃描目標不是資料夾：{}", repo_root.display());
        return 2;
    }
    let repo_str = repo_root.to_string_lossy().to_string();

    let started_at = chrono::Utc::now().to_rfc3339();

    // Scanners always run, independent of profile — `--profile` only gates
    // the intake/threat-model pass + AI steps (task spec: "quick=只跑
    // scanners、deep=含 intake 熱點分析＋AI 深度審計").
    let (mut engines_run, mut engines_missing, mut findings) = scanners::run_all(&repo_root).await;
    normalize_scanner_paths(&repo_root, &mut findings);

    let intake_profile = if profile_mode == ProfileMode::Deep {
        Some(intake::build_profile(&repo_root).await)
    } else {
        None
    };

    let mut coverage: Vec<ModuleCoverage> = Vec::new();
    let mut incomplete_reason: Option<IncompleteReason> = None;
    let mut ai_ran = false;
    let agents = if profile_mode == ProfileMode::Deep {
        Some(resolve_agents(home_dir, &opts, &flags))
    } else {
        None
    };

    if let (Some(intake_profile), Some(agents)) = (intake_profile.as_ref(), agents.as_ref()) {
        let step = run_audit_step(home_dir, &repo_root, &opts, intake_profile, agents).await;
        engines_run.extend(step.engine_runs);
        engines_missing.extend(step.missing);
        findings.extend(step.findings);
        coverage = step.coverage;
        incomplete_reason = step.incomplete;
        ai_ran = step.ran;
    }

    let dropped = dedup_by_id(&mut findings);
    if dropped > 0 {
        eprintln!("[secaudit] 提示：{dropped} 筆完全相同（同一 id）的發現已合併為一筆。");
    }

    // Prior-run carry-over runs BEFORE adversarial review (§3.4 item 8):
    // carried Refuted/Suppressed findings never cost a verifier call.
    prior_run::fill_file_hashes(&repo_root, &mut findings);
    let prior_info = if flags.no_prior {
        None
    } else {
        prior_run::load_prior(home_dir, &repo_str)
            .map(|idx| prior_run::apply_prior(&mut findings, &idx))
    };

    let mut verifier_ran = false;
    if let (true, Some(agents)) = (ai_ran, agents.as_ref()) {
        verifier_ran = findings.iter().any(adversarial::is_eligible);
        if verifier_ran {
            let adv_caller = llm_util::SecauditCaller {
                home_dir: home_dir.to_path_buf(),
                agent_dir: agents.verifier_dir.clone(),
                attribution: "secaudit-adversarial-review",
                max_tokens: adversarial::ADVERSARIAL_MAX_TOKENS,
            };
            findings =
                adversarial::review_all(&repo_root, std::mem::take(&mut findings), &adv_caller)
                    .await;
        }
        if opts.poc {
            let poc_caller = llm_util::SecauditCaller {
                home_dir: home_dir.to_path_buf(),
                agent_dir: agents.verifier_dir.clone(),
                attribution: "secaudit-poc-generate",
                max_tokens: poc::POC_GEN_MAX_TOKENS,
            };
            for f in findings.iter_mut() {
                poc::maybe_run_poc(&repo_root, f, &poc_caller, opts.poc).await;
            }
        }
    }

    let verifier = match agents {
        Some(a) => VerifierInfo {
            independence: if !verifier_ran {
                VerifierIndependence::NotRun
            } else if a.audit_dir == a.verifier_dir {
                VerifierIndependence::SameAgent
            } else {
                VerifierIndependence::DifferentAgent
            },
            audit_agent: a.audit_id,
            verifier_agent: a.verifier_id,
        },
        None => VerifierInfo::default(),
    };

    let gate = GatePolicy {
        include_needs_human: flags.fail_on_needs_human,
    };
    let summary = Summary::from_findings(
        &findings,
        engines_run.len(),
        engines_missing.len(),
        gate,
        &coverage,
    );
    let audit_report = AuditReport {
        schema_version: CURRENT_SCHEMA_VERSION,
        repo: repo_str,
        started_at,
        profile: ScanProfile {
            mode: profile_mode,
            intake: intake_profile,
        },
        engines_run,
        engines_missing,
        findings,
        summary,
        run_status: if incomplete_reason.is_some() {
            RunStatus::Incomplete
        } else {
            RunStatus::Complete
        },
        incomplete_reason,
        coverage,
        prior_run: prior_info,
        verifier,
    };

    // The validator runs before anything is shown or written: a report that
    // breaks its own invariants is a pipeline bug, never output (exit 2).
    let violations = validator::validate_report(&audit_report);
    if !violations.is_empty() {
        eprintln!("[secaudit] 報告未通過內部驗證，不輸出（這是 secaudit 本身的錯誤）：");
        for v in &violations {
            eprintln!("  {v}");
        }
        return 2;
    }

    println!("{}", report::render_summary(&audit_report, fail_on));

    if let Some(path) = &opts.report {
        if let Err(e) = report::write_json_report(&audit_report, path) {
            eprintln!("[secaudit] 無法寫入報告檔 {}：{e}", path.display());
            return 2;
        }
        println!("報告已寫入：{}", path.display());
    }

    if opts.save {
        match report::save_report(home_dir, &audit_report) {
            Ok(path) => println!("報告副本已存至：{}", path.display()),
            Err(e) => {
                eprintln!("[secaudit] 無法寫入 --save 報告副本：{e}");
                return 2;
            }
        }
    }

    // S6 / secaudit→C1 bridge: a High+ finding is worth feeding the
    // autopilot security-event loop (`AutopilotEvent::SecurityEvent`, a
    // later wave — this is only the producer side). Gated on "a report was
    // actually persisted" (`--report` and/or `--save`, both of which would
    // already have returned exit 2 above on failure — reaching here means
    // whichever was requested succeeded): a console-only summary has no
    // durable artifact backing it, so this event only fires when there is
    // something on disk an operator/agent can follow up on. Never fires for
    // a bare `duduclaw secaudit` with neither flag.
    if (opts.report.is_some() || opts.save)
        && let Some(event_severity) = secaudit_findings_event_severity(&audit_report.summary)
    {
        let gated = audit_report.summary.gate_counts();
        let high_plus = gated.count_at_or_above(Severity::High);
        duduclaw_security::audit::append_audit_event(
            home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "secaudit_findings",
                opts.agent.as_deref().unwrap_or("secaudit"),
                event_severity,
                serde_json::json!({
                    "repo": audit_report.repo,
                    "profile": opts.profile,
                    "high_plus_count": high_plus,
                    "critical_count": gated.critical,
                    "high_count": gated.high,
                    "report_path": opts.report.as_ref().map(|p| p.to_string_lossy().to_string()),
                    "saved": opts.save,
                }),
            ),
        );
    }

    report::exit_code(&audit_report, fail_on)
}

/// `duduclaw secaudit-validate <report.json>`: run the report validator on a
/// saved report. Exit `0` valid; `1` violations (one per line on stderr,
/// including "not an AuditReport shape"); `2` unreadable or not JSON.
pub async fn cmd_secaudit_validate(report_path: &Path) -> i32 {
    const MAX_BYTES: u64 = 64 * 1024 * 1024;
    match std::fs::metadata(report_path) {
        Ok(m) if m.len() > MAX_BYTES => {
            eprintln!(
                "[secaudit-validate] 報告檔過大（{} bytes，上限 {MAX_BYTES}）：{}",
                m.len(),
                report_path.display()
            );
            return 2;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "[secaudit-validate] 無法讀取 {}：{e}",
                report_path.display()
            );
            return 2;
        }
    }
    let raw = match std::fs::read(report_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[secaudit-validate] 無法讀取 {}：{e}",
                report_path.display()
            );
            return 2;
        }
    };
    let value: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[secaudit-validate] 不是 JSON：{e}");
            return 2;
        }
    };
    let parsed: AuditReport = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("report does not match the AuditReport schema: {e}");
            return 1;
        }
    };
    let violations = validator::validate_report(&parsed);
    if violations.is_empty() {
        println!("報告驗證通過：{}", report_path.display());
        0
    } else {
        for v in &violations {
            eprintln!("{v}");
        }
        1
    }
}

/// Scanner paths are tool output: strip a leading `./` and a repo-root
/// prefix so they become repo-relative (the report validator requires a
/// safe repo-relative path), then recompute the id and root fingerprint.
fn normalize_scanner_paths(repo_root: &Path, findings: &mut [schema::Finding]) {
    let root = repo_root.to_string_lossy().to_string();
    for f in findings.iter_mut() {
        let mut p = f.file.as_str();
        if let Some(rest) = p.strip_prefix(root.as_str())
            && let Some(rest) = rest.strip_prefix('/')
        {
            p = rest;
        }
        while let Some(rest) = p.strip_prefix("./") {
            p = rest;
        }
        if p != f.file {
            let new_file = p.to_string();
            f.id = schema::compute_finding_id(
                &f.source_engine,
                &f.rule_id,
                &new_file,
                f.line,
                &f.snippet,
            );
            f.root_fingerprint = schema::compute_root_fingerprint(
                &f.source_engine,
                &f.rule_id,
                &new_file,
                &f.rule_id,
            );
            f.file = new_file;
        }
    }
}

/// Drop findings whose id repeats an earlier one (the same engine, rule,
/// file, line and snippet — e.g. overlapping scanner matches). The report
/// requires unique ids. Returns how many were dropped.
fn dedup_by_id(findings: &mut Vec<schema::Finding>) -> usize {
    let before = findings.len();
    let mut seen: HashSet<String> = HashSet::new();
    findings.retain(|f| seen.insert(f.id.clone()));
    before - findings.len()
}

/// Pure: decide whether the `secaudit_findings` audit event should fire, and
/// at what severity. Extracted so the severity-mapping logic is unit
/// testable without a live scan (`scanners::run_all` depends on external
/// tool availability, so driving `cmd_secaudit` end-to-end can't
/// deterministically produce a High/Critical finding in a unit test).
///
/// `None` when there is no High-or-above finding in the gate's counts
/// ([`Summary::gate_counts`]: Refuted/Suppressed never count, NeedsHuman only
/// under `--fail-on-needs-human`) — the same discipline the `--fail-on` CI
/// gate uses.
fn secaudit_findings_event_severity(
    summary: &Summary,
) -> Option<duduclaw_security::audit::Severity> {
    let gated = summary.gate_counts();
    let high_plus = gated.count_at_or_above(Severity::High);
    if high_plus == 0 {
        return None;
    }
    Some(if gated.critical > 0 {
        duduclaw_security::audit::Severity::Critical
    } else {
        duduclaw_security::audit::Severity::Warning
    })
}

/// What the deep-audit step produced.
struct AuditStep {
    engine_runs: Vec<schema::EngineRun>,
    missing: Option<EngineMissing>,
    findings: Vec<schema::Finding>,
    coverage: Vec<ModuleCoverage>,
    incomplete: Option<IncompleteReason>,
    /// At least one module call reached the model.
    ran: bool,
}

/// Step 3b: rank every module, audit the first `--max-modules`, build the
/// coverage list for all of them. Thin orchestration over tested pieces
/// (`ai_audit`, `coverage`).
async fn run_audit_step(
    home_dir: &Path,
    repo_root: &Path,
    opts: &SecauditOptions,
    intake_profile: &intake::RepoProfile,
    agents: &AgentSelection,
) -> AuditStep {
    let hotspots: Vec<intake::HotspotFile> = match &intake_profile.git_history {
        intake::GitHistoryStatus::Available { hotspots } => hotspots.clone(),
        intake::GitHistoryStatus::Unavailable { .. } => Vec::new(),
    };
    let all_files = intake::walk_repo_files(repo_root);
    let ranked = ai_audit::rank_modules(&all_files, &hotspots, &intake_profile.entry_points);
    let take = opts.max_modules.min(ranked.len());

    let ai_caller = llm_util::SecauditCaller {
        home_dir: home_dir.to_path_buf(),
        agent_dir: agents.audit_dir.clone(),
        attribution: "secaudit-ai-audit",
        max_tokens: ai_audit::AI_AUDIT_MAX_TOKENS,
    };

    match ai_audit::run_ai_audit(repo_root, &ranked[..take], &ai_caller).await {
        ai_audit::AiAuditOutcome::Unavailable {
            reason,
            engine_unreachable,
            module_reports,
        } => AuditStep {
            engine_runs: Vec::new(),
            missing: Some(EngineMissing {
                engine: "ai_audit".to_string(),
                reason,
            }),
            findings: Vec::new(),
            coverage: coverage::build_module_coverage(&ranked, &module_reports),
            incomplete: engine_unreachable.then_some(IncompleteReason::EngineUnavailable),
            ran: false,
        },
        ai_audit::AiAuditOutcome::Ran {
            engine_runs,
            findings,
            module_reports,
            candidate_cap_hit,
        } => AuditStep {
            engine_runs,
            missing: None,
            findings,
            coverage: coverage::build_module_coverage(&ranked, &module_reports),
            incomplete: candidate_cap_hit.then_some(IncompleteReason::ValidationBudgetExhausted),
            ran: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(repo_path: PathBuf) -> SecauditOptions {
        SecauditOptions {
            repo_path,
            profile: "quick".to_string(),
            report: None,
            fail_on: "high".to_string(),
            agent: None,
            max_modules: 5,
            poc: false,
            save: false,
        }
    }

    #[tokio::test]
    async fn invalid_repo_path_exits_2() {
        let home = tempfile::tempdir().unwrap();
        let code = cmd_secaudit(
            home.path(),
            opts(PathBuf::from("/definitely/does/not/exist/xyz")),
        )
        .await;
        assert_eq!(code, 2);
    }

    #[tokio::test]
    async fn repo_path_that_is_a_file_not_a_dir_exits_2() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir.txt");
        std::fs::write(&file, "x").unwrap();
        let code = cmd_secaudit(home.path(), opts(file)).await;
        assert_eq!(code, 2);
    }

    #[tokio::test]
    async fn invalid_profile_exits_2() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path().to_path_buf());
        o.profile = "bogus".to_string();
        assert_eq!(cmd_secaudit(home.path(), o).await, 2);
    }

    #[tokio::test]
    async fn invalid_fail_on_exits_2() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path().to_path_buf());
        o.fail_on = "bogus".to_string();
        assert_eq!(cmd_secaudit(home.path(), o).await, 2);
    }

    /// Dogfood requirement: an empty repo with no Cargo.lock produces zero
    /// findings no matter which OSS scanners happen to be installed on the
    /// machine running this test (nothing to scan) — exit code must be 0,
    /// never a crash, regardless of local tool availability. `--profile`
    /// defaults to quick, so this also never touches the AI steps (no live
    /// LLM call from a unit test).
    #[tokio::test]
    async fn quick_scan_of_empty_repo_never_panics_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let code = cmd_secaudit(home.path(), opts(dir.path().to_path_buf())).await;
        assert_eq!(code, 0);
    }

    /// Deliberately scans an EMPTY repo directory even in `--profile deep`:
    /// `ai_audit::rank_modules` over zero files produces zero modules, so
    /// `run_ai_audit` short-circuits to `AiAuditOutcome::Unavailable` WITHOUT
    /// ever attempting an LLM call (see `ai_audit::run_ai_audit_empty_modules_is_unavailable_without_calling_llm`).
    /// This keeps the test deterministic/fast/free regardless of whether a
    /// `claude` CLI happens to be installed+authenticated on the machine
    /// running `cargo test` — writing a real source file here would make
    /// this test's outcome depend on the live LLM call, which is exactly
    /// what unit tests must not do.
    #[tokio::test]
    async fn deep_profile_populates_intake_quick_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        let mut deep = opts(dir.path().to_path_buf());
        deep.profile = "deep".to_string();
        let report_path = dir.path().join("out-deep.json");
        deep.report = Some(report_path.clone());
        let code = cmd_secaudit(home.path(), deep).await;
        assert_eq!(code, 0);
        let raw = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["profile"]["mode"], serde_json::json!("deep"));
        assert!(!parsed["profile"]["intake"].is_null());
        // Zero files ⇒ ai_audit never attempted an LLM call ⇒ reported as
        // engines_missing, honestly, not silently absent.
        let missing: Vec<&str> = parsed["engines_missing"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["engine"].as_str())
            .collect();
        assert!(missing.contains(&"ai_audit"));

        let quick_report_path = dir.path().join("out-quick.json");
        let mut quick = opts(dir.path().to_path_buf());
        quick.report = Some(quick_report_path.clone());
        let code = cmd_secaudit(home.path(), quick).await;
        assert_eq!(code, 0);
        let raw = std::fs::read_to_string(&quick_report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["profile"]["mode"], serde_json::json!("quick"));
        assert!(parsed["profile"]["intake"].is_null());
        // Quick profile never even looks at ai_audit.
        let missing: Vec<&str> = parsed["engines_missing"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["engine"].as_str())
            .collect();
        assert!(!missing.contains(&"ai_audit"));
    }

    #[tokio::test]
    async fn report_json_is_written_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let report_path = dir.path().join("report.json");
        let mut o = opts(dir.path().to_path_buf());
        o.report = Some(report_path.clone());
        let _ = cmd_secaudit(home.path(), o).await;
        assert!(report_path.exists());
        let raw = std::fs::read_to_string(&report_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(parsed.get("engines_missing").is_some());
        assert!(parsed.get("summary").is_some());
    }

    #[tokio::test]
    async fn unwritable_report_path_exits_2() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path().to_path_buf());
        o.report = Some(PathBuf::from("/nonexistent-dir-xyz-abc/report.json"));
        assert_eq!(cmd_secaudit(home.path(), o).await, 2);
    }

    #[tokio::test]
    async fn save_writes_a_timestamped_copy_under_home_secaudit_reports() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path().to_path_buf());
        o.save = true;
        let code = cmd_secaudit(home.path(), o).await;
        assert_eq!(code, 0);
        let saved_dir = home.path().join("secaudit").join("reports");
        let entries: Vec<_> = std::fs::read_dir(&saved_dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn unknown_agent_falls_back_to_global_runtime_without_erroring() {
        // A nonexistent --agent id must not fail the scan (soft fallback,
        // see `llm_util::resolve_agent_dir`); quick profile so this never
        // reaches an AI call either way.
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path().to_path_buf());
        o.agent = Some("does-not-exist".to_string());
        let code = cmd_secaudit(home.path(), o).await;
        assert_eq!(code, 0);
    }

    // ── S6 / secaudit→C1 bridge: secaudit_findings audit event ─────────

    #[test]
    fn findings_event_severity_none_below_high() {
        let f = schema::Finding::candidate(
            "semgrep",
            schema::FindingKind::StaticAnalysis,
            Severity::Medium,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        let summary = Summary::from_findings(&[f], 1, 0, GatePolicy::default(), &[]);
        assert!(secaudit_findings_event_severity(&summary).is_none());
    }

    #[test]
    fn findings_event_severity_warning_for_high_without_critical() {
        let f = schema::Finding::candidate(
            "semgrep",
            schema::FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        let summary = Summary::from_findings(&[f], 1, 0, GatePolicy::default(), &[]);
        let sev = secaudit_findings_event_severity(&summary).expect("High must trigger");
        assert!(matches!(sev, duduclaw_security::audit::Severity::Warning));
    }

    #[test]
    fn findings_event_severity_critical_when_any_critical_present() {
        let high = schema::Finding::candidate(
            "semgrep",
            schema::FindingKind::StaticAnalysis,
            Severity::High,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        let critical = schema::Finding::candidate(
            "gitleaks",
            schema::FindingKind::Secret,
            Severity::Critical,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        let summary = Summary::from_findings(&[high, critical], 2, 0, GatePolicy::default(), &[]);
        let sev = secaudit_findings_event_severity(&summary).expect("Critical must trigger");
        assert!(matches!(sev, duduclaw_security::audit::Severity::Critical));
    }

    #[test]
    fn findings_event_severity_ignores_refuted_and_suppressed() {
        // by_severity already excludes Refuted/Suppressed (see
        // `Summary::from_findings`) — this inherits that discipline for
        // free rather than re-deriving it.
        let mut refuted = schema::Finding::candidate(
            "ai_audit",
            schema::FindingKind::Other,
            Severity::Critical,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        refuted.status = schema::FindingStatus::Refuted;
        let summary = Summary::from_findings(&[refuted], 1, 0, GatePolicy::default(), &[]);
        assert!(secaudit_findings_event_severity(&summary).is_none());
    }

    /// D1=B: a NeedsHuman critical (model self-reported) fires the event
    /// only under --fail-on-needs-human.
    #[test]
    fn findings_event_severity_counts_needs_human_only_when_gated() {
        let mut nh = schema::Finding::candidate(
            "ai_audit",
            schema::FindingKind::Other,
            Severity::Critical,
            "t",
            "f",
            None,
            "s",
            "r",
            vec![],
        );
        nh.status = schema::FindingStatus::NeedsHuman;
        let off =
            Summary::from_findings(std::slice::from_ref(&nh), 1, 0, GatePolicy::default(), &[]);
        assert!(secaudit_findings_event_severity(&off).is_none());
        let on = Summary::from_findings(
            &[nh],
            1,
            0,
            GatePolicy {
                include_needs_human: true,
            },
            &[],
        );
        assert!(matches!(
            secaudit_findings_event_severity(&on),
            Some(duduclaw_security::audit::Severity::Critical)
        ));
    }

    // ── v2: report shape, validate command, helpers ─────────────────

    #[tokio::test]
    async fn written_report_is_schema_v2_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let report_path = dir.path().join("r.json");
        let mut o = opts(dir.path().to_path_buf());
        o.report = Some(report_path.clone());
        assert_eq!(cmd_secaudit(home.path(), o).await, 0);
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
        assert_eq!(parsed["schema_version"], serde_json::json!(2));
        assert_eq!(parsed["run_status"], serde_json::json!("complete"));
        assert_eq!(
            parsed["verifier"]["independence"],
            serde_json::json!("not_run")
        );
        assert_eq!(cmd_secaudit_validate(&report_path).await, 0);
    }

    #[tokio::test]
    async fn validate_command_exit_codes() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let report_path = dir.path().join("r.json");
        let mut o = opts(dir.path().to_path_buf());
        o.report = Some(report_path.clone());
        assert_eq!(cmd_secaudit(home.path(), o).await, 0);

        // Tampered: summary no longer matches the findings ⇒ 1.
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
        v["summary"]["total_findings"] = serde_json::json!(99);
        let tampered = dir.path().join("tampered.json");
        std::fs::write(&tampered, serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(cmd_secaudit_validate(&tampered).await, 1);

        // JSON but not an AuditReport ⇒ 1.
        let wrong = dir.path().join("wrong.json");
        std::fs::write(&wrong, "{\"a\":1}").unwrap();
        assert_eq!(cmd_secaudit_validate(&wrong).await, 1);

        // Not JSON / missing ⇒ 2.
        let garbage = dir.path().join("garbage.json");
        std::fs::write(&garbage, "not json").unwrap();
        assert_eq!(cmd_secaudit_validate(&garbage).await, 2);
        assert_eq!(
            cmd_secaudit_validate(&dir.path().join("missing.json")).await,
            2
        );
    }

    #[tokio::test]
    async fn v2_flags_on_quick_profile_still_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let flags = SecauditV2Flags {
            fail_on_needs_human: true,
            verifier_agent: Some("nobody".into()),
            no_prior: true,
        };
        assert_eq!(
            cmd_secaudit_v2(home.path(), opts(dir.path().to_path_buf()), flags).await,
            0
        );
    }

    #[test]
    fn normalize_scanner_paths_makes_paths_repo_relative() {
        let root = Path::new("/abs/repo");
        let mk = |file: &str| {
            schema::Finding::candidate(
                "semgrep",
                schema::FindingKind::StaticAnalysis,
                Severity::Low,
                "t",
                file,
                Some(1),
                "s",
                "r",
                vec![],
            )
        };
        let mut fs = vec![mk("./src/a.rs"), mk("/abs/repo/src/b.rs"), mk("src/c.rs")];
        let ids_before: Vec<String> = fs.iter().map(|f| f.id.clone()).collect();
        normalize_scanner_paths(root, &mut fs);
        assert_eq!(fs[0].file, "src/a.rs");
        assert_eq!(fs[1].file, "src/b.rs");
        assert_eq!(fs[2].file, "src/c.rs");
        assert_eq!(fs[0].id, mk("src/a.rs").id);
        assert_eq!(fs[0].root_fingerprint, mk("src/a.rs").root_fingerprint);
        assert_eq!(fs[2].id, ids_before[2]);
    }

    #[test]
    fn dedup_by_id_keeps_the_first() {
        let f = schema::Finding::candidate(
            "semgrep",
            schema::FindingKind::StaticAnalysis,
            Severity::Low,
            "t",
            "a",
            Some(1),
            "s",
            "r",
            vec![],
        );
        let mut fs = vec![f.clone(), f.clone(), f];
        assert_eq!(dedup_by_id(&mut fs), 2);
        assert_eq!(fs.len(), 1);
    }

    #[tokio::test]
    async fn zero_findings_never_writes_a_secaudit_findings_event_even_with_report() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let report_path = dir.path().join("report.json");
        let mut o = opts(dir.path().to_path_buf());
        o.report = Some(report_path);
        let code = cmd_secaudit(home.path(), o).await;
        assert_eq!(code, 0);

        let audit_path = home.path().join("security_audit.jsonl");
        // Empty repo ⇒ zero findings ⇒ no event at all, so the file may not
        // even have been created.
        if let Ok(body) = std::fs::read_to_string(&audit_path) {
            assert!(!body.contains("secaudit_findings"));
        }
    }

    #[tokio::test]
    async fn no_report_and_no_save_never_writes_a_secaudit_findings_event() {
        // Bare invocation (neither --report nor --save): even if findings
        // existed, the gate itself must not fire — verified here on the
        // always-true "no findings" empty-repo case, which exercises the
        // same code path without needing a live scanner finding.
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let o = opts(dir.path().to_path_buf());
        assert!(o.report.is_none() && !o.save);
        let code = cmd_secaudit(home.path(), o).await;
        assert_eq!(code, 0);
        let audit_path = home.path().join("security_audit.jsonl");
        assert!(
            !audit_path.exists(),
            "no persistence requested ⇒ no audit event ⇒ file never created"
        );
    }
}
