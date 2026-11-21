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
/// RM-181 (v1.84.0): an authentication failure of the PRIMARY engine that a
/// DuDuClaw-written prefix identifies (here `API error (401 …)`) is a reason
/// to try the configured fallback engine. The primary's auth failure stays
/// attributed to the primary leg. Untyped text such as `401 invalid bearer
/// token` keeps the v1.83.0 outcome and stops (F2, see the next test).
#[tokio::test]
async fn primary_auth_error_uses_the_explicit_fallback_and_stays_attributed_to_the_primary() {
    let home = tempfile::tempdir().unwrap();
    let (reg, a, b) = registry(
        Err("API error (401 Unauthorized): invalid bearer token"),
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
    assert!(matches!(&error.steps[0].outcome, RuntimeStepOutcome::ExecutionError(e) if e.starts_with("API error (401 Unauthorized)")));
    assert_eq!(error.steps[1].runtime, RuntimeType::Claude);
    assert!(error.fallback_skipped.is_none());
}
/// F2 (v1.84 security re-review): untyped authentication text that is not
/// DuDuClaw-formatted gets the v1.83.0 outcome — no fallback leg runs.
#[tokio::test]
async fn untyped_auth_text_keeps_the_v183_no_fallback_outcome() {
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
    assert_eq!(b.load(Ordering::SeqCst), 0);
    assert_eq!(error.fallback_skipped.map(|s| s.reason), Some(FallbackRefusal::UntypedAuthFailure));
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

/// A bare Claude alias is replaced by the fallback engine's catalog default;
/// the product model policy is applied to that SUBSTITUTED model, so the
/// substitution never bypasses it. (The restricted test policy blocks the
/// provider `openai`; the Codex default is an OpenAI model the pricing table
/// does not list.)
#[tokio::test]
async fn substituted_catalog_default_is_refused_by_a_restrictive_model_policy() {
    let codex_default = RuntimeType::Codex.spec().fallback_models[0].0;
    for (config, allowed) in [("[product]\nprofile='restricted'\n", false), ("", true)] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Grok, Box::new(Counted { id: RuntimeType::Grok, result: Err("Grok CLI exited with 1: boom"), calls: primary_calls.clone() }));
        map.insert(RuntimeType::Codex, Box::new(Counted { id: RuntimeType::Codex, result: Ok("codex answer"), calls: fallback_calls.clone() }));
        let reg = RuntimeRegistry::with_runtimes(map);
        let mut context = ctx(home.path());
        context.model = "opus".into();
        let result = FailoverManager::new(60)
            .execute_with_failover_detailed(&reg, &RuntimeType::Grok, Some(&RuntimeType::Codex), "hi", &context, &[])
            .await;
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        if allowed {
            let response = result.expect("standard policy: the substituted model runs");
            assert_eq!(response.model_used, codex_default);
            assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        } else {
            let failure = result.unwrap_err();
            assert_eq!(fallback_calls.load(Ordering::SeqCst), 0, "the substituted model must not run");
            let step = failure.steps.last().unwrap();
            assert_eq!(step.runtime, RuntimeType::Codex);
            assert_eq!(step.model.as_deref(), Some(codex_default));
            assert!(matches!(&step.outcome, RuntimeStepOutcome::PolicyRefused(e) if e.contains("Product profile")), "{:?}", step.outcome);
        }
    }
}

/// A runtime that fails with `message` and records `kind` for it (or for a
/// different string, to prove a stale kind is never borrowed).
struct TypedFailure {
    id: RuntimeType,
    message: &'static str,
    kind: super::failure_kind::FailureKind,
    record_for_other_text: bool,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl crate::runtime::AgentRuntime for TypedFailure {
    fn name(&self) -> &str { self.id.as_str() }
    async fn execute(&self, _: &str, _: &RuntimeContext) -> Result<RuntimeResponse, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.record_for_other_text {
            let _ = super::failure_kind::typed(self.kind, format!("{} (earlier attempt)", self.message));
            return Err(self.message.to_string());
        }
        Err(super::failure_kind::typed(self.kind, self.message.to_string()))
    }
    async fn is_available(&self) -> bool { true }
}

fn typed_registry(primary: TypedFailure, fallback: RuntimeType) -> (RuntimeRegistry, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
    let id = primary.id;
    map.insert(id, Box::new(primary));
    map.insert(fallback, Box::new(Counted { id: fallback, result: Ok("fallback answer"), calls: calls.clone() }));
    (RuntimeRegistry::with_runtimes(map), calls)
}

