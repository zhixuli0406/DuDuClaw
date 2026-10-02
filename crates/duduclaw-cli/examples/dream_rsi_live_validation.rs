//! V2 development driver. All DuDuClaw data is created in a fresh private /tmp home.
//! F2 uses a mock branch executor and the production manual promotion handler.
//! F6 uses the production scheduler. A1 opt-in uses the production dispatcher and judge.
//! This is evidence collection, not an OS sandbox or a production deployment.

use std::{collections::BTreeMap, error::Error, fs, io, path::{Path, PathBuf}, sync::Arc, time::{Duration, Instant}};
use chrono::Utc;
use clap::Parser;
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::{traits::MemoryEngine, types::{MemoryEntry, MemoryLayer}};
use duduclaw_fork::{BranchRow, CopyPolicy, ForkRow, ForkStore};
use duduclaw_gateway::{dispatch_engine::{DispatchEngine, GoalAcceptanceCaller, LlmAcceptanceJudge}, goal_loop::{GoalLoopConfig, GoalLoopDriver}, message_queue::MessageQueue, task_store::{TaskRow, TaskStore}};
use serde_json::{Value, json};
use tokio::sync::RwLock;

const AGENT: &str = "v2-worker";
const TASK: &str = "v2-goal";
const PRIVATE_MCP_SCOPE: &str = "admin";
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
struct Args {
    /// offline = F2 + F6; f2, f6, a1, or all are also supported.
    #[arg(long, default_value = "offline")]
    phase: String,
    /// Explicit opt-in to up to two real Haiku CLI calls (worker + judge).
    #[arg(long)]
    a1_live: bool,
    /// Real fork_run background executor and manual adoption, in its own run.
    #[arg(long)]
    f2_live: bool,
    /// Absolute path to the real, already authenticated Claude CLI.
    #[arg(long)]
    claude: Option<PathBuf>,
    /// Absolute path to the built duduclaw CLI, used by the production MCP server.
    #[arg(long)]
    duduclaw: Option<PathBuf>,
}

fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(io::Error::other(message).into()) }
}

fn emit(home: &Path, name: &str, evidence: Value) -> Result<()> {
    fs::write(home.join(format!("evidence-{name}.json")), serde_json::to_vec_pretty(&evidence)?)?;
    println!("{}", json!({"phase":name,"evidence":evidence}));
    Ok(())
}

fn finish_live_guard(home: &Path, failed: bool) -> io::Result<()> {
    duduclaw_core::with_file_lock(&home.join("live-call-lock"), || {
        let stop = home.join("live-stop");
        // Preserve the first terminal cause recorded by the wrapper/guardian.
        if !stop.exists() {
            fs::write(stop, if failed { "[driver_failed] validation failed" } else { "[driver_completed] validation completed" })?;
        }
        Ok(())
    })
}

fn live_stop_kind(home: &Path) -> Option<&'static str> {
    let reason = fs::read_to_string(home.join("live-stop")).ok()?;
    if reason.starts_with("[rate_limit]") { Some("rate_limit") }
    else if reason.starts_with("[hard_bound]") { Some("hard_bound") }
    else { None }
}

fn main() {
    let args = Args::parse();
    if !["offline", "f2", "f6", "a1", "all"].contains(&args.phase.as_str()) {
        eprintln!("phase must be offline, f2, f6, a1, or all");
        std::process::exit(2);
    }
    if args.f2_live && (args.phase != "f2" || args.a1_live) {
        eprintln!("--f2-live requires --phase f2 and a separate call budget from A1");
        std::process::exit(2);
    }
    // Explicit /tmp, never an ambient TMPDIR that might point into a real home.
    let temp = tempfile::Builder::new().prefix("dudu-dream-v2-").tempdir_in("/tmp").expect("private /tmp home");
    let home = temp.keep();
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("private home mode"); }
    println!("{}", json!({"private_home":home,"raw_content_printed":false}));
    // SAFETY: environment is initialized before any runtime/thread is created.
    unsafe {
        std::env::set_var("DUDUCLAW_HOME", &home);
        for key in ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL", "OPENAI_API_KEY", "OPENAI_BASE_URL", "GEMINI_API_KEY", "GOOGLE_API_KEY", "DUDUCLAW_MCP_API_KEY", "DUDUCLAW_AGENT_ID", "DUDUCLAW_AGENT_TOKEN", "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED", "DUDUCLAW_FORK_NO_EXEC"] {
            std::env::remove_var(key);
        }
    }
    let result = prepare(&home, &args).and_then(|()| {
        tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(run(&home, &args))
    });
    if let Err(error) = result {
        // Errors can contain runtime replies. Keep them in the private evidence home.
        let _ = fs::write(home.join("validation-error.txt"), error.to_string());
        if let Some(kind) = live_stop_kind(&home) {
            let instruction = if kind == "rate_limit" {
                "通知 S0；停止全部 live 驗證，不重試額度。"
            } else {
                "本次驗證已達硬上限，不再啟動新呼叫；這不是 provider 限流，不停止 S0。"
            };
            eprintln!("{}", json!({"live_validation_stopped":true,"stop_kind":kind,"instruction":instruction}));
        }
        eprintln!("{}", json!({"validation":"failed","private_error_file":home.join("validation-error.txt")}));
        std::process::exit(1);
    }
}

