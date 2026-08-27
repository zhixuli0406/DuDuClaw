// mcp_auth.rs — MCP API Key authentication module (W19-P0)
//
// Provides API key validation, principal extraction, and scope enforcement
// for the MCP server's authentication layer.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use subtle::ConstantTimeEq;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    MemoryRead,
    MemoryWrite,
    WikiRead,
    WikiWrite,
    MessagingSend,
    /// RFC-21 §1: gates `identity_resolve` and friends. Distinct from
    /// `WikiRead` because operators may want to grant agents read access to
    /// the shared wiki *without* exposing the canonical person registry.
    IdentityRead,
    /// RFC-21 §2: gates Odoo `search_read` / list / status — read-class
    /// `odoo_*` MCP tools that don't mutate Odoo state.
    OdooRead,
    /// RFC-21 §2: gates Odoo `create` / `write` — mutating `odoo_*` tools
    /// that change record state but don't fire workflows.
    OdooWrite,
    /// RFC-21 §2: gates Odoo `execute_kw` workflow buttons (e.g.
    /// `action_confirm`) and the generic `odoo_execute` / `odoo_report`
    /// surfaces, which can fire side-effects beyond simple writes.
    OdooExecute,
    /// Google Workspace: gates the read-class native tools (`google_status`,
    /// `gmail_search`, `gmail_read`, `calendar_list_events`). Distinct scope so
    /// operators can grant Gmail/Calendar read without full `Admin`.
    GoogleRead,
    /// Google Workspace: gates the write-class native tools
    /// (`gmail_create_draft` — draft only, never sends; `calendar_create_event`
    /// — creates a real, externally-visible event; `sheets_append`).
    GoogleWrite,
    /// Notion: gates the read-class native tools (`notion_status`,
    /// `notion_search`, `notion_page_read`). Notion content is an external
    /// knowledge source surfaced for query/citation only.
    NotionRead,
    /// Notion: gates the write-class native tool (`notion_page_append` — appends
    /// paragraph blocks to an existing page; never deletes/overwrites).
    NotionWrite,
    /// GitHub: gates the read-class native tools (`github_status`,
    /// `github_search_issues`, `github_issue_read`, `github_pr_read`).
    GithubRead,
    /// GitHub: gates the write-class native tool (`github_issue_comment` — posts
    /// a publicly visible comment).
    GithubWrite,
    /// RFC-26: gates the Live Run Forking tools (`fork_run`, `inspect_branches`,
    /// `diff_branches`, `merge_or_select`, `terminate_branch`, `fork_cost`).
    /// Distinct from `Admin` so operators can grant an agent the ability to fork
    /// its own runs without granting full superuser scope.
    ForkExecute,
    /// OS-native Phase 1: gates the `os_notify` / `os_watch_status` / `os_open`
    /// MCP tools. Distinct scope so operators can grant OS integration without
    /// granting `Admin`; enforcement additionally requires the per-agent
    /// `[capabilities] os_native` flag at the dispatch gate (defence-in-depth).
    OsNative,
    /// Gates the `office_script` MCP tool — server-side execution of a bundled
    /// office skill's vetted `scripts/*.py` (docx/xlsx/pptx/pdf) so API-mode
    /// agents that have no Bash tool can still produce document files.
    /// Deliberately narrower than `Admin` (which the code-execution
    /// `execute_program` requires): constrained to the four built-in skills and
    /// the caller's own agent directory, so operators can grant document
    /// production without granting superuser.
    SkillExecute,
    /// WP3.3 recording-to-skill: gates the `browser_record_start` /
    /// `browser_record_stop` / `desktop_record_start` / `desktop_record_stop` /
    /// `skill_from_recording` MCP tools. Distinct scope so operators can grant
    /// recording without Admin; the dispatch gate ADDITIONALLY requires the
    /// per-agent `[capabilities] recording = true` flag (defence-in-depth,
    /// deny-by-default).
    Recording,
    /// Agent Mail (P2-d): gates `mail_list` / `mail_read`. Split from
    /// [`Scope::MailSend`] so an operator can let an agent *see* the mailbox
    /// without granting it the ability to queue outbound correspondence.
    MailRead,
    /// Agent Mail (P2-d): gates `mail_send`. Named as a send scope even though
    /// the tool cannot transmit — what it grants is the ability to put a draft
    /// in front of a human, which is the step worth authorising separately.
    MailSend,
    Admin,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Scope::MemoryRead => "memory:read",
            Scope::MemoryWrite => "memory:write",
            Scope::WikiRead => "wiki:read",
            Scope::WikiWrite => "wiki:write",
            Scope::MessagingSend => "messaging:send",
            Scope::IdentityRead => "identity:read",
            Scope::OdooRead => "odoo:read",
            Scope::OdooWrite => "odoo:write",
            Scope::OdooExecute => "odoo:execute",
            Scope::GoogleRead => "google:read",
            Scope::GoogleWrite => "google:write",
            Scope::NotionRead => "notion:read",
            Scope::NotionWrite => "notion:write",
            Scope::GithubRead => "github:read",
            Scope::GithubWrite => "github:write",
            Scope::ForkExecute => "fork:execute",
            Scope::OsNative => "os:native",
            Scope::SkillExecute => "skill:execute",
            Scope::Recording => "recording",
            Scope::MailRead => "mail:read",
            Scope::MailSend => "mail:send",
            Scope::Admin => "admin",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone)]
pub struct Principal {
    pub client_id: String,
    pub scopes: HashSet<Scope>,
    pub is_external: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, PartialEq)]
pub enum AuthError {
    MissingKey,
    InvalidFormat,
    UnknownKey,
    KeyExpired { days_old: u64 },
    InvalidScope(String),
    /// Gap (a), WP-H2 §1.3: a per-call re-authentication attempt needed to
    /// reload the on-disk key registry (its mtime had changed) but the
    /// reload itself failed — an I/O error other than "file does not exist",
    /// or malformed TOML. Fail-closed: this is intentionally a HARD deny,
    /// never a silent fall-back to whatever was cached before.
    ReloadFailed,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingKey => write!(f, "DUDUCLAW_MCP_API_KEY environment variable not set"),
            AuthError::InvalidFormat => write!(f, "API key has invalid format"),
            AuthError::UnknownKey => write!(f, "API key not found in registry"),
            AuthError::KeyExpired { days_old } => {
                write!(f, "API key expired ({days_old} days old, max 30)")
            }
            AuthError::InvalidScope(s) => write!(f, "Unknown scope: {s}"),
            AuthError::ReloadFailed => write!(
                f,
                "MCP key registry reload failed (I/O or parse error) — denying (fail-closed)"
            ),
        }
    }
}

// ── Key format validation ────────────────────────────────────────────────────

/// Validate: ^ddc_(prod|staging|dev)_[a-f0-9]{32}$
fn is_valid_key_format(key: &str) -> bool {
    let re = regex::Regex::new(r"^ddc_(prod|staging|dev)_[a-f0-9]{32}$").unwrap();
    re.is_match(key)
}

// ── Config parsing ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct KeyEntry {
    client_id: String,
    scopes: HashSet<Scope>,
    is_external: bool,
    created_at: DateTime<Utc>,
}

/// Outcome of attempting to (re)parse the on-disk `[mcp_keys]` registry.
///
/// Distinguishing "loaded, possibly empty" from "failed to load" is what lets
/// [`KeyRegistryCache`] implement the fail-closed reload contract (Gap (a),
/// WP-H2 §1.3): a config.toml that never existed (or has no `[mcp_keys]`
/// table) is a legitimate empty-registry state, but a config.toml that
/// EXISTS and cannot be read/parsed is a genuine failure — a previously-good
/// cache must never be reused past that point.
enum LoadOutcome {
    /// Parsed successfully. An empty map is legitimate (no `[mcp_keys]`
    /// configured, or the file does not exist yet) — not a failure.
    Loaded(HashMap<String, KeyEntry>),
    /// The file exists but could not be read or parsed (an I/O error other
    /// than "not found", or malformed TOML).
    Failed,
}

/// Parse `[mcp_keys]` from `<config_dir>/config.toml`, distinguishing a
/// genuine reload failure from "nothing configured". See [`LoadOutcome`].
fn load_key_registry_checked(config_dir: &Path) -> LoadOutcome {
    let config_path = config_dir.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return LoadOutcome::Loaded(HashMap::new());
        }
        Err(_) => return LoadOutcome::Failed,
    };

    let doc: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => return LoadOutcome::Failed,
    };

    let mut registry = HashMap::new();

    let mcp_keys = match doc.get("mcp_keys").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return LoadOutcome::Loaded(registry),
    };

    for (key, val) in mcp_keys {
        let tbl = match val.as_table() {
            Some(t) => t,
            None => continue,
        };

        let client_id = match tbl.get("client_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let is_external = tbl
            .get("is_external")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let created_at_str = match tbl.get("created_at").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };

        let created_at = match DateTime::parse_from_rfc3339(created_at_str) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => continue,
        };

        let scopes_raw = tbl
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();

        let scopes = parse_scopes(&scopes_raw).unwrap_or_default();

        registry.insert(
            key.clone(),
            KeyEntry {
                client_id,
                scopes,
                is_external,
                created_at,
            },
        );
    }

    LoadOutcome::Loaded(registry)
}

