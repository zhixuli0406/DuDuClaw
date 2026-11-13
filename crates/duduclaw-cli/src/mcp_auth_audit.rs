//! mcp_auth_audit — a bounded audit trail of MCP authentication failures
//! (v1.82).
//!
//! Before this module every refused MCP credential (missing header, wrong
//! scheme, malformed / unknown / disabled / expired / deleted key, an
//! unreadable key store, a key whose employee is gone) returned its 401 and
//! left no trace anywhere. Owners could not see a key being probed, or a
//! revoked key still being retried by an integration.
//!
//! ## Two sinks
//!
//! Anyone who can reach the HTTP server can produce failures, so nothing an
//! unauthenticated client controls may decide how often the hash-chained
//! `security_audit.jsonl` is written: that writer takes the shared
//! security-audit lock, scans the never-rotated file for its last hash and
//! fsyncs on every record, and every other security-event writer waits
//! behind it. Failures therefore go to two sinks:
//!
//! * **Detail sink** — `<home>/`[`DETAIL_FILE`]: plain JSON lines (no hash
//!   chain, no per-line fsync), appended under its own
//!   `duduclaw_core::with_file_lock` (never the security-audit lock). On
//!   Unix the file is created owner-only (`0600`), never written through a
//!   symlink, a hard link or a non-regular file; on Windows a reparse point
//!   or non-regular file is refused on the opened handle, but the link count
//!   and ACL are not checked (access relies on the home directory's ACL);
//!   elsewhere it is not written at all. When an append would take it past
//!   [`DETAIL_MAX_BYTES`] it is renamed, under the same lock, to
//!   [`DETAIL_ROTATED_FILE`] (one generation, replacing the previous one), so
//!   the two files never hold more than 2 × 4 MiB = 8 MiB, whatever a client
//!   does (a single record larger than the cap is refused; a failed append
//!   is cut back so no partial line stays). It holds the per-minute aggregation ([`Aggregator`]) with the
//!   socket peer of every failure; see "Detail bounds" below.
//! * **Chain sink** — `<home>/security_audit.jsonl`, through the existing
//!   strict writer (`duduclaw_security::audit::try_append_audit_event`), but
//!   only ([`ChainAggregator`]):
//!   1. stored-key failures (`key_disabled` / `key_expired` / `key_deleted` /
//!      `key_unbound` with the store's key id): the first failure of each
//!      (key id, reason) in an aligned UTC hour is written at once as
//!      [`EVENT_MCP_AUTH_FAILED`] (`aggregation: "first_in_hour"`, `count`
//!      1); the rest of that hour is written at the hour's end as one
//!      `aggregation: "hour_summary"` record with the count. At most
//!      [`MAX_CHAIN_STORED_KEYS_PER_HOUR`] (key id, reason) pairs per hour;
//!      failures of further pairs fold into the hour summary below
//!      (`stored_key_overflow`). These need a real stored key: a client
//!      without one cannot produce them.
//!   2. everything else: ONE [`EVENT_MCP_AUTH_FAILED_SUMMARY`] record per
//!      aligned hour, written when the hour has ended (at the HTTP server's
//!      periodic flush or the next failure), only if the hour saw a failure
//!      or a dropped record: the total, a count per reason (a closed set),
//!      the number of distinct sources (counted up to
//!      [`MAX_COUNTED_SOURCES`], then `distinct_sources_capped: true`), the
//!      records dropped since the last summary and `detail_log` naming the
//!      detail file. No source address is ever in the chain; the addresses
//!      are in the detail file.
//!
//!   Worst case for the chain, per `http-server` process (the counters live
//!   in the process: an operator running N servers on one home gets N
//!   summaries per hour): a client without a stored key causes at most 1
//!   line per hour (24 a day); with real stored keys at most
//!   [`MAX_CHAIN_RECORDS_PER_HOUR`] = 32 immediate + 32 summaries + 1 = 65
//!   lines per hour.
//!
//! ## What an entry contains — and never contains
//!
//! `reason` (a closed set, [`FailureReason`]), `transport` (`http` | `stdio`),
//! `route` (a fixed label from [`route_class`], never the raw path or
//! query), `count` (failures the entry stands for), `window_secs` and
//! `aggregation`. Detail lines add `source`: the peer IP the server socket
//! saw — never `X-Forwarded-For` or any other client-controlled header — or
//! `unknown` (`many` on an overflow line). A failure that resolved to a row
//! of the hashed key store (disabled / expired / deleted / unbound) adds that
//! row's `key_id` (`mk_…`) — read from the row the decision already loaded,
//! so no extra store read happens on the unauthenticated path. Nothing
//! derived from the presented secret is ever kept: no prefix, no length, no
//! hint, no SHA-256, no `Authorization` value, no query string, no
//! User-Agent.
//!
//! ## Detail bounds (HTTP)
//!
//! * Time is cut into fixed windows of [`WINDOW_SECS`] seconds, aligned to
//!   the aggregator's start. Failures are counted per
//!   (reason, source, key id or none) — a *bucket*.
//! * The first failure of a bucket in a window is written immediately
//!   (`aggregation: "first"`, `count: 1`). Later failures of that bucket in
//!   the same window only increment a counter, written at rollover as one
//!   `aggregation: "window_summary"` line with `count = N`. The rollover
//!   happens at the next failure after the window ended, or at the HTTP
//!   server's periodic flush (every [`FLUSH_TICK_SECS`] seconds).
//! * The bucket table holds at most [`MAX_TRACKED_KEYS`] buckets without a
//!   key id plus [`MAX_TRACKED_STORED_KEYS`] stored-key buckets, emptied at
//!   every rollover; when full, new buckets fold into the overflow counters.
//! * At most [`MAX_LINES_PER_WINDOW`] first / summary lines per window (of
//!   which [`RESERVED_STORED_KEY_LINES`] only for stored-key buckets).
//!   Failures beyond fold into one per-reason overflow counter, written at
//!   rollover as ONE `aggregation: "overflow"` line (`source` / `route`
//!   `many`, a `reasons` breakdown, and `dropped_records` when records were
//!   dropped).
//!
//! Worst case: `MAX_LINES_PER_WINDOW + 1` = 61 detail lines per minute
//! (3,660 per hour, each well under 1 KiB) per `http-server` process,
//! whatever the number of sources — and never more than 8 MiB on disk.
//!
//! ## Memory
//!
//! The detail bucket tables (≤ 1,280 small entries), the chain table
//! (≤ 32 stored-key buckets, ≤ 1,024 remembered sources, counters per
//! reason), and two outboxes: [`MAX_CHAIN_OUTBOX`] records for the chain and
//! [`MAX_DETAIL_OUTBOX`] for the detail file.
//!
//! ## Off the request path
//!
//! Records go to the two bounded outboxes, each drained by at most one
//! `spawn_blocking` writer of its own, so a writer stuck on the
//! security-audit lock never delays the detail file and vice versa. The
//! chain outbox is reserved for chain records: detail records and
//! anonymous failures can never take its room, so the first record of a
//! revoked key still in use is not lost to a flood. A record that does not
//! fit its outbox, or whose write fails, is counted in `dropped_records` of
//! the next hour summary (and of the next detail overflow line); a lost
//! summary / overflow line also gives back the drops it reported, so the
//! next one carries them. A stored
//! key's bucket is marked "first record done" only once that record was
//! enqueued; otherwise the failure stays pending and is in the hour summary.
//! One `tracing::warn!` per chain record; detail records log at debug. A
//! write problem is logged at most once per
//! [`WRITE_ERROR_WARN_INTERVAL_SECS`]. Auditing never changes an
//! authentication decision, status code or body: it runs after the decision
//! and only reads a marker the auth code attached to the response.
//!
//! Not guaranteed: counters not yet flushed when the process exits are lost
//! (no flush on shutdown — accepted; the first failure of every stored-key
//! pair was already written at once).
//!
//! ## stdio
//!
//! The stdio `mcp-server` authenticates from `DUDUCLAW_MCP_API_KEY` of its
//! own process; its failure is local. One chain record (`transport:
//! "stdio"`, `route: "stdio"`, `source: "local"`) per process, no detail
//! line. The write never blocks the async server: it runs on a blocking
//! thread; where the process is about to exit the server waits for it at
//! most [`STDIO_WRITE_TIMEOUT`], elsewhere it does not wait at all. The
//! once-per-process mark is set only after a successful write; a failed
//! write may be retried once, on a later failure ([`MAX_STDIO_ATTEMPTS`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::response::Response;
use serde_json::{Map, Value, json};

