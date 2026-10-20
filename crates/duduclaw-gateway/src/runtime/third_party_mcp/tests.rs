//! Tests for the runtime-agnostic third-party MCP half (2026-10-20). The
//! Codex-specific coverage stays in `codex/third_party/tests.rs`.

use super::env_files::{
    ENV_FILE_DIR, ENV_FILE_PREFIX, env_file_dir, sweep_env_files_older_than, write_spawn_env_dir,
};
use super::*;
use duduclaw_core::mcp_proxy_rewrite::{PROXY_UPSTREAM_ENV_DIR_VAR, PROXY_UPSTREAM_ENV_VAR};
use serde_json::json;

const EXE: &str = "/opt/duduclaw/bin/duduclaw";

fn employee(mcp_json: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("trader");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".mcp.json"), mcp_json).unwrap();
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
            "DUDUCLAW_MCP_API_KEY": "ddc_dev_key",
        }
    })
}

fn broker_and_bridge() -> String {
    json!({ "mcpServers": {
        "duduclaw": { "command": "duduclaw", "args": ["mcp-server"] },
        "masterlink": {
            "command": "python3",
            "args": ["-m", "masterlink_mcp"],
            "env": { "ML_PASSWORD": "broker-secret-pw" }
        },
        "zap": { "command": EXE, "args": ["mcp-remote-bridge", "--agent", "trader", "--server", "zap"],
                 "env": { "DUDUCLAW_HOME": "/h" } },
        "web": { "url": "https://example.com/mcp" }
    }})
    .to_string()
}

fn scoped_servers(home: &Path, dir: &Path) -> BTreeMap<String, ScopedServer> {
    let gated = gated_servers(dir, home, &own_server(home), false).unwrap();
    spawn_scoped(&gated)
}

#[test]
fn a_proxied_server_becomes_spawn_scoped_without_its_credentials() {
    let (home, dir) = employee(&broker_and_bridge());
    let scoped = scoped_servers(home.path(), &dir);
    let names: Vec<&String> = scoped.keys().collect();
    assert_eq!(names, vec!["masterlink", "zap"], "the URL entry is dropped");

    let ml = &scoped["masterlink"];
    assert_eq!(ml.command, EXE);
    assert_eq!(
        ml.args,
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
    // Only the proxy's own names, never the upstream env or the identity MAC.
    assert_eq!(
        ml.proxy_env.get("DUDUCLAW_AGENT_ID").map(String::as_str),
        Some("trader")
    );
    assert!(!ml.proxy_env.contains_key("DUDUCLAW_AGENT_TOKEN"));
    assert!(!ml.proxy_env.contains_key(PROXY_UPSTREAM_ENV_VAR));
    assert!(
        !ml.proxy_env
            .values()
            .any(|v| v.contains("broker-secret-pw"))
    );
    let upstream: Value = serde_json::from_str(&ml.upstream_env).unwrap();
    assert_eq!(upstream["ML_PASSWORD"], "broker-secret-pw");
}

#[test]
fn a_bridge_is_wrapped_and_gets_its_env_through_the_spawn_file() {
    let (home, dir) = employee(&broker_and_bridge());
    let zap = scoped_servers(home.path(), &dir)["zap"].clone();
    assert_eq!(zap.command, EXE);
    assert_eq!(
        zap.args,
        vec![
            "mcp-proxy",
            "--spawn-scoped",
            "--server",
            "zap",
            "--",
            EXE,
            "mcp-remote-bridge",
            "--agent",
            "trader",
            "--server",
            "zap"
        ]
    );
    let upstream: Value = serde_json::from_str(&zap.upstream_env).unwrap();
    assert_eq!(
        upstream["DUDUCLAW_HOME"], "/h",
        "the bridge's own declared env"
    );
    assert_eq!(upstream["DUDUCLAW_AGENT_ID"], "trader");
    assert!(upstream.get("DUDUCLAW_AGENT_TOKEN").is_none());
}

#[test]
fn spawn_scoping_is_idempotent_for_an_already_scoped_entry() {
    let g = GatedServer {
        command: EXE.into(),
        args: ["mcp-proxy", "--spawn-scoped", "--server", "x", "--", "cmd"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        env: BTreeMap::from([("PGPASSWORD".to_string(), "pw".to_string())]),
    };
    let s = scope_one("x", &g);
    assert_eq!(s.args.iter().filter(|a| *a == "--spawn-scoped").count(), 1);
    // A non-proxy name an entry carried still reaches the upstream.
    let upstream: Value = serde_json::from_str(&s.upstream_env).unwrap();
    assert_eq!(upstream["PGPASSWORD"], "pw");
    assert!(is_managed_entry(&s.command, &s.args, Path::new(EXE)));
    assert!(!is_managed_entry("/other/bin", &s.args, Path::new(EXE)));
    assert!(!is_managed_entry(
        EXE,
        &["mcp-proxy".into(), "--server".into()],
        Path::new(EXE)
    ));
}

#[test]
fn the_spawn_env_dir_is_owner_only_and_holds_one_file_per_server() {
    let (home, dir) = employee(&broker_and_bridge());
    let scoped = scoped_servers(home.path(), &dir);
    let env_dir = write_spawn_env_dir(home.path(), &scoped).unwrap().unwrap();
    let parent = home.path().join("tmp").join(ENV_FILE_DIR);
    assert_eq!(env_dir.path().parent(), Some(parent.as_path()));
    assert!(
        env_dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(ENV_FILE_PREFIX)
    );
    let body = std::fs::read_to_string(env_dir.path().join("masterlink.json")).unwrap();
    assert!(body.contains("broker-secret-pw"));
    assert!(env_dir.path().join("zap.json").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&parent), 0o700);
        assert_eq!(mode(env_dir.path()), 0o700);
        assert_eq!(mode(&env_dir.path().join("masterlink.json")), 0o600);
    }
    let (k, v) = env_dir.env_pair();
    assert_eq!(k, PROXY_UPSTREAM_ENV_DIR_VAR);
    assert_eq!(Path::new(&v), env_dir.path());

    // Dropping the guard deletes the directory with the credentials.
    let path = env_dir.path().to_path_buf();
    drop(env_dir);
    assert!(!path.exists());
}

