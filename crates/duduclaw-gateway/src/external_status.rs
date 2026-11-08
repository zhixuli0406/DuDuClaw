//! RM-064 — an external business system reads AI-employee work status.
//!
//! `GET /api/v1/status/employees` with `Authorization: Bearer ddc_status_…`.
//! The credential belongs to one dashboard account (`duduclaw_auth::
//! status_tokens`); the employees shown are that account's *direct* bindings,
//! read live on every request. The answer is counts plus the latest finished
//! task's title (masked, at most [`TITLE_MAX_CHARS`] characters) and time —
//! never conversation text, task descriptions or any id other than
//! `agent_id`.
//!
//! ## Why the gateway's dashboard HTTP server
//!
//! The approval broker and the task store both live in the gateway process,
//! and the dashboard HTTP server already holds `MethodHandler` (task store,
//! agent registry) and `UserDb` (bindings). Serving here needs no new signed
//! loopback route between the MCP process and the gateway, and the MCP HTTP
//! server's credential surface (`[mcp_keys]`, OAuth) stays untouched. The
//! Odoo connector already talks to this server.
//!
//! ## Fail-closed posture
//!
//! - `config.toml [external_status] enabled` defaults to **false**. The value
//!   is cached for at most [`ENABLED_TTL`] and dropped as soon as the
//!   dashboard writes the config ([`invalidate_enabled_cache`]); while off
//!   every request (any method, any credential) gets the same generic `404`,
//!   and no credential is looked up.
//! - Admission comes before any credential or database work: a global
//!   limit ([`GLOBAL_LIMIT_PER_MINUTE`], checked first so its refusals never
//!   touch the source map) and a per-source one ([`SOURCE_LIMIT_PER_MINUTE`],
//!   IPv6 by /64, bounded O(1) map with eviction), so a flood of
//!   random well-formed credentials is refused with `429` without touching
//!   SQLite. Credential lookups and snapshots then run at most
//!   [`MAX_CONCURRENT_WORK`] at a time; a request that cannot get a slot
//!   quickly gets `503`.
//! - Only `GET`. The only query parameter is an optional `agent_id`, which
//!   must be one of the caller's bound employees (else `404`).
//! - Unknown / revoked credentials, an inactive owner or a database error
//!   never reach the counts. Every attributable call is audited (credential
//!   id, account id, result, number of employees) with no response content.
//! - The counts are the owner's: another account's private A2A work, tasks
//!   whose packet audience excludes the owner and approvals the owner could
//!   not read on the dashboard are not counted, and an unreadable ACL refuses
//!   the request.
//! - A title is returned only while the on-device PII model (the redaction
//!   NER) is installed and runs on it; it is Unicode-normalised, then
//!   credential-, model- and rule-masked before truncation and rescanned
//!   after ([`title_mask`]). Without the model, or when anything cannot be
//!   masked, the title is omitted (`null`).
//! - Per-credential rate limit ([`RATE_LIMIT_PER_MINUTE`]); no CORS headers
//!   (server-to-server), `Cache-Control: no-store`.
//! - Audit rows are aggregated per (credential, result, 10-minute window):
//!   the first call writes the audit-log row, later ones only bump a counter
//!   (`duduclaw_auth::status_tokens`), and `last_used_at` moves at most once
//!   per window. Both are pruned after 90 days.

pub(crate) mod title_mask;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Local, TimeZone, Utc};
use duduclaw_auth::UserDb;
use duduclaw_auth::status_tokens::{StatusTokenLookup, is_status_token};
use serde::Serialize;
use serde_json::json;

use crate::handlers::MethodHandler;

/// The one route this credential type may call.
pub const STATUS_PATH: &str = "/api/v1/status/employees";
/// Calls per credential per minute.
pub const RATE_LIMIT_PER_MINUTE: u32 = 60;
/// Requests admitted per source address per minute, before any credential
/// work. Several integrations behind one proxy share it.
pub const SOURCE_LIMIT_PER_MINUTE: u32 = 240;
/// Requests admitted per minute across all sources, before any credential
/// work.
pub const GLOBAL_LIMIT_PER_MINUTE: u32 = 600;
/// Credential lookups + snapshots running at once.
pub const MAX_CONCURRENT_WORK: usize = 4;
/// How long a request may wait for a work slot before `503`.
pub const WORK_WAIT: Duration = Duration::from_secs(2);
/// Longest reuse of one `[external_status] enabled` read.
pub const ENABLED_TTL: Duration = Duration::from_secs(2);
/// Longest task title returned (characters, not bytes).
pub const TITLE_MAX_CHARS: usize = 60;
/// Longest `agent_id` filter accepted.
const AGENT_ID_MAX_CHARS: usize = 64;
/// Distinct keys tracked by one limiter at once. Past this the credential
/// limiter refuses new keys rather than growing (fail closed); the source
/// limiter evicts the oldest window instead (the global limit still holds).
const LIMITER_MAX_KEYS: usize = 4096;
const WINDOW: Duration = Duration::from_secs(60);

