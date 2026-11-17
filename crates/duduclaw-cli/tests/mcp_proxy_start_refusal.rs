//! A proxy's unverified process environment is never a gateway status source.
use std::{collections::BTreeMap, io::Write, path::{Path, PathBuf}, process::{Command, Stdio}};
use serde_json::{Value, json};
use duduclaw_gateway::{handlers::MethodHandler, protocol::WsFrame};

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, dir: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                entries.insert(relative, None);
                visit(root, &path, entries);
            } else {
                entries.insert(relative, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response { ok: true, payload: Some(value), .. } => value,
        other => panic!("unexpected RPC response: {other:?}"),
    }
}

#[tokio::test]
async fn forged_identity_refusal_returns_frame_without_home_writes_or_list_changes() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[delegation]\nrequire_identity_token = true\n").unwrap();
    duduclaw_core::ensure_identity_key(home.path()).unwrap();
    for employee in ["alice", "bob"] {
        let dir = home.path().join("agents").join(employee);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".mcp.json"), json!({"mcpServers":{"fixture":{
            "command":"fixture", "args":[], "env":{}
        }}}).to_string()).unwrap();
    }
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let admin = duduclaw_auth::UserContext::admin_fallback();
    let before_list = payload(Box::pin(handler.handle("mcp.list", json!({}), &admin)).await);
    for claimed in ["alice", "bob", "dashboard"] {
        let before = snapshot(home.path());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_duduclaw"));
        cmd.env_clear();
        for name in ["PATH", "TMPDIR", "SystemRoot"] {
            if let Some(value) = std::env::var_os(name) { cmd.env(name, value); }
        }
        let mut child = cmd.env("HOME", home.path()).env("DUDUCLAW_HOME", home.path())
            .env("DUDUCLAW_AGENT_ID", claimed).env("DUDUCLAW_AGENT_TOKEN", "forged")
            .args(["mcp-proxy", "--server", "fixture", "--", "never-spawn-this-command"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"initialize\"}\n").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        let frame: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(frame["id"], 7);
        assert_eq!(frame["error"]["code"], -32001);
        assert_eq!(frame["error"]["data"]["reason_code"], "identity_rejected");
        assert_eq!(String::from_utf8(output.stderr).unwrap().trim(),
            "mcp-proxy startup_refused reason_code=identity_rejected");
        assert_eq!(snapshot(home.path()), before, "claim {claimed} wrote under home");
        let after = payload(Box::pin(handler.handle("mcp.list", json!({}), &admin)).await);
        assert_eq!(after, before_list, "claim {claimed} changed mcp.list");
    }
}

// Reexec keeps upstream behaviour and process environment isolated from tests.
#[test]
#[ignore = "stdio upstream fixture"]
fn proxy_upstream_fixture() {
    use std::io::BufRead;
    if std::env::var("MCP_FIXTURE_MODE").as_deref() == Ok("immediate") {
        std::process::exit(1);
    }
    for line in std::io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if frame["method"] == "initialize" {
            println!("{}", json!({"jsonrpc":"2.0", "id":frame["id"], "result":{
                "protocolVersion":"2024-11-05", "capabilities":{}, "serverInfo":{"name":"fixture","version":"1"}
            }}));
            std::io::stdout().flush().unwrap();
        } else if frame.get("id").is_some() {
            std::process::exit(1);
        }
    }
}

fn upstream_exit_with_open_client(mode: &str) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[delegation]\nrequire_identity_token=true\n").unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
    let key = duduclaw_core::ensure_identity_key(home.path()).unwrap();
    let token = duduclaw_core::mint_identity_token(&key, "alice");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_duduclaw"));
    cmd.env_clear();
    for name in ["PATH", "TMPDIR", "SystemRoot"] {
        if let Some(value) = std::env::var_os(name) { cmd.env(name, value); }
    }
    let upstream_env = json!({"MCP_FIXTURE_MODE":mode}).to_string();
    let mut child = cmd.env("HOME", home.path()).env("DUDUCLAW_HOME", home.path())
        .env("DUDUCLAW_AGENT_ID", "alice").env("DUDUCLAW_AGENT_TOKEN", token)
        .env("DUDUCLAW_MCP_PROXY_ENV", upstream_env)
        .args(["mcp-proxy", "--server", "fixture", "--"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "proxy_upstream_fixture", "--ignored", "--nocapture"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n").unwrap();
    if mode != "immediate" {
        input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n").unwrap();
    }
    // What matters is "exits while stdin is still open". The budget only has to
    // cover two cold debug process launches (the proxy and the re-exec'd test
    // binary as upstream), which can take seconds on a loaded runner.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if std::time::Instant::now() >= end {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("proxy waited for client stdin to close ({mode})");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Keep this handle alive until after observing the proxy's exit.
    let output = child.wait_with_output().unwrap();
    drop(input);
    let frames: Vec<Value> = String::from_utf8(output.stdout).unwrap().lines()
        .filter_map(|line| serde_json::from_str(line).ok()).collect();
    if mode == "immediate" {
        assert!(frames.iter().any(|f| f["id"] == 1 && f["error"]["data"]["reason_code"] == "upstream_io_unavailable"));
        assert!(String::from_utf8(output.stderr).unwrap().contains("startup_refused reason_code=upstream_io_unavailable"));
    } else {
        assert!(frames.iter().any(|f| f["id"] == 1 && f.get("result").is_some()));
        assert!(frames.iter().any(|f| f["id"] == 2
            && f["error"]["data"]["reason_code"] == "upstream_io_unavailable"));
        assert!(!String::from_utf8(output.stderr).unwrap().contains("startup_refused"));
    }
}

#[test]
fn immediate_upstream_exit_refuses_start_with_client_stdin_open() {
    upstream_exit_with_open_client("immediate");
}

#[test]
fn initialized_upstream_exit_answers_pending_request_with_client_stdin_open() {
    upstream_exit_with_open_client("initialized");
}
