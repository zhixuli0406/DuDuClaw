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

#[cfg(test)]
mod tests {
    use super::*;

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
