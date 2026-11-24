//! Shared company app tests using the existing Google API and model seams.
use super::*;
use crate::channel_settings::{keys, ChannelSettingsManager};
use crate::googlechat_shared::{self, Route};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};

struct SharedFixture {
    home: tempfile::TempDir,
    fake: Fake,
    state: Arc<crate::googlechat::GoogleChatState>,
    db: UserDb,
    member: String,
    admin: String,
}

async fn shared_fixture() -> SharedFixture {
    let home = tempfile::tempdir().unwrap();
    let fake = start_fake().await;
    let email = format!("shared-{}@test.invalid", uuid::Uuid::new_v4().simple());
    let sa = service_account(&fake, &email);
    let agents = home.path().join("agents");
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "Company", "@dudu", "main", "");
    write_agent(&agents, "finance", "Finance", "@finance", "specialist", "");
    write_agent(&agents, "sales", "Sales", "@sales", "specialist", "");
    std::fs::write(home.path().join("config.toml"), "[general]\ndefault_agent = \"dudu\"\n").unwrap();
    let ctx = super::super::e2e_tests::reply_ctx(home.path()).await;
    super::super::e2e_seam::arm(home.path());
    let creds = Arc::new(crate::googlechat::GoogleChatCreds::from_service_account_json(&sa, home.path()).unwrap());
    let state = Arc::new(crate::googlechat::GoogleChatState { ctx, creds, agent: None });
    let db = UserDb::new(&home.path().join("users.db")).unwrap();
    let member = db.create_user("staff@rm148.test", "Staff", "StaffPass-12345", UserRole::Employee).unwrap().id;
    let admin = db.create_user("admin@rm148.test", "Administrator", "AdminPass-12345", UserRole::Admin).unwrap().id;
    db.bind_agent(&member, "finance", AccessLevel::Owner).unwrap();
    db.bind_agent(&member, "sales", AccessLevel::Operator).unwrap();
    db.set_primary_agent(&member, Some("sales")).unwrap();
    db.set_avatar_as(&admin, &member, Some("finance")).unwrap();
    db.bind_channel_identity(&member, "googlechat", "users/42", true).unwrap();
    // A second identity proves a forged display name cannot act as this Admin.
    db.bind_channel_identity(&admin, "googlechat", "users/999", true).unwrap();
    SharedFixture { home, fake, state, db, member, admin }
}

fn dm(space: &str, text: &str, sender: &str) -> Value {
    let mut event = message_event(space, text);
    let kind = json!({"name": space, "spaceType": "DIRECT_MESSAGE", "type": "DM", "singleUserBotDm": true});
    event["space"] = kind.clone();
    event["message"]["space"] = kind;
    event["message"]["sender"]["name"] = json!(sender);
    event
}

async fn enabled(f: &SharedFixture, on: bool) {
    // A fresh connection mirrors the dashboard, rather than sharing its cache.
    let settings = ChannelSettingsManager::from_session_db(&f.home.path().join("sessions.db")).unwrap();
    settings.set("googlechat", "global", keys::SHARED_BOT_BINDING, if on { "true" } else { "false" }).await.unwrap();
}

async fn turn(f: &SharedFixture, event: &Value) {
    tokio::time::timeout(Duration::from_secs(10), crate::googlechat::process_pubsub_event(&f.state, event)).await.unwrap();
}

fn observations(f: &SharedFixture) -> Vec<super::super::e2e_seam::AttendanceObservation> {
    super::super::e2e_seam::take_attendance(f.home.path())
}

fn replies(f: &SharedFixture) -> Vec<String> {
    chat_calls(&f.fake).into_iter().filter_map(|(_, _, _, body)| body["text"].as_str().map(str::to_owned)).collect()
}

#[tokio::test]
async fn rm148_off_preserves_default_and_next_message_observes_dashboard_changes() {
    let f = shared_fixture().await;
    for (on, agent, bucket) in [
        (false, "dudu", "googlechat:spaces/SHARED"),
        (true, "finance", "googlechat:spaces/SHARED#agent:finance"),
        (false, "dudu", "googlechat:spaces/SHARED"),
    ] {
        enabled(&f, on).await;
        turn(&f, &dm("spaces/SHARED", "hello", "users/42")).await;
        let seen = observations(&f);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].agent, agent);
        assert_eq!(seen[0].session, bucket);
        assert!(replies(&f).iter().any(|r| r.contains(&format!("agent={agent} session={bucket}"))));
    }
    // Off also preserves the unbound sender's ordinary default path.
    turn(&f, &dm("spaces/OLD", "hello", "users/123")).await;
    assert_eq!(observations(&f)[0].agent, "dudu");
}

