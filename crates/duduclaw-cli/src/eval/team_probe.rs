//! P2b full-team 2×2 probe. Each of the four arms invokes the production
//! composer on a fresh copy of the eval home; the score is the independent
//! verifier's actual PASS bit, never the planner's prose or a replay.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use duduclaw_core::role_model_matrix::{
    MatrixCell, MatrixHeader, MatrixRole, MatrixVerdict, PlannerState, RoleModelMatrix, matrix_path,
};
use duduclaw_core::types::{RoleSpec, TeamConfig, validate_team};
use duduclaw_gateway::task_store::TaskRow;
use duduclaw_gateway::team_composer::{
    FrozenTeamSpec, TeamRoundContext, TeamRoundOutcome, run_team_round,
};
use serde::Serialize;

use super::case;
use super::matrix::{self, CostSource, ModelRef};
use super::{
    ALPHA, EvalOptions, POWER, case_id, case_key, cluster_key_for, f64_or_null, path_excluded,
    resolution_row, stats,
};

#[derive(Debug, Clone)]
pub(super) struct ProbeSpec {
    planner: [ModelRef; 2],
    executor: [ModelRef; 2],
    verifier: ModelRef,
    effort: Option<String>,
    fanout: u8,
    grok_sandbox_off: bool,
    domains: Vec<PathBuf>,
    report: PathBuf,
}

fn required_model(flag: &str, value: &Option<String>) -> Result<ModelRef, String> {
    ModelRef::parse(
        value
            .as_deref()
            .ok_or_else(|| format!("--team-2x2 requires {flag}"))?,
    )
}

impl ProbeSpec {
    pub(super) fn parse(opts: &EvalOptions) -> Result<Self, String> {
        if !opts.matrix || opts.replay || opts.record {
            return Err("--team-2x2 requires --matrix and forbids --replay/--record: every arm must run a live team round".into());
        }
        if !opts.roles.is_empty()
            || !opts.models.is_empty()
            || opts.weak.is_some()
            || opts.strong.is_some()
            || opts.runtime.is_some()
            || opts.model.is_some()
        {
            return Err("--team-2x2 uses its explicit role model flags; do not combine it with --roles/--models/--weak/--strong/--runtime/--model".into());
        }
        if opts.paired_seeds {
            return Err("--paired-seeds cannot be applied by the production team composer; refusing a false paired-seed claim".into());
        }
        if opts.no_judge || opts.baseline.is_some() || opts.temperature.is_some() {
            return Err("--team-2x2 always uses its independent verifier and does not support --no-judge/--baseline/--temperature".into());
        }
        if opts.repeats == 0
            || !(opts.mde.is_finite() && opts.mde > 0.0 && opts.mde < 1.0)
            || opts.cluster_by != "dir"
        {
            return Err(
                "--team-2x2 requires repeats >= 1, 0 < MDE < 1, and --cluster-by dir".into(),
            );
        }
        if opts.budget_usd.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err("--budget-usd must be a positive finite number".into());
        }
        let planner = [
            required_model("--planner-weak", &opts.planner_weak)?,
            required_model("--planner-strong", &opts.planner_strong)?,
        ];
        let executor = [
            required_model("--executor-weak", &opts.executor_weak)?,
            required_model("--executor-strong", &opts.executor_strong)?,
        ];
        if planner[0] == planner[1] || executor[0] == executor[1] {
            return Err("the weak and strong models of each role must differ".into());
        }
        let verifier = required_model("--verifier-model", &opts.verifier_model)?;
        let fanout = opts.team_fanout.unwrap_or(1);
        if !(1..=duduclaw_core::types::TEAM_EXECUTOR_FANOUT_MAX).contains(&fanout) {
            return Err(format!(
                "--team-fanout must be 1..={}",
                duduclaw_core::types::TEAM_EXECUTOR_FANOUT_MAX
            ));
        }
        let effort = opts
            .team_effort
            .as_deref()
            .map(|raw| {
                raw.parse::<duduclaw_core::effort::Effort>()
                    .map(|value| value.as_str().to_string())
                    .map_err(|_| {
                        format!("invalid --team-effort {raw:?}; expected low/medium/high/xhigh/max")
                    })
            })
            .transpose()?;
        let report = opts.report.clone().ok_or("--team-2x2 requires --report")?;
        let domains = if opts.domain.is_empty() {
            vec![opts.path.clone().unwrap_or_else(|| PathBuf::from("evals"))]
        } else {
            opts.domain.clone()
        };
        Ok(Self {
            planner,
            executor,
            verifier,
            effort,
            fanout,
            grok_sandbox_off: opts.team_grok_sandbox_off,
            domains,
            report,
        })
    }

    fn frozen(&self, p: usize, e: usize) -> Result<FrozenTeamSpec, String> {
        let role = |m: &ModelRef| RoleSpec {
            runtime: Some(m.runtime.as_str().into()),
            model: Some(m.model.clone()),
            effort: self.effort.clone(),
        };
        let mut cfg = TeamConfig {
            enabled: Some(true),
            gate: Some("always_team".into()),
            executor_fanout: Some(self.fanout.into()),
            ..Default::default()
        };
        cfg.roles.planner = role(&self.planner[p]);
        cfg.roles.executor = role(&self.executor[e]);
        cfg.roles.verifier = role(&self.verifier);
        let resolved = validate_team(&cfg)
            .map_err(|err| format!("invalid 2×2 arm planner={p} executor={e}: {err}"))?;
        Ok(FrozenTeamSpec::from_resolved(&resolved))
    }
}

