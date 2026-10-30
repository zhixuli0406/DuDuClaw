//! Durable transport keeps detected message values encrypted, never in its text projection.
use serde_json::Value;
use std::path::Path;

const CIPHER: &str = "_credential_text_enc";

pub(super) fn protect(payload: &str, home: Option<&Path>) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(payload) else {
        return duduclaw_security::credential_detect::mask(payload);
    };
    if let Some(message) = value.pointer_mut("/event/message").and_then(Value::as_object_mut) {
        // Never trust a provider-supplied ciphertext marker.
        message.remove(CIPHER);
        if let Some(text) = message.get("text").and_then(Value::as_str).map(str::to_owned) {
            let masked = duduclaw_security::credential_detect::mask(&text);
            if masked != text {
                let bound = serde_json::json!({"purpose":"credential-ingress/v1", "id":message.get("id"), "text":text}).to_string();
                if let Some(cipher) = home.and_then(|home| crate::config_crypto::encrypt_value(&bound, home)) {
                    message.insert(CIPHER.into(), Value::String(cipher));
                }
                message.insert("text".into(), Value::String(masked));
            }
        }
    }
    value.to_string()
}

/// Call only on the claimed worker's private copy. Admin views remain masked.
pub(crate) fn restore_event(event: &mut Value, home: &Path) {
    let Some(message) = event.get_mut("message").and_then(Value::as_object_mut) else { return; };
    let Some(cipher) = message.remove(CIPHER) else { return; };
    let original = cipher.as_str()
        .and_then(|cipher| crate::config_crypto::decrypt_value(cipher, home))
        .and_then(|plain| serde_json::from_str::<Value>(&plain).ok())
        .filter(|plain| plain["purpose"] == "credential-ingress/v1" && plain.get("id") == message.get("id"))
        .and_then(|plain| plain["text"].as_str().map(str::to_owned))
        .unwrap_or_else(|| duduclaw_security::credential_detect::UNAVAILABLE_MASK.into());
    message.insert("text".into(), Value::String(original));
}

/// Merging uses persisted text for route decisions. Keep encrypted events as
/// individual turns so mentions/commands and source ids cannot change on decrypt.
pub(crate) fn is_encrypted(event: &Value) -> bool {
    event.pointer("/message/_credential_text_enc").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingress_masks_text_without_touching_delivery_token() {
        let payload = serde_json::json!({"event":{"replyToken":"routing-token", "message":{"id":"m1", "text":"密碼：fixture"}}});
        let stored = protect(&payload.to_string(), None);
        assert!(!stored.contains("fixture"));
        let stored: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["event"]["replyToken"], "routing-token");
    }

    #[test]
    fn invalid_ciphertext_cannot_restore_plaintext() {
        let mut event = serde_json::json!({"message":{"id":"m1", "text":"masked", "_credential_text_enc":"invalid"}});
        restore_event(&mut event, Path::new("/nonexistent-credential-test"));
        assert_eq!(event["message"]["text"], duduclaw_security::credential_detect::UNAVAILABLE_MASK);
    }

    #[test]
    fn provider_cannot_supply_ciphertext_marker() {
        let payload = serde_json::json!({"event":{"message":{"id":"m1", "text":"hello", "_credential_text_enc":"fake"}}});
        let stored: Value = serde_json::from_str(&protect(&payload.to_string(), None)).unwrap();
        assert!(!is_encrypted(&stored["event"]));
    }
    #[test]
    fn encrypted_ingress_round_trip_is_bound_to_message_id() {
        let home = tempfile::tempdir().unwrap();
        let payload = serde_json::json!({"event":{"message":{"id":"m1", "text":"password: fixture-roundtrip"}}});
        let stored = protect(&payload.to_string(), Some(home.path()));
        assert!(!stored.contains("fixture-roundtrip"));
        let stored: Value = serde_json::from_str(&stored).unwrap();
        assert!(is_encrypted(&stored["event"]));
        let mut restored = stored["event"].clone();
        restore_event(&mut restored, home.path());
        assert_eq!(restored["message"]["text"], "password: fixture-roundtrip");
        let mut swapped = stored["event"].clone();
        swapped["message"]["id"] = Value::String("m2".into());
        restore_event(&mut swapped, home.path());
        assert_eq!(swapped["message"]["text"], duduclaw_security::credential_detect::UNAVAILABLE_MASK);
    }

}
