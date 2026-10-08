//! The `.mcp.json` entry for a remote MCP server.
//!
//! Installed form (what the Claude CLI starts):
//!
//! ```json
//! { "command": "<absolute duduclaw>",
//!   "args": ["mcp-remote-bridge", "--agent", "<id>", "--server", "<name>"],
//!   "env": { "DUDUCLAW_HOME": "<home>" } }
//! ```
//!
//! No URL, token or key appears in the entry (argv is world-readable through
//! `/proc/<pid>/cmdline`); the bridge reads everything from the encrypted
//! store. `DUDUCLAW_HOME` is in `env` because the employee's spawn
//! environment is scrubbed and does not carry it.
//!
//! Candidate form (what a manifest parse produces before anyone has picked
//! an employee): `{"command": "duduclaw", "args": ["mcp-remote-bridge",
//! "--url", "<url>"]}`. [`prepare_for_install`] turns a candidate into the
//! installed form and records the URL in the store (status `not_connected`
//! until an operator connects it from the dashboard).

use std::path::Path;

use duduclaw_agent::mcp_template::McpServerDef;

use super::store::{self, AuthKind, ConnStatus, RemoteSecrets, RemoteServerRecord};

/// The hidden CLI subcommand that serves the bridge.
pub const BRIDGE_SUBCOMMAND: &str = "mcp-remote-bridge";
/// Carries the URL in a candidate definition only (never installed).
pub const URL_FLAG: &str = "--url";

/// A candidate definition for a remote URL found in a manifest.
pub fn candidate_def(url: &str) -> McpServerDef {
    McpServerDef {
        command: "duduclaw".to_string(),
        args: vec![
            BRIDGE_SUBCOMMAND.to_string(),
            URL_FLAG.to_string(),
            url.to_string(),
        ],
        env: Default::default(),
    }
}

/// Whether a definition is a bridge entry (candidate or installed): its
/// command is a `duduclaw` binary (by file name, or exactly the binary this
/// process resolves, which a `DUDUCLAW_BIN` override may name differently)
/// and its first argument is the subcommand.
pub fn is_bridge_def(def: &McpServerDef) -> bool {
    if def.args.first().map(String::as_str) != Some(BRIDGE_SUBCOMMAND) {
        return false;
    }
    let cmd = Path::new(def.command.trim());
    let stem = cmd.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    stem.eq_ignore_ascii_case("duduclaw")
        || stem.eq_ignore_ascii_case("duduclaw-pro")
        || cmd == duduclaw_core::resolve_duduclaw_bin().as_path()
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].as_str())
}

/// The URL a candidate carries.
pub fn carried_url(def: &McpServerDef) -> Option<&str> {
    if !is_bridge_def(def) {
        return None;
    }
    flag_value(&def.args, URL_FLAG)
}

/// `(agent, server)` of an installed bridge entry, as JSON (doctor reads
/// `.mcp.json` as raw JSON).
pub fn installed_target(entry: &serde_json::Value) -> Option<(String, String)> {
    let def: McpServerDef = serde_json::from_value(entry.clone()).ok()?;
    if !is_bridge_def(&def) {
        return None;
    }
    let agent = flag_value(&def.args, "--agent")?.to_string();
    let server = flag_value(&def.args, "--server")?.to_string();
    Some((agent, server))
}

/// The installed definition for (employee, server).
pub fn installed_def(home: &Path, agent_id: &str, server: &str) -> Result<McpServerDef, String> {
    store::validate_ids(agent_id, server)?;
    let exe = duduclaw_core::resolve_duduclaw_bin();
    if !exe.is_absolute() {
        return Err(format!(
            "the duduclaw binary path could not be resolved to an absolute path ({})",
            exe.display()
        ));
    }
    let mut env = std::collections::HashMap::new();
    env.insert("DUDUCLAW_HOME".to_string(), home.to_string_lossy().into_owned());
    Ok(McpServerDef {
        command: exe.to_string_lossy().into_owned(),
        args: vec![
            BRIDGE_SUBCOMMAND.to_string(),
            "--agent".to_string(),
            agent_id.to_string(),
            "--server".to_string(),
            server.to_string(),
        ],
        env,
    })
}

