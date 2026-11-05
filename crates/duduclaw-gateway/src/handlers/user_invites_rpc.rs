//! Admin-only invitation lifecycle. No token, link, hash or transport error is logged.
use super::*;

/// Explicit Admin scope registry for gateway-owned invitation RPCs.
pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        _ if method.starts_with("users.invite") => Some(UserRole::Admin),
        _ => None,
    }
}

pub(super) fn invite_error(code: &str) -> WsFrame {
    let message = match code {
        "public_url_required" => "請先設定對外網址，再寄送邀請。",
        "invalid_public_url" => "對外網址格式不正確。",
        _ => "目前無法建立邀請，請確認帳號狀態後再試。",
    };
    WsFrame::Response { id: String::new(), ok: false, payload: None,
        error: Some(json!({"code":code,"message":message})) }
}

/// Refuse synthetic/legacy Admin contexts: the invitation must name a live
/// human Admin, including when an internal caller bypasses normal dispatch.
pub(super) fn live_admin(db: &UserDb, ctx: &UserContext) -> bool {
    if ctx.role != UserRole::Admin || ctx.must_change_password { return false; }
    matches!(db.get_user(&ctx.user_id), Ok(Some(user))
        if user.role == UserRole::Admin && user.status == duduclaw_auth::UserStatus::Active
        && !user.must_change_password && user.credential_epoch == ctx.credential_epoch)
}

impl MethodHandler {
    pub(crate) async fn invitation_base(&self, params: &Value) -> Result<String, WsFrame> {
        if params.get("public_url").is_some_and(|v| !v.is_string()) {
            return Err(invite_error("invalid_public_url"));
        }
        crate::user_invites::base_from_home(&self.home_dir, params.get("public_url").and_then(Value::as_str))
            .await.map_err(invite_error)
    }

