//! Read-only client for the official MCP Registry
//! (`https://registry.modelcontextprotocol.io`, API `v0`, no auth).
//!
//! Only this fixed host is ever contacted (no caller-supplied URL), through
//! the same screened, pinned, redirect-checked HTTP path as the remote MCP
//! feature, with a 20 s timeout and a 2 MiB answer cap. Search answers are
//! cached in memory for ten minutes.
//!
//! [`normalize_server`] turns one `server.json` (2025 schema) into the flat
//! shape the dashboard shows, with an `installable` flag and a reason code
//! when it is not.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::remote_mcp::http::get_json;
use crate::remote_mcp::url_policy::OutboundPolicy;

/// The registry this gateway reads.
pub const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";
/// Results per search page.
pub const PAGE_SIZE: u32 = 30;
const MAX_BODY: usize = 2 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(600);
const CACHE_MAX: usize = 128;

/// One search result as the dashboard shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RegistryHit {
    pub name: String,
    pub title: String,
    pub description: String,
    pub version: String,
    /// `npm` / `pypi` / `oci` / other registry types, as listed.
    pub package_kinds: Vec<String>,
    pub has_remotes: bool,
    /// Transport types of the remotes (`streamable-http`, `sse`).
    pub remote_kinds: Vec<String>,
    /// Remote needs a bearer token (an `Authorization` header is declared).
    pub remote_needs_bearer: bool,
    pub repository_url: Option<String>,
    pub website_url: Option<String>,
    /// Required env var names of the first installable package.
    pub required_env: Vec<EnvVarInfo>,
    pub installable: bool,
    /// Why not installable (code): `no_supported_transport`,
    /// `sse_remote_only`, `custom_headers` (a required header DuDuClaw
    /// cannot send, e.g. `Cookie`), `deprecated`, `deleted`.
    pub reason: Option<String>,
    /// The install will create a remote connection that must be signed in.
    pub install_is_remote: bool,
    /// Headers the first supported remote declares (not `Authorization`),
    /// asked for in the connect dialog.
    pub remote_headers: Vec<HeaderInfo>,
}

/// One declared environment variable.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EnvVarInfo {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub secret: bool,
}

const SUPPORTED_PACKAGES: [&str; 3] = ["npm", "pypi", "oci"];

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

fn clean(text: &str, max_chars: usize) -> String {
    let cleaned: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    duduclaw_core::truncate_chars(cleaned.trim(), max_chars)
}

fn http_url(raw: &str) -> Option<String> {
    let u = Url::parse(raw).ok()?;
    matches!(u.scheme(), "https" | "http").then(|| duduclaw_core::truncate_chars(u.as_str(), 300))
}

