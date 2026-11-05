//! Gateway-owned invitation copy and per-user first-login progress.
use super::*;
use duduclaw_auth::invitation_content::{InvitationContent, valid_locale};

pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "invitation.get" | "invitation.update" | "invitation.preview" => Some(UserRole::Admin),
        "onboarding.get" | "onboarding.update" => Some(UserRole::Employee),
        _ if method.starts_with("invitation.") || method.starts_with("onboarding.") => Some(UserRole::Admin),
        _ => None,
    }
}
fn error(code: &str) -> WsFrame {
    WsFrame::Response { id: String::new(), ok: false, payload: None,
        error: Some(json!({"code":code,"message":"目前無法完成設定，請確認內容後再試。"})) }
}
fn locale(params: &Value) -> Result<&str, WsFrame> {
    match params.get("locale") {
        None => Ok("zh-TW"),
        Some(Value::String(locale)) if valid_locale(locale) => Ok(locale),
        _ => Err(error("invalid_locale")),
    }
}

impl MethodHandler {
    pub(crate) async fn handle_invitation_content(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else { return error("invitation_unavailable"); };
        if !super::user_invites_rpc::live_admin(&db, ctx) { return error("permission_denied"); }
        let locale = match locale(&params) { Ok(locale) => locale, Err(frame) => return frame };
        let content = match (method, params.get("content")) {
            ("invitation.update" | "invitation.preview", Some(raw)) => match serde_json::from_value::<InvitationContent>(raw.clone()) {
                Ok(content) => content,
                Err(_) => return error("invalid_invitation_content"),
            },
            ("invitation.update", None) => return error("invalid_invitation_content"),
            _ => match db.invitation_content(locale) { Ok(content) => content, Err(_) => return error("invitation_unavailable") },
        };
        if let Err(code) = crate::invitation_email::validate(&content) { return error(code); }
        match method {
            "invitation.get" => WsFrame::ok_response("", json!({"locale":locale,"content":content})),
            "invitation.update" => match db.update_invitation_content(&ctx.user_id, ctx.credential_epoch, locale, &content) {
                Ok(()) => WsFrame::ok_response("", json!({"locale":locale,"content":content})),
                Err(_) => error("invitation_unavailable"),
            },
            "invitation.preview" => {
                let name = match params.get("display_name") {
                    None => "",
                    Some(Value::String(name)) if name.chars().count() <= 200 => name,
                    _ => return error("invalid_display_name"),
                };
                // No token is issued for preview and no request host is trusted.
                match crate::user_invites::render_invitation(&self.home_dir, &content, locale, name, "https://example.invalid/invite#preview", None) {
                    Ok(rendered) => WsFrame::ok_response("", json!(rendered)),
                    Err(code) => error(code),
                }
            },
            _ => error("unknown_method"),
        }
    }