use crate::mcp_auth::{AuthError, InactiveReason, Refusal};

/// `event_type` of a single-failure record (immediate stored-key record,
/// stored-key hour summary, stdio record, every detail line).
pub(crate) const EVENT_MCP_AUTH_FAILED: &str = "mcp_auth_failed";

/// `event_type` of the one hourly chain record for every failure that has
/// no stored key of its own.
pub(crate) const EVENT_MCP_AUTH_FAILED_SUMMARY: &str = "mcp_auth_failed_summary";

// ── Detail sink ───────────────────────────────────────────────────────────────

/// The detail file in the home directory.
pub(crate) const DETAIL_FILE: &str = "mcp_auth_failures.jsonl";

/// The one rotated generation of [`DETAIL_FILE`].
pub(crate) const DETAIL_ROTATED_FILE: &str = "mcp_auth_failures.jsonl.1";

/// Largest size of [`DETAIL_FILE`] (and so of [`DETAIL_ROTATED_FILE`]):
/// 4 MiB, so both together never exceed 8 MiB.
pub(crate) const DETAIL_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Length of one detail aggregation window, in seconds.
pub(crate) const WINDOW_SECS: u64 = 60;

/// How often the HTTP server rolls the windows (detail minute, chain hour)
/// over without waiting for the next failure.
pub(crate) const FLUSH_TICK_SECS: u64 = 10;

/// Most detail buckets without a key id (unknown / malformed / missing …)
/// tracked in one window.
pub(crate) const MAX_TRACKED_KEYS: usize = 1024;

/// Most detail stored-key buckets tracked in one window, separately from
/// [`MAX_TRACKED_KEYS`].
pub(crate) const MAX_TRACKED_STORED_KEYS: usize = 256;

/// Most detail first / summary lines per window, across all sources. The
/// overflow line (one per window at most) comes on top: ≤ 61 lines a minute.
pub(crate) const MAX_LINES_PER_WINDOW: u32 = 60;

/// Part of [`MAX_LINES_PER_WINDOW`] only stored-key buckets may use.
pub(crate) const RESERVED_STORED_KEY_LINES: u32 = 10;

// ── Chain sink ────────────────────────────────────────────────────────────────

/// Length of one chain window: an aligned UTC hour.
pub(crate) const CHAIN_WINDOW_SECS: u64 = 3600;

/// Most (key id, reason) pairs of stored keys given their own chain records
/// in one hour; failures of further pairs fold into the hour summary.
pub(crate) const MAX_CHAIN_STORED_KEYS_PER_HOUR: usize = 32;

/// Most distinct sources remembered per hour for the summary's
/// `distinct_sources` (beyond: `distinct_sources_capped: true`).
pub(crate) const MAX_COUNTED_SOURCES: usize = 1024;

/// Most chain records one HTTP server writes per hour: an immediate record
/// and a summary per stored-key pair, plus the one hour summary (65).
/// Without a stored key a client causes at most the 1 summary.
pub(crate) const MAX_CHAIN_RECORDS_PER_HOUR: usize = 2 * MAX_CHAIN_STORED_KEYS_PER_HOUR + 1;

// ── Outboxes ──────────────────────────────────────────────────────────────────

/// Chain records waiting for the chain writer: more than one hour's worst
/// case ([`MAX_CHAIN_RECORDS_PER_HOUR`]). Only chain records use it.
pub(crate) const MAX_CHAIN_OUTBOX: usize = 80;
const _: () = assert!(MAX_CHAIN_OUTBOX >= MAX_CHAIN_RECORDS_PER_HOUR);

/// Detail lines waiting for the detail writer: two minutes' worth.
pub(crate) const MAX_DETAIL_OUTBOX: usize = 128;

/// At most one "record not written" warning per this many seconds.
pub(crate) const WRITE_ERROR_WARN_INTERVAL_SECS: u64 = 60;

// ── stdio ─────────────────────────────────────────────────────────────────────

/// How long a stdio server about to exit waits for its audit record.
pub(crate) const STDIO_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Most stdio write attempts per process (the first one and one retry).
pub(crate) const MAX_STDIO_ATTEMPTS: u8 = 2;

/// `agent_id` of the audit record: no employee is involved.
const AUDIT_AGENT_ID: &str = "system";

pub(crate) const TRANSPORT_HTTP: &str = "http";
pub(crate) const TRANSPORT_STDIO: &str = "stdio";
pub(crate) const ROUTE_STDIO: &str = "stdio";
/// A bucket that saw failures on more than one route in its window.
pub(crate) const ROUTE_MIXED: &str = "mixed";
/// Route / source of the overflow line.
pub(crate) const MANY: &str = "many";
pub(crate) const SOURCE_UNKNOWN: &str = "unknown";
pub(crate) const SOURCE_LOCAL: &str = "local";
/// `reason` of an overflow line that folds more than one reason (or none).
pub(crate) const REASON_MIXED: &str = "mixed";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

// ── Reasons ───────────────────────────────────────────────────────────────────

/// Why a credential was refused (closed set, written as snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum FailureReason {
    /// No `Authorization` header (HTTP, no `?api_key=` on SSE either), or
    /// `DUDUCLAW_MCP_API_KEY` not set (stdio).
    MissingHeader,
    /// An `Authorization` header that is not `Bearer <key>` (or not valid
    /// header text).
    BadScheme,
    /// `Bearer ` with nothing after it, or a key that is not in the key
    /// format.
    MalformedKey,
    /// A credential of another kind: a `ddc_refresh_` token (stdio only) or
    /// a `ddc_status_` token presented to the MCP HTTP server.
    WrongCredentialType,
    /// A well-formed key (or OAuth access token) that was never issued, or
    /// that the authority no longer accepts with no stored row to name.
    UnknownKey,
    KeyDisabled,
    /// A stored key past its expiry, or a legacy / gateway-internal key past
    /// the 30-day rule.
    KeyExpired,
    /// A stored key that was deleted (tombstoned).
    KeyDeleted,
    /// An authenticated non-external key the HTTP server refuses every tool
    /// because it is not bound to an existing employee (no binding, or its
    /// employee was removed / re-created).
    KeyUnbound,
    /// The key store or the key registry could not be read (fail-closed).
    StoreUnavailable,
}

