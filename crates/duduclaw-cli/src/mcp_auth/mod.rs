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
//                 Since RM-134 (v1.82) dashboard keys are decided by the
//                 hashed store (`duduclaw_core::mcp_key_store`, opened
//                 read-only); the config registry is consulted only for the
//                 gateway-internal key whose fingerprint the gateway
//                 recorded, and in the documented legacy window (no store
//                 yet, or recorded as pending legacy) — see
//                 [`authenticate_against_registry`].

mod grants;
mod scope;
pub mod strategy;

pub use grants::{EXTERNALLY_GRANTABLE_SCOPES, external_tool_allowed};
pub(crate) use grants::{dedicated_google_call, dedicated_google_tool_allowed};
pub use scope::{Scope, parse_scopes, tool_requires_scope, tool_requires_scope_for_args};

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use subtle::ConstantTimeEq;

mod accepted_google;
pub(crate) use accepted_google::{
    AcceptedGoogleCall, accept_google_lane, current_accepted_google_call,
    current_google_account_approval, with_accepted_google_call, with_google_account_approval,
};

#[derive(Debug, Clone)]
pub struct Principal {
    pub client_id: String,
    pub scopes: HashSet<Scope>,
    pub is_external: bool,
    pub created_at: DateTime<Utc>,
}

/// How the authority decided a key (v1.82, RM-134 third review). Carried on
/// the [`AuthenticatedCall`] — never on the `Principal`, so it cannot be
/// claimed — and read by the HTTP MCP server's gate
/// (`crate::mcp::remote_access`), which decides from it alone which
/// non-external principal is the gateway's own internal key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrincipalKind {
    /// The gateway's own `gateway-internal` key, accepted through the
    /// fingerprint the gateway recorded (`Authority::Internal`).
    GatewayInternal,
    /// A key in the hashed store; `agent_binding` is its immutable employee
    /// binding (`StoredKey::agent_binding`) and `key_created_at` its
    /// creation time, which the HTTP gate compares with the bound
    /// employee's directory birth time (fourth review: a key never
    /// predates its employee — `duduclaw_core::mcp_key_store::key_binding_state`).
    Stored { agent_binding: Option<String>, key_created_at: DateTime<Utc> },
    /// Anything else: a `[mcp_keys]` entry read in the legacy window, a
    /// refresh token, an OAuth token, the explicit unauthenticated default,
    /// or a call constructed without an authority decision.
    Other,
}

/// The same successful authentication that produced this call's principal.
/// OAuth and explicitly unauthenticated calls have no Google execution receipt.
#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedCall {
    principal: Principal,
    credential_ref: Option<duduclaw_core::google_identity::CredentialRef>,
    kind: PrincipalKind,
}

impl AuthenticatedCall {
    pub(crate) fn principal(&self) -> &Principal {
        &self.principal
    }
    pub(crate) fn credential_ref(&self) -> Option<&duduclaw_core::google_identity::CredentialRef> {
        self.credential_ref.as_ref()
    }
    /// How the key was decided ([`PrincipalKind`]).
    pub(crate) fn kind(&self) -> &PrincipalKind {
        &self.kind
    }
    pub(crate) fn without_reference(principal: Principal) -> Self {
        Self {
            principal,
            credential_ref: None,
            kind: PrincipalKind::Other,
        }
    }
    /// The same call with the authority's decision attached.
    pub(crate) fn with_kind(mut self, kind: PrincipalKind) -> Self {
        self.kind = kind;
        self
    }
    fn verified(
        principal: Principal,
        raw: &str,
        kind: duduclaw_core::google_identity::CredentialKind,
    ) -> Self {
        use sha2::Digest;
        Self {
            principal,
            credential_ref: Some(duduclaw_core::google_identity::CredentialRef {
                kind,
                sha256: format!("{:x}", sha2::Sha256::digest(raw.as_bytes())),
            }),
            kind: PrincipalKind::Other,
        }
    }
}

tokio::task_local! {
    static AUTHENTICATED_CALL: AuthenticatedCall;
}

