//! Owner-gateway control-plane endpoints (P2): `POST /v1/license/refresh` and
//! `GET /v1/license/crl`.
//!
//! When an owner instance is configured as a white-label issuer (`config.toml`
//! `[distributor] issuer_key_path`), it can also act as the *control-plane* for
//! the OEM licenses it signs — re-signing them on phone-home so they never trip
//! the 60-day offline downgrade, and publishing a signed CRL so revocations
//! propagate. This mirrors the cloud control-plane
//! (`commercial/cloud-control-plane/src/handlers/{refresh,crl}.rs`) so the
//! zero-code distributor client (which only reads `DUDUCLAW_CONTROL_URL`)
//! points at the owner gateway and Just Works.
//!
//! Security posture (fail-closed):
//!   - Both routes are public (no bearer) — trust is proven by
//!     `subscription_id` + `machine_fingerprint`, exactly like the cloud plane.
//!   - **Not configured as an issuer ⇒ 404** on every path: a plain gateway must
//!     not reveal that these endpoints even exist.
//!   - Per-IP rate limits (hand-written, same shape as `server.rs`): refresh
//!     30/min/IP, crl 60/min/IP.
//!   - The issuer private key is read per request, never logged, never echoed in
//!     any response; a key-read failure is a bare 500.
//!   - `refresh` NEVER extends validity (re-issue is the renewal path); it only
//!     advances `last_phone_home`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::distributor_store::{DistributorStore, IssuedLicense};

/// CRL TTL: 7 days, matching the cloud control-plane (`crl.rs`) and the
/// distributor-client polling cadence documented in the white-label guide.
const CRL_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// The issuer key id this owner instance signs under (pinned to the baked v2
/// production key so a stock download trusts refreshed/CRL output).
const ISSUER_KEY_ID: &str = "v2";

// ── Rate limiters (per-IP, hand-written — same pattern as server.rs) ─────────

static REFRESH_RATE_LIMITER: LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static CRL_RATE_LIMITER: LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// §10.3 branding-sign limiter: 10 req/min/IP (self-service bundle signing is
/// far rarer than phone-home).
static BRANDING_SIGN_RATE_LIMITER: LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Returns `true` if the request from `ip` is within `max` per 60s.
pub fn within_rate_limit(
    limiter: &Mutex<HashMap<IpAddr, (Instant, u32)>>,
    ip: IpAddr,
    max: u32,
) -> bool {
    let mut map = limiter.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if map.len() > 10000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 60);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 60 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= max
}

// ── State + router ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LicenseServeState {
    pub home_dir: PathBuf,
}

/// Build the control-plane router. Always mounted; every handler self-gates on
/// `[distributor] issuer_key_path` (absent ⇒ 404), so a non-issuer gateway
/// exposes no behaviour. 64 KiB body cap on the (tiny) refresh JSON.
pub fn router(home_dir: PathBuf) -> Router {
    Router::new()
        .route("/v1/license/refresh", post(refresh_handler))
        .route("/v1/license/crl", get(crl_handler))
        .route("/v1/branding/sign", post(branding_sign_handler))
        // Bundles carry an inline logo (≤512 KB) + 64 KB about_html → allow a
        // larger body here than the tiny refresh JSON.
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(LicenseServeState { home_dir })
}

