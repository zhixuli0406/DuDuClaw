//! MCP Bridge Adapter Framework — mount external third-party MCP servers.
//!
//! Rather than hand-writing a Rust connector for every SaaS (Plane, Chatwoot,
//! Invoice Ninja, Gmail, …), an agent can declare external MCP servers in its
//! `agent.toml` and DuDuClaw spawns them alongside the internal duduclaw MCP
//! server, exposing their tools to the agent's tool loop. This reuses the
//! existing `duduclaw_llm::{McpClient, ToolRegistry}` transport — the only new
//! pieces are this config reader, credential resolution, and a per-server tool
//! allow/deny filter ([`duduclaw_llm::ToolFilter`]).
//!
//! ## `agent.toml` schema
//!
//! ```toml
//! # stdio server (spawned child process):
//! [[mcp.external]]
//! name = "chatwoot"
//! command = "npx"
//! args = ["-y", "@chatwoot/mcp-server-chatwoot"]
//! enabled = true
//! # env values: plain literal; `env://VAR` to pull from the gateway process
//! # environment; or `secret://<backend>/<name>` to pull from the configured
//! # secret manager (vault / onepassword / infisical / local / env) at spawn
//! # time — keeps secrets out of both agent.toml and the process environment.
//! env = { CHATWOOT_BASE_URL = "https://app.chatwoot.com", CHATWOOT_API_TOKEN = "secret://vault/chatwoot_token" }
//! allowed_tools = ["chatwoot_list_conversations", "chatwoot_get_conversation"]  # allowlist (deny-by-default)
//! denied_tools  = []                                                            # always removed
//!
//! # Google's official Workspace MCP servers via a built-in preset (endpoint
//! # + `oauth://google` bearer are filled in automatically):
//! [[mcp.external]]
//! preset = "google:gmail"      # gmail|calendar|drive|docs|sheets|slides|chat
//! allowed_tools = ["search_threads", "get_thread", "create_draft"]
//!
//! # remote Streamable-HTTP server, spelled out (equivalent to the preset):
//! [[mcp.external]]
//! name = "gmail"
//! url = "https://gmailmcp.googleapis.com/mcp/v1"
//! # bearer_token: literal, `env://VAR`, `secret://<backend>/<name>`, or
//! # `oauth://google` (reuse the dashboard's connected Google account token,
//! # auto-refreshed). Sent as `Authorization: Bearer <token>`.
//! bearer_token = "oauth://google"
//! # optional extra request headers (values support env:// and secret://):
//! # headers = { X-Custom = "env://MY_HEADER_VALUE" }
//! allowed_tools = ["search_threads", "get_thread", "create_draft"]
//! ```
//!
//! ## Safety
//!
//! - A server with an unresolvable `env://`, `secret://` or `oauth://`
//!   credential is **skipped** (a server spawned/mounted without its token
//!   would misbehave) — fail-safe, logged.
//! - `allowed_tools` is deny-by-default: if set, only those tools are exposed.
//! - The internal duduclaw server always wins name collisions (it is client 0).
//! - Exactly one of `command` / `url` per entry; entries with both or neither
//!   are skipped loudly.

use std::path::Path;

use duduclaw_core::agent_toml::AgentTomlSections;
use duduclaw_core::lenient::Tri;
use duduclaw_llm::ToolFilter;

/// Bearer scheme that resolves to the dashboard's connected Google account
/// access token (auto-refreshed via the stored refresh token).
const OAUTH_GOOGLE_REF: &str = "oauth://google";

