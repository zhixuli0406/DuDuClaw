//! Sensitivity labelling for memory writes (P3-2).
//!
//! A memory row's sensitivity is carried **inside the existing `metadata` JSON
//! blob** — additive, no schema change — exactly like the origin-binding
//! convention (`reaffirmed_by` etc. also live in that blob). Perception write
//! paths (P4-4 digital-footprint → temporal memory) stamp the source's
//! [`Sensitivity`] here so downstream context assembly can withhold Personal+
//! facts from shared sessions.
//!
//! Read default is [`Sensitivity::Internal`], **not** the perception fail-closed
//! `Personal`: unlabeled rows predate the label (every pre-P3-2 memory) and must
//! not be silently reclassified as personal, which would strip them from group
//! chats they used to appear in. New perception writes carry an explicit label,
//! so only genuinely-legacy rows take the Internal default.

use duduclaw_core::Sensitivity;
use serde_json::Value;

/// Metadata JSON key under which the sensitivity label is stored.
pub const METADATA_KEY: &str = "sensitivity";

/// Stamp `s` into a memory metadata blob (additive — existing keys preserved).
/// A non-object metadata value is replaced by a fresh object carrying the label
/// (metadata blobs are objects by convention across the engine).
pub fn stamp_metadata(metadata: Option<Value>, s: Sensitivity) -> Value {
    let mut obj = match metadata {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert(
        METADATA_KEY.to_string(),
        Value::String(s.as_str().to_string()),
    );
    Value::Object(obj)
}

/// Read the sensitivity label from a memory metadata blob. Absent / malformed /
/// unknown value → [`Sensitivity::Internal`] (backward-compatible default; see
/// module docs on why this is not the perception `Personal` fail-closed).
pub fn read_from_metadata(metadata: Option<&Value>) -> Sensitivity {
    metadata
        .and_then(|v| v.get(METADATA_KEY))
        .and_then(|v| v.as_str())
        .and_then(Sensitivity::parse)
        .unwrap_or(Sensitivity::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stamp_into_empty_metadata_creates_object() {
        let v = stamp_metadata(None, Sensitivity::Personal);
        assert_eq!(v[METADATA_KEY], json!("personal"));
    }

    #[test]
    fn stamp_preserves_existing_keys() {
        let existing = json!({ "source_mistakes": [1, 2], "note": "x" });
        let v = stamp_metadata(Some(existing), Sensitivity::Restricted);
        assert_eq!(v["source_mistakes"], json!([1, 2]));
        assert_eq!(v["note"], json!("x"));
        assert_eq!(v[METADATA_KEY], json!("restricted"));
    }

    #[test]
    fn stamp_replaces_non_object_metadata() {
        let v = stamp_metadata(Some(json!("a bare string")), Sensitivity::Internal);
        assert!(v.is_object());
        assert_eq!(v[METADATA_KEY], json!("internal"));
    }

    #[test]
    fn read_roundtrips_stamped_value() {
        let v = stamp_metadata(None, Sensitivity::Restricted);
        assert_eq!(read_from_metadata(Some(&v)), Sensitivity::Restricted);
    }

    #[test]
    fn read_defaults_to_internal_when_absent_or_malformed() {
        assert_eq!(read_from_metadata(None), Sensitivity::Internal);
        assert_eq!(read_from_metadata(Some(&json!({}))), Sensitivity::Internal);
        assert_eq!(
            read_from_metadata(Some(&json!({ "sensitivity": "bogus" }))),
            Sensitivity::Internal
        );
        assert_eq!(
            read_from_metadata(Some(&json!({ "sensitivity": 5 }))),
            Sensitivity::Internal
        );
    }
}