#[derive(Debug, Serialize)]
struct ProbeRun {
    domain: String,
    case_id: String,
    cluster: String,
    repeat: u32,
    arm: String,
    planner: String,
    executor: String,
    verifier: String,
    score: Option<f64>,
    outcome: String,
    estimated_cost_usd: f64,
    cost_source: &'static str,
    stages: Vec<ProbeStage>,
    team_handoff_successes: u32,
    team_handoff_failures: u32,
}

#[derive(Debug, Serialize)]
struct ProbeStage {
    member_id: String,
    role: String,
    outcome: String,
    packet_path: Option<String>,
    observation_fidelity: String,
    requested_model: Option<String>,
    response_model: Option<String>,
    error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_edge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fault_side: Option<String>,
}

fn team_handoff_counts(home: &Path, member_ids: &BTreeSet<String>) -> (u32, u32) {
    let Ok(body) = std::fs::read_to_string(home.join("tool_calls.jsonl")) else {
        return (0, 0);
    };
    let mut successes = 0u32;
    let mut failures = 0u32;
    for line in body.lines() {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(agent_id) = row.get("agent_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if !member_ids.contains(agent_id) {
            continue;
        }
        let Some(tool_name) = row.get("tool_name").and_then(|v| v.as_str()) else {
            continue;
        };
        if tool_name != "team_handoff" && tool_name != "mcp__duduclaw__team_handoff" {
            continue;
        }
        match row.get("success").and_then(|v| v.as_bool()) {
            Some(true) => successes = successes.saturating_add(1),
            Some(false) => failures = failures.saturating_add(1),
            None => {}
        }
    }
    (successes, failures)
}

/// Caps on one per-run clone of the eval home.
///
/// 2026-09-28 review (`review_team.md` §3 "評測"): the byte cap and the
/// symlink refusal were already here, but nothing bounded **shape** — a deep
/// directory chain recurses without limit (the copy is recursive, so the
/// stack, not the disk, is the resource that runs out) and a directory of
/// millions of tiny files passes the byte cap while taking the run down. A
/// clone is made per arm per repeat, so any of the three is multiplied.
#[derive(Debug, Clone, Copy)]
struct CopyLimits {
    max_depth: u32,
    max_files: u64,
    max_bytes: u64,
}

impl CopyLimits {
    const DEFAULT: CopyLimits = CopyLimits {
        max_depth: 32,
        max_files: 20_000,
        max_bytes: 256 * 1024 * 1024,
    };
}

/// Running totals for one clone, checked against [`CopyLimits`].
#[derive(Debug, Default, Clone, Copy)]
struct CopyBudget {
    bytes: u64,
    files: u64,
}

fn copy_tree(src: &Path, dst: &Path, budget: &mut CopyBudget) -> Result<(), String> {
    copy_tree_limited(src, dst, budget, 0, &CopyLimits::DEFAULT)
}

