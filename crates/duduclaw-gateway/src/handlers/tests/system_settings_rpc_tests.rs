//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// System-settings app — dispatch-level coverage for the five new
/// `device.about` / `device.timedate` / `device.timedate_set` /
/// `network.wired_status` / `network.wired_config` RPCs. A separate module
/// (rather than extending `device_rpc_tests`'s own tables) so this
/// system-settings work stays a pure ADDITION to `handlers.rs` — no
/// existing test literal is edited, only new ones appended.
use super::*;

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

/// Same fail-closed contract as `device_rpc_tests::
/// all_device_methods_fail_closed_off_appliance` — the five new methods
/// share the exact same `require_admin!() + require_appliance!()` gate,
/// so on this off-appliance test host every one of them must refuse
/// with `not_appliance`, admin or not.
#[tokio::test]
async fn all_system_settings_methods_fail_closed_off_appliance() {
    assert!(
        std::env::var(duduclaw_core::APPLIANCE_ENV).is_err(),
        "precondition: DUDUCLAW_APPLIANCE must be unset in the test process"
    );
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = admin_ctx();

    for (method, params) in [
        ("device.about", json!({})),
        ("device.timedate", json!({})),
        ("device.timedate_set", json!({"timezone": "Asia/Taipei"})),
        ("network.wired_status", json!({})),
        ("network.wired_config", json!({"mode": "dhcp"})),
    ] {
        let frame = handler.handle(method, params, &ctx).await;
        assert_eq!(
            frame_error_code(&frame).as_deref(),
            Some(DEVICE_NOT_APPLIANCE_ERROR_CODE),
            "{method} must refuse off-appliance: {frame:?}"
        );
    }
}

#[test]
fn timedate_set_error_frame_carries_given_code_and_message() {
    let frame = timedate_set_error_frame("invalid_timezone", "測試訊息");
    assert_eq!(
        frame_error_code(&frame).as_deref(),
        Some("invalid_timezone")
    );
    match &frame {
        WsFrame::Response { error: Some(e), .. } => {
            assert_eq!(e.get("message").and_then(|m| m.as_str()), Some("測試訊息"));
        }
        other => panic!("expected an error response: {other:?}"),
    }
}

#[test]
fn network_wired_config_error_frame_carries_the_closed_taxonomy() {
    for code in [
        crate::network::wired::WiredConfigErrorCode::NoInterface,
        crate::network::wired::WiredConfigErrorCode::InvalidMode,
        crate::network::wired::WiredConfigErrorCode::InvalidAddress,
        crate::network::wired::WiredConfigErrorCode::InvalidDns,
        crate::network::wired::WiredConfigErrorCode::BackendUnavailable,
        crate::network::wired::WiredConfigErrorCode::ApplyFailed,
    ] {
        let frame = network_wired_config_error_frame(code);
        assert_eq!(frame_error_code(&frame).as_deref(), Some(code.code()));
    }
}
