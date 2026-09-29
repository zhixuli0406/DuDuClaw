//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// IMPL-POWER — dispatch-level coverage for the lock screen's login-free
/// power surface (`device.power_local`) and the pre-auth allowlist that
/// contains it.
///
/// The pure gate matrix (appliance × loopback × action, plus the rate limit,
/// the audit rows and the `DeviceOps` execution path driven through
/// `MockDeviceOps`) lives in `power_local.rs`'s own test module — deliberately
/// there rather than here, because those cases need to vary the appliance flag
/// as a plain boolean and this process never sets the real, process-global
/// `DUDUCLAW_APPLIANCE` env var (same discipline as `device_rpc_tests` above
/// and `duduclaw_core::appliance::appliance_flag`). What THIS module locks
/// down is what only exists at the dispatch layer: which methods a pre-auth
/// connection may reach, and that the transport facts are actually consulted.
use super::*;
use crate::power_local::RpcConnInfo;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

fn admin_ctx() -> UserContext {
    UserContext::admin_fallback()
}

fn frame_error_code(f: &WsFrame) -> Option<String> {
    match f {
        WsFrame::Response { error: Some(e), .. } => {
            e.get("code").and_then(|c| c.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

fn loopback_conn(pre_auth: bool) -> RpcConnInfo {
    RpcConnInfo::from_ws(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 54321),
        pre_auth,
    )
}

fn lan_conn(pre_auth: bool) -> RpcConnInfo {
    RpcConnInfo::from_ws(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 42)), 54321),
        pre_auth,
    )
}

async fn handler() -> (tempfile::TempDir, MethodHandler) {
    let home = tempfile::tempdir().unwrap();
    let h = MethodHandler::new(home.path().to_path_buf()).await;
    (home, h)
}

// ── The pre-auth allowlist at the dispatch chokepoint ────────────────

/// The direction that matters most: a credential-less connection reaches
/// NOTHING except the one power method — not the admin-only `device.power`
/// twin, not the self-service methods the *password-change* allowlist
/// opens, not ordinary daily-driver RPCs. Every refusal is the
/// distinguishable coded error, never a silent drop.
#[tokio::test]
async fn pre_auth_connection_is_blocked_from_everything_else() {
    let (_home, handler) = handler().await;
    for method in [
        "device.power",
        "device.status",
        "device.factory_reset",
        "users.me",
        "users.change_password",
        "users.list",
        "agents.list",
        "system.status",
        "tasks.list",
        "evolution.status",
    ] {
        let frame = handler
            .handle_conn(method, json!({}), &admin_ctx(), loopback_conn(true))
            .await;
        assert!(
            !matches!(frame, WsFrame::Response { ok: true, .. }),
            "{method} must be blocked on a pre-auth connection: {frame:?}"
        );
        assert_eq!(
            frame_error_code(&frame).as_deref(),
            Some(LOGIN_REQUIRED_ERROR_CODE),
            "{method} must carry the login-required code, not a generic denial"
        );
    }
}

/// The pre-auth gate runs ahead of the role check, so even a context that
/// claims Admin (as the fixture above does — a pre-auth connection could
/// never legitimately carry one, which is exactly why the test uses it)
/// gets nowhere. The gate is a property of the CONNECTION, not the claimed
/// identity.
#[tokio::test]
async fn pre_auth_gate_outranks_an_admin_looking_context() {
    let (_home, handler) = handler().await;
    let frame = handler
        .handle_conn("users.list", json!({}), &admin_ctx(), loopback_conn(true))
        .await;
    assert_eq!(
        frame_error_code(&frame).as_deref(),
        Some(LOGIN_REQUIRED_ERROR_CODE),
        "{frame:?}"
    );
}