fn registry_type(pkg: &Value) -> String {
    let t = pkg
        .get("registryType")
        .or_else(|| pkg.get("registry_name"))
        .or_else(|| pkg.get("registryName"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    t.to_ascii_lowercase()
}

/// Remote transport type (absent ⇒ streamable-http, the registry default).
pub fn remote_kind(remote: &Value) -> String {
    let t = s(remote, "type");
    if t.is_empty() { "streamable-http".into() } else { t.to_ascii_lowercase() }
}

/// One header a remote declares (other than `Authorization`), which the
/// connect dialog asks for and the bridge sends (2026-10-08 close-out; the
/// value is stored encrypted with the connection like any custom header).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HeaderInfo {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub secret: bool,
}

/// The declared non-`Authorization` headers of a remote, in order.
fn declared_headers(remote: &Value) -> Vec<&Value> {
    remote
        .get("headers")
        .and_then(|h| h.as_array())
        .map(|hs| hs.iter().filter(|h| !s(h, "name").eq_ignore_ascii_case("authorization")).collect())
        .unwrap_or_default()
}

/// Whether a remote requires a header DuDuClaw cannot send (a reserved or
/// malformed name such as `Cookie` or `Host`), or more than
/// `store::MAX_CUSTOM_HEADERS` of them.
fn unsendable_required_headers(remote: &Value) -> bool {
    let hs = declared_headers(remote);
    hs.len() > crate::remote_mcp::store::MAX_CUSTOM_HEADERS
        || hs.iter().any(|h| {
            h.get("isRequired").and_then(|v| v.as_bool()).unwrap_or(false)
                && !crate::remote_mcp::store::custom_header_name_allowed(s(h, "name"))
        })
}

/// The headers of `remote` the connect dialog should ask for: sendable
/// declared names only (an optional unsendable one is left out).
pub fn remote_header_prompts(remote: &Value) -> Vec<HeaderInfo> {
    declared_headers(remote)
        .into_iter()
        .filter(|h| crate::remote_mcp::store::custom_header_name_allowed(s(h, "name")))
        .take(crate::remote_mcp::store::MAX_CUSTOM_HEADERS)
        .map(|h| HeaderInfo {
            name: s(h, "name").to_string(),
            description: clean(s(h, "description"), 200),
            required: h.get("isRequired").and_then(|v| v.as_bool()).unwrap_or(false),
            secret: h.get("isSecret").and_then(|v| v.as_bool()).unwrap_or(false),
        })
        .collect()
}

fn needs_bearer(remote: &Value) -> bool {
    remote
        .get("headers")
        .and_then(|h| h.as_array())
        .is_some_and(|hs| hs.iter().any(|h| s(h, "name").eq_ignore_ascii_case("authorization")))
}

/// Whether a remote is one the bridge can serve.
pub fn remote_supported(remote: &Value) -> bool {
    let kind = remote_kind(remote);
    (kind == "streamable-http" || kind == "streamable_http" || kind == "http")
        && !s(remote, "url").is_empty()
        && !unsendable_required_headers(remote)
}

/// Declared env vars of a package.
pub fn package_env(pkg: &Value) -> Vec<EnvVarInfo> {
    pkg.get("environmentVariables")
        .or_else(|| pkg.get("environment_variables"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let name = s(e, "name");
                    let valid = !name.is_empty()
                        && name.len() <= 128
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                    valid.then(|| EnvVarInfo {
                        name: name.to_string(),
                        description: clean(s(e, "description"), 200),
                        required: e.get("isRequired").and_then(|v| v.as_bool()).unwrap_or(false),
                        secret: e.get("isSecret").and_then(|v| v.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The first package of a supported type.
pub fn first_supported_package(server: &Value) -> Option<&Value> {
    server
        .get("packages")
        .and_then(|v| v.as_array())?
        .iter()
        .find(|p| SUPPORTED_PACKAGES.contains(&registry_type(p).as_str()) && !s(p, "identifier").is_empty())
}

/// Normalize one registry entry. `meta` is the entry's `_meta` (status).
pub fn normalize_server(server: &Value, meta: Option<&Value>) -> RegistryHit {
    let packages: Vec<&Value> = server
        .get("packages")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let remotes: Vec<&Value> = server
        .get("remotes")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let mut package_kinds: Vec<String> = packages.iter().map(|p| registry_type(p)).filter(|k| !k.is_empty()).collect();
    package_kinds.dedup();
    let mut remote_kinds: Vec<String> = remotes.iter().map(|r| remote_kind(r)).collect();
    remote_kinds.dedup();
    let pkg = first_supported_package(server);
    let remote_ok = remotes.iter().any(|r| remote_supported(r));
    let status = meta
        .and_then(|m| m.get("io.modelcontextprotocol.registry/official"))
        .map(|o| s(o, "status").to_ascii_lowercase())
        .unwrap_or_default();
    let reason = if status == "deleted" || status == "deprecated" {
        Some(status.clone())
    } else if pkg.is_some() || remote_ok {
        None
    } else if !remotes.is_empty() && remotes.iter().any(|r| unsendable_required_headers(r)) {
        Some("custom_headers".into())
    } else if !remotes.is_empty() && remote_kinds.iter().all(|k| k == "sse") {
        Some("sse_remote_only".into())
    } else {
        Some("no_supported_transport".into())
    };
    RegistryHit {
        name: clean(s(server, "name"), 200),
        title: clean(s(server, "title"), 120),
        description: clean(s(server, "description"), 300),
        version: clean(s(server, "version"), 64),
        package_kinds,
        has_remotes: !remotes.is_empty(),
        remote_kinds,
        remote_needs_bearer: remotes.iter().any(|r| remote_supported(r) && needs_bearer(r)),
        repository_url: server
            .get("repository")
            .and_then(|r| r.get("url"))
            .and_then(|u| u.as_str())
            .and_then(http_url),
        website_url: server.get("websiteUrl").and_then(|u| u.as_str()).and_then(http_url),
        required_env: pkg.map(package_env).unwrap_or_default(),
        installable: reason.is_none(),
        reason,
        install_is_remote: pkg.is_none() && remote_ok,
        remote_headers: remotes
            .iter()
            .find(|r| remote_supported(r))
            .map(|r| remote_header_prompts(r))
            .unwrap_or_default(),
    }
}

/// Normalize a `GET /v0/servers` answer: `(hits, next_cursor)`.
pub fn normalize_search_response(body: &Value) -> (Vec<RegistryHit>, Option<String>) {
    let hits = body
        .get("servers")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|entry| {
                    // v0 wraps each entry as { server, _meta }; older answers
                    // were the server object itself.
                    let (server, meta) = match entry.get("server") {
                        Some(srv) if srv.is_object() => (srv, entry.get("_meta")),
                        _ => (entry, entry.get("_meta")),
                    };
                    let hit = normalize_server(server, meta);
                    (!hit.name.is_empty()).then_some(hit)
                })
                .collect()
        })
        .unwrap_or_default();
    let cursor = body
        .get("metadata")
        .and_then(|m| m.get("nextCursor").or_else(|| m.get("next_cursor")))
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    (hits, cursor)
}

/// Registry server names: reverse-DNS namespace `/` name.
pub fn valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name.contains('/')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
        && !name.contains("..")
}

/// A version selector (`latest` or a version string).
pub fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

fn valid_query(q: &str) -> bool {
    q.chars().count() <= 100 && !q.chars().any(|c| c.is_control())
}

fn valid_cursor(c: &str) -> bool {
    c.len() <= 300 && !c.chars().any(|ch| ch.is_control())
}

fn cache() -> &'static Mutex<HashMap<String, (Instant, Value)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, Value)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn fetch(url: &Url) -> Result<Value, String> {
    let (status, body) = get_json(url, OutboundPolicy::PUBLIC_ONLY, MAX_BODY).await?;
    match status {
        200 => Ok(body),
        404 => Err("not found in the MCP Registry".into()),
        s => Err(format!("the MCP Registry answered HTTP {s}")),
    }
}

/// Search the registry. Answer: `{ servers: [RegistryHit], next_cursor, cached }`.
pub async fn search(query: &str, cursor: Option<&str>) -> Result<Value, String> {
    let query = query.trim();
    if !valid_query(query) {
        return Err("search text is too long or contains control characters".into());
    }
    let cursor = cursor.map(str::trim).filter(|c| !c.is_empty());
    if let Some(c) = cursor
        && !valid_cursor(c)
    {
        return Err("invalid cursor".into());
    }
    let key = format!("{query}\u{1f}{}", cursor.unwrap_or(""));
    {
        let map = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = map.get(&key)
            && at.elapsed() < CACHE_TTL
        {
            let mut v = v.clone();
            v["cached"] = json!(true);
            return Ok(v);
        }
    }
    let mut url = Url::parse(&format!("{REGISTRY_BASE}/v0/servers")).map_err(|e| e.to_string())?;
    {
        let mut q = url.query_pairs_mut();
        if !query.is_empty() {
            q.append_pair("search", query);
        }
        q.append_pair("limit", &PAGE_SIZE.to_string());
        q.append_pair("version", "latest");
        if let Some(c) = cursor {
            q.append_pair("cursor", c);
        }
    }
    let body = fetch(&url).await?;
    let (hits, next) = normalize_search_response(&body);
    let out = json!({ "servers": hits, "next_cursor": next, "cached": false });
    {
        let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        if map.len() >= CACHE_MAX
            && let Some(oldest) = map.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone())
        {
            map.remove(&oldest);
        }
        map.insert(key, (Instant::now(), out.clone()));
    }
    Ok(out)
}

/// Fetch one server's `server.json` (`version` defaults to `latest`).
/// Returns the `server` object and its `_meta`.
pub async fn fetch_server(name: &str, version: Option<&str>) -> Result<(Value, Option<Value>), String> {
    if !valid_server_name(name) {
        return Err("invalid registry server name".into());
    }
    let version = version.map(str::trim).filter(|v| !v.is_empty()).unwrap_or("latest");
    if !valid_version(version) {
        return Err("invalid version".into());
    }
    let mut url = Url::parse(REGISTRY_BASE).map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "registry URL cannot take a path".to_string())?
        .extend(["v0", "servers", name, "versions", version]);
    let body = fetch(&url).await?;
    let (server, meta) = match body.get("server") {
        Some(s) if s.is_object() => (s.clone(), body.get("_meta").cloned()),
        _ => (body.clone(), body.get("_meta").cloned()),
    };
    if s(&server, "name") != name {
        return Err("the registry answered with a different server".into());
    }
    Ok((server, meta))
}