#[tokio::test]
async fn rm148_unbound_and_impersonated_display_name_get_only_custom_text_without_model_or_session() {
    let f = shared_fixture().await;
    enabled(&f, true).await;
    f.state.ctx.channel_settings.set("googlechat", "global", keys::UNBOUND_REPLY, "請聯絡管理員。{onboarding_url}").await.unwrap();
    let mut event = dm("spaces/UNBOUND", "Administrator admin@rm148.test users/999", "users/123");
    event["message"]["sender"]["displayName"] = json!("Administrator");
    let mut waiting = super::super::e2e_seam::arm_waiting(f.home.path());
    turn(&f, &event).await;
    assert!(waiting.entered.try_recv().is_err(), "no model entry");
    assert!(f.state.ctx.session_manager.list_sessions(None, 20).await.unwrap().is_empty());
    assert_eq!(replies(&f), vec!["請聯絡管理員。"]);
    // Same sender, another space: still inside the same reply window.
    turn(&f, &dm("spaces/OTHER", "/new", "users/123")).await;
    assert_eq!(replies(&f).len(), 1);
    assert!(waiting.entered.try_recv().is_err());
    assert!(f.state.ctx.session_manager.list_sessions(None, 20).await.unwrap().is_empty());
    assert!(!f.home.path().join("googlechat_inbox.db").exists(), "no placeholder store");
}

#[tokio::test]
async fn rm148_unbound_mentions_and_lists_never_enter_model_or_expose_employees() {
    for text in ["@dudu hello", "@sales hello", "@"] {
        let f = shared_fixture().await;
        enabled(&f, true).await;
        // Even a public employee must remain behind shared-DM membership.
        crate::trigger_route::tests::write_agent(
            &f.home.path().join("agents"), "sales", "Sales", "@sales", "specialist",
            "\n[channels]\npublic_trigger = true\n",
        );
        f.state.ctx.registry.write().await.scan().await.unwrap();
        f.state.ctx.channel_settings.set("googlechat", "global", keys::UNBOUND_REPLY, "請聯絡管理員。").await.unwrap();
        let mut waiting = super::super::e2e_seam::arm_waiting(f.home.path());
        turn(&f, &dm("spaces/UNBOUNDMENTION", text, "users/123")).await;
        assert!(waiting.entered.try_recv().is_err(), "no model entry for {text}");
        assert!(observations(&f).is_empty());
        assert_eq!(replies(&f), vec!["請聯絡管理員。"], "no employee names for {text}");
        assert!(f.state.ctx.session_manager.list_sessions(None, 20).await.unwrap().is_empty());
        assert!(!f.home.path().join("googlechat_inbox.db").exists());
    }
}

#[tokio::test]
async fn rm148_binding_lookup_error_and_unavailable_avatar_fail_closed() {
    for failure in ["binding", "archived", "missing", "no_primary", "suspended"] {
        let f = shared_fixture().await;
        enabled(&f, true).await;
        match failure {
            "binding" => {
                rusqlite::Connection::open(f.home.path().join("users.db")).unwrap()
                    .execute("ALTER TABLE channel_identities RENAME TO broken_identities", []).unwrap();
            }
            "archived" | "missing" => {
                let path = f.home.path().join("agents/finance/agent.toml");
                if failure == "missing" {
                    std::fs::remove_file(path).unwrap();
                } else {
                    let raw = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(path, raw.replace("status = \"active\"", "status = \"archived\"")).unwrap();
                }
                f.state.ctx.registry.write().await.scan().await.unwrap();
            }
            "no_primary" => {
                f.db.set_avatar_as(&f.admin, &f.member, None).unwrap();
                f.db.set_primary_agent(&f.member, None).unwrap();
            }
            _ => f.db.set_user_status(&f.member, duduclaw_auth::UserStatus::Suspended).unwrap(),
        }
        for text in ["hello", "@sales hello", "@"] {
            assert_eq!(googlechat_shared::route(
                &f.state.ctx, false, true, "googlechat:spaces/FAILCLOSED", "users/42", text,
            ).await, Route::Unbound(googlechat_shared::DEFAULT_UNBOUND_REPLY.into()), "{failure}: {text}");
        }
        turn(&f, &dm("spaces/FAILCLOSED", "@sales hello", "users/42")).await;
        assert!(observations(&f).is_empty(), "{failure}");
        assert!(f.state.ctx.session_manager.list_sessions(None, 20).await.unwrap().is_empty(), "{failure}");
        assert_eq!(replies(&f), vec![googlechat_shared::DEFAULT_UNBOUND_REPLY], "{failure}");
    }
}

