//! Upstream env for a `duduclaw mcp-proxy --spawn-scoped` entry (2026-10-20).
//!
//! Grok registers MCP servers in a persistent per-directory
//! file (`.grok/config.toml`) that every spawn of
//! the employee shares, so nothing per spawn (the upstream server's own
//! credentials, the decision "this spawn may use this server") can live in
//! it. The gateway writes those into a per-spawn, owner-only directory and
//! hands its path to the CLI in [`PROXY_UPSTREAM_ENV_DIR_VAR`]; the CLI passes
//! it on to the proxy (by `${…}` expansion
//! of the entry's `env`).
//!
//! A spawn-scoped proxy that finds no such directory, or no file for its
//! server in it, refuses to start anything: the persistent entry is then
//! inert (a utility call, a manual CLI run in the employee's directory, a
//! spawn of another employee sharing the directory).

use std::ffi::OsString;
use std::path::Path;

pub use duduclaw_core::mcp_proxy_rewrite::PROXY_UPSTREAM_ENV_DIR_VAR;

/// Largest upstream env file read (a JSON map of env strings).
const MAX_ENV_FILE_BYTES: u64 = 1024 * 1024;

/// The upstream env blob for `server` from the directory `dir` names.
///
/// `Err` (the proxy then starts nothing) when `dir` is unset, empty or not an
/// absolute path, when `server` is not a portable name, or when the file is
/// missing, a symbolic link, not a regular file, too large or unreadable.
pub fn scoped_env_blob(dir: Option<OsString>, server: &str) -> Result<String, String> {
    let dir = dir
        .filter(|d| !d.is_empty())
        .ok_or_else(|| "not started by a DuDuClaw spawn that registered this server".to_string())?;
    let dir = Path::new(&dir);
    if !dir.is_absolute() {
        return Err(format!(
            "{PROXY_UPSTREAM_ENV_DIR_VAR} is not an absolute path"
        ));
    }
    let name = duduclaw_core::mcp_proxy_rewrite::spawn_env_file_name(server)
        .ok_or_else(|| format!("server name {server:?} is not a portable name"))?;
    let path = dir.join(name);
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("this spawn did not register the server ({e})"))?;
    if !meta.file_type().is_file() {
        return Err("the server's env entry is not a regular file".to_string());
    }
    if meta.len() > MAX_ENV_FILE_BYTES {
        return Err("the server's env file is too large".to_string());
    }
    std::fs::read_to_string(&path).map_err(|e| format!("the server's env file is unreadable: {e}"))
}

/// Resolve declared env before the permissive environment-merging helper runs.
/// No reference is ever handed to the upstream, including malformed schemes.
/// Resolving a key requires an explicit employee identity and its host-issued
/// HMAC. The delegation soft-mode policy and default agent never authorize it.
pub fn resolve_declared_env(
    home: &Path,
    agent: Option<&str>,
    token: Option<&str>,
    server: &str,
    encoded: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(encoded) = encoded else { return Ok(None) };
    let env: serde_json::Map<String, serde_json::Value> = serde_json::from_str(encoded)
        .map_err(|_| format!("MCP server '{server}': invalid upstream env map"))?;
    if let Some((name, _)) = env.iter().find(|(_, value)| !value.is_string()) {
        return Err(format!("MCP server '{server}': env '{name}' must be a string"));
    }
    if let Some(name) = env.keys().find(|name| {
        duduclaw_core::mcp_proxy_rewrite::is_proxy_private_env_key(name)
    }) {
        return Err(format!("MCP server '{server}': upstream env '{name}' is reserved for the host"));
    }
    let reference = env.iter().find(|(_, value)| {
        value.as_str().is_some_and(|v| v.starts_with("secret://"))
    });
    if let Some((name, _)) = reference {
        let agent = agent.filter(|id| {
            duduclaw_core::is_valid_agent_id(id)
                && !duduclaw_core::is_reserved_agent_id(id)
                && duduclaw_core::identity_token::verify_claim(
                    home, id, token.unwrap_or_default(), true,
                ) == duduclaw_core::IdentityVerdict::Verified
        }).ok_or_else(|| {
            format!("MCP server '{server}': cannot resolve env '{name}' without a verified employee identity")
        })?;
        let resolved = duduclaw_security::mcp_secret_store::resolve_mcp_env(home, agent, server, &env)
            .map_err(|e| e.to_string())?;
        return Ok(Some(serde_json::Value::Object(resolved).to_string()));
    }
    Ok(Some(encoded.to_string()))
}

