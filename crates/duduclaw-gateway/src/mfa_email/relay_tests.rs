use super::*;
use std::{collections::VecDeque, sync::{Arc, Mutex}};

fn credential(id: &str) -> Credential { Credential { credential_id: id.into(), secret: hex::encode([7u8;32]) } }
fn accepted() -> Reply { Reply { status: 202, body: br#"{"sent":true}"#.to_vec() } }
fn revoked() -> Reply { Reply { status: 403, body: br#"{"error":"relay_not_allowed"}"#.to_vec() } }
fn enrolled(id: &str) -> Reply { Reply { status: 201, body: serde_json::to_vec(&credential(id)).unwrap() } }

#[test]
fn signature_matches_independent_known_answer_and_exact_bytes() {
    let key: Vec<u8> = (0..32).collect();
    let body = br#"{"recipient":"mfa@example.com","code":"123456","locale":"zh-TW"}"#;
    assert_eq!(signature(&key, "1700000000", "0123456789abcdef0123456789abcdef", body),
        "5bff9d48a2cebf6c5a29333e977305bfcc9cbc18ce09e70cb53b3b4a1ce9bc0b");
    assert_ne!(signature(&key, "1700000000", "nonce", body), signature(&key, "1700000001", "nonce", body));
    assert_ne!(signature(&key, "1700000000", "nonce", body), signature(&key, "1700000000", "nonce", b"{}"));
}

#[test]
fn send_wire_contains_only_code_recipient_locale_and_signed_headers() {
    let c = credential("cred-1");
    let a = send_call(&c, "mfa@example.com", "123456", 1700000000).unwrap();
    let b = send_call(&c, "mfa@example.com", "123456", 1700000000).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&a.body).unwrap();
    assert_eq!(body, serde_json::json!({"recipient":"mfa@example.com","code":"123456","locale":"zh-TW"}));
    assert_eq!(a.headers.len(), 4);
    assert_eq!(a.headers["x-relay-credential"], "cred-1");
    assert_eq!(a.headers["x-relay-timestamp"], "1700000000");
    assert_ne!(a.headers["x-relay-nonce"], b.headers["x-relay-nonce"]);
    let nonce = a.headers["x-relay-nonce"].to_str().unwrap();
    assert_eq!(nonce.len(), 32);
    assert_eq!(a.headers["x-relay-signature"].to_str().unwrap(), signature(&c.key().unwrap(), "1700000000", nonce, &a.body));
    assert!(a.headers.values().all(|value| value.is_sensitive()));
}

#[test]
fn enrollment_wire_retains_signed_license_and_has_no_recipient_or_otp() {
    let state = EnrollmentState::new();
    let license = serde_json::json!({"signature":"opaque","future":{"keep":true},"control_url":"https://attacker.example.com"});
    let a = enroll_call(&license, "binding", &state, 1700000000).unwrap();
    let b = enroll_call(&license, "binding", &state, 1700000000).unwrap();
    assert_eq!(a.endpoint.path(), "/api/v1/relay/enroll");
    let body: serde_json::Value = serde_json::from_slice(&a.body).unwrap();
    let other: serde_json::Value = serde_json::from_slice(&b.body).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 5);
    assert_eq!(body["license"], license);
    assert_eq!(body["machine_fingerprint"], "binding");
    assert_eq!(body["installation_id"], state.installation_id);
    assert_eq!(body["issued_at"], 1700000000i64);
    assert_ne!(body["nonce"], other["nonce"]);
    assert!(a.headers.is_empty());
}

fn mock(replies: Vec<Reply>) -> (Arc<Mutex<Vec<Call>>>, impl FnMut(Call) -> std::future::Ready<Result<Reply, &'static str>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let mut replies = VecDeque::from(replies);
    (calls, move |call| {
        seen.lock().unwrap().push(call);
        std::future::ready(Ok(replies.pop_front().expect("unexpected extra relay request")))
    })
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn lazy_enroll_persists_and_reuses_credential_across_restarts() {
    let home = tempfile::tempdir().unwrap();
    assert!(!home.path().join("state/mfa-relay.json").exists());
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![enrolled("first"), accepted()]);
    assert!(deliver(&store, serde_json::json!({"signature":"signed"}), "fp", "x@y.test", "123456", 10000, exchange).await.is_ok());
    assert_eq!(calls.lock().unwrap().len(), 2);
    let installation_id = store.state().unwrap().installation_id;
    drop(store);
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![accepted()]);
    assert!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 10001, exchange).await.is_ok());
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0].headers["x-relay-credential"], "first");
    assert_eq!(store.state().unwrap().installation_id, installation_id);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn revoked_credential_rotates_once_and_budget_survives_restart() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    store.save_credential(&credential("old")).unwrap();
    let (calls, exchange) = mock(vec![revoked(), enrolled("new"), accepted()]);
    assert!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 10000, exchange).await.is_ok());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].headers["x-relay-credential"], "old");
    assert_eq!(calls[1].endpoint.path(), "/api/v1/relay/enroll");
    assert_eq!(calls[2].headers["x-relay-credential"], "new");
    drop(store);
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![revoked()]);
    assert_eq!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 13599, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 1);
    let (calls, exchange) = mock(vec![revoked(), enrolled("third"), revoked()]);
    assert_eq!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 13600, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 3, "second rejection never loops");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn failed_initial_enrollment_is_bounded_and_generic_403_does_not_rotate() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let (_, exchange) = mock(vec![Reply { status: 503, body: b"private remote diagnostic".to_vec() }]);
    assert_eq!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 10000, exchange).await, Err(DELIVERY_FAILED));
    let (calls, exchange) = mock(vec![]);
    assert_eq!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 10001, exchange).await, Err(DELIVERY_FAILED));
    assert!(calls.lock().unwrap().is_empty());
    store.save_credential(&credential("existing")).unwrap();
    let (calls, exchange) = mock(vec![Reply { status: 403, body: br#"{"error":"other"}"#.to_vec() }]);
    assert_eq!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 20000, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[test]
fn rejects_malformed_credential_and_responses() {
    for (id, secret) in [("", "11".repeat(32)), ("id\r\nx:bad", "11".repeat(32)), ("id", "xx".repeat(32)), ("id", "11".repeat(31))] {
        assert!(Credential { credential_id:id.into(), secret }.key().is_err());
    }
    for status in [200, 301, 302, 400, 403, 500, 503] {
        assert_eq!(sent(&Reply {status, body:br#"{"sent":true}"#.to_vec()}), Err(DELIVERY_FAILED));
    }
    assert_eq!(sent(&Reply {status:429, body:Vec::new()}), Err(RATE_LIMITED));
    assert_eq!(sent(&Reply {status:202, body:vec![b' ';4097]}), Err(DELIVERY_FAILED));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn fresh_enrollment_rejected_immediately_cannot_enroll_again() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![enrolled("fresh"), revoked()]);
    assert_eq!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 10000, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn malformed_enrollment_never_persists_or_sends_a_credential() {
    for body in [br#"{"credential_id":"id","secret":"bad"}"#.to_vec(), b"not-json".to_vec(), vec![b' ';4097]] {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let (calls, exchange) = mock(vec![Reply { status:201, body }]);
        assert_eq!(deliver(&store, serde_json::json!({}), "fp", "x@y.test", "123456", 10000, exchange).await, Err(DELIVERY_FAILED));
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert!(store.credential().unwrap().is_none());
        assert_eq!(store.state().unwrap().last_enrollment_attempt, Some(10000));
    }
}
