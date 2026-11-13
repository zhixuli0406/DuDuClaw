use super::*;
use crate::channel_reply::*;
use crate::session::SessionManager;
use duduclaw_agent::AgentRegistry;
use duduclaw_core::types::RuntimeType;
use std::sync::Arc;
use tokio::sync::RwLock;

fn fixture() -> (tempfile::TempDir, ReplyContext) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\ndefault_language='zh-TW'\n").unwrap();
    let registry = Arc::new(RwLock::new(AgentRegistry::new(home.path().join("agents"))));
    let sessions = Arc::new(SessionManager::new(&home.path().join("sessions.db")).unwrap());
    let status = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let (tx, _) = tokio::sync::broadcast::channel(16);
    let ctx = ReplyContext::new(registry, home.path().into(), sessions, status, tx);
    (home, ctx)
}

async fn assert_notice(ctx: &ReplyContext, sid: &str, reason: TurnFailureReason, users: usize) {
    let rows = ctx.session_manager.webchat_history(sid).await.unwrap();
    let notices: Vec<_> = rows.iter().filter(|r| r.notice_kind.as_deref() == Some("turn_failure")).collect();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].message.content, reason.text(None));
    assert_eq!(notices[0].message.tokens, 0);
    assert_eq!(ctx.session_manager.get_messages(sid).await.unwrap().len(), users);
}

#[tokio::test]
async fn removed_explicit_agent_never_creates_or_changes_a_conversation() {
    let (_home, ctx) = fixture();
    for existing in [false, true] {
        let sid = if existing { "telegram:existing" } else { "telegram:new" };
        if existing {
            ctx.session_manager.get_or_create(sid, "default").await.unwrap();
            ctx.session_manager.append_message(sid, "user", "old question", 1).await.unwrap();
            ctx.session_manager.append_message(sid, "assistant", "old answer", 1).await.unwrap();
        }
        let before = ctx.session_manager.webchat_history(sid).await.unwrap();
        let (reply, failed) = track_failure(super::super::inner::build_reply_with_session_inner(
            "question", &ctx, Some("removed"), sid, "sender", None,
        )).await;
        assert!(!failed, "a display notice must not change the transport failure signal");
        assert_eq!(reply, SELECTED_AGENT_UNAVAILABLE);
        assert_eq!(ctx.session_manager.webchat_history(sid).await.unwrap().len(), before.len());
        assert_eq!(ctx.session_manager.session_agent(sid).await.unwrap().as_deref(), existing.then_some("default"));
    }
}

#[tokio::test]
async fn pre_persist_gates_and_silent_drops_leave_no_notice() {
    let (home, ctx) = fixture();
    let sid = "line:early";
    ctx.session_manager.get_or_create(sid, "worker").await.unwrap();
    ctx.session_manager.append_message(sid, "user", "old question", 1).await.unwrap();
    ctx.session_manager.append_message(sid, "assistant", "old answer", 1).await.unwrap();
    std::fs::write(home.path().join("KILLSWITCH.toml"), "[triggers]\nmax_replies_per_minute = 1\n").unwrap();
    assert!(crate::killswitch_triggers::gate_before_reply(&ctx, sid, "sender", "worker").await.is_none());
    assert_eq!(crate::killswitch_triggers::gate_before_reply(&ctx, sid, "sender", "worker").await, Some(String::new()));
    assert!(model_alignment_gate(&ctx, sid, "worker", "t1", "gpt-5.4", RuntimeType::Claude, None).await.is_err());
    assert_eq!(ctx.session_manager.webchat_history(sid).await.unwrap().len(), 2);
    assert!(ctx.session_manager.webchat_history(sid).await.unwrap().iter().all(|r| r.notice_kind.is_none()));
}

async fn bound<F: std::future::Future>(sid: &str, generation: &str, work: F) -> F::Output {
    FAILURE_SESSION.scope(Arc::new(std::sync::Mutex::new(Some((sid.into(), generation.into(), "worker".into())))), work).await
}

