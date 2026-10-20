//! Tests for the Antigravity third-party MCP registration (2026-10-20).

use super::*;
use crate::runtime::antigravity_setup as setup;
use serde_json::json;
use std::path::PathBuf;

fn employee(mcp_json: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("trader");
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(body) = mcp_json {
        std::fs::write(dir.join(".mcp.json"), body).unwrap();
    }
    (home, dir)
}

fn broker_mcp_json() -> String {
    json!({ "mcpServers": {
        "duduclaw": { "command": "duduclaw", "args": ["mcp-server"] },
        "masterlink": {
            "command": "python3",
            "args": ["-m", "masterlink_mcp"],
            "env": { "ML_PASSWORD": "broker-secret-pw" }
        },
        "web": { "url": "https://example.com/mcp" },
        "bad name": { "command": "npx", "args": [] }
    }})
    .to_string()
}

#[tokio::test]
async fn no_mcp_json_or_only_duduclaw_registers_nothing() {
    let only_own =
        json!({ "mcpServers": { "duduclaw": { "command": "duduclaw", "args": [] } } }).to_string();
    for body in [None, Some(only_own)] {
        let (home, dir) = employee(body.as_deref());
        let before = std::fs::read(dir.join(".mcp.json")).ok();
        let p = prepare(Some(&dir), "trader", home.path()).await.unwrap();
        assert!(p.servers.is_empty() && p.env_dir.is_none(), "{body:?}");
        assert!(p.entries().is_empty() && p.names().is_empty());
        assert_eq!(
            std::fs::read(dir.join(".mcp.json")).ok(),
            before,
            "no gate ran"
        );
        assert!(!home.path().join("tmp").exists(), "no env directory");
    }
}

#[tokio::test]
async fn a_third_party_server_gets_an_env_free_entry_and_its_own_env_dir() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    let p = prepare(Some(&dir), "trader", home.path()).await.unwrap();
    assert_eq!(
        p.names(),
        vec!["masterlink"],
        "URL entry and bad name dropped"
    );
    let entries = p.entries();
    let (name, entry) = &entries[0];
    assert_eq!(name, "masterlink");
    assert!(
        entry.get("env").is_none(),
        "nothing per spawn or secret in the workspace file"
    );
    assert_eq!(entry["args"][0], "mcp-proxy");
    assert_eq!(entry["args"][1], "--spawn-scoped");
    assert!(!entry.to_string().contains("broker-secret-pw"));
    let env_dir = p.env_dir.as_ref().expect("this spawn's env directory");
    let body = std::fs::read_to_string(env_dir.path().join("masterlink.json")).unwrap();
    assert!(body.contains("broker-secret-pw"));
    assert!(p.self_exe.is_some());
}

#[tokio::test]
async fn a_malformed_mcp_json_refuses_the_spawn() {
    for body in ["{ not json", "[1, 2]", r#"{"mcpServers": 3}"#] {
        let (home, dir) = employee(Some(body));
        let err = prepare(Some(&dir), "trader", home.path())
            .await
            .expect_err(body);
        assert!(
            duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
            "{body}: {err}"
        );
    }
}

#[tokio::test]
async fn a_utility_call_gets_nothing_and_reads_nothing() {
    for body in [broker_mcp_json(), "{ not json".to_string()] {
        let (home, dir) = employee(Some(body.as_str()));
        let p = crate::runtime_dispatch::UTILITY_CALL
            .scope((), prepare(Some(&dir), "trader", home.path()))
            .await
            .unwrap();
        assert!(p.servers.is_empty() && p.env_dir.is_none() && p.self_exe.is_none());
    }
}

#[tokio::test]
async fn concurrent_spawns_get_distinct_env_dirs() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    let (a, b) = tokio::join!(
        prepare(Some(&dir), "trader", home.path()),
        prepare(Some(&dir), "trader", home.path())
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.entries(), b.entries(), "same workspace file content");
    let (da, db) = (a.env_dir.unwrap(), b.env_dir.unwrap());
    assert_ne!(da.path(), db.path());
}

