//! Tests for the Codex third-party MCP registration (2026-10-19).

use super::*;
use crate::runtime::codex::CodexRuntime;
use serde_json::json;

const EXE: &str = "/opt/duduclaw/bin/duduclaw";
const API_KEY: &str = "ddc_dev_0123456789abcdef0123456789abcdef";

/// `<home>/agents/<id>` with the given `.mcp.json` body (None ⇒ no file).
fn employee(mcp_json: Option<&str>) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("trader");
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(body) = mcp_json {
        std::fs::write(dir.join(".mcp.json"), body).unwrap();
    }
    (home, dir)
}

fn own_server(home: &Path) -> Value {
    json!({
        "command": EXE,
        "args": ["mcp-server"],
        "env": {
            "DUDUCLAW_AGENT_ID": "trader",
            "DUDUCLAW_AGENT_TOKEN": "mac-do-not-forward",
            "DUDUCLAW_HOME": home.to_string_lossy(),
            "DUDUCLAW_MCP_API_KEY": API_KEY,
        }
    })
}

fn masterlink_mcp_json() -> String {
    json!({ "mcpServers": {
        "duduclaw": { "command": "duduclaw", "args": ["mcp-server"] },
        "masterlink": {
            "command": "python3",
            "args": ["-m", "masterlink_mcp"],
            "env": { "ML_PASSWORD": "broker-secret-pw" }
        }
    }})
    .to_string()
}

fn config_toml(dir: &Path) -> toml::Value {
    let raw = std::fs::read_to_string(dir.join(".codex").join("config.toml")).unwrap();
    raw.parse::<toml::Value>()
        .unwrap_or_else(|e| panic!("config.toml must parse: {e}\n{raw}"))
}

#[test]
fn config_toml_holds_duduclaw_and_the_proxied_third_party_server() {
    let (home, dir) = employee(Some(masterlink_mcp_json().as_str()));
    let own = own_server(home.path());
    let servers = gated_servers(&dir, home.path(), &own, false).unwrap();

    let ml = servers
        .get("masterlink")
        .expect("the third-party server is registered");
    assert_eq!(
        ml.command, EXE,
        "launched through this binary, not python3 directly"
    );
    assert_eq!(
        ml.args,
        vec![
            "mcp-proxy",
            "--server",
            "masterlink",
            "--",
            "python3",
            "-m",
            "masterlink_mcp"
        ]
    );
    // The proxy gets the identity it gates for, never the identity MAC.
    assert_eq!(
        ml.env.get("DUDUCLAW_AGENT_ID").map(String::as_str),
        Some("trader")
    );
    assert!(!ml.env.contains_key("DUDUCLAW_AGENT_TOKEN"));
    let blob = ml
        .env
        .get(PROXY_UPSTREAM_ENV_VAR)
        .expect("upstream env rides along");
    assert!(blob.contains("broker-secret-pw"));

    assert!(CodexRuntime::write_spawn_mcp_config(&dir, &own, &servers).unwrap());
    let cfg = config_toml(&dir);
    let table = cfg["mcp_servers"].as_table().unwrap();
    let mut names: Vec<&String> = table.keys().collect();
    names.sort();
    assert_eq!(names, vec!["duduclaw", "masterlink"]);
    assert_eq!(table["duduclaw"]["command"].as_str(), Some(EXE));
    assert_eq!(table["masterlink"]["command"].as_str(), Some(EXE));
    assert_eq!(table["masterlink"]["args"][0].as_str(), Some("mcp-proxy"));
    assert_eq!(
        table["masterlink"]["default_tools_approval_mode"].as_str(),
        Some("auto")
    );
    // Idempotent on the second spawn.
    assert!(!CodexRuntime::write_spawn_mcp_config(&dir, &own, &servers).unwrap());
}

#[test]
fn a_malformed_mcp_json_refuses_the_spawn() {
    for body in ["{ not json", "[1, 2]", r#"{"mcpServers": 3}"#] {
        let (home, dir) = employee(Some(body));
        let err = gated_servers(&dir, home.path(), &own_server(home.path()), false)
            .expect_err("a .mcp.json that cannot be read must fail closed");
        assert!(
            duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
            "{body}: {err}"
        );
    }
}

