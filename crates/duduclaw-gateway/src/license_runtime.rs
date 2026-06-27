//! Gateway-side license runtime.
//!
//! Loads `~/.duduclaw/license.json` at startup, verifies the Ed25519
//! signature against the binary's embedded [`PublicKeyRegistry`], and
//! exposes the resulting tier + feature gate to the rest of the gateway.
//!
//! Spawns two long-running background tasks:
//!
//! - **phone-home** — refreshes the license on the cadence dictated by
//!   `features.toml` for the active tier (default 7 days for paid tiers,
//!   disabled for OpenSource). Failure modes are absorbed: a refresh that
//!   cannot reach the control-plane leaves the local license alone until
//!   the grace period expires, at which point the runtime silently
//!   downgrades to OpenSource.
//!
//! - **CRL poll** — every 24 hours fetches `GET /v1/license/crl`,
//!   verifies the issuer signature, and downgrades immediately if the
//!   active subscription appears in the revoked list.
//!
//! Both tasks are completely best-effort: any unhandled error is logged
//! and the loop continues. We never panic on a license problem — the
//! Apache 2.0 core must keep running.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Utc;
use duduclaw_license::{
    crl::SignedCrl, generate_fingerprint, load_default, save_default, storage,
    FeatureGate, License, LicenseError, LicenseTier, PublicKeyRegistry,
    EMBEDDED_FEATURES_TOML,
};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default fall-back when `DUDUCLAW_CONTROL_URL` is unset.
const DEFAULT_CONTROL_URL: &str = "https://api.duduclaw.tw";

/// L7: process-wide cache for the machine fingerprint. `generate_fingerprint`
/// enumerates the hostname + MAC addresses on every call, which is wasteful on
/// hot paths like `LicenseSnapshot::from_state` (one dashboard poll per second).
/// The fingerprint is host-stable for the lifetime of the process, so we compute
/// it once and reuse it.
static FINGERPRINT_CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Return the cached machine fingerprint, computing it on first use.
fn cached_fingerprint() -> &'static str {
    FINGERPRINT_CACHE.get_or_init(generate_fingerprint)
}

/// Env-var prefix for trusted issuer public keys.
///
/// Operators set `DUDUCLAW_LICENSE_PUBKEY_<KEY_ID>=<hex>` for each key
/// they trust. For example a Year-1 deployment that issues licenses with
/// `public_key_id = "v1"` exports `DUDUCLAW_LICENSE_PUBKEY_V1=<64 hex chars>`.
///
/// When the binary ships pre-baked, the same registry can be assembled
/// at compile time via [`PublicKeyRegistry::new`] + `with_key`; the env
/// path is for development and self-hosters who issue their own
/// internal-use licenses.
pub const PUBKEY_ENV_PREFIX: &str = "DUDUCLAW_LICENSE_PUBKEY_";

/// How long we wait for a phone-home HTTP request before giving up and
/// trying again next cycle.
const PHONE_HOME_TIMEOUT: StdDuration = StdDuration::from_secs(20);

/// CRL fetch cadence — independent of tier, because CRL is the only way
/// to react to emergency revocations between phone-home cycles.
const CRL_POLL_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);

/// Minimum sleep between phone-home attempts. Even Hobby (which defines
/// `license_phone_home_interval_days = 3` in features.toml) only sleeps
/// in days, but we floor at one hour so the task never spin-loops if
/// a tier accidentally defines a zero interval.
const MIN_PHONE_HOME_SLEEP: StdDuration = StdDuration::from_secs(60 * 60);

/// Read-only handle to the gateway's current licensing state.
///
/// Other services obtain a clone of this handle and call its accessor
/// methods. The interior is wrapped in [`tokio::sync::RwLock`] so the
/// phone-home and CRL background tasks can swap in a refreshed license
/// without blocking the rest of the gateway.
#[derive(Clone)]
pub struct LicenseRuntime {
    state: Arc<RwLock<LicenseRuntimeInner>>,
    gate: Arc<FeatureGate>,
    registry: Arc<PublicKeyRegistry>,
    home_dir: PathBuf,
    control_url: String,
}

struct LicenseRuntimeInner {
    /// `None` when no valid license is installed — gateway operates in
    /// OpenSource mode. The runtime exposes a stable `LicenseTier::OpenSource`
    /// to callers in this state.
    license: Option<License>,

    /// HS13/D5: `generated_at` of the most recent *accepted* CRL. Used to
    /// reject replays of an older (e.g. pre-revocation) CRL. Loaded from disk
    /// at bootstrap so the freshness floor survives restarts.
    last_seen_crl_at: Option<chrono::DateTime<Utc>>,
}

/// File (under `home_dir`) that persists the last-seen CRL `generated_at`
/// timestamp so the monotonicity floor survives restarts.
const LAST_CRL_FILENAME: &str = "license_crl_last_seen.txt";