#[tokio::test]
async fn late_model_alignment_failure_stays_with_the_stored_user_turn() {
    let (_home, ctx) = fixture();
    let session = ctx.session_manager.get_or_create("slack:late", "worker").await.unwrap();
    ctx.session_manager.append_message("slack:late", "user", "question", 1).await.unwrap();
    bound("slack:late", &session.created_at, async {
    for _ in 0..2 {
        assert!(model_alignment_gate(&ctx, "slack:late", "worker", "t1", "gpt-5.4", RuntimeType::Claude, Some(&session.created_at)).await.is_err());
    }
    }).await;
    assert_notice(&ctx, "slack:late", TurnFailureReason::Unavailable, 1).await;
}

#[tokio::test]
async fn active_provider_read_failure_writes_one_fixed_notice() {
    let (home, ctx) = fixture();
    let agent_dir = home.path().join("invalid-private-path");
    std::fs::create_dir(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("agent.toml"), "[invalid TOML").unwrap();
    let session = ctx.session_manager.get_or_create("teams:late", "worker").await.unwrap();
    ctx.session_manager.append_message("teams:late", "user", "question", 1).await.unwrap();
    bound("teams:late", &session.created_at, async {
    for _ in 0..2 {
        assert!(active_provider_gate(&ctx, "teams:late", "worker", "t1", &agent_dir, Some(&session.created_at)).await.is_err());
    }
    }).await;
    assert_notice(&ctx, "teams:late", TurnFailureReason::Unavailable, 1).await;
}

#[tokio::test]
async fn public_widget_always_uses_generic_vocabulary_and_odoo_keeps_notice_suppression() {
    let (_home, ctx) = fixture();
    let sid = "webchat:widget";
    let origin = crate::session_origin::SessionOrigin::of_kind(crate::session_origin::OriginKind::WebchatVisitor);
    ctx.session_manager.get_or_create_with_origin(sid, "worker", &origin).await.unwrap();
    ctx.session_manager.append_message(sid, "user", "question", 1).await.unwrap();
    for reason in [TurnFailureReason::Timeout, TurnFailureReason::Unavailable, TurnFailureReason::SafetyGate] {
        ctx.session_manager.record_turn_failure(sid, "t1", reason, None, None).await.unwrap();
    }
    assert_notice(&ctx, sid, TurnFailureReason::Unknown, 1).await;
    ctx.session_manager.record_turn_failure("odoolivechat:visitor", "t1", TurnFailureReason::Unavailable, None, None).await.unwrap();
    assert!(ctx.session_manager.webchat_history("odoolivechat:visitor").await.unwrap().is_empty());
}

#[tokio::test]
async fn terminal_notices_are_idempotent_across_managers_and_do_not_enter_model_history() {
    let (home, ctx) = fixture();
    let sid = "discord:failure";
    let session = ctx.session_manager.get_or_create(sid, "worker").await.unwrap();
    ctx.session_manager.append_message(sid, "user", "question", 1).await.unwrap();
    let other = SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let (a, b) = tokio::join!(
        ctx.session_manager.record_turn_failure(sid, "t1", TurnFailureReason::Timeout, None, Some(&session.created_at)),
        other.record_turn_failure(sid, "t1", TurnFailureReason::Internal, None, Some(&session.created_at)),
    );
    a.unwrap(); b.unwrap();
    assert_notice(&ctx, sid, TurnFailureReason::Timeout, 1).await;
    // A different turn can settle independently; a stale generation cannot.
    other.record_turn_failure(sid, "t2", TurnFailureReason::Unknown, None, Some("stale-generation")).await.unwrap();
    assert_notice(&ctx, sid, TurnFailureReason::Timeout, 1).await;
    other.record_turn_failure(sid, "t2", TurnFailureReason::Unknown, None, Some(&session.created_at)).await.unwrap();
    assert_eq!(ctx.session_manager.webchat_history(sid).await.unwrap().len(), 3);
    assert_eq!(ctx.session_manager.get_messages(sid).await.unwrap()[0].role, "user");
    assert_eq!(ctx.session_manager.read_last_n_turns_text(sid, 10).await.unwrap(), "user: question\n");
}

