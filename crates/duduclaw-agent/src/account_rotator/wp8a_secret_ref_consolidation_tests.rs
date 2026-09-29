/// WP-8A / credentials doctrine P2 (item #5): `resolve_api_key` and
/// `resolve_oauth_token` used to be two independent hand-rolled "decrypt
/// `<field>_enc` via keyfile, else resolve a `secret://` reference, else use
/// the plaintext literally" chains. This module pins down that the
/// consolidation onto `duduclaw_security::secret_ref::SecretRef` is
/// behavior-preserving: encrypted still wins over plaintext, a `secret://`
/// reference still resolves through the configured backend (here: `env`,
/// the only local, no-network backend that's practical to exercise without a
/// live Vault/1Password/Infisical), and a bare plaintext `oauth_token` is
/// still never consumed.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway home directory carrying its own `.keyfile`, mirroring the
/// helper `duduclaw-security/src/secret_ref.rs` uses for the same purpose
/// — kept local here since `duduclaw-agent` cannot depend on
/// `duduclaw-security`'s `#[cfg(test)]`-only items across the crate
/// boundary.
struct TempHome(std::path::PathBuf);
impl TempHome {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "duduclaw-rotator-secretref-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn encrypt(&self, plain: &str) -> String {
        use duduclaw_security::crypto::CryptoEngine;
        let keyfile = self.0.join(".keyfile");
        let key = if keyfile.exists() {
            let bytes = std::fs::read(&keyfile).unwrap();
            let mut k = [0u8; 32];
            k.copy_from_slice(&bytes);
            k
        } else {
            let k = CryptoEngine::generate_key().unwrap();
            std::fs::write(&keyfile, k).unwrap();
            k
        };
        CryptoEngine::new(&key).unwrap().encrypt_string(plain).unwrap()
    }
}
impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ── resolve_api_key ──────────────────────────────────────────

#[tokio::test]
async fn enc_field_wins_over_a_differently_named_plaintext_field() {
    let home = TempHome::new();
    let enc = home.encrypt("from-api-key-enc");
    let table: toml::Table = format!(
        "api_key_enc = \"{enc}\"\nanthropic_api_key = \"plaintext-should-lose\"\n"
    )
    .parse()
    .unwrap();
    // Encrypted always wins over plaintext, even though `api_key_enc`
    // and `anthropic_api_key` are different field names — precedence is
    // "any enc field" before "any plaintext field", preserved from
    // before the WP-8A consolidation.
    assert_eq!(resolve_api_key(home.path(), &table).await, "from-api-key-enc");
}

#[tokio::test]
async fn secret_env_reference_resolves_through_the_shared_resolver() {
    let home = TempHome::new();
    let var = format!("DUDUCLAW_ROTATOR_APIKEY_TEST_{}", std::process::id());
    // SAFETY: process-unique variable name, set and removed within this test.
    unsafe { std::env::set_var(&var, "from-env-var") };
    let table: toml::Table = format!("anthropic_api_key = \"secret://env/{var}\"\n")
        .parse()
        .unwrap();
    let got = resolve_api_key(home.path(), &table).await;
    unsafe { std::env::remove_var(&var) };
    assert_eq!(got, "from-env-var");
}

#[tokio::test]
async fn plaintext_literal_still_works_as_legacy_fallback() {
    let home = TempHome::new();
    let table: toml::Table = "api_key = \"sk-legacy-literal\"\n".parse().unwrap();
    assert_eq!(resolve_api_key(home.path(), &table).await, "sk-legacy-literal");
}

#[tokio::test]
async fn unresolvable_secret_reference_falls_through_to_the_next_field_name() {
    let home = TempHome::new();
    // anthropic_api_key points at an unset env var (unresolvable);
    // api_key carries a usable literal. Must not just give up on the
    // first field's failure.
    let table: toml::Table =
        "anthropic_api_key = \"secret://env/DUDUCLAW_DEFINITELY_UNSET_ROTATOR_XYZ\"\napi_key = \"sk-fallback\"\n"
            .parse()
            .unwrap();
    assert_eq!(resolve_api_key(home.path(), &table).await, "sk-fallback");
}

#[tokio::test]
async fn nothing_configured_resolves_to_empty_string() {
    let home = TempHome::new();
    let table: toml::Table = "".parse().unwrap();
    assert_eq!(resolve_api_key(home.path(), &table).await, "");
}

// ── resolve_oauth_token ──────────────────────────────────────

#[tokio::test]
async fn oauth_token_enc_decrypts_via_keyfile() {
    let home = TempHome::new();
    let enc = home.encrypt("real-oauth-token");
    let table: toml::Table = format!("oauth_token_enc = \"{enc}\"\n").parse().unwrap();
    assert_eq!(
        resolve_oauth_token(home.path(), &table).await.as_deref(),
        Some("real-oauth-token")
    );
}

#[tokio::test]
async fn oauth_token_secret_reference_resolves() {
    let home = TempHome::new();
    let var = format!("DUDUCLAW_ROTATOR_OAUTH_TEST_{}", std::process::id());
    // SAFETY: process-unique variable name, set and removed within this test.
    unsafe { std::env::set_var(&var, "oauth-via-env") };
    let table: toml::Table = format!("oauth_token = \"secret://env/{var}\"\n")
        .parse()
        .unwrap();
    let got = resolve_oauth_token(home.path(), &table).await;
    unsafe { std::env::remove_var(&var) };
    assert_eq!(got.as_deref(), Some("oauth-via-env"));
}

/// The behavior-preservation guarantee this whole consolidation exists to
/// keep intact: a bare plaintext `oauth_token` (not a `secret://`
/// reference) is NEVER consumed, even though `resolve_api_key`'s
/// plaintext-field precedence tier would happily accept the equivalent
/// shape for an API key. `oauth_token` and `api_key` are not
/// interchangeable dialects — only `oauth_token_enc` and a `secret://`
/// reference are legitimate sources for an OAuth token.
#[tokio::test]
async fn bare_plaintext_oauth_token_is_never_consumed() {
    let home = TempHome::new();
    let table: toml::Table = "oauth_token = \"sk-ant-oat01-literal-not-a-reference\"\n"
        .parse()
        .unwrap();
    assert_eq!(resolve_oauth_token(home.path(), &table).await, None);
}

#[tokio::test]
async fn oauth_token_enc_wins_over_a_secret_reference_plaintext() {
    let home = TempHome::new();
    let enc = home.encrypt("enc-wins");
    let table: toml::Table = format!(
        "oauth_token_enc = \"{enc}\"\noauth_token = \"secret://env/DUDUCLAW_UNUSED_ROTATOR\"\n"
    )
    .parse()
    .unwrap();
    assert_eq!(
        resolve_oauth_token(home.path(), &table).await.as_deref(),
        Some("enc-wins")
    );
}

#[tokio::test]
async fn nothing_configured_oauth_token_is_none() {
    let home = TempHome::new();
    let table: toml::Table = "".parse().unwrap();
    assert_eq!(resolve_oauth_token(home.path(), &table).await, None);
}