/// Box before returning the scope future: dispatch itself can be large.
pub(crate) fn with_authenticated_call<F: std::future::Future>(
    call: AuthenticatedCall,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    AUTHENTICATED_CALL.scope(call, Box::pin(future))
}

pub(crate) fn current_authenticated_call() -> Option<AuthenticatedCall> {
    AUTHENTICATED_CALL.try_with(Clone::clone).ok()
}

pub(crate) fn current_call_matches(principal: &Principal) -> Option<AuthenticatedCall> {
    current_authenticated_call().filter(|call| {
        let actual = call.principal();
        actual.client_id == principal.client_id
            && actual.scopes == principal.scopes
            && actual.is_external == principal.is_external
            && actual.created_at == principal.created_at
    })
}

#[derive(Debug, PartialEq)]
pub enum AuthError {
    MissingKey,
    InvalidFormat,
    UnknownKey,
    /// A legacy `[mcp_keys]` entry (the gateway-internal key, or an entry not
    /// yet migrated) older than 30 days.
    KeyExpired {
        days_old: u64,
    },
    /// RM-134: a key in the hashed store that is expired or disabled. The
    /// reason is kept for logs and tests; [`std::fmt::Display`] is the same
    /// text for both so a caller cannot tell them apart.
    KeyInactive(InactiveReason),
    /// RM-134: the hashed key store exists but could not be read. Fail-closed.
    StoreUnavailable,
    InvalidScope(String),
    /// Gap (a), WP-H2 §1.3: a per-call re-authentication attempt needed to
    /// reload the on-disk key registry (its mtime had changed) but the
    /// reload itself failed — an I/O error other than "file does not exist",
    /// or malformed TOML. Fail-closed: this is intentionally a HARD deny,
    /// never a silent fall-back to whatever was cached before.
    ReloadFailed,
}

/// Why a stored key was refused (internal only — see [`AuthError::KeyInactive`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactiveReason {
    Expired,
    Disabled,
}

/// The one external text for an expired or disabled stored key.
pub const KEY_INACTIVE_MESSAGE: &str = "API key is expired or disabled";

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingKey => write!(f, "DUDUCLAW_MCP_API_KEY environment variable not set"),
            AuthError::InvalidFormat => write!(f, "API key has invalid format"),
            AuthError::UnknownKey => write!(f, "API key not found in registry"),
            AuthError::KeyExpired { days_old } => {
                write!(f, "API key expired ({days_old} days old, max 30)")
            }
            AuthError::KeyInactive(_) => f.write_str(KEY_INACTIVE_MESSAGE),
            AuthError::StoreUnavailable => write!(
                f,
                "MCP key store unavailable — denying (fail-closed)"
            ),
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
    authenticate_against_registry(raw_key, config_dir, &registry).map(|(principal, _)| principal)
}

pub(crate) fn authenticate_with_key_call(
    raw_key: &str,
    config_dir: &Path,
) -> Result<AuthenticatedCall, AuthError> {
    let registry = load_key_registry(config_dir);
    authenticate_against_registry(raw_key, config_dir, &registry).map(|(principal, kind)| {
        AuthenticatedCall::verified(
            principal,
            raw_key,
            duduclaw_core::google_identity::CredentialKind::Legacy,
        )
        .with_kind(kind)
    })
}

