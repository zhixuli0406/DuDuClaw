//! Channel-origin regression tests. These deliberately exercise the host run
//! mapping without reading or changing the test process environment.
use super::*;
use std::sync::Arc;
use duduclaw_gateway::isolation_egress::{RunMetadata, RunTurnIdentity, RunTurnResolver};
use duduclaw_mcp_boundary::{RequestContext, VerificationError};

/// The bound channel sender of the fixture account.
const SENDER: &str = "person";

struct HostTurn { reply: Option<String>, turn: String }
impl RunTurnResolver for HostTurn {
    fn resolve(&self, _: &RequestContext) -> std::result::Result<RunTurnIdentity, VerificationError> {
        Ok(RunTurnIdentity { turn: self.turn.clone(), approval_session: Some("conversation".into()) })
    }
    fn metadata(&self, _: &RequestContext) -> std::result::Result<Option<RunMetadata>, VerificationError> {
        Ok(Some(RunMetadata { reply_channel: self.reply.clone(), reply_sender: Some("sales".into()),
            ..RunMetadata::root() }))
    }
}

/// A live gateway turn of `sales` answering `reply`, for the fixture account
/// (verified Google Chat binding). The session row of `reply` exists, so the
/// gateway can mint an origin proof for a supported channel conversation.
async fn in_turn<F: std::future::Future>(home: &Path, reply: Option<&str>, future: F) -> F::Output {
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
    let turn = uuid::Uuid::new_v4().to_string();
    let user = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap()
        .find_verified_user_id_by_channel("googlechat", SENDER).unwrap().unwrap();
    let _transport = match reply {
        Some(reply) => {
            // A live channel conversation row, as the reply pipeline stamps it.
            duduclaw_gateway::session::SessionManager::new(&home.join("sessions.db")).unwrap()
                .get_or_create_with_origin(reply, "sales", &duduclaw_gateway::session_origin::SessionOrigin::of_kind(
                    duduclaw_gateway::session_origin::OriginKind::Channel)).await.unwrap();
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(reply.to_owned()), async {
                duduclaw_gateway::turn_context::register("sales", &turn, reply)
            }).await
        }
        None => None,
    };
    let _requester = duduclaw_gateway::knowledge::turn_requesters::register("sales", &turn,
        duduclaw_knowledge::requester::RequesterIdentity::ChannelUser {
            channel: "googlechat".into(), chat: "spaces/ORIGIN".into(), sender: SENDER.into(), user_id: Some(user),
            attested_private: false,
        });
    let context = RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Background { tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
        gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
    });
    duduclaw_gateway::isolation_egress::scope(home, "sales", &context,
        Some(Arc::new(HostTurn { reply: reply.map(str::to_owned), turn })), future).await.unwrap()
}

/// A home with the `sales` employee and one Employee account bound to it
/// (Operator) through a verified Google Chat identity.
fn home() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agents/sales")).unwrap();
    std::fs::write(dir.path().join("agents/sales/agent.toml"),
        "[agent]\nname='sales'\nstatus='active'\n[model]\npreferred='claude-sonnet-4-6'\n").unwrap();
    let db = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
    let user = db.create_user("origin@test.invalid", "Origin", "fixture-password",
        duduclaw_auth::UserRole::Employee).unwrap();
    db.bind_agent(&user.id, "sales", duduclaw_auth::AccessLevel::Operator).unwrap();
    db.bind_channel_identity(&user.id, "googlechat", SENDER, true).unwrap();
    dir
}

#[tokio::test]
async fn cron_reminder_and_one_shot_default_to_google_chat() {
    let home = home(); let h = home.path();
    in_turn(h, Some("googlechat:spaces/ORIGIN"), async {
        let cron = handle_schedule_task_for_actor(&serde_json::json!({
            "agent_id":"sales", "cron":"0 9 * * *", "task":"report", "name":"Daily"
        }), h, RecordActor::Agent("sales")).await;
        assert_ne!(cron["isError"], true, "{cron}");
        let rows = duduclaw_gateway::cron_store::CronStore::open(h).unwrap().list_all().await.unwrap();
        assert_eq!(rows[0].notify_channel.as_deref(), Some("googlechat"));
        assert_eq!(rows[0].notify_chat_id.as_deref(), Some("spaces/ORIGIN"));
        assert_eq!(rows[0].notify_sender.as_deref(), Some("sales"));
        assert_eq!(rows[0].notify_origin.as_deref(), Some("googlechat:spaces/ORIGIN"));
        // H2: the origin is stored with only the opaque id of the gateway
        // record, which the gateway re-verifies at send.
        let proof = rows[0].notify_origin_proof.as_deref().expect("gateway origin proof id");
        assert!(proof.len() == 64 && proof.bytes().all(|b| b.is_ascii_hexdigit()), "an opaque id, not a tuple");
        let record = duduclaw_gateway::recipient_guard::origin::verify(h, "sales", "googlechat:spaces/ORIGIN", Some(proof)).unwrap();
        assert_eq!(record.channel_user_id, SENDER);
        let reminder = handle_create_reminder(&serde_json::json!({"time":"10m", "message":"check"}),
            h, RecordActor::Agent("sales")).await;
        assert_ne!(reminder["isError"], true, "{reminder}");
        let once = super::super::handle_tasks_create(&serde_json::json!({
            "title":"Once", "assigned_to":"sales", "schedule":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()
        }), h, RecordActor::Agent("sales")).await;
        assert_ne!(once["isError"], true, "{once}");
        let rows = duduclaw_gateway::reminder_scheduler::list_reminders(h, None, None).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.channel == "googlechat" && r.chat_id == "spaces/ORIGIN"));
        assert!(rows.iter().all(|r| r.notify_origin.as_deref() == Some("googlechat:spaces/ORIGIN")
            && r.notify_origin_proof.is_some()));
    }).await;
}