// ── Config ──────────────────────────────────────────────────────

/// `config.toml [external_status]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExternalStatusConfig {
    /// Master switch. Defaults **false**; a non-boolean value reads as false.
    pub enabled: bool,
}

impl ExternalStatusConfig {
    /// Missing / unreadable / malformed `config.toml` ⇒ disabled.
    pub fn from_home(home: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(home.join("config.toml")) else {
            return Self::default();
        };
        let Ok(table) = raw.parse::<toml::Table>() else {
            return Self::default();
        };
        Self::from_table(&table)
    }

    pub fn from_table(table: &toml::Table) -> Self {
        let enabled = table
            .get("external_status")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self { enabled }
    }
}

/// Is the feature switched on right now? Reads `config.toml`; the HTTP path
/// uses the gate's cached copy instead.
pub fn enabled(home: &Path) -> bool {
    ExternalStatusConfig::from_home(home).enabled
}

/// Drop the endpoint's cached `enabled` flag so the next request re-reads
/// `config.toml`. Called after the dashboard commits a config change.
pub fn invalidate_enabled_cache() {
    GATE.invalidate();
}

// ── Request checks ──────────────────────────────────────────────

fn valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= AGENT_ID_MAX_CHARS
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The optional `agent_id` filter. Any other parameter, a repeated
/// `agent_id`, or an implausible value is refused.
pub(crate) fn parse_query(raw: Option<&str>) -> Result<Option<String>, ()> {
    let Some(raw) = raw.filter(|q| !q.is_empty()) else {
        return Ok(None);
    };
    let mut agent = None;
    for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if key != "agent_id" || agent.is_some() || !valid_agent_id(&value) {
            return Err(());
        }
        agent = Some(value.into_owned());
    }
    Ok(agent)
}

/// `Authorization: Bearer ddc_status_…`, or `None`.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| is_status_token(t))
}

/// Limiter key for a peer: the IPv4 address, or the /64 of an IPv6 one (a
/// single host commonly owns a whole /64). No peer ⇒ one shared key.
fn source_key(peer: Option<IpAddr>) -> String {
    match peer {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        None => "unknown".into(),
    }
}

// ── Rate limit ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateDecision {
    Allowed,
    /// Over the limit; `first` is true for the first refusal in the window,
    /// so a client hammering the endpoint writes one audit row, not one per call.
    Limited { first: bool },
}

/// "No slot" in the limiter's intrusive list.
const NIL: usize = usize::MAX;

/// One key's window, linked in window-start order.
struct WindowSlot {
    key: String,
    start: Instant,
    count: u32,
    prev: usize,
    next: usize,
}

/// Fixed one-minute window per key, at most `max_keys` keys, O(1) per call.
///
/// Slots live in a fixed-capacity arena, indexed by a `HashMap`, and are
/// linked oldest-window-first: a new or restarted window moves to the tail,
/// so the head is always the oldest window. Making room for a new key looks
/// at the head only — reclaimed if expired, evicted if this limiter evicts,
/// otherwise the new key is refused — never a scan of every key.
pub(crate) struct Limiter {
    index: HashMap<String, usize>,
    slots: Vec<WindowSlot>,
    free: Vec<usize>,
    head: usize,
    tail: usize,
    max_keys: usize,
    evict_when_full: bool,
}

impl Limiter {
    fn with(max_keys: usize, evict_when_full: bool) -> Self {
        let max_keys = max_keys.max(1);
        Self {
            index: HashMap::with_capacity(max_keys),
            slots: Vec::new(),
            free: Vec::new(),
            head: NIL,
            tail: NIL,
            max_keys,
            evict_when_full,
        }
    }

    /// Refuses new keys while full of live windows.
    pub(crate) fn new() -> Self {
        Self::with(LIMITER_MAX_KEYS, false)
    }

    /// Evicts the oldest window to admit a new key while full.
    pub(crate) fn evicting(max_keys: usize) -> Self {
        Self::with(max_keys, true)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.index.len()
    }

    fn unlink(&mut self, i: usize) {
        let (prev, next) = (self.slots[i].prev, self.slots[i].next);
        match prev {
            NIL => self.head = next,
            p => self.slots[p].next = next,
        }
        match next {
            NIL => self.tail = prev,
            n => self.slots[n].prev = prev,
        }
        self.slots[i].prev = NIL;
        self.slots[i].next = NIL;
    }

