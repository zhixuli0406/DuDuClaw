//! Shared utilities for reading encrypted config fields.
//!
//! Provides a single `decrypt_config_field()` function used by all channel
//! bots and handlers to read tokens from `config.toml`, trying the encrypted
//! `_enc` field first and falling back to plaintext for backwards compatibility.
//!
//! ## Credentials doctrine (WP-H1, P0)
//!
//! Every read point in this module now classifies the `<field>_enc` /
//! `<field>` pair through the one shared type,
//! [`duduclaw_security::secret_ref::SecretRef`], and resolves it through that
//! type's resolver. Two things follow:
//!
//! - **`secret://<backend>/<name>` is understood everywhere.** It used to be
//!   honoured only by the async [`read_encrypted_config_field`]; every
//!   synchronous path (per-agent channel tokens, the `reports_to` cascade, the
//!   raw-table reader) handed the URI back verbatim and it was sent to the
//!   vendor API as the bot token. WP-H1 made those paths resolve inline
//!   ciphertext, legacy plaintext and `secret://env/…`, with a reference to a
//!   *network* backend failing closed with a warning there (never a literal).
//!   WP-6C (2026-08) finished the job: every production caller of the
//!   per-agent token / `reports_to` cascade helpers below now runs on the
//!   gateway's tokio runtime and reaches full network-backend resolution
//!   too — [`resolve_agent_token`], [`read_agent_channel_token`], and
//!   [`resolve_agent_channel_token_via_reports_to`] are `async fn`. Only
//!   [`decrypt_config_field`] keeps a synchronous, fail-closed-on-network
//!   twin ([`decrypt_config_field_async`] is its async counterpart) for any
//!   future genuinely-synchronous caller.
//! - **"Empty" is not a value.** Resolvers return `Option<Secret>`, and
//!   `Secret` cannot hold the empty string, so callers no longer each decide
//!   what `""` means.

use std::path::Path;

use duduclaw_security::secret_manager::SecretManagerConfig;
use duduclaw_security::secret_ref::{Secret, SecretRef, SecretStatus};

/// Extract `[secret_manager]` from a config table already sitting in memory.
///
/// Shared by [`decrypt_config_field_async`] and [`read_encrypted_config_field`]
/// so the "how do I turn a parsed table into a `SecretManagerConfig`" step
/// exists exactly once here (the disk-reading half — when a caller has only a
/// `home_dir` and no table yet — is
/// [`duduclaw_security::secret_manager::SecretManagerConfig::load_from_home`]).
fn secret_manager_config_from_table(table: &toml::Table) -> SecretManagerConfig {
    table
        .get("secret_manager")
        .cloned()
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default()
}

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
///
/// Delegates to the canonical read-only primitive
/// [`duduclaw_security::keyfile::decrypt_keyfile_value`] so all crates share
/// one decrypt path. The encrypt/write helpers below keep their own
/// `load_keyfile`-based body (which may *create* a keyfile).
pub(crate) fn decrypt_value(encrypted: &str, home_dir: &Path) -> Option<String> {
    duduclaw_security::keyfile::decrypt_keyfile_value(encrypted, home_dir)
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

/// Resolve a per-agent channel token from `agent.toml`'s typed
/// `[channels.*]` sections: encrypted twin first, then plaintext (which may be
/// a `secret://` reference).
///
/// Used by `start_*_bots()`. Returns `None` — never an empty string — when
/// nothing is configured; the six call sites that used to re-check
/// `!token.is_empty()` after this call no longer need to, and Discord's
/// hand-inlined copy of this logic is gone (WP-H1 P0).
///
/// Async since WP-6C: every `start_*_bots()` caller already runs on the
/// gateway's tokio runtime, so this now resolves network-backed `secret://`
/// references (Vault / 1Password / Infisical) instead of failing closed on
/// them. `sm_cfg` is threaded in rather than loaded here because callers
/// resolve this once per agent in a loop — loading it inside would mean one
/// `config.toml` read per agent (per channel, for Slack's two tokens) instead
/// of one per `start_*_bots()` call.
pub(crate) async fn resolve_agent_token(
    encrypted: &Option<String>,
    plaintext: &str,
    home_dir: &Path,
    sm_cfg: &SecretManagerConfig,
) -> Option<Secret> {
    SecretRef::from_typed(encrypted.as_deref(), plaintext)
        .resolve(sm_cfg, home_dir)
        .await
}

/// Non-secret status of a per-agent channel credential: configured / source /
/// writable / plaintext-residue. Never touches a backend, never holds a value.
pub fn describe_agent_token(encrypted: &Option<String>, plaintext: &str) -> SecretStatus {
    SecretRef::from_typed(encrypted.as_deref(), plaintext).describe()
}

/// Repair a Telegram bot token whose `:` separator was lost (WP12).
///
/// A Telegram bot token is **always** `<bot_id>:<secret>` — BotFather has never
/// issued one without the colon, and the id part is always decimal. A customer
/// upgrade surfaced a stored token in which the colon had been replaced by a
/// hyphen, which makes every Bot API call address a bot that does
/// not exist, so the channel shows up permanently offline.
///
/// This is a **read-time** compatibility repair for values already sitting in a
/// config file. The guards are deliberately narrow so a healthy token can never
/// be rewritten:
///
/// - the value must contain **no** colon at all — a well-formed token always
///   does, so this can only ever fire on an already-broken value; and
/// - it must match `<5–16 digits>-<≥30 chars of [A-Za-z0-9_-]>`.
///
/// Only the **first** hyphen is restored; hyphens inside the secret (which are
/// legal) are untouched. Everything else is returned byte-identical.
pub fn repair_telegram_token(raw: &str) -> std::borrow::Cow<'_, str> {
    let t = raw.trim();
    if t.contains(':') {
        return std::borrow::Cow::Borrowed(raw);
    }
    let Some(idx) = t.find('-') else {
        return std::borrow::Cow::Borrowed(raw);
    };
    let (id, rest) = t.split_at(idx);
    // `-` is a single ASCII byte, so this cannot land mid-char.
    let secret = &rest[1..];
    let id_ok = (5..=16).contains(&id.len()) && id.chars().all(|c| c.is_ascii_digit());
    let secret_ok = secret.len() >= 30
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if id_ok && secret_ok {
        tracing::warn!(
            "Telegram bot token for bot id {id} is missing its ':' separator — \
             repaired in memory for this run. Re-save the token in the dashboard \
             to fix the stored value."
        );
        std::borrow::Cow::Owned(format!("{id}:{secret}"))
    } else {
        std::borrow::Cow::Borrowed(raw)
    }
}