/// Read `[distributor] issuer_key_path` from `config.toml`. `None` when
/// unset/empty — the caller then 404s (fail-closed: not an issuer).
async fn issuer_key_path(home_dir: &Path) -> Option<String> {
    let content = tokio::fs::read_to_string(home_dir.join("config.toml"))
        .await
        .ok()?;
    let table = content.parse::<toml::Table>().ok()?;
    table
        .get("distributor")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("issuer_key_path"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn store_for(home_dir: &Path) -> DistributorStore {
    DistributorStore::new(&home_dir.join("distributor.db"))
}

// ── POST /v1/license/refresh ─────────────────────────────────────────────────

/// Pure outcome of the three refresh rejection gates (spec §9.2 steps 3-5),
/// extracted so the exact branch logic the handler runs is unit-testable
/// without a live HTTP stack. Order is significant: revocation is terminal and
/// checked before fingerprint; expiry last.
#[derive(Debug, PartialEq, Eq)]
pub enum RefreshDecision {
    /// Terminal — the key is revoked; downgrade the client.
    Revoked { effective_from: String },
    /// 403 without revealing which field failed (fingerprint mismatch OR
    /// expired OR an unparseable stored `expires_at`).
    Forbidden,
    /// Passed all gates — safe to re-sign.
    Proceed,
}

/// Decide the refresh outcome for a found ledger record. `now` is injected so
/// the expiry gate is deterministic in tests.
pub fn refresh_decision(
    rec: &IssuedLicense,
    machine_fingerprint: &str,
    now: DateTime<Utc>,
) -> RefreshDecision {
    // 1) Revocation is terminal — checked before fingerprint (§9.2 step 3).
    if rec.status == "revoked" {
        let effective_from = rec
            .revoked_at
            .clone()
            .unwrap_or_else(|| now.to_rfc3339());
        return RefreshDecision::Revoked { effective_from };
    }
    // 2) Fingerprint mismatch → forbidden.
    if rec.machine_fingerprint != machine_fingerprint {
        return RefreshDecision::Forbidden;
    }
    // 3) Expired (or unparseable stored timestamp) → forbidden; refresh never
    //    extends validity, so a lapsed key is not re-signed.
    match DateTime::parse_from_rfc3339(&rec.expires_at) {
        Ok(exp) if now > exp.with_timezone(&Utc) => RefreshDecision::Forbidden,
        Err(_) => RefreshDecision::Forbidden,
        _ => RefreshDecision::Proceed,
    }
}

async fn refresh_handler(
    State(state): State<LicenseServeState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !within_rate_limit(&REFRESH_RATE_LIMITER, addr.ip(), 30) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "rate_limited" })))
            .into_response();
    }

    // Not an issuer ⇒ 404 (do not reveal the endpoint exists).
    let key_path = match issuer_key_path(&state.home_dir).await {
        Some(p) => p,
        None => return not_found(),
    };

    let subscription_id = body
        .get("subscription_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let machine_fingerprint = body
        .get("machine_fingerprint")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // Optional anti-replay nonce (gateway client sends it; CLI client does not).
    let nonce = body
        .get("nonce")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // `telemetry` is accepted but ignored (P3).

    if subscription_id.is_empty() || machine_fingerprint.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "subscription_id and machine_fingerprint are required" })),
        )
            .into_response();
    }

    let store = store_for(&state.home_dir);
    let rec = match store.get_license_by_subscription_id(&subscription_id) {
        Some(r) => r,
        None => return not_found(),
    };

    // Three rejection gates (revoked → fingerprint → expired), spec §9.2.
    match refresh_decision(&rec, &machine_fingerprint, Utc::now()) {
        RefreshDecision::Revoked { effective_from } => {
            return (
                StatusCode::OK,
                Json(json!({
                    "status": "revoked",
                    "reason": "license_revoked",
                    "effective_from": effective_from,
                })),
            )
                .into_response();
        }
        RefreshDecision::Forbidden => {
            return (StatusCode::FORBIDDEN, Json(json!({ "error": "forbidden" }))).into_response();
        }
        RefreshDecision::Proceed => {}
    }

    // Load the issuer seed per request; a read failure is a bare 500.
    let seed = match crate::distributor_store::load_issuer_signing_seed(Path::new(&key_path)) {
        Ok(s) => s,
        Err(_) => return internal_error(),
    };

    let registry = crate::license_runtime::production_registry();
    let (license, _blob) =
        match crate::distributor_store::resign_license_for_refresh(&seed, &registry, &rec) {
            Ok(v) => v,
            Err(_) => return internal_error(),
        };

    // Best-effort ledger update (still-alive signal). Never blocks the response.
    let _ = store.touch_refresh(&rec.id);

    // Audit — private key never appears.
    duduclaw_security::audit::append_audit_event(
        &state.home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "license_refresh_served",
            "control-plane",
            duduclaw_security::audit::Severity::Info,
            json!({
                "license_id": rec.id,
                "subscription_id": rec.subscription_id,
                "tier": rec.tier,
            }),
        ),
    );

    let license_v = match serde_json::to_value(&license) {
        Ok(v) => v,
        Err(_) => return internal_error(),
    };
    let mut resp = json!({
        "status": "active",
        "license": license_v,
        "warnings": [],
    });
    // Echo the request nonce verbatim (anti-replay) when present.
    if let (Some(n), Some(obj)) = (nonce, resp.as_object_mut()) {
        obj.insert("nonce".into(), Value::String(n));
    }
    (StatusCode::OK, Json(resp)).into_response()
}

