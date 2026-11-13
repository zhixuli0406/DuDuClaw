//! `users.change_email`: self-service email change, and the `email_setup`
//! block `users.me` returns so the dashboard knows whether to ask for a real
//! address and whether to ask for the current password.
use super::*;

struct Fixture {
    handler: MethodHandler,
    db: Arc<UserDb>,
    owner: UserContext,
    _home: tempfile::TempDir,
}

/// A claimed bootstrap owner still on `admin@local` with a known password.
/// `auto_login_off` writes `local_auto_login = "off"` so the owner must
/// present that password.
async fn fixture(auto_login_off: bool) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    if auto_login_off {
        std::fs::write(
            home.path().join("config.toml"),
            "[dashboard]\nlocal_auto_login = \"off\"\n",
        )
        .unwrap();
    }
    let db = UserDb::new(&home.path().join("users.db")).expect("open user db");
    db.ensure_default_admin().unwrap().expect("admin created");
    assert!(db.claim_default_admin("owner-password-1").unwrap());
    let owner = db.get_bootstrap_owner().unwrap().expect("owner");

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let db = Arc::new(db);
    let jwt = Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"));
    handler.set_user_db(db.clone(), jwt).await;
    let ctx = UserContext {
        credential_epoch: owner.credential_epoch,
        user_id: owner.id.clone(),
        email: owner.email.clone(),
        role: UserRole::Admin,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    };
    Fixture {
        handler,
        db,
        owner: ctx,
        _home: home,
    }
}

fn ok_payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        other => panic!("expected ok response, got {other:?}"),
    }
}

fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(e),
            ..
        } => e.to_string(),
        other => panic!("expected error response, got {other:?}"),
    }
}

/// Personal edition, auto-login on (the defaults): the owner sets a real
/// email without a password, which they never chose.
#[tokio::test]
async fn personal_auto_login_owner_sets_email_without_password() {
    let mut f = fixture(false).await;
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &f.owner).await);
    assert_eq!(me["email_setup"]["placeholder"], json!(true));
    assert_eq!(me["email_setup"]["password_required"], json!(false));

    let frame = f
        .handler
        .handle(
            "users.change_email",
            json!({"email": " owner@example.com "}),
            &f.owner,
        )
        .await;
    assert_eq!(ok_payload(&frame)["email"], json!("owner@example.com"));
    assert_eq!(
        f.db.get_bootstrap_owner().unwrap().unwrap().email,
        "owner@example.com"
    );

    assert!(matches!(f.handler.handle("users.me", json!({}), &f.owner).await,
        WsFrame::Response { ok: false, .. }));
    f.owner = f.db.live_user_context(&f.owner.user_id).unwrap().unwrap();
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &f.owner).await);
    assert_eq!(me["email_setup"]["placeholder"], json!(false));
}

#[tokio::test]
async fn owner_without_auto_login_needs_current_password() {
    let f = fixture(true).await;
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &f.owner).await);
    assert_eq!(me["email_setup"]["password_required"], json!(true));

    let frame = f
        .handler
        .handle(
            "users.change_email",
            json!({"email": "owner@example.com"}),
            &f.owner,
        )
        .await;
    assert!(error_text(&frame).contains("current password is incorrect"));
    assert_eq!(
        f.db.get_bootstrap_owner().unwrap().unwrap().email,
        "admin@local"
    );

    let frame = f
        .handler
        .handle(
            "users.change_email",
            json!({"email": "owner@example.com", "current_password": "owner-password-1"}),
            &f.owner,
        )
        .await;
    assert_eq!(ok_payload(&frame)["status"], json!("changed"));
    assert!(f
        .db
        .verify_password("owner@example.com", "owner-password-1")
        .is_ok());
}

/// The passwordless exception belongs to the bootstrap owner only: any other
/// account, even an admin on the same Personal install, needs its password.
#[tokio::test]
async fn other_accounts_always_need_password() {
    let f = fixture(false).await;
    let other =
        f.db.create_user(
            "second@example.com",
            "Second",
            "second-password",
            UserRole::Admin,
        )
        .unwrap();
    let ctx = UserContext {
        credential_epoch: 0,
        user_id: other.id.clone(),
        email: other.email.clone(),
        role: UserRole::Admin,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    };
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &ctx).await);
    assert_eq!(me["email_setup"]["placeholder"], json!(false));
    assert_eq!(me["email_setup"]["password_required"], json!(true));

    let frame = f
        .handler
        .handle(
            "users.change_email",
            json!({"email": "x@example.com"}),
            &ctx,
        )
        .await;
    assert!(error_text(&frame).contains("current password is incorrect"));
}