/// Load mcp_keys from ~/.duduclaw/config.toml.
///
/// Backward-compatible wrapper over [`load_key_registry_checked`]: any load
/// failure degrades to an empty registry, matching this function's original
/// (pre-cache) behavior — an unreadable/malformed file must not crash the
/// boot path, it just means "no keys usable right now" (which itself
/// composes into a fail-closed `UnknownKey` the moment a caller looks up a
/// real key against the empty map). [`KeyRegistryCache`] deliberately does
/// NOT go through this wrapper — it needs the `Failed` distinction to refuse
/// reusing a stale good cache after a broken reload.
fn load_key_registry(config_dir: &Path) -> HashMap<String, KeyEntry> {
    match load_key_registry_checked(config_dir) {
        LoadOutcome::Loaded(registry) => registry,
        LoadOutcome::Failed => HashMap::new(),
    }
}

// ── Per-call re-authentication cache (Gap (a), WP-H2 §1.3) ──────────────────
//
// The MCP stdio server previously resolved `DUDUCLAW_MCP_API_KEY` against the
// on-disk registry exactly ONCE at process startup (`mcp.rs::run_mcp_server`)
// and reused that `Principal` for the lifetime of the long-running
// subprocess — rotating scopes or revoking a key in `config.toml [mcp_keys]`
// had no observable effect until the child was restarted. `KeyRegistryCache`
// + [`authenticate_from_env_cached`] let every dispatch re-validate while
// keeping the hot path cheap: `config.toml` is only re-parsed when its mtime
// has changed since the last check (one `fs::metadata` stat otherwise).

enum CacheState {
    Empty,
    Loaded {
        mtime: Option<SystemTime>,
        registry: HashMap<String, KeyEntry>,
    },
}

/// mtime-aware cache over the `[mcp_keys]` registry. One instance is created
/// per long-lived MCP server process and shared across every dispatch.
pub struct KeyRegistryCache {
    inner: Mutex<CacheState>,
}

impl Default for KeyRegistryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyRegistryCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CacheState::Empty),
        }
    }

    /// Return the current registry. Reparses `config.toml` only when its
    /// mtime differs from the last successful load (or there is no cache
    /// yet).
    ///
    /// Fail-closed: a reload that hits [`LoadOutcome::Failed`] clears the
    /// cache and returns `Err(())` instead of returning the previously
    /// cached (and now possibly stale) registry — the caller must deny the
    /// in-flight auth attempt, never fall back to "whatever used to work".
    fn registry(&self, config_dir: &Path) -> Result<HashMap<String, KeyEntry>, ()> {
        let config_path = config_dir.join("config.toml");
        let current_mtime = std::fs::metadata(&config_path)
            .and_then(|m| m.modified())
            .ok();

        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let CacheState::Loaded { mtime, registry } = &*guard {
            if let (Some(cur), Some(cached)) = (current_mtime, mtime) {
                if cur == *cached {
                    return Ok(registry.clone());
                }
            }
            // Either the mtime moved, or the file that used to exist can no
            // longer be stat'd (e.g. deleted) — both are changes; fall
            // through and reload from scratch rather than trusting `guard`.
        }

        match load_key_registry_checked(config_dir) {
            LoadOutcome::Loaded(fresh) => {
                *guard = CacheState::Loaded {
                    mtime: current_mtime,
                    registry: fresh.clone(),
                };
                Ok(fresh)
            }
            LoadOutcome::Failed => {
                *guard = CacheState::Empty;
                Err(())
            }
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Authenticate a pre-validated raw API key against the key registry.
///
/// This is the **core** authentication function.  It does not touch environment
/// variables — callers must supply the key directly.
///
/// Used by:
/// - [`authenticate_from_env`] (reads key from `DUDUCLAW_MCP_API_KEY`)
/// - [`crate::mcp_auth_strategy::ApiKeyAuthStrategy`] when a credential is
///   injected via [`crate::mcp_auth_strategy::AuthContext::credential`]
pub fn authenticate_with_key(raw_key: &str, config_dir: &Path) -> Result<Principal, AuthError> {
    let registry = load_key_registry(config_dir);
    authenticate_against_registry(raw_key, &registry)
}

/// Core lookup shared by [`authenticate_with_key`] (fresh parse every call)
/// and [`authenticate_from_env_cached`] (mtime-cached parse) — the constant-
/// time comparison and expiry check are identical either way; only how the
/// `registry` argument was obtained differs.
fn authenticate_against_registry(
    raw_key: &str,
    registry: &HashMap<String, KeyEntry>,
) -> Result<Principal, AuthError> {
    if !is_valid_key_format(raw_key) {
        return Err(AuthError::InvalidFormat);
    }

    // Constant-time key lookup: iterate ALL entries so the number of iterations
    // does not leak whether a key prefix matches.  Within each comparison,
    // subtle::ConstantTimeEq prevents early-exit on the first differing byte.
    let entry = {
        let raw_bytes = raw_key.as_bytes();
        let mut found: Option<&KeyEntry> = None;
        for (stored_key, entry) in registry {
            let stored_bytes = stored_key.as_bytes();
            // Lengths must match; pad to avoid length-based side-channel.
            // Both sides are the same fixed-length format (validated above), so
            // this is a belt-and-suspenders guard.
            let len_match = stored_bytes.len() == raw_bytes.len();
            // Run the byte-wise constant-time comparison regardless of length
            // to avoid timing differences on key-not-found vs key-found paths.
            let bytes_match = if len_match {
                stored_bytes.ct_eq(raw_bytes).into()
            } else {
                // Different lengths can never match; still do a dummy comparison
                // on a zero-length slice so the branch executes the same code
                // path in every iteration.
                let _ = b"".ct_eq(b"");
                false
            };
            if bytes_match {
                found = Some(entry);
            }
        }
        found.ok_or(AuthError::UnknownKey)?
    };

    // Expiry check: key must not be older than 30 days.
    // L12: a future-dated `created_at` yields a negative duration; clamp to 0
    // so the `as u64` cast can't wrap into an absurd "age" and falsely expire it.
    let age = Utc::now().signed_duration_since(entry.created_at);
    let days_old = age.num_days().max(0) as u64;
    if days_old > 30 {
        return Err(AuthError::KeyExpired { days_old });
    }

    Ok(Principal {
        client_id: entry.client_id.clone(),
        scopes: entry.scopes.clone(),
        is_external: entry.is_external,
        created_at: entry.created_at,
    })
}

/// Authenticate from DUDUCLAW_MCP_API_KEY env var.
///
/// Accepts two credential formats and dispatches to the appropriate validator:
///
/// 1. **Refresh tokens** (v1.16.0+) — format `ddc_refresh_<env>_<64hex>`.
///    Validated against the SQLite-backed token store, 90-day lifetime,
///    individually revocable. See [`crate::mcp_refresh`].
///
/// 2. **Legacy API keys** — format `ddc_<env>_<32hex>`. Validated against
///    `[mcp_keys]` in `config.toml`, 30-day rotation policy.
///
/// Backwards-compatible: if the env var is absent AND no `[mcp_keys]` is
/// configured AND no refresh tokens exist, returns a default internal
/// Principal with all scopes so existing internal tooling keeps working
/// unchanged.
///
/// For programmatic key injection (e.g. tests, HTTP transport), use
/// [`authenticate_with_key`] directly.
pub fn authenticate_from_env(config_dir: &Path) -> Result<Principal, AuthError> {
    let registry = load_key_registry(config_dir);

    let raw_key = match std::env::var("DUDUCLAW_MCP_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            // M6: fail-closed. Previously an unconfigured peer (no
            // DUDUCLAW_MCP_API_KEY *and* no [mcp_keys]) was silently granted an
            // all-scopes Admin principal. That fails open: any stdio/external
            // caller would inherit Admin. Now the unauthenticated default
            // requires an *explicit* operator opt-in so it can never be granted
            // by accident.
            if registry.is_empty() && allow_unauthenticated_default() {
                tracing::warn!(
                    "MCP server starting without API key authentication \
                     (DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED=1, no DUDUCLAW_MCP_API_KEY and no \
                     [mcp_keys] in config.toml). All scopes granted to default internal \
                     principal. This is only safe for trusted local usage."
                );
                return Ok(default_internal_principal());
            }
            return Err(AuthError::MissingKey);
        }
    };

    // Prefix-based dispatch: refresh tokens carry the explicit `ddc_refresh_`
    // marker so the validator can tell which storage backend to query without
    // attempting one then the other (and leaking which backend held the key
    // via timing). Legacy API keys keep the original `ddc_<env>_<32hex>` path.
    if raw_key.starts_with(crate::mcp_refresh::REFRESH_TOKEN_PREFIX) {
        return crate::mcp_refresh::authenticate_with_refresh_token(&raw_key, config_dir);
    }

    authenticate_with_key(&raw_key, config_dir)
}

/// Per-call variant of [`authenticate_from_env`] backed by a
/// [`KeyRegistryCache`] (Gap (a), WP-H2 §1.3): a caller that re-authenticates
/// on every MCP request — instead of once at process boot — pays only an
/// `fs::metadata` stat when `config.toml` hasn't changed. Behaviorally
/// identical to `authenticate_from_env` on the happy path; the moment an
/// operator edits `[mcp_keys]` (rotate scopes, revoke, add a key), the very
/// next call observes it — no gateway/child restart required.
///
/// Refresh tokens are NOT routed through the cache:
/// [`crate::mcp_refresh::authenticate_with_refresh_token`] already re-queries
/// the SQLite token store on every call (real-time revocation by
/// construction — see that module's doc comment), so wrapping it here would
/// only add complexity without a performance win.
///
/// Fail-closed on a broken reload: if the registry needed to be reparsed
/// (mtime changed) and that reparse fails, this returns
/// [`AuthError::ReloadFailed`] rather than reusing a previously-cached
/// principal — see [`KeyRegistryCache::registry`].
pub fn authenticate_from_env_cached(
    config_dir: &Path,
    cache: &KeyRegistryCache,
) -> Result<Principal, AuthError> {
    let raw_key = match std::env::var("DUDUCLAW_MCP_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            // Mirrors `authenticate_from_env`'s unauthenticated-default
            // fallback (M6). A reload failure here degrades to "treat the
            // registry as non-empty" (`unwrap_or(false)`) so a broken
            // config.toml can never accidentally unlock the all-scopes
            // default principal — fail-closed bias, consistent with the rest
            // of this function.
            let registry_empty = cache.registry(config_dir).map(|r| r.is_empty()).unwrap_or(false);
            if registry_empty && allow_unauthenticated_default() {
                tracing::warn!(
                    "MCP server starting without API key authentication \
                     (DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED=1, no DUDUCLAW_MCP_API_KEY and no \
                     [mcp_keys] in config.toml). All scopes granted to default internal \
                     principal. This is only safe for trusted local usage."
                );
                return Ok(default_internal_principal());
            }
            return Err(AuthError::MissingKey);
        }
    };

    // Same prefix-based dispatch as `authenticate_from_env`.
    if raw_key.starts_with(crate::mcp_refresh::REFRESH_TOKEN_PREFIX) {
        return crate::mcp_refresh::authenticate_with_refresh_token(&raw_key, config_dir);
    }

    let registry = cache.registry(config_dir).map_err(|()| {
        tracing::warn!(
            "MCP key registry reload failed (I/O or parse error on config.toml) — \
             denying this call (fail-closed); a stale cached principal is never reused"
        );
        AuthError::ReloadFailed
    })?;

    authenticate_against_registry(&raw_key, &registry)
}

