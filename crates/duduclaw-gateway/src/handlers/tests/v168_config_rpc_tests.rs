//! v1.68.0 dashboard-switch RPCs, end to end through `MethodHandler::handle`
//! (so the `require_admin!` / `check_agent!` gates are exercised too):
//! `system.update_config` new keys + `restart_required`, `tick.sources.*`,
//! `config.raw.*`, `agents.update` authority guard / partial writes / typed
//! KV, `agents.inspect` prefill, `inference.update` engine reset.
use super::*;

fn ok(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p.clone(),
        other => panic!("expected ok, got {other:?}"),
    }
}

fn err(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error: Some(e), .. } => e.as_str().unwrap_or_default().to_string(),
        other => panic!("expected error, got {other:?}"),
    }
}

fn admin() -> UserContext {
    let mut c = UserContext::admin_fallback();
    c.user_id = "admin-1".into();
    c
}

/// A non-admin user who owns agent `a1`.
fn owner_of_a1() -> UserContext {
    UserContext {
        user_id: "owner-7".into(),
        email: "owner@example.com".into(),
        role: duduclaw_auth::models::UserRole::Employee,
        agent_access: [("a1".to_string(), AccessLevel::Owner)].into_iter().collect(),
        must_change_password: false,
    }
}

fn audit_rows(home: &Path, event: &str) -> Vec<Value> {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event_type"] == event)
        .collect()
}

fn config(home: &Path) -> toml::Table {
    std::fs::read_to_string(home.join("config.toml")).unwrap().parse().unwrap()
}

async fn handler_with_agent() -> (tempfile::TempDir, MethodHandler) {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    ok(&handler
        .handle_agents_create_as(json!({ "name": "a1", "display_name": "A1" }), Some(&admin()))
        .await);
    (home, handler)
}

// ── system.update_config ─────────────────────────────────────────────────────

#[tokio::test]
async fn update_config_writes_new_keys_partially_and_reports_restart() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[general]\ndefault_agent = \"dudu\"\n# keep\n").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = ok(&handler
        .handle(
            "system.update_config",
            json!({
                "takeover": { "enabled": true, "duration_minutes": 45 },
                "telemetry.otlp_endpoint": "http://127.0.0.1:4317",
                "webchat": { "public_widget": true, "widget_key": "abcdefghijklmnop" },
                "dispatch": { "policy": "role_team" },
            }),
            &admin(),
        )
        .await);
    assert_eq!(p["restart_required"], json!(["telemetry.otlp_endpoint"]), "{p}");
    assert!(!p.to_string().contains("abcdefghijklmnop"), "widget key echoed: {p}");
    let t = config(home.path());
    assert_eq!(t["takeover"]["duration_minutes"].as_integer(), Some(45));
    assert_eq!(t["dispatch"]["policy"].as_str(), Some("role_team"));
    assert_eq!(t["general"]["default_agent"].as_str(), Some("dudu"), "untouched key kept");
}

#[tokio::test]
async fn update_config_invalid_value_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[takeover]\nenabled = false\n").unwrap();
    let before = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let e = err(&handler
        .handle(
            "system.update_config",
            json!({ "takeover": { "enabled": true }, "files": { "allowed_roots": ["relative"] } }),
            &admin(),
        )
        .await);
    assert!(e.contains("absolute"), "{e}");
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), before);
}

#[tokio::test]
async fn update_config_refuses_unparsable_file_instead_of_clobbering_it() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[general\nbroken").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let e = err(&handler
        .handle("system.update_config", json!({ "night": { "llm_enabled": true } }), &admin())
        .await);
    assert!(e.contains("not valid TOML"), "{e}");
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), "[general\nbroken");
}

#[tokio::test]
async fn acp_trusted_writes_a_protected_audit_row() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    ok(&handler
        .handle("system.update_config", json!({ "acp": { "trusted": true } }), &admin())
        .await);
    let rows = audit_rows(home.path(), "config_protected_key_changed");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["details"]["key"], "acp.trusted");
    assert_eq!(rows[0]["details"]["after"], true);
}

