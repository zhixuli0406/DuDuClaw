use super::*;

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(value),
            ..
        } => value,
        other => panic!("expected success: {other:?}"),
    }
}
fn refused(frame: WsFrame) {
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "expected refusal: {frame:?}"
    );
}
async fn rpc(h: &MethodHandler, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
    Box::pin(h.handle(method, params, ctx)).await
}

struct PrimaryProfileFixture {
    _dir: tempfile::TempDir,
    home: std::path::PathBuf,
    db: Arc<UserDb>,
    handler: MethodHandler,
    alice: String,
    bob: String,
    admin: String,
}

impl PrimaryProfileFixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let alice = db
            .create_user(
                "alice@test.invalid",
                "Alice",
                "isolated-fixture-password",
                UserRole::Employee,
            )
            .unwrap();
        let bob = db
            .create_user(
                "bob@test.invalid",
                "Bob",
                "isolated-fixture-password",
                UserRole::Employee,
            )
            .unwrap();
        let admin = db
            .create_user(
                "admin@test.invalid",
                "Admin",
                "isolated-fixture-password",
                UserRole::Admin,
            )
            .unwrap();
        db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
        db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
        let handler = MethodHandler::new(home.clone()).await;
        handler
            .set_user_db(
                db.clone(),
                Arc::new(JwtConfig::new(b"isolated-fixture-signing-key-only")),
            )
            .await;
        handler
            .set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
            .await;
        let admin_ctx = db.live_user_context(&admin.id).unwrap().unwrap();
        payload(
            handler
                .handle_agents_create_as(
                    json!({"name":"bot","display_name":"Bot"}),
                    Some(&admin_ctx),
                )
                .await,
        );
        handler
            .update_agent_toml("bot", |table| {
                let caps = table
                    .entry("capabilities")
                    .or_insert_with(|| toml::Value::Table(Default::default()))
                    .as_table_mut()
                    .unwrap();
                caps.insert("computer_use".into(), toml::Value::Boolean(true));
                let config = caps
                    .entry("computer_use_config")
                    .or_insert_with(|| toml::Value::Table(Default::default()))
                    .as_table_mut()
                    .unwrap();
                config.insert("persist_login".into(), toml::Value::Boolean(true));
                Ok(())
            })
            .await
            .unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[computer_use.profiles]\nenabled=true\n",
        )
        .unwrap();
        std::fs::write(home.join(".keyfile"), [42; 32]).unwrap();
        duduclaw_core::platform::set_owner_only(&home.join(".keyfile")).unwrap();
        db.set_google_primary_owner("bot", &alice.id, &admin.id)
            .unwrap();
        Self {
            _dir: dir,
            home,
            db,
            handler,
            alice: alice.id,
            bob: bob.id,
            admin: admin.id,
        }
    }

    fn saved_profile(&self) -> crate::computer_profiles::ProfileSession {
        let profile = crate::computer_profiles::ProfileSession::prepare(
            &self.home,
            "bot",
            &[],
            crate::computer_profiles::ProfilesConfig::active(&self.home).unwrap(),
        )
        .unwrap();
        profile
            .save_guarded_before(
                b"synthetic-alice-cookie",
                std::time::Instant::now() + std::time::Duration::from_secs(10),
                &|| true,
            )
            .unwrap();
        profile
    }

    async fn select(&self, user: &str) -> WsFrame {
        let admin = self.db.live_user_context(&self.admin).unwrap().unwrap();
        rpc(
            &self.handler,
            "agents.primary_owner_set",
            json!({"agent_id":"bot","user_id":user}),
            &admin,
        )
        .await
    }
}

