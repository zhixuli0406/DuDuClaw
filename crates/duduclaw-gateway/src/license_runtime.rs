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
//!   the grace period expires, at which point the runtime downgrades to
//!   OpenSource and exposes an admin-facing validation warning.
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
    EMBEDDED_FEATURES_TOML, FeatureGate, License, LicenseError, LicenseTier, PublicKeyRegistry,
    crl::SignedCrl, fingerprint_candidates, fingerprint_is_hostname_only, generate_fingerprint,
    FingerprintCandidate, storage,
};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default fall-back when `DUDUCLAW_CONTROL_URL` is unset.
use duduclaw_license::control_url::DEFAULT_CONTROL_URL;

/// Process-wide cache for the strong fingerprint. The license crate retains
/// live hardware observations in memory; no editable disk cache is trusted.
static STRONG_FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Same, for the full candidate list (strong + v1.66.1 legacy compatibility).
static FINGERPRINT_CANDIDATES: std::sync::OnceLock<Vec<FingerprintCandidate>> = std::sync::OnceLock::new();

/// The fingerprint that actually identifies this machine to the control-plane:
/// the candidate the installed license validated against. Falls back to the
/// strong fingerprint when no license is installed or none matched, so a fresh
/// issuance always binds to the strong value.
///
/// Deliberately a `RwLock`, not a `OnceLock`: the effective value is only known
/// after `load_and_validate` runs, and a `OnceLock` read that happened to fire
/// first would pin the wrong value for the process lifetime.
static FINGERPRINT_CACHE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// One-shot guard so the "re-issue recommended" warning is logged once per
/// process rather than on every phone-home / dashboard poll.
static LEGACY_BINDING_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// This machine's strong fingerprint — what `duduclaw license fingerprint`
/// prints and the only value a *new* license should be issued against.
pub(crate) fn strong_fingerprint() -> &'static str {
    STRONG_FINGERPRINT.get_or_init(|| {
        if fingerprint_is_hostname_only() {
            // Reported, not silent: neither a platform UUID nor a usable MAC
            // could be read, so the fingerprint has no hardware component at
            // all. A weak binding must never be mistaken for a strong one.
            warn!(
                "no hardware identity readable on this host (no platform UUID, no \
                 usable MAC address) — the machine fingerprint binds to the hostname \
                 alone and provides no hardware binding"
            );
        }
        generate_fingerprint()
    })
}

/// Every fingerprint this machine may legitimately present, strongest first.
fn cached_candidates() -> &'static [FingerprintCandidate] {
    #[cfg(test)]
    if let Ok(candidates) = tests::VALIDATION_CANDIDATES.try_with(|candidates| *candidates) {
        return candidates;
    }
    FINGERPRINT_CANDIDATES.get_or_init(fingerprint_candidates)
}

/// Preferred locally observed identity class for the handshake snapshot.
pub(crate) fn preferred_fingerprint_strength() -> Option<duduclaw_license::FingerprintStrength> {
    cached_candidates().first().map(FingerprintCandidate::strength)
}

/// Return the effective machine fingerprint (see [`FINGERPRINT_CACHE`]).
pub(crate) fn cached_fingerprint() -> String {
    FINGERPRINT_CACHE
        .read()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| strong_fingerprint().to_string())
}

/// Record which candidate the installed license validated against.
fn set_effective_fingerprint(fingerprint: &str) {
    if let Ok(mut guard) = FINGERPRINT_CACHE.write() {
        *guard = Some(fingerprint.to_string());
    }
}

/// Reset the effective fingerprint to the strong value (no license, or the
/// installed one did not validate).
fn clear_effective_fingerprint() {
    if let Ok(mut guard) = FINGERPRINT_CACHE.write() {
        *guard = None;
    }
}

/// Tell the operator — once — that the active license is bound to a
/// historical hostname-based fingerprint. The binding stays accepted; the *next*
/// issuance should use the strong value.
fn warn_legacy_binding_once() {
    if !LEGACY_BINDING_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        warn!(
            "license is bound to a legacy fingerprint (issued before this machine's \
             identity source changed). The binding stays accepted; re-issue \
             when convenient with `duduclaw license rebind` (which calls the control \
             plane's /v1/license/rebind) or by re-signing against \
             `duduclaw license fingerprint`"
        );
    }
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

/// Active production issuer key id claimed by real licenses
/// (`license.public_key_id`).
///
/// **v1 was formally RETIRED on 2026-07-09** — its private counterpart was
/// unaccounted-for on the issuing side (no `license-signing-v1.key` under the
/// operator's control), so trusting it as a permanently-baked issuer was a
/// liability. It is intentionally NOT in the registry anymore; do not re-add v1.
/// The replacement v2 keypair is generated on a secure offline machine
/// (`license-keygen keygen --key-id v2`); only its public half is baked below.
const PROD_ISSUER_KEY_ID: &str = "v2";

/// The production issuer's Ed25519 **public** key (hex), baked into every stock
/// `duduclaw` binary so the commercial upgrade needs no separate build and no
/// environment variable: an enterprise drops its signed `license.json` into
/// `~/.duduclaw/`, restarts, and the paid tier unlocks — no `duduclaw-pro`.
///
/// Embedding a *public* key is safe and intended — the private signing key is
/// generated and held offline and never ships.
///
/// Empty would mean "no key baked (env-only, OpenSource)". Set below to the v2
/// issuer public key (id `v2`), generated offline via
/// `license-keygen keygen --key-id v2` — its private half is held offline and
/// never ships. This activates the single-binary upgrade path: a stock binary
/// verifies a v2-signed `license.json` with no env var and no `duduclaw-pro`.
const PROD_ISSUER_PUBKEY_HEX: &str =
    "942d6abae25dcd782c0586132143940003a10b1efd11e0a0b2f5a86d303476d0";

/// Build the trusted-issuer registry for a normal gateway boot.
///
/// Precedence: `DUDUCLAW_LICENSE_PUBKEY_<ID>` env vars are collected first and
/// win on id collision (self-hosters / self-issuers / emergency key rotation
/// can override), then the baked production key (`PROD_ISSUER_KEY_ID`) is
/// appended so a stock download trusts DuDuClaw-issued licenses with zero setup.
///
/// Fail-safe: an empty or malformed baked constant yields an env-only registry
/// (OpenSource unless an env key is supplied) rather than panicking. The worst
/// case is "no baked key" ⇒ OpenSource, never a crash of the Apache-2.0 core.
pub fn production_registry() -> PublicKeyRegistry {
    let registry = embedded_registry_from_env();
    if registry.get(PROD_ISSUER_KEY_ID).is_some() {
        // An operator-supplied key for this id takes precedence; don't shadow it.
        return registry;
    }
    if PROD_ISSUER_PUBKEY_HEX.is_empty() {
        // v1 retired; v2 not baked yet → env-only. Commercial licenses verify
        // only if an operator supplies a key via env; otherwise OpenSource.
        return registry;
    }
    match hex::decode(PROD_ISSUER_PUBKEY_HEX) {
        Ok(bytes) if bytes.len() == 32 => {
            info!(
                key_id = PROD_ISSUER_KEY_ID,
                "registered baked production issuer public key"
            );
            registry.with_key(PROD_ISSUER_KEY_ID, bytes)
        }
        _ => {
            warn!(
                "baked production issuer key is malformed — commercial licenses \
                 will not verify from this binary; running OpenSource unless a \
                 DUDUCLAW_LICENSE_PUBKEY_* env var supplies a valid key"
            );
            registry
        }
    }
}

