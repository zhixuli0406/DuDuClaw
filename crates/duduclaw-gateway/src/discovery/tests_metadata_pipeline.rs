//! Exercises the production Dream writer before the run's real terminal
//! attestation. Fixture isolation is explicit; no provider or Docker is used.
use super::budget::SharedBudget;
use super::contracts::{
    AttemptInfraError, AttemptOutcome, AttemptRequest, AttemptRunner, IsolationBackend,
    PolicyDegraded, PolicyVersion, RunBudget,
};
use super::dream::{DreamRequest, dream};
use super::night::{DefaultsNamespace, FrozenDefaults};
use super::online::RunIdentity;
use super::policy::{BASELINE_POLICY_ID, GridPlan};
use super::policy_runner::{BASELINE_SOURCE, ManagedPolicySource};
use super::store::DiscoveryStore;
use super::tree::{Direction, Node, WORLD_SCHEMA, World, WorldTree};
use std::path::Path;
use std::time::Duration;

struct NoCalls;
#[async_trait::async_trait]
impl AttemptRunner for NoCalls {
    async fn run_attempt(&self, _: &AttemptRequest) -> Result<AttemptOutcome, AttemptInfraError> {
        panic!("metadata integration must not launch a provider attempt")
    }
}
fn limits() -> RunBudget {
    RunBudget {
        max_agent_calls: 1,
        max_usd: 1.0,
        max_wall_secs: 30,
        max_rounds: 1,
    }
}
fn fixture(home: &Path, run_id: &str) -> (DiscoveryStore, WorldTree, DreamRequest) {
    let store = DiscoveryStore::open(home).unwrap();
    store
        .create_run(
            run_id,
            "fixture",
            "metadata-worker",
            "score",
            &"a".repeat(64),
            Direction::Max,
            &limits(),
        )
        .unwrap();
    store
        .attach_run_identity(&RunIdentity {
            run_id: run_id.into(),
            task_id: Some("metadata-task".into()),
            creator_id: "fixture-host".into(),
            creator_origin: "integration-fixture".into(),
            approved_root_id: Some("approved-fixture-root".into()),
        })
        .unwrap();
    store.set_run_runtime(run_id, "claude", "haiku").unwrap();
    let world = World {
        schema: WORLD_SCHEMA.into(),
        run_id: run_id.into(),
        round: 1,
        direction: Direction::Max,
        baseline_score: 0.0,
        branch_count: 1,
        refine_count: 0,
        max_parallelism: 1,
        policy_id: BASELINE_POLICY_ID.into(),
        beta: 0.6,
    };
    store.insert_world(&world).unwrap();
    store
        .freeze_round_plan(
            &world,
            &PolicyVersion {
                policy_id: BASELINE_POLICY_ID.into(),
                source_sha256: None,
            },
            None,
            &GridPlan {
                branch_count: 1,
                refine_count: 0,
                reason: "metadata fixture".into(),
            },
        )
        .unwrap();
    let node: Node = serde_json::from_value(
        serde_json::json!({"run_id":run_id,"round":1,"cell_id":"r1-b0-a0",
        "branch":0,"attempt":0,"seq":1,"evaluated":true,"valid":true,"score":1.0,
        "fail_class":"ok","visible_set":[]}),
    )
    .unwrap();
    store.insert_nodes(&[node.clone()]).unwrap();
    store
        .set_node_isolation(run_id, &node.cell_id, IsolationBackend::Container)
        .unwrap();
    // Host-created artifact digest fixture, rather than bypassing the run's
    // attestation flag or the evaluation comparison flag with UPDATE SQL.
    let artifact = home.join(format!("fixture-artifact-{run_id}"));
    super::workspace::create_private_directory(&artifact).unwrap();
    std::fs::write(artifact.join("solution.txt"), "fixture output").unwrap();
    store
        .record_verified_artifact(
            run_id,
            &node.cell_id,
            &super::workspace::directory_sha256(&artifact).unwrap(),
        )
        .unwrap();
    store.finish_round(&world).unwrap();
    let tree = WorldTree::new(world, vec![node]).unwrap();
    let request = DreamRequest {
        home_dir: home.into(),
        run_dir: home.join("discovery/runs").join(run_id),
        run_id: run_id.into(),
        round: 1,
        agent_id: "metadata-worker".into(),
        model: None,
        account_pool: Vec::new(),
        versions: 0,
        max_turns: 1,
        timeout: Duration::from_secs(5),
        round_probe_cap: 1,
    };
    (store, tree, request)
}
fn namespace() -> DefaultsNamespace {
    DefaultsNamespace {
        agent_id: "metadata-worker".into(),
        scorer_name: "score".into(),
        scorer_hash: "a".repeat(64),
        direction: Direction::Max,
        approved_root_id: "approved-fixture-root".into(),
        runtime: "claude".into(),
        configured_model: "haiku".into(),
    }
}
#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn completed_dream_writer_rows_become_comparable_only_after_terminal_attestation() {
    let home = tempfile::tempdir().unwrap();
    let (store, tree, request) = fixture(home.path(), "terminal-pipeline");
    let source = ManagedPolicySource::new(Err(PolicyDegraded::NoIsolation));
    let budget = SharedBudget::new(limits()).unwrap();
    let audit = dream(&source, &NoCalls, &budget, &[tree.clone()], &request)
        .await
        .unwrap();
    assert_eq!(budget.snapshot().agent_calls, 0);
    let db = rusqlite::Connection::open(store.db_path()).unwrap();
    let original_params: String = db
        .query_row(
            "SELECT policy_params FROM discovery_policy_evals LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for mode in [
        "invalid",
        "out_of_support",
        "context_mismatch",
        "violation",
        "unknown_origin",
    ] {
        let mut evaluation = audit.candidates[0].evaluation.clone();
        let origin = if mode == "unknown_origin" {
            "unknown"
        } else {
            "builtin"
        };
        match mode {
            "invalid" => {
                evaluation.valid = false;
                evaluation.violation = Some("invalid fixture".into());
            }
            "out_of_support" => {
                evaluation.worlds[0].out_of_support = true;
            }
            "context_mismatch" => {
                evaluation.context_mismatch_rate = Some(0.25);
                for point in &mut evaluation.worlds[0].points {
                    point.context_mismatch_rate = 0.25;
                }
            }
            "violation" => {
                evaluation.violation = Some("contradictory fixture".into());
            }
            _ => {}
        }
        let mut params: serde_json::Value = serde_json::from_str(&original_params).unwrap();
        params["fixture_variant"] = mode.into();
        store
            .record_candidate_evaluation_with_origin(
                BASELINE_POLICY_ID,
                &params.to_string(),
                &[tree.clone()],
                &evaluation,
                &[],
                origin,
            )
            .unwrap();
    }
    let count = || {
        db.query_row(
            "SELECT COUNT(*) FROM discovery_policy_evals WHERE comparison_available=1",
            [],
            |row| row.get::<_, usize>(0),
        )
        .unwrap()
    };
    assert_eq!(count(), 0, "a still-running run is not comparison evidence");
    store
        .finish_run(&request.run_id, "complete", Some("r1-b0-a0"))
        .unwrap();
    assert_eq!(
        count(),
        0,
        "terminal status alone must not replace host provenance attestation"
    );
    store.verify_run_provenance(&request.run_id).unwrap();
    assert!(
        store
            .load_run(&request.run_id)
            .unwrap()
            .unwrap()
            .provenance_verified
    );
    assert_eq!(
        count(),
        audit.candidates.len() * super::score::BETA_GRID.len(),
        "valid Dream evaluations written while running must become comparable after host verification"
    );
    drop(db);
    drop(store);
    let reopened = DiscoveryStore::open(home.path()).unwrap();
    assert!(
        reopened
            .load_round_metadata(&request.run_id, 1)
            .unwrap()
            .unwrap()
            .comparison_available
    );
    let db = rusqlite::Connection::open(reopened.db_path()).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM discovery_policy_evals WHERE comparison_available=1",
            [],
            |row| row.get::<_, usize>(0)
        )
        .unwrap(),
        5
    );
    reopened
        .set_node_isolation(&request.run_id, "r1-b0-a0", IsolationBackend::Native)
        .unwrap();
    reopened.verify_run_provenance(&request.run_id).unwrap();
    assert!(
        !reopened
            .load_run(&request.run_id)
            .unwrap()
            .unwrap()
            .provenance_verified
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM discovery_policy_evals WHERE comparison_available=1",
            [],
            |row| row.get::<_, usize>(0)
        )
        .unwrap(),
        0,
        "failed re-attestation must revoke stale positive qualification"
    );
}
#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn dream_canonical_policy_identity_is_separate_from_its_development_occurrence() {
    let home = tempfile::tempdir().unwrap();
    let (store, tree, request) = fixture(home.path(), "canonical-pipeline");
    let source = ManagedPolicySource::new(Err(PolicyDegraded::NoIsolation));
    let budget = SharedBudget::new(limits()).unwrap();
    dream(&source, &NoCalls, &budget, &[tree], &request)
        .await
        .unwrap();
    let db = rusqlite::Connection::open(store.db_path()).unwrap();
    let (id,params,origin,origins):(String,String,String,String)=db.query_row(
        "SELECT policy_id,policy_params,evaluation_origin,source_origin_task_ids FROM discovery_policy_evals LIMIT 1",[],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    let params: serde_json::Value = serde_json::from_str(&params).unwrap();
    assert_eq!(params["source"], BASELINE_SOURCE);
    assert_eq!(
        id, BASELINE_POLICY_ID,
        "occurrence IDs must never masquerade as frozen policy identity"
    );
    assert_eq!(params["policy_id"], id);
    let occurrence = params["occurrence_id"]
        .as_str()
        .expect("development occurrence must be preserved independently");
    assert!(occurrence.starts_with("canonical-pipeline:r1:v0:"));
    assert_ne!(occurrence, id);
    let stored_occurrence: String = db
        .query_row(
            "SELECT occurrence_id FROM discovery_policy_evals LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_occurrence, occurrence);
    assert_eq!(origin, "builtin");
    assert_eq!(origins, "[]");
    store
        .finish_run(&request.run_id, "complete", Some("r1-b0-a0"))
        .unwrap();
    store.verify_run_provenance(&request.run_id).unwrap();
    assert_eq!(
        store.night_candidates(&namespace()).unwrap(),
        vec![FrozenDefaults::default()],
        "Night must recognize the production writer's canonical builtin, without inventing Python origins"
    );
    assert_eq!(budget.snapshot().agent_calls, 0);
}