/// The allowlisted method is NOT short-circuited by the pre-auth gate —
/// it reaches the real handler, which then applies its own fences (here,
/// off-appliance, so `not_appliance`). A `login_required` answer would
/// mean the allowlist never let it through.
#[tokio::test]
async fn the_allowlisted_method_passes_the_pre_auth_gate() {
    let (_home, handler) = handler().await;
    let frame = handler
        .handle_conn(
            "device.power_local",
            json!({"action": "reboot"}),
            &admin_ctx(),
            loopback_conn(true),
        )
        .await;
    assert_eq!(
        frame_error_code(&frame).as_deref(),
        Some(crate::power_local::PowerLocalDenial::NotAppliance.code()),
        "must reach the handler's own fences, not stop at the allowlist: {frame:?}"
    );
}

/// An ordinary authenticated connection is completely unaffected by the
/// new gate — the pre-auth restriction is opt-in per connection.
#[tokio::test]
async fn authenticated_connections_are_unaffected() {
    let (_home, handler) = handler().await;
    let frame = handler
        .handle_conn(
            "system.status",
            json!({}),
            &admin_ctx(),
            loopback_conn(false),
        )
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "an authenticated connection must still work: {frame:?}"
    );
}

#[test]
fn login_required_frame_is_coded_and_leak_free() {
    match login_required_reject_frame() {
        WsFrame::Response {
            ok: false,
            error: Some(err),
            ..
        } => {
            assert_eq!(
                err.get("code").and_then(|v| v.as_str()),
                Some(LOGIN_REQUIRED_ERROR_CODE)
            );
            let msg = err.get("message").and_then(|v| v.as_str()).unwrap();
            assert!(msg.contains("登入"), "plain-language copy: {msg}");
            for leak in [
                "RPC",
                "dispatch",
                "pre_auth",
                "WsFrame",
                "device.power_local",
            ] {
                assert!(!msg.contains(leak), "internal term leaked: {leak}");
            }
        }
        other => panic!("expected structured error response, got {other:?}"),
    }
}

// ── The RPC's own fences, as seen through dispatch ───────────────────

/// Off-appliance (this process never sets `DUDUCLAW_APPLIANCE`) the widest
/// fence answers first, for every combination of connection and action —
/// including a LAN peer, which must never learn anything more specific.
#[tokio::test]
async fn off_appliance_refuses_every_combination() {
    assert!(
        !duduclaw_core::is_appliance(),
        "precondition: DUDUCLAW_APPLIANCE must be unset in the test process"
    );
    let (_home, handler) = handler().await;
    for conn in [
        loopback_conn(true),
        loopback_conn(false),
        lan_conn(true),
        lan_conn(false),
    ] {
        for action in [
            json!({"action": "reboot"}),
            json!({"action": "shutdown"}),
            json!({}),
        ] {
            let frame = handler
                .handle_conn("device.power_local", action.clone(), &admin_ctx(), conn)
                .await;
            assert_eq!(
                frame_error_code(&frame).as_deref(),
                Some("not_appliance"),
                "conn={conn:?} action={action} → {frame:?}"
            );
        }
    }
}

/// The in-process entry point (`handle`, used by every non-WebSocket
/// caller and by 90-odd existing tests) carries no peer, and `None` reads
/// as NOT loopback — so the surface is unreachable that way by
/// construction, not by accident.
#[tokio::test]
async fn in_process_dispatch_has_no_peer_and_is_never_local() {
    let (_home, handler) = handler().await;
    assert!(!RpcConnInfo::internal().peer_is_loopback());
    let frame = handler
        .handle(
            "device.power_local",
            json!({"action": "reboot"}),
            &admin_ctx(),
        )
        .await;
    assert!(
        !matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );
}

/// The method is discoverable in the RPC catalog — a surface the shell
/// has to call but that no dashboard screen lists would otherwise be
/// invisible to anyone auditing what this gateway exposes.
#[tokio::test]
async fn power_local_is_listed_in_the_method_catalog() {
    let (_home, handler) = handler().await;
    let frame = handler
        .handle("tools.catalog", json!({}), &admin_ctx())
        .await;
    let listed = serde_json::to_string(&frame).unwrap();
    assert!(
        listed.contains("device.power_local"),
        "device.power_local must appear in the method catalog"
    );
}
