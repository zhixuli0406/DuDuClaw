//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

fn table(body: &str) -> toml::Table {
    toml::from_str(body).expect("test fixture must parse")
}

#[test]
fn read_defaults_to_on() {
    assert_eq!(redaction_data_file_guard(&table("")), "on");
    assert_eq!(
        redaction_data_file_guard(&table("[redaction]\nenabled = true\n")),
        "on"
    );
}

#[test]
fn read_round_trips_the_three_modes_and_fails_safe_on_a_typo() {
    for mode in ["on", "read_only", "off"] {
        let t = table(&format!("[redaction]\ndata_file_guard = \"{mode}\"\n"));
        assert_eq!(redaction_data_file_guard(&t), mode);
    }
    let typo = table("[redaction]\ndata_file_guard = \"readonly\"\n");
    assert_eq!(redaction_data_file_guard(&typo), "on");
    let wrong_type = table("[redaction]\ndata_file_guard = 1\n");
    assert_eq!(redaction_data_file_guard(&wrong_type), "on");
}

#[test]
fn update_writes_the_value_and_records_the_change() {
    let mut t = table("[redaction]\nenabled = true\n");
    let mut changes = Vec::new();
    apply_data_file_guard_to_table(
        &mut t,
        &json!({ "data_file_guard": "READ_ONLY" }),
        &mut changes,
    )
    .expect("valid mode");
    assert_eq!(redaction_data_file_guard(&t), "read_only");
    assert_eq!(changes, vec!["redaction.data_file_guard = read_only"]);
}

#[test]
fn update_refuses_an_unrecognized_mode_instead_of_silently_correcting_it() {
    let mut t = table("[redaction]\n");
    let mut changes = Vec::new();
    let err = apply_data_file_guard_to_table(
        &mut t,
        &json!({ "data_file_guard": "readonly" }),
        &mut changes,
    )
    .expect_err("typo must be rejected");
    assert!(err.contains("read_only"), "{err}");
    assert!(changes.is_empty());
    assert!(
        t.get("redaction")
            .and_then(|r| r.get("data_file_guard"))
            .is_none(),
        "a rejected update must write nothing"
    );

    let mut changes = Vec::new();
    assert!(
        apply_data_file_guard_to_table(
            &mut t,
            &json!({ "data_file_guard": true }),
            &mut changes
        )
        .is_err(),
        "a non-string must be rejected"
    );
}

#[test]
fn an_absent_field_is_a_no_op() {
    let mut t = table("[redaction]\nenabled = true\n");
    let before = t.clone();
    let mut changes = Vec::new();
    apply_data_file_guard_to_table(&mut t, &json!({ "enabled": false }), &mut changes).unwrap();
    assert!(changes.is_empty());
    assert_eq!(t, before);
}
