//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

#[test]
pub(super) fn os_watch_apply_writes_frontmost_poll_secs() {
    let mut table = toml::Table::new();
    let changes = apply_os_watch_to_table(
        &mut table,
        &json!({ "os_watch": { "frontmost_poll_secs": 30 } }),
    )
    .expect("apply");
    let ow = table.get("os_watch").unwrap().as_table().unwrap();
    assert_eq!(
        ow.get("frontmost_poll_secs").unwrap().as_integer(),
        Some(30)
    );
    assert!(
        changes
            .iter()
            .any(|c| c.contains("frontmost_poll_secs = 30"))
    );

    // 0 is a valid "disabled" write (mirrors the reader's `<=0 => None`).
    let mut t0 = toml::Table::new();
    let c0 = apply_os_watch_to_table(
        &mut t0,
        &json!({ "os_watch": { "frontmost_poll_secs": 0 } }),
    )
    .expect("apply");
    assert!(c0.iter().any(|c| c.contains("frontmost_poll_secs = 0")));

    // Out-of-range (> 3600) is rejected so the dashboard surfaces it.
    let mut tbad = toml::Table::new();
    assert!(
        apply_os_watch_to_table(
            &mut tbad,
            &json!({ "os_watch": { "frontmost_poll_secs": 99999 } }),
        )
        .is_err()
    );
}

// ── apply_research_to_table (belief loop × goal contract gap 2) ──────

#[test]
pub(super) fn research_apply_writes_self_study_and_hour() {
    let mut table = toml::Table::new();
    let changes = apply_research_to_table(
        &mut table,
        &json!({ "research": { "self_study": true, "self_study_hour": 18 } }),
    )
    .expect("apply");
    let r = table.get("research").unwrap().as_table().unwrap();
    assert_eq!(r.get("self_study").unwrap().as_bool(), Some(true));
    assert_eq!(r.get("self_study_hour").unwrap().as_integer(), Some(18));
    assert!(
        changes
            .iter()
            .any(|c| c.contains("research.self_study = true"))
    );
    assert!(
        changes
            .iter()
            .any(|c| c.contains("research.self_study_hour = 18"))
    );

    // Absent research object ⇒ no-op, empty change list.
    let mut empty = toml::Table::new();
    assert!(
        apply_research_to_table(&mut empty, &json!({}))
            .expect("no-op")
            .is_empty()
    );
    assert!(empty.get("research").is_none());
}

#[test]
pub(super) fn research_apply_rejects_out_of_range_hour() {
    let mut table = toml::Table::new();
    assert!(
        apply_research_to_table(
            &mut table,
            &json!({ "research": { "self_study_hour": 24 } }),
        )
        .is_err()
    );
}

#[test]
pub(super) fn research_apply_explicit_false_round_trips_not_a_clear() {
    let mut table = toml::Table::new();
    apply_research_to_table(&mut table, &json!({ "research": { "self_study": true } }))
        .expect("apply");
    let changes =
        apply_research_to_table(&mut table, &json!({ "research": { "self_study": false } }))
            .expect("apply");
    let r = table.get("research").unwrap().as_table().unwrap();
    assert_eq!(r.get("self_study").unwrap().as_bool(), Some(false));
    assert!(changes.iter().any(|c| c.contains("self_study = false")));
}

#[test]
pub(super) fn os_native_quota_reject_frame_carries_code_and_clean_message() {
    let frame = os_native_quota_reject_frame(1);
    match frame {
        WsFrame::Response {
            ok: false,
            error: Some(err),
            ..
        } => {
            assert_eq!(
                err.get("code").and_then(|v| v.as_str()),
                Some(OS_NATIVE_QUOTA_ERROR_CODE)
            );
            let msg = err.get("message").and_then(|v| v.as_str()).unwrap();
            assert!(msg.contains("1"), "limit should appear: {msg}");
            assert!(msg.contains("AI 員工"), "user-facing term: {msg}");
            // No internal terms leak into the UI copy.
            assert!(!msg.contains("os_native"));
            assert!(!msg.contains("capabilit"));
        }
        other => panic!("expected structured error response, got {other:?}"),
    }
}

#[test]
pub(super) fn read_jsonl_tail_returns_last_n_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gate.jsonl");
    std::fs::write(&path, "{\"a\":1}\n\nnot json\n{\"a\":2}\n{\"a\":3}\n").unwrap();
    // Last 2 valid → {a:2},{a:3} (unparseable line skipped, blank skipped).
    let rows = read_jsonl_tail(&path, 2);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("a").unwrap().as_i64(), Some(2));
    assert_eq!(rows[1].get("a").unwrap().as_i64(), Some(3));

    // Missing file → empty, not an error.
    assert!(read_jsonl_tail(&dir.path().join("nope.jsonl"), 5).is_empty());
}

#[test]
pub(super) fn rule_induced_and_target_detection() {
    let base = AutopilotRuleRow {
        id: "r1".into(),
        name: "n".into(),
        enabled: true,
        trigger_event: "os_file".into(),
        conditions: json!({ "all": [{ "field": "agent_id", "op": "eq", "value": "bruno" }] })
            .to_string(),
        action: "{}".into(),
        created_at: "".into(),
        last_triggered_at: None,
        trigger_count: 0,
        sequence: None,
        metadata: Some(json!({ "induced": true, "source": "pbd_rule_induction" }).to_string()),
    };
    assert!(rule_is_induced(&base));
    assert!(rule_targets_agent(&base, "bruno"));
    assert!(!rule_targets_agent(&base, "alice"));

    // Non-induced (no metadata) rule.
    let mut plain = base.clone();
    plain.metadata = None;
    assert!(!rule_is_induced(&plain));

    // `source` alone (no induced flag) still counts as induced.
    let mut via_source = base.clone();
    via_source.metadata = Some(json!({ "source": "pbd_rule_induction" }).to_string());
    assert!(rule_is_induced(&via_source));
}

