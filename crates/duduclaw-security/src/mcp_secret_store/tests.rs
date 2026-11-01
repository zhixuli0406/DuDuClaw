use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("mcp-secrets-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path { &self.0 }
    fn config(&self, value: &Value) -> PathBuf {
        let path = self.0.join("agents/alice/.mcp.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        path
    }
}
impl Drop for Home {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn env(value: Value) -> Map<String, Value> { value.as_object().unwrap().clone() }

struct ParentSyncFailure;
impl ParentSyncFailure {
    fn at(path: PathBuf) -> Self {
        FAIL_PARENT_SYNC.with(|failure| *failure.borrow_mut() = Some((path, 0)));
        Self
    }
}
impl Drop for ParentSyncFailure {
    fn drop(&mut self) {
        FAIL_PARENT_SYNC.with(|failure| *failure.borrow_mut() = None);
    }
}

fn config() -> Value {
    json!({"mcpServers": {
        "duduclaw": {"command":duduclaw_core::resolve_duduclaw_bin(), "args":["mcp-server"], "env":{"DUDUCLAW_API_KEY":"own-key"}},
        "vendor": {"command":"vendor", "args":["unchanged"], "env":{"TOKEN":"sk-test-only", "PATH":"/bin"}}
    }})
}

#[test]
fn round_trip_seals_third_party_values_and_preserves_duduclaw_env() {
    let home = Home::new();
    let input = env(json!({"TOKEN":"sk-test-only", "OPTION":"even non-token text", "EMPTY":"", "PATH":"/bin", "HOME":"/home", "NODE_ENV":"production", "LOG_LEVEL":"warn"}));
    let protected = store_mcp_env(home.path(), "alice", "vendor", &input).unwrap();
    for name in ["TOKEN", "OPTION", "EMPTY"] {
        assert_eq!(protected[name], format!("secret://mcp/vendor/{name}"));
    }
    for name in PUBLIC_ENV { assert_eq!(protected[*name], input[*name]); }
    assert_eq!(resolve_mcp_env(home.path(), "alice", "vendor", &protected).unwrap(), input);
    let stored = fs::read_to_string(home.path().join("secrets/mcp/alice.enc")).unwrap();
    assert!(!stored.contains("sk-test-only"));
    let duduclaw_env = env(json!({"DUDUCLAW_API_KEY":"own-key", "DUDUCLAW_AGENT_ID":"alice"}));
    assert_eq!(store_mcp_env(home.path(), "alice", "duduclaw", &duduclaw_env).unwrap(), duduclaw_env);
    let evil_env = env(json!({"VENDOR_KEY":"vendor-secret", "DUDUCLAW_FOO":"leave-unchanged"}));
    let evil_protected = store_mcp_env(home.path(), "alice", "duduclaw-evil", &evil_env).unwrap();
    assert_eq!(evil_protected["VENDOR_KEY"], "secret://mcp/duduclaw-evil/VENDOR_KEY");
    assert_eq!(evil_protected["DUDUCLAW_FOO"], evil_env["DUDUCLAW_FOO"]);
    assert_eq!(resolve_mcp_env(home.path(), "alice", "duduclaw-evil", &evil_protected).unwrap(), evil_env);
    assert_eq!(store_mcp_env(home.path(), "alice", "vendor", &protected).unwrap(), protected);
}

#[test]
fn wrong_missing_or_corrupt_key_fails_closed_without_replacing_store() {
    let home = Home::new();
    let input = env(json!({"TOKEN":"sk-test-only"}));
    let protected = store_mcp_env(home.path(), "alice", "vendor", &input).unwrap();
    let path = home.path().join("secrets/mcp/alice.enc");
    let original = fs::read(&path).unwrap();
    fs::write(home.path().join(".keyfile"), [3u8; 32]).unwrap();
    let error = resolve_mcp_env(home.path(), "alice", "vendor", &protected).unwrap_err().to_string();
    assert!(error.contains("TOKEN"));
    assert!(!error.contains("sk-test-only") && !error.contains("secret://"));
    assert!(store_mcp_env(home.path(), "alice", "vendor", &input).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::remove_file(home.path().join(".keyfile")).unwrap();
    assert!(store_mcp_env(home.path(), "bob", "vendor", &input).is_err());
    assert!(!home.path().join(".keyfile").exists());
    fs::write(home.path().join(".keyfile"), b"short").unwrap();
    assert!(store_mcp_env(home.path(), "bob", "vendor", &input).is_err());
    assert_eq!(fs::read(home.path().join(".keyfile")).unwrap(), b"short");
}

#[test]
fn references_cannot_select_another_employee_server_or_variable() {
    let home = Home::new();
    let protected = store_mcp_env(home.path(), "alice", "vendor", &env(json!({"TOKEN":"alice-secret"}))).unwrap();
    assert!(resolve_mcp_env(home.path(), "bob", "vendor", &protected).is_err());
    for reference in ["secret://mcp/alice/vendor/TOKEN", "secret://mcp/other/TOKEN", "secret://mcp/vendor/OTHER", "secret://env/TOKEN", "secret://"] {
        let input = env(json!({"TOKEN":reference}));
        let error = resolve_mcp_env(home.path(), "alice", "vendor", &input).unwrap_err().to_string();
        assert!(error.contains("TOKEN"));
        assert!(!error.contains("secret://") && !error.contains("alice-secret"));
    }
    assert!(resolve_mcp_env(home.path(), "../alice", "vendor", &protected).is_err());
    assert!(resolve_mcp_env(home.path(), "", "vendor", &Map::new()).is_ok());
}

#[test]
fn invalid_names_and_nonstring_values_are_rejected_before_writes() {
    let home = Home::new();
    for (agent, server, input) in [
        ("../alice", "vendor", env(json!({"TOKEN":"x"}))),
        ("alice", "../vendor", env(json!({"TOKEN":"x"}))),
        ("alice", "vendor", env(json!({"Token":"x"}))),
        ("alice", "vendor", env(json!({"1TOKEN":"x"}))),
        ("alice", "vendor", env(json!({"TOKEN":42}))),
    ] {
        assert!(store_mcp_env(home.path(), agent, server, &input).is_err());
    }
    assert!(!home.path().join("secrets").exists());
}

#[test]
fn server_and_employee_removal_preserve_other_secrets() {
    let home = Home::new();
    let input = env(json!({"TOKEN":"secret"}));
    let a = store_mcp_env(home.path(), "alice", "a", &input).unwrap();
    let b = store_mcp_env(home.path(), "alice", "b", &input).unwrap();
    let other = store_mcp_env(home.path(), "bob", "a", &input).unwrap();
    remove_server(home.path(), "alice", "a").unwrap();
    assert!(resolve_mcp_env(home.path(), "alice", "a", &a).is_err());
    assert_eq!(resolve_mcp_env(home.path(), "alice", "b", &b).unwrap(), input);
    remove_agent(home.path(), "alice").unwrap();
    assert!(resolve_mcp_env(home.path(), "alice", "b", &b).is_err());
    assert_eq!(resolve_mcp_env(home.path(), "bob", "a", &other).unwrap(), input);
}

#[test]
fn migration_is_idempotent_preserves_own_entry_and_encrypts_backup() {
    let home = Home::new();
    let path = home.config(&config());
    let original = fs::read(&path).unwrap();
    assert!(migrate_agent(home.path(), "alice", &path).unwrap());
    let rewritten = fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&rewritten).contains("sk-test-only"));
    let parsed: Value = serde_json::from_slice(&rewritten).unwrap();
    assert_eq!(parsed["mcpServers"]["duduclaw"], config()["mcpServers"]["duduclaw"]);
    assert!(!migrate_agent(home.path(), "alice", &path).unwrap());
    assert_eq!(fs::read(&path).unwrap(), rewritten);
    let backups: Vec<_> = fs::read_dir(home.path().join("backups/mcp-migrate")).unwrap().map(Result::unwrap).collect();
    assert_eq!(backups.len(), 1);
    let encrypted = fs::read(backups[0].path()).unwrap();
    assert!(!String::from_utf8_lossy(&encrypted).contains("sk-test-only"));
    assert_eq!(crypto(home.path(), false).unwrap().decrypt(&encrypted).unwrap(), original);
}

#[test]
fn migration_failure_keeps_original_bytes_and_existing_store() {
    let home = Home::new();
    let path = home.config(&config());
    store_mcp_env(home.path(), "alice", "vendor", &env(json!({"TOKEN":"old"}))).unwrap();
    let original = fs::read(&path).unwrap();
    let store = fs::read(home.path().join("secrets/mcp/alice.enc")).unwrap();
    fs::write(home.path().join(".keyfile"), [9u8; 32]).unwrap();
    assert!(migrate_agent(home.path(), "alice", &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read(home.path().join("secrets/mcp/alice.enc")).unwrap(), store);
}

#[test]
fn key_parent_sync_failure_prevents_ciphertext_and_is_retried() {
    let home = Home::new();
    let input = env(json!({"TOKEN":"secret"}));
    let key_path = home.path().join(".keyfile");
    let store = home.path().join("secrets/mcp/alice.enc");
    for _ in 0..2 {
        // The first call creates the key; the second encounters that same
        // visible key and must repeat its durability barrier before sealing.
        let _failure = ParentSyncFailure::at(key_path.clone());
        let error = store_mcp_env(home.path(), "alice", "vendor", &input).unwrap_err();
        assert!(error.to_string().contains("injected MCP parent sync failure"));
        assert!(!store.exists());
    }
    assert!(key_path.exists());
    assert!(store_mcp_env(home.path(), "alice", "vendor", &input).is_ok());
}

#[test]
fn atomic_private_write_reports_parent_sync_failure() {
    let home = Home::new();
    let path = home.path().join("private.enc");
    let _failure = ParentSyncFailure::at(path.clone());
    let error = atomic_private_write(&path, b"ciphertext").unwrap_err();
    assert!(error.to_string().contains("injected MCP parent sync failure"));
}

#[test]
fn syncing_an_existing_key_preserves_its_contents() {
    let home = Home::new();
    let path = home.path().join(".keyfile");
    let original = [41_u8; 32];
    fs::write(&path, original).unwrap();
    sync_existing_file(&path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(sync_existing_file(&home.path().join("missing.key")).is_err());
    assert!(!home.path().join("missing.key").exists());
}

#[test]
fn every_migration_durability_failure_keeps_plaintext_config() {
    for stage in ["key", "backup", "store", "config"] {
        let home = Home::new();
        let path = home.config(&config());
        let original = fs::read(&path).unwrap();
        let store = home.path().join("secrets/mcp/alice.enc");
        let backups = home.path().join("backups/mcp-migrate");
        let target = match stage {
            "key" => home.path().join(".keyfile"),
            "backup" => backups.clone(),
            "store" => store.clone(),
            "config" => path.clone(),
            _ => unreachable!(),
        };
        let _failure = ParentSyncFailure::at(target);
        let error = migrate_agent(home.path(), "alice", &path).unwrap_err();
        assert!(error.to_string().contains("injected MCP parent sync failure"), "{stage}: {error}");
        assert_eq!(fs::read(&path).unwrap(), original, "{stage}");
        if matches!(stage, "key" | "backup") {
            assert!(!store.exists(), "{stage}: later store write must not run");
        }
        if stage == "key" {
            assert_eq!(fs::read_dir(&backups).unwrap().count(), 0);
        }
        if stage == "config" {
            // The encrypted backup remains available until publication has
            // been retried successfully, even though plaintext was restored.
            assert_eq!(fs::read_dir(&backups).unwrap().count(), 1);
            assert!(fs::read_dir(path.parent().unwrap()).unwrap().all(|entry| {
                !entry.unwrap().file_name().to_string_lossy().starts_with(".mcp-migration-rollback-")
            }));
        }
    }
}

#[test]
fn migration_refuses_bad_env_without_backup_or_file_changes() {
    let home = Home::new();
    let path = home.config(&json!({"mcpServers":{"vendor":{"env":{"TOKEN":42}}}}));
    let original = fs::read(&path).unwrap();
    assert!(migrate_agent(home.path(), "alice", &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!home.path().join("backups").exists());
}

#[test]
fn migration_refuses_unrelated_project_path_before_key_or_backup_creation() {
    let home = Home::new();
    let path = home.path().join(".mcp.json");
    let original = serde_json::to_vec(&config()).unwrap();
    fs::write(&path, &original).unwrap();
    assert!(migrate_agent(home.path(), "alice", &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!home.path().join(".keyfile").exists());
    assert!(!home.path().join("backups").exists());
}

#[test]
fn migration_failure_preserves_existing_backups() {
    let home = Home::new();
    let path = home.config(&config());
    assert!(migrate_agent(home.path(), "alice", &path).unwrap());
    let backup_dir = home.path().join("backups/mcp-migrate");
    let before: Vec<_> = fs::read_dir(&backup_dir).unwrap().map(|entry| {
        let path = entry.unwrap().path();
        (path.clone(), fs::read(path).unwrap())
    }).collect();
    let path = home.config(&config());
    fs::write(home.path().join("secrets/mcp/alice.enc"), b"broken ciphertext").unwrap();
    let original = fs::read(&path).unwrap();
    assert!(migrate_agent(home.path(), "alice", &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read_dir(&backup_dir).unwrap().count(), before.len());
    for (path, bytes) in before { assert_eq!(fs::read(path).unwrap(), bytes); }
}

#[test]
fn concurrent_store_writers_keep_every_server() {
    let home = Home::new();
    let mut threads = Vec::new();
    for i in 0..8 {
        let path = home.path().to_owned();
        threads.push(std::thread::spawn(move || {
            let server = format!("server{i}");
            store_mcp_env(&path, "alice", &server, &env(json!({"TOKEN":server}))).unwrap();
        }));
    }
    for thread in threads { thread.join().unwrap(); }
    for i in 0..8 {
        let server = format!("server{i}");
        let reference = env(json!({"TOKEN":format!("secret://mcp/{server}/TOKEN")}));
        assert_eq!(resolve_mcp_env(home.path(), "alice", &server, &reference).unwrap()["TOKEN"], server);
    }
}

#[test]
fn migration_keeps_three_encrypted_backups_per_exact_agent() {
    let home = Home::new();
    for _ in 0..5 {
        let path = home.config(&config());
        assert!(migrate_agent(home.path(), "alice", &path).unwrap());
    }
    let backups: Vec<_> = fs::read_dir(home.path().join("backups/mcp-migrate")).unwrap().map(Result::unwrap).collect();
    assert_eq!(backups.len(), 3);
    for backup in backups {
        assert!(crypto(home.path(), false).unwrap().decrypt(&fs::read(backup.path()).unwrap()).is_ok());
    }
}

#[cfg(unix)]
#[test]
fn private_modes_and_symlink_refusal() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    store_mcp_env(home.path(), "alice", "vendor", &env(json!({"TOKEN":"secret"}))).unwrap();
    for dir in ["secrets", "secrets/mcp"] {
        assert_eq!(fs::metadata(home.path().join(dir)).unwrap().permissions().mode() & 0o777, 0o700);
    }
    for file in [".keyfile", "secrets/mcp/alice.enc"] {
        assert_eq!(fs::metadata(home.path().join(file)).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let path = home.path().join("secrets/mcp/bob.enc");
    std::os::unix::fs::symlink(home.path().join("secrets/mcp/alice.enc"), path).unwrap();
    assert!(store_mcp_env(home.path(), "bob", "vendor", &env(json!({"TOKEN":"new"}))).is_err());
}

#[test]
fn platform_env_names_are_preserved_on_every_server_without_trusting_names() {
    let home = Home::new();
    let platform = env(json!({"DUDUCLAW_AGENT_TOKEN":"host-token", "DUDUCLAW_HOME":"/home/host"}));
    for server in ["duduclaw", "duduclaw-pro", "duduclaw-secondary", "duduclaw-evil", "vendor"] {
        assert_eq!(store_mcp_env(home.path(), "alice", server, &platform).unwrap(), platform);
    }
    let aliases = json!({"mcpServers":{
        "duduclaw-pro":{"env":platform},
        "duduclaw-secondary":{"env":platform},
        "duduclaw-evil":{"command":"evil", "env":platform}
    }});
    let path = home.config(&aliases);
    let before = fs::read(&path).unwrap();
    assert!(!migrate_agent(home.path(), "alice", &path).unwrap());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!home.path().join("secrets").exists());
    assert!(!home.path().join("backups").exists());
    let mut mixed = platform.clone();
    mixed.insert("API_KEY".into(), json!("vendor-secret"));
    let stored = store_mcp_env(home.path(), "alice", "duduclaw-evil", &mixed).unwrap();
    assert_eq!(stored["DUDUCLAW_AGENT_TOKEN"], platform["DUDUCLAW_AGENT_TOKEN"]);
    assert_eq!(stored["API_KEY"], "secret://mcp/duduclaw-evil/API_KEY");
    let raw = json!({"mcpServers":{"duduclaw-evil":{"command":"evil", "env":stored}}});
    assert!(duduclaw_core::mcp_proxy_rewrite::validate_raw_mcp_config(
        &raw, &duduclaw_core::resolve_duduclaw_bin(), &Value::Null,
    ).is_err());
}

#[test]
fn duduclaw_prefix_and_wrong_builtin_command_are_migrated() {
    let home = Home::new();
    let mut input = config();
    for name in ["duduclaw-evil", "duduclaw-pro"] {
        input["mcpServers"][name] = json!({"command":"evil-mcp", "env":{"TOKEN":"third-party-secret"}});
    }
    let path = home.config(&input);
    assert!(migrate_agent(home.path(), "alice", &path).unwrap());
    let migrated: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for name in ["duduclaw-evil", "duduclaw-pro"] {
        assert_eq!(migrated["mcpServers"][name]["env"]["TOKEN"], format!("secret://mcp/{name}/TOKEN"));
    }
    assert_eq!(migrated["mcpServers"]["duduclaw"], input["mcpServers"]["duduclaw"]);
    let rewritten = duduclaw_core::mcp_proxy_rewrite::rewrite_mcp_config_for_gate(
        &migrated, &duduclaw_core::resolve_duduclaw_bin(),
        duduclaw_core::mcp_proxy_rewrite::ProxyPurpose { redaction: false, tool_gate: false, secret_env: true },
        &input["mcpServers"]["duduclaw"],
    ).unwrap();
    assert_eq!(rewritten.config["mcpServers"]["duduclaw-evil"]["args"][0], "mcp-proxy");
    let rewritten_builtin = &rewritten.config["mcpServers"]["duduclaw-pro"];
    assert_eq!(rewritten_builtin["args"][0], "mcp-server");
    assert_eq!(rewritten_builtin["command"], input["mcpServers"]["duduclaw"]["command"]);
    assert_ne!(rewritten_builtin["command"], "evil-mcp");
}

#[test]
fn remove_agent_reports_parent_sync_failure_after_unlink() {
    let home = Home::new();
    store_mcp_env(home.path(), "alice", "vendor", &env(json!({"TOKEN":"secret"}))).unwrap();
    let path = home.path().join("secrets/mcp/alice.enc");
    let _failure = ParentSyncFailure::at(path.clone());
    let error = remove_agent(home.path(), "alice").unwrap_err();
    assert!(error.to_string().contains("injected MCP parent sync failure"));
    assert!(!path.exists());
}
