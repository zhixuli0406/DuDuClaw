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
// P2 (Team-as-Agent): the role→model capability matrix and its verifier cells.
mod matrix;
mod runner;
pub mod stats;
mod team_probe;
mod transcript;
mod verifier_cell;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use console::style;
use duduclaw_fork::judge::LlmCaller;

use assertions::AssertionResult;
use runner::RunMode;

/// Significance level and target power for every sample-size / resolution
/// computation in this module (Miller 2024 Eq. 9/10). Not exposed as CLI
/// flags — only the declared MDE (`--mde`) is a free parameter; alpha/power
/// are the paper's own defaults and changing them silently would make
/// reports across runs incomparable.
pub(super) const ALPHA: f64 = 0.05;
pub(super) const POWER: f64 = 0.8;

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
    /// P0/WP-D — Miller 2024 `K`-repeat design: run each case this many
    /// times and aggregate its pass rate. `<= 1` (the default) is
    /// byte-identical to the pre-existing single-run behavior, transcript
    /// filenames included.
    pub repeats: u32,
    /// P0/WP-D: paired statistical comparison against a previously written
    /// `--report` JSON file (matched by `EvalCaseRef` id). Refused (with an
    /// explicit error inside `stats.baseline_comparison`, not a crash) when
    /// either report is in replay mode against a different model — the
    /// Replay Gap (arXiv:2608.08239): a frozen replayed transcript must
    /// never stand in for a live run of a *different* model.
    pub baseline: Option<PathBuf>,
    /// P0/WP-D: declared minimum detectable effect for the resolution
    /// diagnostic (arXiv:2605.30315), as a pass-rate fraction
    /// (`0.10` = 10 percentage points). Default `0.10`.
    pub mde: f64,
    /// P0/WP-D: cluster key for cluster-robust standard errors (Miller 2024
    /// App. C). Only `"dir"` (each case's directory) is implemented —
    /// `cmd_eval` refuses any other value rather than silently falling back
    /// to unclustered SEs. Default `"dir"`.
    pub cluster_by: String,

    // ── P2 (Team-as-Agent): runtime/model selection + the matrix ─────────
    /// `--runtime`: which backend runs every case. `None` ⇒ `claude`, taking
    /// the pre-P2 direct-CLI path byte-identically.
    pub runtime: Option<String>,
    /// `--model`: model id override for every case. `None` ⇒ each case's own
    /// `[case] model`.
    pub model: Option<String>,
    /// `--paired-seeds`: derive a deterministic seed per `(case_id, repeat)`.
    /// Recorded, not applied — see `runner::SEED_APPLIED_ANYWHERE`.
    pub paired_seeds: bool,
    /// `--temperature`: declared sampling temperature. Only the matrix path
    /// reads it, and only to REFUSE a value below production (Miller §3.3);
    /// no runtime in this build exposes a temperature knob, so an accepted
    /// value is recorded in the report header and otherwise inert.
    pub temperature: Option<f64>,
    /// `--matrix`: measure the role→model capability matrix instead of running
    /// the suite once. See [`matrix`].
    pub matrix: bool,
    pub team_2x2: bool,
    pub planner_weak: Option<String>,
    pub planner_strong: Option<String>,
    pub executor_weak: Option<String>,
    pub executor_strong: Option<String>,
    pub verifier_model: Option<String>,
    pub team_effort: Option<String>,
    pub team_fanout: Option<u8>,
    pub team_grok_sandbox_off: bool,
    /// `--roles` (matrix only).
    pub roles: Vec<String>,
    /// `--models` (matrix only), each `<runtime>:<model>`.
    pub models: Vec<String>,
    /// `--weak` / `--strong` (matrix only): the bottleneck probe's two arms.
    pub weak: Option<String>,
    pub strong: Option<String>,
    /// `--domain` (matrix only): suite roots, one per domain. Empty ⇒ `path`.
    pub domain: Vec<PathBuf>,
    /// `--budget-usd` (matrix only): stop before the run that would exceed it.
    pub budget_usd: Option<f64>,
    /// `--max-cases` (matrix only): cap cases per suite, for smoke runs.
    pub max_cases: Option<usize>,
    /// `--agent`: run every case under THIS provisioned agent instead of each
    /// case's own `[case] agent`. `None` ⇒ the case's own.
    ///
    /// Declared, never inferred: it changes the system prompt every case runs
    /// under, so the report header carries `agent_override`.
    pub agent: Option<String>,
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
    /// Stable `EvalCaseRef` id — the case file's filename stem (B4). Shared
    /// across every repeat of the same case under `--repeats N > 1` — this
    /// is the key `stats::paired_comparison` and the by-case aggregation in
    /// this module group on.
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
    // ── P0/WP-D additive fields (existing fields/values are unchanged) ──
    /// Effective model for the case (`case_file.model()`); `""` for reports
    /// created before a case could be loaded (discovery/parse failures).
    /// Feeds the Replay Gap guard's header `model` field.
    model: String,
    /// Cluster key for cluster-robust SEs: the case file's directory
    /// relative to the discovery root (`--cluster-by dir`, the only
    /// implemented mode today). `""` for reports created before a case's
    /// path was resolved.
    cluster: String,
    /// 1-based repeat index under `--repeats N > 1`; `None` for a normal
    /// single run (including every run when `--repeats` is unset/`1`).
    repeat_index: Option<u32>,
    // ── P2 additive fields ──────────────────────────────────────────────
    /// Runtime id this run asked for (`--runtime`, default `claude`).
    runtime: String,
    /// Agent directory this run used — the `--agent` override when given, else
    /// the case's own `[case] agent`.
    agent: String,
    /// Deterministic `(case_id, repeat)` seed under `--paired-seeds`.
    /// `None` when the flag is off. Recorded, never applied — see
    /// `runner::SEED_APPLIED_ANYWHERE`.
    seed: Option<u64>,
}

impl CaseReport {
    /// All-defaults constructor so every construction site only has to name
    /// the fields it actually knows about; new additive fields land in one
    /// place instead of six.
    fn blank(id: impl Into<String>, name: impl Into<String>, path: impl Into<String>) -> Self {
        CaseReport {
            id: id.into(),
            name: name.into(),
            path: path.into(),
            passed: false,
            error: None,
            assertions: Vec::new(),
            judge: None,
            tool_calls: Vec::new(),
            diagnostics: None,
            duration_ms: 0,
            model: String::new(),
            cluster: String::new(),
            repeat_index: None,
            runtime: String::new(),
            agent: String::new(),
            seed: None,
        }
    }
}