/// M6: whether the operator has explicitly opted into running the MCP server
/// without any authentication (granting the default Admin principal). Defaults
/// to `false` (deny). Set `DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED=1` to enable for
/// trusted local-only usage.
fn allow_unauthenticated_default() -> bool {
    matches!(
        std::env::var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Build a default all-scopes internal principal for backwards-compatible
/// scenarios where no API key is configured.
fn default_internal_principal() -> Principal {
    let all_scopes = [
        Scope::MemoryRead,
        Scope::MemoryWrite,
        Scope::WikiRead,
        Scope::WikiWrite,
        Scope::MessagingSend,
        Scope::Admin,
    ]
    .into_iter()
    .collect();

    Principal {
        client_id: "default".to_string(),
        scopes: all_scopes,
        is_external: false,
        created_at: Utc::now(),
    }
}

/// Map a single canonical scope wire string to its `Scope` variant. The
/// reverse of `Scope`'s `Display` impl above — kept immediately next to
/// `parse_scopes` so the two stay easy to eyeball together, and locked
/// bidirectionally against `duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS` (the
/// shared canonical list the gateway also reads) by the
/// `scope_enum_matches_canonical_list` test below.
fn scope_from_str(s: &str) -> Option<Scope> {
    Some(match s {
        "memory:read" => Scope::MemoryRead,
        "memory:write" => Scope::MemoryWrite,
        "wiki:read" => Scope::WikiRead,
        "wiki:write" => Scope::WikiWrite,
        "messaging:send" => Scope::MessagingSend,
        "identity:read" => Scope::IdentityRead,
        "odoo:read" => Scope::OdooRead,
        "odoo:write" => Scope::OdooWrite,
        "odoo:execute" => Scope::OdooExecute,
        "google:read" => Scope::GoogleRead,
        "google:write" => Scope::GoogleWrite,
        "notion:read" => Scope::NotionRead,
        "notion:write" => Scope::NotionWrite,
        "github:read" => Scope::GithubRead,
        "github:write" => Scope::GithubWrite,
        "fork:execute" => Scope::ForkExecute,
        "os:native" => Scope::OsNative,
        "skill:execute" => Scope::SkillExecute,
        "mail:read" => Scope::MailRead,
        "mail:send" => Scope::MailSend,
        "recording" => Scope::Recording,
        "admin" => Scope::Admin,
        _ => return None,
    })
}

/// Parse a comma-separated scope string into a HashSet<Scope>.
/// e.g. "memory:read,wiki:write" → {MemoryRead, WikiWrite}
pub fn parse_scopes(s: &str) -> Result<HashSet<Scope>, AuthError> {
    if s.trim().is_empty() {
        return Ok(HashSet::new());
    }

    let mut result = HashSet::new();
    for part in s.split(',') {
        let part = part.trim();
        match scope_from_str(part) {
            Some(scope) => {
                result.insert(scope);
            }
            None => return Err(AuthError::InvalidScope(part.to_string())),
        }
    }
    Ok(result)
}

/// Return the minimum Scope required to call this tool.
///
/// C2 (2026-06 deep review): this table is **fail-closed**. Every tool not
/// explicitly mapped to a narrower scope falls through to `Some(Scope::Admin)`,
/// so a deliberately narrow-scoped key (e.g. `memory:read`) can never reach an
/// unenumerated high-impact tool (`execute_program`, `agent_update_soul`, …).
/// The default in-process agent uses `default_internal_principal`, which holds
/// `Scope::Admin` (a superuser in the dispatcher check), so normal operation is
/// unaffected. When adding a new tool, map it to the least scope it needs here;
/// leaving it unmapped means it requires Admin.

/// C4 (ecosystem WP3.2): scopes an OPERATOR may grant to external clients at
/// key issuance. Curated and conservative — credential-adjacent connector
/// scopes (Odoo/Google/Notion/Github), execution-class scopes (Fork/OsNative/
/// SkillExecute/Recording), the person registry (IdentityRead) and Admin are
/// deliberately absent: external surfaces never reach them regardless of what
/// a key claims.
pub const EXTERNALLY_GRANTABLE_SCOPES: &[Scope] = &[
    Scope::MemoryRead,
    Scope::MemoryWrite,
    Scope::WikiRead,
    Scope::WikiWrite,
    Scope::MessagingSend,
];

/// C4: may this EXTERNAL principal call `tool_name`?
///
/// Replaces the binary 7-tool whitelist with a scope-driven policy whose
/// zero-config default is byte-identical to the old behavior:
///   1. legacy whitelist tools → allowed (baseline unchanged), else
///   2. the tool must HAVE a scope-table entry (unscoped ⇒ Admin-class ⇒
///      never external), and
///   3. that scope must be externally grantable ([`EXTERNALLY_GRANTABLE_SCOPES`]), and
///   4. the key must carry that scope EXPLICITLY — Admin does not substitute
///      here (an external Admin key still only widens within the grantable set).
/// Callable ⇔ discoverable: `tools/list` filters with this same predicate.
pub fn external_tool_allowed(tool_name: &str, principal: &Principal) -> bool {
    if crate::mcp::EXTERNAL_TOOLS_WHITELIST.contains(&tool_name) {
        return true;
    }
    let Some(required) = tool_requires_scope(tool_name) else {
        return false;
    };
    EXTERNALLY_GRANTABLE_SCOPES.contains(&required) && principal.scopes.contains(&required)
}

pub fn tool_requires_scope(tool_name: &str) -> Option<Scope> {
    match tool_name {
        // ── Memory: read family ──────────────────────────────────────────
        "memory_search"
        | "memory_read"
        | "memory_fetch_batch"
        // D1 bi-temporal read APIs — same read tier as the rest of the family.
        | "memory_get_history"
        | "memory_get_at"
        // D3.2 entity-alias listing — read tier.
        | "memory_alias_list"
        | "memory_search_by_layer"
        | "memory_successful_conversations"
        | "memory_consolidation_status"
        | "memory_improve"
        | "memory_episodic_pressure"
        | "user_profile_get"
        | "user_code_profile"
        // Cross-wake working state — read tier.
        | "working_state_get"
        | "code_map" => Some(Scope::MemoryRead),
        // D3.2 entity-alias mutation — write tier.
        "memory_store" | "memory_alias_add" | "user_profile_record" => Some(Scope::MemoryWrite),
        // Cross-wake working state mutation (D3 ghost-memory fix) — the
        // agent's own authoritative posture, same trust tier as memory_store.
        "working_state_set" | "working_state_clear" | "working_state_handoff" => {
            Some(Scope::MemoryWrite)
        }
        // ── Wiki: read family ────────────────────────────────────────────
        "wiki_read"
        | "wiki_search"
        | "wiki_ls"
        | "wiki_stats"
        | "wiki_export"
        | "wiki_graph"
        | "wiki_lint"
        | "wiki_namespace_status"
        | "shared_wiki_read"
        | "shared_wiki_search"
        | "shared_wiki_ls"
        | "shared_wiki_stats"
        | "shared_wiki_lint" => Some(Scope::WikiRead),
        // ── Wiki: write family (incl. destructive shared_wiki_delete) ─────
        "wiki_write"
        | "wiki_share"
        | "wiki_dedup"
        | "wiki_rebuild_fts"
        | "shared_wiki_write"
        | "shared_wiki_delete" => Some(Scope::WikiWrite),
        // ── G15 Live Canvas ──────────────────────────────────────────────
        // Agent-authored presentation content pushed to the dashboard — same
        // trust tier as shared_wiki_write (agent-visible content mutation;
        // server-side ammonia-sanitized at write, sandbox-iframed at render).
        // No MCP read tool exists: viewing goes through the dashboard
        // `canvas.get` RPC only.
        "canvas_push" | "canvas_clear" => Some(Scope::WikiWrite),
        // ── Messaging / media egress ─────────────────────────────────────
        "send_message" | "send_photo" | "send_sticker" | "synthesize_speech"
        | "transcribe_audio" => Some(Scope::MessagingSend),
        // ── Agent Mail (P2-d) ────────────────────────────────────────────
        // Read and draft-send are separate grants. `mail_send` cannot
        // transmit (a human decision in `mail_worker::settle_outbox` does),
        // but queuing correspondence in front of a person is still an egress
        // -shaped act, so it gets its own scope rather than riding on
        // `MailRead`.
        "mail_list" | "mail_read" => Some(Scope::MailRead),
        "mail_send" => Some(Scope::MailSend),
        // RFC-21 §1: identity resolution requires its own scope so operators
        // can grant wiki access without exposing the person registry.
        "identity_resolve" => Some(Scope::IdentityRead),
        // RFC-21 §2: Odoo tool surface — three-tier scope split so an agent
        // granted only `odoo:read` cannot accidentally (or via prompt
        // injection) call mutating tools. These checks are defence-in-depth
        // *in addition to* the per-agent connector pool's `allowed_actions`
        // filter — both must pass.
        //
        // Read class: pure search_read / list / status.
        "odoo_status"
        | "odoo_crm_leads"
        | "odoo_sale_orders"
        | "odoo_inventory_products"
        | "odoo_inventory_check"
        | "odoo_invoice_list"
        | "odoo_payment_status"
        | "odoo_partner_search"
        | "odoo_schema_fields"
        | "odoo_search" => Some(Scope::OdooRead),
        // Connect is read-class — it acquires/refreshes the connection but
        // doesn't mutate Odoo state. Without it, no read can happen either.
        "odoo_connect" => Some(Scope::OdooRead),
        // Write class: create / write that mutate records but don't fire
        // workflow side-effects.
        "odoo_crm_create_lead"
        | "odoo_crm_update_stage"
        | "odoo_sale_create_quotation" => Some(Scope::OdooWrite),
        // Execute class: workflow buttons + generic execute_kw + report
        // generation. These can fire arbitrary Odoo-side actions.
        "odoo_sale_confirm" | "odoo_execute" | "odoo_report" => Some(Scope::OdooExecute),
        // Google Workspace native tools. Read class: connection diagnostics,
        // mail search/read, calendar listing, spreadsheet read — no external
        // side-effects.
        // Forms (structure + responses) and Google Tasks listing are read-only
        // too. `gtasks_*` is Google Tasks — distinct from DuDuClaw's own
        // `tasks_*` task-board tools.
        "google_status" | "gmail_search" | "gmail_read" | "calendar_list_events"
        | "sheets_read" | "forms_get" | "forms_list_responses" | "gtasks_lists"
        | "gtasks_list" | "drive_search" | "drive_read" | "docs_read" | "slides_read" => {
            Some(Scope::GoogleRead)
        }
        // Write class: draft creation (never sends) + real calendar-event
        // creation + spreadsheet row append + Google Tasks create/complete.
        // Defence-in-depth beyond any per-agent approval_required_tools gate
        // the operator adds.
        "gmail_create_draft" | "calendar_create_event" | "sheets_append" | "gtasks_create"
        | "gtasks_complete" | "docs_append" => Some(Scope::GoogleWrite),
        // Notion native tools. Read class: connection diagnostics, search, and
        // page read. Write class: append paragraph blocks to an existing page.
        "notion_status" | "notion_search" | "notion_page_read" => Some(Scope::NotionRead),
        "notion_page_append" => Some(Scope::NotionWrite),
        // GitHub native tools. Read class: diagnostics, issue/PR search and
        // read. Write class: post a publicly visible issue/PR comment.
        "github_status" | "github_search_issues" | "github_issue_read" | "github_pr_read" => {
            Some(Scope::GithubRead)
        }
        "github_issue_comment" => Some(Scope::GithubWrite),
        // W19-P1 M4: Audit Trail 查詢 API — admin-only，與 WebSocket 路徑
        // `require_admin!()` 保持對等訪問控制。
        "audit_trail_query" => Some(Scope::Admin),
        // W20-P0: Reliability Dashboard — admin-only，敏感指標資料。
        "reliability_summary" => Some(Scope::Admin),
        // R4 review: WebSocket dashboard requires manager+ for these via
        // `require_manager!()`; mirror as Admin scope at the MCP boundary
        // since MCP scopes lack a Manager tier. `wiki_trust_audit` exposes
        // page-level trust trends; `wiki_trust_history` exposes
        // `conversation_id` correlatable with user activity.
        "wiki_trust_audit" | "wiki_trust_history" => Some(Scope::Admin),
        // RFC-26: Live Run Forking surface. Gated by its own `fork:execute`
        // scope (defence-in-depth in addition to the per-agent `[fork] enabled`
        // toggle, which is checked at handler entry).
        "fork_run"
        | "inspect_branches"
        | "diff_branches"
        | "merge_or_select"
        | "terminate_branch"
        | "fork_cost" => Some(Scope::ForkExecute),
        // OS-native Phase 1: native notification, watch-status read, and open.
        // Gated by their own scope so OS integration can be granted without
        // Admin; the dispatch gate ALSO requires `[capabilities] os_native`.
        "os_notify" | "os_watch_status" | "os_open" => Some(Scope::OsNative),
        // OS-native P2-4: structured sensing sources (frontmost app/window,
        // Spotlight search, today's calendar events). Read-only — same scope
        // as the P1 tools, gated by [capabilities] os_native at the dispatch
        // gate; no ActionGuard (they have no host side-effect).
        "os_frontmost" | "os_spotlight_search" | "os_calendar_today" => Some(Scope::OsNative),
        // O-0: system-operator tool face bridging the dashboard-only
        // `device.*`/`system.*` RPCs to agents. Explicitly Admin-scoped
        // (matches the unmapped-tool fail-closed default byte-for-byte,
        // mapped here for clarity/lockability) — these operate the
        // physical/production machine, a strictly higher trust tier than
        // `os:native`'s host-automation tools. External clients can never
        // reach Admin (not in `EXTERNALLY_GRANTABLE_SCOPES`), so this
        // surface is internal-agent only. O-4 additionally requires the
        // agent's own explicit `agent.toml [capabilities] system_operator =
        // true` at the dispatch gate (`mcp_dispatch.rs`'s
        // `SYSTEM_OPERATOR_TOOLS` check) — Admin scope alone is no longer
        // sufficient, closing the "any internal agent could try these"
        // residual risk. Per-agent access is further scoped by `agent.toml
        // [capabilities] allowed_tools`/`denied_tools`.
        "os_device_status"
        | "os_system_status"
        | "os_check_update"
        | "os_backup_list"
        | "os_network_info"
        | "os_wifi_status"
        | "os_wifi_scan"
        | "os_wifi_connect"
        | "os_apply_update"
        | "os_boot_assessment"
        | "os_update_rollback"
        | "os_backup_create"
        | "os_power"
        | "os_factory_reset"
        | "os_doctor_repair"
        | "os_display_get"
        | "os_display_set"
        // Y10-1: agent→audio bridge (wpctl volume/mute/output), same tier
        // as os_display_get/set — see `duduclaw_gateway::audio_bridge`'s
        // module doc for why this never touches duduclaw-comp.
        | "os_audio_get"
        | "os_audio_set" => Some(Scope::Admin),
        // Server-side office-document script execution (docx/xlsx/pptx/pdf).
        // Its own least-privilege scope instead of the Admin `execute_program`
        // uses: the tool is constrained to the four bundled skills' vetted
        // scripts and the caller's agent directory.
        "office_script" => Some(Scope::SkillExecute),
        // WP3.3 recording-to-skill capture + distillation. Own scope so the
        // capability can be granted without Admin; the dispatch gate ALSO
        // requires `[capabilities] recording = true` (deny-by-default).
        "browser_record_start"
        | "browser_record_stop"
        | "desktop_record_start"
        | "desktop_record_stop"
        | "skill_from_recording" => Some(Scope::Recording),
        // CD-1 human-machine co-drive: GUI mouse/keyboard injection into a
        // shared desktop via `duduclaw-comp`. Explicitly Admin — the
        // highest internal trust tier, never externally grantable — same
        // tier as the other high-blast-radius tools below; enumerated on
        // its own line (not the Admin fall-through) so a future scope
        // split for co-drive is a one-line diff, not a silent behavior
        // change. The dispatch gate ADDITIONALLY requires the agent's own
        // `[capabilities] codrive = true` (deny-by-default, defence in
        // depth — see `mcp_dispatch.rs`'s `CODRIVE_TOOLS` check).
        //
        // A2: `codrive_status` is the read-only driving-state query on the
        // same socket. It is deliberately held to the SAME tier as
        // `codrive_run` rather than being softened for being a read: it
        // reveals whether a human is at the shared desktop right now, which
        // is exactly the signal an agent would want in order to time an
        // action around the human's absence. Enumerated on its own line for
        // the same reason `codrive_run` is — never the Admin fall-through.
        "codrive_run" | "codrive_status" => Some(Scope::Admin),
        // ── High-impact tools — explicitly Admin (C2 fix) ────────────────
        // Arbitrary code execution, agent lifecycle/identity mutation, prompt
        // rewrite, cross-agent dispatch, scheduling, and evolution control.
        // These previously fell through to `None` (no scope), letting any
        // narrowly-scoped internal key invoke them.
        // D1 source rollback: mass-expires facts + cascades trust downgrades —
        // high blast radius, so it requires Admin (the strictest reasonable
        // scope) rather than plain memory:write.
        "memory_invalidate_by_origin"
        | "execute_program"
        | "create_agent"
        | "spawn_agent"
        // O2 ephemeral synthesis: same blast radius as spawn_agent (agent
        // lifecycle + dispatch) — enumerated explicitly instead of relying
        // on the Admin fall-through (2026-07 scope-table consistency).
        | "spawn_ephemeral"
        | "agent_update"
        | "agent_update_soul"
        | "agent_remove"
        | "send_to_agent"
        | "evolution_toggle"
        | "schedule_task"
        | "delete_cron_task"
        | "update_cron_task"
        | "pause_cron_task"
        | "run_cron_task"
        | "channel_config"
        | "model_download"
        | "model_load"
        | "model_unload"
        | "llamafile_start"
        | "llamafile_stop"
        | "inference_mode"
        // JitRL feedback mutates the local-inference experience store —
        // same tier as the other inference-control tools above.
        | "jitrl_feedback"
        // Cost analytics comparison: enumerated explicitly at the same
        // effective scope it already had via the Admin default (the other
        // cost_* tools also resolve to Admin today).
        | "cost_multi_vs_single"
        | "skill_extract"
        | "skill_graduate"
        | "skill_security_scan"
        | "skill_synthesis_run"
        | "shared_skill_adopt"
        | "shared_skill_share" => Some(Scope::Admin),
        // Fail-closed: any tool not enumerated above requires Admin. See the
        // doc comment on this function.
        _ => Some(Scope::Admin),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Global mutex to serialize tests that manipulate environment variables.
    // env::set_var / remove_var are inherently process-global; running them
    // concurrently across threads is UB in Rust 2024.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_config_dir_with_key(
        key: &str,
        client_id: &str,
        scopes: &[&str],
        is_external: bool,
        created_at: &str,
    ) -> TempDir {
        let dir = TempDir::new().unwrap();
        let scopes_toml = scopes
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let content = format!(
            r#"
[mcp_keys."{key}"]
client_id = "{client_id}"
scopes = [{scopes_toml}]
created_at = "{created_at}"
is_external = {is_external}
"#
        );
        let mut f = std::fs::File::create(dir.path().join("config.toml")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        dir
    }

    fn fresh_key(env_suffix: &str) -> String {
        // Generate a valid-format key with fresh created_at
        format!("ddc_{env_suffix}_a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4")
    }

    /// Today's date in RFC-3339 form, for tests that need a fresh `created_at`.
    ///
    /// Replaces the hardcoded `2026-04-29T00:00:00Z` string that was used
    /// across these tests pre-2026-06-01 and which became a time-bomb: once
    /// the wall-clock crossed 30 days past 2026-04-29, every test that
    /// expected `Ok(Principal)` started failing with `KeyExpired`. Calling
    /// `Utc::now()` keeps the suite robust to time.
    fn fresh_today_rfc3339() -> String {
        Utc::now().to_rfc3339()
    }

    // ── Test 1: valid key returns correct Principal ───────────────────────────
    #[test]
    fn test_valid_key_returns_principal() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let today = fresh_today_rfc3339();
        let dir = make_config_dir_with_key(
            &key,
            "claude-desktop",
            &["memory:read", "wiki:read"],
            true,
            &today,
        );
        // SAFETY: protected by ENV_LOCK — no concurrent env mutation.
        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
        let result = authenticate_from_env(dir.path());
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        let principal = result.expect("should authenticate successfully");
        assert_eq!(principal.client_id, "claude-desktop");
        assert!(principal.is_external);
        assert!(principal.scopes.contains(&Scope::MemoryRead));
        assert!(principal.scopes.contains(&Scope::WikiRead));
    }

    // ── Test 2: missing env var → MissingKey (registry has entries) ──────────
    #[test]
    fn test_missing_env_var_returns_missing_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let today = fresh_today_rfc3339();
        let dir = make_config_dir_with_key(
            &key,
            "claude-desktop",
            &["memory:read"],
            true,
            &today,
        );
        // SAFETY: protected by ENV_LOCK.
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
        let result = authenticate_from_env(dir.path());
        assert_eq!(result.unwrap_err(), AuthError::MissingKey);
    }

    // ── Test 3: key format error (too short) → InvalidFormat ─────────────────
    #[test]
    fn test_invalid_format_too_short() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        // SAFETY: protected by ENV_LOCK.
        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", "ddc_prod_tooshort") };
        let result = authenticate_from_env(dir.path());
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
        assert_eq!(result.unwrap_err(), AuthError::InvalidFormat);
    }

    // ── Test 4: valid format but not in registry → UnknownKey ────────────────
    #[test]
    fn test_unknown_key_not_in_registry() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        // Empty config (no mcp_keys section)
        std::fs::write(dir.path().join("config.toml"), "[settings]\nfoo = 1\n").unwrap();
        let key = fresh_key("prod");
        // SAFETY: protected by ENV_LOCK.
        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
        let result = authenticate_from_env(dir.path());
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
        assert_eq!(result.unwrap_err(), AuthError::UnknownKey);
    }

    // ── Test 5: key older than 30 days → KeyExpired ───────────────────────────
    #[test]
    fn test_expired_key_31_days_old() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        // Use a date clearly more than 30 days in the past relative to any
        // reasonable "now" during CI — 2025-01-01 is well over 90 days before
        // the earliest possible test run date.
        let old_date = "2025-01-01T00:00:00Z";
        let dir = make_config_dir_with_key(
            &key,
            "claude-desktop",
            &["memory:read"],
            true,
            old_date,
        );
        // SAFETY: protected by ENV_LOCK.
        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
        let result = authenticate_from_env(dir.path());
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        match result.unwrap_err() {
            AuthError::KeyExpired { days_old } => {
                assert!(days_old >= 31, "expected at least 31 days, got {days_old}");
            }
            other => panic!("expected KeyExpired, got {other:?}"),
        }
    }

    // ── Test 6: parse_scopes happy path ───────────────────────────────────────
    #[test]
    fn test_parse_scopes_memory_read_wiki_write() {
        let scopes = parse_scopes("memory:read,wiki:write").expect("should parse");
        assert!(scopes.contains(&Scope::MemoryRead));
        assert!(scopes.contains(&Scope::WikiWrite));
        assert_eq!(scopes.len(), 2);
    }

    // ── Test 7: parse_scopes unknown scope → InvalidScope ────────────────────
    #[test]
    fn test_parse_scopes_unknown_returns_invalid_scope() {
        let result = parse_scopes("unknown:scope");
        assert!(matches!(result, Err(AuthError::InvalidScope(_))));
    }

    // ── P0-S3 (2026-08 audit): lock `Scope` ↔ the shared canonical list in
    // duduclaw-core bidirectionally. The gateway's `mcp_keys.create` scope
    // validator and the frontend's scope picker both read
    // `duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS` (gateway can't depend on
    // this crate to read `Scope` directly — see that module's doc comment).
    // If a future scope is added to the enum but not the shared list (or vice
    // versa), this test goes red instead of the drift silently reappearing as
    // "Unknown scope" in the dashboard for the new scope.
    #[test]
    fn scope_enum_matches_canonical_list() {
        use duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS;

        // Every enum variant, listed explicitly (Scope has no EnumIter — this
        // hardcoded list IS the trip-wire: forgetting to add a new variant
        // here is caught by the length assertion below).
        let all_variants = [
            Scope::MemoryRead,
            Scope::MemoryWrite,
            Scope::WikiRead,
            Scope::WikiWrite,
            Scope::MessagingSend,
            Scope::IdentityRead,
            Scope::OdooRead,
            Scope::OdooWrite,
            Scope::OdooExecute,
            Scope::GoogleRead,
            Scope::GoogleWrite,
            Scope::NotionRead,
            Scope::NotionWrite,
            Scope::GithubRead,
            Scope::GithubWrite,
            Scope::ForkExecute,
            Scope::OsNative,
            Scope::SkillExecute,
            Scope::Recording,
            Scope::MailRead,
            Scope::MailSend,
            Scope::Admin,
        ];

        assert_eq!(
            all_variants.len(),
            MCP_SCOPE_STRINGS.len(),
            "Scope enum and duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS have \
             drifted in size — add the new variant to BOTH lists"
        );

        // Direction 1: every enum variant's Display string is in the shared
        // list, and parses back to the same variant.
        for variant in &all_variants {
            let wire = variant.to_string();
            assert!(
                MCP_SCOPE_STRINGS.contains(&wire.as_str()),
                "Scope::{variant:?} ({wire}) missing from \
                 duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS"
            );
            let mut parsed = parse_scopes(&wire).expect("must parse its own Display string");
            assert_eq!(parsed.len(), 1);
            assert!(
                parsed.remove(variant),
                "parse_scopes({wire}) did not round-trip to Scope::{variant:?}"
            );
        }

        // Direction 2: every string in the shared list parses successfully
        // (no orphan entries the enum doesn't back).
        for s in MCP_SCOPE_STRINGS {
            assert!(
                parse_scopes(s).is_ok(),
                "canonical scope string '{s}' does not parse via Scope::from_str"
            );
        }
    }

    // ── OS-native Phase 1: os:native scope round-trips ───────────────────────
    #[test]
    fn test_os_native_scope_parse_and_display() {
        let scopes = parse_scopes("os:native").expect("should parse");
        assert!(scopes.contains(&Scope::OsNative));
        assert_eq!(scopes.len(), 1);
        assert_eq!(Scope::OsNative.to_string(), "os:native");
    }

    // ── Test 8: tool_requires_scope memory_store → MemoryWrite ───────────────
    #[test]
    fn test_tool_requires_scope_memory_store() {
        assert_eq!(
            tool_requires_scope("memory_store"),
            Some(Scope::MemoryWrite)
        );
    }

    // ── Test 9: tool_requires_scope memory_search → MemoryRead ───────────────
    #[test]
    fn test_tool_requires_scope_memory_search() {
        assert_eq!(
            tool_requires_scope("memory_search"),
            Some(Scope::MemoryRead)
        );
    }

    // ── Test 10: tool_requires_scope totally_unknown → None ──────────────────
    #[test]
    fn test_recording_scope_parses_and_maps() {
        // WP3.3: the recording scope round-trips through parse/Display and
        // every recording tool maps to it (never falls through to Admin).
        let scopes = parse_scopes("recording").expect("should parse");
        assert!(scopes.contains(&Scope::Recording));
        assert_eq!(Scope::Recording.to_string(), "recording");
        for tool in [
            "browser_record_start",
            "browser_record_stop",
            "desktop_record_start",
            "desktop_record_stop",
            "skill_from_recording",
        ] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::Recording),
                "tool {tool} must require the recording scope"
            );
        }
    }

    #[test]
    fn test_tool_requires_scope_unknown_tool() {
        // C2: fail-closed — unknown tools require Admin, not None.
        assert_eq!(tool_requires_scope("totally_unknown"), Some(Scope::Admin));
    }

    // ── O-0: system-operator tool face (DESIGN-agent-os-native-apps-2026-08.md
    //    §6.3) — every os_* system tool maps to Admin, and is therefore never
    //    reachable by an external MCP client (Admin is not in
    //    EXTERNALLY_GRANTABLE_SCOPES). ──────────────────────────────────────

    #[test]
    fn os_ops_tools_require_admin_scope() {
        for tool in [
            "os_device_status",
            "os_system_status",
            "os_check_update",
            "os_backup_list",
            "os_network_info",
            "os_wifi_status",
            "os_wifi_scan",
            "os_wifi_connect",
            "os_apply_update",
            "os_boot_assessment",
            "os_update_rollback",
            "os_backup_create",
            "os_power",
            "os_factory_reset",
            "os_doctor_repair",
        ] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::Admin),
                "tool {tool} must require Admin scope"
            );
        }
    }

    #[test]
    fn os_ops_tools_never_reachable_by_external_clients() {
        // Even an external principal that explicitly claims Admin cannot
        // reach these — `external_tool_allowed` only substitutes within
        // `EXTERNALLY_GRANTABLE_SCOPES`, which does not include Admin.
        let principal = Principal {
            client_id: "external-client".to_string(),
            scopes: [Scope::Admin].into_iter().collect(),
            is_external: true,
            created_at: chrono::Utc::now(),
        };
        for tool in [
            "os_device_status",
            "os_factory_reset",
            "os_power",
            "os_apply_update",
            "os_boot_assessment",
            "os_update_rollback",
        ] {
            assert!(
                !external_tool_allowed(tool, &principal),
                "tool {tool} must never be reachable by an external client"
            );
        }
    }

    // ── Drift guard: dashboard tool catalog ↔ security gate ──────────────────
    // The dashboard "add from built-in tools" picker is fed by
    // `duduclaw_core::tool_catalog::builtin_tool_catalog()`, which lives in
    // `duduclaw-core` because the gateway cannot depend on this crate (cli →
    // gateway → core; a gateway → cli dep would be a cycle). This test is the
    // mechanical guard that keeps the catalog's advertised scope byte-identical
    // to what `tool_requires_scope` actually enforces. If someone changes a
    // tool's scope in the gate but not the catalog (or vice versa), this fails.
    #[test]
    fn test_catalog_scopes_match_tool_requires_scope() {
        for entry in duduclaw_core::tool_catalog::builtin_tool_catalog() {
            if entry.kind != "mcp" {
                continue; // native Claude tools have no MCP scope
            }
            let enforced = tool_requires_scope(entry.name)
                .expect("enumerated MCP tool must resolve to a scope")
                .to_string();
            assert_eq!(
                enforced, entry.scope,
                "catalog scope for `{}` ({}) drifted from tool_requires_scope ({})",
                entry.name, entry.scope, enforced
            );
        }
    }

    /// A2: both co-drive tool faces resolve to Admin through their OWN
    /// enumerated arm, never the Admin fall-through — so a future scope
    /// split for co-drive stays a one-line diff instead of a silent
    /// behavior change. The read-only `codrive_status` is deliberately held
    /// to the same tier as `codrive_run`: knowing whether a human is
    /// currently at the shared desktop is not a harmless read.
    #[test]
    fn test_both_codrive_tools_require_admin_and_are_internal_only() {
        for tool in ["codrive_run", "codrive_status"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::Admin),
                "tool {tool} must require Admin"
            );
            let external = Principal {
                client_id: "external-client".to_string(),
                scopes: [Scope::Admin].into_iter().collect(),
                is_external: true,
                created_at: chrono::Utc::now(),
            };
            assert!(
                !external_tool_allowed(tool, &external),
                "tool {tool} must never be reachable by an external client"
            );
        }
    }

    #[test]
    fn test_dangerous_tools_require_admin() {
        // C2 regression: these previously returned None (no scope), letting a
        // narrowly-scoped key invoke them.
        for tool in [
            "execute_program",
            "agent_update_soul",
            "agent_remove",
            "agent_update",
            "spawn_agent",
            "create_agent",
            "send_to_agent",
            "evolution_toggle",
            "schedule_task",
            "delete_cron_task",
            "run_cron_task",
            "shared_wiki_write",
            "shared_wiki_delete",
        ] {
            let req = tool_requires_scope(tool);
            assert!(
                matches!(req, Some(Scope::Admin) | Some(Scope::WikiWrite)),
                "tool {tool} must require a real scope, got {req:?}"
            );
            // A memory:read-only principal must NOT satisfy it.
            assert_ne!(req, Some(Scope::MemoryRead));
        }
    }

    #[test]
    fn test_new_odoo_read_tools_require_odoo_read() {
        // The safe customer search + schema introspection tools must sit in the
        // read scope class — never fall through to Admin or (worse) None.
        for tool in ["odoo_partner_search", "odoo_schema_fields"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::OdooRead),
                "tool {tool} must require odoo:read"
            );
        }
    }

    #[test]
    fn test_explicitly_enumerated_admin_tools_2026_07() {
        // Scope-table consistency (2026-07): these previously relied on the
        // Admin fall-through; now enumerated explicitly with the same
        // effective scope. `jitrl_feedback` sits with the inference tools.
        for tool in ["spawn_ephemeral", "cost_multi_vs_single", "jitrl_feedback"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::Admin),
                "tool {tool} must be explicitly Admin"
            );
        }
    }

    #[test]
    fn test_read_tools_keep_narrow_scope() {
        // Narrow read keys must keep working (not forced to Admin).
        assert_eq!(tool_requires_scope("wiki_ls"), Some(Scope::WikiRead));
        assert_eq!(
            tool_requires_scope("memory_search_by_layer"),
            Some(Scope::MemoryRead)
        );
        assert_eq!(tool_requires_scope("send_photo"), Some(Scope::MessagingSend));
    }

    // ── D3.2: entity-alias tools sit in the memory scope family ──────────────
    #[test]
    fn test_entity_alias_tools_scope() {
        // Adding an alias mutates the knowledge graph → write tier.
        assert_eq!(
            tool_requires_scope("memory_alias_add"),
            Some(Scope::MemoryWrite),
            "memory_alias_add must require memory:write"
        );
        // Listing aliases is read-only.
        assert_eq!(
            tool_requires_scope("memory_alias_list"),
            Some(Scope::MemoryRead),
            "memory_alias_list must require memory:read"
        );
        // A read-only key must NOT satisfy the write tool.
        assert_ne!(tool_requires_scope("memory_alias_add"), Some(Scope::MemoryRead));
    }

    #[test]
    fn test_google_scopes_parse_and_display() {
        let scopes = parse_scopes("google:read,google:write").expect("should parse");
        assert!(scopes.contains(&Scope::GoogleRead));
        assert!(scopes.contains(&Scope::GoogleWrite));
        assert_eq!(Scope::GoogleRead.to_string(), "google:read");
        assert_eq!(Scope::GoogleWrite.to_string(), "google:write");
    }

    #[test]
    fn test_google_tools_scope_split() {
        // Read class.
        for tool in ["google_status", "gmail_search", "gmail_read", "calendar_list_events"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::GoogleRead),
                "tool {tool} must require google:read"
            );
        }
        // Write class — a read-only key must NOT satisfy it.
        for tool in ["gmail_create_draft", "calendar_create_event"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::GoogleWrite),
                "tool {tool} must require google:write"
            );
            assert_ne!(tool_requires_scope(tool), Some(Scope::GoogleRead));
        }
    }

    #[test]
    fn test_notion_scopes_parse_and_display() {
        let scopes = parse_scopes("notion:read,notion:write").expect("should parse");
        assert!(scopes.contains(&Scope::NotionRead));
        assert!(scopes.contains(&Scope::NotionWrite));
        assert_eq!(Scope::NotionRead.to_string(), "notion:read");
        assert_eq!(Scope::NotionWrite.to_string(), "notion:write");
    }

    #[test]
    fn test_github_scopes_parse_and_display() {
        let scopes = parse_scopes("github:read,github:write").expect("should parse");
        assert!(scopes.contains(&Scope::GithubRead));
        assert!(scopes.contains(&Scope::GithubWrite));
        assert_eq!(Scope::GithubRead.to_string(), "github:read");
        assert_eq!(Scope::GithubWrite.to_string(), "github:write");
    }

    #[test]
    fn test_notion_tools_scope_split() {
        for tool in ["notion_status", "notion_search", "notion_page_read"] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::NotionRead),
                "tool {tool} must require notion:read"
            );
        }
        assert_eq!(
            tool_requires_scope("notion_page_append"),
            Some(Scope::NotionWrite)
        );
        // A read-only key must NOT satisfy the write tool.
        assert_ne!(tool_requires_scope("notion_page_append"), Some(Scope::NotionRead));
    }

    #[test]
    fn test_github_tools_scope_split() {
        for tool in [
            "github_status",
            "github_search_issues",
            "github_issue_read",
            "github_pr_read",
        ] {
            assert_eq!(
                tool_requires_scope(tool),
                Some(Scope::GithubRead),
                "tool {tool} must require github:read"
            );
        }
        assert_eq!(
            tool_requires_scope("github_issue_comment"),
            Some(Scope::GithubWrite)
        );
        assert_ne!(tool_requires_scope("github_issue_comment"), Some(Scope::GithubRead));
    }

    #[test]
    fn test_sheets_tools_join_google_scope_split() {
        assert_eq!(tool_requires_scope("sheets_read"), Some(Scope::GoogleRead));
        assert_eq!(tool_requires_scope("sheets_append"), Some(Scope::GoogleWrite));
        assert_ne!(tool_requires_scope("sheets_append"), Some(Scope::GoogleRead));
    }

    #[test]
    fn test_user_code_profile_is_memory_read() {
        // UaC profile compilation is a read-only memory view — same scope as
        // memory_search / user_profile_get.
        assert_eq!(
            tool_requires_scope("user_code_profile"),
            Some(Scope::MemoryRead)
        );
    }

    // ── M6: fail-closed when nothing is configured ────────────────────────────
    #[test]
    fn test_unconfigured_is_fail_closed_by_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap(); // no config.toml ⇒ empty registry
        // SAFETY: protected by ENV_LOCK.
        unsafe {
            std::env::remove_var("DUDUCLAW_MCP_API_KEY");
            std::env::remove_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED");
        }
        let result = authenticate_from_env(dir.path());
        assert_eq!(
            result.unwrap_err(),
            AuthError::MissingKey,
            "unauthenticated peer must NOT be granted the default Admin principal"
        );
    }

    #[test]
    fn test_unconfigured_grants_default_only_with_explicit_optin() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        // SAFETY: protected by ENV_LOCK.
        unsafe {
            std::env::remove_var("DUDUCLAW_MCP_API_KEY");
            std::env::set_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED", "1");
        }
        let result = authenticate_from_env(dir.path());
        unsafe { std::env::remove_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED") };
        let principal = result.expect("explicit opt-in should grant default principal");
        assert_eq!(principal.client_id, "default");
        assert!(principal.scopes.contains(&Scope::Admin));
        assert!(!principal.is_external);
    }

    // ── L12: future-dated key must not be treated as ancient ──────────────────
    #[test]
    fn test_future_dated_key_is_not_expired() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        // created_at 10 days in the FUTURE (clock skew / mis-set system time).
        let future = (Utc::now() + chrono::Duration::days(10)).to_rfc3339();
        let dir = make_config_dir_with_key(&key, "client-future", &["memory:read"], false, &future);
        // SAFETY: protected by ENV_LOCK.
        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
        let result = authenticate_from_env(dir.path());
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
        // Before the L12 fix, num_days() was negative and `as u64` wrapped to a
        // huge value ⇒ KeyExpired. Now age clamps to 0 ⇒ authenticates.
        let principal = result.expect("future-dated key must authenticate, not falsely expire");
        assert_eq!(principal.client_id, "client-future");
    }

    // ── Test 11: constant-time lookup — valid key matching different entries ──
    // Verifies that the constant-time scan selects the correct entry even when
    // multiple keys share the same prefix (tests that the full 48-char comparison
    // is completed, not short-circuited).
    #[test]
    fn test_constant_time_lookup_selects_correct_entry() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Two keys that share the same env prefix (prod) but differ only in the
        // hex body — simulates a timing-attack scenario where a partial match
        // could be detected via early-exit.
        let key_a = "ddc_prod_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; // 32 × 'a'
        let key_b = "ddc_prod_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"; // 32 × 'b'
        let dir = TempDir::new().unwrap();
        let today = fresh_today_rfc3339();
        let content = format!(
            r#"
[mcp_keys."{key_a}"]
client_id = "client-a"
scopes = ["memory:read"]
created_at = "{today}"
is_external = false

[mcp_keys."{key_b}"]
client_id = "client-b"
scopes = ["wiki:read"]
created_at = "{today}"
is_external = true
"#
        );
        std::fs::write(dir.path().join("config.toml"), &content).unwrap();

        // Authenticate with key_b — must resolve to client-b, not client-a.
        // SAFETY: protected by ENV_LOCK.
        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", key_b) };
        let result = authenticate_from_env(dir.path());
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        let principal = result.expect("key_b should authenticate");
        assert_eq!(principal.client_id, "client-b");
        assert!(principal.is_external);
        assert!(principal.scopes.contains(&Scope::WikiRead));
        assert!(!principal.scopes.contains(&Scope::MemoryRead));
    }

    // ── Gap (a), WP-H2 §1.3: KeyRegistryCache / authenticate_from_env_cached ──

    /// Explicitly set a file's mtime forward so tests never depend on
    /// filesystem mtime resolution (HFS+ can be 1s-granular) or need a real
    /// sleep to observe a "changed" mtime.
    fn bump_mtime(path: &std::path::Path, seconds_forward: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let current = f.metadata().unwrap().modified().unwrap();
        f.set_modified(current + std::time::Duration::from_secs(seconds_forward))
            .unwrap();
    }

    /// Key rotation (scopes changed in `[mcp_keys]`) takes effect on the very
    /// next call once the file's mtime has moved — no restart, no waiting for
    /// a TTL. This is the direct regression test for Gap (a): before the fix,
    /// only a fresh `authenticate_from_env` call (i.e. process restart) would
    /// observe a scope change.
    #[test]
    fn cached_auth_observes_scope_rotation_after_mtime_change() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let today = fresh_today_rfc3339();
        let dir = make_config_dir_with_key(&key, "rotating-client", &["memory:read"], false, &today);
        let cache = KeyRegistryCache::new();

        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

        let first = authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
        assert!(first.scopes.contains(&Scope::MemoryRead));
        assert!(!first.scopes.contains(&Scope::WikiWrite));

        // Operator rotates the key's scopes in place (same key string, wider
        // grant) and the file's mtime visibly advances.
        let dir2 = make_config_dir_with_key(&key, "rotating-client", &["memory:read", "wiki:write"], false, &today);
        std::fs::copy(dir2.path().join("config.toml"), dir.path().join("config.toml")).unwrap();
        bump_mtime(&dir.path().join("config.toml"), 5);

        let second = authenticate_from_env_cached(dir.path(), &cache).expect("second call authenticates");
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        assert!(
            second.scopes.contains(&Scope::WikiWrite),
            "rotated scope must be visible on the very next call, not just after a restart"
        );
    }

    /// Key revocation (entry removed from `[mcp_keys]`) takes effect on the
    /// very next call once the file's mtime has moved.
    #[test]
    fn cached_auth_observes_revocation_after_mtime_change() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let today = fresh_today_rfc3339();
        let dir = make_config_dir_with_key(&key, "revoked-client", &["memory:read"], false, &today);
        let cache = KeyRegistryCache::new();

        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

        let first = authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
        assert_eq!(first.client_id, "revoked-client");

        // Operator revokes the key: the `[mcp_keys]` table loses the entry.
        std::fs::write(dir.path().join("config.toml"), "[settings]\nfoo = 1\n").unwrap();
        bump_mtime(&dir.path().join("config.toml"), 5);

        let second = authenticate_from_env_cached(dir.path(), &cache);
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        assert_eq!(
            second.unwrap_err(),
            AuthError::UnknownKey,
            "a revoked key must be denied on the very next call, not just after a restart"
        );
    }

    /// When `config.toml`'s mtime has NOT changed, the cache must be reused
    /// rather than re-read from disk. Proven by making the file unreadable
    /// (permission-denied) WITHOUT touching its mtime: a naive
    /// "re-parse every call" implementation would start failing immediately,
    /// while the cached implementation keeps succeeding because it never
    /// re-opens the file.
    #[cfg(unix)]
    #[test]
    fn cached_auth_reuses_registry_when_mtime_unchanged() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let today = fresh_today_rfc3339();
        let dir = make_config_dir_with_key(&key, "cached-client", &["memory:read"], false, &today);
        let cache = KeyRegistryCache::new();
        let config_path = dir.path().join("config.toml");

        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

        let first = authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
        assert_eq!(first.client_id, "cached-client");

        // Make the file unreadable WITHOUT changing its mtime.
        let original_perms = std::fs::metadata(&config_path).unwrap().permissions();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let second = authenticate_from_env_cached(dir.path(), &cache);

        // Restore permissions before any panic/cleanup can trip over it.
        std::fs::set_permissions(&config_path, original_perms).unwrap();
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        let second = second.expect(
            "an unchanged mtime must serve the cached registry, not attempt (and fail) a fresh read",
        );
        assert_eq!(second.client_id, "cached-client");
    }

    /// Fail-closed reload: once a reload is triggered (mtime changed) and the
    /// new content is malformed TOML, the call must be DENIED — never fall
    /// back to whatever principal was cached from the last good load.
    #[test]
    fn cached_auth_fails_closed_when_reload_hits_malformed_toml() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let today = fresh_today_rfc3339();
        let dir = make_config_dir_with_key(&key, "good-client", &["memory:read"], false, &today);
        let cache = KeyRegistryCache::new();
        let config_path = dir.path().join("config.toml");

        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

        let first = authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
        assert_eq!(first.client_id, "good-client");

        // Corrupt the file (unterminated table header) and bump its mtime so
        // a reload is triggered.
        std::fs::write(&config_path, "[mcp_keys.\"broken\n\nnot valid toml at all {{{{").unwrap();
        bump_mtime(&config_path, 5);

        let second = authenticate_from_env_cached(dir.path(), &cache);
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        assert_eq!(
            second.unwrap_err(),
            AuthError::ReloadFailed,
            "a broken reload must deny outright, never reuse the previously-cached good principal"
        );
    }

    /// Fail-closed reload with the file becoming genuinely unreadable
    /// (permission denied) after a mtime change — same contract as the
    /// malformed-TOML case above, exercised via a real I/O error instead of a
    /// parse error.
    #[cfg(unix)]
    #[test]
    fn cached_auth_fails_closed_when_reload_hits_io_error() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let today = fresh_today_rfc3339();
        let dir = make_config_dir_with_key(&key, "good-client", &["memory:read"], false, &today);
        let cache = KeyRegistryCache::new();
        let config_path = dir.path().join("config.toml");

        unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

        let first = authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
        assert_eq!(first.client_id, "good-client");

        // Touch the file (new content, so mtime genuinely changes) then
        // revoke read permission entirely.
        std::fs::write(&config_path, "# still readable for a moment\n").unwrap();
        bump_mtime(&config_path, 5);
        let original_perms = std::fs::metadata(&config_path).unwrap().permissions();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let second = authenticate_from_env_cached(dir.path(), &cache);

        std::fs::set_permissions(&config_path, original_perms).unwrap();
        unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

        assert_eq!(
            second.unwrap_err(),
            AuthError::ReloadFailed,
            "an I/O error on reload must deny outright, never reuse the previously-cached principal"
        );
    }
}

