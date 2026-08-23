//! Unit tests for [`super`] (`network/mod.rs`) — split into a sibling file
//! purely to keep `mod.rs` inside this project's 800-line hard cap; included
//! via `#[path = "tests_network.rs"] mod tests;` in `mod.rs`, which makes
//! this a literal CHILD module (`network::tests`), not an independent
//! sibling — `use super::*` below therefore reaches every private item in
//! `network/mod.rs` exactly as it would if this were still inline. Same
//! split technique this crate already uses for `codrive/registry.rs` /
//! `codrive/tests_registry.rs`.

use super::*;

// ── dbm_centi_to_bars ───────────────────────────────────────────────

#[test]
fn dbm_centi_to_bars_boundaries() {
    assert_eq!(dbm_centi_to_bars(-4000), 4, "stronger than -50dBm");
    assert_eq!(dbm_centi_to_bars(-5000), 4, "exactly -50dBm boundary");
    assert_eq!(dbm_centi_to_bars(-5001), 3, "just past -50dBm");
    assert_eq!(dbm_centi_to_bars(-6000), 3, "exactly -60dBm boundary");
    assert_eq!(dbm_centi_to_bars(-6001), 2, "just past -60dBm");
    assert_eq!(dbm_centi_to_bars(-7000), 2, "exactly -70dBm boundary");
    assert_eq!(dbm_centi_to_bars(-7001), 1, "just past -70dBm");
    assert_eq!(dbm_centi_to_bars(-9000), 1, "very weak");
    assert_eq!(
        dbm_centi_to_bars(i16::MIN),
        1,
        "never 0, even at the extreme"
    );
}

// ── iwd_network_type_to_security ─────────────────────────────────────

#[test]
fn iwd_network_type_maps_known_values() {
    assert_eq!(iwd_network_type_to_security("open"), "open");
    assert_eq!(iwd_network_type_to_security("wep"), "wep");
    assert_eq!(iwd_network_type_to_security("psk"), "psk");
    assert_eq!(iwd_network_type_to_security("8021x"), "8021x");
}

#[test]
fn iwd_network_type_unknown_falls_back() {
    assert_eq!(iwd_network_type_to_security(""), "unknown");
    assert_eq!(iwd_network_type_to_security("WPA2-Personal"), "unknown");
    assert_eq!(iwd_network_type_to_security("sae"), "unknown");
}

// ── classify_iwd_error ────────────────────────────────────────────────

#[test]
fn classify_iwd_error_only_compares_the_suffix() {
    assert_eq!(
        classify_iwd_error("net.connman.iwd.NotFound", false),
        WifiErrorCode::NotFound
    );
    // Same suffix, different (even malformed) prefix — still matches.
    assert_eq!(
        classify_iwd_error("NotFound", false),
        WifiErrorCode::NotFound
    );
    assert_eq!(
        classify_iwd_error("a.b.c.NotFound", false),
        WifiErrorCode::NotFound
    );
}

#[test]
fn classify_iwd_error_failed_depends_on_psk_supplied() {
    assert_eq!(
        classify_iwd_error("net.connman.iwd.Failed", true),
        WifiErrorCode::WrongPassword
    );
    assert_eq!(
        classify_iwd_error("net.connman.iwd.Failed", false),
        WifiErrorCode::OutOfRange
    );
}

#[test]
fn classify_iwd_error_known_table() {
    let cases: &[(&str, WifiErrorCode)] = &[
        ("net.connman.iwd.Timeout", WifiErrorCode::OutOfRange),
        ("net.connman.iwd.Aborted", WifiErrorCode::OutOfRange),
        ("net.connman.iwd.NoNetwork", WifiErrorCode::NotFound),
        (
            "net.connman.iwd.NotSupported",
            WifiErrorCode::UnsupportedSecurity,
        ),
        ("net.connman.iwd.NoAgent", WifiErrorCode::BackendUnavailable),
        (
            "org.freedesktop.DBus.Error.ServiceUnknown",
            WifiErrorCode::BackendUnavailable,
        ),
        (
            "org.freedesktop.DBus.Error.NameHasNoOwner",
            WifiErrorCode::BackendUnavailable,
        ),
        ("net.connman.iwd.Busy", WifiErrorCode::BackendUnavailable),
        (
            "net.connman.iwd.InProgress",
            WifiErrorCode::BackendUnavailable,
        ),
        (
            "net.connman.iwd.InvalidArguments",
            WifiErrorCode::WrongPassword,
        ),
        (
            "net.connman.iwd.InvalidFormat",
            WifiErrorCode::WrongPassword,
        ),
    ];
    for (name, expected) in cases {
        assert_eq!(classify_iwd_error(name, false), *expected, "{name}");
    }
}

