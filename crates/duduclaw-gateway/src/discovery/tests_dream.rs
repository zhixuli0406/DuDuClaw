use super::dream::{DreamRequest, dream, select_candidate};
use super::policy_runner::{CandidateEvaluation, ManagedPolicySource, PythonPolicyRuntime, BASELINE_SOURCE};
use super::contracts::*;
use super::budget::SharedBudget;
use super::tree::{NodeCost, WorldTree};
use std::sync::Mutex;

fn evaluation(value: Option<f64>, valid: bool) -> CandidateEvaluation {
    CandidateEvaluation { valid, violation: None, value, worlds: vec![], context_mismatch_rate: None }
}
#[test]
fn selection_includes_incumbent_keeps_ties_and_excludes_invalid_or_nonfinite_values() {
    assert_eq!(select_candidate(&[evaluation(Some(0.3), true), evaluation(Some(0.3), true)]), 0);
    assert_eq!(select_candidate(&[evaluation(Some(0.3), true), evaluation(Some(0.9), false)]), 0);
    assert_eq!(select_candidate(&[evaluation(Some(0.3), true), evaluation(Some(f64::NAN), true)]), 0);
    assert_eq!(select_candidate(&[evaluation(Some(0.3), true), evaluation(Some(0.4), true)]), 1);
}
struct Developer {
    budget: SharedBudget,
    requests: Mutex<Vec<AttemptRequest>>,
}
#[async_trait::async_trait]
impl AttemptRunner for Developer {
    async fn run_attempt(&self, req: &AttemptRequest) -> Result<AttemptOutcome, AttemptInfraError> {
        let call = self.budget.reserve_call()?;
        let mut requests = self.requests.lock().unwrap();
        requests.push(req.clone());
        let code = match requests.len() {
            1 => "import os\nclass OptimalPolicy: pass".to_string(),
            2 => BASELINE_SOURCE.to_string(),
            _ => BASELINE_SOURCE.replace("        while True:", "        return\n        while True:")
                .replace("self.beta = 0.6", "self.beta = float(config.get('beta', 0.8))"),
        };
        std::fs::write(req.node_dir.join("method.py"), code).unwrap();
        self.budget.finish_call(call, 0.1);
        Ok(AttemptOutcome { cost: NodeCost { usd: 0.1, ..Default::default() },
            runtime: "fake".into(), model: "fake".into(), isolation: IsolationBackend::Native,
            final_text: String::new(), timed_out: false, infra_retries: 0 })
    }
}
fn request(dir: &std::path::Path) -> DreamRequest {
    super::workspace::create_private_directory(&dir.join("home")).unwrap();
    DreamRequest { home_dir: dir.join("home"), run_dir: dir.join("run"), run_id: "test".into(),
        round: 1, agent_id: "developer".into(), model: None, account_pool: vec![], versions: 3,
        max_turns: 5, timeout: std::time::Duration::from_secs(10), round_probe_cap: 16 }
}
fn budget(calls: u32) -> SharedBudget {
    SharedBudget::new(RunBudget { max_agent_calls: calls, max_usd: 2.0, max_wall_secs: 60, max_rounds: 1 }).unwrap()
}
fn worlds() -> Vec<WorldTree> {
    vec![WorldTree::load_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/discovery/full_grid")).unwrap()]
}
fn runtime() -> Option<PythonPolicyRuntime> {
    match PythonPolicyRuntime::experimental_native_for_tests() {
        Ok(runtime) => Some(runtime),
        Err(reason) => {
            #[cfg(target_os = "macos")]
            panic!("dream runtime must be exercised on macOS: {reason}");
            #[cfg(not(target_os = "macos"))]
            { eprintln!("dream runtime unavailable: {reason}"); None }
        }
    }
}

#[tokio::test]
async fn dream_replays_each_revision_selects_improvement_and_persists_audit() {
    let Some(runtime) = runtime() else { return };
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path());
    let source = ManagedPolicySource::new(Ok(runtime));
    let budget = budget(3);
    let runner = Developer { budget: budget.clone(), requests: Mutex::new(vec![]) };
    let audit = dream(&source, &runner, &budget, &worlds(), &req).await.unwrap();
    assert_eq!(budget.snapshot().agent_calls, 3, "no double reservation in dream");
    assert_eq!(audit.selected_version, 3);
    assert!(audit.changed);
    assert!(!audit.candidates[1].evaluation.valid);
    assert_eq!(audit.candidates[2].evaluation.value, audit.candidates[0].evaluation.value);
    assert!(source.current().source_sha256.is_some());
    assert_eq!(source.live_beta(0.1), 0.8, "the selected version's default must reach the next live episode");
    let rows = std::fs::read_to_string(req.run_dir.join("dream_audit.jsonl")).unwrap();
    let row: serde_json::Value = serde_json::from_str(rows.lines().last().unwrap()).unwrap();
    assert_eq!(row["selected_version"], 3);
    assert!(row["candidates"][3]["evaluation"]["worlds"].is_array());
    assert!(std::fs::read_to_string(req.home_dir.join("security_audit.jsonl")).unwrap()
        .contains("discovery_policy_changed"));
    let store = super::store::DiscoveryStore::open(&req.home_dir).unwrap();
    let db = rusqlite::Connection::open(store.db_path()).unwrap();
    let count: usize = db.query_row("SELECT COUNT(*) FROM discovery_policy_evals", [],
        |row| row.get(0)).unwrap();
    assert_eq!(count, audit.candidates.len() * worlds().len() * super::score::BETA_GRID.len(),
        "every candidate, including invalid revisions, must persist every world/beta result");
    for dev in runner.requests.lock().unwrap().iter() {
        assert!(!dev.run_dir.starts_with(&req.run_dir));
        assert!(!dev.run_dir.join("budget.json").exists());
        assert!(!dev.run_dir.join("evaluator.py").exists());
        assert!(dev.prompt.contains("method.py"));
        assert!(!dev.prompt.contains("max_usd"));
    }
}

#[tokio::test]
async fn dream_budget_exhaustion_keeps_incumbent_and_degraded_source_skips_calls() {
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path());
    let budget = budget(1);
    let runner = Developer { budget: budget.clone(), requests: Mutex::new(vec![]) };
    let degraded = ManagedPolicySource::new(Err(PolicyDegraded::NoIsolation));
    let audit = dream(&degraded, &runner, &budget, &worlds(), &req).await.unwrap();
    assert!(!audit.changed);
    assert!(audit.skipped_reason.is_some());
    assert_eq!(budget.snapshot().agent_calls, 0);
    if let Some(runtime) = runtime() {
        let source = ManagedPolicySource::new(Ok(runtime));
        let audit = dream(&source, &runner, &budget, &worlds(), &req).await.unwrap();
        assert_eq!(audit.selected_version, 0);
        assert!(!audit.changed);
        assert_eq!(budget.snapshot().agent_calls, 1);
        assert_eq!(runner.requests.lock().unwrap().len(), 1);
        assert!(audit.skipped_reason.as_deref().unwrap().contains("budget"));
    }
}
