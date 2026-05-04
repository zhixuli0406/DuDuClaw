// mcp_auth.rs — MCP API Key authentication module (W19-P0)
//
// Provides API key validation, principal extraction, and scope enforcement
// for the MCP server's authentication layer.

use std::collections::{HashMap, HashSet};
use std::path::Path;

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

#[derive(Debug)]
struct KeyEntry {
    client_id: String,
    scopes: HashSet<Scope>,
    is_external: bool,
    created_at: DateTime<Utc>,
}

/// Load mcp_keys from ~/.duduclaw/config.toml
fn load_key_registry(config_dir: &Path) -> HashMap<String, KeyEntry> {
    let config_path = config_dir.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let doc: toml::Value = match toml::from_str(&content) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let mut registry = HashMap::new();

    let mcp_keys = match doc.get("mcp_keys").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return registry,
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

    registry
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

    if !is_valid_key_format(raw_key) {
        return Err(AuthError::InvalidFormat);
    }

    // Constant-time key lookup: iterate ALL entries so the number of iterations
    // does not leak whether a key prefix matches.  Within each comparison,
    // subtle::ConstantTimeEq prevents early-exit on the first differing byte.
    let entry = {
        let raw_bytes = raw_key.as_bytes();
        let mut found: Option<&KeyEntry> = None;
        for (stored_key, entry) in &registry {
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

    // Expiry check: key must not be older than 30 days
    let age = Utc::now().signed_duration_since(entry.created_at);
    let days_old = age.num_days() as u64;
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
/// Backwards-compatible: if the env var is absent AND the registry has no
/// mcp_keys at all, returns a default internal Principal with all scopes so
/// existing internal tooling keeps working unchanged.
///
/// For programmatic key injection (e.g. tests, HTTP transport), use
/// [`authenticate_with_key`] directly.
pub fn authenticate_from_env(config_dir: &Path) -> Result<Principal, AuthError> {
    let registry = load_key_registry(config_dir);

    let raw_key = match std::env::var("DUDUCLAW_MCP_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            // No env var set — fall back to default internal principal
            // when registry is also empty (backwards-compatible).
            if registry.is_empty() {
                // ⚠️ SECURITY WARNING: Running without any MCP key authentication.
                // Set DUDUCLAW_MCP_API_KEY and configure [mcp_keys] in config.toml
                // for production deployments.
                tracing::warn!(
                    "MCP server starting without API key authentication (no DUDUCLAW_MCP_API_KEY \
                     and no [mcp_keys] in config.toml). All scopes granted to default internal \
                     principal. This is only safe for trusted local usage."
                );
                return Ok(default_internal_principal());
            }
            return Err(AuthError::MissingKey);
        }
    };

    authenticate_with_key(&raw_key, config_dir)
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

/// Parse a comma-separated scope string into a HashSet<Scope>.
/// e.g. "memory:read,wiki:write" → {MemoryRead, WikiWrite}
pub fn parse_scopes(s: &str) -> Result<HashSet<Scope>, AuthError> {
    if s.trim().is_empty() {
        return Ok(HashSet::new());
    }

    let mut result = HashSet::new();
    for part in s.split(',') {
        let part = part.trim();
        match part {
            "memory:read" => {
                result.insert(Scope::MemoryRead);
            }
            "memory:write" => {
                result.insert(Scope::MemoryWrite);
            }
            "wiki:read" => {
                result.insert(Scope::WikiRead);
            }
            "wiki:write" => {
                result.insert(Scope::WikiWrite);
            }
            "messaging:send" => {
                result.insert(Scope::MessagingSend);
            }
            "identity:read" => {
                result.insert(Scope::IdentityRead);
            }
            "odoo:read" => {
                result.insert(Scope::OdooRead);
            }
            "odoo:write" => {
                result.insert(Scope::OdooWrite);
            }
            "odoo:execute" => {
                result.insert(Scope::OdooExecute);
            }
            "admin" => {
                result.insert(Scope::Admin);
            }
            other => return Err(AuthError::InvalidScope(other.to_string())),
        }
    }
    Ok(result)
}

/// Return the minimum Scope required to call this tool.
/// Returns None for unknown / unrestricted tools.
pub fn tool_requires_scope(tool_name: &str) -> Option<Scope> {
    match tool_name {
        "memory_search" | "memory_read" => Some(Scope::MemoryRead),
        "memory_store" => Some(Scope::MemoryWrite),
        "wiki_read" | "wiki_search" => Some(Scope::WikiRead),
        "wiki_write" => Some(Scope::WikiWrite),
        "send_message" => Some(Scope::MessagingSend),
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
        _ => None,
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

    // ── Test 1: valid key returns correct Principal ───────────────────────────
    #[test]
    fn test_valid_key_returns_principal() {
        let _guard = ENV_LOCK.lock().unwrap();
        let key = fresh_key("prod");
        let dir = make_config_dir_with_key(
            &key,
            "claude-desktop",
            &["memory:read", "wiki:read"],
            true,
            "2026-04-29T00:00:00Z",
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
        let dir = make_config_dir_with_key(
            &key,
            "claude-desktop",
            &["memory:read"],
            true,
            "2026-04-29T00:00:00Z",
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
    fn test_tool_requires_scope_unknown_tool() {
        assert_eq!(tool_requires_scope("totally_unknown"), None);
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
        let content = format!(
            r#"
[mcp_keys."{key_a}"]
client_id = "client-a"
scopes = ["memory:read"]
created_at = "2026-04-29T00:00:00Z"
is_external = false

[mcp_keys."{key_b}"]
client_id = "client-b"
scopes = ["wiki:read"]
created_at = "2026-04-29T00:00:00Z"
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
}