    pub(crate) async fn handle_user_invites(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else { return invite_error("invite_unavailable"); };
        if !live_admin(&db, ctx) { return WsFrame::error_response("", "permission denied"); }
        if method == "users.invite_status" {
            let users = match db.list_users() { Ok(users) => users, Err(_) => return invite_error("invite_unavailable") };
            let mut statuses = serde_json::Map::new();
            for user in users {
                match db.user_invite_status(&user.id) {
                    Ok(state) => { statuses.insert(user.id, json!(state)); }
                    Err(_) => return invite_error("invite_unavailable"),
                }
            }
            return WsFrame::ok_response("", json!({"statuses":statuses}));
        }
        if method != "users.invite" { return invite_error("invite_unavailable"); }
        let Some(user_id) = params.get("user_id").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 128) else {
            return invite_error("invite_target_invalid");
        };
        let base = match self.invitation_base(&params).await { Ok(base) => base, Err(frame) => return frame };
        match self.create_and_send_invitation(&db, user_id, ctx, &base).await {
            Ok(payload) => WsFrame::ok_response("", payload),
            Err(frame) => frame,
        }
    }

    pub(crate) async fn create_and_send_invitation(&self, db: &Arc<UserDb>, user_id: &str, ctx: &UserContext, base: &str) -> Result<Value, WsFrame> {
        if !live_admin(db, ctx) { return Err(WsFrame::error_response("", "permission denied")); }
        let base = crate::user_invites::public_base_url(Some(base), None).map_err(invite_error)?;
        let work = db.clone();
        let id = user_id.to_owned();
        let actor = ctx.user_id.clone();
        let epoch = ctx.credential_epoch;
        let issued = tokio::task::spawn_blocking(move || work.issue_user_invite_at_epoch(&id, &actor, epoch)).await;
        let issued = match issued { Ok(Ok(issued)) => issued, _ => return Err(invite_error("invite_target_invalid")) };
        self.deliver_user_invitation(db, issued, ctx, &base, "zh-TW").await
    }

    pub(crate) async fn deliver_user_invitation(&self, db: &Arc<UserDb>, issued: duduclaw_auth::user_invites::IssuedUserInvite, ctx: &UserContext, base: &str, locale: &str) -> Result<Value, WsFrame> {
        if !live_admin(db, ctx) { return Err(WsFrame::error_response("", "permission denied")); }
        let user_id = issued.user_id.clone();
        // Re-read eligibility before delivery; the recipient itself was read
        // in the issuance transaction so a concurrent email edit cannot redirect it.
        if !matches!(db.user_invite_email(&issued.token), Ok(Some(_))) {
            return Err(invite_error("invite_unavailable"));
        }
        let email = issued.email;
        let link = format!("{base}/invite#{}", issued.token);
        let content = db.invitation_content(locale).map_err(|_| invite_error("invite_content_unavailable"))?;
        let name = db.get_user(&user_id).ok().flatten().map(|u| u.display_name).unwrap_or_default();
        let rendered = crate::invitation_email::render(&content, locale, &crate::distributor_brand::product_name(), &name, &link)
            .map_err(invite_error)?;
        let payload = crate::user_invites::send_rendered(&self.home_dir, &email, &link, &rendered).await;
        let action = if payload["sent"] == true { "user_invite.sent" } else { "user_invite.failed" };
        let _ = db.log_action(Some(&ctx.user_id), action, Some(&user_id), None, None);
        // Recheck before exposing a capability after an asynchronous delivery.
        if !live_admin(db, ctx) { return Err(WsFrame::error_response("", "permission denied")); }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, MethodHandler, Arc<UserDb>, UserContext, String) {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@test.invalid", "Admin", "password123", UserRole::Admin).unwrap();
        let teammate = db.create_invited_user_with(&duduclaw_auth::NewUser {
            email: "teammate@test.invalid", display_name: "Teammate", password: "ignored",
            role: UserRole::Employee, department: None, manager: None,
        }).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
        handler.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise)).await;
        let mut ctx = UserContext::admin_fallback();
        ctx.user_id = admin.id;
        ctx.email = admin.email;
        ctx.credential_epoch = admin.credential_epoch;
        (home, handler, db, ctx, teammate.id)
    }
    fn payload(frame: WsFrame) -> Value {
        match frame { WsFrame::Response { ok: true, payload: Some(value), .. } => value,
            _ => panic!("expected successful invitation response") }
    }
    fn refused(frame: WsFrame) -> bool {
        matches!(frame, WsFrame::Response { ok: false, .. })
    }

    #[test]
    fn invitation_scope_is_explicit_and_admin_only() {
        assert_eq!(minimum_role("users.invite"), Some(UserRole::Admin));
        assert_eq!(minimum_role("users.invite_status"), Some(UserRole::Admin));
        assert_eq!(minimum_role("users.invite_unknown"), Some(UserRole::Admin));
    }

    #[tokio::test]
    async fn admin_can_copy_failed_delivery_and_reissue_revokes_previous_link() {
        let (_home, handler, db, admin, teammate) = fixture().await;
        let params = json!({"user_id":teammate,"public_url":"https://trusted.test"});
        let first = payload(handler.handle("users.invite", params.clone(), &admin).await);
        assert_eq!(first["sent"], false);
        let token = first["link"].as_str().unwrap().split_once('#').unwrap().1;
        assert!(db.user_invite_email(token).unwrap().is_some());
        let _second = payload(handler.handle("users.invite", params, &admin).await);
        assert!(db.user_invite_email(token).unwrap().is_none());
        let statuses = payload(handler.handle("users.invite_status", json!({}), &admin).await);
        assert_eq!(statuses["statuses"][&teammate], "pending");
        assert!(!statuses.to_string().contains("link"));
    }

    #[tokio::test]
    async fn insecure_urls_and_password_bearing_accounts_are_refused() {
        let (home, handler, db, admin, teammate) = fixture().await;
        for base in ["http://example.com", "http://localhost.evil.com"] {
            assert!(refused(handler.handle("users.invite", json!({"user_id":teammate,"public_url":base}), &admin).await));
        }
        assert_eq!(db.user_invite_status(&teammate).unwrap(), "none");
        db.update_user(&teammate, None, None, Some("existing-password")).unwrap();
        let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        conn.execute("UPDATE users SET must_change_password=1 WHERE id=?1", [&teammate]).unwrap();
        assert!(refused(handler.handle("users.invite", json!({"user_id":teammate,"public_url":"https://trusted.test"}), &admin).await));
        assert_eq!(db.user_invite_status(&teammate).unwrap(), "none");
    }

    #[tokio::test]
    async fn non_admin_and_forged_admin_are_refused_without_invitation() {
        let (_home, handler, db, admin, teammate) = fixture().await;
        let employee = db.create_user("employee@test.invalid", "Employee", "password123", UserRole::Employee).unwrap();
        let mut forged = admin.clone();
        forged.user_id = employee.id;
        forged.credential_epoch = employee.credential_epoch;
        for method in ["users.invite", "users.invite_status"] {
            assert!(refused(handler.handle(method, json!({"user_id":teammate,"public_url":"https://trusted.test"}), &forged).await));
        }
        assert_eq!(db.user_invite_status(&teammate).unwrap(), "none");
    }

    #[tokio::test]
    async fn invitation_create_requires_base_before_creating_user_and_never_keeps_supplied_password() {
        let (_home, handler, db, admin, _) = fixture().await;
        let params = json!({"invite":true,"email":"new@test.invalid","display_name":"New","password":"should-not-work"});
        let before = db.list_users().unwrap().len();
        assert!(refused(handler.handle("users.create", params.clone(), &admin).await));
        assert_eq!(db.list_users().unwrap().len(), before);
        let mut params = params;
        params["public_url"] = json!("https://trusted.test");
        let created = payload(handler.handle("users.create", params, &admin).await);
        assert_eq!(created["invite"]["sent"], false);
        assert!(db.verify_password("new@test.invalid", "should-not-work").is_err());
        assert!(created["invite"]["link"].is_string());
    }
}
