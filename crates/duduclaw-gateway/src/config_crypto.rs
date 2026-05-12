//! Shared utilities for reading encrypted config fields.
//!
//! Provides a single `decrypt_config_field()` function used by all channel
//! bots and handlers to read tokens from `config.toml`, trying the encrypted
//! `_enc` field first and falling back to plaintext for backwards compatibility.

use std::path::Path;

/// Load the AES-256 keyfile from `~/.duduclaw/.keyfile`.
/// Used by GVU encryption, the ObservationFinalizer CLI, and other internal
/// consumers that need to talk to the same VersionStore as the gateway.
pub fn load_keyfile_public(home_dir: &Path) -> Option<[u8; 32]> {
    load_keyfile(home_dir)
}

fn load_keyfile(home_dir: &Path) -> Option<[u8; 32]> {
    let keyfile = home_dir.join(".keyfile");
    let bytes = std::fs::read(&keyfile).ok()?;
    if bytes.len() == 32 {
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Some(key)
    } else {
        tracing::warn!(
            path = %keyfile.display(),
            actual_len = bytes.len(),
            "Keyfile has incorrect length (expected 32 bytes) — encryption disabled"
        );
        None
    }
}

/// Load the keyfile, **generating a fresh one if absent**. Used on the
/// encrypt-write path — a brand-new client that has never run the CLI
/// init flow may not have `.keyfile` yet, and a write of a new credential
/// should not fail merely because of that. Decrypt paths intentionally
/// stay read-only via [`load_keyfile`] so a missing keyfile never silently
/// destroys an existing ciphertext.
///
/// Returns `None` only when keyfile creation itself fails (RNG failure,
/// disk full, permission denied) — those cases are logged at `error!`.
fn load_or_create_keyfile(home_dir: &Path) -> Option<[u8; 32]> {
    if let Some(key) = load_keyfile(home_dir) {
        return Some(key);
    }

    // Ensure home_dir exists so the write below can succeed on a fresh install.
    if !home_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(home_dir) {
            tracing::error!(
                path = %home_dir.display(),
                "Failed to create DuDuClaw home dir for keyfile: {e}"
            );
            return None;
        }
    }

    let key = match duduclaw_security::crypto::CryptoEngine::generate_key() {
        Ok(k) => k,
        Err(e) => {
            tracing::error!("Failed to generate AES-256 keyfile (RNG failure): {e}");
            return None;
        }
    };

    let keyfile = home_dir.join(".keyfile");
    if let Err(e) = std::fs::write(&keyfile, key) {
        tracing::error!(
            path = %keyfile.display(),
            "Failed to write new keyfile: {e}"
        );
        return None;
    }

    // Tighten permissions to owner-only — best-effort, don't fail the write.
    if let Err(e) = duduclaw_core::platform::set_owner_only(&keyfile) {
        tracing::warn!(
            path = %keyfile.display(),
            "Could not tighten keyfile permissions to owner-only: {e}"
        );
    }

    tracing::info!(path = %keyfile.display(), "Generated new AES-256 keyfile");
    Some(key)
}

/// Decrypt a base64-encoded encrypted value using the per-machine keyfile.
pub(crate) fn decrypt_value(encrypted: &str, home_dir: &Path) -> Option<String> {
    let key = load_keyfile(home_dir).or_else(|| {
        tracing::warn!("Keyfile not found — cannot decrypt config value");
        None
    })?;
    let engine = duduclaw_security::crypto::CryptoEngine::new(&key).ok()?;
    match engine.decrypt_string(encrypted) {
        Ok(plain) if !plain.is_empty() => Some(plain),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("Decryption failed: {e}");
            None
        }
    }
}

/// Encrypt a plaintext value using the per-machine keyfile.
///
/// **Auto-generates the keyfile on first use**: a fresh client install may
/// not yet have `~/.duduclaw/.keyfile`. The first credential write
/// triggers `load_or_create_keyfile`, so the dashboard "Save" flow works
/// even before the user ever invoked the CLI init path.
///
/// Returns `None` only when:
/// - `plaintext` is empty,
/// - the OS RNG cannot produce a key (extremely rare), or
/// - the keyfile cannot be written (disk full / permission denied).
///
/// Decrypt-side helpers stay read-only by design — see [`load_keyfile`].
pub fn encrypt_value(plaintext: &str, home_dir: &Path) -> Option<String> {
    if plaintext.is_empty() { return None; }
    let key = load_or_create_keyfile(home_dir)?;
    let engine = duduclaw_security::crypto::CryptoEngine::new(&key).ok()?;
    engine.encrypt_string(plaintext).ok()
}