#[test]
fn grants_cover_each_registered_server_and_nothing_else() {
    let home = tempfile::tempdir().unwrap();
    let grants = setup::tool_grants_for(
        home.path(),
        &[
            "masterlink".to_string(),
            "bad name".to_string(),
            "duduclaw".to_string(),
        ],
    );
    assert!(grants.contains(&"mcp(duduclaw/*)".to_string()));
    assert!(grants.contains(&"mcp(masterlink/*)".to_string()));
    // Built with `Path::join` so the separator matches the platform (`\` on Windows).
    let schema_dir = home
        .path()
        .join(".gemini")
        .join("antigravity-cli")
        .join("mcp")
        .join("masterlink");
    assert!(grants.contains(&format!("read_file({})", schema_dir.to_string_lossy())));
    assert!(!grants.iter().any(|g| g.contains("bad name")));
    assert_eq!(grants.iter().filter(|g| *g == "mcp(duduclaw/*)").count(), 1);
    // No third-party server ⇒ exactly the old rules.
    assert_eq!(
        setup::tool_grants_for(home.path(), &[]),
        setup::tool_grants(home.path())
    );
}

#[test]
fn stale_managed_entries_are_pruned_and_others_kept() {
    let exe = Path::new("/opt/duduclaw");
    let existing = json!({ "mcpServers": {
        "duduclaw": { "command": "/opt/duduclaw", "args": ["mcp-server"] },
        "gone": { "command": "/opt/duduclaw", "args": ["mcp-proxy", "--spawn-scoped", "--server", "gone", "--", "x"] },
        "handmade": { "command": "/usr/bin/true", "args": [] }
    }})
    .to_string();
    let servers = vec![(
        "duduclaw".to_string(),
        json!({ "command": "/opt/duduclaw", "args": ["mcp-server"] }),
    )];
    let out = setup::merge_mcp_config_pruning(Some(&existing), &servers, Some(exe))
        .unwrap()
        .expect("changed");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["mcpServers"].get("gone").is_none());
    assert!(v["mcpServers"].get("handmade").is_some());
    // Without pruning (a utility call, a borrowed root) nothing is removed.
    assert!(
        setup::merge_mcp_config_pruning(Some(&existing), &servers, None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unmanaged_entries_are_listed_for_the_warning() {
    let exe = Path::new("/opt/duduclaw");
    let existing = json!({ "mcpServers": {
        "duduclaw": { "command": "/opt/duduclaw", "args": ["mcp-server"] },
        "ours": { "command": "/opt/duduclaw", "args": ["mcp-proxy", "--spawn-scoped", "--server", "ours", "--", "x"] },
        "handmade": { "command": "npx", "args": [] }
    }})
    .to_string();
    assert_eq!(
        setup::unmanaged_mcp_entries(Some(&existing), exe),
        vec!["handmade"]
    );
    assert!(setup::unmanaged_mcp_entries(None, exe).is_empty());
    assert!(setup::unmanaged_mcp_entries(Some("{ broken"), exe).is_empty());
}

// ── Live: the real agy (temp HOME, fake key, stub upstream) ─────────────
//
// Needs a `duduclaw` binary built from this tree (the proxy) in
// `DUDUCLAW_BIN` and python3; the developer's real `~/.gemini` and Google
// sign-in are never read (test-only HOME hook) and the key is fake, so no
// model call can succeed. Run with:
//   DUDUCLAW_BIN=<built duduclaw> cargo test -p duduclaw-gateway --lib \
//     e2e_agy_third_party -- --ignored --nocapture --test-threads=1

const FAKE_KEY: &str = concat!("AI", "zaSyDUDUCLAW-e2e-fake-key-000000000000");

fn stub_upstream(dir: &Path, marker: &Path) -> PathBuf {
    let script = dir.join("stub_upstream.py");
    std::fs::write(
        &script,
        format!(
            r#"import json, os, sys
with open({marker:?}, "a") as f:
    f.write(json.dumps({{k: v for k, v in os.environ.items() if k.startswith(("ML_", "DUDUCLAW"))}}) + "\n")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if "id" not in msg:
        continue
    m = msg.get("method")
    if m == "initialize":
        r = {{"protocolVersion": msg.get("params", {{}}).get("protocolVersion", "2024-11-05"),
             "capabilities": {{"tools": {{}}}}, "serverInfo": {{"name": "stub", "version": "0"}}}}
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": r}}
    elif m == "tools/list":
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"tools": []}}}}
    else:
        out = {{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -32601, "message": "no"}}}}
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
"#,
            marker = marker.to_string_lossy()
        ),
    )
    .unwrap();
    script
}

