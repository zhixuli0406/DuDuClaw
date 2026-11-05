//! `on_behalf_of` through the MCP server: read from the host env only,
//! written onto what the employee hands on, and (enforce mode) narrowing.

use super::*;

/// Serializes the tests that set `DUDUCLAW_ON_BEHALF_OF` (process-global).
static OBO_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct HostPrincipal;

impl HostPrincipal {
    fn set(value: &str) -> Self {
        unsafe { std::env::set_var(duduclaw_core::ENV_ON_BEHALF_OF, value) };
        Self
    }
}

impl Drop for HostPrincipal {
    fn drop(&mut self) {
        unsafe { std::env::remove_var(duduclaw_core::ENV_ON_BEHALF_OF) };
    }
}

fn home_with_queue() -> TempDir {
    let tmp = TempDir::new();
    // Main-target admission fails closed if the gateway config is missing.
    fs::write(tmp.path().join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    let agents_dir = tmp.path().join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");
    // The gateway's own schema (it migrates the queue at start).
    duduclaw_gateway::message_queue::MessageQueue::open(tmp.path()).unwrap();
    tmp
}

fn account_principal(home: &std::path::Path, name: &str, role: duduclaw_auth::UserRole) -> String {
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let user = db.create_user(&format!("{name}@test.local"), name, "TeamPass-12345", role).unwrap();
    format!("user:{}", user.id)
}

fn queued_principal(home: &std::path::Path) -> Vec<Option<String>> {
    let conn = rusqlite::Connection::open(home.join("message_queue.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT on_behalf_of FROM message_queue")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

fn ctx() -> DelegationContext {
    DelegationContext {
        depth: 0,
        origin: None,
    }
}

fn audit_events(home: &std::path::Path, event: &str) -> Vec<serde_json::Value> {
    fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["event_type"] == event)
        .collect()
}

#[tokio::test]
async fn send_to_agent_stamps_the_host_principal_and_ignores_arguments() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let principal = account_principal(home, "employee", duduclaw_auth::UserRole::Employee);
    let _host = HostPrincipal::set(&principal);
    let params = serde_json::json!({
        "agent_id": "worker",
        "prompt": "整理報價單",
        "on_behalf_of": "user:someone-else",
    });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(
        queued_principal(home),
        vec![Some(principal)]
    );
}

#[tokio::test]
async fn send_to_agent_without_a_principal_writes_none() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let params =
        serde_json::json!({ "agent_id": "worker", "prompt": "x", "on_behalf_of": "user:u-1" });
    let result = send_to_agent_with_ctx(&params, tmp.path(), "main", ctx()).await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(queued_principal(tmp.path()), vec![None]);
}

#[tokio::test]
async fn a_system_sender_name_is_refused_as_a_principal() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let _host = HostPrincipal::set("user:dashboard");
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "x" });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(queued_principal(home).is_empty(), "unknown authority is refused before queueing");
    assert_eq!(duduclaw_core::host_principal_checked().unwrap_err().reason, "system_sender");
    assert!(duduclaw_core::host_principal().is_none());
}

#[tokio::test]
async fn spawn_agent_carries_the_principal_on_the_bus_line() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let _host = HostPrincipal::set("unmapped:line:0123456789abcdef");
    let params =
        serde_json::json!({ "agent_id": "worker", "task": "背景工作", "on_behalf_of": "user:x" });
    let result = spawn_agent_with_ctx(&params, home, "main", ctx()).await;
    assert!(result.get("isError").is_none(), "{result}");
    let line = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(line.lines().last().unwrap()).unwrap();
    assert_eq!(v["on_behalf_of"], "unmapped:line:0123456789abcdef");
}