#[tokio::test]
async fn admin_rpcs_refuse_non_admin() {
    let (_home, handler) = handler_with_agent().await;
    let user = owner_of_a1();
    for (method, params) in [
        ("system.update_config", json!({ "acp": { "trusted": true } })),
        ("tick.sources.list", json!({})),
        ("tick.sources.upsert", json!({ "id": "x", "kind": "http_poll", "url": "https://example.com" })),
        ("tick.sources.remove", json!({ "id": "x" })),
        ("config.raw.get", json!({ "file": "config" })),
        ("config.raw.set", json!({ "file": "config", "content": "" })),
        ("inference.update", json!({ "enabled": false })),
    ] {
        let frame = handler.handle(method, params, &user).await;
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{method} must be refused: {frame:?}");
    }
}

// ── tick.sources.* ───────────────────────────────────────────────────────────

#[tokio::test]
async fn tick_sources_crud_hides_header_values() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = ok(&handler
        .handle(
            "tick.sources.upsert",
            json!({ "source": {
                "id": "quotes", "kind": "websocket", "url": "wss://example.com/feed",
                "headers": { "X-API-Key": "super-secret" }, "json_fields": { "price": "/p" }
            }}),
            &admin(),
        )
        .await);
    // `[tick] enabled` is off ⇒ stored, not running, nothing to restart.
    assert_eq!(p["hot_reloaded"], false, "{p}");
    assert_eq!(p["restart_required"], false, "{p}");
    assert!(p["note"].as_str().unwrap().contains("not running"), "{p}");
    let list = ok(&handler.handle("tick.sources.list", json!({}), &admin()).await);
    assert!(!list.to_string().contains("super-secret"), "{list}");
    assert_eq!(list["sources"][0]["headers_count"], 1);
    assert_eq!(list["sources"][0]["valid"], true);

    // Partial upsert keeps the stored header.
    ok(&handler
        .handle("tick.sources.upsert", json!({ "id": "quotes", "interval_secs": 20 }), &admin())
        .await);
    let t = config(home.path());
    let src = &t["tick"]["sources"][0];
    assert_eq!(src["headers"]["X-API-Key"].as_str(), Some("super-secret"));
    assert_eq!(src["interval_secs"].as_integer(), Some(20));

    // Invalid source (SSRF target) is refused.
    let e = err(&handler
        .handle("tick.sources.upsert", json!({ "id": "bad", "kind": "http_poll", "url": "http://169.254.169.254/" }), &admin())
        .await);
    assert!(e.contains("url"), "{e}");

    ok(&handler.handle("tick.sources.remove", json!({ "id": "quotes" }), &admin()).await);
    assert!(config(home.path())["tick"].get("sources").is_none());
}

// ── config.raw.* ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn raw_editor_masks_restores_backs_up_and_audits() {
    let home = tempfile::tempdir().unwrap();
    let original = "[gateway]\nbind = \"127.0.0.1\"\nauth_token_enc = \"ENCRYPTED\"\n\n[acp]\ntrusted = false\n";
    std::fs::write(home.path().join("config.toml"), original).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let got = ok(&handler.handle("config.raw.get", json!({ "file": "config" }), &admin()).await);
    let content = got["content"].as_str().unwrap();
    assert!(!content.contains("ENCRYPTED") && content.contains("«set»"), "{content}");

    let edited = content.replace("trusted = false", "trusted = true");
    let set = ok(&handler
        .handle(
            "config.raw.set",
            json!({ "file": "config", "content": edited, "base_hash": got["hash"] }),
            &admin(),
        )
        .await);
    assert_eq!(set["changed_sections"], json!(["acp"]), "{set}");
    assert_eq!(set["protected_changed"], true);
    let t = config(home.path());
    assert_eq!(t["gateway"]["auth_token_enc"].as_str(), Some("ENCRYPTED"), "secret restored");
    assert_eq!(t["acp"]["trusted"].as_bool(), Some(true));

    let backups: Vec<_> = std::fs::read_dir(home.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("config.toml.bak-"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read_to_string(backups[0].path()).unwrap(), original);

    let rows = audit_rows(home.path(), "config_raw_edited");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["details"]["protected_changed"], true);
    assert_eq!(rows[0]["details"]["changed_sections"], json!(["acp"]));

    // A stale base_hash is refused.
    let e = err(&handler
        .handle("config.raw.set", json!({ "file": "config", "content": edited, "base_hash": got["hash"] }), &admin())
        .await);
    assert!(e.contains("changed since"), "{e}");
}