    fn push_back(&mut self, i: usize) {
        self.slots[i].prev = self.tail;
        self.slots[i].next = NIL;
        match self.tail {
            NIL => self.head = i,
            t => self.slots[t].next = i,
        }
        self.tail = i;
    }

    /// Drop the oldest window.
    fn pop_front(&mut self) {
        let i = self.head;
        if i == NIL {
            return;
        }
        self.unlink(i);
        let key = std::mem::take(&mut self.slots[i].key);
        self.index.remove(&key);
        self.free.push(i);
    }

    pub(crate) fn check(&mut self, key: &str, now: Instant, limit: u32) -> RateDecision {
        let i = match self.index.get(key) {
            Some(&i) => {
                if now.duration_since(self.slots[i].start) >= WINDOW {
                    self.slots[i].start = now;
                    self.slots[i].count = 0;
                    self.unlink(i);
                    self.push_back(i);
                }
                i
            }
            None => {
                if self.index.len() >= self.max_keys {
                    let oldest_expired = self.head != NIL
                        && now.duration_since(self.slots[self.head].start) >= WINDOW;
                    if !oldest_expired && !self.evict_when_full {
                        return RateDecision::Limited { first: false };
                    }
                    self.pop_front();
                }
                let slot = WindowSlot { key: key.to_string(), start: now, count: 0, prev: NIL, next: NIL };
                let i = match self.free.pop() {
                    Some(i) => {
                        self.slots[i] = slot;
                        i
                    }
                    None => {
                        self.slots.push(slot);
                        self.slots.len() - 1
                    }
                };
                self.index.insert(key.to_string(), i);
                self.push_back(i);
                i
            }
        };
        let slot = &mut self.slots[i];
        slot.count = slot.count.saturating_add(1);
        if slot.count <= limit {
            RateDecision::Allowed
        } else {
            RateDecision::Limited { first: slot.count == limit + 1 }
        }
    }
}

// ── Gate: admission, concurrency, cached switch ─────────────────

/// Knobs of one [`Gate`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub global_per_minute: u32,
    pub source_per_minute: u32,
    pub token_per_minute: u32,
    pub concurrency: usize,
    pub work_wait: Duration,
    pub enabled_ttl: Duration,
}

impl Limits {
    pub(crate) const PRODUCTION: Limits = Limits {
        global_per_minute: GLOBAL_LIMIT_PER_MINUTE,
        source_per_minute: SOURCE_LIMIT_PER_MINUTE,
        token_per_minute: RATE_LIMIT_PER_MINUTE,
        concurrency: MAX_CONCURRENT_WORK,
        work_wait: WORK_WAIT,
        enabled_ttl: ENABLED_TTL,
    };
}

struct EnabledEntry {
    home: PathBuf,
    value: bool,
    read_at: Instant,
    generation: u64,
}

/// Everything the endpoint decides before (and around) credential work.
/// One process-wide instance serves the route; tests build their own.
pub(crate) struct Gate {
    limits: Limits,
    global: Mutex<(Instant, u32)>,
    sources: Mutex<Limiter>,
    tokens: Mutex<Limiter>,
    work: tokio::sync::Semaphore,
    generation: AtomicU64,
    enabled: Mutex<Option<EnabledEntry>>,
    config_reads: AtomicU64,
    lookups: AtomicU64,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
}

/// A held work slot; keeps the in-flight gauge honest on every exit path.
pub(crate) struct WorkSlot<'a> {
    gate: &'a Gate,
    _permit: tokio::sync::SemaphorePermit<'a>,
}