#[test]
fn classify_iwd_error_unknown_suffix_is_fail_safe() {
    assert_eq!(
        classify_iwd_error("net.connman.iwd.SomeFutureError", true),
        WifiErrorCode::BackendUnavailable
    );
    assert_eq!(
        classify_iwd_error("", false),
        WifiErrorCode::BackendUnavailable
    );
    assert_eq!(
        classify_iwd_error(".", false),
        WifiErrorCode::BackendUnavailable
    );
}

// ── classify_adapter_absence ─────────────────────────────────────────

#[test]
fn classify_adapter_absence_driver_missing_when_pci_present() {
    assert_eq!(
        classify_adapter_absence(0, true),
        WifiErrorCode::DriverMissing
    );
}

#[test]
fn classify_adapter_absence_no_adapter_when_no_pci() {
    assert_eq!(classify_adapter_absence(0, false), WifiErrorCode::NoAdapter);
}

// ── validate_psk ──────────────────────────────────────────────────────

#[test]
fn validate_psk_accepts_8_to_63_chars() {
    assert!(validate_psk(&"a".repeat(8)).is_ok());
    assert!(validate_psk(&"a".repeat(63)).is_ok());
    assert!(validate_psk("spike12345").is_ok());
}

#[test]
fn validate_psk_rejects_out_of_range() {
    assert_eq!(
        validate_psk(&"a".repeat(7)),
        Err(WifiErrorCode::WrongPassword)
    );
    assert_eq!(
        validate_psk(&"a".repeat(64)),
        Err(WifiErrorCode::WrongPassword)
    );
    assert_eq!(validate_psk(""), Err(WifiErrorCode::WrongPassword));
}

#[test]
fn validate_psk_counts_chars_not_bytes_for_cjk() {
    // Each CJK character is 3 bytes in UTF-8 — 20 chars is 60 bytes, well
    // inside the byte-length range too, so this alone wouldn't catch a
    // byte-vs-char bug. Use a length where the two disagree instead: 21
    // CJK characters is 63 bytes (in range) but 21 chars is ALSO in
    // range, so the real test is the boundary: 22 CJK chars is valid by
    // char count (22 <= 63) but would be 66 bytes — REJECTED if this
    // function mistakenly counted bytes instead of chars.
    let cjk_22 = "測".repeat(22);
    assert_eq!(cjk_22.chars().count(), 22);
    assert_eq!(cjk_22.len(), 66, "sanity: 22 CJK chars is 66 bytes");
    assert!(
        validate_psk(&cjk_22).is_ok(),
        "22 chars must pass on CHAR count"
    );
}

// ── sort_and_dedup_networks ──────────────────────────────────────────

fn net(ssid: &str, bars: u8) -> WifiNetwork {
    WifiNetwork {
        ssid: ssid.to_string(),
        signal_bars: bars,
        security: "psk".to_string(),
        connected: false,
        known: false,
        hidden: false,
    }
}

#[test]
fn dedup_keeps_the_strongest_signal_per_ssid() {
    let out = sort_and_dedup_networks(vec![net("A", 2), net("B", 4), net("A", 4), net("A", 1)]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].ssid, "A");
    assert_eq!(
        out[0].signal_bars, 4,
        "the strongest A survives, not the first-seen"
    );
    assert_eq!(out[1].ssid, "B");
}

#[test]
fn sort_order_is_signal_desc_then_ssid_asc() {
    let out = sort_and_dedup_networks(vec![net("Zeta", 4), net("Alpha", 4), net("Beta", 2)]);
    let names: Vec<&str> = out.iter().map(|n| n.ssid.as_str()).collect();
    assert_eq!(names, vec!["Alpha", "Zeta", "Beta"]);
}

#[test]
fn dedup_and_sort_handle_empty_and_singleton() {
    assert!(sort_and_dedup_networks(vec![]).is_empty());
    let one = sort_and_dedup_networks(vec![net("Solo", 3)]);
    assert_eq!(one.len(), 1);
}

// ── normalize_station_state ──────────────────────────────────────────

#[test]
fn normalize_station_state_known_values() {
    assert_eq!(normalize_station_state("connected"), "connected");
    assert_eq!(normalize_station_state("roaming"), "connected");
    assert_eq!(normalize_station_state("connecting"), "connecting");
    assert_eq!(normalize_station_state("disconnected"), "disconnected");
    assert_eq!(normalize_station_state("disconnecting"), "disconnected");
}

#[test]
fn normalize_station_state_unknown_degrades_to_disconnected() {
    assert_eq!(normalize_station_state(""), "disconnected");
    assert_eq!(normalize_station_state("some-future-state"), "disconnected");
}

// ── JSON shapes ───────────────────────────────────────────────────────

#[test]
fn scan_result_to_json_shape() {
    let result = ScanResult {
        networks: vec![net("A", 4)],
        scanning: false,
    };
    let v = scan_result_to_json(&result);
    assert_eq!(v["scanning"], false);
    assert_eq!(v["networks"][0]["ssid"], "A");
}