/// Whether a config field name denotes a Telegram bot token, so the read path
/// knows to apply [`repair_telegram_token`].
fn is_telegram_token_field(field_base: &str) -> bool {
    field_base == "telegram_bot_token"
}

/// Whether `token` is a structurally valid Telegram bot token.
///
/// Deliberately generous — this gates a *save*, so a false reject is worse than
/// a false accept. It only asserts the two properties BotFather has always
/// guaranteed: a decimal bot id, a `:` separator, and a URL-safe secret.
pub fn is_valid_telegram_token(token: &str) -> bool {
    let Some((id, secret)) = token.split_once(':') else {
        return false;
    };
    (5..=20).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_digit())
        && secret.len() >= 20
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Validate (and where possible silently repair) a channel credential at the
/// **write** boundary.
///
/// Before WP12 nothing between the dashboard text field and the encrypted
/// config checked the shape of a channel token: `channels.add` and the
/// `agents.update` channel closure both only tested `is_empty()`. A token that
/// arrived corrupted — a hyphen where the colon belongs —
/// was encrypted and stored, and the first sign of trouble was the channel
/// silently sitting offline. Now the save either fixes it or refuses it with a
/// message the operator can act on.
///
/// Returns the value to persist, or a user-facing (zh-TW) error.
/// Channels other than Telegram pass through unchanged — their token formats
/// are not stable enough to gate on.
pub fn validate_channel_token(channel: &str, token: &str) -> Result<String, String> {
    if channel != "telegram" {
        return Ok(token.to_string());
    }
    let candidate = repair_telegram_token(token.trim()).into_owned();
    if is_valid_telegram_token(&candidate) {
        Ok(candidate)
    } else {
        Err("Telegram Bot Token 格式不正確。正確格式是「數字:英數字串」（例如 123456789:AAE...），\
             中間是半形冒號。請從 BotFather 的訊息完整複製後再貼上。"
            .to_string())
    }
}

/// Classify a `<section>.<field_base>` credential in an already-parsed
/// `config.toml` table into the shared [`SecretRef`].
///
/// This is the **single** place that knows the config-file layout convention
/// (`<field>_enc` beats `<field>`, and a present-but-blank `<field>` is an
/// explicit removal marker that suppresses a stale ciphertext twin). Both the
/// synchronous [`decrypt_config_field`] and the asynchronous
/// [`read_encrypted_config_field`] build on it, so they can no longer drift
/// apart.
pub fn config_secret_ref(table: &toml::Table, section: &str, field_base: &str) -> SecretRef {
    let Some(section_table) = table.get(section).and_then(|v| v.as_table()) else {
        return SecretRef::classify(None, None);
    };
    let enc_field = format!("{field_base}_enc");
    SecretRef::classify(
        section_table.get(&enc_field).and_then(|v| v.as_str()),
        section_table.get(field_base).and_then(|v| v.as_str()),
    )
}

/// Non-secret status of a `config.toml` credential field: configured / source /
/// writable / plaintext-residue. Never resolves, never holds a value.
///
/// `residue == true` is the design §1.5 case: a plaintext twin sitting next to
/// a `_enc` twin. The plaintext is inert for reads (the ciphertext wins) but it
/// is still a live credential in the file, and until now nothing in the system
/// could see it.
pub fn describe_config_field(
    table: &toml::Table,
    section: &str,
    field_base: &str,
) -> SecretStatus {
    config_secret_ref(table, section, field_base).describe()
}

/// Read a config field, trying the encrypted version first.
///
/// For example, `decrypt_config_field(table, "channels", "telegram_bot_token", home_dir)`
/// will try `channels.telegram_bot_token_enc` first, decrypt it, and fall back
/// to `channels.telegram_bot_token` if the encrypted field is missing or empty.
///
/// The plaintext twin may hold a `secret://<backend>/<name>` reference:
/// `secret://env/…` resolves here; a network-backed reference fails closed with
/// a warning (it needs [`decrypt_config_field_async`] or
/// [`read_encrypted_config_field`], which can `await`).
/// Either way the URI itself is never returned as if it were the credential.
///
/// Retained for any genuinely-synchronous caller (none exists in production as
/// of WP-6C — every real call site that used to reach this now goes through
/// [`decrypt_config_field_async`]) and exercised directly by this module's own
/// tests, which assert the fail-closed behaviour on a network reference.
pub fn decrypt_config_field(
    table: &toml::Table,
    section: &str,
    field_base: &str,
    home_dir: &Path,
) -> Option<Secret> {
    let raw = config_secret_ref(table, section, field_base).resolve_sync(home_dir)?;
    Some(apply_field_repairs(field_base, raw))
}

