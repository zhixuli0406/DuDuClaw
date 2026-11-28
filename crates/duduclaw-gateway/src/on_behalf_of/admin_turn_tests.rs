//! Kept under context so negative fixtures can corrupt private registry state
//! without introducing a production registration or mutation API.
use super::*;
use super::super::admin_turn::{admin_turn, AdminTurn, AdminTurnLevel, AdminTurnRefusal};
use crate::approval::{ConversationAudience, DecisionContext, TrustedReplyTarget};
use crate::computer_use_sessions::attended;
use crate::knowledge::turn_requesters::{self, CURRENT_REQUESTER};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_knowledge::requester::RequesterIdentity;
use duduclaw_memory::feedback::{CURRENT_SESSION_ID, CURRENT_TURN_ID};
use rusqlite::Connection;
use AdminTurnLevel::{Read, Write};
use AdminTurnRefusal::*;

struct Fixture {
    home: tempfile::TempDir,
    uid: String,
    other: String,
    agent: String,
    turn: String,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[admin_ai]\nenabled=true\n").unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let uid = db.create_user("admin@test.invalid", "Admin", "test-password", UserRole::Admin).unwrap().id;
        let other = db.create_user("other@test.invalid", "Other", "test-password", UserRole::Employee).unwrap().id;
        let agent = format!("admin-avatar-{}", uuid::Uuid::new_v4().simple());
        db.bind_agent(&uid, &agent, AccessLevel::Operator).unwrap();
        db.set_avatar_as(&uid, &uid, Some(&agent)).unwrap();
        db.bind_channel_identity(&uid, "telegram", "101", true).unwrap();
        Self { home, uid, other, agent, turn: uuid::Uuid::new_v4().to_string() }
    }
    fn dashboard(&self) -> RequesterIdentity {
        RequesterIdentity::DashboardUser { user_id: self.uid.clone() }
    }
    fn channel(&self) -> RequesterIdentity {
        RequesterIdentity::ChannelUser { channel: "telegram".into(), chat: "101".into(),
            sender: "101".into(), user_id: Some(self.uid.clone()), attested_private: false }
    }
    fn sql(&self, sql: &str) {
        Connection::open(self.home.path().join("users.db")).unwrap().execute_batch(sql).unwrap();
    }
    fn proof(&self, guard: &ContextGuard, level: AdminTurnLevel) -> Result<AdminTurn, AdminTurnRefusal> {
        admin_turn(self.home.path(), &self.agent, guard.reference(), level)
    }
    async fn live<T>(&self, who: RequesterIdentity, webchat: bool, chat_type: Option<&str>,
        check: impl FnOnce(&ContextGuard) -> T) -> T {
        let session = if webchat { format!("webchat:{}", self.turn) } else { "telegram:101".into() };
        let target = (!webchat).then(|| TrustedReplyTarget::new(DecisionContext {
            channel: "telegram".into(), account_id: "bot".into(),
            conversation_id: "101".into(), principal_id: "101".into(),
        }, "test-token".into(), "101".into(), None).unwrap().with_chat_type(chat_type));
        let decision = target.as_ref().map(|target| target.context.clone());
        let body = async {
            assert!(attended::bind_final_route(&session, &session, &self.agent, &self.turn));
            CURRENT_SESSION_ID.scope(Some(session.clone()),
                CURRENT_TURN_ID.scope(Some(self.turn.clone()),
                    CURRENT_REQUESTER.scope(who,
                        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(self.uid.clone(),
                            crate::approval::CURRENT_DECISION_CONTEXT.scope(decision,
                                crate::approval::CURRENT_TRUSTED_REPLY_TARGET.scope(target, async {
                                    let _requester = turn_requesters::register_current_turn(&self.agent).unwrap();
                                    let _turn = crate::turn_context::register(&self.agent, &self.turn, &session).unwrap();
                                    let guard = register(self.home.path(), &self.agent).unwrap();
                                    check(&guard)
                                })))))).await
        };
        if webchat {
            attended::verified_webchat_message(&session, Some(&self.uid), body).await
        } else {
            attended::verified_channel_message(&session, "101", false, body).await
        }
    }
}

