//! Spawn-time `.mcp.json` rewrite that routes third-party **stdio** MCP
//! servers through `duduclaw mcp-proxy`.
//!
//! Rewriting takes trusted host context separately from raw configuration.
//! Lives in `duduclaw-core` so both the gateway
//! (`redaction_proxy::maybe_proxy_mcp_config*`, every Claude CLI spawn) and
//! `duduclaw-agent` (the heartbeat proactive check, which runs in the explore
//! lane) can apply it; `duduclaw-agent` does not depend on the gateway.
//!
//! Three reasons put the proxy in the path, and the rewrite differs in
//! what it does with `duduclaw mcp-remote-bridge` entries:
//!
//! - **Redaction** (RFC-23 §13.6): every third-party stdio entry is wrapped,
//!   bridges included, because the bridge itself does not redact.
//! - **Third-party tool gate** (2026-10-08: `action_rules`, explore lane):
//!   bridges are left alone, because the bridge applies the same gate itself
//!   and wrapping it would ask twice; direct remote entries (`url` / `type`)
//!   are removed, because nothing can gate them
//!   ([`rewrite_mcp_config_for_gate`]).
//! - **Secret env references**: resolve only in the proxy before upstream
//!   spawn; bridges carrying references are wrapped too, and direct remote
//!   entries with references are removed because they cannot resolve them.

use std::path::Path;

use serde_json::{Map, Value, json};

/// Env var carrying the wrapped server's original `.mcp.json` `env` map,
/// JSON-encoded, to the proxy process. Must match
/// `duduclaw_cli::mcp_proxy::PROXY_UPSTREAM_ENV_VAR`.
pub const PROXY_UPSTREAM_ENV_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV";

/// Env var naming a file that holds the same JSON map as
/// [`PROXY_UPSTREAM_ENV_VAR`]; read by the proxy only when that variable is
/// absent. Used where the server definition travels on a command line (Codex
/// `-c` overrides): the upstream server's own credentials then sit in an
/// owner-only per-spawn file, not in `argv`. Must match
/// `duduclaw_cli::mcp_proxy::PROXY_UPSTREAM_ENV_FILE_VAR`.
pub const PROXY_UPSTREAM_ENV_FILE_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV_FILE";

/// Env var naming a per-spawn, owner-only **directory** that holds one
/// `<server>.json` per third-party server ([`spawn_env_file_name`]), each the
/// same JSON map as [`PROXY_UPSTREAM_ENV_VAR`]. Used by runtimes whose MCP
/// registration is a persistent file shared by every spawn of the employee
/// (Grok `.grok/config.toml`,
/// 2026-10-20): the file names only this variable (or nothing, when the CLI
/// hands its own environment to MCP children), the spawn sets it. Read by a
/// proxy started with [`PROXY_SPAWN_SCOPED_FLAG`]. Must match
/// `duduclaw_cli::mcp_proxy::PROXY_UPSTREAM_ENV_DIR_VAR`.
pub const PROXY_UPSTREAM_ENV_DIR_VAR: &str = "DUDUCLAW_MCP_PROXY_ENV_DIR";

/// `duduclaw mcp-proxy` flag for an entry that lives in a persistent CLI
/// config: the proxy starts its upstream only when [`PROXY_UPSTREAM_ENV_DIR_VAR`]
/// names a directory holding this server's file, and otherwise exits without
/// starting anything. So the persistent entry is inert outside a DuDuClaw
/// spawn that registered the server (a utility call, a manual CLI run, a spawn
/// of another employee in the same directory).
pub const PROXY_SPAWN_SCOPED_FLAG: &str = "--spawn-scoped";

/// Longest third-party server name a runtime registers (same bound as
/// `mcp_scan::is_valid_mcp_server_name`).
pub const MAX_PORTABLE_SERVER_NAME_LEN: usize = 64;

/// Is `name` usable as-is in every CLI config format DuDuClaw writes (a bare
/// TOML key, a JSON key) and as a file name?
/// ASCII letters, digits, `_` and `-`, 1–64 characters.
pub fn is_portable_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PORTABLE_SERVER_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The file name of `server`'s upstream env inside the
/// [`PROXY_UPSTREAM_ENV_DIR_VAR`] directory, or `None` when the name is not
/// [`is_portable_server_name`] (never a path separator, never `..`).
pub fn spawn_env_file_name(server: &str) -> Option<String> {
    is_portable_server_name(server).then(|| format!("{server}.json"))
}

