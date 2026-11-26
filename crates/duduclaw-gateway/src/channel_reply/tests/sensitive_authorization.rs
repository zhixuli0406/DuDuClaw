//! RM-147: capabilities reach live transport only, including existing sessions.
use super::*;
use crate::google_authorization_requests::{render, turn};
use tracing::instrument::WithSubscriber;

#[derive(Clone)]
struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn assert_no_ticket_on_disk(dir: &Path, ticket: &str) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() { assert_no_ticket_on_disk(&path, ticket); }
        else {
            let bytes = std::fs::read(&path).unwrap();
            assert!(!bytes.windows(ticket.len()).any(|part| part == ticket.as_bytes()), "{}", path.display());
        }
    }
}

async fn context(home: &Path) -> ReplyContext {
    crate::trigger_route::tests::write_agent(&home.join("agents"), "worker", "Worker", "@worker", "main", "");
    std::fs::write(home.join("config.toml"), "[general]\ndefault_agent='worker'\ndefault_language='zh-TW'\n").unwrap();
    let registry = Arc::new(tokio::sync::RwLock::new(duduclaw_agent::AgentRegistry::new(home.join("agents"))));
    registry.write().await.scan().await.unwrap();
    let sessions = Arc::new(SessionManager::new(&home.join("sessions.db")).unwrap());
    let status: ChannelStatusMap = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _) = tokio::sync::broadcast::channel(16);
    ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
}

#[tokio::test]
async fn existing_sessions_never_persist_authorization_tickets_on_any_channel() {
    for channel in ["webchat", "telegram", "slack", "discord", "line", "whatsapp", "feishu", "googlechat", "teams", "wecom", "dingtalk"] {
        for for_agent in [false, true] {
            for pipeline_stored in [false, true] {
                let home = tempfile::tempdir().unwrap();
                let ctx = context(home.path()).await;
                let sid = format!("{channel}:rm147-{}", uuid::Uuid::new_v4());
                ctx.session_manager.get_or_create(&sid, "worker").await.unwrap();
                let ticket = "a137bc90".repeat(8);
                let hint = render::notice(&format!("https://dashboard.example/me/google/authorize#ticket={ticket}"));
                let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
                let writer = LogWriter(logs.clone());
                let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                    .with_max_level(tracing::Level::TRACE).with_writer(move || writer.clone()).finish();
                let sessions = ctx.session_manager.clone();
                let stored_sid = sid.clone();
                let _job = crate::decision_notify::native_loop_fixture::install_named_job(
                    &crate::channel_decision_route::inner_test_seam_key(&sid), "authorization test", async move {
                        // The model seam only supplies the trusted host notice.
                        // Every subsequent operation uses the real reply/delivery pipeline.
                        turn::NOTICE.with(|cell| cell.lock().unwrap().text = Some(hint));
                        if pipeline_stored {
                            let id = append_turn_assistant(&sessions, &stored_sid,
                                crate::channel_decision_route::INNER_TEST_SEAM_REPLY, 0, None).await.unwrap();
                            record_turn_assistant_message(&stored_sid, id);
                        }
                    });
                let run = async {
                    let reply = if for_agent {
                        build_guarded_reply_for_agent("authorization test", &ctx, "worker", &sid, "member", None).await
                    } else {
                        build_guarded_reply_with_session("authorization test", &ctx, &sid, "member", None).await
                    };
                    assert!(reply.text.contains(&ticket), "the live transport still receives its capability");
                    assert!(reply.text.contains(crate::channel_decision_route::INNER_TEST_SEAM_REPLY));
                    tracing::debug!(?reply, "delivery diagnostic must use safe history text");
                    if channel == "webchat" {
                        // The model seam has no WebChat socket/state. Match
                        // production's pending assistant row before publication.
                        ctx.session_manager.acquire().await.execute(
                            "UPDATE session_messages SET delivery_pending=1, undone_at='pending' WHERE session_id=?1 AND role='assistant'", [&sid]).unwrap();
                        // Repeated staging of the actual transport text used to
                        // save the raw ticket in an existing session (Daybreak M1).
                        reply.persist_webchat(&ctx.session_manager, &sid, &reply.text).await.unwrap();
                        reply.persist_webchat(&ctx.session_manager, &sid, &reply.text).await.unwrap();
                        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
                        reply.publish_webchat(&ctx.session_manager, &sid, &users, users.acl_generation().unwrap(),
                            &crate::webchat::access::TargetPolicy::test_admin(), &tokio_util::sync::CancellationToken::new()).await.unwrap();
                    }
                }.with_subscriber(subscriber);
                if channel == "webchat" {
                    let ((), cleanup) = with_delivery_cleanup(run).await;
                    cleanup.unwrap();
                } else { run.await; }
                let history = ctx.session_manager.get_messages(&sid).await.unwrap();
                let assistant: Vec<_> = history.iter().filter(|row| row.role == "assistant").collect();
                assert_eq!(assistant.len(), 1, "{channel}: fixed history is stored exactly once");
                assert_eq!(assistant[0].content,
                    render::history_text(crate::channel_decision_route::INNER_TEST_SEAM_REPLY));
                assert!(crate::search_index::search_conversations(&home.path().join("sessions.db"), None, &ticket, 10).is_empty());
                assert!(!crate::search_index::search_conversations(&home.path().join("sessions.db"), None, render::HISTORY_NOTICE, 10).is_empty());
                assert!(!String::from_utf8(logs.lock().unwrap().clone()).unwrap().contains(&ticket));
                // Covers SQLite pages/WAL, CCR data, transcripts and file logs,
                // including rows hidden from normal history/search.
                assert_no_ticket_on_disk(home.path(), &ticket);
            }
        }
    }
}