impl FailureReason {
    pub(crate) const ALL: [Self; 10] = [
        Self::MissingHeader,
        Self::BadScheme,
        Self::MalformedKey,
        Self::WrongCredentialType,
        Self::UnknownKey,
        Self::KeyDisabled,
        Self::KeyExpired,
        Self::KeyDeleted,
        Self::KeyUnbound,
        Self::StoreUnavailable,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MissingHeader => "missing_header",
            Self::BadScheme => "bad_scheme",
            Self::MalformedKey => "malformed_key",
            Self::WrongCredentialType => "wrong_credential_type",
            Self::UnknownKey => "unknown_key",
            Self::KeyDisabled => "key_disabled",
            Self::KeyExpired => "key_expired",
            Self::KeyDeleted => "key_deleted",
            Self::KeyUnbound => "key_unbound",
            Self::StoreUnavailable => "store_unavailable",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// The reason for an authentication error. `stored_row` = the presented
    /// key resolved to a row of the hashed store: an `UnknownKey` decided
    /// from a row is a deleted (tombstoned) key.
    pub(crate) fn from_auth_error(error: &AuthError, stored_row: bool) -> Self {
        match error {
            AuthError::MissingKey => Self::MissingHeader,
            AuthError::InvalidFormat => Self::MalformedKey,
            AuthError::UnknownKey if stored_row => Self::KeyDeleted,
            // `InvalidScope` comes only from an OAuth token whose stored
            // scope no longer parses: an unusable token.
            AuthError::UnknownKey | AuthError::InvalidScope(_) => Self::UnknownKey,
            AuthError::KeyExpired { .. } | AuthError::KeyInactive(InactiveReason::Expired) => {
                Self::KeyExpired
            }
            AuthError::KeyInactive(InactiveReason::Disabled) => Self::KeyDisabled,
            AuthError::StoreUnavailable | AuthError::ReloadFailed => Self::StoreUnavailable,
        }
    }
}

/// Per-reason counts as a JSON object (non-zero reasons only).
fn reasons_object(counts: &[u64; FailureReason::ALL.len()]) -> Value {
    let reasons: Map<String, Value> = FailureReason::ALL
        .iter()
        .zip(counts)
        .filter(|(_, count)| **count > 0)
        .map(|(reason, count)| (reason.as_str().to_string(), json!(count)))
        .collect();
    Value::Object(reasons)
}

// ── Route / source ────────────────────────────────────────────────────────────

/// The fixed label of an HTTP route. Exact path matches only; anything else
/// is `other`. Never returns request text.
pub(crate) fn route_class(path: &str) -> &'static str {
    match path {
        "/mcp" => "mcp",
        "/mcp/v1/call" => "mcp_v1_call",
        "/mcp/v1/stream" => "mcp_v1_stream",
        "/mcp/v1/stream/call" => "mcp_v1_stream_call",
        "/ingest/transcript" => "ingest_transcript",
        "/oauth/decision" => "oauth_decision",
        _ => "other",
    }
}

/// The peer of a failed request, as the server socket saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    Peer(IpAddr),
    Unknown,
}

impl Source {
    pub(crate) fn from_peer(peer: Option<IpAddr>) -> Self {
        peer.map_or(Self::Unknown, Self::Peer)
    }

    fn label(self) -> String {
        match self {
            Self::Peer(ip) => ip.to_string(),
            Self::Unknown => SOURCE_UNKNOWN.to_string(),
        }
    }
}

// ── Response marker ───────────────────────────────────────────────────────────

/// Attached by the HTTP auth code to a refusal response (a response
/// extension: never sent to the client) and taken off again by the
/// admission middleware, which knows the peer and the route.
#[derive(Debug, Clone)]
pub(crate) struct AuthFailureTag {
    pub(crate) reason: FailureReason,
    pub(crate) key_id: Option<String>,
}

/// `response` with the refusal marker attached. Status, headers and body are
/// untouched.
pub(crate) fn tag(mut response: Response, reason: FailureReason, key_id: Option<String>) -> Response {
    response.extensions_mut().insert(AuthFailureTag { reason, key_id });
    response
}

/// [`tag`] for a refusal decided by `mcp_auth`.
pub(crate) fn tag_refusal(response: Response, refusal: &Refusal) -> Response {
    tag(response, refusal.audit_reason(), refusal.stored_key_id.clone())
}

// ── Entries ───────────────────────────────────────────────────────────────────

/// How an entry aggregates failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aggregation {
    /// Detail: the first failure of a bucket in a window (`count` = 1).
    First,
    /// Detail: the rest of a bucket's failures in a window that ended.
    WindowSummary,
    /// Detail: failures that did not fit the tables or the line budget.
    Overflow,
    /// Chain: the first failure of a stored-key pair in an hour.
    FirstInHour,
    /// Chain: the rest of an hour (per stored-key pair, or the one summary).
    HourSummary,
    /// The one stdio entry of a process.
    OncePerProcess,
}

impl Aggregation {
    fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::WindowSummary => "window_summary",
            Self::Overflow => "overflow",
            Self::FirstInHour => "first_in_hour",
            Self::HourSummary => "hour_summary",
            Self::OncePerProcess => "once_per_process",
        }
    }
}

/// One detail line (or the stdio record's content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// `None` only on an overflow line folding several reasons (or none).
    pub(crate) reason: Option<FailureReason>,
    pub(crate) transport: &'static str,
    pub(crate) route: &'static str,
    pub(crate) source: String,
    pub(crate) key_id: Option<String>,
    pub(crate) count: u64,
    pub(crate) window_secs: u64,
    pub(crate) aggregation: Aggregation,
    /// Overflow only: failures per reason.
    pub(crate) reasons: Vec<(FailureReason, u64)>,
    /// Overflow only: records dropped since the previous overflow line.
    pub(crate) dropped: u64,
}

impl Entry {
    fn http(
        reason: FailureReason,
        route: &'static str,
        source: Source,
        key_id: Option<&str>,
        count: u64,
        aggregation: Aggregation,
    ) -> Self {
        Self {
            reason: Some(reason),
            transport: TRANSPORT_HTTP,
            route,
            source: source.label(),
            key_id: key_id.map(str::to_owned),
            count,
            window_secs: WINDOW_SECS,
            aggregation,
            reasons: Vec::new(),
            dropped: 0,
        }
    }

