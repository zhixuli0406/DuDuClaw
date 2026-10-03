//! Synthetic-tree property tests for tree / replay / score / policy.

use super::eval::{ReplayConfig, evaluate_world, run_point};
use super::policy::{
    BaselineParallelRefine, ExplorationPolicy, GridContext, GridPlan, PolicyConfig, PolicyError,
    Question,
};
use super::replay::{IllegalBatchReason, ProbeError, Replay, TerminationReason};
use super::score::{attainment, pareto_auc, round9};
use super::tree::{
    Direction, FailClass, Node, NodeCost, TreeError, World, WorldTree, cell_id, parse_cell_id,
    parse_tree_jsonl,
};

pub(super) const RUN: &str = "test-run";

pub(super) fn world(branch_count: u32, refine_count: u32, mp: u32) -> World {
    World {
        schema: super::tree::WORLD_SCHEMA.to_string(),
        run_id: RUN.to_string(),
        round: 1,
        direction: Direction::Max,
        baseline_score: 0.0,
        branch_count,
        refine_count,
        max_parallelism: mp,
        policy_id: "baseline-parallel-refine".to_string(),
        beta: 0.6,
    }
}

/// A node; `score = None` makes it a failed (invalid) node.
pub(super) fn node(branch: u32, attempt: u32, seq: u64, score: Option<f64>) -> Node {
    Node {
        schema: super::tree::NODE_SCHEMA.to_string(),
        run_id: RUN.to_string(),
        round: 1,
        cell_id: cell_id(1, branch, attempt),
        parent_id: (attempt > 0).then(|| cell_id(1, branch, attempt - 1)),
        branch,
        attempt,
        seq,
        dispatched_at: None,
        finished_at: None,
        evaluated: score.is_some(),
        valid: score.is_some(),
        score,
        fail_class: if score.is_some() {
            FailClass::Ok
        } else {
            FailClass::RuntimeError
        },
        error: None,
        visible_set: Vec::new(),
        cost: NodeCost::default(),
        runtime: None,
        model: None,
        workspace: None,
        proposal_excerpt: None,
    }
}