/// Env names copied from the host-generated built-in definition onto every
/// rewritten proxy entry, so the proxy process resolves the same home, the
/// same agent identity and the same MCP credential the built-in server does.
///
/// `DUDUCLAW_AGENT_TOKEN` is deliberately absent from this persistent carry
/// set. The host supplies it and the turn identity separately at spawn for
/// the proxy's signed egress-ledger requests; the external upstream never
/// inherits these values.
pub const PROXY_CARRY_ENV: &[&str] = &[
    "DUDUCLAW_HOME",
    "DUDUCLAW_PORT",
    "DUDUCLAW_INSTANCE",
    "DUDUCLAW_AGENT_ID",
    "DUDUCLAW_MCP_API_KEY",
    "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
];

/// The `duduclaw` subcommand of a remote MCP bridge entry.
pub const REMOTE_BRIDGE_SUBCOMMAND: &str = "mcp-remote-bridge";

/// Exact host-owned names reserve built-in slots, regardless of raw contents.
/// This classifies a slot, not a trusted command: effective configs must replace
/// its entire definition with the host-generated canonical server.
pub fn is_builtin_slot_name(name: &str) -> bool {
    name == "duduclaw" || name == "duduclaw-pro" || name == crate::mcp_server_key()
}

/// Strict host-owned classification: only an exact built-in name invoking this
/// binary with exactly `mcp-server` may keep its host-owned environment.
/// Slot replacement uses `is_builtin_slot_name` instead; a reserved name alone
/// must never exempt third-party credentials from encryption at rest.
pub fn is_host_owned_entry(name: &str, def: &Value, self_exe: &Path) -> bool {
    is_builtin_slot_name(name)
        && def.get("command").and_then(Value::as_str)
            .is_some_and(|command| Some(command) == self_exe.to_str())
        && def.get("args") == Some(&json!(["mcp-server"]))
}

/// The host-generated built-in definition; never derive identity from raw JSON.
pub fn canonical_duduclaw_server(home: &Path, agent: &str, self_exe: &Path) -> Value {
    let mut env = Map::new();
    for (key, value) in crate::agent_identity_env_vars(home, agent)
        .into_iter().chain(crate::mcp_forward_env_vars())
    {
        env.insert(key, Value::String(value));
    }
    env.insert("DUDUCLAW_HOME".into(), json!(home.to_string_lossy()));
    json!({"command": self_exe.to_string_lossy(), "args": ["mcp-server"], "env": env})
}

/// These fields belong only to host-generated, per-spawn proxy definitions.
pub fn is_proxy_private_env_key(name: &str) -> bool {
    // Windows env lookup is case-insensitive; use the same boundary everywhere.
    let name = name.to_ascii_uppercase();
    name.starts_with("DUDUCLAW_MCP_PROXY_ENV")
        || name.starts_with("DUDUCLAW_AGENT_")
        || name == "DUDUCLAW_ON_BEHALF_OF"
}

fn has_private_env(def: &Value) -> bool {
    def.get("env").and_then(Value::as_object)
        .is_some_and(|env| env.keys().any(|key| is_proxy_private_env_key(key)))
}

fn invokes_proxy(def: &Value) -> bool {
    def.get("args").and_then(Value::as_array)
        .and_then(|args| args.first()).and_then(Value::as_str) == Some("mcp-proxy")
}

/// Validate the raw file before any repair, substitution, or rewrite. Even a
/// plausible proxy is refused: only the host may manufacture proxy entries.
/// Built-in slots are replaced wholesale; their raw command and env confer no trust.
pub fn validate_raw_mcp_config(config: &Value, _self_exe: &Path, _own_server: &Value) -> Result<(), String> {
    let Some(servers) = config.get("mcpServers").and_then(Value::as_object) else {
        return Ok(());
    };
    for (name, def) in servers {
        if invokes_proxy(def) || (has_private_env(def)
            && !is_builtin_slot_name(name))
        {
            return Err(format!("MCP server '{name}': raw configuration contains a proxy or untrusted host identity"));
        }
    }
    Ok(())
}

