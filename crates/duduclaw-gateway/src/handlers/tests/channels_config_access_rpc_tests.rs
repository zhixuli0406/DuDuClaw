//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// W2-2 (E1/E2) — `channels.config_*` / `channels.access_*` /
/// `channels.pairing_*` RPCs. These share `ChannelSettingsManager` /
/// `AccessController` with the `channel_config`/`pairing_manage` MCP tools
/// (`duduclaw-cli::mcp`), so a round-trip here also exercises the exact
/// storage format the MCP side reads.
use super::*;

fn admin_ctx() -> UserContext {
    UserContext::admin_fallback()
}

fn manager_ctx() -> UserContext {
    UserContext {
        user_id: "m1".to_string(),
        email: "m1@test.local".to_string(),
        role: UserRole::Manager,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    }
}

fn frame_ok(f: &WsFrame) -> bool {
    matches!(f, WsFrame::Response { ok: true, .. })
}

fn frame_data(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response { payload, .. } => payload.clone().unwrap_or(Value::Null),
        other => panic!("expected response, got {other:?}"),
    }
}

// ── config_get / config_set round trip ─────────────────────────────

#[tokio::test]
async fn config_get_defaults_on_a_never_configured_channel() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "channels.config_get",
            json!({ "channel": "discord" }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    let data = frame_data(&frame);
    assert_eq!(data["channel"], "discord");
    assert_eq!(data["scope_id"], "global");
    assert_eq!(data["settings"]["mention_only"], false);
    assert_eq!(data["settings"]["auto_thread"], false);
    assert_eq!(data["settings"]["response_mode"], "auto");
    assert_eq!(data["settings"]["agent_override"], "");
    assert_eq!(data["settings"]["allowed_channels"], json!([]));
    assert!(data["settings"]["thread_archive_minutes"].is_null());
}

#[tokio::test]
async fn config_set_then_get_round_trips_every_field() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let set_frame = handler
        .handle(
            "channels.config_set",
            json!({
                "channel": "discord",
                "settings": {
                    "mention_only": true,
                    "auto_thread": true,
                    "allowed_channels": ["c1", "c2"],
                    "allowed_guilds": ["g1"],
                    "agent_override": "sam",
                    "response_mode": "embed",
                    "thread_archive_minutes": 1440,
                },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&set_frame), "{set_frame:?}");
    assert_eq!(
        frame_data(&set_frame)["changes"].as_array().unwrap().len(),
        7
    );

    let get_frame = handler
        .handle(
            "channels.config_get",
            json!({ "channel": "discord" }),
            &admin_ctx(),
        )
        .await;
    let data = frame_data(&get_frame);
    assert_eq!(data["settings"]["mention_only"], true);
    assert_eq!(data["settings"]["auto_thread"], true);
    assert_eq!(data["settings"]["allowed_channels"], json!(["c1", "c2"]));
    assert_eq!(data["settings"]["allowed_guilds"], json!(["g1"]));
    assert_eq!(data["settings"]["agent_override"], "sam");
    assert_eq!(data["settings"]["response_mode"], "embed");
    // thread_archive_minutes round-trips as its string encoding.
    assert_eq!(data["settings"]["thread_archive_minutes"], "1440");
}

#[tokio::test]
async fn config_set_null_clears_a_previously_set_field() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    handler
        .handle(
            "channels.config_set",
            json!({ "channel": "telegram", "settings": { "agent_override": "sam" } }),
            &admin_ctx(),
        )
        .await;
    let cleared = handler
        .handle(
            "channels.config_set",
            json!({ "channel": "telegram", "settings": { "agent_override": null } }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&cleared), "{cleared:?}");
    let get_frame = handler
        .handle(
            "channels.config_get",
            json!({ "channel": "telegram" }),
            &admin_ctx(),
        )
        .await;
    assert_eq!(frame_data(&get_frame)["settings"]["agent_override"], "");
}

// ── access_get / access_set round trip ──────────────────────────────

#[tokio::test]
async fn access_set_then_get_round_trips_admin_users() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let set_frame = handler
        .handle(
            "channels.access_set",
            json!({
                "channel": "line",
                "settings": {
                    "require_pairing": true,
                    "allowed_users": ["u1"],
                    "blocked_users": ["u2"],
                    "admin_users": ["u1"],
                },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&set_frame), "{set_frame:?}");

    let get_frame = handler
        .handle(
            "channels.access_get",
            json!({ "channel": "line" }),
            &admin_ctx(),
        )
        .await;
    let data = frame_data(&get_frame);
    assert_eq!(data["settings"]["require_pairing"], true);
    assert_eq!(data["settings"]["allowed_users"], json!(["u1"]));
    assert_eq!(data["settings"]["blocked_users"], json!(["u2"]));
    assert_eq!(data["settings"]["admin_users"], json!(["u1"]));

    // Audit log recorded the write with Warning severity (admin_users touched).
    let audit_raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit_raw.contains("channel_access_set"));
    assert!(audit_raw.contains("\"warning\""));
}