#[tokio::test]
async fn admin_turn_happy_path_read_and_write() {
    let f = Fixture::new();
    for webchat in [true, false] {
        let who = if webchat { f.dashboard() } else { f.channel() };
        f.live(who, webchat, Some("private"), |guard| {
            for level in [Read, Write] {
                let proof = f.proof(guard, level).unwrap();
                assert_eq!(proof.user_id, f.uid);
                assert_eq!(proof.agent, f.agent);
                assert_eq!(proof.turn_id, f.turn);
                assert_eq!(proof.session_id, if webchat { format!("webchat:{}", f.turn) } else { "telegram:101".into() });
                assert_eq!(proof.channel, if webchat { "webchat" } else { "telegram" });
                assert_eq!(proof.audience, ConversationAudience::Private);
                assert_eq!(proof.decision_context.unwrap().principal_id, if webchat { f.uid.clone() } else { "101".into() });
            }
        }).await;
    }
}

#[tokio::test]
async fn admin_turn_refuses_non_admin_requester() {
    let f = Fixture::new();
    f.live(RequesterIdentity::DashboardUser { user_id: f.other.clone() }, true, None, |guard| {
        assert_eq!(f.proof(guard, Read).unwrap_err(), IneligibleAccount);
    }).await;
    for who in [RequesterIdentity::Unknown,
        RequesterIdentity::System { kind: "cron".into(), on_behalf_of: Some(f.uid.clone()) },
        RequesterIdentity::ChannelUser { channel: "telegram".into(), chat: "101".into(),
            sender: "101".into(), user_id: None, attested_private: false },
        RequesterIdentity::ChannelUser { channel: "widget".into(), chat: "visitor".into(),
            sender: "visitor".into(), user_id: Some(f.uid.clone()), attested_private: true },
    ] {
        f.live(who, true, None, |guard| {
            assert_eq!(f.proof(guard, Read).unwrap_err(), UnverifiedRequester);
        }).await;
    }
}

#[tokio::test]
async fn admin_turn_requires_own_unique_avatar() {
    let f = Fixture::new();
    // Simulate legacy schemas that predate the unique avatar index.
    f.sql("DROP INDEX idx_users_avatar");
    let db = Connection::open(f.home.path().join("users.db")).unwrap();
    for case in ["shared", "other", "unbound", "viewer"] {
        db.execute("UPDATE users SET avatar_agent_id=NULL", []).unwrap();
        db.execute("UPDATE users SET avatar_agent_id=?1 WHERE id=?2", rusqlite::params![f.agent, f.uid]).unwrap();
        db.execute("UPDATE user_agent_bindings SET access_level='operator'", []).unwrap();
        match case {
            "shared" => { db.execute("UPDATE users SET avatar_agent_id=?1 WHERE id=?2", rusqlite::params![f.agent, f.other]).unwrap(); },
            "other" => {
                db.execute("UPDATE users SET avatar_agent_id=NULL WHERE id=?1", [&f.uid]).unwrap();
                db.execute("UPDATE users SET avatar_agent_id=?1 WHERE id=?2", rusqlite::params![f.agent, f.other]).unwrap();
            },
            "unbound" => { db.execute("UPDATE user_agent_bindings SET agent_name='different-agent'", []).unwrap(); },
            "viewer" => { db.execute("UPDATE user_agent_bindings SET agent_name=?1, access_level='viewer'", [&f.agent]).unwrap(); },
            _ => unreachable!(),
        }
        f.live(f.dashboard(), true, None, |guard| {
            assert_eq!(f.proof(guard, Read).unwrap_err(), InvalidAvatar, "{case}");
        }).await;
    }
}

