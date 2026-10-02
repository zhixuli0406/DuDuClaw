//! Authentication of `POST /api/internal/computer-use` (design §3.2,
//! security review F7a) and the per-employee rate limit. All checks are
//! required and fail closed:
//!
//! 1. the TCP peer is loopback (the connection's address, never a forwarded
//!    header);
//! 2. the request carries `X-Duduclaw-Agent-Id`, `X-Duduclaw-Timestamp`
//!    (unix seconds), `X-Duduclaw-Nonce` (16 random bytes, 32 lowercase hex)
//!    and `X-Duduclaw-Signature` =
//!    `hex(HMAC-SHA256(internal key, agent_id \n agent_token \n timestamp \n
//!    nonce \n hex(sha256(body))))`
//!    ([`duduclaw_core::internal_request_signature`]). The gateway derives
//!    the agent token itself from `<home>/identity.key` (missing key ⇒
//!    refused) and tries every currently valid `gateway-internal` key
//!    ([`crate::mcp_internal_key::valid_internal_keys`]); the comparison is
//!    constant-time;
//! 3. the timestamp is within ±[`MAX_CLOCK_SKEW_SECS`] of the gateway's clock;
//! 4. the nonce has not been seen within [`NONCE_WINDOW`] ([`ReplayGuard`],
//!    recorded only for authenticated, rate-allowed requests, so
//!    unauthenticated traffic cannot grow it).
//!
//! Neither the internal key nor the identity token travels on the wire: a
//! local process that binds the port while the gateway is down receives a
//! signature good for nothing else. `Authorization` is ignored on this route.
//!
//! Known limit (same threat model as the identity token): a process of the
//! same OS user can read `identity.key` and the internal key.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;

/// Header carrying the calling employee's id.
pub const AGENT_ID_HEADER: &str = "x-duduclaw-agent-id";
/// Header carrying the request time (unix seconds, decimal).
pub const TIMESTAMP_HEADER: &str = "x-duduclaw-timestamp";
/// Header carrying 16 random bytes as 32 lowercase hex characters.
pub const NONCE_HEADER: &str = "x-duduclaw-nonce";
/// Header carrying the request signature (64 hex characters).
pub const SIGNATURE_HEADER: &str = "x-duduclaw-signature";

/// Accepted difference between the request time and the gateway's clock.
pub const MAX_CLOCK_SKEW_SECS: u64 = 60;
/// How long a used nonce is remembered (covers the whole skew window).
pub const NONCE_WINDOW: Duration = Duration::from_secs(120);
/// Most nonces remembered at once; when full, new requests are refused
/// (fail closed). The per-employee rate limit keeps real traffic far below.
pub const MAX_REMEMBERED_NONCES: usize = 65_536;

/// Requests one employee may make per minute.
pub const RATE_LIMIT_PER_MINUTE: usize = 120;
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// Which check refused the request. Never sent to the caller (the response
/// is one uniform refusal); logged at debug level and asserted by tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    NotLoopback,
    MissingHeaders,
    InvalidAgentId,
    MalformedHeaders,
    StaleTimestamp,
    NoIdentityKey,
    BadSignature,
    Replayed,
}

impl AuthFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotLoopback => "not_loopback",
            Self::MissingHeaders => "missing_headers",
            Self::InvalidAgentId => "invalid_agent_id",
            Self::MalformedHeaders => "malformed_headers",
            Self::StaleTimestamp => "stale_timestamp",
            Self::NoIdentityKey => "no_identity_key",
            Self::BadSignature => "bad_signature",
            Self::Replayed => "replayed",
        }
    }
}

/// A request whose signature verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authenticated {
    pub agent_id: String,
    pub nonce: String,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Check 1 alone (before the body is read).
pub fn check_peer(peer: SocketAddr) -> Result<(), AuthFailure> {
    // An IPv4-mapped IPv6 peer is canonicalised first.
    if peer.ip().to_canonical().is_loopback() {
        Ok(())
    } else {
        Err(AuthFailure::NotLoopback)
    }
}

/// Checks 1–3 (the nonce is recorded separately, after the rate limit).
/// `now_unix` is the gateway's clock in unix seconds.
pub fn authenticate(
    home: &Path,
    peer: SocketAddr,
    headers: &HeaderMap,
    body: &[u8],
    now_unix: u64,
) -> Result<Authenticated, AuthFailure> {
    check_peer(peer)?;
    let (Some(agent_id), Some(timestamp), Some(nonce), Some(signature)) = (
        header(headers, AGENT_ID_HEADER),
        header(headers, TIMESTAMP_HEADER),
        header(headers, NONCE_HEADER),
        header(headers, SIGNATURE_HEADER),
    ) else {
        return Err(AuthFailure::MissingHeaders);
    };
    // The id is validated before any use (paths, MAC input).
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Err(AuthFailure::InvalidAgentId);
    }
    let ts_ok = !timestamp.is_empty() && timestamp.len() <= 20 && timestamp.bytes().all(|c| c.is_ascii_digit());
    if !ts_ok || !is_lower_hex(nonce, 32) || !is_lower_hex(signature, 64) {
        return Err(AuthFailure::MalformedHeaders);
    }
    let Ok(sent_at) = timestamp.parse::<u64>() else {
        return Err(AuthFailure::MalformedHeaders);
    };
    if sent_at.abs_diff(now_unix) > MAX_CLOCK_SKEW_SECS {
        return Err(AuthFailure::StaleTimestamp);
    }
    let Some(identity_key) = duduclaw_core::load_identity_key(home) else {
        return Err(AuthFailure::NoIdentityKey);
    };
    let agent_token = duduclaw_core::mint_identity_token(&identity_key, agent_id);
    // Every valid internal key is tried, without early exit.
    let verified = crate::mcp_internal_key::valid_internal_keys(home).iter().fold(false, |ok, key| {
        ok | duduclaw_core::verify_internal_request_signature(
            key.as_bytes(),
            agent_id,
            &agent_token,
            timestamp,
            nonce,
            body,
            signature,
        )
    });
    if !verified {
        return Err(AuthFailure::BadSignature);
    }
    Ok(Authenticated { agent_id: agent_id.to_string(), nonce: nonce.to_string() })
}

/// Nonces of authenticated requests seen within [`NONCE_WINDOW`].
#[derive(Default)]
pub struct ReplayGuard {
    seen: Mutex<HashMap<String, Instant>>,
}

impl ReplayGuard {
    /// Record `nonce` at `now`. `false` when it was already seen within the
    /// window, or when the set is full after pruning (fail closed).
    pub fn record(&self, nonce: &str, now: Instant) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.retain(|_, at| now.saturating_duration_since(*at) < NONCE_WINDOW);
        if seen.contains_key(nonce) || seen.len() >= MAX_REMEMBERED_NONCES {
            return false;
        }
        seen.insert(nonce.to_string(), now);
        true
    }
}

/// Sliding one-minute window per employee.
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    /// Record one request at `now`; `false` when the employee is over the
    /// limit (the refused request is not counted).
    pub fn allow(&self, agent_id: &str, now: Instant) -> bool {
        let mut hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        // Drop employees whose window emptied, so the map stays bounded.
        hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < RATE_WINDOW));
        let queue = hits.entry(agent_id.to_string()).or_default();
        while queue.front().is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW) {
            queue.pop_front();
        }
        if queue.len() >= RATE_LIMIT_PER_MINUTE {
            return false;
        }
        queue.push_back(now);
        true
    }
}
