use super::*;
use duduclaw_auth::signin_members::{SigninApproval, SigninOutcome, SigninPolicy, VerifiedGoogleIdentity};
use sha2::{Digest, Sha256};

const METHODS: &[&str] = &[
    "google.signin.get", "google.signin.set", "users.signin_requests.list",
    "users.signin_requests.approve", "users.signin_requests.reject", "users.google_signin.attest",
    "users.google_signin.unlink", "users.signin_onboarding.retry_avatar",
];
const BASE: &str = "[dashboard]\npublic_url='https://company.test/app'\n[member_api_keys]\nenabled=true\nrate_per_minute=100\n";
const CSRF: [u8; 32] = [7; 32];
struct Fixture {
    home: tempfile::TempDir, db: Arc<UserDb>, handler: MethodHandler,
    admin: UserContext, employee: UserContext, manager: UserContext, session: String,
}
async fn fixture() -> Fixture {
    let home = crate::fs_safe::private_test_home().unwrap();
    std::fs::write(home.path().join("config.toml"), BASE).unwrap();
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let admin = db.create_user("admin@company.test", "Admin", "password123", UserRole::Admin).unwrap();
    let employee = db.create_user("employee@company.test", "Employee", "password123", UserRole::Employee).unwrap();
    let manager = db.create_user("manager@company.test", "Manager", "password123", UserRole::Manager).unwrap();
    // Fixture for an already completed password + TOTP step-up. Auth tests
    // exercise proof issuance; these tests exercise its RPC consumption.
    let raw = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
    raw.execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'test-only-unusable-secret',0)", [&admin.id]).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"google-signin-rpc-tests-32-byte-secret"))).await;
    handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    Fixture { admin: db.live_user_context(&admin.id).unwrap().unwrap(),
        employee: db.live_user_context(&employee.id).unwrap().unwrap(),
        manager: db.live_user_context(&manager.id).unwrap().unwrap(),
        home, db, handler, session: uuid::Uuid::new_v4().to_string() }
}
impl Fixture {
    fn raw(&self) -> rusqlite::Connection { rusqlite::Connection::open(self.home.path().join("users.db")).unwrap() }
    fn proof(&self, at: i64, session: &str, epoch: u64) {
        let hash = format!("{:x}", Sha256::digest(session.as_bytes()));
        self.raw().execute("INSERT INTO migration_dashboard_proofs(user_id,session_hash,epoch,verified_at,csrf)
            VALUES(?1,?2,?3,?4,?5) ON CONFLICT(user_id,session_hash) DO UPDATE SET
            epoch=excluded.epoch,verified_at=excluded.verified_at,csrf=excluded.csrf",
            params![self.admin.user_id, hash, epoch, at, CSRF.as_slice()]).unwrap();
    }
    fn fresh_proof(&self) { self.proof(Utc::now().timestamp(), &self.session, self.admin.credential_epoch); }
    async fn call(&self, ctx: &UserContext, method: &str, params: Value) -> WsFrame {
        crate::google_personal::DASHBOARD_SESSION.scope(Some((ctx.user_id.clone(), self.session.clone())),
            self.handler.handle(method, params, ctx)).await
    }
    async fn admin(&self, method: &str, params: Value) -> WsFrame { self.call(&self.admin, method, params).await }
    fn provision(&self, sub: &str, approval: SigninApproval) -> String {
        let identity = VerifiedGoogleIdentity { sub: sub.into(), email: format!("new{sub}@company.test"),
            hd: "company.test".into(), display_name: "新同仁".into() };
        let policy = SigninPolicy { approval, daily_cap: 100, settings_version: "v1".into(),
            ack_by: self.admin.user_id.clone(), ack_at: Utc::now().to_rfc3339() };
        match self.db.resolve_google_signin(true, &identity, &policy, Utc::now().timestamp(), &|_| Ok(None)).unwrap() {
            SigninOutcome::PendingApproval { request_id } => request_id,
            SigninOutcome::Ok { user_id, .. } => user_id,
            other => panic!("unexpected provisioning result: {other:?}"),
        }
    }
}
fn settings() -> Value {
    json!({"enabled": true, "allowed_hd": ["company.test"], "approval": "admin", "daily_new_accounts": 20,
        "avatar_template": "", "avatar_template_revision": 0, "approval_grants_mfa_enrollment": false,
        "acknowledge_risk": true, "step_up_csrf": CSRF})
}
fn payload(frame: WsFrame) -> Value {
    match frame { WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("expected success: {other:?}") }
}
fn code(frame: WsFrame) -> String {
    match frame { WsFrame::Response { ok: false, error: Some(value), .. } => value["code"].as_str().unwrap_or("").into(),
        other => panic!("expected refusal: {other:?}") }
}
fn refused(frame: WsFrame) -> bool { matches!(frame, WsFrame::Response { ok: false, .. }) }