/// Async twin of [`decrypt_config_field`] (WP-6C) for callers already holding
/// a parsed table on an async call path: resolves a network-backed
/// `secret://` reference (Vault / 1Password / Infisical) instead of failing
/// closed.
///
/// `table` supplies both the field being read and the `[secret_manager]`
/// section describing how to reach a network backend — the same table
/// [`read_encrypted_config_field`] would otherwise re-read from disk, so a
/// caller that already parsed `config.toml` (e.g. `dispatcher.rs`'s
/// delegation-forward path, which needs the table for other fields too) does
/// not pay for a second read just to gain network-backend support.
pub async fn decrypt_config_field_async(
    table: &toml::Table,
    section: &str,
    field_base: &str,
    home_dir: &Path,
) -> Option<Secret> {
    let sm_cfg = secret_manager_config_from_table(table);
    let raw = config_secret_ref(table, section, field_base)
        .resolve(&sm_cfg, home_dir)
        .await?;
    Some(apply_field_repairs(field_base, raw))
}

/// Field-specific read-time repairs applied to a resolved value.
/// Currently only the WP12 Telegram `:`-separator repair.
fn apply_field_repairs(field_base: &str, value: Secret) -> Secret {
    if !is_telegram_token_field(field_base) {
        return value;
    }
    match repair_telegram_token(value.expose()) {
        std::borrow::Cow::Borrowed(_) => value,
        std::borrow::Cow::Owned(fixed) => Secret::new(fixed).unwrap_or(value),
    }
}

/// Read a config field from a TOML file, with encryption support.
///
/// Resolution order for the effective value:
/// 1. `<field>_enc` (decrypted) → `<field>` plaintext.
/// 2. If either yields a `secret://<backend>/<name>` reference, resolve it
///    through the configured `SecretManager` (e.g. HashiCorp Vault) — this is
///    the opt-in externalised-secret path. Non-reference values are returned
///    unchanged, so existing inline / `_enc` secrets behave exactly as before.
///
/// Returns `Option<String>` rather than `Option<Secret>` only because its ~40
/// call sites hand the value straight to an HTTP client; tightening those is a
/// P1 item.
pub async fn read_encrypted_config_field(
    home_dir: &Path,
    section: &str,
    field_base: &str,
) -> Option<String> {
    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    decrypt_config_field_async(&table, section, field_base, home_dir)
        .await
        .map(Secret::expose_owned)
}

// ─── Per-agent channel token + reports_to cascade ───────────

/// Read a single agent's `[channels.<channel>] bot_token(_enc)` from its
/// `agent.toml`. Returns `None` when the file is missing or the agent
/// has no token for that channel.
///
/// Async since WP-6C — see [`resolve_agent_channel_token_via_reports_to`] for
/// why `sm_cfg` is a parameter rather than loaded inside.
pub async fn read_agent_channel_token(
    home_dir: &Path,
    agent_id: &str,
    channel: &str,
    sm_cfg: &SecretManagerConfig,
) -> Option<Secret> {
    agent_channel_token_ref(home_dir, agent_id, channel)?
        .resolve(sm_cfg, home_dir)
        .await
}

