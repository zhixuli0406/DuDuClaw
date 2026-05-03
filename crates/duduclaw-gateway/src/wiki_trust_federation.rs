//! Phase 7 — Federation transport for wiki trust state.
//!
//! Glues `WikiTrustStore::{export_federated, import_federated}` to a daily
//! HTTP push so peer gateways can converge on per-agent wiki trust without
//! needing direct DB access. Wire format is plain JSON; auth is a shared
//! bearer token configured per peer in `config.toml`.
//!
//! Direction is push-only by design: each peer schedules its own outbound
//! sync, so a node that goes offline simply stops pushing and resumes when
//! it comes back. Receivers expose `POST /api/v1/wiki_trust/federation`.
//!
//! ```toml
//! # ~/.duduclaw/config.toml
//! [wiki.trust_feedback.federation]
//! enabled = true
//! interval_hours = 24
//! shared_secret = "abc123..."     # required for inbound auth
//! peers = [
//!   "https://peer-a.example.com",
//!   "https://peer-b.example.com",
//! ]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse, Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use duduclaw_memory::trust_store::{FederatedTrustUpdate, WikiTrustStore};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct FederationConfig {
    pub enabled: bool,
    pub interval_hours: u64,
    pub shared_secret: Option<String>,
    pub peers: Vec<String>,
    /// Persisted last-pushed checkpoint (per-process; falls back to
    /// `~/.duduclaw/wiki_trust_federation.json`).
    pub state_path: Option<PathBuf>,
}

impl FederationConfig {
    pub fn from_toml(root: &toml::Table) -> Self {
        let mut cfg = Self {
            enabled: false,
            interval_hours: 24,
            shared_secret: None,
            peers: Vec::new(),
            state_path: None,
        };
        let section = root
            .get("wiki")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("trust_feedback"))
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("federation"))
            .and_then(|v| v.as_table());
        let Some(s) = section else { return cfg; };

        cfg.enabled = s.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        if let Some(v) = s.get("interval_hours").and_then(|v| v.as_integer()) {
            cfg.interval_hours = v.clamp(1, 24 * 30) as u64;
        }
        if let Some(v) = s.get("shared_secret").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                cfg.shared_secret = Some(v.to_string());
            }
        }
        if let Some(arr) = s.get("peers").and_then(|v| v.as_array()) {
            for p in arr {
                if let Some(url) = p.as_str() {
                    let trimmed = url.trim();
                    if !trimmed.is_empty()
                        && (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
                        // (review MED R2: SSRF) reject loopback / private IPs
                        // so a config-write attacker can't pivot federation
                        // pushes against internal services.
                        && !is_internal_url(trimmed)
                    {
                        cfg.peers.push(trimmed.trim_end_matches('/').to_string());
                    }
                }
            }
        }
        if let Some(v) = s.get("state_path").and_then(|v| v.as_str()) {
            cfg.state_path = Some(PathBuf::from(v));
        }
        // Disable when explicitly enabled but missing peers — avoids surprise traffic.
        if cfg.peers.is_empty() {
            cfg.enabled = false;
        }
        cfg
    }
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

/// Wire-format version for the federation payload. Receivers reject any
/// payload with a `schema_version > FEDERATION_SCHEMA_VERSION` so a future
/// schema change can roll out without silently mangling legacy peers.
/// (review BLOCKER R2-3 / m8.)
pub const FEDERATION_SCHEMA_VERSION: u32 = 1;

/// Body of `POST /api/v1/wiki_trust/federation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationPushBody {
    /// Wire format version — defaults to 1 for backward compat with peers
    /// that pre-date the field.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Sender id — free-form, used for logging only.
    #[serde(default)]
    pub from: Option<String>,
    /// Updates to merge.
    pub updates: Vec<FederatedTrustUpdate>,
}

fn default_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize)]
struct FederationPushResponse {
    applied: u64,
    received: usize,
}

/// Persisted checkpoint — last successful push timestamp per peer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FederationState {
    /// Map of `peer_url → last_pushed_at` (RFC3339).
    last_pushed: std::collections::HashMap<String, String>,
}

impl FederationState {
    fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(c) => serde_json::from_str(&c).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            // Atomic-ish write: temp + rename.
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, &json).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    fn since(&self, peer: &str) -> DateTime<Utc> {
        self.last_pushed
            .get(peer)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|| Utc::now() - chrono::Duration::days(7))
    }

    fn set(&mut self, peer: &str, ts: DateTime<Utc>) {
        self.last_pushed
            .insert(peer.to_string(), ts.to_rfc3339());
    }
}

// ---------------------------------------------------------------------------
// Outbound — periodic push to peers
// ---------------------------------------------------------------------------

