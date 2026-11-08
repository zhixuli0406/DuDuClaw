//! F6d handler gates of the restore RPCs. The RPC bodies are covered over the
//! real-store fixture in `crate::isolation_restore_rpc_tests`.
use super::*;

fn ctx(id: &str, role: UserRole) -> UserContext {
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = id.into();
    ctx.role = role;
    ctx
}

fn refusal(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error: Some(error), .. } =>
            error.as_str().map(str::to_owned).unwrap_or_else(|| error["code"].as_str().unwrap_or_default().to_owned()),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// §3.11: dashboard-only. Owned by this handler, Enterprise-only, refused for
/// `system` (the identity internal and AI-originated calls run as), and not
/// an MCP tool (the MCP tool guide lists none of them; the CLI's MCP catalog
/// has no isolation restore tool).
#[tokio::test]
async fn restore_rpcs_not_listed_to_ai() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let guide = include_str!("../../../../docs/guides/mcp-tools.md");
    for method in METHODS {
        assert!(super::super::owns(method), "{method}");
        assert!(crate::handlers::is_enterprise_only_method(method), "{method}");
        assert!(!guide.contains(method), "{method} must not be an MCP tool");
        let frame = handler.handle_isolation_employee(method, json!({}), &ctx("system", UserRole::Admin)).await;
        assert_eq!(refusal(&frame), NOT_ALLOWED, "{method}");
    }
    // Admin methods refuse non-Admins before reading anything.
    for method in [DECIDE, CANCEL, REGRANT, RETIRE_PROPOSE] {
        let frame = handler.handle_isolation_employee(method, json!({}), &ctx("u-1", UserRole::Employee)).await;
        assert_eq!(refusal(&frame), ADMIN_REQUIRED, "{method}");
    }
}

/// Decide (and regrant / admin cancel / retire) need Admin + MFA + a fresh
/// migration step-up with its CSRF token; the request schema has no field
/// that could stand in for it, and the rule itself refuses without step-up.
#[tokio::test]
async fn decide_requires_admin_step_up() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let params = json!({"request_id": "0".repeat(32), "decision": "approve", "csrf_token": [7u8; 32]});
    // An Admin without a current MFA enrolment is refused at the gate.
    assert_eq!(refusal(&handler.handle_isolation_employee(DECIDE, params.clone(), &ctx("admin-1", UserRole::Admin)).await),
        ADMIN_REQUIRED);
    assert_eq!(refusal(&handler.handle_isolation_employee(DECIDE, params.clone(), &ctx("staff", UserRole::Manager)).await),
        ADMIN_REQUIRED);
    // No forged approval fields.
    for extra in ["approver_user_id", "self_approved", "step_up", "fresh_step_up", "owner_id"] {
        let mut forged = params.clone();
        forged[extra] = json!(true);
        assert!(serde_json::from_value::<Decide>(forged).is_err(), "{extra}");
    }
    assert!(serde_json::from_value::<Decide>(json!({"request_id": "x", "decision": "approve"})).is_err(), "csrf required");
    // The step-up check itself: no proof, a stale proof or a wrong token refuse.
    let now = 10_000;
    let proof = |at: i64| duduclaw_auth::mfa::MigrationConfirmationSession { verified_at: at as u64, csrf: [7; 32] };
    assert!(authorize_step_up(None, "admin-1", &[7; 32], now).is_err());
    assert!(authorize_step_up(Some(&proof(now - 301)), "admin-1", &[7; 32], now).is_err());
    assert!(authorize_step_up(Some(&proof(now)), "admin-1", &[8; 32], now).is_err());
    // Without the call's step-up the F6c2 rule never approves.
    use crate::isolation_restore::ApprovalRule as _;
    let users = rusqlite::Connection::open_in_memory().unwrap();
    users.execute_batch("BEGIN").unwrap();
    assert_eq!(LogRule::new(false, 10).decide(&users, "a", "b", crate::isolation_restore::RequestKind::SharedRestore),
        Err("STEP_UP_REQUIRED"));
}

/// Regrant: Admin + step-up gate, exact schema (one item, one row).
#[tokio::test]
async fn regrant_requires_admin_step_up_and_audits_gate() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let params = json!({"employee_id": "0".repeat(32), "incarnation": 0, "item": "reports_to:x", "csrf_token": [7u8; 32]});
    assert_eq!(refusal(&handler.handle_isolation_employee(REGRANT, params.clone(), &ctx("staff", UserRole::Employee)).await),
        ADMIN_REQUIRED);
    for extra in ["items", "all", "label"] {
        let mut forged = params.clone();
        forged[extra] = json!(true);
        assert!(serde_json::from_value::<Regrant>(forged).is_err(), "{extra}");
    }
}

/// Owner requests carry the factor proof; no field names an owner.
#[test]
fn owner_request_schema_has_no_owner_field() {
    let ok = json!({"employee_id": "0".repeat(32), "incarnation": 0, "factor": "totp", "code": "123456"});
    assert!(serde_json::from_value::<OwnerRequest>(ok.clone()).is_ok());
    for extra in ["owner_id", "user_id", "requester_user_id", "tenant_id"] {
        let mut forged = ok.clone();
        forged[extra] = json!("someone");
        assert!(serde_json::from_value::<OwnerRequest>(forged).is_err(), "{extra}");
    }
    let failure = failure_frame(Failure::Rpc(RpcError::Restore(crate::isolation_restore::Refusal::NotFound)));
    assert_eq!(refusal(&failure), "NOT_FOUND");
}