fn prepare(home: &Path, args: &Args) -> Result<()> {
    let dir = home.join("agents").join(AGENT);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("agent.toml"), AGENT_CONFIG)?;
    fs::write(dir.join("SOUL.md"), "You validate a synthetic arithmetic task. Use only tasks_claim and tasks_complete. Never access external files or services.\n")?;
    fs::write(dir.join("AGENTS.md"), "Synthetic validation only. Claim the assigned task, then call tasks_complete with the arithmetic answer and explanation. Do not delegate or create tasks.\n")?;
    let key = format!("ddc_dev_{}", uuid::Uuid::new_v4().simple());
    // The production task tools require Admin. This key exists only in the
    // fresh synthetic home; the wrapper still exposes only claim/complete.
    let config = format!("{GLOBAL_CONFIG}\n[mcp_keys.\"{key}\"]\nclient_id = \"v2-validation\"\nscopes = [\"{PRIVATE_MCP_SCOPE}\"]\nis_external = false\ncreated_at = \"{}\"\n", Utc::now().to_rfc3339());
    fs::write(home.join("config.toml"), config)?;
    if args.a1_live {
        // Use the existing one-hour ingestion throttle to suppress unrelated
        // post-dispatch wiki/memory utility calls during this <=300s test.
        // This is an intentional development guard, not cost or task evidence.
        let throttle = home.join("ingest_throttle");
        fs::create_dir_all(&throttle)?;
        fs::write(throttle.join(format!("{AGENT}.stamp")), Utc::now().to_rfc3339())?;
    }
    if args.a1_live || args.f2_live {
        let real = args.claude.as_ref().ok_or_else(|| io::Error::other("--claude is required for A1 live"))?.canonicalize()?;
        let dudu = args.duduclaw.as_ref().ok_or_else(|| io::Error::other("--duduclaw is required for A1 live"))?.canonicalize()?;
        require(real.is_file() && dudu.is_file(), "live binaries must be existing files")?;
        let mcp = json!({"mcpServers":{"duduclaw":{"command":dudu,"args":["mcp-server"],"env":{"DUDUCLAW_HOME":home,"DUDUCLAW_AGENT_ID":AGENT,"DUDUCLAW_MCP_API_KEY":key}}}});
        let mcp_path = home.join("validation-mcp.json");
        fs::write(&mcp_path, serde_json::to_vec_pretty(&mcp)?)?;
        fs::write(dir.join(".mcp.json"), serde_json::to_vec_pretty(&mcp)?)?;
        let tools = home.join("validation-tools");
        fs::create_dir(&tools)?;
        let bootstrap = tools.join("validation-bootstrap.json");
        fs::write(&bootstrap, serde_json::to_vec_pretty(&json!({
            "home":home,"real":real,"kind":if args.f2_live { "fork" } else { "goal" },"mcp_config":mcp_path
        }))?)?;
        let wrapper = tools.join("claude");
        fs::write(&wrapper, CLAUDE_WRAPPER)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tools, fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(&bootstrap, fs::Permissions::from_mode(0o600))?;
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![tools.clone()];
        paths.extend(std::env::split_paths(&path));
        // SAFETY: still before runtime construction.
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(paths)?);
            std::env::set_var("DUDUCLAW_BIN", dudu);
            std::env::set_var("DUDUCLAW_MCP_API_KEY", key);
        }
        // The resolver prefers the newest installation. Never allow a different
        // installed binary to bypass the budget wrapper.
        require(duduclaw_core::which_claude().as_deref() == wrapper.to_str(), "Claude resolver bypassed validation wrapper; choose its newest installed real CLI")?;
        if args.f2_live {
            let parent = home.join("synthetic-live-parent");
            fs::create_dir(&parent)?;
            fs::write(parent.join(".env"), "SYNTHETIC_ONLY=excluded")?;
            std::env::set_current_dir(parent)?;
        }
    }
    Ok(())
}

async fn run(home: &Path, args: &Args) -> Result<()> {
    if ["offline", "all", "f2"].contains(&args.phase.as_str()) {
        if args.f2_live { f2_live(home).await?; } else { f2(home).await?; }
    }
    let mut registry = AgentRegistry::new(home.join("agents"));
    registry.scan().await?;
    require(registry.get(AGENT).is_some(), "synthetic agent must load")?;
    let registry = Arc::new(RwLock::new(registry));
    if ["offline", "all", "f6"].contains(&args.phase.as_str()) { f6(home, registry.clone()).await?; }
    if ["all", "a1"].contains(&args.phase.as_str()) {
        require(args.a1_live, "A1 requires explicit --a1-live; no live result was claimed")?;
        a1(home, registry).await?;
    }
    Ok(())
}