/// Entry point for the `Eval` subcommand.
pub async fn cmd_eval(home: &Path, opts: EvalOptions) -> duduclaw_core::error::Result<()> {
    // P2: `--matrix` is a different command shape (cells, not one pass over a
    // suite) with its own hard rules, so it validates and runs on its own path.
    // Dispatched before anything else so a matrix invocation never half-applies
    // the single-suite flag validation below.
    if opts.team_2x2 {
        return team_probe::run_team_probe(home, &opts).await;
    }
    if let Some(flag) = first_team_probe_only_flag(&opts) {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "{flag} requires --team-2x2"
        )));
    }
    if opts.matrix {
        return matrix::run_matrix(home, &opts).await;
    }
    // P2: `--roles` / `--models` / `--weak` / `--strong` / `--domain` /
    // `--budget-usd` / `--max-cases` only mean anything under `--matrix`.
    // Accepting them silently on the ordinary path would let an operator think a
    // matrix ran when one plain suite pass did — refuse instead.
    if let Some(flag) = first_matrix_only_flag(&opts) {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "{flag} requires --matrix (it has no meaning for a single-suite run)"
        )));
    }
    // P2: `--record` writes the suite's COMMITTED baseline transcripts. Doing
    // that under a `--runtime`/`--model` override would silently replace each
    // case's baseline with a recording of a model the case does not declare —
    // A case may now explicitly pin both `[case] runtime` and `[case] model`;
    // that makes a non-Claude synthesized transcript an intentional baseline
    // whose fidelity is visible to the author. CLI overrides still cannot
    // record because they would contradict the case's declaration.
    // `--matrix` never records at all for the same reason; here it is a refusal
    // rather than a silent overwrite of data the operator committed.
    if let Some(flag) = record_override_conflict(&opts) {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "--record cannot be combined with {flag}: recording would overwrite each case's \
             committed baseline transcript with a run of a model the case does not declare. Pin \
             the model in `[case] model` and, for a non-Claude baseline, its \
             `[case] runtime`; then record without CLI overrides"
        )));
    }
    // P0/WP-D: fail fast on statistically nonsensical flags — never silently
    // fall back to an unclustered/undeclared computation.
    if opts.cluster_by != "dir" {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "--cluster-by {:?} is not implemented (only \"dir\" is supported today)",
            opts.cluster_by
        )));
    }
    if !(opts.mde > 0.0 && opts.mde < 1.0) {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "--mde must be a fraction in (0, 1), got {}",
            opts.mde
        )));
    }
    // P0/WP-D live-fire fix (2026-09-24): `--repeats N > 1` measures
    // run-to-run variance across N independent samples of the same case — a
    // frozen `--replay` transcript is one fixed sample with zero variance to
    // measure (the Replay Gap, arXiv:2608.08239, in miniature). Previously
    // this silently looked for `.r1.jsonl`/`.r2.jsonl`/… replay files that
    // `--record` never wrote at `--repeats 1` (the pre-existing baseline
    // naming), so every case failed with a misleading "transcript missing"
    // error instead of an explicit rejection. `--repeats 1` (the default) is
    // unaffected — it reads the plain `<case>.transcript.jsonl` file exactly
    // as before this flag existed.
    if opts.replay && opts.repeats > 1 {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "--repeats {} cannot be combined with --replay: repeats measure run-to-run variance, \
             and a frozen replay transcript has none (Replay Gap, arXiv:2608.08239) — run live with \
             `--repeats {} --record` to actually capture that many samples (written as \
             `<case>.transcript.r1.jsonl` … `.r{}.jsonl`), then `--replay` against them",
            opts.repeats, opts.repeats, opts.repeats
        )));
    }

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

/// Which `--matrix`-only flag the operator set without `--matrix`, if any.
/// Deterministic order so the message is stable.
fn first_matrix_only_flag(opts: &EvalOptions) -> Option<&'static str> {
    if !opts.roles.is_empty() {
        return Some("--roles");
    }
    if !opts.models.is_empty() {
        return Some("--models");
    }
    if opts.weak.is_some() {
        return Some("--weak");
    }
    if opts.strong.is_some() {
        return Some("--strong");
    }
    if !opts.domain.is_empty() {
        return Some("--domain");
    }
    if opts.budget_usd.is_some() {
        return Some("--budget-usd");
    }
    if opts.max_cases.is_some() {
        return Some("--max-cases");
    }
    if opts.temperature.is_some() {
        return Some("--temperature");
    }
    None
}

fn first_team_probe_only_flag(opts: &EvalOptions) -> Option<&'static str> {
    if opts.planner_weak.is_some() {
        return Some("--planner-weak");
    }
    if opts.planner_strong.is_some() {
        return Some("--planner-strong");
    }
    if opts.executor_weak.is_some() {
        return Some("--executor-weak");
    }
    if opts.executor_strong.is_some() {
        return Some("--executor-strong");
    }
    if opts.verifier_model.is_some() {
        return Some("--verifier-model");
    }
    if opts.team_effort.is_some() {
        return Some("--team-effort");
    }
    if opts.team_fanout.is_some() {
        return Some("--team-fanout");
    }
    if opts.team_grok_sandbox_off {
        return Some("--team-grok-sandbox-off");
    }
    None
}

/// Which override makes `--record` unsafe, if any. `--runtime claude` is fine:
/// it names the path the case already takes.
fn record_override_conflict(opts: &EvalOptions) -> Option<&'static str> {
    let runtime_overridden = opts
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some_and(|id| id != duduclaw_core::types::RuntimeType::Claude.as_str());
    if runtime_overridden {
        return Some("--runtime (non-claude)");
    }
    if opts
        .model
        .as_deref()
        .map(str::trim)
        .is_some_and(|m| !m.is_empty())
    {
        return Some("--model");
    }
    None
}

/// Resolve `--runtime` / `--model` into a [`runner::RunOverrides`] template
/// (the per-run seed is filled in per repeat by `run_eval`).
///
/// `Err` on an unknown runtime id — never a silent fall back to Claude, which
/// would report a Claude measurement under another vendor's name.
fn base_overrides(opts: &EvalOptions) -> Result<runner::RunOverrides, String> {
    let runtime = match opts
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(id) => Some(
            duduclaw_core::types::RuntimeType::from_id(id)
                .ok_or_else(|| format!("--runtime {id:?} is not a runtime on this build"))?,
        ),
        None => None,
    };
    let agent = match opts
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(a) if !duduclaw_core::is_valid_agent_id(a) => {
            return Err(format!(
                "--agent {a:?} is not a valid agent id (1-64 chars of [a-zA-Z0-9_-])"
            ));
        }
        Some(a) => Some(a.to_string()),
        None => None,
    };
    Ok(runner::RunOverrides {
        runtime,
        model: opts.model.clone(),
        agent,
        seed: None,
    })
}

/// Core loop, judge injected for testability. Cases run sequentially:
/// deterministic order, and live runs must not contend for the operator's
/// account quota.
async fn run_eval(
    home: &Path,
    opts: &EvalOptions,
    judge_caller: &dyn LlmCaller,
) -> Vec<CaseReport> {
    let root = opts.path.clone().unwrap_or_else(|| PathBuf::from("evals"));
    // P2: an unknown `--runtime` is a whole-run refusal, reported the same way a
    // discovery failure is (one failed row) rather than per case.
    let base = match base_overrides(opts) {
        Ok(b) => b,
        Err(e) => {
            return vec![CaseReport {
                error: Some(e),
                ..CaseReport::blank("<runtime>", "<runtime>", root.display().to_string())
            }];
        }
    };
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
                error: Some(e),
                ..CaseReport::blank("<discovery>", "<discovery>", root.display().to_string())
            }];
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
                    error: Some(format!(
                        "duplicate case id {id:?} (filename stem must be unique across the suite): {} and {}",
                        prev.display(),
                        p.display()
                    )),
                    ..CaseReport::blank(
                        id.clone(),
                        "<duplicate-case-id>",
                        root.display().to_string(),
                    )
                }];
            }
        }
    }

    // `--case` (B4): exact `EvalCaseRef` selection, evaluated against the
    // filename stem before any TOML parsing — unlike `--filter` this never
    // needs to load a case to decide whether to run it.
    if !opts.case.is_empty() {
        let wanted: std::collections::HashSet<&str> =
            opts.case.iter().map(String::as_str).collect();
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
                    error: Some(e),
                    duration_ms: started.elapsed().as_millis(),
                    ..CaseReport::blank(
                        id,
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("<unnamed>"),
                        path.display().to_string(),
                    )
                });
                continue;
            }
        };

        if let Some(f) = &opts.filter {
            if !case_file.case.name.contains(f.as_str()) {
                continue;
            }
        }

        // P0/WP-D: `--repeats N` (Miller 2024 K-repeat design). `N <= 1`
        // (the default) runs the case exactly once with `repeat_index: None`
        // — byte-identical to pre-existing behavior, including transcript
        // filenames (`runner::transcript_path`'s `None` branch).
        let cluster = cluster_key_for(&root, &path);
        let repeats = opts.repeats.max(1);
        for i in 1..=repeats {
            let repeat_index = if repeats > 1 { Some(i) } else { None };
            // P2 `--paired-seeds`: the seed depends only on `(case_id, repeat)`,
            // never on the model, so the same draws line up across runtimes.
            let overrides = runner::RunOverrides {
                runtime: base.runtime.or_else(|| {
                    case_file
                        .case
                        .runtime
                        .as_deref()
                        .and_then(duduclaw_core::types::RuntimeType::from_id)
                }),
                seed: opts.paired_seeds.then(|| matrix::derive_seed(&id, i)),
                ..base.clone()
            };
            reports.push(
                run_one(
                    &path,
                    &id,
                    &cluster,
                    &case_file,
                    home,
                    mode,
                    repeat_index,
                    opts.no_judge,
                    judge_caller,
                    &overrides,
                )
                .await,
            );
        }
    }
    reports
}