// ── GET /v1/license/crl ──────────────────────────────────────────────────────

/// CRL payload — field order IS the wire format and must be byte-identical to
/// `duduclaw_license::crl`'s private `CrlPayload` so `SignedCrl::verify` matches
/// the canonical bytes. Do not reorder.
#[derive(Serialize)]
struct CrlPayload<'a> {
    generated_at: DateTime<Utc>,
    revoked: &'a [String],
    ttl_seconds: u64,
    public_key_id: &'a str,
}

/// Sign the CRL canonical payload with the issuer seed. Pure so the byte-level
/// signature is unit-testable against `SignedCrl::verify`.
pub fn sign_crl(
    generated_at: DateTime<Utc>,
    revoked: &[String],
    ttl_seconds: u64,
    public_key_id: &str,
    signing_seed: &[u8; 32],
) -> Result<Vec<u8>, String> {
    use ed25519_dalek::{Signer, SigningKey};
    let payload = CrlPayload {
        generated_at,
        revoked,
        ttl_seconds,
        public_key_id,
    };
    let canonical =
        serde_json::to_vec(&payload).map_err(|e| format!("serialize CRL payload: {e}"))?;
    let key = SigningKey::from_bytes(signing_seed);
    Ok(key.sign(&canonical).to_bytes().to_vec())
}

async fn crl_handler(
    State(state): State<LicenseServeState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    if !within_rate_limit(&CRL_RATE_LIMITER, addr.ip(), 60) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "rate_limited" })))
            .into_response();
    }

    let key_path = match issuer_key_path(&state.home_dir).await {
        Some(p) => p,
        None => return not_found(),
    };
    let seed = match crate::distributor_store::load_issuer_signing_seed(Path::new(&key_path)) {
        Ok(s) => s,
        Err(_) => return internal_error(),
    };

    let store = store_for(&state.home_dir);
    let revoked: Vec<String> = store
        .list_licenses(None)
        .into_iter()
        .filter(|l| l.status == "revoked")
        .map(|l| l.subscription_id)
        .collect();

    let generated_at = Utc::now();
    let signature = match sign_crl(generated_at, &revoked, CRL_TTL_SECONDS, ISSUER_KEY_ID, &seed) {
        Ok(s) => s,
        Err(_) => return internal_error(),
    };

    (
        StatusCode::OK,
        Json(json!({
            "generated_at": generated_at,
            "revoked": revoked,
            "ttl_seconds": CRL_TTL_SECONDS,
            "public_key_id": ISSUER_KEY_ID,
            "signature": BASE64.encode(signature),
        })),
    )
        .into_response()
}

// ── POST /v1/branding/sign (§10.3) ───────────────────────────────────────────

