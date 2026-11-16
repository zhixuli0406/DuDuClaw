//! Real Claude preparation -> real proxy -> Rust-only stdio fixture.
use serde_json::{Value, json};
use std::{io::{BufRead, Write}, path::{Path, PathBuf}, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// Run only as a child of the regression test. libtest's framing is ignored by
// the protocol reader; the MCP replies themselves are ordinary JSON-RPC lines.
#[test]
#[ignore = "stdio child fixture"]
fn stdio_fixture() {
    // Separate libtest progress text from protocol frames.
    println!();
    assert!(std::env::var_os("DUDUCLAW_AGENT_TOKEN").is_none());
    assert!(std::env::var_os("DUDUCLAW_MCP_API_KEY").is_none());
    let mode = std::env::var("MCP_FIXTURE_MODE").unwrap_or_default();
    if mode == "timeout" { std::thread::sleep(Duration::from_secs(30)); return; }
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break; };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue; };
        if mode == "failure" && v.get("id").is_some() {
            println!("{}", json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32603,"message":"fixture refusal"}}));
            std::io::stdout().flush().unwrap();
            continue;
        }
        let result = match v["method"].as_str() {
            Some("initialize") => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"rust-fixture","version":"1"}}),
            Some("tools/list") => json!({"tools":[{"name":"echo_text","description":"Read a fixture value","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}]}),
            _ => continue,
        };
        println!("{}", json!({"jsonrpc":"2.0","id":v["id"],"result":result}));
        std::io::stdout().flush().unwrap();
    }
}

fn binary() -> Option<PathBuf> {
    let path = std::env::var_os("DUDUCLAW_P1_PILOT_BINARY").map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_duduclaw").map(PathBuf::from));
    path.filter(|p| p.is_file()).and_then(|p| p.canonicalize().ok())
}

async fn response<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R, id: u64) -> Value {
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0, "unexpected proxy EOF");
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            if v["id"] == id { return v; }
        }
    }
}

async fn exchange(entry: &Value, home: &Path, missing_token: bool) -> (Value, Value, String) {
    let mut env: std::collections::HashMap<String, String> = serde_json::from_value(entry["env"].clone()).unwrap();
    if missing_token { env.remove(duduclaw_core::ENV_AGENT_TOKEN); }
    let mut cmd = duduclaw_core::platform::async_command_for(entry["command"].as_str().unwrap());
    cmd.args(entry["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()))
        .env_clear().env("HOME", home.join("os-home")).envs(env)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = duduclaw_core::process_tree::ManagedChild::spawn(&mut cmd).unwrap();
    let mut input = child.child().stdin.take().unwrap();
    let mut reader = BufReader::new(child.child().stdout.take().unwrap());
    let stderr = child.child().stderr.take().unwrap();
    let drain = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut text = String::new();
        BufReader::new(stderr).read_to_string(&mut text).await.unwrap();
        text
    });
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n").await.unwrap();
        let init = response(&mut reader, 1).await;
        if missing_token {
            drop(input);
            child.child().wait().await.unwrap();
            (init, Value::Null)
        } else {
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n").await.unwrap();
            let tools = response(&mut reader, 2).await;
            (init, tools)
        }
    }).await;
    child.kill_and_reap().await;
    let stderr = drain.await.unwrap();
    let (init, tools) = result.expect("bounded real proxy exchange");
    (init, tools, stderr)
}

