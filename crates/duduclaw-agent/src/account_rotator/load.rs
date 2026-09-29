//! Free helpers behind the load path: cooldown maths, OAuth-session
//! detection, secret resolution, the provider env table and the
//! `account_pool` narrowing. Moved verbatim out of `account_rotator.rs`.

use super::*;

/// Next cooldown deadline after a probe confirms the credential is dead:
/// double whatever is left, capped at 6 h.
///
/// An expired / absent cooldown has nothing to double, so it restarts at the
/// [`AUTH_DEAD_BASE_MINUTES`] base — doubling zero would hand the account
/// straight back to the next tick, which is the loop this whole change exists
/// to break.
pub(super) fn doubled_cooldown(current: Option<DateTime<Utc>>) -> DateTime<Utc> {
    let now = Utc::now();
    let next = match current
        .map(|cd| cd - now)
        .filter(|d| *d > chrono::Duration::zero())
    {
        Some(remaining) => remaining
            .checked_mul(2)
            .unwrap_or_else(|| chrono::Duration::minutes(AUTH_DEAD_CAP_MINUTES)),
        None => chrono::Duration::minutes(AUTH_DEAD_BASE_MINUTES),
    };
    now + next.min(chrono::Duration::minutes(AUTH_DEAD_CAP_MINUTES))
}

// ── OAuth helpers ───────────────────────────────────────────

/// Whether the Anthropic host-login auto-detect should still run for the
/// loaded pool: yes unless an **Anthropic** OAuth account is already
/// configured. Foreign-provider OAuth seats (copilot / qwen / codex from
/// `duduclaw auth device`) do not count — they serve a different provider
/// pool and must never mask the missing Anthropic session (pure, testable).
pub(super) fn should_autodetect_anthropic_oauth(loaded: &[Account]) -> bool {
    !loaded
        .iter()
        .any(|a| a.auth_method == AuthMethod::OAuth && a.provider == "anthropic")
}

/// Detect the default OAuth session via `claude auth status`.
///
/// Works with all Claude Code versions — does not depend on `.credentials.json`
/// which no longer exists in recent versions. The `claude` CLI manages its own
/// auth state (OS keychain / internal storage).
///
/// ## Two different sessions look identical to `claude auth status`
///
/// `loggedIn: true` is reported both when the CLI found a keychain session
/// **and** when it merely read `CLAUDE_CODE_OAUTH_TOKEN` out of the ambient
/// environment (the `setup-token` flow every container deployment uses).
///
/// Before the P3 env scrub those two were interchangeable here, because a
/// spawned child inherited the gateway's environment and found the token by
/// itself. Since v1.61.0 the spawn environment is an allowlist that
/// deliberately drops `*_TOKEN`, so an account carrying neither `oauth_token`
/// nor a usable keychain leaves the child with no credential at all —
/// every dispatch dies as `authentication_failed`, while a manual
/// `claude -p` in the same container still works (it *does* inherit the env).
///
/// So: when the session came from the env var, capture that token on the
/// account. `build_env_for` then injects it explicitly, which is exactly what
/// the scrub intends — credentials travel as data, not as ambient state.
pub(super) fn detect_default_oauth_session() -> Option<Account> {
    let claude = duduclaw_core::which_claude()?;
    let claude_dir = dirs::home_dir()?.join(".claude");
    // Captured before the probe so a token-derived session is never mistaken
    // for a keychain one.
    let env_token = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty());

    let output = duduclaw_core::platform::command_for(&claude)
        .args(["auth", "status"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;

    let logged_in = json.get("loggedIn").and_then(|v| v.as_bool()).unwrap_or(false);
    if !logged_in {
        return None;
    }

    let subscription = json
        .get("subscriptionType")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let email = json
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    info!(subscription, email, "OAuth session detected via `claude auth status`");

    Some(Account {
        id: "oauth-default".to_string(),
        auth_method: AuthMethod::OAuth,
        provider: "anthropic".to_string(),
        priority: 1, // OAuth preferred over API key
        monthly_budget_cents: 0,
        tags: Vec::new(),
        profile: "default".to_string(),
        email: email.to_string(),
        subscription: subscription.to_string(),
        label: if env_token.is_some() {
            "setup-token".to_string()
        } else {
            "本機登入".to_string()
        },
        expires_at: None, // OS keychain manages token lifecycle
        api_key: String::new(),
        // `Some` ⇒ inject explicitly (setup-token deployments); `None` ⇒ let the
        // CLI read its own keychain via `credentials_dir`.
        oauth_token: env_token,
        credentials_dir: Some(claude_dir),
        is_healthy: true,
        consecutive_errors: 0,
        spent_this_month: 0,
        cooldown_until: None,
        last_used: None,
        total_requests: 0,
        // `claude auth status` said `loggedIn: true` — which, per this
        // function's own doc comment, proves only that *some* session exists.
        // Unverified until a real request (or a real probe) succeeds.
        credential_state: CredentialState::Unverified,
        auth_dead_strikes: 0,
        next_probe_at: None,
        probe_failures: 0,
    })
}

/// Resolve OAuth credentials directory for a named profile.
///
/// Modern Claude CLI versions no longer use `.credentials.json` — auth is
/// managed via OS keychain / internal storage. We check for the directory
/// itself (which still exists) and fall back to `.credentials.json` for
/// older versions.
pub(super) fn resolve_oauth_credentials(profile: &str) -> Option<PathBuf> {
    let claude_dir = dirs::home_dir()?.join(".claude");

    let dir = if profile == "default" || profile.is_empty() {
        claude_dir.clone()
    } else {
        claude_dir.join("profiles").join(profile)
    };

    if !dir.exists() {
        return None;
    }

    // Accept if directory exists — modern CLI manages auth internally.
    // Legacy check (.credentials.json) is subsumed: if the file exists,
    // the directory also exists.
    Some(dir)
}

// ── API Key helpers ─────────────────────────────────────────

/// Load the `[secret_manager]` config from a top-level config table.
///
/// `table` here is a sub-table (e.g. `[api]` or an `[[accounts]]` entry), so we
/// cannot read `[secret_manager]` from it directly. The rotator only has the
/// per-account table at the call sites, not the full config, so we re-read the
/// top-level config to recover `[secret_manager]`. Absent / malformed →
/// `Default` (backend `local`), matching the gateway's fail-safe behavior.
async fn load_secret_manager_config(home_dir: &Path) -> SecretManagerConfig {
    let config_path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(_) => return SecretManagerConfig::default(),
    };
    content
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| {
            t.get("secret_manager")
                .cloned()
                .and_then(|v| v.try_into().ok())
        })
        .unwrap_or_default()
}

