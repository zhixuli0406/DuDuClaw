//! Contract v2: challenge-response enrollment signed by the installation key,
//! and exact-byte HMAC request authentication for sends.
use super::{DELIVERY_FAILED, RATE_LIMITED, store::{Store, Credential, EnrollmentState}, transport::{Call, Endpoint, Reply}};
use crate::license_runtime::RelayLicense;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use reqwest::header::{HeaderMap, HeaderValue};
use zeroize::Zeroizing;

/// Domain prefix of the enrollment canonical message (design §2.4).
pub(super) const ENROLL_DOMAIN: &str = "duduclaw:mfa-relay-enroll:v1\n";
const MAX_CHALLENGE_LEN: usize = 512;
const MAX_ENROLL_BODY: usize = 16384;

fn signature(secret: &[u8], timestamp: &str, nonce: &str, body: &[u8]) -> String {
    let canonical = format!("v1\nPOST\n{}\n{timestamp}\n{nonce}\n{}",
        Endpoint::Send.path(), hex::encode(Sha256::digest(body)));
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(canonical.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// `"duduclaw:mfa-relay-enroll:v1\n" + challenge, subscription, fingerprint,
/// pubkey, current_credential_id|"", issued_at` — fields joined by `"\n"`,
/// no trailing newline. Fields containing `"\n"` are refused so the encoding
/// stays unambiguous.
pub(super) fn enroll_canonical(challenge: &str, subscription_id: &str, fingerprint: &str,
    public_key_hex: &str, current_credential_id: Option<&str>, issued_at: i64) -> Result<String, &'static str> {
    let fields = [challenge, subscription_id, fingerprint, public_key_hex, current_credential_id.unwrap_or("")];
    if fields.iter().any(|field| field.contains('\n') || field.contains('\r')) { return Err(DELIVERY_FAILED); }
    Ok(format!("{ENROLL_DOMAIN}{}\n{issued_at}", fields.join("\n")))
}

/// HMAC-SHA256(current secret, canonical), lowercase hex.
pub(super) fn credential_proof(secret: &[u8], canonical: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(canonical.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn send_call(credential: &Credential, email: &str, code: &str, now: i64) -> Result<Call, &'static str> {
    let key = Zeroizing::new(credential.key()?);
    let body = Zeroizing::new(serde_json::to_vec(&serde_json::json!({"recipient":email,"code":code,"locale":"zh-TW"}))
        .map_err(|_| DELIVERY_FAILED)?);
    let timestamp = now.to_string();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let mut headers = HeaderMap::new();
    for (name, value) in [("x-relay-credential", credential.credential_id.as_str()),
        ("x-relay-timestamp", &timestamp), ("x-relay-nonce", &nonce),
        ("x-relay-signature", &signature(&key, &timestamp, &nonce, &body))] {
        let mut value = HeaderValue::from_str(value).map_err(|_| DELIVERY_FAILED)?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(Call { endpoint: Endpoint::Send, body, headers })
}

fn challenge_call(relay: &RelayLicense) -> Result<Call, &'static str> {
    let body = Zeroizing::new(serde_json::to_vec(&serde_json::json!({"subscription_id":relay.subscription_id}))
        .map_err(|_| DELIVERY_FAILED)?);
    Ok(Call { endpoint: Endpoint::Challenge, body, headers: HeaderMap::new() })
}

/// Parse `{"challenge":"…"}`. The value is opaque, but must be bounded,
/// printable ASCII so it cannot alter the newline-delimited canonical form.
fn challenge_value(reply: &Reply) -> Result<String, &'static str> {
    if reply.status == 429 { return Err(RATE_LIMITED); }
    if !matches!(reply.status, 200 | 201) || reply.body.len() > super::transport::MAX_RESPONSE_BYTES {
        return Err(DELIVERY_FAILED);
    }
    let value: serde_json::Value = serde_json::from_slice(&reply.body).map_err(|_| DELIVERY_FAILED)?;
    let challenge = value.get("challenge").and_then(|c| c.as_str()).ok_or(DELIVERY_FAILED)?;
    if challenge.is_empty() || challenge.len() > MAX_CHALLENGE_LEN
        || !challenge.bytes().all(|b| b.is_ascii_graphic()) { return Err(DELIVERY_FAILED); }
    Ok(challenge.to_owned())
}

/// Body: license, machine_fingerprint, challenge, issued_at,
/// installation_signature, and when rotating current_credential_id +
/// credential_proof. The server derives installation_id from the bound key.
fn enroll_call(relay: &RelayLicense, challenge: &str, current: Option<&Credential>, now: i64)
    -> Result<Call, &'static str> {
    let current_id = current.map(|c| c.credential_id.as_str());
    let canonical = enroll_canonical(challenge, &relay.subscription_id, &relay.fingerprint,
        &relay.key.public_key_hex(), current_id, now)?;
    let mut body = serde_json::json!({
        "license": relay.artifact,
        "machine_fingerprint": relay.fingerprint,
        "challenge": challenge,
        "issued_at": now,
        "installation_signature": hex::encode(relay.key.sign(canonical.as_bytes())),
    });
    if let Some(current) = current {
        let secret = Zeroizing::new(current.key()?);
        body["current_credential_id"] = serde_json::json!(current.credential_id);
        body["credential_proof"] = serde_json::json!(credential_proof(&secret, &canonical));
    }
    let body = Zeroizing::new(serde_json::to_vec(&body).map_err(|_| DELIVERY_FAILED)?);
    if body.len() > MAX_ENROLL_BODY { return Err(DELIVERY_FAILED); }
    Ok(Call { endpoint: Endpoint::Enroll, body, headers: HeaderMap::new() })
}

