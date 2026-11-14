//! No employee resolves for a channel reply: every channel answers with a
//! fixed line and no model runs (the Claude CLI boundary spy panics if one
//! is reached). Everyone gets the neutral line; only a requester the host
//! verified as internal gets the setup sentence. The spawn and dispatch
//! choke points refuse an employee turn without a directory on the caller's
//! stated purpose, with or without the channel-turn task-local.
use super::*;

fn test_ctx(home: &std::path::Path) -> ReplyContext {
    let registry = Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.join("agents")),
    ));
    let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let status: ChannelStatusMap = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
}

async fn reply_as(ctx: &ReplyContext, principal: Option<&str>, session: &str, user: &str) -> String {
    let turn = Box::pin(build_reply_with_session_inner("hello", ctx, None, session, user, None));
    match principal {
        Some(p) => {
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
                .scope(p.to_string(), turn)
                .await
        }
        None => turn.await,
    }
}

const ZH_TW_PUBLIC: &str = "目前無法提供服務，請稍後再試。";

#[tokio::test]
async fn widget_visitor_without_an_employee_gets_the_neutral_line_and_no_model_call() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx(dir.path());
    let spy = NoImplicitCloud::at(dir.path());
    let visitor = "widget-visitor:7f1c2a90-0000-4000-8000-000000000001";
    // A widget visitor on the WebChat socket, the bare visitor session, an
    // Odoo Live Chat visitor and an unrecognised internal session id.
    for (principal, session) in [
        (Some(visitor), "webchat:conn-1"),
        (None, visitor),
        (None, "odoolivechat:abcdef012345:42"),
        (None, "default"),
        (None, "webchatty:1"),
    ] {
        let reply = reply_as(&ctx, principal, session, "anonymous").await;
        assert_eq!(reply, ZH_TW_PUBLIC, "{session}");
        assert_ne!(reply, NO_RESPONDER_TEXT, "{session}: operator wording leaked");
    }
    assert_eq!(spy.counts(), [0, 0], "no model call");
}

#[tokio::test]
async fn neutral_line_follows_the_global_default_language() {
    for (language, expected) in [
        ("en", "This service is not available right now. Please try again later."),
        ("ja-JP", "現在サービスをご利用いただけません。しばらくしてからもう一度お試しください。"),
        ("zh-TW", ZH_TW_PUBLIC),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            format!("[general]\ndefault_language = \"{language}\"\n"),
        )
        .unwrap();
        let ctx = test_ctx(dir.path());
        let spy = NoImplicitCloud::at(dir.path());
        let reply = reply_as(&ctx, Some("widget-visitor:abc"), "webchat:conn-2", "anonymous").await;
        assert_eq!(reply, expected, "{language}");
        assert_eq!(spy.counts(), [0, 0]);
    }
    assert_eq!(no_responder_public_text(None), ZH_TW_PUBLIC);
    assert_eq!(no_responder_public_text(Some("en-US")), no_responder_public_text(Some("en")));
    assert_eq!(no_responder_public_text(Some("ja")), no_responder_public_text(Some("ja-JP")));
}

#[tokio::test]
async fn signed_in_webchat_user_without_an_employee_gets_the_staff_sentence() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx(dir.path());
    let spy = NoImplicitCloud::at(dir.path());
    let reply = reply_as(&ctx, Some("user-1"), "webchat:conn-3", "user-1").await;
    assert_eq!(reply, NO_RESPONDER_TEXT);
    assert_eq!(spy.counts(), [0, 0]);
}

#[tokio::test]
async fn an_unknown_sender_on_every_messaging_channel_gets_the_neutral_line() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = test_ctx(dir.path());
    let spy = NoImplicitCloud::at(dir.path());
    // The access gate is open (nothing configured): an unidentified sender
    // reaches this point and must not learn the configuration state.
    for channel in [
        "telegram", "line", "discord", "slack", "whatsapp", "feishu", "teams", "wecom",
        "dingtalk", "googlechat",
    ] {
        let session = format!("{channel}:chat-1");
        let reply = reply_as(&ctx, None, &session, "sender-1").await;
        assert_eq!(reply, ZH_TW_PUBLIC, "{channel}");
    }
    assert_eq!(spy.counts(), [0, 0]);
}