#[tokio::test]
async fn invalid_or_taken_email_is_refused() {
    let f = fixture(false).await;
    f.db.create_user(
        "taken@example.com",
        "T",
        "taken-password",
        UserRole::Employee,
    )
    .unwrap();
    let frame = f
        .handler
        .handle(
            "users.change_email",
            json!({"email": "not-an-email"}),
            &f.owner,
        )
        .await;
    assert!(error_text(&frame).contains("invalid email format"));
    let frame = f
        .handler
        .handle(
            "users.change_email",
            json!({"email": "taken@example.com"}),
            &f.owner,
        )
        .await;
    assert!(error_text(&frame).contains(duduclaw_auth::db::EMAIL_IN_USE));
    assert_eq!(
        f.db.get_bootstrap_owner().unwrap().unwrap().email,
        "admin@local"
    );
}

#[test]
fn change_email_is_open_in_personal_and_closed_to_flagged_accounts() {
    assert!(!is_enterprise_only_method("users.change_email"));
    assert!(!is_password_change_allowlisted("users.change_email"));
}

/// The same exception covers setting a password: a Personal owner signed in
/// by local auto-login can set one (the step before turning auto-login off)
/// without the random password the implicit claim discarded.
#[tokio::test]
async fn personal_auto_login_owner_sets_password_without_current() {
    let f = fixture(false).await;
    let frame = f
        .handler
        .handle(
            "users.change_password",
            json!({"new_password": "chosen-password-1"}),
            &f.owner,
        )
        .await;
    assert_eq!(ok_payload(&frame)["status"], json!("changed"));
    assert!(f
        .db
        .verify_password("admin@local", "chosen-password-1")
        .is_ok());
}

#[tokio::test]
async fn password_change_still_needs_current_password_elsewhere() {
    let f = fixture(true).await;
    let frame = f
        .handler
        .handle(
            "users.change_password",
            json!({"new_password": "chosen-password-1"}),
            &f.owner,
        )
        .await;
    assert!(error_text(&frame).contains("current password is incorrect"));
    assert!(f
        .db
        .verify_password("admin@local", "owner-password-1")
        .is_ok());

    let frame = f
        .handler
        .handle(
            "users.change_password",
            json!({"current_password": "owner-password-1", "new_password": "chosen-password-1"}),
            &f.owner,
        )
        .await;
    assert_eq!(ok_payload(&frame)["status"], json!("changed"));
}