impl Drop for WorkSlot<'_> {
    fn drop(&mut self) {
        self.gate.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Gate {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            global: Mutex::new((Instant::now(), 0)),
            sources: Mutex::new(Limiter::evicting(LIMITER_MAX_KEYS)),
            tokens: Mutex::new(Limiter::new()),
            work: tokio::sync::Semaphore::new(limits.concurrency.max(1)),
            generation: AtomicU64::new(0),
            enabled: Mutex::new(None),
            config_reads: AtomicU64::new(0),
            lookups: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            peak_in_flight: AtomicUsize::new(0),
        }
    }

    /// Forget the cached switch; the next request re-reads `config.toml`.
    pub(crate) fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// `[external_status] enabled`, re-read at most every `enabled_ttl`, on
    /// a change of home, or after [`Self::invalidate`]. A poisoned cache
    /// reads as off.
    pub(crate) fn enabled(&self, home: &Path) -> bool {
        let now = Instant::now();
        let generation = self.generation.load(Ordering::SeqCst);
        let Ok(mut slot) = self.enabled.lock() else {
            return false;
        };
        if let Some(entry) = slot.as_ref() {
            if entry.home == home
                && entry.generation == generation
                && now.duration_since(entry.read_at) < self.limits.enabled_ttl
            {
                return entry.value;
            }
        }
        self.config_reads.fetch_add(1, Ordering::SeqCst);
        let value = enabled(home);
        *slot = Some(EnabledEntry { home: home.to_path_buf(), value, read_at: now, generation });
        value
    }

    /// Global, then per-source admission. Nothing has been read yet.
    ///
    /// The global budget is checked first, so a request it refuses never
    /// touches (or churns) the source map; it is only spent once the source
    /// admits, so a source-refused request costs nobody else. The global
    /// lock is held across both (lock order: global, then sources).
    pub(crate) fn admit(&self, peer: Option<IpAddr>, now: Instant) -> bool {
        let Ok(mut global) = self.global.lock() else {
            return false;
        };
        if now.duration_since(global.0) >= WINDOW {
            *global = (now, 0);
        }
        if global.1 >= self.limits.global_per_minute {
            return false;
        }
        let source_ok = match self.sources.lock() {
            Ok(mut sources) => {
                sources.check(&source_key(peer), now, self.limits.source_per_minute)
                    == RateDecision::Allowed
            }
            Err(_) => false,
        };
        if !source_ok {
            return false;
        }
        global.1 += 1;
        true
    }

    fn rate_check_token(&self, token_id: &str) -> RateDecision {
        match self.tokens.lock() {
            Ok(mut limiter) => limiter.check(token_id, Instant::now(), self.limits.token_per_minute),
            // A poisoned limiter refuses rather than letting everything through.
            Err(_) => RateDecision::Limited { first: false },
        }
    }

    /// A work slot, waiting at most `work_wait`.
    pub(crate) async fn acquire_work(&self) -> Option<WorkSlot<'_>> {
        let permit = tokio::time::timeout(self.limits.work_wait, self.work.acquire())
            .await
            .ok()?
            .ok()?;
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
        Some(WorkSlot { gate: self, _permit: permit })
    }

    /// Credential lookups started (observability for tests).
    #[cfg(test)]
    pub(crate) fn lookups(&self) -> u64 {
        self.lookups.load(Ordering::SeqCst)
    }

    /// `config.toml` reads made by [`Self::enabled`].
    #[cfg(test)]
    pub(crate) fn config_reads(&self) -> u64 {
        self.config_reads.load(Ordering::SeqCst)
    }

    /// Most work slots ever held at once.
    #[cfg(test)]
    pub(crate) fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    /// Keys currently held by the source limiter.
    #[cfg(test)]
    pub(crate) fn tracked_sources(&self) -> usize {
        self.sources.lock().map(|s| s.len()).unwrap_or(0)
    }
}

static GATE: LazyLock<Gate> = LazyLock::new(|| Gate::new(Limits::PRODUCTION));

// ── Response shape ──────────────────────────────────────────────

