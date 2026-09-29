//! `duduclaw eval --matrix` — the role→model capability matrix (Team-as-Agent
//! P2).
//!
//! ## What a cell is
//!
//! One cell = one `(domain, role, runtime, model)`. It is measured by running
//! every case of that domain's eval suite `K` times on that `(runtime, model)`
//! in that role, and scoring the result **deterministically**:
//!
//! * **executor** — run the case's prompt live and check the `[expect]`
//!   assertions. Score = pass rate.
//! * **verifier** — show the model the case's *recorded* transcript plus the
//!   acceptance criteria, ask for `PASS`/`FAIL`, and compare with the assertion
//!   outcome on that same transcript (the gold label). Score = agreement; the
//!   cell also reports false-accept / false-reject rates with Wilson intervals.
//!   See [`super::verifier_cell`].
//! * **planner** — deferred to P2b. A planner call emits sub-task packets rather
//!   than an answer a deterministic assertion can grade, so scoring it needs a
//!   full team-round harness. `--roles planner` is refused, not silently scored:
//!   `role_model_matrix.toml` records `planner = "deferred"` so a reader can tell
//!   *unmeasured* from *bad*.
//!
//! ## The bottleneck heuristic, and its limits
//!
//! For each role, Δ = score(strong model) − score(weak model) on the same cases.
//! The role with the larger Δ is the one whose model choice buys the most, i.e.
//! the bottleneck — but it is only *called* the bottleneck when its confidence
//! interval excludes every other role's. Otherwise the answer is `unresolved`,
//! which is a real answer and not a failure.
//!
//! This is the **decoupled** form of AgentCARD's Shapley probe (arXiv:2606.20629):
//! roles are measured independently, one at a time, with no joint team run. That
//! is what makes it affordable — 2 roles × 2 models instead of |M|^|R| team
//! configurations — and also what it cannot see: a genuine interaction (a strong
//! verifier only paying off behind a weak executor) is invisible to it by
//! construction. Treat the output as "which role to spend on first", never as a
//! team-level attribution.
//!
//! ## Hard rules (in code, not just in the docs)
//!
//! * `--matrix` refuses `--replay`. Comparing models through frozen transcripts
//!   is the Replay Gap (arXiv:2608.08239): a recorded run of model A says
//!   nothing about model B. Verifier cells read recorded transcripts *by design*
//!   and need no flag for it — the model under test is still called live.
//! * `--matrix` never `--record`s. Recording here would overwrite a domain's
//!   baseline transcripts with some other model's run.
//! * A declared `--temperature` below production is refused (Miller 2024 §3.3 —
//!   lowering temperature to suppress variance manufactures resolution).
//! * A cell whose resolution ratio `q < 1` is `unresolved` and must not be read
//!   as a ranking. The declared MDE is printed in the console summary and written
//!   into both the report and the matrix header.
//! * A run the gateway's failover answered with a *different* `(runtime, model)`
//!   is excluded from its cell and counted as `substituted` — never credited to
//!   the model that was asked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use console::style;
use duduclaw_core::role_model_matrix::{
    MatrixCell, MatrixHeader, MatrixRole, MatrixVerdict, PlannerState, RoleModelMatrix, matrix_path,
};
use duduclaw_core::types::RuntimeType;

use super::case::{self, EvalCaseFile};
use super::runner::{self, ReportedUsage, RunMode, RunOverrides};
use super::stats;
use super::verifier_cell::{self, VerifierRow, VerifierTally};
use super::{
    ALPHA, EvalOptions, MIN_RELIABLE_CLUSTERS, POWER, case_id, case_key, cluster_key_for,
    f64_or_null, path_excluded, resolution_row,
};

/// Decision line every cell's interval is tested against: chance. An executor
/// cell's pass rate and a verifier cell's agreement are both "better than a coin
/// flip?" questions.
const PASS_LINE: f64 = 0.5;

/// Production sampling temperature. A declared value below this is refused.
pub const PRODUCTION_TEMPERATURE: f64 = 1.0;

/// Coarse per-run token assumption used ONLY when a runtime reported no usage
/// (the Claude CLI path reports none). Taken from the design's own cost model
/// (`research/multi-model-routing-2026-09/13-F-…` §4.4: ~25k in / 4k out median
/// per role-run). Priced through the same `ModelRegistry` as real usage, so the
/// only difference between a reported and a coarse estimate is where the token
/// counts came from — which the report states per run.
pub const COARSE_INPUT_TOKENS: u64 = 25_000;
pub const COARSE_OUTPUT_TOKENS: u64 = 4_000;
/// Flat per-run fallback for a model the registry has never heard of. Deliberately
/// crude and deliberately labelled: an unknown model has no price, and inventing
/// a precise-looking one would be worse than admitting the estimate is a stub.
pub const COARSE_UNKNOWN_MODEL_USD: f64 = 0.05;

// ─────────────────────────────────────────────────────────────────────────
// `runtime:model` references
// ─────────────────────────────────────────────────────────────────────────

/// A `runtime:model` pair, the spelling `--models` / `--weak` / `--strong` take.
///
/// Only `Eq` — `RuntimeType` has no total order, and a model reference has no
/// meaningful one either (they are compared for identity, never sorted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub runtime: RuntimeType,
    pub model: String,
}

impl ModelRef {
    /// Parse `"<runtime>:<model>"`. The runtime must be a canonical runtime id on
    /// this build; a bare model with no runtime is refused rather than guessed —
    /// guessing is how `claude-sonnet-4-6` ends up measured as a codex model.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let Some((rt, model)) = s.split_once(':') else {
            return Err(format!(
                "model reference {s:?} must be `<runtime>:<model>` (e.g. \
                 `claude:claude-haiku-4-5`, `codex:gpt-5.6-sol`)"
            ));
        };
        let runtime = RuntimeType::from_id(rt.trim()).ok_or_else(|| {
            format!(
                "model reference {s:?}: {:?} is not a runtime on this build",
                rt.trim()
            )
        })?;
        let model = model.trim();
        if model.is_empty() {
            return Err(format!("model reference {s:?} has an empty model id"));
        }
        Ok(ModelRef {
            runtime,
            model: model.to_string(),
        })
    }

    pub fn as_ref_string(&self) -> String {
        format!("{}:{}", self.runtime.as_str(), self.model)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Validated options
// ─────────────────────────────────────────────────────────────────────────

/// A validated `--matrix` invocation.
#[derive(Debug, Clone)]
pub struct MatrixSpec {
    pub roles: Vec<MatrixRole>,
    /// Every model that gets cells: `--models`, plus `--weak`/`--strong` when
    /// they name a model not already listed (the bottleneck probe needs both
    /// arms measured even if the operator only listed the candidates).
    pub models: Vec<ModelRef>,
    pub weak: Option<ModelRef>,
    pub strong: Option<ModelRef>,
    /// Suite roots, one per domain.
    pub domains: Vec<PathBuf>,
    pub repeats: u32,
    pub paired_seeds: bool,
    pub budget_usd: Option<f64>,
    pub max_cases: Option<usize>,
    pub mde: f64,
    pub cluster_by: String,
    pub temperature: Option<f64>,
    /// `--agent`: run every case under this provisioned agent instead of each
    /// case's own `[case] agent`. See [`MatrixSpec::from_options`]'s validation.
    pub agent: Option<String>,
    pub report: Option<PathBuf>,
    pub filter: Option<String>,
    pub case: Vec<String>,
    pub exclude_dir: Vec<String>,
}

impl MatrixSpec {
    /// Validate the CLI flags. Every refusal here is a hard rule from the design
    /// (§3.12) — none of them degrade to a weaker measurement.
    pub fn from_options(opts: &EvalOptions) -> Result<Self, String> {
        if opts.replay {
            return Err(
                "--matrix cannot be combined with --replay: a matrix compares MODELS, and a frozen \
                 transcript recorded from one model says nothing about another (the Replay Gap, \
                 arXiv:2608.08239). Verifier cells do read recorded transcripts — that is their \
                 design, and it needs no flag: the verifier model itself is always called live."
                    .to_string(),
            );
        }
        if let Some(t) = opts.temperature {
            if !t.is_finite() {
                return Err(format!("--temperature must be a finite number, got {t}"));
            }
            if t < PRODUCTION_TEMPERATURE {
                return Err(format!(
                    "--temperature {t} is below the production value {PRODUCTION_TEMPERATURE}: \
                     lowering temperature suppresses run-to-run variance and manufactures \
                     resolution the deployed system does not have (Miller 2024 §3.3). Refusing \
                     rather than measuring something you will never ship."
                ));
            }
        }
        if !(opts.mde > 0.0 && opts.mde < 1.0) {
            return Err(format!(
                "--mde must be a fraction in (0, 1), got {}",
                opts.mde
            ));
        }
        if opts.cluster_by != "dir" {
            return Err(format!(
                "--cluster-by {:?} is not implemented (only \"dir\" is supported today)",
                opts.cluster_by
            ));
        }

        let roles = parse_roles(&opts.roles)?;
        let mut models = parse_models(&opts.models)?;
        let weak = opts.weak.as_deref().map(ModelRef::parse).transpose()?;
        let strong = opts.strong.as_deref().map(ModelRef::parse).transpose()?;
        match (&weak, &strong) {
            (Some(w), Some(s)) if w == s => {
                return Err(
                    "--weak and --strong name the same model: a bottleneck probe needs two \
                     distinct tiers"
                        .to_string(),
                );
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(
                    "--weak and --strong must be given together (the bottleneck probe compares \
                     the two)"
                        .to_string(),
                );
            }
            _ => {}
        }
        // The probe arms must be measured even when they are not `--models`
        // entries — otherwise Δ has nothing to read.
        for arm in [weak.as_ref(), strong.as_ref()].into_iter().flatten() {
            if !models.contains(arm) {
                models.push(arm.clone());
            }
        }
        if models.is_empty() {
            return Err(
                "--matrix needs at least one `--models <runtime:model>` entry (or a \
                 --weak/--strong pair)"
                    .to_string(),
            );
        }

        let domains = if opts.domain.is_empty() {
            vec![opts.path.clone().unwrap_or_else(|| PathBuf::from("evals"))]
        } else {
            opts.domain.clone()
        };
        if let Some(0) = opts.max_cases {
            return Err("--max-cases must be >= 1".to_string());
        }
        if let Some(b) = opts.budget_usd {
            if !(b.is_finite() && b > 0.0) {
                return Err(format!("--budget-usd must be a positive number, got {b}"));
            }
        }
        // `--agent` borrows one provisioned agent to carry a suite authored for
        // another. A matrix measures the MODEL, not the persona, so that is an
        // acceptable probe-run compromise — but it changes the system prompt
        // every case runs under, so the id is validated here and declared in the
        // report header (`agent_override`) rather than quietly applied.
        if let Some(a) = opts
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            if !duduclaw_core::is_valid_agent_id(a) {
                return Err(format!(
                    "--agent {a:?} is not a valid agent id (1-64 chars of [a-zA-Z0-9_-])"
                ));
            }
        }

        Ok(MatrixSpec {
            roles,
            models,
            weak,
            strong,
            domains,
            repeats: opts.repeats.max(1),
            paired_seeds: opts.paired_seeds,
            budget_usd: opts.budget_usd,
            max_cases: opts.max_cases,
            mde: opts.mde,
            cluster_by: opts.cluster_by.clone(),
            temperature: opts.temperature,
            agent: opts
                .agent
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_string),
            report: opts.report.clone(),
            filter: opts.filter.clone(),
            case: opts.case.clone(),
            exclude_dir: opts.exclude_dir.clone(),
        })
    }
}