/// Resolve the control-plane base URL with §10.5 precedence:
/// `DUDUCLAW_CONTROL_URL` env > `license.control_url` (self-carried) > the baked
/// default. A blank env value is treated as unset. This is only an untrusted
/// hint; license_transport validates the endpoint before every outbound call.
pub fn resolve_control_url(license: Option<&License>) -> String {
    if let Ok(env_url) = std::env::var("DUDUCLAW_CONTROL_URL") {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(lic) = license {
        if let Some(url) = lic.control_url.as_deref() {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    DEFAULT_CONTROL_URL.to_string()
}

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
    revision: u64,
    /// `None` when no valid license is installed — gateway operates in
    /// OpenSource mode. The runtime exposes a stable `LicenseTier::OpenSource`
    /// to callers in this state.
    license: Option<License>,
    validation_warning: Option<&'static str>,

    /// HS13/D5: `generated_at` of the most recent *accepted* CRL. Used to
    /// reject replays of an older (e.g. pre-revocation) CRL. Loaded from disk
    /// at bootstrap so the freshness floor survives restarts.
    last_seen_crl_at: Option<chrono::DateTime<Utc>>,
}

/// File (under `home_dir`) that persists the last-seen CRL `generated_at`
/// timestamp so the monotonicity floor survives restarts.
const LAST_CRL_FILENAME: &str = "license_crl_last_seen.txt";

impl LicenseRuntime {
    /// Resolve the boot edition from the same verified license as live requests,
    /// before callers create users.db and consume the fresh-install signal.
    /// Background tasks remain the gateway caller's responsibility.
    pub async fn bootstrap_with_edition(
        home_dir: PathBuf,
        edition_override: Option<duduclaw_core::EditionProfile>,
    ) -> (Self, duduclaw_core::EditionProfile) {
        let _ = crate::license_seed::seed_license_if_absent(&home_dir);
        let runtime = Self::bootstrap(home_dir, production_registry()).await;
        let env = std::env::var("DUDUCLAW_EDITION").ok();
        let edition = runtime.boot_edition(env.as_deref(), edition_override).await;
        (runtime, edition)
    }

    async fn boot_edition(
        &self,
        env: Option<&str>,
        edition_override: Option<duduclaw_core::EditionProfile>,
    ) -> duduclaw_core::EditionProfile {
        let tier = self.snapshot().await.tier;
        duduclaw_core::EditionProfile::resolve(
            env, edition_override.map(|e| e.as_str()), Some(tier.as_toml_key()),
        )
    }

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

        let (license, validation_warning) = load_and_validate(&home_dir, &registry, &gate).await;

        // §10.5: resolve the control-plane URL AFTER loading the license so a
        // self-carried `license.control_url` (baked in by the issuer) is honored
        // when the operator has not set `DUDUCLAW_CONTROL_URL`. Precedence:
        // env > license.control_url > DEFAULT.
        let control_url = resolve_control_url(license.as_ref());

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

        // Proactive pre-expiry warning at boot (an already-expired license was
        // rejected above and logged separately; this covers the 1–30 day window).
        if let Some(lic) = license.as_ref() {
            log_expiry_warning(lic.tier, lic.days_until_expiry());
        }

        let last_seen_crl_at = read_last_seen_crl(&home_dir);
        crate::distributor_notice::runtime::load_cached(
            &home_dir,
            &registry,
            license.as_ref().map(|l| l.subscription_id.as_str()),
        );

        crate::distributor_brand::runtime::load_cached(
            &home_dir, &registry, license.as_ref().map(|l| l.subscription_id.as_str()),
        );

        Self {
            state: Arc::new(RwLock::new(LicenseRuntimeInner {
                revision: 0,
                license,
                validation_warning,
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
        let notice_poll = tokio::spawn(crate::distributor_notice::runtime::poll_loop(self.clone()));
        let brand_poll = tokio::spawn(crate::distributor_brand::runtime::poll_loop(self.clone()));
        vec![phone_home, crl_poll, notice_poll, brand_poll]
    }

    /// `(subscription_id, machine_fingerprint)` of the installed license, for
    /// the distributor-notice fetch.
    pub(crate) async fn notice_identity(&self) -> Option<(String, String)> {
        let inner = self.state.read().await;
        inner
            .license
            .as_ref()
            .map(|l| (l.subscription_id.clone(), l.machine_fingerprint.clone()))
    }

    pub(crate) fn control_url(&self) -> &str {
        &self.control_url
    }

    pub(crate) fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub(crate) fn registry(&self) -> &PublicKeyRegistry {
        &self.registry
    }

    /// Currently-active tier. Always returns `OpenSource` when no license
    /// is installed or the installed license has been downgraded by the
    /// runtime (e.g. revoked, grace exceeded). Time-dependent validity is
    /// checked on every read, even if all background refreshes fail.
    pub async fn current_tier(&self) -> LicenseTier {
        let inner = self.state.read().await;
        self.live_tier(inner.license.as_ref())
    }

    fn live_tier(&self, license: Option<&License>) -> LicenseTier {
        let Some(license) = license else { return LicenseTier::OpenSource; };
        let grace = self.gate.grace_period_days(license.tier);
        // Match License::validate: zero grace disables the offline check.
        if license.is_expired() || (grace > 0 && license.grace_period_exceeded(grace)) {
            LicenseTier::OpenSource
        } else {
            license.tier
        }
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


    /// Effective agent-count limit for the active license: the signed
    /// per-license `max_agents` override when present, else the tier default.
    /// `0` means unlimited. Falls back to the tier default when no license is
    /// installed (OpenSource). The single resolution point the agent-cap
    /// enforcement in `handlers.rs` must consult (P-License).
    pub async fn effective_max_agents(&self) -> usize {
        let inner = self.state.read().await;
        match inner.license.as_ref() {
            Some(lic) => self.gate.effective_max_agents(lic),
            None => self.gate.max_agents(LicenseTier::OpenSource),
        }
    }

    /// Effective Cloud memory storage quota in GB for the active tier
    /// (M1 moat-gate). `0` means unlimited — the value for OpenSource / free /
    /// self-host tiers, so memory-write enforcement is a no-op there. Only Cloud
    /// paid tiers with an explicit cap (Studio = 1, Business = 10) return
    /// nonzero. The single resolution point the memory-write path consults; the
    /// `duduclaw-memory` crate stays license-agnostic (quota is passed in as a
    /// parameter, not looked up).
    pub async fn effective_memory_quota_gb(&self) -> usize {
        let tier = self.current_tier().await;
        self.gate.effective_memory_quota_gb(tier)
    }

    /// Snapshot of the current license, if any. Returned by reference
    /// for read-only inspection (dashboard `license.status` RPC, etc.).
    pub async fn snapshot(&self) -> LicenseSnapshot {
        let inner = self.state.read().await;
        let tier = self.live_tier(inner.license.as_ref());
        let mut snapshot = LicenseSnapshot::from_state(inner.license.as_ref(), tier);
        snapshot.validation_warning = inner.validation_warning.or_else(|| {
            inner.license.as_ref().and_then(|license| {
                if license.is_expired() { Some("expired") }
                else if self.gate.grace_period_days(license.tier) > 0
                    && license.grace_period_exceeded(self.gate.grace_period_days(license.tier)) {
                    Some("grace_period_expired")
                } else { None }
            })
        });
        // An invalid installed file is different from a fresh personal install.
        snapshot.installed |= snapshot.validation_warning.is_some();
        if snapshot.validation_warning.is_some() { snapshot.mode = "opensource"; }
        snapshot
    }

    /// Verify a candidate license against this runtime's issuer registry +
    /// machine fingerprint + expiry, persist it as `license.json`, and
    /// hot-swap the in-memory license — the dashboard activation flow unlocks
    /// commercial features WITHOUT a gateway restart. Fail-closed: nothing is
    /// written unless signature, fingerprint, and expiry all pass; the final
    /// in-memory swap re-runs the full `load_and_validate` (M51 tier/deploy
    /// binding, phone-home grace, …) so the RPC path can never be more lenient
    /// than a restart. Error strings are operator-facing zh-TW.
    pub async fn install_and_reload(&self, license: License) -> Result<LicenseSnapshot, String> {
        if self.registry.is_empty() {
            return Err(
                "此版本未內建授權簽發公鑰，無法驗證授權（OSS 建置不支援商用授權）".to_string(),
            );
        }
        self.registry
            .verify(&license)
            .map_err(|e| format!("授權簽章驗證失敗：{e}"))?;
        let matched = license.validate_any(cached_candidates(),
            self.gate.phone_home_interval_days(license.tier), self.gate.grace_period_days(license.tier))
            .map_err(|e| format!("授權驗證失敗：{e}；換綁須由帳號持有人在控制台核准"))?;
        if license.is_expired() {
            return Err(format!("授權已於 {} 過期", license.expires_at));
        }
        // M51 pre-check: surface the tier ↔ deployment-mode mismatch BEFORE
        // writing anything — the post-save re-validation would reject it
        // anyway, but with an error that hides the actionable reason.
        let is_self_host = is_self_host_deployment();
        if license
            .validate_tier_deployment_binding(is_self_host)
            .is_err()
        {
            return Err(format!(
                "授權方案「{}」與本機部署型態不符：本機是{}部署。\
                 自架機器請簽發 self-host-pro / personal-pro-self-host / partner / oem；\
                 雲端託管才使用 solo / studio / business",
                license.tier,
                if is_self_host {
                    "自架（self-host）"
                } else {
                    "雲端（cloud）"
                },
            ));
        }
        // Serialize disk and memory changes with refresh CAS and revocation.
        let mut inner = self.state.write().await;
        let store = storage::LockedLicenseStore::acquire(&self.home_dir.join(storage::DEFAULT_LICENSE_FILENAME))
            .map_err(|e| format!("鎖定授權檔失敗：{e}"))?;
        store.save(&license).map_err(|e| format!("寫入授權檔失敗：{e}"))?;
        let (validated, validation_warning) = match read_validated_license_locked(&self.home_dir, &self.registry, &self.gate, &store) {
            Ok(license) => (license, None),
            Err(reason) => (None, Some(reason)),
        };
        let ok = validated.is_some();
        // A notice cached for the previous license no longer applies; the
        // next fetch brings the new subscription's.
        crate::distributor_notice::runtime::reset(&self.home_dir);
        crate::distributor_brand::runtime::reset(&self.home_dir);
        inner.license = validated;
        inner.validation_warning = validation_warning;
        inner.revision += 1;
        if ok {
            if matched.legacy { warn_legacy_binding_once(); }
            set_effective_fingerprint(&matched.fingerprint);
        }
        drop(store);
        drop(inner);
        if !ok {
            return Err(
                "授權已寫入但完整驗證未通過（詳見 gateway 日誌），目前仍為 OpenSource 模式"
                    .to_string(),
            );
        }
        crate::distributor_notice::runtime::spawn_fetch_now(self.clone());
        crate::distributor_brand::runtime::spawn_fetch_now(self.clone());
        Ok(self.snapshot().await)
    }
}

/// Parse a dashboard-supplied license key: a base64-encoded license blob or
/// the raw license JSON. Deliberately does NOT accept filesystem paths (the
/// CLI does; a path coming from a browser form is a smell, not a feature).
pub fn parse_license_key(input: &str) -> Result<License, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("授權金鑰不可為空".to_string());
    }
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).map_err(|e| format!("授權 JSON 解析失敗：{e}"));
    }
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|_| "無法辨識的授權格式（需為 base64 金鑰或授權 JSON 全文）".to_string())?;
    serde_json::from_slice(&decoded).map_err(|e| format!("授權內容解析失敗：{e}"))
}

/// Read-only view of the runtime state for dashboards / metrics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LicenseSnapshot {
    pub tier: LicenseTier,
    pub mode: &'static str,
    pub installed: bool,
    /// Stable reason code, only exposed by license.status to Admin. No IDs or hashes.
    pub validation_warning: Option<&'static str>,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub expires_at: Option<chrono::DateTime<Utc>>,
    pub days_until_expiry: Option<i64>,
    pub last_phone_home: Option<chrono::DateTime<Utc>>,
    pub days_since_phone_home: Option<i64>,
    pub fingerprint_match: Option<bool>,
    /// Informational identity class, independent of paid-tier validation warnings.
    pub fingerprint_strength: Option<duduclaw_license::FingerprintStrength>,
    /// P-License: the active license's signed per-license agent-count quota, if
    /// any. `None` = no override → the tier default applies. Surfaced so the
    /// enforcement path can detect an explicit issuer-set limit (`is_some()`)
    /// even on a self-host tier — see `handlers::tier_limit_message`.
    pub max_agents: Option<u32>,
    /// NFR (Not-For-Resale) internal-test marker from the signed license. The
    /// dashboard MUST render this as a visible badge that operators
    /// cannot remove — it is the anti-resale watermark for free test licenses.
    pub nfr: bool,
}