#[tokio::test]
async fn primary_agent_supports_admin_target_and_uses_live_bindings() {
    let f = fixture(false).await;
    f.db.bind_agent_as(&f.owner.user_id, &f.owner.user_id, "first", duduclaw_auth::AccessLevel::Owner).unwrap();
    f.db.bind_agent(&f.owner.user_id, "chosen", duduclaw_auth::AccessLevel::Viewer).unwrap();
    let other = f.db.create_user("primary-other@example.com", "Other", "password123", UserRole::Employee).unwrap();
    f.db.bind_agent_as(&f.owner.user_id, &other.id, "other-first", duduclaw_auth::AccessLevel::Owner).unwrap();
    f.db.bind_agent_as(&f.owner.user_id, &other.id, "other-only", duduclaw_auth::AccessLevel::Operator).unwrap();
    assert_eq!(f.db.bound_primary_agent(&other.id).unwrap().as_deref(), Some("other-first"));
    ok_payload(&f.handler.handle("users.set_primary_agent", json!({"agent_id": "other-only", "user_id": other.id}), &f.owner).await);
    assert_eq!(f.db.bound_primary_agent(&other.id).unwrap().as_deref(), Some("other-only"));
    let refused = f.handler.handle("users.set_primary_agent", json!({"agent_id": "chosen"}), &f.owner).await;
    assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
    assert_eq!(f.db.primary_agent(&f.owner.user_id).unwrap().as_deref(), Some("first"));
    // The existing context must use the upgraded live binding, not a JWT snapshot.
    f.db.bind_agent(&f.owner.user_id, "chosen", duduclaw_auth::AccessLevel::Operator).unwrap();
    ok_payload(&f.handler.handle("users.set_primary_agent", json!({"agent_id": "chosen"}), &f.owner).await);
    let refused = f.handler.handle("users.set_primary_agent", json!({"agent_id": "chosen", "user_id": other.id}), &f.owner).await;
    assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
    assert_eq!(f.db.primary_agent(&other.id).unwrap().as_deref(), Some("other-only"));
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &f.owner).await);
    assert_eq!(me["user"]["primary_agent_id"], "chosen");
    f.db.bind_agent(&f.owner.user_id, "chosen", duduclaw_auth::AccessLevel::Viewer).unwrap();
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &f.owner).await);
    assert_eq!(me["user"]["primary_agent_id"], "first");
    let refused = f.handler.handle("users.set_primary_agent", json!({"agent_id": "chosen"}), &f.owner).await;
    assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
    f.db.unbind_agent(&f.owner.user_id, "chosen").unwrap();
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &f.owner).await);
    assert_eq!(me["user"]["primary_agent_id"], "first");
    ok_payload(&f.handler.handle("users.set_primary_agent", json!({"agent_id": null}), &f.owner).await);
    let me = ok_payload(&f.handler.handle("users.me", json!({}), &f.owner).await);
    assert!(me["user"]["primary_agent_id"].is_null());
    let mut employee = f.owner.clone();
    employee.user_id = other.id.clone();
    employee.email = other.email.clone();
    employee.role = UserRole::Employee;
    ok_payload(&f.handler.handle("users.set_primary_agent", json!({"agent_id": "other-only"}), &employee).await);
    assert_eq!(f.db.primary_agent(&other.id).unwrap().as_deref(), Some("other-only"));
    let refused = f.handler.handle("users.set_primary_agent", json!({"agent_id": "first", "user_id": f.owner.user_id}), &employee).await;
    assert!(matches!(refused, WsFrame::Response { ok: false, .. }));
    assert!(!is_enterprise_only_method("users.set_primary_agent"));
    assert!(!is_password_change_allowlisted("users.set_primary_agent"));
}

#[tokio::test]
async fn invalid_user_settings_leave_every_field_unchanged() {
    let f = fixture(false).await;
    for params in [
        json!({"user_id": f.owner.user_id, "display_name": "Changed", "primary_agent_id": "ignored"}),
        json!({"user_id": f.owner.user_id, "display_name": "Changed", "role": "admn"}),
        json!({"user_id": f.owner.user_id, "display_name": "Changed", "role": 123}),
        json!({"user_id": f.owner.user_id, "display_name": "Changed", "department": false}),
        json!({"user_id": f.owner.user_id, "display_name": false}),
        json!({"user_id": f.owner.user_id, "password": null}),
    ] {
        let before = f.db.get_user(&f.owner.user_id).unwrap().unwrap();
        let frame = f.handler.handle_users_update(params, &f.owner).await;
        match frame {
            WsFrame::Response { ok: false, error: Some(error), .. } => {
                assert_eq!(error["code"], "invalid_setting");
                assert!(error["key"].is_string());
            }
            other => panic!("expected structured error: {other:?}"),
        }
        let after = f.db.get_user(&f.owner.user_id).unwrap().unwrap();
        assert_eq!(before.display_name, after.display_name);
        assert_eq!(before.role, after.role);
        assert_eq!(before.updated_at, after.updated_at);
    }
    for extra in [json!({"unknown": true}), json!({"role": false}), json!({"role": "admn"})] {
        let mut params = json!({"email": "invalid-create@example.com", "display_name": "New", "password": "password123"});
        params.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        error_text(&f.handler.handle_users_create(params, &f.owner).await);
    }
    assert_eq!(f.db.list_users().unwrap().len(), 1);
    error_text(&f.handler.handle_users_bind_agent(json!({"user_id": f.owner.user_id, "agent_name": "test", "typo": true}), &f.owner).await);
    assert!(f.db.get_user_agents(&f.owner.user_id).unwrap().is_empty());
}

