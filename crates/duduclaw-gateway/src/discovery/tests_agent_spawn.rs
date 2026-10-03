use super::*;
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn ordinary_success_text_and_empty_error_fields_do_not_stop_for_rate_limit() {
    for event in [
        serde_json::json!({"type":"result","is_error":false,"result":"Explain rate limits and quota","error":null,"errors":[]}),
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"429 and usage limit"}]}}),
    ] { assert!(!rate_limit_error_envelope(&event)); }
}

#[test]
fn argv_has_strict_empty_mcp_and_restricted_tools_and_no_prompt() {
    let argv = build_claude_argv("m", 7, Path::new("/r/empty.json"));
    let joined = argv.join(" ");
    assert!(joined.contains("--strict-mcp-config --mcp-config /r/empty.json"));
    assert!(joined.contains("--tools Read,Write,Edit,Glob,Grep,Bash"));
    assert!(!joined.contains("WebFetch") && !joined.contains("Task"));
    assert!(!joined.contains("retry_context"));
    assert!(!joined.contains("--dangerously-skip-permissions"));
}

#[test]
fn env_overlay_removes_nested_session_and_empty_values() {
    let mut acc = HashMap::new();
    acc.insert("ANTHROPIC_API_KEY".to_string(), String::new());
    let o = attempt_env_overlay(&acc, Path::new("/t"));
    assert_eq!(o.get("ANTHROPIC_API_KEY"), Some(&None));
    assert_eq!(o.get("CLAUDECODE"), Some(&None));
    assert_eq!(o.get("CLAUDE_CODE_TMPDIR"), Some(&Some("/t".to_string())));
}

#[test]
fn parse_stream_prefers_result_event() {
    let s = parse_stream(
        "garbage\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-haiku-4-5-20251001\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n{\"type\":\"result\",\"result\":\"done\",\"total_cost_usd\":0.5,\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":3}}\n",
    );
    assert_eq!(s.events, 2);
    assert_eq!(s.final_text.as_deref(), Some("done"));
    assert_eq!(s.usage.as_ref().map(|u| u.input_tokens), Some(10));
    assert_eq!(usd_for("claude-haiku-4-5-20251001", &s), 0.5);
}

#[test]
fn fallback_pricing_when_no_dollar_figure() {
    let s = parse_stream(
        "{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":1000000,\"output_tokens\":0}}}\n",
    );
    assert!(s.total_cost_usd.is_none());
    assert!(usd_for("unknown-model", &s) > 0.0);
}

