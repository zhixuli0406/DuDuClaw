//! `[tick]` configuration — resident-sensing source declarations (WP1).
//!
//! Split out of [`crate::tick_source`] (which owns the runtime: pollers, ring
//! buffer, field derivation) purely to keep both files inside the project's
//! 200-400-line convention. This module has no dependency back on the runtime.
//!
//! ## Shape
//!
//! ```toml
//! [tick]
//! enabled = false                 # master switch — default OFF
//! allow_command_sources = false   # D5: `command` kind is fail-closed
//!
//! [[tick.sources]]
//! id = "twse-2330"
//! kind = "http_poll"              # http_poll | command | file_tail | websocket
//! enabled = true
//! interval_secs = 10
//! url = "https://example.invalid/quote"
//! json_fields = { price = "/data/price", vol = "/data/volume" }
//! emit_unchanged = false
//! max_events_per_minute = 120
//! persist_every_n = 0
//! ```
//!
//! ## Loading discipline
//!
//! - The whole feature is **default off** ([`TickConfig::default`] has
//!   `enabled = false`), so an install that never writes a `[tick]` section
//!   is byte-identical to before this change.
//! - A missing / malformed `config.toml`, or a missing / malformed `[tick]`
//!   section, resolves to [`TickConfig::default`] — same defensive convention
//!   as `TaskForwardModelConfig::from_home` / `GoalLoopConfig::from_home`.
//! - **Per-source fail-closed (D7)**: one malformed or invalid
//!   `[[tick.sources]]` entry is warned about and dropped; it never aborts the
//!   load of the other sources and never blocks gateway boot.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::warn;

/// Frequency floor (D6). A source asking for less than this is clamped up —
/// a 0/sub-second poll loop is a runaway, not a configuration.
pub const MIN_INTERVAL_SECS: u64 = 1;
/// Poll interval used when a source omits `interval_secs`.
pub const DEFAULT_INTERVAL_SECS: u64 = 10;
/// Per-source emission cap (D6) used when a source omits
/// `max_events_per_minute`. Over-cap ticks are dropped **and counted** — never
/// silently swallowed.
pub const DEFAULT_MAX_EVENTS_PER_MINUTE: u32 = 120;
/// Upper bound on `id` / extracted-field-name length.
const MAX_ID_LEN: usize = 64;
/// D5-W — how many `subscribe` frames a `websocket` source may send after
/// connecting. A subscription handshake is a handful of frames; anything
/// longer is a configuration mistake, not a feed.
pub const MAX_SUBSCRIBE_FRAMES: usize = 8;
/// D5-W — byte ceiling on one `subscribe` frame.
pub const MAX_SUBSCRIBE_FRAME_BYTES: usize = 4096;
/// D5-W2 — how many custom `headers` one source may declare.
pub const MAX_HEADERS: usize = 8;
/// D5-W2 — byte ceiling on one header value.
pub const MAX_HEADER_VALUE_BYTES: usize = 1024;
/// D5-W2 — default WS client-ping period (seconds of inbound silence before a
/// `Ping` frame is sent). `0` disables pinging.
pub const DEFAULT_PING_INTERVAL_SECS: u64 = 30;
/// D5-W2 — default WS idle watchdog (seconds of *total* inbound silence,
/// pongs included, before the connection is recycled). `0` disables it.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
/// D5-W2 — floor on a non-zero `ping_interval_secs`. Below this the client
/// would prod a healthy peer several times a second for no diagnostic gain.
pub const MIN_PING_INTERVAL_SECS: u64 = 5;
/// D5-W2 — floor on a non-zero `idle_timeout_secs`. The recycle path redials
/// **immediately** (a quiet feed is not a failure, so it does not pay the
/// backoff), which means the timeout itself is the only thing bounding how
/// often a permanently-silent peer is redialled. Anything under this turns the
/// watchdog into a reconnect storm, so it is refused rather than clamped —
/// a source asking for a 1-second watchdog has misunderstood the setting, and
/// silently rewriting it to 30 would hide that.
pub const MIN_IDLE_TIMEOUT_SECS: u64 = 30;
/// R1 — default lifetime of a per-field delta baseline (1 hour).
pub const DEFAULT_BASELINE_MAX_AGE_SECS: u64 = 3600;
/// R1 — a non-zero baseline lifetime under this many seconds is *warned*
/// about, not refused.
///
/// There is no floor here, unlike the watchdog clocks, because the failure
/// modes are different in kind: a too-small `idle_timeout_secs` produces an
/// unbounded reconnect loop (a runaway), whereas a too-small
/// `baseline_max_age_secs` merely means the delta trio is almost always
/// absent. That is a *loss of function*, and a legitimate one for a
/// sub-second feed — refusing it would be inventing a policy. But it is also
/// exactly the silent no-delta symptom two live-fire rounds were spent
/// removing, so it does not get to happen quietly.
pub const BASELINE_AGE_WARN_BELOW_SECS: u64 = 60;
/// R2 — default reuse window for a screened DNS answer.
pub const DEFAULT_DNS_TTL_SECS: u64 = 60;

/// D5-W2 — header names the transport owns. An operator-supplied value for any
/// of these would either corrupt the HTTP/WS framing or forge the handshake, so
/// they are refused at config load rather than silently dropped at send time.
/// Compared **lowercased and whole** (never `contains`).
pub const RESERVED_HEADER_NAMES: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "upgrade",
    "transfer-encoding",
];
/// D5-W2 — prefix reserved for the websocket handshake's own headers
/// (`sec-websocket-key` / `-version` / `-protocol` / `-extensions`).
pub const RESERVED_HEADER_PREFIX: &str = "sec-websocket-";

/// D7 — field names an extraction may not claim, because
/// [`crate::autopilot_engine::AutopilotEvent::Tick`]'s `to_fields()` owns
/// them. A source declaring one of these is disabled at load time rather than
/// silently shadowing the event's own identity fields.
pub const RESERVED_FIELD_NAMES: &[&str] = &["event", "source", "ts", "kind"];
/// D7 — prefixes reserved for the deterministic delta derivation (D2:
/// `prev_<f>` / `delta_<f>` / `pct_<f>`).
pub const RESERVED_FIELD_PREFIXES: &[&str] = &["prev_", "delta_", "pct_"];

/// How a source produces its payload (D5, D5-W).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickKind {
    /// HTTP GET on an interval. SSRF-validated with the shared
    /// `web_fetch::validate_url` gate.
    HttpPoll,
    /// Execute an argv vector (never a shell string) and take stdout as the
    /// payload. Requires the global `allow_command_sources` switch.
    Command,
    /// Poll a file's length, read newly-appended lines; one line = one tick.
    FileTail,
    /// D5-W — a long-lived WebSocket connection; every **text** frame is one
    /// payload, pushed rather than polled. `interval_secs` stops meaning
    /// "poll period" here and becomes the reconnect-backoff starting point.
    Websocket,
}

impl TickKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HttpPoll => "http_poll",
            Self::Command => "command",
            Self::FileTail => "file_tail",
            Self::Websocket => "websocket",
        }
    }
}