#[tokio::test]
async fn raw_editor_rejects_invalid_toml_and_escaping_agent_ids() {
    let (home, handler) = handler_with_agent().await;
    // `base_hash` is required.
    let e = err(&handler
        .handle("config.raw.set", json!({ "file": "config", "content": "a = 1\n" }), &admin())
        .await);
    assert!(e.contains("base_hash"), "{e}");
    let h = ok(&handler.handle("config.raw.get", json!({ "file": "config" }), &admin()).await)["hash"].clone();
    let e = err(&handler
        .handle("config.raw.set", json!({ "file": "config", "content": "a = 1\nb = \n", "base_hash": h }), &admin())
        .await);
    assert!(e.contains("line 2"), "{e}");
    for bad in ["agent:../config", "agent:..", "agent:missing", "agent:_trash", "agent:"] {
        let e = err(&handler.handle("config.raw.get", json!({ "file": bad }), &admin()).await);
        assert!(!e.is_empty(), "{bad}");
    }
    // A real agent can be read and must keep loading after a write.
    let got = ok(&handler.handle("config.raw.get", json!({ "file": "agent:a1" }), &admin()).await);
    let broken = format!("{}\n[heartbeat2]\n", got["content"].as_str().unwrap()).replace("[heartbeat]", "[heartbeat_x]");
    let e = err(&handler
        .handle("config.raw.set", json!({ "file": "agent:a1", "content": broken, "base_hash": got["hash"] }), &admin())
        .await);
    assert!(e.contains("agent.toml"), "{e}");
    assert!(home.path().join("agents/a1/agent.toml").exists());
}

// ── agents.update / agents.inspect ───────────────────────────────────────────

#[tokio::test]
async fn non_admin_owner_cannot_change_authority_keys_but_can_edit_the_rest() {
    let (home, handler) = handler_with_agent().await;
    let owner = owner_of_a1();
    let e = err(&handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "capabilities": { "git_credentials": true } }),
            &owner,
        )
        .await);
    assert!(e.contains("administrator"), "{e}");
    let refused = audit_rows(home.path(), "agent_authority_refused");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["details"]["user_id"], "owner-7");
    let e = err(&handler
        .handle("agents.update", json!({ "agent_id": "a1", "can_schedule_tasks": false }), &owner)
        .await);
    assert!(e.contains("permissions.can_schedule_tasks"), "{e}");
    let e = err(&handler
        .handle("agents.update", json!({ "agent_id": "a1", "department": "ops" }), &owner)
        .await);
    assert!(e.contains("agent.department"), "{e}");

    // Re-sending the unchanged prefilled values is not a change.
    let inspect = ok(&handler.handle("agents.inspect", json!({ "agent_id": "a1" }), &owner).await);
    ok(&handler
        .handle(
            "agents.update",
            json!({
                "agent_id": "a1",
                "display_name": "A1 renamed",
                "reports_to": inspect["reports_to"],
                "department": inspect["department"].as_str().unwrap_or(""),
                "sandbox_enabled": false,
            }),
            &owner,
        )
        .await);
    assert!(audit_rows(home.path(), "agent_authority_changed").is_empty());

    // The admin can, and the change is audited with the diff.
    ok(&handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "capabilities": { "git_credentials": true, "approval_required_tools": ["send_message"] } }),
            &admin(),
        )
        .await);
    let rows = audit_rows(home.path(), "agent_authority_changed");
    assert_eq!(rows.len(), 1, "{rows:?}");
    let keys: Vec<&str> = rows[0]["details"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"capabilities.git_credentials"), "{keys:?}");
    assert_eq!(rows[0]["details"]["user_id"], "admin-1");
}

