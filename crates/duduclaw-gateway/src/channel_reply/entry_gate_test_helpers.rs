//! Shared fixtures for entry-gate coverage (W1/W7 and W8).
use super::*;
use duduclaw_core::types::RuntimeType;

struct PrimaryFailure {
    id: RuntimeType,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait::async_trait]
impl crate::runtime::AgentRuntime for PrimaryFailure {
    fn name(&self) -> &str {
        self.id.as_str()
    }
    async fn execute(
        &self,
        _: &str,
        _: &crate::runtime::RuntimeContext,
    ) -> Result<crate::runtime::RuntimeResponse, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("401 invalid bearer token; misleading Claude /login text".into())
    }
    async fn is_available(&self) -> bool {
        true
    }
}
pub(crate) async fn owned_channel(
    provider: RuntimeType,
) -> (
    tempfile::TempDir,
    ReplyContext,
    crate::runtime_dispatch::testing::RegistryGuard,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),"[general]\ninference_mode='claude'\n[goal_intent]\nenabled=false\nmode='off'\n[[accounts]]\nid='owned-fake-api'\ntype='api_key'\nprovider='anthropic'\napi_key='owned-not-a-real-key'\n").unwrap();
    for (id, role, display, model, runtime) in [
        ("main", "main", "Wrong Main", "claude-sonnet-4-6", "claude"),
        (
            "worker",
            "specialist",
            "Actual Worker",
            "gpt-5.4",
            provider.as_str(),
        ),
    ] {
        let dir = home.path().join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!(
                r#"[agent]
name='{id}'
display_name='{display}'
role='{role}'
status='active'
trigger='@{display}'
reports_to=''
icon='🤖'
[model]
preferred='{model}'
fallback=''
account_pool=[]
[runtime]
provider='{runtime}'
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
[goal_intent]
enabled=false
mode='off'
[evolution]
enabled=false
"#
            ),
        )
        .unwrap();
    }
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.path().join("agents")),
    ));
    registry.write().await.scan().await.unwrap();
    {
        let loaded = registry.read().await;
        assert_eq!(loaded.list().len(), 2);
        assert_eq!(
            loaded.get("main").unwrap().config.agent.display_name,
            "Wrong Main"
        );
        let worker = loaded.get("worker").unwrap();
        assert_eq!(worker.config.agent.display_name, "Actual Worker");
        assert_eq!(worker.config.model.preferred, "gpt-5.4");
        assert_eq!(
            worker.config.runtime.provider.as_deref(),
            Some(provider.as_str())
        );
    }
    let sessions =
        std::sync::Arc::new(SessionManager::new(&home.path().join("sessions.db")).unwrap());
    let status = std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let context = ReplyContext::new(registry, home.path().to_owned(), sessions, status, tx);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut runtimes: std::collections::HashMap<
        RuntimeType,
        Box<dyn crate::runtime::AgentRuntime>,
    > = std::collections::HashMap::new();
    runtimes.insert(
        provider,
        Box::new(PrimaryFailure {
            id: provider,
            calls: calls.clone(),
        }),
    );
    let owned = crate::runtime_dispatch::testing::RegistryGuard::install(
        home.path(),
        crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
    )
    .await;
    (home, context, owned, calls)
}

/// Nothing of the turn exists after an early gate refusal: no session row or
/// message (for the conversation or the employee's called bucket), no stored
/// notice, no runtime call.
pub(crate) async fn assert_refused_before_any_side_effect(
    ctx: &ReplyContext, sid: &str, calls: &std::sync::atomic::AtomicUsize, model_calls_before: usize,
) {
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "no runtime call");
    assert_eq!(crate::model_call_probe::calls(), model_calls_before, "no utility, plan or model entry point reached");
    for bucket in [sid.to_string(), crate::trigger_route::called_session(sid, "worker")] {
        assert_eq!(ctx.session_manager.session_agent(&bucket).await.unwrap(), None, "{bucket}: no session row");
        assert!(ctx.session_manager.get_messages(&bucket).await.unwrap().is_empty(), "{bucket}: no message");
    }
    assert_eq!(ctx.skill_bank.lock().await.len(), 0, "no extracted skill");
    assert!(ctx.skill_cache.lock().await.all().is_empty(), "no skill refresh");
    for bucket in [sid.to_string(), crate::trigger_route::called_session(sid, "worker")] {
        assert!(!ctx.skill_recorder.lock().await.is_recording(&format!("{bucket}:worker")));
        assert!(ctx.failsafe.as_ref().unwrap().get_state(&bucket).await.is_none(), "no failsafe mutation");
    }
    for (file, table) in [("tasks.db", "tasks"), ("approvals.db", "approvals")] {
        let path = ctx.home_dir.join(file);
        if path.exists() {
            let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let count: i64 = db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap();
            assert_eq!(count, 0, "no {table} created");
        }
    }
    assert!(!ctx.home_dir.join("recent_skills.db").exists());
    assert!(!ctx.home_dir.join("credentials/ledger.db").exists());
    assert!(!ctx.home_dir.join("skill_extraction_audit.jsonl").exists());
}

/// The worker on disk is misaligned (`gpt-5.4` on Grok); the fake runtime
/// counts every Grok call, model or utility.
pub(crate) async fn misaligned_worker(extra: &str) -> (
    tempfile::TempDir, ReplyContext, crate::runtime_dispatch::testing::RegistryGuard,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let (home, ctx, registry, calls) = owned_channel(RuntimeType::Grok).await;
    if !extra.is_empty() {
        let path = home.path().join("agents/worker/agent.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{extra}")).unwrap();
        ctx.registry.write().await.scan().await.unwrap();
    }
    (home, ctx, registry, calls)
}


/// Inbound files must never land for a refused entry, on disk or in a ledger.
pub(crate) fn assert_no_saved_attachments(home: &std::path::Path) {
    let shared = home.join("attachments");
    assert!(!shared.exists() || std::fs::read_dir(shared).unwrap().next().is_none());
    for agent in ["main", "worker"] {
        let path = home.join("agents").join(agent).join("attachments");
        assert!(!path.exists() || std::fs::read_dir(path).unwrap().next().is_none());
    }
    // Admission is before the upload reservation too.
    assert!(!home.join("webchat_upload_quota.db").exists());
    let ledger = home.join(crate::artifacts::ARTIFACTS_FILE);
    assert!(!ledger.exists() || std::fs::read_to_string(ledger).unwrap().trim().is_empty());
    let authority = home.join("artifact_provenance.db");
    if authority.exists() {
        let db = rusqlite::Connection::open_with_flags(authority, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM artifact_provenance", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "no media provenance row");
    }
}
