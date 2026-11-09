//! F6k handler gates of `isolation.shared_restore_request` /
//! `isolation.shared_restore_options`. The bodies are covered over the
//! real-store fixture in `crate::isolation_restore_shared_tests`.
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

/// §3.11: dashboard-only. Owned by the employee handler, Enterprise-only,
/// refused for `system`, never an MCP tool; Admin (+ current MFA) only.
#[tokio::test]
async fn shared_restore_rpcs_not_listed_to_ai() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let guide = include_str!("../../../../docs/guides/mcp-tools.md");
    let tools: Vec<&str> = duduclaw_core::mcp_tool_defs::tools().map(|tool| tool.name).collect();
    for method in METHODS {
        assert!(super::super::super::owns(method), "{method}");
        assert!(super::super::handles(method), "{method}");
        assert!(crate::handlers::is_enterprise_only_method(method), "{method}");
        assert!(!guide.contains(method), "{method} must not be an MCP tool");
        for name in [method.to_string(), method.replace('.', "_")] {
            assert!(!tools.contains(&name.as_str()), "{name}");
            assert!(!duduclaw_core::mcp_tool_defs::EXTERNAL_TOOLS_WHITELIST.contains(&name.as_str()), "{name}");
        }
        let frame = handler.handle_isolation_employee(method, json!({}), &ctx("system", UserRole::Admin)).await;
        assert_eq!(refusal(&frame), NOT_ALLOWED, "{method}");
        for role in [UserRole::Employee, UserRole::Manager] {
            let frame = handler.handle_isolation_employee(method, json!({}), &ctx("staff", role)).await;
            assert_eq!(refusal(&frame), ADMIN_REQUIRED, "{method}");
        }
        // An Admin without a current MFA enrolment is refused at the gate.
        let frame = handler.handle_isolation_employee(method, json!({}), &ctx("admin-1", UserRole::Admin)).await;
        assert_eq!(refusal(&frame), ADMIN_REQUIRED, "{method}");
    }
}

/// The request schema: CSRF (fresh step-up in the call), members and ticked
/// routes required; no field names a requester, approver, tenant or owner.
#[test]
fn shared_restore_request_schema_has_no_authority_fields() {
    let params = json!({"employee_id": "0".repeat(32), "incarnation": 0, "members": [], "routes": [], "csrf_token": vec![7u8; 32]});
    assert!(serde_json::from_value::<SharedRestoreRequest>(params.clone()).is_ok());
    for extra in ["requester_user_id", "approver_user_id", "owner_id", "tenant_id", "step_up", "self_approved", "fresh_step_up"] {
        let mut forged = params.clone();
        forged[extra] = json!("x");
        assert!(serde_json::from_value::<SharedRestoreRequest>(forged).is_err(), "{extra}");
    }
    for missing in ["csrf_token", "members", "routes", "employee_id", "incarnation"] {
        let mut partial = params.clone();
        partial.as_object_mut().unwrap().remove(missing);
        assert!(serde_json::from_value::<SharedRestoreRequest>(partial).is_err(), "{missing} required");
    }
    assert!(serde_json::from_value::<SharedRestoreOptions>(json!({"employee_id": "x", "incarnation": 0})).is_ok());
    assert!(serde_json::from_value::<SharedRestoreOptions>(json!({"employee_id": "x", "incarnation": 0, "all": true})).is_err());
}