/// Core lookup shared by [`authenticate_with_key`] (fresh parse every call)
/// and [`authenticate_from_env_cached`] (mtime-cached parse) — the decision
/// is identical either way; only how the `registry` argument was obtained
/// differs.
///
/// RM-134: the authority store decides which source applies
/// (`duduclaw_core::mcp_key_store::resolve`, opened strictly read-only):
/// 1. an existing store that cannot be read, or carries an unexpected
///    schema, denies everything — the gateway-internal key included
///    ([`AuthError::StoreUnavailable`]);
/// 2. a stored dashboard key decides alone (disabled / expired / deleted
///    refuse even if a stale config copy exists);
/// 3. the gateway's own key is accepted only when the gateway recorded its
///    fingerprint, as a `gateway-internal`, non-external `[mcp_keys]` entry
///    inside the 30-day rule (entry and recorded `created_at` both) — a
///    config entry merely claiming that client id is not enough;
/// 4. `[mcp_keys]` is read for any other key only in the documented legacy
///    window: no store yet (bootstrap), or a fingerprint the gateway recorded
///    as pending legacy when it began the migration;
/// 5. everything else is an unknown key, whatever `config.toml` says.
///
/// Returns the principal with how it was decided ([`PrincipalKind`]).
fn authenticate_against_registry(
    raw_key: &str,
    config_dir: &Path,
    registry: &HashMap<String, KeyEntry>,
) -> Result<(Principal, PrincipalKind), AuthError> {
    use duduclaw_core::mcp_key_store::{self as ks, Authority};
    if !is_valid_key_format(raw_key) {
        return Err(AuthError::InvalidFormat);
    }
    let legacy = legacy_lookup(raw_key, registry);
    match ks::resolve(config_dir, raw_key) {
        Err(error) => {
            tracing::warn!(%error, "MCP key store unreadable — denying key authentication (fail-closed)");
            Err(AuthError::StoreUnavailable)
        }
        Ok(Authority::Stored(key)) => stored_principal(config_dir, &key).map(|principal| {
            (principal, PrincipalKind::Stored { agent_binding: key.agent_binding.clone(), key_created_at: key.created_at })
        }),
        Ok(Authority::Internal(internal)) => match legacy {
            Some(entry) if !entry.is_external && entry.client_id == ks::RESERVED_INTERNAL_CLIENT_ID => {
                if !ks::legacy_age_accepts(internal.created_at, Utc::now()) {
                    let days_old = Utc::now().signed_duration_since(internal.created_at).num_days().max(0) as u64;
                    return Err(AuthError::KeyExpired { days_old });
                }
                legacy_principal(entry).map(|principal| (principal, PrincipalKind::GatewayInternal))
            }
            _ => Err(AuthError::UnknownKey),
        },
        Ok(Authority::Legacy(window)) => match legacy {
            Some(entry) => {
                tracing::debug!(
                    client_id = entry.client_id.as_str(),
                    window = ?window,
                    "MCP key authenticated through the legacy [mcp_keys] read path"
                );
                legacy_principal(entry).map(|principal| (principal, PrincipalKind::Other))
            }
            None => Err(AuthError::UnknownKey),
        },
        Ok(Authority::Unknown) => {
            if legacy.is_some() {
                tracing::info!("MCP key refused: a [mcp_keys] entry the key store does not know is ignored");
            }
            Err(AuthError::UnknownKey)
        }
    }
}

/// Decide a key found in the hashed store, and record its use (throttled)
/// in the separate usage file on success. Recording never writes the
/// authority store, and a failed record never denies the call.
fn stored_principal(
    config_dir: &Path,
    key: &duduclaw_core::mcp_key_store::StoredKey,
) -> Result<Principal, AuthError> {
    use duduclaw_core::mcp_key_store::KeyState;
    let now = Utc::now();
    match key.state(now) {
        KeyState::Active => {}
        KeyState::Disabled => {
            tracing::info!(key_id = key.id.as_str(), reason = "disabled", "MCP key refused");
            return Err(AuthError::KeyInactive(InactiveReason::Disabled));
        }
        KeyState::Expired => {
            tracing::info!(key_id = key.id.as_str(), reason = "expired", "MCP key refused");
            return Err(AuthError::KeyInactive(InactiveReason::Expired));
        }
        KeyState::Deleted => {
            tracing::info!(key_id = key.id.as_str(), reason = "deleted", "MCP key refused");
            return Err(AuthError::UnknownKey);
        }
    }
    if let Err(error) = duduclaw_core::mcp_key_store::record_usage(config_dir, &key.id, now) {
        tracing::warn!(key_id = key.id.as_str(), %error, "MCP key last use not recorded");
    }
    // Same rule as the legacy registry: an unknown scope string (a newer
    // binary's row read by an older one) collapses to no scopes.
    let scopes = parse_scopes(&key.scopes.join(",")).unwrap_or_default();
    // RM-134 fourth review (Medium 2): a key bound to an employee carries
    // only per-employee scopes, whatever its row says — `admin` (or any
    // other scope) on such a row never takes effect, so it can never stand
    // in for the individual scopes either.
    let scopes = if !key.is_external && key.agent_binding.is_some() {
        employee_key_effective_scopes(&scopes)
    } else {
        scopes
    };
    Ok(Principal {
        client_id: key.client_id.clone(),
        scopes,
        is_external: key.is_external,
        created_at: key.created_at,
    })
}