#[tokio::test]
async fn tasks_create_records_the_principal_from_the_host_only() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = TempDir::new();
    let home = tmp.path();
    fs::write(home.join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    create_test_agent(&home.join("agents"), "agnes", "");
    let principal = account_principal(home, "manager", duduclaw_auth::UserRole::Manager);
    let _host = HostPrincipal::set(&principal);
    let args = serde_json::json!({
        "title": "追款",
        "assigned_to": "agnes",
        "on_behalf_of": "user:someone-else",
        "created_via": "line",
    });
    let result = crate::mcp::caller_shims::handle_tasks_create(&args, home, "agnes").await;
    assert!(result.get("isError").is_none(), "{result}");
    let store = duduclaw_gateway::task_store::TaskStore::open(home).unwrap();
    let rows = store
        .list_by_principal(&principal, false, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{result}");
    assert_eq!(rows[0].created_via.as_deref(), Some("agent"));
    assert!(
        store
            .list_by_principal("user:someone-else", false, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn enforce_requires_host_context_even_after_binding_while_record_does_not() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    // A real account with no binding on `worker`.
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    let u = db
        .create_user(
            "v@test.local",
            "訪客",
            "TeamPass-12345",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap()
        .id;
    let _host = HostPrincipal::set(&format!("user:{u}"));
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "x" });

    // record (default): allowed and recorded.
    let ok = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert!(ok.get("isError").is_none(), "{ok}");

    fs::write(
        home.join("config.toml"),
        "[delegation]\nhuman_principal = \"enforce\"\n",
    )
    .unwrap();
    let refused = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert_eq!(refused["isError"], true, "{refused}");
    let text = refused["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("發出這個指令的人沒有權限"), "{text}");
    assert_eq!(queued_principal(home).len(), 1, "nothing queued on refusal");
    // The denial row names the person (tool_calls.jsonl host stamp).
    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).unwrap();
    let row = audit
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["tool_name"] == "delegation_denied")
        .expect("delegation_denied row");
    assert_eq!(row["reason"], "human_principal_not_permitted");
    assert_eq!(row["on_behalf_of"], format!("user:{u}"));

    // A binding cannot replace the gateway's live host context. Positive
    // authenticated cases run through the real route in native MCP tests.
    db.bind_agent(&u, "worker", duduclaw_auth::AccessLevel::Operator)
        .unwrap();
    let refused = send_to_agent_with_ctx(&params, home, "main", ctx()).await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(
        queued_principal(home).len(),
        1,
        "no context means no new queue row even after binding"
    );

    // Self-assignment also needs host context, including for a Viewer.
    let viewer = db
        .create_user(
            "w@test.local",
            "看看",
            "TeamPass-12345",
            duduclaw_auth::UserRole::Employee,
        )
        .unwrap()
        .id;
    db.bind_agent(&viewer, "worker", duduclaw_auth::AccessLevel::Viewer)
        .unwrap();
    drop(_host);
    let _host = HostPrincipal::set(&format!("user:{viewer}"));
    let args = serde_json::json!({ "title": "自己做", "assigned_to": "worker" });
    let refused = crate::mcp::caller_shims::handle_tasks_create(&args, home, "worker").await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(audit_events(home, "on_behalf_of_rejected").is_empty());
}

#[tokio::test]
async fn spawn_ephemeral_carries_host_principal_through_admission_replay() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    create_test_agent_with_caps(&home.join("agents"), "main", &["Read"]);
    fs::write(home.join("config.toml"), "[dispatch]\nephemeral_max_active = 1\n").unwrap();
    let args = serde_json::json!({
        "instruction": "Read the report.", "context": "Review the report.", "tools": ["Read"],
        "on_behalf_of": "user:forged",
    });
    let principal = account_principal(home, "owner", duduclaw_auth::UserRole::Admin);
    {
        let _host = HostPrincipal::set(&principal);
        for _ in 0..2 {
            let result = spawn_ephemeral_with_ctx_and_upstream(
                &args, home, "main", ctx(),
                (Some("origin-turn".into()), Some("webchat:owner#agent:main".into())),
            ).await;
            assert_ne!(result["isError"], true, "{result}");
        }
    }
    let bus = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let first: serde_json::Value = serde_json::from_str(bus.trim()).unwrap();
    assert_eq!(first["on_behalf_of"], principal);
    let dir = duduclaw_gateway::ephemeral::resolve_agent_dir(
        home, first["agent_id"].as_str().unwrap(),
    ).unwrap();
    fs::remove_dir_all(dir).unwrap();
    assert_eq!(duduclaw_gateway::ephemeral::drain_admission_queue(home).await.admitted, 1);
    let bus = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let replayed: serde_json::Value = serde_json::from_str(bus.lines().last().unwrap()).unwrap();
    assert_eq!(replayed["on_behalf_of"], principal, "the sweeper has no host principal");
    assert_eq!(replayed["turn_id"], "origin-turn");
    assert_eq!(replayed["session_id"], "webchat:owner#agent:main");
}


