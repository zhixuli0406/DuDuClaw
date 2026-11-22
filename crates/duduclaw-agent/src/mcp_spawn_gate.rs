//! Shared pieces of the `.mcp.json` spawn gate that callers outside
//! [`crate::mcp_template`] need: where the writers' lock lives, the marker a
//! gate refusal carries, and the outcome type of the gate.
//!
//! The lock deliberately does not sit next to `.mcp.json`. The employee
//! directory is writable by the employee's own tools, and
//! `duduclaw_core::with_file_lock` opens `<path>.lock` with create + follow,
//! so a directory or file planted under that name would make every repair
//! (and therefore every spawn) fail, and a planted symbolic link would make
//! the lock open a file elsewhere. For an employee directory the lock lives
//! under `<home>/locks/`, one per employee directory, keyed by a hash of the
//! canonical directory path. Directories that belong to no employee keep the old sidecar
//! location; nothing spawns an employee from them.

use std::path::{Path, PathBuf};

/// Prefix of every error [`crate::mcp_template::prepare_mcp_config_for_spawn`]
/// returns. Spawn loops detect it with [`is_spawn_gate_error`] and stop: the
/// failure is about the employee's configuration file, not about the account
/// that would have run the CLI, so it must not count against account health
/// and retrying with another account cannot help.
pub const SPAWN_GATE_ERROR_PREFIX: &str = "[mcp_config_unverified] ";

/// Wrap a gate refusal message in [`SPAWN_GATE_ERROR_PREFIX`].
///
/// A message may start with text the employee controls (a file path, an MCP
/// server name). One that happens to start with
/// [`ENGINE_LOCAL_REFUSAL_MARKER`] gets a leading space, so an employee-gate
/// refusal can never carry the engine-local marker at its anchored position;
/// only [`engine_local_spawn_gate_error`] places it there.
pub fn spawn_gate_error(message: &str) -> String {
    if message.starts_with(ENGINE_LOCAL_REFUSAL_MARKER) {
        format!("{SPAWN_GATE_ERROR_PREFIX} {message}")
    } else {
        format!("{SPAWN_GATE_ERROR_PREFIX}{message}")
    }
}

/// Second marker, directly after [`SPAWN_GATE_ERROR_PREFIX`], on a refusal
/// that is about an ENGINE's own host configuration (e.g. Codex external
/// configuration layers, Grok's global MCP sources) rather than about the
/// employee. Such a refusal is still a spawn-gate refusal everywhere
/// ([`is_spawn_gate_error`] is `true`: no account rotation, no implicit
/// fallbacks); only the gateway's explicit runtime-fallback rule may treat it
/// differently. Set at the place the refusal is created, never inferred from
/// message text.
pub const ENGINE_LOCAL_REFUSAL_MARKER: &str = "[engine_local_config] ";

/// Wrap an engine-local host configuration refusal: a spawn-gate refusal
/// carrying [`ENGINE_LOCAL_REFUSAL_MARKER`].
pub fn engine_local_spawn_gate_error(message: &str) -> String {
    format!("{SPAWN_GATE_ERROR_PREFIX}{ENGINE_LOCAL_REFUSAL_MARKER}{message}")
}

/// Whether `err` is a spawn-gate refusal (anchored prefix match).
pub fn is_spawn_gate_error(err: &str) -> bool {
    err.starts_with(SPAWN_GATE_ERROR_PREFIX)
}

/// Whether `err` is an engine-local spawn-gate refusal (both markers,
/// anchored at the start).
pub fn is_engine_local_spawn_gate_error(err: &str) -> bool {
    err.strip_prefix(SPAWN_GATE_ERROR_PREFIX)
        .is_some_and(|rest| rest.starts_with(ENGINE_LOCAL_REFUSAL_MARKER))
}

/// The message to show a person for a spawn-gate refusal (the text after the
/// marker(s)), or `None` when `err` is not one.
pub fn spawn_gate_user_message(err: &str) -> Option<&str> {
    err.strip_prefix(SPAWN_GATE_ERROR_PREFIX)
        .map(|rest| rest.strip_prefix(ENGINE_LOCAL_REFUSAL_MARKER).unwrap_or(rest))
}

/// What the spawn gate did for one working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpGateOutcome {
    /// An employee directory: its `.mcp.json` was regenerated or already
    /// matched what DuDuClaw writes.
    Confirmed,
    /// Not an employee directory (no `<home>/agents/<id>` or
    /// `<home>/agents/.ephemeral/<id>` shape and no `agent.toml`), so there
    /// is no employee configuration to confirm. Carries the reason for logs.
    NotApplicable(&'static str),
}

