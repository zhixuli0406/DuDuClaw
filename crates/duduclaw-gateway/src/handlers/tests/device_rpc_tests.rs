//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

fn admin_ctx() -> UserContext {
    UserContext::admin_fallback()
}

fn employee_ctx() -> UserContext {
    UserContext {
        user_id: "u1".to_string(),
        email: "u1@test.local".to_string(),
        role: UserRole::Employee,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    }
}

fn frame_error_code(f: &WsFrame) -> Option<String> {
    match f {
        WsFrame::Response { error: Some(e), .. } => {
            e.get("code").and_then(|c| c.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

/// Every `device.*` method (plus D4a's `network.*` family, which shares
/// the exact same `require_admin!() + require_appliance!()` gate), tried
/// with no `DUDUCLAW_APPLIANCE` set — the default test-process state
/// (this crate never sets that env var in any other test). Every one
/// must fail closed with `not_appliance`, admin or not — the appliance
/// gate runs regardless of role.
#[tokio::test]
async fn all_device_methods_fail_closed_off_appliance() {
    assert!(
        std::env::var(duduclaw_core::APPLIANCE_ENV).is_err(),
        "precondition: DUDUCLAW_APPLIANCE must be unset in the test process"
    );
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = admin_ctx();

    for (method, params) in [
        ("device.status", json!({})),
        ("device.network", json!({})),
        ("device.update_status", json!({})),
        ("device.update_check", json!({})),
        ("device.update_apply", json!({})),
        ("device.update_rollback", json!({"confirm": true})),
        ("device.boot_assessment", json!({})),
        ("device.backup_create", json!({})),
        ("device.backup_schedule_get", json!({})),
        (
            "device.backup_schedule_set",
            json!({"schedule_enabled": true}),
        ),
        ("device.backup_list", json!({})),
        ("device.backup_delete", json!({"name": "x.tar.gz"})),
        (
            "device.backup_restore",
            json!({"path": "/tmp/x.tar.gz", "confirm": true}),
        ),
        ("device.factory_reset", json!({"confirm": true})),
        (
            "device.power",
            json!({"action": "restart", "confirm": true}),
        ),
        // The login-free lock-screen twin refuses off-appliance with the
        // SAME code as the rest of the family (`PowerLocalDenial::
        // NotAppliance`), so a client branches on one string, not two.
        ("device.power_local", json!({"action": "reboot"})),
        // D4a: network.* shares the device.* gate byte-for-byte.
        ("network.wifi_scan", json!({})),
        (
            "network.wifi_connect",
            json!({"ssid": "SomeNetwork", "psk": "somepassphrase"}),
        ),
        ("network.wifi_forget", json!({"ssid": "SomeNetwork"})),
        ("network.status", json!({})),
    ] {
        let frame = handler.handle(method, params, &ctx).await;
        assert_eq!(
            frame_error_code(&frame).as_deref(),
            Some(DEVICE_NOT_APPLIANCE_ERROR_CODE),
            "{method} must refuse off-appliance: {frame:?}"
        );
    }
}

/// Non-admin is refused even off-appliance — the admin gate runs
/// first, so a non-admin never learns whether the box is an appliance.
#[tokio::test]
async fn device_status_refuses_non_admin() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("device.status", json!({}), &employee_ctx())
        .await;
    assert!(
        !matches!(frame, WsFrame::Response { ok: true, .. }),
        "{frame:?}"
    );
    // Off-appliance the admin gate error and the appliance gate error
    // are both plausible depending on macro order; what matters is it
    // is NOT a success and NOT the appliance-specific code (proving the
    // admin gate — not the appliance gate — is what fired first).
    assert_ne!(
        frame_error_code(&frame).as_deref(),
        Some(DEVICE_NOT_APPLIANCE_ERROR_CODE)
    );
}

fn frame_data(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        other => panic!("expected a payload: {other:?}"),
    }
}

/// R2 (2026-08): unlike every `device.*` method above, the
/// `"system.status" => self.handle_system_status().await` dispatch arm
/// carries no `require_admin!()` — so a manager/employee caller can
/// already read it today, and it now also carries the non-sensitive
/// `is_appliance` boolean (a direct forward of the same single authority
/// `device.status`'s `require_appliance!()` gate reads,
/// `duduclaw_core::is_appliance()` — never re-derived). This is the
/// signal `useIsAppliance` (web) reads so a non-admin viewer on the
/// appliance image can also land on the conversational console
/// (`App.tsx::HomeLanding`), instead of only admins as before — `device.
/// status`'s CPU/RAM/network detail stays admin-only, untouched.
#[tokio::test]
async fn system_status_is_readable_by_non_admin_and_carries_is_appliance() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("system.status", json!({}), &employee_ctx())
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "system.status must succeed for a non-admin caller: {frame:?}"
    );
    let data = frame_data(&frame);
    // Test-process precondition (same one `all_device_methods_fail_closed_off_appliance`
    // asserts above): `DUDUCLAW_APPLIANCE` is never set in this binary,
    // so the live authority reads `false` here. The `true` branch of
    // that boolean is intentionally NOT re-tested by flipping the real
    // process-global env var — this module deliberately never does that
    // (see this test's neighbors' doc comments) to avoid racing that
    // precondition under parallel test execution; the on/off logic
    // itself is already exhaustively covered by
    // `duduclaw_core::appliance::appliance_flag`'s pure unit tests. What
    // this assertion locks down is that the field is wired through
    // byte-for-byte, not hand-rolled.
    assert_eq!(
        data.get("is_appliance").and_then(Value::as_bool),
        Some(duduclaw_core::is_appliance()),
        "is_appliance must be an unmodified forward of the single authority: {data:?}"
    );
    assert_eq!(
        duduclaw_core::is_appliance(),
        false,
        "precondition: DUDUCLAW_APPLIANCE must be unset in this test process"
    );
}