    pub(crate) fn reason_label(&self) -> &'static str {
        self.reason.map_or(REASON_MIXED, FailureReason::as_str)
    }

    /// The `dropped_records` this line reports for the detail sink (an
    /// overflow line's `dropped`; 0 for every other line). Given back to the
    /// detail counter if this line is itself lost.
    fn reported_drops(&self) -> u64 {
        self.dropped
    }

    /// The entry's fields as a JSON object.
    pub(crate) fn details(&self) -> Value {
        let mut details = Map::new();
        details.insert("reason".into(), self.reason_label().into());
        details.insert("transport".into(), self.transport.into());
        details.insert("route".into(), self.route.into());
        details.insert("source".into(), self.source.clone().into());
        if let Some(key_id) = &self.key_id {
            details.insert("key_id".into(), key_id.clone().into());
        }
        details.insert("count".into(), self.count.into());
        details.insert("window_secs".into(), self.window_secs.into());
        details.insert("aggregation".into(), self.aggregation.as_str().into());
        if !self.reasons.is_empty() {
            let reasons: Map<String, Value> = self
                .reasons
                .iter()
                .map(|(reason, count)| (reason.as_str().to_string(), json!(count)))
                .collect();
            details.insert("reasons".into(), Value::Object(reasons));
        }
        if self.dropped > 0 {
            details.insert("dropped_records".into(), self.dropped.into());
        }
        Value::Object(details)
    }

    /// The detail-file line: a timestamp, the event type and the fields.
    fn detail_line(&self, timestamp: &str) -> String {
        let mut line = Map::new();
        line.insert("timestamp".into(), timestamp.into());
        line.insert("event_type".into(), EVENT_MCP_AUTH_FAILED.into());
        if let Value::Object(details) = self.details() {
            line.extend(details);
        }
        Value::Object(line).to_string()
    }
}

// ── Detail aggregator (pure, injected clock) ──────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BucketKey {
    reason: FailureReason,
    source: Source,
    /// A stored key row's id (`mk_…`), never request text.
    key_id: Option<Box<str>>,
}

#[derive(Debug)]
struct Bucket {
    route: &'static str,
    /// Failures counted but not yet written.
    pending: u64,
}

/// The bounded per-minute counter state of the detail sink. Pure: the caller
/// passes the clock and receives the entries to write. See the module doc.
#[derive(Debug)]
pub(crate) struct Aggregator {
    origin: Instant,
    window: u64,
    anonymous: HashMap<BucketKey, Bucket>,
    stored: HashMap<BucketKey, Bucket>,
    lines_in_window: u32,
    overflow: [u64; FailureReason::ALL.len()],
    /// Records dropped (outbox full / write failed) not yet reported.
    dropped: u64,
}

impl Aggregator {
    pub(crate) fn new(origin: Instant) -> Self {
        Self {
            origin,
            window: 0,
            anonymous: HashMap::new(),
            stored: HashMap::new(),
            lines_in_window: 0,
            overflow: [0; FailureReason::ALL.len()],
            dropped: 0,
        }
    }

    fn window_of(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.origin).as_secs() / WINDOW_SECS
    }

    /// Take one line of this window's budget.
    fn take_line(&mut self, stored_key: bool) -> bool {
        let limit = if stored_key {
            MAX_LINES_PER_WINDOW
        } else {
            MAX_LINES_PER_WINDOW - RESERVED_STORED_KEY_LINES
        };
        if self.lines_in_window < limit {
            self.lines_in_window += 1;
            true
        } else {
            false
        }
    }

    fn fold(&mut self, reason: FailureReason, count: u64) {
        let slot = &mut self.overflow[reason.index()];
        *slot = slot.saturating_add(count);
    }

    /// Count `n` records that were not written (reported on the next
    /// overflow line).
    pub(crate) fn note_dropped(&mut self, n: u64) {
        self.dropped = self.dropped.saturating_add(n);
    }

    /// Buckets currently tracked (both tables).
    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.anonymous.len() + self.stored.len()
    }

    /// If `now` is in a later window: write the ended window's summaries
    /// (stored keys first, then the busiest, within the new window's budget;
    /// the rest folds into the overflow), then the overflow line, and start
    /// the new window with empty tables.
    pub(crate) fn roll(&mut self, now: Instant, out: &mut Vec<Entry>) {
        let window = self.window_of(now);
        if window <= self.window {
            return;
        }
        self.window = window;
        self.lines_in_window = 0;
        let mut ended: Vec<(BucketKey, Bucket)> = self
            .stored
            .drain()
            .chain(self.anonymous.drain())
            .filter(|(_, bucket)| bucket.pending > 0)
            .collect();
        ended.sort_by(|(key_a, a), (key_b, b)| {
            key_b
                .key_id
                .is_some()
                .cmp(&key_a.key_id.is_some())
                .then(b.pending.cmp(&a.pending))
        });
        for (key, bucket) in ended {
            if self.take_line(key.key_id.is_some()) {
                out.push(Entry::http(
                    key.reason,
                    bucket.route,
                    key.source,
                    key.key_id.as_deref(),
                    bucket.pending,
                    Aggregation::WindowSummary,
                ));
            } else {
                self.fold(key.reason, bucket.pending);
            }
        }
        if let Some(entry) = self.take_overflow() {
            out.push(entry);
        }
    }

    fn take_overflow(&mut self) -> Option<Entry> {
        let total = self.overflow.iter().fold(0u64, |sum, n| sum.saturating_add(*n));
        if total == 0 && self.dropped == 0 {
            return None;
        }
        let reasons: Vec<(FailureReason, u64)> = FailureReason::ALL
            .iter()
            .copied()
            .zip(self.overflow)
            .filter(|(_, count)| *count > 0)
            .collect();
        self.overflow = [0; FailureReason::ALL.len()];
        let dropped = std::mem::take(&mut self.dropped);
        Some(Entry {
            reason: (reasons.len() == 1).then(|| reasons[0].0),
            transport: TRANSPORT_HTTP,
            route: MANY,
            source: MANY.to_string(),
            key_id: None,
            count: total,
            window_secs: WINDOW_SECS,
            aggregation: Aggregation::Overflow,
            reasons,
            dropped,
        })
    }

    /// Count one failure at `now`; push what must be written to `out`.
    /// The same few map operations for every reason.
    pub(crate) fn record(
        &mut self,
        now: Instant,
        reason: FailureReason,
        source: Source,
        route: &'static str,
        key_id: Option<&str>,
        out: &mut Vec<Entry>,
    ) {
        self.roll(now, out);
        let stored_key = key_id.is_some();
        let key = BucketKey { reason, source, key_id: key_id.map(Box::from) };
        let (table, cap) = if stored_key {
            (&mut self.stored, MAX_TRACKED_STORED_KEYS)
        } else {
            (&mut self.anonymous, MAX_TRACKED_KEYS)
        };
        if let Some(bucket) = table.get_mut(&key) {
            bucket.pending = bucket.pending.saturating_add(1);
            if bucket.route != route {
                bucket.route = ROUTE_MIXED;
            }
            return;
        }
        if table.len() >= cap {
            self.fold(reason, 1);
            return;
        }
        if self.take_line(stored_key) {
            out.push(Entry::http(reason, route, source, key_id, 1, Aggregation::First));
            let table = if stored_key { &mut self.stored } else { &mut self.anonymous };
            table.insert(key, Bucket { route, pending: 0 });
        } else {
            // Tracked, but over the line budget: its count is written with
            // the window's summaries (or folded into the overflow there).
            let table = if stored_key { &mut self.stored } else { &mut self.anonymous };
            table.insert(key, Bucket { route, pending: 1 });
        }
    }
}