/// The lock file path base for `config_path` (`<dir>/.mcp.json`); the lock
/// itself is this path plus `.lock` (`with_file_lock` appends it).
pub fn mcp_config_lock_base(config_path: &Path) -> Result<PathBuf, String> {
    let Some(dir) = config_path.parent() else {
        return Ok(config_path.to_path_buf());
    };
    if !crate::mcp_template::is_repairable_agent_dir(dir) {
        return Ok(config_path.to_path_buf());
    }
    let home = crate::mcp_template::derive_home_from_agent_dir(dir);
    #[cfg(windows)]
    let key = {
        use duduclaw_core::path_compare;
        let canon = path_compare::canonicalize_allow_missing(dir)
            .and_then(|path| path_compare::comparison_key(&path));
        canon.map_err(|_| spawn_gate_error("無法確認員工設定鎖定資料夾；拒絕啟動。"))?
    };
    #[cfg(not(windows))]
    let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()).to_string_lossy().into_owned();
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    let hex: String = digest.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(home.join("locks").join(format!("mcp-json-{hex}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_anchored_and_strippable() {
        let e = spawn_gate_error("員工 a 的 MCP 設定無法確認");
        assert!(is_spawn_gate_error(&e));
        assert_eq!(
            spawn_gate_user_message(&e),
            Some("員工 a 的 MCP 設定無法確認")
        );
        assert!(!is_spawn_gate_error("rate limit [mcp_config_unverified] "));
        assert_eq!(spawn_gate_user_message("plain"), None);
        assert!(!is_engine_local_spawn_gate_error(&e));
    }

    #[test]
    fn the_engine_local_marker_is_a_spawn_gate_refusal_with_a_clean_message() {
        let e = engine_local_spawn_gate_error("引擎設定被拒絕");
        assert!(is_spawn_gate_error(&e), "still stops rotation and implicit fallbacks");
        assert!(is_engine_local_spawn_gate_error(&e));
        assert_eq!(spawn_gate_user_message(&e), Some("引擎設定被拒絕"));
        // Only directly after the gate prefix, never elsewhere in the text.
        assert!(!is_engine_local_spawn_gate_error(&format!("x {e}")));
        assert!(!is_engine_local_spawn_gate_error(&spawn_gate_error("note [engine_local_config] x")));
        assert!(!is_engine_local_spawn_gate_error("[engine_local_config] x"));
    }

    /// An employee-gate message that itself starts with the engine-local
    /// marker (an employee-chosen server name or path) never yields it.
    #[test]
    fn an_employee_gate_message_cannot_forge_the_engine_local_marker() {
        for message in [
            format!("{ENGINE_LOCAL_REFUSAL_MARKER}crm：定義了 mcp_servers"),
            format!("{ENGINE_LOCAL_REFUSAL_MARKER}/Users/a/.mcp.json"),
            ENGINE_LOCAL_REFUSAL_MARKER.to_string(),
        ] {
            let error = spawn_gate_error(&message);
            assert!(is_spawn_gate_error(&error));
            assert!(!is_engine_local_spawn_gate_error(&error), "{error}");
            assert_eq!(spawn_gate_user_message(&error).map(str::trim_start), Some(message.as_str()));
        }
        // Ordinary messages are unchanged.
        assert_eq!(spawn_gate_error("x"), format!("{SPAWN_GATE_ERROR_PREFIX}x"));
    }

    #[test]
    fn an_employee_lock_lives_under_home_locks_not_in_the_employee_directory() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents").join("agnes");
        std::fs::create_dir_all(&agent).unwrap();
        let base = mcp_config_lock_base(&agent.join(".mcp.json")).unwrap();
        assert!(
            base.starts_with(home.path().join("locks")),
            "{}",
            base.display()
        );
        assert!(!base.starts_with(&agent));
        let eph = home.path().join("agents/.ephemeral/eph-1");
        std::fs::create_dir_all(&eph).unwrap();
        let eph_base = mcp_config_lock_base(&eph.join(".mcp.json")).unwrap();
        assert!(eph_base.starts_with(home.path().join("locks")));
        assert_ne!(base, eph_base, "one lock per employee directory");
        // Not an employee directory: the old sidecar location.
        let other = tempfile::tempdir().unwrap();
        let p = other.path().join(".mcp.json");
        assert_eq!(mcp_config_lock_base(&p).unwrap(), p);
    }

    #[cfg(windows)]
    #[test]
    fn windows_employee_lock_is_shared_across_path_spellings() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/.ephemeral/MixedCase");
        std::fs::create_dir_all(&agent).unwrap();
        let canonical = std::fs::canonicalize(&agent).unwrap();
        let upper = PathBuf::from(canonical.to_str().unwrap().to_uppercase());
        let expected = mcp_config_lock_base(&agent.join(".mcp.json")).unwrap();
        for dir in [&canonical, &upper] {
            let actual = mcp_config_lock_base(&dir.join(".mcp.json")).unwrap();
            assert_eq!(actual.file_name(), expected.file_name());
            assert_eq!(duduclaw_core::path_compare::file_identity(actual.parent().unwrap().parent().unwrap()).unwrap(),
                duduclaw_core::path_compare::file_identity(home.path()).unwrap());
        }
    }
}
