//! Standard Webhooks secrets and signatures for MCP Events deliveries.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

/// Prefix of a Standard Webhooks symmetric secret.
pub const SECRET_PREFIX: &str = "whsec_";
/// Random bytes in a generated secret (the draft allows 24–64).
pub const SECRET_BYTES: usize = 32;
/// Deliveries whose timestamp is further than this from now are refused.
pub const TIMESTAMP_TOLERANCE_SECS: i64 = 300;

/// A new `whsec_` secret from the OS random source.
pub fn generate_secret() -> String {
    let mut buf = [0u8; SECRET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    format!("{SECRET_PREFIX}{}", B64.encode(buf))
}

/// The key bytes of a `whsec_` secret (24–64 bytes), or `None`.
pub fn secret_key(secret: &str) -> Option<Vec<u8>> {
    let raw = B64.decode(secret.strip_prefix(SECRET_PREFIX)?).ok()?;
    (24..=64).contains(&raw.len()).then_some(raw)
}

/// `v1,<base64>` for one message (used by tests and the loopback fake).
pub fn sign(secret: &str, msg_id: &str, timestamp: i64, body: &[u8]) -> Option<String> {
    let key = secret_key(secret)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(format!("{msg_id}.{timestamp}.").as_bytes());
    mac.update(body);
    Some(format!("v1,{}", B64.encode(mac.finalize().into_bytes())))
}

/// Why a delivery failed verification (for the audit row; the HTTP answer
/// never says which).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// Timestamp missing, not an integer, or outside the tolerance.
    Timestamp,
    /// No `v1,` signature matched any accepted secret.
    Signature,
}

impl VerifyError {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerifyError::Timestamp => "timestamp",
            VerifyError::Signature => "signature",
        }
    }
}

/// Verify one delivery against any of `secrets` (current, then a previous
/// one inside its rotation grace). The HMAC comparison is constant time
/// (`Mac::verify_slice`). Signatures other than `v1,` are ignored.
pub fn verify(
    secrets: &[&str],
    msg_id: &str,
    timestamp: &str,
    signature_header: &str,
    body: &[u8],
    now: i64,
) -> Result<(), VerifyError> {
    let ts: i64 = timestamp.trim().parse().map_err(|_| VerifyError::Timestamp)?;
    if (now - ts).abs() > TIMESTAMP_TOLERANCE_SECS {
        return Err(VerifyError::Timestamp);
    }
    let candidates: Vec<Vec<u8>> = signature_header
        .split_whitespace()
        .filter_map(|s| s.strip_prefix("v1,"))
        .filter_map(|b| B64.decode(b).ok())
        .take(8)
        .collect();
    for secret in secrets {
        let Some(key) = secret_key(secret) else { continue };
        for sig in &candidates {
            let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&key) else { continue };
            mac.update(format!("{msg_id}.{ts}.").as_bytes());
            mac.update(body);
            if mac.verify_slice(sig).is_ok() {
                return Ok(());
            }
        }
    }
    Err(VerifyError::Signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_are_valid_and_signatures_verify() {
        let s = generate_secret();
        assert!(s.starts_with("whsec_"));
        assert_eq!(secret_key(&s).unwrap().len(), 32);
        let sig = sign(&s, "evt_1", 1000, b"{}").unwrap();
        assert!(verify(&[&s], "evt_1", "1000", &sig, b"{}", 1100).is_ok());
        // Rotation: several signatures, any may match.
        let other = generate_secret();
        let both = format!("{} {sig}", sign(&other, "evt_1", 1000, b"{}").unwrap());
        assert!(verify(&[&s], "evt_1", "1000", &both, b"{}", 1000).is_ok());
        assert!(verify(&[&other, &s], "evt_1", "1000", &sig, b"{}", 1000).is_ok());
    }

    #[test]
    fn tampering_staleness_and_bad_secrets_fail() {
        let s = generate_secret();
        let sig = sign(&s, "evt_1", 1000, b"{}").unwrap();
        assert_eq!(verify(&[&s], "evt_1", "1000", &sig, b"{ }", 1000), Err(VerifyError::Signature));
        assert_eq!(verify(&[&s], "evt_2", "1000", &sig, b"{}", 1000), Err(VerifyError::Signature));
        assert_eq!(verify(&[&s], "evt_1", "1000", &sig, b"{}", 1301), Err(VerifyError::Timestamp));
        assert_eq!(verify(&[&s], "evt_1", "x", &sig, b"{}", 1000), Err(VerifyError::Timestamp));
        assert_eq!(verify(&["whsec_short"], "evt_1", "1000", &sig, b"{}", 1000), Err(VerifyError::Signature));
        assert!(secret_key("whsec_").is_none());
        assert!(secret_key("plain").is_none());
        // A `v1a,` (asymmetric) entry is ignored, not an error.
        assert_eq!(
            verify(&[&s], "evt_1", "1000", "v1a,AAAA", b"{}", 1000),
            Err(VerifyError::Signature)
        );
    }
}
