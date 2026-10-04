//! Pre-spawn setup for the Antigravity (`agy`) runtime: the auth route, the
//! MCP registration and the spawn environment.
//!
//! Split out of `antigravity.rs` so every decision is a pure function over
//! explicit paths / strings and can be unit-tested without spawning `agy` and
//! without touching the real `~/.gemini`.
//!
//! First-hand facts (agy 1.2.14, 2026-10-01 —
//! `commercial/docs/REPORT-antigravity-runtime-auth-2026-10.md`):
//!   - there is no `ANTIGRAVITY_API_KEY` (the binary does not contain the
//!     string); the API-key route needs `GEMINI_API_KEY` in the environment
//!     AND `"modelProvider": "gemini"` in `~/.gemini/antigravity-cli/settings.json`;
//!   - `mcpServers` inside any `settings.json` is ignored. A headless run loads
//!     `<workspace>/.agents/mcp_config.json` when the workspace is in
//!     `trustedWorkspaces`; the MCP child inherits agy's spawn environment and
//!     concurrent runs each keep their own environment.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The one variable agy reads for the Gemini API-key route.
pub(crate) const GEMINI_KEY_ENV: &str = "GEMINI_API_KEY";
/// Provider id used for the account rotator and the env-var table.
pub(crate) const GEMINI_PROVIDER: &str = "gemini";
/// Name the DuDuClaw MCP server is registered under.
pub(crate) const MCP_SERVER_NAME: &str = "duduclaw";
/// The one `permissions.allow` rule the gateway adds to agy's user settings:
/// every tool of the MCP server registered as [`MCP_SERVER_NAME`].
///
/// Why (agy 1.2.16, measured 2026-10-04): in print mode agy runs with
/// `toolPermission = request-review` and soft-denies any tool confirmation it
/// cannot ask a human about, so under `--sandbox` every DuDuClaw MCP call was
/// refused and the run failed. agy matches `permissions.allow` rules per kind
/// (`command(…)`, `read_file(…)`, `write_file(…)`, `read_url(…)`,
/// `execute_url(…)`, `mcp(<server>/<tool>)`); an `mcp(…)` rule approves MCP
/// calls only, so shell commands and file writes keep their confirmation and
/// are still denied headless. The DuDuClaw MCP server authorizes every call
/// itself (scopes, `[capabilities]`, approval lists).
pub(crate) const MCP_TOOL_GRANT: &str = "mcp(duduclaw/*)";

/// The `permissions.allow` rules the gateway needs for the duduclaw MCP server
/// under `user_home`: [`MCP_TOOL_GRANT`] plus a read-only grant on agy's
/// lazily generated schema directory for that one server,
/// `<user_home>/.gemini/antigravity-cli/mcp/duduclaw` (agy 1.2.16 tells the
/// model to read `<tool>.json` there before each call; outside the system temp
/// dir that read is soft-denied in print mode exactly like the call itself).
/// When `user_home` canonicalizes to another spelling (macOS `/var` →
/// `/private/var`) both spellings are granted. Nothing here grants a shell
/// command, a file write, or a read outside that directory.
///
/// A spelling that [`schema_read_grant`] refuses is left out with a `warn!`
/// (the MCP grant is still returned): a broken or wider rule is worse than a
/// missing one.
pub(crate) fn tool_grants(user_home: &Path) -> Vec<String> {
    let mut grants = vec![MCP_TOOL_GRANT.to_string()];
    let mut spellings = vec![user_home.to_path_buf()];
    if let Ok(canon) = user_home.canonicalize() {
        if canon != user_home {
            spellings.push(canon);
        }
    }
    for home in spellings {
        match schema_read_grant(&home) {
            Ok(rule) => {
                if !grants.contains(&rule) {
                    grants.push(rule);
                }
            }
            Err(reason) => tracing::warn!(
                runtime = "antigravity",
                reason = %reason,
                "not adding the agy read rule for the duduclaw MCP schema directory; \
                 agy may refuse to read tool schemas in print mode"
            ),
        }
    }
    grants
}

/// `read_file(<home>/.gemini/antigravity-cli/mcp/duduclaw)`, or why it is not
/// written. Refused: a path that is not valid UTF-8, or that contains `(`,
/// `)`, `,`, `*`, CR or LF — agy's parsing of those characters inside a rule
/// is not known, so such a rule could be broken or match more than intended.
pub(crate) fn schema_read_grant(home: &Path) -> Result<String, String> {
    let dir = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("mcp")
        .join(MCP_SERVER_NAME);
    let Some(text) = dir.to_str() else {
        return Err("the user home path is not valid UTF-8".to_string());
    };
    if let Some(c) = text
        .chars()
        .find(|c| matches!(c, '(' | ')' | ',' | '*' | '\n' | '\r'))
    {
        return Err(format!(
            "the user home path contains {c:?}, which an agy permission rule may not parse safely"
        ));
    }
    Ok(format!("read_file({text})"))
}

// ── config.toml [antigravity] auth ──────────────────────────────

/// `config.toml [antigravity] auth`.
///
/// Global rather than per agent because the file `modelProvider` lives in is
/// shared by every agy run of this OS user: two agents with different values
/// would overwrite each other between calls. Two consequences the operator
/// must know about:
///   * the setting also switches the operator's OWN interactive `agy` (same OS
///     user, same `~/.gemini/antigravity-cli/settings.json`) to the key route;
///   * two DuDuClaw gateways running as one OS user with different `auth`
///     values overwrite each other's `modelProvider` on every call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AntigravityAuth {
    /// Key absent (or unrecognised): leave `modelProvider` alone and inject no
    /// key — the behaviour before this setting existed.
    Unset,
    /// Explicit Google sign-in: remove `modelProvider = "gemini"` if present.
    Login,
    /// Gemini API key: ensure `modelProvider = "gemini"` and pass the key.
    ApiKey,
}

/// Why a present `[antigravity] auth` value was not understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthParseIssue {
    NotAString,
    Unknown(String),
}

/// Parse `[antigravity] auth` from an already-parsed `config.toml`.
/// `Ok(Unset)` when the section or key is absent.
pub(crate) fn parse_auth_mode(table: &toml::Table) -> Result<AntigravityAuth, AuthParseIssue> {
    let raw = table
        .get("antigravity")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("auth"));
    match raw {
        None => Ok(AntigravityAuth::Unset),
        Some(v) => match v.as_str() {
            None => Err(AuthParseIssue::NotAString),
            Some(s) => match s.trim() {
                "login" => Ok(AntigravityAuth::Login),
                "api_key" => Ok(AntigravityAuth::ApiKey),
                other => Err(AuthParseIssue::Unknown(other.to_string())),
            },
        },
    }
}