#[tokio::test]
#[ignore = "drives the real agy and a built duduclaw (temp HOME, fake key, stub upstream)"]
async fn e2e_agy_third_party_server_runs_behind_the_proxy_only_in_its_spawn() {
    use crate::runtime::AgentRuntime;
    let (Some(agy), Ok(bin)) = (duduclaw_core::which_agy(), std::env::var("DUDUCLAW_BIN")) else {
        eprintln!("agy or DUDUCLAW_BIN missing — skipping");
        return;
    };
    let python = [
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python3",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .expect("python3");
    let tmp = tempfile::tempdir().unwrap();
    let user_home = tmp.path().join("user-home");
    let home = tmp.path().join("duduclaw-home");
    let dir = home.join("agents").join("trader");
    for d in [&user_home, &dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(
        home.join("config.toml"),
        "[antigravity]\nauth = \"api_key\"\n",
    )
    .unwrap();
    let marker = tmp.path().join("upstream-started.jsonl");
    let script = stub_upstream(tmp.path(), &marker);
    std::fs::write(
        dir.join(".mcp.json"),
        json!({ "mcpServers": {
            "duduclaw": { "command": bin, "args": ["mcp-server"] },
            "masterlink": {
                "command": python.to_string_lossy(),
                "args": [script.to_string_lossy()],
                "env": { "ML_PASSWORD": "broker-secret-pw" }
            }
        }})
        .to_string(),
    )
    .unwrap();
    let rt = super::super::AntigravityRuntime {
        agy_path: agy,
        hooks: super::super::TestHooks {
            user_home: Some(user_home.clone()),
            gemini_key: Some(FAKE_KEY.to_string()),
            mcp_command: Some((PathBuf::from("/usr/bin/true"), vec![])),
        },
    };
    let ctx = crate::runtime::RuntimeContext {
        agent_dir: Some(dir.clone()),
        system_prompt: String::new(),
        model: String::new(),
        max_tokens: 64,
        home_dir: home.clone(),
        agent_id: "trader".into(),
        preferred_provider: None,
        conversation_history: Vec::new(),
        capabilities: None,
        account_pool: Vec::new(),
        effort: None,
        allow_cross_family_failover: false,
    };

    let err = rt.execute("Reply PONG", &ctx).await.expect_err("fake key");
    eprintln!("agy failure text: {err}");
    let cfg = std::fs::read_to_string(dir.join(".agents").join("mcp_config.json")).unwrap();
    eprintln!("mcp_config.json: {cfg}");
    assert!(!cfg.contains("broker-secret-pw"), "{cfg}");
    let started = std::fs::read_to_string(&marker).unwrap_or_default();
    eprintln!("upstream env seen: {started}");
    let first: Value =
        serde_json::from_str(started.lines().next().expect("upstream started")).unwrap();
    assert_eq!(
        first["ML_PASSWORD"], "broker-secret-pw",
        "declared env reached the upstream"
    );
    assert!(first.get("DUDUCLAW_AGENT_TOKEN").is_none());
    assert!(first.get("DUDUCLAW_MCP_PROXY_ENV_DIR").is_none());
    let settings = std::fs::read_to_string(setup::user_settings_path(&user_home)).unwrap();
    assert!(settings.contains("mcp(masterlink/*)"), "{settings}");
    // The spawn's env directory is gone once agy exited.
    let left: Vec<_> = std::fs::read_dir(home.join("tmp").join("mcp-proxy-env"))
        .map(|r| r.flatten().collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");

    // A utility call: the workspace file still holds the entry, but its
    // proxy finds no env directory and starts nothing.
    let starts = started.lines().count();
    let _ = crate::runtime_dispatch::UTILITY_CALL
        .scope((), rt.execute("Reply PONG", &ctx))
        .await;
    let cfg = std::fs::read_to_string(dir.join(".agents").join("mcp_config.json")).unwrap();
    assert!(cfg.contains("masterlink"), "{cfg}");
    let after = std::fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(
        after.lines().count(),
        starts,
        "a utility call must not start the upstream"
    );
}
