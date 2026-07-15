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
const DEFAULT_CONTROL_URL: &str = "https://api.duduclaw.dudustudio.monster";

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
/// default. A blank env value is treated as unset.
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

    /// The resolved control-plane base URL for this instance (§10.5). Used by
    /// `branding.bundle.create` to reach the owner gateway's `/v1/branding/sign`
    /// with the exact same precedence phone-home uses.
    pub fn control_url(&self) -> &str {
        &self.control_url
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
        let tier = self.current_tier().await;
        let inner = self.state.read().await;
        LicenseSnapshot::from_state(inner.license.as_ref(), tier)
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
        let current_fp = generate_fingerprint();
        if !license.is_valid_for_machine(&current_fp) {
            return Err(format!(
                "授權綁定的機器指紋與本機不符（本機指紋：{current_fp}）。\
                 若是從舊機器搬移，請在本機執行 `duduclaw license rebind`"
            ));
        }
        if license.is_expired() {
            return Err(format!("授權已於 {} 過期", license.expires_at));
        }
        // M51 pre-check: surface the tier ↔ deployment-mode mismatch BEFORE
        // writing anything — the post-save re-validation would reject it
        // anyway, but with an error that hides the actionable reason.
        let is_self_host = is_self_host_deployment();
        if license.validate_tier_deployment_binding(is_self_host).is_err() {
            return Err(format!(
                "授權方案「{}」與本機部署型態不符：本機是{}部署。\
                 自架機器請簽發 self-host-pro / personal-pro-self-host / partner / oem；\
                 雲端託管才使用 solo / studio / business",
                license.tier,
                if is_self_host { "自架（self-host）" } else { "雲端（cloud）" },
            ));
        }
        save_default(&license).map_err(|e| format!("寫入授權檔失敗：{e}"))?;

        // A freshly-verified license supersedes a stale revocation marker
        // (re-subscription path). If this subscription is ALSO on the CRL, the
        // background CRL poller re-revokes it — we don't weaken that check.
        let marker = self.home_dir.join(REVOKED_FILENAME);
        if marker.exists() {
            if let Err(e) = std::fs::remove_file(&marker) {
                warn!(error = %e, "could not clear revocation marker after new license install");
            } else {
                info!("revocation marker cleared by newly-installed license");
            }
        }

        let validated = load_and_validate(&self.registry, &self.gate).await;
        let ok = validated.is_some();
        {
            let mut inner = self.state.write().await;
            inner.license = validated;
        }
        if !ok {
            return Err(
                "授權已寫入但完整驗證未通過（詳見 gateway 日誌），目前仍為 OpenSource 模式"
                    .to_string(),
            );
        }
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

/// Redeem a partner (NFR) code at the control-plane and install the returned
/// license. Mirrors `duduclaw license redeem`, but finishes with the runtime
/// hot-reload so the dashboard unlocks immediately.
pub async fn redeem_partner_code(
    rt: &LicenseRuntime,
    code: &str,
    email: Option<String>,
) -> Result<LicenseSnapshot, String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("兌換碼不可為空".to_string());
    }
    let endpoint = format!("{}/v1/partner/redeem", rt.control_url().trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(20))
        .build()
        .map_err(|e| format!("建立 HTTP client 失敗：{e}"))?;
    let body = json!({
        "code": code,
        "machine_fingerprint": generate_fingerprint(),
        "email": email,
    });
    let resp = client
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("無法連線授權伺服器：{e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(format!("授權伺服器回應 HTTP {status}：{msg}"));
    }
    let envelope: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("兌換回應解析失敗：{e}"))?;
    let license: License = envelope
        .get("license")
        .cloned()
        .ok_or_else(|| "兌換回應缺少 license 欄位".to_string())
        .and_then(|v| serde_json::from_value(v).map_err(|e| format!("授權內容解析失敗：{e}")))?;
    rt.install_and_reload(license).await
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
    /// WP8: the active license's signed white-label field-level edit claim, if
    /// any. `None` (no claim) resolves to the full vendor-editable set — see
    /// `crate::branding::resolve_edit_scope`.
    pub branding_editable: Option<Vec<String>>,
    /// P-License: the active license's signed per-license agent-count quota, if
    /// any. `None` = no override → the tier default applies. Surfaced so the
    /// enforcement path can detect an explicit issuer-set limit (`is_some()`)
    /// even on a self-host tier — see `handlers::tier_limit_message`.
    pub max_agents: Option<u32>,
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
            branding_editable: license.and_then(|l| l.branding_editable.clone()),
            max_agents: license.and_then(|l| l.max_agents),
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
///
/// Unbound-OEM note: the machine-binding decision lives entirely inside
/// `License::validate` (empty fingerprint + `tier == Oem` ⇒ skip binding, all
/// other tiers ⇒ `InvalidFingerprint`). Both the bootstrap load
/// (`load_and_validate`) and this refresh path route through `validate`, so an
/// unbound OEM license refreshed by the control-plane is accepted here for the
/// same reason it loaded at boot. Do NOT add an independent `machine_fingerprint`
/// comparison in this path — it would re-break the Docker-rebuild case.
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
        let bytes = hex::decode(PROD_ISSUER_PUBKEY_HEX)
            .expect("baked pubkey must be valid hex");
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
