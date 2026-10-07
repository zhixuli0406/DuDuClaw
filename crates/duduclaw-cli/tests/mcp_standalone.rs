//! Standalone MCP profile, end to end: `duduclaw mcp init` issues the key,
//! the real `duduclaw mcp-server` serves it over stdio, every listed tool is
//! callable, and memory and wiki round-trip.
//!
//! Isolated home, empty environment (only PATH/TMPDIR plus the variables set
//! here), so no ambient credential or identity reaches the child.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const CLI: &str = env!("CARGO_BIN_EXE_duduclaw");

fn base_command(home: &Path, data: &Path) -> Command {
    let mut cmd = Command::new(CLI);
    cmd.env_clear();
    for key in ["PATH", "TMPDIR", "SystemRoot"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    cmd.env("HOME", home).env("DUDUCLAW_HOME", data);
    cmd
}

/// Run `duduclaw mcp init --client print` and take the key from its output.
fn init_token(home: &Path, data: &Path) -> String {
    let out = base_command(home, data)
        .args(["mcp", "init", "--client", "print"])
        .output()
        .expect("run mcp init");
    assert!(
        out.status.success(),
        "mcp init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let marker = "DUDUCLAW_MCP_API_KEY=";
    let start = stdout.find(marker).expect("key in output") + marker.len();
    stdout[start..]
        .split_whitespace()
        .next()
        .unwrap()
        .trim_matches('\'')
        .to_string()
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: i64,
}

impl Server {
    fn start(home: &Path, data: &Path, token: &str, cwd: &Path) -> Self {
        let mut child = base_command(home, data)
            .env("DUDUCLAW_MCP_API_KEY", token)
            .arg("mcp-server")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mcp-server");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let mut s = Server {
            child,
            stdin,
            rx,
            next_id: 0,
        };
        let init = s.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "standalone-test", "version": "0"}}),
        );
        assert!(init.get("result").is_some(), "initialize failed: {init}");
        s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        s
    }

    fn send(&mut self, v: &Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self
                .rx
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
        }
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": args}))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text_of(resp: &Value) -> String {
    resp["result"]["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// A tool counts as callable when the server runs it: no JSON-RPC refusal
/// (scope, external availability) and no "acts for another agent" failure.
/// A tool error about missing arguments is a run, not a refusal.
fn refusal(resp: &Value) -> Option<String> {
    if let Some(err) = resp.get("error") {
        return Some(err.to_string());
    }
    let text = text_of(resp);
    if text.contains("unknown agent") {
        return Some(text);
    }
    None
}

#[test]
fn standalone_init_key_lists_only_callable_tools_and_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = home.join(".duduclaw");
    let cwd = tmp.path().join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();

    let token = init_token(&home, &data);
    assert!(token.starts_with("ddc_refresh_"), "{token}");
    assert!(data.join("mcp_tokens.db").is_file());

    let mut server = Server::start(&home, &data, &token, &cwd);
    let list = server.request("tools/list", json!({}));
    let names: Vec<String> = list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.len() >= 20, "standalone listing collapsed: {names:?}");
    for must in ["memory_store", "memory_search", "wiki_write", "wiki_read"] {
        assert!(names.iter().any(|n| n == must), "{must} missing: {names:?}");
    }
    for hidden in [
        "working_state_set",
        "tasks_list",
        "send_message",
        "canvas_push",
    ] {
        assert!(
            !names.iter().any(|n| n == hidden),
            "{hidden} listed: {names:?}"
        );
    }

    // Every listed tool is callable (empty arguments: missing-argument tool
    // errors are fine, refusals are not).
    let mut refused = Vec::new();
    for name in &names {
        if let Some(why) = refusal(&server.call(name, json!({}))) {
            refused.push(format!("{name}: {why}"));
        }
    }
    let callable = names.len() - refused.len();
    assert_eq!(
        names.len(),
        callable,
        "listed tools that the server refused: {refused:#?}"
    );
    eprintln!("standalone tools/list: {} tools: {names:?}", names.len());

    // Unlisted tools stay refused by the dispatch gate, including the ones
    // the key's scopes would reach but that act for the process's employee,
    // and shared-wiki writes (made as that employee).
    let denied = server.call("tasks_list", json!({}));
    assert!(
        denied.get("error").is_some(),
        "tasks_list must stay refused: {denied}"
    );
    for (tool, args) in [
        ("working_state_get", json!({})),
        ("memory_search_by_layer", json!({"query": "x"})),
        (
            "wiki_write",
            json!({"scope": "shared", "page_path": "x.md", "content": "# x"}),
        ),
    ] {
        let denied = server.call(tool, args);
        assert_eq!(denied["error"]["code"], json!(-32003), "{tool}: {denied}");
    }
    assert!(!data.join("shared/wiki/x.md").exists());

    // Memory round trip.
    let stored = server.call(
        "memory_store",
        json!({"content": "The staging database lives in asia-east1"}),
    );
    assert!(refusal(&stored).is_none(), "{stored}");
    let stored_text = text_of(&stored);
    assert!(
        stored_text.contains("external/standalone-print"),
        "memory must land in the caller's own namespace: {stored_text}"
    );
    let found = server.call("memory_search", json!({"query": "staging database"}));
    assert!(
        text_of(&found).contains("asia-east1"),
        "memory_search did not find the stored row: {found}"
    );

    // Wiki round trip, in the caller's own wiki.
    let wrote = server.call(
        "wiki_write",
        json!({"page_path": "notes/deploy.md", "content": "# Deploy\n\nRegion asia-east1."}),
    );
    assert!(
        text_of(&wrote).contains("notes/deploy.md") && wrote["result"]["isError"] != json!(true),
        "{wrote}"
    );
    let read = server.call("wiki_read", json!({"page_path": "notes/deploy.md"}));
    assert!(text_of(&read).contains("Region asia-east1."), "{read}");
    assert!(
        data.join("agents/standalone-print/wiki/notes/deploy.md")
            .is_file(),
        "the page must be in the caller's own wiki directory"
    );
    assert!(
        !data.join("agents/dudu").exists(),
        "nothing may be written for the default agent"
    );
}

#[test]
fn mcp_server_without_key_names_the_init_command() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = home.join(".duduclaw");
    std::fs::create_dir_all(&data).unwrap();
    let out = base_command(&home, &data)
        .arg("mcp-server")
        .stdin(Stdio::null())
        .output()
        .expect("run mcp-server");
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        all.contains("Run: duduclaw mcp init --client claude-code"),
        "missing hint: {all}"
    );
}