/// Sign a distributor's branding into a portable, self-applying bundle.
///
/// Trust model mirrors `refresh`: the caller proves entitlement with
/// `subscription_id` + `machine_fingerprint` (reusing [`refresh_decision`] as
/// the gate), so a revoked / expired / fingerprint-mismatched key cannot obtain
/// a fresh bundle. The owner is the authoritative sanitizer — the submitted
/// branding is run through `validate_input` (ammonia + all field checks) before
/// signing, so a distributor cannot smuggle unsafe HTML into the signed body.
async fn branding_sign_handler(
    State(state): State<LicenseServeState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !within_rate_limit(&BRANDING_SIGN_RATE_LIMITER, addr.ip(), 10) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": "rate_limited" })))
            .into_response();
    }

    // Not an issuer ⇒ 404 (do not reveal the endpoint exists).
    let key_path = match issuer_key_path(&state.home_dir).await {
        Some(p) => p,
        None => return not_found(),
    };

    let subscription_id = body
        .get("subscription_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let machine_fingerprint = body
        .get("machine_fingerprint")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if subscription_id.is_empty() || machine_fingerprint.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "subscription_id and machine_fingerprint are required" })),
        )
            .into_response();
    }

    // Parse the submitted branding (may include server-stamped fields — they are
    // dropped by the `BrandingConfig → BrandingInput` projection).
    let submitted: crate::branding::BrandingConfig = match body.get("branding") {
        Some(v) => match serde_json::from_value(v.clone()) {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("invalid branding: {e}") })),
                )
                    .into_response();
            }
        },
        None => crate::branding::BrandingConfig::default(),
    };

    let store = store_for(&state.home_dir);
    let rec = match store.get_license_by_subscription_id(&subscription_id) {
        Some(r) => r,
        None => return not_found(),
    };

    // Same three gates as refresh — revoked / fingerprint / expired.
    match refresh_decision(&rec, &machine_fingerprint, Utc::now()) {
        RefreshDecision::Revoked { .. } => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "license_revoked" })),
            )
                .into_response();
        }
        RefreshDecision::Forbidden => {
            return (StatusCode::FORBIDDEN, Json(json!({ "error": "forbidden" }))).into_response();
        }
        RefreshDecision::Proceed => {}
    }

    // Owner-authoritative sanitize + validation (fail-closed on any bad field).
    let sanitized = match crate::branding::validate_input(submitted.into()) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("branding rejected: {e}") })),
            )
                .into_response();
        }
    };

    let seed = match crate::distributor_store::load_issuer_signing_seed(Path::new(&key_path)) {
        Ok(s) => s,
        Err(_) => return internal_error(),
    };

    let issued_at = Utc::now().to_rfc3339();
    let bundle = match crate::branding::sign_bundle(
        &seed,
        &rec.distributor_id,
        &subscription_id,
        &sanitized,
        &issued_at,
        crate::branding::BUNDLE_KEY_ID,
    ) {
        Ok(b) => b,
        Err(_) => return internal_error(),
    };

    duduclaw_security::audit::append_audit_event(
        &state.home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "branding_bundle_signed",
            "control-plane",
            duduclaw_security::audit::Severity::Info,
            json!({
                "distributor_id": rec.distributor_id,
                "subscription_id": rec.subscription_id,
            }),
        ),
    );

    let bundle_v = match serde_json::to_value(&bundle) {
        Ok(v) => v,
        Err(_) => return internal_error(),
    };
    (StatusCode::OK, Json(bundle_v)).into_response()
}

// ── Error responses (no internal detail leaks) ───────────────────────────────

fn not_found() -> axum::response::Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
}

