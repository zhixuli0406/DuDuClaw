use super::*;

/// Read an agent's config from disk.
pub(crate) async fn read_agent_config(
    agents_dir: &Path,
    agent_id: &str,
) -> Option<duduclaw_core::types::AgentConfig> {
    let toml_path = agents_dir.join(agent_id).join("agent.toml");
    let content = tokio::fs::read_to_string(&toml_path).await.ok()?;
    toml::from_str(&content).ok()
}

pub(crate) async fn resolve_main_agent_name(home_dir: &Path) -> String {
    let agents_dir = home_dir.join("agents");
    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let toml_path = path.join("agent.toml");
        if let Ok(content) = tokio::fs::read_to_string(&toml_path).await
            && let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content)
            && config.agent.role == duduclaw_core::types::AgentRole::Main
        {
            return config.agent.name;
        }
    }

    String::new()
}

// ── Config reader ────────────────────────────────────────────

pub(crate) async fn read_config(home_dir: &Path) -> Option<toml::Table> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    content.parse().ok()
}

/// Decrypt a channel token from `config.toml`'s `[channels]` table.
///
/// WP-8C: previously a hand-rolled "try `_enc`, else plaintext, else resolve
/// `secret://…`" reader that duplicated (and had drifted from)
/// [`duduclaw_security::secret_ref::SecretRef`] — the shared classifier the
/// rest of the codebase (`duduclaw-gateway::config_crypto`, which reads this
/// exact `[channels] telegram_bot_token(_enc)` / `discord_bot_token(_enc)`
/// shape for the real channel bots) already resolves through. `field_base` is
/// the plaintext key name (e.g. `"telegram_bot_token"`); the `_enc` twin is
/// derived from it, matching the config-file convention `SecretRef::classify`
/// itself documents.
///
/// Behaviour is unchanged: `_enc` ciphertext wins, falls back to the
/// plaintext field, and a `secret://<backend>/<name>` value in either field
/// (including network-backed backends) resolves instead of being returned as
/// a literal. Returns `""` when nothing is configured or resolvable —
/// matches the pre-existing "empty means unset" contract callers already
/// check for.
pub(crate) async fn decrypt_channel_token(config: &toml::Table, field_base: &str, home_dir: &Path) -> String {
    let channels = config.get("channels").and_then(|c| c.as_table());
    let enc_field = format!("{field_base}_enc");
    let secret_ref = SecretRef::classify(
        channels
            .and_then(|c| c.get(&enc_field))
            .and_then(|v| v.as_str()),
        channels
            .and_then(|c| c.get(field_base))
            .and_then(|v| v.as_str()),
    );
    let sm_cfg: duduclaw_security::secret_manager::SecretManagerConfig = config
        .get("secret_manager")
        .cloned()
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default();
    secret_ref
        .resolve(&sm_cfg, home_dir)
        .await
        .map(Secret::expose_owned)
        .unwrap_or_default()
}

