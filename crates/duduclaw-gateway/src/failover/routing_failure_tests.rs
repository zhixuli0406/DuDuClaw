//! #210 B source-first: actual manager/dispatch counted stubs, UNRUN.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Counted {
    id: RuntimeType,
    result: Result<&'static str, &'static str>,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl crate::runtime::AgentRuntime for Counted {
    fn name(&self) -> &str {
        self.id.as_str()
    }
    async fn execute(
        &self,
        _prompt: &str,
        ctx: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.result
            .map(|content| RuntimeResponse {
                partial_timeout_secs: None,
                content: content.into(),
                model_used: ctx.model.clone(),
                runtime_name: self.id.as_str().into(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
            })
            .map_err(str::to_owned)
    }
    async fn is_available(&self) -> bool {
        true
    }
}
fn ctx(home: &std::path::Path) -> RuntimeContext {
    RuntimeContext {
        agent_dir: None,
        system_prompt: String::new(),
        model: "gpt-5.4".into(),
        max_tokens: 8,
        home_dir: home.to_owned(),
        agent_id: "worker".into(),
        preferred_provider: None,
        conversation_history: vec![],
        capabilities: None,
        account_pool: vec![],
        effort: None,
        allow_cross_family_failover: true,
    }
}
fn registry(
    a: Result<&'static str, &'static str>,
    b: Result<&'static str, &'static str>,
) -> (RuntimeRegistry, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let primary = Arc::new(AtomicUsize::new(0));
    let fallback = Arc::new(AtomicUsize::new(0));
    let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
    map.insert(
        RuntimeType::Codex,
        Box::new(Counted {
            id: RuntimeType::Codex,
            result: a,
            calls: primary.clone(),
        }),
    );
    map.insert(
        RuntimeType::Claude,
        Box::new(Counted {
            id: RuntimeType::Claude,
            result: b,
            calls: fallback.clone(),
        }),
    );
    (RuntimeRegistry::with_runtimes(map), primary, fallback)
}
#[tokio::test]
async fn none_fallback_never_calls_registered_claude_and_preserves_primary_failure() {
    let home = tempfile::tempdir().unwrap();
    let (reg, a, b) = registry(Err("500 primary failure"), Err("Claude spy must never run"));
    let error = FailoverManager::new(60)
        .execute_with_failover_detailed(
            &reg,
            &RuntimeType::Codex,
            None,
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 0);
    assert_eq!(error.steps.len(), 1);
    assert!(error.to_string().contains("primary failure"));
}
#[tokio::test]
async fn explicit_two_leg_failure_keeps_each_origin_model_and_original_error() {
    let home = tempfile::tempdir().unwrap();
    let (reg, a, b) = registry(Err("500 first-source"), Err("401 second-source"));
    let error = FailoverManager::new(60)
        .execute_with_failover_detailed(
            &reg,
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 1);
    assert_eq!(error.steps.len(), 2);
    assert_eq!(error.steps[0].runtime, RuntimeType::Codex);
    assert_eq!(error.steps[1].runtime, RuntimeType::Claude);
    assert_eq!(error.steps[0].model.as_deref(), Some("gpt-5.4"));
    assert_ne!(error.steps[1].model.as_deref(), Some("gpt-5.4"));
    assert!(error.to_string().contains("first-source"));
    assert!(error.to_string().contains("second-source"));
}
/// RM-181 (v1.84.0): an authentication failure of the PRIMARY engine is a
/// reason to try the configured fallback engine (it used to stop as a 4xx).
/// The primary's auth failure stays attributed to the primary leg.
#[tokio::test]
async fn primary_auth_error_uses_the_explicit_fallback_and_stays_attributed_to_the_primary() {
    let home = tempfile::tempdir().unwrap();
    let (reg, a, b) = registry(
        Err("401 invalid bearer token; misleading claude /login"),
        Err("500 fallback also down"),
    );
    let error = FailoverManager::new(60)
        .execute_with_failover_detailed(
            &reg,
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 1);
    assert_eq!(error.steps.len(), 2);
    assert_eq!(error.steps[0].runtime, RuntimeType::Codex);
    assert!(matches!(&error.steps[0].outcome, RuntimeStepOutcome::ExecutionError(e) if e.starts_with("401 invalid bearer token")));
    assert_eq!(error.steps[1].runtime, RuntimeType::Claude);
    assert!(error.fallback_skipped.is_none());
}
#[tokio::test]
async fn unavailable_and_refused_fallbacks_keep_primary_and_report_real_skip() {
    let home = tempfile::tempdir().unwrap();
    let mgr = FailoverManager::new(60);
    let error = mgr
        .execute_with_failover_detailed(
            &RuntimeRegistry::with_runtimes(HashMap::new()),
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.steps[0].outcome,
        RuntimeStepOutcome::NotRegistered
    ));
    assert!(matches!(
        error.steps[1].outcome,
        RuntimeStepOutcome::NotRegistered
    ));
    assert!(!error.to_string().contains("no fallback configured"));
    let (reg, a, b) = registry(Err("500 primary"), Ok("must not run"));
    let mut context = ctx(home.path());
    context.allow_cross_family_failover = false;
    let error = mgr
        .execute_with_failover_detailed(
            &reg,
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &context,
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 0);
    assert!(matches!(
        error.steps[1].outcome,
        RuntimeStepOutcome::CrossFamilyRefused
    ));
}
#[tokio::test]
async fn dispatch_absent_and_invalid_fallback_configs_reach_real_manager_without_hidden_claude() {
    for stored in ["", "fallback='typo'"] {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/worker");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("agent.toml"),
            format!("[runtime]\nprovider='codex'\n{stored}\n"),
        )
        .unwrap();
        let (reg, a, b) = registry(Err("500 dispatch primary"), Ok("hidden fallback"));
        let _owned =
            crate::runtime_dispatch::testing::RegistryGuard::install(home.path(), reg).await;
        let request = crate::runtime_dispatch::AgentPrompt {
            agent_dir: Some(&agent),
            home_dir: home.path(),
            agent_id: "worker",
            prompt: "hi",
            system_prompt: "",
            model: "gpt-5.4",
            max_tokens: 8,
            provider_override: None,
            conversation_history: &[],
            request_type: crate::cost_telemetry::RequestType::Chat,
            runtime_settings: None,
            effort: None,
            allow_cross_family_failover: true,
            purpose: crate::runtime_dispatch::SpawnPurpose::EmployeeTurn,
        };
        let error = crate::runtime_dispatch::run_agent_prompt_detailed(request)
            .await
            .unwrap_err();
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 0);
        assert!(error.to_string().contains("dispatch primary"));
    }
}
#[tokio::test]
async fn dispatch_known_mismatch_is_call_zero_and_never_changes_the_profile() {
    let home = tempfile::tempdir().unwrap();
    let agent = home.path().join("agents/worker");
    std::fs::create_dir_all(&agent).unwrap();
    let original = "[runtime]\nprovider='claude'\n";
    std::fs::write(agent.join("agent.toml"), original).unwrap();
    let (reg, a, b) = registry(Ok("wrong"), Ok("wrong"));
    let _owned = crate::runtime_dispatch::testing::RegistryGuard::install(home.path(), reg).await;
    let request = crate::runtime_dispatch::AgentPrompt {
        agent_dir: Some(&agent),
        home_dir: home.path(),
        agent_id: "worker",
        prompt: "hi",
        system_prompt: "",
        model: "gpt-5.4",
        max_tokens: 8,
        provider_override: None,
        conversation_history: &[],
        request_type: crate::cost_telemetry::RequestType::Chat,
        runtime_settings: None,
        effort: None,
        allow_cross_family_failover: true,
        purpose: crate::runtime_dispatch::SpawnPurpose::EmployeeTurn,
    };
    let error = crate::runtime_dispatch::run_agent_prompt_detailed(request)
        .await
        .unwrap_err();
    assert!(matches!(
        error.preflight.as_ref().map(|p| p.kind),
        Some(RuntimePreflightKind::Config)
    ));
    assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (0, 0));
    assert_eq!(
        std::fs::read_to_string(agent.join("agent.toml")).unwrap(),
        original
    );
}

