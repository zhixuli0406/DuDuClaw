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
    Caller, EgressDecision, ManagerPaths, RedactionConfig, RedactionManager, RestoreArgsMode,
    RestoreTarget, Source, ToolEgressRule,
};
use std::collections::HashMap;
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
        .decide_tool_call("send_email", &llm_tool_call, "agnes", Some("s1"))
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