/// Resolve a per-agent channel token: try encrypted version first, fallback to plaintext.
///
/// Used by `start_*_bots()` to read tokens from agent.toml `[channels.*]` sections.
pub(crate) fn resolve_agent_token(
    encrypted: &Option<String>,
    plaintext: &str,
    home_dir: &Path,
) -> String {
    if let Some(enc) = encrypted {
        if !enc.is_empty() {
            if let Some(decrypted) = decrypt_value(enc, home_dir) {
                return decrypted;
            }
        }
    }
    plaintext.to_string()
}

/// Read a config field, trying the encrypted version first.
///
/// For example, `decrypt_config_field(table, "channels", "telegram_bot_token", home_dir)`
/// will try `channels.telegram_bot_token_enc` first, decrypt it, and fall back
/// to `channels.telegram_bot_token` if the encrypted field is missing or empty.
pub fn decrypt_config_field(
    table: &toml::Table,
    section: &str,
    field_base: &str,
    home_dir: &Path,
) -> Option<String> {
    let section_table = table.get(section)?.as_table()?;

    // If the plaintext field explicitly exists and is empty, the channel was removed.
    // Respect this even if a stale _enc value remains (defensive against incomplete cleanup).
    if let Some(plain_val) = section_table.get(field_base).and_then(|v| v.as_str()) {
        if plain_val.is_empty() {
            return None;
        }
    }

    // Try encrypted field first
    let enc_field = format!("{field_base}_enc");
    if let Some(enc_val) = section_table.get(&enc_field).and_then(|v| v.as_str()) {
        if !enc_val.is_empty() {
            if let Some(decrypted) = decrypt_value(enc_val, home_dir) {
                return Some(decrypted);
            }
        }
    }

    // Fallback: plaintext field (backwards compatibility)
    let plain = section_table.get(field_base)?.as_str()?;
    if plain.is_empty() { None } else { Some(plain.to_string()) }
}

/// Read a config field from a TOML file, with encryption support.
pub async fn read_encrypted_config_field(
    home_dir: &Path,
    section: &str,
    field_base: &str,
) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    decrypt_config_field(&table, section, field_base, home_dir)
}

// ─── Per-agent channel token + reports_to cascade ───────────

