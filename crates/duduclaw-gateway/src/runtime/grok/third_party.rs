//! Third-party MCP servers for a Grok spawn (2026-10-20).
//!
//! Before this module the Grok runtime registered only DuDuClaw's own server
//! in `<cwd>/.grok/config.toml`. But Grok (1.0.62, measured with `grok mcp
//! doctor` / `grok inspect`, and its bundled docs) also loads the `.mcp.json`
//! of its working directory by itself, which for an employee is its own
//! `<agent_dir>/.mcp.json`: the employee's other servers were started by Grok
//! directly, without `duduclaw mcp-proxy`, so none of the third-party tool
//! gate applied (and `url` entries were connected directly), under
//! `--permission-mode bypassPermissions`.
//!
//! Grok merges its sources by priority, `config.toml` first and `.mcp.json`
//! last, a name defined higher winning (also verified: a `config.toml` entry
//! with `enabled = false` hides the `.mcp.json` server of that name). So the
//! config this module writes holds, next to `duduclaw`:
//!
//! - every gated server of the employee, as a spawn-scoped proxy
//!   ([`crate::runtime::third_party_mcp::spawn_scoped`]);
//! - a disabled *shadow* for every other name the directory's `.mcp.json`
//!   lists (direct URL entries, names that are not plain, DuDuClaw-named
//!   extras), so Grok never starts them itself.
//!
//! **Credentials and concurrency (the trade-off).** `.grok/config.toml` is a
//! persistent file shared by every spawn in that directory, and there is no
//! per-invocation MCP override for `grok -p` (`GROK_CONFIG` overlays exclude
//! `mcp_servers`). So the file holds only what is the same for every spawn:
//! the proxy command, the proxy's own carry env (home, port, instance, agent
//! id and the MCP credential the `duduclaw` entry of the same file already
//! holds — never the identity MAC) and `DUDUCLAW_MCP_PROXY_ENV_DIR =
//! "${DUDUCLAW_MCP_PROXY_ENV_DIR:-}"`, which Grok expands from its own
//! environment when it loads the file (documented; measured with `grok mcp
//! doctor`). The upstream servers' own env lives only in a per-spawn,
//! owner-only directory the spawn sets that variable to, deleted when Grok
//! exits and swept at gateway start. Two spawns of the same employee write
//! identical file content and each get their own directory, so neither
//! clobbers the other. A proxy started without the variable (a utility call,
//! an operator running `grok` in the directory) starts nothing.
//!
//! Fail closed: when third-party servers are listed, an unreadable or
//! malformed `.mcp.json`, a spawn-gate refusal, a config that cannot be
//! written or an env directory that cannot be created refuses the spawn
//! (before, a failed write only warned; now a missing shadow would hand Grok
//! an ungated server). With no third-party server listed anywhere the spawn
//! is exactly the old one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use duduclaw_core::mcp_proxy_rewrite::{PROXY_SPAWN_SCOPED_FLAG, PROXY_UPSTREAM_ENV_DIR_VAR};

use crate::runtime::third_party_mcp::env_files::{SpawnEnvDir, write_spawn_env_dir};
use crate::runtime::third_party_mcp::{
    ScopedServer, gated_servers_and_names_checked, spawn_scoped,
};

mod sources;

/// The value Grok expands into each proxy's env at load time.
fn env_dir_reference() -> String {
    format!("${{{PROXY_UPSTREAM_ENV_DIR_VAR}:-}}")
}

/// Upstream command of a disabled shadow entry (never started: `enabled =
/// false`, and a spawn-scoped proxy for it would find no env file anyway).
const SHADOW_UPSTREAM: &[&str] = &["shadowed", "--", "false"];

/// What a spawn prepared.
#[derive(Debug, Default)]
pub(crate) struct Prepared {
    /// The config was written here (`duduclaw` + third-party + shadows), so
    /// the caller must not run the old duduclaw-only writer on these dirs.
    pub managed: bool,
    /// This spawn's upstream-env directory; hold it until Grok has exited.
    pub env_dir: Option<SpawnEnvDir>,
}