#[tokio::test]
async fn admin_turn_refuses_dispatch_and_background() {
    let f = Fixture::new();
    let guard = crate::memory_provenance::DISPATCH_RUN.scope(Some(("dispatch".into(), "run".into())),
        async { register(f.home.path(), &f.agent).unwrap() }).await;
    assert_eq!(f.proof(&guard, Read).unwrap_err(), InvalidContext);
    let guard = crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(
        crate::runtime::GoalRoundAttribution { episode_id: "task".into(), round: Some(1) },
        async { register(f.home.path(), &f.agent).unwrap() }).await;
    assert_eq!(f.proof(&guard, Read).unwrap_err(), InvalidContext);
    let guard = register(f.home.path(), &f.agent).unwrap();
    assert_eq!(f.proof(&guard, Read).unwrap_err(), Unattended);
    f.live(f.dashboard(), true, None, |guard| {
        let other = register(f.home.path(), "different-agent").unwrap();
        // Model an incorrectly attached proof: valid_for must still reject it.
        let call = attended_call(f.home.path(), &f.agent, guard.reference()).unwrap();
        entries().lock().unwrap().get_mut(other.reference()).unwrap().attendance = Some(call);
        assert_eq!(admin_turn(f.home.path(), "different-agent", other.reference(), Read).unwrap_err(), Unattended);
    }).await;
}

#[tokio::test]
async fn stale_context_after_turn_end_refused() {
    let f = Fixture::new();
    f.live(f.dashboard(), true, None, |_| {
        let guard = register(f.home.path(), &f.agent).unwrap();
        let reference = guard.reference().to_owned();
        drop(guard);
        assert_eq!(admin_turn(f.home.path(), &f.agent, &reference, Read).unwrap_err(), InvalidContext);
    }).await;
    let guard = f.live(f.dashboard(), true, None, |_| register(f.home.path(), &f.agent).unwrap()).await;
    assert_eq!(f.proof(&guard, Read).unwrap_err(), Unattended);
}

#[tokio::test]
async fn admin_turn_feature_off_and_malformed_config_refused() {
    let f = Fixture::new();
    for config in ["", "[admin_ai]\nenabled=false", "[admin_ai]\nenabled='true'", "invalid["] {
        std::fs::write(f.home.path().join("config.toml"), config).unwrap();
        f.live(f.dashboard(), true, None, |guard| {
            assert_eq!(f.proof(guard, Read).unwrap_err(), FeatureDisabled);
        }).await;
    }
}

#[tokio::test]
async fn admin_turn_must_change_password_and_disabled_admin_refused() {
    let f = Fixture::new();
    for sql in ["UPDATE users SET must_change_password=1",
        "UPDATE users SET must_change_password=0, status='disabled'",
        "UPDATE users SET status='active', role='Admin'"] {
        f.sql(sql);
        f.live(f.dashboard(), true, None, |guard| {
            assert_eq!(f.proof(guard, Read).unwrap_err(), IneligibleAccount);
        }).await;
    }
}

#[tokio::test]
async fn admin_turn_missing_or_unreadable_users_db_refused() {
    let f = Fixture::new();
    let path = f.home.path().join("users.db");
    std::fs::rename(&path, f.home.path().join("saved-users.db")).unwrap();
    f.live(f.dashboard(), true, None, |guard| {
        assert_eq!(f.proof(guard, Read).unwrap_err(), IneligibleAccount);
        assert!(!path.exists(), "authorization must never create users.db");
    }).await;
    // A directory is reliably unreadable as SQLite, including privileged CI.
    std::fs::create_dir(&path).unwrap();
    f.live(f.dashboard(), true, None, |guard| {
        assert_eq!(f.proof(guard, Read).unwrap_err(), IneligibleAccount);
    }).await;
}

#[tokio::test]
async fn write_requires_private_audience() {
    let f = Fixture::new();
    for kind in [Some("group"), None] {
        f.live(f.channel(), false, kind, |guard| {
            assert_eq!(f.proof(guard, Read).unwrap().audience, ConversationAudience::MultiParty);
            assert_eq!(f.proof(guard, Write).unwrap_err(), PrivateAudienceRequired);
        }).await;
    }
    f.live(f.dashboard(), true, None, |guard| {
        assert!(f.proof(guard, Write).is_ok());
    }).await;
    // A dashboard requester snapshot alone cannot turn channel attendance
    // into a verified WebChat input.
    f.live(f.dashboard(), false, Some("private"), |guard| {
        assert_eq!(f.proof(guard, Read).unwrap().audience, ConversationAudience::MultiParty);
        assert_eq!(f.proof(guard, Write).unwrap_err(), PrivateAudienceRequired);
    }).await;
    f.live(f.channel(), false, Some("private"), |guard| {
        let context = DecisionContext { channel: "telegram".into(), account_id: "bot".into(),
            conversation_id: "101".into(), principal_id: "different-sender".into() };
        // Overlapping host registration cannot borrow another sender's DM.
        let _turn = crate::approval::CURRENT_DECISION_CONTEXT.sync_scope(Some(context), || {
            crate::turn_context::register(&f.agent, &f.turn, "telegram:101").unwrap()
        });
        assert_eq!(f.proof(guard, Write).unwrap_err(), PrivateAudienceRequired);
    }).await;
}