#[tokio::test]
async fn non_admin_scheduled_work_preserves_host_principal_and_only_refuses_main_targets() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    create_test_agent(&home.join("agents"), "configured", "main");
    let path = home.join("agents/main/agent.toml");
    fs::write(&path, fs::read_to_string(&path).unwrap().replace("role = \"specialist\"", "role = \"main\"")).unwrap();
    fs::write(home.join("config.toml"), "[general]\ndefault_agent = \"configured\"\n[delegation]\nhuman_principal = \"record\"\n").unwrap();
    let principal = account_principal(home, "employee", duduclaw_auth::UserRole::Employee);
    let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
    for target in ["worker", "main", "configured"] {
        db.bind_agent(principal.strip_prefix("user:").unwrap(), target, duduclaw_auth::AccessLevel::Owner).unwrap();
    }
    let _host = HostPrincipal::set(&principal);
    let cron = handle_schedule_task(&serde_json::json!({
        "agent_id":"worker", "cron":"0 9 * * *", "task":"daily report",
        "on_behalf_of":"user:forged"
    }), home, "worker").await;
    assert_ne!(cron["isError"], true, "{cron}");
    let rows = duduclaw_gateway::cron_store::CronStore::open(home).unwrap().list_all().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].on_behalf_of.as_deref(), Some(principal.as_str()));
    let reminder = handle_create_reminder(&serde_json::json!({
        "agent_id":"worker", "mode":"agent_callback", "prompt":"daily report",
        "time":"1h", "channel":"line", "chat_id":"test-chat", "on_behalf_of":"user:forged"
    }), home, RecordActor::Agent("worker")).await;
    assert_ne!(reminder["isError"], true, "{reminder}");
    let rows = duduclaw_gateway::reminder_scheduler::list_reminders(home, None, Some("worker")).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].on_behalf_of.as_deref(), Some(principal.as_str()));
    let spec = handle_create_task(&serde_json::json!({
        "goal":"daily report", "steps":[{"description":"do work"}], "on_behalf_of":"user:forged"
    }), home, "worker").await;
    assert_ne!(spec["isError"], true, "{spec}");
    let dir = home.join("agents/worker");
    let ids = duduclaw_gateway::task_spec::TaskSpec::list(&dir);
    assert_eq!(ids.len(), 1);
    assert_eq!(duduclaw_gateway::task_spec::TaskSpec::load(&dir, &ids[0]).unwrap().on_behalf_of.as_deref(), Some(principal.as_str()));
    for target in ["main", "configured", "default"] {
        for kind in ["task", "goal"] {
            let result = crate::mcp::caller_shims::handle_tasks_create(
                &serde_json::json!({"title":"refuse main work","kind":kind,"assigned_to":target}), home, "worker",
            ).await;
            assert_eq!(result["isError"], true, "{target} {kind}: {result}");
        }
        let cron = handle_schedule_task(&serde_json::json!({
            "agent_id":target, "cron":"0 9 * * *", "task":"main work"
        }), home, "worker").await;
        assert_eq!(cron["isError"], true, "{target}: {cron}");
        let reminder = handle_create_reminder(&serde_json::json!({
            "agent_id":target, "mode":"agent_callback", "prompt":"main work",
            "time":"1h", "channel":"line", "chat_id":"test-chat"
        }), home, RecordActor::Agent(target)).await;
        assert_eq!(reminder["isError"], true, "{target}: {reminder}");
        let spec = handle_create_task(&serde_json::json!({
            "goal":"main work", "steps":[{"description":"do work", "agent":target}]
        }), home, "worker").await;
        assert_eq!(spec["isError"], true, "{target}: {spec}");
        let discovery = handle_discovery_create(
            &serde_json::json!({"title":"discovery", "assigned_to":target}), home, "worker",
        ).await;
        assert_eq!(discovery["isError"], true, "{target}: {discovery}");
        assert!(discovery["content"][0]["text"].as_str().unwrap().contains("主代理"));
    }
    // Ordinary targets reach the unchanged signed-caller gate, not a blanket refusal.
    for discovery in [
        handle_discovery_create(&serde_json::json!({"title":"discovery"}), home, "worker").await,
        crate::mcp::caller_shims::handle_tasks_create(
            &serde_json::json!({"kind":"discovery", "title":"discovery"}), home, "worker",
        ).await,
    ] {
        assert_eq!(discovery["isError"], true, "{discovery}");
        assert!(discovery["content"][0]["text"].as_str().unwrap().contains("identity"), "{discovery}");
    }
}

