//! Between-round policy development. Development calls share the exploration
//! runner's budget, while policy replay receives neither budget nor tool data.
use std::path::{Path, PathBuf};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::budget::SharedBudget;
use super::contracts::{AttemptInfraError, AttemptRequest, AttemptRunner, PolicySource};
use super::policy::{BaselineParallelRefine, PolicyConfig};
use super::policy_runner::{BASELINE_SOURCE, CandidateEvaluation, ManagedPolicySource,
    evaluate_candidate_with_timeout};
use super::score::{BETA_GRID, round9};
use super::tree::{NodeCost, WorldTree};

const DEV_PROMPT: &str = include_str!("python/policy_dev_prompt.txt");
const MAX_SOURCE: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub struct DreamRequest {
    pub home_dir: PathBuf,
    pub run_dir: PathBuf,
    pub run_id: String,
    pub round: u32,
    pub agent_id: String,
    pub model: Option<String>,
    pub account_pool: Vec<String>,
    /// Number of revisions after this round; the operator default is three.
    pub versions: u32,
    pub max_turns: u32,
    pub timeout: Duration,
    pub round_probe_cap: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamCandidate {
    pub version: u32,
    pub source_sha256: String,
    pub evaluation: CandidateEvaluation,
    pub cost: Option<NodeCost>,
    pub default_beta: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamAudit {
    pub schema: String,
    pub at: String,
    pub run_id: String,
    pub after_round: u32,
    pub incumbent_hash: String,
    pub selected_hash: String,
    pub selected_version: u32,
    pub changed: bool,
    pub candidates: Vec<DreamCandidate>,
    pub skipped_reason: Option<String>,
}
fn hash(source: &str) -> String { format!("{:x}", Sha256::digest(source.as_bytes())) }
fn invalid(reason: impl Into<String>) -> CandidateEvaluation {
    CandidateEvaluation { valid: false, violation: Some(reason.into()), value: None,
        worlds: vec![], context_mismatch_rate: None }
}
fn evaluate_baseline(worlds: &[WorldTree]) -> CandidateEvaluation {
    let result = worlds.iter().map(|tree| super::eval::evaluate_world(
        &|cfg: &PolicyConfig| Box::new(BaselineParallelRefine::new(cfg)), tree,
        &super::eval::ReplayConfig::for_world(tree))).collect::<Result<Vec<_>, _>>();
    match result {
        Err(e) => invalid(e.to_string()),
        Ok(scores) if scores.is_empty() => invalid("no_completed_worlds"),
        Ok(scores) => {
            let value = round9(scores.iter().map(|w| w.pareto_reward).sum::<f64>() / scores.len() as f64);
            let mismatch = round9(scores.iter().flat_map(|w| &w.points)
                .map(|p| p.context_mismatch_rate).sum::<f64>() / (scores.len() * BETA_GRID.len()) as f64);
            CandidateEvaluation { valid: true, violation: None, value: Some(value),
                worlds: scores, context_mismatch_rate: Some(mismatch) }
        }
    }
}
/// Index zero is the incumbent. Strict improvement preserves it on every tie.
pub fn select_candidate(candidates: &[CandidateEvaluation]) -> usize {
    let mut best = 0;
    let mut best_value = candidates.first().filter(|c| c.valid)
        .and_then(|c| c.value).filter(|v| v.is_finite());
    for (i, candidate) in candidates.iter().enumerate().skip(1) {
        if let Some(value) = candidate.value.filter(|v| candidate.valid && v.is_finite()) {
            if best_value.is_none_or(|current| value > current) { best = i; best_value = Some(value); }
        }
    }
    best
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("missing artifact parent")?;
    super::workspace::create_private_directory(parent).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    use std::io::Write;
    temp.write_all(&bytes).map_err(|e| e.to_string())?;
    temp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
fn archive(root: &Path, candidate: &DreamCandidate, source: &str) -> Result<(), String> {
    let dir = root.join(format!("r{:04}_{}", candidate.version,
        duduclaw_core::truncate_bytes(&candidate.source_sha256, 16)));
    super::workspace::create_private_directory(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("method.py"), source).map_err(|e| e.to_string())?;
    write_json(&dir.join("proposal_results/beta_sweep.json"), &candidate.evaluation)?;
    let traces = candidate.evaluation.worlds.iter().flat_map(|w| w.points.iter().map(|p|
        serde_json::json!({"run_id":w.run_id,"round":w.round,"point":p})))
        .map(|row| serde_json::to_string(&row).unwrap()).collect::<Vec<_>>().join("\n");
    std::fs::write(dir.join("proposal_results/policy_execution_traces.jsonl"), traces)
        .map_err(|e| e.to_string())
}
fn prompt(method: &Path, developer: &Path, cap: u32) -> String {
    DEV_PROMPT.replace("$method_file", &method.to_string_lossy())
        .replace("$history_dir", &developer.join("history").to_string_lossy())
        .replace("$trace_pool", &developer.join("trace_pool").to_string_lossy())
        .replace("$round_probe_cap", &cap.to_string())
        .replace("$check_cmd", "The host performs the isolated replay after this call. No checker is exposed here.")
        .replace("opened_branch_count", "actual_branch_count")
        .replace("max_attempt_reached", "actual_refine_count")
}
fn copy_history(from: &Path, to: &Path) -> Result<(), String> {
    if !from.exists() { return Ok(()); }
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if file_type.is_symlink() { return Err("symlink in policy history".into()); }
        if file_type.is_dir() {
            super::workspace::create_private_directory(&target).map_err(|e| e.to_string())?;
            copy_history(&entry.path(), &target)?;
        } else if file_type.is_file() {
            let name = entry.file_name();
            if !matches!(name.to_str(), Some("method.py" | "beta_sweep.json" | "policy_execution_traces.jsonl")) {
                return Err("unexpected file in policy history".into());
            }
            if entry.metadata().map_err(|e| e.to_string())?.len() > 8 * 1024 * 1024 {
                return Err("policy history artifact too large".into());
            }
            std::fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        } else { return Err("nonregular file in policy history".into()); }
    }
    Ok(())
}
fn exhausted(budget: &SharedBudget) -> bool {
    let spent = budget.snapshot();
    let limits = budget.limits();
    spent.agent_calls >= limits.max_agent_calls || spent.spent_usd >= limits.max_usd
        || budget.remaining_wall().is_zero()
}
fn read_method(path: &Path) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("method_unreadable:{e}"))?;
    if !meta.file_type().is_file() || meta.len() > MAX_SOURCE {
        return Err("method_not_regular_or_too_large".into());
    }
    std::fs::read_to_string(path).map_err(|e| format!("method_unreadable:{e}"))
}