/// Encrypted-field names an `[[accounts]] type = "api_key"` entry may carry.
/// Mirrors `resolve_api_key`'s `*_enc` precedence list.
pub(super) const API_KEY_ENC_FIELDS: &[&str] = &["anthropic_api_key_enc", "api_key_enc"];

/// Encrypted-field name an `[[accounts]] type = "oauth"` entry may carry.
pub(super) const OAUTH_TOKEN_ENC_FIELDS: &[&str] = &["oauth_token_enc"];

/// Whether the entry declares at least one of `fields` with a non-empty value.
///
/// The D4 detector's precondition: "the operator stored a credential here".
/// A field that is absent (or present but blank) is *not* a broken credential
/// — an OAuth entry with no `oauth_token_enc` legitimately relies on the OS
/// keychain, and must keep behaving exactly as before.
pub(super) fn has_nonempty_field(table: &toml::Table, fields: &[&str]) -> bool {
    fields.iter().any(|name| {
        table
            .get(*name)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    })
}

/// Resolve an `[[accounts]]` entry's OAuth token from a TOML table.
///
/// WP-8A: goes through the shared [`SecretRef`] resolver instead of a
/// hand-rolled "decrypt keyfile, else resolve secret:// reference" pair that
/// duplicated `SecretRef`'s own logic.
///
/// Precedence:
/// 1. inline `oauth_token_enc` (decrypted via the per-machine keyfile)
/// 2. `oauth_token` plaintext that is a `secret://` reference
///
/// A non-reference plaintext `oauth_token` is intentionally NOT consumed
/// (preserving prior behavior, which only ever read `oauth_token_enc`) — the
/// plaintext candidate passed to [`SecretRef::classify`] is pre-filtered to
/// `None` unless it is itself a `secret://` reference, so a bare plaintext
/// token can never be picked up through this path.
pub(super) async fn resolve_oauth_token(home_dir: &Path, table: &toml::Table) -> Option<String> {
    let enc = table.get("oauth_token_enc").and_then(|v| v.as_str());
    let plain = table
        .get("oauth_token")
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with("secret://"));
    if enc.is_none() && plain.is_none() {
        return None;
    }
    let sm_cfg = load_secret_manager_config(home_dir).await;
    SecretRef::classify(enc, plain)
        .resolve(&sm_cfg, home_dir)
        .await
        .map(|s| s.expose_owned())
}