#[tokio::test]
async fn only_a_verified_company_member_on_a_messaging_channel_gets_the_staff_sentence() {
    let dir = tempfile::tempdir().unwrap();
    let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
    let member = db.create_user("staff@test.local", "Staff", "StaffPass-12345",
        duduclaw_auth::UserRole::Employee).unwrap();
    db.bind_channel_identity(&member.id, "telegram", "tg-staff", true).unwrap();
    db.bind_channel_identity(&member.id, "telegram", "tg-unverified", false).unwrap();
    let ctx = test_ctx(dir.path());
    let spy = NoImplicitCloud::at(dir.path());
    assert_eq!(reply_as(&ctx, None, "telegram:chat-2", "tg-staff").await, NO_RESPONDER_TEXT);
    for sender in ["tg-unverified", "tg-stranger", ""] {
        assert_eq!(reply_as(&ctx, None, "telegram:chat-2", sender).await, ZH_TW_PUBLIC, "{sender:?}");
    }
    // A verified identity on another channel does not carry over.
    assert_eq!(reply_as(&ctx, None, "line:chat-2", "tg-staff").await, ZH_TW_PUBLIC);
    assert_eq!(spy.counts(), [0, 0]);
}

#[tokio::test]
async fn an_install_with_a_main_employee_still_reaches_the_model_turn() {
    let dir = tempfile::tempdir().unwrap();
    crate::trigger_route::tests::write_agent(
        &dir.path().join("agents"), "dudu", "Main", "@dudu", "main", "",
    );
    let ctx = test_ctx(dir.path());
    ctx.registry.write().await.scan().await.unwrap();
    // The e2e seam stands in for the model turn after the employee resolved.
    crate::agent_channels::e2e_seam::arm(dir.path());
    // (A signed-in dashboard account is left out: its own ACL check on the
    // resolved employee is unrelated to this guard.)
    for (principal, session) in [
        (None, "telegram:42"),
        (Some("widget-visitor:abc"), "webchat:conn-5"),
    ] {
        let reply = reply_as(&ctx, principal, session, "42").await;
        assert!(reply.starts_with("OWN_BOT_REPLY agent=dudu"), "{session}: {reply}");
    }
}

use crate::runtime_dispatch::SpawnPurpose;

async fn spawn_without_dir(
    home: &std::path::Path,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    purpose: SpawnPurpose,
) -> Result<String, String> {
    let env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    spawn_claude_cli_with_env(
        "hi", "claude-haiku-4-5", "", home, None, None, capabilities, &env, None, None, purpose,
    )
    .await
}

#[tokio::test]
async fn claude_employee_turn_without_a_dir_is_refused_even_without_the_task_local() {
    let dir = tempfile::tempdir().unwrap();
    // No channel-turn marker at all (as after a `tokio::spawn`).
    assert!(!in_channel_reply_turn());
    let error = spawn_without_dir(dir.path(), None, SpawnPurpose::EmployeeTurn).await.unwrap_err();
    assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
    assert_eq!(error, no_employee_spawn_refusal());
    // An allowlist does not turn an employee turn into utility work.
    let caps = crate::night_llm::night_capabilities();
    let error = spawn_without_dir(dir.path(), Some(&caps), SpawnPurpose::EmployeeTurn).await.unwrap_err();
    assert_eq!(error, no_employee_spawn_refusal());
    // Utility work without an explicit allowlist is the full tool surface.
    for caps in [None, Some(duduclaw_core::types::CapabilitiesConfig::default())] {
        let error = spawn_without_dir(dir.path(), caps.as_ref(), SpawnPurpose::Utility).await.unwrap_err();
        assert_eq!(error, no_employee_spawn_refusal());
    }
    // The task-local stays as a second refusal, even for utility work.
    let error = crate::claude_runner::CHANNEL_REPLY_USER_ID
        .scope("u1".to_string(), spawn_without_dir(dir.path(), Some(&caps), SpawnPurpose::Utility))
        .await
        .unwrap_err();
    assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
}

#[test]
fn claude_utility_lane_with_an_allowlist_may_run_without_a_dir() {
    // Night passes, expert-pack and widget generation (no tools / WebSearch).
    for caps in [crate::night_llm::night_capabilities(), crate::night_llm::research_capabilities()] {
        assert!(!directory_less_spawn_refused(SpawnPurpose::Utility, Some(&caps)));
        assert!(directory_less_spawn_refused(SpawnPurpose::EmployeeTurn, Some(&caps)));
    }
    assert!(directory_less_spawn_refused(SpawnPurpose::Utility, None));
}