#[tokio::test]
async fn admin_turn_channel_binding_is_re_resolved() {
    let f = Fixture::new();
    f.live(f.channel(), false, Some("private"), |guard| {
        assert!(f.proof(guard, Read).is_ok());
        f.sql("UPDATE channel_identities SET verified=0");
        assert_eq!(f.proof(guard, Read).unwrap_err(), UnverifiedRequester);
    }).await;
    let db = Connection::open(f.home.path().join("users.db")).unwrap();
    db.execute("UPDATE channel_identities SET verified=1, user_id=?1", [&f.other]).unwrap();
    f.live(f.channel(), false, Some("private"), |guard| {
        assert_eq!(f.proof(guard, Read).unwrap_err(), UnverifiedRequester);
    }).await;
}

#[tokio::test]
async fn admin_turn_matches_host_turn_session_and_context_identity() {
    let f = Fixture::new();
    f.live(f.dashboard(), true, None, |guard| {
        let other_home = tempfile::tempdir().unwrap();
        assert_eq!(admin_turn(f.home.path(), "other-agent", guard.reference(), Read).unwrap_err(), InvalidContext);
        std::fs::write(other_home.path().join("config.toml"), "[admin_ai]\nenabled=true").unwrap();
        assert_eq!(admin_turn(other_home.path(), &f.agent, guard.reference(), Read).unwrap_err(), InvalidContext);
        entries().lock().unwrap().get_mut(guard.reference()).unwrap().turn = Some("wrong-turn".into());
        assert_eq!(f.proof(guard, Read).unwrap_err(), TurnMismatch);
        {
            let mut entries = entries().lock().unwrap();
            let entry = entries.get_mut(guard.reference()).unwrap();
            entry.turn = Some(f.turn.clone());
            entry.session = Some("webchat:wrong-session".into());
        }
        assert_eq!(f.proof(guard, Read).unwrap_err(), TurnMismatch);
    }).await;
}

#[tokio::test]
async fn admin_turn_attendance_without_session_id_refused() {
    let f = Fixture::new();
    f.live(f.dashboard(), true, None, |_| {
        let guard = CURRENT_SESSION_ID.sync_scope(None, || {
            register(f.home.path(), &f.agent).unwrap()
        });
        assert!(attended_call(f.home.path(), &f.agent, guard.reference()).is_none());
        for session in [None, Some(format!("webchat:{}", f.turn))] {
            entries().lock().unwrap().get_mut(guard.reference()).unwrap().session = session;
            for level in [Read, Write] {
                assert_eq!(f.proof(&guard, level).unwrap_err(), Unattended);
            }
        }
    }).await;
}

#[test]
fn admin_turn_refusal_codes_are_stable_and_distinct() {
    let expected = [
        (FeatureDisabled, "admin_ai_disabled"), (ProtectedExecution, "admin_ai_protected_execution"),
        (InvalidContext, "admin_ai_invalid_context"), (Unattended, "admin_ai_unattended"),
        (UnverifiedRequester, "admin_ai_unverified_requester"), (IneligibleAccount, "admin_ai_ineligible_account"),
        (InvalidAvatar, "admin_ai_invalid_avatar"), (TurnMismatch, "admin_ai_turn_mismatch"),
        (PrivateAudienceRequired, "admin_ai_private_audience_required"),
    ];
    for (refusal, code) in expected { assert_eq!(refusal.code(), code); }
    assert_eq!(expected.map(|(refusal, _)| refusal.code()).into_iter().collect::<std::collections::HashSet<_>>().len(), 9);
}
