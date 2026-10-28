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
#[tokio::test]
async fn nonretryable_primary_auth_error_never_attributes_a_claude_attempt() {
    let home = tempfile::tempdir().unwrap();
    let (reg, a, b) = registry(
        Err("401 invalid bearer token; misleading claude /login"),
        Ok("wrong"),
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
    assert_eq!(error.steps.len(), 1);
    assert_eq!(error.steps[0].runtime, RuntimeType::Codex);
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
}