// ── Chain aggregator (pure, injected clock) ───────────────────────────────────

/// A stored key's (key id, reason) pair: one chain bucket per hour.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct StoredKey {
    key_id: Box<str>,
    reason: FailureReason,
}

#[derive(Debug)]
struct StoredBucket {
    route: &'static str,
    /// The immediate record of this hour was enqueued.
    first_done: bool,
    /// Failures counted but not yet written.
    pending: u64,
}

/// One record for the hash-chained security audit.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChainRecord {
    pub(crate) event_type: &'static str,
    pub(crate) details: Value,
    /// Set on a stored-key pair's immediate record: the pair whose "first
    /// record done" mark depends on this record being enqueued.
    first_of: Option<StoredKey>,
    /// The `dropped_records` this record reports for the chain sink (the
    /// hour summary); 0 for every other record. Given back to the chain
    /// counter if this record is itself lost.
    reported_drops: u64,
}

fn hour_index(now_unix: i64) -> u64 {
    u64::try_from(now_unix).unwrap_or(0) / CHAIN_WINDOW_SECS
}

fn hour_start_label(hour: u64) -> String {
    i64::try_from(hour.saturating_mul(CHAIN_WINDOW_SECS))
        .ok()
        .and_then(|secs| chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0))
        .map(|start| start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

impl ChainRecord {
    fn stored(key: &StoredKey, route: &'static str, count: u64, hour: u64, aggregation: Aggregation) -> Self {
        let details = json!({
            "reason": key.reason.as_str(),
            "transport": TRANSPORT_HTTP,
            "route": route,
            "key_id": &*key.key_id,
            "count": count,
            "window_secs": CHAIN_WINDOW_SECS,
            "hour_start": hour_start_label(hour),
            "aggregation": aggregation.as_str(),
            "detail_log": DETAIL_FILE,
        });
        let first_of = (aggregation == Aggregation::FirstInHour).then(|| key.clone());
        Self { event_type: EVENT_MCP_AUTH_FAILED, details, first_of, reported_drops: 0 }
    }

    fn stdio(refusal: &Refusal) -> Self {
        Self {
            event_type: EVENT_MCP_AUTH_FAILED,
            details: stdio_entry(refusal).details(),
            first_of: None,
            reported_drops: 0,
        }
    }
}

/// The bounded hourly state of the chain sink. Pure: the caller passes the
/// clock (unix seconds; hours are aligned UTC hours) and receives the
/// records to write. See the module doc.
#[derive(Debug, Default)]
pub(crate) struct ChainAggregator {
    /// The current hour; `None` before the first event.
    hour: Option<u64>,
    stored: HashMap<StoredKey, StoredBucket>,
    /// Failures of this hour summarised in the hour summary, per reason
    /// (everything without a stored-key bucket).
    reasons: [u64; FailureReason::ALL.len()],
    /// Stored-key failures folded into the summary because the pair table
    /// was full (also in `reasons`).
    stored_overflow: u64,
    sources: HashSet<Source>,
    sources_capped: bool,
    /// Records dropped (outbox full / write failed) not yet reported.
    dropped: u64,
}

impl ChainAggregator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Count `n` records that were not written (reported in the next hour
    /// summary).
    pub(crate) fn note_dropped(&mut self, n: u64) {
        self.dropped = self.dropped.saturating_add(n);
    }

    fn note_source(&mut self, source: Source) {
        if self.sources.len() < MAX_COUNTED_SOURCES {
            self.sources.insert(source);
        } else if !self.sources.contains(&source) {
            self.sources_capped = true;
        }
    }

    /// Stored-key pairs tracked this hour.
    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.stored.len()
    }

    /// If `now_unix` is in a later hour: write the ended hour's per-pair
    /// summaries and the hour summary, and start the new hour empty. A clock
    /// that went back counts into the current hour.
    pub(crate) fn roll(&mut self, now_unix: i64, out: &mut Vec<ChainRecord>) {
        let hour = hour_index(now_unix);
        let Some(current) = self.hour else {
            self.hour = Some(hour);
            return;
        };
        if hour <= current {
            return;
        }
        self.hour = Some(hour);
        self.close_hour(current, out);
    }

    fn close_hour(&mut self, hour: u64, out: &mut Vec<ChainRecord>) {
        let mut ended: Vec<(StoredKey, StoredBucket)> =
            self.stored.drain().filter(|(_, bucket)| bucket.pending > 0).collect();
        ended.sort_by(|(a, _), (b, _)| a.key_id.cmp(&b.key_id).then(a.reason.cmp(&b.reason)));
        for (key, bucket) in ended {
            out.push(ChainRecord::stored(&key, bucket.route, bucket.pending, hour, Aggregation::HourSummary));
        }
        let total = self.reasons.iter().fold(0u64, |sum, n| sum.saturating_add(*n));
        if total > 0 || self.dropped > 0 {
            out.push(ChainRecord {
                event_type: EVENT_MCP_AUTH_FAILED_SUMMARY,
                details: json!({
                    "transport": TRANSPORT_HTTP,
                    "window_secs": CHAIN_WINDOW_SECS,
                    "hour_start": hour_start_label(hour),
                    "aggregation": Aggregation::HourSummary.as_str(),
                    "count": total,
                    "reasons": reasons_object(&self.reasons),
                    "stored_key_overflow": self.stored_overflow,
                    "distinct_sources": self.sources.len(),
                    "distinct_sources_capped": self.sources_capped,
                    "dropped_records": self.dropped,
                    "detail_log": DETAIL_FILE,
                }),
                first_of: None,
                // Zeroed below; given back if this summary is lost.
                reported_drops: self.dropped,
            });
        }
        self.reasons = [0; FailureReason::ALL.len()];
        self.stored_overflow = 0;
        self.sources.clear();
        self.sources_capped = false;
        self.dropped = 0;
    }

    /// Count one failure at `now_unix`; push what must be written to `out`.
    /// A stored-key pair's immediate record is pushed last and carries
    /// `first_of`: the caller reports its enqueue result through
    /// [`Self::confirm_first`].
    pub(crate) fn record(
        &mut self,
        now_unix: i64,
        reason: FailureReason,
        source: Source,
        route: &'static str,
        key_id: Option<&str>,
        out: &mut Vec<ChainRecord>,
    ) {
        self.roll(now_unix, out);
        let hour = self.hour.unwrap_or_else(|| hour_index(now_unix));
        let summarise = |this: &mut Self| {
            let slot = &mut this.reasons[reason.index()];
            *slot = slot.saturating_add(1);
            this.note_source(source);
        };
        let Some(key_id) = key_id else {
            summarise(self);
            return;
        };
        let key = StoredKey { key_id: Box::from(key_id), reason };
        if let Some(bucket) = self.stored.get_mut(&key) {
            if bucket.route != route {
                bucket.route = ROUTE_MIXED;
            }
            if bucket.first_done {
                bucket.pending = bucket.pending.saturating_add(1);
            } else {
                // The immediate record of this hour was not enqueued: try
                // again with this failure (the earlier one stays pending).
                out.push(ChainRecord::stored(&key, route, 1, hour, Aggregation::FirstInHour));
            }
            return;
        }
        if self.stored.len() >= MAX_CHAIN_STORED_KEYS_PER_HOUR {
            self.stored_overflow = self.stored_overflow.saturating_add(1);
            summarise(self);
            return;
        }
        out.push(ChainRecord::stored(&key, route, 1, hour, Aggregation::FirstInHour));
        self.stored.insert(key, StoredBucket { route, first_done: false, pending: 0 });
    }

    /// The immediate record of `key` was (`enqueued`) or was not handed to
    /// the writer. Only an enqueued record marks the pair done; otherwise
    /// its failure stays pending and is in the hour's summary.
    pub(crate) fn confirm_first(&mut self, key: &StoredKey, enqueued: bool) {
        if let Some(bucket) = self.stored.get_mut(key) {
            if enqueued {
                bucket.first_done = true;
            } else {
                bucket.pending = bucket.pending.saturating_add(1);
            }
        }
    }
}

