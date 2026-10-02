//! Cross-validation against the Python implementation's fixtures (SPEC §9)
//! and `discovery.db` round-trip tests.
//!
//! Fixtures live in `tests/fixtures/discovery/<name>/{world,tree}.{json,jsonl}`
//! plus `expected.json`, and are read at test runtime (not `include_str!`),
//! so this file compiles before they exist.
//!
//! Assumed `expected.json` shape (field aliases accepted where noted):
//!
//! ```json
//! {
//!   "pareto_auc": 0.0,
//!   "pareto_reward": 0.0,
//!   "parallel_penalty": 0.0,          // optional: mean over the five points
//!   "out_of_support": false,          // optional
//!   "points": [                       // alias "betas" / "sweep"; array, or
//!     {                               // object keyed by the beta as a string
//!       "beta": 0.0,                  // optional: else matched by index
//!       "probes": 9,
//!       "decision_rounds": 6,
//!       "effective_sequential_rounds": 6,
//!       "attainment": 1.0,
//!       "work": 1.0,
//!       "parallel_penalty": 0.666666667,
//!       "context_mismatch_rate": 0.0,
//!       "batches": [["r1-b0-a0", "r1-b1-a0"], ["r1-b2-a0"]]
//!                                     // alias "probe_sequence" /
//!                                     // "probe_batches" / "sequence"
//!     }
//!   ]
//! }
//! ```

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::eval::{EvalError, ReplayConfig, evaluate_world};
use super::policy::{
    BaselineParallelRefine, ExplorationPolicy, GridContext, GridPlan, PolicyConfig, PolicyError,
    Question,
};
use super::score::{BETA_GRID, PointScore, WorldScore, round9};
use super::store::{DiscoveryStore, PolicyEvalRow};
use super::tests::{full_grid, node, world};
use super::tree::{NodeCost, WorldTree};

fn fixtures_root() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/discovery/"
    ))
}

fn first_key<'a>(obj: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| obj.get(*k))
}