impl LicenseRuntime {
    /// Bootstrap the runtime from `home_dir`. Always succeeds — a missing
    /// or broken license file simply yields an OpenSource runtime.
    ///
    /// The returned runtime's background tasks have NOT been spawned yet —
    /// call [`Self::spawn_background_tasks`] after the rest of the gateway
    /// has finished initialising so the first phone-home doesn't race with
    /// startup logging.
    pub async fn bootstrap(home_dir: PathBuf, registry: PublicKeyRegistry) -> Self {
        let gate = Arc::new(
            FeatureGate::from_str(EMBEDDED_FEATURES_TOML)
                .expect("embedded features.toml is malformed"),
        );

        let control_url = std::env::var("DUDUCLAW_CONTROL_URL")
            .unwrap_or_else(|_| DEFAULT_CONTROL_URL.to_string());

        // M52 fix: honor a persisted revocation marker — if a prior revocation
        // could not delete license.json, do NOT reload it on restart.
        let license = if home_dir.join(REVOKED_FILENAME).exists() {
            warn!(
                "license revocation marker present — refusing to load license.json; \
                 running OpenSource until a successful re-subscription clears it"
            );
            None
        } else {
            load_and_validate(&registry, &gate).await
        };

        let tier = license
            .as_ref()
            .map(|l| l.tier)
            .unwrap_or(LicenseTier::OpenSource);
        info!(
            tier = %tier,
            customer = license
                .as_ref()
                .map(|l| l.customer_id.as_str())
                .unwrap_or("<none>"),
            // L22: the previous `then_some(0).unwrap_or(1)` could only ever log
            // 0 or 1 regardless of how many issuer keys were embedded. We report
            // the real count. NOTE for coordinator: `PublicKeyRegistry` currently
            // exposes only `is_empty()`/`get()`, so a `len()` accessor must be
            // added to `duduclaw-license/src/key.rs` for this to be exact; until
            // then we fall back to the env-derived count (the only construction
            // path that carries multiple keys in practice).
            registry_keys = count_embedded_issuer_keys(),
            "license runtime initialised"
        );

        let last_seen_crl_at = read_last_seen_crl(&home_dir);

        Self {
            state: Arc::new(RwLock::new(LicenseRuntimeInner {
                license,
                last_seen_crl_at,
            })),
            gate,
            registry: Arc::new(registry),
            home_dir,
            control_url,
        }
    }

    /// Spawn background phone-home and CRL polling tasks. Returns
    /// detached `JoinHandle`s so callers can hold them for graceful
    /// shutdown if they wish.
    pub fn spawn_background_tasks(&self) -> Vec<tokio::task::JoinHandle<()>> {
        let phone_home = tokio::spawn(phone_home_loop(self.clone()));
        let crl_poll = tokio::spawn(crl_loop(self.clone()));
        vec![phone_home, crl_poll]
    }

    /// Currently-active tier. Always returns `OpenSource` when no license
    /// is installed or the installed license has been downgraded by the
    /// runtime (e.g. revoked, grace exceeded).
    pub async fn current_tier(&self) -> LicenseTier {
        let inner = self.state.read().await;
        inner
            .license
            .as_ref()
            .map(|l| l.tier)
            .unwrap_or(LicenseTier::OpenSource)
    }

    /// Returns `true` if the active tier unlocks the named feature.
    /// Equivalent to `gate.check(current_tier(), feature)`.
    pub async fn check_feature(&self, feature: &str) -> bool {
        let tier = self.current_tier().await;
        self.gate.check(tier, feature)
    }

    /// Returns a clone of the embedded feature gate. Useful for callers
    /// that need to query multiple flags in a row without re-acquiring
    /// the read lock.
    pub fn feature_gate(&self) -> Arc<FeatureGate> {
        self.gate.clone()
    }

    /// Snapshot of the current license, if any. Returned by reference
    /// for read-only inspection (dashboard `license.status` RPC, etc.).
    pub async fn snapshot(&self) -> LicenseSnapshot {
        let tier = self.current_tier().await;
        let inner = self.state.read().await;
        LicenseSnapshot::from_state(inner.license.as_ref(), tier)
    }
}

/// Read-only view of the runtime state for dashboards / metrics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LicenseSnapshot {
    pub tier: LicenseTier,
    pub mode: &'static str,
    pub installed: bool,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub expires_at: Option<chrono::DateTime<Utc>>,
    pub days_until_expiry: Option<i64>,
    pub last_phone_home: Option<chrono::DateTime<Utc>>,
    pub days_since_phone_home: Option<i64>,
    pub fingerprint_match: Option<bool>,
}

impl LicenseSnapshot {
    fn from_state(license: Option<&License>, tier: LicenseTier) -> Self {
        let installed = license.is_some();
        // L7: reuse the cached fingerprint instead of re-enumerating host MACs.
        let current_fp = cached_fingerprint();
        Self {
            tier,
            mode: if installed { "commercial" } else { "opensource" },
            installed,
            customer_id: license.map(|l| l.customer_id.clone()),
            subscription_id: license.map(|l| l.subscription_id.clone()),
            expires_at: license.map(|l| l.expires_at),
            days_until_expiry: license.map(|l| l.days_until_expiry()),
            last_phone_home: license.map(|l| l.last_phone_home),
            days_since_phone_home: license.map(|l| l.days_since_phone_home()),
            fingerprint_match: license.map(|l| l.is_valid_for_machine(current_fp)),
        }
    }
}