#[cfg(unix)]
#[tokio::test]
async fn primary_owner_browser_return_to_original_owner_never_restores_previous_login() {
    let f = PrimaryProfileFixture::new().await;
    drop(f.saved_profile());
    assert!(
        crate::computer_profiles::state(&f.home, "bot")
            .unwrap()
            .is_some()
    );
    payload(f.select(&f.bob).await);
    payload(f.select(&f.alice).await);
    // No profile read between the two writes: the final identity matches A,
    // so a final-state fingerprint comparison alone cannot satisfy this test.
    assert!(
        crate::computer_profiles::state(&f.home, "bot")
            .unwrap()
            .is_none()
    );
    let profile = crate::computer_profiles::ProfileSession::prepare(
        &f.home,
        "bot",
        &[],
        crate::computer_profiles::ProfilesConfig::active(&f.home).unwrap(),
    )
    .unwrap();
    assert!(profile.restore().unwrap().0.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn primary_owner_browser_reset_failure_aborts_the_ownership_write() {
    let f = PrimaryProfileFixture::new().await;
    let _busy_profile = f.saved_profile();
    refused(f.select(&f.bob).await);
    assert_eq!(f.db.google_owner("bot").unwrap().id, f.alice);
}

#[cfg(unix)]
#[tokio::test]
async fn primary_owner_browser_invalid_candidate_does_not_discard_valid_login() {
    let f = PrimaryProfileFixture::new().await;
    drop(f.saved_profile());
    let before = crate::computer_profiles::state(&f.home, "bot")
        .unwrap()
        .unwrap();
    refused(f.select("missing-owner").await);
    assert_eq!(f.db.google_owner("bot").unwrap().id, f.alice);
    assert_eq!(
        crate::computer_profiles::state(&f.home, "bot")
            .unwrap()
            .unwrap()
            .profile_id,
        before.profile_id
    );
    let profile = crate::computer_profiles::ProfileSession::prepare(
        &f.home,
        "bot",
        &[],
        crate::computer_profiles::ProfilesConfig::active(&f.home).unwrap(),
    )
    .unwrap();
    let archive = profile.restore().unwrap().0.unwrap();
    assert_eq!(archive.as_slice(), b"synthetic-alice-cookie");
}

#[cfg(unix)]
async fn assert_primary_owner_rejects_actor_change_after_browser_reset(change: &str) {
    let f = PrimaryProfileFixture::new().await;
    // Keep another administrator so revoking this actor cannot be refused by
    // a last-administrator invariant.
    f.db.create_user("backup@test.invalid", "Backup", "isolated-fixture-password", UserRole::Admin).unwrap();
    drop(f.saved_profile());
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let request = super::super::google_personal_rpc::PRIMARY_OWNER_AFTER_BROWSER_RESET.scope(
        (reached.clone(), release.clone()), f.select(&f.bob)
    );
    tokio::pin!(request);
    tokio::select! {
        _ = reached.notified() => {},
        response = &mut request => panic!("request finished before controlled reset boundary: {response:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("reset boundary was not reached"),
    }
    assert!(crate::computer_profiles::state(&f.home, "bot").unwrap().is_none());
    match change {
        "demoted" => f.db.apply_user_update(&f.admin, &duduclaw_auth::UserUpdate {
            role: Some(UserRole::Employee), ..Default::default()
        }).unwrap(),
        "suspended" => f.db.set_user_status(&f.admin, duduclaw_auth::UserStatus::Suspended).unwrap(),
        "password_reset" => {
            // A host-owned account reset can set this flag while the RPC is
            // suspended. Use the real SQLite row rather than a stale ctx.
            let conn = rusqlite::Connection::open(f.home.join("users.db")).unwrap();
            conn.execute("UPDATE users SET must_change_password = 1 WHERE id = ?1", [&f.admin]).unwrap();
        }
        _ => panic!("unknown actor change"),
    }
    release.notify_one();
    refused(request.await);
    assert_eq!(f.db.google_owner("bot").unwrap().id, f.alice);
}

#[cfg(unix)]
#[tokio::test]
async fn primary_owner_browser_rejects_admin_demoted_during_reset() {
    assert_primary_owner_rejects_actor_change_after_browser_reset("demoted").await;
}

#[cfg(unix)]
#[tokio::test]
async fn primary_owner_browser_rejects_admin_suspended_during_reset() {
    assert_primary_owner_rejects_actor_change_after_browser_reset("suspended").await;
}

#[cfg(unix)]
#[tokio::test]
async fn primary_owner_browser_rejects_password_reset_during_reset() {
    assert_primary_owner_rejects_actor_change_after_browser_reset("password_reset").await;
}

#[tokio::test]
async fn personal_rpc_self_consent_owner_change_and_legacy_session_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
    let alice = db
        .create_user(
            "alice@c.test",
            "Alice",
            "fixture-password",
            UserRole::Employee,
        )
        .unwrap();
    let bob = db
        .create_user("bob@c.test", "Bob", "fixture-password", UserRole::Employee)
        .unwrap();
    let admin = db
        .create_user("admin@c.test", "Admin", "fixture-password", UserRole::Admin)
        .unwrap();
    db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
    db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    let h = MethodHandler::new(home.clone()).await;
    h.set_user_db(
        db.clone(),
        Arc::new(JwtConfig::new(b"fixture-signing-key-32-bytes-only")),
    )
    .await;
    h.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise))
        .await;
    let alice_ctx = db.live_user_context(&alice.id).unwrap().unwrap();
    let admin_ctx = db.live_user_context(&admin.id).unwrap().unwrap();
    // Register a complete employee through the real creation path. A minimal
    // hand-written TOML is not a valid AgentConfig and is skipped by the scan.
    payload(
        h.handle_agents_create_as(json!({"name":"bot","display_name":"Bot"}), Some(&admin_ctx))
            .await,
    );
    // Admin may select Alice, but the selection creates no permission.
    payload(
        rpc(
            &h,
            "agents.google_identity_set",
            json!({"agent_id":"bot","identity":format!("user:{}",alice.id)}),
            &admin_ctx,
        )
        .await,
    );
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), None);
    refused(rpc(&h,"me.google.consents.set",json!({"agent_id":"bot","user_id":alice.id,"allowed":true,"confirm_shared_access":true}),&admin_ctx).await);
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), None);
    // The person param cannot redirect a self-service grant to Bob.
    payload(
        rpc(
            &h,
            "me.google.consents.set",
            json!({"agent_id":"bot","user_id":bob.id,"allowed":true,"confirm_shared_access":true}),
            &alice_ctx,
        )
        .await,
    );
    assert_eq!(db.google_consent(&alice.id, "bot").unwrap(), Some(true));
    assert_eq!(db.google_consent(&bob.id, "bot").unwrap(), None);
    refused(rpc(&h,"me.google.connect_start",json!({"redirect_origin":"http://127.0.0.1:18789","session_ref":uuid::Uuid::new_v4(),"jti":uuid::Uuid::new_v4(),"user_id":bob.id}),&alice_ctx).await);
    refused(rpc(&h,"agents.google_identity_set",json!({"agent_id":"bot","identity":format!("user:{}",bob.id),"confirm_shared_access":true}),&alice_ctx).await);
    refused(
        rpc(
            &h,
            "agents.google_identity_set",
            json!({"agent_id":"bot","identity":format!("user:{}",alice.id)}),
            &alice_ctx,
        )
        .await,
    );
    payload(rpc(&h,"agents.google_identity_set",json!({"agent_id":"bot","identity":format!("user:{}",alice.id),"confirm_shared_access":true}),&alice_ctx).await);
    // A stale Owner context cannot keep changing the employee after unbind.
    db.unbind_agent(&alice.id, "bot").unwrap();
    refused(
        rpc(
            &h,
            "agents.google_identity_set",
            json!({"agent_id":"bot","identity":"none"}),
            &alice_ctx,
        )
        .await,
    );
    h.set_edition_override(Some(duduclaw_core::EditionProfile::Personal))
        .await;
    refused(rpc(&h, "me.google.status", json!({}), &admin_ctx).await);
    refused(
        rpc(
            &h,
            "agents.primary_owner_set",
            json!({"agent_id":"bot","user_id":bob.id}),
            &admin_ctx,
        )
        .await,
    );
}

