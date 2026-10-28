//! Dependency-light protocol shared verbatim with the native desktop shell.
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
pub const ROUTE: &str = "/api/internal/local-browser-companion";
pub const SERVICE: &str = "ai.duduclaw.local-browser-companion.v1";
pub const CLOCK_SKEW_SECONDS: u64 = 30;
pub fn account(home: &std::path::Path) -> String {
    format!(
        "home-{:x}",
        Sha256::digest(home.as_os_str().as_encoded_bytes())
    )
}
pub fn nonce_valid(nonce: &str) -> bool {
    nonce.len() == 32
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn message(response: bool, time: u64, nonce: &str, body: &[u8]) -> String {
    format!(
        "local-browser-companion-v1\n{}\nPOST\n{ROUTE}\n{time}\n{nonce}\n{:x}",
        if response { "response" } else { "request" },
        Sha256::digest(body)
    )
}
pub fn sign(
    key: &[u8],
    response: bool,
    time: u64,
    nonce: &str,
    body: &[u8],
) -> Result<String, &'static str> {
    if key.len() != 32 || !nonce_valid(nonce) {
        return Err("invalid local credential");
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| "invalid local credential")?;
    mac.update(message(response, time, nonce, body).as_bytes());
    Ok(mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
pub fn verify(
    key: &[u8],
    response: bool,
    time: u64,
    nonce: &str,
    body: &[u8],
    signature: &str,
    now: u64,
) -> bool {
    if key.len() != 32
        || !nonce_valid(nonce)
        || time.abs_diff(now) > CLOCK_SKEW_SECONDS
        || signature.len() != 64
        || !signature
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return false;
    }
    let digit = |b: u8| {
        if b.is_ascii_digit() {
            b - b'0'
        } else {
            b - b'a' + 10
        }
    };
    let bytes: Vec<_> = signature
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| digit(pair[0]) * 16 + digit(pair[1]))
        .collect();
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return false;
    };
    mac.update(message(response, time, nonce, body).as_bytes());
    mac.verify_slice(&bytes).is_ok()
}
