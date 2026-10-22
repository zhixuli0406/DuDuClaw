//! Runtime copy of the distributor notice, fetched from the control-plane.
//!
//! The distributor edits its notice in the license console; this gateway
//! asks for it (`POST /v1/license/distributor-notice`, license proof + nonce)
//! shortly after start and then every [`POLL_INTERVAL`]. The answer is an
//! Ed25519-signed document bound to this subscription
//! ([`duduclaw_license::SignedDistributorNotice`]). It is verified with the
//! same issuer keys as the license, re-sanitized, and cached on disk as the
//! signed document, so it survives restarts and offline periods and the
//! customer cannot edit it.
//!
//! Precedence on the About page: a verified runtime document (even a signed
//! "no notice") wins over the notice compiled into the build; without one
//! (never fetched, license removed, OpenSource) the compiled notice shows.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use duduclaw_license::{PublicKeyRegistry, SignedDistributorNotice};
use tracing::{debug, info, warn};

use super::sanitize;

/// Cache file under the gateway home.
pub const CACHE_FILENAME: &str = "distributor_notice.json";
/// Delay before the first fetch after start.
const FIRST_POLL_DELAY: Duration = Duration::from_secs(60);
/// Fetch cadence.
pub const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest response body read (a 64 KiB notice plus JSON overhead).
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// A verified notice in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub subscription_id: String,
    pub distributor_id: String,
    pub version: i64,
    /// Re-sanitized HTML, `None` = show no notice.
    pub html: Option<String>,
}

static ACCEPTED: RwLock<Option<Accepted>> = RwLock::new(None);

/// The runtime notice decision, if one is in force: `Some(None)` means a
/// signed "no notice", `None` means fall back to the compiled notice.
pub fn current() -> Option<Option<String>> {
    ACCEPTED
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|a| a.html.clone()))
}

fn set(value: Option<Accepted>) {
    if let Ok(mut guard) = ACCEPTED.write() {
        *guard = value;
    }
}

fn current_accepted() -> Option<Accepted> {
    ACCEPTED.read().ok().and_then(|guard| guard.clone())
}

/// Verify a signed document for `subscription_id` and turn it into the
/// notice to show. Refuses an older version from the same distributor than
/// the one in force (`previous`), and HTML the product sanitizer refuses.
pub fn accept(
    registry: &PublicKeyRegistry,
    subscription_id: &str,
    doc: &SignedDistributorNotice,
    previous: Option<&Accepted>,
) -> Result<Accepted, String> {
    doc.verify_for(registry, subscription_id)
        .map_err(|e| format!("distributor notice refused: {e}"))?;
    if let Some(prev) = previous {
        if prev.subscription_id == subscription_id
            && prev.distributor_id == doc.distributor_id
            && doc.version < prev.version
        {
            return Err(format!(
                "distributor notice version {} is older than {} in force",
                doc.version, prev.version
            ));
        }
    }
    let html = match doc.html.as_deref() {
        Some(raw) => Some(
            sanitize::sanitize_notice_html(raw)
                .map_err(|e| format!("distributor notice refused by the sanitizer: {e}"))?,
        ),
        None => None,
    };
    Ok(Accepted {
        subscription_id: subscription_id.to_string(),
        distributor_id: doc.distributor_id.clone(),
        version: doc.version,
        html,
    })
}

fn cache_path(home: &Path) -> PathBuf {
    home.join(CACHE_FILENAME)
}

/// The cached signed document, verified for `subscription_id`. `Ok(None)`
/// when there is no cache file.
pub fn read_cached(
    home: &Path,
    registry: &PublicKeyRegistry,
    subscription_id: &str,
) -> Result<Option<Accepted>, String> {
    let Ok(raw) = std::fs::read_to_string(cache_path(home)) else {
        return Ok(None);
    };
    let doc = serde_json::from_str::<SignedDistributorNotice>(&raw).map_err(|e| e.to_string())?;
    accept(registry, subscription_id, &doc, None).map(Some)
}

/// Load the cached signed document at start. Anything that does not verify
/// for the installed license is ignored (and left for the next fetch to
/// replace).
pub fn load_cached(home: &Path, registry: &PublicKeyRegistry, subscription_id: Option<&str>) {
    let Some(subscription_id) = subscription_id else {
        set(None);
        return;
    };
    match read_cached(home, registry, subscription_id) {
        Ok(accepted) => set(accepted),
        Err(error) => {
            warn!(%error, path = %cache_path(home).display(), "cached distributor notice ignored");
            set(None);
        }
    }
}

