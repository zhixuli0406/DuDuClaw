//! #210 B source-first origin/formatter contracts, UNRUN.
use super::reply_origin::*;
use super::*;
use super::entry_gate_test_helpers::{owned_channel, misaligned_worker, assert_refused_before_any_side_effect};
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
            assert_eq!(row["agent"], "worker");
            assert_eq!(
                row["runtime_failure"]["steps"][0]["runtime"],
                provider.as_str()
            );
            assert_eq!(row["runtime_failure"]["steps"][0]["reason"], "AuthFailed");
            let history = context.session_manager.webchat_history("discord:owned").await.unwrap();
            let notice = history.iter().find(|m| m.notice_kind.as_deref() == Some("turn_failure")).unwrap();
            let turn = notice.client_msg_id.as_deref().unwrap().strip_prefix("turn_failure:").unwrap();
            context.session_manager.record_turn_failure("discord:owned", turn, TurnFailureReason::Internal, None, None).await.unwrap();
            let history = context.session_manager.webchat_history("discord:owned").await.unwrap();
            assert_eq!(history.iter().filter(|m| m.notice_kind.as_deref() == Some("turn_failure")).count(), 1);
            assert!(notice.message.content.starts_with(TurnFailureReason::Unavailable.text(None)));
            assert!(!notice.message.content.contains("/login"));
            assert!(context.session_manager.get_messages("discord:owned").await.unwrap().iter().all(|m| m.role != "assistant"));
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
        .replace("provider='codex'", "provider='claude'\nfallback='codex'");
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
    assert!(reply.contains("model_provider_mismatch"), "{reply}");
    assert!(reply.contains("已設定備援引擎，但模型設定本身有誤，因此沒有切換。"), "{reply}");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(spy.counts(), [0, 0]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    for visitor in [crate::odoo_livechat::session_key("owned", 42), "widget-visitor:fallback".into()] {
        let reply =
            build_reply_for_agent("hello", &context, "worker", &visitor, "owned-visitor", None).await;
        assert_eq!(reply, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE, "{visitor}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(spy.counts(), [0, 0]);
    }
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

#[tokio::test(flavor = "current_thread")]
async fn webchat_provider_failure_is_typed_and_persists_only_one_vocabulary_notice() {
    let (_home, ctx, _registry, _calls) = owned_channel(RuntimeType::Codex).await;
    let sid = "webchat:provider-failure";
    let (_, reason) = track_failure_reason(build_guarded_reply_for_agent(
        "普通訊息", &ctx, "worker", sid, "owner", None,
    )).await;
    assert_eq!(reason, Some(TurnFailureReason::Unavailable));
    let rows = ctx.session_manager.webchat_history(sid).await.unwrap();
    let notice = rows.iter().find(|r| r.notice_kind.as_deref() == Some("turn_failure")).unwrap();
    let turn = notice.client_msg_id.as_deref().unwrap().strip_prefix("turn_failure:").unwrap();
    ctx.session_manager.record_turn_failure(sid, turn, TurnFailureReason::Internal, None, None).await.unwrap();
    assert_eq!(ctx.session_manager.webchat_history(sid).await.unwrap().iter().filter(|r| r.notice_kind.as_deref() == Some("turn_failure")).count(), 1);
    assert!(notice.message.content.starts_with(TurnFailureReason::Unavailable.text(None)));
    assert!(ctx.session_manager.get_messages(sid).await.unwrap().iter().all(|m| m.role == "user"));
}

#[tokio::test(flavor = "current_thread")]
async fn disk_provider_and_model_edits_take_effect_without_registry_rescan() {
    for (before, after, before_model, after_model) in [
        ("codex", RuntimeType::Grok, "gpt-5.4", "grok-4"),
        ("grok", RuntimeType::Codex, "grok-4", "gpt-5.6-sol"),
    ] {
        let (home, ctx, _registry, calls) = owned_channel(after).await;
        let path = home.path().join("agents/worker/agent.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        let cached = original.replace(&format!("provider='{}'", after.as_str()), &format!("provider='{before}'"))
            .replace("preferred='gpt-5.4'", &format!("preferred='{before_model}'"));
        std::fs::write(&path, &cached).unwrap();
        ctx.registry.write().await.scan().await.unwrap();
        let live = cached.replace(&format!("provider='{before}'"), &format!("provider='{}'", after.as_str()))
            .replace(&format!("preferred='{before_model}'"), &format!("preferred='{after_model}'"));
        std::fs::write(&path, live).unwrap();
        let reply = build_reply_for_agent("普通訊息", &ctx, "worker", "discord:disk-edit", "owned-user", None).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "{reply}");
        assert!(!reply.contains("model_provider_mismatch"), "{reply}");
        assert_eq!(ctx.registry.read().await.get("worker").unwrap().config.model.preferred, before_model,
            "the test must leave the registry stale");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn disk_provider_only_mismatch_is_refused_before_any_session_or_model_call() {
    let model_calls_before = crate::model_call_probe::calls();
    let (home, ctx, _registry, calls) = owned_channel(RuntimeType::Codex).await;
    let path = home.path().join("agents/worker/agent.toml");
    let text = std::fs::read_to_string(&path).unwrap().replace("provider='codex'", "provider='grok'");
    std::fs::write(path, text).unwrap();
    let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
    let sid = "webchat:provider-edit";
    let reply = build_guarded_reply_for_agent("普通訊息", &ctx, "worker", sid, "owned-user", None).await;
    assert!(reply.text.contains("model_provider_mismatch"), "{}", reply.text);
    assert!(reply.text.contains("模型與執行引擎設定不相容"), "{}", reply.text);
    assert!(!reply.text.contains("unknown"), "{}", reply.text);
    assert_eq!(spy.counts(), [0, 0]);
    assert_refused_before_any_side_effect(&ctx, sid, &calls, model_calls_before).await;
    assert!(ctx.session_manager.webchat_history(sid).await.unwrap().is_empty(), "no stored notice without a session");
}

#[tokio::test(flavor = "current_thread")]
async fn misaligned_employee_is_refused_before_os_intent_goal_confirmation_and_knowledge_command() {
    let model_calls_before = crate::model_call_probe::calls();
    // OS-intent grey band: a system-operator employee and a message that names
    // two machine tools (L1 grey ⇒ L2 utility model when it is reached).
    let (home, ctx, _registry, calls) = misaligned_worker("[capabilities]\nsystem_operator=true\n").await;
    assert!(ctx.registry.read().await.get("worker").unwrap().config.capabilities.system_operator);
    let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
    let sid = "discord:gate-os";
    crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Personal);
    let reply = crate::on_behalf_of::scope_first(Some("unmapped:discord:owned-user".into()), async {
        assert!(operator_machine_access_allowed(home.path(), sid), "fixture must reach OS routing without the alignment gate");
        build_reply_for_agent("幫我看一下裝置狀態，然後建立備份", &ctx, "worker", sid, "owned-user",
            Some(Box::new(|_| panic!("refused turn emitted progress")))).await
    }).await;
    assert!(reply.contains("model_provider_mismatch"), "{reply}");
    // Not the operator-access refusal or an OS-intent short circuit either.
    assert!(!reply.contains(operator_access_refusal(None)), "{reply}");
    assert_eq!(spy.counts(), [0, 0]);
    assert_refused_before_any_side_effect(&ctx, sid, &calls, model_calls_before).await;

    // Pending goal confirmation: "1" creates a task; "2" generates a plan first.
    for choice in ["1", "2"] {
        let (home, ctx, _registry, calls) = misaligned_worker("").await;
        let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
        let sid = format!("discord:gate-goal-{choice}");
        let called = crate::trigger_route::called_session(&sid, "worker");
        for bucket in [sid.as_str(), called.as_str()] {
            crate::goal_intent::store_pending_for_test(bucket, "worker", "整理本週報表");
        }
        let reply = build_reply_for_agent(choice, &ctx, "worker", &sid, "owned-user",
            Some(Box::new(|_| panic!("refused turn emitted progress")))).await;
        assert!(reply.contains("model_provider_mismatch"), "{choice}: {reply}");
        // The confirmation interceptor consumes the slot first thing; it is
        // still there, so it never ran (no plan call, no task).
        for bucket in [sid.as_str(), called.as_str()] {
            assert!(crate::goal_intent::pending_button_nonce(bucket).is_some(), "{choice}: {bucket}");
        }
        assert_eq!(spy.counts(), [0, 0]);
        assert_refused_before_any_side_effect(&ctx, &sid, &calls, model_calls_before).await;
    }

    // An authenticated account, readable collection and adapter-owned file:
    // this command could file an approval if it crossed the alignment gate.
    let (home, ctx, _registry, calls) = misaligned_worker("").await;
    let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
    let sid = "webchat:gate-kb";
    let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    let admin = users.create_user("gate-kb@test.invalid", "Admin", "Test-Password-12345",
        duduclaw_auth::UserRole::Admin).unwrap();
    crate::knowledge::test_fixtures::collection(home.path(), "handbook", "internal", &[], "既有內容。");
    let file = home.path().join("agents/worker/attachments/rules.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "# 規定\n加班要事先申請。").unwrap();
    let text = format!("加入知識庫 handbook\n[📎 規定.md (file)]({})", file.display());
    crate::knowledge::chat_attachments::note_saved(&file, "規定.md");
    crate::knowledge::chat_attachments::claim_in_text(sid, &admin.id, &text);
    assert!(crate::knowledge::chat_attachments::owned(sid, &admin.id, &file).is_some());
    let reply = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(admin.id.clone(),
        build_reply_for_agent(&text, &ctx, "worker", sid, &admin.id,
            Some(Box::new(|_| panic!("refused turn emitted progress")))),
    ).await;
    assert!(reply.contains("model_provider_mismatch"), "{reply}");
    assert_eq!(spy.counts(), [0, 0]);
    assert_refused_before_any_side_effect(&ctx, sid, &calls, model_calls_before).await;
    // Positive control through the actual command, proving the fixture can
    // create a proposal. The refused turn above left no proposal or session.
    let control = crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(
        duduclaw_knowledge::requester::RequesterIdentity::DashboardUser { user_id: admin.id.clone() },
        crate::knowledge::chat_add::run_in_turn(&ctx, "worker", sid, &admin.id, &text, "handbook".into()),
    ).await;
    assert!(control.contains("已送出審核"), "{control}");
}

#[tokio::test(flavor = "current_thread")]
async fn broken_employee_can_stop_resume_and_query_failsafe_before_config_gate() {
    use duduclaw_security::failsafe::FailsafeLevel;
    let model_calls_before = crate::model_call_probe::calls();
    let (home, ctx, _registry, calls) = misaligned_worker("").await;
    ctx.channel_settings.set("discord", "global", crate::channel_settings::keys::ADMIN_USERS,
        r#"["owned-user"]"#).await.unwrap();
    let path = home.path().join("agents/worker/agent.toml");
    // Cover both a readable mismatch and a file that cannot be parsed.
    for broken in [false, true] {
        if broken { std::fs::write(&path, "[runtime").unwrap(); }
        let sid = "discord:gate-safety";
        let failsafe = ctx.failsafe.as_ref().unwrap();
        for (text, expected) in [
            ("!STOP", FailsafeLevel::L4Halted),
            ("!STATUS", FailsafeLevel::L4Halted),
            ("!RESUME", FailsafeLevel::L0Normal),
            ("!STOP ALL", FailsafeLevel::L4Halted),
            ("!RESUME", FailsafeLevel::L0Normal),
        ] {
            let reply = build_reply_for_agent(text, &ctx, "worker", sid, "owned-user", None).await;
            assert!(!reply.is_empty(), "{text}");
            assert!(!reply.contains("model_provider_mismatch"), "{reply}");
            assert!(!reply.contains("agent_config_unreadable"), "{reply}");
            assert_eq!(failsafe.get_level(sid).await, expected, "{text}");
            for denied in ["!STOP", "!STOP ALL", "!RESUME"] {
                let reply = build_reply_for_agent(denied, &ctx, "worker", sid, "non-admin", None).await;
                assert!(reply.contains("只有管理員"), "{denied}: {reply}");
                assert_eq!(failsafe.get_level(sid).await, expected, "{denied}: no state change");
            }
            // Inline STOP ALL must retain the old scope-only restriction.
            assert_eq!(failsafe.get_level("__global__").await, FailsafeLevel::L0Normal);
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(crate::model_call_probe::calls(), model_calls_before);
            assert!(ctx.session_manager.get_messages(sid).await.unwrap().is_empty());
            assert!(ctx.session_manager.session_agent(sid).await.unwrap().is_none());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_agent_toml_is_refused_with_a_specific_code_and_no_model_call() {
    let model_calls_before = crate::model_call_probe::calls();
    let (home, ctx, _registry, calls) = owned_channel(RuntimeType::Codex).await;
    // The registry keeps its cached (valid) snapshot; the file on disk breaks.
    let path = home.path().join("agents/worker/agent.toml");
    let broken = std::fs::read_to_string(&path).unwrap().replace("[runtime]", "[runtime");
    std::fs::write(&path, &broken).unwrap();
    let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
    let sid = "discord:broken-config";
    let reply = build_reply_for_agent("普通訊息", &ctx, "worker", sid, "owned-user", None).await;
    assert!(reply.contains("agent_config_unreadable"), "{reply}");
    assert!(reply.contains("無法讀取這位員工的設定"), "{reply}");
    assert!(!reply.contains("agent.toml"), "no path in the user text: {reply}");
    assert!(!reply.contains(&home.path().display().to_string()), "{reply}");
    assert!(!reply.contains("unknown"), "{reply}");
    assert!(!reply.contains("model_provider_mismatch"), "{reply}");
    assert_eq!(spy.counts(), [0, 0]);
    assert_refused_before_any_side_effect(&ctx, sid, &calls, model_calls_before).await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken, "reading never repairs the file");
    // A public visitor never sees configuration.
    let visitor = crate::odoo_livechat::session_key("owned", 43);
    let reply = build_reply_for_agent("hello", &ctx, "worker", &visitor, "owned-visitor", None).await;
    assert_eq!(reply, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn missing_openai_compat_model_is_refused_with_a_specific_code_and_no_model_call() {
    let model_calls_before = crate::model_call_probe::calls();
    let (home, ctx, _registry, calls) = owned_channel(RuntimeType::OpenAiCompat).await;
    // The loaded employee remains in the registry after its preferred model is lost.
    let path = home.path().join("agents/worker/agent.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    let broken = original.replace("preferred='gpt-5.4'\n", "");
    assert_ne!(broken, original, "fixture must remove the configured model");
    std::fs::write(&path, &broken).unwrap();
    let refusal = {
        let registry = ctx.registry.read().await;
        crate::entry_gate::resolve_and_check(home.path(), &*registry, "worker").unwrap_err()
    };
    assert_eq!(
        refusal,
        crate::entry_gate::EntryRefusal::Config(crate::runtime_config::RuntimeSettingsError::MissingModel),
    );
    let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
    let sid = "discord:missing-model";
    let reply = build_reply_for_agent("普通訊息", &ctx, "worker", sid, "owned-user", None).await;
    assert!(reply.contains("agent_config_unreadable"), "{reply}");
    assert!(reply.contains("無法讀取這位員工的設定"), "{reply}");
    assert!(!reply.contains("agent.toml"), "no path in the user text: {reply}");
    assert!(!reply.contains(&home.path().display().to_string()), "{reply}");
    assert!(!reply.contains("missing_model"), "{reply}");
    assert!(!reply.contains("preferred"), "{reply}");
    assert!(!reply.contains("MissingModel"), "{reply}");
    assert!(!reply.contains("model_provider_mismatch"), "{reply}");
    assert_eq!(spy.counts(), [0, 0]);
    assert_refused_before_any_side_effect(&ctx, sid, &calls, model_calls_before).await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken, "reading never repairs the file");
    // A public visitor never sees configuration.
    let visitor = crate::odoo_livechat::session_key("owned", 43);
    let reply = build_reply_for_agent("hello", &ctx, "worker", &visitor, "owned-visitor", None).await;
    assert_eq!(reply, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn turn_snapshot_survives_broken_source_for_public_and_operator_audiences() {
    let (home, ctx, _registry, calls) = owned_channel(RuntimeType::Codex).await;
    let raw = home.path().join("agents/worker/agent.toml");
    let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
    std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
    std::fs::copy(&raw, &resolved).unwrap();
    // Entry preflight reads the valid effective artifact. Prompt assembly must
    // reuse that turn snapshot instead of reopening the broken raw source.
    std::fs::write(&raw, "[broken-source").unwrap();
    let spy = super::reply_origin::testing::NoImplicitCloud::at(home.path());
    for (index, (sid, principal, public)) in [
        ("odoolivechat:owned:44", None, true),
        ("widget-visitor:profile", None, true),
        ("webchat:operator-profile", None, false),
        ("webchat:widget-profile", Some("widget-visitor:profile-account"), true),
    ].into_iter().enumerate() {
        let calls_before = calls.load(std::sync::atomic::Ordering::SeqCst);
        let turn = build_reply_for_agent("hello", &ctx, "worker", sid, "owned-user", None);
        let reply = match principal {
            Some(principal) => crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
                .scope(principal.into(), turn).await,
            None => turn.await,
        };
        // The fixture registers only the gated Codex runtime, which counts its
        // call before returning an auth error (not a successful model reply).
        assert_eq!(calls_before, index);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), calls_before + 1, "{sid}: {reply}");
        assert!(!reply.contains("agent_config_unreadable"), "{sid}: {reply}");
        assert!(!reply.contains("無法讀取這位員工的設定"), "{sid}: {reply}");
        if public {
            assert!(reply.starts_with(crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE), "{sid}: {reply}");
            for operator_text in ["Actual Worker", "Codex", "401", "invalid bearer token", "/login", "Claude"] {
                assert!(!reply.contains(operator_text), "{sid}: {reply}");
            }
        } else {
            assert!(reply.contains("Actual Worker"), "{reply}");
            assert!(reply.contains("Codex"), "{reply}");
        }
    }
    assert_eq!(spy.counts(), [0, 0]);
    assert_eq!(std::fs::read_to_string(&raw).unwrap(), "[broken-source");
}

/// Shared owned-home fixture for the chat and dispatch entry-point regressions.
pub(crate) async fn owned_claude_fallback(fallback: &str) -> (
    tempfile::TempDir, ReplyContext, crate::runtime_dispatch::testing::RegistryGuard,
    crate::runtime_dispatch::testing::RecordedFallback,
) {
    let (home, ctx, original, _) = owned_channel(RuntimeType::Claude).await;
    drop(original);
    // No real API account or inference service may be reached by this fixture.
    std::fs::write(home.path().join("config.toml"),
        "[general]\ninference_mode='claude'\n[goal_intent]\nenabled=false\nmode='off'\n").unwrap();
    let path = home.path().join("agents/worker/agent.toml");
    let text = std::fs::read_to_string(&path).unwrap()
        .replace("preferred='gpt-5.4'", "preferred='claude-does-not-exist'")
        .replace("provider='claude'", &if fallback.is_empty() {
            "provider='claude'".to_string()
        } else { format!("provider='claude'\nfallback='{fallback}'") });
    std::fs::write(&path, format!("{text}\n[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n")).unwrap();
    ctx.registry.write().await.scan().await.unwrap();
    let mut runtimes: std::collections::HashMap<RuntimeType, Box<dyn crate::runtime::AgentRuntime>> = Default::default();
    let calls: crate::runtime_dispatch::testing::RecordedFallback = Default::default();
    runtimes.insert(RuntimeType::Codex, Box::new(crate::runtime_dispatch::testing::RecordingCodex(calls.clone())));
    let guard = crate::runtime_dispatch::testing::RegistryGuard::install(home.path(),
        crate::runtime::RuntimeRegistry::with_runtimes(runtimes)).await;
    (home, ctx, guard, calls)
}

#[cfg(unix)]
#[test]
fn claude_chat_runtime_fallback_preserves_history_and_empty_fallback_stops() {
    crate::runtime_dispatch::testing::isolated_claude_test(
        concat!(module_path!(), "::claude_chat_runtime_fallback_preserves_history_and_empty_fallback_stops"), |cli_home| {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            for stream in [r#"{"type":"result","subtype":"success","is_error":true,"result":"404 model not found"}"#, ""] {
                std::fs::write(cli_home.join("stream.jsonl"), format!("{stream}\n")).unwrap();
                for fallback in ["codex", ""] {
                    let (home, ctx, _registry, calls) = owned_claude_fallback(fallback).await;
                    let sid = "discord:claude-fallback";
                    ctx.session_manager.get_or_create(sid, "worker").await.unwrap();
                    ctx.session_manager.append_message(sid, "user", "EARLIER_USER", 0).await.unwrap();
                    ctx.session_manager.append_message(sid, "assistant", "EARLIER_ASSISTANT", 0).await.unwrap();
                    let spy = super::reply_origin::testing::NoImplicitCloud::allowing_claude_at(home.path());
                    let reply = build_reply_for_agent("NEXT_USER", &ctx, "worker", sid, "owned-user", None).await;
                    let recorded = calls.lock().unwrap();
                    assert_eq!(recorded.len(), usize::from(!fallback.is_empty()), "{reply}");
                    assert_eq!(spy.counts()[1], 0, "no Direct API call: fallback={fallback:?}");
                    if !fallback.is_empty() {
                        assert!(reply.contains("CODEX_FALLBACK_OK"), "{reply}");
                        assert_eq!(recorded[0].model, RuntimeType::Codex.spec().fallback_models[0].0);
                        let history: Vec<_> = recorded[0].conversation_history.iter()
                            .map(|turn| (turn.role.as_str(), turn.content.as_str())).collect();
                        assert_eq!(history, vec![("user", "EARLIER_USER"), ("assistant", "EARLIER_ASSISTANT")]);
                        let argv = std::fs::read_to_string(cli_home.join("claude-argv")).unwrap();
                        assert!(argv.contains("EARLIER_USER") && argv.contains("EARLIER_ASSISTANT"));
                        assert_eq!(spy.counts(), [1, 0],
                            "explicit fallback must follow Claude and precede Direct API");
                        let audit = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
                        assert!(audit.lines().any(|row| {
                            let row: serde_json::Value = serde_json::from_str(row).unwrap();
                            row["event"] == "runtime_fallback_substitution" && row["actual"] == "codex"
                        }));
                    } else {
                        assert!(!reply.contains("CODEX_FALLBACK_OK"), "{reply}");
                    }
                }
            }
        });
    });
}

#[cfg(unix)]
#[test]
fn claude_chat_spawn_gate_and_budget_do_not_switch_runtime() {
    crate::runtime_dispatch::testing::isolated_claude_test(
        concat!(module_path!(), "::claude_chat_spawn_gate_and_budget_do_not_switch_runtime"), |cli_home| {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            std::fs::write(cli_home.join("stream.jsonl"), "").unwrap();
            for stop in ["spawn_gate", "budget"] {
                let (home, ctx, _registry, calls) = owned_claude_fallback("codex").await;
                let dir = home.path().join("agents/worker");
                if stop == "spawn_gate" {
                    std::fs::write(dir.join(".mcp.json"), "{broken").unwrap();
                } else {
                    // The singleton is confined to this isolated test process.
                    let _ = crate::cost_telemetry::init_telemetry(home.path());
                    crate::cost_telemetry::get_telemetry().unwrap().record("worker",
                        crate::cost_telemetry::RequestType::Chat, "claude-sonnet-5",
                        &crate::cost_telemetry::TokenUsage {
                            input_tokens: 1000, output_tokens: 50_000_000,
                            cache_read_tokens: 0, cache_creation_tokens: 0,
                        }).await;
                    assert!(crate::budget::gate_before_spawn(home.path(), Some(&dir), "worker", "test")
                        .await.is_err(), "fixture must exceed the budget");
                }
                let reply = build_reply_for_agent("NEXT_USER", &ctx, "worker",
                    "discord:claude-stopped", "owned-user", None).await;
                assert!(calls.lock().unwrap().is_empty(), "{stop}: {reply}");
                assert!(!reply.contains("CODEX_FALLBACK_OK"), "{stop}: {reply}");
                assert!(!cli_home.join("claude-argv").exists(),
                    "{stop}: stop must precede Claude model spawn too; argv={:?}; reply={reply}",
                    std::fs::read_to_string(cli_home.join("claude-argv")));
            }
        });
    });
}