/// The stable `EvalCaseRef` id for a case file: its filename stem. Matches
/// the existing `commercial/evals/` convention (360 shipped cases, each
/// filename already globally unique) — `[case] name` stays the human-readable
/// title, never the identity.
pub(super) fn case_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("<unnamed>")
        .to_string()
}

/// Unique within a suite even when separate fixture directories reuse a
/// filename. Matrix and full-team reports use this for pairing and seeds;
/// the short [`case_id`] remains accepted by `--case` for existing suites.
pub(super) fn case_key(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut without_extension = relative.to_path_buf();
    without_extension.set_extension("");
    without_extension.to_string_lossy().replace('\\', "/")
}

/// True when `path` (relative to `root`) has a directory component matching
/// one of `exclude_dirs` by exact name. Falls back to matching against the
/// absolute path's components when `path` doesn't start with `root` (e.g. a
/// caller-supplied absolute path outside the walked root).
pub(super) fn path_excluded(root: &Path, path: &Path, exclude_dirs: &[String]) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components().any(|c| {
        matches!(c, std::path::Component::Normal(os) if os
            .to_str()
            .map(|s| exclude_dirs.iter().any(|d| d == s))
            .unwrap_or(false))
    })
}

/// Cluster key for cluster-robust standard errors (`--cluster-by dir`, the
/// only implemented mode today — see `cmd_eval`'s validation): the case
/// file's directory relative to the discovery root. A case sitting directly
/// in the root uses `"."` so the key is never the empty string (which this
/// module also uses as the "cluster unknown" sentinel on `CaseReport::blank`,
/// for reports created before a case's path resolved at all — discovery and
/// duplicate-id failures).
pub(super) fn cluster_key_for(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    match rel.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_one(
    path: &Path,
    id: &str,
    cluster: &str,
    case_file: &case::EvalCaseFile,
    home: &Path,
    mode: RunMode,
    repeat_index: Option<u32>,
    no_judge: bool,
    judge_caller: &dyn LlmCaller,
    overrides: &runner::RunOverrides,
) -> CaseReport {
    let started = std::time::Instant::now();
    let mut report = CaseReport {
        // P2: the EFFECTIVE model (a `--model` override wins over `[case] model`)
        // so the report's `model` header always names what actually ran.
        model: overrides.effective_model(case_file).to_string(),
        cluster: cluster.to_string(),
        repeat_index,
        runtime: overrides.effective_runtime(case_file).as_str().to_string(),
        agent: overrides.effective_agent(case_file).to_string(),
        seed: overrides.seed,
        ..CaseReport::blank(id, case_file.case.name.clone(), path.display().to_string())
    };

    let transcript =
        match runner::obtain_transcript(path, case_file, home, mode, repeat_index, overrides).await
        {
            Ok((t, facts)) => {
                // A failover substitution means the answer came from a model
                // nobody asked about. Recorded on the report rather than folded
                // into the score silently.
                if let Some((rt, model)) = facts.substituted {
                    report.error = Some(format!(
                        "failover answered with ({rt}, {model}) instead of the requested ({}, {})",
                        report.runtime, report.model
                    ));
                    report.duration_ms = started.elapsed().as_millis();
                    return report;
                }
                t
            }
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

// ─────────────────────────────────────────────────────────────────────────
// P0/WP-D: honest statistics glue (pure math lives in `stats.rs`; this is
// the report-assembly layer that turns `CaseReport`s into it).
// ─────────────────────────────────────────────────────────────────────────

/// A point estimate's resolution check against the standalone chance line
/// (`0.5`) — shared by the suite-wide row and every per-directory row.
pub(super) struct ResolutionRow {
    pub(super) ci_low: f64,
    pub(super) ci_high: f64,
    pub(super) n_required: f64,
    pub(super) q: f64,
    pub(super) mde_at_n: f64,
    pub(super) verdict: stats::Verdict,
    pub(super) label: stats::HonestLabel,
}

/// `se` must already be the *variance-of-the-mean*-scale standard error
/// (what `stats::se_clt`/`se_clustered` return) — this un-does the `/n` to
/// recover an effective per-question variance, then feeds Miller 2024 Eq. 9
/// with `omega2 = 0, sigma2_b = 0, k_a = k_b = 1` (the whole observed
/// variance in one bucket — exact, not approximate, whenever the caller
/// can't separately estimate a between-question vs. within-question split;
/// see `stats::n_required_for_mde`'s doc comment).
pub(super) fn resolution_row(
    point_estimate: f64,
    se: f64,
    n: f64,
    mde: f64,
    pass_line: f64,
) -> ResolutionRow {
    let z = stats::z_two_sided(ALPHA);
    let ci_low = point_estimate - z * se;
    let ci_high = point_estimate + z * se;
    let effective_variance = se * se * n;
    let n_required =
        stats::n_required_for_mde(ALPHA, POWER, mde, 0.0, effective_variance, 0.0, 1.0, 1.0);
    let q = stats::resolution_ratio_q(n, n_required);
    let mde_at_n = stats::mde_for_n(ALPHA, POWER, n, 0.0, effective_variance, 0.0, 1.0, 1.0);
    let (verdict, label) = stats::classify(point_estimate, ci_low, ci_high, pass_line, q);
    ResolutionRow {
        ci_low,
        ci_high,
        n_required,
        q,
        mde_at_n,
        verdict,
        label,
    }
}

/// serde_json rejects non-finite floats outright — every stats field that
/// can legitimately be NaN (e.g. `se_ratio` when the suite has zero
/// unclustered variance — a perfectly healthy all-pass run) must degrade to
/// JSON `null`, never crash the whole `--report` write.
pub(super) fn f64_or_null(v: f64) -> serde_json::Value {
    if v.is_finite() {
        serde_json::json!(v)
    } else {
        serde_json::Value::Null
    }
}

/// Single-string model summary for the Replay Gap guard and the `stats`
/// header: `"unknown"` when no case resolved a model, the shared model
/// string when every case agrees, or `"mixed:<sorted,comma,joined>"` when
/// the suite pins more than one.
fn header_model_string<'a>(models: impl Iterator<Item = &'a str>) -> String {
    let mut distinct: Vec<&str> = models.filter(|m| !m.is_empty()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    match distinct.as_slice() {
        [] => "unknown".to_string(),
        [one] => (*one).to_string(),
        many => format!("mixed:{}", many.join(",")),
    }
}

/// Load a `--baseline` `--report` JSON file: `(mode, model, per-case
/// pass-rate values)`. `Err` on any I/O/parse/shape problem — the caller
/// degrades to `baseline_comparison: {"error": ...}`, never a crash and
/// never a fabricated comparison.
fn load_baseline(path: &Path) -> Result<(String, String, Vec<(String, f64)>), String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read baseline report {}: {e}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("baseline report {} is not valid JSON: {e}", path.display()))?;
    let mode = json
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let model = json
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let per_case = json.get("per_case").and_then(|v| v.as_array()).ok_or_else(|| {
        format!(
            "baseline report {} has no `per_case` array (write it with `--report` from this same `duduclaw eval`)",
            path.display()
        )
    })?;
    let values = per_case
        .iter()
        .filter_map(|c| {
            let id = c.get("id")?.as_str()?.to_string();
            let passed = c.get("passed")?.as_bool()?;
            Some((id, if passed { 1.0 } else { 0.0 }))
        })
        .collect();
    Ok((mode, model, values))
}

/// Everything the honest-statistics console line and the `--report` JSON
/// `stats` block need, computed once from the run's `CaseReport`s.
/// Minimum cluster count for `se_clustered` to be a trustworthy estimate
/// (Miller 2024 App. C — cluster-robust SEs need enough clusters to estimate
/// the between-cluster variance component at all; below this the number is
/// printed/flagged but never hidden). Live-fire evidence (2026-09-24,
/// hr-recruit suite): 2 clusters produced `se_ratio: 0.27`, a number with no
/// statistical meaning at that cluster count.
pub(super) const MIN_RELIABLE_CLUSTERS: usize = 5;

struct StatsBundle {
    /// The `"stats"` object for the JSON report.
    json: serde_json::Value,
    /// Header `model` string for the JSON root — mirrors `mode`.
    header_model: String,
    /// Top-level `verdict`/`label`/`resolution_ratio_q` — **precedence,
    /// spelled out because it looks contradictory otherwise** (live-fire
    /// evidence, 2026-09-24, hr-recruit suite: top level printed `→ pass`
    /// while `stats.suite.verdict` was `fail`; both were correct, they just
    /// answer different questions):
    ///
    /// - **With `--baseline`, accepted** (Replay Gap guard passed, ≥1
    ///   overlapping case id): this is the **paired comparison's** verdict —
    ///   candidate vs. baseline, pass line `0` (no difference). It answers
    ///   "did this run change relative to the baseline?", not "is this run's
    ///   raw pass rate good."
    /// - **Otherwise** (no `--baseline`, or it was refused/had no overlap):
    ///   this is the **suite's own standalone** verdict — pass line `0.5`
    ///   (chance level). It answers "is this run's pass rate distinguishable
    ///   from a coin flip?", independent of any baseline.
    ///
    /// `stats.suite.verdict`/`.label` (in `json`, below) are **always** the
    /// standalone chance-line check, even when `--baseline` overrides the
    /// top-level fields — so the two can legitimately disagree. See
    /// `docs/guides/evals.md`'s "Honest statistics → verdict / label"
    /// section for the user-facing version of this same explanation.
    verdict: stats::Verdict,
    label: stats::HonestLabel,
    resolution_ratio_q: f64,
    /// `true` when this run's top-level verdict came from the `--baseline`
    /// paired comparison rather than the standalone chance-line check —
    /// lets the console summary say which question `verdict` is answering.
    verdict_is_baseline_comparison: bool,
    n_cases: usize,
    n_clusters: usize,
    pass_pct: f64,
    ci_halfwidth_pct: f64,
    mde_at_n_pct: f64,
    se_ratio: f64,
    /// Which estimator produced the suite CI (see `matrix::choose_se`).
    se_source: matrix::SeSource,
    /// `true` when `n_clusters < MIN_RELIABLE_CLUSTERS` — `se_clustered`
    /// (and everything downstream of it: the CI, `q`, `verdict`) is not
    /// trustworthy at that cluster count. Mirrored at `stats.suite.small_cluster_warning`.
    small_cluster_warning: bool,
}

fn build_stats(reports: &[CaseReport], opts: &EvalOptions) -> StatsBundle {
    // Sentinel whole-suite-failure reports (`<discovery>`,
    // `<duplicate-case-id>`) never resolved a `cluster` — excluded here so
    // they don't masquerade as a zero-pass-rate case in the statistics.
    let real: Vec<&CaseReport> = reports.iter().filter(|r| !r.cluster.is_empty()).collect();

    // Group `--repeats N` runs of the same case id into one aggregate pass
    // RATE (mean over repeats); `N <= 1` degenerates to each case's own
    // `passed` bool, unchanged from pre-`--repeats` behavior.
    let mut by_id: BTreeMap<String, Vec<bool>> = BTreeMap::new();
    let mut cluster_of: HashMap<String, String> = HashMap::new();
    for r in &real {
        by_id.entry(r.id.clone()).or_default().push(r.passed);
        cluster_of
            .entry(r.id.clone())
            .or_insert_with(|| r.cluster.clone());
    }
    let case_rates: Vec<(String, f64)> = by_id
        .iter()
        .map(|(id, outcomes)| {
            let rate = outcomes.iter().filter(|p| **p).count() as f64 / outcomes.len() as f64;
            (id.clone(), rate)
        })
        .collect();

    let values: Vec<f64> = case_rates.iter().map(|(_, v)| *v).collect();
    let cluster_keys: Vec<&str> = case_rates
        .iter()
        .map(|(id, _)| cluster_of.get(id).map(String::as_str).unwrap_or("default"))
        .collect();

    let n_cases = values.len();
    let n_clusters: usize = cluster_keys
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();

    let mean = stats::mean(&values);
    let se_clt = stats::se_clt(&values);
    let se_clustered = stats::se_clustered(&values, &cluster_keys);
    let se_ratio = stats::se_ratio(se_clustered, se_clt);
    // Same-class sweep of the P2 smoke-3 defect (`matrix::choose_se`): with ONE
    // cluster the cluster-robust estimator is identically zero by construction,
    // and feeding that to `resolution_row` collapses the suite CI to a point.
    // The per-directory rows below already knew this (see their comment); the
    // suite row did not, so a single-directory suite printed `±0.0pp`.
    let suite_se = matrix::choose_se(&values, &cluster_keys);
    let suite_row = resolution_row(mean, suite_se.se, n_cases as f64, opts.mde, 0.5);

    // ── per-directory rows: each directory's OWN plain (unclustered) SE —
    // clustering is a suite-level concept comparing across directories, and
    // `se_clustered` on a single directory's cases (all sharing one cluster
    // key) degenerates to exactly 0 by construction, not a useful number.
    let mut by_cluster_values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for (id, v) in &case_rates {
        let c = cluster_of
            .get(id)
            .cloned()
            .unwrap_or_else(|| "default".to_string());
        by_cluster_values.entry(c).or_default().push(*v);
    }
    let by_cluster_json: Vec<serde_json::Value> = by_cluster_values
        .iter()
        .map(|(cluster, vals)| {
            let m = stats::mean(vals);
            let se = stats::se_clt(vals);
            let row = resolution_row(m, se, vals.len() as f64, opts.mde, 0.5);
            serde_json::json!({
                "cluster": cluster,
                "n_cases": vals.len(),
                "mean": f64_or_null(m),
                "se_clt": f64_or_null(se),
                "ci95_low": f64_or_null(row.ci_low),
                "ci95_high": f64_or_null(row.ci_high),
                "resolution_ratio_q": f64_or_null(row.q),
                "verdict": row.verdict,
                "label": row.label,
            })
        })
        .collect();

    // ── `--baseline` paired comparison (Replay Gap guarded) ──
    let header_model = header_model_string(real.iter().map(|r| r.model.as_str()));
    let current_mode = if opts.replay { "replay" } else { "live" };
    let mut baseline_comparison = serde_json::Value::Null;
    let mut top_verdict = suite_row.verdict;
    let mut top_label = suite_row.label;
    let mut top_q = suite_row.q;
    let mut verdict_is_baseline_comparison = false;

    if let Some(baseline_path) = &opts.baseline {
        match load_baseline(baseline_path) {
            Err(e) => {
                baseline_comparison = serde_json::json!({ "error": e });
            }
            Ok((baseline_mode, baseline_model, baseline_rates)) => {
                // Replay Gap (arXiv:2608.08239): a frozen replayed transcript
                // must never stand in for a *different* model's live run in
                // a comparison. Same-model replay-vs-replay (a regression
                // check across code versions, not a model comparison) is
                // allowed through.
                let replay_gap_violation = (current_mode == "replay" || baseline_mode == "replay")
                    && baseline_model != header_model;
                if replay_gap_violation {
                    baseline_comparison = serde_json::json!({
                        "error": format!(
                            "refused (Replay Gap, arXiv:2608.08239): this run is mode={current_mode:?} model={header_model:?}, \
                             baseline is mode={baseline_mode:?} model={baseline_model:?} — a replayed transcript must never \
                             stand in for a different model's live run in a comparison"
                        ),
                    });
                } else {
                    match stats::paired_comparison(&case_rates, &baseline_rates, &cluster_of, ALPHA)
                    {
                        None => {
                            baseline_comparison = serde_json::json!({
                                "error": "no overlapping case ids between this run and the baseline report",
                            });
                        }
                        Some(pr) => {
                            let effective_variance = pr.se * pr.se * pr.n as f64;
                            let n_required = stats::n_required_for_mde(
                                ALPHA,
                                POWER,
                                opts.mde,
                                0.0,
                                effective_variance,
                                0.0,
                                1.0,
                                1.0,
                            );
                            let q = stats::resolution_ratio_q(pr.n as f64, n_required);
                            let (verdict, label) =
                                stats::classify(pr.paired_delta, pr.ci95_low, pr.ci95_high, 0.0, q);
                            top_verdict = verdict;
                            top_label = label;
                            top_q = q;
                            verdict_is_baseline_comparison = true;
                            baseline_comparison = serde_json::json!({
                                "n": pr.n,
                                "paired_delta": f64_or_null(pr.paired_delta),
                                "corr_with_baseline": f64_or_null(pr.corr_with_baseline),
                                "se": f64_or_null(pr.se),
                                // Which estimator produced `se`, and whether it
                                // degenerated (review finding 9): a zero-width
                                // interval is not certainty, and a
                                // single-cluster suite must say so rather than
                                // letting a reader assume the cluster-robust
                                // number.
                                "se_source": pr.se_source,
                                "degenerate": pr.degenerate,
                                "ci95_low": f64_or_null(pr.ci95_low),
                                "ci95_high": f64_or_null(pr.ci95_high),
                                "fallback_to_unpaired": pr.fallback_to_unpaired,
                                "n_required_for_mde": f64_or_null(n_required),
                                "resolution_ratio_q": f64_or_null(q),
                                "verdict": verdict,
                                "label": label,
                            });
                        }
                    }
                }
            }
        }
    }

    // Live-fire evidence (2026-09-24, hr-recruit suite): 2 clusters produced
    // `se_ratio: 0.27` — a number with no statistical meaning at that
    // cluster count (Miller 2024 App. C needs enough clusters to estimate
    // the between-cluster variance component at all). Flagged, never hidden.
    let small_cluster_warning = n_cases > 0 && n_clusters < MIN_RELIABLE_CLUSTERS;

    let json = serde_json::json!({
        "declared_mde": opts.mde,
        "alpha": ALPHA,
        "power": POWER,
        "repeats": opts.repeats.max(1),
        "cluster_by": opts.cluster_by,
        "replay_forbidden_for_model_comparison": true,
        "suite": {
            "n_cases": n_cases,
            "n_clusters": n_clusters,
            "mean": f64_or_null(mean),
            "se_clt": f64_or_null(se_clt),
            "se_clustered": f64_or_null(se_clustered),
            "se_ratio": f64_or_null(se_ratio),
            // Which SE the suite CI actually used, and why (a one-cluster suite
            // cannot use the cluster-robust zero).
            "se_used": f64_or_null(suite_se.se),
            "se_source": suite_se.source,
            "small_cluster_warning": small_cluster_warning,
            "ci95_low": f64_or_null(suite_row.ci_low),
            "ci95_high": f64_or_null(suite_row.ci_high),
            "n_required_for_mde": f64_or_null(suite_row.n_required),
            "resolution_ratio_q": f64_or_null(suite_row.q),
            "mde_at_n": f64_or_null(suite_row.mde_at_n),
            "verdict": suite_row.verdict,
            "label": suite_row.label,
        },
        "by_cluster": by_cluster_json,
        "baseline_comparison": baseline_comparison,
    });

    StatsBundle {
        json,
        header_model,
        verdict: top_verdict,
        label: top_label,
        resolution_ratio_q: top_q,
        verdict_is_baseline_comparison,
        n_cases,
        n_clusters,
        pass_pct: mean * 100.0,
        ci_halfwidth_pct: (suite_row.ci_high - mean) * 100.0,
        mde_at_n_pct: suite_row.mde_at_n * 100.0,
        small_cluster_warning,
        se_ratio,
        se_source: suite_se.source,
    }
}

pub(super) fn verdict_word(v: stats::Verdict) -> &'static str {
    match v {
        stats::Verdict::Pass => "pass",
        stats::Verdict::Fail => "fail",
        stats::Verdict::Unresolved => "unresolved",
    }
}