#[tokio::test]
async fn role_table_dashboard_only_and_member_keys_refused() {
    let f = fixture().await;
    let key = f.db.member_key_create(&f.employee.user_id, "Test", 1, f.employee.credential_epoch, false, &[]).unwrap();
    let key_ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &key.secret).unwrap();
    for method in METHODS {
        assert_eq!(super::super::gates::google_signin_minimum_role(method), Some(UserRole::Admin));
        assert_eq!(super::super::member_contract::member_key_method(method), None);
        for ctx in [&f.employee, &f.manager] {
            assert_eq!(code(f.call(ctx, method, json!({})).await), "signin_admin_required", "{method}");
        }
        assert_eq!(code(f.handler.handle(method, json!({}), &key_ctx).await), "member_key_forbidden", "{method}");
        assert_eq!(code(f.handler.handle(method, json!({}), &f.admin).await), "signin_dashboard_required", "{method}");
        // There is no MCP definition under either RPC or MCP-style spelling.
        assert!(!duduclaw_core::mcp_tool_defs::tools().any(|tool| tool.name == *method || tool.name == method.replace('.', "_")));
    }
    for method in ["google.signin.future", "users.signin_requests.future", "users.google_signin.future", "users.signin_onboarding.future"] {
        assert_eq!(super::super::gates::google_signin_minimum_role(method), Some(UserRole::Admin));
        assert!(refused(f.admin(method, json!({})).await));
    }
    assert_eq!(super::super::gates::google_signin_minimum_role("google.signinx.get"), None);
    payload(f.admin("google.signin.get", json!({})).await);
    payload(f.admin("users.signin_requests.list", json!({})).await);
}

#[tokio::test]
async fn set_and_attest_require_fresh_session_bound_step_up() {
    let f = fixture().await;
    let attest = json!({"user_ids": [f.employee.user_id], "step_up_csrf": CSRF});
    for (method, input) in [("google.signin.set", settings()), ("users.google_signin.attest", attest)] {
        assert_eq!(code(f.admin(method, input.clone()).await), "signin_step_up_required");
        f.proof(Utc::now().timestamp() - 301, &f.session, f.admin.credential_epoch);
        assert_eq!(code(f.admin(method, input.clone()).await), "signin_step_up_required");
        f.proof(Utc::now().timestamp(), &f.session, f.admin.credential_epoch + 1);
        assert_eq!(code(f.admin(method, input.clone()).await), "signin_step_up_required");
        f.fresh_proof();
        let mut missing = input.clone();
        missing.as_object_mut().unwrap().remove("step_up_csrf");
        assert_eq!(code(f.admin(method, missing).await), "signin_step_up_required");
        let mut wrong = input.clone(); wrong["step_up_csrf"] = json!(vec![8u8; 32]);
        assert_eq!(code(f.admin(method, wrong).await), "signin_step_up_required");
        let other_session = uuid::Uuid::new_v4().to_string();
        let result = crate::google_personal::DASHBOARD_SESSION.scope(Some((f.admin.user_id.clone(), other_session)),
            f.handler.handle(method, input.clone(), &f.admin)).await;
        assert_eq!(code(result), "signin_step_up_required");
        payload(f.admin(method, input.clone()).await);
        let mut stale = f.admin.clone();
        stale.credential_epoch += 1;
        assert_eq!(code(f.call(&stale, method, input.clone()).await), "signin_admin_required");
        let mut forged = f.employee.clone();
        forged.role = UserRole::Admin;
        assert_eq!(code(f.call(&forged, method, input).await), "signin_admin_required");
        f.raw().execute("DELETE FROM migration_dashboard_proofs", []).unwrap();
    }
}