/// Turn a definition about to be written into `<agent>/.mcp.json` into its
/// installed form. Non-bridge definitions are returned unchanged.
///
/// For a bridge definition: the carried URL (if any) is validated and
/// recorded in the store for (employee, server) — a new record, or the
/// existing record reset to `not_connected` when the URL changed; the same
/// URL keeps the existing connection. No URL and no record ⇒ error.
/// Returns the installed definition and whether the server still needs to be
/// connected from the dashboard.
pub fn prepare_for_install(
    home: &Path,
    agent_id: &str,
    server: &str,
    def: &McpServerDef,
) -> Result<(McpServerDef, bool), String> {
    if !is_bridge_def(def) {
        return Ok((def.clone(), false));
    }
    store::validate_ids(agent_id, server)?;
    let existing = store::get(home, agent_id, server)?;
    let needs_connect = match carried_url(def) {
        Some(raw) => {
            let url = super::url_policy::validate_remote_url(raw)?;
            let same = match &existing {
                Some(rec) => store::open(home, rec).map(|s| s.url == url.as_str()).unwrap_or(false),
                None => false,
            };
            if same {
                existing.as_ref().is_none_or(|r| r.status != ConnStatus::Connected)
            } else {
                let now = store::now_rfc3339();
                let secrets = RemoteSecrets { url: url.to_string(), bearer: None, oauth: None, headers: vec![] };
                store::upsert(
                    home,
                    RemoteServerRecord {
                        agent_id: agent_id.to_string(),
                        server: server.to_string(),
                        auth: AuthKind::None,
                        host: super::http::host_label(&url),
                        status: ConnStatus::NotConnected,
                        created_at: existing.as_ref().map(|r| r.created_at.clone()).unwrap_or_else(|| now.clone()),
                        updated_at: now,
                        access_expires_at: None,
                        has_refresh_token: false,
                        server_stream: false,
                        header_names: vec![],
                        secret_enc: store::seal(home, &secrets)?,
                    },
                )?;
                true
            }
        }
        None => match &existing {
            Some(rec) => rec.status != ConnStatus::Connected,
            None => {
                return Err(format!(
                    "remote MCP server '{server}' has no URL and is not connected; connect it from the dashboard first"
                ));
            }
        },
    };
    Ok((installed_def(home, agent_id, server)?, needs_connect))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_and_installed_forms() {
        let c = candidate_def("https://mcp.example.com/mcp");
        assert!(is_bridge_def(&c));
        assert_eq!(carried_url(&c), Some("https://mcp.example.com/mcp"));
        let plain = McpServerDef { command: "npx".into(), args: vec!["-y".into(), "x".into()], env: Default::default() };
        assert!(!is_bridge_def(&plain));
        let spoof = McpServerDef { command: "/tmp/evil".into(), args: vec![BRIDGE_SUBCOMMAND.into()], env: Default::default() };
        assert!(!is_bridge_def(&spoof));
    }

    #[test]
    fn prepare_records_the_url_and_installs_without_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let (def, needs) = prepare_for_install(home, "a1", "zap", &candidate_def("https://mcp.example.com/api/s/KEY/mcp")).unwrap();
        assert!(needs);
        assert!(std::path::Path::new(&def.command).is_absolute());
        assert_eq!(def.args, vec!["mcp-remote-bridge", "--agent", "a1", "--server", "zap"]);
        assert!(!def.args.iter().any(|a| a.contains("KEY")));
        assert_eq!(def.env.get("DUDUCLAW_HOME").map(String::as_str), Some(home.to_str().unwrap()));
        let rec = store::get(home, "a1", "zap").unwrap().unwrap();
        assert_eq!(rec.status, ConnStatus::NotConnected);
        assert_eq!(store::open(home, &rec).unwrap().url, "https://mcp.example.com/api/s/KEY/mcp");

        let json = serde_json::to_value(&def).unwrap();
        assert_eq!(installed_target(&json), Some(("a1".into(), "zap".into())));
        // Re-installing the installed form keeps the record.
        let (_, needs) = prepare_for_install(home, "a1", "zap", &def).unwrap();
        assert!(needs);
    }

    #[test]
    fn prepare_refuses_bad_urls_and_unknown_servers() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert!(prepare_for_install(home, "a1", "x", &candidate_def("http://mcp.example.com/mcp")).is_err());
        assert!(prepare_for_install(home, "a1", "x", &candidate_def("https://10.0.0.1/mcp")).is_err());
        let installed = installed_def(home, "a1", "x").unwrap();
        assert!(prepare_for_install(home, "a1", "x", &installed).is_err());
        assert!(prepare_for_install(home, "a1", "duduclaw-x", &candidate_def("https://mcp.example.com")).is_err());
    }

    #[test]
    fn installed_entry_passes_the_scan_and_is_wrapped_by_the_redaction_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let def = installed_def(dir.path(), "a1", "zap").unwrap();
        assert!(crate::mcp_scan::scan_mcp_server_def("zap", &def).passed);
        let exe = std::path::Path::new("/opt/duduclaw/bin/duduclaw");
        let cfg = serde_json::json!({ "mcpServers": { "zap": def } });
        let out = crate::redaction_proxy::rewrite_mcp_config_for_proxy(&cfg, exe);
        let args: Vec<String> = serde_json::from_value(out["mcpServers"]["zap"]["args"].clone()).unwrap();
        assert_eq!(args[0], "mcp-proxy");
        assert!(args.iter().any(|a| a == BRIDGE_SUBCOMMAND), "{args:?}");
        // The bridge's own env (DUDUCLAW_HOME) rides in the proxy's upstream env.
        let env = out["mcpServers"]["zap"]["env"].as_object().unwrap();
        assert!(env.values().any(|v| v.as_str().is_some_and(|s| s.contains("DUDUCLAW_HOME"))));
    }

    #[test]
    fn non_bridge_defs_pass_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let plain = McpServerDef { command: "npx".into(), args: vec!["-y".into(), "x".into()], env: Default::default() };
        let (def, needs) = prepare_for_install(dir.path(), "a1", "x", &plain).unwrap();
        assert_eq!(def.args, plain.args);
        assert!(!needs);
        assert!(store::load_all(dir.path()).unwrap().is_empty());
    }
}