/// Google's official Workspace remote MCP endpoints, keyed by the short name
/// used in `preset = "google:<name>"`.
///
/// Verified 2026-07-30 two ways: a live `initialize` + `tools/list` probe of
/// every endpoint below, and Google's own
/// <https://developers.google.com/workspace/guides/configure-mcp-servers>.
/// Keeping the URLs here (instead of in each user's `agent.toml`) makes this
/// the single place to fix if Google renames an endpoint at GA.
///
/// Deliberately absent: **Forms** and **Tasks** have no official MCP server
/// (probed 404, and absent from Google's docs) — DuDuClaw serves those through
/// its own native `forms_*` / `tasks_*` MCP tools instead. `people` is absent
/// because it does not follow the `<svc>mcp.googleapis.com` pattern.
const GOOGLE_MCP_PRESETS: &[(&str, &str)] = &[
    ("gmail", "https://gmailmcp.googleapis.com/mcp/v1"),
    ("calendar", "https://calendarmcp.googleapis.com/mcp/v1"),
    ("drive", "https://drivemcp.googleapis.com/mcp/v1"),
    ("docs", "https://docsmcp.googleapis.com/mcp/v1"),
    ("sheets", "https://sheetsmcp.googleapis.com/mcp/v1"),
    ("slides", "https://slidesmcp.googleapis.com/mcp/v1"),
    ("chat", "https://chatmcp.googleapis.com/mcp/v1"),
];

/// Resolve `preset = "google:gmail"` to `(url, default_bearer)`. Unknown
/// namespace or service ⇒ `None` (caller skips the entry loudly rather than
/// mounting something unintended).
fn resolve_preset(preset: &str) -> Option<(String, String)> {
    let (ns, svc) = preset.split_once(':')?;
    match ns.trim() {
        "google" => GOOGLE_MCP_PRESETS
            .iter()
            .find(|(name, _)| *name == svc.trim())
            .map(|(_, url)| (url.to_string(), OAUTH_GOOGLE_REF.to_string())),
        _ => None,
    }
}

/// Every preset name a config may reference, for docs/diagnostics.
pub fn known_presets() -> Vec<String> {
    GOOGLE_MCP_PRESETS
        .iter()
        .map(|(name, _)| format!("google:{name}"))
        .collect()
}

/// One resolved external MCP server ready to spawn (stdio) or mount (http).
#[derive(Debug, Clone)]
pub struct ExternalMcpServer {
    pub name: String,
    /// stdio transport: the command to spawn. Empty when `url` is set.
    pub command: String,
    pub args: Vec<String>,
    /// Fully-resolved child environment (`env://` refs already pulled).
    pub env: Vec<(String, String)>,
    /// Streamable-HTTP transport: the remote endpoint. `None` for stdio.
    pub url: Option<String>,
    /// Raw bearer credential for HTTP (`env://` already resolved; `secret://`
    /// and `oauth://google` still verbatim until the async resolve pass).
    pub bearer_token: Option<String>,
    /// Extra HTTP request headers (same staged resolution as `env`).
    pub headers: Vec<(String, String)>,
    /// Per-server tool visibility filter.
    pub filter: ToolFilter,
}

impl ExternalMcpServer {
    /// Final HTTP header set for a mounted remote server: caller headers plus
    /// the bearer credential as `Authorization`. Call only after the async
    /// resolve pass (all refs resolved).
    pub fn http_headers(&self) -> Vec<(String, String)> {
        let mut out = self.headers.clone();
        if let Some(tok) = &self.bearer_token {
            out.push(("authorization".to_string(), format!("Bearer {tok}")));
        }
        out
    }
}

/// Resolve one env value: `env://VAR` → the gateway's env (None if unset),
/// anything else → itself. `secret://` values pass through here verbatim and
/// are resolved later by the async [`resolve_secret_refs`] pass (the secret
/// backend is async and needs the DuDuClaw home, neither available in this pure
/// sync parse).
fn resolve_env_value(raw: &str) -> Option<String> {
    if let Some(var) = raw.strip_prefix("env://") {
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => None, // missing/empty → signal "skip this server"
        }
    } else {
        Some(raw.to_string())
    }
}