/// Read `config.toml [antigravity] auth` from the DuDuClaw home, per call (the
/// same hot-reload timing as `[dispatch] judge`). A missing/unreadable/malformed
/// file is `Unset`; an unrecognised value warns once per process and is treated
/// as absent.
pub(crate) fn auth_mode_from_home(home_dir: &Path) -> AntigravityAuth {
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return AntigravityAuth::Unset;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return AntigravityAuth::Unset;
    };
    match parse_auth_mode(&table) {
        Ok(mode) => mode,
        Err(issue) => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                let shown = match &issue {
                    AuthParseIssue::NotAString => "<not a string>".to_string(),
                    AuthParseIssue::Unknown(s) => duduclaw_core::truncate_chars(s, 40).to_string(),
                };
                tracing::warn!(
                    value = %shown,
                    "unknown config.toml [antigravity] auth — treated as absent. \
                     Valid: \"login\", \"api_key\""
                );
            });
            AntigravityAuth::Unset
        }
    }
}

// ── Gemini key ───────────────────────────────────────────────────

/// A resolved Gemini API key. Never Debug-printed, never logged.
pub(crate) struct GeminiKey(String);

impl GeminiKey {
    /// `None` for an empty / whitespace-only value.
    pub(crate) fn new(raw: String) -> Option<Self> {
        if raw.trim().is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for GeminiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GeminiKey(<redacted>)")
    }
}

/// Pure: prefer the rotator-selected account's key, else the env key.
pub(crate) fn choose_gemini_key(
    rotator_key: Option<String>,
    env_key: Option<String>,
) -> Option<GeminiKey> {
    rotator_key
        .and_then(GeminiKey::new)
        .or_else(|| env_key.and_then(GeminiKey::new))
}

/// Resolve the Gemini key: the account rotator's `gemini` provider (honouring
/// the agent's `account_pool`), else `GEMINI_API_KEY` / `GOOGLE_API_KEY`.
/// An OAuth-shaped rotator pick (no static key) falls through to env.
pub(crate) async fn resolve_gemini_key(home_dir: &Path, account_pool: &[String]) -> Option<GeminiKey> {
    let rotator_key = match crate::claude_runner::get_rotator_cached(home_dir).await {
        Ok(rotator) => rotator
            .select_for_provider_with_pool(GEMINI_PROVIDER, account_pool)
            .await
            .and_then(|env| env.raw_key),
        Err(e) => {
            tracing::warn!(error = %e, "antigravity: account rotator unavailable — trying env vars");
            None
        }
    };
    choose_gemini_key(
        rotator_key,
        duduclaw_core::provider_env::resolve_env_key(GEMINI_PROVIDER),
    )
}

/// Pure gate run BEFORE spawning: in `api_key` mode a missing key is an error
/// (agy would otherwise only say "authentication required"); in every other
/// mode no key is passed at all.
pub(crate) fn require_key_for_mode(
    auth: AntigravityAuth,
    key: Option<GeminiKey>,
) -> Result<Option<GeminiKey>, String> {
    match auth {
        AntigravityAuth::ApiKey => match key {
            Some(k) => Ok(Some(k)),
            None => Err(format!(
                "Antigravity 設為 API key 模式（config.toml [antigravity] auth = \"api_key\"），\
                 但找不到 Gemini API key：請在帳號頁新增 gemini 帳號，或在 gateway 環境設定 \
                 {GEMINI_KEY_ENV}（亦接受 GOOGLE_API_KEY）。若要改用 Google 登入，請把 auth 改成 \
                 \"login\" 並在本機終端機執行 `agy` 完成登入。注意：此設定作用於整個作業系統使用者——\
                 它同時讓操作者自己互動執行的 `agy` 也改走 API key；同一使用者下若有兩個 gateway \
                 設定不同的 auth，會在每次呼叫時互相覆寫。"
            )),
        },
        AntigravityAuth::Login | AntigravityAuth::Unset => Ok(None),
    }
}

// ── ~/.gemini/antigravity-cli/settings.json ──────────────────────

/// agy's user-level settings file under `user_home`.
pub(crate) fn user_settings_path(user_home: &Path) -> PathBuf {
    user_home
        .join(".gemini")
        .join("antigravity-cli")
        .join("settings.json")
}

/// What [`merge_user_settings`] produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MergedSettings {
    /// New file content when anything changed, `None` when the file already
    /// says what we want.
    pub(crate) content: Option<String>,
    /// Why the `permissions.allow` rules could not be added (the operator's
    /// `permissions` has an unexpected shape and is left untouched). The
    /// trust and `modelProvider` changes are still in `content`.
    pub(crate) grant_issue: Option<String>,
}

/// Pure merge of the user settings.
///
/// * `trusted_dir` is appended to `trustedWorkspaces` when missing, and each
///   of `grants` (see [`tool_grants`]) to `permissions.allow` (every auth mode).
/// * `ApiKey` sets `modelProvider = "gemini"`; `Login` removes it only when it
///   equals `"gemini"` (any other provider is the operator's own choice);
///   `Unset` never touches it.
/// * Unrelated keys are preserved. Content that is not a JSON object is
///   refused rather than overwritten — it is the operator's file.
pub(crate) fn merge_user_settings(
    existing: Option<&str>,
    trusted_dir: Option<&str>,
    auth: AntigravityAuth,
    grants: &[String],
) -> Result<MergedSettings, String> {
    let mut settings: Value = match existing.map(str::trim) {
        None | Some("") => serde_json::json!({}),
        Some(s) => serde_json::from_str(s)
            .map_err(|e| format!("antigravity settings.json is not valid JSON ({e}); not rewriting it"))?,
    };
    let Some(obj) = settings.as_object_mut() else {
        return Err("antigravity settings.json is not a JSON object; not rewriting it".to_string());
    };
    let mut changed = false;
    let mut grant_issue = None;

    if let Some(dir) = trusted_dir {
        let mut list: Vec<Value> = match obj.get("trustedWorkspaces") {
            None => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => {
                return Err(
                    "antigravity settings.json has a non-array trustedWorkspaces; not rewriting it"
                        .to_string(),
                );
            }
        };
        if !list.iter().any(|v| v.as_str() == Some(dir)) {
            list.push(Value::String(dir.to_string()));
            obj.insert("trustedWorkspaces".to_string(), Value::Array(list));
            changed = true;
        }
        // The trusted dir is the workspace whose `.agents/mcp_config.json`
        // registers the duduclaw server; without the grants agy's print mode
        // refuses every call to it.
        // A malformed `permissions` must not block trust / modelProvider:
        // those are what keep agy from hanging on its trust prompt.
        match ensure_allow_rules(obj, grants) {
            Ok(c) => changed |= c,
            Err(e) => grant_issue = Some(e),
        }
    }

    match auth {
        AntigravityAuth::ApiKey => {
            if obj.get("modelProvider").and_then(Value::as_str) != Some(GEMINI_PROVIDER) {
                obj.insert(
                    "modelProvider".to_string(),
                    Value::String(GEMINI_PROVIDER.to_string()),
                );
                changed = true;
            }
        }
        AntigravityAuth::Login => {
            if obj.get("modelProvider").and_then(Value::as_str) == Some(GEMINI_PROVIDER) {
                obj.remove("modelProvider");
                changed = true;
            }
        }
        AntigravityAuth::Unset => {}
    }

    if !changed {
        return Ok(MergedSettings {
            content: None,
            grant_issue,
        });
    }
    serde_json::to_string_pretty(&settings)
        .map(|out| MergedSettings {
            content: Some(out),
            grant_issue,
        })
        .map_err(|e| e.to_string())
}