fn is_revoked(reply: &Reply) -> bool {
    reply.status == 403 && serde_json::from_slice::<serde_json::Value>(&reply.body).ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|e| e == "relay_not_allowed")) == Some(true)
}
pub(super) fn sent(reply: &Reply) -> Result<(), &'static str> {
    if reply.body.len() > super::transport::MAX_RESPONSE_BYTES { return Err(DELIVERY_FAILED); }
    if reply.status == 429 { return Err(RATE_LIMITED); }
    if reply.status == 202 && serde_json::from_slice::<serde_json::Value>(&reply.body).ok()
        .and_then(|v| v.get("sent").and_then(|sent| sent.as_bool())) == Some(true) { Ok(()) }
    else { Err(DELIVERY_FAILED) }
}

/// Challenge then signed enroll. Both requests share one durable hourly
/// attempt, reserved and persisted before any network I/O.
async fn enroll<F, Fut>(store: &Store, state: &mut EnrollmentState, relay: &RelayLicense,
    current: Option<&Credential>, now: i64, exchange: &mut F) -> Result<Credential, &'static str>
where F: FnMut(Call) -> Fut, Fut: std::future::Future<Output = Result<Reply, &'static str>> {
    state.reserve(now)?;
    store.save_state(state).map_err(|_| DELIVERY_FAILED)?;
    let challenge = challenge_value(&exchange(challenge_call(relay)?).await?)?;
    let reply = exchange(enroll_call(relay, &challenge, current, now)?).await?;
    if reply.status == 429 { return Err(RATE_LIMITED); }
    if !matches!(reply.status, 200 | 201 | 202) || reply.body.len() > super::transport::MAX_RESPONSE_BYTES {
        return Err(DELIVERY_FAILED);
    }
    let credential: Credential = serde_json::from_slice(&reply.body).map_err(|_| DELIVERY_FAILED)?;
    let _key = Zeroizing::new(credential.key()?);
    store.save_credential(&credential).map_err(|_| DELIVERY_FAILED)?;
    Ok(credential)
}

pub(super) async fn deliver<F, Fut>(store: &Store, relay: &RelayLicense,
    email: &str, code: &str, now: i64, mut exchange: F) -> Result<(), &'static str>
where F: FnMut(Call) -> Fut, Fut: std::future::Future<Output = Result<Reply, &'static str>> {
    let mut state = store.state().map_err(|_| DELIVERY_FAILED)?;
    let credential = match store.credential().map_err(|_| DELIVERY_FAILED)? {
        Some(credential) => credential,
        None => enroll(store, &mut state, relay, None, now, &mut exchange).await?,
    };
    let reply = exchange(send_call(&credential, email, code, now)?).await?;
    if is_revoked(&reply) {
        // One retry, and all enrollment attempts share the durable hourly budget.
        // A freshly enrolled credential rejected immediately cannot loop. The
        // rotation proves possession of the credential being replaced.
        let credential = enroll(store, &mut state, relay, Some(&credential), now, &mut exchange).await?;
        return sent(&exchange(send_call(&credential, email, code, now)?).await?);
    }
    sent(&reply)
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
