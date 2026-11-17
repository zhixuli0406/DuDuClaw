//! Reviewer-built Rust fixture for scripts/live-test/third-party-mcp.sh.
//! `--stdio` is the upstream; `--claude --record PATH` is a local Claude shim.
use serde_json::{Value, json};
use std::{io::{BufRead, Write}, path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn stdio() {
    assert!(std::env::var_os("DUDUCLAW_AGENT_TOKEN").is_none());
    for line in std::io::stdin().lock().lines() {
        let Ok(v) = line.and_then(|s| serde_json::from_str::<Value>(&s).map_err(std::io::Error::other)) else { break; };
        let result = match v["method"].as_str() {
            Some("initialize") => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"rust-fixture","version":"1"}}),
            Some("tools/list") => json!({"tools":[{"name":"echo_text","description":"Local test value","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}}]}),
            _ => continue,
        };
        println!("{}", json!({"jsonrpc":"2.0","id":v["id"],"result":result}));
        std::io::stdout().flush().unwrap();
    }
}
async fn read(reader: &mut BufReader<tokio::process::ChildStdout>, id: u64) -> Value {
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["id"] == id { return v; }
    }
}
async fn exchange(entry: &Value, missing: bool) {
    let mut cmd = duduclaw_core::platform::async_command_for(entry["command"].as_str().unwrap());
    let mut env: std::collections::HashMap<String, String> = serde_json::from_value(entry["env"].clone()).unwrap();
    if missing { env.remove(duduclaw_core::ENV_AGENT_TOKEN); }
    cmd.args(entry["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()))
        .env_clear().envs(duduclaw_core::spawn_env::agent_cli_spawn_env_pairs()).envs(env)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = duduclaw_core::process_tree::ManagedChild::spawn(&mut cmd).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut input = child.child().stdin.take().unwrap();
        let mut reader = BufReader::new(child.child().stdout.take().unwrap());
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"fixture\",\"version\":\"1\"}}}\n").await.unwrap();
        let init = read(&mut reader, 1).await;
        if missing {
            assert_eq!(init["error"]["data"]["reason_code"], "identity_rejected");
        } else {
            assert_eq!(init["result"]["serverInfo"]["name"], "rust-fixture");
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n").await.unwrap();
            let tools = read(&mut reader, 2).await;
            assert!(tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "echo_text"));
        }
    }).await;
    child.kill_and_reap().await;
    result.expect("fixture proxy handshake timed out");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|s| s == "--stdio") { return stdio(); }
    if args.iter().any(|s| s == "--version") { println!("99.0.0"); return; }
    if args.windows(2).any(|s| s == ["auth", "status"]) {
        println!("{}", json!({"loggedIn":true,"authMethod":"claude.ai","email":"fixture@example.invalid"})); return;
    }
    let Some(index) = args.iter().position(|s| s == "--mcp-config") else {
        println!("{}", json!({"type":"result","result":"Local fixture","is_error":false})); return;
    };
    assert!(std::env::var_os(duduclaw_core::ENV_AGENT_TOKEN).is_none(), "token must not be on Claude CLI environment");
    let config: Value = serde_json::from_slice(&std::fs::read(&args[index+1]).unwrap()).unwrap();
    let entry = &config["mcpServers"]["fixture"];
    assert!(entry.is_object(), "fixture registration absent");
    exchange(entry, false).await;
    exchange(entry, true).await;
    let kind = if args.iter().any(|s| s.contains("[Scheduled Task:")) { "cron" } else { "chat" };
    let index = args.iter().position(|s| s == "--record").unwrap();
    let path = Path::new(&args[index+1]);
    duduclaw_core::with_file_lock(path, || {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", json!({"kind":kind,"initialize":true,"tools_list":true,"missing_token_refused":true}))
    }).unwrap();
    println!("{}", json!({"type":"system","subtype":"init","mcp_servers":[{"name":"fixture","status":"connected"}]}));
    println!("{}", json!({"type":"result","result":"Local MCP fixture passed","is_error":false}));
}