impl LicenseSnapshot {
    pub(crate) fn for_admin(mut self, is_admin: bool) -> Self {
        if !is_admin { self.validation_warning = None; }
        self
    }

    fn from_state(license: Option<&License>, tier: LicenseTier) -> Self {
        let installed = license.is_some();
        // L7: reuse the cached fingerprint instead of re-enumerating host MACs.
        // This is the *effective* fingerprint (the candidate the active license
        // validated against), so a still-accepted legacy binding reports a
        // match rather than a scary false mismatch in the dashboard.
        let current_fp = cached_fingerprint();
        Self {
            tier,
            mode: if installed {
                "commercial"
            } else {
                "opensource"
            },
            installed,
            validation_warning: None,
            customer_id: license.map(|l| l.customer_id.clone()),
            subscription_id: license.map(|l| l.subscription_id.clone()),
            expires_at: license.map(|l| l.expires_at),
            days_until_expiry: license.map(|l| l.days_until_expiry()),
            last_phone_home: license.map(|l| l.last_phone_home),
            days_since_phone_home: license.map(|l| l.days_since_phone_home()),
            fingerprint_match: license.map(|l| l.is_valid_for_machine(&current_fp)),
            fingerprint_strength: cached_candidates().iter()
                .find(|c| c.fingerprint() == current_fp)
                .map(|c| c.strength()),
            max_agents: license.and_then(|l| l.max_agents),
            nfr: license.is_some_and(|l| l.nfr),
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

/// Diagnose the on-disk license with the same checks as gateway bootstrap.
/// Returns only fixed reason codes; never license content, fingerprints or paths.
pub fn installed_license_warning(home_dir: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate) -> Option<&'static str> {
    read_validated_license(home_dir, registry, gate).err()
}

fn validation_warning(error: &LicenseError) -> &'static str {
    match error {
        LicenseError::WeakFingerprint => "weak_binding",
        LicenseError::InvalidFingerprint if duduclaw_license::fingerprint_hardware_unavailable() => "hardware_identity_unavailable",
        LicenseError::InvalidFingerprint => "machine_mismatch",
        LicenseError::Expired => "expired",
        LicenseError::GracePeriodExceeded(_) => "grace_period_expired",
        LicenseError::InvalidSignature | LicenseError::UnknownPublicKeyId(_) => "invalid_signature",
        _ => "invalid_license",
    }
}

fn read_validated_license(home_dir: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate) -> Result<Option<License>, &'static str> {
    let path = home_dir.join(storage::DEFAULT_LICENSE_FILENAME);
    // Preserve read-only diagnostics on an unused home. Returning no grant
    // without a lock is safe; any existing artifact is read under the lock.
    if !path.try_exists().map_err(|_| "unreadable")?
        && !home_dir.join(REVOKED_FILENAME).try_exists().map_err(|_| "unreadable")? {
        return Ok(None);
    }
    let store = storage::LockedLicenseStore::acquire(&path).map_err(|_| "unreadable")?;
    read_validated_license_locked(home_dir, registry, gate, &store)
}

fn read_validated_license_locked(home_dir: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate,
    store: &storage::LockedLicenseStore) -> Result<Option<License>, &'static str> {
    if home_dir.join(REVOKED_FILENAME).try_exists().map_err(|_| "unreadable")? {
        return Err("revoked");
    }
    let license = match store.load() {
        Ok(license) => license,
        Err(LicenseError::FileNotFound(_)) => return Ok(None),
        Err(_) => return Err("unreadable"),
    };
    registry.verify(&license).map_err(|e| validation_warning(&e))?;
    license.validate_any(cached_candidates(), gate.phone_home_interval_days(license.tier),
        gate.grace_period_days(license.tier)).map_err(|e| validation_warning(&e))?;
    license.validate_tier_deployment_binding(is_self_host_deployment())
        .map_err(|e| validation_warning(&e))?;
    Ok(Some(license))
}

async fn load_and_validate(home_dir: &Path, registry: &PublicKeyRegistry, gate: &FeatureGate) -> (Option<License>, Option<&'static str>) {
    match read_validated_license(home_dir, registry, gate) {
        Ok(Some(license)) => {
            let matched = license.validate_any(cached_candidates(), gate.phone_home_interval_days(license.tier),
                gate.grace_period_days(license.tier));
            // Recheck before publishing the effective identity; time may have advanced.
            match matched {
                Ok(matched) => {
                    if matched.legacy { warn_legacy_binding_once(); }
                    set_effective_fingerprint(&matched.fingerprint);
                    (Some(license), None)
                }
                Err(e) => {
                    clear_effective_fingerprint();
                    (None, Some(validation_warning(&e)))
                }
            }
        }
        Ok(None) => {
            clear_effective_fingerprint();
            debug!("no license file found; running in OpenSource mode");
            (None, None)
        }
        Err(reason) => {
            clear_effective_fingerprint();
            warn!(reason, "installed license validation failed; running in OpenSource mode");
            (None, Some(reason))
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
pub fn is_self_host_deployment() -> bool {
    match std::env::var("DUDUCLAW_DEPLOYMENT") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "cloud" | "managed"),
        Err(_) => true,
    }
}

/// Proactive license-expiry urgency, mirroring the dashboard's `classifyExpiry`
/// buckets so the log warnings and the UI agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryUrgency {
    /// More than 30 days left — no warning.
    Ok,
    /// 8–30 days left — plan renewal.
    Warning,
    /// 0–7 days left — renew now to avoid a downgrade.
    Critical,
    /// Already past expiry.
    Expired,
}

/// Pure classification of days-until-expiry into a warning bucket. Thresholds
/// match the dashboard (`LicensePage.classifyExpiry`) and the OAuth-token
/// expiry-warning convention (30/7-day pre-warnings).
pub fn classify_expiry_urgency(days_until_expiry: i64) -> ExpiryUrgency {
    if days_until_expiry < 0 {
        ExpiryUrgency::Expired
    } else if days_until_expiry <= 7 {
        ExpiryUrgency::Critical
    } else if days_until_expiry <= 30 {
        ExpiryUrgency::Warning
    } else {
        ExpiryUrgency::Ok
    }
}

/// Emit a proactive expiry warning to the tracing log for a still-valid license
/// nearing its term end. An already-*expired* license never reaches this path
/// with `Some` (it is rejected during load and separately logged), so this only
/// fires for the 1–30 day pre-expiry window — the whole point of a proactive
/// warning is to fire *before* the downgrade.
fn log_expiry_warning(tier: LicenseTier, days_until_expiry: i64) {
    match classify_expiry_urgency(days_until_expiry) {
        ExpiryUrgency::Critical => warn!(
            tier = %tier,
            days_left = days_until_expiry,
            "license expires within 7 days — renew now to avoid an automatic downgrade to OpenSource"
        ),
        ExpiryUrgency::Warning => warn!(
            tier = %tier,
            days_left = days_until_expiry,
            "license expires within 30 days — plan renewal"
        ),
        ExpiryUrgency::Ok | ExpiryUrgency::Expired => {}
    }
}

/// Pure cap check used by the Cloud resource limits (agents / channels).
/// `max == 0` means unlimited (the features.toml convention), so it never
/// caps. Otherwise the cap is reached once `current >= max`.
pub fn cap_exceeded(max: usize, current: usize) -> bool {
    max != 0 && current >= max
}

/// P-License §7(b) enforcement gate for the agent cap. Self-host deployments are
/// normally exempt from resource caps (the Apache 2.0 promise), but a signed
/// per-license `max_agents` override is the issuer's EXPLICIT intent to cap this
/// seat count, so it overrides the exemption. Cloud deployments always enforce.
/// Pure so the decision is unit-testable without a live runtime / env.
pub fn agent_cap_enforced(is_self_host: bool, has_max_agents_override: bool) -> bool {
    !is_self_host || has_max_agents_override
}

/// How many agents may be registered as OS-native ("OS 原生員工") for the given
/// edition. This is a **quota** lock, not a capability lock — the `os_native`
/// capability itself is never removed or crippled on any edition; the free core
/// keeps every feature, we only cap *how many seats* may use it at once
/// (`edition-split-moat`: 鎖 quota 不鎖能力).
///
/// - **Personal** → `Some(1)`: exactly one AI employee may be the OS-native one.
/// - **Enterprise** → `None`: uncapped. The signed license schema carries no
///   os-native-specific field, so there is nothing to read here — paid tiers are
///   simply unlimited. (Do NOT invent a license field that doesn't exist; the
///   whole-fleet `max_agents` quota is a *separate* concern enforced elsewhere.)
///
/// `None` means unlimited; a `Some(n)` return is the number of agents that may
/// have `[capabilities] os_native = true` simultaneously. Pure so both the
/// write-time gate (`agents.update` / `os.settings.update`) and the startup
/// gate (`resolve_os_native_allowed`) consult one identical decision.
pub fn os_native_agent_quota(edition: duduclaw_core::EditionProfile) -> Option<u32> {
    if edition.is_personal() { Some(1) } else { None }
}