/// RM-134 fourth review (Medium 2): `scopes` ∩
/// `duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS` — the only scopes
/// a key bound to an AI employee ever exercises. Never `admin`.
pub(crate) fn employee_key_effective_scopes(scopes: &HashSet<Scope>) -> HashSet<Scope> {
    scopes.iter().filter(|scope| employee_key_scope(scope)).cloned().collect()
}

/// Whether `scope` is one a per-employee key may exercise.
pub(crate) fn employee_key_scope(scope: &Scope) -> bool {
    duduclaw_core::mcp_scopes::EMPLOYEE_KEY_SCOPE_STRINGS.contains(&scope.to_string().as_str())
}

/// The scope gate (RM-134 fourth review, Medium 2). For a principal acting
/// as a bound employee over the HTTP MCP server (`bound_employee`), the
/// effective scopes are its scopes ∩ the per-employee scopes: the required
/// scope must be one of those AND held — `admin` never stands in, even if a
/// row somehow carries it. Every other principal: the required scope, or
/// `admin`, as before.
pub(crate) fn scope_gate_admits(principal: &Principal, required: &Scope, bound_employee: bool) -> bool {
    if bound_employee {
        return employee_key_scope(required) && principal.scopes.contains(required);
    }
    principal.scopes.contains(required) || principal.scopes.contains(&Scope::Admin)
}

/// The dispatch scope gate as one decision: the scope `tool` called with
/// `args` requires ([`tool_requires_scope_for_args`]; a tool missing from the
/// table needs `admin`) when [`scope_gate_admits`] refuses it, else `None`.
///
/// Both scope checks in `McpDispatcher::dispatch_tool_call` and the HTTP
/// `tools/list` for a key bound to an employee (`mcp::tools_list`, with
/// `args = Value::Null`, i.e. [`tool_requires_scope`]) call this function, so
/// a bound key lists exactly the tools this gate lets it call.
pub(crate) fn scope_gate_refusal(
    principal: &Principal,
    tool: &str,
    args: &serde_json::Value,
    bound_employee: bool,
) -> Option<Scope> {
    tool_requires_scope_for_args(tool, args)
        .filter(|required| !scope_gate_admits(principal, required, bound_employee))
}

/// The 30-day rule of a `[mcp_keys]` entry
/// (`duduclaw_core::mcp_key_store::legacy_age_accepts`).
fn legacy_principal(entry: &KeyEntry) -> Result<Principal, AuthError> {
    // Expiry check: key must not be older than 30 days.
    // L12: a future-dated `created_at` yields a negative duration; clamp to 0
    // so the `as u64` cast can't wrap into an absurd "age" and falsely expire it.
    let now = Utc::now();
    if !duduclaw_core::mcp_key_store::legacy_age_accepts(entry.created_at, now) {
        let days_old = now.signed_duration_since(entry.created_at).num_days().max(0) as u64;
        return Err(AuthError::KeyExpired { days_old });
    }

    Ok(Principal {
        client_id: entry.client_id.clone(),
        scopes: entry.scopes.clone(),
        is_external: entry.is_external,
        created_at: entry.created_at,
    })
}

