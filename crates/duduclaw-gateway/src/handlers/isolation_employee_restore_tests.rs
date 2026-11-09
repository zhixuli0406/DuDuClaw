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
    for method in [DECIDE, CANCEL, REGRANT, RETIRE_PROPOSE, SHARED_ADOPTION_REQUEST, RESTORE_FOR_OWNER, ADOPTION_FOR_OWNER] {
        let frame = handler.handle_isolation_employee(method, json!({}), &ctx("u-1", UserRole::Employee)).await;
        assert_eq!(refusal(&frame), ADMIN_REQUIRED, "{method}");
    }
}

/// F6fa (review item 8, verified): the restore RPCs and the owner's own
/// credential writers (`users.change_password`, `users.change_email`) are
/// unreachable from an AI employee. AI employees act only through MCP tools,
/// which the CLI implements itself (no tool forwards a dashboard RPC method);
/// none of these names is an MCP tool (registry, external whitelist, tool
/// guide); and the identity internal / legacy-token connections run as
/// (`system`) is refused by every one of them. The on-behalf-of context
/// (`on_behalf_of/`) only attributes MCP tool and bus dispatch to a human; it
/// never opens a dashboard session.
#[tokio::test]
async fn f6fa_restore_and_credential_rpcs_unreachable_from_ai() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let guide = include_str!("../../../../docs/guides/mcp-tools.md");
    let tools: Vec<&str> = duduclaw_core::mcp_tool_defs::tools().map(|tool| tool.name).collect();
    let credentials = ["users.change_password", "users.change_email"];
    for method in METHODS.iter().chain(credentials.iter()) {
        for name in [method.to_string(), method.replace('.', "_")] {
            assert!(!tools.contains(&name.as_str()), "{name} must not be an MCP tool");
            assert!(!duduclaw_core::mcp_tool_defs::EXTERNAL_TOOLS_WHITELIST.contains(&name.as_str()), "{name}");
        }
        assert!(!guide.contains(method), "{method} must not be documented as an MCP tool");
    }
    for method in METHODS {
        let frame = handler.handle_isolation_employee(method, json!({}), &ctx("system", UserRole::Admin)).await;
        assert_eq!(refusal(&frame), NOT_ALLOWED, "{method}");
    }
    let system = ctx("system", UserRole::Admin);
    let frame = handler.handle_users_change_password(json!({"current_password": "x", "new_password": "long-enough-1"}), &system).await;
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "users.change_password as system: {frame:?}");
    let frame = handler.handle_users_change_email(json!({"email": "x@example.test", "current_password": "x"}), &system).await;
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "users.change_email as system: {frame:?}");
}

/// Decide (and regrant / admin cancel / retire) need Admin + MFA + a fresh
/// migration step-up with its CSRF token; the request schema has no field
/// that could stand in for it, and the rule itself refuses without step-up.
#[tokio::test]
async fn decide_requires_admin_step_up() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let csrf = vec![7u8; 32];
    let params = json!({"request_id": "0".repeat(32), "decision": "approve", "csrf_token": csrf});
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
    let csrf = vec![7u8; 32];
    let params = json!({"employee_id": "0".repeat(32), "incarnation": 0, "item": "reports_to:x", "csrf_token": csrf});
    assert_eq!(refusal(&handler.handle_isolation_employee(REGRANT, params.clone(), &ctx("staff", UserRole::Employee)).await),
        ADMIN_REQUIRED);
    for extra in ["items", "all", "label"] {
        let mut forged = params.clone();
        forged[extra] = json!(true);
        assert!(serde_json::from_value::<Regrant>(forged).is_err(), "{extra}");
    }
}

