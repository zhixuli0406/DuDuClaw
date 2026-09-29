//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// The Odoo tab holds the same shape of write-only credentials as the OAuth
/// providers, so it gets the same guarantee, asserted the same way: the config
/// read path proves what is stored without ever shipping the stored value.
///
/// Symmetric with `google_credentials_display_tests` — the whole serialized
/// response is scanned, not individual fields, because a leak would most likely
/// arrive through a field nobody thought to assert on.
use super::*;

const API_KEY: &str = "odoo-api-key-8e41f7c2";
const PASSWORD: &str = "odoo-password-3b9d0a";
const WEBHOOK_SECRET: &str = "odoo-webhook-secret-77c1";

/// A config.toml with all three credentials stored, in the encrypted-at-rest
/// shape `odoo.configure` writes.
async fn handler_with_stored_credentials() -> (tempfile::TempDir, MethodHandler) {
    let home = tempfile::tempdir().unwrap();
    let enc = |v: &str| {
        crate::config_crypto::encrypt_value(v, home.path())
            .expect("encryption keyfile is writable in a temp home")
    };
    let config = format!(
        r#"
[odoo]
url = "https://mycompany.odoo.com"
db = "mycompany"
protocol = "jsonrpc"
auth_method = "api_key"
username = "admin@mycompany.com"
api_key_enc = "{}"
password_enc = "{}"
webhook_enabled = true
webhook_secret_enc = "{}"
"#,
        enc(API_KEY),
        enc(PASSWORD),
        enc(WEBHOOK_SECRET),
    );
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    (home, handler)
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok, got {other:?}"),
    }
}

#[tokio::test]
async fn odoo_config_reports_stored_credentials_without_returning_them() {
    let (_home, handler) = handler_with_stored_credentials().await;
    let body = payload(handler.handle_odoo_config().await);

    assert_eq!(body["has_api_key"], json!(true));
    assert_eq!(body["has_password"], json!(true));
    assert_eq!(body["has_webhook_secret"], json!(true));

    // Whole-payload scan: no credential, in plaintext or ciphertext, in any
    // field. The encrypted forms are checked too — they are still the
    // credential, just wrapped, and the browser has no business holding one.
    let raw = serde_json::to_string(&body).unwrap();
    for secret in [API_KEY, PASSWORD, WEBHOOK_SECRET] {
        assert!(
            !raw.contains(secret),
            "odoo credential leaked into the dashboard payload: {raw}"
        );
    }
    for key in ["api_key", "password", "webhook_secret"] {
        assert!(
            body.get(key).is_none() && body.get(format!("{key}_enc")).is_none(),
            "`{key}` must not be a field of the config response: {raw}"
        );
    }
}

#[tokio::test]
async fn odoo_config_reports_absent_credentials_as_not_stored() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[odoo]\nurl = \"https://mycompany.odoo.com\"\ndb = \"mycompany\"\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let body = payload(handler.handle_odoo_config().await);

    // Distinguishing these two cases is the entire point — without it the
    // form shows identical dots whether or not anything was ever saved.
    assert_eq!(body["has_api_key"], json!(false));
    assert_eq!(body["has_password"], json!(false));
    assert_eq!(body["has_webhook_secret"], json!(false));
}