fn internal_error() -> axum::response::Response {
    // Deliberately opaque — a key-read/sign failure must not leak paths.
    warn!("license control-plane: internal error serving request");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal_error" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_license::{PublicKeyRegistry, SignedCrl};
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    #[test]
    fn signed_crl_verifies_with_matching_key() {
        let signing = SigningKey::generate(&mut OsRng);
        let seed: [u8; 32] = signing.to_bytes();
        let registry =
            PublicKeyRegistry::new().with_key("v2", signing.verifying_key().to_bytes().to_vec());

        let generated_at = Utc::now();
        let revoked = vec!["dist-a-1".to_string(), "dist-b-2".to_string()];
        let sig = sign_crl(generated_at, &revoked, CRL_TTL_SECONDS, "v2", &seed).unwrap();

        // Reconstruct the wire document the handler would emit, then verify it
        // through the *client* verifier — proving byte-level payload alignment.
        let crl = SignedCrl {
            generated_at,
            revoked: revoked.clone(),
            ttl_seconds: CRL_TTL_SECONDS,
            public_key_id: "v2".to_string(),
            signature: BASE64.encode(sig),
        };
        assert!(crl.verify(&registry).is_ok());
        assert!(crl.is_revoked("dist-a-1"));
        assert!(!crl.is_revoked("dist-z-9"));
    }

    #[test]
    fn signed_crl_rejects_tampered_list() {
        let signing = SigningKey::generate(&mut OsRng);
        let seed: [u8; 32] = signing.to_bytes();
        let registry =
            PublicKeyRegistry::new().with_key("v2", signing.verifying_key().to_bytes().to_vec());

        let generated_at = Utc::now();
        let revoked = vec!["dist-a-1".to_string()];
        let sig = sign_crl(generated_at, &revoked, CRL_TTL_SECONDS, "v2", &seed).unwrap();

        let mut crl = SignedCrl {
            generated_at,
            revoked,
            ttl_seconds: CRL_TTL_SECONDS,
            public_key_id: "v2".to_string(),
            signature: BASE64.encode(sig),
        };
        crl.revoked.push("dist-injected".into());
        assert!(crl.verify(&registry).is_err());
    }

    fn rec_with(status: &str, fingerprint: &str, expires_at: DateTime<Utc>) -> IssuedLicense {
        IssuedLicense {
            id: "lic".into(),
            distributor_id: "d".into(),
            subscription_id: "dist-x-1".into(),
            customer_id: "dist-x".into(),
            tier: "oem".into(),
            machine_fingerprint: fingerprint.into(),
            issued_at: Utc::now().to_rfc3339(),
            expires_at: expires_at.to_rfc3339(),
            status: status.into(),
            revoked_at: if status == "revoked" {
                Some("2026-07-01T00:00:00+00:00".into())
            } else {
                None
            },
            license_blob: "blob".into(),
            last_refresh_at: None,
        }
    }

    #[test]
    fn refresh_decision_revoked_is_terminal_even_with_bad_fingerprint() {
        let rec = rec_with("revoked", "fp-a", Utc::now() + chrono::Duration::days(365));
        // Wrong fingerprint, but revoked wins (checked first).
        match refresh_decision(&rec, "fp-WRONG", Utc::now()) {
            RefreshDecision::Revoked { effective_from } => {
                assert_eq!(effective_from, "2026-07-01T00:00:00+00:00");
            }
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    #[test]
    fn refresh_decision_fingerprint_mismatch_is_forbidden() {
        let rec = rec_with("active", "fp-a", Utc::now() + chrono::Duration::days(365));
        assert_eq!(
            refresh_decision(&rec, "fp-b", Utc::now()),
            RefreshDecision::Forbidden
        );
    }

    #[test]
    fn refresh_decision_expired_is_forbidden() {
        let rec = rec_with("active", "fp-a", Utc::now() - chrono::Duration::days(1));
        assert_eq!(
            refresh_decision(&rec, "fp-a", Utc::now()),
            RefreshDecision::Forbidden
        );
    }

    #[test]
    fn refresh_decision_valid_proceeds() {
        let rec = rec_with("active", "fp-a", Utc::now() + chrono::Duration::days(30));
        assert_eq!(
            refresh_decision(&rec, "fp-a", Utc::now()),
            RefreshDecision::Proceed
        );
    }

    #[test]
    fn rate_limiter_trips_after_max() {
        let limiter: Mutex<HashMap<IpAddr, (Instant, u32)>> = Mutex::new(HashMap::new());
        let ip: IpAddr = "203.0.113.5".parse().unwrap();
        // First `max` requests pass; the next one trips.
        for _ in 0..3 {
            assert!(within_rate_limit(&limiter, ip, 3));
        }
        assert!(!within_rate_limit(&limiter, ip, 3));
        // A different IP is unaffected.
        let other: IpAddr = "203.0.113.6".parse().unwrap();
        assert!(within_rate_limit(&limiter, other, 3));
    }
}
