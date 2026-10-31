use super::*;

fn record_at(home: &Path, text: &str, source: &str, now: i64) -> Vec<String> {
    record_with_key(home, text, &credential_detect::detect(text).unwrap(), source, "sender", "agent-a", "chat", None, now).unwrap()
}

#[test]
fn passwords_and_connection_strings_never_have_fingerprints() {
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
    let finding = credential_detect::detect(&key).unwrap().remove(0);
    assert!(fingerprint(&finding, &key, None).is_none());
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
    assert!(declare(home.path(), "other", &id, "mail").is_err());
    declare(home.path(), "agent-a", &id, "mail").unwrap();
    update(home.path(), &id, &json!({"service":"approved"})).unwrap();
    assert!(declare(home.path(), "agent-a", &id, "different").is_err());
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

// Security review regressions: written but not run (owner prohibits builds).
#[cfg(test)]
mod f2_f4_regressions {
    use super::*;

    #[test]
    fn context_only_values_have_no_fingerprint_even_when_long() {
        let home = tempfile::tempdir().unwrap();
        for (index, label) in ["api key", "token", "金鑰", "password"].iter().enumerate() {
            let text = format!("{label}: {}", "guessable".repeat(8));
            let findings = credential_detect::detect(&text).unwrap();
            assert_eq!(findings.len(), 1);
            assert!(!findings[0].fingerprint_eligible);
            record_at(home.path(), &text, &format!("source-{index}"), 100);
        }
        let db = open(home.path()).unwrap();
        let fingerprints: i64 = db.query_row("SELECT count(fingerprint) FROM credentials", [], |r| r.get(0)).unwrap();
        assert_eq!(fingerprints, 0);
    }

    #[tokio::test]
    async fn reconnects_share_authenticated_and_anonymous_quota_principals() {
        let principal = &crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL;
        let first = principal.scope("alice".into(), async { chat_principal("webchat:first", "webchat:first") }).await;
        let second = principal.scope("alice".into(), async { chat_principal("webchat:second", "webchat:second") }).await;
        assert_eq!(first, second);
        let home = tempfile::tempdir().unwrap();
        for i in 0..SENDER_LIMIT + 1 {
            let sender = principal.scope(format!("widget-visitor:{i}"), async {
                chat_principal(&format!("webchat:{i}"), &format!("webchat:{i}"))
            }).await;
            assert_eq!(sender, ANONYMOUS_CHAT);
            let text = "password: fixture";
            let ids = record_with_key(home.path(), text, &credential_detect::detect(text).unwrap(),
                &format!("source-{i}"), &sender, "agent", "chat", None, 100).unwrap();
            if i == SENDER_LIMIT { assert_eq!(ids, vec!["cred_throttled"]); }
        }
        assert_eq!(chat_principal("webchat:missing-auth", "spoofed"), ANONYMOUS_CHAT);
        assert_eq!(chat_principal("line:chat#agent:one", "one"), chat_principal("line:chat#agent:two", "two"));
        let db = open(home.path()).unwrap();
        let users: i64 = db.query_row("SELECT count(*) FROM credential_holders WHERE holder_kind='user'", [], |r| r.get(0)).unwrap();
        assert_eq!(users, 0);
        let ephemeral: i64 = db.query_row("SELECT count(*) FROM credentials WHERE provided_by LIKE 'widget-visitor:%' OR provided_by LIKE 'webchat:%'", [], |r| r.get(0)).unwrap();
        assert_eq!(ephemeral, 0);
    }

    #[test]
    fn deduplicated_credentials_cannot_grow_past_holder_limit() {
        let home = tempfile::tempdir().unwrap();
        let text = "password: fixture";
        let findings = credential_detect::detect(text).unwrap();
        let mut id = String::new();
        for i in 0..HOLDERS_PER_CREDENTIAL {
            let ids = record_with_key(home.path(), text, &findings, "same-source", ANONYMOUS_CHAT,
                &format!("agent-{i}"), "chat", None, 100).unwrap();
            id = ids[0].clone();
            assert_ne!(id, "cred_throttled");
        }
        assert_eq!(record_with_key(home.path(), text, &findings, "same-source", ANONYMOUS_CHAT,
            "one-too-many", "chat", None, 100).unwrap(), vec!["cred_throttled"]);
        let db = open(home.path()).unwrap();
        let count: i64 = db.query_row("SELECT count(*) FROM credential_holders WHERE credential_id=?1", [&id], |r| r.get(0)).unwrap();
        assert_eq!(count, HOLDERS_PER_CREDENTIAL);
        assert!(db.execute("INSERT INTO credential_holders VALUES(?1,'agent','bypass','0','routing',NULL)", [&id]).is_err());
    }

    #[test]
    fn total_holder_cap_applies_before_adding_an_existing_credential_holder() {
        let home = tempfile::tempdir().unwrap();
        let id = record_at(home.path(), "password: fixture", "same-source", 100).remove(0);
        let db = open(home.path()).unwrap();
        // Seed the cap without invoking a count trigger for each fixture row.
        db.execute_batch("DROP TRIGGER credential_holder_limit;
            WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<6250)
            INSERT INTO credentials(id,kind,service_source,source_kind,source_ref,provided_by,status,first_seen_at,last_seen_at)
            SELECT 'cap'||x,'password','detected','chat','cap'||x,'fixture','active','0','0' FROM n;
            WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<199998)
            INSERT INTO credential_holders(credential_id,holder_kind,holder_id,granted_at,granted_by)
            SELECT 'cap'||((x-1)/32+1),'agent','fixture-'||x,'0','routing' FROM n;").unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        let count: i64 = db.query_row("SELECT count(*) FROM credential_holders", [], |r| r.get(0)).unwrap();
        assert_eq!(count, HOLDER_LIMIT);
        let text = "password: fixture";
        assert_eq!(record_with_key(home.path(), text, &credential_detect::detect(text).unwrap(),
            "same-source", "sender", "new-agent", "chat", None, 101).unwrap(), vec!["cred_throttled"]);
        assert!(db.execute("INSERT INTO credential_holders VALUES(?1,'agent','bypass','0','routing',NULL)", [&id]).is_err());
    }

    #[test]
    fn quota_rows_expire_at_the_window_boundary() {
        let home = tempfile::tempdir().unwrap();
        record_at(home.path(), "password: fixture", "old", 100);
        record_at(home.path(), "password: fixture", "new", 3700);
        let db = open(home.path()).unwrap();
        let old: i64 = db.query_row("SELECT count(*) FROM detection_quota WHERE at<=100", [], |r| r.get(0)).unwrap();
        assert_eq!(old, 0);
    }

    #[test]
    fn upgrade_drops_unproven_fingerprints_and_ephemeral_holders() {
        let home = tempfile::tempdir().unwrap();
        let id = record_at(home.path(), "password: fixture", "source", 100).remove(0);
        let db = open(home.path()).unwrap();
        db.execute("UPDATE credentials SET fingerprint='old-unproven',note='old-note',provided_by='webchat:connection' WHERE id=?1", [&id]).unwrap();
        db.execute("INSERT INTO credential_holders VALUES(?1,'user','webchat:connection','0','routing',NULL)", [&id]).unwrap();
        db.execute_batch("PRAGMA user_version=0;").unwrap();
        drop(db);
        let db = open(home.path()).unwrap();
        let row: (Option<String>,String,String) = db.query_row("SELECT fingerprint,note_source,provided_by FROM credentials WHERE id=?1", [&id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(row, (None,"admin".into(),ANONYMOUS_CHAT.into()));
        let count: i64 = db.query_row("SELECT count(*) FROM credential_holders WHERE holder_id='webchat:connection'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn timeout_releases_delivery_permit_before_blocking_job_finishes() {
        let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = slots.clone().acquire_owned().await.unwrap();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let job = tokio::task::spawn_blocking(move || { blocked.recv().unwrap(); });
        assert!(wait_for_job(permit, job).await.is_none());
        assert_eq!(slots.available_permits(), 1);
        release.send(()).unwrap();
    }
}

// LINE durable retry regressions: written but not run (owner prohibits builds).
#[cfg(test)]
mod ingress_holder_tests {
    use super::*;

    #[test]
    fn expired_ingress_job_cannot_create_late_unrouted_metadata() {
        let home = tempfile::tempdir().unwrap();
        let text = "password: fixture";
        let deadline = std::time::Instant::now() - std::time::Duration::from_millis(1);
        assert!(record_with_deadline(home.path(), text, &credential_detect::detect(text).unwrap(),
            "line-ingress:late", "channel:line:chat", "", "chat", None, 100, Some(deadline)).is_err());
        let db = open_at(home.path(), 100).unwrap();
        let credentials: i64 = db.query_row("SELECT count(*) FROM credentials", [], |r| r.get(0)).unwrap();
        let links: i64 = db.query_row("SELECT count(*) FROM credential_ingress_links", [], |r| r.get(0)).unwrap();
        assert_eq!((credentials, links), (0, 0));
    }

    #[test]
    fn ingress_detection_routes_from_metadata_without_an_empty_agent_holder() {
        let home = tempfile::tempdir().unwrap();
        let text = "password: ingress-fixture";
        let id = record_with_key(home.path(), text, &credential_detect::detect(text).unwrap(),
            "line-ingress:event", "channel:line:chat", "", "chat", None, 100).unwrap().remove(0);
        let db = open_at(home.path(), 100).unwrap();
        let agents: i64 = db.query_row("SELECT count(*) FROM credential_holders WHERE holder_kind='agent'", [], |r| r.get(0)).unwrap();
        assert_eq!(agents, 0);
        attach_ingress_holder_inner(home.path(), "line-ingress:event", "routed-agent", 101).unwrap();
        attach_ingress_holder_inner(home.path(), "line-ingress:event", "routed-agent", 102).unwrap();
        let holders: i64 = db.query_row("SELECT count(*) FROM credential_holders WHERE credential_id=?1 AND holder_kind='agent' AND holder_id='routed-agent'", [&id], |r| r.get(0)).unwrap();
        assert_eq!(holders, 1);
        let links: i64 = db.query_row("SELECT count(*) FROM credential_ingress_links", [], |r| r.get(0)).unwrap();
        assert_eq!(links, 0);
        assert!(!get(home.path(), &id).unwrap().to_string().contains("ingress-fixture"));
    }

    #[test]
    fn ingress_link_routes_a_credential_deduplicated_from_another_source() {
        let home = tempfile::tempdir().unwrap();
        let id = record_at(home.path(), "password: fixture", "prior-source", 100).remove(0);
        let db = open_at(home.path(), 100).unwrap();
        db.execute("INSERT INTO credential_ingress_links VALUES('line-ingress:retry',?1,100)", [&id]).unwrap();
        attach_ingress_holder_inner(home.path(), "line-ingress:retry", "next-agent", 101).unwrap();
        let row = get(home.path(), &id).unwrap();
        assert_eq!(row["source_ref"], "prior-source");
        assert!(row["holders"].as_array().unwrap().iter().any(|holder| holder["holder_id"] == "next-agent"));
    }

    #[test]
    fn ingress_holder_limit_rolls_back_and_retains_retry_metadata() {
        let home = tempfile::tempdir().unwrap();
        let text = "password: fixture";
        let findings = credential_detect::detect(text).unwrap();
        for i in 0..HOLDERS_PER_CREDENTIAL {
            record_with_key(home.path(), text, &findings, "line-ingress:full", ANONYMOUS_CHAT,
                &format!("agent-{i}"), "chat", None, 100).unwrap();
        }
        assert!(attach_ingress_holder_inner(home.path(), "line-ingress:full", "overflow-agent", 101).is_err());
        let db = open_at(home.path(), 101).unwrap();
        let links: i64 = db.query_row("SELECT count(*) FROM credential_ingress_links WHERE source_ref='line-ingress:full'", [], |r| r.get(0)).unwrap();
        assert_eq!(links, 1);
        let overflow: i64 = db.query_row("SELECT count(*) FROM credential_holders WHERE holder_id='overflow-agent'", [], |r| r.get(0)).unwrap();
        assert_eq!(overflow, 0);
    }
}


#[test]
fn separate_values_without_fingerprints_keep_distinct_ids_in_both_modes() {
    for mode in ["audit", "mask"] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), format!("[security]\nchat_credentials='{mode}'\n")).unwrap();
        let text = "password: fixture-first\npassword: fixture-second";
        // Deterministically exercise the no-fingerprint path, independently
        // of the platform key store's availability.
        let findings = credential_detect::detect(text).unwrap();
        let ids = record_with_key(home.path(), text, &findings, "message", "sender", "agent", "chat", None, 100).unwrap();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert_eq!(record_with_key(home.path(), text, &findings, "message", "sender", "agent", "chat", None, 101).unwrap(), ids);
        assert_eq!(list(home.path(), &json!({})).unwrap()["total"], 2);
        let output = process(home.path(), text, "message", "sender", "agent", "chat");
        if mode == "audit" {
            assert_eq!(output, text);
        } else {
            assert_eq!(output, format!("password: [已遮罩的密碼 {}]\npassword: [已遮罩的密碼 {}]", ids[0], ids[1]));
        }
    }
}

