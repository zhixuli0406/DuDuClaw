//! IMPL-POWER — the `device.power_local` gate chain with appliance mode
//! actually **on**.
//!
//! Why a separate test binary: `DUDUCLAW_APPLIANCE` is a process-global env
//! var, and `duduclaw-gateway`'s lib tests deliberately never set it (several
//! of them assert it is unset as a precondition — see
//! `handlers.rs::device_rpc_tests`). An integration test gets its own process,
//! so flipping it here races nothing. Every test in this file wants the same
//! value, set once, never cleared.
//!
//! ## What this file deliberately does NOT do
//!
//! It never drives an **accepted** power request through dispatch. The accept
//! path ends at `DeviceOps::reboot()` / `poweroff()`, which on a Linux CI
//! runner is a real `systemctl reboot` — a test that reboots the machine
//! running it is not a test. The accepted path is covered instead in
//! `power_local.rs`'s own unit tests, driven through `MockDeviceOps`.
//!
//! What is left for a human to verify on real hardware is therefore exactly
//! one hop: accepted request → the box actually restarts. Everything leading
//! up to it is locked down here and in the unit tests.

use duduclaw_auth::UserContext;
use duduclaw_gateway::handlers::MethodHandler;
use duduclaw_gateway::power_local::RpcConnInfo;
use duduclaw_gateway::protocol::WsFrame;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Once;

static APPLIANCE: Once = Once::new();

/// Turn appliance mode on for this process, once.
fn ensure_appliance_mode() {
    APPLIANCE.call_once(|| {
        // SAFETY: single-threaded initialization guarded by `Once`, before any
        // test in this binary reads the value, and no test ever unsets it.
        unsafe { std::env::set_var(duduclaw_core::APPLIANCE_ENV, "1") };
    });
    assert!(
        duduclaw_core::is_appliance(),
        "fixture precondition: appliance mode must be on for this binary"
    );
}

