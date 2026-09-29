//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

#[test]
fn known_checks_map_to_their_specific_hint() {
    assert!(doctor_repair_hint("config_file").contains("duduclaw init"));
    assert!(doctor_repair_hint("agents").contains("duduclaw agent create"));
    assert!(doctor_repair_hint("api_key").contains("ANTHROPIC_API_KEY"));
}

#[test]
fn unknown_check_falls_back_to_generic_hint() {
    assert_eq!(
        doctor_repair_hint("mcp_server"),
        "Check the documentation for repair instructions."
    );
    assert_eq!(
        doctor_repair_hint("totally_unknown"),
        "Check the documentation for repair instructions."
    );
}
