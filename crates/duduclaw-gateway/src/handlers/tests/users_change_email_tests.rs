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
        user_id: owner.id.clone(),
        email: owner.email.clone(),
        role: UserRole::Admin,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
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
    let f = fixture(false).await;
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
        user_id: other.id.clone(),
        email: other.email.clone(),
        role: UserRole::Admin,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
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
