use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CapturingExtension(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl GatewayExtension for CapturingExtension {
    async fn handle_method(&self, _: &str, _: Value, _: &UserContext) -> Option<WsFrame> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(WsFrame::ok_response("", json!({"extension": true})))
    }
}

#[tokio::test]
async fn dashboard_scope_unknown_methods_are_denied_before_extension() {
    let home = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = MethodHandler::with_extension(
        home.path().to_path_buf(), Arc::new(CapturingExtension(calls.clone())),
    ).await;
    handler.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise)).await;
    for role in [UserRole::Employee, UserRole::Manager] {
        let mut ctx = UserContext::admin_fallback();
        ctx.role = role;
        for method in ["cron.future", "reminders.future", "assign_schedules.future",
            "agents.org_chart.future", "skills.recent.future"] {
            assert!(matches!(handler.handle(method, json!({}), &ctx).await,
                WsFrame::Response { ok: false, .. }), "{method}");
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(handler.handle("cron.future", json!({}), &UserContext::admin_fallback()).await,
        WsFrame::Response { ok: true, .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dashboard_scope_uses_live_role_before_extension() {
    let home = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let handler = MethodHandler::with_extension(
        home.path().to_path_buf(), Arc::new(CapturingExtension(calls.clone())),
    ).await;
    let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
    let user = db.create_user("scope@test.invalid", "scope", "test-password-123", UserRole::Employee).unwrap();
    handler.set_user_db(db, Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
    let mut stale_admin = UserContext::admin_fallback();
    stale_admin.user_id = user.id;
    assert!(matches!(handler.handle("cron.future", json!({}), &stale_admin).await,
        WsFrame::Response { ok: false, .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(handler.handle("skills.recent", json!({}), &stale_admin).await,
        WsFrame::Response { ok: true, .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
