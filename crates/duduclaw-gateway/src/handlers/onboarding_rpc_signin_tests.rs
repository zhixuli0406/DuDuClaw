use super::*;
use super::tests::{fixture, payload};

fn signin_row(home: &Path, user_id: &str) {
    rusqlite::Connection::open(home.join("users.db")).unwrap().execute(
        "INSERT INTO user_onboarding(user_id,source,avatar) VALUES(?1,'google_signin','pending')", [user_id]).unwrap();
}
fn policy(home: &Path, enforced: bool) {
    std::fs::write(home.join("config.toml"), format!("[auth]\nmfa_enforced={enforced}\n")).unwrap();
    crate::mfa::reload_policy(home);
}

#[tokio::test]
async fn signin_five_steps_include_waiting_reason_admin_binding_and_disabled_odoo() {
    let (home, handler, _db, ctx) = fixture().await;
    signin_row(home.path(), &ctx.user_id);
    policy(home.path(), false);
    rusqlite::Connection::open(home.path().join("users.db")).unwrap().execute(
        "UPDATE user_onboarding SET avatar='waiting_admin',avatar_error='template_missing' WHERE user_id=?1", [&ctx.user_id]).unwrap();
    let result = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
    let steps = &result["signin_steps"];
    assert_eq!(steps.as_object().unwrap().len(), 5);
    assert_eq!(steps["mfa"]["status"], "recommended");
    assert_eq!(steps["avatar"], json!({"status":"waiting_admin","reason":"template_missing"}));
    assert_eq!(steps["google"]["status"], "pending");
    assert_eq!(steps["google_chat"]["status"], "admin_binds");
    assert_eq!(steps["odoo"], json!({"status":"not_applicable"}));
    assert_eq!(result["steps_remaining"], 3);
}

#[tokio::test]
async fn signin_stored_done_flags_do_not_complete_live_steps_even_after_dismissal() {
    let (home, handler, _db, ctx) = fixture().await;
    signin_row(home.path(), &ctx.user_id);
    policy(home.path(), true);
    rusqlite::Connection::open(home.path().join("users.db")).unwrap().execute(
        "UPDATE user_onboarding SET mfa='done',avatar='done',google='done',google_chat='done',odoo='done',dismissed=1 WHERE user_id=?1", [&ctx.user_id]).unwrap();
    let result = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
    let steps = &result["signin_steps"];
    assert_eq!(steps["mfa"]["status"], "required");
    assert_eq!(steps["avatar"]["status"], "preparing");
    assert_eq!(steps["google"]["status"], "pending");
    assert_eq!(steps["google_chat"]["status"], "admin_binds");
    assert_eq!(steps["odoo"]["status"], "not_applicable");
    assert_eq!(result["steps_remaining"], 4);
}

#[tokio::test]
async fn signin_steps_follow_live_mfa_avatar_and_verified_chat_authorities() {
    let (home, handler, db, ctx) = fixture().await;
    signin_row(home.path(), &ctx.user_id);
    policy(home.path(), true);
    db.bind_channel_identity(&ctx.user_id, "googlechat", "users/test-onboarding", false).unwrap();
    let before = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
    assert_eq!(before["signin_steps"]["google_chat"]["status"], "admin_binds");
    let raw = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
    raw.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'test-only-unusable-secret',0)", [&ctx.user_id]).unwrap();
    raw.execute("UPDATE users SET avatar_agent_id='test-avatar' WHERE id=?1", [&ctx.user_id]).unwrap();
    db.bind_channel_identity(&ctx.user_id, "googlechat", "users/test-onboarding", true).unwrap();
    let result = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
    for step in ["mfa", "avatar", "google_chat"] { assert_eq!(result["signin_steps"][step]["status"], "done"); }
    assert_eq!(result["steps_remaining"], 1);
    // Removing a binding must be visible on the very next call.
    db.unbind_channel_identity("googlechat", "users/test-onboarding").unwrap();
    let next = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
    assert_eq!(next["signin_steps"]["google_chat"]["status"], "admin_binds");
}

#[tokio::test]
async fn legacy_users_omit_signin_steps_with_and_without_onboarding_rows() {
    let (home, handler, _db, ctx) = fixture().await;
    for insert in [false, true] {
        if insert {
            rusqlite::Connection::open(home.path().join("users.db")).unwrap().execute(
                "INSERT INTO user_onboarding(user_id) VALUES(?1)", [&ctx.user_id]).unwrap();
        }
        let result = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
        for field in ["signin", "signin_steps", "steps_remaining"] { assert!(result.get(field).is_none()); }
    }
}