/// `--roles` → validated, de-duplicated, order preserved.
pub fn parse_roles(raw: &[String]) -> Result<Vec<MatrixRole>, String> {
    let mut out: Vec<MatrixRole> = Vec::new();
    let items: Vec<&str> = raw
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return Err(format!(
            "--matrix needs --roles (one or more of {})",
            MatrixRole::MEASURED_IN_P2
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for item in items {
        let role = MatrixRole::parse(item).ok_or_else(|| {
            format!(
                "--roles {item:?} is not a role (expected one of {})",
                MatrixRole::ALL
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        if role == MatrixRole::Planner {
            return Err(
                "--roles planner is not measurable in P2: a planner call produces sub-task \
                 packets, not an answer a deterministic `[expect]` assertion can grade, so scoring \
                 it needs a full team-round harness (deferred to P2b). `role_model_matrix.toml` \
                 records `planner = \"deferred\"` so its absence reads as unmeasured, not bad."
                    .to_string(),
            );
        }
        if !out.contains(&role) {
            out.push(role);
        }
    }
    Ok(out)
}

/// `--models` → validated, de-duplicated, order preserved.
pub fn parse_models(raw: &[String]) -> Result<Vec<ModelRef>, String> {
    let mut out: Vec<ModelRef> = Vec::new();
    for item in raw.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let m = ModelRef::parse(item)?;
        if !out.contains(&m) {
            out.push(m);
        }
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────
// Cell / run expansion (pure — no I/O, fully testable)
// ─────────────────────────────────────────────────────────────────────────

/// One cell to measure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellPlan {
    pub domain: String,
    pub role: MatrixRole,
    pub model: ModelRef,
}

/// One run inside a cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    /// Index into the cell list.
    pub cell: usize,
    pub case_id: String,
    /// 1-based, `None` when `repeats == 1` (mirrors `--repeats`' own convention).
    pub repeat_index: Option<u32>,
    pub seed: Option<u64>,
}

/// Cells in report order: domain → role → model, each in the order the operator
/// gave them. Deterministic, so two runs of the same command produce comparable
/// reports.
pub fn expand_cells(
    domains: &[String],
    roles: &[MatrixRole],
    models: &[ModelRef],
) -> Vec<CellPlan> {
    let mut out = Vec::with_capacity(domains.len() * roles.len() * models.len());
    for domain in domains {
        for role in roles {
            for model in models {
                out.push(CellPlan {
                    domain: domain.clone(),
                    role: *role,
                    model: model.clone(),
                });
            }
        }
    }
    out
}

/// Runs in execution order — **strictly serial**: cell by cell, case by case,
/// repeat by repeat. One CLI spawn at a time is the point: these runs contend
/// for the operator's own account quota, and a parallel matrix would both
/// rate-limit itself and correlate its own samples.
pub fn expand_runs(
    cells: &[CellPlan],
    cases_per_domain: &BTreeMap<String, Vec<String>>,
    repeats: u32,
    paired_seeds: bool,
) -> Vec<RunPlan> {
    let repeats = repeats.max(1);
    let mut out = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        let Some(cases) = cases_per_domain.get(&cell.domain) else {
            continue;
        };
        for case_id in cases {
            for r in 1..=repeats {
                let repeat_index = if repeats > 1 { Some(r) } else { None };
                out.push(RunPlan {
                    cell: i,
                    case_id: case_id.clone(),
                    repeat_index,
                    seed: paired_seeds.then(|| derive_seed(case_id, r)),
                });
            }
        }
    }
    out
}

/// Deterministic per-`(case_id, repeat)` seed (FNV-1a 64).
///
/// Deliberately **not** `DefaultHasher` (unspecified and version-dependent) and
/// deliberately independent of the model, so the same `(case, repeat)` gets the
/// same seed in every cell — that is what "paired" means. See
/// [`runner::SEED_APPLIED_ANYWHERE`] for the honest note that no runtime in this
/// build can actually consume it yet.
pub fn derive_seed(case_id: &str, repeat_index: u32) -> u64 {
    // FNV-1a 64 constants: offset basis and the 2^40 + 2^8 + 0xb3 prime.
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x100_0000_01b3;
    let mut h = OFFSET;
    for b in case_id
        .as_bytes()
        .iter()
        .chain(b"#")
        .chain(repeat_index.to_be_bytes().iter())
    {
        h ^= *b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

// ─────────────────────────────────────────────────────────────────────────
// Cost estimation + budget stop
// ─────────────────────────────────────────────────────────────────────────

/// Where a run's cost number came from. Never averaged into a single unlabelled
/// figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    /// Priced from the usage the runtime actually reported.
    Reported,
    /// Priced from [`COARSE_INPUT_TOKENS`]/[`COARSE_OUTPUT_TOKENS`] because the
    /// runtime reported no usage.
    Coarse,
    /// The model is not in the registry — [`COARSE_UNKNOWN_MODEL_USD`] flat.
    CoarseUnknownModel,
}

/// Estimated USD cost of one run.
pub fn run_cost_usd(
    registry: &duduclaw_llm::ModelRegistry,
    model: &str,
    usage: Option<ReportedUsage>,
) -> (f64, CostSource) {
    let Some(info) = registry.get(model) else {
        return (COARSE_UNKNOWN_MODEL_USD, CostSource::CoarseUnknownModel);
    };
    let (normalized, source) = match usage {
        Some(u) => (
            duduclaw_llm::NormalizedUsage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_read_tokens: u.cache_read_tokens,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            CostSource::Reported,
        ),
        None => (
            duduclaw_llm::NormalizedUsage {
                input_tokens: COARSE_INPUT_TOKENS,
                output_tokens: COARSE_OUTPUT_TOKENS,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            CostSource::Coarse,
        ),
    };
    // Millicents → USD: $1 = 100_000 mc.
    let usd = registry.cost_millicents(&normalized, info) as f64 / 100_000.0;
    (usd, source)
}

/// Would one more run at `next_estimate` stay inside the cap? `None` cap ⇒ yes.
///
/// Checked BEFORE dispatching, so the cap is a ceiling on what gets spent and
/// not merely a report of having exceeded it.
pub fn budget_allows(spent_usd: f64, next_estimate_usd: f64, cap_usd: Option<f64>) -> bool {
    match cap_usd {
        None => true,
        Some(cap) => spent_usd + next_estimate_usd <= cap,
    }
}

fn next_run_reserve(coarse_usd: f64, largest_reported_usd: Option<f64>) -> f64 {
    coarse_usd.max(largest_reported_usd.unwrap_or(0.0) * 2.0)
}

/// A budget cannot be enforced against an invented flat price. Check every
/// candidate before the first live dispatch, including probe-only arms.
fn validate_budget_prices(
    registry: &duduclaw_llm::ModelRegistry,
    spec: &MatrixSpec,
    home: &Path,
) -> Result<(), String> {
    if spec.budget_usd.is_none() {
        return Ok(());
    }
    for model in &spec.models {
        if registry.get(&model.model).is_none() {
            return Err(format!(
                "--budget-usd requires a price for {} in {} or the vendored registry; refusing to dispatch with an unknown-model cost stub",
                model.as_ref_string(),
                home.join("models.toml").display()
            ));
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Per-role Δ and the bottleneck call
// ─────────────────────────────────────────────────────────────────────────

/// One case's contribution to a cell: `(case_id, cluster_key, score)`.
pub type Scored = (String, String, f64);

// ─────────────────────────────────────────────────────────────────────────
// Choosing the standard error (smoke-3 bug 1)
// ─────────────────────────────────────────────────────────────────────────

/// Where a reported standard error came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeSource {
    /// Miller App. C cluster-robust SE — needs ≥2 clusters.
    Clustered,
    /// Unclustered CLT SE, because the sample has **one** cluster and the
    /// cluster-robust estimator is undefined there.
    CltSingleCluster,
    /// Nothing to estimate from (`n == 0`).
    Undefined,
}

/// The SE a cell (or a Δ) should actually report, plus where it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChosenSe {
    pub se: f64,
    pub source: SeSource,
    pub n_clusters: usize,
}

impl ChosenSe {
    /// A zero-width interval: `se == 0`. With `n > 1` this can only mean every
    /// observation was identical; with `n == 1` there is no spread to estimate.
    /// Either way the interval carries no information and must not be read as
    /// certainty — see [`VERDICT_REASON_DEGENERATE_INTERVAL`].
    pub fn is_degenerate(&self) -> bool {
        !self.se.is_finite() || self.se == 0.0
    }
}

/// Pick the SE for `values`, falling back off the cluster-robust estimator when
/// it is undefined.
///
/// **Why (smoke 3, 2026-09-25).** Every cell of a single-directory suite has
/// `n_clusters == 1`, and [`stats::se_clustered`] is *identically zero* there —
/// with one cluster the residual sum `S_c` is zero by construction, so the
/// between-cluster term exactly cancels `se_clt²`. That is the correct value of
/// a **useless** estimator, and it was being reported as certainty: cells came
/// back `mean 0.25 ci=[0.25,0.25]`, the Δ intervals were zero-width, and the
/// bottleneck was declared *resolved* on four cases — the exact false claim this
/// whole layer exists to prevent.
///
/// So: fewer than two clusters ⇒ report the unclustered CLT SE and say so
/// (`SeSource::CltSingleCluster`). It is the honest weaker estimate — it cannot
/// see within-directory correlation, which is why the small-cluster warning
/// still fires — rather than a fabricated zero.
pub fn choose_se(values: &[f64], clusters: &[&str]) -> ChosenSe {
    let n_clusters = clusters.iter().collect::<BTreeSet<_>>().len();
    if values.is_empty() {
        return ChosenSe {
            se: f64::NAN,
            source: SeSource::Undefined,
            n_clusters,
        };
    }
    if n_clusters < 2 {
        return ChosenSe {
            se: stats::se_clt(values),
            source: SeSource::CltSingleCluster,
            n_clusters,
        };
    }
    ChosenSe {
        se: stats::se_clustered(values, clusters),
        source: SeSource::Clustered,
        n_clusters,
    }
}

/// Δ = score(strong) − score(weak) for one role.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RoleDelta {
    pub role: String,
    pub weak_mean: Option<f64>,
    pub strong_mean: Option<f64>,
    pub delta: Option<f64>,
    pub se: Option<f64>,
    /// Which estimator produced `se` (a single-cluster suite falls back off the
    /// cluster-robust one — see [`choose_se`]).
    pub se_source: SeSource,
    pub ci95_low: Option<f64>,
    pub ci95_high: Option<f64>,
    /// `true` when the two arms shared case ids and Δ is a per-case paired
    /// difference (Miller's paired design — removes question-difficulty variance
    /// for free). `false` = the unpaired fallback below.
    pub paired: bool,
    /// Cases contributing to Δ.
    pub n: usize,
    /// Δ is not interpretable — a degenerate gold behind it, or a zero-width
    /// interval. [`bottleneck`] refuses to resolve on such a Δ.
    #[serde(default)]
    pub degenerate: bool,
    /// Machine-readable cause when `degenerate`. Precedence: a degenerate gold
    /// wins (it invalidates the metric itself), else the interval.
    #[serde(default)]
    pub degenerate_reason: Option<String>,
    /// Why Δ or its interval is absent / weakened, when it is.
    pub note: Option<String>,
}

/// Compute one role's Δ from the two arms' per-case scores.
///
/// Paired whenever the arms share case ids: `d_i = strong_i − weak_i`, and the
/// SE is the cluster-robust SE of those differences (`--cluster-by dir`). With no
/// shared ids it degrades to the independent-samples SE `sqrt(se_w² + se_s²)` and
/// says so in `note` — never silently, because an unpaired Δ on a clustered suite
/// is materially weaker evidence.
pub fn role_delta(role: MatrixRole, weak: &[Scored], strong: &[Scored]) -> RoleDelta {
    let mean_of = |v: &[Scored]| -> Option<f64> {
        if v.is_empty() {
            return None;
        }
        let m = stats::mean(&v.iter().map(|(_, _, s)| *s).collect::<Vec<_>>());
        m.is_finite().then_some(m)
    };
    let weak_mean = mean_of(weak);
    let strong_mean = mean_of(strong);
    let mut out = RoleDelta {
        role: role.as_str().to_string(),
        weak_mean,
        strong_mean,
        delta: None,
        se: None,
        ci95_low: None,
        ci95_high: None,
        se_source: SeSource::Undefined,
        paired: false,
        n: 0,
        degenerate: false,
        degenerate_reason: None,
        note: None,
    };
    let (Some(w), Some(s)) = (weak_mean, strong_mean) else {
        out.note = Some("one or both arms produced no usable observation".to_string());
        return out;
    };

    let weak_by_id: BTreeMap<&str, (&str, f64)> = weak
        .iter()
        .map(|(id, cl, sc)| (id.as_str(), (cl.as_str(), *sc)))
        .collect();
    let mut diffs: Vec<f64> = Vec::new();
    let mut clusters: Vec<&str> = Vec::new();
    for (id, cluster, strong_score) in strong {
        if let Some((_, weak_score)) = weak_by_id.get(id.as_str()) {
            diffs.push(strong_score - weak_score);
            clusters.push(cluster.as_str());
        }
    }

    let z = stats::z_two_sided(ALPHA);
    if !diffs.is_empty() {
        let delta = stats::mean(&diffs);
        // Same single-cluster fallback as a cell: the paired-difference SE over a
        // one-directory suite must come from the CLT estimator, not from a
        // cluster-robust one that is identically zero there.
        let chosen = choose_se(&diffs, &clusters);
        out.paired = true;
        out.n = diffs.len();
        out.se_source = chosen.source;
        out.delta = delta.is_finite().then_some(delta);
        out.se = chosen.se.is_finite().then_some(chosen.se);
        if let (Some(d), Some(se)) = (out.delta, out.se) {
            out.ci95_low = Some(d - z * se);
            out.ci95_high = Some(d + z * se);
        } else {
            out.note = Some("standard error not computable (n < 1)".to_string());
        }
    } else {
        let weak_values: Vec<f64> = weak.iter().map(|(_, _, s)| *s).collect();
        let weak_clusters: Vec<&str> = weak.iter().map(|(_, c, _)| c.as_str()).collect();
        let strong_values: Vec<f64> = strong.iter().map(|(_, _, s)| *s).collect();
        let strong_clusters: Vec<&str> = strong.iter().map(|(_, c, _)| c.as_str()).collect();
        let cw = choose_se(&weak_values, &weak_clusters);
        let cs = choose_se(&strong_values, &strong_clusters);
        let delta = s - w;
        out.n = weak.len().min(strong.len());
        out.delta = Some(delta);
        out.se_source = if cw.source == SeSource::Clustered && cs.source == SeSource::Clustered {
            SeSource::Clustered
        } else {
            SeSource::CltSingleCluster
        };
        out.note = Some(
            "the two arms share no case ids — Δ is an UNPAIRED difference of means, materially \
             weaker on a clustered suite than the paired design this command normally uses"
                .to_string(),
        );
        if cw.se.is_finite() && cs.se.is_finite() {
            let se = (cw.se * cw.se + cs.se * cs.se).sqrt();
            out.se = Some(se);
            out.ci95_low = Some(delta - z * se);
            out.ci95_high = Some(delta + z * se);
        }
    }

    // A zero-width Δ interval is not a precise measurement. It means either one
    // paired observation, or every case moved by exactly the same amount — in
    // both cases the spread is unestimated, and declaring a bottleneck on it
    // (which smoke 3 did, on four cases) is a fabricated certainty.
    if matches!(out.se, Some(se) if se == 0.0) || (out.delta.is_some() && out.se.is_none()) {
        out.degenerate = true;
        out.degenerate_reason = Some(VERDICT_REASON_DEGENERATE_INTERVAL.to_string());
        let note = format!(
            "zero-width Δ interval (n={}, se={}) — no spread to estimate, so Δ cannot be \
             distinguished from any other value",
            out.n,
            out.se
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".into())
        );
        out.note = Some(match out.note.take() {
            Some(prev) => format!("{prev}; {note}"),
            None => note,
        });
    }
    out
}

/// The bottleneck call.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BottleneckOutcome {
    /// `Some(role)` only when that role's Δ interval excludes every other role's.
    pub bottleneck_role: Option<String>,
    /// `"resolved"` / `"unresolved"`.
    pub resolution: &'static str,
    pub reason: String,
}

/// Decide which role is the bottleneck, or refuse to.
///
/// Resolved iff the largest Δ's `ci95_low` is strictly above every other Δ's
/// `ci95_high`. With fewer than two comparable roles, or any missing interval,
/// the answer is `unresolved` — the interesting case (two roles whose intervals
/// overlap) is exactly the one a point-estimate ranking would get wrong.
pub fn bottleneck(deltas: &[RoleDelta]) -> BottleneckOutcome {
    let comparable: Vec<&RoleDelta> = deltas
        .iter()
        .filter(|d| d.delta.is_some() && d.ci95_low.is_some() && d.ci95_high.is_some())
        // A Δ built on a degenerate-gold cell is arithmetic without meaning; it
        // must not be allowed to win (or lose) a bottleneck comparison.
        .filter(|d| !d.degenerate)
        .collect();
    if comparable.len() < 2 {
        let degenerate: Vec<String> = deltas
            .iter()
            .filter(|d| d.degenerate)
            .map(|d| {
                format!(
                    "{}:{}",
                    d.role,
                    d.degenerate_reason.as_deref().unwrap_or("degenerate")
                )
            })
            .collect();
        return BottleneckOutcome {
            bottleneck_role: None,
            resolution: "unresolved",
            reason: if degenerate.is_empty() {
                format!(
                    "need at least two roles with a computable Δ interval to compare; got {}",
                    comparable.len()
                )
            } else {
                format!(
                    "need at least two roles with an interpretable Δ interval to compare; \
                     got {} ({} excluded). A `degenerate_gold` role needs its suite re-recorded \
                     with a mixed gold; a `degenerate_interval` role needs more cases or more \
                     clusters.",
                    comparable.len(),
                    degenerate.join(", ")
                )
            },
        };
    }
    let mut sorted = comparable.clone();
    sorted.sort_by(|a, b| {
        b.delta
            .unwrap()
            .partial_cmp(&a.delta.unwrap())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top = sorted[0];
    let top_low = top.ci95_low.unwrap();
    let rivals: Vec<&RoleDelta> = sorted[1..].to_vec();
    let overlapping: Vec<&str> = rivals
        .iter()
        .filter(|r| r.ci95_high.unwrap() >= top_low)
        .map(|r| r.role.as_str())
        .collect();
    if overlapping.is_empty() {
        BottleneckOutcome {
            bottleneck_role: Some(top.role.clone()),
            resolution: "resolved",
            reason: format!(
                "Δ({}) = {:.3} [{:.3}, {:.3}] excludes every other role's interval",
                top.role,
                top.delta.unwrap(),
                top_low,
                top.ci95_high.unwrap()
            ),
        }
    } else {
        BottleneckOutcome {
            bottleneck_role: None,
            resolution: "unresolved",
            reason: format!(
                "Δ({}) = {:.3} [{:.3}, {:.3}] overlaps {} — the suite cannot tell these roles \
                 apart at this sample size; collect more cases or repeats rather than reading the \
                 point estimates as a ranking",
                top.role,
                top.delta.unwrap(),
                top_low,
                top.ci95_high.unwrap(),
                overlapping.join(", ")
            ),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Discovery
// ─────────────────────────────────────────────────────────────────────────

/// One domain's loaded cases.
struct DomainCases {
    name: String,
    root: PathBuf,
    /// `(case path, case id, cluster key, loaded case)`, in discovery order.
    cases: Vec<(PathBuf, String, String, EvalCaseFile)>,
}

fn discover_domain(root: &Path, spec: &MatrixSpec) -> Result<DomainCases, String> {
    let name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("evals")
        .to_string();
    let mut paths = case::discover_cases(root)?;
    if !spec.exclude_dir.is_empty() {
        paths.retain(|p| !path_excluded(root, p, &spec.exclude_dir));
    }
    if !spec.case.is_empty() {
        let wanted: BTreeSet<&str> = spec.case.iter().map(String::as_str).collect();
        paths.retain(|p| {
            wanted.contains(case_id(p).as_str()) || wanted.contains(case_key(root, p).as_str())
        });
    }
    let mut cases = Vec::new();
    for path in paths {
        let loaded = case::load_case(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some(f) = &spec.filter {
            if !loaded.case.name.contains(f.as_str()) {
                continue;
            }
        }
        let id = case_key(root, &path);
        let cluster = cluster_key_for(root, &path);
        cases.push((path, id, cluster, loaded));
    }
    if let Some(cap) = spec.max_cases {
        cases.truncate(cap);
    }
    Ok(DomainCases {
        name,
        root: root.to_path_buf(),
        cases,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Execution
// ─────────────────────────────────────────────────────────────────────────

/// One executed run's record.
#[derive(Debug, Clone, serde::Serialize)]
struct RunRecord {
    domain: String,
    role: String,
    runtime: String,
    model: String,
    /// The agent directory this run used — the `--agent` override when one was
    /// given, else the case's own `[case] agent`.
    agent: String,
    case_id: String,
    cluster: String,
    repeat_index: Option<u32>,
    seed: Option<u64>,
    seed_applied: bool,
    /// `None` when the run did not produce a score (error / skip / substitution).
    score: Option<f64>,
    error: Option<String>,
    skip: Option<verifier_cell::VerifierSkip>,
    substituted: Option<(String, String)>,
    /// Verifier runs only: the runtime returned an event stream and the agent's
    /// message had to be recovered from it (see
    /// `runner::normalize_runtime_message_text`). Surfaced so the workaround for
    /// the upstream codex-content defect stays visible instead of silently
    /// masking it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    message_recovered_from_stream: bool,
    cost_usd: f64,
    cost_source: CostSource,
    duration_ms: u128,
    /// Verifier rows only.
    verifier: Option<VerifierRow>,
}

/// A measured cell.
#[derive(Debug, Clone)]
struct CellResult {
    plan: CellPlan,
    /// Per-case scores that entered the statistics.
    scored: Vec<Scored>,
    runs: usize,
    errors: usize,
    skipped: usize,
    substituted: usize,
    verifier_rows: Vec<VerifierRow>,
    cost_usd: f64,
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    home: &Path,
    spec: &MatrixSpec,
    cells: &[CellPlan],
    domains: &BTreeMap<String, DomainCases>,
    runs: &[RunPlan],
    registry: &duduclaw_llm::ModelRegistry,
) -> (Vec<CellResult>, Vec<RunRecord>, Option<serde_json::Value>) {
    let mut cell_results: Vec<CellResult> = cells
        .iter()
        .map(|plan| CellResult {
            plan: plan.clone(),
            scored: Vec::new(),
            runs: 0,
            errors: 0,
            skipped: 0,
            substituted: 0,
            verifier_rows: Vec::new(),
            cost_usd: 0.0,
        })
        .collect();
    let mut records: Vec<RunRecord> = Vec::new();
    let mut spent = 0.0_f64;
    let mut budget_stop: Option<serde_json::Value> = None;
    // A real CLI run can dwarf the 25k/4k coarse assumption (long prompts,
    // nested tool turns). Reserve twice the largest observed cost for later
    // runs of that model. This remains an estimate, not provider billing.
    let mut observed_cost_ceiling: BTreeMap<String, f64> = BTreeMap::new();
    // Per-(cell, case) accumulator so `K` repeats collapse into one case score.
    let mut per_case: BTreeMap<(usize, String), Vec<f64>> = BTreeMap::new();

    for (i, run) in runs.iter().enumerate() {
        let cell = &cells[run.cell];
        let Some(domain) = domains.get(&cell.domain) else {
            continue;
        };
        let Some((path, _, cluster, case_file)) =
            domain.cases.iter().find(|(_, id, _, _)| *id == run.case_id)
        else {
            continue;
        };

        // Budget gate BEFORE spawning. The pre-run estimate is always the coarse
        // one (no usage is known yet), which is the conservative direction.
        let (coarse, _) = run_cost_usd(registry, &cell.model.model, None);
        let pre_estimate = next_run_reserve(
            coarse,
            observed_cost_ceiling.get(&cell.model.model).copied(),
        );
        if !budget_allows(spent, pre_estimate, spec.budget_usd) {
            budget_stop = Some(serde_json::json!({
                "stopped": true,
                "at_run_index": i,
                "runs_planned": runs.len(),
                "runs_executed": records.len(),
                "runs_skipped": runs.len() - records.len(),
                "spent_usd": f64_or_null(spent),
                "cap_usd": spec.budget_usd.map(|b| f64_or_null(b)),
                "estimated_overrun_usd": spec.budget_usd.map(|b| f64_or_null((spent - b).max(0.0))),
                "next_run_estimate_usd": f64_or_null(pre_estimate),
                "note": "cells after this point carry fewer cases than planned; their `n` and \
                         verdicts reflect only what was actually run. A single live call can \
                         exceed the pre-run estimate; inspect estimated_overrun_usd",
            }));
            break;
        }

        let started = std::time::Instant::now();
        let overrides = RunOverrides {
            runtime: Some(cell.model.runtime),
            model: Some(cell.model.model.clone()),
            agent: spec.agent.clone(),
            seed: run.seed,
        };
        let mut record = RunRecord {
            domain: cell.domain.clone(),
            role: cell.role.as_str().to_string(),
            runtime: cell.model.runtime.as_str().to_string(),
            model: cell.model.model.clone(),
            agent: overrides.effective_agent(case_file).to_string(),
            case_id: run.case_id.clone(),
            cluster: cluster.clone(),
            repeat_index: run.repeat_index,
            seed: run.seed,
            seed_applied: runner::SEED_APPLIED_ANYWHERE,
            score: None,
            error: None,
            skip: None,
            substituted: None,
            cost_usd: 0.0,
            cost_source: CostSource::Coarse,
            duration_ms: 0,
            message_recovered_from_stream: false,
            verifier: None,
        };

        let usage = match cell.role {
            MatrixRole::Executor => {
                run_executor_cell(
                    home,
                    path,
                    case_file,
                    &overrides,
                    run.repeat_index,
                    &mut record,
                )
                .await
            }
            MatrixRole::Verifier => {
                run_verifier_cell(home, path, case_file, cell, &mut record).await
            }
            // Refused at flag-parse time; unreachable, and a `continue` here
            // would silently shrink the matrix if that ever changed.
            MatrixRole::Planner => {
                record.error = Some(
                    "planner cells are deferred to P2b and must be refused at flag-parse time"
                        .to_string(),
                );
                None
            }
        };

        let (cost, source) = run_cost_usd(registry, &cell.model.model, usage);
        if source == CostSource::Reported {
            let ceiling = observed_cost_ceiling
                .entry(cell.model.model.clone())
                .or_default();
            *ceiling = (*ceiling).max(cost);
        }
        record.cost_usd = cost;
        record.cost_source = source;
        record.duration_ms = started.elapsed().as_millis();
        spent += cost;

        let cr = &mut cell_results[run.cell];
        cr.runs += 1;
        cr.cost_usd += cost;
        if record.error.is_some() {
            cr.errors += 1;
        }
        if record.skip.is_some() {
            cr.skipped += 1;
        }
        if record.substituted.is_some() {
            cr.substituted += 1;
        }
        if let Some(row) = record.verifier.clone() {
            cr.verifier_rows.push(row);
        }
        if let Some(score) = record.score {
            per_case
                .entry((run.cell, run.case_id.clone()))
                .or_default()
                .push(score);
        }
        println!(
            "  [{}/{}] {} · {} · {} · {} {}",
            i + 1,
            runs.len(),
            cell.domain,
            cell.role.as_str(),
            cell.model.as_ref_string(),
            run.case_id,
            match (&record.error, &record.skip, record.score) {
                (Some(e), _, _) => style(format!("ERROR {}", duduclaw_core::truncate_chars(e, 80)))
                    .red()
                    .to_string(),
                (_, Some(s), _) => style(format!("skip:{}", s.as_str())).yellow().to_string(),
                (_, _, Some(sc)) => {
                    // For a verifier run, the score alone ("1.00") hides which
                    // way the model was wrong — say the verdict and the gold it
                    // was scored against.
                    let detail = match &record.verifier {
                        Some(row) => format!(
                            "score={sc:.2} verdict={} gold={}",
                            row.verdict.as_str(),
                            row.gold.as_str()
                        ),
                        None => format!("score={sc:.2}"),
                    };
                    style(detail).green().to_string()
                }
                _ => style("no score").dim().to_string(),
            }
        );
        records.push(record);
    }

    // Collapse repeats into one score per case, then attach the cluster key.
    for ((cell_idx, case_id_key), scores) in per_case {
        let cluster = records
            .iter()
            .find(|r| r.case_id == case_id_key && r.domain == cells[cell_idx].domain)
            .map(|r| r.cluster.clone())
            .unwrap_or_else(|| ".".to_string());
        cell_results[cell_idx]
            .scored
            .push((case_id_key, cluster, stats::mean(&scores)));
    }
    (cell_results, records, budget_stop)
}

/// Executor cell: run the case live on this `(runtime, model)` and score the
/// deterministic assertions. `--record` is never honoured here (see module docs).
async fn run_executor_cell(
    home: &Path,
    path: &Path,
    case_file: &EvalCaseFile,
    overrides: &RunOverrides,
    repeat_index: Option<u32>,
    record: &mut RunRecord,
) -> Option<ReportedUsage> {
    match runner::obtain_transcript(
        path,
        case_file,
        home,
        RunMode::Live { record: false },
        repeat_index,
        overrides,
    )
    .await
    {
        Ok((transcript, facts)) => {
            record.substituted = facts.substituted.clone();
            if facts.substituted.is_some() {
                // A substituted run is evidence about the substitute, not about
                // the model that was asked — it must not enter the cell.
                record.error = Some(format!(
                    "excluded: failover answered with {:?} instead of the requested \
                     ({}, {})",
                    facts.substituted, record.runtime, record.model
                ));
                return facts.usage;
            }
            let results = super::assertions::run_assertions(&case_file.expect, &transcript);
            record.score = Some(if results.iter().all(|a| a.passed) {
                1.0
            } else {
                0.0
            });
            facts.usage
        }
        Err(e) => {
            record.error = Some(e);
            None
        }
    }
}

/// Verifier cell: judge the case's recorded transcript on this
/// `(runtime, model)` and score the verdict against the deterministic gold.
async fn run_verifier_cell(
    home: &Path,
    path: &Path,
    case_file: &EvalCaseFile,
    cell: &CellPlan,
    record: &mut RunRecord,
) -> Option<ReportedUsage> {
    if case_file.expect.is_empty() {
        record.skip = Some(verifier_cell::VerifierSkip::NoDeterministicCriteria);
        return None;
    }
    // The BASELINE transcript (no `.r<N>`): a verifier cell's repeats re-ask the
    // judge about the same recorded work, they do not re-record it.
    let transcript_file = runner::transcript_path(path, case_file, None);
    let raw = match std::fs::read_to_string(&transcript_file) {
        Ok(raw) => raw,
        Err(_) => {
            record.skip = Some(verifier_cell::VerifierSkip::NoRecordedTranscript {
                path: transcript_file.display().to_string(),
            });
            return None;
        }
    };
    let transcript = match super::transcript::parse_stream_json(&raw) {
        Ok(t) => t,
        Err(e) => {
            record.skip = Some(verifier_cell::VerifierSkip::UnparseableTranscript {
                path: transcript_file.display().to_string(),
                error: e,
            });
            return None;
        }
    };
    let (gold, _) = verifier_cell::gold_verdict(case_file, &transcript);
    let prompt = verifier_cell::build_verifier_prompt(case_file, &transcript);
    match verifier_cell::ask_verifier(home, cell.model.runtime, &cell.model.model, &prompt).await {
        Ok(outcome) => {
            record.substituted = outcome.substituted.clone();
            record.message_recovered_from_stream = outcome.message_recovered_from_stream;
            if outcome.substituted.is_some() {
                record.error = Some(format!(
                    "excluded: the verifier call was answered by {:?} instead of the requested \
                     ({}, {})",
                    outcome.substituted, record.runtime, record.model
                ));
                return None;
            }
            let row = VerifierRow::score(&record.case_id, gold, &outcome.raw);
            // Agreement is the cell's score; an unparseable verdict contributes
            // no score (and is counted separately by `VerifierTally`).
            record.score = row.agreement.map(|ok| if ok { 1.0 } else { 0.0 });
            record.verifier = Some(row);
            None
        }
        Err(e) => {
            record.error = Some(e);
            None
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Reporting
// ─────────────────────────────────────────────────────────────────────────

/// Reason a cell's statistical verdict was overridden to `unresolved`.
/// `None` ⇒ the verdict is the statistics' own.
pub const VERDICT_REASON_DEGENERATE_GOLD: &str = "degenerate_gold";
/// The confidence interval has zero width, so it carries no information: either
/// `n == 1`, or every observation was identical. Reporting `pass`/`fail` off such
/// an interval is a fabricated certainty (smoke 3, 2026-09-25).
pub const VERDICT_REASON_DEGENERATE_INTERVAL: &str = "degenerate_interval";

fn cell_json(
    cell: &CellResult,
    mde: f64,
) -> (serde_json::Value, Option<MatrixCell>, MatrixVerdict) {
    let values: Vec<f64> = cell.scored.iter().map(|(_, _, s)| *s).collect();
    let clusters: Vec<&str> = cell.scored.iter().map(|(_, c, _)| c.as_str()).collect();
    let n = values.len();
    let n_clusters = clusters.iter().collect::<BTreeSet<_>>().len();
    let mean = stats::mean(&values);
    let se_clt = stats::se_clt(&values);
    let se_clustered = stats::se_clustered(&values, &clusters);
    let se_ratio = stats::se_ratio(se_clustered, se_clt);
    // Both estimators are still reported for transparency; `chosen` is the one
    // the interval actually uses (see `choose_se` for why a one-cluster suite
    // must NOT use the cluster-robust zero).
    let chosen = choose_se(&values, &clusters);
    let row = resolution_row(mean, chosen.se, n as f64, mde, PASS_LINE);
    let tally =
        (!cell.verifier_rows.is_empty()).then(|| VerifierTally::from_rows(&cell.verifier_rows));

    // Degenerate gold (verifier cells only): one gold class absent ⇒ agreement
    // cannot separate "judges well" from "always answers the same thing", so the
    // verdict is forced `unresolved` however clean the interval looks. The
    // arithmetic is still reported in full — nothing is hidden, the CONCLUSION is
    // withheld — and `verdict_reason` says which rule withheld it.
    let degenerate_gold = tally.as_ref().is_some_and(|t| t.degenerate_gold);
    // A zero-width interval (n == 1, or every observation identical) says
    // nothing, however clean it looks. Smoke 3 produced `mean 0.5 ci=[0.5,0.5]`
    // and a `resolved` bottleneck from exactly this.
    let degenerate_interval = n > 0 && chosen.is_degenerate();
    let (stat_verdict, stat_label) = (row.verdict, row.label);
    // Precedence: a degenerate gold invalidates the metric itself, so it is
    // named first; a degenerate interval invalidates only the conclusion drawn
    // from it. Both force `unresolved`.
    let forced_reason = if degenerate_gold {
        Some(VERDICT_REASON_DEGENERATE_GOLD)
    } else if degenerate_interval {
        Some(VERDICT_REASON_DEGENERATE_INTERVAL)
    } else {
        None
    };
    let (verdict_word, label_word, verdict_reason) = match forced_reason {
        Some(reason) => (
            stats::Verdict::Unresolved,
            // `Candidate`'s documented meaning is "not a judgment" — which is
            // exactly the claim here; `verdict_reason` distinguishes this from a
            // plain sample-size shortfall.
            stats::HonestLabel::Candidate,
            Some(reason),
        ),
        None => (stat_verdict, stat_label, None),
    };
    // Why `q` could not be computed, when it could not. Printed by the console
    // summary so a null `q` is never left looking like a bug.
    let q_note: Option<&str> = if row.q.is_finite() {
        None
    } else if n == 0 {
        Some("no observations")
    } else if chosen.is_degenerate() {
        Some("zero-variance interval, so the required sample size is 0")
    } else {
        Some("required sample size not computable")
    };
    let verdict = match verdict_word {
        stats::Verdict::Pass => MatrixVerdict::Pass,
        stats::Verdict::Fail => MatrixVerdict::Fail,
        stats::Verdict::Unresolved => MatrixVerdict::Unresolved,
    };

    let json = serde_json::json!({
        "domain": cell.plan.domain,
        "role": cell.plan.role.as_str(),
        "runtime": cell.plan.model.runtime.as_str(),
        "model": cell.plan.model.model,
        "n": n,
        "n_clusters": n_clusters,
        "runs": cell.runs,
        "errors": cell.errors,
        "skipped": cell.skipped,
        "substituted": cell.substituted,
        "mean": f64_or_null(mean),
        "se_clt": f64_or_null(se_clt),
        "se_clustered": f64_or_null(se_clustered),
        "se_ratio": f64_or_null(se_ratio),
        // The SE the interval below actually used, and which estimator it is.
        "se_used": f64_or_null(chosen.se),
        "se_source": chosen.source,
        "ci95_low": f64_or_null(row.ci_low),
        "ci95_high": f64_or_null(row.ci_high),
        "n_required_for_mde": f64_or_null(row.n_required),
        "resolution_ratio_q": f64_or_null(row.q),
        "mde_at_n": f64_or_null(row.mde_at_n),
        "verdict": verdict_word,
        "label": label_word,
        // Why `verdict` differs from what the statistics alone said. `null`
        // when it does not.
        "verdict_reason": verdict_reason,
        // The statistics' own answer, kept alongside the override so the two are
        // never conflated and an operator can see exactly what was withheld.
        "verdict_statistical": stat_verdict,
        "label_statistical": stat_label,
        "degenerate_gold": degenerate_gold,
        "degenerate_interval": degenerate_interval,
        "q_note": q_note,
        "small_cluster_warning": n_clusters > 0 && n_clusters < MIN_RELIABLE_CLUSTERS,
        "cost_usd": f64_or_null(cell.cost_usd),
        "verifier": tally,
    });

    // A cell with zero usable observations is not a measurement — it gets a
    // report row (so the gap is visible) but no matrix cell.
    let matrix_cell = (n > 0).then(|| {
        let mut c = MatrixCell::new(
            &cell.plan.domain,
            cell.plan.role,
            cell.plan.model.runtime.as_str(),
            &cell.plan.model.model,
            n,
            mean,
            row.ci_low,
            row.ci_high,
            verdict,
            chrono::Utc::now().to_rfc3339(),
            row.mde_at_n,
        )
        // 2026-09-28 review: cells measured under different conditions are not
        // comparable, and the file has to say which. `--matrix` runs one role
        // at a time with **no other role's model in the loop** — an executor
        // cell runs the case directly, a verifier cell re-judges a recorded
        // transcript — so `solo` is the honest condition here, as against the
        // `planner=strong` / `executor=strong` a `--team-2x2` probe writes.
        .with_conditioned_on("solo");
        // Review finding (P2): the JSON half of this function already reports
        // `degenerate_interval`, but the PERSISTED cell kept writing the
        // zero-width interval and `mde = 0.0` — violating
        // `role_model_matrix`'s own module rule ("zero variance ⇒ absent key")
        // and handing P5's durable prior an interval that reads as certainty.
        // `team_probe::cell_from_scores` has always done this; `matrix.rs` did
        // not.
        if degenerate_interval {
            c.ci95_low = None;
            c.ci95_high = None;
            c.mde = None;
        }
        c
    });
    (json, matrix_cell, verdict)
}

/// The statistics half of one cell's console line.
///
/// Keyed on the **mean**, not on `q`. Smoke 3 (2026-09-25) printed
/// "no usable observation" for cells that had a perfectly good mean, purely
/// because `q` was null — telling the operator the opposite of the truth. A null
/// `q` now still prints the mean and interval, and says WHY `q` is missing.
fn cell_console_numbers(json: &serde_json::Value) -> String {
    match json["mean"].as_f64() {
        None => "no usable observation".to_string(),
        Some(mean) => {
            let interval = match (json["ci95_low"].as_f64(), json["ci95_high"].as_f64()) {
                (Some(lo), Some(hi)) => format!(" [{lo:.2},{hi:.2}]"),
                _ => String::new(),
            };
            let q = match (json["resolution_ratio_q"].as_f64(), json["q_note"].as_str()) {
                (Some(q), _) => format!(" q={q:.2}"),
                (None, Some(note)) => format!(" q=n/a ({note})"),
                (None, None) => " q=n/a".to_string(),
            };
            format!("mean={mean:.2}{interval}{q}")
        }
    }
}

/// Run the whole matrix. Errors here are infra/spec failures; a failing *cell*
/// is data, not an error — see [`run_matrix`]'s exit contract in the guide.
pub async fn run_matrix(home: &Path, opts: &EvalOptions) -> duduclaw_core::error::Result<()> {
    let spec =
        MatrixSpec::from_options(opts).map_err(duduclaw_core::error::DuDuClawError::Agent)?;

    let mut domains: BTreeMap<String, DomainCases> = BTreeMap::new();
    let mut domain_order: Vec<String> = Vec::new();
    for root in &spec.domains {
        let d = discover_domain(root, &spec).map_err(duduclaw_core::error::DuDuClawError::Agent)?;
        if domains.contains_key(&d.name) {
            return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
                "two --domain paths resolve to the same domain name {:?} ({} and {}) — a cell key \
                 would be ambiguous",
                d.name,
                domains[&d.name].root.display(),
                d.root.display()
            )));
        }
        domain_order.push(d.name.clone());
        domains.insert(d.name.clone(), d);
    }

    let cells = expand_cells(&domain_order, &spec.roles, &spec.models);
    let cases_per_domain: BTreeMap<String, Vec<String>> = domains
        .iter()
        .map(|(name, d)| {
            (
                name.clone(),
                d.cases.iter().map(|(_, id, _, _)| id.clone()).collect(),
            )
        })
        .collect();
    let runs = expand_runs(&cells, &cases_per_domain, spec.repeats, spec.paired_seeds);

    println!();
    println!(
        "  {} {}",
        style("🧪").bold(),
        style("Role→model capability matrix").bold()
    );
    println!(
        "  {} cells × up to {} runs | declared MDE {:.1}pp | α={} power={} | K={} | cluster-by {}",
        cells.len(),
        runs.len(),
        spec.mde * 100.0,
        ALPHA,
        POWER,
        spec.repeats,
        spec.cluster_by,
    );
    println!(
        "  {} planner is deferred to P2b (not measured, not scored)",
        style("note:").dim()
    );
    println!();

    let mut registry = duduclaw_llm::ModelRegistry::vendored();
    // A matrix can use models added after the vendored seed was published.
    // Price the same operator overrides as the rest of the LLM layer.
    registry
        .load_override(&home.join("models.toml"))
        .map_err(duduclaw_core::error::DuDuClawError::Agent)?;
    validate_budget_prices(&registry, &spec, home)
        .map_err(duduclaw_core::error::DuDuClawError::Agent)?;
    let (cell_results, records, budget_stop) =
        execute(home, &spec, &cells, &domains, &runs, &registry).await;

    // ── cells + matrix file ──────────────────────────────────────────────
    let mut cells_json = Vec::new();
    let mut matrix = RoleModelMatrix::new(MatrixHeader {
        declared_mde: spec.mde,
        alpha: ALPHA,
        power: POWER,
        repeats: spec.repeats,
        cluster_by: spec.cluster_by.clone(),
        planner: PlannerState::Deferred,
        generated_at: chrono::Utc::now().to_rfc3339(),
        paired_seeds: spec.paired_seeds,
    });
    for cell in &cell_results {
        let (json, matrix_cell, _) = cell_json(cell, spec.mde);
        cells_json.push(json);
        if let Some(mc) = matrix_cell {
            matrix.cells.push(mc);
        }
    }

    // ── bottleneck ───────────────────────────────────────────────────────
    let deltas: Vec<RoleDelta> = match (&spec.weak, &spec.strong) {
        (Some(weak), Some(strong)) => spec
            .roles
            .iter()
            .map(|role| {
                let arm = |m: &ModelRef| -> Vec<Scored> {
                    cell_results
                        .iter()
                        .filter(|c| c.plan.role == *role && c.plan.model == *m)
                        .flat_map(|c| c.scored.iter().cloned())
                        .collect()
                };
                role_delta(*role, &arm(weak), &arm(strong))
            })
            .collect(),
        _ => Vec::new(),
    };
    // A degenerate-gold arm makes that role's Δ arithmetic meaningless. The arm
    // is NOT dropped (that would shrink a comparison behind the operator's back);
    // the Δ is annotated, and `bottleneck` then refuses to resolve on it.
    let mut deltas = deltas;
    for d in deltas.iter_mut() {
        let role = MatrixRole::parse(&d.role);
        let degenerate: Vec<String> = cell_results
            .iter()
            .filter(|c| Some(c.plan.role) == role)
            .filter(|c| {
                !c.verifier_rows.is_empty()
                    && VerifierTally::from_rows(&c.verifier_rows).degenerate_gold
            })
            .map(|c| c.plan.model.as_ref_string())
            .collect();
        if !degenerate.is_empty() {
            let note = format!(
                "one or more cells for this role had a degenerate gold ({}) — Δ is not \
                 interpretable until those suites are re-recorded with a mixed gold",
                degenerate.join(", ")
            );
            d.note = Some(match d.note.take() {
                Some(prev) => format!("{prev}; {note}"),
                None => note,
            });
            d.degenerate = true;
            // Gold wins over an interval cause: it invalidates the metric, not
            // just the conclusion drawn from it.
            d.degenerate_reason = Some(VERDICT_REASON_DEGENERATE_GOLD.to_string());
        }
    }
    let bottleneck_outcome = if deltas.is_empty() {
        BottleneckOutcome {
            bottleneck_role: None,
            resolution: "unresolved",
            reason: "no --weak/--strong pair was given, so no Δ was measured".to_string(),
        }
    } else {
        bottleneck(&deltas)
    };

    // ── cost roll-up ─────────────────────────────────────────────────────
    let total_cost: f64 = records.iter().map(|r| r.cost_usd).sum();
    let reported = records
        .iter()
        .filter(|r| r.cost_source == CostSource::Reported)
        .count();
    let coarse = records
        .iter()
        .filter(|r| r.cost_source == CostSource::Coarse)
        .count();
    let unknown_model: BTreeSet<&str> = records
        .iter()
        .filter(|r| r.cost_source == CostSource::CoarseUnknownModel)
        .map(|r| r.model.as_str())
        .collect();
    let cost_json = serde_json::json!({
        "total_usd": f64_or_null(total_cost),
        "runs_priced_from_reported_usage": reported,
        "runs_priced_coarsely": coarse,
        "runs_priced_as_unknown_model": records.len() - reported - coarse,
        "models_not_in_registry": unknown_model,
        "coarse_assumption": {
            "input_tokens": COARSE_INPUT_TOKENS,
            "output_tokens": COARSE_OUTPUT_TOKENS,
            "unknown_model_usd_per_run": COARSE_UNKNOWN_MODEL_USD,
        },
        "note": "priced through the vendored ModelRegistry plus <DUDUCLAW_HOME>/models.toml. \
                 A run without reported usage uses the coarse token assumption; an unknown model \
                 uses a labelled flat stub only when no budget was requested. Later runs reserve \
                 twice the largest observed cost for that model. This remains an estimate: one run \
                 may exceed its reserve and the budget cap; inspect budget_stop.spent_usd.",
    });

    // ── console summary ──────────────────────────────────────────────────
    println!();
    println!("  {}", style("─".repeat(60)).dim());
    println!(
        "  Declared MDE: {} — the matrix cannot support any finer claim",
        style(format!("{:.1}pp", spec.mde * 100.0)).bold()
    );
    for (cell, json) in cell_results.iter().zip(cells_json.iter()) {
        let v = json["verdict"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_default();
        // An empty cell has no statistics at all. Printing `mean=NaN [NaN,NaN]`
        // reads like a broken computation rather than "nothing was measured", so
        // say the latter — the JSON's `null`s say the same thing to a machine.
        let numbers = cell_console_numbers(json);
        // Verifier cells show their gold split: `agreement 1.00` over an
        // all-FAIL gold is the exact shape that looked like a result in the
        // smoke run and was not one.
        let gold = match json["verifier"].as_object() {
            Some(t) => format!(
                " gold={}P/{}F{}",
                t["gold_pass"].as_u64().unwrap_or(0),
                t["gold_fail"].as_u64().unwrap_or(0),
                if t["unparseable"].as_u64().unwrap_or(0) > 0 {
                    format!(" unparseable={}", t["unparseable"].as_u64().unwrap_or(0))
                } else {
                    String::new()
                }
            ),
            None => String::new(),
        };
        let reason = match json["verdict_reason"].as_str() {
            Some(r) => style(format!(" ({r})")).yellow().to_string(),
            None => String::new(),
        };
        println!(
            "  {:<12} {:<9} {:<34} n={:<3} {}{} → {}{}",
            cell.plan.domain,
            cell.plan.role.as_str(),
            cell.plan.model.as_ref_string(),
            json["n"].as_u64().unwrap_or(0),
            numbers,
            gold,
            v,
            reason,
        );
        if json["degenerate_interval"].as_bool().unwrap_or(false) {
            println!(
                "               {} zero-width interval ({}) — every observation identical or \
                 n=1, so this is not a precise measurement, it is an unestimated one.",
                style("WARNING:").yellow().bold(),
                match json["se_source"].as_str() {
                    Some("clt_single_cluster") =>
                        "one cluster, so the unclustered CLT SE was used and it came out 0",
                    _ => "se = 0",
                },
            );
        }
        if json["degenerate_gold"].as_bool().unwrap_or(false) {
            println!(
                "               {} one gold class is absent — agreement cannot tell a good judge \
                 from one that always answers the same way. Re-record this suite's transcripts \
                 so its gold is mixed.",
                style("WARNING:").yellow().bold(),
            );
        }
        if cell.errors > 0 || cell.skipped > 0 || cell.substituted > 0 {
            println!(
                "               {} {} error(s), {} skipped, {} substituted",
                style("·").dim(),
                cell.errors,
                cell.skipped,
                cell.substituted
            );
        }
    }
    println!();
    for d in &deltas {
        match (d.delta, d.ci95_low, d.ci95_high) {
            (Some(delta), Some(lo), Some(hi)) => println!(
                "  Δ {:<9} = {:+.3} [{:+.3}, {:+.3}] (n={}, {}, se={}){}",
                d.role,
                delta,
                lo,
                hi,
                d.n,
                if d.paired { "paired" } else { "UNPAIRED" },
                match d.se_source {
                    SeSource::Clustered => "clustered",
                    SeSource::CltSingleCluster => "clt/1-cluster",
                    SeSource::Undefined => "n/a",
                },
                match &d.degenerate_reason {
                    Some(r) => style(format!(" [{r}]")).yellow().to_string(),
                    None => String::new(),
                },
            ),
            _ => println!(
                "  Δ {:<9} = {}",
                d.role,
                style(
                    d.note
                        .clone()
                        .unwrap_or_else(|| "not computable".to_string())
                )
                .yellow()
            ),
        }
    }
    println!(
        "  Bottleneck: {} — {}",
        match &bottleneck_outcome.bottleneck_role {
            Some(r) => style(r.clone()).green().bold().to_string(),
            None => style("unresolved").yellow().bold().to_string(),
        },
        bottleneck_outcome.reason,
    );
    println!();
    println!(
        "  Estimated cost: ${:.4} over {} run(s) ({} from reported usage, {} coarse)",
        total_cost,
        records.len(),
        reported,
        coarse
    );
    if let Some(stop) = &budget_stop {
        println!(
            "  {} budget cap reached — {} of {} planned run(s) were skipped",
            style("WARNING:").yellow().bold(),
            stop["runs_skipped"].as_u64().unwrap_or(0),
            runs.len()
        );
    }
    println!();

    // ── files ────────────────────────────────────────────────────────────
    if let Some(report_path) = &spec.report {
        let json = serde_json::json!({
            "kind": "role_model_matrix",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "header": {
                "declared_mde": spec.mde,
                "alpha": ALPHA,
                "power": POWER,
                "repeats": spec.repeats,
                "cluster_by": spec.cluster_by,
                "paired_seeds": spec.paired_seeds,
                "seed_applied": runner::SEED_APPLIED_ANYWHERE,
                "planner": PlannerState::Deferred.as_str(),
                "planner_note": "deferred to P2b: a planner call emits sub-task packets, not an \
                                 answer a deterministic assertion can grade",
                "replay_forbidden": true,
                "record_forbidden": true,
                "temperature_declared": spec.temperature.map(f64_or_null),
                "production_temperature": PRODUCTION_TEMPERATURE,
                "roles": spec.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                "models": spec.models.iter().map(|m| m.as_ref_string()).collect::<Vec<_>>(),
                "weak": spec.weak.as_ref().map(|m| m.as_ref_string()),
                "strong": spec.strong.as_ref().map(|m| m.as_ref_string()),
                "domains": domain_order,
                "max_cases": spec.max_cases,
                "pass_line": PASS_LINE,
                // `null` ⇒ every case ran under its own `[case] agent`. A
                // non-null value means one provisioned agent carried the whole
                // matrix: the model is still what is measured, but the system
                // prompt every case ran under was NOT the case's own, so the
                // override is declared here rather than inferred from `runs[]`.
                "agent_override": spec.agent,
                "agent_override_note": "a matrix measures the model, not the persona; borrowing one \
                                        provisioned agent is acceptable for a probe run but must be \
                                        declared, because it changes the system prompt every case \
                                        ran under",
                "verifier_output_schema": verifier_cell::verifier_output_schema(),
                "verifier_output_schema_note": "requested on every verifier call; honoured only by \
                                                codex (`codex exec --output-schema`). Other runtimes \
                                                log and ignore it, and the parser accepts the plain \
                                                first-token PASS/FAIL form either way.",
            },
            "cells": cells_json,
            "bottleneck": {
                "method": "decoupled 2x2 probe (AgentCARD arXiv:2606.20629, Shapley probe without \
                           the joint team run): each role is measured independently on the same \
                           weak/strong pair, so a genuine role interaction is invisible by \
                           construction",
                "per_role": deltas,
                "outcome": bottleneck_outcome,
            },
            "cost_estimate": cost_json,
            "budget_stop": budget_stop,
            "runs": records,
        });
        std::fs::write(report_path, serde_json::to_string_pretty(&json)? + "\n")?;
        println!("  Report written to {}", report_path.display());

        let dir = report_path.parent().unwrap_or_else(|| Path::new("."));
        let toml_path = matrix_path(dir);
        match matrix.save(&toml_path) {
            Ok(()) => println!("  Matrix written to {}", toml_path.display()),
            Err(e) => println!(
                "  {} could not write {}: {e}",
                style("WARNING:").yellow().bold(),
                toml_path.display()
            ),
        }
        println!();
    } else {
        println!(
            "  {} no --report path given, so neither the JSON report nor {} was written",
            style("note:").dim(),
            duduclaw_core::role_model_matrix::MATRIX_FILE_NAME
        );
        println!();
    }

    // Exit contract: a failing or unresolved cell is a MEASUREMENT, so it exits
    // 0. Zero usable observations across the whole matrix is an infra failure —
    // nothing was measured and a green exit would say otherwise.
    if !cell_results.is_empty() && cell_results.iter().all(|c| c.scored.is_empty()) {
        return Err(duduclaw_core::error::DuDuClawError::Agent(format!(
            "no cell produced a single usable observation across {} run(s) — every run errored, \
             was skipped, or was answered by a substituted model; see the report's `runs` array",
            records.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> EvalOptions {
        EvalOptions {
            path: Some(PathBuf::from("evals/demo")),
            filter: None,
            replay: false,
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
            matrix: true,
            team_2x2: false,
            planner_weak: None,
            planner_strong: None,
            executor_weak: None,
            executor_strong: None,
            verifier_model: None,
            team_effort: None,
            team_fanout: None,
            team_grok_sandbox_off: false,
            roles: vec!["executor".to_string(), "verifier".to_string()],
            models: vec!["claude:claude-haiku-4-5".to_string()],
            weak: None,
            strong: None,
            domain: Vec::new(),
            budget_usd: None,
            max_cases: None,
            agent: None,
        }
    }

    fn mr(rt: RuntimeType, model: &str) -> ModelRef {
        ModelRef {
            runtime: rt,
            model: model.to_string(),
        }
    }

    // ── flag parsing / hard rules ────────────────────────────────────────

    #[test]
    fn matrix_refuses_replay_and_names_the_replay_gap() {
        let mut o = opts();
        o.replay = true;
        let err = MatrixSpec::from_options(&o).unwrap_err();
        assert!(err.contains("Replay Gap"), "{err}");
        assert!(err.contains("2608.08239"), "{err}");
    }

    #[test]
    fn matrix_refuses_a_temperature_below_production_and_accepts_at_or_above() {
        let mut o = opts();
        o.temperature = Some(0.0);
        let err = MatrixSpec::from_options(&o).unwrap_err();
        assert!(err.contains("below the production value"), "{err}");
        o.temperature = Some(PRODUCTION_TEMPERATURE);
        assert!(MatrixSpec::from_options(&o).is_ok());
        o.temperature = Some(1.5);
        assert!(MatrixSpec::from_options(&o).is_ok());
        o.temperature = Some(f64::NAN);
        assert!(MatrixSpec::from_options(&o).unwrap_err().contains("finite"));
    }

    #[test]
    fn planner_is_refused_with_the_deferral_reason() {
        let err = parse_roles(&["executor".into(), "planner".into()]).unwrap_err();
        assert!(err.contains("deferred to P2b"), "{err}");
        assert!(err.contains("deferred"), "{err}");
    }

    #[test]
    fn roles_are_comma_split_deduped_and_order_preserving() {
        let r = parse_roles(&["verifier, executor".into(), "executor".into()]);
        // `--roles` is comma-delimited by clap, so a single element with a comma
        // only occurs in tests; either way the parser must refuse it rather than
        // inventing a role.
        assert!(r.is_err(), "a comma inside one element is not a role name");
        let r = parse_roles(&["verifier".into(), "executor".into(), "verifier".into()]).unwrap();
        assert_eq!(r, vec![MatrixRole::Verifier, MatrixRole::Executor]);
        assert!(parse_roles(&[]).unwrap_err().contains("--roles"));
    }

    #[test]
    fn model_refs_require_an_explicit_runtime() {
        assert_eq!(
            ModelRef::parse("codex:gpt-5.6-sol").unwrap(),
            mr(RuntimeType::Codex, "gpt-5.6-sol")
        );
        assert!(
            ModelRef::parse("claude-sonnet-4-6")
                .unwrap_err()
                .contains("<runtime>:<model>")
        );
        assert!(
            ModelRef::parse("nope:x")
                .unwrap_err()
                .contains("not a runtime")
        );
        assert!(
            ModelRef::parse("claude:")
                .unwrap_err()
                .contains("empty model")
        );
        assert_eq!(
            ModelRef::parse(" claude : claude-haiku-4-5 ")
                .unwrap()
                .as_ref_string(),
            "claude:claude-haiku-4-5"
        );
    }

    #[test]
    fn weak_and_strong_must_come_as_a_distinct_pair_and_are_always_measured() {
        let mut o = opts();
        o.weak = Some("claude:claude-haiku-4-5".into());
        assert!(
            MatrixSpec::from_options(&o)
                .unwrap_err()
                .contains("must be given together")
        );
        o.strong = Some("claude:claude-haiku-4-5".into());
        assert!(
            MatrixSpec::from_options(&o)
                .unwrap_err()
                .contains("same model")
        );
        o.strong = Some("claude:claude-sonnet-4-6".into());
        let spec = MatrixSpec::from_options(&o).unwrap();
        // sonnet was not in --models but must still get cells, or Δ has no arm.
        assert!(
            spec.models
                .contains(&mr(RuntimeType::Claude, "claude-sonnet-4-6"))
        );
        assert_eq!(spec.models.len(), 2);
    }

    #[test]
    fn matrix_discovers_same_stem_cases_in_distinct_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("suite");
        for folder in ["north", "south"] {
            let dir = root.join(folder);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("checkins.toml"),
                format!("[case]\nname = \"{folder}\"\nagent = \"care\"\nprompt = \"draft\"\n[expect]\noutput_contains = [\"draft\"]\n"),
            )
            .unwrap();
        }
        let spec = MatrixSpec::from_options(&opts()).unwrap();
        let domain = discover_domain(&root, &spec).unwrap();
        let ids: Vec<_> = domain
            .cases
            .iter()
            .map(|(_, id, _, _)| id.as_str())
            .collect();
        assert_eq!(ids, ["north/checkins", "south/checkins"]);
    }

    #[test]
    fn a_bad_mde_cluster_key_or_budget_is_refused() {
        let mut o = opts();
        o.mde = 0.0;
        assert!(MatrixSpec::from_options(&o).unwrap_err().contains("--mde"));
        o = opts();
        o.cluster_by = "case".to_string();
        assert!(
            MatrixSpec::from_options(&o)
                .unwrap_err()
                .contains("not implemented")
        );
        o = opts();
        o.budget_usd = Some(0.0);
        assert!(
            MatrixSpec::from_options(&o)
                .unwrap_err()
                .contains("--budget-usd")
        );
        o = opts();
        o.max_cases = Some(0);
        assert!(
            MatrixSpec::from_options(&o)
                .unwrap_err()
                .contains("--max-cases")
        );
    }

    #[test]
    fn domains_default_to_the_positional_path() {
        let spec = MatrixSpec::from_options(&opts()).unwrap();
        assert_eq!(spec.domains, vec![PathBuf::from("evals/demo")]);
        let mut o = opts();
        o.domain = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(MatrixSpec::from_options(&o).unwrap().domains.len(), 2);
    }

    // ── expansion + serial order ─────────────────────────────────────────

    #[test]
    fn cells_expand_domain_then_role_then_model_in_operator_order() {
        let cells = expand_cells(
            &["d1".to_string(), "d2".to_string()],
            &[MatrixRole::Executor, MatrixRole::Verifier],
            &[
                mr(RuntimeType::Claude, "haiku"),
                mr(RuntimeType::Codex, "gpt"),
            ],
        );
        assert_eq!(cells.len(), 8);
        let spelled: Vec<String> = cells
            .iter()
            .map(|c| {
                format!(
                    "{}/{}/{}",
                    c.domain,
                    c.role.as_str(),
                    c.model.as_ref_string()
                )
            })
            .collect();
        assert_eq!(
            spelled,
            vec![
                "d1/executor/claude:haiku",
                "d1/executor/codex:gpt",
                "d1/verifier/claude:haiku",
                "d1/verifier/codex:gpt",
                "d2/executor/claude:haiku",
                "d2/executor/codex:gpt",
                "d2/verifier/claude:haiku",
                "d2/verifier/codex:gpt",
            ]
        );
    }

    #[test]
    fn runs_are_serial_cell_then_case_then_repeat() {
        let cells = expand_cells(
            &["d".to_string()],
            &[MatrixRole::Executor],
            &[
                mr(RuntimeType::Claude, "haiku"),
                mr(RuntimeType::Codex, "gpt"),
            ],
        );
        let mut cases = BTreeMap::new();
        cases.insert("d".to_string(), vec!["c1".to_string(), "c2".to_string()]);
        let runs = expand_runs(&cells, &cases, 2, true);
        assert_eq!(runs.len(), 2 * 2 * 2);
        let spelled: Vec<String> = runs
            .iter()
            .map(|r| format!("{}:{}:{}", r.cell, r.case_id, r.repeat_index.unwrap()))
            .collect();
        assert_eq!(
            spelled,
            vec![
                "0:c1:1", "0:c1:2", "0:c2:1", "0:c2:2", "1:c1:1", "1:c1:2", "1:c2:1", "1:c2:2",
            ],
            "one CLI spawn at a time, cell by cell"
        );
        // `--repeats 1` keeps the pre-P2 `None` convention.
        let single = expand_runs(&cells, &cases, 1, false);
        assert!(single.iter().all(|r| r.repeat_index.is_none()));
        assert!(single.iter().all(|r| r.seed.is_none()));
    }

    #[test]
    fn a_domain_with_no_cases_contributes_no_runs() {
        let cells = expand_cells(
            &["d".to_string()],
            &[MatrixRole::Executor],
            &[mr(RuntimeType::Claude, "haiku")],
        );
        assert!(expand_runs(&cells, &BTreeMap::new(), 1, false).is_empty());
    }

    #[test]
    fn paired_seeds_are_model_independent_and_stable() {
        let a = derive_seed("p1-hr-recruit-candidate-001", 1);
        let b = derive_seed("p1-hr-recruit-candidate-001", 1);
        assert_eq!(a, b, "same (case, repeat) ⇒ same seed, in every cell");
        assert_ne!(a, derive_seed("p1-hr-recruit-candidate-001", 2));
        assert_ne!(a, derive_seed("p1-hr-recruit-candidate-002", 1));
        // Known-value lock so a refactor cannot silently change the pairing
        // (FNV-1a 64 over `"c1" + "#" + 1u32.to_be_bytes()`).
        assert_eq!(derive_seed("c1", 1), 0x0a1b_eb09_baae_ece1_u64);
    }

    // ── cost + budget ────────────────────────────────────────────────────

    #[test]
    fn an_unknown_model_is_priced_as_a_labelled_stub() {
        let reg = duduclaw_llm::ModelRegistry::empty();
        let (usd, src) = run_cost_usd(&reg, "no-such-model", None);
        assert_eq!(src, CostSource::CoarseUnknownModel);
        assert_eq!(usd, COARSE_UNKNOWN_MODEL_USD);
    }

    #[test]
    fn budget_refuses_unknown_model_before_live_dispatch() {
        let mut o = opts();
        o.models = vec!["codex:no-such-model".to_string()];
        o.budget_usd = Some(1.0);
        let spec = MatrixSpec::from_options(&o).unwrap();
        let registry = duduclaw_llm::ModelRegistry::vendored();
        let err =
            validate_budget_prices(&registry, &spec, Path::new("/tmp/eval-home")).unwrap_err();
        assert!(err.contains("codex:no-such-model"), "{err}");
        assert!(err.contains("models.toml"), "{err}");
    }

    #[test]
    fn reported_usage_prices_differently_from_the_coarse_assumption() {
        let reg = duduclaw_llm::ModelRegistry::vendored();
        // Pick any model the vendored table actually knows, so the test does not
        // hard-code a model id that may be retired.
        let Some(known) = reg.models().next().map(|m| m.id.clone()) else {
            return;
        };
        let (coarse, src_coarse) = run_cost_usd(&reg, &known, None);
        assert_eq!(src_coarse, CostSource::Coarse);
        let (tiny, src_reported) = run_cost_usd(
            &reg,
            &known,
            Some(ReportedUsage {
                input_tokens: 10,
                output_tokens: 1,
                cache_read_tokens: 0,
            }),
        );
        assert_eq!(src_reported, CostSource::Reported);
        assert!(tiny < coarse, "10 tokens must not cost what 25k does");
    }

    #[test]
    fn the_budget_gate_stops_before_the_run_that_would_exceed_the_cap() {
        assert!(budget_allows(0.0, 1.0, None), "no cap ⇒ always allowed");
        assert!(
            budget_allows(0.9, 0.1, Some(1.0)),
            "exactly at the cap is allowed"
        );
        assert!(!budget_allows(0.95, 0.1, Some(1.0)));
        assert!(!budget_allows(1.0, 0.0001, Some(1.0)));
    }

    #[test]
    fn observed_large_run_raises_the_next_reserve() {
        assert_eq!(next_run_reserve(0.18, None), 0.18);
        assert_eq!(next_run_reserve(0.18, Some(6.0)), 12.0);
        assert!(!budget_allows(
            6.0,
            next_run_reserve(0.18, Some(6.0)),
            Some(10.0)
        ));
    }

    // ── Δ and the bottleneck ─────────────────────────────────────────────

    fn scored(pairs: &[(&str, &str, f64)]) -> Vec<Scored> {
        pairs
            .iter()
            .map(|(id, cl, s)| (id.to_string(), cl.to_string(), *s))
            .collect()
    }

    // ── smoke-3 bug 1: single-cluster SE must not collapse to zero ───────

    #[test]
    fn one_cluster_falls_back_to_the_clt_se_instead_of_a_fabricated_zero() {
        // Smoke 3's exact shape: 4 cases, all in one suite directory.
        let values = [0.0, 0.0, 0.0, 1.0]; // mean 0.25, like executor/haiku
        let clusters = ["hr-recruit"; 4];
        // The cluster-robust estimator is IDENTICALLY zero here — that is the
        // bug this fallback exists for, asserted so nobody "fixes" it away.
        assert_eq!(stats::se_clustered(&values, &clusters), 0.0);

        let chosen = choose_se(&values, &clusters);
        assert_eq!(chosen.source, SeSource::CltSingleCluster);
        assert_eq!(chosen.n_clusters, 1);
        let expect = stats::se_clt(&values);
        assert!(expect > 0.0, "the CLT SE is the informative one here");
        assert_eq!(chosen.se, expect);
        assert!(!chosen.is_degenerate());

        // ≥2 clusters keeps the cluster-robust estimator, unchanged.
        let two = ["a", "a", "b", "b"];
        let c2 = choose_se(&values, &two);
        assert_eq!(c2.source, SeSource::Clustered);
        assert_eq!(c2.n_clusters, 2);
        assert_eq!(c2.se, stats::se_clustered(&values, &two));

        // No observations ⇒ undefined, not zero.
        let empty = choose_se(&[], &[]);
        assert_eq!(empty.source, SeSource::Undefined);
        assert!(empty.se.is_nan());
        assert!(empty.is_degenerate());
    }

    #[test]
    fn a_zero_width_interval_only_happens_when_every_observation_is_identical() {
        // All identical ⇒ genuinely zero spread ⇒ degenerate, by design.
        let same = choose_se(&[1.0, 1.0, 1.0], &["d"; 3]);
        assert_eq!(same.se, 0.0);
        assert!(same.is_degenerate());
        // n == 1 ⇒ nothing to estimate ⇒ also degenerate.
        let one = choose_se(&[0.5], &["d"]);
        assert!(one.is_degenerate());
        // Any spread at all ⇒ a real interval.
        let spread = choose_se(&[0.0, 1.0], &["d"; 2]);
        assert!(spread.se > 0.0 && !spread.is_degenerate());
    }

    #[test]
    fn a_single_cluster_delta_gets_a_real_interval_not_a_point() {
        // haiku (mean .25) vs sonnet (mean .5) on 4 shared cases, one directory.
        let weak = scored(&[
            ("c1", "hr", 0.0),
            ("c2", "hr", 0.0),
            ("c3", "hr", 0.0),
            ("c4", "hr", 1.0),
        ]);
        let strong = scored(&[
            ("c1", "hr", 1.0),
            ("c2", "hr", 0.0),
            ("c3", "hr", 0.0),
            ("c4", "hr", 1.0),
        ]);
        let d = role_delta(MatrixRole::Executor, &weak, &strong);
        assert!(d.paired);
        assert_eq!(d.se_source, SeSource::CltSingleCluster);
        assert!((d.delta.unwrap() - 0.25).abs() < 1e-12);
        let (lo, hi) = (d.ci95_low.unwrap(), d.ci95_high.unwrap());
        assert!(
            hi - lo > 0.5,
            "the interval must have real width: [{lo},{hi}]"
        );
        assert!(
            lo < 0.0 && hi > 0.0,
            "and it must straddle zero: [{lo},{hi}]"
        );
        assert!(!d.degenerate, "a real interval is not degenerate");
    }

    #[test]
    fn a_delta_whose_every_case_moved_identically_is_degenerate_not_certain() {
        // Every diff == +1: Δ looks like a perfect +1.000 with zero width. That
        // is unestimated spread, not certainty — smoke 3's `Δ = +0.250
        // [+0.250,+0.250]` was this bug.
        let weak = scored(&[("c1", "hr", 0.0), ("c2", "hr", 0.0)]);
        let strong = scored(&[("c1", "hr", 1.0), ("c2", "hr", 1.0)]);
        let d = role_delta(MatrixRole::Executor, &weak, &strong);
        assert_eq!(d.se, Some(0.0));
        assert_eq!(d.ci95_low, d.ci95_high);
        assert!(d.degenerate);
        assert_eq!(
            d.degenerate_reason.as_deref(),
            Some(VERDICT_REASON_DEGENERATE_INTERVAL)
        );
        assert!(d.note.unwrap().contains("zero-width"));
    }

    #[test]
    fn the_bottleneck_refuses_a_degenerate_interval_however_wide_the_gap_looks() {
        let mut zero_width = delta_row("verifier", 0.90, 0.90, 0.90);
        zero_width.degenerate = true;
        zero_width.degenerate_reason = Some(VERDICT_REASON_DEGENERATE_INTERVAL.to_string());
        let out = bottleneck(&[delta_row("executor", 0.20, 0.10, 0.30), zero_width]);
        assert_eq!(out.bottleneck_role, None);
        assert_eq!(out.resolution, "unresolved");
        assert!(out.reason.contains("degenerate_interval"), "{}", out.reason);
        assert!(
            out.reason.contains("more cases or more clusters"),
            "{}",
            out.reason
        );

        // Two zero-width Δs — the literal smoke-3 output — must also refuse.
        let mut a = delta_row("executor", 0.25, 0.25, 0.25);
        a.degenerate = true;
        a.degenerate_reason = Some(VERDICT_REASON_DEGENERATE_INTERVAL.to_string());
        let mut b = delta_row("verifier", 0.0, 0.0, 0.0);
        b.degenerate = true;
        b.degenerate_reason = Some(VERDICT_REASON_DEGENERATE_INTERVAL.to_string());
        let out = bottleneck(&[a, b]);
        assert_eq!(out.resolution, "unresolved");
        assert_eq!(out.bottleneck_role, None);
    }

    #[test]
    fn the_console_line_shows_the_mean_even_when_q_is_not_computable() {
        // Smoke 3's regression: mean present, `q` null ⇒ it printed
        // "no usable observation", the opposite of the truth.
        let json = serde_json::json!({
            "mean": 0.25,
            "ci95_low": -0.17,
            "ci95_high": 0.67,
            "resolution_ratio_q": null,
            "q_note": "zero-variance interval, so the required sample size is 0",
        });
        let line = cell_console_numbers(&json);
        assert!(line.starts_with("mean=0.25"), "{line}");
        assert!(line.contains("[-0.17,0.67]"), "{line}");
        assert!(line.contains("q=n/a (zero-variance interval"), "{line}");
        assert!(!line.contains("no usable observation"), "{line}");

        let ok = serde_json::json!({
            "mean": 0.5, "ci95_low": 0.1, "ci95_high": 0.9, "resolution_ratio_q": 0.03,
        });
        assert_eq!(cell_console_numbers(&ok), "mean=0.50 [0.10,0.90] q=0.03");

        // Only a truly empty cell says nothing was measured.
        let empty = serde_json::json!({
            "mean": null, "ci95_low": null, "ci95_high": null, "resolution_ratio_q": null,
        });
        assert_eq!(cell_console_numbers(&empty), "no usable observation");
    }

    fn cell_result(domain: &str, model: &str, pairs: &[(&str, &str, f64)]) -> CellResult {
        CellResult {
            plan: CellPlan {
                domain: domain.to_string(),
                role: MatrixRole::Executor,
                model: mr(RuntimeType::Claude, model),
            },
            scored: scored(pairs),
            runs: pairs.len(),
            errors: 0,
            skipped: 0,
            substituted: 0,
            verifier_rows: Vec::new(),
            cost_usd: 0.0,
        }
    }

    /// End-to-end of bug 1 through `cell_json`: the single-cluster cell smoke 3
    /// reported as `mean 0.25 ci=[0.25,0.25]` must now carry a real interval,
    /// name the estimator, and keep the small-cluster warning.
    #[test]
    fn a_single_cluster_cell_reports_a_real_interval_and_names_the_estimator() {
        let cell = cell_result(
            "hr-recruit",
            "claude-haiku-4-5",
            &[
                ("c1", "hr-recruit", 0.0),
                ("c2", "hr-recruit", 0.0),
                ("c3", "hr-recruit", 0.0),
                ("c4", "hr-recruit", 1.0),
            ],
        );
        let (json, matrix_cell, verdict) = cell_json(&cell, 0.10);
        assert_eq!(json["n"], 4);
        assert_eq!(json["n_clusters"], 1);
        assert_eq!(json["se_source"], "clt_single_cluster");
        assert_eq!(json["small_cluster_warning"], true);
        assert_eq!(json["degenerate_interval"], false);
        let (lo, hi) = (
            json["ci95_low"].as_f64().unwrap(),
            json["ci95_high"].as_f64().unwrap(),
        );
        assert!(hi - lo > 0.5, "interval must not be a point: [{lo},{hi}]");
        // The clustered estimator's fabricated zero is still reported for
        // transparency, but it is NOT what the interval used.
        assert_eq!(json["se_clustered"].as_f64(), Some(0.0));
        assert!(json["se_used"].as_f64().unwrap() > 0.0);
        // Four cases cannot resolve a 10pp MDE.
        assert_eq!(verdict, MatrixVerdict::Unresolved);
        assert_eq!(json["verdict"], "unresolved");
        assert_eq!(matrix_cell.unwrap().n, 4);
    }

    /// The mirror case: every observation identical ⇒ zero-width interval ⇒
    /// `unresolved` with `degenerate_interval`, never a confident pass.
    #[test]
    fn an_all_identical_cell_is_unresolved_with_a_degenerate_interval() {
        let cell = cell_result(
            "d",
            "m",
            &[("c1", "d", 1.0), ("c2", "d", 1.0), ("c3", "d", 1.0)],
        );
        let (json, matrix_cell, verdict) = cell_json(&cell, 0.10);
        assert_eq!(json["mean"].as_f64(), Some(1.0));
        assert_eq!(json["degenerate_interval"], true);
        // Review finding (P2) regression: this test only ever checked the JSON
        // half, so the PERSISTED cell kept a zero-width interval and
        // `mde = 0.0` — a durable claim of certainty that
        // `role_model_matrix`'s "zero variance ⇒ absent key" rule forbids.
        let persisted = matrix_cell.expect("a cell with n > 0 is still persisted");
        assert_eq!(persisted.n, 3);
        assert_eq!(persisted.mean, 1.0);
        assert_eq!(
            (persisted.ci95_low, persisted.ci95_high, persisted.mde),
            (None, None, None),
            "a degenerate interval must be ABSENT from the stored cell, not zero-width"
        );
        // And the serialized form carries no keys at all for them.
        let stored = serde_json::to_value(&persisted).unwrap();
        assert!(stored.get("ci95_low").is_none(), "{stored}");
        assert!(stored.get("mde").is_none(), "{stored}");
        assert_eq!(verdict, MatrixVerdict::Unresolved);
        assert_eq!(json["verdict_reason"], VERDICT_REASON_DEGENERATE_INTERVAL);
        assert!(
            json["q_note"]
                .as_str()
                .unwrap()
                .contains("zero-variance interval")
        );
        // The console still shows the mean rather than claiming nothing was seen.
        assert!(cell_console_numbers(&json).starts_with("mean=1.00"));
    }

    #[test]
    fn delta_is_paired_when_the_arms_share_case_ids() {
        let weak = scored(&[("c1", "a", 0.0), ("c2", "b", 0.0), ("c3", "c", 1.0)]);
        let strong = scored(&[("c1", "a", 1.0), ("c2", "b", 1.0), ("c3", "c", 1.0)]);
        let d = role_delta(MatrixRole::Executor, &weak, &strong);
        assert!(d.paired);
        assert_eq!(d.n, 3);
        assert!((d.delta.unwrap() - 2.0 / 3.0).abs() < 1e-12);
        assert!(d.ci95_low.unwrap() < d.delta.unwrap());
        assert!(d.ci95_high.unwrap() > d.delta.unwrap());
        assert_eq!(d.note, None);
    }

    #[test]
    fn delta_degrades_to_unpaired_loudly_when_no_case_ids_overlap() {
        let weak = scored(&[("c1", "a", 0.0), ("c2", "b", 0.0)]);
        let strong = scored(&[("z1", "a", 1.0), ("z2", "b", 1.0)]);
        let d = role_delta(MatrixRole::Verifier, &weak, &strong);
        assert!(!d.paired);
        assert!((d.delta.unwrap() - 1.0).abs() < 1e-12);
        assert!(d.note.unwrap().contains("UNPAIRED"));
    }

    #[test]
    fn delta_is_absent_when_an_arm_produced_nothing() {
        let d = role_delta(MatrixRole::Executor, &[], &scored(&[("c1", "a", 1.0)]));
        assert_eq!(d.delta, None);
        assert_eq!(d.strong_mean, Some(1.0));
        assert_eq!(d.weak_mean, None);
        assert!(d.note.unwrap().contains("no usable observation"));
    }

    fn delta_row(role: &str, delta: f64, lo: f64, hi: f64) -> RoleDelta {
        RoleDelta {
            role: role.to_string(),
            weak_mean: Some(0.5),
            strong_mean: Some(0.5 + delta),
            delta: Some(delta),
            se: Some((hi - lo) / 3.92),
            se_source: SeSource::Clustered,
            ci95_low: Some(lo),
            ci95_high: Some(hi),
            paired: true,
            n: 10,
            degenerate: false,
            degenerate_reason: None,
            note: None,
        }
    }

    #[test]
    fn the_bottleneck_is_named_only_when_its_interval_excludes_the_others() {
        let resolved = bottleneck(&[
            delta_row("executor", 0.60, 0.50, 0.70),
            delta_row("verifier", 0.10, 0.00, 0.20),
        ]);
        assert_eq!(resolved.bottleneck_role.as_deref(), Some("executor"));
        assert_eq!(resolved.resolution, "resolved");
        assert!(resolved.reason.contains("excludes"));
    }

    #[test]
    fn overlapping_intervals_are_unresolved_not_a_point_estimate_ranking() {
        let out = bottleneck(&[
            delta_row("executor", 0.40, 0.10, 0.70),
            delta_row("verifier", 0.35, 0.05, 0.65),
        ]);
        assert_eq!(out.bottleneck_role, None);
        assert_eq!(out.resolution, "unresolved");
        assert!(out.reason.contains("overlaps verifier"), "{}", out.reason);
        assert!(out.reason.contains("cannot tell these roles apart"));
    }

    #[test]
    fn one_role_or_a_missing_interval_cannot_resolve_a_bottleneck() {
        let single = bottleneck(&[delta_row("executor", 0.6, 0.5, 0.7)]);
        assert_eq!(single.resolution, "unresolved");
        assert!(single.reason.contains("at least two roles"));

        let mut incomplete = delta_row("verifier", 0.5, 0.4, 0.6);
        incomplete.ci95_low = None;
        let out = bottleneck(&[delta_row("executor", 0.6, 0.5, 0.7), incomplete]);
        assert_eq!(out.resolution, "unresolved");
        assert!(out.reason.contains("at least two roles"));
    }

    #[test]
    fn a_degenerate_delta_cannot_win_a_bottleneck_comparison() {
        let mut degenerate = delta_row("verifier", 0.90, 0.80, 1.00);
        degenerate.degenerate = true;
        degenerate.degenerate_reason = Some(VERDICT_REASON_DEGENERATE_GOLD.to_string());
        // Without the exclusion this Δ (0.90) would beat the executor's 0.20
        // outright; with it, only one interpretable Δ is left.
        let out = bottleneck(&[delta_row("executor", 0.20, 0.10, 0.30), degenerate]);
        assert_eq!(out.bottleneck_role, None);
        assert_eq!(out.resolution, "unresolved");
        assert!(
            out.reason.contains("verifier:degenerate_gold"),
            "the excluded role AND its cause must be named: {}",
            out.reason
        );
        assert!(out.reason.contains("re-record"), "{}", out.reason);
    }

    #[test]
    fn the_agent_override_is_validated_and_trimmed() {
        let mut o = opts();
        assert_eq!(MatrixSpec::from_options(&o).unwrap().agent, None);
        o.agent = Some("  agnes  ".to_string());
        assert_eq!(
            MatrixSpec::from_options(&o).unwrap().agent.as_deref(),
            Some("agnes"),
            "trimmed, and carried so the header can declare it"
        );
        o.agent = Some("   ".to_string());
        assert_eq!(
            MatrixSpec::from_options(&o).unwrap().agent,
            None,
            "a blank override is no override"
        );
        o.agent = Some("../escape".to_string());
        assert!(
            MatrixSpec::from_options(&o)
                .unwrap_err()
                .contains("not a valid agent id")
        );
    }

    #[test]
    fn touching_intervals_stay_unresolved() {
        // ci95_high of the rival == ci95_low of the top: not an exclusion.
        let out = bottleneck(&[
            delta_row("executor", 0.60, 0.50, 0.70),
            delta_row("verifier", 0.30, 0.10, 0.50),
        ]);
        assert_eq!(out.resolution, "unresolved");
    }
}