/// The whole response body. Field set is the contract the external card and
/// the dashboard design rely on — add nothing here that is not a count.
#[derive(Debug, Serialize)]
pub(crate) struct StatusResponse {
    pub generated_at: String,
    pub employees: Vec<EmployeeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EmployeeStatus {
    pub agent_id: String,
    pub display_name: String,
    pub in_progress: i64,
    pub awaiting_confirmation: i64,
    pub done_today: i64,
    pub done_7d: i64,
    pub latest_done: Option<LatestDone>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LatestDone {
    /// `null` when the on-device PII model is unavailable or the title could
    /// not be masked safely.
    pub title: Option<String>,
    pub completed_at: String,
}

/// A task title fit to leave the building: Unicode-normalised, credentials
/// and personal data masked (the on-device PII model included), control
/// characters flattened, at most [`TITLE_MAX_CHARS`] characters (CJK-safe).
/// `None` ⇒ omit it — always, while the model is not installed or fails.
/// Blocking (file stat, regex, model inference).
pub(crate) fn safe_title(home: &Path, raw: &str) -> Option<String> {
    title_mask::external_title(home, raw, TITLE_MAX_CHARS)
}

/// `(start of today in the gateway's local time zone, now − 7 days)`, in UTC.
pub(crate) fn day_bounds(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let local = now.with_timezone(&Local);
    let today = local
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| now - chrono::Duration::hours(24));
    (today, now - chrono::Duration::days(7))
}

// ── HTTP ────────────────────────────────────────────────────────

fn generic(status: StatusCode, error: &'static str) -> Response {
    let mut response = (status, axum::Json(json!({ "error": error }))).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn not_found() -> Response {
    generic(StatusCode::NOT_FOUND, "not_found")
}

fn unauthorized() -> Response {
    let mut response = generic(StatusCode::UNAUTHORIZED, "unauthorized");
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn unavailable() -> Response {
    generic(StatusCode::SERVICE_UNAVAILABLE, "unavailable")
}

fn busy() -> Response {
    let mut response = unavailable();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

fn rate_limited() -> Response {
    let mut response = generic(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    response
}

/// Audit one attributable call: credential id, account id, result, employee
/// count — never the response body. Aggregated per (credential, result,
/// 10-minute window): the first call writes the audit-log row, later ones
/// bump its counter; an `ok` window also stamps `last_used_at` (so at most
/// once per window). Best effort — a failed write only logs.
async fn audit(
    users: &Arc<UserDb>,
    user_id: &str,
    token_id: &str,
    result: &'static str,
    employees: usize,
    ip: Option<String>,
) {
    let users = users.clone();
    let user_id = user_id.to_string();
    let token_id = token_id.to_string();
    let written = tokio::task::spawn_blocking(move || {
        users.status_token_record_call(
            &token_id,
            &user_id,
            result,
            employees,
            ip.as_deref(),
            Utc::now(),
        )
    })
    .await;
    if !matches!(written, Ok(Ok(_))) {
        tracing::warn!(result, "external status: audit write failed");
    }
}

/// Serve one request through the process-wide gate. `home` is the gateway
/// home (config + agents).
pub(crate) async fn serve(
    handler: &MethodHandler,
    users: &Arc<UserDb>,
    home: &Path,
    request: axum::extract::Request,
) -> Response {
    serve_with(&GATE, handler, users, home, request).await
}

/// [`serve`] through an explicit gate.
pub(crate) async fn serve_with(
    gate: &Gate,
    handler: &MethodHandler,
    users: &Arc<UserDb>,
    home: &Path,
    request: axum::extract::Request,
) -> Response {
    // Off ⇒ indistinguishable from a route that does not exist, for every
    // method and credential, without touching the credential store.
    if !gate.enabled(home) {
        return not_found();
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|peer| peer.0.ip());
    // Before any credential, config or database work: a flood of random
    // well-formed credentials stops here.
    if !gate.admit(peer, Instant::now()) {
        return rate_limited();
    }
    if *request.method() != Method::GET {
        let mut response = generic(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
        response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET"));
        return response;
    }
    let Ok(filter) = parse_query(request.uri().query()) else {
        return generic(StatusCode::BAD_REQUEST, "bad_request");
    };
    let Some(raw) = bearer(request.headers()).map(str::to_owned) else {
        return unauthorized();
    };
    let ip = peer.map(|ip| ip.to_string());

    // Every blocking step below (lookup, identity, counts, audit) runs
    // inside one bounded work slot.
    let Some(_slot) = gate.acquire_work().await else {
        return busy();
    };

    gate.lookups.fetch_add(1, Ordering::SeqCst);
    let lookup = {
        let users = users.clone();
        tokio::task::spawn_blocking(move || users.status_token_lookup(&raw)).await
    };
    let (token_id, user_id, refused) = match lookup {
        Ok(Ok(StatusTokenLookup::Valid { token_id, user_id })) => (token_id, user_id, None),
        Ok(Ok(StatusTokenLookup::Refused { token_id, user_id, reason })) => {
            (token_id, user_id, Some(reason))
        }
        Ok(Ok(StatusTokenLookup::Unknown)) => return unauthorized(),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "external status: credential lookup failed");
            return unavailable();
        }
        Err(_) => return unavailable(),
    };

    // Rate-limit every attributable credential — refused ones too — so a
    // revoked credential still polled by a forgotten integration costs at
    // most its quota in audit writes (each aggregated into one row per
    // 10-minute window), and the refusals past it one write per minute.
    match gate.rate_check_token(&token_id) {
        RateDecision::Allowed => {}
        RateDecision::Limited { first } => {
            if first {
                audit(users, &user_id, &token_id, "rate_limited", 0, ip).await;
            }
            return rate_limited();
        }
    }
    if let Some(reason) = refused {
        audit(users, &user_id, &token_id, reason, 0, ip).await;
        return unauthorized();
    }

    // The owner's live direct bindings decide what is visible: an unbinding,
    // suspension or deletion applies to this very request.
    let live = {
        let users = users.clone();
        let home = home.to_path_buf();
        let id = user_id.clone();
        tokio::task::spawn_blocking(move || {
            crate::machine_employee::live_user_context(&home, &users, &id)
        })
        .await
    };
    let ctx = match live {
        Ok(Ok(Some(ctx))) if !ctx.must_change_password => ctx,
        Ok(Ok(_)) => {
            audit(users, &user_id, &token_id, "account_inactive", 0, ip).await;
            return unauthorized();
        }
        _ => return unavailable(),
    };
    let mut agents: Vec<String> = ctx.agent_access.keys().cloned().collect();
    if let Some(wanted) = filter {
        if !agents.contains(&wanted) {
            audit(users, &user_id, &token_id, "agent_not_visible", 0, ip).await;
            return not_found();
        }
        agents = vec![wanted];
    }

    // Counted for this owner only (A2A owner, packet audience, approval
    // visibility), re-checked against a fresh identity inside.
    let employees = match handler.external_status_snapshot(&ctx, &agents).await {
        Ok(employees) => employees,
        Err(e) => {
            tracing::warn!(error = %e, "external status: snapshot unavailable");
            audit(users, &user_id, &token_id, "unavailable", 0, ip).await;
            return unavailable();
        }
    };
    // Also stamps `last_used_at`, at most once per audit window.
    audit(users, &user_id, &token_id, "ok", employees.len(), ip).await;

    let body = StatusResponse {
        generated_at: Utc::now().to_rfc3339(),
        employees,
    };
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet() -> Limits {
        Limits {
            global_per_minute: 1_000,
            source_per_minute: 1_000,
            token_per_minute: 1_000,
            concurrency: 2,
            work_wait: Duration::from_millis(50),
            enabled_ttl: Duration::from_secs(3600),
        }
    }

    #[test]
    fn config_defaults_off_and_only_a_real_true_enables() {
        let table = |s: &str| s.parse::<toml::Table>().unwrap();
        assert!(!ExternalStatusConfig::from_table(&table("")).enabled);
        assert!(!ExternalStatusConfig::from_table(&table("[external_status]\n")).enabled);
        assert!(!ExternalStatusConfig::from_table(&table("[external_status]\nenabled = \"true\"\n")).enabled);
        assert!(!ExternalStatusConfig::from_table(&table("external_status = true\n")).enabled);
        assert!(ExternalStatusConfig::from_table(&table("[external_status]\nenabled = true\n")).enabled);

        let home = tempfile::tempdir().unwrap();
        assert!(!enabled(home.path()), "missing config.toml ⇒ off");
        std::fs::write(home.path().join("config.toml"), "[external_status\nbroken").unwrap();
        assert!(!enabled(home.path()), "malformed config.toml ⇒ off");
    }

    #[test]
    fn enabled_flag_is_cached_and_reloads_on_invalidate_ttl_or_home() {
        let home = tempfile::tempdir().unwrap();
        let write = |on: bool| {
            std::fs::write(home.path().join("config.toml"), format!("[external_status]\nenabled = {on}\n")).unwrap();
        };
        write(true);
        let gate = Gate::new(quiet());
        assert!(gate.enabled(home.path()));
        for _ in 0..50 {
            assert!(gate.enabled(home.path()));
        }
        assert_eq!(gate.config_reads(), 1, "one read serves the TTL");

        write(false);
        assert!(gate.enabled(home.path()), "cached until invalidated or expired");
        gate.invalidate();
        assert!(!gate.enabled(home.path()), "a config write takes effect at once");
        assert_eq!(gate.config_reads(), 2);

        let other = tempfile::tempdir().unwrap();
        assert!(!gate.enabled(other.path()), "another home is read, not borrowed");
        assert_eq!(gate.config_reads(), 3);

        let short = Gate::new(Limits { enabled_ttl: Duration::from_millis(20), ..quiet() });
        write(true);
        assert!(short.enabled(home.path()));
        write(false);
        std::thread::sleep(Duration::from_millis(30));
        assert!(!short.enabled(home.path()), "disabling applies within the TTL");
        assert!(ENABLED_TTL <= Duration::from_secs(5));
    }

    #[test]
    fn admission_limits_each_source_then_everyone() {
        let t0 = Instant::now();
        let gate = Gate::new(Limits { source_per_minute: 3, global_per_minute: 5, ..quiet() });
        let a: IpAddr = "203.0.113.7".parse().unwrap();
        let b: IpAddr = "198.51.100.2".parse().unwrap();
        let c: IpAddr = "192.0.2.9".parse().unwrap();
        for _ in 0..3 {
            assert!(gate.admit(Some(a), t0));
        }
        assert!(!gate.admit(Some(a), t0), "per source");
        assert!(gate.admit(Some(b), t0));
        assert!(gate.admit(Some(b), t0));
        assert!(!gate.admit(Some(c), t0), "global budget spent");
        assert!(gate.admit(Some(c), t0 + Duration::from_secs(61)), "window rolls");
    }

    #[test]
    fn ipv6_sources_share_their_64_and_the_map_stays_bounded() {
        let x: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let y: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        assert_eq!(source_key(Some(x)), source_key(Some(y)));
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        assert_eq!(source_key(Some(mapped)), "203.0.113.7");
        assert_eq!(source_key(None), "unknown");

        let gate = Gate::new(Limits { source_per_minute: 1, global_per_minute: u32::MAX, ..quiet() });
        let t0 = Instant::now();
        for i in 0..(LIMITER_MAX_KEYS as u32 + 500) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(0x0A00_0000 + i));
            assert!(gate.admit(Some(ip), t0), "new sources are admitted by eviction");
        }
        assert!(gate.tracked_sources() <= LIMITER_MAX_KEYS);
    }

    #[test]
    fn only_a_single_valid_agent_id_filter_is_accepted() {
        assert_eq!(parse_query(None), Ok(None));
        assert_eq!(parse_query(Some("")), Ok(None));
        assert_eq!(parse_query(Some("agent_id=sales-bot_1")), Ok(Some("sales-bot_1".into())));
        let too_long = format!("agent_id={}", "a".repeat(AGENT_ID_MAX_CHARS + 1));
        for bad in [
            "agent_id=a&agent_id=b",
            "agent=sales",
            "agent_id=sales&limit=5",
            "agent_id=",
            "agent_id",
            "agent_id=..%2Fetc",
            "agent_id=%E5%AE%A2%E6%9C%8D",
            too_long.as_str(),
        ] {
            assert_eq!(parse_query(Some(bad)), Err(()), "{bad}");
        }
    }

    #[test]
    fn bearer_must_be_a_status_credential() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer(&headers), None);
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer eyJhbGciOi.jwt.x"));
        assert_eq!(bearer(&headers), None, "a dashboard JWT is not this credential");
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer ddc_prod_0123456789abcdef0123456789abcdef"));
        assert_eq!(bearer(&headers), None, "an MCP key is not this credential");
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer ddc_status_abc"));
        assert_eq!(bearer(&headers), Some("ddc_status_abc"));
    }