/// Resolve API key from a TOML table.
///
/// WP-8A: goes through the shared [`SecretRef`] resolver (credentials
/// doctrine) instead of a hand-rolled "decrypt keyfile, else resolve
/// secret:// reference, else use literally" chain — this was the last of the
/// account_rotator dialects listed in `DESIGN-credentials-doctrine-2026-08.md`
/// §1.1 as reading `secret://` itself rather than sharing the canonical
/// classifier.
///
/// Resolution precedence (unchanged from before this consolidation, since
/// `anthropic_api_key_enc` / `api_key_enc` are two *alternative field names*
/// for the same slot, not an enc/plain pair — encrypted always wins over
/// plaintext regardless of which name holds it):
/// 1. Inline `*_enc` (decrypted via the per-machine keyfile) — tries
///    `anthropic_api_key_enc` then `api_key_enc`.
/// 2. A plaintext field that is a `secret://<backend>/<name>` reference →
///    resolved through the configured secret backend — tries
///    `anthropic_api_key` then `api_key`.
/// 3. A plaintext field used as-is (legacy / dev) — same two names.
pub(super) async fn resolve_api_key(home_dir: &Path, table: &toml::Table) -> String {
    let sm_cfg = load_secret_manager_config(home_dir).await;

    for key_name in &["anthropic_api_key_enc", "api_key_enc"] {
        let enc = table.get(*key_name).and_then(|v| v.as_str());
        if let Some(secret) = SecretRef::classify(enc, None)
            .resolve(&sm_cfg, home_dir)
            .await
        {
            return secret.expose_owned();
        }
    }
    for key_name in &["anthropic_api_key", "api_key"] {
        let plain = table.get(*key_name).and_then(|v| v.as_str());
        if let Some(p) = plain
            && !p.is_empty()
            && !p.starts_with("secret://")
        {
            warn!("Using plaintext API key — run `duduclaw onboard` to encrypt");
        }
        if let Some(secret) = SecretRef::classify(None, plain)
            .resolve(&sm_cfg, home_dir)
            .await
        {
            return secret.expose_owned();
        }
        // A `secret://` reference that failed to resolve falls through to
        // the next key name (treated as unset), matching prior behavior.
    }
    String::new()
}

// ── Provider env-var map ────────────────────────────────────

/// Standard environment-variable name(s) for a provider's API key.
///
/// Thin delegate to `duduclaw_core::provider_env::provider_env_key_names`, the
/// single source of truth for this table (WP-8B, `commercial/docs/DESIGN-credentials-doctrine-2026-08.md`
/// §3 P3). `duduclaw-agent` already depends on `duduclaw-core` (see
/// `Cargo.toml`), so there is no new dependency edge — this used to be a third
/// hand-copied table that had already drifted in comments from the canonical
/// one; behavior (match arms) was verified byte-identical before collapsing.
/// The FIRST name is the canonical one emitted onto a subprocess; the
/// remaining names are accepted aliases when *reading* an env-var fallback
/// value. Unknown providers → empty slice.
pub(super) fn provider_env_key_names(provider: &str) -> &'static [&'static str] {
    duduclaw_core::provider_env::provider_env_key_names(provider)
}

/// Human-facing catalogue of consumer subscription sources the rotator can
/// carry as OAuth pool members (`provider` id → display label).
///
/// Descriptive metadata for status / validation surfaces only — it does NOT
/// gate rotation (any `provider` string is accepted on an account). Codex
/// (`openai`) is live through the Codex runtime's host-login inheritance; the
/// remaining entries are PENDING-LIVE on provider-specific device-code flows.
pub fn known_subscription_providers() -> &'static [(&'static str, &'static str)] {
    &[
        ("anthropic", "Claude Pro/Max"),
        ("openai", "ChatGPT (Codex)"),
        ("github", "GitHub Copilot"),
        ("qwen", "Qwen Portal"),
    ]
}

// ── Account-pool matching (agent.toml [model] account_pool) ─────────

