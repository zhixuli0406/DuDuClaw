//! `secret://` resolution for `[[tick.sources]] headers` (WP-H1 P1).
//!
//! Custom tick headers are how an operator authenticates a polled feed
//! (`headers = { "X-API-Key" = "…" }`), and until now they were the one
//! credential surface in the whole config file stored as **pure plaintext** —
//! the design doc calls it "最刺眼的一項" for good reason: `TickSourceConfig`
//! hand-writes a `Debug` that prints only `headers_count`, and `ticks.sources`
//! exposes only the count, precisely because the values are secrets, yet they
//! sat unencrypted in `config.toml`.
//!
//! This module lets a header value be a `secret://<backend>/<name>` reference
//! resolved **at request time** — every poll, every websocket dial — which is
//! the doctrine's "每次操作重新 resolve" rule applied where it is cheapest to
//! obey (§2.4: local backends are free; the network ones own their own TTL).
//!
//! ## Why resolution re-validates
//!
//! `tick_config::validate_headers` fail-closes on CR/LF at config-load time,
//! which is what stops header injection. A resolved value never passed through
//! that gate — it came from Vault, a keychain, or a mounted file — so it is
//! checked again here with the same rule. Without this, a secret containing a
//! newline would append attacker-chosen headers to every request the source
//! makes. **A value that fails re-validation drops its header entirely**; it is
//! never truncated into something that merely looks safe.
//!
//! ## Failure is omission, never a literal
//!
//! An unresolvable reference removes the header. The alternative — passing the
//! `secret://…` string through as the value — is the exact bug this whole line
//! exists to kill, and here it would additionally ship the operator's secret
//! backend layout to a third-party endpoint.

use std::collections::BTreeMap;
use std::path::Path;

use duduclaw_security::secret_manager::SecretManagerConfig;
use duduclaw_security::secret_ref::SecretRef;
use tracing::warn;

use crate::tick_config::header_value_is_legal;

/// Resolve every `secret://` header value for one request.
///
/// Values that are not references are passed through untouched (byte-identical
/// to the pre-P1 behaviour, which is what keeps every existing deployment
/// working without a config change). `sm_cfg` is read from `config.toml` once
/// per call by [`load_secret_manager_config`] at the call site.
pub async fn resolve_header_secrets(
    headers: &BTreeMap<String, String>,
    sm_cfg: &SecretManagerConfig,
    home_dir: &Path,
    source_id: &str,
) -> BTreeMap<String, String> {
    // Overwhelmingly the common case: nothing to do, no config read, no clone
    // of the resolver machinery.
    if !headers.values().any(|v| v.starts_with("secret://")) {
        return headers.clone();
    }

    let mut out = BTreeMap::new();
    for (name, value) in headers {
        if !value.starts_with("secret://") {
            out.insert(name.clone(), value.clone());
            continue;
        }
        // `classify(None, Some(v))` is the same entry point every other
        // credential read uses; a malformed reference lands as unset there
        // rather than being mistaken for plaintext.
        let Some(secret) = SecretRef::classify(None, Some(value))
            .resolve(sm_cfg, home_dir)
            .await
        else {
            warn!(
                source = %source_id,
                header = %name,
                "tick header holds a secret:// reference that could not be resolved — \
                 header omitted (the reference is never sent as the value)"
            );
            continue;
        };
        if !header_value_is_legal(secret.expose()) {
            warn!(
                source = %source_id,
                header = %name,
                "tick header resolved to a value that is not visible ASCII (a CR/LF would be \
                 header injection) — header omitted"
            );
            continue;
        }
        out.insert(name.clone(), secret.expose_owned());
    }
    out
}

/// Read `[secret_manager]` out of the home's `config.toml`.
///
/// Deliberately per-call rather than cached: the doctrine forbids call sites
/// from holding resolved credentials, and this config is the *connection*
/// settings for the backend, whose rotation must take effect the same way.
pub async fn load_secret_manager_config(home_dir: &Path) -> SecretManagerConfig {
    let Ok(raw) = tokio::fs::read_to_string(home_dir.join("config.toml")).await else {
        return SecretManagerConfig::default();
    };
    raw.parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("secret_manager").cloned())
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn plain_values_pass_through_untouched() {
        let h = map(&[("X-Api-Key", "literal"), ("Accept", "application/json")]);
        let out = resolve_header_secrets(
            &h,
            &SecretManagerConfig::default(),
            Path::new("/nonexistent-home"),
            "src",
        )
        .await;
        assert_eq!(out, h);
    }

    #[tokio::test]
    async fn an_env_reference_resolves_at_request_time() {
        let var = format!("DUDUCLAW_TICK_HEADER_{}", std::process::id());
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "resolved-key") };
        let h = map(&[("X-Api-Key", &format!("secret://env/{var}"))]);
        let out = resolve_header_secrets(
            &h,
            &SecretManagerConfig::default(),
            Path::new("/nonexistent-home"),
            "src",
        )
        .await;
        unsafe { std::env::remove_var(&var) };
        assert_eq!(out.get("X-Api-Key").unwrap(), "resolved-key");
    }

    #[tokio::test]
    async fn an_unresolvable_reference_omits_the_header_rather_than_sending_the_uri() {
        for reference in [
            "secret://env/DUDUCLAW_DEFINITELY_UNSET_HEADER_XYZ",
            "secret://vault/whatever",
            "secret://bogus/whatever",
        ] {
            let h = map(&[("X-Api-Key", reference), ("Accept", "application/json")]);
            let out = resolve_header_secrets(
                &h,
                &SecretManagerConfig::default(),
                Path::new("/nonexistent-home"),
                "src",
            )
            .await;
            assert!(
                !out.contains_key("X-Api-Key"),
                "{reference} must be omitted, got {out:?}"
            );
            assert_eq!(out.get("Accept").unwrap(), "application/json");
        }
    }

    /// The re-validation that config-load-time validation cannot cover.
    #[tokio::test]
    async fn a_resolved_value_carrying_crlf_is_refused_not_truncated() {
        let var = format!("DUDUCLAW_TICK_INJECT_{}", std::process::id());
        // SAFETY: process-unique variable name, set and removed within this test.
        unsafe { std::env::set_var(&var, "ok\r\nX-Injected: evil") };
        let h = map(&[("X-Api-Key", &format!("secret://env/{var}"))]);
        let out = resolve_header_secrets(
            &h,
            &SecretManagerConfig::default(),
            Path::new("/nonexistent-home"),
            "src",
        )
        .await;
        unsafe { std::env::remove_var(&var) };
        assert!(out.is_empty(), "injected header must be dropped whole: {out:?}");
    }
}