/// L2 through the real failover manager: the kind the runtime recorded for
/// exactly its error decides, whatever the text says; a kind recorded for
/// another string is ignored.
#[tokio::test]
async fn the_recorded_failure_kind_decides_and_a_stale_kind_is_ignored() {
    use super::failure_kind::FailureKind;
    let home = tempfile::tempdir().unwrap();
    // Text alone would be an eligible process error; the typed kind refuses.
    let primary_calls = Arc::new(AtomicUsize::new(0));
    let (reg, fallback_calls) = typed_registry(TypedFailure {
        id: RuntimeType::Codex, message: "API error (500 Internal Server Error): invalid model 401 overloaded",
        kind: FailureKind::ContentPolicy, record_for_other_text: false, calls: primary_calls.clone(),
    }, RuntimeType::Claude);
    let failure = FailoverManager::new(60)
        .execute_with_failover_detailed(&reg, &RuntimeType::Codex, Some(&RuntimeType::Claude), "hi", &ctx(home.path()), &[])
        .await.unwrap_err();
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    assert_eq!(failure.fallback_skipped.map(|s| s.reason), Some(FallbackRefusal::ContentPolicy));
    // Text alone would be refused (a 4xx body that names a model); the typed kind falls back.
    let (reg, fallback_calls) = typed_registry(TypedFailure {
        id: RuntimeType::Codex, message: "API error (400 Bad Request): content policy violation billing_hard_limit",
        kind: FailureKind::Server, record_for_other_text: false, calls: primary_calls.clone(),
    }, RuntimeType::Claude);
    let response = FailoverManager::new(60)
        .execute_with_failover_detailed(&reg, &RuntimeType::Codex, Some(&RuntimeType::Claude), "hi", &ctx(home.path()), &[])
        .await.expect("eligible by kind");
    assert_eq!(response.content, "fallback answer");
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    // A kind recorded for a different string does not apply: the anchored status decides.
    let (reg, fallback_calls) = typed_registry(TypedFailure {
        id: RuntimeType::Codex, message: "API error (400 Bad Request): x",
        kind: FailureKind::Server, record_for_other_text: true, calls: primary_calls,
    }, RuntimeType::Claude);
    let failure = FailoverManager::new(60)
        .execute_with_failover_detailed(&reg, &RuntimeType::Codex, Some(&RuntimeType::Claude), "hi", &ctx(home.path()), &[])
        .await.unwrap_err();
    assert_eq!(fallback_calls.load(Ordering::SeqCst), 0);
    assert_eq!(failure.fallback_skipped.map(|s| s.reason), Some(FallbackRefusal::RequestRejected));
}

/// RM-204: a Grok prompt whose required context does not fit one argument
/// falls back to Claude (no argument limit) but not to another
/// single-argument engine, and never cools the primary down.
#[tokio::test]
async fn prompt_too_large_falls_back_only_to_an_engine_without_the_argument_limit() {
    let home = tempfile::tempdir().unwrap();
    let error: &'static str = Box::leak(crate::runtime::prompt_budget::prompt_too_large_error("grok").into_boxed_str());
    let grok = |calls: &Arc<AtomicUsize>| Counted { id: RuntimeType::Grok, result: Err(error), calls: calls.clone() };
    let primary_calls = Arc::new(AtomicUsize::new(0));
    for (fallback, eligible) in [(RuntimeType::Claude, true), (RuntimeType::Qwen, false)] {
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let mut map: HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = HashMap::new();
        map.insert(RuntimeType::Grok, Box::new(grok(&primary_calls)));
        map.insert(fallback, Box::new(Counted { id: fallback, result: Ok("fallback answer"), calls: fallback_calls.clone() }));
        let reg = RuntimeRegistry::with_runtimes(map);
        let mgr = FailoverManager::new(60);
        let mut context = ctx(home.path());
        context.model = "grok-4".into();
        let result = mgr
            .execute_with_failover_detailed(&reg, &RuntimeType::Grok, Some(&fallback), "hi", &context, &[])
            .await;
        if eligible {
            assert_eq!(result.expect("Claude has no argument limit").content, "fallback answer");
            assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
        } else {
            let failure = result.unwrap_err();
            assert_eq!(fallback_calls.load(Ordering::SeqCst), 0, "{fallback:?} shares the limit");
            assert_eq!(failure.fallback_skipped.map(|s| s.reason), Some(FallbackRefusal::PromptTooLarge));
            // The person sees the plain zh-TW explanation.
            let shown = failure.steps.iter().find_map(|step| match &step.outcome {
                RuntimeStepOutcome::ExecutionError(e) => duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(e),
                _ => None,
            }).unwrap();
            assert!(shown.starts_with(crate::runtime::prompt_budget::PROMPT_TOO_LARGE_PREFIX) && shown.contains("新的對話"), "{shown}");
        }
        let grok_health = mgr.health_summary().await.into_iter().find(|h| h.runtime_type == RuntimeType::Grok).unwrap();
        assert_eq!(grok_health.consecutive_failures, 0);
    }
}