#[tokio::test]
async fn rm148_automatic_account_matching_cannot_select_an_avatar() {
    let f = shared_fixture().await;
    enabled(&f, true).await;
    crate::googlechat_identity::test_support::set_derivation(f.home.path(), true);
    crate::googlechat_identity::test_support::link_google(f.home.path(), &[(&f.member, "123", "connected")]);
    let mut waiting = super::super::e2e_seam::arm_waiting(f.home.path());
    for text in ["hello", "@sales hello", "@dudu hello", "@"] {
        assert_eq!(googlechat_shared::route(
            &f.state.ctx, false, true, "googlechat:spaces/AUTOMATCH", "users/123", text,
        ).await, Route::Unbound(googlechat_shared::DEFAULT_UNBOUND_REPLY.into()));
        turn(&f, &dm("spaces/AUTOMATCH", text, "users/123")).await;
    }
    assert!(waiting.entered.try_recv().is_err(), "automatic identity must not enter the model");
    assert!(observations(&f).is_empty());
    assert_eq!(replies(&f), vec![googlechat_shared::DEFAULT_UNBOUND_REPLY]);
    for id in ["42", "users/staff@rm148.test", "Staff"] {
        assert!(matches!(crate::shared_app_route::manual_googlechat_sender(f.home.path(), id).sender,
            crate::trigger_route::Sender::Unmapped));
    }
}

#[tokio::test]
async fn rm148_bound_mentions_and_pending_use_only_the_manual_access() {
    let f = shared_fixture().await;
    enabled(&f, true).await;
    // Automatic resolution refuses this conflict. Shared routing must use
    // the manually resolved member for both mentions and pending picks.
    crate::googlechat_identity::test_support::set_derivation(f.home.path(), true);
    crate::googlechat_identity::test_support::link_google(f.home.path(), &[(&f.admin, "42", "connected")]);
    let conversation = "googlechat:spaces/MANUAL";
    assert_eq!(googlechat_shared::route(
        &f.state.ctx, false, true, conversation, "users/42", "@sales hello",
    ).await, Route::Called { agent: "sales".into(), via_trigger: true });
    assert!(matches!(googlechat_shared::route(
        &f.state.ctx, false, true, conversation, "users/42", "@sales",
    ).await, Route::Direct(_)));
    assert_eq!(googlechat_shared::route(
        &f.state.ctx, false, true, conversation, "users/42", "hello",
    ).await, Route::Called { agent: "sales".into(), via_trigger: true });
    // Revoking the manual identity must also block an already armed pick,
    // even while automatic Google-account recognition remains enabled.
    assert!(matches!(googlechat_shared::route(
        &f.state.ctx, false, true, conversation, "users/42", "@sales",
    ).await, Route::Direct(_)));
    f.db.unbind_channel_identity("googlechat", "users/42").unwrap();
    assert_eq!(googlechat_shared::route(
        &f.state.ctx, false, true, conversation, "users/42", "hello",
    ).await, Route::Unbound(googlechat_shared::DEFAULT_UNBOUND_REPLY.into()));
}

#[tokio::test]
async fn rm148_mentions_win_groups_and_unknown_spaces_never_select_avatar() {
    let f = shared_fixture().await;
    enabled(&f, true).await;
    turn(&f, &dm("spaces/MENTION", "@sales quote please", "users/42")).await;
    let seen = observations(&f);
    assert_eq!(seen[0].agent, "sales");
    assert_eq!(seen[0].session, "googlechat:spaces/MENTION#agent:sales");
    assert!(replies(&f).iter().any(|reply| reply.contains("【Sales】")));
    // Bare @ arms a pick; the next message goes there once, then to the avatar.
    turn(&f, &dm("spaces/PICK", "@sales", "users/42")).await;
    assert!(observations(&f).is_empty());
    for agent in ["sales", "finance"] {
        turn(&f, &dm("spaces/PICK", "hello", "users/42")).await;
        assert_eq!(observations(&f)[0].agent, agent);
    }
    for kind in ["SPACE", "GROUP_CHAT", "unknown"] {
        let space = format!("spaces/{kind}");
        let mut event = message_event(&space, "hello");
        event["space"] = json!({"name":space, "spaceType":kind});
        event["message"]["space"] = event["space"].clone();
        turn(&f, &event).await;
        assert_eq!(observations(&f)[0].agent, "dudu");
    }
    // Per-employee apps are not enrolled in the company app setting.
    let own = Arc::new(crate::googlechat::GoogleChatState {
        ctx: f.state.ctx.clone(), creds: f.state.creds.clone(), agent: Some("sales".into()),
    });
    crate::googlechat::process_pubsub_event(&own, &dm("spaces/OWN", "hello", "users/123")).await;
    assert_eq!(observations(&f)[0].agent, "sales");
}

