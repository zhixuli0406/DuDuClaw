//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

#[test]
fn get_returns_safe_defaults_when_absent() {
    let table = toml::Table::new();
    let resp = skill_synthesis_table_to_response(&table);
    assert_eq!(resp["auto_run"], json!(false));
    assert_eq!(resp["dry_run"], json!(true));
    assert_eq!(resp["interval_hours"], json!(24));
    assert_eq!(resp["lookback_days"], json!(1));
    assert_eq!(resp["target_agent"], json!(""));
}

#[test]
fn apply_sets_all_fields_and_roundtrips() {
    let mut table = toml::Table::new();
    let params = json!({
        "auto_run": true,
        "dry_run": false,
        "interval_hours": 6,
        "lookback_days": 3,
        "target_agent": "agnes",
    });
    let changes = apply_skill_synthesis_to_table(&mut table, &params).unwrap();
    assert_eq!(changes.len(), 5);

    let resp = skill_synthesis_table_to_response(&table);
    assert_eq!(resp["auto_run"], json!(true));
    assert_eq!(resp["dry_run"], json!(false));
    assert_eq!(resp["interval_hours"], json!(6));
    assert_eq!(resp["lookback_days"], json!(3));
    assert_eq!(resp["target_agent"], json!("agnes"));
}

#[test]
fn apply_rejects_out_of_range_lookback() {
    let mut table = toml::Table::new();
    let err = apply_skill_synthesis_to_table(&mut table, &json!({ "lookback_days": 99 }))
        .unwrap_err();
    assert!(err.contains("lookback_days"), "got: {err}");
}

#[test]
fn apply_rejects_zero_interval() {
    let mut table = toml::Table::new();
    let err = apply_skill_synthesis_to_table(&mut table, &json!({ "interval_hours": 0 }))
        .unwrap_err();
    assert!(err.contains("interval_hours"), "got: {err}");
}

#[test]
fn apply_blank_target_clears_key() {
    let mut table = toml::Table::new();
    // Seed an existing value, then clear it.
    apply_skill_synthesis_to_table(&mut table, &json!({ "target_agent": "agnes" })).unwrap();
    let changes =
        apply_skill_synthesis_to_table(&mut table, &json!({ "target_agent": "  " })).unwrap();
    assert!(
        changes.iter().any(|c| c.contains("cleared")),
        "got: {changes:?}"
    );
    let resp = skill_synthesis_table_to_response(&table);
    assert_eq!(resp["target_agent"], json!(""));
}

#[test]
fn apply_rejects_path_traversal_in_target() {
    let mut table = toml::Table::new();
    for bad in ["../etc", "a/b", "a\\b"] {
        let err = apply_skill_synthesis_to_table(&mut table, &json!({ "target_agent": bad }))
            .unwrap_err();
        assert!(
            err.contains("invalid characters"),
            "expected reject for {bad}, got: {err}"
        );
    }
}

#[test]
fn apply_empty_params_yields_no_changes() {
    let mut table = toml::Table::new();
    let changes = apply_skill_synthesis_to_table(&mut table, &json!({})).unwrap();
    assert!(changes.is_empty(), "empty params must produce no changes");
}
