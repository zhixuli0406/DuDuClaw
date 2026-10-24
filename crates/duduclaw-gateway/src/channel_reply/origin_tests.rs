//! #210 B source-first origin/formatter contracts, UNRUN.
use super::reply_origin::*;
use super::*;
use duduclaw_core::types::RuntimeType;
#[test]
fn nonclaude_auth_does_not_emit_claude_advice_or_signal_claude_outage() {
    let failure = ReplyFailure::simple(
        ReplyOrigin::Runtime(RuntimeType::Codex),
        "invalid bearer token; claude /login".into(),
    );
    let home = tempfile::tempdir().unwrap();
    let message = failure.message("Worker", home.path());
    assert!(message.contains("Codex"));
    assert!(!message.contains("/login"));
    assert!(!message.contains("Anthropic"));
    assert!(failure.claude_auth_error().is_none());
}
#[test]
fn exact_success_origin_controls_recovery_and_nonclaude_never_enables_direct_api() {
    assert_eq!(
        ReplyOrigin::runtime_name("claude"),
        ReplyOrigin::Runtime(RuntimeType::Claude)
    );
    for name in ["codex", "openai_compat_sse", "not-claude", "claude-stub"] {
        assert!(!ReplyOrigin::runtime_name(name).is_claude_cli());
    }
    assert!(direct_api_allowed(None, false, false));
    assert!(!direct_api_allowed(Some(RuntimeType::Codex), false, false));
    assert!(!direct_api_allowed(None, true, false));
    assert!(!direct_api_allowed(None, false, true));
}
#[test]
fn anthropic_api_error_does_not_instruct_cli_login_and_local_notice_uses_known_origin() {
    let home = tempfile::tempdir().unwrap();
    let error = ReplyFailure::simple(ReplyOrigin::AnthropicApi, "invalid bearer token".into());
    assert!(!error.message("Worker", home.path()).contains("/login"));
    assert!(error.claude_auth_error().is_none());
    let notice = ReplyFailure::simple(
        ReplyOrigin::Runtime(RuntimeType::Codex),
        "500 failure".into(),
    )
    .local_notice();
    assert!(notice.contains("Codex"));
    assert!(!notice.contains("Claude"));
    assert!(!notice.contains("自動切回 Claude"));
}

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
async fn owned_channel(
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
        std::fs::write(dir.join("agent.toml"),format!("[agent]\nname='{id}'\ndisplay_name='{display}'\nrole='{role}'\n[model]\npreferred='{model}'\n[runtime]\nprovider='{runtime}'\n[goal_intent]\nenabled=false\nmode='off'\n[evolution]\nenabled=false\n")).unwrap();
    }
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.path().join("agents")),
    ));
    registry.write().await.scan().await.unwrap();
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
#[tokio::test(flavor = "current_thread")]
async fn actual_shared_channel_pipeline_uses_effective_agent_and_never_implicit_claude_or_direct_api()
 {
    for provider in [RuntimeType::Codex, RuntimeType::OpenAiCompat] {
        for text in ["普通訊息", "直接開始開發"] {
            let (home, context, _registry, calls) = owned_channel(provider).await;
            let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
            let reply = build_reply_for_agent(
                text,
                &context,
                "worker",
                "discord:owned",
                "owned-user",
                None,
            )
            .await;
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(spy.counts(), [0, 0]);
            assert!(reply.contains("Actual Worker"), "{reply}");
            assert!(!reply.contains("Wrong Main"));
            assert!(!reply.contains("/login"));
            assert!(!crate::auth_outage::state_path(home.path()).exists());
            let raw = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
            let row: serde_json::Value = serde_json::from_str(raw.lines().last().unwrap()).unwrap();
            assert_eq!(row["agent"], "Actual Worker");
            assert_eq!(
                row["runtime_failure"]["steps"][0]["runtime"],
                provider.as_str()
            );
            assert_eq!(row["runtime_failure"]["steps"][0]["reason"], "AuthFailed");
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn gpt_success_origin_does_not_clear_existing_claude_outage_state() {
    let home = tempfile::tempdir().unwrap();
    let path = crate::auth_outage::state_path(home.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let state = serde_json::to_vec(&crate::auth_outage::AuthOutageState {
        active: true,
        ..Default::default()
    })
    .unwrap();
    std::fs::write(&path, &state).unwrap();
    super::reply_origin::record_reply_recovery(
        ReplyOrigin::Runtime(RuntimeType::Codex),
        home.path(),
        "worker",
    )
    .await;
    assert_eq!(std::fs::read(path).unwrap(), state);
}
#[test]
fn explicit_fallback_auth_attribution_is_per_leg_and_audit_contains_no_raw_body() {
    use crate::failover::*;
    let mut failure = RuntimeDispatchFailure::new(RuntimeType::Codex, "gpt-5.4");
    failure.push(
        RuntimeStepRole::Primary,
        RuntimeType::Codex,
        Some("gpt-5.4"),
        RuntimeStepOutcome::ExecutionError("500 primary sensitive body".into()),
    );
    failure.push(
        RuntimeStepRole::Fallback,
        RuntimeType::Claude,
        Some("claude-sonnet-4-6"),
        RuntimeStepOutcome::ExecutionError("invalid bearer token".into()),
    );
    let failure = ReplyFailure::runtime(failure);
    assert_eq!(failure.claude_auth_error(), Some("invalid bearer token"));
    let home = tempfile::tempdir().unwrap();
    let message = failure.message("Worker", home.path());
    assert!(message.contains("明確備援"));
    assert!(message.contains("/login"));
    assert!(!message.contains("sensitive body"));
    let audit = failure.audit();
    assert_eq!(audit["steps"][0]["runtime"], "codex");
    assert_eq!(audit["steps"][1]["runtime"], "claude");
    assert!(!audit.to_string().contains("sensitive body"));
}

#[tokio::test(flavor = "current_thread")]
async fn shared_pipeline_old_known_mismatch_is_call_zero_and_public_visitor_never_sees_configuration()
 {
    let (home, context, _registry, calls) = owned_channel(RuntimeType::Codex).await;
    let path = home.path().join("agents/worker/agent.toml");
    let before = std::fs::read_to_string(&path)
        .unwrap()
        .replace("provider='codex'", "provider='claude'");
    std::fs::write(&path, &before).unwrap();
    let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
    let reply = build_reply_for_agent(
        "hello",
        &context,
        "worker",
        "discord:owned",
        "owned-user",
        None,
    )
    .await;
    assert!(reply.contains("configuration mismatch"), "{reply}");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(spy.counts(), [0, 0]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    let visitor = crate::odoo_livechat::session_key("owned", 42);
    let reply =
        build_reply_for_agent("hello", &context, "worker", &visitor, "owned-visitor", None).await;
    assert_eq!(reply, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}
#[test]
fn local_moa_and_api_failure_metadata_never_claim_claude_cli_origin() {
    let home = tempfile::tempdir().unwrap();
    for origin in [
        ReplyOrigin::Local,
        ReplyOrigin::Moa,
        ReplyOrigin::AnthropicApi,
        ReplyOrigin::Unknown,
    ] {
        let failure = ReplyFailure::simple(origin, "invalid bearer token; Claude /login".into());
        assert!(!origin.is_claude_cli());
        assert!(failure.claude_auth_error().is_none());
        assert!(!failure.message("Worker", home.path()).contains("/login"));
    }
}
