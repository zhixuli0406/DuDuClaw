//! Cross-task defaults use frozen whole policies and task-level held-out
//! evidence. Replay values are the five-beta SPEC V, never raw task scores.
use super::tree::Direction;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_WORLDS: usize = 32;
pub const MAX_CANDIDATES: usize = 8;
pub const NIGHT_WALL_SECS: u64 = 30;

/// Trusted catalog identifiers; no operator filesystem paths are persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultsNamespace {
    pub agent_id: String,
    pub scorer_name: String,
    pub scorer_hash: String,
    pub direction: Direction,
    pub approved_root_id: String,
    pub runtime: String,
    pub configured_model: String,
}
impl DefaultsNamespace {
    pub fn key(&self) -> Result<String, String> {
        for value in [
            &self.agent_id,
            &self.scorer_name,
            &self.approved_root_id,
            &self.runtime,
            &self.configured_model,
        ] {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err("invalid discovery defaults namespace".into());
            }
        }
        if self.scorer_hash.len() != 64 || !self.scorer_hash.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("missing verified scorer hash".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenDefaults {
    pub policy_id: String,
    pub source: Option<String>,
    pub source_sha256: Option<String>,
    pub beta: f64,
    pub knobs: BTreeMap<String, u32>,
    #[serde(default)]
    pub origin_task_ids: Vec<String>,
}
impl Default for FrozenDefaults {
    fn default() -> Self {
        Self {
            policy_id: super::policy::BASELINE_POLICY_ID.into(),
            source: None,
            source_sha256: None,
            beta: 0.6,
            knobs: BTreeMap::new(),
            origin_task_ids: Vec::new(),
        }
    }
}
impl FrozenDefaults {
    pub fn validate(&self) -> Result<(), String> {
        if !self.beta.is_finite()
            || !(0.0..=1.0).contains(&self.beta)
            || self.policy_id.is_empty()
            || self.policy_id.len() > 128
            || self.policy_id.chars().any(char::is_control)
        {
            return Err("invalid frozen policy parameters".into());
        }
        // SPEC exposes beta only. Grid dimensions live in the whole policy's
        // plan_grid code, bounded again by each new task's hard limits.
        if !self.knobs.is_empty() {
            return Err("unsupported policy knobs".into());
        }
        validate_task_ids(&self.origin_task_ids)?;
        match (&self.source, &self.source_sha256) {
            (None, None)
                if self.policy_id == super::policy::BASELINE_POLICY_ID
                    && self.origin_task_ids.is_empty() =>
            {
                Ok(())
            }
            (Some(source), Some(hash))
                if self.policy_id != super::policy::BASELINE_POLICY_ID
                    && !source.is_empty()
                    && source.len() <= 256 * 1024
                    && !self.origin_task_ids.is_empty()
                    && *hash == format!("{:x}", Sha256::digest(source.as_bytes())) =>
            {
                Ok(())
            }
            _ => Err("frozen source hash or development provenance missing/mismatched".into()),
        }
    }
    pub fn fingerprint(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).map_err(|e| e.to_string())?)
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionedDefaults {
    pub version: u64,
    pub bundle: FrozenDefaults,
}
impl Default for VersionedDefaults {
    fn default() -> Self {
        Self {
            version: 0,
            bundle: FrozenDefaults::default(),
        }
    }
}

/// Host-created durable reservation. Consuming a held-out task precedes its
/// first evaluation so a crash or rejected candidate cannot permit reuse.
#[derive(Debug, Clone)]
pub struct HoldoutReceipt {
    pub(crate) id: String,
    pub(crate) namespace_key: String,
    pub(crate) expected_version: u64,
    pub(crate) candidate_hash: String,
    pub(crate) task_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PairedWorld {
    pub task_id: String,
    pub run_id: String,
    pub round: u32,
    pub incumbent: f64,
    pub candidate: f64,
    pub exclusion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PromotionEvidence {
    pub worlds: usize,
    pub tasks: usize,
    pub training_tasks: usize,
    pub heldout_tasks: usize,
    pub training_mean_lift: Option<f64>,
    pub heldout_mean_lift: Option<f64>,
    pub heldout_strict_wins: usize,
    pub wilson_lower: Option<f64>,
    pub bonferroni_candidates: usize,
    pub eligible: bool,
    pub reason: String,
}

/// Assignment is frozen by task ID and never by branch, round or outcome.
pub fn is_heldout(task_id: &str) -> bool {
    Sha256::digest(task_id.as_bytes())[0] & 1 != 0
}

pub(super) fn validate_task_ids(tasks: &[String]) -> Result<(), String> {
    if tasks.len() > 256
        || tasks.iter().any(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        })
        || tasks.iter().collect::<BTreeSet<_>>().len() != tasks.len()
    {
        return Err("invalid source task identities".into());
    }
    Ok(())
}

/// Chosen B3 guard: at least 8 training and 8 distinct held-out tasks,
/// one task mean regardless of its world count, strict training improvement,
/// held-out mean lift >= .01 and one-sided Wilson/Bonferroni alpha=.05.
/// Ties are failures to improve; this is evidence on recorded worlds, not
/// a claim of causal efficacy or optimal default beta.
pub fn promotion_evidence(worlds: &[PairedWorld], candidates: usize) -> PromotionEvidence {
    use crate::prediction::calibration::{bonferroni_z, wilson_bounds};
    let mut evidence = PromotionEvidence {
        worlds: worlds.len(),
        bonferroni_candidates: candidates,
        reason: "insufficient_distinct_tasks".into(),
        ..Default::default()
    };
    if worlds.is_empty()
        || worlds.len() > MAX_WORLDS
        || candidates == 0
        || candidates > MAX_CANDIDATES
    {
        evidence.reason = "invalid_evidence_limits".into();
        return evidence;
    }
    let mut identity = BTreeSet::new();
    let mut tasks: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for world in worlds {
        if let Some(reason) = &world.exclusion {
            evidence.reason = format!("ineligible_world:{reason}");
            return evidence;
        }
        if world.task_id.is_empty()
            || !identity.insert((&world.run_id, world.round))
            || !world.incumbent.is_finite()
            || !world.candidate.is_finite()
            || !(-0.100000001..=1.000000001).contains(&world.incumbent)
            || !(-0.100000001..=1.000000001).contains(&world.candidate)
        {
            evidence.reason = "invalid_paired_world".into();
            return evidence;
        }
        tasks
            .entry(&world.task_id)
            .or_default()
            .push(world.candidate - world.incumbent);
    }
    evidence.tasks = tasks.len();
    let mut train = Vec::new();
    let mut heldout = Vec::new();
    for (task, values) in tasks {
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        if is_heldout(task) {
            heldout.push(mean);
        } else {
            train.push(mean);
        }
    }
    evidence.training_tasks = train.len();
    evidence.heldout_tasks = heldout.len();
    if train.len() < 8 || heldout.len() < 8 {
        return evidence;
    }
    let training = train.iter().sum::<f64>() / train.len() as f64;
    let test = heldout.iter().sum::<f64>() / heldout.len() as f64;
    evidence.training_mean_lift = Some(training);
    evidence.heldout_mean_lift = Some(test);
    evidence.heldout_strict_wins = heldout.iter().filter(|lift| **lift > 0.0).count();
    let z = bonferroni_z(1.6448536269514722, candidates);
    let lower = wilson_bounds(evidence.heldout_strict_wins as u64, heldout.len() as u64, z).0;
    evidence.wilson_lower = Some(lower);
    evidence.reason = if training <= 0.0 {
        "training_not_strictly_better"
    } else if test < 0.01 {
        "heldout_lift_below_minimum"
    } else if !lower.is_finite() || lower <= 0.5 {
        "heldout_wilson_not_supported"
    } else {
        evidence.eligible = true;
        "eligible_recorded_world_comparison"
    }
    .into();
    evidence
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NightReport {
    pub schema: String,
    pub agent_id: String,
    pub namespaces: Vec<NamespaceReport>,
    pub observed_worlds: usize,
    pub observed_tasks: usize,
    pub eligible_worlds: usize,
    /// Bounded selection counts are separate from the complete DB census.
    pub selected_worlds: usize,
    pub selected_tasks: usize,
    pub selection_window_tasks: usize,
    pub selection_exclusions: BTreeMap<String, usize>,
    pub cli_invocations: u32,
    pub elapsed_secs: f64,
    pub exclusions: BTreeMap<String, usize>,
    pub reason: Option<String>,
    pub zero_llm: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct NamespaceReport {
    pub namespace: DefaultsNamespace,
    pub old_default: VersionedDefaults,
    pub new_default: VersionedDefaults,
    pub candidates: usize,
    pub fresh_heldout_tasks: usize,
    pub evidence: PromotionEvidence,
    pub adopted: bool,
    pub reason: String,
}

impl NightReport {
    /// Activity and report-only rows carry hashes and counts, never policy code.
    pub fn public_view(&self) -> Self {
        let mut report = self.clone();
        for item in &mut report.namespaces {
            item.old_default.bundle.source = None;
            item.new_default.bundle.source = None;
        }
        report
    }
}

pub(in crate::discovery) fn namespace_for(
    run: &super::store::RunRecord,
) -> Option<DefaultsNamespace> {
    let namespace = DefaultsNamespace {
        agent_id: run.agent_id.clone(),
        scorer_name: run.scorer_name.clone(),
        scorer_hash: run.scorer_hash.clone(),
        direction: run.direction,
        approved_root_id: run.approved_root_id.clone()?,
        runtime: run.runtime.clone()?,
        configured_model: run.configured_model.clone()?,
    };
    namespace.key().ok()?;
    Some(namespace)
}

fn replay_bundle(
    bundle: &FrozenDefaults,
    trees: &[super::tree::WorldTree],
    runtime: &mut Option<super::policy_runner::PythonPolicyRuntime>,
    home: &std::path::Path,
    run: &str,
    budget: &super::budget::SharedBudget,
) -> Result<super::policy_runner::CandidateEvaluation, String> {
    if budget.remaining_wall().is_zero() {
        return Err("night_deadline_or_cancelled".into());
    }
    let evaluation = if let Some(source) = &bundle.source {
        if runtime.is_none() {
            let config = super::service::load_config(home)
                .map_err(|_| "policy_quota_unavailable".to_string())?;
            let quota = super::attempt_container::QuotaLimits {
                max_run_bytes: config.max_run_bytes,
                max_total_bytes: config.max_total_bytes,
            };
            *runtime = Some(
                super::policy_runner::PythonPolicyRuntime::detect_scoped_with_quota(
                    home,
                    run,
                    budget.clone(),
                    quota,
                )
                .map_err(|_| "policy_runtime_unavailable".to_string())?,
            );
        }
        super::policy_runner::evaluate_candidate_with_timeout(
            runtime.as_ref().unwrap(),
            source,
            trees,
            budget.remaining_wall(),
        )
    } else {
        let mut scores = Vec::new();
        for tree in trees {
            if budget.remaining_wall().is_zero() {
                return Err("night_deadline_or_cancelled".into());
            }
            scores.push(
                super::eval::evaluate_world(
                    &|_| Box::new(super::policy::BaselineParallelRefine),
                    tree,
                    &super::eval::ReplayConfig::for_world(tree),
                )
                .map_err(|_| "invalid_baseline_replay".to_string())?,
            );
        }
        super::policy_runner::CandidateEvaluation {
            valid: true,
            violation: None,
            value: Some(
                scores.iter().map(|score| score.pareto_reward).sum::<f64>() / scores.len() as f64,
            ),
            context_mismatch_rate: Some(0.0),
            worlds: scores,
        }
    };
    Ok(evaluation)
}
fn validate_replay(
    evaluation: &super::policy_runner::CandidateEvaluation,
    worlds: usize,
) -> Result<(), String> {
    if !evaluation.valid || evaluation.worlds.len() != worlds {
        return Err("invalid_policy_replay".into());
    }
    if evaluation.context_mismatch_rate != Some(0.0)
        || evaluation.worlds.iter().any(|score| {
            score
                .points
                .iter()
                .any(|point| point.context_mismatch_rate != 0.0)
        })
    {
        return Err("context_mismatch".into());
    }
    if evaluation.worlds.iter().any(|score| score.out_of_support) {
        return Err("out_of_support".into());
    }
    if evaluation
        .worlds
        .iter()
        .any(|score| !score.pareto_reward.is_finite())
    {
        return Err("nonfinite_replay".into());
    }
    if evaluation.worlds.iter().any(|score| {
        score.points.len() != super::score::BETA_GRID.len()
            || score
                .points
                .iter()
                .zip(super::score::BETA_GRID)
                .any(|(point, beta)| point.beta != beta)
    }) {
        return Err("invalid_beta_sweep".into());
    }
    Ok(())
}

fn pair_values(
    worlds: &[(&str, &super::tree::WorldTree)],
    incumbent: &super::policy_runner::CandidateEvaluation,
    candidate: &super::policy_runner::CandidateEvaluation,
) -> Result<Vec<PairedWorld>, String> {
    worlds
        .iter()
        .map(|(task, tree)| {
            let world = tree.world();
            let find = |evaluation: &super::policy_runner::CandidateEvaluation| {
                evaluation
                    .worlds
                    .iter()
                    .find(|score| score.run_id == world.run_id && score.round == world.round)
                    .map(|score| score.pareto_reward)
                    .ok_or_else(|| "replay_world_missing".to_string())
            };
            Ok(PairedWorld {
                task_id: (*task).into(),
                run_id: world.run_id.clone(),
                round: world.round,
                incumbent: find(incumbent)?,
                candidate: find(candidate)?,
                exclusion: None,
            })
        })
        .collect()
}
fn task_mean_lift(worlds: &[PairedWorld]) -> f64 {
    let mut tasks: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
    for world in worlds {
        let value = tasks.entry(&world.task_id).or_default();
        value.0 += world.candidate - world.incumbent;
        value.1 += 1;
    }
    tasks
        .values()
        .map(|(sum, count)| sum / (*count as f64))
        .sum::<f64>()
        / tasks.len() as f64
}

/// No provider interface is accepted. All subprocesses share the wall deadline
/// and operator cancellation; no CLI budget reservation is ever made.
pub fn run_zero_llm(home: &std::path::Path, agent_id: &str) -> Option<NightReport> {
    run_with_budget(home, agent_id, night_budget())
}
pub(crate) fn night_budget() -> super::budget::SharedBudget {
    super::budget::SharedBudget::new(super::contracts::RunBudget {
        max_agent_calls: 1,
        max_usd: 1.0,
        max_wall_secs: NIGHT_WALL_SECS,
        max_rounds: 1,
    })
    .expect("fixed positive night resource limits")
}
pub(crate) fn run_with_budget(
    home: &std::path::Path,
    agent_id: &str,
    budget: super::budget::SharedBudget,
) -> Option<NightReport> {
    if !home.join("discovery.db").is_file() {
        return None;
    }
    let started = std::time::Instant::now();
    let mut report = NightReport {
        schema: "duduclaw.discovery.night.v1".into(),
        agent_id: agent_id.into(),
        zero_llm: true,
        ..Default::default()
    };
    let work = (|| -> Result<(), String> {
        let store =
            super::store::DiscoveryStore::open(home).map_err(|_| "discovery_store_unavailable")?;
        let snapshot = store
            .night_snapshot(agent_id)
            .map_err(|_| "discovery_snapshot_unavailable")?;
        report.observed_worlds = snapshot.observed_worlds;
        report.observed_tasks = snapshot.observed_tasks;
        if snapshot.observed_worlds == 0 {
            return Err("no_completed_worlds".into());
        }
        report.selected_worlds = snapshot.worlds.len();
        report.selected_tasks = snapshot
            .worlds
            .iter()
            .filter_map(|world| world.run.task_id.as_deref())
            .collect::<BTreeSet<_>>()
            .len();
        report.selection_window_tasks = snapshot.window_tasks;
        report.selection_exclusions = snapshot.selection_exclusions.clone();
        if snapshot.worlds.is_empty() {
            return Err("insufficient_fresh_complete_task_window".into());
        }
        let mut excluded_tasks = BTreeSet::new();
        for world in &snapshot.worlds {
            if let Some(reason) = &world.exclusion {
                *report.exclusions.entry(reason.clone()).or_default() += 1;
                if let Some(task) = &world.run.task_id {
                    excluded_tasks.insert(task.clone());
                }
            }
        }
        let mut groups: BTreeMap<
            String,
            (DefaultsNamespace, Vec<(&str, &super::tree::WorldTree)>),
        > = BTreeMap::new();
        for world in &snapshot.worlds {
            let Some(task) = world.run.task_id.as_deref() else {
                continue;
            };
            if excluded_tasks.contains(task) {
                continue;
            }
            let Some(tree) = &world.tree else { continue };
            let Some(namespace) = namespace_for(&world.run) else {
                *report
                    .exclusions
                    .entry("provenance_unknown".into())
                    .or_default() += 1;
                continue;
            };
            if validate_task_ids(&[task.to_string()]).is_err() {
                *report
                    .exclusions
                    .entry("invalid_task_identity".into())
                    .or_default() += 1;
                continue;
            }
            groups
                .entry(namespace.key()?)
                .or_insert_with(|| (namespace, Vec::new()))
                .1
                .push((task, tree));
            report.eligible_worlds += 1;
        }
        if groups.is_empty() {
            return Err("no_eligible_complete_worlds".into());
        }
        let guard = super::maintenance::OperatorLeaseGuard::acquire(home)
            .map_err(|_| "operator_authority_unavailable")?;
        guard.bind_budget(budget.clone())?;
        let run = format!("night-{}", uuid::Uuid::new_v4());
        let mut runtime = None;
        let mut remaining_candidates = MAX_CANDIDATES;
        for (_, (namespace, worlds)) in groups {
            let old = store
                .load_discovery_default(&namespace)
                .map_err(|_| "default_unavailable")?;
            let mut item = NamespaceReport {
                namespace: namespace.clone(),
                old_default: old.clone(),
                new_default: old.clone(),
                candidates: 0,
                fresh_heldout_tasks: 0,
                evidence: PromotionEvidence::default(),
                adopted: false,
                reason: String::new(),
            };
            let result = (|| -> Result<(), String> {
                if budget.remaining_wall().is_zero() {
                    return Err("night_deadline_or_cancelled".into());
                }
                guard.check()?;
                let training = worlds
                    .iter()
                    .copied()
                    .filter(|(task, _)| !is_heldout(task))
                    .collect::<Vec<_>>();
                let heldout = worlds
                    .iter()
                    .copied()
                    .filter(|(task, _)| is_heldout(task))
                    .collect::<Vec<_>>();
                let training_tasks = training
                    .iter()
                    .map(|(task, _)| *task)
                    .collect::<BTreeSet<_>>();
                let heldout_tasks = heldout
                    .iter()
                    .map(|(task, _)| task.to_string())
                    .collect::<BTreeSet<_>>();
                item.evidence.worlds = worlds.len();
                item.evidence.tasks = training_tasks.len() + heldout_tasks.len();
                item.evidence.training_tasks = training_tasks.len();
                item.evidence.heldout_tasks = heldout_tasks.len();
                if training_tasks.len() < 8 || heldout_tasks.len() < 8 {
                    return Err("insufficient_distinct_tasks".into());
                }
                let mut candidates = store
                    .night_candidates(&namespace)
                    .map_err(|_| "candidate_query_unavailable")?;
                // Incumbent provenance is never inferred from the replay trees.
                if old.bundle.source.is_some()
                    && !candidates.iter().any(|candidate| candidate == &old.bundle)
                {
                    return Err("incumbent_development_provenance_unknown".into());
                }
                candidates.retain(|candidate| candidate != &old.bundle);
                // Builtin is a legitimate challenger to a learned incumbent.
                if old.bundle.source.is_some()
                    && !candidates
                        .iter()
                        .any(|candidate| candidate.source.is_none())
                {
                    candidates.push(FrozenDefaults::default());
                }
                if candidates.is_empty() {
                    return Err("no_training_developed_candidates".into());
                }
                if candidates.len() > remaining_candidates {
                    return Err("candidate_limit_exceeded_report_only".into());
                }
                item.candidates = candidates.len();
                remaining_candidates -= candidates.len();
                let fresh = store
                    .fresh_night_tasks(&namespace, &heldout_tasks.into_iter().collect::<Vec<_>>())
                    .map_err(|_| "heldout_history_unavailable")?;
                item.fresh_heldout_tasks = fresh.len();
                if fresh.len() < 8 {
                    return Err("insufficient_fresh_heldout_tasks".into());
                }
                let heldout = heldout
                    .into_iter()
                    .filter(|(task, _)| fresh.iter().any(|fresh| fresh == task))
                    .collect::<Vec<_>>();
                let training_trees = training
                    .iter()
                    .map(|(_, tree)| (*tree).clone())
                    .collect::<Vec<_>>();
                let incumbent = replay_bundle(
                    &old.bundle,
                    &training_trees,
                    &mut runtime,
                    home,
                    &run,
                    &budget,
                )?;
                validate_replay(&incumbent, training_trees.len())?;
                let mut winner = None;
                let mut best_lift = 0.0;
                for candidate in candidates {
                    let evaluation = replay_bundle(
                        &candidate,
                        &training_trees,
                        &mut runtime,
                        home,
                        &run,
                        &budget,
                    )?;
                    store
                        .record_candidate_evaluation_with_origin(
                            &candidate.policy_id,
                            &serde_json::to_string(&candidate).map_err(|_| "candidate_encode")?,
                            &training_trees,
                            &evaluation,
                            &candidate.origin_task_ids,
                            "night_replay",
                        )
                        .map_err(|_| "replay_ledger_unavailable")?;
                    validate_replay(&evaluation, training_trees.len())?;
                    let pairs = pair_values(&training, &incumbent, &evaluation)?;
                    let lift = task_mean_lift(&pairs);
                    if lift > best_lift {
                        best_lift = lift;
                        winner = Some((candidate, pairs));
                    }
                }
                let Some((candidate, mut pairs)) = winner else {
                    return Err("training_not_strictly_better".into());
                };
                item.evidence.training_mean_lift = Some(best_lift);
                guard.check()?;
                if budget.remaining_wall().is_zero() {
                    return Err("night_deadline_or_cancelled".into());
                }
                let receipt = store
                    .claim_night_holdout(&namespace, old.version, &candidate, &fresh)
                    .map_err(|_| "heldout_claim_unavailable")?
                    .ok_or("heldout_or_default_changed")?;
                let heldout_trees = heldout
                    .iter()
                    .map(|(_, tree)| (*tree).clone())
                    .collect::<Vec<_>>();
                let incumbent = replay_bundle(
                    &old.bundle,
                    &heldout_trees,
                    &mut runtime,
                    home,
                    &run,
                    &budget,
                )?;
                let evaluation = replay_bundle(
                    &candidate,
                    &heldout_trees,
                    &mut runtime,
                    home,
                    &run,
                    &budget,
                )?;
                store
                    .record_candidate_evaluation_with_origin(
                        &candidate.policy_id,
                        &serde_json::to_string(&candidate).map_err(|_| "candidate_encode")?,
                        &heldout_trees,
                        &evaluation,
                        &candidate.origin_task_ids,
                        "night_replay",
                    )
                    .map_err(|_| "replay_ledger_unavailable")?;
                validate_replay(&incumbent, heldout_trees.len())?;
                validate_replay(&evaluation, heldout_trees.len())?;
                pairs.extend(pair_values(&heldout, &incumbent, &evaluation)?);
                item.evidence = promotion_evidence(&pairs, item.candidates);
                guard.check()?;
                if budget.remaining_wall().is_zero() {
                    return Err("night_deadline_or_cancelled".into());
                }
                let changed = store
                    .finish_night_holdout(&namespace, &receipt, &candidate, &item.evidence, || {
                        !budget.remaining_wall().is_zero() && guard.check().is_ok()
                    })
                    .map_err(|_| "default_commit_unavailable")?;
                if let Some(new) = changed {
                    item.new_default = new;
                    item.adopted = true;
                }
                item.reason = if item.evidence.eligible && !item.adopted {
                    "default_compare_and_swap_conflict".into()
                } else {
                    item.evidence.reason.clone()
                };
                Ok(())
            })();
            if let Err(reason) = result {
                item.reason = reason;
                item.evidence.eligible = false;
                item.evidence.reason = item.reason.clone();
            }
            report.namespaces.push(item);
        }
        Ok(())
    })();
    if let Err(reason) = work {
        report.reason = Some(reason);
    }
    report.cli_invocations = budget.snapshot().agent_calls;
    report.elapsed_secs = started.elapsed().as_secs_f64();
    // Persistent reports cannot masquerade as deployments: source is omitted
    // here while the authoritative version table retains its frozen bundle.
    if let Ok(store) = super::store::DiscoveryStore::open(home) {
        let _ = store.save_night_report(&report);
    }
    Some(report.public_view())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task_ids(heldout: bool, count: usize) -> Vec<String> {
        (0..10000)
            .map(|i| format!("task-{i}"))
            .filter(|id| is_heldout(id) == heldout)
            .take(count)
            .collect()
    }
    fn paired(task: &str, round: u32, lift: f64) -> PairedWorld {
        PairedWorld {
            task_id: task.into(),
            run_id: task.into(),
            round,
            incumbent: 0.1,
            candidate: 0.1 + lift,
            exclusion: None,
        }
    }
    fn balanced(train_lift: f64, test_lift: f64) -> Vec<PairedWorld> {
        task_ids(false, 8)
            .into_iter()
            .map(|task| paired(&task, 1, train_lift))
            .chain(
                task_ids(true, 8)
                    .into_iter()
                    .map(|task| paired(&task, 1, test_lift)),
            )
            .collect()
    }
    #[test]
    fn one_task_many_worlds_never_counts_as_independent_heldout_evidence() {
        let worlds = (1..=32)
            .map(|round| paired("same-task", round, 0.2))
            .collect::<Vec<_>>();
        assert!(!promotion_evidence(&worlds, 1).eligible);
    }
    #[test]
    fn training_winner_that_loses_heldout_never_updates_defaults() {
        assert!(!promotion_evidence(&balanced(0.3, -0.05), 1).eligible);
    }
    #[test]
    fn heldout_ties_keep_the_incumbent() {
        assert!(!promotion_evidence(&balanced(0.3, 0.0), 1).eligible);
    }
    #[test]
    fn excluded_or_nonfinite_worlds_cannot_be_cherry_picked_for_promotion() {
        for reason in [
            "unconfined",
            "imported",
            "partial",
            "sparse",
            "provenance_unknown",
            "contamination",
            "context_mismatch",
            "out_of_support",
        ] {
            let mut worlds = balanced(0.3, 0.3);
            worlds[0].exclusion = Some(reason.into());
            assert!(
                !promotion_evidence(&worlds, 1).eligible,
                "accepted {reason}"
            );
        }
        let mut worlds = balanced(0.3, 0.3);
        worlds[0].candidate = f64::INFINITY;
        assert!(!promotion_evidence(&worlds, 1).eligible);
    }
    #[test]
    fn each_task_has_equal_weight_even_with_many_successful_worlds() {
        let train = task_ids(false, 8);
        let mut worlds = (1..=16)
            .map(|round| paired(&train[0], round, 0.8))
            .collect::<Vec<_>>();
        worlds.extend(train[1..].iter().map(|task| paired(task, 1, -0.2)));
        worlds.extend(task_ids(true, 8).iter().map(|task| paired(task, 1, 0.2)));
        assert!(!promotion_evidence(&worlds, 1).eligible);
    }
    #[test]
    fn sufficient_strict_task_wins_report_correct_heldout_denominator() {
        let evidence = promotion_evidence(&balanced(0.2, 0.2), 8);
        assert!(evidence.eligible);
        assert_eq!(evidence.training_tasks, 8);
        assert_eq!(evidence.heldout_tasks, 8);
        assert_eq!(evidence.heldout_strict_wins, 8);
        assert!(evidence.wilson_lower.unwrap() > 0.5);
    }
}
