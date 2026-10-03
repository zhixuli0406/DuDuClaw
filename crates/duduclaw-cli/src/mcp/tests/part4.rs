use super::*;
use crate::mcp::caller_shims::handle_agent_update;

/// A system/human-interface caller (dashboard, ...) has no place of its
/// own in the org tree, so an omitted `reports_to` must still default to
/// the main agent — pre-WP21 behaviour, unchanged for this caller class.
/// Uses its own fixture (rather than `delegation_home`, whose "ceo" is
/// `role = "specialist"`) so `resolve_main_agent_name` has an actual
/// `role = "main"` agent to find.
#[tokio::test]
async fn create_agent_omitted_reports_to_defaults_to_main_for_system_sender() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    let main_dir = agents_dir.join("ceo");
    fs::create_dir_all(&main_dir).unwrap();
    // `AgentConfig` requires the [model]/[container]/[heartbeat]/[budget]/
    // [permissions]/[evolution] sections (no struct-level `#[serde(default)]`),
    // so a minimal `[agent]`-only fixture fails to parse and
    // `resolve_main_agent_name` silently finds no main agent. Full shape,
    // `role = "main"` swapped in.
    fs::write(
        main_dir.join("agent.toml"),
        r#"[agent]
name = "ceo"
display_name = "ceo"
role = "main"
status = "active"
trigger = "@ceo"
reports_to = ""
icon = "🤖"
department = ""

[model]
preferred = "claude-sonnet-4-6"
fallback = ""
api_mode = "cli"
account_pool = []

[budget]
monthly_limit_cents = 1000
warn_threshold_percent = 80
hard_stop = false

[container]
sandbox_enabled = false
network_access = false
timeout_ms = 60000
max_concurrent = 2
readonly_project = false
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 300
max_concurrent_runs = 1
cron = ""

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = false
"#,
    )
    .unwrap();

    let params = serde_json::json!({
        "name": "hire",
        "display_name": "新進",
    });
    let res = handle_create_agent(&params, home, "dashboard").await;
    assert_ne!(res["isError"], true, "{res}");

    let path = home.join("agents").join("hire").join("agent.toml");
    assert!(path.exists());
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(cfg.agent.reports_to, "ceo");
}

#[tokio::test]
async fn agent_update_reports_to_operator_and_open_policy_but_never_self() {
    let tmp = delegation_home();
    let home = tmp.path();

    // An operator (the shim maps the human-interface id to one) is not gated.
    let params = serde_json::json!({ "agent_id": "mkt-rep", "reports_to": "ceo" });
    let res = handle_agent_update(&params, home, "dashboard").await;
    assert_ne!(res["isError"], true, "{res}");

    // Escape hatch.
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();
    // Under `open` another team's agent may be re-parented...
    let params = serde_json::json!({ "agent_id": "mkt-rep", "reports_to": "sales-lead" });
    let res = handle_agent_update(&params, home, "sales-lead").await;
    assert_ne!(res["isError"], true, "{res}");
    // ...but an employee still cannot re-parent itself: `reports_to` is an
    // authority field and the self-edit guard is policy-independent.
    let params = serde_json::json!({ "agent_id": "sales-rep", "reports_to": "ceo" });
    let res = handle_agent_update(&params, home, "sales-rep").await;
    assert_eq!(res["isError"], true, "{res}");
}

#[tokio::test]
async fn validate_reports_to_existing_agent() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    create_test_agent(&agents_dir, "main", "");

    // Valid: new agent reports to existing "main"
    let result = validate_reports_to(home, "worker", "main").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn validate_reports_to_nonexistent_agent() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Invalid: references non-existent agent
    let result = validate_reports_to(home, "worker", "ghost").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("does not exist"));
}

#[tokio::test]
async fn validate_reports_to_self_blocked() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    create_test_agent(&agents_dir, "worker", "");

    let result = validate_reports_to(home, "worker", "worker").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("cannot report to itself"));
}

#[tokio::test]
async fn validate_reports_to_cycle_detected() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    // Create A → B → C, then try to set C → A (cycle)
    create_test_agent(&agents_dir, "a", "");
    create_test_agent(&agents_dir, "b", "a");
    create_test_agent(&agents_dir, "c", "b");

    // Setting a.reports_to = c would create: a → c → b → a (cycle)
    let result = validate_reports_to(home, "a", "c").await;
    assert!(result.is_err(), "Cycle should be detected: {result:?}");
    assert!(result.unwrap_err().contains("Circular"));
}