/// Resolve any `secret://<backend>/<name>` env values against the configured
/// secret manager, returning only the servers whose secrets all resolved.
///
/// A server holding an unresolvable `secret://` ref is dropped (fail-safe,
/// mirroring the `env://` skip in [`parse_external_servers`]). `home_dir` is the
/// DuDuClaw home used to load `config.toml`'s `[secret_manager]` section and the
/// keyfile for encrypted tokens. Kept separate from the pure sync parse so the
/// parse stays unit-testable and the async secret backend lives in one place.
pub async fn resolve_secret_refs(
    servers: Vec<ExternalMcpServer>,
    home_dir: &Path,
) -> Vec<ExternalMcpServer> {
    use duduclaw_security::secret_manager::{resolve_secret_reference, SecretManagerConfig};

    let needs_secret = |s: &ExternalMcpServer| {
        s.env.iter().any(|(_, v)| v.starts_with("secret://"))
            || s.headers.iter().any(|(_, v)| v.starts_with("secret://"))
            || s.bearer_token.as_deref().is_some_and(|v| v.starts_with("secret://"))
    };
    let needs_oauth = |s: &ExternalMcpServer| {
        s.bearer_token.as_deref() == Some(OAUTH_GOOGLE_REF)
    };

    // Fast path: nothing to resolve ⇒ don't even read config.toml.
    if !servers.iter().any(|s| needs_secret(s) || needs_oauth(s)) {
        return servers;
    }

    // Load [secret_manager] once; absent / malformed ⇒ default (local).
    let sm_cfg = match tokio::fs::read_to_string(home_dir.join("config.toml")).await {
        Ok(s) => SecretManagerConfig::from_toml_str(&s).unwrap_or_default(),
        Err(_) => SecretManagerConfig::default(),
    };

    // Resolve the Google OAuth token at most once per pass (shared by all
    // `oauth://google` mounts; the getter refreshes an expired token itself).
    let google_token: Option<String> = if servers.iter().any(needs_oauth) {
        match crate::google_workspace::get_valid_google_token(home_dir).await {
            Ok(t) => Some(t),
            Err(e) => {
                tracing::warn!(error = %e, "oauth://google bearer unresolved (Google account not connected?)");
                None
            }
        }
    } else {
        None
    };

    let mut out = Vec::with_capacity(servers.len());
    'server: for mut server in servers {
        for (key, val) in server
            .env
            .iter_mut()
            .chain(server.headers.iter_mut())
        {
            if val.starts_with("secret://") {
                match resolve_secret_reference(val, &sm_cfg, home_dir).await {
                    Some(resolved) => *val = resolved,
                    None => {
                        tracing::warn!(
                            server = %server.name, key = %key,
                            "external MCP secret:// credential unresolved — skipping server"
                        );
                        continue 'server;
                    }
                }
            }
        }
        if let Some(bearer) = server.bearer_token.as_mut() {
            if bearer == OAUTH_GOOGLE_REF {
                match &google_token {
                    Some(t) => *bearer = t.clone(),
                    None => {
                        tracing::warn!(
                            server = %server.name,
                            "external MCP oauth://google bearer unresolved — skipping server"
                        );
                        continue 'server;
                    }
                }
            } else if bearer.starts_with("secret://") {
                match resolve_secret_reference(bearer, &sm_cfg, home_dir).await {
                    Some(resolved) => *bearer = resolved,
                    None => {
                        tracing::warn!(
                            server = %server.name,
                            "external MCP secret:// bearer unresolved — skipping server"
                        );
                        continue 'server;
                    }
                }
            }
        }
        out.push(server);
    }
    out
}

/// Load + fully resolve external MCP servers for an agent: parse `agent.toml`,
/// then resolve any `secret://` refs against the secret manager rooted at
/// `home_dir`. This is the entry point spawn paths should call.
pub async fn load_external_mcp_servers_resolved(
    agent_dir: &Path,
    home_dir: &Path,
) -> Vec<ExternalMcpServer> {
    resolve_secret_refs(load_external_mcp_servers(agent_dir), home_dir).await
}