/// One-line honest-statistics summary + `se_ratio > 2` / small-cluster
/// design-effect warnings. Silent (no line at all) when the suite has zero
/// real cases — every number would be NaN and printing `NaN%` is noise, not
/// honesty.
///
/// The verdict word carries a `(vs baseline)`/`(vs chance)` suffix — see
/// `StatsBundle::verdict`'s doc comment for why the top-level verdict and
/// `stats.suite.verdict` can legitimately disagree (they answer different
/// questions) and why that isn't a contradiction.
fn print_stats_summary(s: &StatsBundle) {
    if s.n_cases == 0 {
        return;
    }
    let vs = if s.verdict_is_baseline_comparison {
        "vs baseline"
    } else {
        "vs chance"
    };
    println!(
        "  n={} clusters={} pass={:.1}% ±{:.1}pp ({}) | MDE@n={:.1}pp | q={:.2} → {} ({vs})",
        s.n_cases,
        s.n_clusters,
        s.pass_pct,
        s.ci_halfwidth_pct,
        // Naming the estimator matters: with one cluster the CI is NOT clustered
        // (the clustered estimator is identically zero there), and labelling it
        // "clustered" was its own small lie.
        match s.se_source {
            matrix::SeSource::Clustered => "clustered",
            matrix::SeSource::CltSingleCluster => "unclustered CLT, 1 cluster",
            matrix::SeSource::Undefined => "undefined",
        },
        s.mde_at_n_pct,
        s.resolution_ratio_q,
        verdict_word(s.verdict),
    );
    if s.small_cluster_warning {
        println!(
            "  {} only {} clusters — clustered SE is unreliable (Miller App. C)",
            style("WARNING:").yellow().bold(),
            s.n_clusters,
        );
    }
    if s.se_ratio.is_finite() && s.se_ratio > 2.0 {
        println!(
            "  {} clustered SE is {:.1}x the unclustered estimate — cases within a directory are highly correlated; the unclustered number is overconfident",
            style("WARNING:").yellow().bold(),
            s.se_ratio,
        );
    }
    println!();
}

