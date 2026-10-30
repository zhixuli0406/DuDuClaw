use super::*;

fn record_at(home: &Path, text: &str, source: &str, now: i64) -> Vec<String> {
    record_with_key(home, text, &credential_detect::detect(text).unwrap(), source, "sender", "agent-a", "chat", None, now).unwrap()
}

#[test]
fn passwords_and_connection_strings_never_have_fingerprints() {
    assert!(fingerprint("password", "a".repeat(80).as_str(), None).is_none());
    assert!(fingerprint("connection_string", "postgres://u:test-value@localhost/db", None).is_none());
    let home = tempfile::tempdir().unwrap();
    record_at(home.path(), "password: test-value", "s1", 100);
    record_at(home.path(), "password: test-value", "s1", 101);
    record_at(home.path(), "password: test-value", "s2", 102);
    let db = open(home.path()).unwrap();
    let (count, fingerprints): (i64,i64) = db.query_row("SELECT count(*),count(fingerprint) FROM credentials", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((count,fingerprints), (2,0));
}

#[test]
fn unavailable_protection_never_falls_back_to_hash() {
    let key = ["sk-", &"a".repeat(40)].concat();
    assert!(fingerprint("api_key", &key, None).is_none());
    let home = tempfile::tempdir().unwrap();
    record_at(home.path(), &key, "s1", 100);
    record_at(home.path(), &key, "s2", 101);
    assert_eq!(list(home.path(), &json!({})).unwrap()["total"], 2);
}

#[test]
fn quotas_bound_message_sender_and_total_rows() {
    let home = tempfile::tempdir().unwrap();
    let text = (0..9).map(|i| format!("password: fixture-{i}")).collect::<Vec<_>>().join("\n");
    let ids = record_at(home.path(), &text, "message", 100);
    assert_eq!(ids.iter().filter(|id| id.as_str() == "cred_throttled").count(), 4);
    for i in 0..45 { record_at(home.path(), "password: fixture", &format!("s{i}"), 100); }
    assert_eq!(record_at(home.path(), "password: fixture", "blocked", 101), vec!["cred_throttled"]);
    assert_ne!(record_at(home.path(), "password: fixture", "later", 3701), vec!["cred_throttled"]);
    let db = open(home.path()).unwrap();
    db.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT OR IGNORE INTO credentials(id,kind,service_source,source_kind,source_ref,provided_by,status,first_seen_at,last_seen_at) SELECT 'f'||x,'password','detected','chat','f'||x,'f','active','0','0' FROM n;").unwrap();
    assert_eq!(record_at(home.path(), "password: fixture", "full", 7402), vec!["cred_throttled"]);
}

#[test]
fn throttle_events_aggregate_and_never_contain_values_or_accounts() {
    let home = tempfile::tempdir().unwrap();
    let text = "帳號 alice@example.com\n".to_string() + &"password: private-fixture\n".repeat(8);
    record_at(home.path(), &text, "source", 100);
    let raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(!raw.contains("private-fixture"));
    assert!(!raw.contains("alice@example.com"));
    let events: Vec<Value> = raw.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let throttles: Vec<_> = events.iter().filter(|e| e["event_type"] == "credential_detection_throttled").collect();
    assert_eq!(throttles.len(), 1);
    assert_eq!(throttles[0]["details"]["skipped"], 3);
    let row = &list(home.path(), &json!({})).unwrap()["credentials"][0];
    assert_eq!(row["account_hint"], "a***@example.com");
}

#[test]
fn mask_mode_masks_all_findings_even_when_ledger_fails() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[security]\nchat_credentials='mask'\n").unwrap();
    std::fs::write(home.path().join("credentials"), "blocked directory").unwrap();
    let text = "password: private-fixture\n".repeat(9);
    let masked = process(home.path(), &text, "source", "sender", "agent", "chat");
    assert!(!masked.contains("private-fixture"));
    assert_eq!(masked.matches("[已遮罩的密碼 cred_unavailable]").count(), 9);
}