/// Is `(command, args)` DuDuClaw's own remote MCP bridge? The command must
/// be exactly `self_exe` (the binary that writes the entry), so another
/// program that happens to take `mcp-remote-bridge` as its first argument is
/// not mistaken for it.
pub fn is_remote_bridge_invocation(command: &str, args: &[String], self_exe: &Path) -> bool {
    args.first().map(String::as_str) == Some(REMOTE_BRIDGE_SUBCOMMAND)
        && Path::new(command) == self_exe
}

/// Whether a third-party server declares unresolved env references.
/// Args and URLs are deliberately outside this contract.
pub fn server_has_secret_refs(def: &Value) -> bool {
    fn contains_ref(value: &Value, depth: usize) -> bool {
        match value {
            Value::String(s) if s.contains("secret://") => true,
            Value::String(s) if depth < 8 => serde_json::from_str::<Value>(s)
                .is_ok_and(|nested| contains_ref(&nested, depth + 1)),
            Value::Object(map) => map.values().any(|v| contains_ref(v, depth + 1)),
            Value::Array(values) => values.iter().any(|v| contains_ref(v, depth + 1)),
            _ => false,
        }
    }
    def.get("env").is_some_and(|env| contains_ref(env, 0))
}

/// Includes JSON-encoded nested proxy env; raw files cannot hide references
/// inside a purported already-proxied server or a built-in-looking name.
pub fn has_mcp_secret_refs(config: &Value) -> bool {
    config.get("mcpServers").and_then(Value::as_object).is_some_and(|servers| {
        servers.values().any(server_has_secret_refs)
    })
}

/// Post-rewrite check: only host-generated proxy envelopes can retain refs.
/// Must only be used on a successful rewrite, never to classify raw input.
pub fn has_unrouted_mcp_secret_refs(config: &Value) -> bool {
    config.get("mcpServers").and_then(Value::as_object).is_some_and(|servers| {
        servers.values().any(|def| !invokes_proxy(def) && server_has_secret_refs(def))
    })
}

/// Why the proxy is put in the path (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProxyPurpose {
    /// RFC-23 redaction is active: wrap every third-party stdio entry.
    pub redaction: bool,
    /// The third-party tool gate needs the proxy: wrap every entry except
    /// remote bridges (which gate themselves).
    pub tool_gate: bool,
    /// Third-party env references must be resolved by the proxy before spawn.
    pub secret_env: bool,
}

impl ProxyPurpose {
    /// Nothing to do.
    pub fn none(&self) -> bool {
        !self.redaction && !self.tool_gate && !self.secret_env
    }
}

/// Rewrite every third-party **stdio** server so it launches through
/// `duduclaw mcp-proxy` for `purpose`.
///
/// Built-in slots are replaced with `own_server`. Left untouched: `type` /
/// `url` entries (no child
/// process to wrap), entries with no `command`, and, when redaction is not a purpose, remote
/// bridge entries. The original `env` map rides along in
/// [`PROXY_UPSTREAM_ENV_VAR`] rather than in argv.
pub fn rewrite_mcp_config_for_proxy_with(json: &Value, self_exe: &Path, purpose: ProxyPurpose, own_server: &Value) -> Result<Value, String> {
    Ok(rewrite_mcp_config_for_gate(json, self_exe, purpose, own_server)?.config)
}

/// What [`rewrite_mcp_config_for_gate`] produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyRewrite {
    /// The config to hand the Claude CLI.
    pub config: Value,
    /// Direct remote entries (`url` / `type`) removed because the
    /// third-party tool gate was a purpose and nothing can gate them. Sorted
    /// server names; the caller audits each one.
    pub dropped_remote: Vec<String>,
}

/// Is this `.mcp.json` entry a direct remote server (`url`, or a `type`
/// such as `http` / `sse`) that the Claude CLI would reach without any
/// child process DuDuClaw could wrap?
pub fn is_direct_remote_entry(def: &Value) -> bool {
    def.get("url").is_some() || def.get("type").is_some()
}