#[tokio::test]
async fn detached_target_gate_preserves_background_admin_and_unmapped_semantics() {
    use duduclaw_gateway::on_behalf_of::access::check_main_target;
    let tmp = home_with_queue();
    let home = tmp.path();
    fs::write(home.join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    assert!(check_main_target(home, None, "main").await.is_ok());
    assert!(check_main_target(home, Some("unmapped:line:0123456789abcdef"), "main").await.is_ok());
    let admin = account_principal(home, "owner", duduclaw_auth::UserRole::Admin);
    assert!(check_main_target(home, Some(&admin), "main").await.is_ok());
    let employee = account_principal(home, "employee", duduclaw_auth::UserRole::Employee);
    assert!(check_main_target(home, Some(&employee), "main").await.is_err());
    assert!(check_main_target(home, Some(&employee), "worker").await.is_ok());
}


#[tokio::test]
async fn non_admin_task_update_cannot_retarget_or_erase_the_human_origin() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    fs::write(home.join("config.toml"), "[general]\ndefault_agent = \"main\"\n").unwrap();
    let principal = account_principal(home, "employee", duduclaw_auth::UserRole::Employee);
    let store = duduclaw_gateway::task_store::TaskStore::open(home).unwrap();
    for (id, authority) in [("own", Some(principal.clone())), ("background", None)] {
        let mut row = duduclaw_gateway::task_store::TaskRow::new(
            id.into(), "work".into(), "original".into(), "medium".into(), "worker".into(), "worker".into(),
        );
        row.on_behalf_of = authority;
        store.insert_task(&row).await.unwrap();
    }
    let _host = HostPrincipal::set(&principal);
    for args in [
        serde_json::json!({"task_id":"own", "assigned_to":"main"}),
        serde_json::json!({"task_id":"background", "description":"delegate to main"}),
    ] {
        let result = handle_tasks_update(&args, home, RecordActor::Agent("worker")).await;
        assert_eq!(result["isError"], true, "{result}");
    }
    assert_eq!(store.get_task("own").await.unwrap().unwrap().assigned_to, "worker");
    assert_eq!(store.get_task("background").await.unwrap().unwrap().description, "original");
    let result = handle_tasks_update(&serde_json::json!({"task_id":"own", "description":"ordinary update"}),
        home, RecordActor::Agent("worker")).await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(store.get_task("own").await.unwrap().unwrap().on_behalf_of, Some(principal));
}


#[tokio::test]
async fn non_admin_can_resume_legacy_worker_cron_and_records_the_host_principal() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    let row = duduclaw_gateway::cron_store::CronTaskRow::new(
        "legacy".into(), "old routine".into(), "worker".into(), "0 9 * * *".into(), "work".into(),
    );
    store.insert(&row).await.unwrap();
    let principal = account_principal(home, "employee", duduclaw_auth::UserRole::Employee);
    let _host = HostPrincipal::set(&principal);
    let paused = handle_pause_cron_task(&serde_json::json!({"id":"legacy", "enabled":false}),
        home, RecordActor::Agent("worker")).await;
    assert_ne!(paused["isError"], true, "{paused}");
    assert!(!store.get("legacy").await.unwrap().unwrap().enabled);
    for enabled in [serde_json::json!(true), serde_json::json!("true")] {
        let resumed = handle_pause_cron_task(&serde_json::json!({"id":"legacy", "enabled":enabled}),
            home, RecordActor::Agent("worker")).await;
        assert_ne!(resumed["isError"], true, "{resumed}");
        let row = store.get("legacy").await.unwrap().unwrap();
        assert!(row.enabled);
        assert_eq!(row.on_behalf_of.as_deref(), Some(principal.as_str()));
    }
    let removed = handle_delete_cron_task(&serde_json::json!({"id":"legacy"}),
        home, RecordActor::Agent("worker")).await;
    assert_ne!(removed["isError"], true, "{removed}");
    assert!(store.get("legacy").await.unwrap().is_none());
}

