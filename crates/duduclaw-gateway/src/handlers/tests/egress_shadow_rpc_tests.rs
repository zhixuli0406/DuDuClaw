use super::*;

#[tokio::test]
async fn shadow_report_requires_admin_and_valid_days() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().into()).await;
    let mut caller = UserContext::admin_fallback();
    for role in [UserRole::Employee, UserRole::Manager] {
        caller.role = role;
        let frame = handler.handle("security.egress_shadow_report", json!({"days":7}), &caller).await;
        assert!(matches!(frame, WsFrame::Response { error: Some(_), .. }));
    }
    caller.role = UserRole::Admin;
    for params in [json!({}), json!({"days":8}), json!({"days":"7"}), json!({"days":4294967303u64})] {
        let frame = handler.handle("security.egress_shadow_report", params, &caller).await;
        assert!(matches!(frame, WsFrame::Response { error: Some(_), .. }));
    }
    for days in [7,30] {
        let frame = handler.handle("security.egress_shadow_report", json!({"days":days}), &caller).await;
        assert!(matches!(frame, WsFrame::Response { error: None, .. }));
    }
}