/// Resolve a security-relevant tool-filter list (`allowed_tools`/`denied_tools`).
/// Absent ⇒ `Ok(empty)`. Present and an array ⇒ `Ok(strings)`. Present but the
/// WRONG type ⇒ `Err(())` (caller skips the whole server, fail-closed) with a
/// loud warning, so a `"x"`-instead-of-`["x"]` typo can never silently widen the
/// exposed tool surface.
///
/// This three-state distinction is why the view field is a
/// [`duduclaw_core::lenient::Tri`] and not a `Vec<String>`: collapsing
/// "wrong type" into "empty" here would silently turn a typo'd allowlist into
/// a permissive one.
fn tool_list_field(field: &Tri<Vec<String>>, key: &str, server: &str) -> Result<Vec<String>, ()> {
    match field {
        Tri::Absent => Ok(Vec::new()),
        Tri::Value(v) => Ok(v.clone()),
        Tri::WrongType => {
            tracing::warn!(
                server = %server, key = %key,
                "external MCP {key} must be an array of tool names — skipping server (fail-closed)"
            );
            Err(())
        }
    }
}

/// Parse `[[mcp.external]]` entries out of already-parsed `agent.toml`
/// sections. Pure (no filesystem / env) except for `resolve_env_value`; split
/// from the file read so the parsing + skip logic is unit-testable.
///
/// Takes the shared typed projection ([`duduclaw_core::agent_toml`]) rather
/// than a `toml::Value`. Every *semantic* decision below (preset resolution,
/// the exactly-one-transport rule, credential resolution, the fail-closed
/// filter skip) is unchanged — only the field access is now typed.
pub fn parse_external_servers(sections: &AgentTomlSections) -> Vec<ExternalMcpServer> {
    let mut out = Vec::new();
    for entry in &sections.mcp.external {
        // Missing / wrong-typed ⇒ enabled (this is an opt-OUT flag).
        let enabled = entry.enabled.unwrap_or(true);
        if !enabled {
            continue;
        }
        // A `preset` supplies the endpoint URL (and a default bearer) for a
        // known vendor's official remote MCP server, so users don't paste (or
        // mistype) endpoint URLs. An explicit `url` alongside it is ambiguous.
        let preset = entry
            .preset
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mut preset_bearer: Option<String> = None;
        let mut preset_url: Option<String> = None;
        if let Some(p) = preset {
            match resolve_preset(p) {
                Some((url, bearer)) => {
                    preset_url = Some(url);
                    preset_bearer = Some(bearer);
                }
                None => {
                    tracing::warn!(
                        preset = %p,
                        known = ?known_presets(),
                        "external MCP preset unknown — skipping server"
                    );
                    continue;
                }
            }
        }

        let name = entry
            .name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            // A preset entry needs no `name` — the preset itself labels it.
            .or_else(|| preset.map(str::to_string))
            .unwrap_or_default();
        let command = entry.command.as_deref().unwrap_or("").to_string();
        let explicit_url = entry
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if preset_url.is_some() && explicit_url.is_some() {
            tracing::warn!(
                server = %name,
                "external MCP server has BOTH 'preset' and 'url' — ambiguous, skipping"
            );
            continue;
        }
        let url = preset_url.or(explicit_url);

        // Exactly one transport per entry. Both or neither ⇒ skip loudly.
        match (&url, command.trim().is_empty()) {
            (None, true) => {
                tracing::warn!(server = %name, "external MCP server needs 'command' (stdio) or 'url' (http) — skipping");
                continue;
            }
            (Some(_), false) => {
                tracing::warn!(server = %name, "external MCP server has BOTH 'command' and 'url' — ambiguous, skipping");
                continue;
            }
            _ => {}
        }
        let args = entry.args.clone();

        // Resolve env + headers; a missing `env://` credential disables the
        // whole server. (`secret://` / `oauth://` resolve in the async pass.)
        //
        // `string_map` already dropped non-string values and preserves the
        // `toml::Table` (BTreeMap) key order, so which key wins the "first
        // unresolved" warning is unchanged.
        let mut env = Vec::new();
        let mut headers = Vec::new();
        let mut skip = false;
        for (field, source, sink) in [
            ("env", &entry.env, &mut env),
            ("headers", &entry.headers, &mut headers),
        ] {
            for (k, raw) in source {
                match resolve_env_value(raw) {
                    Some(v) => sink.push((k.clone(), v)),
                    None => {
                        tracing::warn!(
                            server = %name, key = %k,
                            "external MCP {field} credential unresolved (env:// unset) — skipping server"
                        );
                        skip = true;
                        break;
                    }
                }
            }
            if skip {
                break;
            }
        }
        if skip {
            continue;
        }

        // Bearer credential: env:// resolves now; secret:// and oauth://google
        // stay verbatim for the async pass; anything else is a literal. An
        // explicit `bearer_token` overrides the preset's default.
        let bearer_token = match entry.bearer_token.as_deref() {
            None => preset_bearer,
            Some(raw) if raw.starts_with("secret://") || raw == OAUTH_GOOGLE_REF => {
                Some(raw.to_string())
            }
            Some(raw) => match resolve_env_value(raw) {
                Some(v) => Some(v),
                None => {
                    tracing::warn!(
                        server = %name,
                        "external MCP bearer_token unresolved (env:// unset) — skipping server"
                    );
                    continue;
                }
            },
        };

        // Tool filter lists are security-relevant: a present-but-wrong-type
        // value (e.g. `allowed_tools = "x"` instead of `["x"]`) must NOT silently
        // become an empty (permissive) allowlist. Fail closed — skip the server
        // loudly so a typo can't expose the whole external tool surface.
        let allowed = match tool_list_field(&entry.allowed_tools, "allowed_tools", &name) {
            Ok(v) => v,
            Err(()) => continue,
        };
        let denied = match tool_list_field(&entry.denied_tools, "denied_tools", &name) {
            Ok(v) => v,
            Err(()) => continue,
        };
        let filter = ToolFilter { allowed, denied };

        out.push(ExternalMcpServer {
            name,
            command,
            args,
            env,
            url,
            bearer_token,
            headers,
            filter,
        });
    }
    out
}