#[tokio::test]
async fn admin_can_still_create_main_cron_callback_and_taskspec() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    fs::write(home.join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
    let principal = account_principal(home, "admin", duduclaw_auth::UserRole::Admin);
    let _host = HostPrincipal::set(&principal);
    let cron = handle_schedule_task(&serde_json::json!({
        "agent_id":"main", "cron":"0 9 * * *", "task":"daily report"
    }), home, "main").await;
    assert_ne!(cron["isError"], true, "{cron}");
    let rows = duduclaw_gateway::cron_store::CronStore::open(home).unwrap().list_all().await.unwrap();
    assert_eq!(rows[0].on_behalf_of.as_deref(), Some(principal.as_str()));
    let reminder = handle_create_reminder(&serde_json::json!({
        "agent_id":"main", "mode":"agent_callback", "prompt":"daily report",
        "time":"1h", "channel":"line", "chat_id":"test-chat"
    }), home, RecordActor::Agent("main")).await;
    assert_ne!(reminder["isError"], true, "{reminder}");
    let rows = duduclaw_gateway::reminder_scheduler::list_reminders(home, None, Some("main")).await;
    assert_eq!(rows[0].on_behalf_of.as_deref(), Some(principal.as_str()));
    let spec = handle_create_task(&serde_json::json!({
        "goal":"daily report", "steps":[{"description":"do work"}]
    }), home, "main").await;
    assert_ne!(spec["isError"], true, "{spec}");
    let dir = home.join("agents/main");
    let ids = duduclaw_gateway::task_spec::TaskSpec::list(&dir);
    assert_eq!(duduclaw_gateway::task_spec::TaskSpec::load(&dir, &ids[0]).unwrap().on_behalf_of.as_deref(), Some(principal.as_str()));
}

#[tokio::test]
async fn cron_update_stamps_host_origin_and_background_edits_do_not_erase_it() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    let row = duduclaw_gateway::cron_store::CronTaskRow::new(
        "legacy-update".into(), "report".into(), "worker".into(), "0 9 * * *".into(), "old".into(),
    );
    store.insert(&row).await.unwrap();
    let principal = account_principal(home, "employee", duduclaw_auth::UserRole::Employee);
    {
        let _host = HostPrincipal::set(&principal);
        let result = handle_update_cron_task(&serde_json::json!({
            "id":row.id, "task":"new", "on_behalf_of":"user:forged"
        }), home, RecordActor::Agent("worker")).await;
        assert_ne!(result["isError"], true, "{result}");
        let stored = store.get(&row.id).await.unwrap().unwrap();
        assert_eq!(stored.task, "new");
        assert_eq!(stored.on_behalf_of.as_deref(), Some(principal.as_str()));
    }
    let result = handle_update_cron_task(&serde_json::json!({"id":row.id, "task":"background edit"}),
        home, RecordActor::Agent("worker")).await;
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(store.get(&row.id).await.unwrap().unwrap().on_behalf_of.as_deref(), Some(principal.as_str()));
}

#[tokio::test]
async fn mcp_cron_caps_expression_bytes_before_parsing_add_and_update() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    let row = duduclaw_gateway::cron_store::CronTaskRow::new("byte-cap".into(), "Cap".into(),
        "worker".into(), "0 9 * * *".into(), "report".into());
    store.insert(&row).await.unwrap();
    let limit = duduclaw_gateway::cron_store::MAX_CRON_BYTES;
    // Invalid at the boundary reaches the parser; over-limit input must not.
    for (expression, message) in [("x".repeat(limit), "invalid cron expression"),
        ("x".repeat(limit + 1), "exceeds 256 bytes"),
        ("é".repeat(limit / 2 + 1), "exceeds 256 bytes")] {
        let added = handle_schedule_task(&serde_json::json!({
            "agent_id":"worker", "cron":expression, "task":"report"
        }), home, "worker").await;
        let updated = handle_update_cron_task(&serde_json::json!({
            "id":row.id, "cron":expression
        }), home, RecordActor::Agent("worker")).await;
        for result in [added, updated] {
            assert_eq!(result["isError"], true, "{result}");
            assert!(result["content"][0]["text"].as_str().unwrap().contains(message), "{result}");
        }
        assert_eq!(store.list_all().await.unwrap().len(), 1);
        assert_eq!(store.get(&row.id).await.unwrap().unwrap().cron, row.cron);
    }
}