#[test]
fn personal_rpc_family_is_enterprise_only_including_future_members() {
    for method in [
        "me.google.status",
        "me.google.connect_start",
        "me.google.future",
        "users.google_disconnect",
        "agents.primary_owner_set",
    ] {
        assert!(super::super::gates::is_enterprise_only_method(method));
    }
    assert!(!super::super::gates::is_enterprise_only_method(
        "me.google_export"
    ));
}


#[cfg(unix)]
#[tokio::test]
async fn primary_owner_browser_overlapping_resets_cannot_restore_a_profile_created_before_aba_writes() {
    let f = PrimaryProfileFixture::new().await;
    drop(f.saved_profile());
    let reached_b = Arc::new(tokio::sync::Notify::new());
    let release_b = Arc::new(tokio::sync::Notify::new());
    let change_b = super::super::google_personal_rpc::PRIMARY_OWNER_AFTER_BROWSER_RESET.scope(
        (reached_b.clone(), release_b.clone()), f.select(&f.bob)
    );
    tokio::pin!(change_b);
    tokio::select! {
        _ = reached_b.notified() => {},
        response = &mut change_b => panic!("B finished before reset boundary: {response:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("B reset not reached"),
    }
    let reached_a = Arc::new(tokio::sync::Notify::new());
    let release_a = Arc::new(tokio::sync::Notify::new());
    let change_a = super::super::google_personal_rpc::PRIMARY_OWNER_AFTER_BROWSER_RESET.scope(
        (reached_a.clone(), release_a.clone()), f.select(&f.alice)
    );
    tokio::pin!(change_a);
    tokio::select! {
        _ = reached_a.notified() => {},
        response = &mut change_a => panic!("A finished before reset boundary: {response:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("A reset not reached"),
    }
    // Both real resets have completed. Mint a NEW synthetic login before
    // either ownership transaction; neither reset can remove it afterwards.
    let profile = f.saved_profile();
    assert!(profile.restore().unwrap().0.is_some());
    let before = f.db.google_primary_owner_generation("bot").unwrap();
    let old_rows = f.db.get_agent_users("bot").unwrap();
    release_b.notify_one();
    payload(change_b.await);
    release_a.notify_one();
    payload(change_a.await);
    assert_eq!(f.db.google_owner("bot").unwrap().id, f.alice);
    assert_eq!(serde_json::to_value(f.db.get_agent_users("bot").unwrap()).unwrap(), serde_json::to_value(old_rows).unwrap(), "the old final-state binding fingerprint is identical");
    let (archive, reason) = profile.restore().unwrap();
    assert!(archive.is_none(), "overlapping A→B→A restored the synthetic previous login");
    assert_eq!(reason, "reset_owner_changed");
    assert_eq!(f.db.google_primary_owner_generation("bot").unwrap(), before + 2);
}


#[tokio::test]
async fn owner_disable_and_reenable_preserves_requester_boundary_through_identity_rpc() {
    for follow_owner in [false, true] {
        let f = PrimaryProfileFixture::new().await;
        let admin = f.db.live_user_context(&f.admin).unwrap().unwrap();
        let owner = f.db.live_user_context(&f.alice).unwrap().unwrap();
        assert!(!owner.is_admin());
        assert_eq!(f.db.check_agent_access(&f.alice, "bot").unwrap(), Some(AccessLevel::Owner));
        let identity = if follow_owner { "owner".to_owned() } else { format!("user:{}", f.alice) };
        let path = f.home.join("agents/bot/agent.toml");
        let seeded = payload(rpc(&f.handler, "agents.google_identity_set", json!({
            "agent_id":"bot", "identity":identity, "requesters":"owner_only"
        }), &admin).await);
        assert_eq!(seeded["requesters"], "owner_only");

        // Disabling needs no sharing confirmation and retains the dormant policy.
        let disabled = payload(rpc(&f.handler, "agents.google_identity_set", json!({
            "agent_id":"bot", "identity":"none"
        }), &owner).await);
        assert_eq!(disabled["requesters"], "owner_only", "follow_owner={follow_owner}");
        let table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(table["google"]["identity"].as_str(), Some("none"));
        assert_eq!(table["google"]["requesters"].as_str(), Some("owner_only"));
        assert!(table["google"].get("fallback").is_none());
        let readback = payload(rpc(&f.handler, "agents.google_identity_get", json!({"agent_id":"bot"}), &owner).await);
        assert_eq!(readback["requesters"], "owner_only");

        // Preserving serialization alone must not permit an explicit widening.
        for requesters in ["bound_people", "anyone"] {
            let before = std::fs::read(&path).unwrap();
            refused(rpc(&f.handler, "agents.google_identity_set", json!({
                "agent_id":"bot", "identity":"none", "requesters":requesters,
                "confirm_anyone":true, "confirm_shared_access":true
            }), &owner).await);
            assert_eq!(std::fs::read(&path).unwrap(), before, "follow_owner={follow_owner}, requesters={requesters}");
        }

        let enabled = payload(rpc(&f.handler, "agents.google_identity_set", json!({
            "agent_id":"bot", "identity":identity, "confirm_shared_access":true
        }), &owner).await);
        assert_eq!(enabled["requesters"], "owner_only");
        assert_eq!(enabled["identity"].as_str(), Some(identity.as_str()));
        let table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(table["google"]["requesters"].as_str(), Some("owner_only"));
        let readback = payload(rpc(&f.handler, "agents.google_identity_get", json!({"agent_id":"bot"}), &owner).await);
        assert_eq!(readback["requesters"], "owner_only");
        assert_eq!(f.db.google_consent(&f.alice, "bot").unwrap(), Some(true));
        let before = std::fs::read(&path).unwrap();
        refused(rpc(&f.handler, "agents.google_identity_set", json!({
            "agent_id":"bot", "identity":format!("user:{}", f.bob), "confirm_shared_access":true
        }), &owner).await);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn generic_agent_update_keeps_google_narrowing_before_owner_reenable() {
    for follow_owner in [false, true] {
        let f = PrimaryProfileFixture::new().await;
        let admin = f.db.live_user_context(&f.admin).unwrap().unwrap();
        let owner = f.db.live_user_context(&f.alice).unwrap().unwrap();
        assert!(!owner.is_admin());
        assert_eq!(f.db.check_agent_access(&f.alice, "bot").unwrap(), Some(AccessLevel::Owner));
        let identity = if follow_owner { "owner".to_owned() } else { format!("user:{}", f.alice) };
        let path = f.home.join("agents/bot/agent.toml");
        payload(rpc(&f.handler, "agents.google_identity_set", json!({
            "agent_id":"bot", "identity":identity, "requesters":"owner_only"
        }), &admin).await);

        payload(rpc(&f.handler, "agents.update", json!({
            "agent_id":"bot", "advanced_kv":[{"section":"google", "key":"identity", "type":"string", "value":"none"}]
        }), &owner).await);
        let table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(table["google"]["identity"].as_str(), Some("none"));
        assert_eq!(table["google"]["requesters"].as_str(), Some("owner_only"));
        let readback = payload(rpc(&f.handler, "agents.google_identity_get", json!({"agent_id":"bot"}), &owner).await);
        assert_eq!(readback["requesters"], "owner_only");
        for requesters in ["bound_people", "anyone"] {
            let before = std::fs::read(&path).unwrap();
            refused(rpc(&f.handler, "agents.update", json!({
                "agent_id":"bot", "advanced_kv":[{"section":"google", "key":"requesters", "type":"string", "value":requesters}]
            }), &owner).await);
            assert_eq!(std::fs::read(&path).unwrap(), before, "follow_owner={follow_owner}, requesters={requesters}");
        }
        // The generic update remains unable to select a different person's account.
        let before = std::fs::read(&path).unwrap();
        refused(rpc(&f.handler, "agents.update", json!({
            "agent_id":"bot", "advanced_kv":[{"section":"google", "key":"identity", "type":"string", "value":format!("user:{}", f.bob)}]
        }), &owner).await);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let enabled = payload(rpc(&f.handler, "agents.google_identity_set", json!({
            "agent_id":"bot", "identity":identity, "confirm_shared_access":true
        }), &owner).await);
        assert_eq!(enabled["requesters"], "owner_only");
        assert_eq!(enabled["identity"].as_str(), Some(identity.as_str()));
        let table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(table["google"]["requesters"].as_str(), Some("owner_only"));
        let readback = payload(rpc(&f.handler, "agents.google_identity_get", json!({"agent_id":"bot"}), &owner).await);
        assert_eq!(readback["requesters"], "owner_only");
    }
}