/// One `[[tick.sources]]` entry, after [`validate_source`] normalization.
///
/// `Debug` is **hand-written** ([`std::fmt::Debug for TickSourceConfig`]) so
/// that `headers` renders as a count. Deriving it would leave one
/// `debug!(?cfg)` anywhere in the gateway's future between an API key and the
/// log file; the convention "don't log the config" is not a guarantee, this is.
/// There is deliberately no `Serialize`, for the same reason.
#[derive(Clone, PartialEq, Deserialize)]
pub struct TickSourceConfig {
    /// `^[a-z0-9][a-z0-9-]{0,63}$` — also the ring-buffer key and the
    /// `source` field rules match on, so it must be stable and unambiguous.
    pub id: String,
    pub kind: TickKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Clamped to at least [`MIN_INTERVAL_SECS`] by [`validate_source`].
    ///
    /// For `websocket` sources this is **not** a poll period (nothing is
    /// polled) — it is the reconnect backoff's starting delay (D5-W).
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// `http_poll` (http/https) and `websocket` (ws/wss) — validated by the
    /// scheme-appropriate gate in [`validate_source`].
    #[serde(default)]
    pub url: Option<String>,
    /// `command` only — argv, executed directly (no shell).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// `file_tail` only — replaced by its canonical form by
    /// [`validate_source`].
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// `websocket` only (D5-W) — verbatim frames sent, in order, right after
    /// the connection opens (a quote feed's subscribe handshake). No
    /// templating: what is written here is what is sent. Capped at
    /// [`MAX_SUBSCRIBE_FRAMES`] × [`MAX_SUBSCRIBE_FRAME_BYTES`], and cleared
    /// for every other kind.
    #[serde(default)]
    pub subscribe: Vec<String>,
    /// D5-W2 — custom request headers, for `http_poll` (every GET) and
    /// `websocket` (the upgrade request). The usual shape for a feed behind an
    /// API key: `headers = { "X-API-Key" = "…" }`.
    ///
    /// **Values are credentials.** They are never logged (not even at
    /// `debug`), never surfaced by `ticks.sources` (which exposes only a
    /// count), and never rendered into a prompt. Validated by
    /// [`validate_headers`]; cleared for the kinds that make no request.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// D5-W2, `websocket` only — seconds of inbound silence after which a WS
    /// `Ping` frame is sent. `0` disables client pinging.
    #[serde(default = "default_ping_interval_secs")]
    pub ping_interval_secs: u64,
    /// D5-W2, `websocket` only — seconds of *total* inbound silence (pongs
    /// count as traffic) after which the connection is recycled and
    /// immediately redialled. `0` disables the watchdog.
    ///
    /// A TCP connection to a feed that stops sending can stay "open" for
    /// hours; without this the source is silently dead and only `last_tick_ts`
    /// says so, after the fact.
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// `field name → JSON pointer` (RFC 6901). A pointer that doesn't resolve
    /// leaves the field **absent**, never zero — an absent field satisfies no
    /// condition (`autopilot_engine::apply_op`), which is exactly the
    /// "no data ⇒ don't fire" semantics rules want.
    #[serde(default)]
    pub json_fields: BTreeMap<String, String>,
    /// When `false` (default) a payload whose content is unchanged since the
    /// previous poll emits nothing.
    #[serde(default)]
    pub emit_unchanged: bool,
    #[serde(default = "default_max_events_per_minute")]
    pub max_events_per_minute: u32,
    /// D1 — `0` (default) means tick events never touch `events.db`. A
    /// positive `n` persists every nth emitted tick for audit trails.
    #[serde(default)]
    pub persist_every_n: u32,
    /// R1 — how long a field's delta baseline stays usable, in seconds.
    /// `0` disables expiry (the pre-R1 behavior: a baseline lives forever).
    ///
    /// The F1 baseline is per-field last-seen, which is right for a feed that
    /// interleaves partial frames but wrong across a long gap: a field that
    /// stopped being reported yesterday would, on its next appearance, produce
    /// a delta against a day-old value — a huge, entirely fictional move,
    /// delivered to a rule as if it had just happened. Past this age the
    /// baseline is discarded and the next observation re-establishes it, which
    /// reads exactly like a first tick (no `prev_`/`delta_`/`pct_` at all).
    #[serde(default = "default_baseline_max_age_secs")]
    pub baseline_max_age_secs: u64,
    /// R2 — seconds a screened DNS answer may be reused before the host is
    /// re-resolved. `0` re-resolves on every request/dial.
    ///
    /// **Not read from the per-source table**: this is the global
    /// `[tick] dns_ttl_secs`, copied onto each source by
    /// [`TickConfig::from_section`] so a source task carries everything it
    /// needs. A `dns_ttl_secs` key written inside `[[tick.sources]]` has no
    /// effect.
    #[serde(skip)]
    pub dns_ttl_secs: u64,
}

impl std::fmt::Debug for TickSourceConfig {
    /// Every field except `headers`, which is reduced to `headers_count` —
    /// the same thing the dashboard RPC exposes, and for the same reason.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TickSourceConfig")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("enabled", &self.enabled)
            .field("interval_secs", &self.interval_secs)
            .field("url", &self.url)
            .field("command", &self.command)
            .field("path", &self.path)
            .field("subscribe", &self.subscribe)
            .field("headers_count", &self.headers.len())
            .field("ping_interval_secs", &self.ping_interval_secs)
            .field("idle_timeout_secs", &self.idle_timeout_secs)
            .field("json_fields", &self.json_fields)
            .field("emit_unchanged", &self.emit_unchanged)
            .field("max_events_per_minute", &self.max_events_per_minute)
            .field("persist_every_n", &self.persist_every_n)
            .field("baseline_max_age_secs", &self.baseline_max_age_secs)
            .field("dns_ttl_secs", &self.dns_ttl_secs)
            .finish()
    }
}

fn default_true() -> bool {
    true
}
fn default_interval_secs() -> u64 {
    DEFAULT_INTERVAL_SECS
}
fn default_max_events_per_minute() -> u32 {
    DEFAULT_MAX_EVENTS_PER_MINUTE
}
fn default_ping_interval_secs() -> u64 {
    DEFAULT_PING_INTERVAL_SECS
}
fn default_idle_timeout_secs() -> u64 {
    DEFAULT_IDLE_TIMEOUT_SECS
}
fn default_baseline_max_age_secs() -> u64 {
    DEFAULT_BASELINE_MAX_AGE_SECS
}

/// The `[tick]` section. `enabled = false` by default — with it unset, no
/// source task is ever spawned and nothing about the gateway changes.
#[derive(Debug, Clone, PartialEq)]
pub struct TickConfig {
    pub enabled: bool,
    /// D5 — `command` sources stay disabled until the operator flips this
    /// global switch (fail-closed: a source-level `kind = "command"` is not
    /// sufficient authority to execute a binary).
    pub allow_command_sources: bool,
    /// R2 — seconds a screened DNS answer may be reused before the host is
    /// re-resolved. `0` re-resolves on every request/dial. Global, not
    /// per-source: it is a property of how aggressively this install trusts
    /// its resolver, not of one feed. Copied onto every source by
    /// [`Self::from_section`].
    pub dns_ttl_secs: u64,
    /// Only entries that parsed AND validated. Invalid entries were warned
    /// about and dropped at load time.
    pub sources: Vec<TickSourceConfig>,
}

impl Default for TickConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_command_sources: false,
            dns_ttl_secs: DEFAULT_DNS_TTL_SECS,
            sources: Vec::new(),
        }
    }
}

impl TickConfig {
    /// Load `[tick]` from `<home>/config.toml`. Absent / malformed file or
    /// section ⇒ [`Self::default`] (feature off).
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::from_toml_str(&content)
    }

    /// Parse a whole `config.toml` body. Public for tests and for callers
    /// that already hold the file contents.
    pub fn from_toml_str(content: &str) -> Self {
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("tick").and_then(|v| v.as_table()) {
            Some(section) => Self::from_section(section),
            None => Self::default(),
        }
    }

    /// Parse an already-extracted `[tick]` table.
    ///
    /// Each source is parsed and validated **individually** (D7): a malformed
    /// entry, a reserved field name, an SSRF-blocked URL, a `command` source
    /// without the global switch, or a duplicate `id` disables just that
    /// source with a warning.
    pub fn from_section(section: &toml::Table) -> Self {
        let enabled = section
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let allow_command_sources = section
            .get("allow_command_sources")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // R2 — a malformed / absent value falls back to the default rather
        // than to `0`: silently switching every source to per-request
        // resolution would be a performance cliff nobody asked for.
        let dns_ttl_secs = section
            .get("dns_ttl_secs")
            .and_then(|v| v.as_integer())
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(DEFAULT_DNS_TTL_SECS);

        let mut sources: Vec<TickSourceConfig> = Vec::new();
        if let Some(arr) = section.get("sources").and_then(|v| v.as_array()) {
            for (index, item) in arr.iter().enumerate() {
                let raw: TickSourceConfig = match item.clone().try_into() {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            index,
                            error = %e,
                            "[tick] source entry ignored — malformed (missing id/kind, or wrong type)"
                        );
                        continue;
                    }
                };
                let id = raw.id.clone();
                if sources.iter().any(|s| s.id == id) {
                    warn!(
                        source = %id,
                        "[tick] source disabled — duplicate id (the first entry wins)"
                    );
                    continue;
                }
                match validate_source(raw, allow_command_sources) {
                    // R2 — the global TTL is stamped onto the source here, so
                    // a task never has to reach back for section-level config.
                    Ok(s) => sources.push(TickSourceConfig { dns_ttl_secs, ..s }),
                    Err(e) => warn!(
                        source = %id,
                        error = %e,
                        "[tick] source disabled — invalid configuration"
                    ),
                }
            }
        }

        Self {
            enabled,
            allow_command_sources,
            dns_ttl_secs,
            sources,
        }
    }

    /// Sources that should actually get a poll task: the feature is on, and
    /// the source itself is not switched off.
    pub fn active_sources(&self) -> Vec<&TickSourceConfig> {
        if !self.enabled {
            return Vec::new();
        }
        self.sources.iter().filter(|s| s.enabled).collect()
    }
}

