//! WP-S (v1.68.0) handler-level regressions: webhook channels hot-start
//! without a restart, `odoo.configure` refuses the unimplemented XML-RPC
//! protocol, and `killswitch.get` reports which triggers are enforced.
use super::*;

fn ok_payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok, got {other:?}"),
    }
}

fn is_error(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: false, .. })
}

fn test_reply_ctx(home: &std::path::Path) -> Arc<crate::channel_reply::ReplyContext> {
    let registry = Arc::new(tokio::sync::RwLock::new(duduclaw_agent::AgentRegistry::new(
        home.join("agents"),
    )));
    let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let status: crate::channel_reply::ChannelStatusMap =
        Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    Arc::new(crate::channel_reply::ReplyContext::new(
        registry,
        home.to_path_buf(),
        sessions,
        status,
        tx,
    ))
}

/// First-time setup of a webhook channel from the dashboard mounts its
/// endpoint immediately; removing the channel unmounts it.
#[tokio::test]
async fn channels_add_webhook_hot_starts_and_remove_unmounts() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_reply_ctx(test_reply_ctx(home.path())).await;

    let slots = crate::webhook_slots::global();
    slots.set("dingtalk", None);

    let body = ok_payload(
        handler
            .handle_channels_add(json!({
                "type": "dingtalk",
                "config": { "token": "dingtalk-app-secret-123", "secret": "appkey-1" }
            }))
            .await,
    );
    assert_eq!(body["hot_started"], json!(true), "{body}");
    assert_eq!(body["restart_required"], json!(false), "{body}");
    assert!(body["not_started_reason"].is_null(), "{body}");
    assert!(slots.is_mounted("dingtalk"));

    ok_payload(handler.handle_channels_remove(json!({ "type": "dingtalk" })).await);
    assert!(!slots.is_mounted("dingtalk"));
    // v1.68: the keys are removed, not left behind as empty strings.
    let cfg = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!cfg.contains("dingtalk_"), "{cfg}");
}

/// An incomplete webhook config (Feishu without a verification token) is
/// reported as not started, with a reason, and never mounted.
#[tokio::test]
async fn channels_add_incomplete_webhook_reports_reason() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler.set_reply_ctx(test_reply_ctx(home.path())).await;
    let slots = crate::webhook_slots::global();
    slots.set("feishu", None);

    let body = ok_payload(
        handler
            .handle_channels_add(json!({
                "type": "feishu",
                "config": { "token": "cli_app_id", "secret": "feishu-secret" }
            }))
            .await,
    );
    assert_eq!(body["hot_started"], json!(false), "{body}");
    assert_eq!(body["restart_required"], json!(false), "{body}");
    assert_eq!(body["not_started_reason"], json!("webhook_config_incomplete"));
    assert!(!slots.is_mounted("feishu"));
}

#[tokio::test]
async fn odoo_configure_refuses_xmlrpc() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_odoo_configure(json!({
            "url": "https://example.odoo.com",
            "db": "example",
            "protocol": "xmlrpc",
        }))
        .await;
    assert!(is_error(&frame), "{frame:?}");
    let cfg = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!cfg.contains("xmlrpc"));
}

#[test]
fn killswitch_get_reports_enforced_triggers() {
    let table: toml::Table = "[triggers]\nmax_consecutive_errors = 4\n".parse().unwrap();
    let resp = killswitch_table_to_response(&table);
    assert_eq!(resp["triggers_enforced"]["max_consecutive_errors"], json!(true));
    assert_eq!(resp["triggers_enforced"]["max_replies_per_minute"], json!(false));
    assert_eq!(resp["triggers_enforced"]["cost_limit_usd"], json!(false));
    // Display values still fall back to the documented defaults.
    assert_eq!(resp["triggers"]["max_replies_per_minute"], json!(10));
}

/// `triggers.<key>: null` removes the key (trigger disarmed); absent keys
/// are left unchanged.
#[test]
fn killswitch_update_null_disarms_a_trigger() {
    let mut table: toml::Table =
        "[triggers]\nmax_replies_per_minute = 5\ncost_limit_usd = 20.0\n".parse().unwrap();
    let changes = apply_killswitch_to_table(
        &mut table,
        &json!({ "triggers": { "max_replies_per_minute": null } }),
    )
    .unwrap();
    assert_eq!(changes.len(), 1, "{changes:?}");
    let t = table["triggers"].as_table().unwrap();
    assert!(t.get("max_replies_per_minute").is_none());
    assert_eq!(t.get("cost_limit_usd").and_then(|v| v.as_float()), Some(20.0));
    let resp = killswitch_table_to_response(&table);
    assert_eq!(resp["triggers_enforced"]["max_replies_per_minute"], json!(false));
    assert_eq!(resp["triggers_enforced"]["cost_limit_usd"], json!(true));
    // Null on a key that is not set is a no-op, not a change.
    let changes = apply_killswitch_to_table(
        &mut table,
        &json!({ "triggers": { "max_consecutive_errors": null } }),
    )
    .unwrap();
    assert!(changes.is_empty());
}