    #[test]
    fn limiter_allows_the_quota_then_refuses_until_the_window_rolls() {
        let mut limiter = Limiter::new();
        let t0 = Instant::now();
        for _ in 0..RATE_LIMIT_PER_MINUTE {
            assert_eq!(limiter.check("tok", t0, RATE_LIMIT_PER_MINUTE), RateDecision::Allowed);
        }
        assert_eq!(limiter.check("tok", t0, RATE_LIMIT_PER_MINUTE), RateDecision::Limited { first: true });
        assert_eq!(limiter.check("tok", t0, RATE_LIMIT_PER_MINUTE), RateDecision::Limited { first: false });
        assert_eq!(limiter.check("other", t0, RATE_LIMIT_PER_MINUTE), RateDecision::Allowed, "per credential");
        let later = t0 + Duration::from_secs(61);
        assert_eq!(limiter.check("tok", later, RATE_LIMIT_PER_MINUTE), RateDecision::Allowed);
    }

    #[test]
    fn limiter_refuses_new_keys_when_full_of_live_windows() {
        let mut limiter = Limiter::new();
        let t0 = Instant::now();
        for i in 0..LIMITER_MAX_KEYS {
            limiter.check(&format!("k{i}"), t0, 1);
        }
        assert_eq!(limiter.check("new", t0, 1), RateDecision::Limited { first: false });
        // Expired windows are pruned to make room.
        assert_eq!(limiter.check("new", t0 + Duration::from_secs(61), 1), RateDecision::Allowed);
    }