// ── Outboxes ──────────────────────────────────────────────────────────────────

/// A bounded queue drained by at most one blocking writer.
#[derive(Debug)]
struct Outbox<T> {
    queue: Mutex<VecDeque<T>>,
    capacity: usize,
    running: AtomicBool,
}

impl<T> Outbox<T> {
    fn new(capacity: usize) -> Self {
        Self { queue: Mutex::new(VecDeque::new()), capacity, running: AtomicBool::new(false) }
    }

    /// Queue `item`; `false` (and nothing queued) when full.
    fn push(&self, item: T) -> bool {
        let mut queue = lock(&self.queue);
        if queue.len() >= self.capacity {
            return false;
        }
        queue.push_back(item);
        true
    }

    fn len(&self) -> usize {
        lock(&self.queue).len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The writer: empty the queue, then stop. An item pushed while it stops
    /// is picked up either by it (the re-check) or by a new writer. `failed`
    /// gets the item whose write failed.
    fn drain(&self, mut write: impl FnMut(&T) -> Result<(), String>, mut failed: impl FnMut(&T, String)) {
        loop {
            let batch: Vec<T> = lock(&self.queue).drain(..).collect();
            if batch.is_empty() {
                self.running.store(false, Ordering::Release);
                let more = !self.is_empty();
                if !more || self.running.swap(true, Ordering::AcqRel) {
                    return;
                }
                continue;
            }
            for item in &batch {
                if let Err(problem) = write(item) {
                    failed(item, problem);
                }
            }
        }
    }
}

// ── Auditor (aggregators + off-request writers) ───────────────────────────────

#[derive(Debug)]
struct AuditState {
    detail: Aggregator,
    chain: ChainAggregator,
}

impl AuditState {
    /// `n` records were lost (not enqueued, or their write failed).
    ///
    /// Accounting rule — nothing is ever counted twice in either counter:
    /// * every lost record counts 1 in each sink's counter, exactly once (as
    ///   before: the chain hour summary and the detail overflow line each
    ///   report every lost record);
    /// * the drops a lost item itself reported (`reported_drops`: an hour
    ///   summary's or an overflow line's `dropped_records`, which were moved
    ///   out of its sink's counter when it was built) go back to the counter
    ///   of the sink it was destined for, and only there — the other sink's
    ///   counter still holds those records on its own.
    ///
    /// So a lost summary that reported N drops makes the next one report
    /// N + 1 (+ anything lost since).
    fn note_lost(&mut self, n: u64, chain_reported: u64, detail_reported: u64) {
        self.detail.note_dropped(n);
        self.chain.note_dropped(n);
        self.chain.note_dropped(chain_reported);
        self.detail.note_dropped(detail_reported);
    }
}

#[derive(Debug, Clone, Copy)]
enum Sink {
    Chain,
    Detail,
}

/// One per HTTP server (router): the aggregators and writers for `home`.
pub(crate) struct AuthFailureAudit {
    home: PathBuf,
    state: Mutex<AuditState>,
    chain: Outbox<ChainRecord>,
    detail: Outbox<Entry>,
}

impl std::fmt::Debug for AuthFailureAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthFailureAudit").finish_non_exhaustive()
    }
}

impl AuthFailureAudit {
    pub(crate) fn new(home: PathBuf) -> Arc<Self> {
        Self::new_at(home, Instant::now())
    }

    /// [`Self::new`] with the detail windows aligned to `origin` (tests).
    pub(crate) fn new_at(home: PathBuf, origin: Instant) -> Arc<Self> {
        Arc::new(Self {
            home,
            state: Mutex::new(AuditState { detail: Aggregator::new(origin), chain: ChainAggregator::new() }),
            chain: Outbox::new(MAX_CHAIN_OUTBOX),
            detail: Outbox::new(MAX_DETAIL_OUTBOX),
        })
    }

    /// Count one HTTP authentication failure. Never blocks on I/O.
    pub(crate) fn record(
        self: &Arc<Self>,
        reason: FailureReason,
        source: Source,
        route: &'static str,
        key_id: Option<&str>,
    ) {
        self.record_at(Instant::now(), unix_now(), reason, source, route, key_id);
    }

    /// [`Self::record`] with injected clocks.
    pub(crate) fn record_at(
        self: &Arc<Self>,
        now: Instant,
        now_unix: i64,
        reason: FailureReason,
        source: Source,
        route: &'static str,
        key_id: Option<&str>,
    ) {
        let (mut detail_out, mut chain_out) = (Vec::new(), Vec::new());
        let dropped = {
            let mut state = lock(&self.state);
            state.detail.record(now, reason, source, route, key_id, &mut detail_out);
            state.chain.record(now_unix, reason, source, route, key_id, &mut chain_out);
            self.enqueue_locked(&mut state, chain_out, detail_out)
        };
        self.after_enqueue(dropped);
    }

    /// Roll the windows over if they ended (periodic tick). Never blocks on
    /// I/O.
    pub(crate) fn flush(self: &Arc<Self>) {
        self.flush_at(Instant::now(), unix_now());
    }

    /// [`Self::flush`] with injected clocks.
    pub(crate) fn flush_at(self: &Arc<Self>, now: Instant, now_unix: i64) {
        let (mut detail_out, mut chain_out) = (Vec::new(), Vec::new());
        let dropped = {
            let mut state = lock(&self.state);
            state.detail.roll(now, &mut detail_out);
            state.chain.roll(now_unix, &mut chain_out);
            self.enqueue_locked(&mut state, chain_out, detail_out)
        };
        self.after_enqueue(dropped);
    }