/// Read a single agent's `[channels.<channel>] bot_token(_enc)` from its
/// `agent.toml`. Returns `None` when the file is missing or the agent
/// has no token for that channel.
pub fn read_agent_channel_token(
    home_dir: &Path,
    agent_id: &str,
    channel: &str,
) -> Option<String> {
    let agent_toml = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = std::fs::read_to_string(&agent_toml).ok()?;
    let table: toml::Value = content.parse().ok()?;
    let section = table
        .get("channels")
        .and_then(|c| c.as_table())
        .and_then(|t| t.get(channel))
        .and_then(|v| v.as_table())?;

    // Encrypted form first (bot_token_enc); then plaintext (bot_token).
    if let Some(enc) = section.get("bot_token_enc").and_then(|v| v.as_str()) {
        if !enc.is_empty() {
            if let Some(plain) = decrypt_value(enc, home_dir) {
                return Some(plain);
            }
        }
    }
    section
        .get("bot_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read one agent's `reports_to` field from `agent.toml`.
///
/// Empty string (or missing) is normalized to `None` so callers can
/// detect the chain root cleanly.
fn read_reports_to(home_dir: &Path, agent_id: &str) -> Option<String> {
    let agent_toml = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = std::fs::read_to_string(&agent_toml).ok()?;
    let table: toml::Value = content.parse().ok()?;
    table
        .get("agent")
        .and_then(|a| a.as_table())
        .and_then(|t| t.get("reports_to"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Maximum hops to walk up `reports_to` when resolving a channel token.
/// Real DuDuClaw teams typically stay ≤ 3 levels deep (agent → TL → PM
/// → root); 8 hops is a generous safety cap that also bounds a cyclic
/// configuration where one agent's `reports_to` eventually points back
/// at itself.
const MAX_REPORTS_TO_HOPS: usize = 8;

/// Resolve a channel bot token by walking the `reports_to` chain
/// starting at `agent_id`.
///
/// Returns the first per-agent token found along the chain, or `None`
/// when the chain reaches the root (`reports_to = ""`) without finding
/// one. Callers should fall back to the global
/// `config.toml [channels] <channel>_bot_token(_enc)` in that case.
///
/// ## Why this exists
///
/// Discord threads are bot-scoped: only the bot that opened a thread
/// can post into it. When a cron-fired sub-agent (e.g. `xianwen-pm`)
/// tries to deliver a notification into a thread owned by the team
/// root (`agnes`), falling back to the **global** token — which may be
/// a different bot — yields a 401 Unauthorized even though the bot is
/// in the same guild.
///
/// Walking `reports_to` (xianwen-pm → xianwen-tl → agnes) lets the
/// sub-agent inherit the root's token automatically, matching the
/// hierarchy the user already configured in `agent.toml`.
///
/// ## Cycle + depth safety
///
/// Tracks visited ids in a `HashSet` and bails at `MAX_REPORTS_TO_HOPS`,
/// so a misconfigured loop (`a → b → a`) cannot wedge the resolver.
pub fn resolve_agent_channel_token_via_reports_to(
    home_dir: &Path,
    agent_id: &str,
    channel: &str,
) -> Option<String> {
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current = agent_id.to_string();
    for _ in 0..MAX_REPORTS_TO_HOPS {
        if !visited.insert(current.clone()) {
            // Cycle detected — give up cleanly.
            tracing::warn!(
                agent = %agent_id,
                looped_at = %current,
                "reports_to cycle detected while resolving channel token"
            );
            return None;
        }
        if let Some(tok) = read_agent_channel_token(home_dir, &current, channel) {
            return Some(tok);
        }
        match read_reports_to(home_dir, &current) {
            Some(parent) => current = parent,
            None => return None, // root reached, no token anywhere on the chain
        }
    }
    tracing::warn!(
        agent = %agent_id,
        hops = MAX_REPORTS_TO_HOPS,
        "reports_to chain exceeded max hops while resolving channel token"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("duduclaw-cfgcrypto-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn write_agent(&self, agent: &str, toml_body: &str) {
            let agent_dir = self.0.join("agents").join(agent);
            std::fs::create_dir_all(&agent_dir).unwrap();
            std::fs::write(agent_dir.join("agent.toml"), toml_body).unwrap();
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn agent_toml(name: &str, reports_to: &str, discord_token: Option<&str>) -> String {
        let channels_block = match discord_token {
            Some(tok) => format!(
                "\n[channels.discord]\nbot_token = \"{tok}\"\n"
            ),
            None => String::new(),
        };
        format!(
            "[agent]\nname = \"{name}\"\nreports_to = \"{reports_to}\"\n{channels_block}"
        )
    }

    #[test]
    fn resolves_own_token_when_present() {
        let home = TempHome::new();
        home.write_agent("xianwen-pm", &agent_toml("xianwen-pm", "xianwen-tl", Some("own-token")));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "xianwen-pm", "discord");
        assert_eq!(tok.as_deref(), Some("own-token"));
    }

    #[test]
    fn resolves_parent_token_when_self_empty() {
        let home = TempHome::new();
        home.write_agent("xianwen-pm", &agent_toml("xianwen-pm", "xianwen-tl", None));
        home.write_agent("xianwen-tl", &agent_toml("xianwen-tl", "agnes", None));
        home.write_agent("agnes", &agent_toml("agnes", "", Some("agnes-bot-token")));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "xianwen-pm", "discord");
        assert_eq!(tok.as_deref(), Some("agnes-bot-token"));
    }

    #[test]
    fn returns_none_when_chain_has_no_token() {
        let home = TempHome::new();
        home.write_agent("a", &agent_toml("a", "b", None));
        home.write_agent("b", &agent_toml("b", "c", None));
        home.write_agent("c", &agent_toml("c", "", None));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "a", "discord");
        assert!(tok.is_none());
    }

    #[test]
    fn stops_at_first_token_not_farthest_ancestor() {
        // xianwen-pm has no token; xianwen-tl has a token; agnes also has one.
        // Cascade should return xianwen-tl's (the nearest ancestor).
        let home = TempHome::new();
        home.write_agent("xianwen-pm", &agent_toml("xianwen-pm", "xianwen-tl", None));
        home.write_agent("xianwen-tl", &agent_toml("xianwen-tl", "agnes", Some("tl-token")));
        home.write_agent("agnes", &agent_toml("agnes", "", Some("agnes-token")));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "xianwen-pm", "discord");
        assert_eq!(tok.as_deref(), Some("tl-token"));
    }

    #[test]
    fn cycle_detection_returns_none_without_stack_overflow() {
        let home = TempHome::new();
        home.write_agent("a", &agent_toml("a", "b", None));
        home.write_agent("b", &agent_toml("b", "a", None)); // cycle
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "a", "discord");
        assert!(tok.is_none());
    }

    #[test]
    fn missing_agent_toml_returns_none() {
        let home = TempHome::new();
        // No agent files at all.
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "ghost", "discord");
        assert!(tok.is_none());
    }

    #[test]
    fn reports_to_empty_string_is_treated_as_root() {
        let home = TempHome::new();
        home.write_agent("solo", &agent_toml("solo", "", None));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "solo", "discord");
        assert!(tok.is_none());
    }

    #[test]
    fn different_channel_keys_are_independent() {
        // Agent configures only Telegram; Discord lookup should fall through.
        let home = TempHome::new();
        let tg_body = "[agent]\nname = \"x\"\nreports_to = \"\"\n\
                       [channels.telegram]\nbot_token = \"tg-tok\"\n";
        home.write_agent("x", tg_body);
        assert_eq!(
            resolve_agent_channel_token_via_reports_to(home.path(), "x", "telegram").as_deref(),
            Some("tg-tok")
        );
        assert!(resolve_agent_channel_token_via_reports_to(home.path(), "x", "discord").is_none());
    }

    // ─── encrypt_value auto-generates keyfile on fresh install ─────

    #[test]
    fn encrypt_value_generates_keyfile_when_missing() {
        // Fresh home with no .keyfile — encrypt_value should succeed and
        // create the keyfile rather than fail the dashboard save flow.
        let home = TempHome::new();
        assert!(!home.path().join(".keyfile").exists());

        let enc = encrypt_value("hello-world", home.path());
        assert!(enc.is_some(), "encrypt_value should auto-create keyfile");

        let keyfile = home.path().join(".keyfile");
        assert!(keyfile.exists(), "keyfile should now exist on disk");
        let bytes = std::fs::read(&keyfile).unwrap();
        assert_eq!(bytes.len(), 32, "keyfile must be a 32-byte AES-256 key");
    }

    #[test]
    fn encrypt_then_decrypt_round_trips_after_auto_create() {
        let home = TempHome::new();
        let plain = "round-trip-secret";
        let enc = encrypt_value(plain, home.path()).expect("encrypt");
        let dec = decrypt_value(&enc, home.path()).expect("decrypt");
        assert_eq!(dec, plain);
    }

    #[test]
    fn encrypt_value_rejects_empty_plaintext() {
        let home = TempHome::new();
        assert!(encrypt_value("", home.path()).is_none());
        // And no keyfile should be created for an empty input — there is
        // nothing to encrypt, so we should not pollute the home dir.
        assert!(!home.path().join(".keyfile").exists());
    }

    #[test]
    fn second_encrypt_reuses_existing_keyfile() {
        // Two encrypts in a row must share the same key — otherwise an
        // earlier ciphertext stored in config.toml could no longer be
        // decrypted by a subsequent gateway start.
        let home = TempHome::new();
        let _ = encrypt_value("first", home.path()).expect("first encrypt");
        let key1 = std::fs::read(home.path().join(".keyfile")).unwrap();
        let _ = encrypt_value("second", home.path()).expect("second encrypt");
        let key2 = std::fs::read(home.path().join(".keyfile")).unwrap();
        assert_eq!(key1, key2, "keyfile must be stable across encrypts");
    }

    #[test]
    fn decrypt_value_does_not_create_keyfile() {
        // Decrypt is intentionally read-only: a missing keyfile means the
        // ciphertext cannot be recovered, and silently minting a fresh key
        // would permanently lose the data. The function must return None
        // and leave the home dir untouched.
        let home = TempHome::new();
        let result = decrypt_value("Zm9v", home.path()); // any base64 input
        assert!(result.is_none());
        assert!(!home.path().join(".keyfile").exists());
    }

    #[test]
    fn load_or_create_creates_missing_parent_dir() {
        // If the entire DuDuClaw home dir is absent (very fresh install),
        // the helper must mkdir -p before writing the keyfile.
        let parent = std::env::temp_dir()
            .join(format!("duduclaw-keytest-{}", uuid::Uuid::new_v4()));
        // parent does NOT yet exist.
        assert!(!parent.exists());

        let key = load_or_create_keyfile(&parent);
        assert!(key.is_some());
        assert!(parent.join(".keyfile").exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&parent);
    }
}