/// Agent-count cap check for processes OUTSIDE the gateway (the MCP server is
/// a separate `duduclaw mcp-server` process, so [`global()`] is never
/// initialised there and the dashboard's `tier_limit_message` gate cannot
/// run). Without this, an agent could `create_agent` its way past both the
/// Personal-edition cap and a signed P-License `max_agents` quota.
///
/// Bootstraps a throwaway runtime from disk and mirrors the gateway's agent
/// branch: Personal edition → `personal_max_agents()` (unlimited by default
/// per the 2026-07-16 B+C decision; `DUDUCLAW_PERSONAL_MAX_AGENTS` opts a
/// hosted deployment into a hard cap); otherwise the license tier /
/// signed-override path with the self-host exemption. Edition is resolved
/// the same way the gateway does at boot (env > license tier). The
/// `ServerConfig`-level edition override is gateway-internal and always
/// `None` in the open CLI, so it is not consulted here.
///
/// Returns the zh-TW refusal message when `current` agents already meet the
/// effective cap, else `None` (allowed).
pub async fn agent_cap_message_from_disk(
    home_dir: &std::path::Path,
    current: usize,
) -> Option<String> {
    let cap = agent_cap_from_disk(home_dir).await;
    if !cap_exceeded(cap.max, current) {
        return None;
    }
    let max = cap.max;
    Some(match cap.tier {
        None => format!(
            "個人版最多可建立 {max} 個 AI 員工。\
             如需調整授權範圍，請聯絡總經銷：\
             https://futurecorp.tw"
        ),
        Some(tier) => format!(
            "您的方案（{tier}）最多可建立 {max} 個 Agent。\
             如需調整授權範圍，請聯絡總經銷：https://futurecorp.tw"
        ),
    })
}

/// The agent-count cap that applies to this home, resolved from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCap {
    /// `0` = unlimited (or not enforced on this deployment).
    pub max: usize,
    /// The license tier when the cap comes from the license; `None` for
    /// the Personal-edition cap.
    pub tier: Option<String>,
}

/// [`agent_cap_message_from_disk`]'s decision without a count: the cap a
/// caller outside the gateway must hold to. Used by the pack importer,
/// which adds several employees at once and so has to compare
/// `current + adding` itself.
pub async fn agent_cap_from_disk(home_dir: &std::path::Path) -> AgentCap {
    let runtime = LicenseRuntime::bootstrap(home_dir.to_path_buf(), production_registry()).await;
    let snapshot = runtime.snapshot().await;
    let edition =
        duduclaw_core::EditionProfile::resolve_from_env(None, Some(snapshot.tier.as_toml_key()));

    if edition.is_personal() {
        return AgentCap { max: duduclaw_core::EditionProfile::personal_max_agents(), tier: None };
    }

    let agent_override = snapshot.max_agents.is_some();
    let is_self_host = is_self_host_deployment();
    if !agent_cap_enforced(is_self_host, agent_override) {
        return AgentCap { max: 0, tier: Some(snapshot.tier.to_string()) };
    }
    AgentCap {
        max: runtime.effective_max_agents().await,
        tier: Some(snapshot.tier.to_string()),
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
            // Non-fatal, re-evaluated next cycle, but logged at warn: a
            // refresh that keeps failing ends in the offline grace period
            // (until 2026-10-22 every refresh failed the nonce check at
            // debug level and nobody saw it).
            warn!(error = %e, "phone-home attempt failed; will retry next cycle");
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
        let mut request = self.refresh_snapshot().await?;
        request.proposed_fingerprint = duduclaw_license::fingerprint::refresh_fingerprint(&request.machine_fingerprint)
            .map_err(|e| PhoneHomeError::Rejected(e.to_string()))?;
        let subscription_id = &request.subscription_id;
        let machine_fingerprint = &request.machine_fingerprint;
        let endpoint = format!(
            "{}/v1/license/refresh",
            self.control_url.trim_end_matches('/')
        );

        // Every response authenticates this fresh nonce and full request context.
        let nonce = uuid::Uuid::new_v4().to_string();
        let proposed = &request.proposed_fingerprint;
        let mut request_body = json!({
            "subscription_id": subscription_id,
            "machine_fingerprint": machine_fingerprint,
            "current_version": env!("CARGO_PKG_VERSION"),
            "nonce": nonce,
            "telemetry": {},
        });
        // Existing issuers require exact equality with the installed binding.
        // A proposal is only effective after the account holder approves the
        // re-bind in the console. An old fingerprint is not authentication.
        if proposed != machine_fingerprint {
            request_body["new_fingerprint"] = json!(proposed);
        }

        let response = crate::license_transport::request(reqwest::Method::POST, &endpoint, PHONE_HOME_TIMEOUT)
            .await.map_err(PhoneHomeError::Http)?
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

        if status_code == reqwest::StatusCode::ACCEPTED
            || body.get("status").and_then(|v| v.as_str()) == Some("pending") {
            self.verify_status_response(&request, &nonce, &body, "pending")?;
            info!("license rebind pending account-holder console approval; installed license unchanged");
            return Ok(());
        }
        match body.get("status").and_then(|v| v.as_str()) {
            Some("active") => {
                // Reject unbound responses before artifact validation.
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
                let phone_home = self.gate.phone_home_interval_days(new_license.tier);
                let grace = self.gate.grace_period_days(new_license.tier);
                let matched = accept_refreshed_license(
                    &self.registry,
                    &new_license,
                    cached_candidates(),
                    phone_home,
                    grace,
                    &request, &nonce, &body,
                )?;
                self.apply_refresh(&request, new_license, &matched).await?;
                info!("phone-home succeeded; license refreshed");
                Ok(())
            }
            Some("revoked") => {
                self.apply_revocation_response(&request, &nonce, &body).await?;
                Ok(())
            }
            other => Err(PhoneHomeError::Parse(format!(
                "unexpected refresh response status: {other:?}"
            ))),
        }
    }

    async fn refresh_snapshot(&self) -> Result<RefreshSnapshot, PhoneHomeError> {
        // Lock order is always memory then disk; release both before network I/O.
        let mut inner = self.state.write().await;
        let store = self.lock_store()?;
        let same = match inner.license.as_ref() {
            Some(license) => store.same_license(license).unwrap_or(false),
            None => false,
        };
        if !same {
            // A CLI writer cannot update our memory. Reconcile under the same
            // lock before rejecting the old request, including durable denial.
            let (license, warning) = match read_validated_license_locked(&self.home_dir, &self.registry, &self.gate, &store) {
                Ok(license) => (license, None),
                Err(reason) => (None, Some(reason)),
            };
            inner.license = license;
            inner.validation_warning = warning;
            inner.revision += 1;
            clear_effective_fingerprint();
            if let Some(license) = inner.license.as_ref() {
                set_effective_fingerprint(&license.machine_fingerprint);
            }
            crate::distributor_notice::runtime::reset(&self.home_dir);
            crate::distributor_brand::runtime::reset(&self.home_dir);
            return Err(if inner.license.is_some() { PhoneHomeError::StaleResponse } else { PhoneHomeError::NoLicense });
        }
        let license = inner.license.as_ref().ok_or(PhoneHomeError::NoLicense)?;
        Ok(RefreshSnapshot {
            subscription_id: license.subscription_id.clone(),
            machine_fingerprint: license.machine_fingerprint.clone(),
            proposed_fingerprint: license.machine_fingerprint.clone(),
            revision: inner.revision,
            disk: store.snapshot().map_err(|e| PhoneHomeError::Save(e.to_string()))?,
            is_self_host: is_self_host_deployment(),
        })
    }

    fn lock_store(&self) -> Result<storage::LockedLicenseStore, PhoneHomeError> {
        storage::LockedLicenseStore::acquire(&self.home_dir.join(storage::DEFAULT_LICENSE_FILENAME))
            .map_err(|e| PhoneHomeError::Save(e.to_string()))
    }

    fn verify_status_response(&self, request: &RefreshSnapshot, nonce: &str,
        body: &serde_json::Value, status: &str) -> Result<(), PhoneHomeError> {
        duduclaw_license::refresh::validate_status_response(&self.registry, body, status,
            nonce, &request.subscription_id, &request.machine_fingerprint, &request.proposed_fingerprint)
            .map_err(|e| PhoneHomeError::Rejected(e.to_string()))
    }

    async fn apply_refresh(&self, request: &RefreshSnapshot, license: License,
        matched: &duduclaw_license::FingerprintMatch) -> Result<(), PhoneHomeError> {
        let mut inner = self.state.write().await;
        if !request.matches(&inner) { return Err(PhoneHomeError::StaleResponse); }
        let store = self.lock_store()?;
        if !store.matches(&request.disk).map_err(|e| PhoneHomeError::Save(e.to_string()))? {
            return Err(PhoneHomeError::StaleResponse);
        }
        store.save(&license).map_err(|e| PhoneHomeError::Save(e.to_string()))?;
        inner.license = Some(license);
        inner.validation_warning = None;
        inner.revision += 1;
        if matched.legacy { warn_legacy_binding_once(); }
        set_effective_fingerprint(&matched.fingerprint);
        Ok(())
    }

    async fn apply_revocation_response(&self, request: &RefreshSnapshot, nonce: &str,
        body: &serde_json::Value) -> Result<(), PhoneHomeError> {
        self.verify_status_response(request, nonce, body, "revoked")?;
        let reason = body["reason"].as_str().ok_or_else(|| PhoneHomeError::Parse("missing reason".into()))?;
        self.apply_revocation(request, reason).await
    }

    async fn apply_revocation(&self, request: &RefreshSnapshot, reason: &str) -> Result<(), PhoneHomeError> {
        let mut inner = self.state.write().await;
        if !request.matches(&inner) { return Err(PhoneHomeError::StaleResponse); }
        let store = self.lock_store()?;
        if !store.matches(&request.disk).map_err(|e| PhoneHomeError::Save(e.to_string()))? {
            return Err(PhoneHomeError::StaleResponse);
        }
        // A persisted marker fences restart even if removing the artifact fails.
        let persisted = store.revoke(reason);
        inner.license = None;
        inner.revision += 1;
        inner.validation_warning = Some("revoked");
        clear_effective_fingerprint();
        crate::distributor_notice::runtime::reset(&self.home_dir);
        crate::distributor_brand::runtime::reset(&self.home_dir);
        persisted.map_err(|e| PhoneHomeError::Save(e.to_string()))?;
        info!("downgraded to OpenSource mode");
        Ok(())
    }

}

