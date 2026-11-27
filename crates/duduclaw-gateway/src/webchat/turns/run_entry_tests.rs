//! W3 matrix: actual turn runner, arrival preflight, and pending assignment.
use super::*;
use crate::channel_reply::entry_gate_test_helpers::{
    owned_channel, misaligned_worker, assert_refused_before_any_side_effect, assert_no_saved_attachments,
};
use duduclaw_core::types::RuntimeType;

fn state(ctx: crate::channel_reply::ReplyContext) -> (Arc<WebChatState>, String) {
    let home = &ctx.home_dir;
    crate::team_edition::record(home, duduclaw_core::EditionProfile::Enterprise);
    crate::mfa::reload_policy(home);
    let users = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let admin = users.create_user("entry@test.invalid", "Entry", "Test-Password-12345",
        duduclaw_auth::UserRole::Admin).unwrap();
    (Arc::new(WebChatState::new(Arc::new(ctx), Arc::new(JwtConfig::new(b"entry-gate-test")), users)), admin.id)
}

async fn turn(state: &Arc<WebChatState>, user: &str, sid: &str, content: &str, attach: bool) -> serde_json::Value {
    let (tx, mut rx) = outbound_channel();
    let ticket = state.turns.admit(user, sid, None, "entry", 3, 10).unwrap();
    let input = TurnInput {
        state: state.clone(), auth_user: user.into(), credential: None, user_id: user.into(),
        agent_id: "worker".into(), requested_agent: Some("worker".into()), effective_agent: Some("worker".into()),
        sid: sid.into(), conv_nonce: None, content: content.into(),
        attachments: if attach { vec![ChatAttachment { filename: "test.txt".into(), mime: Some("text/plain".into()),
            data_base64: "dGVzdA==".into() }] } else { vec![] },
        assign_event: Some("entry".into()), resume_acl: false, arrival_seq: 1,
    };
    run(input, TurnSink::new(tx, sid, None, "entry"), &ticket).await;
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await.expect("turn must answer").expect("socket open");
    let Message::Text(text) = output.message else { panic!("text frame") };
    serde_json::from_str(&text).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn broken_entry_matrix_refuses_before_upload_commands_or_models() {
    for broken in [false, true] {
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        let (state, admin) = state(ctx);
        if broken { std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap(); }
        let code = if broken { "agent_config_unreadable" } else { "model_provider_mismatch" };
        let before = crate::model_call_probe::calls();
        for (content, attachment) in [("hello", true), ("hello", false), ("/goal 做報表 || plan", false),
            ("/交辦 @worker 做報表", false), ("/交辦 @worker", false)] {
            let sid = "webchat:entry-refused";
            let arrival = crate::webchat::entry::check(&state, &admin, "worker", sid, &admin,
                content, attachment, "entry").await.unwrap_err();
            assert!(arrival.contains(code), "{arrival}");
            let frame = turn(&state, &admin, sid, content, attachment).await;
            assert_eq!(frame["type"], "error", "{frame}");
            assert!(frame["message"].as_str().unwrap().contains(code), "{frame}");
            assert_refused_before_any_side_effect(&state.ctx, sid, &calls, before).await;
            assert_no_saved_attachments(home.path());
        }
        let access = state.sender_access(&admin).await;
        let caller = crate::trigger_route::assign::AssignCaller {
            home: home.path(), channel: "webchat", conversation: "webchat:arm", sender: &admin,
            event: Some("arm"), seq: 1, fallback: Some("main"), access,
        };
        {
            let registry = state.ctx.registry.read().await;
            crate::trigger_route::assign::arm_on_admission(&registry, "/交辦 @worker", &caller);
        }
        assert!(crate::trigger_route::pending::peek(&crate::trigger_route::assign::pending_key(&caller), 2).is_none());
        for (user, sid) in [(admin.as_str(), "odoolivechat:test:1"), (admin.as_str(), "widget-visitor:test"),
            ("widget-visitor:test-account", "webchat:widget-account")] {
            let text = crate::webchat::entry::check(&state, user, "worker", sid, user, "hello", true, "entry")
                .await.unwrap_err();
            assert_eq!(text, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
            if user == admin.as_str() {
                let frame = turn(&state, user, sid, "hello", true).await;
                assert_eq!(frame["message"], crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn broken_settings_keep_management_commands_available() {
    for broken in [false, true] {
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        let (state, admin) = state(ctx);
        if broken { std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap(); }
        for text in ["!STOP", "!RESUME", "!STOP ALL", "/status", "/help", "/new", "/pair 123456", "/model", "/takeover"] {
            assert!(crate::webchat::entry::check(&state, &admin, "worker", "webchat:exempt", &admin, text, false, "entry").await.is_ok(), "{text}");
        }
        // Exercise the real command handlers as well as exemption classification.
        for text in ["!STOP", "!RESUME", "/help"] {
            let frame = turn(&state, &admin, "webchat:exempt", text, false).await;
            assert_eq!(frame["type"], "assistant_done", "{text}: {frame}");
            assert!(!frame.to_string().contains("model_provider_mismatch"));
            assert!(!frame.to_string().contains("agent_config_unreadable"));
        }
        let frame = turn(&state, &admin, "webchat:stop-with-file", "!STOP", true).await;
        assert_eq!(frame["type"], "error", "the file refusal is still reported");
        assert!(state.ctx.failsafe.as_ref().unwrap().get_state("webchat:stop-with-file").await.is_some());
        assert_no_saved_attachments(home.path());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pending_assignment_checks_its_target_and_keeps_the_pending_state() {
    use crate::trigger_route::pending::{self, PendingKey, PendingKind};
    for broken in [false, true] {
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        let (state, admin) = state(ctx);
        let sid = format!("webchat:pending-{}", uuid::Uuid::new_v4());
        let key = PendingKey::new("webchat", &sid, &admin);
        pending::arm_kind(&key, "worker", PendingKind::Assign, "arm", 1);
        if broken { std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap(); }
        let before = crate::model_call_probe::calls();
        let reply = crate::webchat::route::pending_assign_reply(&state, &admin, "做報表", Some("main"), "main",
            &sid, &admin, None, Some("task"), 2).await.unwrap();
        let value = serde_json::to_value(reply).unwrap();
        assert_eq!(value["type"], "error", "{value}");
        assert_eq!(pending::peek_kind(&key, 2), Some(("worker".into(), PendingKind::Assign)));
        assert_refused_before_any_side_effect(&state.ctx, &sid, &calls, before).await;
        assert_no_saved_attachments(home.path());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn healthy_employee_passes_arrival_and_creates_the_same_assignment() {
    let (home, ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
    let (state, admin) = state(ctx);
    for text in ["hello", "/goal 做報表", "/交辦 @worker 做報表"] {
        assert!(crate::webchat::entry::check(&state, &admin, "worker", "webchat:healthy", &admin, text, true, "entry").await.is_ok());
    }
    let frame = turn(&state, &admin, "webchat:healthy", "/交辦 @worker 做報表", false).await;
    assert_eq!(frame["type"], "assistant_done", "{frame}");
    assert_eq!(frame["task"]["agent"], "worker", "{frame}");
    let store = crate::task_store::TaskStore::open(home.path()).unwrap();
    assert_eq!(store.list_tasks(None, None, None).await.unwrap().len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn empty_target_uses_config_default_and_preserves_no_responder() {
    let (home, ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    std::fs::remove_dir_all(home.path().join("agents/main")).unwrap();
    ctx.registry.write().await.scan().await.unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\ndefault_agent='worker'\n").unwrap();
    let (state, admin) = state(ctx);
    assert!(crate::webchat::entry::check(&state, &admin, "", "webchat:default", &admin, "hello", false, "entry").await.is_ok());
    std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap();
    let error = crate::webchat::entry::check(&state, &admin, "", "webchat:default", &admin, "hello", false, "entry").await.unwrap_err();
    assert!(error.contains("agent_config_unreadable"), "{error}");
    std::fs::remove_dir_all(home.path().join("agents/worker")).unwrap();
    state.ctx.registry.write().await.scan().await.unwrap();
    for attachments in [false, true] {
        assert!(crate::webchat::entry::check(&state, &admin, "", "webchat:empty", &admin, "hello", attachments, "entry").await.is_ok());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn strict_decision_text_is_refused_before_admission_with_or_without_attachment() {
    for broken in [false, true] {
        let (home, ctx, _runtime, calls) = misaligned_worker("").await;
        let (state, admin) = state(ctx);
        if broken { std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap(); }
        let code = if broken { "agent_config_unreadable" } else { "model_provider_mismatch" };
        let before = crate::model_call_probe::calls();
        let command = format!("deny {}", uuid::Uuid::new_v4());
        assert!(crate::channel_decision_route::parse_strict_decision(&command).is_some());
        assert!(!crate::webchat::entry::continues_without_files(&command));
        for attachment in [false, true] {
            let sid = "webchat:decision-shaped-text";
            // Arrival preflight refuses before uploads or turn admission.
            let error = crate::webchat::entry::check(&state, &admin, "worker", sid, &admin,
                &command, attachment, "entry").await.unwrap_err();
            assert!(error.contains(code), "{error}");
            assert!(state.turns.entries.lock().unwrap().is_empty(), "no turn admitted");
            assert_no_saved_attachments(home.path());
            assert_refused_before_any_side_effect(&state.ctx, sid, &calls, before).await;

            // A turn already queued before settings broke is refused again by
            // the runner, without saving its files or reaching a model.
            let frame = turn(&state, &admin, sid, &command, attachment).await;
            assert_eq!(frame["type"], "error", "{frame}");
            assert!(frame["message"].as_str().unwrap().contains(code), "{frame}");
            assert_no_saved_attachments(home.path());
            assert_refused_before_any_side_effect(&state.ctx, sid, &calls, before).await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn widget_principal_gets_neutral_assignment_refusal() {
    let (home, ctx, _runtime, _calls) = misaligned_worker("").await;
    let (state, admin) = state(ctx);
    let caller = crate::trigger_route::assign::AssignCaller {
        home: home.path(), channel: "webchat", conversation: "webchat:widget-account", sender: &admin,
        event: None, seq: 1, fallback: Some("worker"), access: state.sender_access(&admin).await,
    };
    let cmd = crate::trigger_route::assign::AssignCommand::Text("@worker 做報表".into());
    let reply = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
        "widget-visitor:account".to_string(), crate::trigger_route::assign::run(&state.ctx, &cmd, &caller),
    ).await;
    assert_eq!(reply.text, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    let reply = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
        "widget-visitor:account".to_string(), crate::trigger_route::assign::run_pending(&state.ctx, "worker", "做報表", &caller),
    ).await;
    assert_eq!(reply.text, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    // Exercise the typed Entry branches after planning too, as if the
    // effective settings changed between planning and task creation.
    let lang = crate::trigger_route::texts::lang_for(home.path());
    let spec = {
        let registry = state.ctx.registry.read().await;
        let crate::trigger_route::assign::Plan::Create(spec) =
            crate::trigger_route::assign::plan(&*registry, lang, &cmd, &caller)
        else { panic!("expected authorized assignment plan") };
        spec
    };
    let reply = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
        "widget-visitor:account".to_string(),
        crate::trigger_route::assign_create::create(home.path(), lang, spec.clone(), &caller),
    ).await;
    assert_eq!(reply.text, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    let text = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
        "widget-visitor:account".to_string(),
        crate::trigger_route::schedule_chat::create(home.path(), lang, spec, "每週一 9:00", &caller),
    ).await;
    assert_eq!(text, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
}

#[tokio::test(flavor = "current_thread")]
async fn changed_pending_claim_checks_and_runs_the_claimed_employee() {
    use crate::trigger_route::pending::{self, PendingKey, PendingKind};
    for broken in [false, true] {
        let (home, ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
        let (state, admin) = state(ctx);
        let sid = format!("webchat:claim-race-{}", uuid::Uuid::new_v4());
        let key = PendingKey::new("webchat", &sid, &admin);
        pending::arm_kind(&key, "main", PendingKind::Assign, "first", 1);
        if broken { std::fs::write(home.path().join("agents/worker/agent.toml"), "[model").unwrap(); }
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let reply = crate::webchat::route::BEFORE_PENDING_CLAIM.scope(barrier.clone(),
            crate::webchat::route::pending_assign_reply(&state, &admin, "做報表", Some("main"), "main",
                &sid, &admin, None, Some("work"), 3));
        let replace = async {
            barrier.wait().await;
            pending::arm_kind(&key, "worker", PendingKind::Assign, "replacement", 2);
            barrier.wait().await;
        };
        let (reply, ()) = tokio::join!(reply, replace);
        let frame = serde_json::to_value(reply.expect("consumed pending must never fall through")).unwrap();
        if broken {
            assert_eq!(frame["type"], "error", "{frame}");
            assert!(frame["message"].as_str().unwrap().contains("agent_config_unreadable"));
        } else {
            assert_eq!(frame["type"], "assistant_done", "{frame}");
            assert_eq!(frame["task"]["agent"], "worker", "{frame}");
        }
        let tasks = crate::task_store::TaskStore::open(home.path()).unwrap().list_tasks(None, None, None).await.unwrap();
        assert_eq!(tasks.len(), if broken { 0 } else { 1 });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