#[tokio::test]
async fn unchanged_org_fields_do_not_promote_the_mirror() {
    let (home, handler) = handler_with_agent().await;
    // Hand-edit the mirror: the authority must not follow a dashboard save
    // that merely re-sends what the form showed.
    let path = home.path().join("agents/a1/agent.toml");
    let text = std::fs::read_to_string(&path).unwrap().replace("department = \"\"", "department = \"hand\"");
    std::fs::write(&path, text).unwrap();
    let before = std::fs::read_to_string(home.path().join("org.toml")).unwrap_or_default();
    ok(&handler
        .handle("agents.update", json!({ "agent_id": "a1", "department": "hand", "icon": "x" }), &admin())
        .await);
    assert_eq!(std::fs::read_to_string(home.path().join("org.toml")).unwrap_or_default(), before);
    // A real change does move the authority.
    ok(&handler.handle("agents.update", json!({ "agent_id": "a1", "department": "sales" }), &admin()).await);
    assert!(std::fs::read_to_string(home.path().join("org.toml")).unwrap().contains("sales"));
}

#[tokio::test]
async fn partial_writes_keep_cron_and_new_keys_round_trip_through_inspect() {
    let (home, handler) = handler_with_agent().await;
    ok(&handler
        .handle("agents.update", json!({ "agent_id": "a1", "heartbeat_cron": "*/5 * * * *" }), &admin())
        .await);
    ok(&handler
        .handle(
            "agents.update",
            json!({
                "agent_id": "a1",
                "budget": { "daily_cap_cents": 300 },
                "model": { "effort": "high" },
                "fork": { "enabled": true },
                "team": { "enabled": true, "roles": { "verifier": { "runtime": "codex" } } },
                "guardrails": { "enabled": true },
                "memory": { "decision_ttl_days": 9 },
                "night_engine": { "enabled": true },
                "runtime": { "fallback": "qwen", "minimal_context": false },
                "utility": "",
                "advanced_kv": [{ "section": "prompt", "key": "cli_bare_mode", "value": true, "type": "boolean" }],
            }),
            &admin(),
        )
        .await);
    let p = ok(&handler.handle("agents.inspect", json!({ "agent_id": "a1" }), &admin()).await);
    assert_eq!(p["heartbeat"]["cron"], "*/5 * * * *", "{p}");
    assert_eq!(p["budget"]["daily_cap_cents"], 300);
    assert_eq!(p["model"]["effort"], "high");
    assert_eq!(p["fork"]["enabled"], true);
    assert_eq!(p["team"]["roles"]["verifier"]["runtime"], "codex");
    assert_eq!(p["memory"]["decision_ttl_days"], 9);
    assert_eq!(p["night_engine"]["enabled"], true);
    assert_eq!(p["runtime"]["fallback"], "qwen");
    assert_eq!(p["runtime"]["minimal_context"], false);
    assert_eq!(p["prompt"]["cli_bare_mode"], true);
    assert_eq!(p["capabilities"]["approval_required_tools"], json!([]));
    let raw = std::fs::read_to_string(home.path().join("agents/a1/agent.toml")).unwrap();
    assert!(!raw.contains("utility = \"\""), "{raw}");
    assert!(!raw.contains("[sticker]"), "{raw}");
}