#[cfg(test)]
mod external_scope_tests {
    use super::*;
    use std::collections::HashSet;

    fn ext(scopes: &[Scope]) -> Principal {
        Principal {
            client_id: "t".into(),
            scopes: scopes.iter().cloned().collect::<HashSet<_>>(),
            is_external: true,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn default_external_key_is_byte_identical_to_legacy_whitelist() {
        let p = ext(&[]);
        for t in crate::mcp::EXTERNAL_TOOLS_WHITELIST {
            assert!(external_tool_allowed(t, &p), "{t} must stay allowed");
        }
        // Same-family-but-off-whitelist tools stay hidden without a grant.
        assert!(!external_tool_allowed("memory_alias_add", &p));
        assert!(!external_tool_allowed("memory_fetch_batch", &p));
        // Unscoped (Admin-class) tools are never external.
        assert!(!external_tool_allowed("create_agent", &p));
    }

    #[test]
    fn explicit_grant_widens_within_family_only() {
        let p = ext(&[Scope::MemoryWrite]);
        assert!(external_tool_allowed("memory_alias_add", &p));
        assert!(external_tool_allowed("working_state_set", &p));
        // Different family still needs its own grant.
        assert!(!external_tool_allowed("memory_fetch_batch", &p), "read tier not granted");
    }

    #[test]
    fn non_grantable_scopes_are_refused_even_when_the_key_claims_them() {
        // A key that somehow carries a connector scope gains nothing:
        // Odoo/Google/etc. are outside EXTERNALLY_GRANTABLE_SCOPES.
        let p = ext(&[Scope::OdooRead, Scope::GoogleRead, Scope::IdentityRead]);
        assert!(!external_tool_allowed("odoo_search", &p));
        assert!(!external_tool_allowed("identity_resolve", &p));
    }

    #[test]
    fn admin_does_not_substitute_for_explicit_grants_externally() {
        let p = ext(&[Scope::Admin]);
        // Whitelist baseline still works…
        assert!(external_tool_allowed("memory_store", &p));
        // …but Admin alone does not unlock the wider families.
        assert!(!external_tool_allowed("memory_alias_add", &p));
        assert!(!external_tool_allowed("working_state_set", &p));
    }
}