    #[test]
    fn evicting_limiter_drops_the_oldest_window() {
        let mut limiter = Limiter::evicting(2);
        let t0 = Instant::now();
        limiter.check("a", t0, 1);
        limiter.check("b", t0 + Duration::from_secs(1), 1);
        assert_eq!(limiter.check("c", t0 + Duration::from_secs(2), 1), RateDecision::Allowed);
        assert_eq!(limiter.len(), 2);
        assert_eq!(limiter.check("b", t0 + Duration::from_secs(3), 1), RateDecision::Limited { first: true }, "b kept");
    }

    #[test]
    fn a_restarted_window_becomes_the_newest() {
        let mut limiter = Limiter::evicting(2);
        let t0 = Instant::now();
        limiter.check("a", t0, 1);
        limiter.check("b", t0 + Duration::from_secs(1), 1);
        // a's window rolls over: it is now newer than b.
        assert_eq!(limiter.check("a", t0 + Duration::from_secs(61), 1), RateDecision::Allowed);
        assert_eq!(limiter.check("c", t0 + Duration::from_secs(62), 1), RateDecision::Allowed);
        assert_eq!(limiter.len(), 2);
        assert_eq!(limiter.check("a", t0 + Duration::from_secs(63), 1), RateDecision::Limited { first: true }, "a kept");
        assert_eq!(limiter.check("b", t0 + Duration::from_secs(63), 1), RateDecision::Allowed, "b was evicted, starts over");
    }