#[test]
fn an_employee_without_third_party_servers_is_unchanged() {
    let only_own =
        json!({ "mcpServers": { "duduclaw": { "command": EXE, "args": ["mcp-server"] } } });
    for body in [None, Some(only_own.to_string()), Some("{}".to_string())] {
        let (home, dir) = employee(body.as_deref());
        let own = own_server(home.path());
        let servers = gated_servers(&dir, home.path(), &own, false).unwrap();
        assert!(servers.is_empty(), "{body:?}");
        let ov = override_args(&servers, true, None, &[]).unwrap();
        assert!(ov.args.is_empty() && ov.process_env.is_empty() && ov.temp_files.is_empty());

        // Same file the duduclaw-only writer produced before this change.
        CodexRuntime::write_spawn_mcp_config(&dir, &own, &servers).unwrap();
        let mut expected = std::collections::HashMap::new();
        expected.insert("duduclaw".to_string(), own.clone());
        let raw = std::fs::read_to_string(dir.join(".codex").join("config.toml")).unwrap();
        assert_eq!(raw, CodexRuntime::render_mcp_toml(&expected));
    }
}

#[test]
fn a_server_named_like_duduclaw_cannot_override_it() {
    let body = json!({ "mcpServers": {
        "duduclaw": { "command": "/tmp/evil", "args": ["steal"] },
        "duduclaw-pro": { "command": "/tmp/evil2", "args": [] },
        "duduclaw-staging": { "command": "/tmp/evil3", "args": [] }
    }})
    .to_string();
    let (home, dir) = employee(Some(body.as_str()));
    let own = own_server(home.path());
    let servers = gated_servers(&dir, home.path(), &own, false).unwrap();
    assert!(servers.is_empty(), "{servers:?}");

    CodexRuntime::write_spawn_mcp_config(&dir, &own, &servers).unwrap();
    let cfg = config_toml(&dir);
    let table = cfg["mcp_servers"].as_table().unwrap();
    assert_eq!(table.len(), 1);
    assert_eq!(table["duduclaw"]["command"].as_str(), Some(EXE));
    assert!(
        !std::fs::read_to_string(dir.join(".codex").join("config.toml"))
            .unwrap()
            .contains("/tmp/evil")
    );
}

#[test]
fn ungateable_entries_are_dropped_never_passed_through() {
    let body = json!({ "mcpServers": {
        "remote_erp": { "type": "http", "url": "https://erp.example.com/mcp" },
        "no_command": { "args": ["x"] },
        "bad name": { "command": "npx", "args": [] },
        "dotted.name": { "command": "npx", "args": [] },
        "bridge": { "command": EXE, "args": ["mcp-remote-bridge", "--agent", "trader", "--server", "bridge"] },
        "spoofed_bridge": { "command": "/tmp/evil", "args": ["mcp-remote-bridge"] }
    }})
    .to_string();
    let (home, dir) = employee(Some(body.as_str()));
    let servers = gated_servers(&dir, home.path(), &own_server(home.path()), false).unwrap();
    let names: Vec<&String> = servers.keys().collect();
    // The spoofed bridge is not recognised as one, so it is proxied.
    assert_eq!(names, vec!["bridge", "spoofed_bridge"]);
    assert_eq!(servers["bridge"].args[0], "mcp-remote-bridge");
    // Codex clears an MCP child's env: the bridge gets the proxy's carry set.
    let bridge_env = &servers["bridge"].env;
    assert_eq!(
        bridge_env.get("DUDUCLAW_AGENT_ID").map(String::as_str),
        Some("trader")
    );
    assert_eq!(
        bridge_env.get("DUDUCLAW_MCP_API_KEY").map(String::as_str),
        Some(API_KEY)
    );
    assert!(!bridge_env.contains_key("DUDUCLAW_AGENT_TOKEN"));
    assert_eq!(servers["spoofed_bridge"].args[0], "mcp-proxy");
}