fn dispatch_request(home: &std::path::Path, purpose: SpawnPurpose) -> crate::runtime_dispatch::AgentPrompt<'_> {
    crate::runtime_dispatch::AgentPrompt {
        agent_dir: None,
        home_dir: home,
        agent_id: "",
        prompt: "hi",
        system_prompt: "",
        model: "gpt-5.4",
        max_tokens: 16,
        provider_override: Some(duduclaw_core::types::RuntimeType::Codex),
        conversation_history: &[],
        request_type: crate::cost_telemetry::RequestType::Chat,
        runtime_settings: None,
        effort: None,
        allow_cross_family_failover: false,
        purpose,
    }
}

#[tokio::test]
async fn runtime_dispatch_employee_turn_without_a_dir_is_refused_even_without_the_task_local() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!in_channel_reply_turn());
    let failure = crate::runtime_dispatch::run_agent_prompt_detailed(
        dispatch_request(dir.path(), SpawnPurpose::EmployeeTurn),
    )
    .await
    .unwrap_err();
    let preflight = failure.preflight.expect("refused before any runtime runs");
    assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&preflight.error), "{}", preflight.error);
    assert!(failure.steps.is_empty(), "no runtime was attempted");
    // The task-local remains a second refusal for a directory-less call
    // outside the utility lane.
    let failure = crate::claude_runner::CHANNEL_REPLY_AGENT_ID
        .scope(String::new(), crate::runtime_dispatch::run_agent_prompt_detailed(
            dispatch_request(dir.path(), SpawnPurpose::Utility),
        ))
        .await
        .unwrap_err();
    let preflight = failure.preflight.expect("refused before any runtime runs");
    assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&preflight.error), "{}", preflight.error);
    assert!(failure.steps.is_empty(), "no runtime was attempted");
}

struct UtilityRuntime;

#[async_trait::async_trait]
impl crate::runtime::AgentRuntime for UtilityRuntime {
    fn name(&self) -> &str { "openai_compat" }
    async fn is_available(&self) -> bool { panic!("fixture must not probe a CLI") }
    async fn execute(
        &self, _: &str, ctx: &crate::runtime::RuntimeContext,
    ) -> Result<crate::runtime::RuntimeResponse, String> {
        assert!(ctx.agent_dir.is_none());
        assert!(crate::runtime_dispatch::in_utility_call(), "restricted utility lane");
        Ok(crate::runtime::RuntimeResponse {
            content: "OK".into(), runtime_name: "openai_compat".into(),
            model_used: "gpt-5.4".into(), ..Default::default()
        })
    }
}

#[tokio::test]
async fn the_utility_lane_still_runs_without_an_employee_dir() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),
        "[runtime]\nutility_provider='openai_compat'\nutility_model='gpt-5.4'\n").unwrap();
    let mut runtimes: std::collections::HashMap<
        duduclaw_core::types::RuntimeType, Box<dyn crate::runtime::AgentRuntime>,
    > = std::collections::HashMap::new();
    runtimes.insert(duduclaw_core::types::RuntimeType::OpenAiCompat, Box::new(UtilityRuntime));
    let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
        home.path(), crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
    ).await;
    let reply = crate::runtime_dispatch::with_utility_context(
        home.path(),
        crate::runtime_dispatch::UtilityContext::Autonomous { target: "", callers: &[] },
        crate::runtime_dispatch::run_utility_prompt(home.path(), None, "", "", "summarize", 16),
    )
    .await;
    assert_eq!(reply.unwrap(), "OK");
}

#[tokio::test]
async fn local_moa_and_direct_api_entries_refuse_without_an_employee_dir() {
    let home = tempfile::tempdir().unwrap();
    let assembly = super::super::assembly::Assembly::new("telegram:1", home.path(), None, "");
    let called = std::sync::atomic::AtomicBool::new(false);
    for origin in [ReplyOrigin::Local, ReplyOrigin::Moa, ReplyOrigin::AnthropicApi] {
        let error = assembly
            .text(origin, || async {
                called.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok("answer".to_string())
            })
            .await
            .unwrap_err();
        assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
    }
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst), "no model entry was called");
}