#[tokio::test]
async fn goal_source_is_the_host_bound_conversation() {
    let home = home(); let h = home.path();
    in_turn(h, Some("googlechat:spaces/GOAL"), async {
        let result = super::super::handle_tasks_create(&serde_json::json!({
            "kind":"goal", "title":"Goal", "description":"Prepare report", "assigned_to":"sales"
        }), h, RecordActor::Agent("sales")).await;
        assert_ne!(result["isError"], true, "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let result: Value = serde_json::from_str(text).unwrap();
        let id = result["task"]["id"].as_str().unwrap();
        let task = duduclaw_gateway::task_store::TaskStore::open(h).unwrap().get_task(id).await.unwrap().unwrap();
        assert_eq!(task.source_channel.as_deref(), Some("googlechat"));
        assert_eq!(task.source_chat_id.as_deref(), Some("spaces/GOAL"));
        // H3: stored with the proof every goal push re-verifies.
        assert!(task.source_origin_proof.is_some());
    }).await;
}

/// H1/H3: a creator who cannot be proven (here: the account was suspended
/// before the turn) gets no default destination and no goal source at all.
#[tokio::test]
async fn unprovable_creator_gets_no_default_origin() {
    let home = home(); let h = home.path();
    let db = duduclaw_auth::UserDb::new(&h.join("users.db")).unwrap();
    let user = db.find_verified_user_id_by_channel("googlechat", SENDER).unwrap().unwrap();
    db.set_user_status(&user, duduclaw_auth::UserStatus::Suspended).unwrap();
    in_turn(h, Some("googlechat:spaces/ORIGIN"), async {
        assert!(schedule_origin(h, Some("sales"), "sales").await.is_none());
        let cron = handle_schedule_task_for_actor(&serde_json::json!({
            "agent_id":"sales", "cron":"0 9 * * *", "task":"report", "name":"Daily"
        }), h, RecordActor::Agent("sales")).await;
        assert_ne!(cron["isError"], true, "{cron}");
        let rows = duduclaw_gateway::cron_store::CronStore::open(h).unwrap().list_all().await.unwrap();
        assert!(rows[0].notify_channel.is_none() && rows[0].notify_origin.is_none() && rows[0].notify_origin_proof.is_none());
    }).await;
}

#[tokio::test]
async fn explicit_target_wins_and_other_executors_never_inherit_origin() {
    let home = home(); let h = home.path();
    in_turn(h, Some("googlechat:spaces/ORIGIN"), async {
        let context = turn_context(h, Some("sales"), "sales").await;
        assert!(context.as_ref().is_some_and(|context| context.origin_proof_id.is_some()));
        let explicit = resolve_turn_notify_target(&serde_json::json!({"notify_channel":"line", "notify_chat_id":"other"}),
            Some("sales"), "sales", context.as_ref()).unwrap();
        assert_eq!(explicit.channel.as_deref(), Some("line"));
        assert_eq!(explicit.chat.as_deref(), Some("other"));
        assert_eq!(resolve_turn_notify_target(&serde_json::json!({}), Some("sales"), "other", context.as_ref())
            .unwrap().delivery(), Delivery::Nowhere);
        assert!(schedule_origin(h, None, "sales").await.is_none());
        assert!(schedule_origin(h, Some("other"), "other").await.is_none());
    }).await;
    // No host run, even if a tool supplied a forged origin argument.
    assert!(schedule_origin(h, Some("sales"), "sales").await.is_none());
    for raw in [None, Some("googlechat:"), Some("webchat:fake"), Some("mail:x@y.test")] {
        in_turn(h, raw, async {
            let context = turn_context(h, Some("sales"), "sales").await;
            assert_eq!(resolve_turn_notify_target(&serde_json::json!({"reply_channel":"googlechat:spaces/FORGED"}),
                Some("sales"), "sales", context.as_ref()).unwrap().delivery(), Delivery::Nowhere);
        }).await;
    }
}

#[test]
fn delivery_failure_is_in_the_mcp_listing() {
    let mut row = duduclaw_gateway::cron_store::CronTaskRow::new("id".into(), "report".into(), "sales".into(), "0 9 * * *".into(), "report".into());
    row.last_status = Some("success".into());
    row.last_delivery_status = Some("delivery_failed".into());
    row.last_delivery_reason = Some("notification_delivery_failed".into());
    let result = render_cron_tasks(&[row], "sales");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("last_status: success"));
    assert!(text.contains("delivery_failed"));
    assert!(text.contains("notification_delivery_failed"));
}

#[tokio::test]
async fn forged_process_reply_channel_cannot_establish_a_source() {
    const CHILD: &str = "DUDUCLAW_SCHEDULE_ORIGIN_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp::cron_tasks::channel_schedule_tests::forged_process_reply_channel_cannot_establish_a_source"])
            .env(CHILD, "1")
            .env(duduclaw_core::ENV_REPLY_CHANNEL, "googlechat:spaces/FORGED")
            .env(duduclaw_core::ENV_REPLY_SENDER, "ceo")
            .env_remove(duduclaw_core::ENV_TRUST_TURN_ID)
            .env_remove(duduclaw_core::egress_guard::ENV_TURN_ALIAS)
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    let home = home(); let h = home.path();
    assert!(schedule_origin(h, Some("sales"), "sales").await.is_none());
    assert!(turn_context(h, Some("sales"), "sales").await.is_none());
    assert!(schedule_reply_sender(h, Some("sales"), "sales", "googlechat", "spaces/FORGED").await.is_none());
}
