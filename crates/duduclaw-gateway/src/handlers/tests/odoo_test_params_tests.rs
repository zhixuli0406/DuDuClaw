//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

fn full_params() -> Value {
    json!({
        "url": "https://fc00.example.com/odoo",
        "db": "odoo_demo",
        "protocol": "jsonrpc",
        "auth_method": "api_key",
        "username": "admin",
        "api_key": "secret-key",
    })
}

#[test]
fn happy_path_returns_config_and_credential() {
    let (cfg, cred) = MethodHandler::build_test_config_from_params(&full_params()).unwrap();
    assert_eq!(cfg.url, "https://fc00.example.com/odoo");
    assert_eq!(cfg.db, "odoo_demo");
    assert_eq!(cfg.protocol, "jsonrpc");
    assert_eq!(cfg.auth_method, "api_key");
    assert_eq!(cfg.username, "admin");
    assert!(cfg.is_configured());
    assert_eq!(cred.as_deref(), Some("secret-key"));
}

#[test]
fn missing_url_is_rejected() {
    let p = json!({ "db": "x" });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(err.contains("url"), "got: {err}");
}

#[test]
fn missing_db_is_rejected() {
    let p = json!({ "url": "https://x.example.com" });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(err.contains("db"), "got: {err}");
}

#[test]
fn http_non_localhost_url_is_rejected() {
    // SSRF guard: only HTTPS, except for strict localhost.
    let p = json!({ "url": "http://evil.example.com", "db": "x" });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(err.contains("HTTPS"), "got: {err}");
}

#[test]
fn private_ip_is_rejected() {
    let p = json!({ "url": "https://10.0.0.1", "db": "x" });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(
        err.contains("HTTPS") || err.contains("localhost"),
        "got: {err}"
    );
}

#[test]
fn fc00_dotted_hostname_is_not_misclassified_as_ipv6_ula() {
    // Regression: `fc00.example.com` shares the IPv6 ULA prefix label but is
    // a domain — must not be rejected as a private IP.
    let p = json!({
        "url": "https://fc00.example.com",
        "db": "test",
    });
    let res = MethodHandler::build_test_config_from_params(&p);
    assert!(res.is_ok(), "got: {res:?}");
}

#[test]
fn invalid_db_name_is_rejected() {
    let p = json!({ "url": "https://x.example.com", "db": "bad name!" });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(err.contains("database name"), "got: {err}");
}

#[test]
fn invalid_protocol_is_rejected() {
    let p = json!({
        "url": "https://x.example.com",
        "db": "x",
        "protocol": "soap",
    });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(err.contains("protocol"), "got: {err}");
}

#[test]
fn invalid_auth_method_is_rejected() {
    let p = json!({
        "url": "https://x.example.com",
        "db": "x",
        "auth_method": "oauth",
    });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(err.contains("auth_method"), "got: {err}");
}

#[test]
fn missing_credential_returns_none_for_fallback() {
    // Caller intentionally omits the credential field → handler should
    // fall back to stored credential. Helper returns Ok with `None`.
    let p = json!({
        "url": "https://x.example.com",
        "db": "x",
        "auth_method": "api_key",
    });
    let (_cfg, cred) = MethodHandler::build_test_config_from_params(&p).unwrap();
    assert!(cred.is_none());
}

#[test]
fn empty_credential_string_treated_as_missing() {
    let p = json!({
        "url": "https://x.example.com",
        "db": "x",
        "api_key": "",
    });
    let (_cfg, cred) = MethodHandler::build_test_config_from_params(&p).unwrap();
    assert!(cred.is_none());
}

#[test]
fn password_auth_method_picks_password_field() {
    let p = json!({
        "url": "https://x.example.com",
        "db": "x",
        "auth_method": "password",
        "password": "pw",
        "api_key": "should-be-ignored",
    });
    let (cfg, cred) = MethodHandler::build_test_config_from_params(&p).unwrap();
    assert_eq!(cfg.auth_method, "password");
    assert_eq!(cred.as_deref(), Some("pw"));
}

#[test]
fn username_over_256_chars_is_rejected() {
    let long = "a".repeat(257);
    let p = json!({
        "url": "https://x.example.com",
        "db": "x",
        "username": long,
    });
    let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
    assert!(err.contains("Username"), "got: {err}");
}

#[test]
fn localhost_http_is_allowed_for_dev() {
    let p = json!({ "url": "http://127.0.0.1:8069", "db": "dev" });
    let res = MethodHandler::build_test_config_from_params(&p);
    assert!(res.is_ok(), "got: {res:?}");
}

#[test]
fn scrub_long_error_truncates_with_ellipsis() {
    let long_err = "x".repeat(300);
    let out = MethodHandler::scrub_odoo_error(&long_err);
    assert!(
        out.chars().count() <= 241,
        "got len {}",
        out.chars().count()
    );
    assert!(out.ends_with('…'));
}

#[test]
fn scrub_short_error_unchanged() {
    let short = "401 Unauthorized";
    let out = MethodHandler::scrub_odoo_error(short);
    assert_eq!(out, short);
}

// ── M19: scrubbing must strip URLs/credentials even on SHORT errors ──

#[test]
fn scrub_short_error_removes_url_query_string() {
    // The reqwest error echoes the full URL incl. a leaking query string.
    let err =
        "error sending request for url (https://erp.example.com/jsonrpc?api_key=topsecret)";
    let out = MethodHandler::scrub_odoo_error(err);
    assert!(!out.contains("topsecret"), "token leaked: {out}");
    assert!(
        !out.contains("api_key=topsecret"),
        "query string leaked: {out}"
    );
    // Host is retained so the user can still act on the error.
    assert!(out.contains("erp.example.com"), "host should remain: {out}");
}

#[test]
fn scrub_short_error_removes_url_userinfo() {
    let err = "connect failed: https://admin:hunter2@erp.example.com/odoo";
    let out = MethodHandler::scrub_odoo_error(err);
    assert!(!out.contains("hunter2"), "password leaked: {out}");
    assert!(!out.contains("admin:"), "userinfo leaked: {out}");
    assert!(out.contains("erp.example.com"), "host should remain: {out}");
}

#[test]
fn scrub_short_error_redacts_credential_kv() {
    let err = "auth rejected token=abc123 password=p@ss";
    let out = MethodHandler::scrub_odoo_error(err);
    assert!(!out.contains("abc123"), "token leaked: {out}");
    assert!(!out.contains("p@ss"), "password leaked: {out}");
    assert!(
        out.contains("[REDACTED]"),
        "expected redaction marker: {out}"
    );
}