#[test]
fn closed_vocabulary_covers_every_reason_and_falls_back_to_traditional_chinese() {
    let reasons = [TurnFailureReason::Timeout, TurnFailureReason::Unavailable, TurnFailureReason::Quota,
        TurnFailureReason::Cancelled, TurnFailureReason::SafetyGate, TurnFailureReason::Internal, TurnFailureReason::Unknown];
    for language in ["zh-TW", "zh-CN", "zh_CN", "en", "en-US", "en_US", "ja", "ja-JP", "ja_JP"] {
        let words: std::collections::HashSet<_> = reasons.iter().map(|reason| reason.text(Some(language))).collect();
        assert_eq!(words.len(), reasons.len());
        if language.starts_with("zh") {
            assert!(words.iter().all(|text| !text.contains([',', '.']) && text.contains('，') && text.ends_with('。')));
        }
        assert!(words.iter().all(|text| !text.contains("/login") && !text.contains("provider") && !text.contains("stack")));
    }
    for reason in reasons {
        assert_eq!(reason.text(Some("unknown-language")), reason.text(None));
    }
    for reason in reasons {
        assert_eq!(reason.text(Some("en-US")), reason.text(Some("en")));
        assert_eq!(reason.text(Some("ja")), reason.text(Some("ja-JP")));
        assert_eq!(reason.text(Some("zh_CN")), reason.text(Some("zh-CN")));
    }
    assert_eq!(TurnFailureReason::from(FailureReason::Unknown), TurnFailureReason::Unknown);
    assert_eq!(TurnFailureReason::from(FailureReason::Billing), TurnFailureReason::Quota);
}

#[tokio::test]
async fn notice_language_follows_the_existing_global_fixed_message_setting() {
    let (home, ctx) = fixture();
    for language in ["en", "en-US", "ja", "ja-JP", "zh-TW", "zh-CN"] {
        std::fs::write(home.path().join("config.toml"), format!("[general]\ndefault_language='{language}'\n")).unwrap();
        let sid = format!("telegram:{language}");
        let session = ctx.session_manager.get_or_create(&sid, "worker").await.unwrap();
        ctx.session_manager.append_message(&sid, "user", "question", 1).await.unwrap();
        bound(&sid, &session.created_at, persist_failure_notice(&ctx, &sid, "worker", "t1", TurnFailureReason::Timeout, Some(&session.created_at))).await;
        let rows = ctx.session_manager.webchat_history(&sid).await.unwrap();
        assert_eq!(rows[1].message.content, TurnFailureReason::Timeout.text(Some(language)));
    }
}

#[tokio::test]
async fn all_channels_record_post_contract_empty_output_and_adapter_panic_once() {
    use futures_util::FutureExt;
    for channel in ["line", "telegram", "discord", "webchat"] {
        for panic in [false, true] {
            let (home, ctx) = fixture();
            crate::trigger_route::tests::write_agent(&home.path().join("agents"), "worker", "Worker", "@worker", "main", "");
            std::fs::write(home.path().join("config.toml"), "[general]\ndefault_agent='worker'\n").unwrap();
            ctx.registry.write().await.scan().await.unwrap();
            let mut seam = crate::agent_channels::e2e_seam::arm_waiting(home.path());
            crate::agent_channels::e2e_seam::empty_or_panicking_reply(home.path(), panic);
            let sid = format!("{channel}:adapter");
            // Both public guarded entry points cover the full adapter scope.
            let work = async {
                if channel == "line" {
                    build_guarded_reply_with_session("question", &ctx, &sid, "sender", None).await
                } else {
                    build_guarded_reply_for_agent("question", &ctx, "worker", &sid, "sender", None).await
                }
            };
            let (result, ()) = tokio::join!(std::panic::AssertUnwindSafe(work).catch_unwind(), async {
                seam.entered.recv().await.unwrap().release.send(()).unwrap();
            });
            assert_eq!(result.is_err(), panic);
            if let Ok(reply) = result { assert!(reply.text.is_empty()); }
            assert_notice(&ctx, &sid, TurnFailureReason::Internal, 1).await;
        }
    }
}