    #[test]
    fn limiter_slots_are_reused_and_never_exceed_the_cap() {
        let mut limiter = Limiter::evicting(3);
        let t0 = Instant::now();
        for i in 0..1_000u64 {
            limiter.check(&format!("k{i}"), t0 + Duration::from_millis(i), 5);
        }
        assert_eq!(limiter.len(), 3);
        assert!(limiter.slots.len() <= 3, "arena bounded: {}", limiter.slots.len());
        // The three newest are the ones kept.
        for i in 997..1_000u64 {
            assert!(limiter.index.contains_key(&format!("k{i}")));
        }
    }

    #[test]
    fn a_spent_global_budget_leaves_the_source_map_alone() {
        let t0 = Instant::now();
        let gate = Gate::new(Limits { source_per_minute: 100, global_per_minute: 3, ..quiet() });
        for i in 0..3u8 {
            assert!(gate.admit(Some(IpAddr::from([198, 51, 100, i])), t0));
        }
        assert_eq!(gate.tracked_sources(), 3);
        for i in 0..200u8 {
            assert!(!gate.admit(Some(IpAddr::from([203, 0, 113, i])), t0), "global budget spent");
        }
        assert_eq!(gate.tracked_sources(), 3, "global refusals never reach the source map");
        // A source refusal does not spend the global budget.
        let gate = Gate::new(Limits { source_per_minute: 1, global_per_minute: 2, ..quiet() });
        let a = IpAddr::from([192, 0, 2, 1]);
        assert!(gate.admit(Some(a), t0));
        for _ in 0..10 {
            assert!(!gate.admit(Some(a), t0));
        }
        assert!(gate.admit(Some(IpAddr::from([192, 0, 2, 2])), t0), "one global slot left");
    }

    #[tokio::test]
    async fn work_slots_are_capped() {
        let gate = Gate::new(Limits { concurrency: 2, work_wait: Duration::from_millis(20), ..quiet() });
        let one = gate.acquire_work().await.unwrap();
        let two = gate.acquire_work().await.unwrap();
        assert!(gate.acquire_work().await.is_none(), "third waits, then gives up");
        drop(one);
        let three = gate.acquire_work().await;
        assert!(three.is_some());
        assert_eq!(gate.peak_in_flight(), 2);
        drop((two, three));
        assert_eq!(gate.in_flight.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn titles_are_omitted_without_the_on_device_model() {
        // Nothing installed in a temp home, no test double registered.
        let home = tempfile::tempdir().unwrap();
        assert_eq!(safe_title(home.path(), "拜訪王小明確認續約"), None);
        assert_eq!(safe_title(home.path(), "寄出報價單"), None);
        title_mask::test_ner::register(home.path(), Arc::new(title_mask::test_ner::People::of(&["王小明"])));
        assert_eq!(safe_title(home.path(), "拜訪王小明確認續約").as_deref(), Some("拜訪＊＊＊確認續約"));
        title_mask::test_ner::register(home.path(), Arc::new(title_mask::test_ner::Failing));
        assert_eq!(safe_title(home.path(), "寄出報價單"), None, "a failing model omits");
        title_mask::test_ner::unregister(home.path());
        assert_eq!(safe_title(home.path(), "寄出報價單"), None);
    }

    #[test]
    fn titles_are_truncated_by_character_and_masked() {
        let home = tempfile::tempdir().unwrap();
        title_mask::test_ner::register(home.path(), Arc::new(title_mask::test_ner::People::none()));
        let title = |raw: &str| safe_title(home.path(), raw).unwrap();
        let cjk = "請".repeat(70);
        let t = title(&cjk);
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        assert!(t.chars().all(|c| c == '請'));
        let mixed = format!("整理{}報表\n第二行", "季".repeat(80));
        let t = title(&mixed);
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        assert!(!t.contains('\n'));
        assert_eq!(title("  寄出報價單  "), "寄出報價單");
        let leaked = title("設定主機，密碼是測試值");
        assert!(!leaked.contains("測試值"), "{leaked}");
        let pii = title("通知王小明 ming@example.com、0912-345-678，身分證Ａ１２３４５６７８９");
        for secret in ["ming@", "345-678", "１２３４５６７８９", "123456789"] {
            assert!(!pii.contains(secret), "{pii}");
        }
        assert_eq!(safe_title(home.path(), &"x".repeat(5000)), None, "unmaskable ⇒ omitted");
    }

    #[test]
    fn day_bounds_are_ordered() {
        let now = Utc::now();
        let (today, week) = day_bounds(now);
        assert!(today <= now && now - today <= chrono::Duration::hours(25));
        assert_eq!(now - week, chrono::Duration::days(7));
    }
}