/// File (under `home_dir`) marking that the installed license was revoked.
/// Honored at bootstrap so a revocation survives restarts even if the license
/// file itself could not be deleted. Cleared on a successful re-subscription.
const REVOKED_FILENAME: &str = storage::REVOKED_FILENAME;

#[derive(Debug, thiserror::Error)]
enum PhoneHomeError {
    #[error("license changed while refresh was in flight; stale response discarded")]
    StaleResponse,
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
/// type, or a different value all fail. This detects verbatim replay only;
/// response-signature v2 is needed to authenticate the echo.
fn response_nonce_ok(sent: &str, body: &serde_json::Value) -> bool {
    body.get("nonce").and_then(|v| v.as_str()) == Some(sent)
}

/// Captured under the state lock before network I/O. Revision also fences an
/// ABA change where a new installation restores the same subscription/binding.
struct RefreshSnapshot {
    subscription_id: String,
    machine_fingerprint: String,
    proposed_fingerprint: String,
    revision: u64,
    disk: storage::LicenseSnapshot,
    is_self_host: bool,
}

impl RefreshSnapshot {
    fn matches(&self, inner: &LicenseRuntimeInner) -> bool {
        inner.revision == self.revision && inner.license.as_ref().is_some_and(|license|
            license.subscription_id == self.subscription_id &&
            license.machine_fingerprint == self.machine_fingerprint)
    }
}

/// Validate the signed artifact, request identity and deployment before CAS.
#[allow(clippy::too_many_arguments)]
fn accept_refreshed_license(
    registry: &PublicKeyRegistry,
    new_license: &License,
    candidates: &[FingerprintCandidate],
    phone_home_days: i64,
    grace_days: i64,
    request: &RefreshSnapshot,
    nonce: &str,
    body: &serde_json::Value,
) -> Result<duduclaw_license::FingerprintMatch, PhoneHomeError> {
    duduclaw_license::refresh::validate_response(registry, new_license, body, nonce,
        &request.subscription_id, &request.machine_fingerprint, &request.proposed_fingerprint)
        .map_err(|e| PhoneHomeError::Rejected(e.to_string()))?;
    new_license.validate_tier_deployment_binding(request.is_self_host)
        .map_err(|e| PhoneHomeError::Rejected(e.to_string()))?;
    new_license.validate_any(candidates, phone_home_days, grace_days)
        .map_err(|e| PhoneHomeError::Rejected(e.to_string()))
}

// ── CRL loop ──────────────────────────────────────────────────

async fn crl_loop(runtime: LicenseRuntime) {
    loop {
        // Re-emit a proactive expiry warning on each daily cycle so a
        // long-running gateway surfaces the 30/7-day pre-expiry window even if
        // it was started while the license was still comfortably in date.
        runtime.log_expiry_status().await;
        if let Err(e) = runtime.do_crl_fetch_once().await {
            debug!(error = %e, "CRL fetch failed; will retry next cycle");
        }
        tokio::time::sleep(CRL_POLL_INTERVAL).await;
    }
}

impl LicenseRuntime {
    /// Best-effort, read-only proactive expiry warning for the current license.
    /// Called once per CRL cycle (daily).
    async fn log_expiry_status(&self) {
        let inner = self.state.read().await;
        if let Some(lic) = inner.license.as_ref() {
            log_expiry_warning(lic.tier, lic.days_until_expiry());
        }
    }

    async fn do_crl_fetch_once(&self) -> Result<(), PhoneHomeError> {
        let request = match self.refresh_snapshot().await {
            Ok(request) => request,
            Err(PhoneHomeError::NoLicense) => return Ok(()),
            Err(error) => return Err(error),
        };
        let subscription_id = &request.subscription_id;

        let endpoint = format!("{}/v1/license/crl", self.control_url.trim_end_matches('/'));
        let crl: SignedCrl = crate::license_transport::request(reqwest::Method::GET, &endpoint, PHONE_HOME_TIMEOUT)
            .await.map_err(PhoneHomeError::Http)?
            .send()
            .await
            .map_err(|e| PhoneHomeError::Http(e.to_string()))?
            .json()
            .await
            .map_err(|e| PhoneHomeError::Parse(e.to_string()))?;

        if let Err(e) = crl.verify(&self.registry) {
            return Err(PhoneHomeError::Parse(format!("CRL signature invalid: {e}")));
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
            if let Err(reason) = check_crl_monotonic(crl.generated_at, inner.last_seen_crl_at) {
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
            self.apply_revocation(&request, "crl_revoked").await?;
        } else {
            debug!(
                revoked_count = crl.revoked.len(),
                "CRL fetched; subscription not revoked"
            );
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
    duduclaw_core::config::create_home_dir(home_dir)?;
    let path = home_dir.join(LAST_CRL_FILENAME);
    std::fs::write(path, ts.to_rfc3339())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use duduclaw_license::License;

    static FINGERPRINT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) const TEST_HARDWARE_FINGERPRINT: &str = "synthetic-hardware-for-license-runtime-tests";

    tokio::task_local! {
        pub(super) static VALIDATION_CANDIDATES: &'static [FingerprintCandidate];
    }

    // Scope fixtures to explicitly selected tests. Never upgrade the real MAC
    // candidates, mutate process-wide caches, or pass the override to child tasks.
    pub(crate) async fn with_hardware_fixture<T>(future: impl std::future::Future<Output = T>) -> T {
        static CANDIDATES: std::sync::OnceLock<Vec<FingerprintCandidate>> = std::sync::OnceLock::new();
        let candidates = CANDIDATES.get_or_init(|| vec![FingerprintCandidate::for_test(TEST_HARDWARE_FINGERPRINT,
            duduclaw_license::FingerprintStrength::Hardware)]);
        VALIDATION_CANDIDATES.scope(candidates.as_slice(), future).await
    }

    #[tokio::test]
    async fn hardware_fixture_is_scoped_and_does_not_override_child_tasks() {
        let observed = cached_candidates().to_vec();
        let child_expected = observed.clone();
        with_hardware_fixture(async {
            assert_eq!(cached_candidates()[0].fingerprint(), TEST_HARDWARE_FINGERPRINT);
            assert_eq!(cached_candidates()[0].strength(), duduclaw_license::FingerprintStrength::Hardware);
            let child = tokio::spawn(async move {
                assert_eq!(cached_candidates(), child_expected.as_slice());
            });
            child.await.unwrap();
        }).await;
        assert_eq!(cached_candidates(), observed.as_slice());
    }

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

    #[test]
    fn parse_license_key_rejects_junk_and_paths() {
        assert!(parse_license_key("").is_err());
        assert!(parse_license_key("   ").is_err());
        assert!(parse_license_key("not-base64-not-json!!!").is_err());
        // A filesystem path must NOT be accepted from the dashboard form.
        assert!(parse_license_key("/etc/passwd").is_err());
        // Valid base64 of non-license JSON still fails at deserialization.
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"{\"foo\":1}");
        assert!(parse_license_key(&b64).is_err());
    }

    #[test]
    fn parse_license_key_accepts_json_and_base64() {
        let lic = fake_license(LicenseTier::Studio, "fp_test");
        let json = serde_json::to_string(&lic).unwrap();
        let parsed = parse_license_key(&json).unwrap();
        assert_eq!(parsed.subscription_id, "sub_test");

        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        // Whitespace/newlines inside a pasted base64 blob are tolerated.
        let wrapped = format!("{}\n{}", &b64[..10], &b64[10..]);
        let parsed = parse_license_key(&wrapped).unwrap();
        assert_eq!(parsed.customer_id, "cus_test");
    }

    #[tokio::test]
    async fn install_and_reload_fails_closed_without_registry_keys() {
        let tmp = std::env::temp_dir().join(format!("dudu-lic-rt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let rt = LicenseRuntime::bootstrap(tmp.clone(), PublicKeyRegistry::new()).await;
        let lic = fake_license(LicenseTier::Studio, "fp_test");
        let err = rt.install_and_reload(lic).await.unwrap_err();
        assert!(err.contains("公鑰"), "unexpected error: {err}");
        let _ = std::fs::remove_dir_all(&tmp);
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
        let ph = gate.phone_home_interval_days(license.tier);
        let gp = gate.grace_period_days(license.tier);
        match license.validate_any(cached_candidates(), ph, gp) {
            Ok(_) => Some(license),
            Err(_) => None,
        }
    }

    #[tokio::test]
    async fn mfa_bootstrap_uses_explicit_home_and_verifies_signed_license() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (key, public) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", public);
            let tier = if is_self_host_deployment() { LicenseTier::Oem } else { LicenseTier::Business };
            let mut license = fake_license(tier, TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &key);
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            assert_eq!(runtime.snapshot().await.tier, tier);
            assert!(!runtime.boot_edition(None, None).await.is_personal());
            assert!(!home.path().join("users.db").exists());

            std::fs::write(home.path().join(REVOKED_FILENAME), "revoked").unwrap();
            let revoked = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            assert_eq!(revoked.snapshot().await.tier, LicenseTier::OpenSource);
            std::fs::remove_file(home.path().join(REVOKED_FILENAME)).unwrap();
            license.signature[0] ^= 1;
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let invalid = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            assert_eq!(invalid.snapshot().await.tier, LicenseTier::OpenSource);
        }).await;
    }

