//! End-to-end integration tests for the redaction pipeline.
//!
//! These tests exercise the full Manager → Pipeline → Vault → Egress flow
//! using the public API only (no internal access). They cover the three
//! scenarios the design promises:
//!
//! 1. **Tool result round trip** — sensitive data from an `odoo.*` tool is
//!    redacted before LLM sees it, then restored on its way to the user.
//! 2. **Tool egress whitelist** — `send_email` restores, `web_fetch` denies.
//! 3. **Fail-closed + restart resilience** — vault survives process restart;
//!    a hallucinated token never decrypts.

use duduclaw_redaction::{
    Caller, DataSourceDef, EgressDecision, ManagerPaths, RedactionConfig, RedactionManager,
    RestoreArgsMode, RestoreScope, RestoreTarget, RuleKind, RuleSpec, Source, ToolContext,
    ToolEgressRule,
};
use std::collections::{BTreeMap, HashMap};
use tempfile::TempDir;

fn config_for_test() -> RedactionConfig {
    let mut cfg = RedactionConfig::default();
    cfg.enabled = true;
    cfg.profiles = vec!["taiwan_strict".into(), "general".into()];

    let mut egress = HashMap::new();
    egress.insert(
        "send_email".into(),
        ToolEgressRule {
            restore_args: RestoreArgsMode::Restore,
            audit_reveal: true,
        },
    );
    egress.insert(
        "odoo.*".into(),
        ToolEgressRule {
            restore_args: RestoreArgsMode::Restore,
            audit_reveal: false,
        },
    );
    egress.insert(
        "log_event".into(),
        ToolEgressRule {
            restore_args: RestoreArgsMode::Passthrough,
            audit_reveal: false,
        },
    );
    // web_fetch deliberately absent → default deny
    cfg.tool_egress = egress;
    cfg
}

#[test]
fn full_round_trip_odoo_tool_result_to_channel_reply() {
    let tmp = TempDir::new().unwrap();
    let paths = ManagerPaths::under_home(tmp.path());
    let manager = RedactionManager::open(config_for_test(), paths).unwrap();
    let pipeline = manager.pipeline("agnes", Some("session-001".into())).unwrap();

    // 1. Simulated odoo.search_partner response.
    let tool_result = r#"
        Customer: Alice Wong
        Email: alice@acme.com
        National ID: A123456789
        Phone: 0912345678
    "#;

    let redacted = pipeline
        .redact(
            tool_result,
            &Source::ToolResult { tool_name: "odoo.search_partner".into() },
        )
        .unwrap();

    // 2. The LLM-bound payload contains tokens, not original values.
    assert!(!redacted.redacted_text.contains("alice@acme.com"));
    assert!(!redacted.redacted_text.contains("A123456789"));
    assert!(!redacted.redacted_text.contains("0912345678"));
    assert!(redacted.redacted_text.contains("<REDACT:EMAIL:"));
    assert!(redacted.redacted_text.contains("<REDACT:TW_ID:"));
    assert!(redacted.redacted_text.contains("<REDACT:TW_MOBILE:"));

    // 3. Channel-reply restore returns the user's view (original values).
    let restored = pipeline
        .restore(
            &redacted.redacted_text,
            &Caller::owner("agnes"),
            RestoreTarget::UserChannel,
        )
        .unwrap();
    assert!(restored.contains("alice@acme.com"));
    assert!(restored.contains("A123456789"));
    assert!(restored.contains("0912345678"));
}

#[test]
fn send_email_tool_call_restores_args_and_executes() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();

    // Tool result yields a token.
    let red = pipeline
        .redact(
            "alice@acme.com",
            &Source::ToolResult { tool_name: "odoo.search_partner".into() },
        )
        .unwrap();
    let email_token = red.tokens_written[0].as_str().to_string();

    // LLM constructs a tool call using the token.
    let llm_tool_call = serde_json::json!({
        "to": email_token,
        "subject": "Order confirmation",
        "body": format!("Dear customer at {}, your order is confirmed.", email_token),
    });

    // Egress evaluator restores tokens and yields the executable payload.
    let dec = manager
        .decide_tool_call("send_email", &llm_tool_call, "agnes", Some("s1"), &duduclaw_redaction::Caller::owner("agnes"))
        .unwrap();
    match dec {
        EgressDecision::Allow { args, tokens_restored } => {
            assert_eq!(tokens_restored, 2);
            assert_eq!(args["to"], serde_json::Value::String("alice@acme.com".into()));
            assert!(
                args["body"].as_str().unwrap().contains("alice@acme.com"),
                "body should contain restored email"
            );
        }
        other => panic!("expected Allow, got {other:?}"),
    }
}