/// Expected points as `(optional beta, point object)`.
fn expected_points(expected: &Value) -> Vec<(Option<f64>, Value)> {
    match first_key(expected, &["points", "betas", "sweep"]) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|p| (p.get("beta").and_then(Value::as_f64), p.clone()))
            .collect(),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, p)| {
                let beta = p
                    .get("beta")
                    .and_then(Value::as_f64)
                    .or_else(|| k.parse::<f64>().ok());
                (beta, p.clone())
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn cmp_f64(errs: &mut Vec<String>, ctx: &str, field: &str, expected: Option<&Value>, actual: f64) {
    match expected.and_then(Value::as_f64) {
        Some(e) if round9(e) == actual => {}
        Some(e) => errs.push(format!("{ctx}.{field}: expected {e}, got {actual}")),
        None => errs.push(format!(
            "{ctx}.{field}: missing or not a number in expected.json"
        )),
    }
}

fn cmp_u64(errs: &mut Vec<String>, ctx: &str, field: &str, expected: Option<&Value>, actual: u64) {
    match expected.and_then(Value::as_u64) {
        Some(e) if e == actual => {}
        Some(e) => errs.push(format!("{ctx}.{field}: expected {e}, got {actual}")),
        None => errs.push(format!(
            "{ctx}.{field}: missing or not an integer in expected.json"
        )),
    }
}

fn cmp_point(errs: &mut Vec<String>, ctx: &str, exp: &Value, got: &PointScore) {
    cmp_u64(errs, ctx, "probes", exp.get("probes"), got.probes);
    cmp_u64(
        errs,
        ctx,
        "decision_rounds",
        exp.get("decision_rounds"),
        got.decision_rounds,
    );
    cmp_u64(
        errs,
        ctx,
        "effective_sequential_rounds",
        exp.get("effective_sequential_rounds"),
        got.effective_sequential_rounds,
    );
    cmp_f64(
        errs,
        ctx,
        "attainment",
        exp.get("attainment"),
        got.attainment,
    );
    cmp_f64(errs, ctx, "work", exp.get("work"), got.work);
    cmp_f64(
        errs,
        ctx,
        "parallel_penalty",
        exp.get("parallel_penalty"),
        got.parallel_penalty,
    );
    cmp_f64(
        errs,
        ctx,
        "context_mismatch_rate",
        exp.get("context_mismatch_rate"),
        got.context_mismatch_rate,
    );
    let batches = first_key(
        exp,
        &["batches", "probe_sequence", "probe_batches", "sequence"],
    )
    .and_then(|v| serde_json::from_value::<Vec<Vec<String>>>(v.clone()).ok());
    match batches {
        Some(b) if b == got.batches => {}
        Some(b) => errs.push(format!(
            "{ctx}.batches: expected {b:?}, got {:?}",
            got.batches
        )),
        None => errs.push(format!(
            "{ctx}.batches: missing or malformed in expected.json"
        )),
    }
}

fn compare_world(errs: &mut Vec<String>, name: &str, expected: &Value, got: &WorldScore) {
    let points = expected_points(expected);
    if points.len() != BETA_GRID.len() {
        errs.push(format!(
            "{name}: expected.json has {} points, want {}",
            points.len(),
            BETA_GRID.len()
        ));
        return;
    }
    for (i, (beta, exp)) in points.iter().enumerate() {
        let got_point = match beta {
            Some(b) => got.points.iter().find(|p| p.beta == round9(*b)),
            None => got.points.get(i),
        };
        let ctx = format!("{name}.points[{i}]");
        match got_point {
            Some(p) => cmp_point(errs, &ctx, exp, p),
            None => errs.push(format!("{ctx}: no replay point for beta {beta:?}")),
        }
    }
    cmp_f64(
        errs,
        name,
        "pareto_auc",
        expected.get("pareto_auc"),
        got.pareto_auc,
    );
    cmp_f64(
        errs,
        name,
        "pareto_reward",
        expected.get("pareto_reward"),
        got.pareto_reward,
    );
    if let Some(v) = expected.get("parallel_penalty") {
        cmp_f64(
            errs,
            name,
            "parallel_penalty",
            Some(v),
            got.parallel_penalty,
        );
    }
    if let Some(v) = expected.get("out_of_support").and_then(Value::as_bool)
        && v != got.out_of_support
    {
        errs.push(format!(
            "{name}.out_of_support: expected {v}, got {}",
            got.out_of_support
        ));
    }
}

fn evaluate_baseline(tree: &WorldTree) -> WorldScore {
    let make = |c: &PolicyConfig| -> Box<dyn ExplorationPolicy> {
        Box::new(BaselineParallelRefine::new(c))
    };
    evaluate_world(&make, tree, &ReplayConfig::for_world(tree)).unwrap()
}

fn fixture_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

#[test]
fn baseline_matches_python_fixtures() {
    let root = fixtures_root();
    let dirs = fixture_dirs(&root);
    assert!(
        dirs.len() >= 3,
        "need at least 3 fixture worlds under {}, found {}",
        root.display(),
        dirs.len()
    );
    let mut errs = Vec::new();
    for dir in &dirs {
        let name = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let tree = match WorldTree::load_dir(dir) {
            Ok(t) => t,
            Err(e) => {
                errs.push(format!("{name}: load failed: {e}"));
                continue;
            }
        };
        let expected: Value = match std::fs::read_to_string(dir.join("expected.json"))
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        {
            Ok(v) => v,
            Err(e) => {
                errs.push(format!("{name}: expected.json: {e}"));
                continue;
            }
        };
        let got = evaluate_baseline(&tree);
        compare_world(&mut errs, &name, &expected, &got);
    }
    assert!(errs.is_empty(), "fixture mismatches:\n{}", errs.join("\n"));
}

/// One `script.json` point (SPEC §11): the fixed batch sequence for one beta.
#[derive(Debug, Clone, serde::Deserialize)]
struct ScriptPoint {
    beta: f64,
    batches: Vec<Vec<String>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Script {
    points: Vec<ScriptPoint>,
}

/// SPEC §11 scripted policy: `plan_grid` as the baseline, `solve` probes the
/// scripted batches of its beta in order and stops. Any probe error (an
/// illegal batch included) is surfaced, never tolerated.
struct ScriptedPolicy {
    batches: Vec<Vec<String>>,
}

impl ScriptedPolicy {
    fn new(script: &Script, config: &PolicyConfig) -> Self {
        let batches = script
            .points
            .iter()
            .find(|p| (p.beta - config.beta).abs() < 1e-9)
            .map(|p| p.batches.clone())
            .unwrap_or_default();
        Self { batches }
    }
}

impl ExplorationPolicy for ScriptedPolicy {
    fn id(&self) -> &str {
        "scripted-replay"
    }

    fn plan_grid(&mut self, ctx: &GridContext) -> Result<GridPlan, PolicyError> {
        BaselineParallelRefine.plan_grid(ctx)
    }

    fn solve(&mut self, question: &mut dyn Question) -> Result<(), PolicyError> {
        for batch in &self.batches {
            question.probe_batch(batch)?;
        }
        Ok(())
    }
}

fn evaluate_scripted(tree: &WorldTree, script: &Script) -> Result<WorldScore, EvalError> {
    let make = |c: &PolicyConfig| -> Box<dyn ExplorationPolicy> {
        Box::new(ScriptedPolicy::new(script, c))
    };
    evaluate_world(&make, tree, &ReplayConfig::for_world(tree))
}

fn read_json(path: &Path) -> Result<Value, String> {
    std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
}

#[test]
fn scripted_replay_matches_python_fixtures() {
    let root = fixtures_root();
    let mut errs = Vec::new();
    let mut scored: Vec<WorldScore> = Vec::new();
    for dir in fixture_dirs(&root) {
        if !dir.join("script.json").exists() {
            continue;
        }
        let name = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let tree = match WorldTree::load_dir(&dir) {
            Ok(t) => t,
            Err(e) => {
                errs.push(format!("{name}: load failed: {e}"));
                continue;
            }
        };
        let script: Script = match read_json(&dir.join("script.json"))
            .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
        {
            Ok(s) => s,
            Err(e) => {
                errs.push(format!("{name}: script.json: {e}"));
                continue;
            }
        };
        let expected = match read_json(&dir.join("expected_script.json")) {
            Ok(v) => v,
            Err(e) => {
                errs.push(format!("{name}: expected_script.json: {e}"));
                continue;
            }
        };
        match evaluate_scripted(&tree, &script) {
            Ok(got) => {
                compare_world(&mut errs, &name, &expected, &got);
                // The replayed batches must be exactly the scripted ones.
                for (sp, gp) in script.points.iter().zip(&got.points) {
                    if sp.batches != gp.batches {
                        errs.push(format!(
                            "{name}: beta {}: replayed batches differ from script",
                            sp.beta
                        ));
                    }
                }
                scored.push(got);
            }
            Err(e) => errs.push(format!("{name}: scripted replay failed: {e}")),
        }
    }
    assert!(errs.is_empty(), "scripted fixture mismatches:\n{}", errs.join("\n"));
    assert!(
        scored.len() >= 2,
        "need at least 2 scripted fixtures under {}, found {}",
        root.display(),
        scored.len()
    );

    // SPEC §11 conditions, across the scripted fixtures (anti-vacuity).
    let points = || scored.iter().flat_map(|w| w.points.iter());
    let works: Vec<f64> = points().map(|p| p.work).collect();
    assert!(
        works.iter().any(|w| (w - works[0]).abs() > 1e-12),
        "work must differ across points: {works:?}"
    );
    assert!(
        points().any(|p| p.attainment < 1.0),
        "no point with attainment < 1"
    );
    assert!(
        scored.iter().any(|w| w.pareto_auc > 0.0 && w.pareto_auc < 1.0),
        "no world with 0 < pareto_auc < 1"
    );
    assert!(
        points().any(|p| p.context_mismatch_rate > 0.0),
        "no point with context_mismatch_rate > 0"
    );
}

#[test]
fn scripted_policy_with_illegal_batch_fails_evaluation() {
    let tree = full_grid(2, 1, 2);
    let script = Script {
        points: BETA_GRID
            .iter()
            .map(|b| ScriptPoint {
                beta: *b,
                // A frontier before its root is not legal.
                batches: vec![vec!["r1-b0-a1".to_string()]],
            })
            .collect(),
    };
    assert!(evaluate_scripted(&tree, &script).is_err());
}

#[test]
fn store_open_is_idempotent_and_round_trips_a_world() {
    let dir = tempfile::tempdir().unwrap();
    drop(DiscoveryStore::open(dir.path()).unwrap());
    let store = DiscoveryStore::open(dir.path()).unwrap();
    assert!(store.db_path().ends_with("discovery.db"));

    let tree = full_grid(2, 1, 2);
    let mut nodes = tree.nodes().to_vec();
    nodes[1].visible_set = vec!["r1-b1-a0".to_string()];
    nodes[1].cost = NodeCost {
        input_tokens: 12,
        output_tokens: 34,
        cache_read_tokens: 5,
        usd: 0.0123,
        usd_source: super::tree::CostSource::Reported,
        unknown_calls: 0,
        wall_secs: 250.5,
    };
    nodes[1].model = Some("claude-haiku-4-5".to_string());
    nodes[1].error = Some("部分錯誤".to_string());
    nodes.push(node(1, 2, 99, None));
    let w = world(2, 2, 2);

    store.insert_world(&w).unwrap();
    store.insert_world(&w).unwrap();
    store.insert_nodes(&nodes).unwrap();
    assert_eq!(
        store.load_world(&w.run_id, w.round).unwrap(),
        Some(w.clone())
    );
    assert_eq!(store.load_world(&w.run_id, 7).unwrap(), None);
    let mut loaded = store.load_nodes(&w.run_id, w.round).unwrap();
    let mut want = nodes.clone();
    loaded.sort_by(|a, b| a.cell_id.cmp(&b.cell_id));
    want.sort_by(|a, b| a.cell_id.cmp(&b.cell_id));
    assert_eq!(loaded, want);

    // Reopen after writes: migrations stay idempotent, data persists.
    drop(store);
    let store = DiscoveryStore::open(dir.path()).unwrap();
    assert_eq!(
        store.load_nodes(&w.run_id, w.round).unwrap().len(),
        nodes.len()
    );

    let score = evaluate_baseline(&tree);
    let p = &score.points[0];
    let row = PolicyEvalRow {
        policy_id: super::policy::BASELINE_POLICY_ID.to_string(),
        policy_params: "{}".to_string(),
        run_id: w.run_id.clone(),
        round: w.round,
        beta: p.beta,
        attainment: p.attainment,
        work: p.work,
        probes: p.probes,
        decision_rounds: p.decision_rounds,
        effective_sequential_rounds: p.effective_sequential_rounds,
        parallel_penalty: p.parallel_penalty,
        context_mismatch_rate: p.context_mismatch_rate,
        pareto_auc: Some(score.pareto_auc),
        pareto_reward: Some(score.pareto_reward),
        out_of_support: score.out_of_support,
    };
    assert!(store.insert_policy_eval(&row).unwrap() > 0);
    assert_eq!(
        store
            .count_policy_evals(super::policy::BASELINE_POLICY_ID)
            .unwrap(),
        1
    );
}