/// Console + optional JSON report rendering (style mirrors `duduclaw test`).
fn render(reports: &[CaseReport], opts: &EvalOptions) -> duduclaw_core::error::Result<()> {
    println!();
    println!(
        "  {} {}",
        style("🧪").bold(),
        style("Agent Behavior Eval").bold()
    );
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
            let mark = if a.passed {
                style("ok").green()
            } else {
                style("FAIL").red()
            };
            println!("         [{mark}] {} — {}", a.name, a.detail);
        }
        if let Some(j) = &r.judge {
            let mark = if j.passed {
                style("ok").green()
            } else {
                style("FAIL").red()
            };
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

    // ── P0/WP-D: honest statistics (Miller 2024 / resolution diagnostics /
    // Replay Gap) — computed once, printed always, embedded in `--report`.
    let stats_bundle = build_stats(reports, opts);
    print_stats_summary(&stats_bundle);

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
            // P0/WP-D additive: header model, used by the Replay Gap guard
            // when this report is later fed to another run as `--baseline`.
            "model": stats_bundle.header_model,
            // P2 additive: which runtime answered. Same "unknown" / shared /
            // "mixed:a,b" vocabulary as `model`.
            "runtime": header_model_string(reports.iter().map(|r| r.runtime.as_str())),
            // P2 additive: `null` ⇒ every case ran under its own `[case] agent`.
            // A non-null value means one provisioned agent carried the whole run,
            // which changes the system prompt every case ran under — declared
            // here rather than left to be inferred from the per-case rows.
            "agent_override": opts.agent,
            "total": total,
            "passed": passed,
            "failed": total - passed,
            "mast_breakdown": mast_breakdown.iter().map(|(label, count)| serde_json::json!({
                "label": label,
                "count": count,
            })).collect::<Vec<_>>(),
            "per_case": per_case_json(reports),
            "cases": reports,
            // P0/WP-D additive: statistically honest reporting block +
            // top-level verdict/label/resolution_ratio_q mirror.
            "stats": stats_bundle.json,
            "verdict": stats_bundle.verdict,
            "label": stats_bundle.label,
            "resolution_ratio_q": f64_or_null(stats_bundle.resolution_ratio_q),
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

    fn report(
        name: &str,
        passed: bool,
        error: Option<&str>,
        assertions: Vec<(&str, bool)>,
    ) -> CaseReport {
        CaseReport {
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
            ..CaseReport::blank(name, name, "p")
        }
    }

    #[test]
    fn mast_breakdown_attributes_failures() {
        let reports = vec![
            report("ok", true, None, vec![("must_use_tools: x", true)]),
            report(
                "spec-fail",
                false,
                None,
                vec![("must_use_tools: tasks_create", false)],
            ),
            report(
                "spec-fail-2",
                false,
                None,
                vec![("output_contains: \"x\"", false)],
            ),
            report("infra", false, Some("spawn failed"), vec![]),
            report(
                "judge-only",
                false,
                None,
                vec![("output_contains: \"x\"", true)],
            ),
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
            repeats: 1,
            baseline: None,
            mde: 0.10,
            cluster_by: "dir".to_string(),
            runtime: None,
            model: None,
            paired_seeds: false,
            temperature: None,
            matrix: false,
            team_2x2: false,
            planner_weak: None,
            planner_strong: None,
            executor_weak: None,
            executor_strong: None,
            verifier_model: None,
            team_effort: None,
            team_fanout: None,
            team_grok_sandbox_off: false,
            roles: Vec::new(),
            models: Vec::new(),
            weak: None,
            strong: None,
            domain: Vec::new(),
            budget_usd: None,
            max_cases: None,
            agent: None,
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
        let root = write_suite(dir.path(), "[expect]\nmust_use_tools = [\"Bash\"]\n\n", "");
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
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/examples");

        case::load_case(&examples.join("refund-flow.toml")).unwrap();
        case::load_case(&examples.join("greeting-replay.toml")).unwrap();
        case::load_case(&examples.join("grounded-replay.toml")).unwrap();

        let judge = StubJudge("unused");
        let o = EvalOptions {
            path: Some(examples.join("greeting-replay.toml")),
            no_judge: true,
            ..opts(&examples)
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
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/examples");
        let judge = StubJudge("unused");
        let o = EvalOptions {
            path: Some(examples.join("grounded-replay.toml")),
            no_judge: true,
            ..opts(&examples)
        };
        let reports = run_eval(&examples, &o, &judge).await;
        assert_eq!(reports.len(), 1);
        assert!(
            reports[0].passed,
            "shipped grounded example failed: error={:?} assertions={:?}",
            reports[0].error, reports[0].assertions
        );
        assert!(
            reports[0]
                .assertions
                .iter()
                .any(|a| a.name.starts_with("grounded:") && a.passed)
        );
    }

    // ── P2: runtime/model overrides + matrix-flag gating ──────────────

    #[test]
    fn matrix_only_flags_are_refused_without_matrix() {
        let dir = tempfile::tempdir().unwrap();
        for (mutate, expected) in [
            (
                Box::new(|o: &mut EvalOptions| o.roles = vec!["executor".into()])
                    as Box<dyn Fn(&mut EvalOptions)>,
                "--roles",
            ),
            (
                Box::new(|o: &mut EvalOptions| o.models = vec!["claude:m".into()]),
                "--models",
            ),
            (
                Box::new(|o: &mut EvalOptions| o.weak = Some("claude:m".into())),
                "--weak",
            ),
            (
                Box::new(|o: &mut EvalOptions| o.strong = Some("claude:m".into())),
                "--strong",
            ),
            (
                Box::new(|o: &mut EvalOptions| o.domain = vec![PathBuf::from("x")]),
                "--domain",
            ),
            (
                Box::new(|o: &mut EvalOptions| o.budget_usd = Some(1.0)),
                "--budget-usd",
            ),
            (
                Box::new(|o: &mut EvalOptions| o.max_cases = Some(3)),
                "--max-cases",
            ),
            (
                Box::new(|o: &mut EvalOptions| o.temperature = Some(1.0)),
                "--temperature",
            ),
        ] {
            let mut o = opts(dir.path());
            mutate(&mut o);
            assert_eq!(
                first_matrix_only_flag(&o),
                Some(expected),
                "{expected} must be refused without --matrix"
            );
        }
        // A plain invocation sets none of them.
        assert_eq!(first_matrix_only_flag(&opts(dir.path())), None);
    }

    #[test]
    fn team_probe_role_flags_require_explicit_probe_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        o.matrix = true;
        o.planner_weak = Some("claude:claude-haiku-4-5".into());
        assert_eq!(first_team_probe_only_flag(&o), Some("--planner-weak"));
        o.planner_weak = None;
        o.team_grok_sandbox_off = true;
        assert_eq!(
            first_team_probe_only_flag(&o),
            Some("--team-grok-sandbox-off")
        );
        o.team_grok_sandbox_off = false;
        o.team_2x2 = true;
        assert!(super::team_probe::ProbeSpec::parse(&o).is_err());
    }

    #[test]
    fn team_probe_rejects_replay_and_unapplied_seeds_before_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        o.matrix = true;
        o.team_2x2 = true;
        o.replay = false;
        o.report = Some(dir.path().join("probe.json"));
        o.planner_weak = Some("codex:gpt-5.6-terra".into());
        o.planner_strong = Some("codex:gpt-5.6-sol".into());
        o.executor_weak = Some("codex:gpt-5.6-terra".into());
        o.executor_strong = Some("codex:gpt-5.6-sol".into());
        o.verifier_model = Some("antigravity:gemini-3.7-flash".into());
        assert!(super::team_probe::ProbeSpec::parse(&o).is_ok());
        o.team_fanout = Some(4);
        assert!(
            super::team_probe::ProbeSpec::parse(&o)
                .unwrap_err()
                .contains("--team-fanout")
        );
        o.team_fanout = Some(2);
        assert!(super::team_probe::ProbeSpec::parse(&o).is_ok());
        o.paired_seeds = true;
        assert!(
            super::team_probe::ProbeSpec::parse(&o)
                .unwrap_err()
                .contains("cannot be applied")
        );
        o.paired_seeds = false;
        o.replay = true;
        assert!(
            super::team_probe::ProbeSpec::parse(&o)
                .unwrap_err()
                .contains("live team round")
        );
    }

    #[test]
    fn the_agent_override_is_validated_and_reaches_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        o.agent = Some("../escape".to_string());
        assert!(
            base_overrides(&o)
                .unwrap_err()
                .contains("not a valid agent id"),
            "a path-traversal agent id must never reach `home/agents/<id>`"
        );
        o.agent = Some("  ".to_string());
        assert_eq!(base_overrides(&o).unwrap().agent, None);
        o.agent = Some("agnes".to_string());
        assert_eq!(base_overrides(&o).unwrap().agent.as_deref(), Some("agnes"));
        // `--agent` works on the ordinary single-suite path too, so it must NOT
        // be in the matrix-only refusal list.
        assert_eq!(first_matrix_only_flag(&o), None);
    }

    #[tokio::test]
    async fn the_agent_override_is_recorded_per_case() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"Refund approved\"]\n\n",
            "",
        );
        let judge = StubJudge("unused");
        let o = EvalOptions {
            agent: Some("agnes".to_string()),
            ..opts(&root)
        };
        let reports = run_eval(dir.path(), &o, &judge).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].agent, "agnes");
        // Without the override the case's own agent is recorded.
        let plain = run_eval(dir.path(), &opts(&root), &judge).await;
        assert_eq!(plain[0].agent, "support-bot");
    }

    #[test]
    fn record_is_refused_under_a_runtime_or_model_override() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        // Baseline: no override ⇒ recording is fine.
        assert_eq!(record_override_conflict(&o), None);
        // `--runtime claude` names the path the case already takes.
        o.runtime = Some("claude".to_string());
        assert_eq!(record_override_conflict(&o), None);
        o.runtime = Some("  ".to_string());
        assert_eq!(record_override_conflict(&o), None);
        o.runtime = Some("codex".to_string());
        assert_eq!(record_override_conflict(&o), Some("--runtime (non-claude)"));
        o.runtime = None;
        o.model = Some("claude-opus-4-6".to_string());
        assert_eq!(record_override_conflict(&o), Some("--model"));
        o.model = Some("".to_string());
        assert_eq!(record_override_conflict(&o), None);
    }

    #[test]
    fn an_unknown_runtime_is_refused_not_silently_treated_as_claude() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        o.runtime = Some("clause".to_string());
        let err = base_overrides(&o).unwrap_err();
        assert!(err.contains("not a runtime"), "{err}");

        // Blank / absent ⇒ NO CLI override. What runs is then decided by
        // `effective_runtime(case)`: the case's own `[case] runtime` if it
        // pins one, else Claude (review finding 10 — before that fix this
        // assertion was on `is_claude_cli_path()`, which could not see the
        // case at all and so hard-coded Claude for a codex-pinned case).
        o.runtime = Some("   ".to_string());
        assert_eq!(base_overrides(&o).unwrap().runtime, None);
        o.runtime = None;
        assert_eq!(base_overrides(&o).unwrap().runtime, None);
        o.runtime = Some("codex".to_string());
        assert_eq!(
            base_overrides(&o).unwrap().runtime,
            Some(duduclaw_core::types::RuntimeType::Codex)
        );
    }

    #[tokio::test]
    async fn a_model_override_wins_over_the_case_model_in_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"Refund approved\"]\n\n",
            "",
        );
        let judge = StubJudge("unused");
        let o = EvalOptions {
            model: Some("claude-opus-4-6".to_string()),
            paired_seeds: true,
            ..opts(&root)
        };
        let reports = run_eval(dir.path(), &o, &judge).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].model, "claude-opus-4-6");
        assert_eq!(reports[0].runtime, "claude");
        // `--paired-seeds` records a seed even at `--repeats 1`.
        assert!(reports[0].seed.is_some());
        assert_eq!(
            reports[0].seed,
            Some(matrix::derive_seed(&reports[0].id, 1))
        );
    }

    #[tokio::test]
    async fn discovery_failure_is_one_failed_report() {
        let dir = tempfile::tempdir().unwrap();
        let judge = StubJudge("unused");
        let o = EvalOptions {
            path: Some(dir.path().join("missing")),
            no_judge: true,
            ..opts(dir.path())
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
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
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
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
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
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
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
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
        // A same-named case nested under a subdirectory: same stable id
        // (`refund-flow`) as the top-level case — must be rejected, not
        // silently shadow one result with the other's.
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        add_case(&nested, "refund-flow");

        let reports = run_eval(dir.path(), &opts(&root), &StubJudge("unused")).await;
        assert_eq!(
            reports.len(),
            1,
            "suite fails fast as a single report, no partial run"
        );
        assert!(!reports[0].passed);
        let err = reports[0].error.as_ref().expect("error must be set");
        assert!(err.contains("duplicate case id"), "unexpected error: {err}");
        assert!(
            err.contains("refund-flow"),
            "error should name the colliding id: {err}"
        );
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
        assert!(
            !reports[0].passed,
            "must_use_tools = Bash never fires in TRANSCRIPT"
        );
        render(&reports, &o).unwrap();

        let raw = std::fs::read_to_string(&report_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(
            json["suite"],
            serde_json::json!(root.file_name().unwrap().to_str().unwrap())
        );
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
            failed
                .iter()
                .any(|v| v.as_str().unwrap().contains("must_use_tools")),
            "failed_assertions should name the failing check: {failed:?}"
        );
        assert!(
            case["mast_class"].is_string(),
            "a failed case must carry a mast_class"
        );
        assert!(
            case["judge_score"].is_null(),
            "no [judge] configured for this case"
        );
    }

    // ── P0/WP-D: statistically honest reporting ───────────────────────

    #[test]
    fn cluster_key_for_uses_relative_directory_or_dot() {
        let root = Path::new("/evals");
        assert_eq!(
            cluster_key_for(root, Path::new("/evals/support/refund.toml")),
            "support"
        );
        assert_eq!(cluster_key_for(root, Path::new("/evals/refund.toml")), ".");
        assert_eq!(cluster_key_for(root, Path::new("/evals/a/b/c.toml")), "a/b");
    }

    #[test]
    fn f64_or_null_guards_non_finite() {
        assert_eq!(f64_or_null(1.5), serde_json::json!(1.5));
        assert_eq!(f64_or_null(f64::NAN), serde_json::Value::Null);
        assert_eq!(f64_or_null(f64::INFINITY), serde_json::Value::Null);
        assert_eq!(f64_or_null(f64::NEG_INFINITY), serde_json::Value::Null);
    }

    #[test]
    fn header_model_string_variants() {
        assert_eq!(header_model_string(std::iter::empty()), "unknown");
        assert_eq!(header_model_string(["m1", "m1"].into_iter()), "m1");
        assert_eq!(header_model_string(["m2", "m1"].into_iter()), "mixed:m1,m2");
        // Empty strings (unresolved model, e.g. a discovery-failure sentinel
        // report) are excluded, never counted as a distinct "model".
        assert_eq!(header_model_string(["", "m1", ""].into_iter()), "m1");
    }

    #[test]
    fn build_stats_aggregates_repeats_by_case_id_and_clusters_by_directory() {
        let o = EvalOptions {
            repeats: 2,
            ..opts(Path::new("evals"))
        };
        // c1 (dirA): 1 pass, 1 fail → rate 0.5. c2 (dirB): 2 passes → rate 1.0.
        // Suite mean over the two per-case rates = (0.5 + 1.0) / 2 = 0.75.
        let reports = vec![
            CaseReport {
                passed: true,
                cluster: "dirA".into(),
                repeat_index: Some(1),
                model: "m".into(),
                ..CaseReport::blank("c1", "c1", "p")
            },
            CaseReport {
                passed: false,
                cluster: "dirA".into(),
                repeat_index: Some(2),
                model: "m".into(),
                ..CaseReport::blank("c1", "c1", "p")
            },
            CaseReport {
                passed: true,
                cluster: "dirB".into(),
                repeat_index: Some(1),
                model: "m".into(),
                ..CaseReport::blank("c2", "c2", "p")
            },
            CaseReport {
                passed: true,
                cluster: "dirB".into(),
                repeat_index: Some(2),
                model: "m".into(),
                ..CaseReport::blank("c2", "c2", "p")
            },
        ];
        let bundle = build_stats(&reports, &o);
        assert_eq!(
            bundle.n_cases, 2,
            "two distinct case ids, repeats aggregated"
        );
        assert_eq!(bundle.n_clusters, 2);
        assert!(
            (bundle.pass_pct - 75.0).abs() < 1e-6,
            "got {}",
            bundle.pass_pct
        );
        assert_eq!(bundle.header_model, "m");
        assert!(
            bundle.json["baseline_comparison"].is_null(),
            "no --baseline given"
        );
        // Live-fire fix: 2 clusters is below MIN_RELIABLE_CLUSTERS (5) — the
        // se_ratio computed here has no statistical meaning at this count.
        assert!(bundle.small_cluster_warning, "2 clusters must warn");
        assert_eq!(
            bundle.json["suite"]["small_cluster_warning"],
            serde_json::json!(true)
        );
        assert!(
            !bundle.verdict_is_baseline_comparison,
            "no --baseline given, so the top-level verdict is the standalone chance-line check"
        );
    }

    #[test]
    fn small_cluster_warning_is_false_at_or_above_five_clusters() {
        let o = opts(Path::new("evals"));
        // 5 distinct single-case clusters — right at MIN_RELIABLE_CLUSTERS,
        // must NOT warn.
        let reports: Vec<CaseReport> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|dir| CaseReport {
                passed: true,
                cluster: (*dir).to_string(),
                model: "m".into(),
                ..CaseReport::blank(*dir, *dir, "p")
            })
            .collect();
        let bundle = build_stats(&reports, &o);
        assert_eq!(bundle.n_clusters, 5);
        assert!(!bundle.small_cluster_warning, "5 clusters must not warn");
        assert_eq!(
            bundle.json["suite"]["small_cluster_warning"],
            serde_json::json!(false)
        );
    }

    #[tokio::test]
    async fn cmd_eval_rejects_unsupported_cluster_by_and_nonsense_mde() {
        let dir = tempfile::tempdir().unwrap();

        let mut o = opts(dir.path());
        o.cluster_by = "case".to_string();
        let err = cmd_eval(dir.path(), o).await.unwrap_err();
        assert!(err.to_string().contains("cluster-by"), "unexpected: {err}");

        let mut o2 = opts(dir.path());
        o2.mde = 0.0;
        let err2 = cmd_eval(dir.path(), o2).await.unwrap_err();
        assert!(err2.to_string().contains("mde"), "unexpected: {err2}");
    }

    /// Live-fire regression (2026-09-24): `--repeats 2 --replay` used to
    /// silently look for `.r1.jsonl`/`.r2.jsonl` replay files that
    /// `--record` never wrote (the pre-existing baseline is the plain
    /// `<case>.transcript.jsonl`), failing every case with a misleading
    /// "transcript missing" error instead of an explicit rejection.
    #[tokio::test]
    async fn cmd_eval_rejects_repeats_greater_than_one_combined_with_replay() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        o.repeats = 2;
        assert!(o.replay, "opts() defaults to replay: true");
        let err = cmd_eval(dir.path(), o).await.unwrap_err();
        assert!(err.to_string().contains("--repeats"), "unexpected: {err}");
        assert!(err.to_string().contains("--replay"), "unexpected: {err}");

        // repeats == 1 (the default) is unaffected — proven by every other
        // `opts()`-based replay test in this module already passing.
    }

    #[tokio::test]
    async fn repeats_run_each_case_n_times_with_distinguishable_transcripts_and_aggregate() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
        // This exercises `run_eval`/`runner::transcript_path` directly (not
        // `cmd_eval`, which now rejects `--repeats > 1` combined with
        // `--replay` — see `cmd_eval_rejects_repeats_greater_than_one_combined_with_replay`
        // above): replay is used only as a deterministic, credential-free
        // way to test the `.r<N>` transcript-seeding + aggregation plumbing
        // that a real `--record --repeats N` live run also exercises. One
        // repeat passes, one doesn't, to prove aggregation.
        let failing_transcript = concat!(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"no info\"}]}}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"\"}\n",
        );
        std::fs::write(root.join("refund-flow.transcript.r1.jsonl"), TRANSCRIPT).unwrap();
        std::fs::write(
            root.join("refund-flow.transcript.r2.jsonl"),
            failing_transcript,
        )
        .unwrap();

        let o = EvalOptions {
            repeats: 2,
            ..opts(&root)
        };
        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        assert_eq!(reports.len(), 2, "one report per repeat");
        assert!(reports.iter().all(|r| r.id == "refund-flow"));
        assert_eq!(reports[0].repeat_index, Some(1));
        assert_eq!(reports[1].repeat_index, Some(2));
        assert_eq!(reports.iter().filter(|r| r.passed).count(), 1);

        let bundle = build_stats(&reports, &o);
        assert_eq!(bundle.n_cases, 1, "both repeats aggregate into one case id");
        assert!(
            (bundle.pass_pct - 50.0).abs() < 1e-6,
            "got {}",
            bundle.pass_pct
        );
    }

    #[tokio::test]
    async fn baseline_replay_gap_guard_refuses_cross_model_replay_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
        let baseline_path = dir.path().join("baseline.json");
        std::fs::write(
            &baseline_path,
            serde_json::json!({
                "mode": "replay",
                "model": "claude-haiku-4-5",
                "per_case": [{"id": "refund-flow", "passed": true}],
            })
            .to_string(),
        )
        .unwrap();

        // `opts()` defaults to `replay: true` and the case pins no model
        // (falls back to `case::DEFAULT_EVAL_MODEL`, which is not
        // "claude-haiku-4-5") — both sides are replay mode with different
        // models, so the comparison must be refused, not fabricated.
        let o = EvalOptions {
            baseline: Some(baseline_path),
            ..opts(&root)
        };
        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        let bundle = build_stats(&reports, &o);
        let err = bundle.json["baseline_comparison"]["error"]
            .as_str()
            .expect("must carry an error, never a fabricated cross-model comparison");
        assert!(err.contains("Replay Gap"), "unexpected: {err}");
        // The top-level verdict must not be silently sourced from the
        // refused comparison — it falls back to the standalone check.
        assert_ne!(bundle.resolution_ratio_q, 0.0);
        assert!(
            !bundle.verdict_is_baseline_comparison,
            "a refused comparison must not claim to drive the top-level verdict"
        );
    }

    #[tokio::test]
    async fn baseline_comparison_computes_paired_delta_when_models_match() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
        let baseline_path = dir.path().join("baseline.json");
        std::fs::write(
            &baseline_path,
            serde_json::json!({
                "mode": "live",
                "model": case::DEFAULT_EVAL_MODEL,
                "per_case": [{"id": "refund-flow", "passed": false}],
            })
            .to_string(),
        )
        .unwrap();

        // Current run is `replay` but the baseline's `live` + matching model
        // means this is NOT a Replay Gap violation (same model throughout).
        let o = EvalOptions {
            baseline: Some(baseline_path),
            ..opts(&root)
        };
        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        let bundle = build_stats(&reports, &o);
        let bc = &bundle.json["baseline_comparison"];
        assert!(bc.get("error").is_none(), "must not be refused: {bc:?}");
        assert_eq!(bc["n"], serde_json::json!(1));
        // candidate passed (1.0) - baseline failed (0.0) = paired_delta 1.0.
        assert!((bc["paired_delta"].as_f64().unwrap() - 1.0).abs() < 1e-9);
        // Top-level precedence: an accepted baseline comparison drives the
        // top-level verdict, distinct from `stats.suite.verdict`.
        assert!(
            bundle.verdict_is_baseline_comparison,
            "an accepted --baseline comparison must drive the top-level verdict"
        );
    }

    #[tokio::test]
    async fn baseline_with_no_overlapping_ids_reports_error_not_a_fabricated_tie() {
        let dir = tempfile::tempdir().unwrap();
        let root = write_suite(
            dir.path(),
            "[expect]\noutput_contains = [\"order #1234\"]\n\n",
            "",
        );
        let baseline_path = dir.path().join("baseline.json");
        std::fs::write(
            &baseline_path,
            serde_json::json!({
                "mode": "live",
                "model": case::DEFAULT_EVAL_MODEL,
                "per_case": [{"id": "some-other-case", "passed": true}],
            })
            .to_string(),
        )
        .unwrap();

        let o = EvalOptions {
            baseline: Some(baseline_path),
            ..opts(&root)
        };
        let reports = run_eval(dir.path(), &o, &StubJudge("unused")).await;
        let bundle = build_stats(&reports, &o);
        let err = bundle.json["baseline_comparison"]["error"]
            .as_str()
            .expect("no overlap must be an explicit error");
        assert!(err.contains("no overlapping"), "unexpected: {err}");
    }
}