#[test]
fn web_fetch_is_denied_by_default() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();

    let red = pipeline
        .redact(
            "alice@acme.com",
            &Source::ToolResult { tool_name: "odoo.x".into() },
        )
        .unwrap();
    let tok = red.tokens_written[0].as_str().to_string();

    let dec = manager
        .decide_tool_call(
            "web_fetch",
            &serde_json::json!({"url": format!("https://x.com?email={tok}")}),
            "agnes",
            Some("s1"),
            &duduclaw_redaction::Caller::owner("agnes"),
        )
        .unwrap();
    match dec {
        EgressDecision::Deny { tokens_seen, .. } => assert_eq!(tokens_seen, 1),
        other => panic!("expected Deny, got {other:?}"),
    }
}

#[test]
fn log_event_passthrough_keeps_tokens_in_arg() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();
    let red = pipeline
        .redact(
            "alice@acme.com",
            &Source::ToolResult { tool_name: "odoo.x".into() },
        )
        .unwrap();
    let tok = red.tokens_written[0].as_str().to_string();

    let dec = manager
        .decide_tool_call(
            "log_event",
            &serde_json::json!({"message": format!("contacted {tok}")}),
            "agnes",
            Some("s1"),
            &duduclaw_redaction::Caller::owner("agnes"),
        )
        .unwrap();
    match dec {
        EgressDecision::Passthrough(args) => {
            assert!(args["message"].as_str().unwrap().contains("<REDACT:"));
        }
        other => panic!("expected Passthrough, got {other:?}"),
    }
}

#[test]
fn vault_survives_process_restart() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().to_path_buf();
    let paths = ManagerPaths::under_home(&home);

    // 1. First run: redact + store.
    let saved_token;
    {
        let m = RedactionManager::open(config_for_test(), paths.clone()).unwrap();
        let p = m.pipeline("agnes", Some("persistent-session".into())).unwrap();
        let red = p
            .redact(
                "alice@acme.com",
                &Source::ToolResult { tool_name: "odoo.x".into() },
            )
            .unwrap();
        saved_token = red.tokens_written[0].as_str().to_string();
    }

    // 2. Second run: fresh manager, same paths.
    {
        let m = RedactionManager::open(config_for_test(), paths).unwrap();
        let p = m.pipeline("agnes", Some("persistent-session".into())).unwrap();
        let restored = p
            .restore(&saved_token, &Caller::owner("agnes"), RestoreTarget::UserChannel)
            .unwrap();
        assert_eq!(restored, "alice@acme.com");
    }
}

#[test]
fn hallucinated_token_does_not_decrypt_in_channel_reply() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();

    let fake = "<REDACT:EMAIL:deadbeef>";
    let out = pipeline
        .restore(fake, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    assert_eq!(out, fake, "hallucinated token must stay verbatim");
}

#[test]
fn user_input_passthrough_does_not_trigger_redact() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();

    let user_msg = "幫我寄信給昨天那位下單金額最高的客戶 (alice@acme.com)";
    let red = pipeline
        .redact(
            user_msg,
            &Source::UserChannelInput { channel_id: "line".into() },
        )
        .unwrap();
    assert_eq!(red.redacted_text, user_msg);
    assert!(red.tokens_written.is_empty());
}

#[test]
fn per_session_isolation_blocks_cross_session_lookup() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();

    let p_a = manager.pipeline("agnes", Some("session-A".into())).unwrap();
    let p_b = manager.pipeline("agnes", Some("session-B".into())).unwrap();

    let red = p_a
        .redact(
            "alice@acme.com",
            &Source::ToolResult { tool_name: "odoo.x".into() },
        )
        .unwrap();
    let token = red.tokens_written[0].as_str().to_string();

    // Session B should not be able to restore session A's token.
    let restored_b = p_b
        .restore(&token, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    assert!(
        restored_b.contains("<REDACT:"),
        "cross-session restore must NOT decrypt: got '{restored_b}'"
    );

    // Same token works in session A.
    let restored_a = p_a
        .restore(&token, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    assert_eq!(restored_a, "alice@acme.com");
}

#[test]
fn per_agent_isolation_blocks_cross_agent_lookup() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();

    let p_agnes = manager.pipeline("agnes", Some("s".into())).unwrap();
    let p_bobby = manager.pipeline("bobby", Some("s".into())).unwrap();

    let red = p_agnes
        .redact(
            "alice@acme.com",
            &Source::ToolResult { tool_name: "odoo.x".into() },
        )
        .unwrap();
    let token = red.tokens_written[0].as_str().to_string();

    let restored = p_bobby
        .restore(&token, &Caller::owner("bobby"), RestoreTarget::UserChannel)
        .unwrap();
    assert!(restored.contains("<REDACT:"));
}

