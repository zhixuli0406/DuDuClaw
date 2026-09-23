//! Locating tokens inside a redacted JSON document.
//!
//! After [`crate::pipeline::RedactionPipeline::redact_value`] has run, every
//! consumer that wants to *evidence* the result (`duduclaw redaction verify`
//! JSON mode, the dashboard's `redaction.dry_run`) needs the same answer:
//! which RFC-6901 pointer holds which token. These are pure functions over a
//! `serde_json::Value` — no vault, no pipeline, no I/O — so both callers share
//! one implementation instead of keeping private copies in step.

use serde_json::Value;

use crate::token::{TOKEN_PREFIX, TOKEN_SUFFIX, Token};

/// Escape one RFC-6901 reference token (`~` → `~0`, `/` → `~1`).
pub fn escape_pointer(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Every well-formed redaction token inside `s`, in order of appearance.
///
/// Candidates that carry the delimiters but fail [`Token::parse`] (wrong
/// category charset, bad hash length) are skipped — the scan reports what the
/// pipeline would actually recognise, never what merely looks token-shaped.
pub fn scan_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find(TOKEN_PREFIX) {
        let from = &rest[start..];
        let Some(end_rel) = from.find(TOKEN_SUFFIX) else {
            break;
        };
        let len = end_rel + TOKEN_SUFFIX.len();
        let candidate = &from[..len];
        if Token::parse(candidate).is_some() {
            out.push(candidate.to_string());
        }
        rest = &from[len..];
    }
    out
}

/// Locate every token in a redacted document as `(pointer, token)`.
///
/// A string leaf that itself holds JSON is descended into, with `#` marking
/// the boundary — `/content/0/text#/0/name` reads "inside the embedded JSON at
/// `content[0].text`, the first record's `name`". That is the shape an MCP
/// tool result actually has, so without the descent every structured hit would
/// report the same useless outer pointer.
pub fn collect_token_locations(v: &Value, prefix: &str, out: &mut Vec<(String, String)>) {
    match v {
        Value::String(s) => {
            if let Ok(inner) = serde_json::from_str::<Value>(s)
                && (inner.is_object() || inner.is_array())
            {
                collect_token_locations(&inner, &format!("{prefix}#"), out);
                return;
            }
            for tok in scan_tokens(s) {
                out.push((prefix.to_string(), tok));
            }
        }
        Value::Array(arr) => {
            for (idx, child) in arr.iter().enumerate() {
                collect_token_locations(child, &format!("{prefix}/{idx}"), out);
            }
        }
        Value::Object(map) => {
            for (key, child) in map {
                collect_token_locations(child, &format!("{prefix}/{}", escape_pointer(key)), out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOK: &str = "<REDACT:DB_FIELD:a1234567891bcdefa1234567891bcdef>";

    #[test]
    fn scan_tokens_finds_only_valid_tokens() {
        assert_eq!(scan_tokens(&format!("x {TOK} y")), vec![TOK.to_string()]);
        assert!(scan_tokens("<REDACT:X:short>").is_empty());
        assert!(scan_tokens("nothing here").is_empty());
        assert_eq!(scan_tokens(&format!("{TOK}{TOK}")).len(), 2);
    }

    #[test]
    fn token_locations_descend_into_embedded_json() {
        let inner = serde_json::json!([{"id": 1, "name": TOK}]);
        let doc = serde_json::json!({
            "content": [{"type": "text", "text": serde_json::to_string_pretty(&inner).unwrap()}]
        });
        let mut out = Vec::new();
        collect_token_locations(&doc, "", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "/content/0/text#/0/name");
        assert_eq!(out[0].1, TOK);
    }

    #[test]
    fn token_locations_handle_a_plain_leaf() {
        let tok = "<REDACT:EMAIL:a1234567891bcdefa1234567891bcdef>";
        let doc = serde_json::json!({"rows": [{"email": tok}]});
        let mut out = Vec::new();
        collect_token_locations(&doc, "", &mut out);
        assert_eq!(out, vec![("/rows/0/email".to_string(), tok.to_string())]);
    }

    #[test]
    fn pointer_escaping_is_rfc6901() {
        assert_eq!(escape_pointer("a/b"), "a~1b");
        assert_eq!(escape_pointer("a~b"), "a~0b");
        assert_eq!(escape_pointer("plain"), "plain");
    }
}