/// Register the employee's third-party servers for one spawn (see the module
/// docs). `work_root` is the directory Grok runs in (`agent_dir` unless a
/// caller overrode it). `Err` is spawn-gate prefixed: refuse the spawn.
pub(crate) async fn prepare(
    agent_dir: Option<&Path>,
    work_root: Option<&Path>,
    agent_id: &str,
    home_dir: &Path,
    explore: bool,
) -> Result<Prepared, String> {
    let mut targets: Vec<PathBuf> = Vec::new();
    for d in [agent_dir, work_root].into_iter().flatten() {
        if !targets.iter().any(|t| t == d) {
            targets.push(d.to_path_buf());
        }
    }
    let refuse = |what: &str| {
        duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
            "Grok 員工的第三方 MCP 伺服器無法安全登記（{what}），這次不啟動。"
        ))
    };
    // Names Grok would load from each directory by itself: its `.mcp.json`
    // (decision read only; the employee's own list is taken again below from
    // the read the gate uses), `.cursor/mcp.json`, and the `.mcp.json` /
    // `.cursor/mcp.json` / `.grok/config.toml` of every directory up to the
    // git root (`sources`).
    let mut other: Vec<(PathBuf, Vec<String>, Vec<String>)> = Vec::new();
    let mut any_third_party = false;
    for dir in &targets {
        let own_names = crate::redaction_proxy::mcp_json_server_names(
            home_dir,
            &dir.join(".mcp.json"),
            explore,
        )?
        .unwrap_or_default();
        let names = sources::other_source_names(dir).map_err(|e| refuse(&e))?;
        any_third_party |= own_names.iter().chain(&names).any(|n| n != "duduclaw");
        other.push((dir.clone(), own_names, names));
    }
    if !any_third_party {
        for dir in &targets {
            warn_unmanaged(dir, agent_id, &duduclaw_core::resolve_duduclaw_bin());
        }
        return Ok(Prepared::default());
    }

    let own = super::super::duduclaw_mcp_server_json_for_home(agent_id, home_dir)
        .ok_or_else(|| refuse("duduclaw binary did not resolve to an absolute path"))?;
    let own_toml = super::GrokRuntime::duduclaw_server_toml(agent_id, home_dir)
        .ok_or_else(|| refuse("duduclaw binary did not resolve to an absolute path"))?;
    let self_exe = PathBuf::from(own.get("command").and_then(|c| c.as_str()).unwrap_or(""));

    // The same set for a utility call as for the employee's own turn, so
    // concurrent spawns write identical content; a utility call simply gets
    // no env directory below. The gated servers and the employee's own
    // `.mcp.json` names come from one read of the file, after the gate.
    let (gated, agent_names) = match agent_dir {
        Some(dir) => gated_servers_and_names_checked(dir, home_dir, &own, explore).await?,
        None => (BTreeMap::new(), Vec::new()),
    };
    let scoped = spawn_scoped(&gated);

    for (dir, dir_names, other_names) in &other {
        let own_dir = Some(dir.as_path()) == agent_dir;
        // The employee's own list from the gated read; a borrowed root's from
        // its single read above (nothing there is gated).
        let mut names = if own_dir {
            agent_names.clone()
        } else {
            dir_names.clone()
        };
        names.extend(other_names.iter().cloned());
        names.sort();
        names.dedup();
        let desired = Desired {
            duduclaw: &own_toml,
            live: &scoped,
            shadow_names: &names,
            own_dir,
            self_exe: &self_exe,
            agent_id,
        };
        write_config(dir, home_dir, &desired).map_err(|e| refuse(&e))?;
    }

    let env_dir = if crate::runtime_dispatch::in_utility_call() {
        None
    } else {
        write_spawn_env_dir(home_dir, &scoped).map_err(|e| refuse(&e))?
    };
    Ok(Prepared {
        managed: true,
        env_dir,
    })
}

/// Warn (every spawn) about each `.grok/config.toml` entry in `dir` that is
/// neither `duduclaw` nor written by DuDuClaw: Grok starts it as written,
/// without `duduclaw mcp-proxy`. Read-only; an unreadable or malformed file
/// is skipped here (the managed path refuses it when it has to write).
fn warn_unmanaged(dir: &Path, agent_id: &str, self_exe: &Path) {
    let Ok(raw) = std::fs::read_to_string(dir.join(".grok").join("config.toml")) else {
        return;
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return;
    };
    if let Some(map) = doc.get("mcp_servers").and_then(|m| m.as_table()) {
        for name in unmanaged_names(map, self_exe) {
            log_unmanaged(agent_id, &name);
        }
    }
}