/// Validate + normalize one source. Returns a **new** value (project
/// immutability convention) rather than mutating in place.
///
/// Fail-closed everywhere: anything not positively understood is an error, and
/// an error disables that one source.
pub fn validate_source(
    raw: TickSourceConfig,
    allow_command_sources: bool,
) -> Result<TickSourceConfig, String> {
    if !is_valid_source_id(&raw.id) {
        return Err(format!(
            "id '{}' must match ^[a-z0-9][a-z0-9-]{{0,63}}$",
            raw.id
        ));
    }

    for (name, pointer) in &raw.json_fields {
        validate_field_name(name)?;
        validate_json_pointer(name, pointer)?;
    }

    // D6 — floor the interval and keep at least one event per minute
    // allowed (a `0` cap would silently mute the source forever).
    let interval_secs = raw.interval_secs.max(MIN_INTERVAL_SECS);
    let max_events_per_minute = raw.max_events_per_minute.max(1);

    // R1 — advisory, never fatal: see `BASELINE_AGE_WARN_BELOW_SECS`.
    if raw.baseline_max_age_secs > 0 && raw.baseline_max_age_secs < BASELINE_AGE_WARN_BELOW_SECS {
        warn!(
            source = %raw.id,
            baseline_max_age_secs = raw.baseline_max_age_secs,
            "[tick] baseline_max_age_secs is very short — prev_/delta_/pct_ fields \
             will be absent from most ticks; set 0 to disable expiry entirely"
        );
    }
    // R1 — the specific combination that guarantees the above rather than
    // merely risking it. Also advisory: a slow source with a short baseline is
    // a coherent thing to write down, it just cannot ever produce a delta.
    if baseline_expires_between_polls(raw.kind, raw.baseline_max_age_secs, interval_secs) {
        warn!(
            source = %raw.id,
            baseline_max_age_secs = raw.baseline_max_age_secs,
            interval_secs,
            "[tick] baseline_max_age_secs is shorter than interval_secs — every \
             baseline expires before the next poll, so prev_/delta_/pct_ will be \
             absent from EVERY tick; raise it above the poll interval, or set 0 \
             to disable expiry"
        );
    }

    let mut url = raw.url.clone();
    let mut command = raw.command.clone();
    let mut path = raw.path.clone();
    // `subscribe` is meaningful only for `websocket`; every other kind gets an
    // empty vector so a stray entry can never be silently carried around (and
    // can never be mistaken for an active handshake later).
    let mut subscribe: Vec<String> = Vec::new();
    // D5-W2 — same reasoning for `headers` and the two websocket clocks: only
    // the kinds that can act on them keep them.
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let mut ping_interval_secs = 0u64;
    let mut idle_timeout_secs = 0u64;

    match raw.kind {
        TickKind::HttpPoll => {
            command = None;
            path = None;
            let u = url
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "kind = \"http_poll\" requires `url`".to_string())?;
            // Shared SSRF gate — same validator every other outbound fetch
            // path in the gateway uses (loopback / private ranges / cloud
            // metadata / non-http schemes are all refused there).
            crate::web_fetch::validate_url(u).map_err(|e| format!("url rejected: {e}"))?;
            headers = validate_headers(&raw.headers)?;
        }
        TickKind::Command => {
            url = None;
            path = None;
            if !allow_command_sources {
                return Err(
                    "kind = \"command\" requires `[tick] allow_command_sources = true`".into(),
                );
            }
            let argv = command
                .as_ref()
                .ok_or_else(|| "kind = \"command\" requires `command` argv".to_string())?;
            if argv.is_empty() || argv[0].trim().is_empty() {
                return Err("`command` argv must have a non-empty program at index 0".into());
            }
        }
        TickKind::FileTail => {
            url = None;
            command = None;
            let p = path
                .as_ref()
                .ok_or_else(|| "kind = \"file_tail\" requires `path`".to_string())?;
            let expanded = duduclaw_core::expand_tilde(&p.to_string_lossy());
            // Canonicalize (resolves symlinks + `..`) and, implicitly, prove
            // the file exists. A path that appears later needs a config
            // reload — deliberate: a tail source pointed at a typo'd path
            // should surface at load time, not stay silently dead.
            let canonical = std::fs::canonicalize(&expanded)
                .map_err(|e| format!("path {} unreadable: {e}", expanded.display()))?;
            if !canonical.is_file() {
                return Err(format!(
                    "path {} is not a regular file",
                    canonical.display()
                ));
            }
            path = Some(canonical);
        }
        TickKind::Websocket => {
            command = None;
            path = None;
            let u = url
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "kind = \"websocket\" requires `url`".to_string())?;
            validate_ws_url(u)?;
            subscribe = validate_subscribe_frames(&raw.subscribe)?;
            headers = validate_headers(&raw.headers)?;
            // D5-W2 — per-clock floors, checked before the pairing rule so the
            // error names the field that is actually wrong. `0` stays legal on
            // both (it means "disabled"); anything between 1 and the floor is a
            // misunderstanding of the unit and is refused, not clamped.
            if raw.ping_interval_secs > 0 && raw.ping_interval_secs < MIN_PING_INTERVAL_SECS {
                return Err(format!(
                    "ping_interval_secs ({}) must be 0 (disabled) or at least \
                     {MIN_PING_INTERVAL_SECS}",
                    raw.ping_interval_secs
                ));
            }
            if raw.idle_timeout_secs > 0 && raw.idle_timeout_secs < MIN_IDLE_TIMEOUT_SECS {
                return Err(format!(
                    "idle_timeout_secs ({}) must be 0 (disabled) or at least \
                     {MIN_IDLE_TIMEOUT_SECS} — the watchdog redials immediately, so a \
                     shorter timeout is a reconnect loop",
                    raw.idle_timeout_secs
                ));
            }
            // D5-W2 — with both clocks on, a ping that is never answered must
            // still be able to trip the watchdog. `idle <= ping` would either
            // recycle before the ping could prove anything (idle < ping) or
            // race it (idle == ping), so the pair is refused outright rather
            // than silently reordered.
            if raw.ping_interval_secs > 0
                && raw.idle_timeout_secs > 0
                && raw.idle_timeout_secs <= raw.ping_interval_secs
            {
                return Err(format!(
                    "idle_timeout_secs ({}) must be greater than ping_interval_secs ({}) \
                     when both are enabled",
                    raw.idle_timeout_secs, raw.ping_interval_secs
                ));
            }
            ping_interval_secs = raw.ping_interval_secs;
            idle_timeout_secs = raw.idle_timeout_secs;
        }
    }

    Ok(TickSourceConfig {
        id: raw.id,
        kind: raw.kind,
        enabled: raw.enabled,
        interval_secs,
        url,
        command,
        path,
        subscribe,
        headers,
        ping_interval_secs,
        idle_timeout_secs,
        json_fields: raw.json_fields,
        emit_unchanged: raw.emit_unchanged,
        max_events_per_minute,
        persist_every_n: raw.persist_every_n,
        baseline_max_age_secs: raw.baseline_max_age_secs,
        // Overwritten from the global `[tick] dns_ttl_secs` by
        // `TickConfig::from_section`; a per-source key never reaches here.
        dns_ttl_secs: raw.dns_ttl_secs,
    })
}

