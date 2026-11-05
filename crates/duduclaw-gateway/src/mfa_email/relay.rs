//! Contract v2 enrollment and exact-byte request authentication.
use super::{DELIVERY_FAILED, RATE_LIMITED, store::{Store, Credential, EnrollmentState}, transport::{Call, Endpoint, Reply}};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use reqwest::header::{HeaderMap, HeaderValue};

fn signature(secret: &[u8], timestamp: &str, nonce: &str, body: &[u8]) -> String {
    let canonical = format!("v1\nPOST\n{}\n{timestamp}\n{nonce}\n{}",
        Endpoint::Send.path(), hex::encode(Sha256::digest(body)));
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(canonical.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
fn send_call(credential: &Credential, email: &str, code: &str, now: i64) -> Result<Call, &'static str> {
    let key = zeroize::Zeroizing::new(credential.key()?);
    let body = serde_json::to_vec(&serde_json::json!({"recipient":email,"code":code,"locale":"zh-TW"}))
        .map_err(|_| DELIVERY_FAILED)?;
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
fn enroll_call(license: &serde_json::Value, fingerprint: &str, state: &EnrollmentState, now: i64) -> Result<Call, &'static str> {
    let body = serde_json::to_vec(&serde_json::json!({"license":license,"machine_fingerprint":fingerprint,
        "installation_id":state.installation_id,"nonce":uuid::Uuid::new_v4().simple().to_string(),"issued_at":now}))
        .map_err(|_| DELIVERY_FAILED)?;
    if body.len() > 16384 { return Err(DELIVERY_FAILED); }
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
async fn enroll<F, Fut>(store: &Store, state: &mut EnrollmentState, license: &serde_json::Value,
    fingerprint: &str, now: i64, exchange: &mut F) -> Result<Credential, &'static str>
where F: FnMut(Call) -> Fut, Fut: std::future::Future<Output = Result<Reply, &'static str>> {
    state.reserve(now)?;
    store.save_state(state).map_err(|_| DELIVERY_FAILED)?;
    let reply = exchange(enroll_call(license, fingerprint, state, now)?).await?;
    if reply.status == 429 { return Err(RATE_LIMITED); }
    if !matches!(reply.status, 200 | 201 | 202) || reply.body.len() > super::transport::MAX_RESPONSE_BYTES {
        return Err(DELIVERY_FAILED);
    }
    let credential: Credential = serde_json::from_slice(&reply.body).map_err(|_| DELIVERY_FAILED)?;
    let _key = zeroize::Zeroizing::new(credential.key()?);
    store.save_credential(&credential).map_err(|_| DELIVERY_FAILED)?;
    Ok(credential)
}

pub(super) async fn deliver<F, Fut>(store: &Store, license: serde_json::Value, fingerprint: &str,
    email: &str, code: &str, now: i64, mut exchange: F) -> Result<(), &'static str>
where F: FnMut(Call) -> Fut, Fut: std::future::Future<Output = Result<Reply, &'static str>> {
    let mut state = store.state().map_err(|_| DELIVERY_FAILED)?;
    let credential = match store.credential().map_err(|_| DELIVERY_FAILED)? {
        Some(credential) => credential,
        None => enroll(store, &mut state, &license, fingerprint, now, &mut exchange).await?,
    };
    let reply = exchange(send_call(&credential, email, code, now)?).await?;
    if is_revoked(&reply) {
        // One retry, and all enrollment attempts share the durable hourly budget.
        // A freshly enrolled credential rejected immediately cannot loop.
        let credential = enroll(store, &mut state, &license, fingerprint, now, &mut exchange).await?;
        return sent(&exchange(send_call(&credential, email, code, now)?).await?);
    }
    sent(&reply)
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
