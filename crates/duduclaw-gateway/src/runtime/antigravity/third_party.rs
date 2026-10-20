//! Third-party MCP servers for an Antigravity (`agy`) spawn (2026-10-20).
//!
//! Until this module agy got only DuDuClaw's own server in
//! `<work_root>/.agents/mcp_config.json`; the employee's other `.mcp.json`
//! servers were not started at all (agy 1.4.2 does not read `.mcp.json`,
//! measured: a workspace `.mcp.json` server is never started, a
//! `.agents/mcp_config.json` one is).
//!
//! Each gated server ([`crate::runtime::third_party_mcp`]) is registered as
//! a spawn-scoped `duduclaw mcp-proxy` entry with `command` and `args` only,
//! like the `duduclaw` entry. **Credentials:** `mcp_config.json` is a
//! persistent file in the (agent-writable) workspace, shared by every spawn
//! there, so it carries no `env` at all. agy hands its own environment to
//! the MCP children it starts (the identity already rides it; measured again
//! on 1.4.2, the declared `env` is merged on top), so the proxy gets its
//! identity, its MCP credential and `DUDUCLAW_MCP_PROXY_ENV_DIR` from this
//! spawn's environment, and the upstream server's own env from that per-spawn
//! owner-only directory. Concurrent spawns of the employee write the same
//! file content and each get their own directory. A proxy started without the
//! variable (a utility call, an operator running agy there) starts nothing.
//!
//! agy's print mode soft-denies MCP calls without a `permissions.allow`
//! rule, so each registered server also gets `mcp(<server>/*)` and a read
//! rule on its schema directory in the user settings
//! (`antigravity_setup::tool_grants_for`); the proxy is the gate.
//!
//! Utility calls get no third-party server, read no `.mcp.json` and change
//! nothing. With no third-party server the spawn is the old one, except that
//! an entry this module wrote for a server the employee no longer lists is
//! removed from the employee's own workspace file.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::runtime::third_party_mcp::env_files::{SpawnEnvDir, write_spawn_env_dir};
use crate::runtime::third_party_mcp::{ScopedServer, gated_servers_for_spawn, spawn_scoped};

/// What a spawn prepared.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    /// The employee's servers, as registered in `mcp_config.json`.
    pub servers: BTreeMap<String, ScopedServer>,
    /// This spawn's upstream-env directory; hold it until agy has exited.
    pub env_dir: Option<SpawnEnvDir>,
    /// The DuDuClaw binary the entries launch (for pruning stale entries);
    /// `None` for a utility call or when no third-party gate ran.
    pub self_exe: Option<std::path::PathBuf>,
}

impl Prepared {
    /// Server names for the agy permission rules.
    pub(crate) fn names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }

    /// `mcp_config.json` entries (`command` + `args`, no `env`).
    pub(crate) fn entries(&self) -> Vec<(String, Value)> {
        self.servers
            .iter()
            .map(|(name, s)| {
                (
                    name.clone(),
                    serde_json::json!({ "command": s.command, "args": s.args }),
                )
            })
            .collect()
    }
}

/// Gate and scope the employee's third-party servers for one spawn and write
/// their per-spawn env directory. `Err` is spawn-gate prefixed: refuse.
pub(crate) async fn prepare(
    agent_dir: Option<&Path>,
    agent_id: &str,
    home_dir: &Path,
) -> Result<Prepared, String> {
    if crate::runtime_dispatch::in_utility_call() {
        return Ok(Prepared::default());
    }
    let Some(agent_dir) = agent_dir else {
        return Ok(Prepared::default());
    };
    let Some(own) = super::super::duduclaw_mcp_server_json_for_home(agent_id, home_dir) else {
        return Ok(Prepared::default());
    };
    let self_exe = own
        .get("command")
        .and_then(|c| c.as_str())
        .map(std::path::PathBuf::from);
    // agy refuses the explore lane before this point (P5).
    let gated = gated_servers_for_spawn(agent_dir, home_dir, &own, false).await?;
    let servers = spawn_scoped(&gated);
    let env_dir = write_spawn_env_dir(home_dir, &servers).map_err(|e| {
        duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
            "第三方 MCP 伺服器的啟動設定無法產生（{e}），這次不啟動。"
        ))
    })?;
    Ok(Prepared {
        servers,
        env_dir,
        self_exe,
    })
}

#[cfg(test)]
mod tests;