#[test]
fn missing_cost_is_unknown_and_subcent_registry_estimates_keep_precision() {
    assert!(usd_for("claude-haiku-4-5", &StreamSummary::default()).is_nan());
    let partial = parse_stream(r#"{"type":"result","usage":{"input_tokens":0}}"#);
    assert!(usd_for("claude-haiku-4-5", &partial).is_nan());
    let one = parse_stream(r#"{"type":"result","usage":{"input_tokens":1,"output_tokens":1}}"#);
    let usd = usd_for("claude-haiku-4-5", &one);
    assert!(usd > 0.0 && usd < 0.01, "registry cost must not be rounded to cents: {usd}");
}

#[cfg(unix)]
#[tokio::test]
async fn configured_model_is_not_reported_as_observed_and_unknown_retry_retains_liability() {
    let (_dir, runner, req) = fake_runner(r#"
cat > /dev/null
printf '%s\n' '{"type":"result","result":"done"}'
"#, 1).await;
    let out = runner.run_attempt(&req).await.unwrap();
    assert!(out.model.is_empty(), "no actual model was reported by the CLI");
    assert_eq!(out.cost.usd_source, super::super::tree::CostSource::Unknown);
    assert_eq!(out.cost.unknown_calls, 1);
    assert!(out.cost.usd.is_finite());
    assert!(runner.budget.as_ref().unwrap().snapshot().unknown_reserved_usd > 0.0);
}

#[test]
fn final_text_cap_on_cjk_boundary() {
    let t = "字".repeat(FINAL_TEXT_MAX_BYTES);
    let c = cap_final_text(&t);
    assert!(c.len() <= FINAL_TEXT_MAX_BYTES);
    assert!(c.chars().all(|ch| ch == '字'));
}

#[test]
fn unsupported_runtime_refused() {
    let f = AttemptRunnerFactory {
        home_dir: PathBuf::from("/h"),
        attempt: AttemptSettings {sandbox:AttemptSandbox::None,..Default::default()},
        quota:crate::discovery::attempt_container::QuotaLimits::default(),
        allow_unconfined: true,
        operator_identity: true, budget: None, account_rotator: None, extra_read_paths: vec![], max_concurrency: 2,
    };
    // The operator-only host experiment stays Claude-only (design §9.1).
    for runtime in ["codex","gemini","antigravity","agy","grok","openai-compat"] {
        assert!(matches!(f.for_runtime(runtime),
            Err(AttemptInfraError::CapabilityUnsupported{capability,..}) if capability=="unconfined operator experiment"),"{runtime}");
    }
    assert!(matches!(f.for_runtime("cursor"),Err(AttemptInfraError::RuntimeUnsupported(_))));
    assert!(f.for_runtime("claude").is_ok());
}

#[tokio::test]
async fn invalid_workspace_fails_closed() {
    let r = ClaudeAttemptRunner {
        home_dir: PathBuf::from("/h"),
        quota:crate::discovery::attempt_container::QuotaLimits::default(),
        claude_bin: None,
        allow_unconfined: true,
        operator_identity: false, budget: None, account_rotator: None, extra_read_paths: vec![], max_concurrency: 2,
    };
    let req = AttemptRequest {
        run_id: "r".into(),
        cell_id: "c".into(),
        node_dir: PathBuf::from("/n"),
        run_dir: PathBuf::from("/r"),
        read_workspaces: vec![],
        prompt: "p".into(),
        agent_id: "a".into(),
        model: None,
        timeout: Duration::from_secs(1),
        max_turns: 1,
        account_pool: vec![],
    };
    assert_eq!(
        r.run_attempt(&req).await.err(),
        Some(AttemptInfraError::IsolationUnavailable)
    );
}

#[cfg(unix)]
async fn fake_runner(script: &str, calls: u32) -> (tempfile::TempDir, ClaudeAttemptRunner, AttemptRequest) {
    use duduclaw_agent::account_rotator::{Account, AccountRotator, RotationStrategy};
    use super::super::contracts::RunBudget;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let node = dir.path().join("discovery/runs/run/r1/b0/a0/ws");
    super::super::workspace::create_private_directory(&node).unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let cli = tools.join("fake-claude");
    std::fs::write(&cli, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let rotator = AccountRotator::new(RotationStrategy::RoundRobin, 0);
    let mut account: Account = serde_json::from_value(serde_json::json!({
        "id":"test", "auth_method":"api_key", "provider":"anthropic",
        "priority":1,"monthly_budget_cents":1000
    })).unwrap();
    account.is_healthy = true;
    account.api_key = "fake-test-key".into();
    rotator.push_account_for_test(account).await;
    let budget = SharedBudget::new(RunBudget { max_agent_calls: calls,
        max_usd: 5.0, max_wall_secs: 30, max_rounds: 1 }).unwrap();
    let runner = ClaudeAttemptRunner { home_dir: dir.path().to_path_buf(),quota:crate::discovery::attempt_container::QuotaLimits::default(),
        claude_bin: Some(cli), allow_unconfined: true, operator_identity: true,
        budget: Some(budget), account_rotator: Some(Arc::new(rotator)), extra_read_paths: vec![], max_concurrency: 2 };
    let request = AttemptRequest { run_id:"run".into(),cell_id:"r1-b0-a0".into(),
        node_dir: node, run_dir: dir.path().join("discovery/runs/run"), read_workspaces: vec![], prompt:"identical prompt 繁中".into(),
        agent_id:"test".into(),model:Some("m".into()),timeout:Duration::from_secs(5),
        max_turns:1,account_pool:vec!["test".into()] };
    (dir, runner, request)
}

#[cfg(unix)]
#[tokio::test]
async fn release_capacity_five_attempts_share_four_slots_without_early_stop() {
    let (dir, mut runner, request) = fake_runner(r#"
cat >/dev/null
sleep 0.2
printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.001}'
"#, 5).await;
    runner.max_concurrency = 4;
    let mut requests = Vec::new();
    for branch in 0..5 {
        let mut req = request.clone();
        req.cell_id = format!("r1-b{branch}-a0");
        req.node_dir = req.run_dir.join(format!("r1/b{branch}/a0/ws"));
        super::super::workspace::create_private_directory(&req.node_dir).unwrap();
        requests.push(req);
    }
    let outcomes = futures_util::future::join_all(requests.iter().map(|req| runner.run_attempt(req))).await;
    assert!(outcomes.iter().all(Result::is_ok), "capacity is temporary contention, not an attempt failure: {outcomes:?}");
    assert_eq!(runner.budget.as_ref().unwrap().snapshot().agent_calls, 5);
    let available = duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(1), 30).unwrap();
    assert!(matches!(available, duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(_)), "terminal attempts release every lease");
}

#[cfg(unix)]
#[tokio::test]
async fn release_capacity_waiting_run_resumes_after_another_run_releases_slot() {
    let (dir, mut runner, request) = fake_runner(r#"
cat >/dev/null
printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.001}'
"#, 1).await;
    runner.max_concurrency = 1;
    let held = match duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(1), 30).unwrap() {
        duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease) => lease,
        _ => panic!("fixture must hold the shared slot"),
    };
    let (_released, outcome) = tokio::join!(async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(runner.budget.as_ref().unwrap().snapshot().agent_calls, 0, "no call is reserved while waiting");
        duduclaw_core::concurrency_gate::release_checked(dir.path(), &held).unwrap();
    }, runner.run_attempt(&request));
    assert!(outcome.is_ok(), "a slot held by a different run must queue, not stop exploration: {outcome:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn release_capacity_wait_is_cancellable_and_does_not_take_a_lease() {
    let (dir, mut runner, request) = fake_runner("exit 99", 1).await;
    runner.max_concurrency = 1;
    let held = match duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(1), 30).unwrap() {
        duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease) => lease,
        _ => panic!("fixture must hold the shared slot"),
    };
    let budget = runner.budget.as_ref().unwrap().clone();
    let (_cancelled, outcome) = tokio::join!(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        budget.cancel();
    }, runner.run_attempt(&request));
    assert_eq!(outcome.unwrap_err(), AttemptInfraError::BudgetExhausted);
    assert_eq!(budget.snapshot().agent_calls, 0);
    assert!(duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(1), 30).unwrap().is_at_capacity());
    duduclaw_core::concurrency_gate::release_checked(dir.path(), &held).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn release_capacity_wait_obeys_attempt_deadline_without_reserving_a_call() {
    let (dir, mut runner, mut request) = fake_runner("exit 99", 1).await;
    runner.max_concurrency = 1;
    request.timeout = Duration::from_millis(100);
    let held = match duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(1), 30).unwrap() {
        duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease) => lease,
        _ => panic!("fixture must hold the shared slot"),
    };
    let started = std::time::Instant::now();
    let outcome = runner.run_attempt(&request).await;
    assert_eq!(outcome.unwrap_err(), AttemptInfraError::BudgetExhausted);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(runner.budget.as_ref().unwrap().snapshot().agent_calls, 0);
    duduclaw_core::concurrency_gate::release_checked(dir.path(), &held).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn release_capacity_dispatch_guard_delay_cannot_leave_an_unstarted_pending_call() {
    let (dir, runner, mut request) = fake_runner(r#"
touch provider-started
printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.001}'
"#, 1).await;
    request.timeout = Duration::from_millis(250);
    let lock_path = dir.path().join("dispatch_guard.json");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        duduclaw_core::with_file_lock(&lock_path, || {
            entered_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(1));
            Ok(())
        }).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let outcome = runner.run_attempt(&request).await;
    holder.join().unwrap();
    assert_eq!(outcome.unwrap_err(), AttemptInfraError::BudgetExhausted);
    assert!(!request.node_dir.join("provider-started").exists());
    let snapshot = runner.budget.as_ref().unwrap().snapshot();
    assert_eq!(snapshot.agent_calls, 0, "deadline exhaustion before spawn must not reserve a call");
    assert_eq!(snapshot.pending_calls, 0, "an unstarted provider call must not leave pending liability");
    assert_eq!(snapshot.pending_reserved_usd, 0.0);
    assert_eq!(snapshot.unknown_calls, 0);
    assert!(runner.budget.as_ref().unwrap().remaining_wall() > Duration::from_secs(20),
        "the attempt deadline, rather than the overall run budget, caused the abort");
    let available = duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(1), 30).unwrap();
    match available {
        duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease) => {
            duduclaw_core::concurrency_gate::release_checked(dir.path(), &lease).unwrap();
        }
        _ => panic!("the aborted attempt must release its worker lease"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn release_capacity_persist_delay_settles_an_unstarted_reservation_at_known_zero() {
    let (dir, runner, mut request) = fake_runner(r#"
touch provider-started
printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.001}'
"#, 1).await;
    request.timeout = Duration::from_millis(120);
    let budget = runner.budget.as_ref().unwrap();
    super::super::store::DiscoveryStore::open(dir.path()).unwrap().create_run(
        "run", "fixture", "test", "score", &"a".repeat(64), super::super::tree::Direction::Max, &budget.limits()).unwrap();
    budget.bind_run(dir.path(), "run").unwrap();
    let database = dir.path().join("discovery.db");
    let dispatch_record = dir.path().join("dispatch_guard.json");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        entered_tx.send(()).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !dispatch_record.exists() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(dispatch_record.exists(), "the attempt must pass admission before the persistence delay");
        // Shorter than budget-ledger's 200 ms busy timeout, but longer than
        // the original attempt deadline. This delays a successful reserve.
        std::thread::sleep(Duration::from_millis(160));
        connection.execute_batch("ROLLBACK").unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let outcome = runner.run_attempt(&request).await;
    holder.join().unwrap();
    assert_eq!(outcome.unwrap_err(), AttemptInfraError::BudgetExhausted);
    assert!(!request.node_dir.join("provider-started").exists());
    let snapshot = budget.snapshot();
    assert_eq!(snapshot.agent_calls, 1, "a successful no-start reservation still conservatively consumes one call slot");
    assert_eq!(snapshot.pending_calls, 0);
    assert_eq!(snapshot.pending_reserved_usd, 0.0);
    assert_eq!(snapshot.unknown_calls, 0, "known no-start must not be charged as an unknown provider call");
    assert_eq!(snapshot.spent_usd, 0.0);
    let durable = super::super::store::DiscoveryStore::open(dir.path()).unwrap()
        .load_run_budget_snapshot("run").unwrap().unwrap();
    assert_eq!(durable.pending_calls, 0, "settlement must also clear the durable pending record");
    assert_eq!(durable.spent_usd, 0.0);
}

#[cfg(unix)]
#[tokio::test]
async fn release_capacity_public_container_path_waits_at_capacity_until_cancelled() {
    let (dir, runner, request) = fake_runner("exit 99", 1).await;
    let budget = runner.budget.as_ref().unwrap().clone();
    let factory = AttemptRunnerFactory {
        home_dir: dir.path().to_path_buf(),
        attempt: AttemptSettings {
            sandbox: AttemptSandbox::Container,
            runtimes: BTreeMap::from([("claude".into(), super::super::config::AttemptRuntimeConfig {
                image: format!("fixture/image@sha256:{}", "a".repeat(64)),
                executable: "/opt/runtime/claude".into(), base_url: None, provider: None,
            })]),
            ..Default::default()
        },
        quota: Default::default(), allow_unconfined: false, operator_identity: false,
        budget: Some(budget.clone()), account_rotator: runner.account_rotator.clone(),
        extra_read_paths: vec![], max_concurrency: 4,
    };
    let container = factory.for_runtime("claude").unwrap();
    let mut held = Vec::new();
    for _ in 0..4 {
        match duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(4), 30).unwrap() {
            duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease) => held.push(lease),
            _ => panic!("fixture must hold the public factory's four shared slots"),
        }
    }
    // The real production runner must reach admission and wait. Cancellation
    // before admission ensures that no Docker or provider is invoked.
    let (_cancelled, outcome) = tokio::join!(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        budget.cancel();
    }, container.run_attempt(&request));
    assert_eq!(outcome.unwrap_err(), AttemptInfraError::BudgetExhausted,
        "AtCapacity must not masquerade as IsolationUnavailable on the public container path");
    assert_eq!(budget.snapshot().agent_calls, 0);
    assert!(duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(4), 30).unwrap().is_at_capacity());
    for lease in held { duduclaw_core::concurrency_gate::release_checked(dir.path(), &lease).unwrap(); }
}