#[tokio::test]
async fn disable_needs_no_step_up() {
    let f = fixture().await;
    f.fresh_proof();
    payload(f.admin("google.signin.set", settings()).await);
    f.raw().execute("DELETE FROM migration_dashboard_proofs", []).unwrap();
    let mut input = settings();
    input["enabled"] = json!(false);
    input.as_object_mut().unwrap().remove("step_up_csrf");
    let result = payload(f.admin("google.signin.set", input).await);
    assert_eq!(result["settings"]["enabled"], false);
    assert!(!config::read_strict(f.home.path()).enabled);
}

#[tokio::test]
async fn concurrent_tightening_survives_unauthenticated_disable() {
    let f = fixture().await;
    let now = Utc::now().to_rfc3339();
    let old = SettingsUpdate {
        enabled: true, allowed_hd: vec!["company.test".into(), "other.test".into()],
        approval: Approval::Direct, daily_new_accounts: 100,
        avatar_template: AvatarTemplate::Company { key: "sales".into(), revision: 7 },
        approval_grants_mfa_enrollment: true,
    };
    config::write_settings(f.home.path(), &f.admin.user_id, &old, true, &now).unwrap();
    let mut stale_disable = old.clone();
    stale_disable.enabled = false;
    // Run the production exemption decision before the other admin tightens.
    let pure_disable = is_pure_disable(f.home.path(), &stale_disable);
    assert!(pure_disable);
    assert!(step_up(&f.db, &f.admin, &f.session, None).is_err());
    let other_admin = f.db.create_user("other-admin@company.test", "Other admin", "password123", UserRole::Admin).unwrap();
    let tightened = SettingsUpdate {
        allowed_hd: vec!["company.test".into()], approval: Approval::Admin,
        daily_new_accounts: 5, approval_grants_mfa_enrollment: false, ..old
    };
    let (_, tightened_before_disable) = config::write_settings(
        f.home.path(), &other_admin.id, &tightened, true, &now,
    ).unwrap();
    assert!(tightened_before_disable.enabled);
    // Resume the same production write branch with the stale exemption result.
    let (before, after) = apply_settings(
        f.home.path(), &f.admin.user_id, &stale_disable, pure_disable, false, &now,
    ).unwrap();
    assert_eq!(before, tightened_before_disable);
    assert_eq!(after, config::read_strict(f.home.path()));
    assert!(!after.enabled);
    assert!(!after.enabled_requested);
    assert!(after.ack.is_none());
    assert_eq!(after.allowed_hd, tightened.allowed_hd);
    assert_eq!(after.approval, Approval::Admin);
    assert_eq!(after.daily_new_accounts, 5);
    assert_eq!(after.avatar_template, tightened.avatar_template);
    assert!(!after.approval_grants_mfa_enrollment);
}

#[tokio::test]
async fn disable_with_other_changes_needs_step_up() {
    let f = fixture().await;
    f.fresh_proof();
    payload(f.admin("google.signin.set", settings()).await);
    f.raw().execute("DELETE FROM migration_dashboard_proofs", []).unwrap();
    let before = std::fs::read_to_string(f.home.path().join("config.toml")).unwrap();
    let intents = f.db.query_audit_log(Some(&f.admin.user_id), Some("google_signin.settings_change_requested"), 100).unwrap().len();
    let mut input = settings();
    input["enabled"] = json!(false);
    input["approval_grants_mfa_enrollment"] = json!(true);
    input.as_object_mut().unwrap().remove("step_up_csrf");
    assert_eq!(code(f.admin("google.signin.set", input).await), "signin_step_up_required");
    assert_eq!(std::fs::read_to_string(f.home.path().join("config.toml")).unwrap(), before);
    assert_eq!(f.db.query_audit_log(Some(&f.admin.user_id), Some("google_signin.settings_change_requested"), 100).unwrap().len(), intents);
}

