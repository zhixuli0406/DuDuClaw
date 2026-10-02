// mcp_auth — MCP API Key authentication (W19-P0)
//
// Provides API key validation, principal extraction, and scope enforcement
// for the MCP server's authentication layer.
//
// ── Layout (audit O14, 2026-09-29) ───────────────────────────────────────
//
// This used to be `mcp_auth.rs` (2,110 lines) plus a sibling
// `mcp_auth_strategy.rs` (679 lines) that held a Strategy-Pattern
// abstraction with **no production call site**: a `dyn AuthStrategy` trait
// whose only real implementation was the API-key path this module already
// exports, two P2 placeholder strategies that returned `InvalidFormat` for
// every request, a one-implementation `KeyRotationPolicy` trait, and a
// `McpAuthMiddleware` wrapper that forwarded to both. The reserved
// abstraction is gone; the behavior it carried (key-age rotation status and
// the registry-wide rotation scan) is inlined as plain functions in
// [`strategy`], where it stays testable and stays next to the code that
// actually authenticates.
//
// - [`scope`]   — the `Scope` vocabulary + the tool → minimum-scope table.
// - [`grants`]  — what an EXTERNAL principal may reach (WP3.2 C4).
// - [`strategy`]— request context, the API-key entry point, key rotation.
// - this file   — `Principal` / `AuthError`, the `[mcp_keys]` registry, its
//                 mtime-aware cache, and the `authenticate_*` entry points.

mod grants;
mod scope;
pub mod strategy;

pub use grants::{external_tool_allowed, EXTERNALLY_GRANTABLE_SCOPES};
pub use scope::{parse_scopes, tool_requires_scope, tool_requires_scope_for_args, Scope};

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use subtle::ConstantTimeEq;

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
    KeyExpired {
        days_old: u64,
    },
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
/// - [`strategy::authenticate`] when a credential is injected via
///   [`strategy::AuthContext::credential`]
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
            let registry_empty = cache
                .registry(config_dir)
                .map(|r| r.is_empty())
                .unwrap_or(false);
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

#[cfg(test)]
mod tests;