/// The injected runner owns call reservation and infrastructure retries. Dream
/// never reserves the same call twice. Replay remains synchronous so dropping
/// this future cannot detach a policy evaluation onto a background task.
pub async fn dream(source: &ManagedPolicySource, runner: &dyn AttemptRunner,
    budget: &SharedBudget, worlds: &[WorldTree], req: &DreamRequest) -> Result<DreamAudit, String> {
    let mut rounds = worlds.iter().map(|w| w.world().round).collect::<Vec<_>>();
    rounds.sort_unstable();
    if req.round == 0 || rounds != (1..=req.round).collect::<Vec<_>>() {
        return Err("dream requires every completed round exactly once".into());
    }
    let runtime = source.runtime();
    let incumbent_source = source.current_source();
    let initial = incumbent_source.as_deref().unwrap_or(BASELINE_SOURCE).to_string();
    let incumbent = match (&runtime, &incumbent_source) {
        (Some(runtime), Some(code)) => evaluate_candidate_with_timeout(runtime, code, worlds, budget.remaining_wall()),
        _ => evaluate_baseline(worlds),
    };
    let initial_hash = hash(&initial);
    let mut candidates = vec![DreamCandidate { version: 0, source_sha256: initial_hash.clone(),
        evaluation: incumbent, cost: None, default_beta: Some(worlds.last().unwrap().world().beta) }];
    let mut sources = vec![initial.clone()];
    let mut skipped_reason = source.degraded().map(|e| e.to_string());
    if !candidates[0].evaluation.valid {
        let reason = candidates[0].evaluation.violation.clone().unwrap_or("invalid incumbent".into());
        source.degrade(super::contracts::PolicyDegraded::Rejected(reason.clone()));
        skipped_reason = Some(reason);
        candidates.push(DreamCandidate { version: 0, source_sha256: hash(BASELINE_SOURCE),
            evaluation: evaluate_baseline(worlds), cost: None, default_beta: Some(0.6) });
        sources.push(BASELINE_SOURCE.to_string());
    }

    // This root contains only policy-development inputs, never exploration
    // workspaces, budget files, evaluator code, credentials or agent prompts.
    let developer = super::attempt_container::create_policy_development_workspace(&req.home_dir, &req.run_id)
        .map_err(|e| e.to_string())?;
    let developer_root = developer.path().canonicalize().map_err(|e| e.to_string())?;
    let history = developer_root.join("history");
    super::workspace::create_private_directory(&history).map_err(|e| e.to_string())?;
    let archive_root = req.run_dir.join("policy_history");
    copy_history(&archive_root, &history)?;
    archive(&history, &candidates[0], &initial)?;
    let baseline = DreamCandidate { version: 0, source_sha256: hash(BASELINE_SOURCE),
        evaluation: evaluate_baseline(worlds), cost: None, default_beta: Some(0.6) };
    let baseline_dir = history.join("baseline");
    super::workspace::create_private_directory(&baseline_dir).map_err(|e| e.to_string())?;
    std::fs::write(baseline_dir.join("method.py"), BASELINE_SOURCE).map_err(|e| e.to_string())?;
    write_json(&baseline_dir.join("proposal_results/beta_sweep.json"), &baseline.evaluation)?;
    for tree in worlds {
        write_json(&developer_root.join(format!("trace_pool/iter{}/live_cycle_manifest.json", tree.world().round)),
            &serde_json::json!({"round":tree.world().round,"beta":tree.world().beta,
                "policy_id":tree.world().policy_id,"planned_branch_count":tree.world().branch_count,
                "planned_refine_count":tree.world().refine_count,
                "best_score":tree.nodes().iter().filter_map(|n| n.valid_score())
                    .max_by(|a,b| tree.world().direction.orient(*a).total_cmp(&tree.world().direction.orient(*b)))}))?;
    }
    archive(&archive_root, &candidates[0], &initial)?;
    let mut previous = initial;
    if skipped_reason.is_none() {
        if let Some(runtime) = runtime {
            for revision in 1..=req.versions {
                if exhausted(budget) { skipped_reason = Some("budget_exhausted".into()); break; }
                let node = developer_root.join(format!("a{revision}/ws"));
                super::workspace::create_private_directory(&node).map_err(|e| e.to_string())?;
                let method = node.join("method.py");
                std::fs::write(&method, &previous).map_err(|e| e.to_string())?;
                let node_history = node.join("history");
                super::workspace::create_private_directory(&node_history).map_err(|e| e.to_string())?;
                copy_history(&history, &node_history)?;
                for tree in worlds {
                    let filename = format!("iter{}/live_cycle_manifest.json", tree.world().round);
                    let original = developer_root.join("trace_pool").join(&filename);
                    let target = node.join("trace_pool").join(filename);
                    super::workspace::create_private_directory(target.parent().unwrap()).map_err(|e| e.to_string())?;
                    std::fs::copy(original, target).map_err(|e| e.to_string())?;
                }
                let request = AttemptRequest { run_id: req.run_id.clone(),
                    cell_id: format!("policy-r{}-v{revision}", req.round), node_dir: node.clone(),
                    run_dir: developer_root.clone(), prompt: prompt(&method, &node, req.round_probe_cap),
                    agent_id: req.agent_id.clone(), model: req.model.clone(), timeout: req.timeout.min(budget.remaining_wall()),
                    max_turns: req.max_turns, account_pool: req.account_pool.clone(), read_workspaces: Vec::new() };
                let outcome = match runner.run_attempt(&request).await {
                    Ok(outcome) => outcome,
                    Err(AttemptInfraError::BudgetExhausted) => { skipped_reason = Some("budget_exhausted".into()); break; }
                    Err(error) => {
                        candidates.push(DreamCandidate { version: revision, source_sha256: hash(&previous),
                            evaluation: invalid(format!("infrastructure:{error}")), cost: None, default_beta: None });
                        sources.push(previous.clone());
                        continue;
                    }
                };
                let (code, evaluation) = match read_method(&method) {
                    Ok(code) if !outcome.timed_out => {
                        let evaluation = evaluate_candidate_with_timeout(&runtime, &code, worlds, budget.remaining_wall());
                        (code, evaluation)
                    }
                    Ok(code) => (code, invalid("policy_development_timeout")),
                    Err(reason) => (previous.clone(), invalid(reason)),
                };
                let (evaluation, default_beta) = if evaluation.valid {
                    match runtime.default_beta(&code, budget.remaining_wall()) {
                        Ok(beta) => (evaluation, Some(beta)),
                        Err(reason) => (invalid(reason.to_string()), None),
                    }
                } else { (evaluation, None) };
                let candidate = DreamCandidate { version: revision, source_sha256: hash(&code),
                    evaluation, cost: Some(outcome.cost), default_beta };
                archive(&archive_root, &candidate, &code)?;
                archive(&history, &candidate, &code)?;
                if candidate.evaluation.valid { previous = code.clone(); }
                candidates.push(candidate);
                sources.push(code);
            }
        } else { skipped_reason = Some("no_policy_runtime".into()); }
    }
    let selected = select_candidate(&candidates.iter().map(|c| c.evaluation.clone()).collect::<Vec<_>>());
    let store = super::store::DiscoveryStore::open(&req.home_dir).map_err(|e| e.to_string())?;
    let task_id = store.load_run(&req.run_id).map_err(|e|e.to_string())?.and_then(|run|run.task_id);
    let incumbent_origins = source.origin_task_ids();
    let candidate_origins = |candidate: &DreamCandidate, code: &str| {
        if code == BASELINE_SOURCE { return Vec::new(); }
        let mut origins = incumbent_origins.clone();
        if candidate.version > 0 {
            if let Some(task) = &task_id { origins.push(task.clone()); }
        }
        origins.sort(); origins.dedup(); origins
    };
    for (candidate, code) in candidates.iter().zip(&sources) {
        let origins = candidate_origins(candidate, code);
        let origin = if code == BASELINE_SOURCE { "builtin" }
            else if candidate.version == 0 { "incumbent" } else { "task_development" };
        let canonical_id=if code == BASELINE_SOURCE {super::policy::BASELINE_POLICY_ID.to_string()}
            else {format!("llm-{}",&candidate.source_sha256[..16])};
        let occurrence=format!("{}:r{}:v{}:{}",req.run_id,req.round,candidate.version,candidate.source_sha256);
        let params = serde_json::to_string(&serde_json::json!({
            "schema":"duduclaw.discovery.candidate.v1", "source":code,
            "source_sha256":candidate.source_sha256, "revision":candidate.version,
            "after_round":req.round, "development_run_id":req.run_id,
            "policy_id":canonical_id,"occurrence_id":occurrence,
            "beta":candidate.default_beta, "knobs":{}, "origin_task_ids":origins,
        })).map_err(|e|e.to_string())?;
        store.record_candidate_evaluation_with_occurrence(&canonical_id, Some(&occurrence), &params, worlds, &candidate.evaluation, &origins, origin)
            .map_err(|e|e.to_string())?;
    }
    let winner = &candidates[selected];
    let changed = winner.source_sha256 != initial_hash;
    let selected_beta = winner.default_beta.unwrap_or(worlds.last().unwrap().world().beta);
    let selected_origins = candidate_origins(winner, &sources[selected]);
    let audit = DreamAudit { schema: "duduclaw.discovery.dream_audit.v1".into(),
        at: chrono::Utc::now().to_rfc3339(), run_id: req.run_id.clone(), after_round: req.round,
        incumbent_hash: initial_hash, selected_hash: winner.source_sha256.clone(),
        selected_version: winner.version, changed, candidates, skipped_reason };
    let policy_dir = req.run_dir.join("policy");
    super::workspace::create_private_directory(&policy_dir).map_err(|e| e.to_string())?;
    let filename = format!("{}.py", duduclaw_core::truncate_bytes(&audit.selected_hash, 16));
    std::fs::write(policy_dir.join(&filename), &sources[selected]).map_err(|e| e.to_string())?;
    write_json(&policy_dir.join("current.json"), &serde_json::json!({"file":filename,
        "hash":audit.selected_hash,"selected_after_round":req.round}))?;
    write_json(&req.run_dir.join(format!("dream_after_r{}/selection.json", req.round)), &audit)?;
    use std::io::Write;
    let mut log = std::fs::OpenOptions::new().create(true).append(true)
        .open(req.run_dir.join("dream_audit.jsonl")).map_err(|e| e.to_string())?;
    writeln!(log, "{}", serde_json::to_string(&audit).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    crate::security_autopilot::audit_and_emit(&req.home_dir,
        &duduclaw_security::audit::AuditEvent::new("discovery_policy_changed", &req.agent_id,
            duduclaw_security::audit::Severity::Info, serde_json::to_value(&audit).map_err(|e| e.to_string())?));
    if changed && (sources[selected] != BASELINE_SOURCE || source.degraded().is_none()) {
        source.install_validated_deployment(&sources[selected], selected_beta, &selected_origins);
    }
    Ok(audit)
}
