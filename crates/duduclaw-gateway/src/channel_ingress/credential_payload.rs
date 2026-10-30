//! Durable transport stores only masked message text in both credential modes.
//! Retrying an event must never require decrypting a conversation credential.
use serde_json::Value;

pub(super) fn protect(payload: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(payload) else {
        return duduclaw_security::credential_detect::UNAVAILABLE_MASK.into();
    };
    if let Some(message) = value.pointer_mut("/event/message").and_then(Value::as_object_mut) {
        // Drop both legacy and provider-supplied markers without decrypting.
        message.remove("_credential_text_enc");
        mask_native_mentions(message);
    }
    // Routing token is provider-owned. All user-controlled fields (including
    // postbacks, filenames and unknown extension fields) use the same sink.
    let reply_token = value.pointer_mut("/event/replyToken").map(Value::take);
    duduclaw_security::credential_detect::mask_json(&mut value);
    if let (Some(token), Some(slot)) = (reply_token, value.pointer_mut("/event/replyToken")) {
        *slot = token;
    }
    value.to_string()
}

// Native mention offsets refer to the original UTF-16 text. Resolve them
// before credential replacement changes its length; human mentions must not
// become employee-routing instructions. Bot presence is a boolean gate only.
fn mask_native_mentions(message: &mut serde_json::Map<String, Value>) {
    let Some(text) = message.get("text").and_then(Value::as_str) else { return };
    if duduclaw_security::credential_detect::mask(text) == text { return; }
    let Some(mentionees) = message.get("mention").and_then(|m| m.get("mentionees")).and_then(Value::as_array) else { return };
    let people: Vec<_> = mentionees.iter().filter(|m| m["isSelf"].as_bool() != Some(true))
        .filter_map(|m| {
            let start = usize::try_from(m["index"].as_u64()?).ok()?;
            let length = usize::try_from(m["length"].as_u64()?).ok()?;
            crate::trigger_route::spans::utf16_span_to_bytes(text, start, length)
        }).collect();
    let bot_mentioned = mentionees.iter().any(|m| m["isSelf"].as_bool() == Some(true));
    let masked = match duduclaw_security::credential_detect::detect(text) {
        Ok(findings) if text.len() <= duduclaw_security::credential_detect::MAX_SCAN_BYTES => {
            let mut rendered = String::new();
            for (index, ch) in text.char_indices() {
                if let Some(finding) = findings.iter().find(|f| f.span.contains(&index)) {
                    if index == finding.span.start { rendered.push_str("[已遮罩的密碼]"); }
                } else if people.iter().any(|(start, end)| index >= *start && index < *end) {
                    rendered.push(' ');
                } else {
                    rendered.push(ch);
                }
            }
            duduclaw_security::credential_detect::mask(&rendered)
        }
        _ => duduclaw_security::credential_detect::UNAVAILABLE_MASK.into(),
    };
    message.insert("text".into(), Value::String(masked));
    let mentions = if bot_mentioned { serde_json::json!([{"index":0,"length":0,"isSelf":true}]) } else { serde_json::json!([]) };
    message.insert("mention".into(), serde_json::json!({"mentionees":mentions}));
}

#[cfg(test)]
mod tests {
    use super::*;

    // Security regressions written for F3; not run (owner forbids builds).
    #[test]
    fn ingress_masks_text_without_touching_delivery_token() {
        let payload = serde_json::json!({"event":{"replyToken":"routing-token", "message":{"id":"m1", "text":"密碼：fixture"}}});
        let stored = protect(&payload.to_string());
        assert!(!stored.contains("fixture"));
        assert!(!stored.contains("_credential_text_enc"));
        let stored: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["event"]["replyToken"], "routing-token");
        assert_eq!(stored["event"]["message"]["id"], "m1");
    }

    #[test]
    fn legacy_and_provider_ciphertext_is_discarded_without_a_key() {
        let payload = serde_json::json!({"event":{"message":{"id":"m1", "text":"hello", "_credential_text_enc":"legacy-or-forged"}}});
        let stored = protect(&payload.to_string());
        assert!(!stored.contains("legacy-or-forged"));
        assert!(!stored.contains("_credential_text_enc"));
        assert_eq!(protect(&stored), stored);
    }

    #[test]
    fn ingress_masks_postbacks_filenames_and_unknown_fields() {
        let payload = serde_json::json!({"event":{"replyToken":"provider-token", "postback":{"data":"password: fixture-postback"},
            "message":{"fileName":"密碼：fixture-name", "text":"safe", "extension":{"password":"fixture-extension"}},
            "unknown":"token: fixture-unknown"}});
        let stored = protect(&payload.to_string());
        assert!(!stored.contains("fixture-"));
        assert!(stored.contains("provider-token"));
    }

    #[test]
    fn credential_before_native_human_mention_cannot_route_an_employee() {
        let payload = serde_json::json!({"event":{"message":{"type":"text", "text":"password: p @sales", "mention":{"mentionees":[{"index":12,"length":6,"isSelf":false}]}}}});
        let stored: Value = serde_json::from_str(&protect(&payload.to_string())).unwrap();
        assert!(!stored["event"]["message"]["text"].as_str().unwrap().contains("@sales"));
        assert_eq!(stored["event"]["message"]["mention"]["mentionees"], serde_json::json!([]));
        assert!(!stored.to_string().contains("password: p"));
    }

    #[test]
    fn malformed_payload_fails_closed() {
        assert_eq!(protect("broken password: fixture"), duduclaw_security::credential_detect::UNAVAILABLE_MASK);
    }
}
