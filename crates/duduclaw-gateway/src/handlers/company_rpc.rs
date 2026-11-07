//! Admin-only customer identity. Product branding stays in the signed feed.
use super::*;
use duduclaw_security::audit::{AuditEvent, Severity};

pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method { "company.get" | "company.update" => Some(UserRole::Admin), _ => None }
}

pub(crate) fn current_admin(home: &Path, ctx: &UserContext) -> Result<(), String> {
    if ctx.must_change_password || !ctx.is_admin() { return Err("只有管理員可以設定公司資訊。".into()); }
    let live = crate::computer_use_sessions::live_ops::live_identity(home, ctx);
    if live.is_some_and(|user| user.is_admin() && !user.must_change_password) { Ok(()) }
    else { Err("只有管理員可以設定公司資訊。".into()) }
}

pub(crate) fn audit(home: &Path, actor: &str, method: &str, status: &str) {
    crate::security_autopilot::audit_and_emit(home, &AuditEvent::new(
        "company_info", "company", Severity::Info,
        json!({"actor_user_id":actor,"method":method,"status":status}),
    ));
}

impl MethodHandler {
    pub(crate) async fn handle_company(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let home = self.home_dir.clone();
        let actor = ctx.clone();
        let method = method.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let result = (|| {
                if minimum_role(&method).is_none() { return Err("不支援此公司資訊操作。".into()); }
                current_admin(&home, &actor)?;
                match method.as_str() {
                    "company.get" => crate::company_info::read(&home),
                    "company.update" => {
                        let input = serde_json::from_value::<crate::company_info::CompanyUpdate>(params)
                            .map_err(|_| "公司資訊格式不正確。".to_owned())?;
                        crate::company_info::update(&home, input, || current_admin(&home, &actor))
                    }
                    _ => unreachable!(),
                }
            })();
            audit(&home, &actor.user_id, &method, if result.is_ok() { "ok" } else { "refused" });
            result
        }).await;
        match result {
            Ok(Ok(info)) => WsFrame::ok_response("", json!(info)),
            Ok(Err(error)) => WsFrame::error_response("", &error),
            Err(_) => WsFrame::error_response("", "公司資訊操作中斷，請稍後再試。"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn refused(frame: WsFrame) -> bool { matches!(frame, WsFrame::Response { ok:false, .. }) }
    #[test]
    fn company_rpc_scope_is_explicit_admin_and_unknown_denied() {
        for method in ["company.get", "company.update"] { assert_eq!(minimum_role(method), Some(UserRole::Admin)); }
        assert_eq!(minimum_role("company.reset"), None);
    }
    #[tokio::test]
    async fn company_rpc_refuses_non_admin_forged_and_demoted_admin() {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@example.test", "Admin", "password-test", UserRole::Admin).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
        let mut ctx = UserContext::admin_fallback();
        ctx.user_id = admin.id.clone();
        ctx.credential_epoch = admin.credential_epoch;
        assert!(!refused(handler.handle("company.update", json!({"name":"範例"}), &ctx).await));
        assert!(refused(handler.handle("company.update", json!({"name":"forged", "logo_base64":"aGVsbG8="}), &ctx).await));
        assert!(!home.path().join("company_logos").exists());
        db.update_user(&admin.id, None, Some(UserRole::Employee), None).unwrap();
        for method in ["company.get", "company.update"] {
            assert!(refused(handler.handle(method, json!({"name":"forged"}), &ctx).await));
            ctx.role = UserRole::Employee;
            assert!(refused(handler.handle(method, json!({"name":"forged"}), &ctx).await));
            ctx.role = UserRole::Admin;
        }
        assert_eq!(crate::company_info::read(home.path()).unwrap().name, "範例");
        assert!(refused(handler.handle("company.unknown", json!({}), &UserContext::admin_fallback()).await));
    }
}