    pub(crate) async fn handle_onboarding(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else { return error("onboarding_unavailable"); };
        let locale = match locale(&params) { Ok(locale) => locale.to_owned(), Err(frame) => return frame };
        // Check the current person before reading their connection metadata.
        let mut progress = match db.onboarding_progress(&ctx.user_id, ctx.credential_epoch) {
            Ok(progress) => progress,
            Err(_) => return error("permission_denied"),
        };
        if method == "onboarding.update" {
            let mut change = params.clone();
            if let Some(object) = change.as_object_mut() { object.remove("locale"); }
            let update = match serde_json::from_value::<duduclaw_auth::onboarding::ProgressUpdate>(change) {
                Ok(update) => update,
                Err(_) => return error("invalid_onboarding_update"),
            };
            progress = match db.update_onboarding(&ctx.user_id, ctx.credential_epoch, &update) {
                Ok(progress) => progress,
                Err(code) => return error(&code),
            };
        } else if method != "onboarding.get" { return error("unknown_method"); }
        // Hidden cards need no connection-vault reads or invitation copy.
        if !progress.enabled || progress.dismissed {
            let mut payload = json!(progress);
            payload["examples"] = json!([]);
            return WsFrame::ok_response("", payload);
        }
        let google = match crate::person_credentials::get(&self.home_dir, &ctx.user_id) {
            Ok(Some(record)) => record.metadata.status == crate::person_credentials::Status::Connected,
            Ok(None) => false,
            Err(_) => return error("onboarding_unavailable"),
        };
        if db.observe_onboarding_connections(&ctx.user_id, ctx.credential_epoch, google).is_err() { return error("onboarding_unavailable"); }
        let progress = match db.onboarding_progress(&ctx.user_id, ctx.credential_epoch) { Ok(progress) => progress, Err(_) => return error("onboarding_unavailable") };
        let content = match db.invitation_content(&locale) { Ok(content) => content, Err(_) => return error("onboarding_unavailable") };
        let mut payload = json!(progress);
        payload["examples"] = json!(content.examples);
        WsFrame::ok_response("", payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (tempfile::TempDir, MethodHandler, Arc<UserDb>, UserContext) {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@test.invalid", "Admin", "password123", UserRole::Admin).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
        handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
        let mut ctx = UserContext::admin_fallback();
        ctx.user_id = admin.id;
        ctx.email = admin.email;
        ctx.credential_epoch = admin.credential_epoch;
        (home, handler, db, ctx)
    }
    fn refused(frame: WsFrame) -> bool { matches!(frame, WsFrame::Response { ok: false, .. }) }
    fn payload(frame: WsFrame) -> Value {
        match frame { WsFrame::Response { ok: true, payload: Some(v), .. } => v, _ => panic!("expected success") }
    }
    #[test]
    fn onboarding_rpc_scopes_are_explicit() {
        for name in ["invitation.get", "invitation.update", "invitation.preview"] { assert_eq!(minimum_role(name), Some(UserRole::Admin)); }
        for name in ["onboarding.get", "onboarding.update"] { assert_eq!(minimum_role(name), Some(UserRole::Employee)); }
        for name in ["onboarding.unknown", "invitation.unknown"] {
            assert_eq!(minimum_role(name), Some(UserRole::Admin));
        }
    }
    struct NamespaceCollision(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl crate::extension::GatewayExtension for NamespaceCollision {
        fn rpc_methods(&self) -> Vec<String> { vec!["ext.duduclaw.control".into()] }
        async fn handle_method(&self, _method: &str, _params: Value, _ctx: &UserContext) -> Option<WsFrame> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(WsFrame::ok_response("", json!({"extension":true})))
        }
    }
    #[tokio::test]
    async fn reserved_namespace_collisions_never_reach_extensions() {
        let (_home, mut handler, db, admin) = fixture().await;
        let extension = Arc::new(NamespaceCollision(std::sync::atomic::AtomicUsize::new(0)));
        handler.extension = crate::extension::register(extension.clone());
        for role in [UserRole::Employee, UserRole::Manager, UserRole::Admin] {
            let user = db.create_user(&format!("collision-{role}@test.invalid"), "User", "password123", role).unwrap();
            let ctx = db.live_user_context(&user.id).unwrap().unwrap();
            for method in ["invitation.unknown", "onboarding.unknown", "members.unknown", "users.create_members_unknown", "users.invite_unknown"] {
                assert!(refused(handler.handle(method, json!({}), &ctx).await), "{role}: {method}");
            }
            if role != UserRole::Admin {
                for method in ["users.create_members", "users.invite", "users.invite_status", "invitation.get", "invitation.update", "invitation.preview"] {
                    assert!(refused(handler.handle(method, json!({}), &ctx).await), "{role}: {method}");
                }
            }
        }
        assert_eq!(extension.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(payload(handler.handle("ext.duduclaw.control", json!({}), &admin).await)["extension"], true);
        assert_eq!(extension.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn invitation_settings_reject_non_admin_and_unsafe_course_and_preview_issues_no_token() {
        let (home, handler, db, admin) = fixture().await;
        let user = db.create_user("user@test.invalid", "User", "password123", UserRole::Employee).unwrap();
        let mut forged = admin.clone(); forged.user_id = user.id; forged.credential_epoch = user.credential_epoch;
        for method in ["invitation.get", "invitation.update", "invitation.preview"] {
            assert!(refused(handler.handle(method, json!({}), &forged).await));
        }
        let mut content = InvitationContent::defaults("en"); content.course_url = "http://example.test".into();
        assert!(refused(handler.handle("invitation.update", json!({"locale":"en","content":content}), &admin).await));
        let preview = payload(handler.handle("invitation.preview", json!({"locale":"en","display_name":"<b>User</b>"}), &admin).await);
        assert!(preview["html"].as_str().unwrap().contains("&lt;b&gt;User&lt;/b&gt;"));
        let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM user_invites", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }
    #[tokio::test]
    async fn onboarding_rpc_legacy_account_is_disabled_without_connection_reads() {
        let (_home, handler, _db, admin) = fixture().await;
        let result = payload(handler.handle("onboarding.get", json!({}), &admin).await);
        assert_eq!(result["enabled"], false);
        assert_eq!(result["examples"], json!([]));
    }
    #[tokio::test]
    async fn onboarding_rpc_binds_progress_to_live_caller() {
        let (home, handler, db, admin) = fixture().await;
        let user = db.create_user("user@test.invalid", "User", "password123", UserRole::Employee).unwrap();
        let mut ctx = admin.clone(); ctx.user_id = user.id.clone(); ctx.email = user.email; ctx.role = UserRole::Employee; ctx.credential_epoch = user.credential_epoch;
        let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        conn.execute("INSERT INTO user_onboarding(user_id) VALUES(?1)", [&ctx.user_id]).unwrap();
        assert!(refused(handler.handle("onboarding.update", json!({"user_id":admin.user_id,"dismissed":true}), &ctx).await));
        let result = payload(handler.handle("onboarding.update", json!({"step":"example","status":"done","locale":"en"}), &ctx).await);
        assert_eq!(result["steps"]["example"], "done");
        assert!(!db.onboarding_progress(&admin.user_id, admin.credential_epoch).unwrap().enabled);
        ctx.credential_epoch += 1;
        assert!(refused(handler.handle("onboarding.get", json!({}), &ctx).await));
    }
}