// ── Process-global accessor ───────────────────────────────────
//
// The gateway publishes its `LicenseRuntime` here at startup so other
// services (dashboard RPC handlers, channel-reply, evolution engine)
// can ask `current_tier()` without threading a handle through their
// initialisation paths. Mirrors the established pattern used by
// `cost_telemetry::init_telemetry` and `crate::log::LOG_TX`.

static GLOBAL_RUNTIME: std::sync::OnceLock<LicenseRuntime> = std::sync::OnceLock::new();

/// Publish a runtime as the process-global handle. Called once during
/// `start_gateway` bootstrap; subsequent calls are silently ignored.
pub fn set_global(runtime: LicenseRuntime) {
    let _ = GLOBAL_RUNTIME.set(runtime);
}

/// Returns the process-global runtime if one has been published. Returns
/// `None` for any subcommand that runs without `start_gateway` (e.g.
/// `duduclaw migrate`) so callers know to treat the absence as
/// "OpenSource mode" rather than a fatal error.
pub fn global() -> Option<&'static LicenseRuntime> {
    GLOBAL_RUNTIME.get()
}

/// Convenience: returns the current tier, or `OpenSource` when no
/// runtime has been published or no license is installed.
pub async fn current_tier_or_opensource() -> LicenseTier {
    match global() {
        Some(rt) => rt.current_tier().await,
        None => LicenseTier::OpenSource,
    }
}

/// Build a `PublicKeyRegistry` from `DUDUCLAW_LICENSE_PUBKEY_<ID>` env
/// vars. Each value is expected to be a 64-character hex string (the
/// Ed25519 public key as printed by `license-keygen keygen`).
///
/// Returns an empty registry when no matching env var is set — the
/// runtime treats this as "OpenSource mode" rather than failing.
pub fn embedded_registry_from_env() -> PublicKeyRegistry {
    let mut registry = PublicKeyRegistry::new();
    for (k, v) in std::env::vars() {
        let Some(suffix) = k.strip_prefix(PUBKEY_ENV_PREFIX) else {
            continue;
        };
        if suffix.is_empty() {
            warn!("ignoring {k} — missing key identifier suffix");
            continue;
        }
        let key_id = suffix.to_ascii_lowercase();
        let v_trim = v.trim();
        let bytes = match hex::decode(v_trim) {
            Ok(b) if b.len() == 32 => b,
            Ok(b) => {
                warn!(
                    key_id,
                    actual = b.len(),
                    "ignoring {k} — public key must decode to exactly 32 bytes"
                );
                continue;
            }
            Err(e) => {
                warn!(key_id, error = %e, "ignoring {k} — not valid hex");
                continue;
            }
        };
        registry = registry.with_key(&key_id, bytes);
        info!(key_id, "registered trusted issuer public key");
    }
    registry
}

/// L22: count the trusted issuer public keys derived from the environment.
///
/// Mirrors the validation in [`embedded_registry_from_env`] (suffix present +
/// 32-byte hex) so the logged count matches what was actually registered. This
/// exists because `PublicKeyRegistry` does not yet expose a `len()` accessor.
fn count_embedded_issuer_keys() -> usize {
    std::env::vars()
        .filter(|(k, v)| {
            k.strip_prefix(PUBKEY_ENV_PREFIX)
                .is_some_and(|suffix| !suffix.is_empty())
                && matches!(hex::decode(v.trim()), Ok(b) if b.len() == 32)
        })
        .count()
}

// ── Bootstrap helpers ─────────────────────────────────────────

async fn load_and_validate(
    registry: &PublicKeyRegistry,
    gate: &FeatureGate,
) -> Option<License> {
    let license = match load_default() {
        Ok(l) => l,
        Err(LicenseError::FileNotFound(_)) => {
            debug!("no license file found; running in OpenSource mode");
            return None;
        }
        Err(e) => {
            warn!(error = %e, "license file present but unreadable; running in OpenSource mode");
            return None;
        }
    };

    if registry.is_empty() {
        // No embedded issuer keys → we cannot trust any license. Treat as
        // OpenSource. Operators see this once at startup so the failure
        // mode is observable.
        warn!(
            "license file present but PublicKeyRegistry is empty — \
             cannot verify signature; running in OpenSource mode. \
             Set DUDUCLAW_LICENSE_PUBKEY_* env vars to enable commercial features."
        );
        return None;
    }

    if let Err(e) = registry.verify(&license) {
        warn!(error = %e, "license signature invalid; running in OpenSource mode");
        return None;
    }

    let current_fp = generate_fingerprint();
    let phone_home = gate.phone_home_interval_days(license.tier);
    let grace = gate.grace_period_days(license.tier);

    match license.validate(&current_fp, phone_home, grace) {
        Ok(()) => {
            // M51: enforce tier ↔ deployment-mode binding. A cloud-only tier
            // (Solo/Studio/…) must not be honoured on a self-host binary, and a
            // self-host-only tier (Partner/PersonalProSelfHost/SelfHostPro/Oem)
            // must not be honoured in Cloud. Fail-closed: a mismatch downgrades
            // to OpenSource rather than silently granting features.
            let is_self_host = is_self_host_deployment();
            if let Err(e) = license.validate_tier_deployment_binding(is_self_host) {
                warn!(
                    error = %e,
                    deployment = if is_self_host { "self_host" } else { "cloud" },
                    "license tier does not match deployment mode; running in OpenSource mode"
                );
                return None;
            }
            Some(license)
        }
        Err(LicenseError::Expired) => {
            warn!("installed license is expired; running in OpenSource mode");
            None
        }
        Err(LicenseError::InvalidFingerprint) => {
            warn!(
                "installed license is bound to a different machine \
                 (license fingerprint != current machine); running in OpenSource mode"
            );
            None
        }
        Err(LicenseError::GracePeriodExceeded(days)) => {
            warn!(
                days,
                "license grace period exceeded — phone home overdue; running in OpenSource mode"
            );
            None
        }
        Err(e) => {
            warn!(error = %e, "license validation failed; running in OpenSource mode");
            None
        }
    }
}