#[test]
fn audit_log_target_never_decrypts() {
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();

    let red = pipeline
        .redact(
            "alice@acme.com",
            &Source::ToolResult { tool_name: "odoo.x".into() },
        )
        .unwrap();
    let out = pipeline
        .restore(&red.redacted_text, &Caller::owner("agnes"), RestoreTarget::AuditLog)
        .unwrap();
    assert!(!out.contains("alice@acme.com"));
    assert!(out.contains("<REDACT:"));
}

/// `taiwan_strict` + `general` plus a `db_field` rule over `res.partner`.
fn config_with_db_field() -> RedactionConfig {
    let mut cfg = config_for_test();
    cfg.rules.insert(
        "customer_master".to_string(),
        RuleSpec {
            id: "customer_master".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::DbField {
                source: Some("odoo".into()),
                connector: None,
                fields: vec!["res.partner.name".into(), "res.partner.street".into()],
            },
        },
    );
    cfg
}

#[test]
fn odoo_search_json_result_round_trips_through_field_rules() {
    // The whole point of the structured pass: `name` and `street` carry no
    // recognisable pattern, so only a field rule can mask them — while the
    // email in the same record is still caught by the ordinary regex rules.
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_with_db_field(), ManagerPaths::under_home(tmp.path()))
            .unwrap();
    let pipeline = manager.pipeline("agnes", Some("session-odoo".into())).unwrap();

    let records = serde_json::json!([
        {
            "id": 1001,
            "name": "王大福",
            "email": "dafu.wang@example.invalid",
            "street": "臺北市中正區虛構路 100 號",
        },
        {
            "id": 1002,
            "name": "陳美玲",
            "email": "meiling.chen@example.invalid",
            "street": "臺中市西屯區範例大道 7 號",
        },
    ]);

    // Exactly what `handle_odoo_*` returns: records pretty-printed into
    // `content[0].text`.
    let mut tool_result = serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&records).unwrap(),
        }]
    });
    let args = serde_json::json!({"model": "res.partner", "limit": "20"});

    let tokens = pipeline
        .redact_value(
            &mut tool_result,
            &ToolContext { tool_name: "odoo_search", args: Some(&args) },
        )
        .unwrap();

    // 2 names + 2 streets (field rules) + 2 emails (regex).
    assert_eq!(tokens.len(), 6, "tokens: {tokens:?}");

    let llm_view = tool_result["content"][0]["text"].as_str().unwrap();
    for secret in [
        "王大福",
        "陳美玲",
        "臺北市中正區虛構路 100 號",
        "dafu.wang@example.invalid",
        "meiling.chen@example.invalid",
    ] {
        assert!(!llm_view.contains(secret), "leaked {secret}: {llm_view}");
    }
    assert!(llm_view.contains("<REDACT:DB_FIELD:"), "{llm_view}");
    assert!(llm_view.contains("<REDACT:EMAIL:"), "{llm_view}");

    // Record ids survive so the agent can still act on the rows it was shown.
    assert!(llm_view.contains("1001"), "{llm_view}");
    assert!(llm_view.contains("1002"), "{llm_view}");

    // Channel reply: the owner gets the real values back.
    let restored = pipeline
        .restore(llm_view, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    for secret in [
        "王大福",
        "陳美玲",
        "臺北市中正區虛構路 100 號",
        "臺中市西屯區範例大道 7 號",
        "dafu.wang@example.invalid",
    ] {
        assert!(restored.contains(secret), "missing {secret}: {restored}");
    }

    // A sub-agent without owner scope gets nothing.
    let outsider = Caller::agent("helper", vec!["SomethingElse".into()]);
    let denied = pipeline
        .restore(
            llm_view,
            &outsider,
            RestoreTarget::SubAgent { agent_id: "helper".into() },
        )
        .unwrap();
    assert!(!denied.contains("王大福"), "{denied}");
}