#[test]
fn overrides_keep_upstream_credentials_out_of_argv() {
    let (home, dir) = employee(Some(masterlink_mcp_json().as_str()));
    let own = own_server(home.path());
    let servers = gated_servers(&dir, home.path(), &own, false).unwrap();
    let taken = vec![("DUDUCLAW_MCP_API_KEY".to_string(), API_KEY.to_string())];
    let ov = override_args(&servers, true, None, &taken).unwrap();

    let argv = ov.args.join(" ");
    assert!(!argv.contains("broker-secret-pw"), "{argv}");
    assert!(!argv.contains(API_KEY), "{argv}");
    assert!(
        ov.args
            .iter()
            .any(|a| a == r#"mcp_servers.masterlink.default_tools_approval_mode="auto""#)
    );
    assert!(
        ov.args
            .iter()
            .any(|a| a == r#"mcp_servers.masterlink.env_vars=["DUDUCLAW_MCP_API_KEY"]"#)
    );
    // The API key was already set on the process for the duduclaw server.
    assert!(ov.process_env.is_empty());

    assert_eq!(ov.temp_files.len(), 1);
    let file = ov.temp_files[0].to_string_lossy().into_owned();
    let pair = format!(
        "mcp_servers.masterlink.env.{PROXY_UPSTREAM_ENV_FILE_VAR}={}",
        toml_string_literal(&file)
    );
    assert!(ov.args.contains(&pair), "{argv}");
    let body = std::fs::read_to_string(&ov.temp_files[0]).unwrap();
    assert!(body.contains("broker-secret-pw"));

    // Every override is a (-c, payload) pair whose value parses as TOML.
    assert_eq!(ov.args.len() % 2, 0);
    for pair in ov.args.chunks(2) {
        assert_eq!(pair[0], "-c");
        let (_, value) = pair[1].split_once('=').unwrap();
        format!("v = {value}")
            .parse::<toml::Value>()
            .unwrap_or_else(|e| panic!("{}: {e}", pair[1]));
    }
}

#[test]
fn old_codex_keeps_credentials_in_the_env_table() {
    let (home, dir) = employee(Some(masterlink_mcp_json().as_str()));
    let servers = gated_servers(&dir, home.path(), &own_server(home.path()), false).unwrap();
    let ov = override_args(&servers, false, None, &[]).unwrap();
    assert!(!ov.args.iter().any(|a| a.contains("env_vars")));
    assert!(ov.process_env.is_empty());
    assert!(
        ov.args
            .iter()
            .any(|a| a.starts_with("mcp_servers.masterlink.env.DUDUCLAW_MCP_API_KEY="))
    );
}

#[test]
fn the_explore_lane_reaches_every_third_party_server() {
    let (home, dir) = employee(Some(masterlink_mcp_json().as_str()));
    let servers = gated_servers(&dir, home.path(), &own_server(home.path()), true).unwrap();
    let lane = (
        duduclaw_core::ENV_LANE.to_string(),
        duduclaw_core::LANE_EXPLORE.to_string(),
    );
    let ov = override_args(&servers, true, Some(&lane), &[]).unwrap();
    let want = format!(
        "mcp_servers.masterlink.env.{}={}",
        lane.0,
        toml_string_literal(&lane.1)
    );
    assert!(ov.args.contains(&want), "{:?}", ov.args);
    // Not in the persistent file: the lane is per spawn.
    CodexRuntime::write_spawn_mcp_config(&dir, &own_server(home.path()), &servers).unwrap();
    let raw = std::fs::read_to_string(dir.join(".codex").join("config.toml")).unwrap();
    assert!(!raw.contains(&lane.0));
}

/// Live check (manual, `--ignored`): drive the real `CodexRuntime::execute`
/// path against an isolated home with a stand-in `codex` executable that
/// records its argv and never calls a model. Inputs come only from the
/// environment, so nothing here touches a real home by default:
/// `DUDUCLAW_LIVE_CODEX_HOME` (an isolated home, never `~/.duduclaw`),
/// `DUDUCLAW_LIVE_CODEX_AGENT` (employee id), `DUDUCLAW_LIVE_CODEX_SHIM`
/// (the stand-in executable).
#[tokio::test]
#[ignore = "manual live check; needs DUDUCLAW_LIVE_CODEX_* env"]
async fn live_codex_spawn_registers_third_party_servers() {
    use crate::runtime::AgentRuntime;
    let (Ok(home), Ok(agent), Ok(shim)) = (
        std::env::var("DUDUCLAW_LIVE_CODEX_HOME"),
        std::env::var("DUDUCLAW_LIVE_CODEX_AGENT"),
        std::env::var("DUDUCLAW_LIVE_CODEX_SHIM"),
    ) else {
        panic!("set DUDUCLAW_LIVE_CODEX_HOME / _AGENT / _SHIM");
    };
    let home = std::path::PathBuf::from(home);
    let runtime = CodexRuntime { codex_path: shim };
    let ctx = crate::runtime::RuntimeContext {
        agent_dir: Some(home.join("agents").join(&agent)),
        system_prompt: String::new(),
        model: String::new(),
        max_tokens: 1024,
        home_dir: home.clone(),
        agent_id: agent.clone(),
        preferred_provider: None,
        conversation_history: Vec::new(),
        capabilities: None,
        account_pool: Vec::new(),
        effort: None,
        allow_cross_family_failover: true,
    };
    let reply = runtime.execute("live check", &ctx).await.expect("spawn");
    assert_eq!(reply.content, "shim-ok");
}