fn unmanaged_names(map: &toml::map::Map<String, toml::Value>, self_exe: &Path) -> Vec<String> {
    map.iter()
        .filter(|(name, def)| name.as_str() != "duduclaw" && !is_managed(def, self_exe))
        .map(|(name, _)| name.clone())
        .collect()
}

fn log_unmanaged(agent_id: &str, server: &str) {
    tracing::warn!(
        runtime = "grok",
        agent = %agent_id,
        server = %server,
        "grok .grok/config.toml holds an MCP server DuDuClaw did not write — Grok starts it as \
         written, without duduclaw mcp-proxy (no third-party tool gate)"
    );
}

/// What one directory's `[mcp_servers]` should hold.
pub(crate) struct Desired<'a> {
    pub duduclaw: &'a toml::Value,
    pub live: &'a BTreeMap<String, ScopedServer>,
    /// Every name the directory's `.mcp.json` lists.
    pub shadow_names: &'a [String],
    /// The employee's own directory (stale managed entries are pruned there;
    /// in a borrowed work root they are left for their owner).
    pub own_dir: bool,
    pub self_exe: &'a Path,
    /// For the unmanaged-entry warning.
    pub agent_id: &'a str,
}

/// The TOML table of a live spawn-scoped proxy.
pub(crate) fn live_entry(s: &ScopedServer) -> toml::Value {
    let mut env = toml::map::Map::new();
    for (k, v) in &s.proxy_env {
        env.insert(k.clone(), toml::Value::String(v.clone()));
    }
    // Values are resolved from this spawn, never written into this shared file.
    for key in [duduclaw_core::ENV_AGENT_TOKEN, duduclaw_core::ENV_TRUST_TURN_ID] {
        env.insert(key.into(), toml::Value::String(format!("${{{key}:-}}")));
    }
    env.insert(
        PROXY_UPSTREAM_ENV_DIR_VAR.to_string(),
        toml::Value::String(env_dir_reference()),
    );
    let mut t = toml::map::Map::new();
    t.insert("command".into(), toml::Value::String(s.command.clone()));
    t.insert(
        "args".into(),
        toml::Value::Array(s.args.iter().cloned().map(toml::Value::String).collect()),
    );
    t.insert("enabled".into(), toml::Value::Boolean(true));
    t.insert("env".into(), toml::Value::Table(env));
    toml::Value::Table(t)
}

/// The TOML table of a disabled shadow.
pub(crate) fn shadow_entry(self_exe: &Path) -> toml::Value {
    let mut args = vec![
        toml::Value::String("mcp-proxy".into()),
        toml::Value::String(PROXY_SPAWN_SCOPED_FLAG.into()),
        toml::Value::String("--server".into()),
    ];
    args.extend(
        SHADOW_UPSTREAM
            .iter()
            .map(|a| toml::Value::String((*a).into())),
    );
    let mut t = toml::map::Map::new();
    t.insert(
        "command".into(),
        toml::Value::String(self_exe.to_string_lossy().into_owned()),
    );
    t.insert("args".into(), toml::Value::Array(args));
    t.insert("enabled".into(), toml::Value::Boolean(false));
    toml::Value::Table(t)
}

fn is_managed(def: &toml::Value, self_exe: &Path) -> bool {
    let command = def.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let args: Vec<String> = def
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    crate::runtime::third_party_mcp::is_managed_entry(command, &args, self_exe)
}

