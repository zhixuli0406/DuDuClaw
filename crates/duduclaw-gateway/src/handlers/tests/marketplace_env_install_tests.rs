//! Marketplace, update and manifest imports persist only encrypted-store references.

use super::MethodHandler;
use crate::protocol::WsFrame;
use serde_json::{Value, json};

async fn handler_with_agent(home: &std::path::Path) -> MethodHandler {
    std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
    MethodHandler::new(home.to_path_buf()).await
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(p), .. } => p,
        other => panic!("expected ok response, got {other:?}"),
    }
}

fn error_text(frame: WsFrame) -> String {
    match frame {
        WsFrame::Response { ok: false, error, .. } => format!("{error:?}"),
        other => panic!("expected error response, got {other:?}"),
    }
}

#[tokio::test]
async fn browserbase_install_writes_references_and_list_masks_them() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;

    payload(
        h.handle_marketplace_install(json!({
            "id": "browserbase",
            "agent_id": "alice",
            "env": {
                "BROWSERBASE_API_KEY": "bb-secret-1",
                "BROWSERBASE_PROJECT_ID": "proj-2",
                "GEMINI_API_KEY": "gm-secret-3"
            }
        }))
        .await,
    );

    let agent_dir = home.path().join("agents").join("alice");
    let cfg = duduclaw_agent::mcp_template::read_mcp_config(&agent_dir).unwrap();
    let env = &cfg.mcp_servers["browserbase"].env;
    assert_eq!(env["BROWSERBASE_API_KEY"], "secret://mcp/browserbase/BROWSERBASE_API_KEY");
    assert_eq!(env["BROWSERBASE_PROJECT_ID"], "secret://mcp/browserbase/BROWSERBASE_PROJECT_ID");
    assert_eq!(env["GEMINI_API_KEY"], "secret://mcp/browserbase/GEMINI_API_KEY");
    let config_text = std::fs::read_to_string(agent_dir.join(".mcp.json")).unwrap();
    for secret in ["bb-secret-1", "proj-2", "gm-secret-3"] {
        assert!(!config_text.contains(secret));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(agent_dir.join(".mcp.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let list = payload(h.handle_mcp_list().await);
    let text = list.to_string();
    for secret in ["bb-secret-1", "proj-2", "gm-secret-3"] {
        assert!(!text.contains(secret), "mcp.list leaked {secret}: {text}");
    }
    let server = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent_id"] == "alice")
        .unwrap()["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "browserbase")
        .unwrap()
        .clone();
    assert_eq!(server["env"]["BROWSERBASE_API_KEY"], "set");
    assert_eq!(server["env"]["GEMINI_API_KEY"], "set");
}

#[tokio::test]
async fn browserbase_install_without_values_is_refused_naming_variables() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;

    let err = error_text(
        h.handle_marketplace_install(json!({
            "id": "browserbase",
            "agent_id": "alice",
            "env": { "BROWSERBASE_API_KEY": "only-this" }
        }))
        .await,
    );
    assert!(err.contains("BROWSERBASE_PROJECT_ID") && err.contains("GEMINI_API_KEY"), "{err}");
    assert!(!err.contains("only-this"), "{err}");
    assert!(!home.path().join("agents/alice/.mcp.json").exists());
}

#[tokio::test]
async fn mcp_update_refuses_catalog_server_with_env_references() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;

    let err = error_text(
        h.handle_mcp_update(&json!({
            "agent_id": "alice",
            "action": "add",
            "server_name": "browserbase",
            "server_def": {
                "command": "npx",
                "args": ["-y", "@browserbasehq/mcp"],
                "env": {
                    "BROWSERBASE_API_KEY": "${BROWSERBASE_API_KEY}",
                    "BROWSERBASE_PROJECT_ID": "proj",
                    "GEMINI_API_KEY": "gm"
                }
            }
        }))
        .await,
    );
    assert!(err.contains("BROWSERBASE_API_KEY"), "{err}");

    payload(
        h.handle_mcp_update(&json!({
            "agent_id": "alice",
            "action": "add",
            "server_name": "browserbase",
            "server_def": {
                "command": "npx",
                "args": ["-y", "@browserbasehq/mcp"],
                "env": {
                    "BROWSERBASE_API_KEY": "sk-update-browserbase",
                    "BROWSERBASE_PROJECT_ID": "project-update-private",
                    "GEMINI_API_KEY": "sk-update-gemini"
                }
            }
        }))
        .await,
    );
    let raw = std::fs::read_to_string(home.path().join("agents/alice/.mcp.json")).unwrap();
    for value in ["sk-update-browserbase", "project-update-private", "sk-update-gemini"] {
        assert!(!raw.contains(value));
    }
    assert!(raw.contains("secret://mcp/browserbase/GEMINI_API_KEY"));

}

#[tokio::test]
async fn playwright_install_needs_no_env() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;
    payload(h.handle_marketplace_install(json!({"id": "playwright", "agent_id": "alice"})).await);
}

