//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── Standalone helpers ────────────────────────────────────────

/// Build one `accounts.list` row from a rotator `AccountStatus` (WP-A —
/// extracted to a pure function for no-I/O testability, matching the
/// `task_row_to_json` precedent elsewhere in this file; also keeps
/// `handle_accounts_list` from drifting in shape from a future second
/// caller).
pub(crate) fn account_status_to_json(a: &duduclaw_agent::account_rotator::AccountStatus) -> Value {
    json!({
        "id": a.id,
        "auth_method": a.auth_method,
        "provider": a.provider,
        "priority": a.priority,
        "is_healthy": a.is_healthy,
        "spent_this_month": a.spent_this_month,
        "monthly_budget_cents": a.monthly_budget_cents,
        "total_requests": a.total_requests,
        "is_available": a.is_available,
        "label": a.label,
        "email": a.email,
        "subscription": a.subscription,
        "expires_at": a.expires_at,
        "days_until_expiry": a.days_until_expiry,
        // D5 — credential state, deliberately the `Display` form
        // (`auth_dead:org_disabled`), NOT `CredentialState`'s serde form
        // (bare `auth_dead`). The dashboard badge distinguishes "re-issue the
        // token" from "ask your org admin", which the flat serde token cannot
        // express; `credential_detail` carries the zh-TW one-liner beside it.
        "credential_state": a.credential_state.to_string(),
        "credential_detail": a.credential_detail,
        // Probe schedule: when the credential probe may next run (RFC 3339,
        // `null` = next tick) and how many conclusive rejections have stacked
        // up. Surfaced so an operator can see that a dead token is being
        // re-checked on a backoff rather than silently forgotten.
        "next_probe_at": a.next_probe_at,
        "probe_failures": a.probe_failures,
    })
}

/// The `[[accounts]]` TOML entry `handle_accounts_add` is about to push, plus
/// whether the credential had to fall back to plaintext.
pub(crate) struct NewAccountEntry {
    pub(crate) table: toml::map::Map<String, toml::Value>,
    /// `true` when `encrypted` was `None` and the credential was stored in
    /// plaintext — the caller uses this to decide whether to log the
    /// "no writable keyfile" warning (kept out of this pure function so it
    /// stays free of side effects and easy to unit test).
    pub(crate) plaintext_fallback: bool,
    /// The TOML field name the credential was written under (`oauth_token`
    /// for `type = "oauth"`; `anthropic_api_key` for provider `"anthropic"`;
    /// `api_key` for every other provider) — echoed back so the caller's
    /// warning names the right field without recomputing it.
    pub(crate) key_field: &'static str,
}

/// Build one `[[accounts]]` entry (WP-6C — extracted from `handle_accounts_add`
/// so the encrypt-vs-plaintext decision is unit-testable without a full
/// `MethodHandler` fixture).
///
/// WP-H1 P1 — write ONE of `<field>_enc` / `<field>`, never both.
///
/// This used to store the plaintext *and* the ciphertext side by side, which
/// is precisely how the 2026-08-15 incident got made
/// (`DESIGN-credentials-doctrine-2026-08.md` §1.5): the read paths only ever
/// consume `<field>_enc`, so the plaintext was inert — and being inert,
/// nothing ever cleaned it up, while `system.config`'s array-of-tables
/// masking gap let it be read straight back out. The channel write path has
/// always done the right thing here (it *removes* the plaintext key after
/// encrypting, `handlers.rs` channel closure); this brings `[[accounts]]`
/// into line.
///
/// Plaintext is written only when encryption is impossible (no writable
/// keyfile, i.e. `encrypted` is `None`) — refusing outright would leave an
/// operator unable to add an account at all, so it degrades loudly instead
/// (via [`NewAccountEntry::plaintext_fallback`]).
///
/// WP-A (TODO-ai-runtimes-2026-09.md §3 WP-A) — `provider` is always written
/// (defaulting to `"anthropic"` at the call site so every existing config
/// keeps behaving byte-identically), and the credential field name follows
/// `duduclaw-agent::account_rotator::resolve_api_key`'s precedence: the
/// Anthropic-only `anthropic_api_key(_enc)` name for provider `"anthropic"`,
/// the provider-agnostic `api_key(_enc)` name for everything else (the
/// rotator has read that fallback name since before this WP; this is the
/// first writer that actually uses it for a non-Anthropic account).
pub(crate) fn build_account_entry(
    id: &str,
    auth_type: &str,
    provider: &str,
    budget_cents: u64,
    priority: u64,
    key: &str,
    encrypted: Option<&str>,
) -> NewAccountEntry {
    let mut account = toml::map::Map::new();
    account.insert("id".into(), toml::Value::String(id.into()));
    account.insert("type".into(), toml::Value::String(auth_type.into()));
    account.insert("provider".into(), toml::Value::String(provider.into()));
    account.insert(
        "monthly_budget_cents".into(),
        toml::Value::Integer(budget_cents as i64),
    );
    account.insert("priority".into(), toml::Value::Integer(priority as i64));

    let key_field = if auth_type == "oauth" {
        "oauth_token"
    } else if provider == "anthropic" {
        "anthropic_api_key"
    } else {
        "api_key"
    };

    let plaintext_fallback = match encrypted {
        Some(enc) => {
            account.insert(
                format!("{key_field}_enc"),
                toml::Value::String(enc.to_string()),
            );
            false
        }
        None => {
            account.insert(key_field.into(), toml::Value::String(key.into()));
            true
        }
    };

    NewAccountEntry {
        table: account,
        plaintext_fallback,
        key_field,
    }
}

