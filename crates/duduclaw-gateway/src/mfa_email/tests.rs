use super::*;
use std::{io::Write, sync::{Arc, Mutex}};
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
                finish_delivery(dir.path(), &db, &token, &email, &code, relay::sent(&transport::Reply::new(status, body))));
            assert_eq!(result.as_ref().err().map(String::as_str), expected);
            let verified = db.mfa_email_verify_and_issue(&token, &code, true, 1001, |_,_,_|Ok(()));
            assert_eq!(verified.is_ok(), expected.is_none());
            let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
            assert!(!logs.contains(&email));
            // Remove the allowed digest before checking a random six-digit OTP;
            // its digits could coincidentally occur in a SHA-256 hex string.
            let digest = recipient_digest(dir.path(), &email);
            assert!(!logs.replace(&digest, "<recipient-hash>").contains(&code));
            assert!(!logs.contains(&token));
            if expected.is_some() { assert!(logs.contains("recipient_hash=")); }
        }
        assert_eq!(relay::sent(&transport::Reply::new(202, &[b' ';4097])), Err(DELIVERY_FAILED));
        for status in [200, 301, 302, 400, 500] {
            assert_eq!(relay::sent(&transport::Reply::new(status, br#"{"sent":true}"#)), Err(DELIVERY_FAILED));
        }
    }

#[test]
fn availability_matrix_and_config_fail_closed() {
    for smtp in [false, true] {
        for enabled in [false, true] {
            for eligibility in [Ok(()), Err("no_smtp"), Err("relay_nfr"), Err("relay_unbound"),
                Err("relay_key_unbound"), Err("relay_key_mismatch")] {
                assert_eq!(availability_reason(smtp, enabled, eligibility).is_none(), smtp || enabled && eligibility.is_ok());
                if !smtp && enabled { assert_eq!(availability_reason(smtp, enabled, eligibility), eligibility.err()); }
            }
        }
    }
    assert_eq!(availability_reason(false, false, Ok(())), Some("relay_disabled"));
    for text in ["", "[mfa]\n", "[mfa]\nemail_relay = true"] { assert!(relay_config_enabled(text)); }
    for text in ["[mfa]\nemail_relay = false", "[mfa]\nemail_relay = 'true'", "mfa = false", "[invalid"] {
        assert!(!relay_config_enabled(text));
    }
    let home = tempfile::tempdir().unwrap();
    assert!(relay_enabled(home.path()));
    assert_eq!(unavailable_reason(home.path()), Some("no_smtp"));
    std::fs::write(home.path().join("config.toml"), "[mfa]\nemail_relay = false").unwrap();
    assert_eq!(unavailable_reason(home.path()), Some("relay_disabled"));
    std::fs::write(home.path().join("config.toml"), "[mfa]\nemail_relay = false\n[channels.email]\nsmtp_host = 'smtp.example.com'\nfrom_addr = 'security@example.com'").unwrap();
    assert!(available(home.path()));
}

#[test]
fn smtp_availability_has_only_a_safe_reason_code() {
    // Released v1.80 contract: SMTP needs host and sender; anything else
    // (including invalid TOML) reports a safe code, never configuration text.
    let home = tempfile::tempdir().unwrap();
    assert_eq!(unavailable_reason(home.path()), Some("no_smtp"));
    std::fs::write(home.path().join("config.toml"), "[channels.email]\nsmtp_host='127.0.0.1'\n").unwrap();
    assert_eq!(unavailable_reason(home.path()), Some("no_smtp"));
    std::fs::write(home.path().join("config.toml"), "[channels.email]\nsmtp_host='127.0.0.1'\nfrom_addr='sender@example.test'\n").unwrap();
    assert_eq!(unavailable_reason(home.path()), None);
    std::fs::write(home.path().join("config.toml"), "invalid toml").unwrap();
    assert_eq!(unavailable_reason(home.path()), Some("relay_disabled"));
}

#[test]
fn unbound_or_mismatched_key_never_opens_the_relay_store() {
    let home = tempfile::tempdir().unwrap();
    for reason in ["relay_key_unbound", "relay_key_mismatch"] {
        assert_eq!(prepare_relay(home.path(), |_| Err(reason)).err(), Some(DELIVERY_FAILED));
        assert!(!home.path().join("state/mfa-relay.lock").exists());
    }
}

#[test]
fn recipient_audit_is_keyed_normalized_domain_separated_or_unavailable() {
    use hmac::{Hmac, Mac};
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("trigger_audit.key"), [42u8;32]).unwrap();
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&[42u8;32]).unwrap();
    mac.update(b"mfa-relay-recipient\0user@example.com");
    let expected = hex::encode(mac.finalize().into_bytes());
    assert_eq!(recipient_digest(home.path(), " User@Example.com "), expected);
    assert_ne!(crate::trigger_route::access::keyed_digest_full(home.path(), "other-domain", "user@example.com"), expected);
    let other = tempfile::tempdir().unwrap();
    assert_ne!(recipient_digest(other.path(), "user@example.com"), expected);
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("trigger_audit.key"), b"broken").unwrap();
    assert_eq!(recipient_digest(broken.path(), "user@example.com"), "unavailable");
}

#[tokio::test]
async fn bounded_http_responses_and_timeouts_preserve_delivery_bookkeeping() {
    use axum::{Router, routing::post, response::IntoResponse};
    use std::time::Duration;
    for (status, response_body, expected) in [
        (202, br#"{"sent":true}"#.to_vec(), None),
        (429, br#"{"error":"rate_limited","retry_after":60}"#.to_vec(), Some(RATE_LIMITED)),
        (403, br#"{"error":"relay_not_allowed"}"#.to_vec(), Some(DELIVERY_FAILED)),
        (503, br#"{"error":"relay_unavailable"}"#.to_vec(), Some(DELIVERY_FAILED)),
        (302, Vec::new(), Some(DELIVERY_FAILED)),
        (202, vec![b' ';4097], Some(DELIVERY_FAILED)),
        (0, Vec::new(), Some(DELIVERY_FAILED)),
    ] {
        let app = Router::new().route("/api/v1/relay/mfa-email", post(move || {
            let response_body = response_body.clone();
            async move {
                if status == 0 { tokio::time::sleep(Duration::from_millis(200)).await; }
                let status = axum::http::StatusCode::from_u16(if status == 0 {503} else {status}).unwrap();
                (status, [("location", "http://127.0.0.1:1/never-follow")], response_body).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let home = tempfile::tempdir().unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("relay-http@example.com", "MFA", "password-test", duduclaw_auth::UserRole::Admin).unwrap();
        let token = db.mfa_factor_password_challenge(&user.id, "password-test", "session", 1000).unwrap();
        let (email, code) = db.mfa_email_send(&token, true, 1000).unwrap();
        // Only the bounded reader is exercised against loopback; production
        // always obtains the endpoint and client from the official transport.
        let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(if status == 0 {100} else {1000})).build().unwrap();
        let result = transport::read_response(client.post(format!("http://{address}/api/v1/relay/mfa-email"))).await
            .and_then(|reply| relay::sent(&reply));
        assert_eq!(finish_delivery(home.path(), &db, &token, &email, &code, result).err().as_deref(), expected);
        assert_eq!(db.mfa_email_verify_and_issue(&token, &code, true, 1001, |_,_,_|Ok(())).is_ok(), expected.is_none());
        server.abort();
    }
}