#[test]
fn concurrent_spawns_get_distinct_env_dirs() {
    let (home, dir) = employee(&broker_and_bridge());
    let scoped = scoped_servers(home.path(), &dir);
    let a = write_spawn_env_dir(home.path(), &scoped).unwrap().unwrap();
    let b = write_spawn_env_dir(home.path(), &scoped).unwrap().unwrap();
    assert_ne!(a.path(), b.path());
    drop(a);
    // The other spawn's file is untouched.
    assert!(b.path().join("masterlink.json").is_file());
}

#[test]
fn nothing_to_register_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    assert!(
        write_spawn_env_dir(home.path(), &BTreeMap::new())
            .unwrap()
            .is_none()
    );
    assert!(!home.path().join("tmp").exists());
}

#[test]
fn the_sweep_removes_stale_spawn_dirs_too() {
    let home = tempfile::tempdir().unwrap();
    let parent = env_file_dir(home.path()).unwrap();
    let stale = parent.join(format!("{ENV_FILE_PREFIX}spawn"));
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("masterlink.json"), "{}").unwrap();
    std::fs::create_dir_all(parent.join("unrelated-dir")).unwrap();
    assert_eq!(
        super::env_files::sweep_stale_env_files(home.path()),
        0,
        "fresh"
    );
    assert_eq!(
        sweep_env_files_older_than(home.path(), std::time::Duration::ZERO),
        1
    );
    assert!(!stale.exists());
    assert!(parent.join("unrelated-dir").exists());
}

#[cfg(unix)]
#[test]
fn the_sweep_never_follows_a_symlink() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep.json"), "{}").unwrap();
    let parent = env_file_dir(home.path()).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        parent.join(format!("{ENV_FILE_PREFIX}link")),
    )
    .unwrap();
    assert_eq!(
        sweep_env_files_older_than(home.path(), std::time::Duration::ZERO),
        1
    );
    assert!(
        outside.path().join("keep.json").exists(),
        "the link went, not its target"
    );
}

#[test]
fn names_that_are_not_plain_are_never_registered() {
    let body = json!({ "mcpServers": {
        "bad name": { "command": "npx", "args": [] },
        "dotted.name": { "command": "npx", "args": [] },
        "名稱": { "command": "npx", "args": [] },
        "ok-name_1": { "command": "npx", "args": [] }
    }})
    .to_string();
    let (home, dir) = employee(&body);
    let scoped = scoped_servers(home.path(), &dir);
    let names: Vec<&String> = scoped.keys().collect();
    assert_eq!(names, vec!["ok-name_1"]);
}

#[tokio::test]
async fn a_utility_call_reads_nothing() {
    let (home, dir) = employee("{ not json");
    let got = crate::runtime_dispatch::UTILITY_CALL
        .scope(
            (),
            gated_servers_for_spawn(&dir, home.path(), &own_server(home.path()), false),
        )
        .await
        .unwrap();
    assert!(got.is_empty());
    // Outside a utility call the same file refuses (fail closed).
    let err = gated_servers_checked(&dir, home.path(), &own_server(home.path()), false)
        .await
        .unwrap_err();
    assert!(
        duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&err),
        "{err}"
    );
}