/// `access_set` always writes at the "global" scope even if a caller
/// passes a different `scope_id` — access-control keys are only ever
/// read at global scope in production (`channel_reply::check_user_access_gate`).
#[tokio::test]
async fn access_set_ignores_a_non_global_scope_id() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "channels.access_set",
            json!({
                "channel": "discord",
                "scope_id": "guild123",
                "settings": { "require_pairing": true },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(frame_data(&frame)["scope_id"], "global");
}

// ── permission denial ────────────────────────────────────────────────

#[tokio::test]
async fn non_admin_is_denied_on_every_method() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = manager_ctx();
    for (method, params) in [
        ("channels.config_get", json!({ "channel": "discord" })),
        (
            "channels.config_set",
            json!({ "channel": "discord", "settings": { "mention_only": true } }),
        ),
        ("channels.access_get", json!({ "channel": "discord" })),
        (
            "channels.access_set",
            json!({ "channel": "discord", "settings": { "require_pairing": true } }),
        ),
        ("channels.pairing_list", json!({})),
        ("channels.pairing_revoke", json!({ "subject": "u1" })),
    ] {
        let frame = handler.handle(method, params, &ctx).await;
        assert!(!frame_ok(&frame), "{method} must deny a manager: {frame:?}");
    }

    // The denied `channels.config_set`/`access_set` calls above must not
    // have written anything — an admin reading the same channel back
    // still sees the untouched defaults.
    let get_frame = handler
        .handle(
            "channels.config_get",
            json!({ "channel": "discord" }),
            &admin_ctx(),
        )
        .await;
    assert_eq!(frame_data(&get_frame)["settings"]["mention_only"], false);
    let access_frame = handler
        .handle(
            "channels.access_get",
            json!({ "channel": "discord" }),
            &admin_ctx(),
        )
        .await;
    assert_eq!(
        frame_data(&access_frame)["settings"]["require_pairing"],
        false
    );
}

// ── unknown-field / invalid-value rejection (fail-closed) ───────────

#[tokio::test]
async fn config_set_rejects_unknown_key() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "channels.config_set",
            // admin_users is a valid key elsewhere, but not through
            // config_set (it's access-control, not behavior).
            json!({ "channel": "discord", "settings": { "admin_users": ["u1"] } }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
    let get_frame = handler
        .handle(
            "channels.config_get",
            json!({ "channel": "discord" }),
            &admin_ctx(),
        )
        .await;
    assert_eq!(
        frame_data(&get_frame)["settings"]["admin_users"],
        Value::Null,
        "a rejected call must not have written anything"
    );
}

#[tokio::test]
async fn access_set_rejects_unknown_key() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "channels.access_set",
            // mention_only is a valid key elsewhere, but not through
            // access_set (it's behavior, not access-control).
            json!({ "channel": "discord", "settings": { "mention_only": true } }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
}

#[tokio::test]
async fn config_set_rejects_invalid_channel_type() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "channels.config_set",
            json!({ "channel": "myspace", "settings": { "mention_only": true } }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
}

#[tokio::test]
async fn config_set_rejects_wrong_value_type_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "channels.config_set",
            // mention_only must be a bool, not a string — and this must
            // fail before the (valid) auto_thread field is written, so a
            // multi-field call cannot partially apply.
            json!({
                "channel": "discord",
                "settings": { "mention_only": "true", "auto_thread": true },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
    let get_frame = handler
        .handle(
            "channels.config_get",
            json!({ "channel": "discord" }),
            &admin_ctx(),
        )
        .await;
    assert_eq!(
        frame_data(&get_frame)["settings"]["auto_thread"],
        false,
        "validate-before-write: the whole call must reject, not partially apply"
    );
}

#[tokio::test]
async fn config_set_rejects_invalid_scope_id() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "channels.config_set",
            json!({
                "channel": "discord",
                "scope_id": "guild;drop table",
                "settings": { "mention_only": true },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
}

// ── pairing_list / pairing_revoke ────────────────────────────────────

#[tokio::test]
async fn pairing_list_and_revoke_share_the_access_control_store() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Simulate what `/pair <code>` (or the MCP `pairing_manage
    // action=approve` tool) already persisted — the same
    // ~/.duduclaw/access_control.json the RPC reads.
    let ctrl = crate::access_control::AccessController::with_persistence(
        home.path().join("access_control.json"),
    );
    ctrl.approve_user("u-alice").await;
    ctrl.approve_user("u-bob").await;

    let list_frame = handler
        .handle("channels.pairing_list", json!({}), &admin_ctx())
        .await;
    assert!(frame_ok(&list_frame), "{list_frame:?}");
    let approved = frame_data(&list_frame)["approved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(approved.contains(&"u-alice".to_string()));
    assert!(approved.contains(&"u-bob".to_string()));

    let revoke_frame = handler
        .handle(
            "channels.pairing_revoke",
            json!({ "subject": "u-alice" }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&revoke_frame), "{revoke_frame:?}");

    let list_frame2 = handler
        .handle("channels.pairing_list", json!({}), &admin_ctx())
        .await;
    let approved2 = frame_data(&list_frame2)["approved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(
        !approved2.contains(&"u-alice".to_string()),
        "revoked subject must be gone"
    );
    assert!(
        approved2.contains(&"u-bob".to_string()),
        "other subjects must be unaffected"
    );

    let audit_raw = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit_raw.contains("channel_pairing_revoke"));
}

#[tokio::test]
async fn pairing_revoke_requires_subject() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("channels.pairing_revoke", json!({}), &admin_ctx())
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
}