#[test]
fn wildcard_field_rule_masks_records_without_eating_the_envelope() {
    // `res.partner.*` expands to paths ["$[*]", "$"]. Through the real MCP
    // envelope the outer `$` must be refused (it is the transport, not a
    // record) while the embedded payload is fully masked except `id`.
    let tmp = TempDir::new().unwrap();
    let mut cfg = config_for_test();
    cfg.rules.insert(
        "customer_all".to_string(),
        RuleSpec {
            id: "customer_all".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::DbField {
                source: Some("odoo".into()),
                connector: None,
                fields: vec!["res.partner.*".into()],
            },
        },
    );
    let manager = RedactionManager::open(cfg, ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("session-wild".into())).unwrap();

    let records = serde_json::json!([
        {"id": 1001, "name": "王大福", "email": "dafu.wang@example.invalid"},
        {"id": 1002, "name": "陳美玲", "email": "meiling.chen@example.invalid"},
    ]);
    let mut tool_result = serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&records).unwrap(),
        }]
    });
    let args = serde_json::json!({"model": "res.partner", "limit": "20"});

    pipeline
        .redact_value(
            &mut tool_result,
            &ToolContext { tool_name: "odoo_search", args: Some(&args) },
        )
        .unwrap();

    // Envelope intact: `type` verbatim, `text` still a JSON string.
    assert_eq!(tool_result["content"][0]["type"], serde_json::json!("text"));
    let text = tool_result["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.trim().starts_with("<REDACT:"),
        "the whole payload must not collapse into one token: {text}"
    );

    // Records masked, ids preserved so the agent can still act on the rows.
    let inner: serde_json::Value = serde_json::from_str(text).unwrap();
    let rows = inner.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], serde_json::json!(1001));
    assert_eq!(rows[1]["id"], serde_json::json!(1002));
    for row in rows {
        for key in ["name", "email"] {
            assert!(
                row[key].as_str().is_some_and(|v| v.starts_with("<REDACT:")),
                "{key} should be tokenised: {row}"
            );
        }
    }
    for secret in ["王大福", "陳美玲", "dafu.wang@example.invalid"] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }

    // Still reversible for the owner.
    let restored = pipeline
        .restore(text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    for secret in ["王大福", "陳美玲", "dafu.wang@example.invalid"] {
        assert!(restored.contains(secret), "missing {secret}: {restored}");
    }
}

#[test]
fn custom_data_source_reaches_a_non_odoo_tool_end_to_end() {
    // The 2026-09 registry's reason to exist: a `db_field` rule bound to the
    // operator's OWN database tool. Nothing here is Odoo-shaped — the tool is
    // `pg_select`, the table comes from `arguments.table`, and the rows sit
    // under `$.rows[*]` inside the usual MCP envelope.
    let tmp = TempDir::new().unwrap();
    let mut cfg = config_for_test();
    cfg.data_sources.insert(
        "crm_pg".to_string(),
        DataSourceDef {
            label: "客戶 CRM 資料庫".into(),
            tools: vec!["pg_query".into(), "pg_select".into()],
            table_arg: Some("table".into()),
            table: None,
            table_result: None,
            record_paths: vec!["$.rows[*]".into()],
            key_alias: BTreeMap::new(),
            free_form_names: false,
        },
    );
    cfg.rules.insert(
        "crm_customers".to_string(),
        RuleSpec {
            id: "crm_customers".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::DbField {
                source: Some("crm_pg".into()),
                connector: None,
                fields: vec!["customers.name".into(), "customers.address".into()],
            },
        },
    );
    let manager = RedactionManager::open(cfg, ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("session-pg".into())).unwrap();

    let payload = serde_json::json!({
        "rows": [
            {"id": 41, "name": "王大福", "address": "臺北市中正區虛構路 100 號"},
            {"id": 42, "name": "陳美玲", "address": "臺中市西屯區範例大道 7 號"},
        ],
        "row_count": 2,
        "truncated": false,
    });
    let mut tool_result = serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&payload).unwrap(),
        }]
    });
    let args = serde_json::json!({"table": "customers", "limit": 50});

    let tokens = pipeline
        .redact_value(
            &mut tool_result,
            &ToolContext { tool_name: "pg_select", args: Some(&args) },
        )
        .unwrap();
    assert_eq!(tokens.len(), 4, "2 names + 2 addresses: {tokens:?}");

    let text = tool_result["content"][0]["text"].as_str().unwrap();
    for secret in [
        "王大福",
        "陳美玲",
        "臺北市中正區虛構路 100 號",
        "臺中市西屯區範例大道 7 號",
    ] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
    let inner: serde_json::Value = serde_json::from_str(text).unwrap();
    let rows = inner["rows"].as_array().unwrap();
    // Record ids survive so the agent can still act on the rows it was shown.
    assert_eq!(rows[0]["id"], serde_json::json!(41));
    assert_eq!(rows[1]["id"], serde_json::json!(42));
    assert_eq!(inner["row_count"], serde_json::json!(2));
    for row in rows {
        for key in ["name", "address"] {
            assert!(
                row[key].as_str().is_some_and(|v| v.starts_with("<REDACT:DB_FIELD:")),
                "{key} should be tokenised: {row}"
            );
        }
    }

    // Reversible for the owner …
    let restored = pipeline
        .restore(text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    for secret in ["王大福", "陳美玲", "臺北市中正區虛構路 100 號"] {
        assert!(restored.contains(secret), "missing {secret}: {restored}");
    }

    // … and still bound to its own table: a different `table` arg must not fire.
    let mut other = serde_json::json!({"rows": [{"id": 7, "name": "王大福"}]});
    let other_args = serde_json::json!({"table": "orders"});
    let tokens = pipeline
        .redact_value(
            &mut other,
            &ToolContext { tool_name: "pg_select", args: Some(&other_args) },
        )
        .unwrap();
    assert!(tokens.is_empty(), "the rule is bound to `customers`: {tokens:?}");
    assert_eq!(other["rows"][0]["name"], serde_json::json!("王大福"));
}

