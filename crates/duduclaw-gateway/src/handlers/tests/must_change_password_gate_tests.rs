//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// Regression coverage for `wiki/reports/resolved-todos/TODO-bootstrap-admin-ws-deadlock.md`.
/// The pure-function half (allowlist + reject frame) mirrors
/// `edition_rpc_gate_table_tests` above; the end-to-end half drives a real
/// `MethodHandler` over a tempdir-backed `UserDb` so the two acceptance
/// cases from the TODO are covered exactly as specified: a flagged caller
/// can still reach `users.change_password` and unlock the account, and every
/// other RPC stays refused until it does.
use super::*;

// ── Pure allowlist / reject-frame checks ──────────────────────────────

#[test]
fn self_service_methods_stay_reachable() {
    for method in [
        "users.me",
        "users.change_password",
        "connect",
        "connect.challenge",
        "ping",
    ] {
        assert!(
            is_password_change_allowlisted(method),
            "{method} must stay reachable for a flagged caller"
        );
    }
}

/// The far more dangerous direction: nothing outside the tiny
/// self-service surface leaks through, including ordinary daily-driver
/// RPCs and even RPCs the Enterprise-only gate already opens.
#[test]
fn everything_else_is_blocked() {
    for method in [
        "agents.list",
        "agents.create",
        "system.status",
        "tasks.list",
        "users.list",
        "users.audit_log",
        "billing.usage",
        "channels.add",
        "evolution.status",
    ] {
        assert!(
            !is_password_change_allowlisted(method),
            "{method} must NOT be reachable for a flagged caller"
        );
    }
}

#[test]
fn reject_frame_is_coded_and_leak_free() {
    match must_change_password_reject_frame() {
        WsFrame::Response {
            ok: false,
            error: Some(err),
            ..
        } => {
            assert_eq!(
                err.get("code").and_then(|v| v.as_str()),
                Some(MUST_CHANGE_PASSWORD_ERROR_CODE)
            );
            let msg = err.get("message").and_then(|v| v.as_str()).unwrap();
            assert!(msg.contains("密碼"), "plain-language copy: {msg}");
            for leak in ["RPC", "dispatch", "must_change_password", "WsFrame"] {
                assert!(!msg.contains(leak), "internal term leaked: {leak}");
            }
        }
        other => panic!("expected structured error response, got {other:?}"),
    }
}

// ── End-to-end: real UserDb + real MethodHandler::handle dispatch ─────

fn e2e_frame_ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

/// Build a `MethodHandler` wired to a real on-disk `UserDb` seeded with
/// the standard bootstrap `admin@local` (`must_change_password = 1`,
/// a random one-time password). The returned `ctx` mirrors exactly what
/// `server.rs::authenticate_jwt` now builds for such an account post-fix
/// — authenticated, flag set — regardless of whether the caller reached
/// the gateway over loopback or a LAN/bridge address; `authenticate_jwt`
/// never looks at the connection's source address at all, unlike
/// `local_session::evaluate`'s Personal+loopback auto-login escape hatch.
async fn handler_with_flagged_admin() -> (MethodHandler, UserContext, Arc<UserDb>, String) {
    let home = tempfile::tempdir().unwrap();
    let db_path = home.path().join("users.db");
    let db = UserDb::new(&db_path).expect("open user db");
    let one_time_password = db
        .ensure_default_admin()
        .expect("bootstrap admin")
        .expect("admin created on empty db");
    let admin = db
        .get_user_by_email("admin@local")
        .expect("lookup")
        .expect("admin exists");
    assert!(
        admin.must_change_password,
        "fixture sanity: a freshly bootstrapped admin starts flagged"
    );

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let db = Arc::new(db);
    let jwt = Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"));
    handler.set_user_db(db.clone(), jwt).await;

    let ctx = UserContext {
        user_id: admin.id.clone(),
        email: admin.email.clone(),
        role: UserRole::Admin,
        agent_access: std::collections::HashMap::new(),
        must_change_password: true,
    };
    (handler, ctx, db, one_time_password)
}

/// Case 1 from the TODO's acceptance list: a LAN-reachable, flagged
/// caller can still complete `users.change_password` and the account is
/// unlocked at the DB level afterward (a subsequent WS handshake would
/// build a ctx with `must_change_password = false`).
#[tokio::test]
async fn flagged_caller_can_change_password_and_unlock() {
    let (handler, ctx, db, one_time_password) = handler_with_flagged_admin().await;

    let frame = handler
        .handle(
            "users.change_password",
            json!({
                "current_password": one_time_password,
                "new_password": "a-Str0ng-New-Passw0rd!",
            }),
            &ctx,
        )
        .await;
    assert!(
        e2e_frame_ok(&frame),
        "change-password must succeed while flagged: {frame:?}"
    );

    let admin = db
        .get_user(&ctx.user_id)
        .unwrap()
        .expect("admin still exists");
    assert!(
        !admin.must_change_password,
        "account must be unlocked (DB flag cleared) after the change"
    );
}

/// Case 2 from the TODO's acceptance list: until the password is
/// changed, every other RPC is refused fail-closed with the
/// distinguishable coded error — never a silent drop, never a generic
/// permission-denied indistinguishable from an ordinary ACL failure.
#[tokio::test]
async fn flagged_caller_is_blocked_from_everything_else() {
    let (handler, ctx, _db, _pw) = handler_with_flagged_admin().await;

    for method in [
        "agents.list",
        "system.status",
        "tasks.list",
        "users.audit_log",
    ] {
        let frame = handler.handle(method, json!({}), &ctx).await;
        assert!(
            !e2e_frame_ok(&frame),
            "{method} must be blocked while flagged: {frame:?}"
        );
        match frame {
            WsFrame::Response {
                error: Some(err), ..
            } => {
                assert_eq!(
                    err.get("code").and_then(|v| v.as_str()),
                    Some(MUST_CHANGE_PASSWORD_ERROR_CODE),
                    "{method} must carry the must-change-password code, not a generic denial"
                );
            }
            other => panic!("expected structured error for {method}, got {other:?}"),
        }
    }

    // The self-service allowlist itself is unaffected by the gate.
    let me = handler.handle("users.me", json!({}), &ctx).await;
    assert!(
        e2e_frame_ok(&me),
        "users.me must stay reachable while flagged: {me:?}"
    );
}

/// A wrong current password is still rejected by
/// `handle_users_change_password`'s own verification — the allowlist
/// only widens *reachability*, it does not bypass the endpoint's
/// existing "prove you know the current password" check.
#[tokio::test]
async fn wrong_current_password_is_still_rejected() {
    let (handler, ctx, db, _pw) = handler_with_flagged_admin().await;

    let frame = handler
        .handle(
            "users.change_password",
            json!({
                "current_password": "definitely-not-it",
                "new_password": "a-Str0ng-New-Passw0rd!",
            }),
            &ctx,
        )
        .await;
    assert!(
        !e2e_frame_ok(&frame),
        "wrong current password must be rejected: {frame:?}"
    );

    let admin = db
        .get_user(&ctx.user_id)
        .unwrap()
        .expect("admin still exists");
    assert!(
        admin.must_change_password,
        "the flag must NOT clear on a rejected attempt"
    );
}
