//! `agents.channels.*` through `MethodHandler::handle` (so the role / binding
//! gates run): who may bind what, duplicate refusals, verify-before-save,
//! ciphertext-only storage.
use super::*;

use crate::agent_channels::e2e_tests::FakeTelegramServer;

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e.as_str().unwrap_or_default().to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn admin() -> UserContext {
    let mut c = UserContext::admin_fallback();
    c.user_id = "admin-1".into();
    c
}

fn user(id: &str, bound: &[(&str, AccessLevel)]) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: id.into(),
        email: format!("{id}@example.invalid"),
        role: UserRole::Employee,
        agent_access: bound.iter().map(|(a, l)| (a.to_string(), *l)).collect(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

fn owner() -> UserContext {
    user("owner-7", &[("a1", AccessLevel::Owner)])
}

async fn setup() -> (tempfile::TempDir, MethodHandler) {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for name in ["a1", "a2"] {
        ok(&handler
            .handle_agents_create_as(
                json!({ "name": name, "display_name": name.to_uppercase() }),
                Some(&admin()),
            )
            .await);
    }
    let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
    for (id, role) in [
        ("admin-1", UserRole::Admin),
        ("owner-7", UserRole::Employee),
        ("viewer-8", UserRole::Employee),
        ("stranger-9", UserRole::Employee),
    ] {
        let u = db
            .create_user(
                &format!("{id}@example.invalid"),
                id,
                "isolated-test-password",
                role,
            )
            .unwrap();
        rusqlite::Connection::open(home.path().join("users.db"))
            .unwrap()
            .execute(
                "UPDATE users SET id=?1 WHERE id=?2",
                rusqlite::params![id, u.id],
            )
            .unwrap();
    }
    db.bind_agent("owner-7", "a1", AccessLevel::Owner).unwrap();
    db.bind_agent("viewer-8", "a1", AccessLevel::Viewer)
        .unwrap();
    (home, handler)
}

fn agent_toml(home: &Path, agent: &str) -> String {
    std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml")).unwrap()
}

#[tokio::test]
async fn admin_binds_a_bot_after_telegram_confirms_it_and_only_ciphertext_is_stored() {
    let (home, h) = setup().await;
    let mut fake = FakeTelegramServer::start().await;
    let token = fake.new_token(7300001);
    // The owner sees the status but cannot bind (per-employee bot tokens are
    // admin-only authority keys).
    let denied = err(&h
        .handle(
            "agents.channels.telegram.set",
            json!({"agent_id": "a1", "token": token}),
            &owner(),
        )
        .await);
    assert!(denied.contains("permission denied"), "{denied}");
    let r = ok(&h
        .handle(
            "agents.channels.telegram.set",
            json!({"agent_id": "a1", "token": token}),
            &admin(),
        )
        .await);
    assert_eq!(r["verified"], true);
    assert_eq!(r["bot_username"], "bot_7300001");
    let toml = agent_toml(home.path(), "a1");
    assert!(toml.contains("bot_token_enc"), "{toml}");
    assert!(!toml.contains(&token), "the token never lands in plaintext");
    assert!(!toml.contains(token.split(':').nth(1).unwrap()));

    let st = ok(&h
        .handle(
            "agents.channels.status",
            json!({"agent_id": "a1"}),
            &owner(),
        )
        .await);
    assert_eq!(st["telegram"]["configured"], true);
    assert_eq!(st["telegram"]["can_edit"], false, "binding is admin-only");
    assert_eq!(st["googlechat"]["can_edit"], false, "binding is admin-only");
    let st_admin = ok(&h
        .handle(
            "agents.channels.status",
            json!({"agent_id": "a1"}),
            &admin(),
        )
        .await);
    assert_eq!(st_admin["telegram"]["can_edit"], true);
    assert_eq!(st_admin["googlechat"]["can_edit"], true);
    assert_eq!(
        st["googlechat"]["endpoint_path"],
        "/webhook/googlechat/agents/a1"
    );

    // The same bot on a second employee is refused on every write path.
    let refused = err(&h
        .handle(
            "agents.channels.telegram.set",
            json!({"agent_id": "a2", "token": token}),
            &admin(),
        )
        .await);
    assert!(
        refused.contains("A1") && refused.contains("專屬 Bot"),
        "{refused}"
    );
    let via_update = err(&h
        .handle(
            "agents.update",
            json!({"agent_id": "a2", "telegram_bot_token": token}),
            &admin(),
        )
        .await);
    assert!(via_update.contains("專屬 Bot"), "{via_update}");
    let via_add = err(&h
        .handle(
            "channels.add",
            json!({"type": "telegram", "agent": "a2", "config": {"token": token}}),
            &admin(),
        )
        .await);
    assert!(via_add.contains("專屬 Bot"), "{via_add}");
    assert!(!agent_toml(home.path(), "a2").contains("[channels.telegram]"));

    // Remove: section gone.
    let e = err(&h
        .handle(
            "agents.channels.telegram.remove",
            json!({"agent_id": "a1"}),
            &owner(),
        )
        .await);
    assert!(e.contains("permission denied"), "{e}");
    ok(&h
        .handle(
            "agents.channels.telegram.remove",
            json!({"agent_id": "a1"}),
            &admin(),
        )
        .await);
    assert!(!agent_toml(home.path(), "a1").contains("[channels.telegram]"));
}

#[tokio::test]
async fn a_token_telegram_rejects_is_never_saved() {
    let (home, h) = setup().await;
    let mut fake = FakeTelegramServer::start().await;
    let token = fake.new_token(7300002);
    fake.state.revoke(&token);
    let e = err(&h
        .handle(
            "agents.channels.telegram.set",
            json!({"agent_id": "a1", "token": token}),
            &admin(),
        )
        .await);
    assert!(e.contains("Telegram 拒絕"), "{e}");
    assert!(!agent_toml(home.path(), "a1").contains("[channels.telegram]"));
    let shape = err(&h
        .handle(
            "agents.channels.telegram.set",
            json!({"agent_id": "a1", "token": "not-a-token"}),
            &admin(),
        )
        .await);
    assert!(shape.contains("格式不正確"), "{shape}");
}

#[tokio::test]
async fn only_owners_and_admins_reach_an_employees_channels() {
    let (_home, h) = setup().await;
    let viewer = user("viewer-8", &[("a1", AccessLevel::Viewer)]);
    let stranger = user("stranger-9", &[]);
    for (who, label) in [(&viewer, "viewer"), (&stranger, "stranger")] {
        let e = err(&h
            .handle("agents.channels.status", json!({"agent_id": "a1"}), who)
            .await);
        assert!(e.contains("permission denied"), "{label}: {e}");
        let e = err(&h
            .handle(
                "agents.channels.telegram.set",
                json!({"agent_id": "a1", "token": "1234567:abcdefghijklmnopqrstuvwxyz"}),
                who,
            )
            .await);
        assert!(e.contains("permission denied"), "{label}: {e}");
    }
    // The owner of a1 is not an owner of a2.
    let e = err(&h
        .handle(
            "agents.channels.status",
            json!({"agent_id": "a2"}),
            &owner(),
        )
        .await);
    assert!(e.contains("permission denied"), "{e}");
    // Every write is admin-only, even for the owner.
    for m in [
        "agents.channels.telegram.set",
        "agents.channels.telegram.remove",
        "agents.channels.googlechat.set",
        "agents.channels.googlechat.remove",
    ] {
        let e = err(&h
            .handle(
                m,
                json!({"agent_id": "a1", "project_number": "123456789"}),
                &owner(),
            )
            .await);
        assert!(e.contains("permission denied"), "{m}: {e}");
    }
}

#[tokio::test]
async fn admin_binds_a_chat_app_per_employee_and_duplicates_are_refused() {
    let (home, h) = setup().await;
    let fake = crate::agent_channels::e2e_gchat_tests::start_fake().await;
    let email = format!(
        "app-rpc-{}@p0-test.iam.gserviceaccount.com",
        uuid::Uuid::new_v4().simple()
    );
    let sa = crate::agent_channels::e2e_gchat_tests::service_account(&fake, &email);

    let missing = err(&h
        .handle(
            "agents.channels.googlechat.set",
            json!({"agent_id": "a1", "project_number": "444444444"}),
            &admin(),
        )
        .await);
    assert!(missing.contains("服務帳戶金鑰"), "{missing}");
    let bad_number = err(&h
        .handle(
            "agents.channels.googlechat.set",
            json!({"agent_id": "a1", "project_number": "my-project", "service_account_json": sa}),
            &admin(),
        )
        .await);
    assert!(bad_number.contains("專案編號"), "{bad_number}");

    let r = ok(&h
        .handle(
            "agents.channels.googlechat.set",
            json!({"agent_id": "a1", "project_number": "444444444", "service_account_json": sa}),
            &admin(),
        )
        .await);
    assert_eq!(r["endpoint_path"], "/webhook/googlechat/agents/a1");
    let toml = agent_toml(home.path(), "a1");
    assert!(
        toml.contains("service_account_json_enc") && toml.contains("444444444"),
        "{toml}"
    );
    assert!(
        !toml.contains("BEGIN PRIVATE KEY") && !toml.contains(&email),
        "key stored as ciphertext only"
    );

    // Project number change only keeps the stored key.
    ok(&h
        .handle(
            "agents.channels.googlechat.set",
            json!({"agent_id": "a1", "project_number": "444444445"}),
            &admin(),
        )
        .await);
    assert!(
        crate::agent_channels::store::agent_chat_app(home.path(), "a1")
            .await
            .is_some()
    );

    let dup = err(&h
        .handle(
            "agents.channels.googlechat.set",
            json!({"agent_id": "a2", "project_number": "444444445", "service_account_json": sa}),
            &admin(),
        )
        .await);
    assert!(dup.contains("A1"), "{dup}");

    // The shared app's project cannot be reused by an employee.
    std::fs::write(
        home.path().join("config.toml"),
        "[channels]\ngooglechat_project_number = \"555555555\"\n",
    )
    .unwrap();
    let shared = err(&h
        .handle(
            "agents.channels.googlechat.set",
            json!({"agent_id": "a2", "project_number": "555555555", "service_account_json": sa}),
            &admin(),
        )
        .await);
    assert!(shared.contains("共用"), "{shared}");

    ok(&h
        .handle(
            "agents.channels.googlechat.remove",
            json!({"agent_id": "a1"}),
            &admin(),
        )
        .await);
    assert!(!agent_toml(home.path(), "a1").contains("[channels.googlechat]"));
}