#[test]
fn ordinal_mapping_and_five_per_message_cap_survive_retry_at_sender_limit() {
    let home = tempfile::tempdir().unwrap();
    let text = (0..7).map(|i| format!("password: fixture-{i}")).collect::<Vec<_>>().join("\n");
    let ids = record_at(home.path(), &text, "message", 100);
    assert_eq!(ids[..MESSAGE_LIMIT].iter().collect::<std::collections::HashSet<_>>().len(), MESSAGE_LIMIT);
    assert_eq!(&ids[MESSAGE_LIMIT..], &["cred_throttled", "cred_throttled"]);
    for i in 0..45 { record_at(home.path(), "password: fixture", &format!("other-{i}"), 100); }
    assert_eq!(record_at(home.path(), &text, "message", 101), ids);
    let masked = credential_detect::mask_with_ids(&text, &credential_detect::detect(&text).unwrap(), &ids);
    for (line, id) in masked.lines().zip(&ids) { assert!(line.contains(id)); }
    assert!(!masked.contains("fixture-"));
    assert_eq!(list(home.path(), &json!({})).unwrap()["total"], 50);
}

#[test]
fn ordinal_migration_preserves_legacy_metadata_and_first_position() {
    let home = tempfile::tempdir().unwrap();
    let old_id = record_at(home.path(), "password: fixture", "message", 100).remove(0);
    let db = open(home.path()).unwrap();
    db.execute_batch("DROP INDEX credential_source_kind; ALTER TABLE credentials DROP COLUMN source_ordinal; PRAGMA user_version=1;").unwrap();
    drop(db);
    let ids = record_at(home.path(), "password: fixture\npassword: second", "message", 101);
    assert_eq!(ids[0], old_id);
    assert_ne!(ids[1], old_id);
    let db = open(home.path()).unwrap();
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 2);
    assert_eq!(list(home.path(), &json!({})).unwrap()["total"], 2);
}


#[test]
fn distinct_api_keys_without_protected_key_map_to_their_own_masks() {
    let home = tempfile::tempdir().unwrap();
    let text = format!("sk-{}\nsk-{}", "a".repeat(40), "b".repeat(40));
    let findings = credential_detect::detect(&text).unwrap();
    assert_eq!(findings.len(), 2);
    let ids = record_with_key(home.path(), &text, &findings, "message", "sender", "agent", "chat", None, 100).unwrap();
    assert_ne!(ids[0], ids[1]);
    let masked = credential_detect::mask_with_ids(&text, &findings, &ids);
    let lines: Vec<_> = masked.lines().collect();
    assert!(lines[0].contains(&ids[0]));
    assert!(lines[1].contains(&ids[1]));
    assert_eq!(record_with_key(home.path(), &text, &findings, "message", "sender", "agent", "chat", None, 101).unwrap(), ids);
}