#[tokio::test]
async fn kv_type_mismatch_is_refused_and_the_employee_keeps_loading() {
    let (home, handler) = handler_with_agent().await;
    let e = err(&handler
        .handle(
            "agents.update",
            json!({ "agent_id": "a1", "advanced_kv": [{ "section": "prompt", "key": "cli_bare_mode", "value": "true", "type": "string" }] }),
            &admin(),
        )
        .await);
    assert!(e.contains("line") && e.contains("agent.toml"), "{e}");
    let raw = std::fs::read_to_string(home.path().join("agents/a1/agent.toml")).unwrap();
    assert!(!raw.contains("cli_bare_mode"));
}

// ── inference.update ─────────────────────────────────────────────────────────

#[tokio::test]
async fn inference_update_resets_the_engine() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let p = ok(&handler
        .handle("inference.update", json!({ "router": { "local_tools": false } }), &admin())
        .await);
    assert_eq!(p["engine_reset"], true);
    assert_eq!(p["restart_required"], json!([]));
}

#[tokio::test]
async fn secret_manager_backend_keys_are_written_and_unknown_keys_refused() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    ok(&handler
        .handle(
            "system.update_config",
            json!({ "secret_manager": {
                "backend": "infisical",
                "infisical_addr": "https://app.infisical.com",
                "infisical_project_id": "p1",
                "infisical_environment": "prod",
                "onepassword_host": "https://op.local:8080",
                "onepassword_vault": "v",
            }}),
            &admin(),
        )
        .await);
    let t = config(home.path());
    assert_eq!(t["secret_manager"]["infisical_project_id"].as_str(), Some("p1"));
    assert_eq!(t["secret_manager"]["onepassword_vault"].as_str(), Some("v"));
    let e = err(&handler
        .handle("system.update_config", json!({ "secret_manager": { "infisical_tokn": "x" } }), &admin())
        .await);
    assert!(e.contains("infisical_tokn"), "{e}");
}


// ── security batch ───────────────────────────────────────────────────────────

#[tokio::test]
async fn raw_editor_errors_never_echo_a_restored_secret() {
    let (home, handler) = handler_with_agent().await;
    let path = home.path().join("agents/a1/agent.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[channels.discord]\nbot_token = \"REAL-DISCORD-TOKEN-0123456789\"\n");
    std::fs::write(&path, &text).unwrap();
    let got = ok(&handler.handle("config.raw.get", json!({ "file": "agent:a1" }), &admin()).await);
    let content = got["content"].as_str().unwrap();
    assert!(!content.contains("REAL-DISCORD"));
    let edited = content.replace(
        "[channels.discord]\nbot_token = \"«set»\"",
        "[channels]\ndiscord = { bot_token = \"«set»\", bindings = 1 }",
    );
    assert_ne!(edited, content, "fixture replace must hit");
    let e = err(&handler
        .handle("config.raw.set", json!({ "file": "agent:a1", "content": edited, "base_hash": got["hash"] }), &admin())
        .await);
    assert!(!e.contains("REAL-DISCORD"), "secret leaked in error: {e}");
}

#[tokio::test]
async fn kept_secret_is_refused_when_its_destination_changes() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[secret_manager]\nvault_addr = \"https://vault.local\"\nvault_token_enc = \"ENC\"\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let got = ok(&handler.handle("config.raw.get", json!({ "file": "config" }), &admin()).await);
    let content = got["content"].as_str().unwrap();
    // Raw editor: address changed, token kept.
    let moved = content.replace("https://vault.local", "https://evil.example");
    let e = err(&handler
        .handle("config.raw.set", json!({ "file": "config", "content": moved, "base_hash": got["hash"] }), &admin())
        .await);
    assert!(e.contains("vault_addr"), "{e}");
    // [[accounts]] entry without id/name cannot keep a secret by position.
    let home2 = tempfile::tempdir().unwrap();
    std::fs::write(home2.path().join("config.toml"), "[[accounts]]\nlabel = \"x\"\napi_key_enc = \"K\"\n").unwrap();
    let h2 = MethodHandler::new(home2.path().to_path_buf()).await;
    let g2 = ok(&h2.handle("config.raw.get", json!({ "file": "config" }), &admin()).await);
    let c2 = g2["content"].as_str().unwrap().replace("label = \"x\"", "label = \"y\"");
    let e = err(&h2
        .handle("config.raw.set", json!({ "file": "config", "content": c2, "base_hash": g2["hash"] }), &admin())
        .await);
    assert!(e.contains("no `id` or `name`"), "{e}");
    // Typed RPC: same rule.
    let e = err(&handler
        .handle("system.update_config", json!({ "secret_manager": { "vault_addr": "https://evil.example" } }), &admin())
        .await);
    assert!(e.contains("vault_token"), "{e}");
}