#[test]
fn error_to_json_never_carries_detail() {
    let err = WifiError {
        code: WifiErrorCode::WrongPassword,
        detail: "a technical detail that must never leak".to_string(),
    };
    let v = error_to_json(&err);
    assert_eq!(v["code"], "wrong_password");
    assert_eq!(v["message"], WifiErrorCode::WrongPassword.message());
    assert!(v.get("detail").is_none());
    assert!(!v.to_string().contains("technical detail"));
}

#[test]
fn error_to_json_with_ssid_only_changes_no_ip_and_portal() {
    let ssid = "MyNetwork";
    for code in [
        WifiErrorCode::WrongPassword,
        WifiErrorCode::NotFound,
        WifiErrorCode::OutOfRange,
        WifiErrorCode::NoAdapter,
        WifiErrorCode::DriverMissing,
        WifiErrorCode::BackendUnavailable,
        WifiErrorCode::UnsupportedSecurity,
    ] {
        let err = WifiError {
            code,
            detail: String::new(),
        };
        let v = error_to_json_with_ssid(&err, ssid);
        assert_eq!(
            v["message"],
            code.message(),
            "{code:?} message must be unchanged without an SSID slot"
        );
    }
    for code in [WifiErrorCode::NoIp, WifiErrorCode::Portal] {
        let err = WifiError {
            code,
            detail: String::new(),
        };
        let v = error_to_json_with_ssid(&err, ssid);
        assert!(
            v["message"].as_str().unwrap().contains(ssid),
            "{code:?} message must name the SSID"
        );
    }
}

// ── wire code stability ──────────────────────────────────────────────

#[test]
fn all_nine_error_codes_have_stable_snake_case_wire_values() {
    let expected: &[(WifiErrorCode, &str)] = &[
        (WifiErrorCode::WrongPassword, "wrong_password"),
        (WifiErrorCode::NotFound, "not_found"),
        (WifiErrorCode::OutOfRange, "out_of_range"),
        (WifiErrorCode::NoAdapter, "no_adapter"),
        (WifiErrorCode::DriverMissing, "driver_missing"),
        (WifiErrorCode::NoIp, "no_ip"),
        (WifiErrorCode::Portal, "portal"),
        (WifiErrorCode::BackendUnavailable, "backend_unavailable"),
        (WifiErrorCode::UnsupportedSecurity, "unsupported_security"),
    ];
    for (code, wire) in expected {
        assert_eq!(code.code(), *wire);
    }
}

// ── WifiConnectRequest PSK redaction (hard requirement) ──────────────

#[test]
fn wifi_connect_request_debug_redacts_psk() {
    let req = WifiConnectRequest {
        ssid: "MyNetwork".to_string(),
        psk: Some("super-secret-passphrase-value".to_string()),
    };
    let rendered = format!("{req:?}");
    assert!(
        !rendered.contains("super-secret-passphrase-value"),
        "psk leaked into Debug: {rendered}"
    );
    assert!(rendered.contains("<redacted>"));
    assert!(
        rendered.contains("MyNetwork"),
        "ssid should still render for diagnostics"
    );
}

#[test]
fn wifi_connect_request_debug_handles_no_psk() {
    let req = WifiConnectRequest {
        ssid: "OpenNet".to_string(),
        psk: None,
    };
    let rendered = format!("{req:?}");
    assert!(
        !rendered.contains("<redacted>"),
        "no psk to redact when it was never supplied"
    );
}

#[test]
fn wifi_connect_request_parses_from_json() {
    let v = serde_json::json!({"ssid": "MyNetwork", "psk": "hunter22"});
    let req: WifiConnectRequest = serde_json::from_value(v).unwrap();
    assert_eq!(req.ssid, "MyNetwork");
    assert_eq!(req.psk.as_deref(), Some("hunter22"));

    let v_no_psk = serde_json::json!({"ssid": "OpenNet"});
    let req2: WifiConnectRequest = serde_json::from_value(v_no_psk).unwrap();
    assert_eq!(
        req2.psk, None,
        "psk is optional (open network / stored credential)"
    );
}

// ── unavailable_link / non-Linux facade ───────────────────────────────

#[test]
fn unavailable_link_shape() {
    let link = unavailable_link();
    assert_eq!(link.state, "unavailable");
    assert!(link.ssid.is_none() && link.signal_bars.is_none() && link.security.is_none());
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn non_linux_facade_reports_backend_unavailable_never_panics() {
    assert_eq!(
        wifi_scan(true).await.unwrap_err().code,
        WifiErrorCode::BackendUnavailable
    );
    assert_eq!(
        wifi_connect("x", None).await.unwrap_err().code,
        WifiErrorCode::BackendUnavailable
    );
    assert_eq!(
        wifi_forget("x").await.unwrap_err().code,
        WifiErrorCode::BackendUnavailable
    );
    // `status()` must still succeed off-Linux — a Mac dev machine running
    // the gateway must not crash on this call.
    let status = status().await.unwrap();
    assert_eq!(status.wifi.state, "unavailable");
}