/// Constant-time scan of the legacy registry for `raw_key`.
fn legacy_lookup<'a>(raw_key: &str, registry: &'a HashMap<String, KeyEntry>) -> Option<&'a KeyEntry> {
    // Constant-time key lookup: iterate ALL entries so the number of iterations
    // does not leak whether a key prefix matches.  Within each comparison,
    // subtle::ConstantTimeEq prevents early-exit on the first differing byte.
    {
        let raw_bytes = raw_key.as_bytes();
        let mut found: Option<&'a KeyEntry> = None;
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
        found
    }
}

/// M6 + RM-134: the explicit unauthenticated opt-in applies only when no key
/// is configured anywhere — neither `[mcp_keys]` nor the hashed store. An
/// unreadable store counts as "keys configured" (fail closed).
fn no_keys_configured(config_dir: &Path, registry_empty: bool) -> bool {
    registry_empty && matches!(duduclaw_core::mcp_key_store::has_keys(config_dir), Ok(false))
}

/// Authenticate from DUDUCLAW_MCP_API_KEY env var.
///
/// Accepts two credential formats and dispatches to the appropriate validator:
///
/// 1. **Refresh tokens** (v1.16.0+) — format `ddc_refresh_<env>_<64hex>`.
///    Validated against the SQLite-backed token store, 90-day lifetime,
///    individually revocable. See [`crate::mcp_refresh`].
///
/// 2. **API keys** — format `ddc_<env>_<32hex>`. Dashboard-issued keys are
///    validated against the hashed store `<home>/mcp_keys.db` (RM-134:
///    Admin-chosen expiry, disable, delete); the recorded gateway-internal
///    key and, only in the legacy window, a not-yet-migrated entry against
///    `[mcp_keys]` in `config.toml` (30 days).
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
            if allow_unauthenticated_default() && no_keys_configured(config_dir, registry.is_empty()) {
                tracing::warn!(
                    "MCP server starting without API key authentication \
                     (DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED=1, no DUDUCLAW_MCP_API_KEY and no \
                     MCP keys configured). All scopes granted to default internal \
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
    authenticate_from_env_cached_call(config_dir, cache).map(|call| call.principal)
}

pub(crate) fn authenticate_from_env_cached_call(
    config_dir: &Path,
    cache: &KeyRegistryCache,
) -> Result<AuthenticatedCall, AuthError> {
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
            if allow_unauthenticated_default() && no_keys_configured(config_dir, registry_empty) {
                tracing::warn!(
                    "MCP server starting without API key authentication \
                     (DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED=1, no DUDUCLAW_MCP_API_KEY and no \
                     MCP keys configured). All scopes granted to default internal \
                     principal. This is only safe for trusted local usage."
                );
                return Ok(AuthenticatedCall::without_reference(
                    default_internal_principal(),
                ));
            }
            return Err(AuthError::MissingKey);
        }
    };

    // Same prefix-based dispatch as `authenticate_from_env`.
    if raw_key.starts_with(crate::mcp_refresh::REFRESH_TOKEN_PREFIX) {
        return crate::mcp_refresh::authenticate_with_refresh_token(&raw_key, config_dir).map(
            |principal| {
                AuthenticatedCall::verified(
                    principal,
                    &raw_key,
                    duduclaw_core::google_identity::CredentialKind::Refresh,
                )
            },
        );
    }

    let registry = cache.registry(config_dir).map_err(|()| {
        tracing::warn!(
            "MCP key registry reload failed (I/O or parse error on config.toml) — \
             denying this call (fail-closed); a stale cached principal is never reused"
        );
        AuthError::ReloadFailed
    })?;

    // The hashed store is not cached: like refresh tokens it is re-read on
    // every call (one indexed SELECT), so a disable takes effect at once.
    authenticate_against_registry(&raw_key, config_dir, &registry).map(|(principal, kind)| {
        AuthenticatedCall::verified(
            principal,
            &raw_key,
            duduclaw_core::google_identity::CredentialKind::Legacy,
        )
        .with_kind(kind)
    })
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
        Scope::MessagingRead,
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

#[cfg(test)]
mod authenticated_call_tests;