    /// Hand the records to the outboxes while the aggregator state is held,
    /// so a stored-key pair's "first record done" mark follows its actual
    /// enqueue result. Returns how many records were dropped.
    fn enqueue_locked(&self, state: &mut AuditState, chain_out: Vec<ChainRecord>, detail_out: Vec<Entry>) -> u64 {
        let mut dropped = 0u64;
        let (mut chain_reported, mut detail_reported) = (0u64, 0u64);
        for record in chain_out {
            let first_of = record.first_of.clone();
            let reported = record.reported_drops;
            let enqueued = self.chain.push(record);
            if !enqueued {
                dropped += 1;
                chain_reported = chain_reported.saturating_add(reported);
            }
            if let Some(key) = first_of {
                state.chain.confirm_first(&key, enqueued);
            }
        }
        for entry in detail_out {
            let reported = entry.reported_drops();
            if !self.detail.push(entry) {
                dropped += 1;
                detail_reported = detail_reported.saturating_add(reported);
            }
        }
        if dropped > 0 {
            state.note_lost(dropped, chain_reported, detail_reported);
        }
        dropped
    }

    fn after_enqueue(self: &Arc<Self>, dropped: u64) {
        if dropped > 0 {
            warn_write_problem(&format!("{dropped} record(s) dropped: the audit writer is behind"));
        }
        if !self.chain.is_empty() {
            self.start_writer(Sink::Chain);
        }
        if !self.detail.is_empty() {
            self.start_writer(Sink::Detail);
        }
    }

    fn start_writer(self: &Arc<Self>, sink: Sink) {
        let running = match sink {
            Sink::Chain => &self.chain.running,
            Sink::Detail => &self.detail.running,
        };
        if running.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = Arc::clone(self);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => drop(handle.spawn_blocking(move || this.drain(sink))),
            // No runtime (sync callers, unit tests): write inline.
            Err(_) => this.drain(sink),
        }
    }

    fn drain(&self, sink: Sink) {
        // A failed write loses one record; see `AuditState::note_lost`.
        let failed = |chain_reported: u64, detail_reported: u64, problem: String| {
            lock(&self.state).note_lost(1, chain_reported, detail_reported);
            warn_write_problem(&problem);
        };
        match sink {
            Sink::Chain => self.chain.drain(
                |record| write_chain(&self.home, record),
                |record, problem| failed(record.reported_drops, 0, problem),
            ),
            Sink::Detail => self.detail.drain(
                |entry| write_detail(&self.home, entry),
                |entry, problem| failed(0, entry.reported_drops(), problem),
            ),
        }
    }
}

// ── Writers ───────────────────────────────────────────────────────────────────

/// Append one chain record through the strict hash-chained writer, with its
/// `tracing::warn!` line. A home that does not exist (any more) is not
/// created by an audit write.
fn write_chain(home: &Path, record: &ChainRecord) -> Result<(), String> {
    tracing::warn!(
        event = record.event_type,
        details = %record.details,
        "MCP authentication failed"
    );
    if !home.is_dir() {
        return Err("home directory missing".into());
    }
    let event = duduclaw_security::audit::AuditEvent::new(
        record.event_type,
        AUDIT_AGENT_ID,
        duduclaw_security::audit::Severity::Warning,
        record.details.clone(),
    );
    duduclaw_security::audit::try_append_audit_event(home, &event)
}

/// Append one detail line (debug log only).
fn write_detail(home: &Path, entry: &Entry) -> Result<(), String> {
    tracing::debug!(
        event = EVENT_MCP_AUTH_FAILED,
        reason = entry.reason_label(),
        route = entry.route,
        source = entry.source.as_str(),
        count = entry.count,
        aggregation = entry.aggregation.as_str(),
        "MCP authentication failure (detail)"
    );
    if !home.is_dir() {
        return Err("home directory missing".into());
    }
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut line = entry.detail_line(&timestamp);
    line.push('\n');
    append_detail_line(home, line.as_bytes(), DETAIL_MAX_BYTES).map_err(|e| format!("{DETAIL_FILE}: {e}"))
}

/// Append `bytes` to the detail file under its own lock, rotating it to
/// [`DETAIL_ROTATED_FILE`] first when the append would take it past `cap`.
/// A record larger than `cap` on its own is refused (never rotated for).
fn append_detail_line(home: &Path, bytes: &[u8], cap: u64) -> std::io::Result<()> {
    let path = home.join(DETAIL_FILE);
    duduclaw_core::with_file_lock(&path, || {
        if bytes.len() as u64 > cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "record larger than the detail file cap",
            ));
        }
        let mut file = open_private_append(&path)?;
        let mut len = file.metadata()?.len();
        if len > 0 && len.saturating_add(bytes.len() as u64) > cap {
            drop(file);
            // One generation: the rename replaces an older rotated file
            // (and replaces, never follows, a symlink planted there).
            std::fs::rename(&path, home.join(DETAIL_ROTATED_FILE))?;
            file = open_private_append(&path)?;
            len = file.metadata()?.len();
        }
        append_or_cut_back(&mut file, len, bytes)
    })
}

/// What [`append_or_cut_back`] needs of the file besides `Write` (a seam so
/// a failing writer can be injected in tests).
trait CutBack {
    fn cut_back(&self, len: u64) -> std::io::Result<()>;
}

impl CutBack for std::fs::File {
    fn cut_back(&self, len: u64) -> std::io::Result<()> {
        self.set_len(len)
    }
}

/// Write `bytes` at the end of `file`, whose length (read from the verified
/// handle, under the lock) is `original_len`. If the write fails part-way,
/// cut the file back to `original_len` (best effort, its error ignored — on
/// Windows an append-only handle may not be allowed to) so no JSON fragment
/// is left for the next record to be appended to; the write's error is
/// returned.
fn append_or_cut_back<F: Write + CutBack>(file: &mut F, original_len: u64, bytes: &[u8]) -> std::io::Result<()> {
    match file.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = file.cut_back(original_len);
            Err(error)
        }
    }
}

/// Win32 `FILE_FLAG_OPEN_REPARSE_POINT`: open a reparse point (symlink,
/// junction, …) itself instead of following it.
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Win32 `FILE_ATTRIBUTE_REPARSE_POINT`.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Open (creating owner-only) a private JSONL file for appending. Refuses a
/// symlink, a non-regular file and a file with more than one link; tightens
/// an existing file's permissions to `0600` on Unix.
///
/// Windows: the open does not follow a reparse point, and the opened handle
/// must be a regular file that is not a reparse point. The link count is not
/// checked there (stable `std` has no handle-based link count on Windows)
/// and neither are owner / ACL: like the MCP key store
/// (`duduclaw_core::private_sqlite`), access relies on the home directory's
/// ACL — a documented limitation. Any other platform fails closed: the
/// detail file is never written (the record counts as dropped).
fn open_private_append(path: &Path) -> std::io::Result<std::fs::File> {
    use std::io::{Error, ErrorKind};
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW: a symlink at the path fails the open (ELOOP);
        // O_NONBLOCK: a FIFO planted there cannot block the writer.
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let refused: std::io::Result<()> =
            Err(Error::new(ErrorKind::Unsupported, "no safe private open on this platform"));
        refused?;
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.nlink() != 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "refusing a file with more than one link"));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Checked on the opened handle (no path re-check, no TOCTOU). The
        // link count is not checked: see the function doc.
        if meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "refusing to write through a reparse point"));
        }
    }
    Ok(file)
}