#[tokio::test]
async fn risk_ack_required_written_and_settings_audited() {
    let f = fixture().await;
    f.fresh_proof();
    for change in ["enabled", "direct", "preauth"] {
        std::fs::write(f.home.path().join("config.toml"), BASE).unwrap();
        let mut input = settings();
        input["enabled"] = json!(change == "enabled");
        input["approval"] = json!(if change == "direct" { "direct" } else { "admin" });
        input["approval_grants_mfa_enrollment"] = json!(change == "preauth");
        input["acknowledge_risk"] = json!(false);
        assert!(refused(f.admin("google.signin.set", input.clone()).await), "{change}");
        assert_eq!(std::fs::read_to_string(f.home.path().join("config.toml")).unwrap(), BASE);
        input["acknowledge_risk"] = json!(true);
        payload(f.admin("google.signin.set", input).await);
    }
    let result = payload(f.admin("google.signin.set", settings()).await);
    assert_eq!(result["settings"]["ack"]["by"], f.admin.user_id);
    assert!(result["settings"]["ack"]["at"].is_string());
    let stored = config::read_strict(f.home.path());
    assert!(stored.enabled);
    assert_eq!(stored.ack.unwrap().0, f.admin.user_id);
    let rows = f.db.query_audit_log(Some(&f.admin.user_id), Some("google_signin.settings_changed"), 100).unwrap();
    assert_eq!(rows.len(), 4);
    for row in rows {
        let detail: Value = serde_json::from_str(row.detail.as_deref().unwrap()).unwrap();
        for side in ["before", "after"] {
            for field in ["allowed_hd", "approval", "daily_new_accounts", "avatar_template", "enabled", "approval_grants_mfa_enrollment"] {
                assert!(detail[side].get(field).is_some());
            }
        }
        assert!(!detail.to_string().contains("client_secret"));
    }
    let mut forged = settings(); forged["actor"] = json!("someone-else");
    assert_eq!(code(f.admin("google.signin.set", forged).await), "signin_invalid_params");
}

#[tokio::test]
async fn settings_checks_reuse_redirect_and_personal_edition_refuses_writes() {
    let f = fixture().await;
    f.fresh_proof();
    payload(f.admin("google.signin.set", settings()).await);
    let get = payload(f.admin("google.signin.get", json!({})).await);
    assert_eq!(get["redirect_uri"], routes::redirect_uri(&routes::public_base(f.home.path()).unwrap().0));
    assert_eq!(get["checks"]["public_url_ok"], true);
    assert_eq!(get["checks"]["client_configured"], false);
    assert_eq!(get["checks"]["domains_set"], true);
    assert_eq!(get["checks"]["personal_connection_domains_cover"], true);
    assert!(!get.to_string().contains("client_secret"));
    let saved = std::fs::read_to_string(f.home.path().join("config.toml")).unwrap();
    for (domains, expected) in [("['other.test']", false), ("['company.test','other.test']", true), ("[]", true)] {
        std::fs::write(f.home.path().join("config.toml"), format!("{saved}\n[integrations.google_identity]\nallowed_domains={domains}\n")).unwrap();
        assert_eq!(payload(f.admin("google.signin.get", json!({})).await)["checks"]["personal_connection_domains_cover"], expected);
    }
    f.handler.set_test_edition(Some(duduclaw_core::EditionProfile::Personal)).await;
    assert_eq!(code(f.admin("google.signin.set", settings()).await), "signin_personal_edition");
    assert_eq!(code(f.admin("google.signin.get", json!({})).await), "signin_personal_edition");
    for method in &METHODS[2..] { assert!(refused(f.admin(method, json!({})).await)); }
}