/// `isolation.shared_adoption_request` (F6g): Admin + current MFA at the
/// gate, then the migration step-up with its CSRF token in the call; no
/// request field names the requester, an approver or an owner.
#[tokio::test]
async fn shared_adoption_request_admin_step_up() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let csrf = vec![7u8; 32];
    let params = json!({"employee_id": "0".repeat(32), "incarnation": 0, "members": [], "csrf_token": csrf});
    assert!(super::super::owns(SHARED_ADOPTION_REQUEST));
    for role in [UserRole::Employee, UserRole::Manager] {
        assert_eq!(refusal(&handler.handle_isolation_employee(SHARED_ADOPTION_REQUEST, params.clone(), &ctx("staff", role)).await),
            ADMIN_REQUIRED);
    }
    // An Admin without a current MFA enrolment is refused at the gate.
    assert_eq!(refusal(&handler.handle_isolation_employee(SHARED_ADOPTION_REQUEST, params.clone(), &ctx("admin-1", UserRole::Admin)).await),
        ADMIN_REQUIRED);
    assert_eq!(refusal(&handler.handle_isolation_employee(SHARED_ADOPTION_REQUEST, params.clone(), &ctx("system", UserRole::Admin)).await),
        NOT_ALLOWED);
    assert!(serde_json::from_value::<SharedAdoption>(params.clone()).is_ok());
    let mut with_target = params.clone();
    with_target["target_label"] = json!("team-y");
    assert!(serde_json::from_value::<SharedAdoption>(with_target).is_ok());
    for extra in ["requester_user_id", "approver_user_id", "owner_id", "tenant_id", "step_up", "self_approved"] {
        let mut forged = params.clone();
        forged[extra] = json!("x");
        assert!(serde_json::from_value::<SharedAdoption>(forged).is_err(), "{extra}");
    }
    for missing in ["csrf_token", "members"] {
        let mut partial = params.clone();
        partial.as_object_mut().unwrap().remove(missing);
        assert!(serde_json::from_value::<SharedAdoption>(partial).is_err(), "{missing} required");
    }
    // The step-up itself: none, stale or a wrong token refuse.
    let now = 10_000;
    let proof = |at: i64| duduclaw_auth::mfa::MigrationConfirmationSession { verified_at: at as u64, csrf: [7; 32] };
    assert!(authorize_step_up(None, "admin-1", &[7; 32], now).is_err());
    assert!(authorize_step_up(Some(&proof(now - 301)), "admin-1", &[7; 32], now).is_err());
    assert!(authorize_step_up(Some(&proof(now)), "00000000-0000-4000-8000-000000000001", &[8; 32], now).is_err());
}

/// F6fa2 (owner decision 2026-11-09): the Admin filing RPCs are
/// dashboard-only, Admin + current MFA at the gate (non-Admins and `system`
/// refused before anything is read), then the step-up with its CSRF token in
/// the call; no field names an owner, a requester, an approver or a flag.
#[tokio::test]
async fn f6fa2_admin_filing_rpcs_gated() {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let handler = MethodHandler::new(dir.path().to_path_buf()).await;
    let csrf = vec![7u8; 32];
    let params = json!({"employee_id": "0".repeat(32), "incarnation": 0, "csrf_token": csrf});
    for method in [RESTORE_FOR_OWNER, ADOPTION_FOR_OWNER] {
        assert!(super::super::owns(method), "{method}");
        assert!(crate::handlers::is_enterprise_only_method(method), "{method}");
        for role in [UserRole::Employee, UserRole::Manager] {
            assert_eq!(refusal(&handler.handle_isolation_employee(method, params.clone(), &ctx("staff", role)).await), ADMIN_REQUIRED,
                "{method}");
        }
        assert_eq!(refusal(&handler.handle_isolation_employee(method, params.clone(), &ctx("admin-1", UserRole::Admin)).await),
            ADMIN_REQUIRED, "{method}: no current MFA");
        assert_eq!(refusal(&handler.handle_isolation_employee(method, params.clone(), &ctx("system", UserRole::Admin)).await),
            NOT_ALLOWED, "{method}");
    }
    assert!(serde_json::from_value::<ForOwner>(params.clone()).is_ok());
    let mut with_target = params.clone();
    with_target["target_label"] = json!("alice-next");
    assert!(serde_json::from_value::<AdoptionForOwner>(with_target.clone()).is_ok());
    assert!(serde_json::from_value::<ForOwner>(with_target).is_err(), "a restore keeps its label");
    for extra in ["owner_id", "owner_user_id", "requester_user_id", "approver_user_id", "filed_by_admin", "owner_unverified",
        "self_approved", "factor", "step_up"] {
        let mut forged = params.clone();
        forged[extra] = json!("x");
        assert!(serde_json::from_value::<ForOwner>(forged.clone()).is_err(), "{extra}");
        assert!(serde_json::from_value::<AdoptionForOwner>(forged).is_err(), "{extra}");
    }
    let mut partial = params.clone();
    partial.as_object_mut().unwrap().remove("csrf_token");
    assert!(serde_json::from_value::<ForOwner>(partial.clone()).is_err(), "csrf required");
    assert!(serde_json::from_value::<AdoptionForOwner>(partial).is_err(), "csrf required");
    // The production rule fails closed: no step-up, or no readable eligibility log.
    use crate::isolation_restore::{ApprovalRule as _, RequestKind};
    let users = rusqlite::Connection::open_in_memory().unwrap();
    users.execute_batch("BEGIN").unwrap();
    assert_eq!(LogRule::new(false, 10).decide_admin_filed(&users, "a", "o", "b", RequestKind::PersonalRestore),
        Err("STEP_UP_REQUIRED"));
    assert_eq!(LogRule::new(true, 10).decide_admin_filed(&users, "a", "o", "b", RequestKind::PersonalAdopt),
        Err("ADMIN_LOG_UNAVAILABLE"));
    assert_eq!(LogRule::new(false, 10).check_admin_filing(&users, "a", "o"), Err("ADMIN_LOG_UNAVAILABLE"));
}