#[cfg(test)]
mod decrypt_channel_token_tests {
    use super::*;

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "duduclaw-mcp-decrypttest-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        /// Encrypt `plain` with a fresh (or existing) per-home keyfile, mirroring
        /// how the gateway's real `_enc` write path produces ciphertext.
        fn encrypt(&self, plain: &str) -> String {
            let keyfile = self.0.join(".keyfile");
            let key = if keyfile.exists() {
                let bytes = std::fs::read(&keyfile).unwrap();
                let mut k = [0u8; 32];
                k.copy_from_slice(&bytes);
                k
            } else {
                let k = duduclaw_security::crypto::CryptoEngine::generate_key().unwrap();
                std::fs::write(&keyfile, k).unwrap();
                k
            };
            duduclaw_security::crypto::CryptoEngine::new(&key)
                .unwrap()
                .encrypt_string(plain)
                .unwrap()
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn channels_table(pairs: &[(&str, &str)]) -> toml::Table {
        let mut channels = toml::Table::new();
        for (k, v) in pairs {
            channels.insert((*k).to_string(), toml::Value::String((*v).to_string()));
        }
        let mut root = toml::Table::new();
        root.insert("channels".to_string(), toml::Value::Table(channels));
        root
    }

    /// `_enc` round-trip: ciphertext decrypts to the original token.
    #[tokio::test]
    async fn enc_field_round_trips() {
        let home = TempHome::new("enc-roundtrip");
        let enc = home.encrypt("tg-secret-token");
        let table = channels_table(&[("telegram_bot_token_enc", &enc)]);
        let got = decrypt_channel_token(&table, "telegram_bot_token", home.path()).await;
        assert_eq!(got, "tg-secret-token");
    }

    /// Plaintext fallback: no `_enc` field, plain field wins as-is.
    #[tokio::test]
    async fn plaintext_fallback_when_no_enc_field() {
        let home = TempHome::new("plaintext-fallback");
        let table = channels_table(&[("discord_bot_token", "plain-discord-token")]);
        let got = decrypt_channel_token(&table, "discord_bot_token", home.path()).await;
        assert_eq!(got, "plain-discord-token");
    }

    /// A `secret://` reference must never be handed back as the literal
    /// string — a network-backed reference with no manager configured fails
    /// closed to empty rather than leaking the URI as if it were the token.
    #[tokio::test]
    async fn secret_reference_never_returned_as_literal_token() {
        let home = TempHome::new("secret-ref-not-literal");
        let table = channels_table(&[("telegram_bot_token", "secret://vault/telegram_bot_token")]);
        let got = decrypt_channel_token(&table, "telegram_bot_token", home.path()).await;
        assert!(
            got.is_empty(),
            "expected fail-closed empty string, got literal: {got:?}"
        );
        assert_ne!(got, "secret://vault/telegram_bot_token");
    }

    /// A `secret://env/…` reference (local, no network) does resolve.
    #[tokio::test]
    async fn secret_env_reference_resolves() {
        let home = TempHome::new("secret-env-resolves");
        let var = format!(
            "DUDUCLAW_MCP_DECRYPT_TEST_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "from-env-token") };
        let table = channels_table(&[("telegram_bot_token", &format!("secret://env/{var}"))]);
        let got = decrypt_channel_token(&table, "telegram_bot_token", home.path()).await;
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got, "from-env-token");
    }

    /// Ciphertext wins over a plaintext twin sitting next to it.
    #[tokio::test]
    async fn enc_field_wins_over_plaintext_twin() {
        let home = TempHome::new("enc-wins");
        let enc = home.encrypt("encrypted-wins");
        let table = channels_table(&[
            ("telegram_bot_token_enc", &enc),
            ("telegram_bot_token", "plaintext-loses"),
        ]);
        let got = decrypt_channel_token(&table, "telegram_bot_token", home.path()).await;
        assert_eq!(got, "encrypted-wins");
    }

    /// Nothing configured at all → empty string, not a panic or an error.
    #[tokio::test]
    async fn nothing_configured_is_empty() {
        let home = TempHome::new("nothing-configured");
        let table = channels_table(&[]);
        let got = decrypt_channel_token(&table, "telegram_bot_token", home.path()).await;
        assert_eq!(got, "");
    }
}

/// Resolve the caller-identity agent name for MCP authorization.
///
/// Preference order (highest → lowest):
/// 1. `DUDUCLAW_AGENT_ID` env var — injected per-agent via `.mcp.json` so
///    the MCP subprocess knows which agent's Claude CLI spawned it. This
///    is the authoritative source: the WP21 delegation gate
///    (`check_delegation_allowed`) and the org-placement gate
///    (`check_org_placement_allowed`) both judge this identity against the
///    target's `reports_to` chain and department.
/// 2. `config.toml [general] default_agent` — legacy fallback, kept for
///    backwards compatibility with installs whose `.mcp.json` hasn't yet
///    been migrated to include the env var (see
///    `duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path`).
/// 3. Hard-coded "dudu" — final fallback for fresh installs with neither
///    env nor config set.
///
/// An empty `DUDUCLAW_AGENT_ID` (e.g. `"env": { "DUDUCLAW_AGENT_ID": "" }`)
/// is treated as missing and falls through to the config lookup — this
/// prevents accidental lockout if a stale migration produced an empty
/// string.
///
/// # WP21 debt ⑧ — the id is now checkable
///
/// Step 1 above was an *unauthenticated assertion*: an agent with a shell could
/// spawn its own `duduclaw mcp-server` with `DUDUCLAW_AGENT_ID=ceo` and every
/// gate downstream would believe it. [`duduclaw_core::identity_token`] adds a
/// MAC (`DUDUCLAW_AGENT_TOKEN`) over the claim. This function consumes the
/// verdict:
///
/// | `identity.key` | token | `[delegation] require_identity_token` | result |
/// |---|---|---|---|
/// | absent | — | — | unchanged (feature not enabled) |
/// | present | valid | — | unchanged, now *proven* |
/// | present | missing/invalid | `false` (default) | unchanged + one `warn!` per process |
/// | present | missing/invalid | `true` | [`duduclaw_core::UNTRUSTED_AGENT_ID`] |
///
/// The sentinel is not an agent id, is nobody's ancestor, shares no department
/// and is not a system sender, so every WP21 gate denies it without needing to
/// know it exists — the fail-closed property survives gates written later.
/// `run_mcp_server` additionally refuses to boot on that verdict, so a
/// misconfigured deployment fails loudly instead of running an agent that
/// silently cannot do anything.
///
/// A system-sender name (`dashboard`, `cron`, `goal-loop-driver`, …) is never
/// a process identity: every point that starts an MCP server for an employee
/// stamps an agent directory id, and those names are reserved at agent
/// creation (`duduclaw_core::is_reserved_agent_id`). Only a self-asserted
/// identity can carry one, and it would inherit the system senders'
/// unconditional delegation reach, so it resolves to the untrusted sentinel.
pub async fn get_default_agent(home_dir: &Path) -> String {
    if caller_identity_verdict(home_dir) == duduclaw_core::IdentityVerdict::Rejected {
        return duduclaw_core::UNTRUSTED_AGENT_ID.to_string();
    }

    let (resolved, source) = if let Ok(env_id) = std::env::var(duduclaw_core::ENV_AGENT_ID)
        && !env_id.trim().is_empty()
    {
        (env_id, "env")
    } else {
        let config = read_config(home_dir).await;
        let resolved = config
            .as_ref()
            .and_then(|t| t.get("general"))
            .and_then(|g| g.get("default_agent"))
            .and_then(|v| v.as_str())
            .unwrap_or("dudu")
            .to_string();
        (resolved, "config")
    };
    if duduclaw_core::is_system_sender(&resolved)
        || duduclaw_core::is_reserved_queue_sender(&resolved)
    {
        audit_system_sender_identity_once(home_dir, resolved.trim(), source);
        return duduclaw_core::UNTRUSTED_AGENT_ID.to_string();
    }
    resolved
}

/// Record, once per process per home, that `get_default_agent` replaced a
/// claimed system-sender identity with the untrusted sentinel. Everything
/// downstream (refusals, audit rows) only sees `__untrusted__`, so this row is
/// the one place that says what was claimed and where it came from (`env` =
/// `DUDUCLAW_AGENT_ID`, `config` = `[general] default_agent`). `claimed` is
/// always one of the fixed `SYSTEM_SENDERS` / `RESERVED_QUEUE_SENDERS`
/// names, never free text.
fn audit_system_sender_identity_once(home_dir: &Path, claimed: &str, source: &str) {
    static SEEN: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());
    {
        let Ok(mut seen) = SEEN.lock() else { return };
        if seen.iter().any(|h| h == home_dir) {
            return;
        }
        seen.push(home_dir.to_path_buf());
    }
    tracing::warn!(
        claimed = %claimed,
        source,
        "MCP process identity is a system-sender name — treated as untrusted"
    );
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        duduclaw_core::UNTRUSTED_AGENT_ID,
        "mcp_identity",
        &format!("denied: process identity '{claimed}' (from {source}) is a system-sender name"),
        false,
        &[
            ("reason", serde_json::json!("system_sender_identity")),
            ("claimed", serde_json::json!(claimed)),
            ("source", serde_json::json!(source)),
        ],
    );
}

/// Verify the ambient `DUDUCLAW_AGENT_ID` / `DUDUCLAW_AGENT_TOKEN` pair against
/// `<home>/identity.key`, under `config.toml [delegation]
/// require_identity_token`.
///
/// Thin wrapper so the strictness lookup and the verification live at one call
/// site; both are cheap file reads and `get_default_agent` runs a handful of
/// times per process, not per tool call.
pub fn caller_identity_verdict(home_dir: &Path) -> duduclaw_core::IdentityVerdict {
    let require = duduclaw_core::require_identity_token_from_home(home_dir);
    duduclaw_core::verify_env_identity(home_dir, require)
}

// ── Main server loop ─────────────────────────────────────────