#[tokio::test]
async fn requests_list_approve_reject_and_preauthorize_policy() {
    let f = fixture().await;
    let id = f.provision("123", SigninApproval::Admin);
    let rows = payload(f.admin("users.signin_requests.list", json!({})).await);
    assert_eq!(rows["requests"][0]["id"], id);
    assert_eq!(rows["requests"][0].as_object().unwrap().len(), 6);
    assert!(rows["requests"][0].get("google_sub").is_none());
    assert_eq!(code(f.admin("users.signin_requests.approve", json!({"id": id, "mfa_preauthorize": true})).await), "signin_preauthorize_disabled");
    assert_eq!(f.db.signin_request_by_id(&id).unwrap().unwrap().status, duduclaw_auth::google_signin::SigninRequestStatus::Pending);
    let created = payload(f.admin("users.signin_requests.approve", json!({"id": id})).await);
    let user = f.db.get_user(created["user_id"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(user.role, UserRole::Employee);
    assert!(f.db.google_signin_link_for_user(&user.id).unwrap().is_some());
    assert_eq!(code(f.admin("users.signin_requests.approve", json!({"id": id})).await), "signin_request_not_pending");
    let rejected = f.provision("456", SigninApproval::Admin);
    payload(f.admin("users.signin_requests.reject", json!({"id": rejected})).await);
    assert_eq!(f.db.signin_request_by_id(&rejected).unwrap().unwrap().status, duduclaw_auth::google_signin::SigninRequestStatus::Rejected);
    f.fresh_proof();
    let mut input = settings(); input["approval_grants_mfa_enrollment"] = json!(true);
    payload(f.admin("google.signin.set", input).await);
    let allowed = f.provision("789", SigninApproval::Admin);
    payload(f.admin("users.signin_requests.approve", json!({"id": allowed, "mfa_preauthorize": true})).await);
    assert!(f.db.signin_request_by_id(&allowed).unwrap().unwrap().mfa_preauthorized);
}

#[tokio::test]
async fn preauthorize_refused_when_signin_disabled() {
    let f = fixture().await;
    f.fresh_proof();
    let mut input = settings();
    input["approval_grants_mfa_enrollment"] = json!(true);
    payload(f.admin("google.signin.set", input.clone()).await);
    f.raw().execute("DELETE FROM migration_dashboard_proofs", []).unwrap();
    input["enabled"] = json!(false);
    input.as_object_mut().unwrap().remove("step_up_csrf");
    payload(f.admin("google.signin.set", input).await);
    let stored = config::read_strict(f.home.path());
    assert!(!stored.enabled);
    assert!(stored.approval_grants_mfa_enrollment);
    let id = f.provision("123", SigninApproval::Admin);
    assert_eq!(code(f.admin("users.signin_requests.approve", json!({"id": id, "mfa_preauthorize": true})).await), "signin_preauthorize_disabled");
    let request = f.db.signin_request_by_id(&id).unwrap().unwrap();
    assert_eq!(request.status, duduclaw_auth::google_signin::SigninRequestStatus::Pending);
    assert!(!request.mfa_preauthorized);
}

#[tokio::test]
async fn attest_writes_rows_all_or_nothing_and_allows_self() {
    let f = fixture().await;
    f.fresh_proof();
    let linked = f.provision("123", SigninApproval::Direct);
    for ids in [vec![f.employee.user_id.clone(), "missing".into()], vec![f.employee.user_id.clone(); 2],
        vec![f.employee.user_id.clone(); 101], vec![f.employee.user_id.clone(), linked]] {
        assert_eq!(code(f.admin("users.google_signin.attest", json!({"user_ids": ids, "step_up_csrf": CSRF})).await), "signin_attest_invalid_target");
        let count: i64 = f.raw().query_row("SELECT COUNT(*) FROM google_signin_attestations", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
        assert!(f.db.query_audit_log(None, Some("google_signin.attested"), 100).unwrap().is_empty());
    }
    let result = payload(f.admin("users.google_signin.attest", json!({"user_ids": [f.employee.user_id, f.admin.user_id], "step_up_csrf": CSRF})).await);
    assert_eq!(result["attested"], 2);
    let (email, actor, seq): (String, String, i64) = f.raw().query_row(
        "SELECT email_at_attestation,attested_by,credential_seq FROM google_signin_attestations WHERE user_id=?1",
        [&f.employee.user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(email, f.employee.email);
    assert_eq!(actor, f.admin.user_id);
    assert!(seq >= 0);
    assert_eq!(f.db.query_audit_log(Some(&f.admin.user_id), Some("google_signin.attested"), 100).unwrap().len(), 2);
}

#[tokio::test]
async fn unlink_revokes_sessions_and_live_socket_cache() {
    let f = fixture().await;
    let id = f.provision("123", SigninApproval::Direct);
    let user = f.db.get_user(&id).unwrap().unwrap();
    let ctx = f.db.live_user_context(&id).unwrap().unwrap();
    let jwt = JwtConfig::new(b"google-signin-rpc-tests-32-byte-secret");
    let (_, refresh) = jwt.issue_session_tokens(&user, &[]).unwrap();
    let claims = jwt.verify_refresh_token(&refresh).unwrap();
    assert!(f.db.mfa_refresh_and_issue(&id, claims.credential_epoch, claims.jti.as_deref(), false, |_, _| Ok(())).is_ok());
    super::super::live_context::resolve_socket_context(&f.db, &f.handler.live_ctx_cache, &ctx, true).unwrap();
    payload(f.admin("users.google_signin.unlink", json!({"user_id": id})).await);
    assert_eq!(f.db.get_user(&id).unwrap().unwrap().credential_epoch, user.credential_epoch + 1);
    assert!(f.db.mfa_refresh_and_issue(&id, claims.credential_epoch, claims.jti.as_deref(), false, |_, _| Ok(())).is_err());
    assert!(super::super::live_context::resolve_socket_context(&f.db, &f.handler.live_ctx_cache, &ctx, false).is_err());
    assert!(f.db.google_signin_link_for_user(&id).unwrap().is_none());
    assert_eq!(code(f.admin("users.google_signin.unlink", json!({"user_id": id})).await), "signin_not_linked");
}

#[tokio::test]
async fn avatar_retry_only_from_waiting_admin() {
    let f = fixture().await;
    let id = f.provision("123", SigninApproval::Direct);
    for state in ["pending", "reserved", "creating", "done"] {
        f.raw().execute("UPDATE user_onboarding SET avatar=?2 WHERE user_id=?1", params![id, state]).unwrap();
        assert_eq!(code(f.admin("users.signin_onboarding.retry_avatar", json!({"user_id": id})).await), "signin_avatar_not_waiting");
    }
    f.raw().execute("UPDATE user_onboarding SET avatar='waiting_admin',avatar_attempts=3,avatar_error='failed' WHERE user_id=?1", [&id]).unwrap();
    payload(f.admin("users.signin_onboarding.retry_avatar", json!({"user_id": id})).await);
    let state: (String, i64) = f.raw().query_row("SELECT avatar,avatar_attempts FROM user_onboarding WHERE user_id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(state, ("pending".into(), 0));
}

#[tokio::test]
async fn google_only_account_cannot_be_admin_through_users_update() {
    let f = fixture().await;
    let id = f.provision("123", SigninApproval::Direct);
    for role in ["admin", "manager"] {
        assert_eq!(code(f.admin("users.update", json!({"user_id": id, "role": role})).await), "role_requires_local_password");
        assert_eq!(f.db.get_user(&id).unwrap().unwrap().role, UserRole::Employee);
    }
    payload(f.admin("users.update", json!({"user_id": id, "role": "employee"})).await);
    payload(f.admin("users.update", json!({"user_id": f.employee.user_id, "role": "manager"})).await);
    assert_eq!(f.db.get_user(&f.employee.user_id).unwrap().unwrap().role, UserRole::Manager);
}

#[tokio::test]
async fn me_google_signin_member_api_keys_refused_for_every_method() {
    let f = fixture().await;
    let key = f.db.member_key_create(&f.employee.user_id, "Self service", 1, f.employee.credential_epoch, false, &[]).unwrap();
    let key_ctx = crate::member_keys::authenticate(f.home.path(), &f.db, &key.secret).unwrap();
    for method in ["me.google_signin.status", "me.google_signin.link_start", "me.google_signin.link_complete",
        "me.google_signin.unlink", "me.google_signin.reauth_start", "me.google_signin.reauth_complete"] {
        assert_eq!(super::super::gates::google_signin_minimum_role(method), Some(UserRole::Employee));
        assert_eq!(super::super::member_contract::member_key_method(method), None);
        assert_eq!(code(f.handler.handle(method, json!({}), &key_ctx).await), "member_key_forbidden");
        assert_eq!(code(f.handler.handle(method, json!({}), &f.employee).await), "signin_dashboard_required");
        assert!(!duduclaw_core::mcp_tool_defs::tools().any(|tool| tool.name == method || tool.name == method.replace('.', "_")));
    }
    for ctx in [&f.admin, &f.manager, &f.employee] {
        let value = payload(f.call(ctx, "me.google_signin.status", json!({})).await);
        assert_eq!(value["has_password"], true);
        assert_eq!(value["linked"], false);
        assert!(value.get("sub").is_none());
        assert!(refused(f.call(ctx, "me.google_signin.status", json!({"user_id": f.admin.user_id})).await));
    }
    assert_eq!(super::super::member_contract::member_key_method("me.google_signin.future"), None);
    assert!(refused(f.admin("me.google_signin.future", json!({})).await));
}
