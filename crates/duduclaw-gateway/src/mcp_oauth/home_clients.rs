//! H1 public client adapter. Client visibility is independent of Home OAuth
//! token/credential/master; only publication/unresolved source state gates it.
use super::{CLIENT_CONFIG_FILE, McpOAuthClientConfig, home_store};
use crate::google_identity::home_source::{self, SourceBinding, SourceGuard};
use home_store::{
    Error, Result,
    raw_array::{RawArray, valid_provider},
};
use serde_json::{Value, json};
use std::path::Path;
const FIELDS: [&str; 7] = [
    "provider_id",
    "client_id",
    "client_secret",
    "auth_url",
    "token_url",
    "redirect_uri",
    "scopes",
];
fn locked<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    duduclaw_core::with_file_lock(path, || Ok(f())).map_err(|_| Error::Io)?
}
fn with_file<T>(home: &Path, f: impl FnOnce(&SourceGuard) -> Result<T>) -> Result<T> {
    home_source::with_source_guard(home, |g| {
        locked(&g.home().join("config.toml"), || {
            locked(&g.home().join(CLIENT_CONFIG_FILE), || f(g))
        })
    })
}
fn view(raw: &Value) -> Result<McpOAuthClientConfig> {
    let config: McpOAuthClientConfig =
        serde_json::from_value(raw.clone()).map_err(|_| Error::Invalid)?;
    if !valid_provider(&config.provider_id) {
        return Err(Error::Invalid);
    }
    for v in [
        &config.client_id,
        &config.client_secret,
        &config.auth_url,
        &config.token_url,
        &config.redirect_uri,
    ] {
        if v.len() > 64 * 1024 || v.chars().any(char::is_control) {
            return Err(Error::Invalid);
        }
    }
    home_store::normalized_scopes(config.scopes.clone())?;
    if config.provider_id == "google" {
        home_source::validate_google_client_raw(raw)?;
    }
    Ok(config)
}
struct ReadSnapshot {
    original: RawArray,
    selected: Vec<McpOAuthClientConfig>,
}
fn capture(home: &Path, provider: Option<&str>) -> Result<ReadSnapshot> {
    if provider.is_some_and(|p| !valid_provider(p)) {
        return Err(Error::Invalid);
    }
    with_file(home, |g| {
        let original = RawArray::read(&g.home().join(CLIENT_CONFIG_FILE))?;
        let wants_google =
            provider.is_none_or(|p| p == "google") && original.element("google").is_some();
        let google_readable = wants_google && home_store::google_client_readable(g).is_ok();
        let mut selected = Vec::new();
        for row in original.values() {
            if provider.is_some_and(|p| row["provider_id"] != p) {
                continue;
            }
            if row["provider_id"] == "google" && !google_readable {
                continue;
            }
            selected.push(view(row)?);
        }
        Ok(ReadSnapshot { original, selected })
    })
}
pub(super) fn load(home: &Path, provider: Option<&str>) -> Result<Vec<McpOAuthClientConfig>> {
    let captured = capture(home, provider)?;
    // Captured plaintext legacy fields do not need a key. Encrypted Google
    // fields must decrypt successfully, never become ciphertext-as-secret.
    let decoded = captured
        .selected
        .iter()
        .filter_map(|stored| {
            let mut config = stored.clone();
            config.client_secret =
                super::decode_secret(home, &stored.provider_id, &stored.client_secret)?;
            Some(config)
        })
        .collect();
    #[cfg(test)]
    home_store::test_probe::preparation_boundary(home);
    with_file(home, |g| {
        let actual = RawArray::read(&g.home().join(CLIENT_CONFIG_FILE))?;
        if !actual.same_bytes(&captured.original) {
            return Err(Error::Stale);
        }
        if captured.selected.iter().any(|c| c.provider_id == "google") {
            home_store::google_client_readable(g)?;
        }
        Ok(decoded)
    })
}
pub(super) fn has(home: &Path, provider: &str) -> bool {
    capture(home, Some(provider)).is_ok_and(|s| s.selected.iter().any(|c| !c.client_id.is_empty()))
}
fn merge(old: &[Value], provider: &str, next: Option<&Value>) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut found = false;
    for row in old {
        if row["provider_id"] != provider {
            rows.push(row.clone());
            continue;
        }
        found = true;
        if let Some(next) = next {
            let mut updated = row.clone();
            for field in FIELDS {
                updated[field] = next[field].clone();
            }
            rows.push(updated);
        }
    }
    if !found {
        if let Some(next) = next {
            rows.push(next.clone());
        }
    }
    rows
}
struct WriteSnapshot {
    original: RawArray,
    source: Option<SourceBinding>,
    token: Option<home_store::StoreSnapshot>,
}
fn preflight_document(
    original: &RawArray,
    config: Option<&McpOAuthClientConfig>,
    provider: &str,
) -> Result<()> {
    for encrypted in [false, true] {
        let next = config
            .map(|c| -> Result<Value> {
                let mut raw = serde_json::to_value(c).map_err(|_| Error::Invalid)?;
                if encrypted {
                    raw["client_secret"] =
                        json!(super::encrypted_size_placeholder(&c.client_secret)?);
                }
                view(&raw)?;
                Ok(raw)
            })
            .transpose()?;
        original.render(&merge(original.values(), provider, next.as_ref()))?;
    }
    Ok(())
}
fn capture_write(
    home: &Path,
    provider: &str,
    config: Option<&McpOAuthClientConfig>,
) -> Result<WriteSnapshot> {
    if provider == "google" {
        home_source::with_metadata(home, |g, source| {
            let original = RawArray::read(&g.home().join(CLIENT_CONFIG_FILE))?;
            preflight_document(&original, config, provider)?;
            Ok(WriteSnapshot {
                original,
                source: Some(source.binding().clone()),
                token: Some(home_store::client_mutation_snapshot(g)?),
            })
        })
    } else {
        with_file(home, |g| {
            home_store::ensure_publication_unblocked(g)?;
            let original = RawArray::read(&g.home().join(CLIENT_CONFIG_FILE))?;
            preflight_document(&original, config, provider)?;
            Ok(WriteSnapshot {
                original,
                source: None,
                token: None,
            })
        })
    }
}
fn apply(home: &Path, provider: &str, captured: WriteSnapshot, next: Option<Value>) -> Result<()> {
    if provider != "google" {
        return with_file(home, |g| {
            home_store::ensure_publication_unblocked(g)?;
            let actual = RawArray::read(&g.home().join(CLIENT_CONFIG_FILE))?;
            if !actual.same_bytes(&captured.original) {
                return Err(Error::Stale);
            }
            let rows = merge(actual.values(), provider, next.as_ref());
            let bytes = actual.render(&rows)?;
            let checked = RawArray::from_bytes(Some(bytes.to_vec()))?;
            if actual.element("google") != checked.element("google") {
                return Err(Error::Invalid);
            }
            let write = home_store::prepare_client_write(actual.bytes(), bytes.to_vec())?;
            home_store::publish_unrelated_source(g, write)
        });
    }
    let change = home_source::with_metadata(home, |g, source| {
        let actual = RawArray::read(&g.home().join(CLIENT_CONFIG_FILE))?;
        if !actual.same_bytes(&captured.original)
            || captured.source.as_ref() != Some(source.binding())
        {
            return Err(Error::Stale);
        }
        let rows = merge(actual.values(), provider, next.as_ref());
        let target = match rows.iter().find(|v| v["provider_id"] == "google") {
            Some(raw) => source.preview_google_client(raw)?,
            None => source.preview_without_google_client(),
        };
        let write =
            home_store::prepare_client_write(actual.bytes(), actual.render(&rows)?.to_vec())?;
        home_store::publication_transaction(g, |r| {
            r.require_snapshot(captured.token.as_ref().ok_or(Error::Invalid)?)?;
            let change = r.fence_source_change(source, target, Some(write))?;
            #[cfg(test)]
            home_store::record_source_fence(g);
            Ok(change)
        })
    })?;
    // The same ChangeTicket spans the Pending gap, never another client/source
    // capture issuing replacement authority or a generic new start.
    home_source::with_metadata(home, |g, actual| {
        home_store::publication_transaction(g, |r| {
            r.finish_change(&change, actual, home_store::host_time_now(g))
        })
    })
}
pub(super) fn upsert(home: &Path, mut config: McpOAuthClientConfig) -> Result<()> {
    // Original scope count must be checked before deduplication and crypto.
    config.scopes = home_store::normalized_scopes(config.scopes.clone())?;
    let raw = serde_json::to_value(&config).map_err(|_| Error::Invalid)?;
    view(&raw)?;
    let captured = capture_write(home, &config.provider_id, Some(&config))?;
    let mut raw = raw;
    if !config.client_secret.is_empty() {
        raw["client_secret"] = if config.provider_id == "google" {
            #[cfg(test)]
            {
                home_store::test_probe::crypto(home);
                home_store::test_probe::google_crypto(home);
            }
            let secret = crate::config_crypto::encrypt_value(&config.client_secret, home)
                .ok_or(Error::Io)?;
            json!(format!("{}{secret}", super::ENC_PREFIX))
        } else {
            json!(super::encrypt_field(&config.client_secret, home))
        };
    }
    #[cfg(test)]
    home_store::test_probe::preparation_boundary(home);
    apply(home, &config.provider_id, captured, Some(raw))
}
pub(super) fn remove(home: &Path, provider: &str) -> Result<()> {
    if !valid_provider(provider) {
        return Err(Error::Invalid);
    }
    // Absent is a read-only no-op, not a reason to create a Google control row.
    let exists = with_file(home, |g| {
        home_store::ensure_publication_unblocked(g)?;
        Ok(RawArray::read(&g.home().join(CLIENT_CONFIG_FILE))?
            .element(provider)
            .is_some())
    })?;
    if !exists {
        return Ok(());
    }
    let captured = capture_write(home, provider, None)?;
    apply(home, provider, captured, None)
}