/// [`rewrite_mcp_config_for_proxy_with`] that also reports what it removed.
///
/// Direct remote entries (`url` / `type: http|sse`, not DuDuClaw's
/// `mcp-remote-bridge`) are reached by the Claude CLI itself, so neither
/// redaction nor the third-party tool gate sees their tools. When the tool
/// gate is a purpose (`action_rules` present or the explore lane) they are
/// **removed** from the spawn config (2026-10-08 close-out): the operator
/// connects them with `mcp.remote_connect`, which writes a bridge entry the
/// gate does cover. They are not rewritten to the bridge on the fly because
/// the bridge reads its URL and credentials from the encrypted remote store,
/// and a spawn must not create store records or probe the network. With
/// redaction as the only purpose they stay (logged), as before.
pub fn rewrite_mcp_config_for_gate(json: &Value, self_exe: &Path, purpose: ProxyPurpose, own_server: &Value) -> Result<ProxyRewrite, String> {
    rewrite_mcp_config_with_slots(json, self_exe, purpose, own_server, true)
}

/// Rewrite for runtimes that register the canonical server separately. Only
/// strict host-owned entries are replaced; third-party names keep their gates.
pub fn rewrite_mcp_config_for_runtime_gate(json: &Value, self_exe: &Path, purpose: ProxyPurpose, own_server: &Value) -> Result<ProxyRewrite, String> {
    rewrite_mcp_config_with_slots(json, self_exe, purpose, own_server, false)
}

fn rewrite_mcp_config_with_slots(json: &Value, self_exe: &Path, purpose: ProxyPurpose, own_server: &Value, replace_builtin_slots: bool) -> Result<ProxyRewrite, String> {
    // The caller supplies the host's canonical definition, never one read out
    // of the raw file. This API accepts raw input only, even with no purpose.
    validate_raw_mcp_config(json, self_exe, own_server)?;
    let mut out = json.clone();
    let mut dropped_remote = Vec::new();
    let exe = self_exe.to_string_lossy().into_owned();

    let Some(servers) = out.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
        return Ok(ProxyRewrite { config: out, dropped_remote });
    };

    let carry = carry_env(own_server);
    let names: Vec<String> = servers.keys().cloned().collect();

    for name in names {
        let Some(def) = servers.get(&name).cloned() else {
            continue;
        };
        if (replace_builtin_slots && is_builtin_slot_name(&name))
            || is_host_owned_entry(&name, &def, self_exe)
        {
            servers.insert(name, own_server.clone());
            continue;
        }
        // Runtime registrations do not replace untrusted built-in slots.
        // A stale or forged host entry; the runtime registers its canonical
        // server separately, so dropping it means it can neither run nor leak identity.
        if !replace_builtin_slots && is_builtin_slot_name(&name) && has_private_env(&def) {
            servers.remove(&name);
            continue;
        }
        // Slot names without private host env remain ordinary third-party entries.
        if has_private_env(&def) {
            return Err(format!("MCP server '{name}': raw configuration contains a proxy or untrusted host identity"));
        }
        if purpose.none() {
            continue;
        }
        if is_direct_remote_entry(&def) {
            if purpose.tool_gate || (purpose.secret_env && server_has_secret_refs(&def)) {
                tracing::warn!(
                    server = %name,
                    "direct HTTP/SSE MCP server removed from this spawn — the third-party tool gate cannot reach it; connect it with mcp.remote_connect"
                );
                servers.remove(&name);
                dropped_remote.push(name);
            } else {
                tracing::warn!(
                    server = %name,
                    "HTTP/SSE MCP server is NOT proxied — redaction does not reach it"
                );
            }
            continue;
        }
        let Some(command) = def.get("command").and_then(|c| c.as_str()) else {
            continue;
        };
        let orig_args: Vec<String> = def
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if !purpose.redaction && !server_has_secret_refs(&def) && is_remote_bridge_invocation(command, &orig_args, self_exe) {
            continue;
        }

        let mut args = vec![
            json!("mcp-proxy"),
            json!("--server"),
            json!(name),
            json!("--"),
            json!(command),
        ];
        if let Some(orig) = def.get("args").and_then(|a| a.as_array()) {
            args.extend(orig.iter().cloned());
        }

        let original_env = def
            .get("env")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let mut env = carry.clone();
        env.insert(
            PROXY_UPSTREAM_ENV_VAR.to_string(),
            Value::String(original_env.to_string()),
        );

        servers.insert(
            name,
            json!({ "command": exe, "args": args, "env": Value::Object(env) }),
        );
    }
    dropped_remote.sort();
    Ok(ProxyRewrite { config: out, dropped_remote })
}