/// Platform allowlist for the migrate RPCs. Only the importer targets the
/// CLI understands are accepted; anything else is rejected before we ever
/// spawn a subprocess (fail-closed).
///
/// WP-9A: `claude-code` requires `--agent <id>` (the CLI hard-errors without
/// it — see `migrate_from::run`); this RPC does not yet forward an `agent`
/// param, so a dashboard-triggered `claude-code` run currently fails fast
/// with that message rather than silently no-op-ing. Wiring the dashboard
/// agent picker through is tracked as follow-up, not done in this pass.
pub(crate) fn migrate_platform_allowed(platform: &str) -> bool {
    matches!(
        platform,
        "openclaw" | "hermes" | "paperclip" | "claude-code"
    )
}

/// Validate an optional migrate `source`. `None` is fine (per-platform
/// defaults / discovery). When present it must be a non-empty absolute path —
/// a relative path would resolve against the gateway's cwd, which is ambiguous
/// under launchd, so we reject it rather than guess.
pub(crate) fn validate_migrate_source(source: Option<&str>) -> Result<(), String> {
    match source {
        None => Ok(()),
        Some(s) if s.trim().is_empty() => Err("source must not be empty".to_string()),
        Some(s) if !Path::new(s).is_absolute() => {
            Err(format!("source must be an absolute path, got '{s}'"))
        }
        Some(_) => Ok(()),
    }
}

/// Upper bound for one `docker info` / `podman info` probe. A hung Docker
/// Desktop VM makes `docker info` block indefinitely (observed 2026-09-28:
/// the daemon processes were alive, the socket existed, and `docker info`
/// never returned), which previously hung `system.doctor`, `doctor_repair`
/// and the gateway test suite with it. The other doctor probes already cap
/// themselves (mcp 10s, grok 15s); this one must too.
pub(crate) const CONTAINER_RUNTIME_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Check if Docker (or Podman) is available by running `docker info`.
/// Returns `("pass"/"warn", message)`.
pub(crate) async fn check_docker() -> (&'static str, String) {
    // Try `docker info` first, then `podman info`
    for cmd_name in &["docker", "podman"] {
        let probe = tokio::process::Command::new(cmd_name)
            .arg("info")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Drop the child if the timeout below wins, so a stuck daemon
            // does not leave a `docker info` process behind on every probe.
            .kill_on_drop(true)
            .output();
        let result = tokio::time::timeout(CONTAINER_RUNTIME_PROBE_TIMEOUT, probe).await;

        match result {
            Ok(Ok(out)) if out.status.success() => {
                return ("pass", format!("{cmd_name} daemon is running"));
            }
            Ok(Ok(_)) => {
                return (
                    "warn",
                    format!("{cmd_name} found but daemon is not running"),
                );
            }
            Err(_elapsed) => {
                return (
                    "warn",
                    format!(
                        "{cmd_name} info did not answer within {}s — the daemon appears hung; restart the container runtime",
                        CONTAINER_RUNTIME_PROBE_TIMEOUT.as_secs()
                    ),
                );
            }
            Ok(Err(_)) => {} // binary not found — try next
        }
    }

    (
        "warn",
        "No container runtime (docker/podman) found in PATH".to_string(),
    )
}