#[tokio::test]
async fn explicit_fallback_success_substitutes_model_once_and_string_wrapper_does_not_reexecute() {
    let home = tempfile::tempdir().unwrap();
    let (reg, a, b) = registry(Err("500 primary"), Ok("fallback answer"));
    let response = FailoverManager::new(60)
        .execute_with_failover(
            &reg,
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(response.runtime_name, "claude");
    assert_ne!(response.model_used, "gpt-5.4");
    assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (1, 1));
}
#[tokio::test]
async fn cooldown_and_fallback_model_refusal_keep_both_steps_without_extra_calls() {
    let home = tempfile::tempdir().unwrap();
    let (reg, a, b) = registry(Err("500 primary"), Ok("fallback"));
    let mgr = FailoverManager::new(60);
    for _ in 0..3 {
        mgr.record_failure(&RuntimeType::Codex).await;
    }
    let response = mgr
        .execute_with_failover_detailed(
            &reg,
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(response.content, "fallback");
    assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (0, 1));
    for _ in 0..3 {
        mgr.record_failure(&RuntimeType::Claude).await;
    }
    let error = mgr
        .execute_with_failover_detailed(
            &reg,
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(error.steps.len(), 2);
    assert!(
        error
            .steps
            .iter()
            .all(|s| matches!(s.outcome, RuntimeStepOutcome::Cooldown))
    );

    let fallback_calls = Arc::new(AtomicUsize::new(0));
    let mut runtimes: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
    runtimes.insert(
        RuntimeType::Codex,
        Box::new(Counted {
            id: RuntimeType::Codex,
            result: Err("500 primary source"),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    );
    runtimes.insert(
        RuntimeType::Kiro,
        Box::new(Counted {
            id: RuntimeType::Kiro,
            result: Ok("must not execute"),
            calls: fallback_calls.clone(),
        }),
    );
    // A familyless runtime accepts nonempty models; branch4 requires none.
    let mut no_model_context = ctx(home.path());
    no_model_context.model.clear();
    let error = FailoverManager::new(60)
        .execute_with_failover_detailed(
            &RuntimeRegistry::with_runtimes(runtimes),
            &RuntimeType::Codex,
            Some(&RuntimeType::Kiro),
            "hi",
            &no_model_context,
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(error.steps.len(), 2);
    assert!(matches!(
        error.steps[1].outcome,
        RuntimeStepOutcome::NoCompatibleModel
    ));
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn spawn_gate_error_refuses_even_explicit_runtime_fallback_and_preserves_legacy_marker() {
    let home = tempfile::tempdir().unwrap();
    let error = Box::leak(
        duduclaw_agent::mcp_spawn_gate::spawn_gate_error("owned worker MCP configuration refused")
            .into_boxed_str(),
    );
    let (reg, a, b) = registry(Err(error), Ok("unsafe fallback"));
    let error = FailoverManager::new(60)
        .execute_with_failover(
            &reg,
            &RuntimeType::Codex,
            Some(&RuntimeType::Claude),
            "hi",
            &ctx(home.path()),
            &[],
        )
        .await
        .unwrap_err();
    assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
    assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (1, 0));
    // The typed record says the configured fallback was refused, and why.
    let (reg, _, b) = registry(
        Err(Box::leak(duduclaw_agent::mcp_spawn_gate::spawn_gate_error("owned worker MCP configuration refused").into_boxed_str())),
        Ok("unsafe fallback"),
    );
    let failure = FailoverManager::new(60)
        .execute_with_failover_detailed(&reg, &RuntimeType::Codex, Some(&RuntimeType::Claude), "hi", &ctx(home.path()), &[])
        .await
        .unwrap_err();
    assert_eq!(b.load(Ordering::SeqCst), 0);
    assert_eq!(failure.fallback_skipped.map(|s| s.reason), Some(FallbackRefusal::EmployeeToolConfig));
}

/// Sibling of the test above (RM-181, owner decision pending): a refusal of
/// the ENGINE's own host configuration carries the engine-local marker and —
/// while `ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK` is on — hands over to the
/// configured fallback, whose own `execute` runs its own gates.
#[tokio::test]
async fn engine_local_config_refusal_uses_explicit_runtime_fallback_and_keeps_the_gate_marker() {
    let home = tempfile::tempdir().unwrap();
    let error = Box::leak(
        duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error("Codex 外部設定 /etc/codex/config.toml：定義了 mcp_servers")
            .into_boxed_str(),
    );
    let (reg, a, b) = registry(Err(error), Ok("fallback answer"));
    let mgr = FailoverManager::new(60);
    let result = mgr
        .execute_with_failover_detailed(&reg, &RuntimeType::Codex, Some(&RuntimeType::Claude), "hi", &ctx(home.path()), &[])
        .await;
    if super::fallback_rule::ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK {
        let response = result.expect("the fallback engine answers");
        assert_eq!(response.content, "fallback answer");
        assert_eq!((a.load(Ordering::SeqCst), b.load(Ordering::SeqCst)), (1, 1));
        // Re-checked every call: an engine-local refusal never cools the primary down.
        let codex = mgr.health_summary().await.into_iter().find(|h| h.runtime_type == RuntimeType::Codex).unwrap();
        assert_eq!(codex.consecutive_failures, 0);
    } else {
        let failure = result.unwrap_err();
        assert_eq!(b.load(Ordering::SeqCst), 0);
        assert_eq!(failure.fallback_skipped.map(|s| s.reason), Some(FallbackRefusal::EngineLocalConfig));
    }
    // With no fallback configured it is still a spawn-gate refusal for every
    // String caller (rotation, implicit fallbacks stop on it).
    let (reg, _, _) = registry(Err(error), Ok("unused"));
    let legacy = FailoverManager::new(60)
        .execute_with_failover(&reg, &RuntimeType::Codex, None, "hi", &ctx(home.path()), &[])
        .await
        .unwrap_err();
    assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&legacy));
    assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&legacy));
}