#[tokio::test]
async fn validate_reports_to_empty_is_root() {
    let tmp = TempDir::new();
    let home = tmp.path();

    // Empty reports_to is valid (root agent)
    let result = validate_reports_to(home, "any", "").await;
    assert!(result.is_ok());

    let result = validate_reports_to(home, "any", "none").await;
    assert!(result.is_ok());
}

#[test]
fn delegation_context_fields() {
    // Test DelegationContext construction directly — no env var mutation needed.
    let ctx = DelegationContext {
        depth: 3,
        origin: Some("main".into()),
    };
    assert_eq!(ctx.depth, 3);
    assert_eq!(ctx.origin.as_deref(), Some("main"));

    // Default-like: depth 0, no origin/sender
    let ctx0 = DelegationContext {
        depth: 0,
        origin: None,
    };
    assert_eq!(ctx0.depth, 0);
    assert!(ctx0.origin.is_none());
}

#[test]
fn delegation_context_from_env() {
    let _lock = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_DEPTH, "3");
        std::env::set_var(duduclaw_core::ENV_DELEGATION_ORIGIN, "main-agent");
        std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "researcher");
    }
    let ctx = DelegationContext::from_env();
    clear_delegation_env();
    assert_eq!(ctx.depth, 3);
    assert_eq!(ctx.origin.as_deref(), Some("main-agent"));
}

#[test]
fn delegation_context_from_env_defaults() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_delegation_env();
    let ctx = DelegationContext::from_env();
    assert_eq!(ctx.depth, 0);
    assert!(ctx.origin.is_none());
}

#[test]
fn delegation_context_from_env_empty_strings() {
    let _lock = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_DEPTH, "0");
        std::env::set_var(duduclaw_core::ENV_DELEGATION_ORIGIN, "");
        std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "");
    }
    let ctx = DelegationContext::from_env();
    clear_delegation_env();
    assert_eq!(ctx.depth, 0);
    assert!(ctx.origin.is_none(), "Empty string should filter to None");
}

#[test]
fn delegation_context_from_env_invalid_depth() {
    let _lock = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_DEPTH, "not_a_number");
    }
    let ctx = DelegationContext::from_env();
    clear_delegation_env();
    assert_eq!(ctx.depth, 0, "Invalid depth should default to 0");
}

// ── WP-A10 BUG-1 regression: execution attribution vs. delegation sender ──
// `resolve_audit_agent` (used for `tool_calls.jsonl` audit rows and
// dashboard live-feedback) must ignore `duduclaw_core::SYSTEM_SENDERS`
// ids — they name who *dispatched* the work (goal-loop/cron/heartbeat/
// autopilot/dashboard/webhook), never who is *executing* a tool call —
// and fall back to the executing agent's own identity. A genuine
// agent-to-agent delegation sender must still be honoured verbatim.
// See wiki/reports/memory-quality/2026-08/wp-a10-live-test-2026-08-06.md
// §1 BUG-1: before this fix every goal-loop worker's tool call was
// recorded under `goal-loop-driver`, permanently starving A3/A4
// (`task_observe`/task-forward-model) and B3 grounding of evidence.

#[test]
fn resolve_audit_agent_ignores_every_system_sender() {
    let _lock = ENV_LOCK.lock().unwrap();
    for sender in duduclaw_core::SYSTEM_SENDERS {
        unsafe {
            std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, sender);
        }
        assert_eq!(
            resolve_audit_agent(|| "tester".to_string()),
            "tester",
            "system sender {sender:?} must not be stamped as the executing agent"
        );
    }
    clear_delegation_env();
}

#[test]
fn resolve_audit_agent_honours_real_delegation_sender() {
    let _lock = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "researcher");
    }
    let resolved = resolve_audit_agent(|| "tester".to_string());
    clear_delegation_env();
    assert_eq!(
        resolved, "researcher",
        "a real agent-to-agent delegation sender must still be stamped verbatim"
    );
}