#[tokio::test]
async fn webchat_sensitive_delivery_without_pipeline_row_stages_only_fixed_notice() {
    let home = tempfile::tempdir().unwrap();
    let ctx = context(home.path()).await;
    let sid = "webchat:rm147-no-assistant-row";
    ctx.session_manager.get_or_create(sid, "worker").await.unwrap();
    let ((), cleanup) = with_delivery_cleanup(async {
        let delivery = Arc::new(CcrTurnDelivery::default());
        CCR_TURN_DELIVERY.scope(delivery.clone(), async {
            mark_authorization_link(render::notice(&format!("/me/google/authorize#ticket={}", "b".repeat(64))));
        }).await;
        let reply = attach_turn_rollback(render::HISTORY_NOTICE.into(), None, Vec::new(), delivery, &ctx).await;
        reply.persist_webchat(&ctx.session_manager, sid, &reply.text).await.unwrap();
        let conn = ctx.session_manager.acquire().await;
        let raw: String = conn.query_row("SELECT content FROM session_messages WHERE session_id=?1", [sid], |row| row.get(0)).unwrap();
        assert_eq!(raw, render::HISTORY_NOTICE);
    }).await;
    cleanup.unwrap();
}

#[tokio::test]
async fn brand_new_webchat_authorization_link_is_live_only() {
    let home = tempfile::tempdir().unwrap();
    let ctx = context(home.path()).await;
    let sid = "webchat:rm147-brand-new";
    let ticket = "c".repeat(64);
    let ((), cleanup) = with_delivery_cleanup(async {
        let delivery = Arc::new(CcrTurnDelivery::default());
        CCR_TURN_DELIVERY.scope(delivery.clone(), async {
            mark_authorization_link(render::notice(&format!("/me/google/authorize#ticket={ticket}")));
        }).await;
        let reply = attach_turn_rollback(render::HISTORY_NOTICE.into(), None, Vec::new(), delivery, &ctx).await;
        assert!(reply.text.contains(&ticket));
        reply.persist_webchat(&ctx.session_manager, sid, &reply.text).await.unwrap();
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        reply.publish_webchat(&ctx.session_manager, sid, &users, users.acl_generation().unwrap(),
            &crate::webchat::access::TargetPolicy::test_admin(), &tokio_util::sync::CancellationToken::new()).await.unwrap();
    }).await;
    cleanup.unwrap();
    assert!(ctx.session_manager.turn_generation(sid).await.unwrap().is_none());
    assert_no_ticket_on_disk(home.path(), &ticket);
}