    #[tokio::test]
    async fn mfa_boot_edition_uses_live_license_before_identity_creation() {
        use duduclaw_core::EditionProfile;
        for (tier, personal) in [
            (LicenseTier::Business, false), (LicenseTier::Oem, false),
            (LicenseTier::Partner, false), (LicenseTier::SelfHostPro, false),
            (LicenseTier::PersonalProSelfHost, true), (LicenseTier::OpenSource, true),
        ] {
            let home = tempfile::tempdir().unwrap();
            // Model an already-validated runtime, as the other snapshot tests do.
            let runtime = LicenseRuntime {
                state: Arc::new(RwLock::new(LicenseRuntimeInner {
                revision: 0,
                    license: Some(fake_license(tier, "fp_mfa")), validation_warning: None, last_seen_crl_at: None,
                })),
                gate: Arc::new(FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap()),
                registry: Arc::new(PublicKeyRegistry::new()),
                home_dir: home.path().into(), control_url: "https://control.invalid".into(),
            };
            let edition = runtime.boot_edition(None, None).await;
            assert_eq!(edition.is_personal(), personal, "{tier:?}");
            assert!(!home.path().join("users.db").exists());
            crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(
                home.path(), "127.0.0.1:18789".parse().unwrap(), edition.is_personal(), None,
            ).unwrap();
            assert_eq!(crate::mfa::boot_policy(home.path(), edition.is_personal()).unwrap(), !personal);
            // Once enabled, even an OpenSource/Personal runtime must keep enforcing.
            drop(duduclaw_auth::UserDb::new_with_mfa_default(&home.path().join("users.db"), true).unwrap());
            std::fs::write(home.path().join("config.toml"), "[auth]\nmfa_enforced=true\n").unwrap();
            crate::mfa::reload_policy(home.path());
            assert!(crate::mfa::boot_policy(home.path(), edition.is_personal()).unwrap());
            assert!(crate::mfa::enforced(home.path(), edition.is_personal()).unwrap());
            assert!(runtime.boot_edition(None, Some(EditionProfile::Personal)).await.is_personal());
            assert_eq!(runtime.boot_edition(Some("enterprise"), Some(EditionProfile::Personal)).await.is_personal(),
                personal);
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

    #[tokio::test]
    async fn brand_reads_recheck_expiry_and_offline_grace_without_a_refresh() {
        use futures_util::FutureExt;
        use crate::distributor_brand::{Brand, ORIGINAL_PRODUCT_NAME, effective_for};

        let home = tempfile::tempdir().unwrap();
        let runtime = LicenseRuntime {
            state: Arc::new(RwLock::new(LicenseRuntimeInner {
                revision: 0,
                license: Some(fake_license(LicenseTier::Oem, "fp_brand")),
                validation_warning: None,
                last_seen_crl_at: None,
            })),
            gate: Arc::new(FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap()),
            registry: Arc::new(PublicKeyRegistry::new()),
            home_dir: home.path().into(),
            control_url: "https://control.invalid".into(),
        };
        let accepted = crate::distributor_brand::runtime::Accepted {
            subscription_id: "sub_test".into(),
            distributor_id: "dist_test".into(),
            version: 1,
            brand: Brand {
                product_name: "Distributor".into(),
                logo_light: "data:image/png;base64,iVBORw0KGgo=".into(),
                logo_dark: None,
            },
        };
        for tier in [LicenseTier::Hobby, LicenseTier::Solo, LicenseTier::Studio,
            LicenseTier::Business, LicenseTier::Partner, LicenseTier::PersonalProSelfHost,
            LicenseTier::SelfHostPro, LicenseTier::Oem] {
            {
                let mut inner = runtime.state.write().await;
                inner.license = Some(fake_license(tier, "fp_brand"));
            }
            // Both brand poll entry points use notice_identity, independently
            // of the customer's tier issued through the distributor console.
            assert_eq!(runtime.notice_identity().await,
                Some(("sub_test".into(), "fp_brand".into())), "{tier:?}");
            let grace = runtime.gate.grace_period_days(tier);
            assert!(grace > 0);

            // Keep the same installed license and accepted brand. Only advance
            // stored times to model a long-running gateway with failed polls.
            for (expires_in_days, offline_days, expected) in [
                (30, 0, "Distributor"),
                (30, grace - 1, "Distributor"),
                (-1, 0, ORIGINAL_PRODUCT_NAME),
                (-1, grace + 1, ORIGINAL_PRODUCT_NAME),
                (30, grace + 1, ORIGINAL_PRODUCT_NAME),
                (30, 0, "Distributor"),
            ] {
                {
                    let mut inner = runtime.state.write().await;
                    let license = inner.license.as_mut().unwrap();
                    license.expires_at = Utc::now() + ChronoDuration::days(expires_in_days);
                    license.last_phone_home = Utc::now() - ChronoDuration::days(offline_days);
                }
                // Synchronous brand renderers use this same nonblocking read.
                let snapshot = runtime.snapshot().now_or_never().unwrap();
                assert_eq!(runtime.current_tier().await, snapshot.tier);
                let expected_warning = if expires_in_days < 0 { Some("expired") }
                    else if offline_days > grace { Some("grace_period_expired") }
                    else { None };
                assert_eq!(snapshot.validation_warning, expected_warning);
                assert_eq!(snapshot.mode, if expected_warning.is_some() { "opensource" } else { "commercial" });
                let name = effective_for(snapshot.tier, snapshot.subscription_id.as_deref(), &accepted)
                    .map(|brand| brand.product_name)
                    .unwrap_or_else(|| ORIGINAL_PRODUCT_NAME.into());
                assert_eq!(name, expected, "{tier:?}: expiry={expires_in_days}, offline={offline_days}");
            }
        }
        runtime.state.write().await.license = None;
        assert_eq!(runtime.notice_identity().await, None);
        let snapshot = runtime.snapshot().await;
        assert_eq!(effective_for(snapshot.tier, snapshot.subscription_id.as_deref(), &accepted), None);
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
            ("DUDUCLAW_LICENSE_PUBKEY_V1", "00".repeat(32).as_str()),
            ("DUDUCLAW_LICENSE_PUBKEY_V2", "ff".repeat(32).as_str()),
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
            ("DUDUCLAW_LICENSE_PUBKEY_OK", "11".repeat(32).as_str()),
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
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("generate pkcs8");
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from pkcs8");
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
    fn validation_warning_is_only_exposed_to_admin() {
        let mut snapshot = LicenseSnapshot::from_state(None, LicenseTier::OpenSource);
        snapshot.installed = true;
        snapshot.validation_warning = Some("machine_mismatch");
        assert_eq!(snapshot.clone().for_admin(true).validation_warning, Some("machine_mismatch"));
        let visible = serde_json::to_value(snapshot.for_admin(false)).unwrap();
        assert!(visible["validation_warning"].is_null());
        assert!(visible.get("machine_fingerprint").is_none());
    }

    #[tokio::test]
    async fn invalid_installed_license_retains_warning_and_recovers_after_activation() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let missing = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            assert!(!missing.snapshot().await.installed);
            assert_eq!(missing.snapshot().await.validation_warning, None);
            let mut license = valid_license_for("another-machine");
            sign_license(&mut license, &kp);
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let mut runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            runtime.control_url = "http://127.0.0.1:9".into();
            let snap = runtime.snapshot().await;
            assert_eq!(snap.tier, LicenseTier::OpenSource);
            assert_eq!(snap.mode, "opensource");
            assert!(snap.installed);
            let reason = if duduclaw_license::fingerprint_hardware_unavailable() {
                "hardware_identity_unavailable"
            } else { "machine_mismatch" };
            assert_eq!(snap.validation_warning, Some(reason));
            assert!(snap.customer_id.is_none());
            assert!(snap.subscription_id.is_none());
            let json = serde_json::to_string(&snap).unwrap();
            assert!(!json.contains("another-machine"));
            license.machine_fingerprint = TEST_HARDWARE_FINGERPRINT.to_string();
            sign_license(&mut license, &kp);
            let restored = runtime.install_and_reload(license).await.unwrap();
            assert_eq!(restored.tier, LicenseTier::SelfHostPro);
            assert!(restored.installed);
            assert_eq!(restored.validation_warning, None);
        }).await;
    }

    #[tokio::test]
    async fn malformed_or_bad_signature_license_warns_without_exposing_content() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
        let home = tempfile::tempdir().unwrap();
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        std::fs::write(home.path().join("license.json"), "private-invalid-content").unwrap();
        let runtime = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
        assert_eq!(runtime.snapshot().await.validation_warning, Some("unreadable"));
        let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
        sign_license(&mut license, &kp);
        license.signature[0] ^= 1;
        storage::save_to(&license, &home.path().join("license.json")).unwrap();
        let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
        assert_eq!(runtime.snapshot().await.validation_warning, Some("invalid_signature"));
    }

