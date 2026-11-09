//! F6i handler gates of `isolation.quarantine_purge`. The engine and the RPC
//! body are covered over the real-store fixture in `crate::isolation_purge_tests`.
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

/// §3.11: dashboard-only. Owned by this handler, Enterprise-only, refused
/// for `system` (internal and AI-originated calls), never an MCP tool (tool
/// registry, external whitelist, tool guide); non-Admins and an Admin without
/// a current MFA enrolment are refused before anything is read.
#[tokio::test]
async fn quarantine_purge_not_listed_to_ai_and_admin_gated() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let guide = include_str!("../../../../docs/guides/mcp-tools.md");
    let tools: Vec<&str> = duduclaw_core::mcp_tool_defs::tools().map(|tool| tool.name).collect();
    let params = json!({"employee_id": "0".repeat(32), "incarnation": 0, "csrf_token": vec![7u8; 32]});
    for method in METHODS {
        assert!(super::super::owns(method), "{method}");
        assert!(crate::handlers::is_enterprise_only_method(method), "{method}");
        assert!(!guide.contains(method), "{method} must not be an MCP tool");
        for name in [method.to_string(), method.replace('.', "_")] {
            assert!(!tools.contains(&name.as_str()), "{name} must not be an MCP tool");
            assert!(!duduclaw_core::mcp_tool_defs::EXTERNAL_TOOLS_WHITELIST.contains(&name.as_str()), "{name}");
        }
        let frame = handler.handle_isolation_employee(method, params.clone(), &ctx("system", UserRole::Admin)).await;
        assert_eq!(refusal(&frame), NOT_ALLOWED, "{method}");
        for role in [UserRole::Employee, UserRole::Manager] {
            let frame = handler.handle_isolation_employee(method, params.clone(), &ctx("staff", role)).await;
            assert_eq!(refusal(&frame), ADMIN_REQUIRED, "{method}");
        }
        let frame = handler.handle_isolation_employee(method, params.clone(), &ctx("admin-1", UserRole::Admin)).await;
        assert_eq!(refusal(&frame), ADMIN_REQUIRED, "{method}: no current MFA");
    }
}

/// The request names the row and the dialog's two choices only; the
/// retire box is checked by default, the early purge is never implied, and
/// no field can stand in for a requester, an approver, an age or a step-up.
#[test]
fn quarantine_purge_schema() {
    let ok = json!({"employee_id": "0".repeat(32), "incarnation": 0, "csrf_token": vec![7u8; 32]});
    let parsed: QuarantinePurge = serde_json::from_value(ok.clone()).unwrap();
    assert!(!parsed.confirm_early, "an early purge is never implied");
    assert!(parsed.retire, "同時釋放主機資源 is checked by default");
    let mut chosen = ok.clone();
    chosen["confirm_early"] = json!(true);
    chosen["retire"] = json!(false);
    let parsed: QuarantinePurge = serde_json::from_value(chosen).unwrap();
    assert!(parsed.confirm_early && !parsed.retire);
    for extra in ["requester_user_id", "approver_user_id", "owner_id", "self_approved", "step_up", "quarantined_at", "now",
        "purge_now", "label"] {
        let mut forged = ok.clone();
        forged[extra] = json!("x");
        assert!(serde_json::from_value::<QuarantinePurge>(forged).is_err(), "{extra}");
    }
    for missing in ["csrf_token", "employee_id", "incarnation"] {
        let mut partial = ok.clone();
        partial.as_object_mut().unwrap().remove(missing);
        assert!(serde_json::from_value::<QuarantinePurge>(partial).is_err(), "{missing} required");
    }
    // Refusals are `{code, message}` objects.
    let frame = failure_frame(Failure::Rpc(RpcError::from(crate::isolation_purge::PurgeError::ConfirmEarly)));
    assert_eq!(refusal(&frame), "EARLY_PURGE_CONFIRM_REQUIRED");
    let frame = failure_frame(Failure::Rpc(RpcError::from(crate::isolation_purge::PurgeError::Restore(Refusal::NotFound))));
    assert_eq!(refusal(&frame), "NOT_FOUND");
}