/// Spawn a tokio task that periodically pushes trust deltas to each
/// configured peer. Cancels naturally when the gateway shuts down (the task
/// is detached; cargo runtime drops it on process exit).
pub fn spawn_federation_pusher(store: Arc<WikiTrustStore>, cfg: FederationConfig) {
    if !cfg.enabled {
        return;
    }
    info!(
        peers = cfg.peers.len(),
        interval_hours = cfg.interval_hours,
        "wiki trust federation pusher enabled"
    );
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(Duration::from_secs(cfg.interval_hours * 3600));
        // Skip the immediate first tick so the first sync waits one full
        // interval — gives the gateway time to settle and avoids a flood
        // of inbound traffic on rolling deploys.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = run_federation_cycle(&store, &cfg).await {
                warn!(error = %e, "wiki trust federation cycle failed");
            }
        }
    });
}

/// Run one federation cycle: for each peer, export deltas since the last
/// successful push and POST them. Errors per-peer are logged but do not
/// stop the cycle; checkpoints are only updated on a 2xx response.
pub async fn run_federation_cycle(
    store: &Arc<WikiTrustStore>,
    cfg: &FederationConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state_path = cfg
        .state_path
        .clone()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".duduclaw/wiki_trust_federation.json"));
    let mut state = FederationState::load(&state_path);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    for peer in &cfg.peers {
        let since = state.since(peer);
        let updates = match store.export_federated(since) {
            Ok(u) => u,
            Err(e) => {
                warn!(peer, error = %e, "federation export failed");
                continue;
            }
        };
        if updates.is_empty() {
            debug!(peer, "no trust deltas to push");
            continue;
        }

        let body = FederationPushBody {
            schema_version: FEDERATION_SCHEMA_VERSION,
            from: hostname::get().ok().and_then(|h| h.into_string().ok()),
            updates,
        };
        let url = format!("{peer}/api/v1/wiki_trust/federation");
        let mut req = client.post(&url).json(&body);
        if let Some(secret) = cfg.shared_secret.as_deref() {
            req = req.bearer_auth(secret);
        }

        let sent = body.updates.len();
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                // (review MAJOR R3-arch) Receiver returns `{applied, received}`.
                // If applied < received, the receiver dropped some updates
                // (validation rejected, locked, etc.). The sender's
                // checkpoint advances regardless, so a sustained gap means
                // certain pages never converge.
                let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
                let applied = body.get("applied").and_then(|v| v.as_u64()).unwrap_or(0);
                if applied < sent as u64 {
                    crate::metrics::global_metrics().wiki_trust_federation_partial();
                    warn!(
                        peer,
                        sent,
                        applied,
                        gap = sent as u64 - applied,
                        "federation push partial: receiver dropped some updates"
                    );
                } else {
                    info!(peer, count = sent, "federation push ok");
                }
                state.set(peer, Utc::now());
            }
            Ok(resp) => {
                warn!(
                    peer,
                    status = %resp.status(),
                    "federation push rejected — keeping checkpoint"
                );
            }
            Err(e) => {
                warn!(peer, error = %e, "federation push failed — keeping checkpoint");
            }
        }
    }

    state.save(&state_path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Inbound — handler for POST /api/v1/wiki_trust/federation
// ---------------------------------------------------------------------------

/// Shared state passed into the federation route. We don't reuse the main
/// `AppState` here because trust feedback is a side concern and should
/// continue working even if the WS RPC layer is misconfigured.
#[derive(Clone)]
pub struct FederationServerState {
    pub store: Arc<WikiTrustStore>,
    pub shared_secret: Option<String>,
}

/// Coarse loopback / RFC1918 / link-local matcher for federation peer URLs.
/// Used as a defence-in-depth against SSRF when an attacker can write the
/// config file (review MED R2). Strings only — no DNS resolution; an
/// attacker who can rebind public DNS to a private IP at runtime is out of
/// scope (mitigations there belong to the HTTP client / firewall layer).
fn is_internal_url(url: &str) -> bool {
    // Strip scheme.
    let after_scheme = url
        .splitn(2, "://")
        .nth(1)
        .unwrap_or(url);
    // Authority section ends at first '/', '?', or '#'.
    let auth_end = after_scheme
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..auth_end];
    // Drop user-info if present.
    let after_userinfo = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    // Bracketed IPv6 authority `[::1]:8080` — peel the brackets first so a
    // port-trim doesn't shred the address.
    let host_lower = if let Some(end) = after_userinfo.strip_prefix('[') {
        let inner = end.split(']').next().unwrap_or("");
        inner.to_lowercase()
    } else {
        after_userinfo
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(after_userinfo)
            .to_lowercase()
    };
    if host_lower.is_empty() {
        return true; // unparseable → treat as internal (safe default)
    }
    // Percent-encoded hosts: reqwest decodes before resolving, so a string
    // check like `host.starts_with("127.")` would miss `%31%32%37...`.
    // Conservative: any `%` in the host means we can't statically classify.
    // (review CRITICAL R3 BYPASS-2.)
    if host_lower.contains('%') {
        return true;
    }
    if host_lower == "localhost"
        || host_lower == "0.0.0.0"
        || host_lower.ends_with(".localhost")
        // (review R4 BYPASS) Linux `getaddrinfo("0")` resolves to 0.0.0.0
        // which is loopback-equivalent on most systems. Same for shorthand
        // `0.0` / `0.0.0`. Reject defensively.
        || host_lower == "0"
        || host_lower == "0.0"
        || host_lower == "0.0.0"
    {
        return true;
    }
    if host_lower.starts_with("127.")
        || host_lower.starts_with("10.")
        || host_lower.starts_with("169.254.")
        || host_lower.starts_with("192.168.")
    {
        return true;
    }
    // 172.16.0.0/12 — match 172.16.* through 172.31.*
    if host_lower.starts_with("172.") {
        if let Some(octet) = host_lower.split('.').nth(1).and_then(|o| o.parse::<u8>().ok()) {
            if (16..=31).contains(&octet) {
                return true;
            }
        }
    }
    // 100.64.0.0/10 — RFC6598 CGNAT shared address space (some VPNs).
    if host_lower.starts_with("100.") {
        if let Some(octet) = host_lower.split('.').nth(1).and_then(|o| o.parse::<u8>().ok()) {
            if (64..=127).contains(&octet) {
                return true;
            }
        }
    }
    // IPv6 loopback / link-local / ULA. Bracket already peeled above.
    if host_lower == "::1"
        || host_lower.starts_with("fe80:")
        || host_lower.starts_with("fc")
        || host_lower.starts_with("fd")
    {
        return true;
    }
    // IPv4-mapped IPv6 (::ffff:a.b.c.d). reqwest happily resolves through
    // these to the underlying IPv4. (review CRITICAL R3 BYPASS-1.)
    if host_lower.starts_with("::ffff:") {
        return true;
    }
    false
}