    fn test_refresh_snapshot(license: &License) -> RefreshSnapshot {
        RefreshSnapshot { subscription_id: license.subscription_id.clone(),
            machine_fingerprint: license.machine_fingerprint.clone(),
            proposed_fingerprint: license.machine_fingerprint.clone(), revision: 0,
            disk: storage::LockedLicenseStore::acquire(&tempfile::tempdir().unwrap().path().join("license.json"))
                .unwrap().snapshot().unwrap(), is_self_host: true }
    }

    fn signed_response(kp: &ring::signature::Ed25519KeyPair, license: Option<&License>,
        request: &RefreshSnapshot, status: &str, nonce: &str) -> serde_json::Value {
        use base64::Engine as _;
        let digest = license.map(duduclaw_license::refresh::license_digest).transpose().unwrap().unwrap_or_default();
        let reason = if status == "revoked" { "cancelled" } else { "" };
        let effective = "2020-01-01T00:00:00Z";
        let payload = duduclaw_license::refresh::response_payload(status, reason, effective, nonce,
            &request.subscription_id, &request.machine_fingerprint, &request.proposed_fingerprint, &digest).unwrap();
        json!({"status":status, "reason":reason, "effective_from":effective,
            "nonce":nonce, "subscription_id":request.subscription_id,
            "original_fingerprint":request.machine_fingerprint,
            "proposed_fingerprint":request.proposed_fingerprint, "license_digest":digest,
            "public_key_id":"v1", "response_signature_version":2,
            "response_signature":base64::engine::general_purpose::STANDARD.encode(kp.sign(&payload).as_ref())})
    }

    fn accept_test_response(kp: &ring::signature::Ed25519KeyPair, registry: &PublicKeyRegistry, license: &License,
        candidates: &[FingerprintCandidate], ph: i64, grace: i64) -> Result<duduclaw_license::FingerprintMatch, PhoneHomeError> {
        let request = test_refresh_snapshot(license);
        let body = signed_response(kp, Some(license), &request, "active", "nonce");
        accept_refreshed_license(registry, license, candidates, ph, grace, &request, "nonce", &body)
    }