/// A failed or dropped audit write, logged at most once per
/// [`WRITE_ERROR_WARN_INTERVAL_SECS`] (process-wide).
fn warn_write_problem(problem: &str) {
    static LAST_WARNED: AtomicU64 = AtomicU64::new(0);
    let now = chrono::Utc::now().timestamp().max(1) as u64;
    let last = LAST_WARNED.load(Ordering::Relaxed);
    if (last == 0 || now.saturating_sub(last) >= WRITE_ERROR_WARN_INTERVAL_SECS)
        && LAST_WARNED.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
    {
        tracing::warn!(problem, "MCP authentication failure audit record not written");
    }
}

// ── HTTP request context ──────────────────────────────────────────────────────

/// What the admission middleware knows about a request: where audits go,
/// the socket peer, and the route label.
#[derive(Clone, Debug)]
pub(crate) struct RequestContext {
    pub(crate) audit: Arc<AuthFailureAudit>,
    pub(crate) source: Source,
    pub(crate) route: &'static str,
}

tokio::task_local! {
    static REQUEST: RequestContext;
}

/// Run `request` (the rest of the middleware stack and the handler) with
/// `context` available to the dispatcher ([`record_remote_unbound`]), then
/// count the refusal its response carries, if any. The response is returned
/// unchanged apart from removing the internal marker.
pub(crate) async fn observe_request<F>(context: RequestContext, request: F) -> Response
where
    F: Future<Output = Response>,
{
    let audit = Arc::clone(&context.audit);
    let (source, route) = (context.source, context.route);
    let mut response = REQUEST.scope(context, request).await;
    if let Some(tag) = response.extensions_mut().remove::<AuthFailureTag>() {
        audit.record(tag.reason, source, route, tag.key_id.as_deref());
    }
    response
}

/// The HTTP dispatcher refused a tool to an authenticated key without a
/// valid employee binding (`RemoteAccess::Unbound`). Counted against the
/// current request's peer and route; outside an HTTP request (no context)
/// nothing is recorded here — the dispatcher's own `tool_calls.jsonl`
/// denial row still is. Returns whether it was counted.
pub(crate) fn record_remote_unbound(key_id: Option<&str>) -> bool {
    REQUEST
        .try_with(|context| {
            context
                .audit
                .record(FailureReason::KeyUnbound, context.source, context.route, key_id)
        })
        .is_ok()
}

// ── stdio ─────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct StdioState {
    written: bool,
    attempts: u8,
    in_flight: bool,
}

/// The once-per-process stdio record: written at most once, attempted at
/// most [`MAX_STDIO_ATTEMPTS`] times, never two writes at the same time.
#[derive(Debug)]
pub(crate) struct StdioRecorder {
    state: Mutex<StdioState>,
}

impl StdioRecorder {
    pub(crate) const fn new() -> Self {
        Self { state: Mutex::new(StdioState { written: false, attempts: 0, in_flight: false }) }
    }

    /// Claim the next attempt, if one is left and none is running.
    fn begin(&self) -> bool {
        let mut state = lock(&self.state);
        if state.written || state.in_flight || state.attempts >= MAX_STDIO_ATTEMPTS {
            return false;
        }
        state.attempts += 1;
        state.in_flight = true;
        true
    }

    /// The claimed attempt ended; only a successful write sets the mark.
    fn finish(&self, written: bool) {
        let mut state = lock(&self.state);
        state.in_flight = false;
        state.written |= written;
    }

    pub(crate) fn written(&self) -> bool {
        lock(&self.state).written
    }

    #[cfg(test)]
    pub(crate) fn attempts(&self) -> u8 {
        lock(&self.state).attempts
    }
}

static STDIO: StdioRecorder = StdioRecorder::new();

/// The blocking write of one claimed attempt; it reports to `recorder`
/// itself, so its result counts even when nobody waits for it any more.
fn stdio_job(recorder: &'static StdioRecorder, home: PathBuf, record: ChainRecord) -> impl FnOnce() -> bool + Send + 'static {
    move || {
        let written = match write_chain(&home, &record) {
            Ok(()) => true,
            Err(problem) => {
                warn_write_problem(&problem);
                false
            }
        };
        recorder.finish(written);
        written
    }
}

/// Synchronous attempt (tests): `None` = no attempt left / already written
/// / one running; `Some(written)` otherwise.
#[cfg(test)]
pub(crate) fn record_stdio_failure_now(recorder: &'static StdioRecorder, home: &Path, refusal: &Refusal) -> Option<bool> {
    recorder
        .begin()
        .then(|| stdio_job(recorder, home.to_path_buf(), ChainRecord::stdio(refusal))())
}

/// Start an attempt on a blocking thread without waiting for it. `None`
/// when no attempt was started (or, without a runtime, it ran inline).
fn spawn_stdio_attempt(
    recorder: &'static StdioRecorder,
    home: &Path,
    refusal: &Refusal,
) -> Option<tokio::task::JoinHandle<bool>> {
    if !recorder.begin() {
        return None;
    }
    let job = stdio_job(recorder, home.to_path_buf(), ChainRecord::stdio(refusal));
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Some(handle.spawn_blocking(job)),
        Err(_) => {
            job();
            None
        }
    }
}

/// The stdio server refused its own key and keeps serving: record it
/// without waiting (fire and forget).
pub(crate) fn record_stdio_failure(home: &Path, refusal: &Refusal) {
    drop(spawn_stdio_attempt(&STDIO, home, refusal));
}

/// The stdio server refused its own key and is about to exit: wait for the
/// record, at most [`STDIO_WRITE_TIMEOUT`]. Returns whether it is written.
pub(crate) async fn record_stdio_failure_before_exit(home: &Path, refusal: &Refusal) -> bool {
    record_stdio_failure_bounded(&STDIO, home, refusal, STDIO_WRITE_TIMEOUT).await
}

/// [`record_stdio_failure_before_exit`] with the recorder and the bound
/// passed in (tests).
pub(crate) async fn record_stdio_failure_bounded(
    recorder: &'static StdioRecorder,
    home: &Path,
    refusal: &Refusal,
    wait: Duration,
) -> bool {
    let Some(handle) = spawn_stdio_attempt(recorder, home, refusal) else {
        return recorder.written();
    };
    // The timeout bounds only this wait. The blocking thread — including a
    // wait for the security-audit lock inside it — cannot be cancelled: it
    // stays detached, finishes on its own and still reports to `recorder`.
    // That is accepted; the async server is never blocked by it.
    matches!(tokio::time::timeout(wait, handle).await, Ok(Ok(true)))
}

fn stdio_entry(refusal: &Refusal) -> Entry {
    Entry {
        reason: Some(refusal.audit_reason()),
        transport: TRANSPORT_STDIO,
        route: ROUTE_STDIO,
        source: SOURCE_LOCAL.to_string(),
        key_id: refusal.stored_key_id.clone(),
        count: 1,
        window_secs: 0,
        aggregation: Aggregation::OncePerProcess,
        reasons: Vec::new(),
        dropped: 0,
    }
}

#[cfg(test)]
#[path = "mcp_auth_audit_tests.rs"]
pub(crate) mod tests;