/// The [`SecretRef`] for one agent's `[channels.<channel>] bot_token(_enc)`.
/// `None` when the agent file or the section is absent (as opposed to present
/// and unset, which is an empty `SecretRef`).
///
/// Kept separate from [`read_agent_channel_token`] so `describe()` can report a
/// per-agent credential's source and plaintext residue without resolving it.
pub fn agent_channel_token_ref(
    home_dir: &Path,
    agent_id: &str,
    channel: &str,
) -> Option<SecretRef> {
    let agent_toml = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = std::fs::read_to_string(&agent_toml).ok()?;
    let table: toml::Value = content.parse().ok()?;
    let section = table
        .get("channels")
        .and_then(|c| c.as_table())
        .and_then(|t| t.get(channel))
        .and_then(|v| v.as_table())?;

    // `agent.toml` writes delete the plaintext key rather than blanking it, and
    // the historical reader here tried `_enc` first and merely *skipped* an
    // empty `bot_token` — it never treated blank-as-removal. `from_typed`
    // preserves exactly that.
    Some(SecretRef::from_typed(
        section.get("bot_token_enc").and_then(|v| v.as_str()),
        section
            .get("bot_token")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
    ))
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
///
/// Async since WP-6C: every production caller (`goal_notify`,
/// `dispatcher::resolve_forward_token`, `cron_scheduler::resolve_channel_token`)
/// already runs on the gateway's tokio runtime, so this can now resolve a
/// network-backed `secret://` reference instead of failing closed on it.
/// `[secret_manager]` is loaded once up front (not per hop) since the chain
/// can walk up to `MAX_REPORTS_TO_HOPS` agents.
pub async fn resolve_agent_channel_token_via_reports_to(
    home_dir: &Path,
    agent_id: &str,
    channel: &str,
) -> Option<Secret> {
    let sm_cfg = SecretManagerConfig::load_from_home(home_dir).await;
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
        if let Some(tok) = read_agent_channel_token(home_dir, &current, channel, &sm_cfg).await {
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

/// Every bot token that could DM a user on `channel` in this deployment:
/// the global `[channels]` token first, then each agent's own
/// `[channels.<channel>] bot_token(_enc)`, deduplicated, agents visited in
/// sorted-id order so the result is deterministic.
///
/// ## Why this exists (2026-08-13 login-OTP outage)
///
/// User-level flows — login OTP, install sign-off DMs, autopilot notify —
/// have no agent context, so they historically read only the global token.
/// A deployment whose Telegram bot is agent-scoped (`agent.toml
/// [channels.telegram]`, global `[channels] telegram_bot_token = ""`) kept a
/// perfectly working bot while every one of those flows failed with "not
/// configured". The agent bot is not merely a fallback there: on Telegram a
/// bot can only DM users who have started a chat with it, and the agent bot
/// is exactly the one the user has talked to. Callers should try each
/// candidate in order until one send succeeds.
pub async fn channel_dm_token_candidates(home_dir: &Path, channel: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(field) = crate::otp_delivery::token_field(channel) {
        // No `is_empty` guard: the resolver cannot hand back an empty token
        // (WP-H1 — `Secret::new` refuses the empty string).
        if let Some(tok) = read_encrypted_config_field(home_dir, "channels", field).await {
            out.push(tok);
        }
    }
    let agents_dir = home_dir.join("agents");
    let mut ids: Vec<String> = match std::fs::read_dir(&agents_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    ids.sort();
    // Loaded once, not per agent — this can iterate every agent directory.
    let sm_cfg = SecretManagerConfig::load_from_home(home_dir).await;
    for id in ids {
        if let Some(tok) = read_agent_channel_token(home_dir, &id, channel, &sm_cfg).await {
            let tok = tok.expose_owned();
            if !out.contains(&tok) {
                out.push(tok);
            }
        }
    }
    out
}

/// DM token candidate resolution (2026-08-13 login-OTP outage regression).
#[cfg(test)]
mod dm_token_candidate_tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[tokio::test]
    async fn global_first_then_agents_sorted_and_deduped() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join("config.toml"),
            "[channels]\ntelegram_bot_token = \"global-tok\"\n",
        );
        write(
            &home.path().join("agents/zeta/agent.toml"),
            "[channels.telegram]\nbot_token = \"agent-z\"\n",
        );
        write(
            &home.path().join("agents/alpha/agent.toml"),
            "[channels.telegram]\nbot_token = \"agent-a\"\n",
        );
        // Duplicate of the global token on another agent must not repeat.
        write(
            &home.path().join("agents/mid/agent.toml"),
            "[channels.telegram]\nbot_token = \"global-tok\"\n",
        );
        let got = channel_dm_token_candidates(home.path(), "telegram").await;
        assert_eq!(got, vec!["global-tok", "agent-a", "agent-z"]);
    }

    /// The outage shape: global field explicitly emptied (channel moved onto
    /// an agent) — the agent token must still be found.
    #[tokio::test]
    async fn empty_global_falls_back_to_agent_token() {
        let home = tempfile::tempdir().unwrap();
        write(
            &home.path().join("config.toml"),
            "[channels]\ntelegram_bot_token = \"\"\ntelegram_bot_token_enc = \"\"\n",
        );
        write(
            &home.path().join("agents/trader-lead/agent.toml"),
            "[channels.telegram]\nbot_token = \"agent-tok\"\n",
        );
        let got = channel_dm_token_candidates(home.path(), "telegram").await;
        assert_eq!(got, vec!["agent-tok"]);
    }

    #[tokio::test]
    async fn no_tokens_anywhere_is_empty() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("config.toml"), "[channels]\n");
        let got = channel_dm_token_candidates(home.path(), "telegram").await;
        assert!(got.is_empty());
    }
}

/// WP12 — Telegram token shape: repair, validation, and the config round-trip.
#[cfg(test)]
mod telegram_token_tests {
    use super::*;

    // Fixtures are assembled at run time from fragments. Every value here is
    // fake, but a *fake* token carrying a real vendor shape still trips GitHub
    // push protection and other source scanners, and a blocked push looks
    // exactly like a real leak until someone reads the diff. Splitting the
    // literals keeps the scanners quiet without changing what the tests assert.

    const TG_ID: &str = "7000000001";

    /// A 35-character URL-safe secret, the length BotFather issues.
    fn tg_secret() -> String {
        ["AAExample", "Example", "Example", "Example", "XYZ12"].concat()
    }

    /// The canonical `<id>:<secret>` form.
    fn good() -> String {
        format!("{TG_ID}:{}", tg_secret())
    }

    /// The corrupted `<id>-<secret>` form WP12 was reported with.
    fn broken() -> String {
        format!("{TG_ID}-{}", tg_secret())
    }

    #[test]
    fn broken_separator_is_repaired() {
        assert_eq!(repair_telegram_token(&broken()), good());
    }