/// The `csv_read` / `xlsx_read` result shape: the table is the file's
/// basename and it is named by the RESULT, not by the arguments (the tools are
/// called with a `path`).
fn file_reader_envelope(
    table: &str,
    columns: serde_json::Value,
    rows: serde_json::Value,
) -> serde_json::Value {
    let payload = serde_json::json!({
        "path": format!("/x/{table}"),
        "table": table,
        "columns": columns,
        "rows": rows,
        "row_count": 1,
        "truncated": false,
    });
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&payload).unwrap(),
        }]
    })
}

#[test]
fn local_data_files_are_masked_by_table_and_cjk_column() {
    // The 2026-09 file layer end to end: one `db_field` rule over the built-in
    // `duduclaw_files` source, naming two files with two different column
    // spellings — an ASCII one and a CJK one. Neither table nor column is an
    // identifier, and neither tool is told which table it is reading.
    let tmp = TempDir::new().unwrap();
    let mut cfg = config_for_test();
    cfg.rules.insert(
        "local_files".to_string(),
        RuleSpec {
            id: "local_files".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::DbField {
                source: Some("duduclaw_files".into()),
                connector: None,
                fields: vec!["customers.csv.name".into(), "客戶清單.xlsx.地址".into()],
            },
        },
    );
    let manager = RedactionManager::open(cfg, ManagerPaths::under_home(tmp.path())).unwrap();
    let pipeline = manager.pipeline("agnes", Some("session-files".into())).unwrap();

    // ── csv_read: ASCII table + ASCII column ──
    let mut csv_result = file_reader_envelope(
        "customers.csv",
        serde_json::json!(["id", "name", "email"]),
        serde_json::json!([{"id": 1, "name": "王大福", "email": "dafu.wang@example.invalid"}]),
    );
    let csv_args = serde_json::json!({"path": "/x/customers.csv"});
    let tokens = pipeline
        .redact_value(
            &mut csv_result,
            &ToolContext { tool_name: "csv_read", args: Some(&csv_args) },
        )
        .unwrap();
    assert_eq!(tokens.len(), 2, "1 name (field rule) + 1 email (general): {tokens:?}");

    let csv_text = csv_result["content"][0]["text"].as_str().unwrap();
    for secret in ["王大福", "dafu.wang@example.invalid"] {
        assert!(!csv_text.contains(secret), "leaked {secret}: {csv_text}");
    }
    let inner: serde_json::Value = serde_json::from_str(csv_text).unwrap();
    assert!(
        inner["rows"][0]["name"]
            .as_str()
            .unwrap()
            .starts_with("<REDACT:DB_FIELD:"),
        "{inner}"
    );
    // The email carries no field rule — the pattern rules of `general` catch it.
    assert!(
        inner["rows"][0]["email"]
            .as_str()
            .unwrap()
            .starts_with("<REDACT:EMAIL:"),
        "{inner}"
    );
    // The row id survives so the agent can still refer to the row it was shown,
    // and so does the table name the gate matched on.
    assert_eq!(inner["rows"][0]["id"], serde_json::json!(1));
    assert_eq!(inner["table"], serde_json::json!("customers.csv"));
    assert_eq!(csv_result["content"][0]["type"], serde_json::json!("text"));

    // Owner restore round-trips both values.
    let restored = pipeline
        .restore(csv_text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    assert!(restored.contains("王大福"), "{restored}");
    assert!(restored.contains("dafu.wang@example.invalid"), "{restored}");

    // ── xlsx_read: CJK table + CJK column ──
    let mut xlsx_result = file_reader_envelope(
        "客戶清單.xlsx",
        serde_json::json!(["id", "姓名", "地址"]),
        serde_json::json!([{"id": 7, "姓名": "陳美玲", "地址": "臺中市西屯區範例大道 7 號"}]),
    );
    let xlsx_args = serde_json::json!({"path": "/x/客戶清單.xlsx"});
    let tokens = pipeline
        .redact_value(
            &mut xlsx_result,
            &ToolContext { tool_name: "xlsx_read", args: Some(&xlsx_args) },
        )
        .unwrap();
    assert_eq!(tokens.len(), 1, "only 地址 has a rule: {tokens:?}");

    let xlsx_text = xlsx_result["content"][0]["text"].as_str().unwrap();
    assert!(!xlsx_text.contains("臺中市西屯區範例大道 7 號"), "{xlsx_text}");
    let inner: serde_json::Value = serde_json::from_str(xlsx_text).unwrap();
    assert!(
        inner["rows"][0]["地址"]
            .as_str()
            .unwrap()
            .starts_with("<REDACT:DB_FIELD:"),
        "{inner}"
    );
    // `姓名` was never named by a rule and matches no pattern — untouched.
    assert_eq!(inner["rows"][0]["姓名"], serde_json::json!("陳美玲"));
    assert_eq!(inner["rows"][0]["id"], serde_json::json!(7));

    let restored = pipeline
        .restore(xlsx_text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
        .unwrap();
    assert!(restored.contains("臺中市西屯區範例大道 7 號"), "{restored}");

    // ── a third file through the SAME tool must not inherit either rule ──
    let mut other = file_reader_envelope(
        "suppliers.csv",
        serde_json::json!(["id", "name"]),
        serde_json::json!([{"id": 2, "name": "王大福"}]),
    );
    let other_args = serde_json::json!({"path": "/x/suppliers.csv"});
    let tokens = pipeline
        .redact_value(
            &mut other,
            &ToolContext { tool_name: "csv_read", args: Some(&other_args) },
        )
        .unwrap();
    assert!(tokens.is_empty(), "bound to customers.csv only: {tokens:?}");
    let inner: serde_json::Value =
        serde_json::from_str(other["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(inner["rows"][0]["name"], serde_json::json!("王大福"));
}

#[test]
fn field_rules_stay_bound_to_their_model() {
    // Same tool, different model: the res.partner rule must not fire, and the
    // text rules must still do their job.
    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_with_db_field(), ManagerPaths::under_home(tmp.path()))
            .unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();

    let mut value = serde_json::json!([
        {"id": 5, "name": "內部專案代號", "email": "lead@example.invalid"}
    ]);
    let args = serde_json::json!({"model": "crm.lead"});
    let tokens = pipeline
        .redact_value(
            &mut value,
            &ToolContext { tool_name: "odoo_search", args: Some(&args) },
        )
        .unwrap();

    assert_eq!(value[0]["name"], serde_json::json!("內部專案代號"));
    assert_eq!(tokens.len(), 1, "only the email regex should fire");
    assert!(value[0]["email"].as_str().unwrap().starts_with("<REDACT:EMAIL:"));
}

#[test]
fn dashboard_handlers_expose_state() {
    use duduclaw_redaction::dashboard::{
        RecentAuditRequest, handle_override_status, handle_policy_status, handle_recent_audit,
        handle_stats,
    };

    let tmp = TempDir::new().unwrap();
    let manager =
        RedactionManager::open(config_for_test(), ManagerPaths::under_home(tmp.path())).unwrap();
    let p = manager.pipeline("agnes", Some("s1".into())).unwrap();

    let _ = p
        .redact(
            "ping alice@acme.com",
            &Source::ToolResult { tool_name: "odoo.x".into() },
        )
        .unwrap();

    let stats = handle_stats(&manager).unwrap();
    assert!(stats.vault.total >= 1);
    assert!(stats.config_enabled);

    let policy = handle_policy_status(&manager).unwrap();
    assert!(policy.config_enabled);
    assert!(!policy.override_active);
    assert!(policy.rule_count > 0);

    let recent = handle_recent_audit(&manager, RecentAuditRequest { limit: 100 }).unwrap();
    // We expect at least one redact line from the call above.
    assert!(!recent.entries.is_empty());

    let override_status = handle_override_status(&manager).unwrap();
    assert!(!override_status.active);
}

// ── Identity rules (spec §10.1) ──────────────────────────────────────────────

/// Write a person record in the same frontmatter shape the identity crate
/// parses, under the home directory `ManagerPaths::under_home` points at.
fn write_identity_person(home: &std::path::Path, file: &str, person_id: &str, display_name: &str) {
    let dir = home.join("shared").join("wiki").join("identity").join("people");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(file),
        format!("---\nperson_id: {person_id}\ndisplay_name: {display_name}\n---\n\nNotes.\n"),
    )
    .unwrap();
}

fn config_with_identity() -> RedactionConfig {
    let mut cfg = RedactionConfig::default();
    cfg.enabled = true;
    cfg.profiles = vec!["general".into()];
    cfg.rules.insert(
        "known_people".into(),
        RuleSpec {
            id: "known_people".into(),
            category: "PERSON".into(),
            restore_scope: RestoreScope::Owner,
            priority: 80,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Identity { source: "wiki".into() },
        },
    );
    cfg
}

#[test]
fn identity_rule_masks_known_people_and_round_trips_to_the_owner() {
    let tmp = TempDir::new().unwrap();
    write_identity_person(tmp.path(), "ruby.md", "person_2f9", "Ruby Lin");
    write_identity_person(tmp.path(), "ming.md", "person_ming", "王小明");

    let manager =
        RedactionManager::open(config_with_identity(), ManagerPaths::under_home(tmp.path()))
            .unwrap();
    let pipeline = manager.pipeline("agnes", Some("s1".into())).unwrap();

    let tool_result =
        "Ruby Lin 與 王小明 今天來訪，Ruby Linden 是另一個人，聯絡信箱 ruby@example.com";
    let redacted = pipeline
        .redact(
            tool_result,
            &Source::ToolResult { tool_name: "shared_wiki_read".into() },
        )
        .unwrap();

    // Both known names are tokenised...
    assert!(!redacted.redacted_text.contains("Ruby Lin 與"), "{}", redacted.redacted_text);
    assert!(!redacted.redacted_text.contains("王小明"), "{}", redacted.redacted_text);
    assert!(redacted.redacted_text.contains("<REDACT:PERSON:"), "{}", redacted.redacted_text);
    // ...the ASCII name stays whole-word: `Ruby Linden` is a different person
    // and must survive untouched.
    assert!(
        redacted.redacted_text.contains("Ruby Linden"),
        "whole-word semantics broken: {}",
        redacted.redacted_text
    );
    // The profile's own rules still run alongside.
    assert!(redacted.redacted_text.contains("<REDACT:EMAIL:"), "{}", redacted.redacted_text);

    // Owner restore returns every original.
    let restored = pipeline
        .restore(
            &redacted.redacted_text,
            &Caller::owner("agnes"),
            RestoreTarget::UserChannel,
        )
        .unwrap();
    assert!(restored.contains("Ruby Lin 與"), "{restored}");
    assert!(restored.contains("王小明"), "{restored}");
    assert!(restored.contains("Ruby Linden"), "{restored}");
    assert!(restored.contains("ruby@example.com"), "{restored}");
}

#[test]
fn identity_rule_without_a_people_directory_fails_the_manager_open() {
    // No `shared/wiki/identity/people` on disk ⇒ misconfiguration, and the
    // whole manager must refuse to open rather than run with a dead rule.
    let tmp = TempDir::new().unwrap();
    let err = RedactionManager::open(config_with_identity(), ManagerPaths::under_home(tmp.path()))
        .err()
        .expect("missing identity directory must fail the load");
    assert!(
        err.to_string().contains("identity people directory not found"),
        "{err}"
    );
}