/// Full grid: every branch has attempts `0..=refine_count`, score = b + a.
pub(super) fn full_grid(branches: u32, refine: u32, mp: u32) -> WorldTree {
    let mut nodes = Vec::new();
    let mut seq = 0;
    for b in 0..branches {
        for a in 0..=refine {
            nodes.push(node(b, a, seq, Some(f64::from(b + a))));
            seq += 1;
        }
    }
    WorldTree::new(world(branches, refine, mp), nodes).unwrap()
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn baseline_trace(tree: &WorldTree, k2: u32) -> super::replay::ReplayTrace {
    let mut replay = Replay::new(tree, k2);
    BaselineParallelRefine.solve(&mut replay).unwrap();
    replay.finish()
}

#[test]
fn full_grid_baseline_reveals_every_node_in_order() {
    let tree = full_grid(3, 2, 2);
    let t = baseline_trace(&tree, 1000);
    let expected: Vec<Vec<String>> = vec![
        ids(&["r1-b0-a0", "r1-b1-a0"]),
        ids(&["r1-b2-a0"]),
        ids(&["r1-b0-a1", "r1-b1-a1"]),
        ids(&["r1-b2-a1"]),
        ids(&["r1-b0-a2", "r1-b1-a2"]),
        ids(&["r1-b2-a2"]),
    ];
    assert_eq!(t.batches, expected);
    assert_eq!(t.counters.probes, 9);
    assert_eq!(t.counters.decision_rounds, 6);
    assert_eq!(t.counters.effective_sequential_rounds, 6);
    assert_eq!(t.terminated, None);
    let revealed = t.batches.iter().flatten().map(String::as_str);
    assert_eq!(attainment(&tree, revealed), 1.0);
}

#[test]
fn irregular_grid_with_failed_nodes() {
    // b0: a0..a2; b1: a0 only (failed); b2: a0, a1 (a1 failed).
    let nodes = vec![
        node(0, 0, 0, Some(1.0)),
        node(1, 0, 1, None),
        node(2, 0, 2, Some(0.5)),
        node(0, 1, 3, Some(2.0)),
        node(2, 1, 4, None),
        node(0, 2, 5, Some(3.0)),
    ];
    let tree = WorldTree::new(world(3, 2, 4), nodes).unwrap();
    let t = baseline_trace(&tree, 1000);
    assert_eq!(
        t.batches,
        vec![
            ids(&["r1-b0-a0", "r1-b1-a0", "r1-b2-a0"]),
            ids(&["r1-b0-a1", "r1-b2-a1"]),
            ids(&["r1-b0-a2"]),
        ]
    );
    assert_eq!(t.counters.probes, 6);
    assert_eq!(t.counters.decision_rounds, 3);
}

#[test]
fn legal_actions_lists_roots_before_frontiers() {
    let tree = full_grid(3, 1, 3);
    let mut r = Replay::new(&tree, 1000);
    assert_eq!(r.legal_roots(), ids(&["r1-b0-a0", "r1-b1-a0", "r1-b2-a0"]));
    r.probe_batch(&ids(&["r1-b1-a0"])).unwrap();
    assert_eq!(
        r.legal_actions(),
        ids(&["r1-b0-a0", "r1-b2-a0", "r1-b1-a1"])
    );
    assert_eq!(r.opened_branches(), vec![1]);
    r.probe_batch(&ids(&["r1-b1-a1"])).unwrap();
    assert_eq!(r.legal_actions(), ids(&["r1-b0-a0", "r1-b2-a0"]));
}

#[test]
fn illegal_batches_leave_state_and_counters_unchanged() {
    let tree = full_grid(3, 2, 2);
    let mut r = Replay::new(&tree, 1000);
    r.probe_batch(&ids(&["r1-b0-a0"])).unwrap();
    let before_counters = r.counters();
    let before_legal = r.legal_actions();
    let before_observed = r.observed();

    let cases: Vec<(Vec<String>, IllegalBatchReason)> = vec![
        (Vec::new(), IllegalBatchReason::Empty),
        (
            ids(&["r1-b1-a0", "r1-b1-a0"]),
            IllegalBatchReason::Duplicate("r1-b1-a0".into()),
        ),
        (
            ids(&["r1-b1-a0", "r1-b2-a0", "r1-b0-a1"]),
            IllegalBatchReason::TooLarge { len: 3, max: 2 },
        ),
        (
            ids(&["r1-b1-a1"]),
            IllegalBatchReason::NotLegal("r1-b1-a1".into()),
        ),
        (
            ids(&["r1-b1-a0", "r1-b1-a1"]),
            IllegalBatchReason::NotLegal("r1-b1-a1".into()),
        ),
        (
            ids(&["r1-b0-a0"]),
            IllegalBatchReason::NotLegal("r1-b0-a0".into()),
        ),
        (
            ids(&["r1-b9-a0"]),
            IllegalBatchReason::NotLegal("r1-b9-a0".into()),
        ),
    ];
    for (batch, reason) in cases {
        assert_eq!(r.probe_batch(&batch), Err(ProbeError::IllegalBatch(reason)));
        assert_eq!(r.counters(), before_counters);
        assert_eq!(r.legal_actions(), before_legal);
        assert_eq!(r.observed(), before_observed);
        assert_eq!(r.batches().len(), 1);
    }
    assert!(r.consecutive_illegal() >= 2);
    r.probe_batch(&ids(&["r1-b1-a0"])).unwrap();
    assert_eq!(r.consecutive_illegal(), 0);
}

#[test]
fn meta_only_for_legal_or_revealed_cells() {
    let tree = full_grid(2, 2, 2);
    let mut r = Replay::new(&tree, 1000);
    let m = r.meta("r1-b0-a0").unwrap();
    assert_eq!((m.branch, m.attempt, m.parent_id.clone()), (0, 0, None));
    assert!(m.tags.is_empty());
    assert!(r.meta("r1-b0-a1").is_err(), "unrevealed, not legal");
    assert!(r.meta("r1-b7-a0").is_err(), "unknown");
    r.probe_batch(&ids(&["r1-b0-a0"])).unwrap();
    assert!(r.meta("r1-b0-a1").is_ok(), "now legal");
    assert!(r.meta("r1-b0-a0").is_ok(), "revealed");
    assert!(r.meta("r1-b0-a2").is_err());
}

#[test]
fn k2_terminates_the_replay() {
    let tree = full_grid(3, 2, 2);
    let t = baseline_trace(&tree, 2);
    assert_eq!(t.counters.decision_rounds, 2);
    assert_eq!(t.batches.len(), 2);
    assert_eq!(t.terminated, Some(TerminationReason::K2Reached));

    let mut r = Replay::new(&tree, 1);
    r.probe_batch(&ids(&["r1-b0-a0"])).unwrap();
    assert_eq!(
        r.probe_batch(&ids(&["r1-b1-a0"])),
        Err(ProbeError::Terminated(TerminationReason::K2Reached))
    );
    assert_eq!(r.counters().probes, 1);
}

#[test]
fn attainment_edge_cases() {
    // No valid node anywhere: ceiling == baseline → 1.0.
    let tree = WorldTree::new(
        world(2, 0, 2),
        vec![node(0, 0, 0, None), node(1, 0, 1, None)],
    )
    .unwrap();
    assert_eq!(attainment(&tree, std::iter::empty()), 1.0);
    assert_eq!(attainment(&tree, ["r1-b0-a0"]), 1.0);

    // Valid nodes but none beats the baseline: ceiling < baseline → 1.0.
    let mut w = world(1, 0, 1);
    w.baseline_score = 5.0;
    let tree = WorldTree::new(w, vec![node(0, 0, 0, Some(3.0))]).unwrap();
    assert_eq!(attainment(&tree, std::iter::empty()), 1.0);

    // Normal case, clamped below at 0.
    let mut w = world(2, 0, 2);
    w.baseline_score = 1.0;
    let tree = WorldTree::new(w, vec![node(0, 0, 0, Some(0.0)), node(1, 0, 1, Some(3.0))]).unwrap();
    assert_eq!(attainment(&tree, std::iter::empty()), 0.0);
    assert_eq!(attainment(&tree, ["r1-b0-a0"]), 0.0);
    assert_eq!(attainment(&tree, ["r1-b1-a0"]), 1.0);
}

#[test]
fn direction_min_negates_before_comparing() {
    let mut w = world(2, 1, 2);
    w.direction = Direction::Min;
    w.baseline_score = 10.0;
    let nodes = vec![
        node(0, 0, 0, Some(8.0)),
        node(1, 0, 1, Some(9.0)),
        node(0, 1, 2, Some(5.0)),
    ];
    let tree = WorldTree::new(w, nodes).unwrap();
    assert_eq!(tree.oriented_ceiling(), -5.0);
    let a = attainment(&tree, ["r1-b0-a0", "r1-b1-a0"]);
    assert_eq!(round9(a), 0.4);
    // Observation deltas stay in raw score space.
    let mut r = Replay::new(&tree, 1000);
    let obs = r.probe_batch(&ids(&["r1-b0-a0"])).unwrap();
    assert_eq!(obs[0].delta_vs_baseline, Some(-2.0));
    let obs = r.probe_batch(&ids(&["r1-b0-a1"])).unwrap();
    assert_eq!(obs[0].delta_vs_parent, Some(-3.0));
}

#[test]
fn observation_fields() {
    let mut failed = node(0, 1, 1, None);
    failed.error = Some("錯".repeat(200)); // 600 bytes
    let nodes = vec![node(0, 0, 0, Some(2.0)), failed, node(0, 2, 2, Some(4.5))];
    let mut w = world(1, 2, 1);
    w.baseline_score = 0.5;
    let tree = WorldTree::new(w, nodes).unwrap();
    let mut r = Replay::new(&tree, 1000);
    let o0 = r.probe_batch(&ids(&["r1-b0-a0"])).unwrap().remove(0);
    assert_eq!(o0.delta_vs_baseline, Some(1.5));
    assert_eq!(o0.delta_vs_parent, Some(1.5));
    assert_eq!((o0.n_valid, o0.n_total), (None, None));
    let o1 = r.probe_batch(&ids(&["r1-b0-a1"])).unwrap().remove(0);
    assert_eq!(o1.score, None);
    assert_eq!(o1.delta_vs_baseline, None);
    assert_eq!(o1.error.as_deref().map(str::len), Some(300));
    let o2 = r.probe_batch(&ids(&["r1-b0-a2"])).unwrap().remove(0);
    assert_eq!(o2.delta_vs_parent, None, "parent has no score");
    assert_eq!(o2.delta_vs_baseline, Some(4.0));
    assert_eq!(r.observed().len(), 3);
    let json = serde_json::to_value(&o2).unwrap();
    assert!(json.get("n_valid").is_some_and(serde_json::Value::is_null));
}

#[test]
fn context_mismatch_same_batch_vs_earlier_batch() {
    let mut b1 = node(1, 0, 1, Some(1.0));
    b1.visible_set = ids(&["r1-b0-a0"]);
    let make =
        |mp| WorldTree::new(world(2, 0, mp), vec![node(0, 0, 0, Some(1.0)), b1.clone()]).unwrap();

    // Same batch: b0-a0 is not yet revealed when b1-a0 is.
    let tree = make(2);
    let t = baseline_trace(&tree, 1000);
    assert_eq!(t.context_mismatches, 1);
    let cfg = ReplayConfig::for_world(&tree);
    let run = run_point(&mut BaselineParallelRefine, &tree, &cfg, 0.0).unwrap();
    let (p, _) = super::eval::score_point(&tree, &run);
    assert_eq!(p.context_mismatch_rate, 0.5);

    // Earlier batch: revealed before, no mismatch.
    let tree = make(1);
    let t = baseline_trace(&tree, 1000);
    assert_eq!(t.context_mismatches, 0);
}

#[test]
fn pareto_auc_hand_computed() {
    // F: [0,.2)=0, [.2,.5)=.5, [.5,.8)=.8, [.8,1)=.8 → .15 + .24 + .16 = .55
    let pts = [(0.2, 0.5), (0.5, 0.8), (0.5, 0.6), (1.0, 1.0), (0.8, 0.7)];
    assert_eq!(round9(pareto_auc(&pts)), 0.55);
    assert_eq!(pareto_auc(&[]), 0.0);
    assert_eq!(pareto_auc(&[(0.0, 1.0)]), 1.0);
}

#[test]
fn round9_matches_python_round() {
    assert_eq!(round9(0.1 + 0.2), 0.3);
    assert_eq!(round9(1.234_567_890_12), 1.234_567_89);
    assert_eq!(round9(2.0 / 3.0), 0.666_666_667);
    assert_eq!(round9(-2.0 / 3.0), -0.666_666_667);
    assert!(round9(f64::NAN).is_nan());
}

#[test]
fn evaluate_world_full_grid_sweep() {
    let tree = full_grid(3, 2, 2);
    let cfg = ReplayConfig::for_world(&tree);
    let make = |c: &PolicyConfig| -> Box<dyn ExplorationPolicy> {
        Box::new(BaselineParallelRefine::new(c))
    };
    let s = evaluate_world(&make, &tree, &cfg).unwrap();
    assert_eq!(s.points.len(), 5);
    for p in &s.points {
        assert_eq!(p.probes, 9);
        assert_eq!(p.work, 1.0);
        assert_eq!(p.attainment, 1.0);
        assert_eq!(p.parallel_penalty, round9(6.0 / 9.0));
        assert_eq!(p.batches, s.points[0].batches);
    }
    // Every point sits at work = 1, so the frontier has zero area.
    assert_eq!(s.pareto_auc, 0.0);
    assert_eq!(s.pareto_reward, round9(-0.1 * (6.0 / 9.0)));
    assert!(!s.out_of_support);
}

struct Greedy;

impl ExplorationPolicy for Greedy {
    fn id(&self) -> &str {
        "greedy-oversized"
    }
    fn plan_grid(&mut self, ctx: &GridContext) -> Result<GridPlan, PolicyError> {
        Ok(GridPlan {
            branch_count: ctx.trace_branch_count.unwrap_or(0) + 1,
            refine_count: 0,
            reason: "too wide".into(),
        })
    }
    fn solve(&mut self, q: &mut dyn Question) -> Result<(), PolicyError> {
        let roots = q.legal_roots();
        q.probe_batch(&roots[1..2])?;
        Ok(())
    }
}

#[test]
fn out_of_support_zeroes_the_reward() {
    let tree = full_grid(2, 1, 2);
    let cfg = ReplayConfig::for_world(&tree);
    let make = |_: &PolicyConfig| -> Box<dyn ExplorationPolicy> { Box::new(Greedy) };
    let s = evaluate_world(&make, &tree, &cfg).unwrap();
    assert!(s.out_of_support);
    assert_eq!(s.pareto_reward, 0.0);
    assert!(s.pareto_auc > 0.0);
}

struct Stubborn;

impl ExplorationPolicy for Stubborn {
    fn id(&self) -> &str {
        "stubborn"
    }
    fn plan_grid(&mut self, _: &GridContext) -> Result<GridPlan, PolicyError> {
        Ok(GridPlan {
            branch_count: 1,
            refine_count: 0,
            reason: String::new(),
        })
    }
    fn solve(&mut self, q: &mut dyn Question) -> Result<(), PolicyError> {
        let _ = q.probe_batch(&[]);
        let _ = q.probe_batch(&[]);
        Ok(())
    }
}

#[test]
fn two_consecutive_illegal_batches_invalidate_in_process() {
    let tree = full_grid(1, 0, 1);
    let cfg = ReplayConfig::for_world(&tree);
    let err = run_point(&mut Stubborn, &tree, &cfg, 0.0).unwrap_err();
    assert!(matches!(
        err,
        super::eval::EvalError::ConsecutiveIllegal { count: 2, .. }
    ));
}

#[test]
fn baseline_plan_grid_caps_at_trace() {
    let ctx = GridContext {
        history: Vec::new(),
        hard_max_branch_count: 8,
        hard_max_refine_count: 2,
        worker_cap: 4,
        trace_branch_count: Some(4),
        trace_refine_count: Some(3),
    };
    let plan = BaselineParallelRefine.plan_grid(&ctx).unwrap();
    assert_eq!((plan.branch_count, plan.refine_count), (4, 2));
    assert_eq!(plan.reason, "fixed baseline");
}

#[test]
fn cell_id_round_trip_and_rejects_non_canonical() {
    assert_eq!(cell_id(1, 0, 2), "r1-b0-a2");
    assert_eq!(parse_cell_id("r12-b3-a0"), Some((12, 3, 0)));
    for bad in [
        "r01-b0-a0",
        "r1-b0",
        "x1-b0-a0",
        "r1-b-1-a0",
        "r1-b0-a0x",
        "r+1-b0-a0",
    ] {
        assert_eq!(parse_cell_id(bad), None, "{bad}");
    }
}

#[test]
fn loader_defaults_and_errors() {
    let good = r#"{"schema":"duduclaw.discovery.node.v1","run_id":"test-run","round":1,"cell_id":"r1-b0-a0","parent_id":null,"branch":0,"attempt":0,"seq":0,"evaluated":true,"valid":true,"score":1.5,"fail_class":"ok","error":null,"visible_set":[],"future_field":{"x":1}}"#;
    let text = format!("{good}\n\n{{not json}}\n");
    match parse_tree_jsonl(text.as_bytes()) {
        Err(TreeError::NodeJson { line, .. }) => assert_eq!(line, 3),
        other => panic!("expected line-3 error, got {other:?}"),
    }
    let nodes = parse_tree_jsonl(format!("{good}\n").as_bytes()).unwrap();
    assert_eq!(nodes[0].cost, NodeCost::default());
    let with_null_cost = good.replace("\"visible_set\":[]", "\"visible_set\":[],\"cost\":null");
    assert_eq!(
        parse_tree_jsonl(with_null_cost.as_bytes()).unwrap()[0].cost,
        NodeCost::default()
    );
    let bad_class = good.replace("\"ok\"", "\"exploded\"");
    assert!(parse_tree_jsonl(bad_class.as_bytes()).is_err());
}

#[test]
fn validation_rejects_broken_trees() {
    let w = || world(2, 2, 2);
    assert!(matches!(
        WorldTree::new(w(), vec![]),
        Err(TreeError::EmptyTree)
    ));
    let dup = vec![node(0, 0, 0, Some(1.0)), node(0, 0, 1, Some(1.0))];
    assert!(matches!(
        WorldTree::new(w(), dup),
        Err(TreeError::DuplicateCell(_))
    ));
    let orphan = vec![node(0, 0, 0, Some(1.0)), node(1, 1, 1, Some(1.0))];
    assert!(matches!(
        WorldTree::new(w(), orphan),
        Err(TreeError::MissingParent { .. })
    ));
    let mut wrong_parent = node(0, 1, 1, Some(1.0));
    wrong_parent.parent_id = None;
    assert!(matches!(
        WorldTree::new(w(), vec![node(0, 0, 0, Some(1.0)), wrong_parent]),
        Err(TreeError::ParentMismatch { .. })
    ));
    let mut bad_id = node(0, 0, 0, Some(1.0));
    bad_id.cell_id = "r1-b0-a00".into();
    assert!(matches!(
        WorldTree::new(w(), vec![bad_id]),
        Err(TreeError::CellIdMismatch { .. })
    ));
    let mut scored_invalid = node(0, 0, 0, Some(1.0));
    scored_invalid.valid = false;
    assert!(matches!(
        WorldTree::new(w(), vec![scored_invalid]),
        Err(TreeError::ScoreInconsistent { .. })
    ));
    assert!(matches!(
        WorldTree::new(w(), vec![node(5, 0, 0, Some(1.0))]),
        Err(TreeError::OutOfGrid { .. })
    ));
    let mut zero_mp = w();
    zero_mp.max_parallelism = 0;
    assert!(matches!(
        WorldTree::new(zero_mp, vec![node(0, 0, 0, Some(1.0))]),
        Err(TreeError::InvalidWorld(_))
    ));
}

/// Discovery is unix-only by design: its store, private directories and
/// integrity trees rely on uid/mode/link-count checks. Every other host must
/// get an explicit "unavailable" answer, never an unverified store.
#[cfg(not(unix))]
#[test]
fn non_unix_hosts_fail_closed_before_touching_discovery_state() {
    let home = tempfile::tempdir().unwrap();
    let error = super::DiscoveryStore::open(home.path())
        .err()
        .expect("store must refuse");
    assert!(
        matches!(error, super::store::StoreError::UnsupportedPlatform(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("unavailable on this platform"),
        "{error}"
    );
    assert!(
        !home.path().join("discovery.db").exists(),
        "no database is created"
    );

    assert!(super::workspace::create_private_directory(&home.path().join("discovery")).is_err());
    assert!(!home.path().join("discovery").exists());

    let tree = home.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("judge"), "x").unwrap();
    assert!(
        super::workspace::directory_sha256(&tree).is_err(),
        "unverifiable link counts fail closed"
    );
}