    #[test]
    fn accept_refreshed_license_binds_request_and_deployment() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let mut license = valid_license_for("old");
        sign_license(&mut license, &kp);
        let mut request = test_refresh_snapshot(&license);
        request.proposed_fingerprint = "new".into();
        let candidates = [
            FingerprintCandidate::for_test("old", duduclaw_license::FingerprintStrength::Hardware),
            FingerprintCandidate::for_test("new", duduclaw_license::FingerprintStrength::Hardware),
            FingerprintCandidate::for_test("unrequested", duduclaw_license::FingerprintStrength::Hardware),
        ];
        let accept = |license: &License, request: &RefreshSnapshot| {
            let body = signed_response(&kp, Some(license), request, "active", "n");
            accept_refreshed_license(&registry, license, &candidates, 7, 14, request, "n", &body)
        };
        assert!(accept(&license, &request).is_ok());
        license.machine_fingerprint = "new".into();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_ok());
        license.subscription_id = "different-subscription".into();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_err());
        license.subscription_id = request.subscription_id.clone();
        license.machine_fingerprint = "unrequested".into();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_err());
        // A legitimate unbound OEM remains supported, but cannot replace a
        // bound license or enter a cloud deployment through refresh.
        license.tier = LicenseTier::Oem;
        license.machine_fingerprint.clear();
        sign_license(&mut license, &kp);
        assert!(accept(&license, &request).is_err());
        request.machine_fingerprint.clear();
        request.proposed_fingerprint.clear();
        assert!(accept(&license, &request).is_ok());
        request.is_self_host = false;
        assert!(accept(&license, &request).is_err());
    }

    #[test]
    fn refresh_snapshot_checks_subscription_binding_and_revision_independently() {
        let license = valid_license_for("fp");
        let request = test_refresh_snapshot(&license);
        let mut inner = LicenseRuntimeInner { revision: 0, license: Some(license.clone()),
            validation_warning: None, last_seen_crl_at: None };
        assert!(request.matches(&inner));
        inner.license.as_mut().unwrap().subscription_id = "other".into();
        assert!(!request.matches(&inner));
        inner.license = Some(license.clone());
        inner.license.as_mut().unwrap().machine_fingerprint = "other".into();
        assert!(!request.matches(&inner));
        inner.license = Some(license);
        inner.revision = 1;
        assert!(!request.matches(&inner));
        inner.license = None;
        assert!(!request.matches(&inner));
    }

    #[tokio::test]
    async fn stale_refresh_cannot_overwrite_reinstalled_license_or_disk() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &kp);
            storage::save_to(&license, &home.path().join("license.json")).unwrap();
            let mut runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            runtime.control_url = "http://127.0.0.1:9".into();
            let request = runtime.refresh_snapshot().await.unwrap();
            // Identical subscription/binding still changes the installation. Both
            // revision and full disk snapshot fence a delayed quota rollback.
            let mut replacement = license.clone();
            replacement.max_agents = Some(42);
            sign_license(&mut replacement, &kp);
            runtime.install_and_reload(replacement).await.unwrap();
            let before = std::fs::read(home.path().join("license.json")).unwrap();
            let matched = license.validate_any(cached_candidates(), 7, 30).unwrap();
            assert!(matches!(runtime.apply_refresh(&request, license, &matched).await,
                Err(PhoneHomeError::StaleResponse)));
            assert_eq!(std::fs::read(home.path().join("license.json")).unwrap(), before);
            let inner = runtime.state.read().await;
            assert_eq!(inner.revision, 1);
            assert_eq!(inner.license.as_ref().unwrap().max_agents, Some(42));
        }).await;
    }

    #[tokio::test]
    async fn external_install_fences_gateway_refresh_and_revocation() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &kp);
            storage::save_to(&license, &path).unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            let request = runtime.refresh_snapshot().await.unwrap();
            let mut replacement = license.clone();
            replacement.max_agents = Some(42);
            sign_license(&mut replacement, &kp);
            // This writer never touches the gateway's in-memory revision.
            storage::save_to(&replacement, &path).unwrap();
            let before = std::fs::read(&path).unwrap();
            let matched = license.validate_any(cached_candidates(), 7, 30).unwrap();
            assert!(matches!(runtime.apply_refresh(&request, license, &matched).await,
                Err(PhoneHomeError::StaleResponse)));
            let revoked = signed_response(&kp, None, &request, "revoked", "n");
            assert!(matches!(runtime.apply_revocation_response(&request, "n", &revoked).await,
                Err(PhoneHomeError::StaleResponse)));
            // The independently authenticated CRL path uses the same disk CAS.
            assert!(matches!(runtime.apply_revocation(&request, "crl_revoked").await,
                Err(PhoneHomeError::StaleResponse)));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!home.path().join(REVOKED_FILENAME).exists());
            assert_eq!(runtime.state.read().await.revision, 0);
            assert!(matches!(runtime.refresh_snapshot().await, Err(PhoneHomeError::StaleResponse)));
            assert_eq!(runtime.snapshot().await.max_agents, Some(42));
            assert!(runtime.refresh_snapshot().await.is_ok());
        }).await;
    }

    #[tokio::test]
    async fn external_revocation_or_deactivation_reconciles_gateway_memory() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            for revoke in [true, false] {
                let home = tempfile::tempdir().unwrap();
                let path = home.path().join("license.json");
                let (kp, pubkey) = gen_issuer_keypair();
                let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
                let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
                sign_license(&mut license, &kp);
                storage::save_to(&license, &path).unwrap();
                let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
                let store = storage::LockedLicenseStore::acquire(&path).unwrap();
                if revoke { store.revoke("cancelled").unwrap(); } else { store.delete().unwrap(); }
                drop(store);
                // CRL capture reconciles disk first, even though there is now no
                // subscription to fetch. It must not retain the old paid state.
                runtime.do_crl_fetch_once().await.unwrap();
                assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
                assert!(runtime.state.read().await.license.is_none());
                assert_eq!(runtime.snapshot().await.validation_warning, if revoke { Some("revoked") } else { None });
            }
        }).await;
    }

    #[test]
    fn bootstrap_read_waits_for_atomic_revocation() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.blocking_lock();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("license.json");
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
        sign_license(&mut license, &kp);
        storage::save_to(&license, &path).unwrap();
        let store = storage::LockedLicenseStore::acquire(&path).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let home_path = home.path().to_path_buf();
        let reader = std::thread::spawn(move || {
            let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).unwrap();
            started_tx.send(()).unwrap();
            done_tx.send(read_validated_license(&home_path, &registry, &gate)).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(StdDuration::from_millis(50)).is_err());
        store.revoke("cancelled").unwrap();
        drop(store);
        assert!(matches!(done_rx.recv_timeout(StdDuration::from_secs(5)).unwrap(), Err("revoked")));
        reader.join().unwrap();
    }

    #[tokio::test]
    async fn unsigned_or_tampered_revocation_cannot_change_disk_or_memory() {
        with_hardware_fixture(async {
            let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("license.json");
            let (kp, pubkey) = gen_issuer_keypair();
            let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
            let mut license = valid_license_for(TEST_HARDWARE_FINGERPRINT);
            sign_license(&mut license, &kp);
            storage::save_to(&license, &path).unwrap();
            let runtime = LicenseRuntime::bootstrap(home.path().into(), registry.clone()).await;
            let request = runtime.refresh_snapshot().await.unwrap();
            let signed = signed_response(&kp, None, &request, "revoked", "n");
            let before = std::fs::read(&path).unwrap();
            let mut forged = vec![json!({"status":"revoked"})];
            for field in ["reason", "effective_from", "nonce", "response_signature_version"] {
                let mut changed = signed.clone();
                changed.as_object_mut().unwrap().remove(field);
                forged.push(changed);
            }
            let mut changed = signed.clone();
            changed["reason"] = json!("different reason");
            forged.push(changed);
            for body in forged {
                assert!(runtime.apply_revocation_response(&request, "n", &body).await.is_err());
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert!(!home.path().join(REVOKED_FILENAME).exists());
                assert_eq!(runtime.snapshot().await.tier, LicenseTier::SelfHostPro);
            }
            runtime.apply_revocation_response(&request, "n", &signed).await.unwrap();
            assert!(!path.exists());
            assert!(home.path().join(REVOKED_FILENAME).exists());
            assert_eq!(runtime.snapshot().await.tier, LicenseTier::OpenSource);
            let restarted = LicenseRuntime::bootstrap(home.path().into(), registry).await;
            assert_eq!(restarted.snapshot().await.validation_warning, Some("revoked"));
        }).await;
    }

    #[tokio::test]
    async fn weak_installed_license_is_opensource_with_admin_only_rebind_warning() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.lock().await;
        let home = tempfile::tempdir().unwrap();
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let weak = cached_candidates().iter().find(|c|
            c.strength() == duduclaw_license::FingerprintStrength::Weak).unwrap();
        let mut license = valid_license_for(weak.fingerprint());
        sign_license(&mut license, &kp);
        storage::save_to(&license, &home.path().join("license.json")).unwrap();
        let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
        let snapshot = runtime.snapshot().await;
        assert_eq!(snapshot.tier, LicenseTier::OpenSource);
        assert_eq!(snapshot.mode, "opensource");
        assert!(snapshot.installed);
        assert_eq!(snapshot.validation_warning, Some("weak_binding"));
        assert_eq!(snapshot.for_admin(false).validation_warning, None);
        assert!(runtime.install_and_reload(license).await.is_err());
    }

    #[test]
    fn accept_refreshed_license_accepts_valid() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let fp = TEST_HARDWARE_FINGERPRINT.to_string();

        let mut lic = valid_license_for(&fp);
        sign_license(&mut lic, &kp);

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(
            res.is_ok(),
            "valid signed+current license must be accepted: {res:?}"
        );
        assert!(!res.unwrap().legacy, "strong match must not be flagged legacy");
    }

    #[test]
    fn accept_refreshed_license_accepts_a_legacy_candidate() {
        // A legacy real-hardware fingerprint remains accepted and flagged.
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let legacy_fp = "1743ebb5466bcad6c206e7e42371f45d".to_string();

        let mut lic = valid_license_for(&legacy_fp);
        sign_license(&mut lic, &kp);

        // Literal candidates, not this machine's real ones: a sandboxed test
        // runner can mask MACs, which would make strong == legacy and silently
        // stop exercising the fallback.
        let candidates = vec![FingerprintCandidate::for_test("strong-fp-of-this-machine", duduclaw_license::FingerprintStrength::Hardware),
            FingerprintCandidate::for_test(legacy_fp.clone(), duduclaw_license::FingerprintStrength::Hardware)];
        let matched = accept_test_response(&kp, &registry, &lic, &candidates, 7, 14)
            .expect("legacy-bound license must still be accepted");
        assert!(matched.legacy, "a compatibility match must be flagged legacy");
        assert_eq!(matched.fingerprint, legacy_fp);
    }

    #[test]
    fn effective_fingerprint_defaults_to_strong_and_records_legacy_matches() {
        let _fingerprint_guard = FINGERPRINT_TEST_LOCK.blocking_lock();
        // The effective fingerprint is what phone-home requests send.
        // It must fall back to the strong value whenever nothing matched, and
        // carry the legacy value while a legacy-bound license is active.
        clear_effective_fingerprint();
        assert_eq!(cached_fingerprint(), strong_fingerprint());

        set_effective_fingerprint("1743ebb5466bcad6c206e7e42371f45d");
        assert_eq!(cached_fingerprint(), "1743ebb5466bcad6c206e7e42371f45d");

        clear_effective_fingerprint();
        assert_eq!(cached_fingerprint(), strong_fingerprint());
    }

    #[test]
    fn candidate_list_leads_with_the_strong_fingerprint() {
        let candidates = cached_candidates();
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].fingerprint(), strong_fingerprint());
    }

    #[test]
    fn accept_refreshed_license_rejects_expired() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let fp = TEST_HARDWARE_FINGERPRINT.to_string();

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

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(res.is_err(), "expired license must be rejected");
    }

    #[test]
    fn accept_refreshed_license_rejects_wrong_fingerprint() {
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let current_fp = TEST_HARDWARE_FINGERPRINT.to_string();

        // Validly signed but bound to a *different* machine.
        let mut lic = valid_license_for("some-other-machine-fingerprint");
        sign_license(&mut lic, &kp);

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&current_fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(
            res.is_err(),
            "license bound to another machine must be rejected"
        );
    }

    #[test]
    fn accept_refreshed_license_rejects_bad_signature() {
        // Sign with one key but trust a *different* key → signature untrusted.
        let (kp, _pubkey) = gen_issuer_keypair();
        let (_other_kp, other_pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", other_pubkey);
        let fp = TEST_HARDWARE_FINGERPRINT.to_string();

        let mut lic = valid_license_for(&fp);
        sign_license(&mut lic, &kp);

        let res = accept_test_response(
            &kp,
            &registry,
            &lic,
            &[FingerprintCandidate::for_test(&fp, duduclaw_license::FingerprintStrength::Hardware)],
            7,
            14,
        );
        assert!(
            res.is_err(),
            "license signed by an untrusted key must be rejected"
        );
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
    fn cap_exceeded_semantics() {
        // 0 = unlimited → never capped.
        assert!(!cap_exceeded(0, 0));
        assert!(!cap_exceeded(0, 1_000));
        // Hobby: max 1 → first is allowed, second is capped.
        assert!(!cap_exceeded(1, 0));
        assert!(cap_exceeded(1, 1));
        // Studio: max 3.
        assert!(!cap_exceeded(3, 2));
        assert!(cap_exceeded(3, 3));
        assert!(cap_exceeded(3, 4));
    }

    #[test]
    fn agent_cap_enforced_p_license_override() {
        // Cloud always enforces, override or not.
        assert!(agent_cap_enforced(false, false));
        assert!(agent_cap_enforced(false, true));
        // Self-host is normally exempt (Apache 2.0)...
        assert!(!agent_cap_enforced(true, false));
        // ...but a signed per-license max_agents override DOES enforce on
        // self-host (§7(b)) — this is the whole point of P-License.
        assert!(agent_cap_enforced(true, true));
    }

    #[test]
    fn os_native_quota_personal_is_one_enterprise_unlimited() {
        use duduclaw_core::EditionProfile;
        // Personal edition: exactly one OS-native seat (quota lock, not a
        // capability lock).
        assert_eq!(os_native_agent_quota(EditionProfile::Personal), Some(1));
        // Enterprise edition: uncapped — the license schema has no os-native
        // field, so there is nothing to gate on.
        assert_eq!(os_native_agent_quota(EditionProfile::Enterprise), None);
    }

    #[test]
    fn effective_max_agents_via_snapshot_field() {
        // A LicenseSnapshot carries the per-license override so the enforcement
        // path can detect it (is_some) even on a self-host tier.
        let mut lic = fake_license(LicenseTier::SelfHostPro, "fp");
        let snap_none = LicenseSnapshot::from_state(Some(&lic), LicenseTier::SelfHostPro);
        assert!(snap_none.max_agents.is_none());
        lic.max_agents = Some(2);
        let snap_some = LicenseSnapshot::from_state(Some(&lic), LicenseTier::SelfHostPro);
        assert_eq!(snap_some.max_agents, Some(2));
    }

    // ── Baked production issuer key (single-binary commercial upgrade) ──────

    #[test]
    fn baked_prod_pubkey_is_empty_or_32_byte_hex() {
        // The baked constant is either empty (v2 issuance pending) or a valid
        // 32-byte Ed25519 public key. A malformed non-empty value would silently
        // disable commercial licensing on stock binaries, so guard against it.
        // This test survives the pending→baked transition unchanged.
        if PROD_ISSUER_PUBKEY_HEX.is_empty() {
            return; // v2 pending — env-only, fail-safe.
        }
        let bytes = hex::decode(PROD_ISSUER_PUBKEY_HEX).expect("baked pubkey must be valid hex");
        assert_eq!(bytes.len(), 32, "Ed25519 public key must be 32 bytes");
    }

    #[test]
    fn production_registry_matches_baked_state() {
        // With no env override for the active key id, the registry reflects the
        // baked state: empty constant ⇒ no key (v1 retired, v2 pending); a baked
        // key ⇒ present. Guard: skip if the host exports an override for this id.
        let env_var = format!(
            "DUDUCLAW_LICENSE_PUBKEY_{}",
            PROD_ISSUER_KEY_ID.to_ascii_uppercase()
        );
        if std::env::var(&env_var).is_ok() {
            return;
        }
        let reg = production_registry();
        if PROD_ISSUER_PUBKEY_HEX.is_empty() {
            assert!(
                reg.get(PROD_ISSUER_KEY_ID).is_none(),
                "no key must be baked while v2 issuance is pending"
            );
        } else {
            assert_eq!(reg.get(PROD_ISSUER_KEY_ID).map(|k| k.len()), Some(32));
        }
    }

    #[test]
    fn expiry_urgency_thresholds() {
        use ExpiryUrgency::*;
        assert_eq!(classify_expiry_urgency(-1), Expired);
        assert_eq!(classify_expiry_urgency(0), Critical); // expires today, still valid
        assert_eq!(classify_expiry_urgency(7), Critical);
        assert_eq!(classify_expiry_urgency(8), Warning);
        assert_eq!(classify_expiry_urgency(30), Warning);
        assert_eq!(classify_expiry_urgency(31), Ok);
        assert_eq!(classify_expiry_urgency(365), Ok);
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