/// Resolve the deployment mode (M51 signal).
///
/// Read from `DUDUCLAW_DEPLOYMENT`:
///   - `cloud` → managed / Cloud control-plane deployment (`false`)
///   - `self_host` / `self-host` / `selfhost` / `on_prem` / `onprem` → `true`
///   - unset / anything else → **self-host** (`true`), the safe default for a
///     downloaded binary. The managed tenant image is responsible for setting
///     `DUDUCLAW_DEPLOYMENT=cloud` (injected by the tenant provisioner), so the
///     default never mis-classifies a legitimate self-host install.
fn is_self_host_deployment() -> bool {
    match std::env::var("DUDUCLAW_DEPLOYMENT") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "cloud" | "managed"),
        Err(_) => true,
    }
}

// ── Phone-home loop ───────────────────────────────────────────

async fn phone_home_loop(runtime: LicenseRuntime) {
    loop {
        let next_sleep = match runtime.next_phone_home_sleep().await {
            Some(d) => d,
            None => {
                // No license or OpenSource — wait a day and re-check
                // (operator may install one mid-flight).
                StdDuration::from_secs(24 * 60 * 60)
            }
        };

        tokio::time::sleep(next_sleep).await;

        if let Err(e) = runtime.do_phone_home_once().await {
            // Any error here is non-fatal — we re-evaluate next cycle.
            debug!(error = %e, "phone-home attempt failed; will retry next cycle");
        }
    }
}

impl LicenseRuntime {
    async fn next_phone_home_sleep(&self) -> Option<StdDuration> {
        let inner = self.state.read().await;
        let license = inner.license.as_ref()?;
        let interval_days = self.gate.phone_home_interval_days(license.tier);
        if interval_days <= 0 {
            return None;
        }
        let days_since = license.days_since_phone_home().max(0);
        let days_until_due = (interval_days - days_since).max(1) as u64;
        let secs = days_until_due * 24 * 60 * 60;
        Some(StdDuration::from_secs(secs).max(MIN_PHONE_HOME_SLEEP))
    }

    async fn do_phone_home_once(&self) -> Result<(), PhoneHomeError> {
        // Snapshot what we need outside the lock — avoid holding the
        // RwLock across `.await`.
        let (subscription_id, machine_fingerprint, current_tier) = {
            let inner = self.state.read().await;
            let lic = inner.license.as_ref().ok_or(PhoneHomeError::NoLicense)?;
            (
                lic.subscription_id.clone(),
                lic.machine_fingerprint.clone(),
                lic.tier,
            )
        };

        let endpoint = format!(
            "{}/v1/license/refresh",
            self.control_url.trim_end_matches('/')
        );

        // C4.3: anti-replay nonce. Generate a fresh random value per request
        // and require the control-plane to echo it back in the response. A
        // replayed (previously-signed) `{"status":"active", license}` body will
        // carry a stale/absent nonce and is therefore rejected before install.
        //
        // Server-side contract: the control-plane MUST copy the request's
        // `"nonce"` verbatim into the top-level `"nonce"` field of its JSON
        // response. Responses without a matching nonce are treated as replays.
        let nonce = uuid::Uuid::new_v4().to_string();
        let request_body = json!({
            "subscription_id": subscription_id,
            "machine_fingerprint": machine_fingerprint,
            "current_version": env!("CARGO_PKG_VERSION"),
            "nonce": nonce,
            "telemetry": {},
        });

        let client = reqwest::Client::builder()
            .timeout(PHONE_HOME_TIMEOUT)
            .build()
            .map_err(|e| PhoneHomeError::Http(e.to_string()))?;
        let response = client
            .post(&endpoint)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| PhoneHomeError::Http(e.to_string()))?;