/// Final check also covers references inherited from the launching CLI.
pub fn reject_unresolved_env(server: &str, env: &[(String, String)]) -> Result<(), String> {
    if let Some((name, _)) = env.iter().find(|(_, value)| value.starts_with("secret://")) {
        return Err(format!("MCP server '{server}': unresolved env '{name}'; upstream was not started"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_references_fail_without_exposing_the_reference() {
        let home = tempfile::tempdir().unwrap();
        for value in ["secret://mcp/server/API_KEY", "secret://mcp/other/API_KEY", "secret://mcp/other-agent/server/API_KEY", "secret://unknown/key"] {
            let raw = serde_json::json!({"API_KEY":value}).to_string();
            for agent in [None, Some("employee")] {
                let error = resolve_declared_env(home.path(), agent, None, "server", Some(&raw)).unwrap_err();
                assert!(error.contains("API_KEY"));
                assert!(!error.contains("secret://"));
            }
        }
    }

    #[test]
    fn declared_refs_resolve_only_from_this_employees_store() {
        let home = tempfile::tempdir().unwrap();
        let env = serde_json::json!({"API_KEY":"sentinel-key"}).as_object().unwrap().clone();
        let refs = duduclaw_security::mcp_secret_store::store_mcp_env(home.path(), "alice", "server", &env).unwrap();
        let raw = serde_json::Value::Object(refs).to_string();
        let key = duduclaw_core::identity_token::ensure_key(home.path()).unwrap();
        let alice_token = duduclaw_core::identity_token::mint_token(&key, "alice");
        let bob_token = duduclaw_core::identity_token::mint_token(&key, "bob");
        let alice = resolve_declared_env(home.path(), Some("alice"), Some(&alice_token), "server", Some(&raw)).unwrap().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&alice).unwrap()["API_KEY"], "sentinel-key");
        let bob = resolve_declared_env(home.path(), Some("bob"), Some(&bob_token), "server", Some(&raw)).unwrap_err();
        assert!(!bob.contains("sentinel-key") && !bob.contains("secret://"));
        for reference in ["secret://mcp/bob/server/API_KEY", "secret://mcp/other/API_KEY"] {
            let raw = serde_json::json!({"API_KEY":reference}).to_string();
            let error = resolve_declared_env(home.path(), Some("alice"), Some(&alice_token), "server", Some(&raw)).unwrap_err();
            assert!(!error.contains("sentinel-key") && !error.contains("secret://"));
        }
    }

    #[test]
    fn malformed_maps_and_inherited_references_are_refused() {
        let home = tempfile::tempdir().unwrap();
        for raw in ["not json", "[]", r#"{"API_KEY":123}"#] {
            assert!(resolve_declared_env(home.path(), None, None, "server", Some(raw)).is_err());
        }
        let env = vec![("API_KEY".into(), "secret://mcp/server/API_KEY".into())];
        let error = reject_unresolved_env("server", &env).unwrap_err();
        assert!(error.contains("API_KEY") && !error.contains("secret://"));
        assert!(resolve_declared_env(home.path(), None, None, "server", Some(r#"{"PATH":"/bin"}"#)).is_ok());
    }

    #[test]
    fn secret_resolution_requires_a_valid_host_token_even_in_soft_mode() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"),
            "[delegation]\nrequire_identity_token = false\n[general]\ndefault_agent = 'alice'\n").unwrap();
        let env = serde_json::json!({"API_KEY":"sentinel-key"}).as_object().unwrap().clone();
        let refs = duduclaw_security::mcp_secret_store::store_mcp_env(home.path(), "alice", "server", &env).unwrap();
        let raw = serde_json::Value::Object(refs).to_string();
        // No identity key is not a feature-off authorization to decrypt.
        assert!(resolve_declared_env(home.path(), Some("alice"), Some("00"), "server", Some(&raw)).is_err());
        let key = duduclaw_core::identity_token::ensure_key(home.path()).unwrap();
        let alice_token = duduclaw_core::identity_token::mint_token(&key, "alice");
        let bob_token = duduclaw_core::identity_token::mint_token(&key, "bob");
        for (agent, token) in [
            (None, Some(alice_token.as_str())),
            (Some("alice"), None),
            (Some("alice"), Some("")),
            (Some("alice"), Some("00")),
            (Some("alice"), Some(bob_token.as_str())),
            (Some("bob"), Some(alice_token.as_str())),
        ] {
            let error = resolve_declared_env(home.path(), agent, token, "server", Some(&raw)).unwrap_err();
            assert!(error.contains("verified employee identity"));
            assert!(!error.contains("sentinel-key") && !error.contains("secret://"));
        }
        for agent in ["__untrusted__", "dashboard", "../alice"] {
            let token = duduclaw_core::identity_token::mint_token(&key, agent);
            assert!(resolve_declared_env(home.path(), Some(agent), Some(&token), "server", Some(&raw)).is_err());
        }
    }

    #[test]
    fn nested_proxy_env_and_owner_overrides_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let key = duduclaw_core::identity_token::ensure_key(home.path()).unwrap();
        let token = duduclaw_core::identity_token::mint_token(&key, "alice");
        for name in ["DUDUCLAW_MCP_PROXY_ENV", "DUDUCLAW_MCP_PROXY_ENV_FILE",
            "DUDUCLAW_MCP_PROXY_ENV_DIR", "DUDUCLAW_AGENT_ID", "DUDUCLAW_AGENT_TOKEN"]
        {
            let raw = serde_json::json!({(name):
                serde_json::json!({"API_KEY":"secret://mcp/bob/server/API_KEY"}).to_string()
            }).to_string();
            let error = resolve_declared_env(home.path(), Some("alice"), Some(&token), "server", Some(&raw)).unwrap_err();
            assert!(error.contains(name) && error.contains("reserved for the host"));
            assert!(!error.contains("secret://"));
        }
    }

    fn dir_with(server: &str, body: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(format!("{server}.json")), body).unwrap();
        d
    }

    #[test]
    fn reads_the_file_of_its_own_server() {
        let d = dir_with("masterlink", r#"{"ML_PASSWORD":"pw"}"#);
        let got = scoped_env_blob(Some(d.path().as_os_str().to_owned()), "masterlink").unwrap();
        assert_eq!(got, r#"{"ML_PASSWORD":"pw"}"#);
    }

    #[test]
    fn refuses_without_a_directory_or_a_file_for_this_server() {
        assert!(scoped_env_blob(None, "masterlink").is_err());
        assert!(scoped_env_blob(Some(OsString::new()), "masterlink").is_err());
        assert!(scoped_env_blob(Some("relative/dir".into()), "masterlink").is_err());
        let d = dir_with("other", "{}");
        assert!(scoped_env_blob(Some(d.path().as_os_str().to_owned()), "masterlink").is_err());
        // A name that could leave the directory is never joined.
        assert!(scoped_env_blob(Some(d.path().as_os_str().to_owned()), "../other").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_env_file() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        std::os::unix::fs::symlink(&outside, d.path().join("masterlink.json")).unwrap();
        assert!(scoped_env_blob(Some(d.path().as_os_str().to_owned()), "masterlink").is_err());
    }
}
