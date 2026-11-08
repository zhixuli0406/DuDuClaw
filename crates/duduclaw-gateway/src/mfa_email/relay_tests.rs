use super::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::{collections::VecDeque, sync::{Arc, Mutex}};

const SEED: [u8; 32] = [0x42; 32];

fn credential(id: &str) -> Credential { Credential { credential_id: id.into(), secret: hex::encode([7u8;32]) } }
fn accepted() -> Reply { Reply::new(202, br#"{"sent":true}"#) }
fn revoked() -> Reply { Reply::new(403, br#"{"error":"relay_not_allowed"}"#) }
fn challenge() -> Reply { Reply::new(200, br#"{"challenge":"chal-0123456789"}"#) }
fn enrolled(id: &str) -> Reply { Reply::new(201, &serde_json::to_vec(&credential(id)).unwrap()) }
fn relay() -> RelayLicense {
    RelayLicense { artifact: serde_json::json!({"signature":"signed","future":{"keep":true}}),
        fingerprint: "macos-uuid-v2:00112233445566778899aabbccddeeff".into(), subscription_id: "sub_relay".into(),
        key: crate::installation_identity::key_from_seed_for_test(SEED) }
}
fn body(call: &Call) -> serde_json::Value { serde_json::from_slice(&call.body).unwrap() }

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
    assert_eq!(body(&a), serde_json::json!({"recipient":"mfa@example.com","code":"123456","locale":"zh-TW"}));
    let _: &Zeroizing<Vec<u8>> = &a.body;
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
fn enroll_canonical_message_is_exact_newline_delimited_v1() {
    let pk = "0123456789abcdef".repeat(4);
    // Hard-coded independently of the shared vector file.
    assert_eq!(enroll_canonical("chal-0123456789", "sub_relay", "fp-1", &pk, None, 1700000000).unwrap(),
        format!("duduclaw:mfa-relay-enroll:v1\nchal-0123456789\nsub_relay\nfp-1\n{pk}\n\n1700000000"));
    assert_eq!(enroll_canonical("chal-0123456789", "sub_relay", "fp-1", &pk, Some("cred-1"), 1700000000).unwrap(),
        format!("duduclaw:mfa-relay-enroll:v1\nchal-0123456789\nsub_relay\nfp-1\n{pk}\ncred-1\n1700000000"));
    assert_eq!(enroll_canonical("c", "s", "", &pk, None, -1).unwrap(),
        format!("duduclaw:mfa-relay-enroll:v1\nc\ns\n\n{pk}\n\n-1"));
    for (challenge, subscription, fingerprint, current) in [("c\nx", "s", "f", None), ("c", "s\n", "f", None),
        ("c", "s", "f\r", None), ("c", "s", "f", Some("id\nforged"))] {
        assert!(enroll_canonical(challenge, subscription, fingerprint, &pk, current, 1).is_err());
    }
}

#[test]
fn challenge_and_enroll_wire_are_signed_and_carry_no_recipient_or_otp() {
    let relay = relay();
    let call = challenge_call(&relay).unwrap();
    assert_eq!(call.endpoint, Endpoint::Challenge);
    assert_eq!(body(&call), serde_json::json!({"subscription_id":"sub_relay"}));
    assert!(call.headers.is_empty());

    let call = enroll_call(&relay, "chal-0123456789", None, 1700000000).unwrap();
    assert_eq!(call.endpoint, Endpoint::Enroll);
    assert!(call.headers.is_empty());
    let value = body(&call);
    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["challenge", "installation_signature", "issued_at", "license", "machine_fingerprint"]);
    assert_eq!(value["license"], relay.artifact);
    assert_eq!(value["machine_fingerprint"], relay.fingerprint);
    assert_eq!(value["challenge"], "chal-0123456789");
    assert_eq!(value["issued_at"], 1700000000i64);
    let canonical = enroll_canonical("chal-0123456789", "sub_relay", &relay.fingerprint,
        &relay.key.public_key_hex(), None, 1700000000).unwrap();
    let signature_hex = value["installation_signature"].as_str().unwrap();
    assert_eq!(signature_hex.len(), 128);
    let verifying = VerifyingKey::from_bytes(&relay.key.public_key_bytes()).unwrap();
    let signature = Signature::from_slice(&hex::decode(signature_hex).unwrap()).unwrap();
    verifying.verify(canonical.as_bytes(), &signature).unwrap();
    assert!(verifying.verify(format!("{canonical}0").as_bytes(), &signature).is_err());
}

#[test]
fn rotation_proves_the_current_credential_over_the_same_canonical() {
    let relay = relay();
    let current = credential("cred-old");
    let value = body(&enroll_call(&relay, "chal-0123456789", Some(&current), 1700000000).unwrap());
    assert_eq!(value.as_object().unwrap().len(), 7);
    assert_eq!(value["current_credential_id"], "cred-old");
    let canonical = enroll_canonical("chal-0123456789", "sub_relay", &relay.fingerprint,
        &relay.key.public_key_hex(), Some("cred-old"), 1700000000).unwrap();
    assert_eq!(value["credential_proof"], credential_proof(&[7u8; 32], &canonical));
    let verifying = VerifyingKey::from_bytes(&relay.key.public_key_bytes()).unwrap();
    let signature = Signature::from_slice(&hex::decode(value["installation_signature"].as_str().unwrap()).unwrap()).unwrap();
    verifying.verify(canonical.as_bytes(), &signature).unwrap();
    // Independent HMAC-SHA256 construction.
    let mut mac = Hmac::<Sha256>::new_from_slice(&[7u8; 32]).unwrap();
    mac.update(canonical.as_bytes());
    assert_eq!(value["credential_proof"], hex::encode(mac.finalize().into_bytes()));
    let malformed = Credential { credential_id: "cred-old".into(), secret: "zz".repeat(32) };
    assert!(enroll_call(&relay, "chal-0123456789", Some(&malformed), 1700000000).is_err());
}

/// Shared with `cloud-control-plane/tests/vectors/mfa_relay_enroll_v1.json`
/// (commercial repo; seed = 32 × 0x42). Copied into this crate by hand.
#[test]
fn shared_enroll_known_answer_vector() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/mfa_relay_enroll_v1.json");
    let text = std::fs::read_to_string(path).unwrap_or_else(|error|
        panic!("copy the control-plane enroll vector to {path}: {error}"));
    let document: serde_json::Value = serde_json::from_str(&text).unwrap();
    let top = |name: &str| document[name].as_str().unwrap_or_else(|| panic!("vector missing {name}")).to_owned();
    let seed: [u8; 32] = hex::decode(top("installation_seed_hex")).unwrap().try_into().unwrap();
    assert_eq!(seed, SEED, "shared vector seed is 32 bytes of 0x42");
    let key = crate::installation_identity::key_from_seed_for_test(seed);
    // (3) identity: pubkey, installation_id, SAS.
    assert_eq!(key.public_key_hex(), top("installation_pubkey_hex"));
    let digest = hex::encode(<Sha256 as sha2::Digest>::digest(key.public_key_bytes()));
    assert_eq!(&digest[..32], top("installation_id"));
    // The vector's SAS is 16 ungrouped hex chars; the gateway shows it grouped 4-4-4-4.
    let sas = top("sas");
    assert_eq!(sas.len(), 16);
    assert_eq!(&digest[..16], sas);
    assert_eq!(key.fingerprint().replace('-', ""), sas);
    assert_eq!(key.fingerprint(),
        sas.as_bytes().chunks(4).map(|g| std::str::from_utf8(g).unwrap()).collect::<Vec<_>>().join("-"));
    let issued_at = document["issued_at"].as_i64().expect("issued_at");
    let cases = document["cases"].as_array().expect("cases");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let case_text = |field: &str| case[field].as_str()
            .unwrap_or_else(|| panic!("case {name} missing {field}")).to_owned();
        let current_raw = case_text("current_credential_id");
        let current = Some(current_raw.as_str()).filter(|s| !s.is_empty());
        // (1) canonical message, byte for byte.
        let canonical = enroll_canonical(&top("challenge"), &top("subscription_id"),
            &top("machine_fingerprint"), &key.public_key_hex(), current, issued_at).unwrap();
        assert_eq!(canonical, case_text("canonical_message"), "{name}: canonical");
        assert_eq!(hex::encode(canonical.as_bytes()), case_text("canonical_message_hex"), "{name}: canonical hex");
        assert_eq!(case_text("canonical_message_template").replace("{installation_pubkey_hex}", &key.public_key_hex()),
            canonical, "{name}: template");
        // (2) installation signature.
        assert_eq!(hex::encode(key.sign(canonical.as_bytes())), case_text("installation_signature_hex"), "{name}: signature");
        // (4) credential proof, present only on the rotation case.
        match (case.get("credential_secret_hex"), case.get("credential_proof_hex")) {
            (Some(secret), Some(proof)) => {
                let secret = hex::decode(secret.as_str().unwrap()).unwrap();
                assert_eq!(credential_proof(&secret, &canonical), proof.as_str().unwrap(), "{name}: proof");
                assert!(current.is_some(), "{name}: rotation case carries a current credential id");
            }
            (None, None) => assert!(current.is_none(), "{name}: rotation needs a proof"),
            _ => panic!("{name}: secret and proof must come together"),
        }
    }
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

fn endpoints(calls: &Arc<Mutex<Vec<Call>>>) -> Vec<Endpoint> {
    calls.lock().unwrap().iter().map(|call| call.endpoint).collect()
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn lazy_enroll_persists_and_reuses_credential_across_restarts() {
    let home = tempfile::tempdir().unwrap();
    assert!(!home.path().join("state/mfa-relay.json").exists());
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![challenge(), enrolled("first"), accepted()]);
    assert!(deliver(&store, &relay(), "x@y.test", "123456", 10000, exchange).await.is_ok());
    assert_eq!(endpoints(&calls), [Endpoint::Challenge, Endpoint::Enroll, Endpoint::Send]);
    assert_eq!(body(&calls.lock().unwrap()[1])["challenge"], "chal-0123456789");
    assert!(body(&calls.lock().unwrap()[1]).get("current_credential_id").is_none());
    drop(store);
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![accepted()]);
    assert!(deliver(&store, &relay(), "x@y.test", "123456", 10001, exchange).await.is_ok());
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0].headers["x-relay-credential"], "first");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn revoked_credential_rotates_once_with_proof_and_budget_survives_restart() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    store.save_credential(&credential("old")).unwrap();
    let (calls, exchange) = mock(vec![revoked(), challenge(), enrolled("new"), accepted()]);
    assert!(deliver(&store, &relay(), "x@y.test", "123456", 10000, exchange).await.is_ok());
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].headers["x-relay-credential"], "old");
        assert_eq!(calls[1].endpoint, Endpoint::Challenge);
        let rotation = body(&calls[2]);
        assert_eq!(rotation["current_credential_id"], "old");
        let canonical = enroll_canonical("chal-0123456789", "sub_relay", &relay().fingerprint,
            &relay().key.public_key_hex(), Some("old"), 10000).unwrap();
        assert_eq!(rotation["credential_proof"], credential_proof(&[7u8; 32], &canonical));
        assert_eq!(calls[3].headers["x-relay-credential"], "new");
    }
    drop(store);
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![revoked()]);
    assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 13599, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 1);
    let (calls, exchange) = mock(vec![revoked(), challenge(), enrolled("third"), revoked()]);
    assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 13600, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 4, "second rejection never loops");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn failed_initial_enrollment_is_bounded_and_generic_403_does_not_rotate() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let (_, exchange) = mock(vec![challenge(), Reply::new(503, b"private remote diagnostic")]);
    assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 10000, exchange).await, Err(DELIVERY_FAILED));
    let (calls, exchange) = mock(vec![]);
    assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 10001, exchange).await, Err(DELIVERY_FAILED));
    assert!(calls.lock().unwrap().is_empty());
    store.save_credential(&credential("existing")).unwrap();
    let (calls, exchange) = mock(vec![Reply::new(403, br#"{"error":"other"}"#)]);
    assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 20000, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn bad_challenge_responses_never_reach_enroll_and_still_spend_the_budget() {
    for (reply, expected) in [
        (Reply::new(200, br#"{}"#), DELIVERY_FAILED),
        (Reply::new(200, br#"{"challenge":""}"#), DELIVERY_FAILED),
        (Reply::new(200, br#"{"challenge":"a\nb"}"#), DELIVERY_FAILED),
        (Reply::new(200, br#"{"challenge":"a b"}"#), DELIVERY_FAILED),
        (Reply::new(200, format!(r#"{{"challenge":"{}"}}"#, "a".repeat(513)).as_bytes()), DELIVERY_FAILED),
        (Reply::new(200, &[b' '; 4097]), DELIVERY_FAILED),
        (Reply::new(403, br#"{"error":"relay_key_unbound"}"#), DELIVERY_FAILED),
        (Reply::new(429, b""), RATE_LIMITED),
    ] {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let (calls, exchange) = mock(vec![reply]);
        assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 10000, exchange).await, Err(expected));
        assert_eq!(endpoints(&calls), [Endpoint::Challenge]);
        assert!(store.credential().unwrap().is_none());
        assert_eq!(store.state().unwrap().last_enrollment_attempt, Some(10000));
    }
}

#[test]
fn rejects_malformed_credential_and_responses() {
    for (id, secret) in [("", "11".repeat(32)), ("id\r\nx:bad", "11".repeat(32)), ("id", "xx".repeat(32)), ("id", "11".repeat(31))] {
        assert!(Credential { credential_id:id.into(), secret }.key().is_err());
    }
    for status in [200, 301, 302, 400, 403, 500, 503] {
        assert_eq!(sent(&Reply::new(status, br#"{"sent":true}"#)), Err(DELIVERY_FAILED));
    }
    assert_eq!(sent(&Reply::new(429, b"")), Err(RATE_LIMITED));
    assert_eq!(sent(&Reply::new(202, &[b' ';4097])), Err(DELIVERY_FAILED));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn fresh_enrollment_rejected_immediately_cannot_enroll_again() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let (calls, exchange) = mock(vec![challenge(), enrolled("fresh"), revoked()]);
    assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 10000, exchange).await, Err(DELIVERY_FAILED));
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn malformed_enrollment_never_persists_or_sends_a_credential() {
    for body in [br#"{"credential_id":"id","secret":"bad"}"#.to_vec(), b"not-json".to_vec(), vec![b' ';4097]] {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let (calls, exchange) = mock(vec![challenge(), Reply::new(201, &body)]);
        assert_eq!(deliver(&store, &relay(), "x@y.test", "123456", 10000, exchange).await, Err(DELIVERY_FAILED));
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert!(store.credential().unwrap().is_none());
        assert_eq!(store.state().unwrap().last_enrollment_attempt, Some(10000));
    }
}