#[tokio::test]
async fn tick_headers_do_not_follow_a_changed_url() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    ok(&handler
        .handle("tick.sources.upsert", json!({ "id": "f", "kind": "http_poll", "url": "https://a.example/x", "headers": { "X-Key": "s3cret" } }), &admin())
        .await);
    let e = err(&handler
        .handle("tick.sources.upsert", json!({ "id": "f", "url": "https://evil.example/x" }), &admin())
        .await);
    assert!(e.contains("re-enter"), "{e}");
    ok(&handler
        .handle("tick.sources.upsert", json!({ "id": "f", "url": "https://b.example/x", "headers": { "X-Key": "new" } }), &admin())
        .await);
}

#[tokio::test]
async fn config_writers_refuse_an_unparsable_file() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[broken\n").unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler.handle("skill_synthesis.update", json!({ "auto_run": true }), &admin()).await;
    assert!(matches!(frame, WsFrame::Response { ok: false, .. }), "{frame:?}");
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), "[broken\n");
}

#[tokio::test]
async fn marketplace_install_refuses_status_words_as_env_values() {
    let (_home, handler) = handler_with_agent().await;
    let e = err(&handler
        .handle(
            "marketplace.install",
            json!({ "id": "browserbase", "agent_id": "a1", "env": { "BROWSERBASE_API_KEY": "not_set" } }),
            &admin(),
        )
        .await);
    assert!(e.contains("BROWSERBASE_API_KEY"), "{e}");
}

#[tokio::test]
async fn inspect_returns_proactive_timezone_and_permission_marker() {
    let (_home, handler) = handler_with_agent().await;
    let p = ok(&handler.handle("agents.inspect", json!({ "agent_id": "a1" }), &admin()).await);
    assert!(p["proactive"].get("timezone").is_some() && p["proactive"].get("max_turns").is_some(), "{p}");
    assert_eq!(p["permissions"]["permissions_enforced_since"], "1.68.0");
    assert_eq!(p["permissions"]["can_schedule_tasks"], true);
}

#[tokio::test]
async fn raw_editor_unchanged_submission_is_a_true_no_op_and_types_are_checked() {
    let home = tempfile::tempdir().unwrap();
    let original = "[gateway]\nport = 18789\n\n[mcp_keys.\"ddc_secret_key\"]\nclient_id = \"x\"\n\n[night]\nllm_enabled = false\n";
    std::fs::write(home.path().join("config.toml"), original).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let got = ok(&handler.handle("config.raw.get", json!({ "file": "config" }), &admin()).await);
    let same = ok(&handler
        .handle("config.raw.set", json!({ "file": "config", "content": got["content"], "base_hash": got["hash"] }), &admin())
        .await);
    assert_eq!(same["unchanged"], true, "{same}");
    assert_eq!(same["hash"], got["hash"]);
    assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), original);
    assert!(audit_rows(home.path(), "config_raw_edited").is_empty());
    let backups = std::fs::read_dir(home.path())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".bak-"))
        .count();
    assert_eq!(backups, 0);

    for (from, to) in [("port = 18789", "port = \"eighteen\""), ("llm_enabled = false", "llm_enabled = \"nah\"")] {
        let edited = got["content"].as_str().unwrap().replace(from, to);
        let e = err(&handler
            .handle("config.raw.set", json!({ "file": "config", "content": edited, "base_hash": got["hash"] }), &admin())
            .await);
        assert!(e.contains("line"), "{e}");
    }
}

