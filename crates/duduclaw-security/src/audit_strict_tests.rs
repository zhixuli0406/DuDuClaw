use super::*;

#[test]
fn strict_append_persists_a_chained_event() {
    let home = super::tests::fresh_home();
    let event = AuditEvent::new("team_history_viewed", "sales", Severity::Info, serde_json::json!({"viewer":"u-1"}));
    try_append_audit_event(&home, &event).unwrap();
    let rows = read_recent_events(&home, 10);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event_type, "team_history_viewed");
    assert_eq!(rows[0].details["viewer"], "u-1");
    let chain = crate::audit_chain::verify_chain(&home.join("security_audit.jsonl")).unwrap();
    assert!(chain.is_intact(), "{chain:?}");
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn strict_append_refuses_an_unwritable_log_target() {
    let home = super::tests::fresh_home();
    std::fs::create_dir(home.join("security_audit.jsonl")).unwrap();
    let event = AuditEvent::new("team_history_viewed", "sales", Severity::Info, serde_json::json!({}));
    assert!(try_append_audit_event(&home, &event).is_err());
    std::fs::remove_dir_all(&home).unwrap();
}