fn copy_tree_limited(
    src: &Path,
    dst: &Path,
    budget: &mut CopyBudget,
    depth: u32,
    limits: &CopyLimits,
) -> Result<(), String> {
    if depth > limits.max_depth {
        return Err(format!(
            "--team-2x2 refuses an eval home nested deeper than {} directories (at {}); use an \
             isolated slim home",
            limits.max_depth,
            src.display()
        ));
    }
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_tree_limited(&path, &target, budget, depth + 1, limits)?;
        } else if ty.is_file() {
            budget.files = budget.files.saturating_add(1);
            if budget.files > limits.max_files {
                return Err(format!(
                    "--team-2x2 needs an eval home with at most {} files; use an isolated slim \
                     home",
                    limits.max_files
                ));
            }
            let size = entry.metadata().map_err(|e| e.to_string())?.len();
            budget.bytes = budget
                .bytes
                .checked_add(size)
                .ok_or("eval home is too large")?;
            if budget.bytes > limits.max_bytes {
                return Err(format!(
                    "--team-2x2 needs an eval home smaller than {} MiB; use an isolated slim home",
                    limits.max_bytes / (1024 * 1024)
                ));
            }
            std::fs::copy(&path, &target).map_err(|e| format!("copy {}: {e}", path.display()))?;
        } else {
            return Err(format!(
                "eval home contains a symlink or special file: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Real measured cost for one probe run, or `None` when the telemetry has
/// nothing for it (the singleton was bound elsewhere, or no leg reported
/// usage). The caller falls back to a coarse list-price estimate and — since
/// the 2026-09-28 review — says so on the console instead of silently.
///
/// Opened **read-only**: `Connection::open` creates the file, so a probe
/// against a home with no telemetry used to leave an empty
/// `cost_telemetry.db` behind in the operator's eval home. A reader that
/// creates what it is reading is not a reader.
fn measured_cost(
    home: &Path,
    task_id: &str,
    registry: &duduclaw_llm::ModelRegistry,
) -> Option<f64> {
    let db = rusqlite::Connection::open_with_flags(
        home.join("cost_telemetry.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let mut stmt = db.prepare("SELECT model, SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens) FROM token_usage WHERE episode_id = ?1 GROUP BY model").ok()?;
    let mut rows = stmt.query([task_id]).ok()?;
    let mut cost = 0.0;
    let mut found = false;
    while let Some(row) = rows.next().ok()? {
        let model: String = row.get(0).ok()?;
        let usage = super::runner::ReportedUsage {
            input_tokens: row.get::<_, i64>(1).ok()?.max(0) as u64,
            output_tokens: row.get::<_, i64>(2).ok()?.max(0) as u64,
            cache_read_tokens: row.get::<_, i64>(3).ok()?.max(0) as u64,
        };
        let (usd, source) = matrix::run_cost_usd(registry, &model, Some(usage));
        if source != CostSource::Reported {
            return None;
        }
        cost += usd;
        found = true;
    }
    found.then_some(cost)
}

/// Collapse `K` repeats of one case into that case's own mean, per arm.
///
/// Input is keyed `(domain, case_id, repeat)`; output is keyed
/// `(domain, case_id)`. For each of the four arms the value is the mean over
/// the repeats that actually produced a score, and `None` when **no** repeat
/// did — a case whose arm never completed stays incomplete rather than being
/// silently averaged from nothing.
///
/// Why this exists at all (Miller 2024, arXiv:2411.00640 §2.3): K repeats of
/// one case are not K independent cases. Treating them as such shrinks every
/// interval by roughly √K, which is resolution the experiment never bought.
/// `MatrixCell::n`'s own doc contracts this unit ("Distinct cases contributing
/// to `mean` (NOT runs: `K` repeats of one case aggregate into that case's own
/// pass rate first)").
///
/// The cluster key travels with the case; repeats of one case always share it,
/// so the first one seen is authoritative.
type ArmScores = ([Option<f64>; 4], String);
fn fold_repeats_per_case(
    matched: &BTreeMap<(String, String, u32), ArmScores>,
) -> BTreeMap<(String, String), ArmScores> {
    let mut sums: BTreeMap<(String, String), ([(f64, u32); 4], String)> = BTreeMap::new();
    for ((domain, case_id, _repeat), (values, cluster)) in matched {
        let entry = sums
            .entry((domain.clone(), case_id.clone()))
            .or_insert(([(0.0, 0); 4], cluster.clone()));
        for (arm, value) in values.iter().enumerate() {
            if let Some(v) = value {
                entry.0[arm].0 += *v;
                entry.0[arm].1 += 1;
            }
        }
    }
    sums.into_iter()
        .map(|(key, (arms, cluster))| {
            let mut out: [Option<f64>; 4] = [None; 4];
            for (arm, (sum, n)) in arms.iter().enumerate() {
                if *n > 0 {
                    out[arm] = Some(sum / f64::from(*n));
                }
            }
            (key, (out, cluster))
        })
        .collect()
}

/// Build one matrix cell from an arm's per-case scores.
///
/// `conditioned_on` names what the OTHER roles were fixed at for this arm —
/// see [`duduclaw_core::role_model_matrix::MatrixCell::conditioned_on`]. In a
/// 2×2 probe the planner arms are measured with a strong executor and the
/// executor arms with a strong planner, so two cells in the same file are NOT
/// measured under the same conditions; before the 2026-09-28 review nothing in
/// the persisted file said so.
fn cell_from_scores(
    domain: &str,
    role: MatrixRole,
    model: &ModelRef,
    values: &[(String, f64)],
    mde: f64,
    conditioned_on: &str,
) -> Option<MatrixCell> {
    if values.is_empty() {
        return None;
    }
    let scores: Vec<f64> = values.iter().map(|(_, score)| *score).collect();
    let clusters: Vec<&str> = values.iter().map(|(cluster, _)| cluster.as_str()).collect();
    let chosen = matrix::choose_se(&scores, &clusters);
    let mean = stats::mean(&scores);
    let row = resolution_row(mean, chosen.se, scores.len() as f64, mde, 0.5);
    let verdict = if chosen.is_degenerate() {
        MatrixVerdict::Unresolved
    } else {
        match row.verdict {
            stats::Verdict::Pass => MatrixVerdict::Pass,
            stats::Verdict::Fail => MatrixVerdict::Fail,
            stats::Verdict::Unresolved => MatrixVerdict::Unresolved,
        }
    };
    let mut cell = MatrixCell::new(
        domain,
        role,
        model.runtime.as_str(),
        &model.model,
        scores.len(),
        mean,
        row.ci_low,
        row.ci_high,
        verdict,
        chrono::Utc::now().to_rfc3339(),
        row.mde_at_n,
    )
    .with_conditioned_on(conditioned_on);
    if chosen.is_degenerate() {
        // A single all-PASS/all-FAIL row has no estimable variance. A
        // zero-width [0,0] interval and MDE=0 would imply false certainty.
        cell.ci95_low = None;
        cell.ci95_high = None;
        cell.mde = None;
    }
    Some(cell)
}

fn score_round(result: TeamRoundOutcome) -> (Option<f64>, String) {
    match result {
        TeamRoundOutcome::Submitted {
            verifier_passed, ..
        } => (
            verifier_passed.map(|v| if v { 1.0 } else { 0.0 }),
            "submitted".into(),
        ),
        TeamRoundOutcome::NeedsHuman { reason, pause } => {
            (None, format!("needs_human:{}:{reason}", pause.as_str()))
        }
        TeamRoundOutcome::Failed { error } => (
            None,
            format!("infra:{}", duduclaw_core::truncate_chars(&error, 160)),
        ),
        TeamRoundOutcome::SoloFallback { reason } => {
            (None, format!("invalid_solo_fallback:{reason}"))
        }
    }
}

pub async fn run_team_probe(home: &Path, opts: &EvalOptions) -> duduclaw_core::error::Result<()> {
    let err = duduclaw_core::error::DuDuClawError::Agent;
    let spec = ProbeSpec::parse(opts).map_err(err)?;
    // The cost store is a process-wide singleton. Pin it to the caller's
    // dedicated eval home before dispatching any temporary arm; each arm's
    // tools and task data still live only in that arm's cloned home.
    if duduclaw_gateway::cost_telemetry::get_telemetry().is_none() {
        duduclaw_gateway::cost_telemetry::init_telemetry(home).map_err(err)?;
    }
    let mut registry = duduclaw_llm::ModelRegistry::vendored();
    registry
        .load_override(&home.join("models.toml"))
        .map_err(err)?;
    let arms = [
        spec.frozen(0, 0),
        spec.frozen(1, 0),
        spec.frozen(0, 1),
        spec.frozen(1, 1),
    ]
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .map_err(err)?;
    let models = [
        &spec.planner[0],
        &spec.planner[1],
        &spec.executor[0],
        &spec.executor[1],
        &spec.verifier,
    ];
    if opts.budget_usd.is_some() {
        for m in models {
            if matrix::run_cost_usd(&registry, &m.model, None).1 == CostSource::CoarseUnknownModel {
                return Err(err(format!(
                    "--budget-usd cannot price {}: add it to <DUDUCLAW_HOME>/models.toml",
                    m.as_ref_string()
                )));
            }
        }
    }
    let mut cases = Vec::new();
    let mut seen_domains = BTreeSet::new();
    for root in &spec.domains {
        let domain = root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("evals")
            .to_string();
        if !seen_domains.insert(domain.clone()) {
            return Err(err(format!("duplicate domain {domain}")));
        }
        let mut paths = case::discover_cases(root).map_err(err)?;
        paths.retain(|p| !path_excluded(root, p, &opts.exclude_dir));
        if !opts.case.is_empty() {
            paths.retain(|p| {
                let short = case_id(p);
                let full = case_key(root, p);
                opts.case.iter().any(|id| id == &short || id == &full)
            });
        }
        if let Some(limit) = opts.max_cases {
            paths.truncate(limit);
        }
        for path in paths {
            let case = case::load_case(&path).map_err(err)?;
            if opts
                .filter
                .as_ref()
                .is_some_and(|f| !case.case.name.contains(f))
            {
                continue;
            }
            if case
                .case
                .team_acceptance
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
            {
                return Err(err(format!(
                    "{} needs nonempty [case] team_acceptance for a full-team probe",
                    path.display()
                )));
            }
            let agent = opts.agent.as_deref().unwrap_or(&case.case.agent);
            if !duduclaw_core::is_valid_agent_id(agent) || !home.join("agents").join(agent).is_dir()
            {
                return Err(err(format!(
                    "{}: provisioned agent {agent:?} is missing or invalid",
                    path.display()
                )));
            }
            cases.push((
                domain.clone(),
                case_key(root, &path),
                cluster_key_for(root, &path),
                agent.to_string(),
                case,
            ));
        }
    }
    if cases.is_empty() {
        return Err(err("--team-2x2 found no cases".into()));
    }
    // A standalone eval process has no gateway boot to provision its internal
    // MCP key. Without this, every role member's MCP sidecar fails M6 auth at
    // initialize and the planner can never call team_handoff. Provision only
    // in the caller-selected eval home, then copy that registry into each arm.
    let mcp_key = duduclaw_gateway::mcp_internal_key::ensure_internal_mcp_key(home).map_err(err)?;
    duduclaw_core::set_internal_mcp_api_key(mcp_key);
    let mut records = Vec::new();
    let mut matched: BTreeMap<(String, String, u32), ([Option<f64>; 4], String)> = BTreeMap::new();
    let mut spent = 0.0;
    let mut budget_stop = None;
    let mut observed_max = 0.0_f64;
    // Runs whose cost is a coarse list-price proxy rather than measured usage
    // — reported on the console, not just in the JSON (2026-09-28 review).
    let mut coarse_runs: u64 = 0;
    'all: for (domain, id, cluster, agent, case) in &cases {
        for repeat in 1..=opts.repeats {
            for (arm, p, e) in [(0, 0, 0), (1, 1, 0), (2, 0, 1), (3, 1, 1)] {
                let coarse = matrix::run_cost_usd(&registry, &spec.planner[p].model, None).0
                    // The verifier may request one same-family executor
                    // repair, so reserve that spawn before dispatch too.
                    + f64::from(spec.fanout.saturating_add(1))
                        * matrix::run_cost_usd(&registry, &spec.executor[e].model, None).0
                    + matrix::run_cost_usd(&registry, &spec.verifier.model, None).0;
                let reserve = coarse.max(2.0 * observed_max);
                if !matrix::budget_allows(spent, reserve, opts.budget_usd) {
                    budget_stop = Some(
                        serde_json::json!({"spent_usd": spent, "reserve_usd": reserve, "cap_usd": opts.budget_usd, "at_case": id, "at_arm": arm}),
                    );
                    break 'all;
                }
                let temp = tempfile::tempdir().map_err(|e| err(e.to_string()))?;
                let run_home = temp.path().join("home");
                let mut copy_budget = CopyBudget::default();
                copy_tree(home, &run_home, &mut copy_budget).map_err(err)?;
                let task_id = format!("eval-team-{}", uuid::Uuid::new_v4());
                let mut task = TaskRow::new(
                    task_id.clone(),
                    case.case.name.clone(),
                    case.case.prompt.clone(),
                    "medium".into(),
                    agent.clone(),
                    "eval-team-2x2".into(),
                );
                task.goal_mode = true;
                task.acceptance_criteria = case.case.team_acceptance.clone();
                task.acceptance_criteria_baseline = task.acceptance_criteria.clone();
                let task_store =
                    duduclaw_gateway::task_store::TaskStore::open(&run_home).map_err(err)?;
                task_store.insert_task(&task).await.map_err(err)?;
                let round = run_team_round(TeamRoundContext {
                    home_dir: &run_home,
                    task: &task,
                    round: 1,
                    spec: &arms[arm],
                    state_text: "",
                    spawns_used: 0,
                    grey_band: false,
                });
                let result = if spec.grok_sandbox_off {
                    duduclaw_gateway::runtime::grok::with_eval_sandbox_off(round).await
                } else {
                    round.await
                };
                let (score, outcome) = score_round(result);
                let cost = measured_cost(home, &task_id, &registry);
                let stages = duduclaw_gateway::role_turns::read_rows_for_task(&run_home, &task_id)
                    .into_iter()
                    .map(|row| ProbeStage {
                        member_id: row.member_id,
                        role: row.role.as_str().into(),
                        outcome: row.outcome.as_str().into(),
                        packet_path: row.packet_path,
                        observation_fidelity: row.observation_fidelity,
                        requested_model: row.request_model,
                        response_model: row.response_model,
                        error_type: row.error_type,
                        failure_edge: row.failure_edge.map(|edge| edge.as_str().into()),
                        fault_side: row.fault_side.map(|side| side.as_str().into()),
                    })
                    .collect::<Vec<_>>();
                let member_ids: BTreeSet<String> =
                    stages.iter().map(|s| s.member_id.clone()).collect();
                let (team_handoff_successes, team_handoff_failures) =
                    team_handoff_counts(&run_home, &member_ids);
                if cost.is_none() {
                    coarse_runs = coarse_runs.saturating_add(1);
                    if coarse_runs == 1 {
                        // Once, at the moment the assumption changes — the
                        // per-run tail is summarised after the loop so a long
                        // probe does not print one line per arm.
                        eprintln!(
                            "  WARNING: no measured usage for this run — cost falls back to a \
                             coarse API-list-price proxy (cost_source=\"coarse\"). Common cause: \
                             the cost-telemetry singleton is already bound elsewhere in this \
                             process, so nothing was written to {}.",
                            home.join("cost_telemetry.db").display()
                        );
                    }
                }
                let charged = cost.unwrap_or(coarse);
                observed_max = observed_max.max(charged);
                spent += charged;
                let key = (domain.clone(), id.clone(), repeat);
                let entry = matched.entry(key).or_insert(([None; 4], cluster.clone()));
                entry.0[arm] = score;
                records.push(ProbeRun {
                    domain: domain.clone(),
                    case_id: id.clone(),
                    cluster: cluster.clone(),
                    repeat,
                    arm: ["ww", "sw", "ws", "ss"][arm].into(),
                    planner: spec.planner[p].as_ref_string(),
                    executor: spec.executor[e].as_ref_string(),
                    verifier: spec.verifier.as_ref_string(),
                    score,
                    outcome,
                    estimated_cost_usd: charged,
                    cost_source: if cost.is_some() {
                        "reported_usage"
                    } else {
                        "coarse"
                    },
                    stages,
                    team_handoff_successes,
                    team_handoff_failures,
                });
            }
        }
    }
    let mut matrix = RoleModelMatrix::new(MatrixHeader {
        declared_mde: opts.mde,
        alpha: ALPHA,
        power: POWER,
        repeats: opts.repeats,
        cluster_by: "dir".into(),
        planner: PlannerState::Deferred,
        generated_at: chrono::Utc::now().to_rfc3339(),
        paired_seeds: false,
    });
    // Review finding 11: `matched` is keyed `(domain, case_id, repeat)`, so
    // `--repeats 3` over 3 cases produced NINE rows and every downstream
    // consumer counted runs as cases — `MatrixCell.n` (whose type contract
    // says "Distinct cases … NOT runs: K repeats of one case aggregate into
    // that case's own pass rate first") and `joint_shapley_2x2`'s n alike,
    // narrowing every interval by ≈√K. That is precisely the manufactured
    // resolution Miller's K-repeat identity exists to prevent. `matrix.rs` has
    // always folded per case; this producer did not.
    let folded = fold_repeats_per_case(&matched);
    let mut domain_results = Vec::new();
    for domain in seen_domains {
        let rows: Vec<([f64; 4], &str)> = folded
            .iter()
            .filter(|((d, _), _)| d == &domain)
            .filter_map(|(_, (v, c))| Some(([v[0]?, v[1]?, v[2]?, v[3]?], c.as_str())))
            .collect();
        let shapley = stats::joint_shapley_2x2(&rows);
        let planner_weak: Vec<(String, f64)> =
            rows.iter().map(|(v, c)| ((*c).into(), v[2])).collect();
        let planner_strong: Vec<(String, f64)> =
            rows.iter().map(|(v, c)| ((*c).into(), v[3])).collect();
        let executor_weak: Vec<(String, f64)> =
            rows.iter().map(|(v, c)| ((*c).into(), v[1])).collect();
        let executor_strong: Vec<(String, f64)> =
            rows.iter().map(|(v, c)| ((*c).into(), v[3])).collect();
        // The conditioning token per arm, read straight off the arm indices
        // above: `planner_weak`/`planner_strong` are arms `ws`/`ss` (executor
        // strong in both), `executor_weak`/`executor_strong` are arms
        // `sw`/`ss` (planner strong in both). Without it a reader comparing a
        // planner cell to an executor cell in one file is comparing two
        // different experiments (2026-09-28 review).
        for (role, model, values, conditioned_on) in [
            (
                MatrixRole::Planner,
                &spec.planner[0],
                &planner_weak,
                "executor=strong",
            ),
            (
                MatrixRole::Planner,
                &spec.planner[1],
                &planner_strong,
                "executor=strong",
            ),
            (
                MatrixRole::Executor,
                &spec.executor[0],
                &executor_weak,
                "planner=strong",
            ),
            (
                MatrixRole::Executor,
                &spec.executor[1],
                &executor_strong,
                "planner=strong",
            ),
        ] {
            if let Some(cell) =
                cell_from_scores(&domain, role, model, values, opts.mde, conditioned_on)
            {
                matrix.cells.push(cell);
            }
        }
        domain_results.push(serde_json::json!({
            "domain": domain,
            // Cases, after folding K repeats into one score per case per arm —
            // the unit `MatrixCell.n` and the Shapley n are contracted to.
            "matched_cases": rows.len(),
            "shapley": shapley,
            "incomplete_cases": folded
                .iter()
                .filter(|((d, _), (v, _))| d == &domain && v.iter().any(Option::is_none))
                .count(),
            // The raw run count, kept so the fold is visible rather than
            // implied (`matched_cases × repeats` is only equal when every
            // repeat completed).
            "runs_folded": matched.iter().filter(|((d, _, _), _)| d == &domain).count(),
        }));
    }
    if matrix.cells.iter().any(|c| c.role == MatrixRole::Planner) {
        matrix.header.planner = PlannerState::Measured;
    }
    let report = serde_json::json!({"kind": "full_team_2x2", "generated_at": chrono::Utc::now().to_rfc3339(), "score_definition": "production composer independent verifier PASS=1, FAIL=0; no verifier verdict is unscored", "paired_seeds_applied": false, "team_effort": spec.effort.as_deref(), "team_fanout": spec.fanout, "grok_sandbox_off": spec.grok_sandbox_off, "arms": {"planner_weak": spec.planner[0].as_ref_string(), "planner_strong": spec.planner[1].as_ref_string(), "executor_weak": spec.executor[0].as_ref_string(), "executor_strong": spec.executor[1].as_ref_string(), "verifier": spec.verifier.as_ref_string()}, "domains": domain_results, "runs": records, "budget_stop": budget_stop, "estimated_cost_usd": f64_or_null(spent), "note": "each arm's tasks and tools ran on a temporary clone; measured cost telemetry is stored in the caller's eval home; fallback costs are coarse API-list-price proxies; the cap is pre-run estimated, not a provider invoice ceiling"});
    if let Some(parent) = spec.report.parent() {
        std::fs::create_dir_all(parent).map_err(|e| err(e.to_string()))?;
    }
    std::fs::write(
        &spec.report,
        serde_json::to_vec_pretty(&report).map_err(|e| err(e.to_string()))?,
    )
    .map_err(|e| err(e.to_string()))?;
    let matrix_file = matrix_path(spec.report.parent().unwrap_or(Path::new(".")));
    matrix.save(&matrix_file).map_err(err)?;
    println!(
        "P2b full-team 2×2: {} runs ({} cases after folding {} repeat(s)), {:.4} estimated USD; \
         report {}; matrix {}",
        records.len(),
        folded.len(),
        opts.repeats.max(1),
        spent,
        spec.report.display(),
        matrix_file.display()
    );
    // 2026-09-28 review: the same silence applied to the cost figure itself.
    // `estimated_cost_usd` above is a sum of whatever each run was charged,
    // and a run with no telemetry contributes a list-price guess — which the
    // console never said.
    if coarse_runs > 0 {
        eprintln!(
            "  WARNING: {coarse_runs} of {} run(s) were costed by COARSE list-price estimate, not \
             measured usage — {:.4} USD above is therefore an estimate, not a measurement \
             (per-run `cost_source` in the report says which).",
            records.len(),
            spent
        );
    }
    // Review finding (P2): a `--budget-usd` cut-off was written to the report
    // and nowhere else — the console looked exactly like a completed run and
    // the process exited 0, so an operator read a partial matrix as a full one.
    // `matrix.rs:1774` already warns; this producer did not.
    if let Some(stop) = &budget_stop {
        let planned = u64::from(opts.repeats.max(1)) * cases.len() as u64 * 4;
        eprintln!(
            "  WARNING: --budget-usd cap reached — stopped at case {} arm {} after {} of {} \
             planned run(s); the matrix below is PARTIAL (spent {:.4} of {:.4} USD)",
            stop["at_case"].as_str().unwrap_or("?"),
            stop["at_arm"],
            records.len(),
            planned,
            spent,
            opts.budget_usd.unwrap_or(f64::NAN),
        );
    }
    if matrix.cells.is_empty() {
        return Err(err(
            "no complete 2×2 case/repeat produced a usable team score".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_stem_in_different_clusters_has_distinct_probe_case_ids() {
        let root = Path::new("/suite");
        assert_eq!(
            case_key(root, Path::new("/suite/checkins.toml")),
            "checkins"
        );
        assert_eq!(
            case_key(root, Path::new("/suite/north/checkins.toml")),
            "north/checkins"
        );
        assert_eq!(
            case_key(root, Path::new("/suite/south/checkins.toml")),
            "south/checkins"
        );
        let mut matched = BTreeSet::new();
        for folder in ["north", "south", "west"] {
            matched.insert((
                "care",
                case_key(root, &root.join(folder).join("checkins.toml")),
            ));
        }
        assert_eq!(matched.len(), 3);
    }

    /// Regression (2026-09-28 review, `review_team.md` §3 "評測"): the eval-home
    /// clone had a byte cap and a symlink refusal but nothing bounded its
    /// SHAPE — an unbounded recursion depth and an unbounded file count, both
    /// multiplied by one clone per arm per repeat.
    #[test]
    fn copy_tree_refuses_an_over_deep_or_over_populated_eval_home() {
        let limits = CopyLimits {
            max_depth: 2,
            max_files: 3,
            max_bytes: 1024,
        };

        // ① Depth. A chain one level past the cap is refused by name.
        let deep = tempfile::tempdir().unwrap();
        let mut p = deep.path().to_path_buf();
        for i in 0..4 {
            p = p.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("f.txt"), b"x").unwrap();
        let out = tempfile::tempdir().unwrap();
        let err = copy_tree_limited(
            deep.path(),
            &out.path().join("clone"),
            &mut CopyBudget::default(),
            0,
            &limits,
        )
        .expect_err("an over-deep home is refused");
        assert!(err.contains("nested deeper than 2"), "{err}");

        // ② File count. Small files pass the byte cap and still have to stop.
        let many = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(many.path().join(format!("f{i}.txt")), b"x").unwrap();
        }
        let err = copy_tree_limited(
            many.path(),
            &out.path().join("clone2"),
            &mut CopyBudget::default(),
            0,
            &limits,
        )
        .expect_err("an over-populated home is refused");
        assert!(err.contains("at most 3 files"), "{err}");

        // ③ A home inside every cap still copies, byte for byte.
        let ok = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ok.path().join("a/b")).unwrap();
        std::fs::write(ok.path().join("a/b/f.txt"), b"hello").unwrap();
        let dst = out.path().join("clone3");
        let mut budget = CopyBudget::default();
        copy_tree_limited(ok.path(), &dst, &mut budget, 0, &limits).expect("copies");
        assert_eq!(
            std::fs::read(dst.join("a/b/f.txt")).unwrap(),
            b"hello".to_vec()
        );
        assert_eq!(budget.files, 1);
        assert_eq!(budget.bytes, 5);
    }

    /// The shipped caps are the ones the operator-facing message names.
    #[test]
    fn default_copy_limits_are_the_documented_ones() {
        assert_eq!(CopyLimits::DEFAULT.max_depth, 32);
        assert_eq!(CopyLimits::DEFAULT.max_files, 20_000);
        assert_eq!(CopyLimits::DEFAULT.max_bytes, 256 * 1024 * 1024);
    }

    #[test]
    fn one_matched_row_never_claims_zero_width_certainty() {
        let model = ModelRef::parse("grok:grok-4.7").unwrap();
        let cell = cell_from_scores(
            "homecare",
            MatrixRole::Planner,
            &model,
            &[("cluster-a".into(), 0.0)],
            0.10,
            "executor=strong",
        )
        .unwrap();
        assert_eq!(cell.verdict, MatrixVerdict::Unresolved);
        assert_eq!(cell.conditioned_on.as_deref(), Some("executor=strong"));
        assert_eq!(cell.ci95_low, None);
        assert_eq!(cell.ci95_high, None);
        assert_eq!(cell.mde, None);
    }

    #[test]
    fn only_an_actual_verifier_verdict_scores_a_full_team_arm() {
        use duduclaw_gateway::pause_reason::PauseReason;
        assert_eq!(
            score_round(TeamRoundOutcome::Submitted {
                summary: String::new(),
                verifier_passed: Some(true)
            })
            .0,
            Some(1.0)
        );
        assert_eq!(
            score_round(TeamRoundOutcome::Submitted {
                summary: String::new(),
                verifier_passed: Some(false)
            })
            .0,
            Some(0.0)
        );
        assert_eq!(
            score_round(TeamRoundOutcome::Submitted {
                summary: String::new(),
                verifier_passed: None
            })
            .0,
            None
        );
        // A planner that never called team_handoff never reached an executor
        // or verifier. Counting its pause as FAIL would create false matched
        // rows and fake executor capability cells.
        assert_eq!(
            score_round(TeamRoundOutcome::NeedsHuman {
                reason: "planner_no_packets".into(),
                pause: PauseReason::BlockedNeedsDecision,
            })
            .0,
            None
        );
    }

    #[test]
    fn handoff_diagnostics_count_only_this_arms_members() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            concat!(
                "{\"agent_id\":\"eph-a\",\"tool_name\":\"team_handoff\",\"success\":true}\n",
                "{\"agent_id\":\"eph-a\",\"tool_name\":\"mcp__duduclaw__team_handoff\",\"success\":false}\n",
                "{\"agent_id\":\"eph-b\",\"tool_name\":\"team_handoff\",\"success\":true}\n"
            ),
        )
        .unwrap();
        let members = BTreeSet::from(["eph-a".into()]);
        assert_eq!(team_handoff_counts(dir.path(), &members), (1, 1));
    }

    /// Review finding 11 regression: `--repeats K` must produce ONE score per
    /// case per arm, so `MatrixCell.n` counts cases and not runs. Counting runs
    /// narrows every interval by ≈√K — manufactured resolution.
    #[test]
    fn repeats_fold_into_one_score_per_case_so_cell_n_counts_cases() {
        let mut matched: BTreeMap<(String, String, u32), ArmScores> = BTreeMap::new();
        // Two cases × three repeats = six runs. Case 1 is mixed on the ss arm.
        for repeat in 1..=3u32 {
            matched.insert(
                ("d".into(), "c1".into(), repeat),
                (
                    [
                        Some(0.0),
                        Some(1.0),
                        Some(0.0),
                        Some(if repeat == 1 { 1.0 } else { 0.0 }),
                    ],
                    "d".into(),
                ),
            );
            matched.insert(
                ("d".into(), "c2".into(), repeat),
                ([Some(1.0), Some(1.0), Some(1.0), Some(1.0)], "d".into()),
            );
        }
        assert_eq!(matched.len(), 6, "six runs went in");

        let folded = fold_repeats_per_case(&matched);
        assert_eq!(folded.len(), 2, "two cases must come out, not six runs");
        let c1 = &folded[&("d".to_string(), "c1".to_string())].0;
        assert_eq!(c1[0], Some(0.0));
        assert_eq!(c1[1], Some(1.0));
        // 1.0 on one repeat, 0.0 on two ⇒ that case's own rate is 1/3.
        assert!((c1[3].unwrap() - 1.0 / 3.0).abs() < 1e-12, "{c1:?}");

        // The cell built from the folded rows reports n = cases.
        let values: Vec<(String, f64)> = folded
            .iter()
            .map(|((_, _), (v, cluster))| (cluster.clone(), v[3].unwrap()))
            .collect();
        let cell = cell_from_scores(
            "d",
            MatrixRole::Executor,
            &ModelRef {
                runtime: duduclaw_core::types::RuntimeType::Claude,
                model: "claude-fable-5-1".into(),
            },
            &values,
            0.10,
            "planner=strong",
        )
        .unwrap();
        assert_eq!(cell.n, 2, "MatrixCell.n is cases, never runs");
        assert_eq!(cell.conditioned_on.as_deref(), Some("planner=strong"));
    }

    /// An arm that never completed in ANY repeat stays incomplete rather than
    /// being averaged out of nothing.
    #[test]
    fn a_case_whose_arm_never_completed_stays_incomplete_after_folding() {
        let mut matched: BTreeMap<(String, String, u32), ArmScores> = BTreeMap::new();
        for repeat in 1..=2u32 {
            matched.insert(
                ("d".into(), "c1".into(), repeat),
                ([Some(1.0), None, Some(1.0), Some(1.0)], "d".into()),
            );
        }
        let folded = fold_repeats_per_case(&matched);
        let arms = &folded[&("d".to_string(), "c1".to_string())].0;
        assert_eq!(arms[1], None, "an all-missing arm must not become 0.0");
        assert!(arms.iter().any(Option::is_none));
    }
}
