//! System security mail. SMTP takes precedence over the licensed code-only relay.
use std::{path::Path, time::Duration};
use duduclaw_auth::UserDb;
use sha2::{Digest, Sha256};

pub(crate) const UNAVAILABLE: &str = "伺服器未設定寄信，且此安裝沒有可用的授權代寄。請管理員設定 SMTP 寄件伺服器與寄件地址。";
const DELIVERY_FAILED: &str = "驗證碼無法寄出，請稍後再試或聯絡管理員。";
pub(crate) const RATE_LIMITED: &str = "寄送太頻繁，請稍後再試。";
const RELAY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 4096;

fn relay_enabled(home: &Path) -> bool {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    relay_config_enabled(&text)
}

fn relay_config_enabled(text: &str) -> bool {
    let Ok(config) = text.parse::<toml::Value>() else { return false; };
    match config.get("mfa") {
        None => true,
        Some(mfa) if mfa.is_table() => match mfa.get("email_relay") {
            None => true,
            Some(value) => value.as_bool() == Some(true),
        },
        _ => false,
    }
}

fn availability_reason(smtp: bool, relay_on: bool, relay_eligible: bool) -> Option<&'static str> {
    if smtp || (relay_on && relay_eligible) { None }
    else if !relay_on { Some("relay_disabled") }
    else { Some("no_smtp_no_relay") }
}

pub(crate) fn unavailable_reason(home: &Path) -> Option<&'static str> {
    let smtp = crate::mail_worker::resolve_smtp_config(home).is_some();
    let enabled = relay_enabled(home);
    availability_reason(smtp, enabled, !smtp && enabled && crate::license_runtime::mfa_relay_license(home).is_some())
}

pub(crate) fn available(home: &Path) -> bool { unavailable_reason(home).is_none() }

// Deliberately no Debug/Display implementation: this object contains a live OTP
// and signed license. Neither requests nor remote error bodies enter logs.
struct RelayRequest {
    endpoint: String,
    body: serde_json::Value,
}
impl RelayRequest {
    fn new(license: serde_json::Value, fingerprint: String, endpoint: String, email: &str, code: &str) -> Self {
        Self { endpoint, body: serde_json::json!({
            "license": license, "machine_fingerprint": fingerprint,
            "nonce": uuid::Uuid::new_v4().simple().to_string(),
            "issued_at": chrono::Utc::now().timestamp(), "recipient": email,
            "code": code, "locale": "zh-TW",
        }) }
    }
}

fn relay_response(status: u16, body: &[u8]) -> Result<(), &'static str> {
    if status == 429 { return Err(RATE_LIMITED); }
    if body.len() > MAX_RESPONSE_BYTES { return Err(DELIVERY_FAILED); }
    if status == 202 && serde_json::from_slice::<serde_json::Value>(body).ok()
        .and_then(|value| value.get("sent").and_then(|v| v.as_bool())) == Some(true) {
        Ok(())
    } else { Err(DELIVERY_FAILED) }
}

async fn send_relay(home: &Path, email: &str, code: &str) -> Result<(), &'static str> {
    // Recheck policy and license at delivery, even if availability was checked earlier.
    if !relay_enabled(home) { return Err(DELIVERY_FAILED); }
    let home = home.to_path_buf();
    // Includes license lookup, DNS, connect, send and bounded response read.
    tokio::time::timeout(RELAY_TIMEOUT, async {
        let (license, fingerprint, endpoint) = tokio::task::spawn_blocking(move ||
            crate::license_runtime::mfa_relay_license(&home)).await.ok().flatten().ok_or(DELIVERY_FAILED)?;
        let request = RelayRequest::new(license, fingerprint, endpoint, email, code);
        let transport = crate::license_transport::request(reqwest::Method::POST, &request.endpoint, RELAY_TIMEOUT)
            .await.map_err(|_| DELIVERY_FAILED)?;
        relay_exchange(transport, &request.body).await
    }).await.map_err(|_| DELIVERY_FAILED)?
}

async fn relay_exchange(transport: reqwest::RequestBuilder, payload: &serde_json::Value) -> Result<(), &'static str> {
    let mut response = transport.json(payload).send().await.map_err(|_| DELIVERY_FAILED)?;
    let status = response.status().as_u16();
    if status == 429 { return Err(RATE_LIMITED); }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| DELIVERY_FAILED)? {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) { return Err(DELIVERY_FAILED); }
        body.extend_from_slice(&chunk);
    }
    relay_response(status, &body)
}

fn finish_delivery(db: &UserDb, token: &str, email: &str, code: &str, result: Result<(), &'static str>) -> Result<(), String> {
    if let Err(message) = result {
        // Only a normalized recipient hash and fixed outcome enter the audit log.
        tracing::warn!(recipient_hash = %hex::encode(Sha256::digest(email.trim().to_lowercase().as_bytes())),
            outcome = if message == RATE_LIMITED { "rate_limited" } else { "delivery_failed" }, "MFA email delivery failed");
        db.mfa_email_delivery_failed(token, code)?;
        return Err(message.into());
    }
    Ok(())
}