#[tokio::test]
async fn persisted_notice_does_not_change_the_transport_failure_signal() {
    let (_home, ctx) = fixture();
    let sid = "webchat:live";
    let session = ctx.session_manager.get_or_create(sid, "worker").await.unwrap();
    ctx.session_manager.append_message(sid, "user", "question", 1).await.unwrap();
    let (_, failed) = track_failure(bound(sid, &session.created_at,
        persist_failure_notice(&ctx, sid, "worker", "turn", TurnFailureReason::Quota, Some(&session.created_at)),
    )).await;
    assert!(!failed);
    assert_notice(&ctx, sid, TurnFailureReason::Quota, 1).await;
}

#[tokio::test]
async fn notice_rejects_a_session_owned_by_another_employee() {
    let (_home, ctx) = fixture();
    let sid = "line:other-owner";
    let session = ctx.session_manager.get_or_create(sid, "other").await.unwrap();
    ctx.session_manager.append_message(sid, "user", "old question", 1).await.unwrap();
    bound(sid, &session.created_at, persist_failure_notice(&ctx, sid, "worker", "turn", TurnFailureReason::Internal, Some(&session.created_at))).await;
    assert_eq!(ctx.session_manager.webchat_history(sid).await.unwrap().len(), 1);
    assert_eq!(ctx.session_manager.session_agent(sid).await.unwrap().as_deref(), Some("other"));
}

async fn failed_webchat_reply(ctx: &ReplyContext, sid: &str, reason: FailureReason) -> GuardedReply {
    let session = ctx.session_manager.get_or_create(sid, "worker").await.unwrap();
    ctx.session_manager.append_message(sid, "user", "question", 1).await.unwrap();
    let delivery = Arc::new(super::super::ccr_delivery::CcrTurnDelivery::default());
    super::super::ccr_delivery::CCR_TURN_DELIVERY.scope(delivery.clone(), bound(sid, &session.created_at, async {
        super::super::record_turn_generation(Some(&session.created_at));
        persist_failure_notice(ctx, sid, "worker", "failed-turn", reason.into(), Some(&session.created_at)).await;
        // Repeated terminal handling must still leave only one notice.
        persist_failure_notice(ctx, sid, "worker", "failed-turn", reason.into(), Some(&session.created_at)).await;
    })).await;
    let live = format_fallback_message("Worker", reason, &ctx.home_dir);
    GuardedReply::new(live, None, Vec::new(), Some(super::super::ccr_delivery::TurnRollback {
        delivery, session_mgr: ctx.session_manager.clone(),
    })).await
}