#[tokio::test]
#[ignore = "run by isolated_spawn_regression"]
async fn claude_spawn_entry_starts_real_proxy_and_missing_token_is_audited() {
    let Some(binary) = binary() else {
        eprintln!("SKIP third-party MCP regression: set DUDUCLAW_P1_PILOT_BINARY to a built duduclaw binary");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/prod-shaped");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(home.path().join("os-home")).unwrap();
    std::fs::write(home.path().join("config.toml"), "[delegation]\nrequire_identity_token = true\n").unwrap();
    duduclaw_core::identity_token::ensure_key(home.path()).unwrap();
    std::fs::write(dir.join("agent.toml"), "[agent]\nname = 'prod-shaped'\nstatus = 'active'\n[capabilities]\nallowed_tools = ['mcp__duduclaw__*']\n").unwrap();
    let config = json!({"mcpServers":{"fixture":{"command":std::env::current_exe().unwrap(),
        "args":["--exact","third_party_mcp_tests::stdio_fixture","--ignored","--nocapture"],"env":{"FIXTURE_SECRET":"fixture-secret-never-log"}}}});
    std::fs::write(dir.join(".mcp.json"), config.to_string()).unwrap();
    // This test process is run as its own filtered child below; no other
    // test threads read the resolver override. Restore it even on unwind.
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore { fn drop(&mut self) { unsafe {
        if let Some(v) = &self.0 { std::env::set_var("DUDUCLAW_BIN", v); }
        else { std::env::remove_var("DUDUCLAW_BIN"); }
    } } }
    let _restore = Restore(std::env::var_os("DUDUCLAW_BIN"));
    unsafe { std::env::set_var("DUDUCLAW_BIN", &binary); }
    duduclaw_agent::mcp_template::prepare_mcp_config_for_spawn(&dir).unwrap();
    let temp = duduclaw_gateway::redaction_proxy::maybe_proxy_mcp_config_in_lane(home.path(), &dir.join(".mcp.json"), false).unwrap().unwrap();
    let prepared: Value = serde_json::from_slice(&std::fs::read(&temp).unwrap()).unwrap();
    let entry = &prepared["mcpServers"]["fixture"];
    assert_eq!(entry["command"], binary.to_string_lossy().as_ref());
    let token = entry["env"][duduclaw_core::ENV_AGENT_TOKEN].as_str().expect("proxy identity attached");
    assert!(!token.is_empty());
    let persisted: Value = serde_json::from_slice(&std::fs::read(dir.join(".mcp.json")).unwrap()).unwrap();
    assert!(persisted["mcpServers"]["fixture"]["env"].get(duduclaw_core::ENV_AGENT_TOKEN).is_none());
    let (init, tools, _) = exchange(entry, home.path(), false).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "rust-fixture");
    assert!(tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "echo_text"));
    let (refused, _, stderr) = exchange(entry, home.path(), true).await;
    assert_eq!(refused["jsonrpc"], "2.0");
    assert_eq!(refused["id"], 1);
    assert_eq!(refused["error"]["code"], -32001);
    assert_eq!(refused["error"]["data"]["reason_code"], "identity_rejected");
    assert!(stderr.contains("MCP caller identity rejected: refusing to start the upstream proxy"));
    let audit = std::fs::read_to_string(home.path().join("channel_failures.jsonl")).unwrap();
    let records: Vec<Value> = audit.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["reason_code"], "identity_rejected");
    assert_eq!(records[0]["agent_id"], "prod-shaped");
    assert_eq!(records[0]["server"], "fixture");
    assert!(!audit.contains(token));
    assert!(!audit.contains("fixture-secret-never-log"));
    assert!(!audit.contains("--nocapture"));
    assert!(!refused.to_string().contains(token));
    // Exercise the admin RPC result contract through the same real proxy.
    let handler = duduclaw_gateway::handlers::MethodHandler::new(home.path().to_path_buf()).await;
    let admin = duduclaw_auth::UserContext::admin_fallback();
    for (mode, action, expected) in [("success", "add", None),
        ("failure", "edit", Some("protocol_error")), ("timeout", "edit", Some("timeout"))] {
        let frame = Box::pin(handler.handle("mcp.update", json!({"agent_id":"prod-shaped", "action":action,
            "server_name":"fixture", "server_def":{"command":std::env::current_exe().unwrap(),
                "args":["--exact","third_party_mcp_tests::stdio_fixture","--ignored","--nocapture"],
                "env":{"MCP_FIXTURE_MODE":mode}}}), &admin)).await;
        let duduclaw_gateway::protocol::WsFrame::Response { ok:true, payload:Some(payload), .. } = frame
            else { panic!("mcp.update failed to save fixture"); };
        assert_eq!(payload["success"], true);
        assert_eq!(payload["probe"]["ok"], expected.is_none());
        if let Some(reason) = expected { assert_eq!(payload["probe"]["reason_code"], reason); }
        else { assert_eq!(payload["probe"]["tool_count"], 1); }
        assert!(duduclaw_agent::mcp_template::read_mcp_config(&dir).unwrap().mcp_servers.contains_key("fixture"));
    }

}

#[test]
fn isolated_spawn_regression() {
    if binary().is_none() {
        eprintln!("SKIP third-party MCP regression: set DUDUCLAW_P1_PILOT_BINARY to a built duduclaw binary");
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "third_party_mcp_tests::claude_spawn_entry_starts_real_proxy_and_missing_token_is_audited",
            "--ignored", "--nocapture"]).status().unwrap();
    assert!(status.success());
}