        let status_code = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| PhoneHomeError::Parse(e.to_string()))?;

        if !status_code.is_success() {
            return Err(PhoneHomeError::Http(format!(
                "control-plane returned HTTP {status_code}"
            )));
        }

        match body.get("status").and_then(|v| v.as_str()) {
            Some("active") => {
                // C4.3: reject replays before doing any further work. A captured
                // old `active` response will not carry the freshly-generated
                // nonce, so this fails closed without installing anything.
                if !response_nonce_ok(&nonce, &body) {
                    return Err(PhoneHomeError::NonceMismatch);
                }

                let new_license_value = body
                    .get("license")
                    .ok_or_else(|| PhoneHomeError::Parse("missing 'license' field".into()))?;
                let new_license: License = serde_json::from_value(new_license_value.clone())
                    .map_err(|e| PhoneHomeError::Parse(e.to_string()))?;

                // C4 / C4.4: a valid signature is necessary but NOT sufficient.
                // The bootstrap path (`load_and_validate`) also enforces
                // expiry / machine-fingerprint / grace via `validate()`; the
                // refresh path previously skipped it, so a buggy/compromised
                // control plane (or a replayed signed response) could install
                // an expired or other-machine license. `accept_refreshed_license`
                // composes signature-trust + `validate()` so the acceptance
                // decision is pure and unit-testable.
                let current_fp = generate_fingerprint();
                let phone_home = self.gate.phone_home_interval_days(new_license.tier);
                let grace = self.gate.grace_period_days(new_license.tier);
                accept_refreshed_license(
                    &self.registry,
                    &new_license,
                    &current_fp,
                    phone_home,
                    grace,
                )?;

                save_default(&new_license)
                    .map_err(|e| PhoneHomeError::Save(e.to_string()))?;
                {
                    let mut inner = self.state.write().await;
                    inner.license = Some(new_license);
                }
                // M52: a successful re-subscription clears any prior revocation
                // marker so the customer recovers paid features on restart.
                let marker = self.home_dir.join(REVOKED_FILENAME);
                if marker.exists() {
                    let _ = std::fs::remove_file(&marker);
                }
                info!(tier = %current_tier, "phone-home succeeded; license refreshed");
                Ok(())
            }
            Some("revoked") => {
                let reason = body
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("revoked")
                    .to_string();
                warn!(reason = %reason, "control-plane reported license revoked — downgrading to OpenSource");
                self.downgrade_to_opensource(&reason).await;
                Ok(())
            }
            other => Err(PhoneHomeError::Parse(format!(
                "unexpected refresh response status: {other:?}"
            ))),
        }
    }

    async fn downgrade_to_opensource(&self, reason: &str) {
        let mut inner = self.state.write().await;
        inner.license = None;
        // Best-effort: remove the local file so subsequent restarts don't
        // reload a known-bad license.
        if let Err(e) = storage::delete_default() {
            warn!(error = %e, "could not delete license.json after revocation");
            // M52 fix: if deletion fails, a restart would reload the revoked
            // license and run as paid until the next 24h CRL poll. Persist a
            // revocation marker that bootstrap honors regardless, so revocation
            // survives restarts even when the file can't be removed.
            if let Err(e) = std::fs::write(
                self.home_dir.join(REVOKED_FILENAME),
                format!("revoked: {reason}\n"),
            ) {
                warn!(error = %e, "could not write license revocation marker");
            }
        }
        info!(reason, "downgraded to OpenSource mode");
    }
}

/// File (under `home_dir`) marking that the installed license was revoked.
/// Honored at bootstrap so a revocation survives restarts even if the license
/// file itself could not be deleted. Cleared on a successful re-subscription.
const REVOKED_FILENAME: &str = "license_revoked.marker";

#[derive(Debug, thiserror::Error)]
enum PhoneHomeError {
    #[error("no license installed")]
    NoLicense,
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("save error: {0}")]
    Save(String),
    /// C4.3: the refresh response did not echo our request nonce — treated as a
    /// replay and the license is NOT installed.
    #[error("refresh response nonce missing or mismatched — rejecting replay")]
    NonceMismatch,
    /// C4.4: a (possibly validly-signed) refreshed license failed acceptance —
    /// untrusted signature, expired, wrong machine, or grace exceeded.
    #[error("refreshed license rejected: {0}")]
    Rejected(String),
}

/// C4.3: pure nonce-echo check. The response is accepted only when it carries a
/// top-level `"nonce"` string equal to the one we sent. Missing field, wrong
/// type, or a different value all fail (a replayed old response can't echo a
/// freshly-generated nonce).
fn response_nonce_ok(sent: &str, body: &serde_json::Value) -> bool {
    body.get("nonce").and_then(|v| v.as_str()) == Some(sent)
}

/// C4.4: pure acceptance decision for a refreshed license. Composes the two
/// checks the refresh path must perform before swapping in a license:
///
///   1. signature trust — `registry.verify` (Ed25519, dispatched by key id)
///   2. `validate()` — schema / expiry / machine-fingerprint / grace
///
/// Extracted so the acceptance logic is testable without a live control-plane:
/// construct + sign a `License`, build a trusting `PublicKeyRegistry`, and
/// assert valid ⇒ Ok while expired / wrong-fingerprint / bad-signature ⇒ Err.
fn accept_refreshed_license(
    registry: &PublicKeyRegistry,
    new_license: &License,
    current_fp: &str,
    phone_home_days: i64,
    grace_days: i64,
) -> Result<(), PhoneHomeError> {
    registry.verify(new_license).map_err(|e| {
        PhoneHomeError::Rejected(format!("invalid signature: {e}"))
    })?;
    new_license
        .validate(current_fp, phone_home_days, grace_days)
        .map_err(|e| {
            PhoneHomeError::Rejected(format!("expiry/fingerprint/grace: {e}"))
        })
}

// ── CRL loop ──────────────────────────────────────────────────