#[tokio::test]
async fn spawn_failure_webchat_persists_one_notice_without_ordinary_legacy_reply() {
    let (home, ctx) = fixture();
    for reason in [FailureReason::SpawnError, FailureReason::BinaryMissing, FailureReason::AuthFailed,
        FailureReason::Timeout, FailureReason::Unknown] {
        let sid = format!("webchat:{}", reason.as_str());
        let reply = failed_webchat_reply(&ctx, &sid, reason).await;
        if reason == FailureReason::SpawnError {
            assert!(reply.text.contains("子程序失敗"));
            assert!(reply.text.contains("~/.duduclaw/debug.log"));
        }
        let live = reply.text.clone();
        reply.persist_webchat(&ctx.session_manager, &sid, &live).await.unwrap();
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        reply.publish_webchat(&ctx.session_manager, &sid, &users, users.acl_generation().unwrap(),
            &crate::webchat::access::TargetPolicy::test_admin(), &tokio_util::sync::CancellationToken::new()).await.unwrap();
        assert_eq!(reply.text, live, "live diagnostic delivery must remain unchanged");
        assert_eq!(ctx.session_manager.webchat_history(&sid).await.unwrap().len(), 2);
        assert_notice(&ctx, &sid, reason.into(), 1).await;
        let conn = rusqlite::Connection::open(home.path().join("sessions.db")).unwrap();
        let rows: Vec<(i64, Option<String>)> = conn.prepare(
            "SELECT hidden, notice_kind FROM session_messages WHERE session_id=?1 AND role='assistant'",
        ).unwrap().query_map([&sid], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
            .collect::<Result<_, _>>().unwrap();
        assert_eq!(rows, vec![(1, Some("turn_failure".into()))]);
    }
}

#[tokio::test]
async fn follow_up_history_excludes_spawn_failure_legacy_text() {
    let (_home, ctx) = fixture();
    let sid = "webchat:spawn-follow-up";
    let reply = failed_webchat_reply(&ctx, sid, FailureReason::SpawnError).await;
    reply.persist_webchat(&ctx.session_manager, sid, &reply.text).await.unwrap();
    ctx.session_manager.append_message(sid, "user", "follow-up", 1).await.unwrap();
    let history = ctx.session_manager.get_messages(sid).await.unwrap();
    assert_eq!(history.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect::<Vec<_>>(),
        vec![("user", "question"), ("user", "follow-up")]);
    assert_eq!(ctx.session_manager.read_last_n_turns_text(sid, 10).await.unwrap(),
        "user: question\nuser: follow-up\n");
}

#[tokio::test]
async fn notice_only_webchat_delivery_rejects_recreated_session_before_persist_or_publish() {
    for clear_before_persist in [true, false] {
        let (home, ctx) = fixture();
        let sid = "webchat:notice-cleared";
        let reply = failed_webchat_reply(&ctx, sid, FailureReason::SpawnError).await;
        assert_notice(&ctx, sid, TurnFailureReason::Internal, 1).await;
        let verdict = reply.rollback.as_ref().unwrap().delivery.subscribe();
        let old_generation = ctx.session_manager.turn_generation(sid).await.unwrap().unwrap();
        if !clear_before_persist {
            reply.persist_webchat(&ctx.session_manager, sid, &reply.text).await.unwrap();
        }
        ctx.session_manager.clear_for_user(sid).await.unwrap().expect("cleared");
        let fresh = ctx.session_manager.get_or_create(sid, "worker").await.unwrap();
        assert_ne!(fresh.created_at, old_generation);
        if clear_before_persist {
            assert!(reply.persist_webchat(&ctx.session_manager, sid, &reply.text).await.is_err());
        }
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        assert!(reply.publish_webchat(&ctx.session_manager, sid, &users, users.acl_generation().unwrap(),
            &crate::webchat::access::TargetPolicy::test_admin(), &tokio_util::sync::CancellationToken::new()).await.is_err());
        assert!(!reply.still_valid().await, "the stale delivery must latch refusal");
        drop(reply);
        assert_eq!(verdict.await, Ok(false), "no successful publication verdict");
        assert!(ctx.session_manager.webchat_history(sid).await.unwrap().is_empty());
        let rows: i64 = rusqlite::Connection::open(home.path().join("sessions.db")).unwrap()
            .query_row("SELECT COUNT(*) FROM session_messages WHERE session_id=?1", [sid], |row| row.get(0)).unwrap();
        assert_eq!(rows, 0, "no diagnostic or notice may enter the fresh conversation");
    }
}

#[tokio::test]
async fn notice_only_webchat_delivery_still_rejects_stale_acl_and_cancellation() {
    let (home, ctx) = fixture();
    let users_path = home.path().join("users.db");
    let users = duduclaw_auth::UserDb::new(&users_path).unwrap();
    let generation = users.acl_generation().unwrap();
    rusqlite::Connection::open(&users_path).unwrap().execute(
        "UPDATE auth_meta SET value=value+1 WHERE key='acl_generation'", [],
    ).unwrap();
    for cancelled in [false, true] {
        let sid = format!("webchat:fenced-{cancelled}");
        let reply = failed_webchat_reply(&ctx, &sid, FailureReason::SpawnError).await;
        reply.persist_webchat(&ctx.session_manager, &sid, &reply.text).await.unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let generation = if cancelled { cancel.cancel(); users.acl_generation().unwrap() } else { generation };
        assert!(reply.publish_webchat(&ctx.session_manager, &sid, &users, generation,
            &crate::webchat::access::TargetPolicy::test_admin(), &cancel).await.is_err());
        assert_notice(&ctx, &sid, TurnFailureReason::Internal, 1).await;
    }
}