/// `me.isolation.request_adoption`: the owner's proof fields only; the
/// adoption challenge is the restore challenge with `for_adoption`.
#[test]
fn owner_adoption_schema_has_no_owner_field() {
    let ok = json!({"employee_id": "0".repeat(32), "incarnation": 0, "factor": "passkey", "challenge_id": "c".repeat(64),
        "credential": {"id": "x"}, "target_label": "alice-next"});
    assert!(serde_json::from_value::<OwnerAdoption>(ok.clone()).is_ok());
    for extra in ["owner_id", "user_id", "requester_user_id", "purpose", "members"] {
        let mut forged = ok.clone();
        forged[extra] = json!("adopt:x:0");
        assert!(serde_json::from_value::<OwnerAdoption>(forged).is_err(), "{extra}");
    }
    let challenge = json!({"employee_id": "0".repeat(32), "incarnation": 0, "for_adoption": true});
    assert!(serde_json::from_value::<OwnerChallenge>(challenge.clone()).is_ok_and(|c| c.for_adoption));
    let mut forged = challenge;
    forged["purpose"] = json!("adopt:x:0");
    assert!(serde_json::from_value::<OwnerChallenge>(forged).is_err());
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
    // Passkey / email proofs carry the challenge (and the assertion); the
    // purpose is never a request field (the server derives it from the row).
    let passkey = json!({"employee_id": "0".repeat(32), "incarnation": 0, "factor": "passkey",
        "challenge_id": "c".repeat(64), "credential": {"id": "x"}});
    assert!(serde_json::from_value::<OwnerRequest>(passkey.clone()).is_ok());
    let cancel = json!({"request_id": "0".repeat(32), "factor": "email", "challenge_id": "c".repeat(64), "code": "123456"});
    assert!(serde_json::from_value::<OwnerCancel>(cancel.clone()).is_ok());
    for extra in ["purpose", "owner_id", "user_id"] {
        let mut forged = passkey.clone();
        forged[extra] = json!("restore:x:0");
        assert!(serde_json::from_value::<OwnerRequest>(forged).is_err(), "{extra}");
        let mut forged = cancel.clone();
        forged[extra] = json!("restore:x:0");
        assert!(serde_json::from_value::<OwnerCancel>(forged).is_err(), "{extra}");
    }
    let challenge = json!({"employee_id": "0".repeat(32), "incarnation": 0});
    assert!(serde_json::from_value::<OwnerChallenge>(challenge.clone()).is_ok());
    for extra in ["factor", "purpose", "owner_id", "email"] {
        let mut forged = challenge.clone();
        forged[extra] = json!("x");
        assert!(serde_json::from_value::<OwnerChallenge>(forged).is_err(), "{extra}");
    }
    let failure = failure_frame(Failure::Rpc(RpcError::Restore(crate::isolation_restore::Refusal::NotFound)));
    assert_eq!(refusal(&failure), "NOT_FOUND");
}
