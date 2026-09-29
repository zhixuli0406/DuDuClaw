//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── P1 INF helpers (inference.toml) ───────────────────────────────────────────

/// Masked placeholder returned to the dashboard in place of a stored secret.
pub(crate) const SECRET_MASK_SET: &str = "***set***";

/// Convert a parsed inference.toml table into the `inference.get` response JSON,
/// MASKING the `[openai_compat]` api key — the cleartext (or `_enc`) is NEVER
/// returned; instead `api_key_set: bool` + a masked placeholder are exposed.
pub(crate) fn inference_table_to_response(table: &toml::Table) -> Value {
    // Serialise the whole table to JSON, then scrub the secret in-place. Using
    // the generic round-trip means new inference.toml sub-sections surface
    // automatically without per-field plumbing.
    let mut v = serde_json::to_value(table).unwrap_or_else(|_| json!({}));
    if let Some(oc) = v.get_mut("openai_compat").and_then(|o| o.as_object_mut()) {
        let has_secret = oc
            .get("api_key")
            .and_then(|k| k.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || oc
                .get("api_key_enc")
                .and_then(|k| k.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
        // Never echo the raw / encrypted secret.
        oc.remove("api_key");
        oc.remove("api_key_enc");
        oc.insert("api_key_set".into(), json!(has_secret));
        oc.insert(
            "api_key".into(),
            json!(if has_secret { SECRET_MASK_SET } else { "" }),
        );
    }
    v
}