async fn f2(home: &Path) -> Result<()> {
    let parent = home.join("synthetic-parent");
    fs::create_dir(&parent)?;
    fs::write(parent.join("answer.txt"), "baseline")?;
    fs::write(parent.join(".env"), "SYNTHETIC_ONLY=excluded")?;
    let root = duduclaw_fork::retention::retained_root(home);
    let branch = duduclaw_fork::retention::branch_dir(&root, "v2-fork", "v2-branch")?;
    duduclaw_fork::retention::ensure_private_dir(&branch)?;
    CopyPolicy::fork_default().copy_tree(&parent, &branch)?;
    require(!branch.join(".env").exists(), "fork copy must exclude secret-shaped files")?;
    // The executor is deliberately mocked; the adoption below is production code.
    fs::write(branch.join("answer.txt"), "2+3=5")?;
    fs::write(branch.join(".env"), "SYNTHETIC_BRANCH=must_not_promote")?;
    let store = ForkStore::open(home.join("fork_store.db"))?;
    store.insert_fork(&ForkRow { fork_id:"v2-fork".into(), agent_id:AGENT.into(), prompt:"synthetic arithmetic".into(), merge_mode:"manual".into(), resolved:false, winner:None, promoted:false, aggregate_spent_usd:0.0, created_at:Utc::now().to_rfc3339() }, &[BranchRow { branch_id:"v2-branch".into(), fork_id:"v2-fork".into(), steering:None, budget_usd:0.0, state:"finished".into(), spent_usd:0.0, output:"mock executor".into(), test_exit_code:Some(0) }])?;
    store.set_parent_workspace("v2-fork", Some(&parent.to_string_lossy()))?;
    store.set_branch_workspace("v2-branch", Some(&branch.to_string_lossy()))?;
    let response = duduclaw_cli::mcp_fork::handle_merge_or_select(&json!({"fork_id":"v2-fork","branch_id":"v2-branch"}), home, AGENT).await;
    require(response.get("isError").and_then(Value::as_bool) != Some(true), "production manual adopt failed")?;
    let row = store.get_fork("v2-fork")?.ok_or_else(|| io::Error::other("missing fork evidence"))?;
    require(row.resolved && row.promoted && row.winner.as_deref() == Some("v2-branch"), "adoption flags must reflect actual promotion")?;
    require(fs::read_to_string(parent.join("answer.txt"))? == "2+3=5", "artifact was not adopted")?;
    require(fs::read_to_string(parent.join(".env"))? == "SYNTHETIC_ONLY=excluded", "branch secret must not overwrite parent")?;
    require(!branch.exists() && store.branch_workspace("v2-branch")?.is_none(), "resolved retention must be cleaned")?;
    emit(home, "f2", json!({"passed":true,"executor":"mock","adoption":"production MCP handler + CopyPolicy","artifact_adopted":true,"secret_excluded":true,"resolved":row.resolved,"promoted":row.promoted,"retention_cleaned":true,"llm_calls":0}))
}