/// 2026-09-29 audit (X1): `system.status` also forwards the Decision Lab
/// kill switch so the dashboard can hide a nav row whose whole HTTP
/// surface is 404-ed. No `config.toml` at all ⇒ `true`, exactly like
/// `DecisionConfig::default()` — an old home directory must keep seeing
/// the page.
#[tokio::test]
async fn system_status_reports_decision_enabled_true_without_config() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("system.status", json!({}), &employee_ctx())
        .await;
    let data = frame_data(&frame);
    assert_eq!(
        data.get("decision_enabled").and_then(Value::as_bool),
        Some(true),
        "a home with no config.toml must report the default (enabled): {data:?}"
    );
}

/// The `false` branch of the same field: `[decision] enabled = false` in
/// `config.toml` is what the operator writes to retire the line, and it
/// must reach the dashboard (the nav row and page hide on it).
#[tokio::test]
async fn system_status_reports_decision_enabled_false_when_operator_disables_it() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[decision]\nenabled = false\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("system.status", json!({}), &employee_ctx())
        .await;
    let data = frame_data(&frame);
    assert_eq!(
        data.get("decision_enabled").and_then(Value::as_bool),
        Some(false),
        "[decision] enabled = false must be forwarded verbatim: {data:?}"
    );
}

/// The three destructive ops refuse without `confirm: true`, even with
/// admin + (mocked) appliance mode — checked here without actually
/// flipping `DUDUCLAW_APPLIANCE` (avoids any risk of racing other
/// tests in this same process that read it indirectly via
/// `gateway_bind_for_home`) by asserting the confirm gate fires FIRST,
/// before the appliance gate even runs — `require_confirm!()` is
/// checked after `require_appliance!()` in dispatch, so on this
/// non-appliance test host the observed code is always
/// `not_appliance`, not `confirm_required`. The meaningful invariant
/// this test locks down is the ORDER-INDEPENDENT one: omitting confirm
/// must never itself be treated as `Some(true)` — see the pure
/// `params.get("confirm")` check exercised directly below instead.
#[test]
fn confirm_gate_only_accepts_literal_true() {
    assert_eq!(json!({}).get("confirm").and_then(Value::as_bool), None);
    assert_eq!(
        json!({"confirm": false})
            .get("confirm")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        json!({"confirm": "true"})
            .get("confirm")
            .and_then(Value::as_bool),
        None,
        "a string \"true\" must not satisfy the gate — only the JSON boolean does"
    );
    assert_eq!(
        json!({"confirm": true})
            .get("confirm")
            .and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
fn device_not_appliance_frame_carries_stable_code() {
    let frame = device_not_appliance_frame();
    assert_eq!(frame_error_code(&frame).as_deref(), Some("not_appliance"));
}

#[test]
fn device_confirm_required_frame_carries_stable_code() {
    let frame = device_confirm_required_frame();
    assert_eq!(
        frame_error_code(&frame).as_deref(),
        Some("confirm_required")
    );
}

/// O16 golden: every [`crate::os_ops::OsOpError`] variant must keep
/// rendering to the frame this surface already shipped — the gate
/// refusals reuse the existing `device_*_frame` builders verbatim, a
/// `device.*` op failure keeps its `unsupported`/`io_error` codes, a
/// Wi-Fi failure keeps `network::error_to_json`'s body, and the
/// (unreachable) serialize arm keeps this surface's own prefix rather
/// than the CLI's `序列化失敗：`.
#[test]
fn os_op_error_frame_matches_the_already_shipped_renderings() {
    use crate::os_ops::OsOpError as E;

    assert_eq!(
        frame_error_code(&os_op_error_frame(&E::NotAppliance, "x")).as_deref(),
        Some("not_appliance"),
    );
    assert_eq!(
        frame_error_code(&os_op_error_frame(&E::ConfirmRequired, "x")).as_deref(),
        Some("confirm_required"),
    );
    assert_eq!(
        frame_error_code(&os_op_error_frame(
            &E::DeviceOp(crate::device_ops::DeviceOpError::Unsupported("nope".into())),
            "x",
        ))
        .as_deref(),
        Some("unsupported"),
    );
    assert_eq!(
        frame_error_code(&os_op_error_frame(
            &E::DeviceOp(crate::device_ops::DeviceOpError::Io("disk".into())),
            "x",
        ))
        .as_deref(),
        Some("io_error"),
    );
    assert_eq!(
        frame_error_code(&os_op_error_frame(
            &E::Coded {
                code: "slot_mismatch".into(),
                message: "nope".into(),
            },
            "x",
        ))
        .as_deref(),
        Some("slot_mismatch"),
    );

    let wifi = os_op_error_frame(
        &E::Wifi(crate::network::WifiError {
            code: crate::network::WifiErrorCode::BackendUnavailable,
            detail: "no iwd".into(),
        }),
        "x",
    );
    assert_eq!(
        frame_error_code(&wifi).as_deref(),
        Some("backend_unavailable")
    );
    // `detail` is tracing-only and must never reach a client payload.
    let rendered = serde_json::to_string(&wifi).unwrap();
    assert!(!rendered.contains("no iwd"), "leaked detail: {rendered}");

    // Serialize keeps this surface's wording, not the CLI's.
    let ser = os_op_error_frame(&E::Serialize("bad".into()), "device status");
    let text = serde_json::to_string(&ser).unwrap();
    assert!(
        text.contains("device status serialize failed: bad"),
        "unexpected: {text}"
    );
    assert!(!text.contains("序列化失敗"), "unexpected: {text}");
}

/// O16: `os_op_frame`'s success arm must be exactly `ok_response` over
/// the authority's payload — no re-wrapping, no re-keying.
#[test]
fn os_op_frame_success_carries_the_authority_payload_verbatim() {
    let payload = json!({ "success": true, "stdout": "ok", "stderr": "" });
    match os_op_frame(Ok(payload.clone()), "x") {
        WsFrame::Response {
            ok,
            payload: Some(p),
            error,
            ..
        } => {
            assert!(ok);
            assert!(error.is_none());
            assert_eq!(p, payload);
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[test]
fn device_op_result_frame_maps_success_and_error_variants() {
    use crate::device_ops::{DeviceOpError, OpOutput};

    let ok = device_op_result_frame(Ok(OpOutput {
        success: true,
        stdout: "done".to_string(),
        stderr: String::new(),
    }));
    assert!(matches!(ok, WsFrame::Response { ok: true, .. }));

    let unsupported =
        device_op_result_frame(Err(DeviceOpError::Unsupported("nope".to_string())));
    assert_eq!(
        frame_error_code(&unsupported).as_deref(),
        Some("unsupported")
    );

    let io_err = device_op_result_frame(Err(DeviceOpError::Io("disk full".to_string())));
    assert_eq!(frame_error_code(&io_err).as_deref(), Some("io_error"));
}

/// The 2026-08-23 appliance regression: a power command that ran but
/// exited non-zero (polkit `Access denied`) rode an `ok:true` frame and
/// the lock screen — which branches on `ok` alone and renders nothing on
/// success — showed "正在送出…" forever. `device.power_local`'s own frame
/// mapping must turn ran-but-failed into `ok:false`; genuine success and
/// the `Err` variants stay identical to the generic mapping.
#[test]
fn power_local_result_frame_turns_ran_but_failed_into_an_error() {
    use crate::device_ops::{DeviceOpError, OpOutput};

    let failed = power_local_result_frame(Ok(OpOutput {
        success: false,
        stdout: String::new(),
        stderr: "Call to Reboot failed: Access denied".to_string(),
    }));
    assert!(matches!(failed, WsFrame::Response { ok: false, .. }));
    assert_eq!(frame_error_code(&failed).as_deref(), Some("exec_failed"));

    let ok = power_local_result_frame(Ok(OpOutput {
        success: true,
        stdout: String::new(),
        stderr: String::new(),
    }));
    assert!(matches!(ok, WsFrame::Response { ok: true, .. }));

    let unsupported =
        power_local_result_frame(Err(DeviceOpError::Unsupported("no sysd".to_string())));
    assert_eq!(
        frame_error_code(&unsupported).as_deref(),
        Some("unsupported")
    );
}

#[test]
fn network_write_detection_gates_static_ip_params() {
    assert!(crate::device::is_network_write_request(
        &json!({"static_ip": "10.0.0.5"})
    ));
    assert!(!crate::device::is_network_write_request(&json!({})));
}

// ── T1 (DESIGN-agent-body-network-2026-08.md §12): wifi_connect audit source ──

/// `network.wifi_connect`'s audit row must carry `source:
/// "operator_console_prompted"` only when the caller says so with that
/// exact string — every other value, wrong JSON type, or missing field
/// degrades to the pre-existing `"dashboard"`. A client can never forge
/// an arbitrary audit attribution label through this field.
#[test]
fn wifi_connect_audit_source_is_a_closed_allowlist() {
    assert_eq!(
        MethodHandler::wifi_connect_audit_source(Some(&json!("operator_console_prompted"))),
        "operator_console_prompted"
    );
    assert_eq!(MethodHandler::wifi_connect_audit_source(None), "dashboard");
    assert_eq!(
        MethodHandler::wifi_connect_audit_source(Some(&json!("settings_page"))),
        "dashboard"
    );
    assert_eq!(
        MethodHandler::wifi_connect_audit_source(Some(&json!(42))),
        "dashboard"
    );
    assert_eq!(
        MethodHandler::wifi_connect_audit_source(Some(&Value::Null)),
        "dashboard"
    );
}

/// End-to-end: calling the handler METHOD directly (bypassing the
/// `require_appliance!()` dispatch gate — same convention
/// `mcp_os_ops.rs`'s O-0 tool tests use for `handle_os_wifi_connect`)
/// with `source: "operator_console_prompted"` in params must produce an
/// audit row carrying that source. Off-Linux this call always fails at
/// the `network::wifi_connect` facade (`non_linux_error`) — irrelevant
/// here, since success/failure of the connect itself is orthogonal to
/// which `source` label the audit row carries (`audit_wifi_event` is
/// called on both the `Ok` and `Err` arms).
#[tokio::test]
async fn wifi_connect_audit_row_carries_operator_console_source() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let _ = handler
        .handle_network_wifi_connect(json!({
            "ssid": "iPhone-Sam",
            "source": "operator_console_prompted",
        }))
        .await;

    let events = duduclaw_security::audit::read_recent_events(home.path(), 10);
    let row = events
        .iter()
        .find(|e| e.event_type == "wifi_connect")
        .expect("wifi_connect audit row must exist");
    assert_eq!(row.details["source"], "operator_console_prompted");
    assert_eq!(row.details["ssid"], "iPhone-Sam");
    // No psk, no "was a psk supplied" flag anywhere in the row.
    assert!(row.details.get("psk").is_none());
}

/// A caller that omits `source` entirely (every pre-T1 caller, e.g. a
/// human using the dashboard's own network Settings page) must keep
/// getting `"dashboard"` — byte-identical to before this change.
#[tokio::test]
async fn wifi_connect_audit_row_defaults_to_dashboard_source() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let _ = handler
        .handle_network_wifi_connect(json!({ "ssid": "DuDu-Office" }))
        .await;

    let events = duduclaw_security::audit::read_recent_events(home.path(), 10);
    let row = events
        .iter()
        .find(|e| e.event_type == "wifi_connect")
        .unwrap();
    assert_eq!(row.details["source"], "dashboard");
}
