use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use duduclaw_license::PublicKeyRegistry;
use super::document::SignedDistributorBrand;
use tracing::{debug, info, warn};

use super::Brand;

/// Cache file under the gateway home.
pub const CACHE_FILENAME: &str = "distributor_brand.json";
/// Delay before the first fetch after start.
const FIRST_POLL_DELAY: Duration = Duration::from_secs(60);
/// Fetch cadence.
pub const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Two 128 KiB raster images, base64 encoded, plus bounded JSON overhead.
const MAX_RESPONSE_BYTES: usize = 384 * 1024;

/// A verified brand in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub subscription_id: String,
    pub distributor_id: String,
    pub version: i64,
    /// Validated name and raster data URIs.
    pub brand: Brand,
}

static VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

static FETCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

static ACCEPTED: RwLock<Option<Accepted>> = RwLock::new(None);

/// The verified runtime identity, or original-brand fallback.
pub fn current() -> Option<Accepted> {
    if !VISIBLE.load(std::sync::atomic::Ordering::Acquire) { return None; }
    ACCEPTED
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

fn set(value: Option<Accepted>) {
    VISIBLE.store(true, std::sync::atomic::Ordering::Release);
    if let Ok(mut guard) = ACCEPTED.write() {
        *guard = value;
    }
}

fn current_accepted() -> Option<Accepted> {
    ACCEPTED.read().ok().and_then(|guard| guard.clone())
}

/// Verify a signed document for `subscription_id` and turn it into the
/// brand to show. Refuses older versions and invalid names or images.
pub fn accept(
    registry: &PublicKeyRegistry,
    subscription_id: &str,
    doc: &SignedDistributorBrand,
    previous: Option<&Accepted>,
) -> Result<Accepted, String> {
    doc.verify_for(registry, subscription_id)
        .map_err(|e| format!("distributor brand refused: {e}"))?;
    if let Some(prev) = previous {
        if prev.subscription_id == subscription_id
            && prev.distributor_id == doc.distributor_id
            && doc.version < prev.version
        {
            return Err(format!(
                "distributor brand version {} is older than {} in force",
                doc.version, prev.version
            ));
        }
    }
    let brand = Brand {
        product_name: doc.product_name.clone(),
        logo_light: doc.logo_light.clone(),
        logo_dark: doc.logo_dark.clone(),
    };
    Ok(Accepted {
        subscription_id: subscription_id.to_string(),
        distributor_id: doc.distributor_id.clone(),
        version: doc.version,
        brand,
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
    use std::io::Read;
    let file = match std::fs::File::open(cache_path(home)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut raw = Vec::new();
    file.take(MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
    if raw.len() > MAX_RESPONSE_BYTES {
        return Err("brand cache too large".into());
    }
    let doc = serde_json::from_slice::<SignedDistributorBrand>(&raw).map_err(|e| e.to_string())?;
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
            warn!(%error, path = %cache_path(home).display(), "cached distributor brand ignored");
            set(None);
        }
    }
}

/// Forget the runtime brand after license replacement/removal.
pub fn reset(home: &Path) {
    set(None);
    let path = cache_path(home);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            warn!(error = %e, "could not remove cached distributor brand");
        }
    }
}

fn store(home: &Path, doc: &SignedDistributorBrand) -> Result<(), String> {
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
    /// The license has no distributor: use the original brand.
    NoOpinion,
}

/// Interpret a response body (pure; network and disk stay with the caller).
pub fn interpret(
    registry: &PublicKeyRegistry,
    subscription_id: &str,
    nonce: &str,
    body: &serde_json::Value,
    previous: Option<&Accepted>,
) -> Result<(FeedOutcome, Option<SignedDistributorBrand>), String> {
    if body.get("nonce").and_then(|v| v.as_str()) != Some(nonce) {
        return Err("distributor brand response nonce missing or mismatched".into());
    }
    match body.get("distributor_brand") {
        None | Some(serde_json::Value::Null) => Ok((FeedOutcome::NoOpinion, None)),
        Some(value) => {
            let doc: SignedDistributorBrand = serde_json::from_value(value.clone())
                .map_err(|e| format!("distributor brand malformed: {e}"))?;
            if doc.nonce != nonce {
                return Err("distributor brand was signed for another request".into());
            }
            let accepted = accept(registry, subscription_id, &doc, previous)?;
            Ok((FeedOutcome::Accept(accepted), Some(doc)))
        }
    }
}

/// One fetch against the control-plane.
async fn fetch_inner(
    control_url: &str,
    home: &Path,
    registry: &PublicKeyRegistry,
    subscription_id: &str,
    machine_fingerprint: &str,
) -> Result<(), String> {
    let nonce = uuid::Uuid::new_v4().to_string();
    let endpoint = format!(
        "{}/v1/license/distributor-brand",
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
        // A control-plane that predates this endpoint cannot provide a brand.
        return Err(format!("control-plane has no brand endpoint ({status})"));
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
                    "distributor brand updated"
                );
            }
            set(Some(accepted));
        }
        _ => reset(home),
    }
    Ok(())
}

/// Fail closed on any failed refresh, while retaining the rollback watermark.
pub async fn fetch_once(
    control_url: &str, home: &Path, registry: &PublicKeyRegistry,
    subscription_id: &str, machine_fingerprint: &str,
) -> Result<(), String> {
    let _fetch_guard = FETCH_LOCK.lock().await;
    let result = fetch_inner(control_url, home, registry, subscription_id, machine_fingerprint).await;
    if result.is_err() {
        VISIBLE.store(false, std::sync::atomic::Ordering::Release);
    }
    result
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
                warn!(%error, "distributor brand fetch after license install failed");
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
                warn!(%error, "distributor brand fetch failed; using the original brand");
            }
        } else {
            debug!("no license installed; distributor brand fetch skipped");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