#[test]
fn resolve_audit_agent_falls_back_when_sender_absent_or_empty() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_delegation_env();
    assert_eq!(resolve_audit_agent(|| "tester".to_string()), "tester");
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "");
    }
    assert_eq!(resolve_audit_agent(|| "tester".to_string()), "tester");
    clear_delegation_env();
}

/// End-to-end goal-loop path: `goal_loop.rs` stamps
/// `sender = "goal-loop-driver"`, `dispatcher.rs`'s `DelegationEnv` injects
/// it as `DUDUCLAW_DELEGATION_SENDER` into the worker's Claude CLI
/// subprocess env, and that subprocess's own MCP server is what actually
/// calls `tools/call`. The audit row must carry the WORKER's id, not the
/// driver's.
#[tokio::test(flavor = "current_thread")]
async fn goal_loop_driver_sender_attributes_tool_call_to_worker_not_driver() {
    let _lock = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "goal-loop-driver");
    }
    let line = dispatch_pairing_manage_list_and_read_audit_line("tester").await;
    clear_delegation_env();

    let rec: serde_json::Value = serde_json::from_str(&line).expect("valid JSONL");
    assert_eq!(
        rec["agent_id"].as_str(),
        Some("tester"),
        "goal-loop-driver must not shadow the worker's own agent_id in tool_calls.jsonl: {line}"
    );
}

/// Same shape, `cron` sender. The live-test report explicitly calls out
/// that every `SYSTEM_SENDERS` dispatch path (cron/heartbeat/autopilot
/// too, all wired via `cron_scheduler.rs` / `dispatcher.rs`) shares this
/// one read site — this pins a second sender id so a future per-sender
/// special case can't silently regress the others.
#[tokio::test(flavor = "current_thread")]
async fn cron_sender_attributes_tool_call_to_worker_not_cron() {
    let _lock = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "cron");
    }
    let line = dispatch_pairing_manage_list_and_read_audit_line("tester").await;
    clear_delegation_env();

    let rec: serde_json::Value = serde_json::from_str(&line).expect("valid JSONL");
    assert_eq!(
        rec["agent_id"].as_str(),
        Some("tester"),
        "cron must not shadow the worker's own agent_id in tool_calls.jsonl: {line}"
    );
}

/// A real agent-to-agent delegation must be unaffected: the sender IS the
/// actual caller in that case, so it is still what gets stamped — this is
/// the behaviour the pre-fix code already had for non-system senders, and
/// must not regress.
#[tokio::test(flavor = "current_thread")]
async fn real_delegation_sender_still_attributes_tool_call_to_sender() {
    let _lock = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var(duduclaw_core::ENV_DELEGATION_SENDER, "researcher");
    }
    let line = dispatch_pairing_manage_list_and_read_audit_line("tester").await;
    clear_delegation_env();

    let rec: serde_json::Value = serde_json::from_str(&line).expect("valid JSONL");
    assert_eq!(
        rec["agent_id"].as_str(),
        Some("researcher"),
        "a real delegating agent must still be stamped as the caller: {line}"
    );
}

#[test]
fn normalize_reports_to_handles_variants() {
    assert_eq!(normalize_reports_to(""), "");
    assert_eq!(normalize_reports_to("none"), "");
    assert_eq!(normalize_reports_to("main"), "main");
    assert_eq!(normalize_reports_to("some-agent"), "some-agent");
}

// ── E2E delegation depth integration tests ──────────────────
// These call send_to_agent_with_ctx / spawn_agent_with_ctx directly with
// injected DelegationContext — no unsafe env var mutation needed, fully
// thread-safe and parallelizable.