#[tokio::test]
async fn signin_google_blocks_only_when_every_signin_domain_is_refused() {
    let (home, handler, _db, ctx) = fixture().await;
    signin_row(home.path(), &ctx.user_id);
    for (allowed, expected) in [("['other.test']", "blocked_domain"), ("['company.test']", "pending"), ("[]", "pending")] {
        std::fs::write(home.path().join("config.toml"), format!(
            "[auth]\nmfa_enforced=false\n[google_signin]\nallowed_hd=['company.test','second.test']\n[integrations.google_identity]\nallowed_domains={allowed}\n")).unwrap();
        let result = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
        assert_eq!(result["signin_steps"]["google"]["status"], expected);
    }
}

#[test]
fn signin_remaining_excludes_recommended_done_and_not_applicable() {
    let mut steps = LiveSigninSteps {
        mfa: SigninStep::new(SigninStatus::Recommended), avatar: SigninStep::new(SigninStatus::Done),
        google: SigninStep::new(SigninStatus::Done), google_chat: SigninStep::new(SigninStatus::Done),
        odoo: SigninStep::new(SigninStatus::NotApplicable),
    };
    assert_eq!(steps.remaining(), 0);
    for status in [SigninStatus::Required, SigninStatus::Unknown, SigninStatus::WaitingAdmin, SigninStatus::Preparing,
        SigninStatus::BlockedDomain, SigninStatus::Pending, SigninStatus::AdminBinds, SigninStatus::NeedsReverify, SigninStatus::Invalid] {
        steps.odoo = SigninStep::new(status);
        assert_eq!(steps.remaining(), 1);
    }
}

#[tokio::test]
async fn signin_google_uses_live_connection_metadata_without_unsealing() {
    let (home, handler, _db, ctx) = fixture().await;
    signin_row(home.path(), &ctx.user_id);
    policy(home.path(), false);
    let file = crate::person_credentials::path(home.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let record = json!({
        "owner_kind":"user", "owner_id":ctx.user_id, "provider":"google",
        "credential_id":uuid::Uuid::new_v4().to_string(), "revision":1,
        "account_email":"personal@company.test", "google_sub":"test-onboarding-sub",
        "hosted_domain":"company.test", "granted_scopes":[], "local_purposes":[],
        "status":"connected", "connected_at":chrono::Utc::now().to_rfc3339(),
        "last_refresh_at":null, "oauth_client_id":"test-client",
        "google_data_consent":crate::google_data_consent::Evidence::issue(&ctx.user_id).unwrap(),
        // Deliberately unusable ciphertext: metadata reads need no bearer.
        "secret":"pk:v1:unusable-test-envelope"
    });
    let mut store = json!({"version":1,"records":[record]});
    for (state, expected) in [("connected", "done"), ("needs_reconnect", "pending"), ("suspended", "pending")] {
        store["records"][0]["status"] = json!(state);
        std::fs::write(&file, serde_json::to_vec(&store).unwrap()).unwrap();
        let result = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
        assert_eq!(result["signin_steps"]["google"]["status"], expected);
        assert!(!result.to_string().contains("unusable-test-envelope"));
        assert!(!result.to_string().contains("personal@company.test"));
    }
    store["records"][0]["status"] = json!("connected");
    store["records"][0].as_object_mut().unwrap().remove("google_data_consent");
    std::fs::write(&file, serde_json::to_vec(&store).unwrap()).unwrap();
    let result = payload(handler.handle("onboarding.get", json!({}), &ctx).await);
    assert_eq!(result["signin_steps"]["google"]["status"], "pending");
}

#[tokio::test]
async fn signin_mfa_is_unknown_without_a_readable_policy() {
    let (home, _handler, db, ctx) = fixture().await;
    signin_row(home.path(), &ctx.user_id);
    // Do not rely on MethodHandler's initial policy snapshot.
    let unreadable = home.path().join("unreadable-policy-home");
    std::fs::create_dir_all(unreadable.join("config.toml")).unwrap();
    crate::mfa::reload_policy(&unreadable);
    let progress = db.onboarding_progress(&ctx.user_id, ctx.credential_epoch).unwrap();
    let steps = signin_steps(&unreadable, &db, &ctx.user_id, progress.signin.as_ref().unwrap()).unwrap();
    assert_eq!(steps.mfa.status, SigninStatus::Unknown);
}
