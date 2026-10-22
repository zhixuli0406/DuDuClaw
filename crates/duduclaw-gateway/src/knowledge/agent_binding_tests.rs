//! `[knowledge] collections` parsing and table writes.

use serde_json::json;

use super::*;

fn agent() -> AgentRef {
    AgentRef {
        id: "a1".into(),
        display_name: "A1".into(),
        department: None,
    }
}

#[test]
fn null_and_empty_need_no_database() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        validate(home.path(), &agent(), &json!({"collections": null})).unwrap(),
        Binding::AllGranted
    );
    assert_eq!(
        validate(home.path(), &agent(), &json!({"collections": []})).unwrap(),
        Binding::Only(vec![])
    );
}

#[test]
fn shapes_are_checked() {
    let home = tempfile::tempdir().unwrap();
    for bad in [
        json!(null),
        json!([]),
        json!({}),
        json!({"collections": 3}),
        json!({"collections": [true]}),
    ] {
        assert!(validate(home.path(), &agent(), &bad).is_err(), "{bad}");
    }
    assert!(validate(home.path(), &agent(), &json!({"collections": ["../x"]})).is_err());
    let too_many: Vec<String> = (0..=MAX_BOUND_COLLECTIONS)
        .map(|i| format!("c{i}"))
        .collect();
    assert!(validate(home.path(), &agent(), &json!({"collections": too_many})).is_err());
}

#[test]
fn ungranted_slugs_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let e = validate(home.path(), &agent(), &json!({"collections": ["hr"]})).unwrap_err();
    assert!(e.contains("hr"), "{e}");
}

#[test]
fn table_writes_keep_other_keys() {
    let mut t: toml::Table = "[knowledge]\nauto_retrieve = true\n".parse().unwrap();
    apply_to_table(&mut t, &Binding::Only(vec!["hr".into(), "faq".into()])).unwrap();
    assert_eq!(t["knowledge"]["collections"].as_array().unwrap().len(), 2);
    assert_eq!(t["knowledge"]["auto_retrieve"].as_bool(), Some(true));
    apply_to_table(&mut t, &Binding::AllGranted).unwrap();
    assert!(t["knowledge"].get("collections").is_none());
    assert_eq!(t["knowledge"]["auto_retrieve"].as_bool(), Some(true));

    let mut bare: toml::Table = "[knowledge]\ncollections = ['a']\n".parse().unwrap();
    apply_to_table(&mut bare, &Binding::AllGranted).unwrap();
    assert!(
        bare.get("knowledge").is_none(),
        "an emptied section goes away"
    );
    let mut none = toml::Table::new();
    apply_to_table(&mut none, &Binding::Only(vec![])).unwrap();
    assert_eq!(
        none["knowledge"]["collections"].as_array().unwrap().len(),
        0
    );
}

#[test]
fn a_non_table_knowledge_key_is_refused_not_overwritten() {
    for binding in [Binding::AllGranted, Binding::Only(vec!["hr".into()])] {
        let mut t: toml::Table = "knowledge = 3\n".parse().unwrap();
        assert!(apply_to_table(&mut t, &binding).is_err());
        assert_eq!(t["knowledge"].as_integer(), Some(3));
    }
}
