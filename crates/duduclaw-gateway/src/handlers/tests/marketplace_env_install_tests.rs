//! Marketplace installs write required env as literal values (the allowlisted
//! spawn env cannot resolve `${NAME}` references) and `mcp.list` masks them.

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
async fn browserbase_install_writes_literals_and_list_masks_them() {
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
    assert_eq!(env["BROWSERBASE_API_KEY"], "bb-secret-1");
    assert_eq!(env["BROWSERBASE_PROJECT_ID"], "proj-2");
    assert_eq!(env["GEMINI_API_KEY"], "gm-secret-3");
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
                    "BROWSERBASE_API_KEY": "k",
                    "BROWSERBASE_PROJECT_ID": "p",
                    "GEMINI_API_KEY": "g"
                }
            }
        }))
        .await,
    );
}

#[tokio::test]
async fn playwright_install_needs_no_env() {
    let home = tempfile::tempdir().unwrap();
    let h = handler_with_agent(home.path()).await;
    payload(h.handle_marketplace_install(json!({"id": "playwright", "agent_id": "alice"})).await);
}