#[tokio::test]
async fn manifest_import_install_stores_only_env_references() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;
    let manifest = r#"{"mcpServers":{"example":{"command":"npx","args":["-y","@modelcontextprotocol/server-memory"],"env":{"API_KEY":"sk-import-private","LOG_LEVEL":"info"}}}}"#;
    let parsed = MethodHandler::parse_mcp_manifest(manifest, "fallback").unwrap();
    let (name, def, _) = &parsed[0];
    payload(h.handle_mcp_import_install(json!({
        "agent_id":"alice", "server_name": name, "server_def": def
    })).await);
    let raw = std::fs::read_to_string(home.path().join("agents/alice/.mcp.json")).unwrap();
    assert!(!raw.contains("sk-import-private"));
    assert!(raw.contains("secret://mcp/example/API_KEY"));
    let doc: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(doc["mcpServers"]["example"]["env"]["LOG_LEVEL"], "info");
}

#[tokio::test]
async fn mcp_list_includes_both_sources_and_keeps_toml_credentials_private() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;
    let dir = home.path().join("agents/alice");
    std::fs::write(dir.join(".mcp.json"), r#"{"mcpServers":{"local":{"command":"fixture","args":[],"env":{}}}}"#).unwrap();
    std::fs::write(dir.join("agent.toml"), "[[mcp.external]]\nname = 'api-only'\ncommand = 'fixture'\nbearer_token = 'do-not-disclose'\nenv = { KEY = 'also-secret' }\n").unwrap();
    let list = payload(h.handle_mcp_list().await);
    let servers = list["agents"][0]["servers"].as_array().unwrap();
    let local = servers.iter().find(|v| v["source"] == "mcp_json").unwrap();
    assert_eq!(local["effective_runtimes"], json!(["claude", "codex", "grok", "in_process"]));
    let external = servers.iter().find(|v| v["source"] == "agent_toml").unwrap();
    assert_eq!(external["read_only"], true);
    assert_eq!(external["effective_runtimes"], json!(["in_process"]));
    assert!(!list.to_string().contains("do-not-disclose"));
    assert!(!list.to_string().contains("also-secret"));
}

#[tokio::test]
async fn mcp_update_probe_is_admin_only() {
    use duduclaw_auth::{UserContext, models::UserRole};
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;
    let employee = UserContext {
            origin: duduclaw_auth::acl::ContextOrigin::Other,
            member_key_id: None, credential_epoch:0, user_id:"employee".into(), email:"employee@example.invalid".into(),
        role:UserRole::Employee, agent_access:Default::default(), must_change_password:false,
        team_access:Default::default(), acl_generation:0 };
    for action in ["add", "edit"] {
        let frame = Box::pin(h.handle("mcp.update", json!({"agent_id":"alice","action":action,
            "server_name":"fixture","server_def":{"command":"fixture","args":[],"env":{}}}), &employee)).await;
        assert!(!error_text(frame).is_empty());
    }
    let frame = Box::pin(h.handle("mcp.probe", json!({"agent_id":"alice","server_name":"fixture"}), &employee)).await;
    assert!(!error_text(frame).is_empty());
    assert!(!home.path().join("agents/alice/.mcp.json").exists());
}

#[tokio::test]
async fn mcp_update_keeps_saved_entry_when_probe_fails() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;
    for action in ["add", "edit"] {
        let result = payload(h.handle_mcp_update(&json!({"agent_id":"alice", "action":action,
            "server_name":"missing", "server_def":{"command":"/nonexistent/mcp-fixture", "args":[],"env":{}}})).await);
        assert_eq!(result["success"], true);
        assert_eq!(result["probe"]["ok"], false);
        assert!(result["probe"]["reason_code"].is_string());
        assert!(duduclaw_agent::mcp_template::read_mcp_config(&home.path().join("agents/alice")).unwrap().mcp_servers.contains_key("missing"));
    }
}

#[tokio::test]
async fn mcp_list_ignores_forged_shared_start_records() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;
    std::fs::write(home.path().join("agents/alice/.mcp.json"), json!({"mcpServers":{
        "fixture":{"command":"fixture","args":[],"env":{}}
    }}).to_string()).unwrap();
    let before = payload(h.handle_mcp_list().await);
    std::fs::write(home.path().join("channel_failures.jsonl"), json!({"classification":"mcp_start",
        "agent_id":"alice","server":"fixture","reason_code":"identity_rejected","ok":false,
        "time":"2099-01-01T00:00:00Z"}).to_string()).unwrap();
    assert_eq!(payload(h.handle_mcp_list().await), before);
}
