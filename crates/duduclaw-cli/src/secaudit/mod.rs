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
//!    a. the intake/threat-model pass ([`intake::build_profile`]) —
//!       language census, entry points, git hotspots;
//!    b. AI deep audit ([`ai_audit::run_ai_audit`]) — up to `--max-modules`
//!       (default 5) module-level LLM calls producing `Candidate` findings;
//!    c. adversarial re-verification ([`adversarial::review_all`]) — one
//!       zero-shared-context LLM call per ai_audit candidate, settling each
//!       to `Refuted` or `NeedsHuman` (never auto-`Confirmed`);
//!    d. with `--poc`, sandboxed PoC generation ([`poc::maybe_run_poc`]) for
//!       every `NeedsHuman` finding at severity ≥ High (拍板 D3).
//!    `--profile quick` (default) skips all of 3a-3d — byte-identical to
//!    the pre-AI-audit wave.
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
//! AI runtime routing (拍板 D2): no model is ever hardcoded. `--agent <id>`
//! routes steps 3b-3d through that agent's `[runtime]` config
//! ([`llm_util::resolve_agent_dir`] → `run_utility_prompt`'s `agent_dir`
//! path); omitting it uses the global `config.toml [runtime]` utility
//! provider/model — the same choke-point every other internal LLM caller in
//! this codebase uses.

mod adversarial;
mod ai_audit;
mod intake;
mod llm_util;
mod poc;
mod report;
mod scanners;
mod schema;

use std::path::{Path, PathBuf};

use schema::{AuditReport, EngineMissing, ProfileMode, ScanProfile, Severity, Summary};

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
    /// `--max-modules`: cap on how many modules step 3b (AI deep audit)
    /// analyzes — the primary cost guard.
    pub max_modules: usize,
    /// `--poc`: explicitly enable step 3d (sandboxed PoC generation +
    /// execution). Off by default (拍板 D3).
    pub poc: bool,
    /// `--save`: additionally write a timestamped copy of the report under
    /// `<home>/secaudit/reports/`.
    pub save: bool,
}

/// Entry point. Returns the process exit code directly (rather than
/// `Result<(), Error>`) because the task spec requires a specific 0/1/2
/// three-way contract that the generic "any Err ⇒ exit 1" wrapper in
/// `entry_point()` can't express — same pattern already used by
/// `Commands::DesktopRecordWorker` in lib.rs.
pub async fn cmd_secaudit(home_dir: &Path, opts: SecauditOptions) -> i32 {
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

    let started_at = chrono::Utc::now().to_rfc3339();

    // Scanners always run, independent of profile — `--profile` only gates
    // the intake/threat-model pass + AI steps (task spec: "quick=只跑
    // scanners、deep=含 intake 熱點分析＋AI 深度審計").
    let (mut engines_run, mut engines_missing, mut findings) = scanners::run_all(&repo_root).await;

    let intake_profile = if profile_mode == ProfileMode::Deep {
        Some(intake::build_profile(&repo_root).await)
    } else {
        None
    };

    if profile_mode == ProfileMode::Deep {
        run_ai_steps(home_dir, &repo_root, &opts, intake_profile.as_ref(), &mut engines_run, &mut engines_missing, &mut findings)
            .await;
    }

    let summary = Summary::from_findings(&findings, engines_run.len(), engines_missing.len());
    let audit_report = AuditReport {
        repo: repo_root.to_string_lossy().to_string(),
        started_at,
        profile: ScanProfile {
            mode: profile_mode,
            intake: intake_profile,
        },
        engines_run,
        engines_missing,
        findings,
        summary,
    };

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

    report::exit_code(&audit_report, fail_on)
}

/// Steps 3b-3d: AI deep audit → adversarial review → (opt-in) PoC. Extracted
/// out of `cmd_secaudit` purely to keep that function's top-level control
/// flow readable; not independently unit-tested (it's a thin orchestration
/// wrapper over already-tested `ai_audit`/`adversarial`/`poc` functions —
/// same reasoning as `scanners::run_all` not being unit-tested directly).
#[allow(clippy::too_many_arguments)]
async fn run_ai_steps(
    home_dir: &Path,
    repo_root: &Path,
    opts: &SecauditOptions,
    intake_profile: Option<&intake::RepoProfile>,
    engines_run: &mut Vec<schema::EngineRun>,
    engines_missing: &mut Vec<EngineMissing>,
    findings: &mut Vec<schema::Finding>,
) {
    let Some(intake_profile) = intake_profile else {
        return;
    };
    let agent_dir = llm_util::resolve_agent_dir(home_dir, opts.agent.as_deref());

    let hotspots: Vec<intake::HotspotFile> = match &intake_profile.git_history {
        intake::GitHistoryStatus::Available { hotspots } => hotspots.clone(),
        intake::GitHistoryStatus::Unavailable { .. } => Vec::new(),
    };
    let all_files = intake::walk_repo_files(repo_root);
    let modules = ai_audit::rank_modules(&all_files, &hotspots, &intake_profile.entry_points, opts.max_modules);

    let ai_caller = llm_util::SecauditCaller {
        home_dir: home_dir.to_path_buf(),
        agent_dir: agent_dir.clone(),
        attribution: "secaudit-ai-audit",
        max_tokens: ai_audit::AI_AUDIT_MAX_TOKENS,
    };

    match ai_audit::run_ai_audit(repo_root, &modules, &ai_caller).await {
        ai_audit::AiAuditOutcome::Unavailable { reason } => {
            engines_missing.push(EngineMissing {
                engine: "ai_audit".to_string(),
                reason,
            });
        }
        ai_audit::AiAuditOutcome::Ran {
            engine_runs: ai_engine_runs,
            findings: ai_findings,
        } => {
            engines_run.extend(ai_engine_runs);

            let adv_caller = llm_util::SecauditCaller {
                home_dir: home_dir.to_path_buf(),
                agent_dir: agent_dir.clone(),
                attribution: "secaudit-adversarial-review",
                max_tokens: adversarial::ADVERSARIAL_MAX_TOKENS,
            };
            let mut reviewed = adversarial::review_all(repo_root, ai_findings, &adv_caller).await;

            if opts.poc {
                let poc_caller = llm_util::SecauditCaller {
                    home_dir: home_dir.to_path_buf(),
                    agent_dir,
                    attribution: "secaudit-poc-generate",
                    max_tokens: poc::POC_GEN_MAX_TOKENS,
                };
                for f in reviewed.iter_mut() {
                    poc::maybe_run_poc(repo_root, f, &poc_caller, opts.poc).await;
                }
            }

            findings.extend(reviewed);
        }
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
        let code = cmd_secaudit(home.path(), opts(PathBuf::from("/definitely/does/not/exist/xyz"))).await;
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
}