/// RM-203 through the real dispatch choke-point: a Codex employee whose
/// primary fails hands the same capped conversation to its Claude fallback.
#[tokio::test]
async fn dispatch_claude_fallback_receives_the_conversation_history() {
    let home = tempfile::tempdir().unwrap();
    let agent = home.path().join("agents/worker");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("agent.toml"), "[runtime]\nprovider='codex'\nfallback='claude'\n").unwrap();
    let seen: Arc<std::sync::Mutex<Vec<(RuntimeType, usize, Vec<String>)>>> = Arc::default();
    struct Recorder {
        id: RuntimeType,
        result: Result<&'static str, &'static str>,
        seen: Arc<std::sync::Mutex<Vec<(RuntimeType, usize, Vec<String>)>>>,
    }
    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for Recorder {
        fn name(&self) -> &str { self.id.as_str() }
        async fn execute(&self, _: &str, ctx: &RuntimeContext) -> Result<RuntimeResponse, String> {
            self.seen.lock().unwrap().push((self.id, ctx.conversation_history.len(),
                ctx.conversation_history.iter().map(|t| format!("{}:{}", t.role, t.content)).collect()));
            self.result.map(|content| RuntimeResponse {
                content: content.into(), model_used: ctx.model.clone(),
                runtime_name: self.id.as_str().into(), ..Default::default()
            }).map_err(str::to_owned)
        }
        async fn is_available(&self) -> bool { true }
    }
    let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
    map.insert(RuntimeType::Codex, Box::new(Recorder { id: RuntimeType::Codex, result: Err("503 service unavailable"), seen: seen.clone() }));
    map.insert(RuntimeType::Claude, Box::new(Recorder { id: RuntimeType::Claude, result: Ok("答案"), seen: seen.clone() }));
    let _owned = crate::runtime_dispatch::testing::RegistryGuard::install(home.path(), RuntimeRegistry::with_runtimes(map)).await;
    let history = vec![
        crate::runtime::ConversationTurn { role: "user".into(), content: "第一輪".into() },
        crate::runtime::ConversationTurn { role: "assistant".into(), content: "第一輪回答".into() },
    ];
    let response = crate::runtime_dispatch::run_agent_prompt_detailed(crate::runtime_dispatch::AgentPrompt {
        agent_dir: Some(&agent),
        home_dir: home.path(),
        agent_id: "worker",
        prompt: "第二輪",
        system_prompt: "",
        model: "gpt-5.4",
        max_tokens: 8,
        provider_override: None,
        conversation_history: &history,
        request_type: crate::cost_telemetry::RequestType::Chat,
        runtime_settings: None,
        effort: None,
        allow_cross_family_failover: true,
        purpose: crate::runtime_dispatch::SpawnPurpose::EmployeeTurn,
    })
    .await
    .expect("the Claude fallback answers");
    assert_eq!(response.content, "答案");
    let expected = vec!["user:第一輪".to_string(), "assistant:第一輪回答".to_string()];
    assert_eq!(*seen.lock().unwrap(), vec![
        (RuntimeType::Codex, 2, expected.clone()),
        (RuntimeType::Claude, 2, expected),
    ]);
}