/// Pin a package definition to the version that was reviewed: `npx -y
/// <pkg>@<ver>`, `uvx <pkg>==<ver>`. OCI identifiers carry their own tag and
/// are left alone, as is any version that is not a plain version string.
pub fn pin_package_version(def: &mut duduclaw_agent::mcp_template::McpServerDef, package: &Value) {
    let version = s(package, "version");
    if version.is_empty()
        || version == "latest"
        || !version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    {
        return;
    }
    let identifier = s(package, "identifier");
    match registry_type(package).as_str() {
        "npm" => {
            if let Some(arg) = def.args.iter_mut().find(|a| *a == identifier) {
                *arg = format!("{identifier}@{version}");
            }
        }
        "pypi" => {
            if let Some(arg) = def.args.iter_mut().find(|a| *a == identifier) {
                *arg = format!("{identifier}=={version}");
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zapier() -> Value {
        json!({
            "server": {
                "name": "com.zapier/mcp",
                "title": "Zapier",
                "description": "Hosted MCP server connecting AI assistants to 9,000+ apps.",
                "repository": {"url": "https://github.com/zapier/zapier-mcp", "source": "github"},
                "version": "1.0.1",
                "websiteUrl": "https://docs.zapier.com/mcp/home",
                "remotes": [{"type": "streamable-http", "url": "https://mcp.zapier.com/api/v1/connect"}]
            },
            "_meta": {"io.modelcontextprotocol.registry/official": {"status": "active", "isLatest": true}}
        })
    }

    #[test]
    fn remote_only_server_normalizes_as_installable_remote() {
        let body = json!({"servers": [zapier()], "metadata": {"nextCursor": "com.zapier/mcp:1.0.1", "count": 1}});
        let (hits, next) = normalize_search_response(&body);
        assert_eq!(next.as_deref(), Some("com.zapier/mcp:1.0.1"));
        let h = &hits[0];
        assert_eq!(h.name, "com.zapier/mcp");
        assert_eq!(h.title, "Zapier");
        assert!(h.installable);
        assert!(h.install_is_remote);
        assert!(h.has_remotes);
        assert_eq!(h.remote_kinds, vec!["streamable-http"]);
        assert!(!h.remote_needs_bearer);
        assert_eq!(h.repository_url.as_deref(), Some("https://github.com/zapier/zapier-mcp"));
    }

    #[test]
    fn package_server_lists_kinds_and_required_env() {
        let server = json!({
            "name": "io.github.x/fs",
            "description": "d",
            "version": "0.1.9",
            "packages": [{
                "registryType": "npm",
                "identifier": "@x/mcp-fs",
                "version": "0.1.9",
                "environmentVariables": [
                    {"name": "WORKSPACE_ROOT", "isRequired": true, "description": "root"},
                    {"name": "TOKEN", "isSecret": true},
                    {"name": "bad name"}
                ]
            }, {"registryType": "nuget", "identifier": "X"}]
        });
        let h = normalize_server(&server, None);
        assert!(h.installable);
        assert!(!h.install_is_remote);
        assert_eq!(h.package_kinds, vec!["npm", "nuget"]);
        assert_eq!(h.required_env.len(), 2);
        assert!(h.required_env[0].required);
        assert!(h.required_env[1].secret);
    }

    #[test]
    fn unsupported_shapes_carry_a_reason() {
        let sse = json!({"name": "a.b/c", "remotes": [{"type": "sse", "url": "https://x.example/sse"}]});
        assert_eq!(normalize_server(&sse, None).reason.as_deref(), Some("sse_remote_only"));
        // Declared custom headers are installable since 2026-10-08: the
        // connect dialog asks for each one.
        let hdr = json!({"name": "a.b/c", "remotes": [{"type": "streamable-http", "url": "https://x.example/mcp",
            "headers": [
                {"name": "X-API-Key", "isRequired": true, "isSecret": true, "description": "Your key\u{0007}"},
                {"name": "X-Region", "isRequired": false},
                {"name": "Authorization", "isRequired": true}
            ]}]});
        let h = normalize_server(&hdr, None);
        assert!(h.installable && h.install_is_remote, "{h:?}");
        assert_eq!(
            h.remote_headers,
            vec![
                HeaderInfo { name: "X-API-Key".into(), description: "Your key".into(), required: true, secret: true },
                HeaderInfo { name: "X-Region".into(), description: String::new(), required: false, secret: false },
            ]
        );
        // A required header DuDuClaw cannot send stays not installable.
        let cookie = json!({"name": "a.b/c", "remotes": [{"type": "streamable-http", "url": "https://x.example/mcp",
            "headers": [{"name": "Cookie", "isRequired": true}]}]});
        assert_eq!(normalize_server(&cookie, None).reason.as_deref(), Some("custom_headers"));
        // An optional one is simply not asked for.
        let opt = json!({"name": "a.b/c", "remotes": [{"type": "streamable-http", "url": "https://x.example/mcp",
            "headers": [{"name": "Cookie", "isRequired": false}]}]});
        let h = normalize_server(&opt, None);
        assert!(h.installable && h.remote_headers.is_empty());
        let bearer = json!({"name": "a.b/c", "remotes": [{"type": "streamable-http", "url": "https://x.example/mcp",
            "headers": [{"name": "Authorization", "isRequired": true, "value": "Bearer {k}"}]}]});
        let h = normalize_server(&bearer, None);
        assert!(h.installable && h.remote_needs_bearer);
        let nuget = json!({"name": "a.b/c", "packages": [{"registryType": "nuget", "identifier": "X"}]});
        assert_eq!(normalize_server(&nuget, None).reason.as_deref(), Some("no_supported_transport"));
        let deleted = zapier();
        let mut meta = deleted["_meta"].clone();
        meta["io.modelcontextprotocol.registry/official"]["status"] = json!("deleted");
        let h = normalize_server(&deleted["server"], Some(&meta));
        assert!(!h.installable);
        assert_eq!(h.reason.as_deref(), Some("deleted"));
    }

    #[test]
    fn long_and_control_text_is_cleaned_safely() {
        let server = json!({"name": "a.b/c", "description": format!("{}\u{7}", "說明".repeat(400)),
            "repository": {"url": "javascript:alert(1)"}, "packages": [{"registryType": "npm", "identifier": "x"}]});
        let h = normalize_server(&server, None);
        assert!(h.description.chars().count() <= 300);
        assert!(!h.description.contains('\u{7}'));
        assert!(h.repository_url.is_none());
    }

    #[test]
    fn name_version_validation() {
        assert!(valid_server_name("com.zapier/mcp"));
        assert!(valid_server_name("io.github.ComposioHQ/composio"));
        assert!(!valid_server_name("noslash"));
        assert!(!valid_server_name("a/../b"));
        assert!(!valid_server_name("a/b?x=1"));
        assert!(valid_version("latest"));
        assert!(valid_version("1.0.0-rc.1"));
        assert!(!valid_version("1.0 0"));
        assert!(!valid_version("../x"));
    }

    #[test]
    fn package_versions_are_pinned() {
        let mut def = duduclaw_agent::mcp_template::McpServerDef { command: "npx".into(), args: vec!["-y".into(), "@x/y".into()], env: Default::default() };
        pin_package_version(&mut def, &json!({"registryType": "npm", "identifier": "@x/y", "version": "1.2.3"}));
        assert_eq!(def.args[1], "@x/y@1.2.3");
        let mut def = duduclaw_agent::mcp_template::McpServerDef { command: "uvx".into(), args: vec!["pkg".into()], env: Default::default() };
        pin_package_version(&mut def, &json!({"registryType": "pypi", "identifier": "pkg", "version": "0.4"}));
        assert_eq!(def.args[0], "pkg==0.4");
        let mut def = duduclaw_agent::mcp_template::McpServerDef { command: "npx".into(), args: vec!["-y".into(), "p".into()], env: Default::default() };
        pin_package_version(&mut def, &json!({"registryType": "npm", "identifier": "p", "version": "1.0;rm"}));
        assert_eq!(def.args[1], "p");
    }

    #[test]
    fn fetch_url_encodes_the_name_as_one_segment() {
        let mut url = Url::parse(REGISTRY_BASE).unwrap();
        url.path_segments_mut().unwrap().extend(["v0", "servers", "com.zapier/mcp", "versions", "latest"]);
        assert_eq!(url.as_str(), "https://registry.modelcontextprotocol.io/v0/servers/com.zapier%2Fmcp/versions/latest");
    }
}
