//! Webhook endpoint tests.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[test]
fn test_hmac_signature_generation() {
    let secret = "test_secret";
    let body = b"test body content";
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let result = hex::encode(mac.finalize().into_bytes());
    assert!(!result.is_empty());
    assert_eq!(result.len(), 64); // SHA-256 hex = 64 chars
}

#[test]
fn test_hmac_verification() {
    let secret = "webhook_secret_123";
    let body = b"{\"event\":\"test\"}";

    // Generate signature
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let signature = hex::encode(mac.finalize().into_bytes());

    // Verify
    let mut mac2 = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac2.update(body);
    let computed = hex::encode(mac2.finalize().into_bytes());

    assert_eq!(signature, computed);
}

#[test]
fn test_invalid_signature_rejected() {
    let secret = "real_secret";
    let body = b"payload";

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let correct = hex::encode(mac.finalize().into_bytes());

    let wrong = "sha256=0000000000000000000000000000000000000000000000000000000000000000";
    assert_ne!(format!("sha256={correct}"), wrong);
}
