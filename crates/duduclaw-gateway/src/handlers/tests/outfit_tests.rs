//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::normalize_outfit;
use serde_json::json;

#[test]
fn normalizes_partial_outfit_and_fills_slots() {
    let v = normalize_outfit(&json!({ "hat": "cap", "tint": 3 })).unwrap();
    assert_eq!(v["schema"], 1);
    assert_eq!(v["tint"], 3);
    assert_eq!(v["hat"], "cap");
    assert_eq!(v["feet"], "");
    assert_eq!(v["accessory"], "");
}

#[test]
fn rejects_bad_shapes_fail_closed() {
    for bad in [
        json!("cap"),                     // not an object
        json!({ "hat": 3 }),              // non-string slot
        json!({ "hat": "CAP" }),          // uppercase
        json!({ "hat": "a".repeat(25) }), // too long
        json!({ "hat": "../x" }),         // traversal chars
        json!({ "tint": 11 }),            // out of range
        json!({ "sneaky_extra": "x" }),   // unknown key
    ] {
        assert!(normalize_outfit(&bad).is_err(), "{bad}");
    }
}