#[tokio::test]
async fn primary_repair_requires_admin_and_exact_confirmation() {
    let f = fixture(false).await;
    let other = f.db.create_user("repair@example.com", "Repair", "password123", UserRole::Employee).unwrap();
    f.db.bind_agent_as(&f.owner.user_id, &other.id, "earliest", duduclaw_auth::AccessLevel::Operator).unwrap();
    assert_eq!(f.db.bound_primary_agent(&other.id).unwrap().as_deref(), Some("earliest"));
    f.db.set_primary_agent(&other.id, None).unwrap();
    f.db.bind_agent(&other.id, "later", duduclaw_auth::AccessLevel::Operator).unwrap();
    assert_eq!(f.db.primary_agent(&other.id).unwrap(), None);
    let mut member = f.owner.clone();
    member.user_id = other.id.clone(); member.email = other.email; member.role = UserRole::Employee;
    error_text(&f.handler.handle_users_repair_primary_agents(json!({}), &member).await);
    let preview = ok_payload(&f.handler.handle_users_repair_primary_agents(json!({}), &f.owner).await);
    assert_eq!(preview["members"][0]["agent_id"], "earliest");
    assert_eq!(f.db.primary_agent(&other.id).unwrap(), None);
    error_text(&f.handler.handle_users_repair_primary_agents(json!({"preview_digest": "invalid"}), &f.owner).await);
    error_text(&f.handler.handle_users_repair_primary_agents(json!({"preview_digest": "0".repeat(64)}), &f.owner).await);
    let changed = ok_payload(&f.handler.handle_users_repair_primary_agents(json!({"preview_digest": preview["preview_digest"]}), &f.owner).await);
    assert_eq!(changed["members"], preview["members"]);
    assert_eq!(f.db.bound_primary_agent(&other.id).unwrap().as_deref(), Some("earliest"));
    let audit = f.db.query_audit_log(Some(&f.owner.user_id), Some("user.set_primary_agent"), 10).unwrap();
    assert!(audit.iter().any(|entry| entry.target.as_deref() == Some(other.id.as_str())));
    let preview = ok_payload(&f.handler.handle_users_repair_primary_agents(json!({}), &f.owner).await);
    assert_eq!(preview["members"], json!([]));
}

fn seed_agent(home: &std::path::Path, name: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = "🤖"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["*"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true
"#
    );
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

#[tokio::test]
async fn bind_rpc_sets_only_the_first_eligible_binding_as_primary() {
    let f = fixture(false).await;
    seed_agent(f._home.path(), "first-ai");
    seed_agent(f._home.path(), "second-ai");
    seed_agent(f._home.path(), "viewer-ai");
    f.handler.registry.write().await.scan().await.unwrap();
    ok_payload(&f.handler.handle_users_bind_agent(json!({"user_id": f.owner.user_id, "agent_name": "viewer-ai", "access_level": "viewer"}), &f.owner).await);
    assert_eq!(f.db.configured_primary_agent(&f.owner.user_id).unwrap(), None);
    ok_payload(&f.handler.handle_users_bind_agent(json!({"user_id": f.owner.user_id, "agent_name": "first-ai", "access_level": "operator"}), &f.owner).await);
    assert_eq!(f.db.bound_primary_agent(&f.owner.user_id).unwrap().as_deref(), Some("first-ai"));
    f.db.set_primary_agent(&f.owner.user_id, None).unwrap();
    ok_payload(&f.handler.handle_users_bind_agent(json!({"user_id": f.owner.user_id, "agent_name": "second-ai", "access_level": "operator"}), &f.owner).await);
    assert_eq!(f.db.configured_primary_agent(&f.owner.user_id).unwrap(), None);
}

#[tokio::test]
async fn unreadable_bindings_refuse_list_and_repair_without_exposing_sql_errors() {
    let f = fixture(false).await;
    let conn = rusqlite::Connection::open(f._home.path().join("users.db")).unwrap();
    conn.execute_batch("DROP TABLE user_agent_bindings").unwrap();
    let list = f.handler.handle_users_list(&f.owner).await;
    assert!(matches!(list, WsFrame::Response { ok: false, .. }));
    let repair = f.handler.handle_users_repair_primary_agents(json!({}), &f.owner).await;
    match repair {
        WsFrame::Response { ok: false, error: Some(error), .. } => {
            assert_eq!(error["code"], "primary_repair_refused");
            assert!(!error.to_string().contains("user_agent_bindings"));
            assert!(!error.to_string().contains("no such table"));
        }
        other => panic!("expected structured refusal: {other:?}"),
    }
}