#[test]
fn audit_mode_retains_history_but_unknown_configuration_masks() {
    let home = tempfile::tempdir().unwrap();
    let text = "password: private-fixture";
    assert_eq!(process(home.path(), text, "source", "sender", "agent", "chat"), text);
    for raw in ["[security]\nchat_credentials='typo'", "[security]\nchat_credentials=1", "broken["] {
        std::fs::write(home.path().join("config.toml"), raw).unwrap();
        assert_eq!(mode(home.path()), "mask");
        assert!(!process(home.path(), text, "s2", "sender", "agent", "chat").contains("private-fixture"));
    }
}

#[test]
fn declarations_require_holder_and_cannot_override_admin() {
    let home = tempfile::tempdir().unwrap();
    let id = record_at(home.path(), "password: private-fixture", "source", 100).remove(0);
    assert!(declare(home.path(), "other", &id, "mail", None).is_err());
    declare(home.path(), "agent-a", &id, "mail", None).unwrap();
    update(home.path(), &id, &json!({"service":"approved"})).unwrap();
    assert!(declare(home.path(), "agent-a", &id, "different", None).is_err());
    assert_eq!(get(home.path(), &id).unwrap()["service"], "approved");
    assert!(update(home.path(), &id, &json!({"value":"forbidden"})).is_err());
}

#[test]
fn metadata_updates_and_exports_emit_value_free_events() {
    let home = tempfile::tempdir().unwrap();
    let id = record_at(home.path(), "password: private-fixture", "source", 100).remove(0);
    update(home.path(), &id, &json!({"service":"mail", "note":"password: private-note", "status":"false_positive"})).unwrap();
    let rows = list(home.path(), &json!({"status":"false_positive", "export":true})).unwrap();
    assert_eq!(rows["total"], 1);
    assert!(!rows.to_string().contains("private-note"));
    let events = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    for name in ["credential_service_set", "credential_status_changed", "credential_exported"] { assert!(events.contains(name)); }
    assert!(!events.contains("private-note"));
    assert!(!events.contains("private-fixture"));
}

#[test]
fn export_refuses_when_audit_append_fails() {
    let home = tempfile::tempdir().unwrap();
    open(home.path()).unwrap();
    std::fs::create_dir(home.path().join("security_audit.jsonl")).unwrap();
    assert!(list(home.path(), &json!({"export":true})).is_err());
}

#[cfg(unix)]
#[test]
fn ledger_is_private_and_symlinks_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let home = tempfile::tempdir().unwrap();
    open(home.path()).unwrap();
    assert_eq!(std::fs::metadata(home.path().join("credentials/ledger.db")).unwrap().permissions().mode() & 0o777, 0o600);
    let other = tempfile::tempdir().unwrap();
    symlink(home.path().join("credentials"), other.path().join("credentials")).unwrap();
    assert!(open(other.path()).is_err());
}

#[test]
fn late_background_write_resolves_already_bound_message_source() {
    let home = tempfile::tempdir().unwrap();
    let source = "session:pending:test";
    source_links().lock().unwrap().insert((home.path().to_path_buf(), source.into()), None);
    bind_message(home.path(), source, "session", 42);
    record_at(home.path(), "password: fixture", source, 100);
    finish_source(home.path(), source);
    let row = &list(home.path(), &json!({})).unwrap()["credentials"][0];
    assert_eq!(row["source_ref"], "session:m:42");
}

#[tokio::test]
async fn full_worker_capacity_masks_without_blocking_delivery() {
    let home = tempfile::tempdir().unwrap();
    let permit = work_slots().acquire_many(4).await.unwrap();
    let text = process_message(home.path(), "password: private-fixture", "source", "sender", "agent", "chat").await;
    assert!(!text.contains("private-fixture"));
    assert!(!home.path().join("credentials/ledger.db").exists());
    drop(permit);
}

#[test]
fn credential_ids_use_ulid_encoding() {
    let id = new_id();
    let suffix = id.strip_prefix("cred_").unwrap();
    assert_eq!(suffix.len(), 26);
    assert!(suffix.chars().next().is_some_and(|c| matches!(c, '0'..='7')));
    assert!(suffix.chars().all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".chars().any(|allowed| allowed == c)));
    assert_ne!(id, new_id());
}