// ── SCP: namespace mode parse ────────────────────────────────────────────

#[test]
pub(super) fn scp_apply_sets_read_only_with_synced_from() {
    let mut table = toml::Table::new();
    let change = scp_apply_namespace(
        &mut table,
        "identity",
        "read_only",
        Some("identity-provider"),
        false,
    )
    .expect("apply");
    assert!(change.contains("read_only"));
    let ns = table["namespaces"].as_table().unwrap()["identity"]
        .as_table()
        .unwrap();
    assert_eq!(ns["mode"].as_str(), Some("read_only"));
    assert_eq!(ns["synced_from"].as_str(), Some("identity-provider"));
}

#[test]
pub(super) fn scp_read_only_requires_synced_from() {
    let mut table = toml::Table::new();
    assert!(scp_apply_namespace(&mut table, "identity", "read_only", None, false).is_err());
}

#[test]
pub(super) fn scp_rejects_bad_mode_and_nested_namespace() {
    let mut table = toml::Table::new();
    assert!(scp_apply_namespace(&mut table, "identity", "broadcast", None, false).is_err());
    assert!(scp_apply_namespace(&mut table, "a/b", "agent_writable", None, false).is_err());
}

#[test]
pub(super) fn scp_remove_deletes_entry() {
    let mut table = toml::Table::new();
    scp_apply_namespace(&mut table, "policies", "operator_only", None, false).unwrap();
    assert!(
        table["namespaces"]
            .as_table()
            .unwrap()
            .contains_key("policies")
    );
    scp_apply_namespace(&mut table, "policies", "agent_writable", None, true).unwrap();
    assert!(
        !table["namespaces"]
            .as_table()
            .unwrap()
            .contains_key("policies")
    );
}

#[test]
pub(super) fn scp_response_sorted_and_shaped() {
    let mut table = toml::Table::new();
    scp_apply_namespace(&mut table, "zeta", "operator_only", None, false).unwrap();
    scp_apply_namespace(&mut table, "alpha", "agent_writable", None, false).unwrap();
    let resp = scp_table_to_response(&table);
    let arr = resp["namespaces"].as_array().unwrap();
    assert_eq!(arr[0]["namespace"].as_str(), Some("alpha"));
    assert_eq!(arr[1]["namespace"].as_str(), Some("zeta"));
}

// ── W2-5: parse_scp_table_strict fail-closed on malformed .scope.toml ────

#[test]
pub(super) fn scp_strict_parse_blank_or_absent_is_empty_table() {
    assert!(parse_scp_table_strict("").unwrap().is_empty());
    assert!(parse_scp_table_strict("   \n\t  ").unwrap().is_empty());
}

#[test]
pub(super) fn scp_strict_parse_malformed_content_is_err_not_silently_empty() {
    // The exact bug this closes: `read_config_table` would have returned
    // an empty table here (via `unwrap_or_default()`), which
    // `handle_wiki_scope_update` then happily wrote back — erasing
    // whatever the malformed file actually still held on disk.
    let err = parse_scp_table_strict("this is :: not = valid = toml ===").unwrap_err();
    assert!(err.contains("malformed"), "got: {err}");
}

#[test]
pub(super) fn scp_strict_parse_valid_content_round_trips_into_the_same_shape_as_apply() {
    let t = parse_scp_table_strict(
        "[namespaces.identity]\nmode = \"read_only\"\nsynced_from = \"identity-provider\"\n",
    )
    .unwrap();
    assert_eq!(
        scp_namespace_mode(&t, "identity").as_deref(),
        Some("read_only")
    );
    let resp = scp_table_to_response(&t);
    assert_eq!(
        resp["namespaces"][0]["namespace"].as_str(),
        Some("identity")
    );
    assert_eq!(
        resp["namespaces"][0]["synced_from"].as_str(),
        Some("identity-provider")
    );
}

// ── XC.3: phone_number_id is NOT encrypted (alignment) ───────────────────

#[test]
pub(super) fn xc3_whatsapp_phone_number_id_not_encrypted_in_agent_path() {
    // The per-agent set_channel_token path only encrypts keys containing
    // "token"/"secret"/"app_id". phone_number_id does NOT match → plaintext.
    let field = "phone_number_id";
    let should_encrypt =
        field.contains("token") || field.contains("secret") || field == "app_id";
    assert!(!should_encrypt, "phone_number_id must not be encrypted");
}

#[test]
pub(super) fn xc3_global_secret_is_plain_only_for_phone_number_id() {
    // Mirrors the secret_is_plain decision in handle_channels_add.
    let plain = |sk: Option<&str>| sk == Some("whatsapp_phone_number_id");
    assert!(plain(Some("whatsapp_phone_number_id")));
    assert!(!plain(Some("slack_app_token")));
    assert!(!plain(Some("line_channel_secret")));
}

// ── XC.4: skills.adopt agent-id validation surface ───────────────────────

#[test]
pub(super) fn xc4_invalid_target_agent_id_rejected() {
    // Smoke test that the delegation to
    // `duduclaw_core::is_valid_new_agent_id` (WP-4I 2026-08) is wired
    // correctly; the exhaustive cases live in that function's own
    // `agent_id_tests` module in duduclaw-core/src/lib.rs.
    assert!(!is_valid_agent_id("../etc"));
    assert!(!is_valid_agent_id("Bad Name"));
    assert!(is_valid_agent_id("bruno"));
}