    #[test]
    fn healthy_token_is_returned_byte_identical() {
        // The critical safety property: a good token is never rewritten.
        let good = good();
        assert!(matches!(
            repair_telegram_token(&good),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(repair_telegram_token(&good), good);
    }

    #[test]
    fn hyphens_inside_the_secret_survive_the_repair() {
        // Only the FIRST hyphen is the separator; the rest belong to the secret.
        let secret = ["AAExample-", "Example", "Example", "Example-", "XYZ12"].concat();
        let broken = format!("7000000002-{secret}");
        assert_eq!(repair_telegram_token(&broken), format!("7000000002:{secret}"));
    }

    #[test]
    fn non_token_hyphenated_values_are_never_touched() {
        let slack_token = ["xoxb", "-1234567890-", "abcdefghijklmnopqrst"].concat();
        for s in [
            "2026-08-04".to_string(),                     // a date
            "12345-abc".to_string(),                      // too-short secret
            format!("abc-{}", tg_secret()),               // non-numeric bot id
            format!("1234-{}", tg_secret()),              // bot id too short
            slack_token,                                  // a Slack token
            String::new(),
        ] {
            assert!(
                matches!(repair_telegram_token(&s), std::borrow::Cow::Borrowed(_)),
                "must not rewrite: {s}"
            );
            assert_eq!(repair_telegram_token(&s), s);
        }
    }

    #[test]
    fn already_colon_separated_values_short_circuit() {
        // Even a weird value keeps its colon untouched — the repair only ever
        // considers colon-less input.
        let odd = "not-a-token:but-has-a-colon";
        assert_eq!(repair_telegram_token(odd), odd);
    }

    #[test]
    fn validity_check_accepts_real_tokens_and_rejects_the_broken_shape() {
        assert!(is_valid_telegram_token(&good()));
        assert!(!is_valid_telegram_token(&broken()));
        assert!(!is_valid_telegram_token("123456789:short"));
        assert!(!is_valid_telegram_token(&format!("abcdefg:{}", tg_secret())));
        assert!(!is_valid_telegram_token(""));
    }

    #[test]
    fn write_boundary_normalises_broken_and_rejects_garbage() {
        // Broken separator → silently repaired on save.
        assert_eq!(
            validate_channel_token("telegram", &broken()).unwrap(),
            good()
        );
        // Surrounding whitespace from a paste is trimmed.
        assert_eq!(
            validate_channel_token("telegram", &format!("  {}\n", good())).unwrap(),
            good()
        );
        // Unusable value → actionable error instead of a silent offline channel.
        let err = validate_channel_token("telegram", "please-paste-your-token").unwrap_err();
        assert!(err.contains("Telegram Bot Token"), "{err}");
        // Other channels are pass-through — their formats are not gated.
        assert_eq!(
            validate_channel_token("line", "whatever/token+value==").unwrap(),
            "whatever/token+value==".to_string()
        );
    }

    /// M1 — the validator is only ever applied to the primary credential.
    /// Non-token fields (chat ids, webhook secrets, future additions) must pass
    /// through untouched even under the `telegram` channel name.
    #[test]
    fn validation_is_a_no_op_for_non_telegram_channels() {
        for channel in ["discord", "slack", "line", "whatsapp", "feishu", "wecom", "dingtalk"] {
            let odd = "9876543210-notATelegramTokenButLooksLikeOne1234";
            assert_eq!(
                validate_channel_token(channel, odd).unwrap(),
                odd.to_string(),
                "{channel} must not be gated by the Telegram rule"
            );
        }
    }

    #[test]
    fn stored_broken_token_is_repaired_on_read() {
        // A config file that already contains the corrupted value (the customer's
        // situation) must yield a working token without a manual re-save.
        let table: toml::Table =
            format!("[channels]\ntelegram_bot_token = \"{}\"\n", broken())
                .parse()
                .unwrap();
        let home = std::env::temp_dir().join("duduclaw-wp12-nonexistent");
        let got = decrypt_config_field(&table, "channels", "telegram_bot_token", &home);
        assert_eq!(got.map(|s| s.expose_owned()), Some(good()));
    }

    #[test]
    fn read_repair_is_scoped_to_the_telegram_field() {
        // A Discord/LINE token that happens to look hyphenated must not be
        // rewritten — only `telegram_bot_token` goes through the repair.
        let table: toml::Table =
            format!("[channels]\ndiscord_bot_token = \"{}\"\n", broken()).parse().unwrap();
        let home = std::env::temp_dir().join("duduclaw-wp12-nonexistent");
        let got = decrypt_config_field(&table, "channels", "discord_bot_token", &home);
        assert_eq!(got.map(|s| s.expose_owned()), Some(broken()));
    }
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

    #[tokio::test]
    async fn resolves_own_token_when_present() {
        let home = TempHome::new();
        home.write_agent("xianwen-pm", &agent_toml("xianwen-pm", "xianwen-tl", Some("own-token")));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "xianwen-pm", "discord").await;
        assert_eq!(tok.map(|t| t.expose_owned()).as_deref(), Some("own-token"));
    }

    #[tokio::test]
    async fn resolves_parent_token_when_self_empty() {
        let home = TempHome::new();
        home.write_agent("xianwen-pm", &agent_toml("xianwen-pm", "xianwen-tl", None));
        home.write_agent("xianwen-tl", &agent_toml("xianwen-tl", "agnes", None));
        home.write_agent("agnes", &agent_toml("agnes", "", Some("agnes-bot-token")));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "xianwen-pm", "discord").await;
        assert_eq!(tok.map(|t| t.expose_owned()).as_deref(), Some("agnes-bot-token"));
    }