#[tokio::test]
async fn release_capacity_shared_slot_wait_also_obeys_run_deadline() {
    use super::super::contracts::RunBudget;
    let dir = tempfile::tempdir().unwrap();
    let budget = SharedBudget::new(RunBudget { max_agent_calls: 1, max_usd: 1.0, max_wall_secs: 1, max_rounds: 1 }).unwrap();
    let held = match duduclaw_core::concurrency_gate::try_acquire_checked(dir.path(), "discovery", Some(1), 30).unwrap() {
        duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease) => lease,
        _ => panic!("fixture must hold the shared slot"),
    };
    let started = std::time::Instant::now();
    let result = acquire_attempt_slot(dir.path(), 1, &budget, started + Duration::from_secs(10)).await;
    assert!(matches!(result, Err(AttemptInfraError::BudgetExhausted)));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(budget.snapshot().agent_calls, 0);
    duduclaw_core::concurrency_gate::release_checked(dir.path(), &held).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn fake_cli_uses_workspace_and_records_its_actual_cost() {
    let (_dir, runner, req) = fake_runner(r#"
cat > prompt.txt
pwd > actual-cwd
printf '%s\n' '{"type":"result","result":"完成","total_cost_usd":0.25,"usage":{"input_tokens":8,"output_tokens":4}}'
"#, 1).await;
    let out = runner.run_attempt(&req).await.unwrap();
    assert_eq!(out.cost.usd, 0.25);
    assert_eq!(out.final_text, "完成");
    assert_eq!(std::fs::read_to_string(req.node_dir.join("prompt.txt")).unwrap(), req.prompt);
    assert_eq!(std::fs::read_to_string(req.node_dir.join("actual-cwd")).unwrap().trim(),
        req.node_dir.canonicalize().unwrap().to_str().unwrap());
    assert_eq!(runner.budget.as_ref().unwrap().snapshot().agent_calls, 1);
    assert_eq!(out.isolation, IsolationBackend::None);
}

#[cfg(unix)]
#[tokio::test]
async fn first_rate_limit_cancels_the_shared_run_without_retry_or_new_spawns() {
    let (dir, runner, req) = fake_runner(r#"
state="$(dirname "$0")"
cat > /dev/null
printf 'spawn\n' >> "$state/spawns"
printf '%s\n' '{"type":"result","is_error":true,"result":"You have hit your limit; usage limit reached","total_cost_usd":0.03}'
sleep 30
"#, 3).await;
    assert_eq!(runner.run_attempt(&req).await.err(), Some(AttemptInfraError::RateLimited));
    let budget = runner.budget.as_ref().unwrap();
    assert!(budget.rate_limit_stopped());
    assert!(budget.is_cancelled());
    assert_eq!(budget.snapshot().agent_calls, 1);
    assert!((budget.snapshot().spent_usd - 0.03).abs() < 1e-9);
    assert_eq!(runner.run_attempt(&req).await.err(), Some(AttemptInfraError::BudgetExhausted));
    assert_eq!(std::fs::read_to_string(dir.path().join("tools/spawns")).unwrap(), "spawn\n");
}

#[cfg(unix)]
#[tokio::test]
async fn rate_limit_error_fields_cancel_before_a_cli_that_keeps_running_can_retry() {
    for event in [
        r#"{"type":"result","is_error":true,"error":"rate_limit_error: 429"}"#,
        r#"{"type":"result","is_error":true,"errors":["usage limit reached"]}"#,
        r#"{"type":"assistant","error":{"type":"rate_limit_error","message":"429"}}"#,
    ] {
        let script = format!("cat > /dev/null\nprintf '%s\\n' '{event}'\nsleep 30");
        let (_dir, runner, req) = fake_runner(&script, 3).await;
        let started = std::time::Instant::now();
        assert_eq!(runner.run_attempt(&req).await.err(), Some(AttemptInfraError::RateLimited));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(runner.budget.as_ref().unwrap().rate_limit_stopped());
        assert_eq!(runner.budget.as_ref().unwrap().snapshot().agent_calls, 1);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn infra_retry_resends_identical_prompt_and_counts_failed_call_cost() {
    let (dir, runner, req) = fake_runner(r#"
state="$(dirname "$0")"
cat >> "$state/prompts.txt"
if [ ! -f "$state/retried" ]; then
  touch "$state/retried"
  touch dirty-marker
  printf '%s\n' '{"type":"result","is_error":true,"result":"API Error: 503 temporarily unavailable","total_cost_usd":0.03}'
else
  if [ -f dirty-marker ]; then
    printf '%s\n' '{"type":"result","is_error":true,"result":"dirty workspace survived retry","total_cost_usd":0.01}'
    exit 1
  fi
  printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.1,"usage":{"input_tokens":1,"output_tokens":1}}'
fi
"#, 2).await;
    let out = runner.run_attempt(&req).await.unwrap();
    assert_eq!(out.infra_retries, 1);
    assert!((out.cost.usd - 0.13).abs() < 1e-9);
    assert_eq!(std::fs::read_to_string(dir.path().join("tools/prompts.txt")).unwrap(), req.prompt.repeat(2));
    assert!(!req.node_dir.join("dirty-marker").exists());
    assert_eq!(runner.budget.as_ref().unwrap().snapshot().agent_calls, 2);
}

#[cfg(unix)]
#[tokio::test]
async fn exhausted_budget_refuses_before_spawning_and_strict_pool_never_spills() {
    let (_dir, runner, mut req) = fake_runner("touch launched", 1).await;
    req.account_pool = vec!["missing-pool".into()];
    assert_eq!(runner.run_attempt(&req).await.err(), Some(AttemptInfraError::NoAccount));
    assert!(!req.node_dir.join("launched").exists());
    req.account_pool = vec!["test".into()];
    runner.budget.as_ref().unwrap().reserve_call().unwrap();
    assert_eq!(runner.run_attempt(&req).await.err(), Some(AttemptInfraError::BudgetExhausted));
    assert!(!req.node_dir.join("launched").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn argv_exposes_only_explicit_completed_workspaces() {
    let (dir, runner, mut req) = fake_runner(r#"
cat > prompt.txt
printf '%s\n' "$@" > argv.txt
printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.01}'
"#, 1).await;
    let completed = req.run_dir.join("r1/b1/a0/ws");
    let hidden = req.run_dir.join("r1/b2/a0/ws");
    std::fs::create_dir_all(&completed).unwrap(); std::fs::create_dir_all(&hidden).unwrap();
    req.read_workspaces = vec![completed.clone()];
    runner.run_attempt(&req).await.unwrap();
    let argv = std::fs::read_to_string(req.node_dir.join("argv.txt")).unwrap();
    let values = argv.lines().collect::<Vec<_>>();
    let read = values.windows(2).filter(|pair| pair[0] == "--add-dir").map(|pair|pair[1]).collect::<Vec<_>>();
    assert_eq!(read, vec![completed.canonicalize().unwrap().to_str().unwrap()]);
    assert!(!read.iter().any(|path| *path == req.run_dir.to_str().unwrap() || *path == hidden.to_str().unwrap()));
    assert!(dir.path().exists());
}

#[cfg(unix)]
#[tokio::test]
async fn refuses_ledger_or_run_root_in_read_workspace_list_before_spawn() {
    let (_dir, runner, mut req) = fake_runner("touch launched", 2).await;
    req.read_workspaces = vec![req.run_dir.clone()];
    assert_eq!(runner.run_attempt(&req).await.err(), Some(AttemptInfraError::IsolationUnavailable));
    assert!(!req.node_dir.join("launched").exists());
}

#[test]
fn interrupted_or_erroring_cli_preserves_known_token_lower_bound_and_unknown_liability() {
    let partial=parse_stream("{\"type\":\"assistant\",\"message\":{\"id\":\"one\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20}}}\n{\"type\":\"result\",\"is_error\":true,\"error\":{\"code\":429}}\n");
    let cost=call_cost("claude-haiku-4-5",&partial);
    assert!(cost.usd>0.0 && cost.usd.is_finite());assert_eq!(cost.source,CostSource::Unknown);
    assert_eq!(partial.usage.unwrap().output_tokens,20);
    let repeated=parse_stream("{\"type\":\"assistant\",\"message\":{\"id\":\"one\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20}}}\n{\"type\":\"assistant\",\"message\":{\"id\":\"one\",\"usage\":{\"input_tokens\":100,\"output_tokens\":20}}}\n");
    assert_eq!(repeated.usage.unwrap().input_tokens,100);
}

#[test]
fn attempt_lease_drop_is_bounded_when_another_process_holds_the_state_lock() {
    let home=tempfile::tempdir().unwrap();
    let lease=match duduclaw_core::concurrency_gate::try_acquire_checked(home.path(),"discovery",Some(1),30).unwrap() {
        duduclaw_core::concurrency_gate::AcquireOutcome::Admitted(lease)=>lease,
        _=>panic!("fresh slot should admit"),
    };
    let state=home.path().join("concurrency_leases.json");
    let (entered_tx,entered_rx)=std::sync::mpsc::channel();
    let (release_tx,release_rx)=std::sync::mpsc::channel();
    let holder=std::thread::spawn(move||duduclaw_core::with_file_lock(&state,||{
        entered_tx.send(()).unwrap();let _=release_rx.recv_timeout(std::time::Duration::from_secs(2));Ok(())
    }).unwrap());
    entered_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    let started=std::time::Instant::now();drop(LeaseGuard {home:home.path().into(),lease});
    let elapsed=started.elapsed();let _=release_tx.send(());holder.join().unwrap();
    assert!(elapsed<std::time::Duration::from_secs(1),"Drop blocked on persistent state: {elapsed:?}");
}

fn retry_seed_fixture()->(tempfile::TempDir,PathBuf) {
    let home=tempfile::tempdir().unwrap();
    let node=home.path().join("discovery/runs/run-1/r1/b0/a0/ws");
    super::super::workspace::create_private_directory(&node).unwrap();
    std::fs::write(node.join("input.txt"),vec![b'x';4096]).unwrap();
    (home,node)
}

fn capture_fixture_retry_seed(home:&Path,node:&Path)->Result<RetrySeed,AttemptInfraError> {
    capture_fixture_retry_seed_with_limits(home,node,crate::discovery::attempt_container::QuotaLimits::default())
}
fn capture_fixture_retry_seed_with_limits(home:&Path,node:&Path,quota:crate::discovery::attempt_container::QuotaLimits)->Result<RetrySeed,AttemptInfraError> {
    let request=AttemptRequest {run_id:"run-1".into(),cell_id:"r1-b0-a0".into(),run_dir:home.join("discovery/runs/run-1"),node_dir:node.into(),read_workspaces:vec![],prompt:String::new(),agent_id:"test".into(),model:None,timeout:Duration::from_secs(5),max_turns:1,account_pool:vec![]};
    RetrySeed::capture(home,&request,quota,std::time::Instant::now()+request.timeout)
}

#[test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
fn retry_seed_is_private_and_associated_with_its_run_for_retention() {
    let (home,node)=retry_seed_fixture();
    let seed=capture_fixture_retry_seed(home.path(),&node).unwrap();
    let association=home.path().canonicalize().unwrap().join("discovery/retry-seeds/run-1");
    assert!(seed.workspace.starts_with(&association),"retry seed escaped the run association");
    #[cfg(unix)] {
        use std::os::unix::fs::{MetadataExt,PermissionsExt};
        let meta=std::fs::symlink_metadata(seed._private.path()).unwrap();
        assert_eq!(meta.uid(),unsafe{libc::geteuid()});assert_eq!(meta.permissions().mode()&0o777,0o700);
    }
    let private=seed._private.path().to_path_buf();drop(seed);assert!(!private.exists());
}

#[test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
fn retained_retry_seeds_are_visible_to_global_and_per_run_quota_accounting() {
    let (home,node)=retry_seed_fixture();
    let discovery=home.path().join("discovery");
    let before=super::super::workspace::tree_bytes(&discovery).unwrap();
    let source_bytes=super::super::workspace::tree_bytes(&node).unwrap();
    let first=capture_fixture_retry_seed(home.path(),&node).unwrap();
    assert!(super::super::workspace::tree_bytes(&discovery).unwrap()>=before+source_bytes,"active retry seed omitted from discovery quota");
    let second=capture_fixture_retry_seed(home.path(),&node).unwrap();
    assert!(super::super::workspace::tree_bytes(&discovery).unwrap()>=before+source_bytes*2,"concurrent retry seeds omitted from discovery quota");
    let association=home.path().join("discovery/retry-seeds/run-1");
    assert_eq!(super::super::workspace::tree_bytes(&association).unwrap(),source_bytes*2);
    drop(first);drop(second);
    assert_eq!(super::super::workspace::tree_bytes(&association).unwrap(),0);
}

#[test]
fn observed_model_ignores_empty_whitespace_and_synthetic_followup_events() {
    for invalid in [""," \t ","<synthetic>","  <synthetic>  "] {
        let first=serde_json::json!({"type":"assistant","message":{"model":"claude-haiku-4-5"}});
        let later=serde_json::json!({"type":"assistant","message":{"model":invalid}});
        let summary=parse_stream(&format!("{first}\n{later}\n"));
        assert_eq!(summary.model.as_deref(),Some("claude-haiku-4-5"),"unobserved model replaced provider model");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn streamed_empty_model_does_not_replace_the_last_actual_provider_model() {
    let (_home,runner,request)=fake_runner(r#"
cat >/dev/null
printf '%s\n' '{"type":"assistant","message":{"model":"claude-haiku-4-5"}}'
printf '%s\n' '{"type":"assistant","message":{"model":"   "}}'
printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.01}'
"#,1).await;
    assert_eq!(runner.run_attempt(&request).await.unwrap().model,"claude-haiku-4-5");
}

#[test]
fn every_family_runs_in_a_container_under_the_host_contract() {
    let mut attempt=AttemptSettings::default();
    for name in ["claude","codex","gemini","antigravity","grok","openai-compat"] {
        attempt.runtimes.insert(name.into(),super::super::config::AttemptRuntimeConfig {
            image:format!("test@sha256:{}","a".repeat(64)),executable:format!("/opt/{name}").into(),base_url:None,provider:None,
        });
    }
    let factory=AttemptRunnerFactory {home_dir:"/private/host".into(),attempt:attempt.clone(),
        quota:crate::discovery::attempt_container::QuotaLimits::default(),allow_unconfined:false,operator_identity:false,
        budget:None,account_rotator:None,extra_read_paths:vec![],max_concurrency:1};
    for runtime in ["claude","codex","gemini","antigravity","agy","grok","openai-compat","openai_compat"] {
        assert!(factory.for_runtime(runtime).is_ok(),"{runtime} must be accepted under the host-side contract");
    }
    // An unconfigured image still fails closed; strict dollar ceilings stay refused for everyone.
    attempt.runtimes.remove("grok");
    assert_eq!(AttemptRunnerFactory::check_runtime_capability(&attempt,"grok").err(),Some(AttemptInfraError::IsolationUnavailable));
    attempt.strict_usd=true;
    for runtime in ["claude","codex","antigravity"] {
        assert!(matches!(AttemptRunnerFactory::check_runtime_capability(&attempt,runtime),Err(AttemptInfraError::StrictUsdUnsupported(_))));
    }
}

#[test]
fn unpriced_models_are_unknown_outside_claude() {
    use super::super::attempt_adapter::RuntimeFamily;
    let summary=parse_stream(r#"{"type":"result","result":"done","usage":{"input_tokens":1000,"output_tokens":10}}"#);
    assert!(summary.complete);
    let claude=call_cost_for(RuntimeFamily::Claude,"unpriced-model-x",&summary);
    assert_eq!(claude.source,CostSource::Estimated);assert!(claude.usd>0.0);
    for family in [RuntimeFamily::Codex,RuntimeFamily::Gemini,RuntimeFamily::Antigravity,RuntimeFamily::Grok,RuntimeFamily::OpenAiCompat] {
        let cost=call_cost_for(family,"unpriced-model-x",&summary);
        assert_eq!(cost.source,CostSource::Unknown,"{family:?}");assert!(cost.usd.is_nan());
    }
    let reported=parse_stream(r#"{"type":"result","result":"done","total_cost_usd":0.02,"usage":{"input_tokens":1,"output_tokens":1}}"#);
    assert_eq!(call_cost_for(RuntimeFamily::Grok,"unpriced-model-x",&reported).source,CostSource::Reported);
}

/// Container runner against a scripted container client: no Docker, no provider.
/// `seats` are (id, oauth, secret): an API key, or for OAuth the login document.
/// The client logs every key it receives on `create` (the only command that
/// carries secrets) and runs `tail` after replaying `stream` on `start`.
#[cfg(unix)]
async fn scripted(family:&str,provider:&str,seats:&[(&str,bool,&str)],stream:&str,tail:&str,max_turns:u32)
    ->(tempfile::TempDir,Box<dyn AttemptRunner>,AttemptRequest,SharedBudget) {
    use duduclaw_agent::account_rotator::{Account, AccountRotator, RotationStrategy};
    use super::super::contracts::RunBudget;
    use std::os::unix::fs::PermissionsExt;
    let dir=tempfile::tempdir().unwrap();
    let node=dir.path().join("discovery/runs/run/r1/b0/a0/ws");
    super::super::workspace::create_private_directory(&node).unwrap();
    let tools=dir.path().join("tools");std::fs::create_dir(&tools).unwrap();
    std::fs::write(tools.join("events.ndjson"),stream).unwrap();
    let client=tools.join("container-client");
    std::fs::write(&client,format!("#!/bin/sh\nd=\"${{0%/*}}\"\ncase \"$1\" in\ncreate) printf '%s\\n' \"$OPENAI_API_KEY$XAI_API_KEY$GEMINI_API_KEY$DUDU_CREDENTIAL_DOC\" >> \"$d/keys\"; printf '%064d\\n' 1;;\nstart) printf 'start\\n' >> \"$d/starts\"; /bin/cat >/dev/null; /bin/cat \"$d/events.ndjson\"; {tail};;\nrm) ;;\n*) exit 3;;\nesac\n")).unwrap();
    std::fs::set_permissions(&client,std::fs::Permissions::from_mode(0o700)).unwrap();
    super::super::attempt_container::DOCKER_PROGRAM.with(|p|*p.borrow_mut()=Some(client));
    let rotator=AccountRotator::new(RotationStrategy::RoundRobin,0);
    for (id,oauth,secret) in seats {
        let mut account:Account=serde_json::from_value(serde_json::json!({
            "id":id,"auth_method":if *oauth {"o_auth"} else {"api_key"},"provider":provider,"priority":1,"monthly_budget_cents":1000})).unwrap();
        account.is_healthy=true;
        if *oauth { account.oauth_token=(!secret.is_empty()).then(||secret.to_string()); } else { account.api_key=(*secret).into(); }
        rotator.push_account_for_test(account).await;
    }
    let budget=SharedBudget::new(RunBudget {max_agent_calls:3,max_usd:5.0,max_wall_secs:60,max_rounds:1}).unwrap();
    let factory=AttemptRunnerFactory {home_dir:dir.path().to_path_buf(),
        attempt:AttemptSettings {sandbox:AttemptSandbox::Container,runtimes:BTreeMap::from([(family.into(),super::super::config::AttemptRuntimeConfig {
            image:format!("fixture/image@sha256:{}","a".repeat(64)),executable:format!("/opt/runtime/{family}").into(),base_url:None,provider:None,
        })]),..Default::default()},
        quota:Default::default(),allow_unconfined:false,operator_identity:false,budget:Some(budget.clone()),
        account_rotator:Some(Arc::new(rotator)),extra_read_paths:vec![],max_concurrency:2};
    let runner=factory.for_runtime(family).unwrap();
    let request=AttemptRequest {run_id:"run".into(),cell_id:"r1-b0-a0".into(),node_dir:node,run_dir:dir.path().join("discovery/runs/run"),
        read_workspaces:vec![],prompt:"identical prompt".into(),agent_id:"test".into(),model:Some("unpriced-fixture-model".into()),
        timeout:Duration::from_secs(10),max_turns,account_pool:seats.iter().map(|(id,_,_)|id.to_string()).collect()};
    (dir,runner,request,budget)
}
#[cfg(unix)]
async fn scripted_container(family:&str,provider:&str,key:&str,events:&[serde_json::Value],max_turns:u32)
    ->(tempfile::TempDir,Box<dyn AttemptRunner>,AttemptRequest,SharedBudget) {
    let stream=events.iter().map(|e|e.to_string()+"\n").collect::<String>();
    scripted(family,provider,&[("test",false,key)],&stream,"/bin/sleep 20",max_turns).await
}
#[cfg(unix)]
fn read_lines(dir:&tempfile::TempDir,name:&str)->Vec<String> {
    std::fs::read_to_string(dir.path().join("tools").join(name)).unwrap_or_default().lines().map(str::to_owned).collect()
}

#[cfg(unix)]
#[tokio::test]
async fn codex_host_step_limit_is_a_scored_outcome_without_retry() {
    use serde_json::json;
    let events=[json!({"type":"thread.started","thread_id":"t"}),json!({"type":"turn.started"}),
        json!({"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I will edit the parser."}}),
        json!({"type":"item.started","item":{"id":"item_1","type":"command_execution","status":"in_progress"}}),
        json!({"type":"item.completed","item":{"id":"item_1","type":"command_execution","exit_code":0}}),
        json!({"type":"item.started","item":{"id":"item_2","type":"file_change","status":"in_progress"}}),
        json!({"type":"item.completed","item":{"id":"item_9","type":"mcp_tool_call"}})];
    let (dir,runner,request,budget)=scripted_container("codex","openai","fake-key",&events,1).await;
    let started=std::time::Instant::now();
    let outcome=runner.run_attempt(&request).await.unwrap();
    assert!(started.elapsed()<Duration::from_secs(8),"the host stops the container at the ceiling");
    assert_eq!(outcome.runtime,"codex");assert!(!outcome.timed_out);assert_eq!(outcome.infra_retries,0);
    assert!(outcome.model.is_empty(),"codex reports no model; none is invented");
    assert_eq!(outcome.final_text,"I will edit the parser.","the step-limit result carries the last agent message");
    assert_eq!(outcome.cost.usd_source,CostSource::Unknown,"codex reports usage only at turn end");
    assert_eq!(std::fs::read_to_string(dir.path().join("tools/starts")).unwrap(),"start\n");
    assert_eq!(budget.snapshot().agent_calls,1);
}

#[cfg(unix)]
#[tokio::test]
async fn tool_surface_violation_stops_without_retry_and_names_a_sanitised_tool() {
    use serde_json::json;
    let events=[json!({"type":"system","subtype":"init","model":"grok-4.7","tools":["read_file"],"mcp_servers":[]}),
        json!({"type":"assistant","message":{"id":"msg_0","model":"grok-4.7","content":[{"type":"tool_use","name":"read_file"},{"type":"tool_use","name":"search_tool;rm -rf /"}],
            "usage":{"input_tokens":10,"output_tokens":2}}})];
    let (dir,runner,request,budget)=scripted_container("grok","xai","fake-key",&events,4).await;
    let error=runner.run_attempt(&request).await.unwrap_err();
    assert_eq!(error,AttemptInfraError::ToolSurfaceViolation {runtime:"grok".into(),tool:"search_toolrm-rf".into()});
    assert_eq!(super::super::stop_code::classify("degraded",Some(&error.to_string())),Some("tool_violation"));
    assert_eq!(std::fs::read_to_string(dir.path().join("tools/starts")).unwrap(),"start\n","a violation is never retried");
    assert_eq!(budget.snapshot().agent_calls,1);
    let audit=std::fs::read_to_string(dir.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("discovery_tool_surface_violation"),"violation must be audited");
}

#[cfg(unix)]
#[tokio::test]
async fn antigravity_host_step_limit_bills_every_seen_generation() {
    use serde_json::json;
    let step=|index:u64,state:&str,kind:&str,extra:serde_json::Value| {
        let mut step=json!({"step_index":index,"state":state,"step_type":kind});
        for (k,v) in extra.as_object().unwrap() { step[k]=v.clone(); }
        json!({"event":"step_update","step_update":step})
    };
    let usage=|i:u64,o:u64|json!({"usage":{"input_tokens":i,"output_tokens":o,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":i+o}});
    let events=[json!({"event":"init","init":{"cwd":"/w","tools":["run_command"],"model":"gemini-3.8-flash-high"}}),
        step(1,"DONE","agent_response",usage(100,10)),
        step(2,"ACTIVE","tool",json!({"tool_name":"write_to_file"})),step(2,"DONE","tool",json!({"tool_name":"write_to_file"})),
        step(3,"ACTIVE","agent_response",json!({"text_delta":"more"})),step(3,"DONE","agent_response",usage(50,5))];
    let (_dir,runner,request,_budget)=scripted_container("antigravity","gemini","fake-key",&events,1).await;
    let outcome=runner.run_attempt(&request).await.unwrap();
    assert_eq!(outcome.runtime,"antigravity");assert_eq!(outcome.model,"gemini-3.8-flash-high");
    assert_eq!((outcome.cost.input_tokens,outcome.cost.output_tokens),(150,15),"the stopping generation is billed too");
    assert_eq!(outcome.final_text,"more");
    assert_eq!(outcome.cost.usd_source,CostSource::Unknown,"an unpriced non-Claude model is not estimated with Claude prices");
}

#[cfg(unix)]
#[tokio::test]
async fn container_attempt_without_family_credentials_refuses_before_spawn() {
    let (dir,runner,request,budget)=scripted_container("grok","xai","",&[],1).await;
    assert_eq!(runner.run_attempt(&request).await.err(),Some(AttemptInfraError::NoAccount));
    assert!(!dir.path().join("tools/starts").exists());assert_eq!(budget.snapshot().agent_calls,0);
}

#[test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
fn retry_seed_refuses_run_and_global_exhaustion_before_copy_and_keeps_other_calls_accounted() {
    let (home,node)=retry_seed_fixture();
    let source=super::super::workspace::tree_bytes(&node).unwrap();
    let quota=crate::discovery::attempt_container::QuotaLimits {max_run_bytes:source*2,max_total_bytes:1024*1024};
    let first=capture_fixture_retry_seed_with_limits(home.path(),&node,quota).unwrap();
    assert!(capture_fixture_retry_seed_with_limits(home.path(),&node,quota).is_err());
    let root=home.path().join("discovery");
    let exact_global=crate::discovery::attempt_container::QuotaLimits {max_run_bytes:1024*1024,max_total_bytes:super::super::workspace::tree_bytes(&root).unwrap()};
    assert!(capture_fixture_retry_seed_with_limits(home.path(),&node,exact_global).is_err());
    assert_eq!(super::super::workspace::tree_bytes(&root.join("retry-seeds/run-1")).unwrap(),source);
    first.guard.verify().unwrap();
}

#[test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
fn retry_restore_direct_copy_preserves_private_workspace_and_reuses_the_reserved_seed_space() {
    let (home,node)=retry_seed_fixture();let node=node.canonicalize().unwrap();
    let source=super::super::workspace::tree_bytes(&node).unwrap();
    let limits=crate::discovery::attempt_container::QuotaLimits {max_run_bytes:source*2+64,max_total_bytes:1024*1024};
    let seed=capture_fixture_retry_seed_with_limits(home.path(),&node,limits).unwrap();
    std::fs::write(node.join("dirty-marker"),b"dirty").unwrap();
    seed.restore(&node).unwrap();
    assert!(!node.join("dirty-marker").exists());assert_eq!(std::fs::read(node.join("input.txt")).unwrap(),vec![b'x';4096]);
    assert_eq!(super::super::workspace::tree_bytes(&home.path().join("discovery")).unwrap(),source*2);
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&node).unwrap().permissions().mode()&0o777,0o700);}
    let other=home.path().join("discovery/runs/run-1/r1/b1/a0/ws");
    super::super::workspace::create_private_directory(&other).unwrap();assert!(seed.restore(&other).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn unconfined_operator_attempt_still_requires_explicit_disk_quota_and_never_spawns_when_exhausted() {
    let (_home,mut runner,request)=fake_runner("touch launched",1).await;
    runner.quota=crate::discovery::attempt_container::QuotaLimits {max_run_bytes:0,max_total_bytes:1024*1024};
    assert!(runner.run_attempt(&request).await.is_err());
    assert!(!request.node_dir.join("launched").exists());
    assert_eq!(runner.budget.as_ref().unwrap().snapshot().agent_calls,0);
}

#[cfg(unix)]
#[tokio::test]
async fn operator_runtime_and_hook_configuration_share_the_run_snapshot_quota() {
    let (_home,runner,request)=fake_runner(r#"
cat > /dev/null
printf '%s\n' "$CLAUDE_CODE_TMPDIR" > runtime-private.txt
printf '%s\n' "$@" > argv-private.txt
printf '%s\n' '{"type":"result","result":"done","total_cost_usd":0.01}'
"#,1).await;
    runner.run_attempt(&request).await.unwrap();
    let expected=runner.home_dir.canonicalize().unwrap().join("discovery/attempt-snapshots/run");
    let runtime=std::fs::read_to_string(request.node_dir.join("runtime-private.txt")).unwrap();
    assert!(Path::new(runtime.trim()).starts_with(&expected));assert!(!Path::new(runtime.trim()).exists());
    let argv=std::fs::read_to_string(request.node_dir.join("argv-private.txt")).unwrap();
    let values=argv.lines().collect::<Vec<_>>();let settings=values.windows(2).find(|pair|pair[0]=="--settings").unwrap()[1];
    assert!(Path::new(settings).starts_with(&expected));assert!(!Path::new(settings).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn grok_native_max_turns_after_tool_use_is_a_scored_reported_outcome() {
    use serde_json::json;
    let stream=[json!({"type":"system","subtype":"init","model":"grok-4.7","tools":["write"],"mcp_servers":[]}),
        json!({"type":"assistant","message":{"id":"msg_0","model":"grok-4.7","content":[{"type":"thinking"},{"type":"tool_use","name":"write","id":"c0"}],
            "usage":{"input_tokens":29406,"output_tokens":756,"cache_read_input_tokens":1664,"cache_creation_input_tokens":0}}}),
        json!({"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":2,"stop_reason":"cancelled","total_cost_usd":0.03283448,
            "usage":{"input_tokens":35627,"output_tokens":817,"cache_read_input_tokens":40832,"cache_creation_input_tokens":0},"errors":["Reached the maximum number of turns"]})]
        .iter().map(|e|e.to_string()+"\n").collect::<String>();
    let (dir,runner,request,_budget)=scripted("grok","xai",&[("test",false,"fake-key")],&stream,"exit 1",2).await;
    let outcome=runner.run_attempt(&request).await.unwrap();
    assert_eq!(outcome.runtime,"grok");assert_eq!(outcome.model,"grok-4.7");
    assert_eq!(outcome.cost.usd_source,CostSource::Reported);assert!((outcome.cost.usd-0.03283448).abs()<1e-12);
    assert_eq!(read_lines(&dir,"starts").len(),1);
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_credentials_are_never_replayed_and_the_next_account_is_tried() {
    use serde_json::json;
    // Container probe shape: codex 0.159.2 with a fake key.
    let mut stream=String::new();
    for n in 1..=3 { stream.push_str(&json!({"type":"error","message":format!("Reconnecting... {n}/5 (unexpected status 401 Unauthorized: Incorrect API key provided: fake****)")}).to_string()); stream.push('\n'); }
    stream.push_str(&json!({"type":"item.completed","item":{"id":"item_0","type":"error","message":"unexpected status 401 Unauthorized"}}).to_string());stream.push('\n');
    stream.push_str(&json!({"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized: Incorrect API key provided: fake****"}}).to_string());stream.push('\n');
    let (dir,runner,request,budget)=scripted("codex","openai",&[("only",false,"key-one")],&stream,"exit 1",3).await;
    assert_eq!(runner.run_attempt(&request).await.err(),Some(AttemptInfraError::NoAccount));
    assert_eq!(read_lines(&dir,"starts").len(),1,"a dead credential is not replayed");
    assert!(!budget.rate_limit_stopped());
    let (dir,runner,request,_budget)=scripted("codex","openai",&[("first",false,"key-one"),("second",false,"key-two")],&stream,"exit 1",3).await;
    assert_eq!(runner.run_attempt(&request).await.err(),Some(AttemptInfraError::NoAccount));
    assert_eq!(read_lines(&dir,"keys"),["key-one","key-two"],"the second account is tried once");
}

#[cfg(unix)]
#[tokio::test]
async fn unusable_seats_are_skipped_for_a_usable_account_in_the_pool() {
    use serde_json::json;
    let stream=[json!({"type":"item.started","item":{"id":"i1","type":"command_execution"}}),
        json!({"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"DONE"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":2}})]
        .iter().map(|e|e.to_string()+"\n").collect::<String>();
    let (dir,runner,request,_budget)=scripted("codex","openai",&[("bad-doc",true,"not json"),("no-doc",true,""),("key",false,"key-valid")],&stream,"exit 0",3).await;
    let outcome=runner.run_attempt(&request).await.unwrap();
    assert_eq!(outcome.final_text,"DONE");
    assert_eq!(read_lines(&dir,"keys"),["key-valid"]);
    // A valid login document is delivered instead of a key.
    let (dir,runner,request,_budget)=scripted("codex","openai",&[("seat",true,"{\"auth_mode\": \"chatgpt\"}")],&stream,"exit 0",3).await;
    runner.run_attempt(&request).await.unwrap();
    assert_eq!(read_lines(&dir,"keys"),["{\"auth_mode\":\"chatgpt\"}"]);
}

#[cfg(unix)]
#[tokio::test]
async fn unparsable_stdout_beyond_three_lines_voids_the_attempt() {
    use serde_json::json;
    let tail=[json!({"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"DONE"}}),
        json!({"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":2}})]
        .iter().map(|e|e.to_string()+"\n").collect::<String>();
    let tolerated=format!("warning: banner\n\n   \n[1,2]\nnot json either\n{tail}");
    let (_dir,runner,request,_budget)=scripted("codex","openai",&[("test",false,"k")],&tolerated,"exit 0",3).await;
    assert_eq!(runner.run_attempt(&request).await.unwrap().final_text,"DONE","three non-JSON lines and blank lines are tolerated");
    let voided=format!("a\nb\nc\nd\n{tail}");
    let (dir,runner,request,_budget)=scripted("codex","openai",&[("test",false,"k")],&voided,"exit 0",3).await;
    assert_eq!(runner.run_attempt(&request).await.err(),Some(AttemptInfraError::ToolSurfaceViolation {runtime:"codex".into(),tool:"unparsable_stream".into()}));
    assert_eq!(read_lines(&dir,"starts").len(),1);
}

#[cfg(unix)]
#[tokio::test]
async fn supervisor_setup_failure_is_a_spawn_error_without_retry() {
    let (dir,runner,request,budget)=scripted("grok","xai",&[("test",false,"k")],"","exit 125",3).await;
    assert_eq!(runner.run_attempt(&request).await.err(),Some(AttemptInfraError::Spawn("attempt supervisor setup failed".into())));
    assert_eq!(read_lines(&dir,"starts").len(),1);assert_eq!(budget.snapshot().agent_calls,1);
}

#[cfg(unix)]
#[tokio::test]
async fn antigravity_forbidden_tool_still_active_at_stream_end_is_a_violation() {
    use serde_json::json;
    let stream=[json!({"event":"step_update","step_update":{"step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"search_web","tool_info":{"name":"search_web"}}})]
        .iter().map(|e|e.to_string()+"\n").collect::<String>();
    let (_dir,runner,request,_budget)=scripted("antigravity","gemini",&[("test",false,"k")],&stream,"exit 0",3).await;
    assert_eq!(runner.run_attempt(&request).await.err(),Some(AttemptInfraError::ToolSurfaceViolation {runtime:"antigravity".into(),tool:"search_web".into()}));
}

/// R1 (2026-10): resolving a configured `gemini` attempt runtime still works
/// and consults the Discovery read-side deprecation notice; a live runtime
/// does not.
#[test]
fn discovery_gemini_attempt_runtime_consults_the_deprecation_notice() {
    use crate::runtime_config::{CONSULTED_NOTICES,DeprecatedRuntimeSource};
    let mut attempt=AttemptSettings {sandbox:AttemptSandbox::Container,..Default::default()};
    for name in ["gemini","codex"] {
        attempt.runtimes.insert(name.into(),super::super::config::AttemptRuntimeConfig {
            image:format!("test@sha256:{}","a".repeat(64)),executable:format!("/opt/{name}").into(),base_url:None,provider:None,
        });
    }
    CONSULTED_NOTICES.with(|n|n.borrow_mut().clear());
    assert!(AttemptRunnerFactory::check_runtime_capability(&attempt,"codex").is_ok());
    assert!(CONSULTED_NOTICES.with(|n|n.borrow().is_empty()));
    assert!(AttemptRunnerFactory::check_runtime_capability(&attempt,"gemini").is_ok());
    let consulted=CONSULTED_NOTICES.with(|n|n.borrow().clone());
    assert_eq!(consulted,vec![("gemini",DeprecatedRuntimeSource::DiscoveryAttempt,None)]);
}