/// Maximum number of `FederatedTrustUpdate` records per push.
/// Defends against malicious peers blowing up memory / SQLite tx duration
/// (review CRITICAL C2 — DoS via huge payload).
pub const MAX_FEDERATION_UPDATES_PER_PUSH: usize = 5_000;

/// Constant-time string comparison for bearer tokens.
/// Avoids the timing oracle inherent in Rust's `==`/`!=` for `&str`.
/// (review CRITICAL C1 — timing attack; R2 update — also avoid leaking
/// the secret length via early return on length mismatch.)
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut diff: u8 = (a.len() ^ b.len()) as u8 | ((a.len() ^ b.len()) >> 8) as u8;
    for i in 0..len {
        let x = if i < a.len() { a[i] } else { 0 };
        let y = if i < b.len() { b[i] } else { 0 };
        diff |= x ^ y;
    }
    diff == 0
}

/// `POST /api/v1/wiki_trust/federation` — accepts a `FederationPushBody`
/// from a peer, verifies the shared bearer secret if configured, and merges
/// updates via `WikiTrustStore::import_federated`.
///
/// Body size is capped via the layer-applied `DefaultBodyLimit` in
/// `server.rs`; this handler additionally caps the *number* of updates as
/// a second line of defence against batched DoS.
pub async fn handle_federation_push(
    State(state): State<FederationServerState>,
    headers: HeaderMap,
    Json(body): Json<FederationPushBody>,
) -> impl IntoResponse {
    if let Some(expected) = state.shared_secret.as_deref() {
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "));
        let ok = match provided {
            Some(p) => constant_time_eq(p.as_bytes(), expected.as_bytes()),
            None => false,
        };
        if !ok {
            return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({
                "error": "invalid or missing bearer token"
            }))).into_response();
        }
    }

    if body.schema_version > FEDERATION_SCHEMA_VERSION {
        warn!(
            from = ?body.from,
            sender_version = body.schema_version,
            our_version = FEDERATION_SCHEMA_VERSION,
            "federation: rejecting payload with newer schema version"
        );
        return (
            StatusCode::from_u16(426).unwrap_or(StatusCode::BAD_REQUEST),
            Json(serde_json::json!({
                "error": "Upgrade Required: payload schema_version exceeds receiver capability",
                "max_supported_version": FEDERATION_SCHEMA_VERSION,
            })),
        )
            .into_response();
    }

    if body.updates.len() > MAX_FEDERATION_UPDATES_PER_PUSH {
        warn!(
            count = body.updates.len(),
            limit = MAX_FEDERATION_UPDATES_PER_PUSH,
            from = ?body.from,
            "federation: rejecting oversized batch"
        );
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!("batch too large: max {MAX_FEDERATION_UPDATES_PER_PUSH} updates")
            })),
        )
            .into_response();
    }

    let received = body.updates.len();
    let applied = match state.store.import_federated(&body.updates) {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, from = ?body.from, "federation import failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({
                "error": format!("import failed: {e}")
            }))).into_response();
        }
    };

    info!(received, applied, from = ?body.from, "wiki trust federation merged");
    (
        StatusCode::OK,
        Json(serde_json::to_value(FederationPushResponse { applied, received }).unwrap()),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_disabled_when_no_peers() {
        let raw = r#"
            [wiki.trust_feedback.federation]
            enabled = true
        "#;
        let table: toml::Table = raw.parse().unwrap();
        let cfg = FederationConfig::from_toml(&table);
        assert!(!cfg.enabled, "no peers → disabled");
        assert!(cfg.peers.is_empty());
    }

    #[test]
    fn config_picks_up_full_section() {
        let raw = r#"
            [wiki.trust_feedback.federation]
            enabled = true
            interval_hours = 6
            shared_secret = "abc"
            peers = ["https://a.example.com/", "https://b.example.com"]
        "#;
        let table: toml::Table = raw.parse().unwrap();
        let cfg = FederationConfig::from_toml(&table);
        assert!(cfg.enabled);
        assert_eq!(cfg.interval_hours, 6);
        assert_eq!(cfg.shared_secret.as_deref(), Some("abc"));
        assert_eq!(cfg.peers.len(), 2);
        assert_eq!(cfg.peers[0], "https://a.example.com");
        assert_eq!(cfg.peers[1], "https://b.example.com");
    }

    #[test]
    fn config_filters_invalid_peer_urls() {
        let raw = r#"
            [wiki.trust_feedback.federation]
            enabled = true
            peers = ["bogus", "ftp://x", "https://ok.example"]
        "#;
        let table: toml::Table = raw.parse().unwrap();
        let cfg = FederationConfig::from_toml(&table);
        assert_eq!(cfg.peers, vec!["https://ok.example".to_string()]);
    }

    #[test]
    fn config_rejects_internal_peer_urls() {
        // Regression for review MED R2: SSRF defence in depth.
        let raw = r#"
            [wiki.trust_feedback.federation]
            enabled = true
            peers = [
                "http://localhost:8080",
                "http://127.0.0.1/x",
                "http://10.0.0.1",
                "http://192.168.1.5",
                "http://172.16.0.1",
                "http://172.31.255.255",
                "http://[::1]/y",
                "http://169.254.1.1",
                "https://public.example.com",
            ]
        "#;
        let table: toml::Table = raw.parse().unwrap();
        let cfg = FederationConfig::from_toml(&table);
        assert_eq!(cfg.peers, vec!["https://public.example.com".to_string()]);
    }

    #[test]
    fn config_rejects_ipv4_mapped_and_percent_encoded() {
        // Regression for review CRITICAL R3 BYPASS-1 + BYPASS-2.
        let raw = r#"
            [wiki.trust_feedback.federation]
            enabled = true
            peers = [
                "http://[::ffff:127.0.0.1]/x",
                "http://[::ffff:10.0.0.1]:8080",
                "http://%31%32%37%2e%30%2e%30%2e%31/internal",
                "http://100.64.0.1/cgnat",
                "https://public.example.com",
            ]
        "#;
        let table: toml::Table = raw.parse().unwrap();
        let cfg = FederationConfig::from_toml(&table);
        assert_eq!(cfg.peers, vec!["https://public.example.com".to_string()]);
    }

    #[test]
    fn constant_time_eq_handles_length_mismatch() {
        // Length-padded XOR: different lengths must compare false WITHOUT
        // returning early (review MED R2: timing oracle).
        assert!(!constant_time_eq(b"shortsec", b"longsecret"));
        assert!(!constant_time_eq(b"abcdef", b"abc"));
        assert!(constant_time_eq(b"identical", b"identical"));
        assert!(!constant_time_eq(b"identical", b"identicaL"));
    }

    #[test]
    fn state_round_trip_preserves_checkpoints() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("fed.json");
        let mut s = FederationState::default();
        let now = Utc::now();
        s.set("https://peer", now);
        s.save(&path);
        let reloaded = FederationState::load(&path);
        let parsed = reloaded.since("https://peer");
        // RFC3339 round-trips to millisecond precision typically.
        assert!(parsed.signed_duration_since(now).num_seconds().abs() <= 1);
    }
}