    #[tokio::test]
    async fn returns_none_when_chain_has_no_token() {
        let home = TempHome::new();
        home.write_agent("a", &agent_toml("a", "b", None));
        home.write_agent("b", &agent_toml("b", "c", None));
        home.write_agent("c", &agent_toml("c", "", None));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "a", "discord").await;
        assert!(tok.is_none());
    }

    #[tokio::test]
    async fn stops_at_first_token_not_farthest_ancestor() {
        // xianwen-pm has no token; xianwen-tl has a token; agnes also has one.
        // Cascade should return xianwen-tl's (the nearest ancestor).
        let home = TempHome::new();
        home.write_agent("xianwen-pm", &agent_toml("xianwen-pm", "xianwen-tl", None));
        home.write_agent("xianwen-tl", &agent_toml("xianwen-tl", "agnes", Some("tl-token")));
        home.write_agent("agnes", &agent_toml("agnes", "", Some("agnes-token")));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "xianwen-pm", "discord").await;
        assert_eq!(tok.map(|t| t.expose_owned()).as_deref(), Some("tl-token"));
    }

    #[tokio::test]
    async fn cycle_detection_returns_none_without_stack_overflow() {
        let home = TempHome::new();
        home.write_agent("a", &agent_toml("a", "b", None));
        home.write_agent("b", &agent_toml("b", "a", None)); // cycle
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "a", "discord").await;
        assert!(tok.is_none());
    }

    #[tokio::test]
    async fn missing_agent_toml_returns_none() {
        let home = TempHome::new();
        // No agent files at all.
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "ghost", "discord").await;
        assert!(tok.is_none());
    }

    #[tokio::test]
    async fn reports_to_empty_string_is_treated_as_root() {
        let home = TempHome::new();
        home.write_agent("solo", &agent_toml("solo", "", None));
        let tok = resolve_agent_channel_token_via_reports_to(home.path(), "solo", "discord").await;
        assert!(tok.is_none());
    }

    #[tokio::test]
    async fn different_channel_keys_are_independent() {
        // Agent configures only Telegram; Discord lookup should fall through.
        let home = TempHome::new();
        let tg_body = "[agent]\nname = \"x\"\nreports_to = \"\"\n\
                       [channels.telegram]\nbot_token = \"tg-tok\"\n";
        home.write_agent("x", tg_body);
        assert_eq!(
            resolve_agent_channel_token_via_reports_to(home.path(), "x", "telegram")
                .await
                .map(|t| t.expose_owned())
                .as_deref(),
            Some("tg-tok")
        );
        assert!(resolve_agent_channel_token_via_reports_to(home.path(), "x", "discord").await.is_none());
    }

    // ─── MED-B: enc-only fields (no plaintext copy) stay readable ──

    /// `channels.add` no longer persists a plaintext copy of the WeCom /
    /// DingTalk secrets — the read path must work from `_enc` alone.
    #[test]
    fn enc_only_field_reads_without_plaintext_copy() {
        let home = TempHome::new();
        let enc = encrypt_value("corp-secret-value", home.path()).expect("encrypt");
        let toml_src = format!("[channels]\nwecom_corp_secret_enc = \"{enc}\"\n");
        let table: toml::Table = toml_src.parse().unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "wecom_corp_secret", home.path())
                .map(|s| s.expose_owned()),
            Some("corp-secret-value".to_string())
        );
    }

    /// An explicitly *empty* plaintext key means "channel removed" even when a
    /// stale `_enc` remains — which is why the write path REMOVES the plaintext
    /// key instead of blanking it.
    #[test]
    fn empty_plaintext_marks_removed_even_with_stale_enc() {
        let home = TempHome::new();
        let enc = encrypt_value("stale-secret", home.path()).expect("encrypt");
        let toml_src = format!(
            "[channels]\nwecom_corp_secret = \"\"\nwecom_corp_secret_enc = \"{enc}\"\n"
        );
        let table: toml::Table = toml_src.parse().unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "wecom_corp_secret", home.path())
                .map(|s| s.expose_owned()),
            None
        );
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

/// WP-H1 (credentials doctrine P0) — behaviour equivalence for the three
/// pre-existing resolution paths, plus the bug the convergence fixes.
///
/// The P0 acceptance bar is "existing `_enc`, plaintext and env paths resolve
/// to the same value as before", which is what lets the whole change ship
/// without a feature flag and be reverted as one commit. Everything asserted
/// here was true before the `SecretRef` rewrite except where a test says
/// otherwise in its own name.
#[cfg(test)]
mod wp_h1_credentials_doctrine_tests {
    use super::*;
    use duduclaw_security::secret_ref::SourceKind;