/// Forget the runtime notice (license replaced or removed): the compiled
/// notice applies until the next fetch.
pub fn reset(home: &Path) {
    set(None);
    let path = cache_path(home);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(error = %e, "could not remove cached distributor notice");
        }
    }
}

fn store(home: &Path, doc: &SignedDistributorNotice) -> Result<(), String> {
    let path = cache_path(home);
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// What one fetch should do with the control-plane's answer.
#[derive(Debug, PartialEq, Eq)]
pub enum FeedOutcome {
    /// A verified document to store and show.
    Accept(Accepted),
    /// The license has no distributor: drop any runtime notice.
    NoOpinion,
}

/// Interpret a response body (pure; network and disk stay with the caller).
pub fn interpret(
    registry: &PublicKeyRegistry,
    subscription_id: &str,
    nonce: &str,
    body: &serde_json::Value,
    previous: Option<&Accepted>,
) -> Result<(FeedOutcome, Option<SignedDistributorNotice>), String> {
    if body.get("nonce").and_then(|v| v.as_str()) != Some(nonce) {
        return Err("distributor notice response nonce missing or mismatched".into());
    }
    match body.get("distributor_notice") {
        None | Some(serde_json::Value::Null) => Ok((FeedOutcome::NoOpinion, None)),
        Some(value) => {
            let doc: SignedDistributorNotice = serde_json::from_value(value.clone())
                .map_err(|e| format!("distributor notice malformed: {e}"))?;
            if doc.nonce != nonce {
                return Err("distributor notice was signed for another request".into());
            }
            let accepted = accept(registry, subscription_id, &doc, previous)?;
            Ok((FeedOutcome::Accept(accepted), Some(doc)))
        }
    }
}

/// One fetch against the control-plane.
pub async fn fetch_once(
    control_url: &str,
    home: &Path,
    registry: &PublicKeyRegistry,
    subscription_id: &str,
    machine_fingerprint: &str,
) -> Result<(), String> {
    let nonce = uuid::Uuid::new_v4().to_string();
    let endpoint = format!(
        "{}/v1/license/distributor-notice",
        control_url.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(&endpoint)
        .json(&serde_json::json!({
            "subscription_id": subscription_id,
            "machine_fingerprint": machine_fingerprint,
            "nonce": nonce,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        // A control-plane that predates this endpoint: keep what we have.
        return Err(format!("control-plane has no notice endpoint ({status})"));
    }
    if !status.is_success() {
        return Err(format!("control-plane returned HTTP {status}"));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err("response too large".into());
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let previous = current_accepted();
    let (outcome, doc) = interpret(registry, subscription_id, &nonce, &body, previous.as_ref())?;
    match (outcome, doc) {
        (FeedOutcome::Accept(accepted), Some(doc)) => {
            store(home, &doc)?;
            if previous.as_ref() != Some(&accepted) {
                info!(
                    distributor = %accepted.distributor_id,
                    version = accepted.version,
                    shown = accepted.html.is_some(),
                    "distributor notice updated"
                );
            }
            set(Some(accepted));
        }
        _ => reset(home),
    }
    Ok(())
}

/// Fetch right away (after a new license is installed), in the background.
pub fn spawn_fetch_now(runtime: crate::license_runtime::LicenseRuntime) {
    tokio::spawn(async move {
        if let Some((subscription_id, fingerprint)) = runtime.notice_identity().await {
            if let Err(error) = fetch_once(
                runtime.control_url(),
                runtime.home_dir(),
                runtime.registry(),
                &subscription_id,
                &fingerprint,
            )
            .await
            {
                warn!(%error, "distributor notice fetch after license install failed");
            }
        }
    });
}

/// Background loop, spawned with the other license tasks.
pub async fn poll_loop(runtime: crate::license_runtime::LicenseRuntime) {
    tokio::time::sleep(FIRST_POLL_DELAY).await;
    loop {
        if let Some((subscription_id, fingerprint)) = runtime.notice_identity().await {
            if let Err(error) = fetch_once(
                runtime.control_url(),
                runtime.home_dir(),
                runtime.registry(),
                &subscription_id,
                &fingerprint,
            )
            .await
            {
                warn!(%error, "distributor notice fetch failed; keeping the current one");
            }
        } else {
            debug!("no license installed; distributor notice fetch skipped");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
    use chrono::Utc;
    use duduclaw_license::notice::{NOTICE_KIND, notice_sha256};
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> (SigningKey, PublicKeyRegistry) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let registry =
            PublicKeyRegistry::new().with_key("v1", key.verifying_key().to_bytes().to_vec());
        (key, registry)
    }

    fn doc(key: &SigningKey, version: i64, html: Option<&str>) -> SignedDistributorNotice {
        let mut d = SignedDistributorNotice {
            kind: NOTICE_KIND.into(),
            subscription_id: "sub_1".into(),
            distributor_id: "dist_1".into(),
            version,
            html: html.map(str::to_string),
            sha256: html.map(notice_sha256),
            issued_at: Utc::now(),
            nonce: "n1".into(),
            public_key_id: "v1".into(),
            signature: String::new(),
        };
        d.signature = BASE64.encode(key.sign(&d.canonical_bytes().unwrap()).to_bytes());
        d
    }

    #[test]
    fn accept_resanitizes_and_keeps_signed_clear() {
        let (key, registry) = key();
        let a = accept(&registry, "sub_1", &doc(&key, 2, Some("<p>hi</p>")), None).unwrap();
        assert_eq!(a.html.as_deref(), Some("<p>hi</p>"));
        let cleared = accept(&registry, "sub_1", &doc(&key, 3, None), Some(&a)).unwrap();
        assert_eq!(cleared.html, None);
    }

    #[test]
    fn older_version_and_other_subscription_are_refused() {
        let (key, registry) = key();
        let current = accept(&registry, "sub_1", &doc(&key, 5, Some("<p>v5</p>")), None).unwrap();
        assert!(
            accept(
                &registry,
                "sub_1",
                &doc(&key, 4, Some("<p>v4</p>")),
                Some(&current)
            )
            .is_err()
        );
        assert!(
            accept(
                &registry,
                "sub_1",
                &doc(&key, 5, Some("<p>v5</p>")),
                Some(&current)
            )
            .is_ok()
        );
        assert!(accept(&registry, "sub_2", &doc(&key, 6, Some("<p>x</p>")), None).is_err());
    }

    #[test]
    fn signed_html_the_sanitizer_refuses_is_not_shown() {
        let (key, registry) = key();
        assert!(
            accept(
                &registry,
                "sub_1",
                &doc(&key, 1, Some("<script>x</script>")),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn interpret_checks_nonce_and_null_means_no_opinion() {
        let (key, registry) = key();
        let signed = serde_json::to_value(doc(&key, 1, Some("<p>x</p>"))).unwrap();
        let body = serde_json::json!({ "nonce": "n1", "distributor_notice": signed });
        assert!(interpret(&registry, "sub_1", "other", &body, None).is_err());
        // A signed document from an earlier request, under a fresh envelope nonce.
        let replay =
            serde_json::json!({ "nonce": "n2", "distributor_notice": body["distributor_notice"] });
        assert!(interpret(&registry, "sub_1", "n2", &replay, None).is_err());
        let (outcome, stored) = interpret(&registry, "sub_1", "n1", &body, None).unwrap();
        assert!(
            matches!(outcome, FeedOutcome::Accept(ref a) if a.html.as_deref() == Some("<p>x</p>"))
        );
        assert!(stored.is_some());
        let none = serde_json::json!({ "nonce": "n1", "distributor_notice": null });
        assert_eq!(
            interpret(&registry, "sub_1", "n1", &none, None).unwrap().0,
            FeedOutcome::NoOpinion
        );
    }

    #[test]
    fn edited_cache_file_is_ignored_on_load() {
        let (key, registry) = key();
        let home = tempfile::tempdir().unwrap();
        let mut d = doc(&key, 1, Some("<p>real</p>"));
        store(home.path(), &d).unwrap();
        let loaded = read_cached(home.path(), &registry, "sub_1")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.html.as_deref(), Some("<p>real</p>"));
        assert!(read_cached(home.path(), &registry, "sub_2").is_err());

        d.html = Some("<p>phish</p>".into());
        d.sha256 = Some(notice_sha256("<p>phish</p>"));
        store(home.path(), &d).unwrap();
        assert!(
            read_cached(home.path(), &registry, "sub_1").is_err(),
            "tampered cache is refused"
        );

        std::fs::remove_file(home.path().join(CACHE_FILENAME)).unwrap();
        assert_eq!(read_cached(home.path(), &registry, "sub_1").unwrap(), None);
    }
}
