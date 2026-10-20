//! Tests for the Grok third-party MCP registration (2026-10-20).

use super::*;
use serde_json::json;

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
        }
    }})
    .to_string()
}

fn config(dir: &Path) -> (String, toml::Value) {
    let raw = std::fs::read_to_string(dir.join(".grok").join("config.toml")).unwrap();
    let parsed = raw
        .parse::<toml::Value>()
        .unwrap_or_else(|e| panic!("config.toml must parse: {e}\n{raw}"));
    (raw, parsed)
}

async fn prepare_for(dir: &Path, home: &Path) -> Result<Prepared, String> {
    prepare(Some(dir), Some(dir), "trader", home, false).await
}

#[tokio::test]
async fn no_mcp_json_or_only_duduclaw_is_the_old_spawn() {
    let only_own =
        json!({ "mcpServers": { "duduclaw": { "command": "duduclaw", "args": [] } } }).to_string();
    for body in [None, Some(only_own), Some("{}".to_string())] {
        let (home, dir) = employee(body.as_deref());
        let before = std::fs::read(dir.join(".mcp.json")).ok();
        let p = prepare_for(&dir, home.path()).await.unwrap();
        assert!(!p.managed && p.env_dir.is_none(), "{body:?}");
        assert!(
            !dir.join(".grok").exists(),
            "prepare wrote nothing: {body:?}"
        );
        assert_eq!(
            std::fs::read(dir.join(".mcp.json")).ok(),
            before,
            "no gate ran: {body:?}"
        );
    }
}