#[tokio::test]
async fn mcp_cron_shared_admission_rejects_fast_insert_update_and_resume() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let principal = account_principal(home, "cron-owner", duduclaw_auth::UserRole::Employee);
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    {
        let _host = HostPrincipal::set(&principal);
        let bad = handle_schedule_task(&serde_json::json!({
            "agent_id":"worker", "cron":"* * * * *", "task":"report"
        }), home, "worker").await;
        assert_eq!(bad["isError"], true, "{bad}");
        assert!(store.list_all().await.unwrap().is_empty());
        let good = handle_schedule_task(&serde_json::json!({
            "agent_id":"worker", "cron":"*/15 * * * *", "task":"report"
        }), home, "worker").await;
        assert_ne!(good["isError"], true, "{good}");
    }
    let row = store.list_all().await.unwrap().remove(0);
    assert_eq!(row.cron_timezone.as_deref(), Some("UTC"));
    assert_eq!(row.on_behalf_of.as_deref(), Some(principal.as_str()));
    // An autonomous MCP edit still inherits the persisted human-origin limits.
    let bad = handle_update_cron_task(&serde_json::json!({"id":row.id, "cron":"* * * * *", "task":"changed"}),
        home, RecordActor::Agent("worker")).await;
    assert_eq!(bad["isError"], true, "{bad}");
    assert_eq!(store.get(&row.id).await.unwrap().unwrap().task, "report");
    let good = handle_update_cron_task(&serde_json::json!({"id":row.id, "cron":"*/30 * * * *"}),
        home, RecordActor::Agent("worker")).await;
    assert_ne!(good["isError"], true, "{good}");
    // Simulate a legacy pre-fix row. Pause works; MCP resume cannot re-arm it.
    rusqlite::Connection::open(home.join("cron_tasks.db")).unwrap()
        .execute("UPDATE cron_tasks SET cron='* * * * *' WHERE id=?1", [&row.id]).unwrap();
    let paused = handle_pause_cron_task(&serde_json::json!({"id":row.id, "enabled":false}),
        home, RecordActor::Agent("worker")).await;
    assert_ne!(paused["isError"], true, "{paused}");
    let resumed = handle_pause_cron_task(&serde_json::json!({"id":row.id, "enabled":true}),
        home, RecordActor::Agent("worker")).await;
    assert_eq!(resumed["isError"], true, "{resumed}");
    assert!(!store.get(&row.id).await.unwrap().unwrap().enabled);
}

#[tokio::test]
async fn mcp_cron_shared_admission_caps_creation_and_keeps_receipts_after_deletion() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let principal = account_principal(home, "cron-owner", duduclaw_auth::UserRole::Employee);
    let _host = HostPrincipal::set(&principal);
    let args = serde_json::json!({"agent_id":"worker", "cron":"0 9 * * *", "task":"report"});
    for _ in 0..20 {
        let result = handle_schedule_task(&args, home, "worker").await;
        assert_ne!(result["isError"], true, "{result}");
    }
    let refused = handle_schedule_task(&args, home, "worker").await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(refused.to_string().contains("for user"));
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    assert_eq!(store.list_all().await.unwrap().len(), 20);
    for row in store.list_all().await.unwrap() {
        let result = handle_delete_cron_task(&serde_json::json!({"id":row.id}), home, RecordActor::Agent("worker")).await;
        assert_ne!(result["isError"], true, "{result}");
    }
    // Each MCP call opens a fresh connection; receipts outlive rows/connections.
    let refused = handle_schedule_task(&args, home, "worker").await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(refused.to_string().contains("per hour"));
    assert!(store.list_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn mcp_cron_shared_admission_checks_update_and_resume_adoption_at_caps() {
    let _lock = OBO_LOCK.lock().await;
    let tmp = home_with_queue();
    let home = tmp.path();
    let principal = account_principal(home, "cron-owner", duduclaw_auth::UserRole::Employee);
    let store = duduclaw_gateway::cron_store::CronStore::open(home).unwrap();
    for i in 0..20 {
        let mut row = duduclaw_gateway::cron_store::CronTaskRow::new(format!("seed{i}"), "seed".into(),
            "worker".into(), "0 9 * * *".into(), "report".into());
        // Dashboard-marked records count against the same MCP user cap.
        row.created_by_user_id = principal.strip_prefix("user:").map(str::to_owned);
        store.insert(&row).await.unwrap();
    }
    let mut legacy = duduclaw_gateway::cron_store::CronTaskRow::new("adopt".into(), "adopt".into(),
        "worker".into(), "0 9 * * *".into(), "original".into());
    legacy.enabled = false;
    store.insert(&legacy).await.unwrap();
    let _host = HostPrincipal::set(&principal);
    let resumed = handle_pause_cron_task(&serde_json::json!({"id":"adopt", "enabled":true}),
        home, RecordActor::Agent("worker")).await;
    assert_eq!(resumed["isError"], true, "{resumed}");
    assert!(resumed.to_string().contains("for user"));
    let updated = handle_update_cron_task(&serde_json::json!({"id":"adopt", "task":"changed"}),
        home, RecordActor::Agent("worker")).await;
    assert_eq!(updated["isError"], true, "{updated}");
    assert!(updated.to_string().contains("per hour"));
    let saved = store.get("adopt").await.unwrap().unwrap();
    assert!(saved.on_behalf_of.is_none());
    assert_eq!(saved.task, "original");
    assert!(!saved.enabled);
}