/// Append each of `rules` to `permissions.allow` when missing. Returns whether
/// anything changed. The operator's own `allow`/`deny`/`ask` rules are
/// kept as they are; a `permissions` value that is not an object, or an `allow`
/// that is not an array, is refused rather than overwritten (`obj` is left
/// unchanged on that path).
fn ensure_allow_rules(
    obj: &mut serde_json::Map<String, Value>,
    rules: &[String],
) -> Result<bool, String> {
    let perms = obj
        .entry("permissions")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(perms) = perms.as_object_mut() else {
        return Err(
            "antigravity settings.json has a non-object permissions; not rewriting it".to_string(),
        );
    };
    let allow = perms
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(allow) = allow.as_array_mut() else {
        return Err(
            "antigravity settings.json has a non-array permissions.allow; not rewriting it"
                .to_string(),
        );
    };
    let mut changed = false;
    for rule in rules {
        if !allow.iter().any(|v| v.as_str() == Some(rule.as_str())) {
            allow.push(Value::String(rule.clone()));
            changed = true;
        }
    }
    Ok(changed)
}

/// Pure (C1): `true` when the settings still route agy through the Gemini key
/// (`modelProvider == "gemini"`, typically left by an earlier `api_key`
/// setting) while no Gemini key is available to the gateway — every agy call
/// in that state fails on a missing key.
pub(crate) fn stale_api_key_route(existing: Option<&str>, env_key_present: bool) -> bool {
    if env_key_present {
        return false;
    }
    existing
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| {
            v.get("modelProvider")
                .and_then(Value::as_str)
                .map(|p| p == GEMINI_PROVIDER)
        })
        .unwrap_or(false)
}

/// What [`ensure_user_settings`] did and saw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SettingsOutcome {
    pub(crate) wrote: bool,
    /// The file (as read, before any write) had `modelProvider == "gemini"`.
    pub(crate) had_gemini_provider: bool,
    /// See [`MergedSettings::grant_issue`].
    pub(crate) grant_issue: Option<String>,
}

/// Apply [`merge_user_settings`] to `<user_home>/.gemini/antigravity-cli/settings.json`
/// in ONE cross-process locked read-merge-write (the file is shared by every
/// agent and every gateway of this OS user).
///
/// A symlinked settings file (dotfile managers) is written through to its
/// resolved target rather than replaced by a plain file. A newly created file
/// is 0600; an existing one keeps its mode.
pub(crate) fn ensure_user_settings(
    user_home: &Path,
    trusted_dir: Option<&Path>,
    auth: AntigravityAuth,
) -> std::io::Result<SettingsOutcome> {
    // Canonicalize so the stored path matches what agy compares against.
    let trusted = trusted_dir.map(|d| {
        d.canonicalize()
            .unwrap_or_else(|_| d.to_path_buf())
            .to_string_lossy()
            .into_owned()
    });
    let path = user_settings_path(user_home);
    let had_gemini = |existing: Option<&str>| stale_api_key_route(existing, false);
    if trusted.is_none() && auth == AntigravityAuth::Unset {
        // Nothing to write: read only (never create the file or its dir).
        let existing = match resolve_write_target(&path) {
            Ok(target) => read_optional(&target)?,
            Err(_) => None,
        };
        return Ok(SettingsOutcome {
            wrote: false,
            had_gemini_provider: had_gemini(existing.as_deref()),
            grant_issue: None,
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    duduclaw_core::with_file_lock(&path, || {
        let target = resolve_write_target(&path)?;
        let existing = read_optional(&target)?;
        let had_gemini_provider = had_gemini(existing.as_deref());
        let grants = tool_grants(user_home);
        match merge_user_settings(existing.as_deref(), trusted.as_deref(), auth, &grants) {
            Ok(MergedSettings {
                content: None,
                grant_issue,
            }) => Ok(SettingsOutcome {
                wrote: false,
                had_gemini_provider,
                grant_issue,
            }),
            Ok(MergedSettings {
                content: Some(out),
                grant_issue,
            }) => write_user_file(&target, &out).map(|()| SettingsOutcome {
                wrote: true,
                had_gemini_provider,
                grant_issue,
            }),
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        }
    })
}

/// The path to actually write: `path` itself, or — when `path` is a symlink —
/// its resolved target, which must be an existing regular file.
fn resolve_write_target(path: &Path) -> std::io::Result<PathBuf> {
    if !super::antigravity_fs::is_symlink(path) {
        return Ok(path.to_path_buf());
    }
    let target = path.canonicalize()?;
    if !target.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "antigravity settings.json symlink does not resolve to a regular file",
        ));
    }
    Ok(target)
}

// ── <work_root>/.agents/mcp_config.json ──────────────────────────

const AGENTS_DIR: &str = ".agents";
const MCP_CONFIG_FILE: &str = "mcp_config.json";

/// The workspace MCP file agy loads for a trusted workspace.
#[cfg(test)]
pub(crate) fn mcp_config_path(work_root: &Path) -> PathBuf {
    work_root.join(AGENTS_DIR).join(MCP_CONFIG_FILE)
}

/// Lock for one work root's `mcp_config.json`. Deliberately NOT next to the
/// file: the work root is agent-writable, and `with_file_lock` creates its
/// sidecar with create+follow, so a planted symlink there could create or
/// truncate a file elsewhere. The lock lives under the DuDuClaw home instead,
/// keyed by a hash of the canonical work root.
pub(crate) fn mcp_lock_path(duduclaw_home: &Path, work_root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let canon = work_root
        .canonicalize()
        .unwrap_or_else(|_| work_root.to_path_buf());
    let digest = Sha256::digest(canon.to_string_lossy().as_bytes());
    duduclaw_home
        .join("locks")
        .join(format!("antigravity-mcp-{}", hex::encode(&digest[..16])))
}