#[tokio::test]
async fn a_third_party_server_is_registered_behind_a_spawn_scoped_proxy() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    let p = prepare_for(&dir, home.path()).await.unwrap();
    assert!(p.managed);
    let (raw, cfg) = config(&dir);
    let servers = cfg["mcp_servers"].as_table().unwrap();
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();
    assert_eq!(names, vec!["duduclaw", "masterlink"]);
    let ml = &servers["masterlink"];
    let args: Vec<&str> = ml["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        args,
        vec![
            "mcp-proxy",
            "--spawn-scoped",
            "--server",
            "masterlink",
            "--",
            "python3",
            "-m",
            "masterlink_mcp"
        ]
    );
    assert_eq!(ml["enabled"].as_bool(), Some(true));
    let exe = duduclaw_core::resolve_duduclaw_bin();
    assert_eq!(ml["command"].as_str(), Some(exe.to_string_lossy().as_ref()));
    assert_eq!(
        ml["env"][PROXY_UPSTREAM_ENV_DIR_VAR].as_str(),
        Some("${DUDUCLAW_MCP_PROXY_ENV_DIR:-}"),
        "Grok expands the per-spawn directory from its own environment"
    );
    assert_eq!(ml["env"]["DUDUCLAW_AGENT_ID"].as_str(), Some("trader"));
    assert!(ml["env"].get("DUDUCLAW_AGENT_TOKEN").is_none());
    // The persistent file never carries the upstream credentials.
    assert!(!raw.contains("broker-secret-pw"), "{raw}");
    assert!(!raw.contains("DUDUCLAW_MCP_PROXY_ENV\""), "{raw}");
    // They are in this spawn's own directory instead.
    let env_dir = p.env_dir.expect("an env directory for this spawn");
    let body = std::fs::read_to_string(env_dir.path().join("masterlink.json")).unwrap();
    assert!(body.contains("broker-secret-pw"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join(".grok").join("config.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[tokio::test]
async fn ungateable_names_are_shadowed_disabled_never_live() {
    let body = json!({ "mcpServers": {
        "duduclaw": { "command": "duduclaw", "args": [] },
        "duduclaw-pro": { "command": "/tmp/evil", "args": [] },
        "remote_erp": { "type": "http", "url": "https://erp.example.com/mcp" },
        "bad name": { "command": "npx", "args": [] },
        "no_command": { "args": ["x"] },
        "masterlink": { "command": "python3", "args": ["-m", "m"] }
    }})
    .to_string();
    let (home, dir) = employee(Some(body.as_str()));
    prepare_for(&dir, home.path()).await.unwrap();
    let (raw, cfg) = config(&dir);
    let servers = cfg["mcp_servers"].as_table().unwrap();
    for shadowed in ["duduclaw-pro", "remote_erp", "bad name", "no_command"] {
        let e = &servers[shadowed];
        assert_eq!(e["enabled"].as_bool(), Some(false), "{shadowed}");
        assert!(
            e.get("url").is_none() && e.get("env").is_none(),
            "{shadowed}"
        );
    }
    assert_eq!(servers["masterlink"]["enabled"].as_bool(), Some(true));
    assert!(
        !raw.contains("/tmp/evil") && !raw.contains("erp.example.com"),
        "{raw}"
    );
    // `duduclaw` stays DuDuClaw's own server.
    assert_eq!(servers["duduclaw"]["args"][0].as_str(), Some("mcp-server"));
}

/// A `.mcp.json` that lists only DuDuClaw-named extras (`duduclaw-pro`)
/// is still shadowed: Grok would otherwise start that entry itself.
#[tokio::test]
async fn duduclaw_named_extras_alone_are_shadowed() {
    let body = json!({ "mcpServers": {
        "duduclaw": { "command": "duduclaw", "args": [] },
        "duduclaw-pro": { "command": "/tmp/evil", "args": [] }
    }})
    .to_string();
    let (home, dir) = employee(Some(body.as_str()));
    let p = prepare_for(&dir, home.path()).await.unwrap();
    assert!(p.managed && p.env_dir.is_none());
    let (raw, cfg) = config(&dir);
    assert_eq!(
        cfg["mcp_servers"]["duduclaw-pro"]["enabled"].as_bool(),
        Some(false)
    );
    assert!(!raw.contains("/tmp/evil"), "{raw}");
}

/// An env directory that cannot be written refuses the spawn: a server
/// must never start without its credentials, nor Grok without its shadows.
#[tokio::test]
async fn an_env_dir_that_cannot_be_written_refuses_the_spawn() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    std::fs::create_dir_all(home.path().join("tmp")).unwrap();
    std::fs::write(home.path().join("tmp").join("mcp-proxy-env"), "not a dir").unwrap();
    let err = prepare_for(&dir, home.path()).await.unwrap_err();
    assert!(
        duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
        "{err}"
    );
}

/// `Foo.json` and `foo.json` are one file on a case-insensitive disk.
#[tokio::test]
async fn names_differing_only_in_case_refuse_the_spawn() {
    let body = json!({ "mcpServers": {
        "Broker": { "command": "python3", "args": ["-m", "a"], "env": { "K": "a" } },
        "broker": { "command": "python3", "args": ["-m", "b"], "env": { "K": "b" } }
    }})
    .to_string();
    let (home, dir) = employee(Some(body.as_str()));
    let err = prepare_for(&dir, home.path()).await.unwrap_err();
    assert!(
        duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
        "{err}"
    );
    assert!(err.contains("letter case"), "{err}");
}

/// Grok also loads `<cwd>/.cursor/mcp.json` by itself: its names are
/// shadowed even when `.mcp.json` lists nothing but DuDuClaw.
#[tokio::test]
async fn cursor_mcp_json_names_are_shadowed() {
    let only_own =
        json!({ "mcpServers": { "duduclaw": { "command": "duduclaw", "args": [] } } }).to_string();
    let (home, dir) = employee(Some(only_own.as_str()));
    std::fs::create_dir_all(dir.join(".cursor")).unwrap();
    std::fs::write(
        dir.join(".cursor").join("mcp.json"),
        json!({ "mcpServers": { "cursor_srv": { "command": "/tmp/evil", "args": [] } } })
            .to_string(),
    )
    .unwrap();
    let p = prepare_for(&dir, home.path()).await.unwrap();
    assert!(p.managed);
    let (raw, cfg) = config(&dir);
    assert_eq!(
        cfg["mcp_servers"]["cursor_srv"]["enabled"].as_bool(),
        Some(false)
    );
    assert!(!raw.contains("/tmp/evil"));

    std::fs::write(dir.join(".cursor").join("mcp.json"), "{ broken").unwrap();
    let err = prepare_for(&dir, home.path()).await.unwrap_err();
    assert!(
        duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
        "{err}"
    );
}

/// Inside a git repository Grok reads `.mcp.json` and `.grok/config.toml`
/// of every directory up to the root; those names are shadowed in the
/// working directory's config. Outside one it reads the working directory
/// only.
#[tokio::test]
async fn ancestor_sources_up_to_the_git_root_are_shadowed() {
    let (home, dir) = employee(None);
    let agents = dir.parent().unwrap();
    std::fs::write(
        agents.join(".mcp.json"),
        json!({ "mcpServers": { "up_one": { "command": "npx", "args": [] } } }).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join(".grok")).unwrap();
    std::fs::write(
        home.path().join(".grok").join("config.toml"),
        "[mcp_servers.up_two]\ncommand = \"npx\"\nargs = []\n",
    )
    .unwrap();
    // No repository: nothing above the working directory is read.
    assert!(sources::ancestors_to_git_root(&dir).is_empty());
    let p = prepare_for(&dir, home.path()).await.unwrap();
    assert!(!p.managed);

    std::fs::create_dir_all(home.path().join(".git")).unwrap();
    let ancestors = sources::ancestors_to_git_root(&dir);
    assert_eq!(ancestors.len(), 2, "{ancestors:?}");
    let p = prepare_for(&dir, home.path()).await.unwrap();
    assert!(p.managed);
    let (_, cfg) = config(&dir);
    assert_eq!(
        cfg["mcp_servers"]["up_one"]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(
        cfg["mcp_servers"]["up_two"]["enabled"].as_bool(),
        Some(false)
    );
}

#[test]
fn unmanaged_entries_are_listed_for_the_warning() {
    let exe = Path::new("/opt/duduclaw");
    let doc: toml::Value = toml::from_str(&format!(
        "[mcp_servers.duduclaw]\ncommand = \"/opt/duduclaw\"\nargs = [\"mcp-server\"]\n\
         [mcp_servers.handmade]\ncommand = \"npx\"\nargs = []\n\
         [mcp_servers.shadowed]\n{}",
        toml::to_string(&shadow_entry(exe)).unwrap()
    ))
    .unwrap();
    let map = doc["mcp_servers"].as_table().unwrap();
    assert_eq!(unmanaged_names(map, exe), vec!["handmade"]);
}

#[tokio::test]
async fn a_malformed_mcp_json_refuses_the_spawn() {
    for body in ["{ not json", "[1, 2]", r#"{"mcpServers": 3}"#] {
        let (home, dir) = employee(Some(body));
        let err = prepare_for(&dir, home.path()).await.expect_err(body);
        assert!(
            duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
            "{body}: {err}"
        );
        assert!(!dir.join(".grok").exists());
    }
}

/// A utility call writes the same file (so it never clobbers a concurrent
/// turn) but gets no env directory: every proxy then starts nothing.
#[tokio::test]
async fn a_utility_call_gets_no_env_dir_and_the_same_config() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    let normal = prepare_for(&dir, home.path()).await.unwrap();
    let (first, _) = config(&dir);
    let utility = crate::runtime_dispatch::UTILITY_CALL
        .scope((), prepare_for(&dir, home.path()))
        .await
        .unwrap();
    assert!(utility.managed && utility.env_dir.is_none());
    assert_eq!(config(&dir).0, first, "identical persistent content");
    drop(normal);
}

#[tokio::test]
async fn concurrent_spawns_keep_their_own_env_dirs() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    let (a, b) = tokio::join!(
        prepare_for(&dir, home.path()),
        prepare_for(&dir, home.path())
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let (da, db) = (a.env_dir.unwrap(), b.env_dir.unwrap());
    assert_ne!(da.path(), db.path());
    drop(da);
    assert!(db.path().join("masterlink.json").is_file());
}

#[tokio::test]
async fn a_server_removed_from_mcp_json_is_pruned_from_its_own_dir() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    prepare_for(&dir, home.path()).await.unwrap();
    // An operator's own hand-written entry is never touched.
    let path = dir.join(".grok").join("config.toml");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("\n[mcp_servers.handmade]\ncommand = \"/usr/bin/true\"\nargs = []\n");
    std::fs::write(&path, raw).unwrap();
    // masterlink renamed to broker2.
    let renamed = broker_mcp_json().replace("masterlink\":", "broker2\":");
    std::fs::write(dir.join(".mcp.json"), renamed).unwrap();
    prepare_for(&dir, home.path()).await.unwrap();
    let (_, cfg) = config(&dir);
    let servers = cfg["mcp_servers"].as_table().unwrap();
    assert!(servers.get("masterlink").is_none(), "{servers:?}");
    assert!(servers.contains_key("broker2"));
    assert!(servers.contains_key("handmade"));
}

/// A borrowed work root (a role member in the employee's workspace): its own
/// `.mcp.json` names are shadowed where nothing defines them yet; entries
/// written for its owner are left alone.
#[tokio::test]
async fn a_borrowed_work_root_is_shadowed_but_not_pruned() {
    let (home, dir) = employee(None);
    let root = home.path().join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(".mcp.json"),
        json!({ "mcpServers": { "owner_srv": { "command": "npx", "args": [] } } }).to_string(),
    )
    .unwrap();
    let exe = duduclaw_core::resolve_duduclaw_bin();
    let owners_live = format!(
        "[mcp_servers.owners]\ncommand = {:?}\nargs = [\"mcp-proxy\", \"--spawn-scoped\", \"--server\", \"owners\", \"--\", \"x\"]\nenabled = true\n",
        exe.to_string_lossy()
    );
    std::fs::create_dir_all(root.join(".grok")).unwrap();
    std::fs::write(root.join(".grok").join("config.toml"), owners_live).unwrap();

    let p = prepare(Some(&dir), Some(&root), "trader", home.path(), false)
        .await
        .unwrap();
    assert!(p.managed);
    assert!(p.env_dir.is_none(), "the member itself registers nothing");
    let (_, cfg) = config(&root);
    let servers = cfg["mcp_servers"].as_table().unwrap();
    assert_eq!(servers["owner_srv"]["enabled"].as_bool(), Some(false));
    assert_eq!(
        servers["owners"]["enabled"].as_bool(),
        Some(true),
        "not pruned"
    );
    assert!(servers.contains_key("duduclaw"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_dot_grok_is_refused() {
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join(".grok")).unwrap();
    let err = prepare_for(&dir, home.path()).await.unwrap_err();
    assert!(
        duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
        "{err}"
    );
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn shadow_and_live_entries_are_recognised_as_managed() {
    let exe = Path::new("/opt/duduclaw");
    assert!(is_managed(&shadow_entry(exe), exe));
    let s = ScopedServer {
        command: "/opt/duduclaw".into(),
        args: vec![
            "mcp-proxy".into(),
            "--spawn-scoped".into(),
            "--server".into(),
            "x".into(),
        ],
        proxy_env: BTreeMap::new(),
        upstream_env: "{}".into(),
    };
    assert!(is_managed(&live_entry(&s), exe));
    let handmade: toml::Value = toml::from_str("command = \"/usr/bin/true\"\nargs = []").unwrap();
    assert!(!is_managed(&handmade, exe));
}

/// The explore lane never reaches this module: the runtime refuses first and
/// writes nothing.
#[tokio::test]
async fn the_explore_lane_is_refused_before_anything_is_written() {
    use crate::runtime::AgentRuntime;
    let (home, dir) = employee(Some(broker_mcp_json().as_str()));
    let ctx = crate::runtime::RuntimeContext {
        agent_dir: Some(dir.clone()),
        system_prompt: String::new(),
        model: String::new(),
        max_tokens: 64,
        home_dir: home.path().to_path_buf(),
        agent_id: "trader".into(),
        preferred_provider: None,
        conversation_history: Vec::new(),
        capabilities: None,
        account_pool: Vec::new(),
        effort: None,
        allow_cross_family_failover: false,
    };
    let rt = crate::runtime::grok::GrokRuntime::new();
    let err = crate::explore_lane::EXPLORE
        .scope(true, rt.execute("hi", &ctx))
        .await
        .unwrap_err();
    assert!(err.contains("explore_lane_unsupported"), "{err}");
    assert!(!dir.join(".grok").exists());
}

// ── Live: the real grok's MCP loader (`grok mcp doctor`) ─────────────────
//
// `grok -p` needs a signed-in Grok, so this drives `grok mcp doctor`, which
// loads the same project config and `.mcp.json` and starts every enabled
// server, under a temporary HOME (never the developer's `~/.grok`). Needs a
// `duduclaw` built from this tree in `DUDUCLAW_BIN` and python3. Run with:
//   DUDUCLAW_BIN=<built duduclaw> cargo test -p duduclaw-gateway --lib \
//     live_grok_doctor -- --ignored --nocapture --test-threads=1

fn stub(dir: &Path, marker: &Path) -> PathBuf {
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
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"tools": [{{"name": "ping", "description": "p", "inputSchema": {{"type": "object"}}}}]}}}}
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
#[ignore = "drives the real grok's MCP loader and a built duduclaw (temp HOME, no sign-in)"]
async fn live_grok_doctor_starts_only_gated_servers_and_only_in_their_spawn() {
    let (Some(grok), Ok(bin)) = (duduclaw_core::which_grok(), std::env::var("DUDUCLAW_BIN")) else {
        eprintln!("grok or DUDUCLAW_BIN missing — skipping");
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
    std::fs::create_dir_all(&user_home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let ml_marker = tmp.path().join("masterlink.jsonl");
    let bad_marker = tmp.path().join("bad.jsonl");
    let ml = stub(tmp.path(), &ml_marker);
    let bad_dir = tmp.path().join("bad");
    std::fs::create_dir_all(&bad_dir).unwrap();
    let bad = stub(&bad_dir, &bad_marker);
    std::fs::write(
        dir.join(".mcp.json"),
        json!({ "mcpServers": {
            "duduclaw": { "command": bin, "args": ["mcp-server"] },
            "masterlink": { "command": python.to_string_lossy(), "args": [ml.to_string_lossy()],
                            "env": { "ML_PASSWORD": "broker-secret-pw" } },
            "bad name": { "command": python.to_string_lossy(), "args": [bad.to_string_lossy()] },
            "web": { "url": "https://example.invalid/mcp" }
        }})
        .to_string(),
    )
    .unwrap();

    let p = prepare_for(&dir, &home).await.unwrap();
    let env_dir = p.env_dir.as_ref().expect("env dir");
    eprintln!(
        "{}",
        std::fs::read_to_string(dir.join(".grok").join("config.toml")).unwrap()
    );

    let doctor = |with_dir: bool| {
        let mut cmd = std::process::Command::new(&grok);
        cmd.args(["mcp", "doctor", "--leader-socket"])
            .arg(user_home.join(".grok").join("leader-test.sock"))
            .current_dir(&dir)
            .env("HOME", &user_home)
            .env("GROK_FOLDER_TRUST", "0")
            .env_remove("XAI_API_KEY")
            .env_remove(PROXY_UPSTREAM_ENV_DIR_VAR);
        if with_dir {
            cmd.env(PROXY_UPSTREAM_ENV_DIR_VAR, env_dir.path());
        }
        let out = cmd.output().expect("grok mcp doctor");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        eprintln!("--- doctor (env dir: {with_dir}) ---\n{text}");
        text
    };

    let text = doctor(true);
    assert!(
        text.contains(".mcp.json") && text.contains("0 servers"),
        "every .mcp.json name is overridden"
    );
    let seen = std::fs::read_to_string(&ml_marker).unwrap_or_default();
    let first: serde_json::Value =
        serde_json::from_str(seen.lines().next().expect("masterlink started")).unwrap();
    assert_eq!(first["ML_PASSWORD"], "broker-secret-pw");
    assert!(first.get("DUDUCLAW_AGENT_TOKEN").is_none());
    assert!(first.get(PROXY_UPSTREAM_ENV_DIR_VAR).is_none());
    assert!(
        !bad_marker.exists(),
        "an ungateable server is never started"
    );

    let starts = seen.lines().count();
    doctor(false);
    let after = std::fs::read_to_string(&ml_marker).unwrap_or_default();
    assert_eq!(
        after.lines().count(),
        starts,
        "without its spawn's env dir the proxy starts nothing"
    );
    assert!(!bad_marker.exists());
}
