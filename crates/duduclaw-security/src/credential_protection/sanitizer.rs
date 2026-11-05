//! Shared output boundary for credential diagnostics, including structured output.

/// Preserve readable Unicode while rendering controls and non-printables visibly.
pub fn sanitize_output_text(input: &str) -> String {
    duduclaw_core::root_key::recovery::escape_diagnostic_text(input)
}

/// Sanitize every string recursively, including strings in nested diagnostics.
pub fn sanitize_output_value(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => Value::String(sanitize_output_text(&text)),
        Value::Array(values) => Value::Array(values.into_iter().map(sanitize_output_value).collect()),
        Value::Object(values) => Value::Object(values.into_iter().map(|(key, value)| {
            (sanitize_output_text(&key), sanitize_output_value(value))
        }).collect()),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_protection_output_escapes_controls_bidi_and_nonprintables() {
        let hostile = "中文\0\u{1b}\u{7f}\u{9b}\u{202e}\u{2066}\u{200b}\u{378}";
        let safe = sanitize_output_text(hostile);
        assert_eq!(safe, "中文\\u{0}\\u{1b}\\u{7f}\\u{9b}\\u{202e}\\u{2066}\\u{200b}\\u{378}");
        assert_eq!(sanitize_output_text(&safe), safe);
        let payload = sanitize_output_value(serde_json::json!({
            "key_id": hostile, "enabled_at": hostile,
            "root_key_recovery": { "detail": [hostile] }, "number": 3,
        }));
        assert_eq!(payload["key_id"], safe);
        assert_eq!(payload["enabled_at"], safe);
        assert_eq!(payload["root_key_recovery"]["detail"][0], safe);
        assert_eq!(payload["number"], 3);
    }
}