#[tokio::test]
async fn e2e_send_to_agent_increments_depth() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    init_message_queue_schema(home);

    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");

    let ctx = DelegationContext {
        depth: 2,
        origin: Some("main".into()),
    };
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "do something" });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;

    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("status=queued"),
        "Expected success, got: {text}"
    );
    assert!(
        text.contains("depth=3"),
        "Expected depth 3 (2+1), got: {text}"
    );

    // v1.8.18: bus_queue.jsonl is NO LONGER written by send_to_agent.
    // This prevents the dual-rail race where the legacy dispatcher
    // (tokio::spawn'd per-message, drops task-locals) would spawn the
    // target agent's Claude CLI without the REPLY_CHANNEL scope.
    let bus_queue_path = home.join("bus_queue.jsonl");
    assert!(
        !bus_queue_path.exists(),
        "send_to_agent must not write to bus_queue.jsonl (v1.8.18 dual-rail race fix)"
    );

    // The delegation lives in SQLite — verify it's there with the
    // correct depth / origin / sender / target.
    let db_path = home.join("message_queue.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open message_queue.db");
    let (sender, target, origin_agent, sender_agent, depth): (
        String,
        String,
        String,
        String,
        i32,
    ) = conn
        .query_row(
            "SELECT sender, target, origin_agent, sender_agent, delegation_depth \
                 FROM message_queue ORDER BY rowid DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("row in message_queue.db");
    assert_eq!(depth, 3);
    assert_eq!(origin_agent, "main");
    assert_eq!(sender_agent, "main");
    assert_eq!(sender, "main");
    assert_eq!(target, "worker");
}

#[tokio::test]
async fn e2e_send_to_agent_rejects_at_depth_limit() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");

    // depth=4 → outgoing=5 >= MAX(5) → rejected
    let ctx = DelegationContext {
        depth: 4,
        origin: Some("main".into()),
    };
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "do something" });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;

    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("delegation depth limit"),
        "Expected depth limit error, got: {text}"
    );

    let queue_path = home.join("bus_queue.jsonl");
    assert!(
        !queue_path.exists(),
        "Queue should not have been written to"
    );
}

/// v1.8.18 regression test. Prevents re-introduction of the dual-rail
/// race fix: `send_to_agent` must NEVER write to `bus_queue.jsonl`.
///
/// If this test starts failing, some refactor has re-enabled the
/// legacy jsonl write — which in turn re-enables the race where the
/// legacy `poll_and_dispatch` loop tokio::spawn's dispatch tasks
/// that drop the REPLY_CHANNEL task-local, silently defeating the
/// v1.8.16 reply_channel propagation.
#[tokio::test]
async fn send_to_agent_never_writes_bus_queue_jsonl() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    init_message_queue_schema(home);

    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");

    // Happy path at depth 0 → outgoing 1. Succeeds.
    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "hi" });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("status=queued"),
        "Expected success, got: {text}"
    );

    // The SQLite queue must have the row...
    let db_path = home.join("message_queue.db");
    let count: i64 = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM message_queue", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "SQLite queue must contain the delegation");

    // ...but bus_queue.jsonl must NOT exist (v1.8.18 fix).
    let bus_queue_path = home.join("bus_queue.jsonl");
    assert!(
        !bus_queue_path.exists(),
        "v1.8.18 regression: send_to_agent must not write to bus_queue.jsonl"
    );
}

#[tokio::test]
async fn e2e_spawn_agent_increments_depth() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();

    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");

    let ctx = DelegationContext {
        depth: 1,
        origin: Some("root-agent".into()),
    };
    let params = serde_json::json!({ "agent_id": "worker", "task": "background work" });
    let result = spawn_agent_with_ctx(&params, home, "main", ctx).await;

    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("spawned successfully"),
        "Expected success, got: {text}"
    );

    let queue_path = home.join("bus_queue.jsonl");
    let content = fs::read_to_string(&queue_path).unwrap();
    let msg: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(msg["delegation_depth"], 2, "Expected depth 2 (1+1)");
    assert_eq!(
        msg["origin_agent"], "root-agent",
        "Origin should be preserved"
    );
    assert_eq!(msg["sender_agent"], "main");
}

#[tokio::test(flavor = "current_thread")]
async fn f2_spawn_rejects_archived_agent() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");
    set_agent_status(&agents_dir, "worker", "archived");

    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let params = serde_json::json!({ "agent_id": "worker", "task": "work" });
    let result = spawn_agent_with_ctx(&params, home, "main", ctx).await;

    assert_eq!(
        result["isError"].as_bool(),
        Some(true),
        "archived spawn must fail: {result}"
    );
    let text = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("not operational"), "got: {text}");
    assert!(
        !home.join("bus_queue.jsonl").exists(),
        "a rejected spawn must not enqueue a bus task"
    );
}