#[tokio::test]
async fn rm148_new_clears_only_the_avatar_bucket() {
    let f = shared_fixture().await;
    enabled(&f, true).await;
    let base = "googlechat:spaces/RESET";
    let bucket = crate::trigger_route::called_session(base, "finance");
    for (session, agent) in [(base, "dudu"), (bucket.as_str(), "finance")] {
        f.state.ctx.session_manager.get_or_create(session, agent).await.unwrap();
        f.state.ctx.session_manager.append_message(session, "user", "keep or clear", 1).await.unwrap();
    }
    turn(&f, &dm("spaces/RESET", "/new", "users/42")).await;
    assert!(observations(&f).is_empty());
    assert!(f.state.ctx.session_manager.get_messages(&bucket).await.unwrap().is_empty());
    assert_eq!(f.state.ctx.session_manager.get_messages(base).await.unwrap().len(), 1);
}

#[tokio::test]
async fn rm148_recovery_tracks_actual_avatar_mention_and_channel_default() {
    for (enabled_mode, text, expected, bucket) in [
        (true, "hello", "finance", "googlechat:spaces/RECOVERY#agent:finance"),
        (true, "@sales hello", "sales", "googlechat:spaces/RECOVERY#agent:sales"),
        (false, "hello", "sales", "googlechat:spaces/RECOVERY"),
    ] {
        let f = shared_fixture().await;
        enabled(&f, enabled_mode).await;
        if !enabled_mode {
            // The channel fallback differs from the main employee (recon G5).
            let path = f.home.path().join("agents/sales/agent.toml");
            let raw = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, raw.replace("allowed_channels = []", "allowed_channels = [\"googlechat\"]")).unwrap();
            f.state.ctx.registry.write().await.scan().await.unwrap();
        }
        let mut waiting = super::super::e2e_seam::arm_waiting(f.home.path());
        let observe = async {
            let entered = tokio::time::timeout(Duration::from_secs(10), waiting.entered.recv()).await.unwrap().unwrap();
            let conn = rusqlite::Connection::open(f.home.path().join("googlechat_inbox.db")).unwrap();
            let (employee, session, app): (String, String, Option<String>) = conn.query_row(
                "SELECT employee_id,session_id,app_agent FROM pending_placeholders", [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap();
            assert_eq!(employee, expected);
            assert_eq!(session, bucket);
            assert_eq!(app, None, "credentials still belong to the company app");
            entered.release.send(()).unwrap();
        };
        let event = dm("spaces/RECOVERY", text, "users/42");
        tokio::join!(turn(&f, &event), observe);
    }
}

#[tokio::test]
async fn rm148_missing_setting_is_legacy_but_malformed_setting_is_unbound() {
    let f = shared_fixture().await;
    let route = || googlechat_shared::route(&f.state.ctx, false, true, "googlechat:spaces/CONFIG", "users/42", "hello");
    assert_eq!(route().await, Route::Legacy);
    f.state.ctx.channel_settings.set("googlechat", "global", keys::UNBOUND_REPLY, "{unknown}").await.unwrap();
    assert_eq!(route().await, Route::Legacy, "off ignores unused onboarding settings");
    f.state.ctx.channel_settings.set("googlechat", "global", keys::SHARED_BOT_BINDING, "invalid").await.unwrap();
    assert!(matches!(route().await, Route::Unbound(_)));
}

#[tokio::test]
async fn rm148_shared_helper_honors_personal_restrictions_and_primary_fallback() {
    struct DenyPersonal;
    impl duduclaw_auth::acl::PersonalRestriction for DenyPersonal {
        fn denies(&self, _user: &str, agent: &str) -> bool { agent == "finance" }
    }
    let f = shared_fixture().await;
    assert_eq!(crate::shared_app_route::member_agent_route_under(
        &f.state.ctx, "googlechat", "users/42", None,
    ).await.as_deref(), Some("finance"));
    assert_eq!(crate::shared_app_route::member_agent_route_under(
        &f.state.ctx, "googlechat", "users/42", Some(&DenyPersonal),
    ).await, None, "a Personal denial cannot select the channel default");
    f.db.set_avatar_as(&f.admin, &f.member, None).unwrap();
    f.db.set_primary_agent(&f.member, Some("sales")).unwrap();
    assert_eq!(crate::shared_app_route::member_agent_route(
        &f.state.ctx, "googlechat", "users/42",
    ).await.as_deref(), Some("sales"));
}