/// Whether an `account_pool` declaration carries at least one usable entry.
///
/// Blank / whitespace-only entries are ignored so a config like
/// `account_pool = ["", "  "]` behaves as "unset" rather than as a pool that
/// matches nothing (which would fail-open anyway, but with a misleading warn).
pub(crate) fn has_pool_entries(pool: &[String]) -> bool {
    pool.iter().any(|p| !p.trim().is_empty())
}

/// Result of narrowing a candidate set by an agent's `account_pool`.
///
/// Split out as a pure decision so the fail-open rule is unit-testable without
/// a rotator, a config file, or a tracing subscriber.
#[derive(Debug)]
pub(crate) enum PoolNarrowing<'a> {
    /// No pool declared (or only blank entries) — candidate set untouched.
    NotRequested,
    /// The pool matched at least one available account; rotate over these.
    Applied(Vec<&'a Account>),
    /// The pool matched no available account. The caller MUST keep the full
    /// candidate set (a stale pool must never brick an agent) and log a warn.
    FailedOpen,
}

/// Narrow `available` to the accounts named by `pool` (see [`PoolNarrowing`]).
///
/// Pure: no I/O, no logging, no rotator state. Applied *before* the rotation
/// strategy runs, so Priority / LeastCost / Failover / RoundRobin keep their
/// exact semantics over the narrowed set.
pub(crate) fn narrow_by_pool<'a>(available: &[&'a Account], pool: &[String]) -> PoolNarrowing<'a> {
    if available.is_empty() || !has_pool_entries(pool) {
        return PoolNarrowing::NotRequested;
    }
    let filtered: Vec<&Account> = available
        .iter()
        .copied()
        .filter(|a| account_in_pool(a, pool))
        .collect();
    if filtered.is_empty() {
        PoolNarrowing::FailedOpen
    } else {
        PoolNarrowing::Applied(filtered)
    }
}

/// Whether `account` is named by the agent's `account_pool`.
///
/// Matching is **exact** (after trimming, ASCII-case-insensitive) against the
/// account `id` and the user-visible `label` — operators reference either one,
/// since the dashboard picker shows the label. Deliberately NOT a substring
/// test (project convention 2: no unanchored `contains` for routing decisions);
/// `word_contains_ci`-style fuzziness would let a pool entry `main` capture an
/// unrelated `main-backup` account.
pub(crate) fn account_in_pool(account: &Account, pool: &[String]) -> bool {
    pool.iter().any(|entry| {
        let entry = entry.trim();
        if entry.is_empty() {
            return false;
        }
        if entry.eq_ignore_ascii_case(account.id.trim()) {
            return true;
        }
        let label = account.label.trim();
        !label.is_empty() && entry.eq_ignore_ascii_case(label)
    })
}