async fn f2_live(home: &Path) -> Result<()> {
    let response = duduclaw_cli::mcp_fork::handle_fork_run(&json!({
        "prompt":"Synthetic validation: use the Write tool to create the new relative file answer.txt containing exactly 2+3=5. Then finish. Do not read any files, use other tools, contact services, delegate, or change configuration.",
        "n":2,"merge_mode":"manual","budget_usd":0.1
    }), home, AGENT).await;
    require(response.get("isError").and_then(Value::as_bool) != Some(true), "production fork_run failed")?;
    let body: Value = serde_json::from_str(response["content"][0]["text"].as_str().ok_or_else(|| io::Error::other("fork_run envelope missing"))?)?;
    require(body["status"].as_str() == Some("running"), "no authenticated local OAuth execution backend; no live fork was claimed")?;
    let id = body["fork_id"].as_str().ok_or_else(|| io::Error::other("fork id missing"))?;
    let store = ForkStore::open(home.join("fork_store.db"))?;
    let start = Instant::now();
    let outcome = async {
        loop {
            require(!home.join("live-stop").exists(), "F2 live wrapper stopped; inspect the private stop marker")?;
            // Background publication holds this lock while writing branch
            // terminal states, retained paths, and final resolution together.
            let (branches, row, selected_id) = duduclaw_core::with_file_lock(&home.join("fork_resolution.lock"), || {
                let branches = store.list_branches(id).map_err(io::Error::other)?;
                let row = store.get_fork(id).map_err(io::Error::other)?
                    .ok_or_else(|| io::Error::other("fork row missing"))?;
                let selected_id = branches.iter().find(|b| {
                    b.state == "finished" && store.branch_workspace(&b.branch_id).ok().flatten()
                        .and_then(|p| fs::read_to_string(Path::new(&p).join("answer.txt")).ok())
                        .is_some_and(|text| text.trim() == "2+3=5")
                }).map(|b| b.branch_id.clone());
                Ok((branches, row, selected_id))
            })?;
            let all_published = !branches.is_empty() && branches.iter().all(|b| b.state != "pending" && b.state != "running");
            if all_published {
                require(!row.resolved && !row.promoted, "manual fork must wait for selection")?;
                let selected_id = selected_id.ok_or_else(|| io::Error::other("live branch did not produce expected actual file"))?;
                // Wait for the background publisher's final resolution under
                // the same lock before making the production manual choice.
                let selected_response = duduclaw_cli::mcp_fork::handle_merge_or_select(&json!({"fork_id":id,"branch_id":selected_id}), home, AGENT).await;
                require(selected_response.get("isError").and_then(Value::as_bool) != Some(true), "live manual adoption failed")?;
                let row = store.get_fork(id)?.ok_or_else(|| io::Error::other("resolved live fork missing"))?;
                let parent = home.join("synthetic-live-parent");
                require(row.resolved && row.promoted && row.winner.as_deref() == Some(selected_id.as_str()), "live resolution flags inconsistent")?;
                require(fs::read_to_string(parent.join("answer.txt"))?.trim() == "2+3=5", "live file was not adopted")?;
                require(!duduclaw_fork::retention::fork_dir(&duduclaw_fork::retention::retained_root(home), id)?.exists(), "live retained files not cleaned")?;
                let calls = fs::read_to_string(home.join("live-call-count"))?.trim().parse::<u32>()?;
                require((1..=2).contains(&calls), "live CLI count exceeds authorized bound")?;
                return emit(home, "f2-live", json!({"passed":true,"executor":"production fork_run + ClaudeCliSpawner","resolver":"production MCP manual handler + CopyPolicy","actual_cli_calls":calls,"branch_count":branches.len(),"fanout_degraded":branches.len()<2,"fanout_limitation":"distinct local OAuth account count caps production branches","manual_selection_required":true,"real_file_adopted":true,"resolved":row.resolved,"promoted":row.promoted,"retention_cleaned":true,"spent_usd":row.aggregate_spent_usd,"raw_content_printed":false}));
            }
            require(start.elapsed() < Duration::from_secs(210), "live fork publication timed out")?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }.await;
    finish_live_guard(home, outcome.is_err())?;
    for branch in store.list_branches(id)? {
        duduclaw_cli::mcp_fork_exec::request_cancel(&branch.branch_id);
    }
    outcome
}

async fn f6(home: &Path, registry: Arc<RwLock<AgentRegistry>>) -> Result<()> {
    let memory = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db"))?;
    for content in ["gateway deploy needs api token", "gateway deploy needs api token in env", "gateway deploy needs api token before boot"] {
        memory.store(AGENT, MemoryEntry { id:uuid::Uuid::new_v4().to_string(), agent_id:AGENT.into(), content:content.into(), timestamp:Utc::now(), tags:vec![], embedding:None, layer:MemoryLayer::Episodic, importance:5.0, access_count:0, last_accessed:None, source_event:"synthetic-v2".into() }).await?;
    }
    let store = TaskStore::open(home).map_err(io::Error::other)?;
    let _engine = duduclaw_gateway::night_engine::spawn_night_engine(home.to_path_buf(), registry, 30);
    println!("{}", json!({"phase":"f6","status":"waiting_for_production_scheduler","first_scan_secs":90}));
    let start = Instant::now();
    loop {
        let (rows, _) = store.list_activity(Some(AGENT), Some("night_engine.pass_complete"), 10, 0).await.map_err(io::Error::other)?;
        if let Some(row) = rows.first() {
            let report: Value = serde_json::from_str(row.metadata.as_deref().ok_or_else(|| io::Error::other("night activity metadata missing"))?)?;
            let schemas = report["schemas_induced"].as_u64().unwrap_or(0);
            let consolidations = report["consolidations_stored"].as_u64().unwrap_or(0);
            require(schemas > 0 && consolidations > 0, "night pass must produce actual schemas and consolidations")?;
            require(report["spent_cents"].as_u64() == Some(0), "deterministic night pass must not spend")?;
            return emit(home, "f6", json!({"passed":true,"scheduler":"production","activity_event":row.event_type,"activity_rows":rows.len(),"schemas_induced":schemas,"consolidations_stored":consolidations,"llm_calls":0,"elapsed_secs":start.elapsed().as_secs()}));
        }
        require(start.elapsed() < Duration::from_secs(180), "production night scheduler activity timed out")?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn a1(home: &Path, registry: Arc<RwLock<AgentRegistry>>) -> Result<()> {
    duduclaw_gateway::cost_telemetry::init_telemetry(home).map_err(io::Error::other)?;
    let store = Arc::new(TaskStore::open(home).map_err(io::Error::other)?);
    let queue = Arc::new(MessageQueue::open(home).map_err(io::Error::other)?);
    duduclaw_gateway::claude_runner::set_shared_task_store(store.clone());
    let mut task = TaskRow::new(TASK.into(), "Synthetic arithmetic validation".into(), "Compute 2+3. Claim this task with tasks_claim, then submit tasks_complete with summary: 2+3=5. Explain briefly that adding two and three gives five. Use only these two task tools, no native tools, no external data, no files. Do not delegate or create tasks.".into(), "medium".into(), AGENT.into(), "system".into());
    task.goal_mode = true;
    task.max_retries = 0;
    task.acceptance_criteria = Some("The submitted result correctly states 2+3=5 with a brief arithmetic explanation. No external actions are required.".into());
    task.acceptance_criteria_baseline = task.acceptance_criteria.clone();
    task.risk_boundary = Some("Only synthetic arithmetic and tasks_claim/tasks_complete; no external services, files, delegation or purchases.".into());
    store.insert_task(&task).await.map_err(io::Error::other)?;
    let driver = GoalLoopDriver::new(store.clone(), queue.clone(), GoalLoopConfig { iteration_cap:1, iteration_cap_simple:1, max_concurrent:1, stalled_secs:600, ..Default::default() }).with_home_dir(home.to_path_buf());
    driver.tick_once().await.map_err(io::Error::other)?;
    let pending = queue.pending_messages(10).await.map_err(io::Error::other)?;
    require(pending.len() == 1, "goal driver must enqueue one real worker message")?;
    let message_id = pending[0].id.clone();
    let dispatcher = duduclaw_gateway::dispatcher::start_agent_dispatcher_with_crypto(home.to_path_buf(), registry, None, Some(queue.clone()), None, None);
    let engine = DispatchEngine::new(store.clone(), Some(Arc::new(LlmAcceptanceJudge::new(GoalAcceptanceCaller { home_dir:home.to_path_buf() })))).with_home_dir(home.to_path_buf());
    let start = Instant::now();
    let result = async {
        loop {
            require(!home.join("live-stop").exists(), "live wrapper stopped; inspect the private stop marker")?;
            let message = queue.get_by_id(&message_id).await.map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("production dispatch message disappeared"))?;
            require(message.status.as_str() != "failed", "production dispatcher failed; inspect the private message_queue ledger; no redispatch was attempted")?;
            let dispatch_done = message.status.as_str() == "done";
            let row = store.get_task(TASK).await.map_err(io::Error::other)?.ok_or_else(|| io::Error::other("goal row missing"))?;
            // tasks_complete publishes review during the worker's tool turn.
            // Queue done proves its stream/result/usage path has finished;
            // judging earlier can abort the worker before telemetry settles.
            if row.status == "review" && dispatch_done {
                // Capture state via the actual driver before production acceptance settles.
                driver.tick_once().await.map_err(io::Error::other)?;
                tokio::time::timeout(Duration::from_secs(185), engine.tick_once()).await?.map_err(io::Error::other)?;
            }
            let row = store.get_task(TASK).await.map_err(io::Error::other)?.ok_or_else(|| io::Error::other("goal row missing"))?;
            if dispatch_done && ["done", "failed", "needs_human", "cancelled"].contains(&row.status.as_str()) {
                let rounds = store.list_iterations(TASK).await.map_err(io::Error::other)?;
                let mut ledger = Vec::new();
                for round in &rounds {
                    let value = serde_json::to_value(round)?;
                    let presence: BTreeMap<String, bool> = value.as_object().unwrap().iter().map(|(k,v)| (k.clone(), !v.is_null())).collect();
                    ledger.push(json!({"round":round.round,"verdict":round.verdict,"fields_present":presence,"team_mode":round.team_mode}));
                }
                // Runtime utility telemetry may flush in a detached writer.
                // Give that existing production write time to settle; do not
                // generate a replacement row in the harness.
                tokio::time::sleep(Duration::from_secs(1)).await;
                let conn = rusqlite::Connection::open(home.join("cost_telemetry.db"))?;
                let (usage_rows, worker_rows, judge_rows, attributed_rows): (i64,i64,i64,i64) = conn.query_row("SELECT COUNT(*), COALESCE(SUM(agent_id='v2-worker'),0), COALESCE(SUM(agent_id='goal-acceptance-judge'),0), COALESCE(SUM(episode_id=?1 AND round=1),0) FROM token_usage", [TASK], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
                let calls = fs::read_to_string(home.join("live-call-count")).unwrap_or_default().trim().parse::<u32>().unwrap_or(0);
                let passed = row.status == "done" && rounds.len() == 1 && rounds[0].round == 1 && rounds[0].verdict.as_deref() == Some("accepted") && rounds[0].submitted_at.is_some() && rounds[0].judged_at.is_some() && rounds[0].worker_excerpt.is_some() && rounds[0].iter_seq == Some(1) && rounds[0].dispatch_count == 1 && rounds[0].state_hash.is_some() && rounds[0].repeat_streak.is_some() && rounds[0].team_mode.as_deref() == Some("solo") && rounds[0].state_block_json.is_some() && rounds[0].knobs_json.is_some() && usage_rows == 2 && attributed_rows == 2 && worker_rows == 1 && judge_rows == 1 && calls == 2;
                emit(home, "a1", json!({"passed":passed,"executor":"real Claude Haiku","production_enqueue_dispatch_mcp_complete_settle":true,"background_ingest_suppressed_by_existing_hourly_throttle":true,"task_status":row.status,"ledger":ledger,"token_usage_rows":usage_rows,"worker_usage_rows":worker_rows,"judge_usage_rows":judge_rows,"episode_round_attributed_rows":attributed_rows,"actual_cli_calls":calls,"raw_content_printed":false,"nullable_fields":"feedback_class reserved; evaluator_verdict disabled; gate_inputs_json team disabled; pause_reason accepted path; token_usage.role may be NULL for Solo"}))?;
                return require(passed, "A1 actual evidence did not meet every completion and attribution check");
            }
            require(!dispatch_done || row.status == "review", "worker dispatch completed without production tasks_complete submission; no redispatch was attempted")?;
            require(start.elapsed() < Duration::from_secs(300), "A1 live driver timed out")?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }.await;
    finish_live_guard(home, result.is_err())?;
    dispatcher.abort();
    let _ = dispatcher.await;
    result
}

const GLOBAL_CONFIG: &str = r#"
[dispatch]
enabled = true
judge = "mav"
two_stage_judge = false
[runtime]
utility_provider = "claude"
utility_model = "claude-haiku-4-5"
[team]
enabled = false
[night]
llm_enabled = false
[task_forward_model]
enabled = false
"#;

#[cfg(test)]
mod live_guard_tests {
    use super::*;

    #[test]
    fn private_key_scope_matches_the_production_task_gate() {
        use duduclaw_cli::mcp_auth::{parse_scopes, tool_requires_scope, Scope};
        let granted = parse_scopes(PRIVATE_MCP_SCOPE).unwrap();
        for tool in ["tasks_claim", "tasks_complete"] {
            let required = tool_requires_scope(tool).unwrap();
            assert!(granted.contains(&required) || granted.contains(&Scope::Admin));
        }
    }

    #[test]
    fn driver_completion_or_failure_does_not_claim_a_quota_stop() {
        for failed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            finish_live_guard(home.path(), failed).unwrap();
            assert!(home.path().join("live-stop").exists());
            assert_eq!(live_stop_kind(home.path()), None);
        }
    }

    #[test]
    fn driver_cleanup_preserves_terminal_limit_or_hard_bound() {
        for (reason, kind) in [("[rate_limit] synthetic limit", "rate_limit"), ("[hard_bound] synthetic wall bound", "hard_bound")] {
            let home = tempfile::tempdir().unwrap();
            fs::write(home.path().join("live-stop"), reason).unwrap();
            finish_live_guard(home.path(), true).unwrap();
            assert_eq!(fs::read_to_string(home.path().join("live-stop")).unwrap(), reason);
            assert_eq!(live_stop_kind(home.path()), Some(kind));
        }
    }
}

const AGENT_CONFIG: &str = r#"
[agent]
name = "v2-worker"
display_name = "Synthetic V2 worker"
role = "specialist"
status = "active"
trigger = "@v2-worker"
reports_to = ""
icon = ""
[model]
preferred = "claude-haiku-4-5"
fallback = "claude-haiku-4-5"
utility = "claude-haiku-4-5"
api_mode = "cli"
account_pool = []
[runtime]
provider = "claude"
pty_pool = false
[container]
timeout_ms = 180000
max_concurrent = 1
readonly_project = true
additional_mounts = []
[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""
[budget]
monthly_limit_cents = 100
warn_threshold_percent = 80
hard_stop = true
[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []
[evolution]
skill_auto_activate = false
skill_security_scan = true
gvu_enabled = false
[capabilities]
autonomy_level = "approver"
[team]
enabled = false
[fork]
enabled = true
merge_mode = "manual"
max_branches = 2
default_budget_usd = 0.1
aggregate_budget_usd = 0.2
[night_engine]
enabled = true
idle_threshold_minutes = 90
max_passes_per_day = 1
sleep_time = false
prefetch = false
schema_induction = true
recurrence_consolidation = true
"#;

// Stream both pipes unchanged into the production parser. Keep bounded diagnostic
// copies in the private home; never print diagnostic contents to the user.
// Advisory rate_limit_event frames are not terminal failures. Only terminal result
// errors / failed process stderr can trip the rate-limit stop. The cap is persistent
// across production model/account retries and any judge utility retry.
const CLAUDE_WRAPPER: &str = r#"#!/usr/bin/env python3
import os, sys, subprocess, selectors, signal, time, json, re, fcntl
from pathlib import Path
# Production env_clear intentionally removes task-specific variables. Read only
# the host-owned bootstrap beside this exact wrapper, never a caller env path.
bootstrap=json.loads(Path(__file__).resolve().with_name('validation-bootstrap.json').read_text())
private_home=Path(bootstrap['home'])
real=bootstrap['real']
args=sys.argv[1:]
if '-p' not in args and '--print' not in args:
    if args not in [['--version'],['-v'],['--help'],['auth','status']]:
        sys.exit(78)
    os.execv(real,[real]+args)
# Give the validation invocation its own group even when the production fork
# spawner only kills its direct child. Never target the driver's shared group.
try: os.setpgid(0,0)
except PermissionError:
    if os.getpgrp()!=os.getpid(): raise
stop=private_home/'live-stop'
kind=bootstrap['kind']
invocation_flags=sorted({arg for arg in args if re.fullmatch(r'--[a-z][a-z-]*',arg)})
if kind=='fork' and '--model' not in args:
    args += ['--model','claude-haiku-4-5']
def halt(reason):
    stop.write_text(reason)
    sys.exit(79)
def record_stop(reason):
    with open(private_home/'live-call-lock.lock','a') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        if not stop.exists(): stop.write_text(reason)
# Pin the private task-only MCP and disable built-in native tools and ambient settings.
for flag in ['--mcp-config','--settings','--setting-sources','--allowedTools','--disallowedTools','--tools','--max-turns','--permission-mode']:
    while flag in args:
        index=args.index(flag); del args[index:index+2]
args=[a for a in args if a not in ['--strict-mcp-config','--safe-mode','--restricted','--bare','--dangerously-skip-permissions','--allow-dangerously-skip-permissions']]
args += ['--setting-sources','','--strict-mcp-config','--max-turns','4','--disable-slash-commands','--no-session-persistence']
private_cwd=None
child_env=dict(os.environ)
child_env['DUDUCLAW_HOME']=str(private_home)
child_env['DUDU_V2_LIVE_KIND']=kind
# Preserve local OAuth while preventing inherited mode flags from disabling
# the explicit MCP (safe mode) or keychain authentication (bare mode).
child_env.pop('CLAUDE_CODE_SIMPLE',None)
if kind=='fork':
    # No MCP or host-reading tools; one synthetic new file per private branch.
    empty_mcp=private_home/'empty-mcp.json'
    empty_mcp.write_text('{"mcpServers":{}}')
    args += ['--safe-mode','--mcp-config',str(empty_mcp),'--tools','Write','--allowedTools','Write','--permission-mode','acceptEdits']
else:
    # safe-mode disables even explicitly supplied stdio MCP in CLI 2.1.285.
    # restricted preserves the pinned task MCP and the existing OAuth login.
    private_cwd=private_home/'agents'/'v2-worker'
    if not private_cwd.is_dir(): halt('[driver_failed] private goal cwd missing')
    child_env.pop('CLAUDE_CODE_SAFE_MODE',None)
    args += ['--restricted','--mcp-config',bootstrap['mcp_config'],'--tools','','--allowedTools','mcp__duduclaw__tasks_claim,mcp__duduclaw__tasks_complete']
# The guardian owns a separate session and Claude gets its own managed group.
# The guardian survives wrapper PID/group cancellation, watches parent death,
# and kills the entire CLI group before returning the original CLI status.
guardian=r'''import os,sys,subprocess,signal,time,fcntl
from pathlib import Path
owner=int(sys.argv[1])
cancelled=False
def cancel(*unused):
    global cancelled
    cancelled=True
signal.signal(signal.SIGTERM,cancel)
signal.signal(signal.SIGINT,cancel)
if os.getppid()!=owner or cancelled: sys.exit(79)
child=subprocess.Popen(sys.argv[2:],start_new_session=True,close_fds=True)
deadline=time.monotonic()+180
code=79
try:
    while True:
        if cancelled or os.getppid()!=owner or time.monotonic()>=deadline:
            home=Path(os.environ['DUDUCLAW_HOME'])
            with open(home/'live-call-lock.lock','a') as lock:
                fcntl.flock(lock,fcntl.LOCK_EX)
                stop=home/'live-stop'
                if not stop.exists(): stop.write_text('[hard_bound] guardian cancellation/parent-death/wall bound; do not retry quota')
            break
        result=child.poll()
        if result is not None:
            code=result if result>=0 else 128-result
            break
        time.sleep(.05)
finally:
    try: os.killpg(child.pid,signal.SIGKILL)
    except ProcessLookupError: pass
    if child.poll() is None: child.wait(timeout=5)
sys.exit(code)
'''
# Match with_file_lock(home/live-call-lock)'s .lock sidecar in the driver.
with open(private_home/'live-call-lock.lock','a') as lock:
    fcntl.flock(lock,fcntl.LOCK_EX)
    if stop.exists(): sys.exit(79)
    countfile=private_home/'live-call-count'
    count=int(countfile.read_text()) if countfile.exists() else 0
    if count>=2: halt('[hard_bound] call cap reached; do not retry quota')
    if '--model' not in args or args[args.index('--model')+1] not in {'haiku','claude-haiku-4-5','claude-haiku-4-5-20251001'}:
        halt('[hard_bound] non-Haiku call refused')
    countfile.write_text(str(count+1))
    if stop.exists(): sys.exit(79)
    # Only flag names and fixed routing metadata are recorded here. Prompt,
    # system text, API keys, and config contents never enter this metadata.
    metadata_path=private_home/f'live-call-{count+1}.json'
    metadata={'invocation':count+1,'kind':kind,'model':args[args.index('--model')+1],
              'original_flags':invocation_flags,'exit_code':None,'stdout_bytes':0,
              'stderr_bytes':0,'stdout_truncated':False,'stderr_truncated':False}
    def write_metadata():
        with os.fdopen(os.open(metadata_path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600),'w') as log:
            json.dump(metadata,log)
    write_metadata()
    logs={name:os.fdopen(os.open(private_home/f'live-call-{count+1}.{name}',os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600),'wb') for name in ['stdout','stderr']}
    process=subprocess.Popen([sys.executable,'-I','-S','-B','-c',guardian,str(os.getpid()),real]+args,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True,cwd=private_cwd,env=child_env)
parent=os.getppid()
selector=selectors.DefaultSelector()
for stream in [process.stdout,process.stderr]:
    os.set_blocking(stream.fileno(),False); selector.register(stream,selectors.EVENT_READ)
linebuf=b''; stderrbuf=b''; terminal_limit=False; deadline=time.monotonic()+180
limit=re.compile(r'rate.?limit|usage.?limit|limit reached|too many requests|\b429\b',re.I)
def terminate(*unused):
    # The guardian checks the group owner before terminating it. A normal
    # completed guardian needs no signal and preserves successful exit status.
    try: process.terminate()
    except ProcessLookupError: pass
signal.signal(signal.SIGTERM,terminate)
signal.signal(signal.SIGINT,terminate)
try:
    while selector.get_map():
        if stop.exists() or time.monotonic()>=deadline or os.getppid()!=parent:
            record_stop('[hard_bound] live wall/cancellation bound; do not retry quota'); terminate(); break
        for event,_ in selector.select(0.2):
            data=os.read(event.fileobj.fileno(),65536)
            if not data: selector.unregister(event.fileobj); continue
            sink=sys.stdout.buffer if event.fileobj is process.stdout else sys.stderr.buffer
            sink.write(data); sink.flush()
            name='stdout' if event.fileobj is process.stdout else 'stderr'
            # A bounded private copy preserves startup/errors even when the
            # production parser reports an empty stream tail. No public dump.
            room=max(0,262144-metadata[name+'_bytes'])
            logs[name].write(data[:room]); logs[name].flush()
            metadata[name+'_bytes']+=min(len(data),room)
            metadata[name+'_truncated']|=len(data)>room
            if event.fileobj is process.stderr:
                merged=stderrbuf+data
                if limit.search(merged.decode('utf-8','replace')):
                    terminal_limit=True
                stderrbuf=merged[-65536:]
            else:
                linebuf += data
                while b'\n' in linebuf:
                    line,linebuf=linebuf.split(b'\n',1)
                    if len(line)>1048576:
                        record_stop('[hard_bound] stream JSON line overflow; do not retry quota'); terminate(); break
                    try:
                        frame=json.loads(line)
                        kind=frame.get('type')
                        trusted=kind in ['result','error','assistant']
                        error_fields=[frame.get('error'),frame.get('errors')]
                        subtype=str(frame.get('subtype',''))
                        if subtype.startswith('error'): error_fields.append(subtype)
                        if kind=='result' and frame.get('is_error'): error_fields.append(frame.get('result'))
                        if kind=='error': error_fields.extend([frame.get('message'),frame.get('code')])
                        if trusted and any(error_fields) and limit.search(json.dumps(error_fields)):
                            terminal_limit=True
                    except (ValueError,AttributeError): pass
                if len(linebuf)>1048576:
                    record_stop('[hard_bound] stream JSON line overflow; do not retry quota'); terminate(); break
        if terminal_limit:
            record_stop('[rate_limit] first terminal rate limit; notify S0; do not retry quota'); terminate(); break
    code=process.wait(timeout=5)
    if code!=0 and limit.search(stderrbuf.decode('utf-8','replace')):
        record_stop('[rate_limit] first terminal rate limit; notify S0; do not retry quota')
    if terminal_limit or stop.exists(): sys.exit(79)
    sys.exit(code if code>=0 else 128-code)
finally:
    terminate()
    if process.poll() is None: process.wait(timeout=5)
    metadata['exit_code']=process.returncode
    write_metadata()
    for log in logs.values(): log.close()
"#;