    struct TempHome(std::path::PathBuf);
    impl TempHome {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("duduclaw-wph1-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn write_agent(&self, agent: &str, body: &str) {
            let dir = self.0.join("agents").join(agent);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("agent.toml"), body).unwrap();
        }
        fn write_config(&self, body: &str) {
            std::fs::write(self.0.join("config.toml"), body).unwrap();
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ─── Equivalence: config.toml `_enc` / plaintext ────────────

    #[test]
    fn enc_field_resolves_unchanged() {
        let home = TempHome::new();
        let enc = encrypt_value("tok-from-enc", home.path()).unwrap();
        let table: toml::Table = format!("[channels]\ndiscord_bot_token_enc = \"{enc}\"\n")
            .parse()
            .unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "discord_bot_token", home.path())
                .map(|s| s.expose_owned()),
            Some("tok-from-enc".to_string())
        );
    }

    #[test]
    fn plaintext_field_resolves_unchanged() {
        let home = TempHome::new();
        let table: toml::Table = "[channels]\ndiscord_bot_token = \"tok-plain\"\n"
            .parse()
            .unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "discord_bot_token", home.path())
                .map(|s| s.expose_owned()),
            Some("tok-plain".to_string())
        );
    }

    #[test]
    fn enc_beats_plaintext_and_undecryptable_enc_falls_back() {
        let home = TempHome::new();
        let enc = encrypt_value("winner", home.path()).unwrap();
        let table: toml::Table =
            format!("[channels]\ndiscord_bot_token = \"loser\"\ndiscord_bot_token_enc = \"{enc}\"\n")
                .parse()
                .unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "discord_bot_token", home.path())
                .map(|s| s.expose_owned()),
            Some("winner".to_string())
        );

        // Garbage ciphertext → the plaintext twin still answers (pre-existing
        // fail-soft behaviour: a rotated keyfile must not brick the channel).
        let table: toml::Table =
            "[channels]\ndiscord_bot_token = \"loser\"\ndiscord_bot_token_enc = \"@@garbage@@\"\n"
                .parse()
                .unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "discord_bot_token", home.path())
                .map(|s| s.expose_owned()),
            Some("loser".to_string())
        );
    }

    #[test]
    fn missing_section_and_missing_field_are_unset() {
        let home = TempHome::new();
        let table: toml::Table = "[other]\nx = 1\n".parse().unwrap();
        assert!(decrypt_config_field(&table, "channels", "discord_bot_token", home.path()).is_none());
        let table: toml::Table = "[channels]\nother_token = \"v\"\n".parse().unwrap();
        assert!(decrypt_config_field(&table, "channels", "discord_bot_token", home.path()).is_none());
    }

    // ─── Equivalence: async file path (incl. env backend) ───────

    #[tokio::test]
    async fn async_path_matches_sync_for_enc_and_plaintext() {
        let home = TempHome::new();
        let enc = encrypt_value("async-enc", home.path()).unwrap();
        home.write_config(&format!(
            "[channels]\ndiscord_bot_token_enc = \"{enc}\"\nslack_bot_token = \"async-plain\"\n"
        ));
        assert_eq!(
            read_encrypted_config_field(home.path(), "channels", "discord_bot_token").await,
            Some("async-enc".to_string())
        );
        assert_eq!(
            read_encrypted_config_field(home.path(), "channels", "slack_bot_token").await,
            Some("async-plain".to_string())
        );
    }

    #[tokio::test]
    async fn async_path_still_resolves_env_references() {
        let home = TempHome::new();
        let var = format!("DUDUCLAW_WPH1_ASYNC_{}", std::process::id());
        // SAFETY: process-unique name, set and removed inside this test.
        unsafe { std::env::set_var(&var, "from-env-async") };
        home.write_config(&format!(
            "[channels]\ndiscord_bot_token = \"secret://env/{var}\"\n"
        ));
        let got = read_encrypted_config_field(home.path(), "channels", "discord_bot_token").await;
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got, Some("from-env-async".to_string()));
    }

    #[tokio::test]
    async fn async_path_still_resolves_an_encrypted_pointer_to_a_reference() {
        // `<field>_enc` whose decrypted content is itself a `secret://` URI —
        // the pre-existing async behaviour, which must survive the rewrite.
        let home = TempHome::new();
        let var = format!("DUDUCLAW_WPH1_PTR_{}", std::process::id());
        // SAFETY: process-unique name, set and removed inside this test.
        unsafe { std::env::set_var(&var, "pointer-target") };
        let enc = encrypt_value(&format!("secret://env/{var}"), home.path()).unwrap();
        home.write_config(&format!("[channels]\ndiscord_bot_token_enc = \"{enc}\"\n"));
        let got = read_encrypted_config_field(home.path(), "channels", "discord_bot_token").await;
        unsafe { std::env::remove_var(&var) };
        assert_eq!(got, Some("pointer-target".to_string()));
    }

    // ─── The bug: `secret://` used to leave sync paths as a literal ───

    /// Before WP-H1 every one of these returned the URI string itself, which
    /// was then sent to Telegram / Discord / Slack as the bot token.
    #[tokio::test]
    async fn sync_paths_never_return_a_secret_uri_as_the_token() {
        let home = TempHome::new();
        let reference = "secret://vault/telegram_bot_token";
        let sm_cfg = SecretManagerConfig::default();

        // (a) raw config table
        let table: toml::Table =
            format!("[channels]\ndiscord_bot_token = \"{reference}\"\n").parse().unwrap();
        let got = decrypt_config_field(&table, "channels", "discord_bot_token", home.path());
        assert!(got.is_none(), "config field leaked a secret:// literal");

        // (b) per-agent typed token (telegram.rs / slack.rs / discord.rs)
        let got = resolve_agent_token(&None, reference, home.path(), &sm_cfg).await;
        assert!(got.is_none(), "agent token leaked a secret:// literal");

        // (c) per-agent channel token + reports_to cascade
        home.write_agent(
            "kid",
            "[agent]\nname = \"kid\"\nreports_to = \"boss\"\n",
        );
        home.write_agent(
            "boss",
            &format!(
                "[agent]\nname = \"boss\"\nreports_to = \"\"\n\
                 [channels.discord]\nbot_token = \"{reference}\"\n"
            ),
        );
        assert!(
            read_agent_channel_token(home.path(), "boss", "discord", &sm_cfg).await.is_none(),
            "per-agent token leaked a secret:// literal"
        );
        assert!(
            resolve_agent_channel_token_via_reports_to(home.path(), "kid", "discord").await.is_none(),
            "reports_to cascade leaked a secret:// literal"
        );
    }

    /// The other half of the fix: a reference that a sync path *can* satisfy.
    #[tokio::test]
    async fn sync_paths_resolve_env_references() {
        let home = TempHome::new();
        let var = format!("DUDUCLAW_WPH1_SYNC_{}", std::process::id());
        // SAFETY: process-unique name, set and removed inside this test.
        unsafe { std::env::set_var(&var, "env-token") };
        let reference = format!("secret://env/{var}");
        let sm_cfg = SecretManagerConfig::default();

        let table: toml::Table =
            format!("[channels]\ndiscord_bot_token = \"{reference}\"\n").parse().unwrap();
        let from_config = decrypt_config_field(&table, "channels", "discord_bot_token", home.path())
            .map(|s| s.expose_owned());
        let from_agent =
            resolve_agent_token(&None, &reference, home.path(), &sm_cfg)
                .await
                .map(|s| s.expose_owned());

        home.write_agent(
            "solo",
            &format!(
                "[agent]\nname = \"solo\"\nreports_to = \"\"\n\
                 [channels.discord]\nbot_token = \"{reference}\"\n"
            ),
        );
        let from_cascade =
            resolve_agent_channel_token_via_reports_to(home.path(), "solo", "discord")
                .await
                .map(|s| s.expose_owned());
        unsafe { std::env::remove_var(&var) };

        assert_eq!(from_config.as_deref(), Some("env-token"));
        assert_eq!(from_agent.as_deref(), Some("env-token"));
        assert_eq!(from_cascade.as_deref(), Some("env-token"));
    }

    // ─── Empty-value semantics collapsed onto Option ────────────

    #[tokio::test]
    async fn empty_inputs_are_none_not_empty_strings() {
        let home = TempHome::new();
        let sm_cfg = SecretManagerConfig::default();
        assert!(resolve_agent_token(&None, "", home.path(), &sm_cfg).await.is_none());
        assert!(resolve_agent_token(&Some(String::new()), "", home.path(), &sm_cfg).await.is_none());
        let table: toml::Table = "[channels]\ndiscord_bot_token = \"\"\n".parse().unwrap();
        assert!(decrypt_config_field(&table, "channels", "discord_bot_token", home.path()).is_none());
    }

    /// The typed `agent.toml` structs cannot express "present but blank", so an
    /// enc-only agent channel must still resolve — the exact case
    /// `handlers.rs`'s enc-only round-trip test covers.
    #[tokio::test]
    async fn typed_enc_only_agent_token_survives_the_blank_plaintext() {
        let home = TempHome::new();
        let enc = encrypt_value("enc-only-agent-token", home.path()).unwrap();
        let sm_cfg = SecretManagerConfig::default();
        assert_eq!(
            resolve_agent_token(&Some(enc), "", home.path(), &sm_cfg)
                .await
                .map(|s| s.expose_owned()),
            Some("enc-only-agent-token".to_string())
        );
    }

    /// **Deliberate convergence, not equivalence.** `config.toml`'s canonical
    /// reader has always treated a present-but-blank plaintext key as "channel
    /// removed" and ignored a stale `_enc` twin; the dispatcher's forward path
    /// had its own reader that did not, so a removed channel kept forwarding on
    /// the stale ciphertext. Both now agree.
    #[test]
    fn blank_plaintext_in_config_suppresses_a_stale_ciphertext() {
        let home = TempHome::new();
        let enc = encrypt_value("stale-after-removal", home.path()).unwrap();
        let table: toml::Table =
            format!("[channels]\ndiscord_bot_token = \"\"\ndiscord_bot_token_enc = \"{enc}\"\n")
                .parse()
                .unwrap();
        assert!(decrypt_config_field(&table, "channels", "discord_bot_token", home.path()).is_none());
    }

    // ─── describe(): one dialect, and residue detection ─────────

    #[test]
    fn describe_reports_source_and_residue_without_the_value() {
        let home = TempHome::new();
        let enc = encrypt_value("real-token", home.path()).unwrap();

        // §1.5 shape: a plaintext twin left behind next to the ciphertext.
        let table: toml::Table = format!(
            "[channels]\ndiscord_bot_token = \"leftover-plaintext\"\n\
             discord_bot_token_enc = \"{enc}\"\n"
        )
        .parse()
        .unwrap();
        let st = describe_config_field(&table, "channels", "discord_bot_token");
        assert!(st.configured);
        assert_eq!(st.source, SourceKind::Inline);
        assert!(st.writable);
        assert!(st.residue, "plaintext twin next to _enc must be reported");
        let json = serde_json::to_string(&st).unwrap();
        assert!(!json.contains("leftover-plaintext") && !json.contains(&enc));

        // Clean encrypted-only field: no residue.
        let table: toml::Table =
            format!("[channels]\ndiscord_bot_token_enc = \"{enc}\"\n").parse().unwrap();
        assert!(!describe_config_field(&table, "channels", "discord_bot_token").residue);

        // Unset field.
        let table: toml::Table = "[channels]\n".parse().unwrap();
        let st = describe_config_field(&table, "channels", "discord_bot_token");
        assert!(!st.configured && st.source == SourceKind::Unset);

        // External reference: configured, but not writable from the dashboard.
        let table: toml::Table =
            "[channels]\ndiscord_bot_token = \"secret://vault/dc\"\n".parse().unwrap();
        let st = describe_config_field(&table, "channels", "discord_bot_token");
        assert!(st.configured && !st.writable);
        assert_eq!(st.source, SourceKind::Vault);
        assert_eq!(st.source_label, "vault:dc");

        // Per-agent variant shares the dialect.
        let st = describe_agent_token(&Some("ct".into()), "pt");
        assert!(st.residue);
    }

    // ─── Telegram read-time repair survives the rewrite ─────────

    #[test]
    fn telegram_repair_still_applies_on_both_paths() {
        let home = TempHome::new();
        let broken = "7000000001-AAExampleExampleExampleExampleXYZ12";
        let fixed = "7000000001:AAExampleExampleExampleExampleXYZ12";
        let table: toml::Table =
            format!("[channels]\ntelegram_bot_token = \"{broken}\"\n").parse().unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "telegram_bot_token", home.path())
                .map(|s| s.expose_owned()),
            Some(fixed.to_string())
        );
        // …and only for the Telegram field.
        let table: toml::Table =
            format!("[channels]\ndiscord_bot_token = \"{broken}\"\n").parse().unwrap();
        assert_eq!(
            decrypt_config_field(&table, "channels", "discord_bot_token", home.path())
                .map(|s| s.expose_owned()),
            Some(broken.to_string())
        );
    }
}
