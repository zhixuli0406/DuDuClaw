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