/// Pure merge of `desired` into the `[mcp_servers]` table `map`. Returns
/// whether anything changed.
///
/// 1. `duduclaw` and every live server are set.
/// 2. A managed entry (a spawn-scoped proxy of this binary) that is no longer
///    live becomes a shadow when `.mcp.json` still lists its name; otherwise
///    it is removed in the employee's own directory and left alone in a
///    borrowed one.
/// 3. Every other `.mcp.json` name with no entry yet gets a shadow. A name
///    that already has an entry (managed, or one the operator added) already
///    outranks `.mcp.json`.
pub(crate) fn merge_servers(
    map: &mut toml::map::Map<String, toml::Value>,
    desired: &Desired,
) -> bool {
    fn set(map: &mut toml::map::Map<String, toml::Value>, name: &str, def: toml::Value) -> bool {
        if map.get(name) == Some(&def) {
            return false;
        }
        map.insert(name.to_string(), def);
        true
    }
    let mut changed = false;
    changed |= set(map, "duduclaw", desired.duduclaw.clone());
    for (name, s) in desired.live {
        changed |= set(map, name, live_entry(s));
    }
    let stale: Vec<String> = map
        .iter()
        .filter(|(name, def)| {
            name.as_str() != "duduclaw"
                && !desired.live.contains_key(name.as_str())
                && is_managed(def, desired.self_exe)
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in stale {
        if desired.shadow_names.contains(&name) {
            changed |= set(map, &name, shadow_entry(desired.self_exe));
        } else if desired.own_dir {
            map.remove(&name);
            changed = true;
        }
    }
    for name in desired.shadow_names {
        if name == "duduclaw" || desired.live.contains_key(name.as_str()) {
            continue;
        }
        if !map.contains_key(name.as_str()) {
            map.insert(name.clone(), shadow_entry(desired.self_exe));
            changed = true;
        }
    }
    // Any entry DuDuClaw did not write (an operator's own, same name as a
    // `.mcp.json` server or not) outranks `.mcp.json` and is started by Grok
    // as written, unproxied. It is the operator's file, so it is kept, but
    // said out loud on every spawn.
    for name in unmanaged_names(map, desired.self_exe) {
        log_unmanaged(desired.agent_id, &name);
    }
    changed
}

/// Read-merge-write `<dir>/.grok/config.toml` under a lock kept in the
/// DuDuClaw home (not in the agent-writable directory). Using
/// [`crate::fs_safe::SafeDir`] prevents symlink traversal: a symlinked `.grok` or
/// `config.toml` is refused, the file is replaced atomically, a new file is
/// 0600 and an existing one is reset to owner-only. Other tables are kept.
pub(crate) fn write_config(dir: &Path, home_dir: &Path, desired: &Desired) -> Result<bool, String> {
    use crate::fs_safe::SafeDir;
    let lock = lock_path(home_dir, dir);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    duduclaw_core::with_file_lock(&lock, || -> std::io::Result<bool> {
        let root = SafeDir::open_root(dir)?;
        let grok = root
            .child_dir(".grok", true)?
            .ok_or_else(|| std::io::Error::other(".grok could not be created"))?;
        let existing = grok.read_file("config.toml")?.unwrap_or_default();
        let mut doc: toml::Value = if existing.trim().is_empty() {
            toml::Value::Table(toml::map::Map::new())
        } else {
            toml::from_str(&existing).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed grok config.toml: {e}"),
                )
            })?
        };
        let root_tbl = doc.as_table_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "grok config.toml root is not a table",
            )
        })?;
        let mcp = root_tbl
            .entry("mcp_servers".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if !mcp.is_table() {
            *mcp = toml::Value::Table(toml::map::Map::new());
        }
        let map = mcp
            .as_table_mut()
            .expect("mcp_servers normalized to a table above");
        if !merge_servers(map, desired) {
            restrict(dir);
            return Ok(false);
        }
        let rendered = toml::to_string_pretty(&doc).map_err(std::io::Error::other)?;
        grok.write_file_atomic("config.toml", &rendered)?;
        restrict(dir);
        Ok(true)
    })
    .map_err(|e| e.to_string())
}

/// Owner-only for the config (it carries `DUDUCLAW_MCP_API_KEY` and the
/// agent token of the `duduclaw` entry); only a regular file is touched.
fn restrict(dir: &Path) {
    let path = dir.join(".grok").join("config.toml");
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
        duduclaw_core::platform::set_owner_only(&path).ok();
    }
}

/// Lock for one directory's `.grok/config.toml`, under the DuDuClaw home and
/// keyed by a hash of the canonical directory.
pub(crate) fn lock_path(home_dir: &Path, dir: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let digest = Sha256::digest(canon.to_string_lossy().as_bytes());
    home_dir
        .join("locks")
        .join(format!("grok-mcp-{}", hex::encode(&digest[..16])))
}

#[cfg(test)]
mod tests;
