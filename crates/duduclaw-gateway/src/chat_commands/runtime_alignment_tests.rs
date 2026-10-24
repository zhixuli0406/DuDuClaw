//! #210 source-first real /model writer seam; UNRUN.
use super::*;

// Keep the real registry parse in the test path; a partial agent document is
// skipped by scan() before either the model writer or its refusal can run.
fn model_agent_fixture(preferred: &str, runtime: &str, comment: &str) -> String {
    let document = format!(
        r#"{comment}[agent]
name='worker'
display_name='Worker'
role='main'
status='active'
trigger='@Worker'
reports_to=''
icon='🤖'
[model]
preferred='{preferred}'
fallback=''
account_pool=[]
{runtime}
[container]
timeout_ms=1800000
[heartbeat]
enabled=false
interval_seconds=3600
max_concurrent_runs=1
cron=''
[budget]
monthly_limit_cents=5000
warn_threshold_percent=80
hard_stop=true
[permissions]
can_create_agents=false
can_send_cross_agent=false
can_modify_own_skills=false
can_modify_own_soul=false
can_schedule_tasks=false
allowed_channels=['*']
[evolution]
enabled=false
"#
    );
    toml::from_str::<duduclaw_core::types::AgentConfig>(&document)
        .expect("model writer fixture must be a complete agent configuration");
    document
}

#[tokio::test(flavor = "current_thread")]
async fn same_model_command_repairs_runtime_and_rescans_without_rewriting_other_keys() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/worker");
    std::fs::create_dir_all(&dir).unwrap();
    let original = model_agent_fixture(
        "gpt-5.4",
        "[runtime]\nprovider='claude'\nfallback='openai_compat'",
        "# retained\n",
    );
    std::fs::write(dir.join("agent.toml"), &original).unwrap();
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.path().join("agents")),
    ));
    registry.write().await.scan().await.unwrap();
    assert!(registry.read().await.get("worker").is_some());
    let sessions = std::sync::Arc::new(
        crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap(),
    );
    let status = std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let ctx = ReplyContext::new(registry, home.path().to_owned(), sessions, status, tx);
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let reply = handle_model(&ctx, "worker", Some("gpt-5.4")).await;
    assert!(reply.contains("已修正執行環境"), "{reply}");
    let raw = std::fs::read_to_string(dir.join("agent.toml")).unwrap();
    let t: toml::Table = raw.parse().unwrap();
    assert_eq!(t["model"]["preferred"].as_str(), Some("gpt-5.4"));
    assert_eq!(t["runtime"]["provider"].as_str(), Some("codex"));
    assert_eq!(t["runtime"]["fallback"].as_str(), Some("openai_compat"));
    assert!(raw.contains("# retained"));
    assert_eq!(
        ctx.registry
            .read()
            .await
            .get("worker")
            .unwrap()
            .config
            .runtime
            .provider
            .as_deref(),
        Some("codex")
    );
    assert!(
        handle_model(&ctx, "worker", Some("gpt-5.4"))
            .await
            .contains("沒有變更")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_and_inferred_runtime_cannot_partially_commit_when_locked_mutation_refuses() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/worker");
    std::fs::create_dir_all(&dir).unwrap();
    let original = model_agent_fixture("claude-sonnet-4-6", "", "");
    std::fs::write(dir.join("agent.toml"), &original).unwrap();
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.path().join("agents")),
    ));
    registry.write().await.scan().await.unwrap();
    assert!(registry.read().await.get("worker").is_some());
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let reached_mutation = std::sync::atomic::AtomicBool::new(false);
    let result = crate::channel_reply::update_agent_toml_edit(
        &registry,
        "worker",
        crate::channel_reply::AgentTomlEdit {
            source: "chat.model",
            user_id: "owned-test",
            guard: || Ok(()),
        },
        |table| {
            table
                .get_mut("model")
                .unwrap()
                .as_table_mut()
                .unwrap()
                .insert("preferred".into(), toml::Value::String("gpt-5.4".into()));
            crate::runtime_config::align_model_runtime(table)?;
            assert_eq!(table["runtime"]["provider"].as_str(), Some("codex"));
            reached_mutation.store(true, std::sync::atomic::Ordering::SeqCst);
            Err("owned atomic refusal".into())
        },
    )
    .await;
    assert!(result.is_err());
    assert!(reached_mutation.load(std::sync::atomic::Ordering::SeqCst));
    assert!(result.unwrap_err().contains("owned atomic refusal"));
    assert_eq!(
        std::fs::read_to_string(dir.join("agent.toml")).unwrap(),
        original
    );
}