/// R1 — does this source's delta baseline expire before its next observation
/// can possibly arrive?
///
/// True when expiry is on and the lifetime is shorter than the poll interval:
/// the baseline recorded by one poll is already stale when the next one lands,
/// so `prev_`/`delta_`/`pct_` are absent from *every* tick. Pure, so the
/// decision is testable without capturing log output.
///
/// **`websocket` is excluded.** For a push source `interval_secs` is the
/// reconnect-backoff starting point, not a tick spacing (D5-W) — frames can
/// arrive hundreds of times a second on a source whose `interval_secs` is 10.
/// Comparing the two there would fire on a perfectly healthy configuration,
/// and a warning that cries wolf is worse than no warning.
pub fn baseline_expires_between_polls(
    kind: TickKind,
    baseline_max_age_secs: u64,
    interval_secs: u64,
) -> bool {
    if matches!(kind, TickKind::Websocket) {
        return false;
    }
    baseline_max_age_secs > 0 && baseline_max_age_secs < interval_secs
}

/// D5-W — validate a `websocket` source's URL.
///
/// Two gates, in this order:
///
/// 1. **Scheme**: only `ws://` / `wss://`. Anything else (including a plain
///    `https://` typo) is refused rather than coerced.
/// 2. **Host**: a loopback host is the one place plaintext `ws://` is allowed
///    — the traffic never leaves the machine, and it is what a local relay
///    process (the documented way to bridge an exotic feed) actually offers.
///    Every other host must be `wss://`, and is then handed to the SAME SSRF
///    validator every other outbound fetch path uses, with the scheme mapped
///    `wss → https` so private ranges, link-local and cloud-metadata hosts are
///    refused by exactly one implementation instead of a second copy here.
///
/// Host comparison is exact (`==` on the lowercased host / a parsed `IpAddr`),
/// never `contains`/`starts_with` — `ws://localhost.evil.example` is a public
/// host and must not inherit the loopback carve-out.
pub fn validate_ws_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("url rejected: invalid URL: {e}"))?;

    let scheme = parsed.scheme();
    if scheme != "ws" && scheme != "wss" {
        return Err(format!(
            "kind = \"websocket\" requires a ws:// or wss:// url (got {scheme}://)"
        ));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "url rejected: missing host".to_string())?;
    let host_lower = host.to_ascii_lowercase();
    // `host_str()` returns IPv6 literals bracketed (`[::1]`).
    let bare = host_lower.trim_start_matches('[').trim_end_matches(']');
    let is_loopback = bare == "localhost"
        || bare
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);

    if is_loopback {
        return Ok(());
    }

    if scheme != "wss" {
        return Err(format!(
            "plaintext ws:// is only allowed for loopback hosts \
             (127.0.0.1 / localhost / ::1); use wss:// for {host_lower}"
        ));
    }

    let mut mapped = parsed.clone();
    mapped
        .set_scheme("https")
        .map_err(|_| "url rejected: could not normalize wss:// for validation".to_string())?;
    crate::web_fetch::validate_url(mapped.as_str()).map_err(|e| format!("url rejected: {e}"))?;
    Ok(())
}

/// D5-W — bound the post-connect subscribe handshake. Returns a **new** vector
/// (the project's immutability convention) rather than trimming in place.
fn validate_subscribe_frames(frames: &[String]) -> Result<Vec<String>, String> {
    if frames.len() > MAX_SUBSCRIBE_FRAMES {
        return Err(format!(
            "`subscribe` has {} frames — the maximum is {MAX_SUBSCRIBE_FRAMES}",
            frames.len()
        ));
    }
    for (index, frame) in frames.iter().enumerate() {
        if frame.trim().is_empty() {
            return Err(format!("`subscribe[{index}]` is empty"));
        }
        if frame.len() > MAX_SUBSCRIBE_FRAME_BYTES {
            return Err(format!(
                "`subscribe[{index}]` is {} bytes — the maximum is {MAX_SUBSCRIBE_FRAME_BYTES}",
                frame.len()
            ));
        }
    }
    Ok(frames.to_vec())
}

/// D5-W2 — validate the custom `headers` table. Returns a **new** map (the
/// project's immutability convention).
///
/// Fail-closed on four axes:
///
/// 1. **Count** — at most [`MAX_HEADERS`].
/// 2. **Name** — `^[A-Za-z0-9-]{1,64}$`, and not a transport-owned name
///    ([`RESERVED_HEADER_NAMES`] / [`RESERVED_HEADER_PREFIX`]). The reserved
///    comparison lowercases both sides and matches the **whole** name (plus one
///    explicit prefix family), never a substring — `X-Host` is a perfectly good
///    custom header and must not be caught by a `contains("host")`.
/// 3. **Value length** — at most [`MAX_HEADER_VALUE_BYTES`].
/// 4. **Value bytes** — visible ASCII plus space only. This bans CR/LF (header
///    injection: a value carrying `\r\n` would otherwise append attacker-chosen
///    headers, or a whole second request, to every poll), every other C0
///    control and DEL, and non-ASCII bytes — which also guarantees the value
///    survives `HeaderValue::from_str` at send time instead of failing there.
///
/// Error messages name the **header**, never the value: an invalid API key is
/// still an API key, and this string reaches the log.
pub fn validate_headers(
    headers: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if headers.len() > MAX_HEADERS {
        return Err(format!(
            "`headers` has {} entries — the maximum is {MAX_HEADERS}",
            headers.len()
        ));
    }
    for (name, value) in headers {
        if name.is_empty() || name.len() > MAX_ID_LEN {
            return Err(format!(
                "header name '{name}' must be 1-{MAX_ID_LEN} characters"
            ));
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!(
                "header name '{name}' may only contain ASCII letters, digits and '-'"
            ));
        }
        let lower = name.to_ascii_lowercase();
        if RESERVED_HEADER_NAMES.iter().any(|r| *r == lower)
            || lower.starts_with(RESERVED_HEADER_PREFIX)
        {
            return Err(format!(
                "header '{name}' is reserved by the transport (reserved: {}, {RESERVED_HEADER_PREFIX}*)",
                RESERVED_HEADER_NAMES.join(", ")
            ));
        }
        if value.len() > MAX_HEADER_VALUE_BYTES {
            return Err(format!(
                "header '{name}' has a {}-byte value — the maximum is {MAX_HEADER_VALUE_BYTES}",
                value.len()
            ));
        }
        if let Some(bad) = value.chars().find(|c| !is_legal_header_value_char(*c)) {
            let described = match bad {
                '\r' => "CR".to_string(),
                '\n' => "LF".to_string(),
                other => format!("U+{:04X}", other as u32),
            };
            return Err(format!(
                "header '{name}' has an illegal character in its value ({described}) — \
                 values must be visible ASCII"
            ));
        }
    }
    Ok(headers.clone())
}

/// Visible ASCII (`0x20`-`0x7E`). Deliberately excludes tab: a tab is legal in
/// an HTTP field value only as obsolete line folding, which is the same class
/// of trouble as a bare CR/LF.
fn is_legal_header_value_char(c: char) -> bool {
    matches!(c, ' '..='~')
}