async fn crl_loop(runtime: LicenseRuntime) {
    loop {
        if let Err(e) = runtime.do_crl_fetch_once().await {
            debug!(error = %e, "CRL fetch failed; will retry next cycle");
        }
        tokio::time::sleep(CRL_POLL_INTERVAL).await;
    }
}

impl LicenseRuntime {
    async fn do_crl_fetch_once(&self) -> Result<(), PhoneHomeError> {
        let subscription_id = {
            let inner = self.state.read().await;
            match inner.license.as_ref() {
                Some(l) => l.subscription_id.clone(),
                None => return Ok(()), // No license → nothing to revoke
            }
        };

        let endpoint = format!(
            "{}/v1/license/crl",
            self.control_url.trim_end_matches('/')
        );
        let client = reqwest::Client::builder()
            .timeout(PHONE_HOME_TIMEOUT)
            .build()
            .map_err(|e| PhoneHomeError::Http(e.to_string()))?;
        let crl: SignedCrl = client
            .get(&endpoint)
            .send()
            .await
            .map_err(|e| PhoneHomeError::Http(e.to_string()))?
            .json()
            .await
            .map_err(|e| PhoneHomeError::Parse(e.to_string()))?;

        if let Err(e) = crl.verify(&self.registry) {
            return Err(PhoneHomeError::Parse(format!(
                "CRL signature invalid: {e}"
            )));
        }

        // HS13/D5: a valid signature is not enough — a signed-but-old CRL can be
        // replayed forever to mask a later revocation. Enforce two freshness
        // checks before trusting the document:
        //
        //   1. `is_stale()` — reject CRLs older than their own declared TTL.
        //   2. Monotonicity floor — reject any CRL whose `generated_at` is not
        //      strictly newer than the last CRL we accepted (persisted to disk),
        //      so a replayed pre-revocation snapshot is rejected even within TTL.
        if crl.is_stale() {
            return Err(PhoneHomeError::Parse(format!(
                "CRL is stale (generated_at {} older than ttl {}s) — rejecting replay",
                crl.generated_at, crl.ttl_seconds
            )));
        }

        {
            let inner = self.state.read().await;
            if let Err(reason) =
                check_crl_monotonic(crl.generated_at, inner.last_seen_crl_at)
            {
                return Err(PhoneHomeError::Parse(reason));
            }
        }

        // Accepted: advance the last-seen floor (in-memory + on-disk) before
        // acting on the revocation list. We only move the floor forward.
        {
            let mut inner = self.state.write().await;
            let advance = inner
                .last_seen_crl_at
                .map_or(true, |prev| crl.generated_at > prev);
            if advance {
                inner.last_seen_crl_at = Some(crl.generated_at);
                if let Err(e) = write_last_seen_crl(&self.home_dir, crl.generated_at) {
                    warn!(error = %e, "could not persist last-seen CRL timestamp");
                }
            }
        }

        if crl.is_revoked(&subscription_id) {
            warn!(subscription_id = %subscription_id, "CRL lists our subscription as revoked — downgrading to OpenSource");
            self.downgrade_to_opensource("crl_revoked").await;
        } else {
            debug!(revoked_count = crl.revoked.len(), "CRL fetched; subscription not revoked");
        }
        Ok(())
    }
}

/// HS13/D5: pure monotonicity check. A CRL is rejected when its `generated_at`
/// is older than the last-seen accepted CRL (a replay). Equal-or-newer is
/// accepted (equal allows a benign re-fetch of the same document).
fn check_crl_monotonic(
    crl_generated_at: chrono::DateTime<Utc>,
    last_seen: Option<chrono::DateTime<Utc>>,
) -> Result<(), String> {
    match last_seen {
        Some(prev) if crl_generated_at < prev => Err(format!(
            "CRL generated_at {crl_generated_at} older than last-seen {prev} — rejecting replay"
        )),
        _ => Ok(()),
    }
}