/// Build the subprocess env vars + direct-API metadata for a selected account.
///
/// Anthropic emission is unchanged from the original inline logic (API key vs.
/// OAuth token vs. keychain `CLAUDE_CONFIG_DIR`). Non-Anthropic providers emit
/// the provider's canonical key env var instead, and every API-key account
/// additionally exposes its raw key on `AccountEnv.raw_key`.
pub(super) fn build_account_env(a: &Account) -> AccountEnv {
    let mut env_vars = HashMap::new();
    let mut raw_key = None;
    let mut seat_token = None;

    if a.provider == "anthropic" {
        match a.auth_method {
            AuthMethod::ApiKey => {
                env_vars.insert("ANTHROPIC_API_KEY".to_string(), a.api_key.clone());
                if !a.api_key.is_empty() {
                    raw_key = Some(a.api_key.clone());
                }
            }
            AuthMethod::OAuth => {
                if let Some(ref token) = a.oauth_token {
                    // setup-token account: inject token via env var
                    env_vars.insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), token.clone());
                } else if let Some(dir) = &a.credentials_dir {
                    // OS keychain account: only set CLAUDE_CONFIG_DIR when it differs
                    // from the default `~/.claude`.
                    //
                    // CRITICAL: setting `CLAUDE_CONFIG_DIR=~/.claude` explicitly —
                    // even with the SAME value as the default — makes `claude` CLI
                    // stop looking at the OS keychain for credentials, producing
                    // "Not logged in · Please run /login" for every call. The CLI
                    // only uses the keychain when no `CLAUDE_CONFIG_DIR` is set.
                    //
                    // Leave the env var unset for the default session so claude
                    // CLI picks up keychain auth normally. Non-default profile
                    // directories (e.g. `~/.claude/profiles/work`) still get the
                    // env var because they need explicit pointing.
                    let is_default_home = dirs::home_dir()
                        .map(|h| h.join(".claude"))
                        .is_some_and(|default_dir| default_dir == *dir);
                    if !is_default_home {
                        env_vars.insert(
                            "CLAUDE_CONFIG_DIR".to_string(),
                            dir.to_string_lossy().to_string(),
                        );
                    }
                }
                // Ensure API key doesn't override OAuth
                env_vars.insert("ANTHROPIC_API_KEY".to_string(), String::new());
            }
        }
    } else {
        // Non-Anthropic provider.
        match a.auth_method {
            AuthMethod::ApiKey => {
                // Emit the provider's canonical env var so a subprocess sees the
                // right variable, and expose the raw key for direct-API callers.
                if let Some(name) = provider_env_key_names(&a.provider).first() {
                    env_vars.insert((*name).to_string(), a.api_key.clone());
                }
                if !a.api_key.is_empty() {
                    raw_key = Some(a.api_key.clone());
                }
            }
            AuthMethod::OAuth => {
                // Subscription OAuth for a non-Anthropic provider (ChatGPT Codex
                // / GitHub Copilot / Qwen Portal). Token acquisition + injection
                // is runtime-specific:
                //   - Codex (openai): inherits the host ChatGPT login — nothing
                //     to inject here (the runtime already sees it).
                //   - Copilot / Qwen: PENDING-LIVE (device-code flows need
                //     provider-specific credentials we do not fabricate).
                // We deliberately do NOT invent env-var names for tokens we
                // cannot verify, and do NOT expose the seat token as `raw_key`
                // (it is a subscription seat, not an API key — a direct-API
                // caller must not treat it as one). The account remains a
                // first-class rotation member: `provider` is carried below.
                //
                // When a persisted seat credential IS present (e.g. a GitHub
                // OAuth token minted by `duduclaw auth device --provider
                // copilot`, decrypted from `oauth_token_enc` at load time), it
                // is surfaced on `seat_token` so `duduclaw proxy` can exchange
                // it for a short-lived upstream token and forward the seat.
                if let Some(ref token) = a.oauth_token {
                    if !token.is_empty() {
                        seat_token = Some(token.clone());
                    }
                }
            }
        }
    }

    AccountEnv {
        id: a.id.clone(),
        auth_method: a.auth_method.clone(),
        provider: a.provider.clone(),
        raw_key,
        seat_token,
        env_vars,
    }
}

/// Synthesize a single ephemeral API-key selection from a provider's standard
/// env var, used when the config declares no accounts for that provider.
///
/// Returns `None` when the provider is unknown or its env var is unset/empty.
/// The ephemeral id (`<provider>-env`) intentionally does not correspond to any
/// stored account, so `on_success`/`on_error` for it are harmless no-ops — the
/// single ephemeral account has no persistent budget/cooldown state to track.
pub(super) fn env_fallback_account_env(provider: &str) -> Option<AccountEnv> {
    let names = provider_env_key_names(provider);
    let emit_name = *names.first()?;
    let key = names
        .iter()
        .filter_map(|n| std::env::var(n).ok())
        .find(|v| !v.is_empty())?;

    let mut env_vars = HashMap::new();
    env_vars.insert(emit_name.to_string(), key.clone());
    info!(
        provider,
        "No configured accounts for provider — using ephemeral env-var account"
    );
    Some(AccountEnv {
        id: format!("{provider}-env"),
        auth_method: AuthMethod::ApiKey,
        provider: provider.to_string(),
        raw_key: Some(key),
        seat_token: None,
        env_vars,
    })
}

/// Create a rotator from config.toml rotation settings.
pub fn create_from_config(config: &toml::Table) -> AccountRotator {
    let rotation = config.get("rotation").and_then(|v| v.as_table());
    let strategy_str = rotation.and_then(|r| r.get("strategy")).and_then(|v| v.as_str()).unwrap_or("priority");
    let cooldown = rotation.and_then(|r| r.get("cooldown_after_rate_limit_seconds")).and_then(|v| v.as_integer()).unwrap_or(120) as u64;
    AccountRotator::new(RotationStrategy::from_str(strategy_str), cooldown)
}