/// Whether a header value is legal to send: non-empty, within the length cap,
/// visible ASCII only.
///
/// The same rule [`validate_headers`] enforces at config-load time, exposed so
/// that a value which never went through config validation — one resolved from
/// a `secret://` backend at request time
/// ([`crate::tick_headers::resolve_header_secrets`]) — is held to it too.
pub(crate) fn header_value_is_legal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_HEADER_VALUE_BYTES
        && value.chars().all(is_legal_header_value_char)
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`, hand-rolled (the project does not carry a
/// regex dependency for this class of check — see
/// `autopilot_engine::is_safe_agent_id`).
pub fn is_valid_source_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or(' ');
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// D7 reserved-word check plus a conservative character allowlist. Both the
/// name comparison and the prefix comparison are case-insensitive so
/// `Delta_x` can't slip past the lowercase prefix table.
fn validate_field_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_ID_LEN {
        return Err(format!(
            "json_fields key '{name}' must be 1-{MAX_ID_LEN} characters"
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "json_fields key '{name}' may only contain ASCII letters, digits and '_'"
        ));
    }
    let lower = name.to_ascii_lowercase();
    if RESERVED_FIELD_NAMES.iter().any(|r| *r == lower) {
        return Err(format!(
            "json_fields key '{name}' is reserved (reserved names: {})",
            RESERVED_FIELD_NAMES.join(", ")
        ));
    }
    if let Some(prefix) = RESERVED_FIELD_PREFIXES
        .iter()
        .find(|p| lower.starts_with(**p))
    {
        return Err(format!(
            "json_fields key '{name}' uses the reserved prefix '{prefix}' \
             (derived delta fields own it)"
        ));
    }
    Ok(())
}

/// RFC 6901: a pointer is either the empty string (whole document) or starts
/// with `/`. `serde_json`'s `Value::pointer` silently returns `None` for any
/// other shape, which would look like "field absent" forever — reject at load
/// time instead.
fn validate_json_pointer(name: &str, pointer: &str) -> Result<(), String> {
    if pointer.is_empty() || pointer.starts_with('/') {
        Ok(())
    } else {
        Err(format!(
            "json_fields['{name}'] pointer '{pointer}' must be a JSON pointer \
             (empty, or starting with '/')"
        ))
    }
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> TickConfig {
        TickConfig::from_toml_str(body)
    }

    fn source(body: &str) -> TickSourceConfig {
        let cfg = parse(body);
        assert_eq!(cfg.sources.len(), 1, "expected exactly one valid source");
        cfg.sources.into_iter().next().unwrap()
    }

    #[test]
    fn absent_section_is_disabled_by_default() {
        let cfg = parse("[general]\nlog_level = \"info\"\n");
        assert!(!cfg.enabled, "master switch defaults OFF");
        assert!(!cfg.allow_command_sources);
        assert!(cfg.sources.is_empty());
    }

    #[test]
    fn malformed_config_falls_back_to_default() {
        let cfg = parse("this is not = toml [[[");
        assert_eq!(cfg, TickConfig::default());
    }

    #[test]
    fn http_poll_source_parses_with_defaults() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "twse-2330"
            kind = "http_poll"
            url = "https://example.com/quote"
            json_fields = { price = "/data/price" }
            "#,
        );
        assert_eq!(s.id, "twse-2330");
        assert_eq!(s.kind, TickKind::HttpPoll);
        assert!(s.enabled, "sources default to enabled");
        assert_eq!(s.interval_secs, DEFAULT_INTERVAL_SECS);
        assert_eq!(s.max_events_per_minute, DEFAULT_MAX_EVENTS_PER_MINUTE);
        assert!(!s.emit_unchanged);
        assert_eq!(s.persist_every_n, 0, "D1: no events.db writes by default");
        assert_eq!(s.json_fields.get("price").unwrap(), "/data/price");
    }

    #[test]
    fn interval_floor_is_one_second() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "fast"
            kind = "http_poll"
            interval_secs = 0
            url = "https://example.com/x"
            "#,
        );
        assert_eq!(s.interval_secs, MIN_INTERVAL_SECS, "D6 floor");
    }

    #[test]
    fn zero_rate_cap_is_raised_to_one() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "muted"
            kind = "http_poll"
            max_events_per_minute = 0
            url = "https://example.com/x"
            "#,
        );
        assert_eq!(s.max_events_per_minute, 1);
    }

    // ── D7: reserved words ───────────────────────────────────

    #[test]
    fn reserved_field_names_disable_the_source() {
        for reserved in RESERVED_FIELD_NAMES {
            let cfg = parse(&format!(
                r#"
                [tick]
                enabled = true
                [[tick.sources]]
                id = "s1"
                kind = "http_poll"
                url = "https://example.com/x"
                json_fields = {{ {reserved} = "/a" }}
                "#
            ));
            assert!(
                cfg.sources.is_empty(),
                "reserved name '{reserved}' must disable the source"
            );
        }
    }

    #[test]
    fn reserved_field_prefixes_disable_the_source() {
        for name in ["prev_price", "delta_price", "pct_price", "Delta_price"] {
            let cfg = parse(&format!(
                r#"
                [tick]
                enabled = true
                [[tick.sources]]
                id = "s1"
                kind = "http_poll"
                url = "https://example.com/x"
                json_fields = {{ {name} = "/a" }}
                "#
            ));
            assert!(
                cfg.sources.is_empty(),
                "reserved prefix in '{name}' must disable the source"
            );
        }
    }

    #[test]
    fn one_invalid_source_does_not_take_down_the_others() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "bad"
            kind = "http_poll"
            url = "https://example.com/x"
            json_fields = { source = "/a" }
            [[tick.sources]]
            id = "good"
            kind = "http_poll"
            url = "https://example.com/y"
            "#,
        );
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.sources[0].id, "good");
    }

    #[test]
    fn malformed_entry_is_skipped_not_fatal() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "no-kind"
            [[tick.sources]]
            id = "good"
            kind = "http_poll"
            url = "https://example.com/y"
            "#,
        );
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.sources[0].id, "good");
    }

    #[test]
    fn duplicate_ids_keep_only_the_first() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "dup"
            kind = "http_poll"
            url = "https://first.example.com/x"
            [[tick.sources]]
            id = "dup"
            kind = "http_poll"
            url = "https://second.example.com/x"
            "#,
        );
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(
            cfg.sources[0].url.as_deref(),
            Some("https://first.example.com/x")
        );
    }

    // ── id + pointer validation ──────────────────────────────

    #[test]
    fn source_id_allowlist() {
        assert!(is_valid_source_id("twse-2330"));
        assert!(is_valid_source_id("a"));
        assert!(is_valid_source_id("0"));
        assert!(!is_valid_source_id(""));
        assert!(!is_valid_source_id("-leading"));
        assert!(!is_valid_source_id("Upper"));
        assert!(!is_valid_source_id("under_score"));
        assert!(!is_valid_source_id("../escape"));
        assert!(!is_valid_source_id(&"a".repeat(65)));
    }

    #[test]
    fn non_pointer_json_field_is_rejected() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "s1"
            kind = "http_poll"
            url = "https://example.com/x"
            json_fields = { price = "data.price" }
            "#,
        );
        assert!(cfg.sources.is_empty(), "dotted path is not a JSON pointer");
    }

    // ── SSRF (D5 http_poll) ──────────────────────────────────

    #[test]
    fn ssrf_urls_disable_the_source() {
        for url in [
            "http://localhost/x",
            "http://127.0.0.1/x",
            "http://169.254.169.254/latest/meta-data",
            "https://metadata.google.internal/x",
            "file:///etc/passwd",
            "http://192.168.1.10/x",
        ] {
            let cfg = parse(&format!(
                r#"
                [tick]
                enabled = true
                [[tick.sources]]
                id = "s1"
                kind = "http_poll"
                url = "{url}"
                "#
            ));
            assert!(cfg.sources.is_empty(), "{url} must be refused");
        }
    }

    #[test]
    fn http_poll_without_url_is_refused() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "s1"
            kind = "http_poll"
            "#,
        );
        assert!(cfg.sources.is_empty());
    }

    // ── D5 command gate ──────────────────────────────────────

    #[test]
    fn command_source_needs_the_global_switch() {
        let body = r#"
            [tick]
            enabled = true
            allow_command_sources = ALLOW
            [[tick.sources]]
            id = "quotes"
            kind = "command"
            command = ["curl", "-s", "https://example.com/q"]
            "#;
        assert!(
            parse(&body.replace("ALLOW", "false")).sources.is_empty(),
            "fail-closed without the global switch"
        );
        let allowed = parse(&body.replace("ALLOW", "true"));
        assert_eq!(allowed.sources.len(), 1);
        assert_eq!(allowed.sources[0].command.as_ref().unwrap()[0], "curl");
    }

    #[test]
    fn empty_command_argv_is_refused() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            allow_command_sources = true
            [[tick.sources]]
            id = "empty"
            kind = "command"
            command = []
            "#,
        );
        assert!(cfg.sources.is_empty());
    }

    // ── file_tail ────────────────────────────────────────────

    #[test]
    fn file_tail_path_is_canonicalized_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("feed.jsonl");
        std::fs::write(&file, "{}\n").unwrap();

        let cfg = parse(&format!(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "feed"
            kind = "file_tail"
            path = "{}"
            "#,
            file.display()
        ));
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(
            cfg.sources[0].path.as_ref().unwrap(),
            &std::fs::canonicalize(&file).unwrap()
        );

        let missing = parse(&format!(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "feed"
            kind = "file_tail"
            path = "{}"
            "#,
            dir.path().join("nope.jsonl").display()
        ));
        assert!(missing.sources.is_empty(), "missing path is fail-closed");
    }

    // ── D5-W websocket ───────────────────────────────────────

    fn websocket(url: &str, extra: &str) -> TickConfig {
        parse(&format!(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "feed"
            kind = "websocket"
            url = "{url}"
            {extra}
            "#
        ))
    }

    #[test]
    fn websocket_source_parses_with_defaults() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "quotes"
            kind = "websocket"
            url = "wss://example.com/stream"
            json_fields = { price = "/p" }
            "#,
        );
        assert_eq!(s.kind, TickKind::Websocket);
        assert_eq!(s.kind.as_str(), "websocket");
        assert!(s.subscribe.is_empty(), "subscribe is optional");
        // `interval_secs` keeps its default; for websocket it is the backoff
        // starting point, not a poll period.
        assert_eq!(s.interval_secs, DEFAULT_INTERVAL_SECS);
        assert!(s.command.is_none() && s.path.is_none());
    }

    #[test]
    fn websocket_rejects_non_ws_schemes() {
        for url in [
            "https://example.com/stream",
            "http://example.com/stream",
            "file:///etc/passwd",
            "wsss://example.com/stream",
        ] {
            assert!(
                websocket(url, "").sources.is_empty(),
                "{url} must be refused — only ws:// / wss://"
            );
        }
    }

    #[test]
    fn websocket_refuses_plaintext_ws_to_a_non_loopback_host() {
        for url in [
            "ws://example.com/stream",
            // A host that merely *contains* a loopback name is a public host.
            "ws://localhost.evil.example/stream",
            "ws://127.0.0.1.evil.example/stream",
            "ws://8.8.8.8/stream",
        ] {
            assert!(
                websocket(url, "").sources.is_empty(),
                "{url} must be refused — plaintext ws:// is loopback-only"
            );
        }
    }

    #[test]
    fn websocket_allows_plaintext_ws_on_loopback() {
        for url in [
            "ws://127.0.0.1:8765/stream",
            "ws://localhost:8765/stream",
            "ws://[::1]:8765/stream",
            "ws://LocalHost:8765/stream",
            "wss://127.0.0.1:8765/stream",
        ] {
            let cfg = websocket(url, "");
            assert_eq!(cfg.sources.len(), 1, "{url} must be allowed (loopback)");
        }
    }

    #[test]
    fn websocket_wss_reuses_the_shared_ssrf_gate() {
        // Public host over TLS — allowed.
        assert_eq!(
            websocket("wss://example.com/stream", "").sources.len(),
            1,
            "a public wss:// endpoint is the normal case"
        );
        // Everything the http_poll gate refuses is refused here too, because
        // it IS the same validator (wss → https before the check).
        for url in [
            "wss://192.168.1.10/stream",
            "wss://10.0.0.5/stream",
            "wss://169.254.169.254/stream",
            "wss://metadata.google.internal/stream",
            "wss://0.0.0.0/stream",
        ] {
            assert!(
                websocket(url, "").sources.is_empty(),
                "{url} must be refused by the shared SSRF gate"
            );
        }
    }

    #[test]
    fn websocket_without_url_is_refused() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "feed"
            kind = "websocket"
            "#,
        );
        assert!(cfg.sources.is_empty());
    }

    #[test]
    fn websocket_subscribe_frames_are_capped() {
        let eight: Vec<String> = (0..MAX_SUBSCRIBE_FRAMES)
            .map(|i| format!("\"sub{i}\""))
            .collect();
        let ok = websocket(
            "wss://example.com/stream",
            &format!("subscribe = [{}]", eight.join(", ")),
        );
        assert_eq!(ok.sources.len(), 1, "exactly the cap is allowed");
        assert_eq!(ok.sources[0].subscribe.len(), MAX_SUBSCRIBE_FRAMES);
        assert_eq!(ok.sources[0].subscribe[0], "sub0");

        let nine: Vec<String> = (0..=MAX_SUBSCRIBE_FRAMES)
            .map(|i| format!("\"sub{i}\""))
            .collect();
        assert!(
            websocket(
                "wss://example.com/stream",
                &format!("subscribe = [{}]", nine.join(", "))
            )
            .sources
            .is_empty(),
            "one over the frame cap disables the source"
        );

        let huge = "x".repeat(MAX_SUBSCRIBE_FRAME_BYTES + 1);
        assert!(
            websocket(
                "wss://example.com/stream",
                &format!("subscribe = [\"{huge}\"]")
            )
            .sources
            .is_empty(),
            "one over-size frame disables the source"
        );

        assert!(
            websocket("wss://example.com/stream", "subscribe = [\"  \"]")
                .sources
                .is_empty(),
            "an empty frame is a configuration mistake, not a handshake"
        );
    }

    #[test]
    fn subscribe_is_cleared_for_non_websocket_kinds() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "poller"
            kind = "http_poll"
            url = "https://example.com/x"
            subscribe = ["{\"op\":\"sub\"}"]
            "#,
        );
        assert!(
            s.subscribe.is_empty(),
            "subscribe only means anything for websocket sources"
        );
    }

    // ── D5-W2: idle watchdog + client ping ───────────────────

    #[test]
    fn websocket_watchdog_clocks_default_to_30s_ping_and_300s_idle() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "quotes"
            kind = "websocket"
            url = "wss://example.com/stream"
            "#,
        );
        assert_eq!(s.ping_interval_secs, DEFAULT_PING_INTERVAL_SECS);
        assert_eq!(s.idle_timeout_secs, DEFAULT_IDLE_TIMEOUT_SECS);
    }

    #[test]
    fn watchdog_clocks_are_cleared_for_non_websocket_kinds() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "poller"
            kind = "http_poll"
            url = "https://example.com/x"
            ping_interval_secs = 5
            idle_timeout_secs = 60
            "#,
        );
        assert_eq!(s.ping_interval_secs, 0, "only websocket sources ping");
        assert_eq!(s.idle_timeout_secs, 0, "only websocket sources idle out");
    }

    #[test]
    fn idle_timeout_must_exceed_ping_interval_when_both_are_on() {
        let both = |ping: u64, idle: u64| {
            websocket(
                "wss://example.com/stream",
                &format!("ping_interval_secs = {ping}\nidle_timeout_secs = {idle}"),
            )
        };
        // Equal and inverted are both refused: a ping must have room to be
        // answered before the watchdog gives up on the connection.
        assert!(both(30, 30).sources.is_empty(), "equal is refused");
        assert!(both(60, 30).sources.is_empty(), "inverted is refused");
        assert_eq!(
            both(30, 31).sources.len(),
            1,
            "one second of room is enough"
        );

        // Either clock disabled ⇒ the ordering rule does not apply.
        assert_eq!(both(0, 30).sources.len(), 1, "ping off");
        assert_eq!(both(5, 0).sources.len(), 1, "watchdog off");
        assert_eq!(both(0, 0).sources.len(), 1, "both off");
    }

    #[test]
    fn watchdog_clocks_have_a_floor_when_enabled() {
        let both = |ping: u64, idle: u64| {
            websocket(
                "wss://example.com/stream",
                &format!("ping_interval_secs = {ping}\nidle_timeout_secs = {idle}"),
            )
        };

        // Exactly the floors is legal — the boundary belongs to the allowed
        // side, and this pair also satisfies `idle > ping`.
        let ok = both(MIN_PING_INTERVAL_SECS, MIN_IDLE_TIMEOUT_SECS);
        assert_eq!(ok.sources.len(), 1, "the floors themselves are allowed");
        assert_eq!(ok.sources[0].ping_interval_secs, MIN_PING_INTERVAL_SECS);
        assert_eq!(ok.sources[0].idle_timeout_secs, MIN_IDLE_TIMEOUT_SECS);

        // One under either floor disables the source. Refused, never clamped:
        // silently rewriting 1 → 30 would hide the misunderstanding.
        for ping in 1..MIN_PING_INTERVAL_SECS {
            assert!(
                both(ping, DEFAULT_IDLE_TIMEOUT_SECS).sources.is_empty(),
                "ping_interval_secs = {ping} is under the floor and must disable the source"
            );
        }
        for idle in [1, 5, 10, MIN_IDLE_TIMEOUT_SECS - 1] {
            assert!(
                both(0, idle).sources.is_empty(),
                "idle_timeout_secs = {idle} is under the floor and must disable the source"
            );
        }

        // `0` still means "disabled" on both and is never treated as under the
        // floor — that is the whole escape hatch.
        assert_eq!(both(0, 0).sources.len(), 1, "0 is disabled, not too small");
        assert_eq!(
            both(0, DEFAULT_IDLE_TIMEOUT_SECS).sources.len(),
            1,
            "ping disabled, watchdog at its default"
        );
        assert_eq!(
            both(DEFAULT_PING_INTERVAL_SECS, 0).sources.len(),
            1,
            "watchdog disabled, ping at its default"
        );

        // The shipped defaults must clear their own floors.
        assert!(DEFAULT_PING_INTERVAL_SECS >= MIN_PING_INTERVAL_SECS);
        assert!(DEFAULT_IDLE_TIMEOUT_SECS >= MIN_IDLE_TIMEOUT_SECS);
    }

    #[test]
    fn watchdog_floor_errors_name_the_offending_field() {
        let err = validate_source(
            TickSourceConfig {
                id: "feed".into(),
                kind: TickKind::Websocket,
                enabled: true,
                interval_secs: 5,
                url: Some("wss://example.com/stream".into()),
                command: None,
                path: None,
                subscribe: Vec::new(),
                headers: BTreeMap::new(),
                ping_interval_secs: 0,
                idle_timeout_secs: 1,
                json_fields: BTreeMap::new(),
                emit_unchanged: false,
                max_events_per_minute: 120,
                persist_every_n: 0,
                baseline_max_age_secs: DEFAULT_BASELINE_MAX_AGE_SECS,
                dns_ttl_secs: 0,
            },
            false,
        )
        .expect_err("a 1-second watchdog must be refused");
        assert!(err.contains("idle_timeout_secs"), "{err}");
        assert!(err.contains("30"), "the floor is stated: {err}");
    }

    // ── R1: baseline lifetime ────────────────────────────────

    #[test]
    fn baseline_max_age_defaults_to_an_hour_and_parses_per_source() {
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "s1"
            kind = "http_poll"
            url = "https://example.com/x"
            "#,
        );
        assert_eq!(s.baseline_max_age_secs, DEFAULT_BASELINE_MAX_AGE_SECS);
        assert_eq!(DEFAULT_BASELINE_MAX_AGE_SECS, 3600);

        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "s1"
            kind = "http_poll"
            url = "https://example.com/x"
            baseline_max_age_secs = 300
            "#,
        );
        assert_eq!(s.baseline_max_age_secs, 300);
    }

    #[test]
    fn a_very_short_baseline_lifetime_is_warned_about_not_refused() {
        // Deliberately NOT a floor: unlike the watchdog clocks this cannot run
        // away, and a sub-minute baseline is legitimate for a fast feed. It is
        // warned about at load because "no deltas, ever" is otherwise silent.
        for secs in [1, 5, BASELINE_AGE_WARN_BELOW_SECS - 1] {
            let s = source(&format!(
                r#"
                [tick]
                enabled = true
                [[tick.sources]]
                id = "s1"
                kind = "http_poll"
                url = "https://example.com/x"
                baseline_max_age_secs = {secs}
                "#
            ));
            assert_eq!(s.baseline_max_age_secs, secs, "kept as written");
        }
        // `0` is the explicit "never expires" escape hatch and is not short.
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "s1"
            kind = "http_poll"
            url = "https://example.com/x"
            baseline_max_age_secs = 0
            "#,
        );
        assert_eq!(s.baseline_max_age_secs, 0);
    }

    #[test]
    fn a_baseline_shorter_than_the_poll_interval_is_flagged() {
        // The guaranteed-no-delta combination: the baseline written by one
        // poll is already stale when the next poll lands.
        assert!(baseline_expires_between_polls(TickKind::HttpPoll, 30, 60));
        assert!(baseline_expires_between_polls(TickKind::Command, 1, 2));
        assert!(baseline_expires_between_polls(
            TickKind::FileTail,
            3599,
            86_400
        ));

        // Equal is fine: the baseline is checked with `>=`, so a baseline
        // written at t and read at t+interval is exactly on the boundary and
        // the operator's intent ("one interval of memory") is honoured.
        assert!(!baseline_expires_between_polls(TickKind::HttpPoll, 60, 60));
        assert!(!baseline_expires_between_polls(
            TickKind::HttpPoll,
            3600,
            10
        ));

        // `0` means "never expires" — the opposite of the problem.
        assert!(!baseline_expires_between_polls(
            TickKind::HttpPoll,
            0,
            86_400
        ));

        // websocket is exempt: `interval_secs` is its reconnect backoff, not a
        // tick spacing, so this comparison is meaningless there.
        assert!(!baseline_expires_between_polls(
            TickKind::Websocket,
            5,
            3600
        ));
    }

    #[test]
    fn a_baseline_shorter_than_the_interval_warns_but_still_loads() {
        // Advisory, exactly like the sub-minute warning: the source is kept
        // verbatim, both values intact.
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "slow"
            kind = "http_poll"
            url = "https://example.com/x"
            interval_secs = 86400
            baseline_max_age_secs = 3600
            "#,
        );
        assert_eq!(s.interval_secs, 86_400);
        assert_eq!(s.baseline_max_age_secs, 3600);
        assert!(baseline_expires_between_polls(
            s.kind,
            s.baseline_max_age_secs,
            s.interval_secs
        ));

        // The documented fixes both silence it, and neither is refused.
        let raised = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "slow"
            kind = "http_poll"
            url = "https://example.com/x"
            interval_secs = 86400
            baseline_max_age_secs = 172800
            "#,
        );
        assert!(!baseline_expires_between_polls(
            raised.kind,
            raised.baseline_max_age_secs,
            raised.interval_secs
        ));
        let disabled = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "slow"
            kind = "http_poll"
            url = "https://example.com/x"
            interval_secs = 86400
            baseline_max_age_secs = 0
            "#,
        );
        assert!(!baseline_expires_between_polls(
            disabled.kind,
            disabled.baseline_max_age_secs,
            disabled.interval_secs
        ));
    }

    #[test]
    fn the_interval_warning_uses_the_floored_interval() {
        // `interval_secs = 0` is floored to 1 (D6); the advisory must compare
        // against what will actually be used, not the raw value.
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "fast"
            kind = "http_poll"
            url = "https://example.com/x"
            interval_secs = 0
            baseline_max_age_secs = 1
            "#,
        );
        assert_eq!(s.interval_secs, MIN_INTERVAL_SECS);
        assert!(
            !baseline_expires_between_polls(s.kind, s.baseline_max_age_secs, s.interval_secs),
            "1s baseline against the 1s floor is not a mismatch"
        );
    }

    // ── R2: DNS TTL ──────────────────────────────────────────

    #[test]
    fn dns_ttl_is_global_defaulted_and_stamped_onto_every_source() {
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "a"
            kind = "http_poll"
            url = "https://example.com/x"
            [[tick.sources]]
            id = "b"
            kind = "websocket"
            url = "wss://example.com/stream"
            "#,
        );
        assert_eq!(cfg.dns_ttl_secs, DEFAULT_DNS_TTL_SECS);
        assert_eq!(DEFAULT_DNS_TTL_SECS, 60);
        for s in &cfg.sources {
            assert_eq!(
                s.dns_ttl_secs, DEFAULT_DNS_TTL_SECS,
                "every source task carries the global TTL: {}",
                s.id
            );
        }

        let cfg = parse(
            r#"
            [tick]
            enabled = true
            dns_ttl_secs = 5
            [[tick.sources]]
            id = "a"
            kind = "http_poll"
            url = "https://example.com/x"
            "#,
        );
        assert_eq!(cfg.dns_ttl_secs, 5);
        assert_eq!(cfg.sources[0].dns_ttl_secs, 5);
    }

    #[test]
    fn a_zero_dns_ttl_is_honoured_but_a_malformed_one_falls_back() {
        let zero = parse(
            r#"
            [tick]
            enabled = true
            dns_ttl_secs = 0
            [[tick.sources]]
            id = "a"
            kind = "http_poll"
            url = "https://example.com/x"
            "#,
        );
        assert_eq!(zero.dns_ttl_secs, 0, "0 = resolve every request");
        assert_eq!(zero.sources[0].dns_ttl_secs, 0);

        // A negative / non-integer value must not silently become `0` — that
        // would flip every source to per-request resolution.
        for bad in ["-1", "\"soon\"", "true"] {
            let cfg = parse(&format!(
                r#"
                [tick]
                enabled = true
                dns_ttl_secs = {bad}
                [[tick.sources]]
                id = "a"
                kind = "http_poll"
                url = "https://example.com/x"
                "#
            ));
            assert_eq!(
                cfg.dns_ttl_secs, DEFAULT_DNS_TTL_SECS,
                "malformed {bad} falls back to the default"
            );
        }
    }

    #[test]
    fn a_per_source_dns_ttl_key_has_no_effect() {
        // The TTL is a property of the install, not of one feed. A key written
        // in the wrong place must not appear to work.
        let cfg = parse(
            r#"
            [tick]
            enabled = true
            dns_ttl_secs = 30
            [[tick.sources]]
            id = "a"
            kind = "http_poll"
            url = "https://example.com/x"
            dns_ttl_secs = 999
            "#,
        );
        assert_eq!(cfg.sources[0].dns_ttl_secs, 30, "the global wins");
    }

    // ── D5-W2: custom headers ────────────────────────────────

    #[test]
    fn headers_parse_for_the_two_kinds_that_make_requests() {
        for (kind, target) in [
            ("http_poll", "url = \"https://example.com/x\""),
            ("websocket", "url = \"wss://example.com/stream\""),
        ] {
            let s = source(&format!(
                r#"
                [tick]
                enabled = true
                [[tick.sources]]
                id = "feed"
                kind = "{kind}"
                {target}
                headers = {{ "X-API-Key" = "secret-value", "Accept" = "application/json" }}
                "#
            ));
            assert_eq!(s.headers.len(), 2, "{kind}");
            assert_eq!(s.headers.get("X-API-Key").unwrap(), "secret-value");
            assert_eq!(s.headers.get("Accept").unwrap(), "application/json");
        }
    }

    #[test]
    fn headers_are_cleared_for_kinds_that_make_no_request() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("feed.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let s = source(&format!(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "feed"
            kind = "file_tail"
            path = "{}"
            headers = {{ "X-API-Key" = "secret-value" }}
            "#,
            file.display()
        ));
        assert!(
            s.headers.is_empty(),
            "a file tail sends no request — a stray credential must not be carried around"
        );
    }

    #[test]
    fn reserved_header_names_disable_the_source() {
        for name in [
            "Host",
            "host",
            "Content-Length",
            "Connection",
            "Upgrade",
            "Transfer-Encoding",
            "Sec-WebSocket-Key",
            "sec-websocket-version",
        ] {
            let cfg = websocket(
                "wss://example.com/stream",
                &format!("headers = {{ \"{name}\" = \"x\" }}"),
            );
            assert!(
                cfg.sources.is_empty(),
                "'{name}' is transport-owned and must disable the source"
            );
        }
    }

    #[test]
    fn a_custom_header_merely_containing_a_reserved_word_is_allowed() {
        // The reserved check is whole-name (plus one explicit prefix family),
        // never a substring — these are ordinary custom headers.
        for name in ["X-Host", "Host-Override", "My-Connection-Id"] {
            let cfg = websocket(
                "wss://example.com/stream",
                &format!("headers = {{ \"{name}\" = \"x\" }}"),
            );
            assert_eq!(cfg.sources.len(), 1, "'{name}' must be allowed");
        }
    }

    #[test]
    fn header_values_with_crlf_or_control_characters_disable_the_source() {
        for value in [
            "bad\\r\\nX-Injected: yes", // CR/LF — header injection
            "bad\\rvalue",
            "bad\\nvalue",
            "bad\\u0000value", // NUL
            "bad\\u0007value", // BEL
            "bad\\tvalue",     // tab: obsolete line folding
            "祕密",            // non-ASCII
        ] {
            let cfg = websocket(
                "wss://example.com/stream",
                &format!("headers = {{ \"X-Api-Key\" = \"{value}\" }}"),
            );
            assert!(
                cfg.sources.is_empty(),
                "value {value:?} must disable the source"
            );
        }
    }

    #[test]
    fn header_count_name_and_value_limits_are_enforced() {
        let ok: Vec<String> = (0..MAX_HEADERS)
            .map(|i| format!("\"X-H{i}\" = \"v\""))
            .collect();
        assert_eq!(
            websocket(
                "wss://example.com/stream",
                &format!("headers = {{ {} }}", ok.join(", "))
            )
            .sources
            .len(),
            1,
            "exactly the cap is allowed"
        );

        let too_many: Vec<String> = (0..=MAX_HEADERS)
            .map(|i| format!("\"X-H{i}\" = \"v\""))
            .collect();
        assert!(
            websocket(
                "wss://example.com/stream",
                &format!("headers = {{ {} }}", too_many.join(", "))
            )
            .sources
            .is_empty(),
            "one over the header cap disables the source"
        );

        let long_value = "v".repeat(MAX_HEADER_VALUE_BYTES + 1);
        assert!(
            websocket(
                "wss://example.com/stream",
                &format!("headers = {{ \"X-Api-Key\" = \"{long_value}\" }}")
            )
            .sources
            .is_empty(),
            "an over-size value disables the source"
        );
        assert_eq!(
            websocket(
                "wss://example.com/stream",
                &format!(
                    "headers = {{ \"X-Api-Key\" = \"{}\" }}",
                    "v".repeat(MAX_HEADER_VALUE_BYTES)
                )
            )
            .sources
            .len(),
            1,
            "exactly the value cap is allowed"
        );

        for name in ["X Api Key", "X_Api_Key", "X-Api:Key", "", &"x".repeat(65)] {
            assert!(
                websocket(
                    "wss://example.com/stream",
                    &format!("headers = {{ \"{name}\" = \"v\" }}")
                )
                .sources
                .is_empty(),
                "header name {name:?} must disable the source"
            );
        }
    }

    #[test]
    fn debug_formatting_reduces_headers_to_a_count() {
        // Structural, not conventional: a future `debug!(?cfg)` cannot leak an
        // API key even if whoever writes it never reads this module.
        let s = source(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "secured"
            kind = "http_poll"
            url = "https://example.com/x"
            headers = { "X-Api-Key" = "sk-live-DO-NOT-LOG" }
            "#,
        );
        let rendered = format!("{s:?}");
        assert!(
            !rendered.contains("sk-live"),
            "the header value must not survive Debug: {rendered}"
        );
        assert!(rendered.contains("headers_count: 1"), "{rendered}");
        // The rest of the config is still fully inspectable.
        assert!(rendered.contains("secured") && rendered.contains("HttpPoll"));

        // Same guarantee through the enclosing `[tick]` section.
        let cfg = TickConfig {
            enabled: true,
            allow_command_sources: false,
            dns_ttl_secs: DEFAULT_DNS_TTL_SECS,
            sources: vec![s],
        };
        assert!(!format!("{cfg:?}").contains("sk-live"));
    }

    #[test]
    fn header_rejection_messages_never_echo_the_value() {
        let secret = "sk-live-DO-NOT-LOG\r\nX-Injected: yes";
        let err = validate_headers(&BTreeMap::from([(
            "X-Api-Key".to_string(),
            secret.to_string(),
        )]))
        .expect_err("CRLF must be refused");
        assert!(err.contains("X-Api-Key"), "the header name is diagnostic");
        assert!(
            !err.contains("sk-live"),
            "the value is a credential and must never reach a log line: {err}"
        );
    }

    // ── active_sources gating ────────────────────────────────

    #[test]
    fn active_sources_respects_both_switches() {
        let cfg = parse(
            r#"
            [tick]
            enabled = false
            [[tick.sources]]
            id = "s1"
            kind = "http_poll"
            url = "https://example.com/x"
            "#,
        );
        assert_eq!(cfg.sources.len(), 1, "still parsed");
        assert!(
            cfg.active_sources().is_empty(),
            "master switch off ⇒ nothing runs"
        );

        let per_source_off = parse(
            r#"
            [tick]
            enabled = true
            [[tick.sources]]
            id = "s1"
            kind = "http_poll"
            enabled = false
            url = "https://example.com/x"
            "#,
        );
        assert!(per_source_off.active_sources().is_empty());
    }
}