/// Only the host's canonical definition supplies identity and home.
fn carry_env(def: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(env) = def.get("env").and_then(|e| e.as_object()) else {
        return out;
    };
    for key in PROXY_CARRY_ENV {
        if let Some(v) = env.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(exe: &str) -> Value {
        json!({ "mcpServers": {
            "duduclaw": { "command": exe, "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "a1", "DUDUCLAW_AGENT_TOKEN": "t" } },
            "pg": { "command": "npx", "args": ["-y", "pg-mcp"], "env": { "PGPASSWORD": "x" } },
            "zap": { "command": exe, "args": ["mcp-remote-bridge", "--agent", "a1", "--server", "zap"] },
            "web": { "url": "https://example.com/mcp" }
        }})
    }

    #[test]
    fn gate_only_rewrite_skips_bridges_and_redaction_wraps_them() {
        let exe = "/opt/duduclaw";
        let p = Path::new(exe);
        let gate = rewrite_mcp_config_for_proxy_with(&cfg(exe), p, ProxyPurpose { redaction: false, tool_gate: true, secret_env: false }, &cfg(exe)["mcpServers"]["duduclaw"]).unwrap();
        assert_eq!(gate["mcpServers"]["pg"]["args"][0], "mcp-proxy");
        assert_eq!(gate["mcpServers"]["zap"]["args"][0], "mcp-remote-bridge");
        assert!(gate["mcpServers"].get("web").is_none(), "a direct remote entry cannot be gated");
        assert!(gate["mcpServers"]["pg"]["env"].get("DUDUCLAW_AGENT_TOKEN").is_none());
        assert_eq!(gate["mcpServers"]["pg"]["env"]["DUDUCLAW_AGENT_ID"], "a1");
        let red = rewrite_mcp_config_for_proxy_with(&cfg(exe), p, ProxyPurpose { redaction: true, tool_gate: false, secret_env: false }, &cfg(exe)["mcpServers"]["duduclaw"]).unwrap();
        assert_eq!(red["mcpServers"]["zap"]["args"][0], "mcp-proxy");
        let none = rewrite_mcp_config_for_proxy_with(&cfg(exe), p, ProxyPurpose { redaction: false, tool_gate: false, secret_env: false }, &cfg(exe)["mcpServers"]["duduclaw"]).unwrap();
        assert_eq!(none, cfg(exe));
    }

    #[test]
    fn the_gate_reports_every_direct_remote_entry_it_drops() {
        let exe = "/opt/duduclaw";
        let mut c = cfg(exe);
        c["mcpServers"]["sse"] = json!({ "type": "sse", "url": "https://x.example/sse" });
        c["mcpServers"]["http"] = json!({ "type": "http", "url": "https://y.example/mcp" });
        let r = rewrite_mcp_config_for_gate(&c, Path::new(exe), ProxyPurpose { redaction: true, tool_gate: true, secret_env: false }, &cfg(exe)["mcpServers"]["duduclaw"]).unwrap();
        assert_eq!(r.dropped_remote, vec!["http", "sse", "web"]);
        for n in ["http", "sse", "web"] {
            assert!(r.config["mcpServers"].get(n).is_none());
        }
        // The bridge and the duduclaw entry survive.
        assert!(r.config["mcpServers"].get("zap").is_some());
        assert!(r.config["mcpServers"].get("duduclaw").is_some());
        // Redaction alone keeps them (the old behaviour).
        let r = rewrite_mcp_config_for_gate(&c, Path::new(exe), ProxyPurpose { redaction: true, tool_gate: false, secret_env: false }, &cfg(exe)["mcpServers"]["duduclaw"]).unwrap();
        assert!(r.dropped_remote.is_empty());
        assert_eq!(r.config["mcpServers"]["web"], c["mcpServers"]["web"]);
    }

    #[test]
    fn secret_purpose_wraps_bridges_and_drops_unresolvable_remote_entries() {
        let exe = Path::new("/opt/duduclaw");
        let mut config = cfg(exe.to_str().unwrap());
        config["mcpServers"]["zap"]["env"] = json!({"API_KEY":"secret://mcp/zap/API_KEY"});
        config["mcpServers"]["web"]["env"] = json!({"API_KEY":"secret://mcp/web/API_KEY"});
        let out = rewrite_mcp_config_for_gate(&config, exe,
            ProxyPurpose { redaction: false, tool_gate: false, secret_env: true }, &cfg(exe.to_str().unwrap())["mcpServers"]["duduclaw"]).unwrap();
        assert_eq!(out.config["mcpServers"]["zap"]["args"][0], "mcp-proxy");
        assert_eq!(out.dropped_remote, vec!["web"]);
        assert!(has_mcp_secret_refs(&out.config));
        assert!(!has_unrouted_mcp_secret_refs(&out.config));
        assert_eq!(out.config["mcpServers"]["duduclaw"], config["mcpServers"]["duduclaw"]);
    }

    #[test]
    fn raw_proxies_and_private_identity_are_refused_even_without_a_purpose() {
        let exe = Path::new("/opt/duduclaw");
        let none = ProxyPurpose { redaction: false, tool_gate: false, secret_env: false };
        let nested = json!({"API_KEY":"secret://mcp/other-agent/vendor/API_KEY"}).to_string();
        for entry in [
            json!({"command":exe, "args":["mcp-proxy", "--server", "vendor", "--", "evil"],
                "env":{"DUDUCLAW_AGENT_ID":"victim", PROXY_UPSTREAM_ENV_VAR:nested}}),
            json!({"command":"evil", "env":{PROXY_UPSTREAM_ENV_VAR:nested}}),
            json!({"command":"evil", "env":{"DUDUCLAW_AGENT_ID":"victim"}}),
            json!({"command":"evil", "env":{"duduclaw_agent_id":"victim"}}),
            json!({"command":"evil", "env":{"duduclaw_mcp_proxy_env":nested}}),
            json!({"command":"evil", "env":{"DUDUCLAW_MCP_PROXY_ENV_FILE":"/tmp/stolen.json"}}),
            json!({"command":"evil", "env":{"DUDUCLAW_MCP_PROXY_ENV_DIR":"/tmp/stolen"}}),
        ] {
            let raw = json!({"mcpServers":{"vendor":entry}});
            assert!(rewrite_mcp_config_for_gate(&raw, exe, none, &Value::Null).is_err());
            assert!(validate_raw_mcp_config(&raw, exe, &Value::Null).is_err());
        }
        let raw = json!({"mcpServers":{"duduclaw-evil":{"command":"evil", "env":{PROXY_UPSTREAM_ENV_VAR:nested}}}});
        assert!(has_mcp_secret_refs(&raw), "the raw scanner must inspect encoded nested env");
    }

    #[test]
    fn builtin_slots_are_replaced_wholesale_and_prefixes_are_not_owned() {
        let home = std::env::temp_dir().join("duduclaw-canonical-test-nonexistent");
        let exe = Path::new("/opt/duduclaw");
        let own = canonical_duduclaw_server(&home, "alice", exe);
        let purpose = ProxyPurpose { redaction: false, tool_gate: true, secret_env: false };
        for name in ["duduclaw".to_string(), "duduclaw-pro".into(), crate::mcp_server_key()] {
            let forged = json!({"command":"/tmp/evil", "args":["run"], "url":"https://evil.test",
                "env":{"DUDUCLAW_AGENT_ID":"victim", "DUDUCLAW_MCP_PROXY_ENV_FILE":"/tmp/stolen", "LD_PRELOAD":"evil"}});
            assert!(is_builtin_slot_name(&name));
            let raw = json!({"mcpServers":{name.clone():forged}});
            // Raw slot contents are discarded, including private host env.
            assert!(validate_raw_mcp_config(&raw, exe, &own).is_ok());
            let rewritten = rewrite_mcp_config_for_gate(&raw, exe, purpose, &own).unwrap();
            assert_eq!(rewritten.config["mcpServers"][&name], own);
            let raw_proxy = json!({"mcpServers":{name:{"command":exe,"args":["mcp-proxy"]}}});
            assert!(validate_raw_mcp_config(&raw_proxy, exe, &own).is_err());
        }
        assert!(!is_builtin_slot_name("duduclaw-evil"));
        let raw = json!({"mcpServers":{"duduclaw-evil":own}});
        assert!(validate_raw_mcp_config(&raw, exe, &own).is_err());
    }

    #[test]
    fn runtime_gate_drops_private_env_in_untrusted_builtin_slots_and_proxies_foreign_commands() {
        let exe = Path::new("/opt/duduclaw");
        let own = json!({"command":exe, "args":["mcp-server"]});
        for name in ["duduclaw".to_string(), "duduclaw-pro".into(), crate::mcp_server_key()] {
            for entry in [
                json!({"command":"/old/duduclaw-pro", "args":["mcp-server"],
                    "env":{"DUDUCLAW_AGENT_ID":"victim"}}),
                json!({"url":"https://evil.test", "env":{"duduclaw_mcp_proxy_env_file":"/tmp/stolen"}}),
            ] {
                let raw = json!({"mcpServers":{name.clone():entry}});
                assert!(validate_raw_mcp_config(&raw, exe, &own).is_ok());
                let purpose = ProxyPurpose { redaction: false, tool_gate: true, secret_env: true };
                let rewritten = rewrite_mcp_config_for_runtime_gate(&raw, exe, purpose, &own).unwrap();
                assert!(rewritten.config["mcpServers"].get(&name).is_none());
                let rewritten = rewrite_mcp_config_for_gate(&raw, exe, purpose, &own).unwrap();
                assert_eq!(rewritten.config["mcpServers"][&name], own);
            }
        }
        let name = "duduclaw";
        let raw = json!({"mcpServers":{name:{"command":"/foreign/server", "args":["serve"]}}});
        let proxied = rewrite_mcp_config_for_runtime_gate(
            &raw, exe, ProxyPurpose { redaction: false, tool_gate: true, secret_env: false }, &own,
        ).unwrap();
        assert_eq!(proxied.config["mcpServers"][name]["command"], exe.to_string_lossy().as_ref());
        assert_eq!(proxied.config["mcpServers"][name]["args"][0], "mcp-proxy");
    }

    #[test]
    fn host_owned_exemption_requires_exact_name_command_and_args() {
        let exe = Path::new("/opt/duduclaw");
        let own = json!({"command":exe, "args":["mcp-server"]});
        for name in ["duduclaw".to_string(), "duduclaw-pro".into(), crate::mcp_server_key()] {
            assert!(is_host_owned_entry(&name, &own, exe));
            for entry in [
                json!({"command":"/tmp/evil", "args":["mcp-server"]}),
                json!({"command":exe}),
                json!({"command":exe, "args":["mcp-server", "--extra"]}),
                json!({"command":exe, "args":["mcp-proxy"]}),
                json!({"args":["mcp-server"]}),
            ] {
                assert!(!is_host_owned_entry(&name, &entry, exe));
                assert!(is_builtin_slot_name(&name));
            }
        }
        assert!(!is_host_owned_entry("duduclaw-evil", &own, exe));
    }

    #[test]
    fn a_rewritten_config_is_never_accepted_as_raw_input() {
        let exe = Path::new("/opt/duduclaw");
        let purpose = ProxyPurpose { redaction: true, tool_gate: true, secret_env: true };
        let once = rewrite_mcp_config_for_gate(&cfg(exe.to_str().unwrap()), exe, purpose, &cfg(exe.to_str().unwrap())["mcpServers"]["duduclaw"]).unwrap();
        assert!(rewrite_mcp_config_for_gate(&once.config, exe, purpose, &cfg(exe.to_str().unwrap())["mcpServers"]["duduclaw"]).is_err());
    }

    #[test]
    fn spawn_env_file_names_are_plain_components() {
        assert_eq!(spawn_env_file_name("masterlink").as_deref(), Some("masterlink.json"));
        assert_eq!(spawn_env_file_name("a-b_C9").as_deref(), Some("a-b_C9.json"));
        for bad in ["", "..", "a/b", "a\\b", "bad name", "dotted.name", "名稱", &"x".repeat(65)] {
            assert!(spawn_env_file_name(bad).is_none(), "{bad:?}");
            assert!(!is_portable_server_name(bad), "{bad:?}");
        }
        assert!(is_portable_server_name(&"x".repeat(64)));
    }

    #[test]
    fn a_bridge_is_recognised_only_from_this_binary() {
        let args = vec!["mcp-remote-bridge".to_string()];
        assert!(is_remote_bridge_invocation("/opt/duduclaw", &args, Path::new("/opt/duduclaw")));
        assert!(!is_remote_bridge_invocation("/tmp/evil", &args, Path::new("/opt/duduclaw")));
        assert!(!is_remote_bridge_invocation("/opt/duduclaw", &["mcp-server".into()], Path::new("/opt/duduclaw")));
    }
}