// ── format preservation (comments, order, arrays of tables, *_enc) ───────────

const COMMENTED: &str = r#"# DuDuClaw config, hand edited

# Gateway section comment
[gateway]
port = 18789 # trailing on port
bind = "127.0.0.1"
# auth_token = "commented-out"

# the mailbox
[mail]
# the switch
enabled = false # off for now
# who reads mail
default_agent = "dudu"
# drop folder
dropfolder_enabled = true

# who may hand work to whom
[delegation]
policy = "department" # the default

[[accounts]]
id = "a"
api_key_enc = "ZW5jcnlwdGVk/base64=="

[tick]
enabled = false

[[tick.sources]]
id = "s1"
kind = "http_poll"
url = "https://example.com"

[[channels.line.accounts]]
name = "oa1"
channel_token_enc = "abc=="

# trailing comment at end
"#;

/// Write the fixture after the handler is built (construction may touch
/// config.toml), then run one RPC and return the file text.
async fn rpc_on_fixture(method: &str, params: Value) -> String {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    std::fs::write(home.path().join("config.toml"), COMMENTED).unwrap();
    ok(&handler.handle(method, params, &admin()).await);
    std::fs::read_to_string(home.path().join("config.toml")).unwrap()
}

#[tokio::test]
async fn update_config_changes_only_the_target_line() {
    let out = rpc_on_fixture("system.update_config", json!({ "mail": { "enabled": true } })).await;
    assert_eq!(out, COMMENTED.replace("enabled = false # off for now", "enabled = true # off for now"));
}

#[tokio::test]
async fn update_config_appends_a_new_key_inside_its_section() {
    let out = rpc_on_fixture("system.update_config", json!({ "mail": { "auto_trigger": true } })).await;
    assert_eq!(
        out,
        COMMENTED.replace("dropfolder_enabled = true\n", "dropfolder_enabled = true\nauto_trigger = true\n")
    );
}

#[tokio::test]
async fn update_config_creates_a_missing_section_at_the_end() {
    let out = rpc_on_fixture("system.update_config", json!({ "night": { "llm_enabled": true } })).await;
    let body = COMMENTED.trim_end_matches("\n# trailing comment at end\n");
    assert!(out.starts_with(body), "{out}");
    assert!(out.contains("[night]\nllm_enabled = true\n"), "{out}");
    assert!(out.ends_with("# trailing comment at end\n"), "{out}");
}

#[tokio::test]
async fn update_config_removing_a_key_keeps_neighbour_comments() {
    let out = rpc_on_fixture("system.update_config", json!({ "mail": { "default_agent": "" } })).await;
    assert_eq!(out, COMMENTED.replace("# who reads mail\ndefault_agent = \"dudu\"\n", ""));
}

#[tokio::test]
async fn delegation_set_preserves_the_rest_of_the_file() {
    let out = rpc_on_fixture("delegation.set", json!({ "policy": "hierarchy" })).await;
    assert_eq!(
        out,
        COMMENTED.replace("policy = \"department\" # the default", "policy = \"hierarchy\" # the default")
    );
}

#[tokio::test]
async fn tick_source_upsert_appends_without_touching_other_tables() {
    let out = rpc_on_fixture(
        "tick.sources.upsert",
        json!({ "source": { "id": "s2", "kind": "http_poll", "url": "https://example.org" } }),
    )
    .await;
    let head = COMMENTED.split("[[channels.line.accounts]]").next().unwrap();
    // Everything before the new element is untouched (comments, *_enc).
    assert!(out.starts_with(head.trim_end()), "{out}");
    assert!(out.contains("[[channels.line.accounts]]\nname = \"oa1\"\nchannel_token_enc = \"abc==\"\n"), "{out}");
    assert!(out.contains("# trailing comment at end\n"), "{out}");
    let t: toml::Table = out.parse().unwrap();
    assert_eq!(t["tick"]["sources"].as_array().unwrap().len(), 2);
}