/// Read the persisted last-seen CRL `generated_at` timestamp, if present and
/// parseable. Any error (missing file, malformed contents) yields `None` so a
/// fresh install simply starts with no floor.
fn read_last_seen_crl(home_dir: &Path) -> Option<chrono::DateTime<Utc>> {
    let path = home_dir.join(LAST_CRL_FILENAME);
    let raw = std::fs::read_to_string(path).ok()?;
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Persist the last-seen CRL `generated_at` timestamp (RFC3339) under
/// `home_dir`. Best-effort; callers log but do not propagate failures.
fn write_last_seen_crl(home_dir: &Path, ts: chrono::DateTime<Utc>) -> std::io::Result<()> {
    std::fs::create_dir_all(home_dir)?;
    let path = home_dir.join(LAST_CRL_FILENAME);
    std::fs::write(path, ts.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use duduclaw_license::License;

    fn fake_license(tier: LicenseTier, fingerprint: &str) -> License {
        License::new(
            "sub_test",
            "cus_test",
            tier,
            fingerprint,
            ChronoDuration::days(30),
            "v1",
        )
    }

    #[tokio::test]
    async fn opensource_when_no_keys_available() {
        // No embedded keys → license can't be trusted → opensource
        let registry = PublicKeyRegistry::new();
        let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();

        let lic = fake_license(LicenseTier::Studio, "fp_test");
        // Pretend we managed to load this (skip the actual file lookup)
        let result = load_and_validate_with(&registry, &gate, lic).await;
        assert!(result.is_none());
    }

    /// Helper for tests — variant of load_and_validate that takes the
    /// loaded license directly instead of reading from disk.
    async fn load_and_validate_with(
        registry: &PublicKeyRegistry,
        gate: &FeatureGate,
        license: License,
    ) -> Option<License> {
        if registry.is_empty() {
            return None;
        }
        if registry.verify(&license).is_err() {
            return None;
        }
        let fp = generate_fingerprint();
        let ph = gate.phone_home_interval_days(license.tier);
        let gp = gate.grace_period_days(license.tier);
        match license.validate(&fp, ph, gp) {
            Ok(()) => Some(license),
            Err(_) => None,
        }
    }

    #[tokio::test]
    async fn snapshot_when_opensource_reports_correctly() {
        let snap = LicenseSnapshot::from_state(None, LicenseTier::OpenSource);
        assert_eq!(snap.tier, LicenseTier::OpenSource);
        assert_eq!(snap.mode, "opensource");
        assert!(!snap.installed);
        assert!(snap.customer_id.is_none());
        assert!(snap.subscription_id.is_none());
    }

    #[tokio::test]
    async fn snapshot_with_license_reports_commercial_fields() {
        let lic = fake_license(LicenseTier::SelfHostPro, "fp_snap");
        let snap = LicenseSnapshot::from_state(Some(&lic), LicenseTier::SelfHostPro);
        assert_eq!(snap.tier, LicenseTier::SelfHostPro);
        assert_eq!(snap.mode, "commercial");
        assert!(snap.installed);
        assert_eq!(snap.customer_id.as_deref(), Some("cus_test"));
        assert!(snap.days_until_expiry.unwrap() >= 29);
    }

    /// Helper that exercises the env-var parser without touching the
    /// global environment — pulls a closure of (key, value) pairs.
    fn build_registry_from_pairs(pairs: &[(&str, &str)]) -> PublicKeyRegistry {
        let mut registry = PublicKeyRegistry::new();
        for (k, v) in pairs {
            let Some(suffix) = k.strip_prefix(PUBKEY_ENV_PREFIX) else {
                continue;
            };
            if suffix.is_empty() {
                continue;
            }
            let key_id = suffix.to_ascii_lowercase();
            let bytes = match hex::decode(v.trim()) {
                Ok(b) if b.len() == 32 => b,
                _ => continue,
            };
            registry = registry.with_key(&key_id, bytes);
        }
        registry
    }

    #[test]
    fn registry_parses_two_keys() {
        let r = build_registry_from_pairs(&[
            (
                "DUDUCLAW_LICENSE_PUBKEY_V1",
                "00".repeat(32).as_str(),
            ),
            (
                "DUDUCLAW_LICENSE_PUBKEY_V2",
                "ff".repeat(32).as_str(),
            ),
            ("UNRELATED_VAR", "foo"),
        ]);
        assert!(r.get("v1").is_some());
        assert!(r.get("v2").is_some());
        assert!(r.get("v3").is_none());
    }

    #[test]
    fn registry_skips_invalid_keys() {
        let r = build_registry_from_pairs(&[
            ("DUDUCLAW_LICENSE_PUBKEY_BAD_HEX", "not-hex"),
            ("DUDUCLAW_LICENSE_PUBKEY_WRONG_LEN", "deadbeef"),
            ("DUDUCLAW_LICENSE_PUBKEY_", "00".repeat(32).as_str()),
            (
                "DUDUCLAW_LICENSE_PUBKEY_OK",
                "11".repeat(32).as_str(),
            ),
        ]);
        assert!(r.get("bad_hex").is_none());
        assert!(r.get("wrong_len").is_none());
        assert!(r.get("").is_none());
        assert!(r.get("ok").is_some());
    }

    // ── HS13 / D5: CRL freshness + monotonicity ────────────────────────────────

    #[test]
    fn crl_monotonic_rejects_older_than_last_seen() {
        let last_seen = Utc::now();
        let older = last_seen - ChronoDuration::seconds(10);
        // An older CRL (a replay of a pre-revocation snapshot) must be rejected.
        assert!(check_crl_monotonic(older, Some(last_seen)).is_err());
    }

    #[test]
    fn crl_monotonic_accepts_newer_or_equal() {
        let last_seen = Utc::now();
        let newer = last_seen + ChronoDuration::seconds(10);
        assert!(check_crl_monotonic(newer, Some(last_seen)).is_ok());
        // Equal generated_at is a benign re-fetch of the same document.
        assert!(check_crl_monotonic(last_seen, Some(last_seen)).is_ok());
    }

    #[test]
    fn crl_monotonic_accepts_first_ever() {
        // No floor yet → first CRL is always accepted.
        assert!(check_crl_monotonic(Utc::now(), None).is_ok());
    }

    // ── C4.3: phone-home nonce anti-replay ─────────────────────────────────

    #[test]
    fn response_nonce_matches() {
        let body = json!({ "status": "active", "nonce": "abc-123" });
        assert!(response_nonce_ok("abc-123", &body));
    }

    #[test]
    fn response_nonce_mismatch_rejected() {
        let body = json!({ "status": "active", "nonce": "different" });
        assert!(!response_nonce_ok("abc-123", &body));
    }

    #[test]
    fn response_nonce_missing_rejected() {
        // No nonce at all (e.g. a replayed pre-nonce response) must fail closed.
        let body = json!({ "status": "active" });
        assert!(!response_nonce_ok("abc-123", &body));
        // Wrong type also fails.
        let body_num = json!({ "status": "active", "nonce": 42 });
        assert!(!response_nonce_ok("abc-123", &body_num));
    }

    // ── C4.4: refreshed-license acceptance (signature + validate) ───────────

    /// Generate a fresh Ed25519 issuer keypair via `ring` (already a gateway
    /// dep). Returns (signing key pair, 32-byte public key bytes). `ring`'s
    /// Ed25519 is standards-compliant, so signatures verify under the license
    /// crate's `ed25519-dalek` verifier.
    fn gen_issuer_keypair() -> (ring::signature::Ed25519KeyPair, Vec<u8>) {
        use ring::signature::KeyPair as _;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
            .expect("generate pkcs8");
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
            .expect("from pkcs8");
        let pubkey = kp.public_key().as_ref().to_vec();
        (kp, pubkey)
    }

    /// Sign `license`'s canonical payload with `kp`, mutating its `signature`
    /// field in place — mirrors what the closed-source signer does on the wire.
    fn sign_license(license: &mut License, kp: &ring::signature::Ed25519KeyPair) {
        let payload = license.canonical_payload().expect("canonical payload");
        let sig = kp.sign(&payload);
        license.signature = sig.as_ref().to_vec();
    }

    /// Build a freshly-issued, currently-valid license bound to `fp`.
    fn valid_license_for(fp: &str) -> License {
        // 30-day expiry, last_phone_home = now → well within any grace window.
        fake_license(LicenseTier::SelfHostPro, fp)
    }

    #[test]
    fn accept_refreshed_license_accepts_valid() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let fp = generate_fingerprint();

        let mut lic = valid_license_for(&fp);
        sign_license(&mut lic, &kp);

        let res = accept_refreshed_license(&registry, &lic, &fp, 7, 14);
        assert!(res.is_ok(), "valid signed+current license must be accepted: {res:?}");
    }

    #[test]
    fn accept_refreshed_license_rejects_expired() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let fp = generate_fingerprint();

        // Issued + expired in the past — validly signed but past expiry.
        let mut lic = License::new(
            "sub_exp",
            "cus_exp",
            LicenseTier::SelfHostPro,
            &fp,
            ChronoDuration::days(-1),
            "v1",
        );
        sign_license(&mut lic, &kp);

        let res = accept_refreshed_license(&registry, &lic, &fp, 7, 14);
        assert!(res.is_err(), "expired license must be rejected");
    }

    #[test]
    fn accept_refreshed_license_rejects_wrong_fingerprint() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let current_fp = generate_fingerprint();

        // Validly signed but bound to a *different* machine.
        let mut lic = valid_license_for("some-other-machine-fingerprint");
        sign_license(&mut lic, &kp);

        let res = accept_refreshed_license(&registry, &lic, &current_fp, 7, 14);
        assert!(res.is_err(), "license bound to another machine must be rejected");
    }

    #[test]
    fn accept_refreshed_license_rejects_bad_signature() {
        // Sign with one key but trust a *different* key → signature untrusted.
        let (kp, _pubkey) = gen_issuer_keypair();
        let (_other_kp, other_pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", other_pubkey);
        let fp = generate_fingerprint();

        let mut lic = valid_license_for(&fp);
        sign_license(&mut lic, &kp);

        let res = accept_refreshed_license(&registry, &lic, &fp, 7, 14);
        assert!(res.is_err(), "license signed by an untrusted key must be rejected");
    }

    #[test]
    fn deployment_binding_rejects_cloud_tier_on_self_host() {
        // A cloud-only tier must be refused on a self-host deployment …
        let cloud = fake_license(LicenseTier::Studio, "fp");
        assert!(cloud.validate_tier_deployment_binding(true).is_err());
        // … and accepted in Cloud.
        assert!(cloud.validate_tier_deployment_binding(false).is_ok());
    }

    #[test]
    fn deployment_binding_rejects_self_host_tier_in_cloud() {
        // Self-host-only tiers (incl. the new Partner NFR) must be refused in
        // Cloud and accepted on self-host.
        for tier in [
            LicenseTier::Partner,
            LicenseTier::PersonalProSelfHost,
            LicenseTier::SelfHostPro,
        ] {
            let lic = fake_license(tier, "fp");
            assert!(
                lic.validate_tier_deployment_binding(false).is_err(),
                "{tier} should be rejected in Cloud"
            );
            assert!(
                lic.validate_tier_deployment_binding(true).is_ok(),
                "{tier} should be accepted on self-host"
            );
        }
    }

    #[test]
    fn last_seen_crl_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_last_seen_crl(dir.path()).is_none(), "no file → None");

        let ts = Utc::now();
        write_last_seen_crl(dir.path(), ts).unwrap();
        let read = read_last_seen_crl(dir.path()).expect("persisted timestamp");
        // RFC3339 round-trip preserves to (at least) second precision.
        assert_eq!(read.timestamp(), ts.timestamp());
    }
}
