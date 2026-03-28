//! Shared utilities for reading encrypted config fields.
//!
//! Provides a single `decrypt_config_field()` function used by all channel
//! bots and handlers to read tokens from `config.toml`, trying the encrypted
//! `_enc` field first and falling back to plaintext for backwards compatibility.

use std::path::Path;

/// Load the AES-256 keyfile from `~/.duduclaw/.keyfile`.
/// Public variant for GVU encryption and other internal consumers.
pub(crate) fn load_keyfile_public(home_dir: &Path) -> Option<[u8; 32]> {
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

/// Decrypt a base64-encoded encrypted value using the per-machine keyfile.
fn decrypt_value(encrypted: &str, home_dir: &Path) -> Option<String> {
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
/// Returns `None` if encryption fails (keyfile missing, etc.).
pub fn encrypt_value(plaintext: &str, home_dir: &Path) -> Option<String> {
    if plaintext.is_empty() { return None; }
    let key = load_keyfile(home_dir)?;
    let engine = duduclaw_security::crypto::CryptoEngine::new(&key).ok()?;
    engine.encrypt_string(plaintext).ok()
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