/// An identity-free server entry: `command` + `args` only. Identity rides the
/// spawn environment, so nothing secret is written to the workspace. `None`
/// when `command` is not absolute (a PATH-relative command breaks for agy
/// launched without the interactive PATH).
pub(crate) fn mcp_server_entry(command: &Path, args: &[String]) -> Option<Value> {
    if !command.is_absolute() {
        return None;
    }
    Some(serde_json::json!({
        "command": command.to_string_lossy(),
        "args": args,
    }))
}

/// Pure merge of `servers` into an existing `mcp_config.json`, per server name.
/// Other servers and unrelated keys are preserved; `Ok(None)` when every entry
/// already matches. Content that is not a JSON object is refused.
pub(crate) fn merge_mcp_config(
    existing: Option<&str>,
    servers: &[(String, Value)],
) -> Result<Option<String>, String> {
    let mut cfg: Value = match existing.map(str::trim) {
        None | Some("") => serde_json::json!({}),
        Some(s) => serde_json::from_str(s)
            .map_err(|e| format!("mcp_config.json is not valid JSON ({e}); not rewriting it"))?,
    };
    let Some(obj) = cfg.as_object_mut() else {
        return Err("mcp_config.json is not a JSON object; not rewriting it".to_string());
    };
    let entry = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    let map = entry
        .as_object_mut()
        .expect("mcpServers normalized to an object above");
    let mut changed = false;
    for (name, def) in servers {
        if map.get(name) != Some(def) {
            map.insert(name.clone(), def.clone());
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    serde_json::to_string_pretty(&cfg)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Write `servers` into `<work_root>/.agents/mcp_config.json`. Returns whether
/// it wrote.
///
/// The work root is agent-writable, so every step below `work_root` is
/// symlink-safe ([`super::antigravity_fs::SafeDir`]): a symlinked `.agents`,
/// a symlinked or non-regular `mcp_config.json`, and a planted temp name are
/// all refused or bypassed, and nothing outside the work root is created,
/// read or modified. The lock lives under `duduclaw_home` ([`mcp_lock_path`]).
pub(crate) fn write_mcp_config(
    work_root: &Path,
    duduclaw_home: &Path,
    servers: &[(String, Value)],
) -> std::io::Result<bool> {
    use super::antigravity_fs::SafeDir;
    let root = SafeDir::open_root(work_root)?;
    let lock = mcp_lock_path(duduclaw_home, work_root);
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    duduclaw_core::with_file_lock(&lock, || {
        let agents = root
            .child_dir(AGENTS_DIR, true)?
            .ok_or_else(|| std::io::Error::other(".agents could not be created"))?;
        let existing = agents.read_file(MCP_CONFIG_FILE)?;
        match merge_mcp_config(existing.as_deref(), servers) {
            Ok(None) => Ok(false),
            Ok(Some(out)) => agents.write_file_atomic(MCP_CONFIG_FILE, &out).map(|()| true),
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        }
    })
}

// ── legacy <agent_dir>/.gemini/antigravity-cli/settings.json ─────

/// Where pre-2026-10 builds wrote the (never read) MCP registration.
#[cfg(test)]
pub(crate) fn legacy_agent_settings_path(agent_dir: &Path) -> PathBuf {
    user_settings_path(agent_dir)
}

/// `true` only when `content` is exactly what the old writer produced: a JSON
/// object whose single top-level key is `mcpServers`, whose single server is
/// `duduclaw`. Anything an operator added keeps the file.
pub(crate) fn legacy_settings_is_mcp_only(content: &str) -> bool {
    match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(map)) if map.len() == 1 => match map.get("mcpServers") {
            Some(Value::Object(servers)) => {
                servers.len() == 1 && servers.contains_key(MCP_SERVER_NAME)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Delete the legacy per-agent file when it is a regular file reached without
/// any symlink (`.gemini`, `antigravity-cli` and the file itself) and holds
/// nothing but the old `duduclaw` MCP block (a plaintext agent token agy never
/// reads). Anything else is left alone. Returns whether it deleted; a symlink
/// on the way is an `Err` so the caller can warn.
pub(crate) fn remove_legacy_agent_settings(agent_dir: &Path) -> std::io::Result<bool> {
    use super::antigravity_fs::SafeDir;
    let root = SafeDir::open_root(agent_dir)?;
    let Some(gemini) = root.child_dir(".gemini", false)? else {
        return Ok(false);
    };
    let Some(cli) = gemini.child_dir("antigravity-cli", false)? else {
        return Ok(false);
    };
    let Some(content) = cli.read_file("settings.json")? else {
        return Ok(false);
    };
    if !legacy_settings_is_mcp_only(&content) {
        return Ok(false);
    }
    cli.remove_file("settings.json")?;
    Ok(true)
}

// ── spawn environment ────────────────────────────────────────────

/// Identity + MCP forward set + `DUDUCLAW_HOME` for the agy child, mirroring the
/// Grok runtime. The MCP server agy starts inherits these, so a role member
/// borrowing an employee's workspace still authenticates as itself.
/// `DUDUCLAW_HOME` is always `home_dir` (the RuntimeContext is authoritative
/// for isolated eval arms), never the ambient one.
pub(crate) fn identity_env_pairs(home_dir: &Path, agent_id: &str) -> Vec<(String, String)> {
    let mut out = duduclaw_core::agent_identity_env_vars(home_dir, agent_id);
    out.extend(
        duduclaw_core::mcp_forward_env_vars()
            .into_iter()
            .filter(|(k, _)| k != "DUDUCLAW_HOME"),
    );
    out.push((
        "DUDUCLAW_HOME".to_string(),
        home_dir.to_string_lossy().into_owned(),
    ));
    out
}

// ── key redaction ────────────────────────────────────────────────

/// Replace the exact resolved key and anything shaped like a Google API key
/// (`AIza` + ≥30 of `[0-9A-Za-z_-]`) with `<redacted>`. Apply to every string
/// derived from agy's output BEFORE truncating it, so a cut never leaves a
/// recognisable key prefix behind.
pub(crate) fn redact_key(text: &str, key: Option<&str>) -> String {
    static GOOGLE_KEY: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let mut out = match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => text.replace(k, "<redacted>"),
        None => text.to_string(),
    };
    let re = GOOGLE_KEY
        .get_or_init(|| regex::Regex::new(r"AIza[0-9A-Za-z_\-]{30,}").expect("static regex"));
    if re.is_match(&out) {
        out = re.replace_all(&out, "<redacted>").into_owned();
    }
    out
}

// ── small IO helpers ─────────────────────────────────────────────

fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Replace the (operator-owned, not agent-writable) user settings file:
/// random-named temp in the same directory (`O_EXCL`), then rename. A new
/// file is 0600; an existing file keeps its permissions.
fn write_user_file(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("settings path has no parent"))?;
    let existing_perms = std::fs::metadata(path).ok().map(|m| m.permissions());
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    match existing_perms {
        Some(p) => std::fs::set_permissions(tmp.path(), p)?,
        None => duduclaw_core::platform::set_owner_only(tmp.path())?,
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    // ── auth mode ──

    #[test]
    fn auth_mode_parses_the_three_states() {
        let t = |s: &str| s.parse::<toml::Table>().unwrap();
        assert_eq!(parse_auth_mode(&t("")), Ok(AntigravityAuth::Unset));
        assert_eq!(
            parse_auth_mode(&t("[antigravity]\nother = 1\n")),
            Ok(AntigravityAuth::Unset)
        );
        assert_eq!(
            parse_auth_mode(&t("[antigravity]\nauth = \"login\"\n")),
            Ok(AntigravityAuth::Login)
        );
        assert_eq!(
            parse_auth_mode(&t("[antigravity]\nauth = \"api_key\"\n")),
            Ok(AntigravityAuth::ApiKey)
        );
        assert_eq!(
            parse_auth_mode(&t("[antigravity]\nauth = \"apikey\"\n")),
            Err(AuthParseIssue::Unknown("apikey".to_string()))
        );
        assert_eq!(
            parse_auth_mode(&t("[antigravity]\nauth = true\n")),
            Err(AuthParseIssue::NotAString)
        );
    }

    #[test]
    fn auth_mode_from_home_treats_unknown_and_malformed_as_unset() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(auth_mode_from_home(dir.path()), AntigravityAuth::Unset);
        std::fs::write(dir.path().join("config.toml"), "not = = toml").unwrap();
        assert_eq!(auth_mode_from_home(dir.path()), AntigravityAuth::Unset);
        std::fs::write(
            dir.path().join("config.toml"),
            "[antigravity]\nauth = \"bogus\"\n",
        )
        .unwrap();
        assert_eq!(auth_mode_from_home(dir.path()), AntigravityAuth::Unset);
        std::fs::write(
            dir.path().join("config.toml"),
            "[antigravity]\nauth = \"api_key\"\n",
        )
        .unwrap();
        assert_eq!(auth_mode_from_home(dir.path()), AntigravityAuth::ApiKey);
    }

    // ── key ──

    #[test]
    fn rotator_key_wins_and_empty_values_count_as_missing() {
        let k = choose_gemini_key(Some("rot".into()), Some("env".into())).unwrap();
        assert_eq!(k.expose(), "rot");
        let k = choose_gemini_key(None, Some("env".into())).unwrap();
        assert_eq!(k.expose(), "env");
        let k = choose_gemini_key(Some("  ".into()), Some("env".into())).unwrap();
        assert_eq!(k.expose(), "env");
        assert!(choose_gemini_key(None, Some(String::new())).is_none());
        assert!(choose_gemini_key(None, None).is_none());
    }

    #[test]
    fn key_debug_never_prints_the_value() {
        let k = GeminiKey::new("AIza-secret-value".into()).unwrap();
        let shown = format!("{k:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    #[test]
    fn api_key_mode_without_a_key_errors_before_spawn_and_names_the_setting() {
        let err = require_key_for_mode(AntigravityAuth::ApiKey, None).unwrap_err();
        assert!(err.contains("[antigravity] auth"), "{err}");
        assert!(err.contains("GEMINI_API_KEY"), "{err}");
        assert!(!err.contains("ANTIGRAVITY_API_KEY"), "{err}");
    }

    #[test]
    fn only_api_key_mode_passes_a_key() {
        let key = || GeminiKey::new("k".into());
        assert!(
            require_key_for_mode(AntigravityAuth::ApiKey, key())
                .unwrap()
                .is_some()
        );
        assert!(
            require_key_for_mode(AntigravityAuth::Login, key())
                .unwrap()
                .is_none()
        );
        assert!(
            require_key_for_mode(AntigravityAuth::Unset, key())
                .unwrap()
                .is_none()
        );
        // No key is not an error outside api_key mode.
        assert!(require_key_for_mode(AntigravityAuth::Unset, None).is_ok());
        assert!(require_key_for_mode(AntigravityAuth::Login, None).is_ok());
    }

    // ── user settings ──

    /// Fixed fake home for the pure-merge tests (does not exist, so
    /// [`tool_grants`] yields exactly one spelling).
    const FAKE_HOME: &str = "/u";

    fn grants() -> Vec<String> {
        tool_grants(Path::new(FAKE_HOME))
    }

    /// The pure merge as `ensure_user_settings` calls it, for [`FAKE_HOME`].
    fn merge_user_settings(
        existing: Option<&str>,
        trusted_dir: Option<&str>,
        auth: AntigravityAuth,
    ) -> Result<Option<String>, String> {
        super::merge_user_settings(existing, trusted_dir, auth, &grants()).map(|m| m.content)
    }

    const EXISTING: &str = r#"{"trustedWorkspaces":["/a"],"theme":"dark","modelProvider":"gemini","permissions":{"allow":["mcp(duduclaw/*)","read_file(/u/.gemini/antigravity-cli/mcp/duduclaw)"]}}"#;

    #[test]
    fn unset_never_touches_model_provider() {
        assert_eq!(
            merge_user_settings(Some(EXISTING), Some("/a"), AntigravityAuth::Unset).unwrap(),
            None,
            "already trusted and Unset ⇒ no write"
        );
        let out = merge_user_settings(Some(EXISTING), Some("/b"), AntigravityAuth::Unset)
            .unwrap()
            .unwrap();
        let v = parse(&out);
        assert_eq!(v["modelProvider"], "gemini");
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["trustedWorkspaces"], serde_json::json!(["/a", "/b"]));
        // No modelProvider and Unset ⇒ none appears.
        let out = merge_user_settings(Some(r#"{"x":1}"#), Some("/b"), AntigravityAuth::Unset)
            .unwrap()
            .unwrap();
        assert!(parse(&out).get("modelProvider").is_none());
    }

    #[test]
    fn api_key_sets_model_provider_and_trust_in_one_write() {
        let out = merge_user_settings(
            Some(r#"{"trustedWorkspaces":["/a"],"theme":"dark"}"#),
            Some("/b"),
            AntigravityAuth::ApiKey,
        )
        .unwrap()
        .unwrap();
        let v = parse(&out);
        assert_eq!(v["modelProvider"], "gemini");
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["trustedWorkspaces"], serde_json::json!(["/a", "/b"]));
        // Idempotent.
        assert_eq!(
            merge_user_settings(Some(&out), Some("/b"), AntigravityAuth::ApiKey).unwrap(),
            None
        );
        // Another provider is replaced in api_key mode.
        let out = merge_user_settings(Some(r#"{"modelProvider":"vertex"}"#), None, AntigravityAuth::ApiKey)
            .unwrap()
            .unwrap();
        assert_eq!(parse(&out)["modelProvider"], "gemini");
        // Missing file.
        let out = merge_user_settings(None, None, AntigravityAuth::ApiKey)
            .unwrap()
            .unwrap();
        assert_eq!(parse(&out), serde_json::json!({"modelProvider": "gemini"}));
    }

    #[test]
    fn login_removes_model_provider_only_when_it_is_gemini() {
        let out = merge_user_settings(Some(EXISTING), Some("/a"), AntigravityAuth::Login)
            .unwrap()
            .unwrap();
        let v = parse(&out);
        assert!(v.get("modelProvider").is_none());
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["trustedWorkspaces"], serde_json::json!(["/a"]));

        assert_eq!(
            merge_user_settings(
                Some(r#"{"modelProvider":"vertex","trustedWorkspaces":["/a"],"permissions":{"allow":["mcp(duduclaw/*)","read_file(/u/.gemini/antigravity-cli/mcp/duduclaw)"]}}"#),
                Some("/a"),
                AntigravityAuth::Login
            )
            .unwrap(),
            None,
            "a non-gemini provider is the operator's choice"
        );
    }

    #[test]
    fn malformed_settings_are_refused_not_overwritten() {
        assert!(merge_user_settings(Some("{not json"), Some("/a"), AntigravityAuth::ApiKey).is_err());
        assert!(merge_user_settings(Some("[1,2]"), Some("/a"), AntigravityAuth::Unset).is_err());
        // Empty file is treated as empty object.
        assert!(
            merge_user_settings(Some("  "), Some("/a"), AntigravityAuth::Unset)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn ensure_user_settings_writes_under_the_given_home_only() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let path = user_settings_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();

        assert!(ensure_user_settings(home.path(), Some(ws.path()), AntigravityAuth::ApiKey).unwrap().wrote);
        let v = parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(v["modelProvider"], "gemini");
        assert_eq!(v["theme"], "dark");
        let canon = ws.path().canonicalize().unwrap();
        assert_eq!(
            v["trustedWorkspaces"],
            serde_json::json!([canon.to_string_lossy()])
        );
        // Second call is a no-op.
        assert!(!ensure_user_settings(home.path(), Some(ws.path()), AntigravityAuth::ApiKey).unwrap().wrote);
        // Switch back to login.
        let o = ensure_user_settings(home.path(), None, AntigravityAuth::Login).unwrap();
        assert!(o.wrote && o.had_gemini_provider);
        let v = parse(&std::fs::read_to_string(&path).unwrap());
        assert!(v.get("modelProvider").is_none());
        assert_eq!(v["theme"], "dark");
        // Unset with nothing to trust never creates the file.
        let other = tempfile::tempdir().unwrap();
        assert!(!ensure_user_settings(other.path(), None, AntigravityAuth::Unset).unwrap().wrote);
        assert!(!user_settings_path(other.path()).exists());
    }

    // ── MCP config ──

    /// An absolute command path on the host platform (a rooted path without a
    /// drive letter is not absolute on Windows).
    #[cfg(unix)]
    const ABS_COMMAND: &str = "/opt/duduclaw/bin/duduclaw";
    #[cfg(windows)]
    const ABS_COMMAND: &str = r"C:\duduclaw\bin\duduclaw.exe";

    fn entry() -> Value {
        mcp_server_entry(Path::new(ABS_COMMAND), &["mcp-server".to_string()]).unwrap()
    }

    #[test]
    fn mcp_entry_is_identity_free_and_requires_an_absolute_command() {
        let e = entry();
        let obj = e.as_object().unwrap();
        assert_eq!(obj.len(), 2, "only command + args: {e}");
        assert_eq!(e["command"], ABS_COMMAND);
        assert_eq!(e["args"], serde_json::json!(["mcp-server"]));
        assert!(e.get("env").is_none());
        assert!(mcp_server_entry(Path::new("duduclaw"), &[]).is_none());
    }

    #[test]
    fn mcp_merge_preserves_other_servers_and_is_idempotent() {
        let servers = vec![(MCP_SERVER_NAME.to_string(), entry())];
        let existing = r#"{"mcpServers":{"playwright":{"command":"/x","args":[]}},"note":1}"#;
        let out = merge_mcp_config(Some(existing), &servers).unwrap().unwrap();
        let v = parse(&out);
        assert_eq!(v["mcpServers"]["playwright"]["command"], "/x");
        assert_eq!(v["mcpServers"]["duduclaw"], entry());
        assert_eq!(v["note"], 1);
        assert_eq!(merge_mcp_config(Some(&out), &servers).unwrap(), None);
        // A stale entry with a plaintext env block is replaced by the clean one.
        let stale = r#"{"mcpServers":{"duduclaw":{"command":"/old","args":["mcp-server"],"env":{"DUDUCLAW_AGENT_TOKEN":"t"}}}}"#;
        let out = merge_mcp_config(Some(stale), &servers).unwrap().unwrap();
        assert!(!out.contains("DUDUCLAW_AGENT_TOKEN"), "{out}");
        assert!(merge_mcp_config(Some("nope"), &servers).is_err());
    }

    #[test]
    fn write_mcp_config_lands_in_dot_agents_and_skips_equal_writes() {
        let ws = tempfile::tempdir().unwrap();
        let servers = vec![(MCP_SERVER_NAME.to_string(), entry())];
        let home = tempfile::tempdir().unwrap();
        assert!(write_mcp_config(ws.path(), home.path(), &servers).unwrap());
        let path = ws.path().join(".agents").join("mcp_config.json");
        let first = std::fs::read_to_string(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(!write_mcp_config(ws.path(), home.path(), &servers).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        assert!(!first.contains("DUDUCLAW_AGENT"), "{first}");
    }

    // ── legacy file ──

    #[test]
    fn legacy_file_is_deleted_only_when_it_holds_nothing_but_mcp_servers() {
        assert!(legacy_settings_is_mcp_only(r#"{"mcpServers":{"duduclaw":{"command":"/x"}}}"#));
        assert!(!legacy_settings_is_mcp_only(r#"{"mcpServers":{}}"#));
        assert!(
            !legacy_settings_is_mcp_only(
                r#"{"mcpServers":{"duduclaw":{},"playwright":{}}}"#
            ),
            "an extra server keeps the file"
        );
        assert!(!legacy_settings_is_mcp_only(r#"{"mcpServers":{},"theme":"x"}"#));
        assert!(!legacy_settings_is_mcp_only(r#"{}"#));
        assert!(!legacy_settings_is_mcp_only("not json"));
        assert!(!legacy_settings_is_mcp_only(r#"["mcpServers"]"#));

        let agent = tempfile::tempdir().unwrap();
        assert!(!remove_legacy_agent_settings(agent.path()).unwrap(), "absent ⇒ no-op");
        let path = legacy_agent_settings_path(agent.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(&path, r#"{"mcpServers":{"duduclaw":{"env":{"DUDUCLAW_AGENT_TOKEN":"t"}}},"theme":"x"}"#).unwrap();
        assert!(!remove_legacy_agent_settings(agent.path()).unwrap());
        assert!(path.exists(), "mixed content stays");

        std::fs::write(&path, r#"{"mcpServers":{"duduclaw":{"env":{"DUDUCLAW_AGENT_TOKEN":"t"}}}}"#).unwrap();
        assert!(remove_legacy_agent_settings(agent.path()).unwrap());
        assert!(!path.exists());
    }

    // ── env ──

    #[test]
    fn spawn_env_carries_identity_and_home_never_the_dead_variable() {
        let home = tempfile::tempdir().unwrap();
        let env = identity_env_pairs(home.path(), "agent-x");
        assert!(
            env.iter()
                .any(|(k, v)| k == duduclaw_core::ENV_AGENT_ID && v == "agent-x"),
            "identity id missing"
        );
        let homes: Vec<_> = env.iter().filter(|(k, _)| k == "DUDUCLAW_HOME").collect();
        assert_eq!(homes.len(), 1, "exactly one DUDUCLAW_HOME");
        assert_eq!(homes[0].1, home.path().to_string_lossy());
        assert!(!env.iter().any(|(k, _)| k == "ANTIGRAVITY_API_KEY"));
        assert!(
            !env.iter().any(|(k, _)| k == GEMINI_KEY_ENV),
            "the key is set separately, only in api_key mode"
        );
    }

    // ── 2026-10: MCP tool grant (agy print mode soft-denies unapproved calls) ──

    fn allow_list(v: &Value) -> Vec<String> {
        v["permissions"]["allow"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn the_grants_name_only_the_duduclaw_server_and_its_schema_dir() {
        assert_eq!(MCP_TOOL_GRANT, "mcp(duduclaw/*)");
        assert_eq!(
            grants(),
            vec![
                "mcp(duduclaw/*)".to_string(),
                format!(
                    "read_file({})",
                    Path::new(FAKE_HOME)
                        .join(".gemini/antigravity-cli/mcp/duduclaw")
                        .to_string_lossy()
                ),
            ]
        );
        // Never a shell, write, URL or wildcard grant.
        for g in grants() {
            assert!(g.starts_with("mcp(duduclaw/") || g.starts_with("read_file("), "{g}");
            assert!(!g.contains("(*)") && !g.ends_with("(/)"), "{g}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_home_is_granted_under_both_spellings() {
        let real = tempfile::tempdir().unwrap();
        let links = tempfile::tempdir().unwrap();
        let home = links.path().join("home");
        std::os::unix::fs::symlink(real.path(), &home).unwrap();
        let g = tool_grants(&home);
        assert_eq!(g.len(), 3, "{g:?}");
        let canon = real.path().canonicalize().unwrap();
        assert!(g.iter().any(|r| r.contains(&*home.to_string_lossy())), "{g:?}");
        assert!(g.iter().any(|r| r.contains(&*canon.to_string_lossy())), "{g:?}");
    }

    #[test]
    fn a_trusted_workspace_gets_the_duduclaw_mcp_grant_in_every_auth_mode() {
        for auth in [AntigravityAuth::Unset, AntigravityAuth::Login, AntigravityAuth::ApiKey] {
            let out = merge_user_settings(Some(r#"{"theme":"dark"}"#), Some("/w"), auth)
                .unwrap()
                .unwrap();
            let v = parse(&out);
            assert_eq!(allow_list(&v), grants(), "{auth:?}");
            // Only `allow` is written: no deny/ask lists, no other grant kind.
            assert_eq!(v["permissions"].as_object().unwrap().len(), 1, "{auth:?}");
            assert_eq!(v["theme"], "dark");
            // Idempotent.
            assert_eq!(merge_user_settings(Some(&out), Some("/w"), auth).unwrap(), None);
        }
    }

    #[test]
    fn the_mcp_grant_is_appended_to_the_operators_own_rules() {
        let existing = r#"{"trustedWorkspaces":["/w"],"permissions":{"allow":["command(git status)"],"deny":["command(rm)"],"ask":["read_url(example.com)"]}}"#;
        let out = merge_user_settings(Some(existing), Some("/w"), AntigravityAuth::Unset)
            .unwrap()
            .unwrap();
        let v = parse(&out);
        let mut want = vec!["command(git status)".to_string()];
        want.extend(grants());
        assert_eq!(allow_list(&v), want);
        assert_eq!(v["permissions"]["deny"], serde_json::json!(["command(rm)"]));
        assert_eq!(v["permissions"]["ask"], serde_json::json!(["read_url(example.com)"]));
    }

    #[test]
    fn ensure_user_settings_grants_the_schema_dir_of_the_given_home() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        ensure_user_settings(home.path(), Some(ws.path()), AntigravityAuth::Unset).unwrap();
        let v = parse(&std::fs::read_to_string(user_settings_path(home.path())).unwrap());
        assert_eq!(allow_list(&v), tool_grants(home.path()));
        // Second call is a no-op.
        assert!(!ensure_user_settings(home.path(), Some(ws.path()), AntigravityAuth::Unset).unwrap().wrote);
    }

    #[test]
    fn no_workspace_means_no_grant() {
        // Nothing registers the MCP server without a working root.
        let out = merge_user_settings(None, None, AntigravityAuth::ApiKey).unwrap().unwrap();
        assert!(parse(&out).get("permissions").is_none());
    }

    #[test]
    fn malformed_permissions_are_left_alone_but_trust_and_provider_are_still_written() {
        for (bad, kept) in [
            (r#"{"permissions":"all"}"#, serde_json::json!("all")),
            (
                r#"{"permissions":{"allow":"mcp(duduclaw/*)"}}"#,
                serde_json::json!({"allow": "mcp(duduclaw/*)"}),
            ),
        ] {
            let m = super::merge_user_settings(Some(bad), Some("/w"), AntigravityAuth::ApiKey, &grants())
                .unwrap();
            let issue = m.grant_issue.expect("the skipped grant is reported");
            assert!(issue.contains("permissions"), "{issue}");
            let v = parse(&m.content.expect("trust/provider still written"));
            assert_eq!(v["trustedWorkspaces"], serde_json::json!(["/w"]));
            assert_eq!(v["modelProvider"], "gemini");
            assert_eq!(v["permissions"], kept, "the operator's value is not overwritten");
        }
    }

    #[test]
    fn ensure_user_settings_reports_a_skipped_grant() {
        let home = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let path = user_settings_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"permissions":"all"}"#).unwrap();
        let o = ensure_user_settings(home.path(), Some(ws.path()), AntigravityAuth::Unset).unwrap();
        assert!(o.wrote);
        assert!(o.grant_issue.is_some());
        let v = parse(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(v["permissions"], "all");
        assert!(v["trustedWorkspaces"].is_array());
    }

    #[test]
    fn a_home_with_rule_syntax_characters_gets_no_read_rule() {
        for home in ["/u(x", "/u)x", "/u,x", "/u*x", "/u\nx", "/u\rx"] {
            assert!(schema_read_grant(Path::new(home)).is_err(), "{home:?}");
            assert_eq!(tool_grants(Path::new(home)), vec![MCP_TOOL_GRANT.to_string()], "{home:?}");
        }
        assert!(schema_read_grant(Path::new("/home/a b/c-d.e_f")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_home_gets_no_read_rule() {
        use std::os::unix::ffi::OsStrExt;
        let home = Path::new(std::ffi::OsStr::from_bytes(b"/u/\xff"));
        assert!(schema_read_grant(home).unwrap_err().contains("UTF-8"));
        assert_eq!(tool_grants(home), vec![MCP_TOOL_GRANT.to_string()]);
    }

    // ── 2026-10 review hardening ──

    #[test]
    fn non_array_trusted_workspaces_is_refused() {
        let err = merge_user_settings(
            Some(r#"{"trustedWorkspaces":"/a"}"#),
            Some("/b"),
            AntigravityAuth::Unset,
        )
        .unwrap_err();
        assert!(err.contains("trustedWorkspaces"), "{err}");
        // Without a dir to add, the field is not touched at all.
        assert!(
            merge_user_settings(Some(r#"{"trustedWorkspaces":"/a"}"#), None, AntigravityAuth::Unset)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stale_api_key_route_predicate() {
        let gem = Some(r#"{"modelProvider":"gemini"}"#);
        assert!(stale_api_key_route(gem, false));
        assert!(!stale_api_key_route(gem, true), "a key is available");
        assert!(!stale_api_key_route(Some(r#"{"modelProvider":"vertex"}"#), false));
        assert!(!stale_api_key_route(Some("{}"), false));
        assert!(!stale_api_key_route(None, false));
        assert!(!stale_api_key_route(Some("broken"), false));
    }

    #[test]
    fn unset_with_nothing_to_trust_still_reports_a_leftover_gemini_provider() {
        let home = tempfile::tempdir().unwrap();
        let path = user_settings_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"modelProvider":"gemini"}"#).unwrap();
        let o = ensure_user_settings(home.path(), None, AntigravityAuth::Unset).unwrap();
        assert_eq!(
            o,
            SettingsOutcome {
                wrote: false,
                had_gemini_provider: true,
                grant_issue: None,
            }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"modelProvider":"gemini"}"#);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_user_settings_are_written_through_and_new_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let dotfiles = tempfile::tempdir().unwrap();
        let real = dotfiles.path().join("agy-settings.json");
        std::fs::write(&real, r#"{"theme":"dark"}"#).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = user_settings_path(home.path());
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(ensure_user_settings(home.path(), None, AntigravityAuth::ApiKey).unwrap().wrote);
        assert!(
            std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "the link must survive"
        );
        let v = parse(&std::fs::read_to_string(&real).unwrap());
        assert_eq!(v["modelProvider"], "gemini");
        assert_eq!(v["theme"], "dark");
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o644);

        let fresh = tempfile::tempdir().unwrap();
        ensure_user_settings(fresh.path(), None, AntigravityAuth::ApiKey).unwrap();
        let p = user_settings_path(fresh.path());
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dot_agents_is_refused_and_nothing_outside_changes() {
        let ws = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.path().join(".agents")).unwrap();
        let servers = vec![(MCP_SERVER_NAME.to_string(), entry())];
        assert!(write_mcp_config(ws.path(), home.path(), &servers).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_mcp_config_is_refused_and_its_target_is_untouched() {
        let ws = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.json");
        std::fs::write(&victim, r#"{"keep":true}"#).unwrap();
        std::fs::create_dir(ws.path().join(".agents")).unwrap();
        std::os::unix::fs::symlink(&victim, mcp_config_path(ws.path())).unwrap();
        let servers = vec![(MCP_SERVER_NAME.to_string(), entry())];
        assert!(write_mcp_config(ws.path(), home.path(), &servers).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), r#"{"keep":true}"#);
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
        // No lock sidecar or temp file appeared in the agent-writable tree.
        let names: Vec<_> = std::fs::read_dir(ws.path().join(".agents"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn the_mcp_lock_lives_under_the_duduclaw_home_not_the_workspace() {
        let ws = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let lock = mcp_lock_path(home.path(), ws.path());
        assert!(lock.starts_with(home.path()));
        assert!(!lock.starts_with(ws.path()));
        let servers = vec![(MCP_SERVER_NAME.to_string(), entry())];
        write_mcp_config(ws.path(), home.path(), &servers).unwrap();
        let names: Vec<_> = std::fs::read_dir(ws.path().join(".agents"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![MCP_CONFIG_FILE.to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_file_behind_a_symlink_is_kept() {
        let agent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let body = r#"{"mcpServers":{"duduclaw":{}}}"#;
        // Symlinked file.
        let path = legacy_agent_settings_path(agent.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let victim = outside.path().join("settings.json");
        std::fs::write(&victim, body).unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        assert!(remove_legacy_agent_settings(agent.path()).is_err());
        assert!(victim.exists() && path.exists());

        // Symlinked intermediate directory.
        let agent2 = tempfile::tempdir().unwrap();
        let real_cli = outside.path().join("cli");
        std::fs::create_dir(&real_cli).unwrap();
        std::fs::write(real_cli.join("settings.json"), body).unwrap();
        std::fs::create_dir(agent2.path().join(".gemini")).unwrap();
        std::os::unix::fs::symlink(&real_cli, agent2.path().join(".gemini").join("antigravity-cli"))
            .unwrap();
        assert!(remove_legacy_agent_settings(agent2.path()).is_err());
        assert!(real_cli.join("settings.json").exists());
    }

    #[test]
    fn redaction_removes_the_exact_key_and_google_shaped_keys() {
        let key = concat!("AI", "zaSyREALKEY-0123456789abcdefghijklmnop");
        let other = concat!("AI", "zaSyOTHER_0123456789abcdefghijklmnopq");
        let text = format!("bad key {key} and {other}; short AIza123 stays");
        let out = redact_key(&text, Some(key));
        assert!(!out.contains(key), "{out}");
        assert!(!out.contains(other), "{out}");
        assert!(out.contains("AIza123 stays"), "{out}");
        assert_eq!(out.matches("<redacted>").count(), 2, "{out}");
        // Non-Google-shaped exact key is still removed.
        let out = redact_key("leak: plain-secret-value", Some("plain-secret-value"));
        assert_eq!(out, "leak: <redacted>");
        assert_eq!(redact_key("nothing here", None), "nothing here");
        assert_eq!(redact_key("x", Some("  ")), "x");
    }
}