fn error_code(f: &WsFrame) -> Option<String> {
    match f {
        WsFrame::Response { error: Some(e), .. } => {
            e.get("code").and_then(|c| c.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

fn conn(ip: IpAddr, pre_auth: bool) -> RpcConnInfo {
    RpcConnInfo::from_ws(SocketAddr::new(ip, 51515), pre_auth)
}

fn audit_rows(home: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// A LAN caller on a REAL appliance is refused — the appliance fence passed
/// (otherwise the code would be `not_appliance`), so this is the loopback
/// fence firing on its own. This is the case that matters most: without it,
/// anyone on the office network could power off the duty box.
#[tokio::test]
async fn lan_caller_is_refused_on_a_real_appliance() {
    ensure_appliance_mode();
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for ip in [
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 42)),
        IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
        IpAddr::V6("fe80::1".parse().unwrap()),
        // The IPv4-mapped form of a LAN address must not sneak past the
        // loopback test either.
        IpAddr::V6("::ffff:192.168.1.42".parse().unwrap()),
    ] {
        for action in ["reboot", "shutdown"] {
            let frame = handler
                .handle_conn(
                    "device.power_local",
                    json!({ "action": action }),
                    &UserContext::admin_fallback(),
                    conn(ip, true),
                )
                .await;
            assert_eq!(
                error_code(&frame).as_deref(),
                Some("not_local"),
                "{ip} / {action} must be refused: {frame:?}"
            );
        }
    }

    // A refusal at the loopback fence is deliberately NOT audited (a
    // log-flood vector for an unauthenticated peer that can never get in).
    assert!(
        audit_rows(home.path()).is_empty(),
        "off-loopback refusals must leave no audit rows: {:?}",
        audit_rows(home.path())
    );
}

/// A local caller on a real appliance clears BOTH fences and is judged on the
/// action alone — proving the appliance and loopback gates passed, without
/// ever reaching the reboot itself. The refusal IS audited here, because this
/// caller is inside both fences.
#[tokio::test]
async fn local_caller_clears_both_fences_and_is_judged_on_the_action() {
    ensure_appliance_mode();
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for (raw, label) in [
        (json!({"action": "restart"}), "restart"), // `device.power`'s spelling, not this one's
        (json!({"action": "Reboot"}), "Reboot"),
        (json!({"action": ""}), ""),
        (json!({}), ""), // absent `action` behaves exactly like an empty one
    ] {
        let frame = handler
            .handle_conn(
                "device.power_local",
                raw.clone(),
                &UserContext::admin_fallback(),
                conn(IpAddr::V4(Ipv4Addr::LOCALHOST), true),
            )
            .await;
        assert_eq!(
            error_code(&frame).as_deref(),
            Some("invalid_action"),
            "{raw} must reach the action check, not stop at an earlier fence: {frame:?}"
        );
        let rows = audit_rows(home.path());
        let last = rows.last().expect("a post-fence refusal must be audited");
        assert_eq!(last["event_type"], "device_power_local_denied");
        assert_eq!(last["details"]["reason"], "unknown_action");
        assert_eq!(last["details"]["action_raw"], label);
        assert_eq!(last["details"]["source"], "127.0.0.1");
        assert_eq!(last["details"]["pre_auth"], true);
    }
}

/// IPv6 loopback and the IPv4-mapped IPv4 loopback are both "at the machine"
/// — a dual-stack listener reports the mapped form for an ordinary IPv4
/// client, and reading it as remote would break the exact caller this surface
/// exists for.
#[tokio::test]
async fn every_loopback_spelling_clears_the_local_fence() {
    ensure_appliance_mode();
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for ip in [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()),
    ] {
        let frame = handler
            .handle_conn(
                "device.power_local",
                json!({ "action": "not-a-real-action" }),
                &UserContext::admin_fallback(),
                conn(ip, true),
            )
            .await;
        assert_eq!(
            error_code(&frame).as_deref(),
            Some("invalid_action"),
            "{ip} must count as local: {frame:?}"
        );
    }
}

/// The pre-auth allowlist still holds with appliance mode on — turning the
/// appliance flag on must not widen what an unauthenticated connection can
/// reach beyond the single power method.
#[tokio::test]
async fn appliance_mode_does_not_widen_the_pre_auth_allowlist() {
    ensure_appliance_mode();
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for method in ["device.power", "device.factory_reset", "device.status", "users.list"] {
        let frame = handler
            .handle_conn(
                method,
                json!({"action": "reboot", "confirm": true}),
                &UserContext::admin_fallback(),
                conn(IpAddr::V4(Ipv4Addr::LOCALHOST), true),
            )
            .await;
        assert_eq!(
            error_code(&frame).as_deref(),
            Some("login_required"),
            "{method} must stay behind login even on an appliance: {frame:?}"
        );
    }
}

/// The rate limit is real and is reached on the appliance path. Driven with an
/// invalid action so the budget is consumed WITHOUT any accepted request ever
/// reaching `DeviceOps` — see this file's header on why no test here reboots
/// anything. (The limiter is checked after the action gate, so an invalid
/// action never consumes budget; this asserts the ordering rather than the
/// exhaustion, which `power_local.rs`'s unit tests cover directly against a
/// private limiter instance.)
#[tokio::test]
async fn invalid_actions_never_consume_the_power_budget() {
    ensure_appliance_mode();
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Far more attempts than the budget — every one must still answer
    // `invalid_action`, never `rate_limited`, because a request that never
    // became a power action must not spend the power budget.
    for i in 0..20 {
        let frame = handler
            .handle_conn(
                "device.power_local",
                json!({ "action": "nope" }),
                &UserContext::admin_fallback(),
                conn(IpAddr::V4(Ipv4Addr::LOCALHOST), true),
            )
            .await;
        assert_eq!(
            error_code(&frame).as_deref(),
            Some("invalid_action"),
            "attempt {i}: {frame:?}"
        );
    }
}