/// Load external MCP servers declared in `<agent_dir>/agent.toml`. Missing /
/// malformed file ⇒ empty (no externals; behavior unchanged).
pub fn load_external_mcp_servers(agent_dir: &Path) -> Vec<ExternalMcpServer> {
    parse_external_servers(&duduclaw_core::agent_toml::load(agent_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<ExternalMcpServer> {
        parse_external_servers(&duduclaw_core::agent_toml::parse(s))
    }

    // ── R5: `[[mcp.external]]` directions, pinned ────────────────────────
    //
    //   whole section absent / malformed ⇒ no external servers (a failure to
    //                 read config never mounts a server).
    //   enabled       absent / wrong-typed ⇒ TRUE. This is an opt-OUT flag;
    //                 a config written before the flag existed must keep
    //                 working. The one place in this parser that defaults
    //                 permissively.
    //   allowed_tools / denied_tools
    //                 absent ⇒ no filter, but present-and-wrong-typed ⇒ SKIP
    //                 THE WHOLE SERVER. Collapsing those two into "no filter"
    //                 would turn `allowed_tools = "x"` into a silently
    //                 permissive allowlist — which is why the view field is
    //                 three-state and not a `Vec<String>`.
    //   a mixed array is FILTERED, not rejected: `["a", 1]` is a live config
    //                 that used to yield `["a"]`, and still must.

    #[test]
    fn default_direction_absent_or_malformed_mounts_nothing() {
        for body in [
            "",                            // empty file
            "[agent]\nname = \"a\"\n",     // no [mcp]
            "[mcp]\n",                     // section, no external
            "mcp = \"scalar\"\n",          // wrong-typed section
            "[mcp]\nexternal = \"nope\"\n", // wrong-typed array
            "not toml [[[",                // malformed file
        ] {
            assert!(parse(body).is_empty(), "for {body:?}");
        }
    }

    #[test]
    fn default_direction_enabled_is_opt_out_not_opt_in() {
        // No `enabled` key at all — a pre-flag config must still mount.
        let base = "[[mcp.external]]\nname = \"s\"\ncommand = \"npx\"\n";
        assert_eq!(parse(base).len(), 1, "absent enabled ⇒ mounted");

        // Wrong-typed ⇒ also mounted (the raw `as_bool()` returned None,
        // which `unwrap_or(true)` turned into "on").
        assert_eq!(
            parse(&format!("{base}enabled = \"false\"\n")).len(),
            1,
            "wrong-typed enabled ⇒ mounted, NOT skipped"
        );

        // Only an explicit `false` opts out.
        assert!(parse(&format!("{base}enabled = false\n")).is_empty());
    }

    #[test]
    fn default_direction_wrong_typed_tool_filter_skips_the_whole_server() {
        let base = "[[mcp.external]]\nname = \"s\"\ncommand = \"npx\"\n";

        // Absent ⇒ mounted with an empty (inert) filter.
        let servers = parse(base);
        assert_eq!(servers.len(), 1);
        assert!(servers[0].filter.allowed.is_empty());
        assert!(servers[0].filter.denied.is_empty());

        // Present-but-wrong-typed ⇒ fail closed, server dropped. This is the
        // distinction that a two-state Option would have destroyed.
        assert!(
            parse(&format!("{base}allowed_tools = \"one_tool\"\n")).is_empty(),
            "a scalar allowlist must not become a permissive empty one"
        );
        assert!(
            parse(&format!("{base}denied_tools = 42\n")).is_empty(),
            "a scalar denylist must skip the server too"
        );

        // A mixed array is filtered, not rejected — the historical behavior.
        let mixed = parse(&format!("{base}allowed_tools = [\"a\", 7, \"b\"]\n"));
        assert_eq!(mixed.len(), 1, "mixed array must not skip the server");
        assert_eq!(mixed[0].filter.allowed, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn default_direction_env_and_headers_drop_non_string_values() {
        // The raw loop did `let Some(raw) = raw.as_str() else { continue }` —
        // a non-string value was skipped, never fatal, and never resolved.
        let servers = parse(
            "[[mcp.external]]\n\
             name = \"s\"\n\
             command = \"npx\"\n\
             env = { A = \"1\", B = 2, C = \"3\" }\n",
        );
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].env,
            vec![("A".to_string(), "1".to_string()), ("C".to_string(), "3".to_string())]
        );

        // A wrong-typed `env` as a whole ⇒ no env, still mounted.
        let servers = parse("[[mcp.external]]\nname = \"s\"\ncommand = \"npx\"\nenv = \"x\"\n");
        assert_eq!(servers.len(), 1);
        assert!(servers[0].env.is_empty());
    }

    #[test]
    fn no_section_is_empty() {
        assert!(parse("[agent]\nname='x'\n").is_empty());
    }

    #[test]
    fn parses_basic_server_with_filter() {
        let s = r#"
[[mcp.external]]
name = "plane"
command = "npx"
args = ["-y", "plane-mcp"]
allowed_tools = ["plane_list_issues"]
denied_tools = ["plane_delete_issue"]
env = { PLANE_BASE_URL = "https://plane.example.com" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        let sv = &servers[0];
        assert_eq!(sv.name, "plane");
        assert_eq!(sv.command, "npx");
        assert_eq!(sv.args, vec!["-y", "plane-mcp"]);
        assert_eq!(sv.env, vec![("PLANE_BASE_URL".into(), "https://plane.example.com".into())]);
        assert!(sv.filter.permits("plane_list_issues"));
        assert!(!sv.filter.permits("plane_delete_issue"));
        assert!(!sv.filter.permits("plane_other"), "allowlist is deny-by-default");
    }

    #[test]
    fn disabled_server_skipped() {
        let s = r#"
[[mcp.external]]
name = "off"
command = "x"
enabled = false
"#;
        assert!(parse(s).is_empty());
    }

    #[test]
    fn missing_command_skipped() {
        let s = "[[mcp.external]]\nname = \"nocmd\"\n";
        assert!(parse(s).is_empty());
    }

    #[test]
    fn env_ref_missing_skips_server() {
        // env:// pointing at an almost-certainly-unset var disables the server.
        let s = r#"
[[mcp.external]]
name = "needsauth"
command = "x"
env = { TOKEN = "env://DUDUCLAW_TEST_DEFINITELY_UNSET_VAR_XYZ" }
"#;
        assert!(parse(s).is_empty(), "unresolved credential ⇒ server skipped");
    }

    #[test]
    fn plain_env_value_passes_through() {
        let s = r#"
[[mcp.external]]
name = "plain"
command = "x"
env = { BASE = "literal-value" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].env, vec![("BASE".into(), "literal-value".into())]);
    }

    #[test]
    fn malformed_allowlist_fails_closed() {
        // allowed_tools as a bare string (typo) must NOT silently expose all
        // tools — the server is skipped entirely.
        let s = r#"
[[mcp.external]]
name = "typo"
command = "x"
allowed_tools = "just_one"
"#;
        assert!(parse(s).is_empty(), "malformed allowlist ⇒ server skipped (fail-closed)");

        // denied_tools as a wrong type likewise skips the server.
        let s2 = r#"
[[mcp.external]]
name = "typo2"
command = "x"
denied_tools = 42
"#;
        assert!(parse(s2).is_empty());
    }

    #[test]
    fn multiple_servers() {
        let s = r#"
[[mcp.external]]
name = "a"
command = "x"
[[mcp.external]]
name = "b"
command = "y"
"#;
        assert_eq!(parse(s).len(), 2);
    }

    #[test]
    fn secret_ref_passes_through_parse_verbatim() {
        // The sync parse keeps `secret://` values untouched (resolved later).
        let s = r#"
[[mcp.external]]
name = "s"
command = "x"
env = { TOKEN = "secret://vault/tok" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].env[0].1, "secret://vault/tok");
    }

    // ── presets (vendor official remote MCP servers) ──

    #[test]
    fn google_presets_expand_to_verified_endpoints() {
        let s = r#"
[[mcp.external]]
preset = "google:gmail"
allowed_tools = ["search_threads"]
[[mcp.external]]
preset = "google:sheets"
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "google:gmail", "preset labels the entry");
        assert_eq!(servers[0].url.as_deref(), Some("https://gmailmcp.googleapis.com/mcp/v1"));
        // Bearer defaults to the connected Google account (resolved async).
        assert_eq!(servers[0].bearer_token.as_deref(), Some("oauth://google"));
        assert!(servers[0].filter.permits("search_threads"));
        assert_eq!(servers[1].url.as_deref(), Some("https://sheetsmcp.googleapis.com/mcp/v1"));
    }

    #[test]
    fn all_known_presets_resolve() {
        for p in known_presets() {
            let (url, bearer) = resolve_preset(&p).unwrap_or_else(|| panic!("{p} unresolved"));
            assert!(url.starts_with("https://") && url.ends_with("/mcp/v1"), "{p}: {url}");
            assert_eq!(bearer, "oauth://google");
        }
        // Forms/Tasks have no official Google MCP server — native tools serve
        // them instead, so they must NOT silently resolve to a bogus endpoint.
        assert!(resolve_preset("google:forms").is_none());
        assert!(resolve_preset("google:tasks").is_none());
        assert!(resolve_preset("google:nope").is_none());
        assert!(resolve_preset("notavendor:gmail").is_none());
        assert!(resolve_preset("malformed").is_none());
    }

    #[test]
    fn unknown_preset_skips_server() {
        assert!(parse("[[mcp.external]]\npreset = \"google:forms\"\n").is_empty());
    }

    #[test]
    fn preset_plus_url_is_ambiguous_and_skipped() {
        let s = r#"
[[mcp.external]]
preset = "google:gmail"
url = "https://evil.example.com/mcp"
"#;
        assert!(parse(s).is_empty());
    }

    #[test]
    fn explicit_bearer_overrides_preset_default() {
        let s = r#"
[[mcp.external]]
preset = "google:drive"
bearer_token = "literal-override"
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].bearer_token.as_deref(), Some("literal-override"));
    }

    // ── remote (Streamable HTTP) mounts ──

    #[test]
    fn parses_http_server_with_bearer_and_headers() {
        let s = r#"
[[mcp.external]]
name = "gmail"
url = "https://gmailmcp.googleapis.com/mcp/v1"
bearer_token = "literal-token"
headers = { X-Extra = "plain" }
allowed_tools = ["search_threads"]
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        let sv = &servers[0];
        assert_eq!(sv.url.as_deref(), Some("https://gmailmcp.googleapis.com/mcp/v1"));
        assert!(sv.command.is_empty());
        assert_eq!(sv.bearer_token.as_deref(), Some("literal-token"));
        assert_eq!(sv.headers, vec![("X-Extra".into(), "plain".into())]);
        let hdrs = sv.http_headers();
        assert!(hdrs.contains(&("authorization".into(), "Bearer literal-token".into())));
        assert!(sv.filter.permits("search_threads"));
        assert!(!sv.filter.permits("create_label"));
    }

    #[test]
    fn both_command_and_url_is_ambiguous_and_skipped() {
        let s = r#"
[[mcp.external]]
name = "ambiguous"
command = "npx"
url = "https://example.com/mcp"
"#;
        assert!(parse(s).is_empty());
    }

    #[test]
    fn neither_command_nor_url_skipped() {
        assert!(parse("[[mcp.external]]\nname = \"none\"\n").is_empty());
    }

    #[test]
    fn bearer_env_ref_missing_skips_server() {
        let s = r#"
[[mcp.external]]
name = "needsbearer"
url = "https://example.com/mcp"
bearer_token = "env://DUDUCLAW_TEST_DEFINITELY_UNSET_VAR_XYZ"
"#;
        assert!(parse(s).is_empty());
    }

    #[test]
    fn bearer_secret_and_oauth_refs_pass_through_parse() {
        let s = r#"
[[mcp.external]]
name = "a"
url = "https://example.com/mcp"
bearer_token = "secret://vault/tok"
[[mcp.external]]
name = "b"
url = "https://example.com/mcp"
bearer_token = "oauth://google"
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].bearer_token.as_deref(), Some("secret://vault/tok"));
        assert_eq!(servers[1].bearer_token.as_deref(), Some("oauth://google"));
    }

    #[tokio::test]
    async fn unresolvable_oauth_google_drops_server() {
        // No connected Google account under a nonexistent home ⇒ the
        // oauth://google mount is dropped fail-safe.
        let s = r#"
[[mcp.external]]
name = "gmail"
url = "https://gmailmcp.googleapis.com/mcp/v1"
bearer_token = "oauth://google"
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1);
        let out = resolve_secret_refs(servers, Path::new("/nonexistent-home")).await;
        assert!(out.is_empty(), "unresolvable oauth://google ⇒ server dropped");
    }

    #[tokio::test]
    async fn resolve_no_secret_refs_is_passthrough() {
        let s = r#"
[[mcp.external]]
name = "plain"
command = "x"
env = { BASE = "literal" }
"#;
        let servers = parse(s);
        // No secret:// ⇒ fast path, home_dir never read.
        let out = resolve_secret_refs(servers, Path::new("/nonexistent-home")).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].env[0].1, "literal");
    }

    #[tokio::test]
    async fn unresolvable_secret_ref_drops_server() {
        // `secret://local/<name>` against an empty ephemeral local store cannot
        // resolve ⇒ the server is dropped fail-safe (never spawned token-less).
        let s = r#"
[[mcp.external]]
name = "needssecret"
command = "x"
env = { TOKEN = "secret://local/definitely-absent-secret" }
"#;
        let servers = parse(s);
        assert_eq!(servers.len(), 1, "parse keeps secret:// verbatim");
        let out = resolve_secret_refs(servers, Path::new("/nonexistent-home")).await;
        assert!(out.is_empty(), "unresolvable secret ⇒ server dropped");
    }
}