pub(crate) async fn send_code(home: &Path, db: &UserDb, token: &str, email: &str, code: &str) -> Result<(), String> {
    let result = if let Some(config) = crate::mail_worker::resolve_smtp_config(home) {
        crate::email::send_email(&config, email, "DuDuClaw Email 驗證碼",
            &format!("您的兩步驟驗證碼為：{code}\n\n此驗證碼於 10 分鐘後失效，且只能使用一次。若您未要求此驗證碼，請忽略這封信。"))
            .await.map_err(|_| DELIVERY_FAILED)
    } else { send_relay(home, email, code).await };
    finish_delivery(db, token, email, code, result)
}

/// A notification failure does not undo an already committed security change.
/// Record that failure without leaking the destination or SMTP credentials.
pub(crate) fn notify_change(home: &Path, db: &std::sync::Arc<UserDb>, user: &str, action: &str) {
    let home=home.to_path_buf();
    let db=db.clone();
    let user=user.to_owned();
    let action=action.to_owned();
    // Deliver after returning the committed replacement session. SMTP must not
    // hold the only copy of recovery codes past the dashboard RPC timeout.
    tokio::spawn(async move {
    let Some(config)=crate::mail_worker::resolve_smtp_config(&home) else {return;};
    let Ok(Some(account))=db.get_user(&user) else {return;};
    let body=format!("您的 DuDuClaw 帳號兩步驟驗證設定已變更：{action}。\n\n若這不是您本人操作，請立即變更密碼並聯絡管理員。");
    if crate::email::send_email(&config,&account.email,"DuDuClaw 帳號安全通知",&body).await.is_err() {
        let _=db.log_action(Some(&user),"mfa.notification_failed",Some(&user),None,None);
        tracing::warn!("MFA security notification delivery failed");
    }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, sync::{Arc, Mutex}};

    #[test]
    fn availability_matrix_and_config_fail_closed() {
        for smtp in [false, true] {
            for enabled in [false, true] {
                for eligible in [false, true] {
                    assert_eq!(availability_reason(smtp, enabled, eligible).is_none(), smtp || (enabled && eligible));
                }
            }
        }
        assert_eq!(availability_reason(false, true, false), Some("no_smtp_no_relay"));
        assert_eq!(availability_reason(false, false, true), Some("relay_disabled"));
        for text in ["", "[mfa]\n", "[mfa]\nemail_relay = true"] { assert!(relay_config_enabled(text)); }
        for text in ["[mfa]\nemail_relay = false", "[mfa]\nemail_relay = 'true'", "mfa = false", "[invalid"] {
            assert!(!relay_config_enabled(text));
        }
        let home = tempfile::tempdir().unwrap();
        assert!(relay_enabled(home.path()));
        assert_eq!(unavailable_reason(home.path()), Some("no_smtp_no_relay"));
        std::fs::write(home.path().join("config.toml"), "[mfa]\nemail_relay = false").unwrap();
        assert_eq!(unavailable_reason(home.path()), Some("relay_disabled"));
        std::fs::write(home.path().join("config.toml"), "[mfa]\nemail_relay = false\n[channels.email]\nsmtp_host = 'smtp.example.com'\nfrom_addr = 'security@example.com'").unwrap();
        assert!(available(home.path()), "SMTP remains available with relay off and no license");
    }

    #[test]
    fn relay_request_preserves_artifact_and_uses_fresh_nonce_and_time() {
        let license = serde_json::json!({"signature":"opaque-signature", "unknown":{"retain":true}});
        let build = || RelayRequest::new(license.clone(), "validated-fingerprint".into(),
            "https://api.duduclaw.dudustudio.monster/api/v1/relay/mfa-email".into(), "mfa@example.com", "123456");
        let before = chrono::Utc::now().timestamp();
        let a = build(); let b = build();
        assert_eq!(a.body["license"], license);
        assert_eq!(a.body["machine_fingerprint"], "validated-fingerprint");
        assert_eq!(a.body["recipient"], "mfa@example.com");
        assert_eq!(a.body["code"], "123456");
        assert_eq!(a.body["locale"], "zh-TW");
        assert_eq!(a.body.as_object().unwrap().len(), 7);
        let nonce = a.body["nonce"].as_str().unwrap();
        assert_eq!(nonce.len(), 32);
        assert!(nonce.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
        assert_ne!(a.body["nonce"], b.body["nonce"]);
        assert!((before..=chrono::Utc::now().timestamp()).contains(&a.body["issued_at"].as_i64().unwrap()));
    }

    #[tokio::test]
    async fn relay_http_request_and_response_stream_are_wired_to_delivery_bookkeeping() {
        use axum::{Router, routing::post, response::IntoResponse};
        for (status, response_body, expected) in [
            (202, br#"{"sent":true}"#.to_vec(), None),
            (429, br#"{"error":"rate_limited","retry_after":60}"#.to_vec(), Some(RATE_LIMITED)),
            (403, br#"{"error":"relay_not_allowed"}"#.to_vec(), Some(DELIVERY_FAILED)),
            (503, br#"{"error":"relay_unavailable"}"#.to_vec(), Some(DELIVERY_FAILED)),
            (202, vec![b' '; MAX_RESPONSE_BYTES+1], Some(DELIVERY_FAILED)),
            (0, Vec::new(), Some(DELIVERY_FAILED)),
        ] {
            let received = Arc::new(Mutex::new(None));
            let captured = received.clone();
            let app = Router::new().route("/api/v1/relay/mfa-email", post(move |request: axum::extract::Request| {
                let captured = captured.clone();
                let response_body = response_body.clone();
                async move {
                    let method = request.method().clone();
                    let path = request.uri().path().to_owned();
                    let bytes = axum::body::to_bytes(request.into_body(), 16384).await.unwrap();
                    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    *captured.lock().unwrap() = Some((method, path, json));
                    if status == 0 { tokio::time::sleep(Duration::from_millis(200)).await; }
                    (axum::http::StatusCode::from_u16(if status == 0 {503} else {status}).unwrap(), response_body).into_response()
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
            let dir = tempfile::tempdir().unwrap();
            let db = UserDb::new(&dir.path().join("users.db")).unwrap();
            let user = db.create_user("relay-http@example.com", "MFA", "password-test", duduclaw_auth::UserRole::Admin).unwrap();
            let token = db.mfa_factor_password_challenge(&user.id, "password-test", "session", 1000).unwrap();
            let (email, code) = db.mfa_email_send(&token, true, 1000).unwrap();
            let request = RelayRequest::new(serde_json::json!({"signature":"signed-artifact"}),
                "validated-machine".into(), format!("http://{address}/api/v1/relay/mfa-email"), &email, &code);
            // Test only: production always obtains the builder from license_transport.
            let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
                .timeout(if status == 0 {Duration::from_millis(100)} else {RELAY_TIMEOUT}).build().unwrap();
            let result = tokio::time::timeout(RELAY_TIMEOUT,
                relay_exchange(client.post(&request.endpoint), &request.body)).await.unwrap();
            assert_eq!(finish_delivery(&db, &token, &email, &code, result).err().as_deref(), expected);
            assert_eq!(db.mfa_email_verify_and_issue(&token, &code, true, 1001, |_,_,_|Ok(())).is_ok(), expected.is_none());
            let (method, path, payload) = received.lock().unwrap().take().unwrap();
            assert_eq!(method, reqwest::Method::POST);
            assert_eq!(path, "/api/v1/relay/mfa-email");
            assert_eq!(payload, request.body);
            server.abort();
        }
    }

    #[derive(Clone)]
    struct LogCapture(Arc<Mutex<Vec<u8>>>);
    impl Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    #[test]
    fn relay_responses_invalidate_failed_codes_without_logging_secrets() {
        for (status, body, expected) in [
            (202, br#"{"sent":true}"#.as_slice(), None),
            (429, br#"{"error":"rate_limited","retry_after":60}"#.as_slice(), Some(RATE_LIMITED)),
            (403, br#"{"error":"relay_not_allowed"}"#.as_slice(), Some(DELIVERY_FAILED)),
            (503, br#"{"error":"relay_unavailable"}"#.as_slice(), Some(DELIVERY_FAILED)),
            (202, br#"{"sent":false}"#.as_slice(), Some(DELIVERY_FAILED)),
            (202, b"invalid-json".as_slice(), Some(DELIVERY_FAILED)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let db = UserDb::new(&dir.path().join("users.db")).unwrap();
            let user = db.create_user("private-recipient@example.com", "MFA", "password-test", duduclaw_auth::UserRole::Admin).unwrap();
            let token = db.mfa_factor_password_challenge(&user.id, "password-test", "session", 1000).unwrap();
            let (email, code) = db.mfa_email_send(&token, true, 1000).unwrap();
            let log = LogCapture(Arc::new(Mutex::new(Vec::new())));
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
                .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
            let result = tracing::subscriber::with_default(subscriber, ||
                finish_delivery(&db, &token, &email, &code, relay_response(status, body)));
            assert_eq!(result.as_ref().err().map(String::as_str), expected);
            let verified = db.mfa_email_verify_and_issue(&token, &code, true, 1001, |_,_,_|Ok(()));
            assert_eq!(verified.is_ok(), expected.is_none());
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert!(!logs.contains(&email));
            // Remove the allowed digest before checking a random six-digit OTP;
            // its digits could coincidentally occur in a SHA-256 hex string.
            let digest = hex::encode(Sha256::digest(email.trim().to_lowercase().as_bytes()));
            assert!(!logs.replace(&digest, "<recipient-hash>").contains(&code));
            assert!(!logs.contains(&token));
            if expected.is_some() { assert!(logs.contains("recipient_hash=")); }
        }
        assert_eq!(relay_response(202, &vec![b' '; MAX_RESPONSE_BYTES+1]), Err(DELIVERY_FAILED));
        for status in [200, 301, 302, 400, 500] {
            assert_eq!(relay_response(status, br#"{"sent":true}"#), Err(DELIVERY_FAILED));
        }
    }
}
