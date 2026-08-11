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
//! kind = "http_poll"              # http_poll | command | file_tail
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

/// D7 — field names an extraction may not claim, because
/// [`crate::autopilot_engine::AutopilotEvent::Tick`]'s `to_fields()` owns
/// them. A source declaring one of these is disabled at load time rather than
/// silently shadowing the event's own identity fields.
pub const RESERVED_FIELD_NAMES: &[&str] = &["event", "source", "ts", "kind"];
/// D7 — prefixes reserved for the deterministic delta derivation (D2:
/// `prev_<f>` / `delta_<f>` / `pct_<f>`).
pub const RESERVED_FIELD_PREFIXES: &[&str] = &["prev_", "delta_", "pct_"];

/// How a source produces its payload (D5). WebSocket is deliberately absent —
/// a streaming feed is reachable today by pointing `command` at a CLI/curl
/// that prints one JSON line per event.
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
}

impl TickKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HttpPoll => "http_poll",
            Self::Command => "command",
            Self::FileTail => "file_tail",
        }
    }
}

/// One `[[tick.sources]]` entry, after [`validate_source`] normalization.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TickSourceConfig {
    /// `^[a-z0-9][a-z0-9-]{0,63}$` — also the ring-buffer key and the
    /// `source` field rules match on, so it must be stable and unambiguous.
    pub id: String,
    pub kind: TickKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Clamped to at least [`MIN_INTERVAL_SECS`] by [`validate_source`].
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// `http_poll` only.
    #[serde(default)]
    pub url: Option<String>,
    /// `command` only — argv, executed directly (no shell).
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// `file_tail` only — replaced by its canonical form by
    /// [`validate_source`].
    #[serde(default)]
    pub path: Option<PathBuf>,
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

/// The `[tick]` section. `enabled = false` by default — with it unset, no
/// source task is ever spawned and nothing about the gateway changes.
#[derive(Debug, Clone, PartialEq)]
pub struct TickConfig {
    pub enabled: bool,
    /// D5 — `command` sources stay disabled until the operator flips this
    /// global switch (fail-closed: a source-level `kind = "command"` is not
    /// sufficient authority to execute a binary).
    pub allow_command_sources: bool,
    /// Only entries that parsed AND validated. Invalid entries were warned
    /// about and dropped at load time.
    pub sources: Vec<TickSourceConfig>,
}

impl Default for TickConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_command_sources: false,
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
                    Ok(s) => sources.push(s),
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

    let mut url = raw.url.clone();
    let mut command = raw.command.clone();
    let mut path = raw.path.clone();

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
    }

    Ok(TickSourceConfig {
        id: raw.id,
        kind: raw.kind,
        enabled: raw.enabled,
        interval_secs,
        url,
        command,
        path,
        json_fields: raw.json_fields,
        emit_unchanged: raw.emit_unchanged,
        max_events_per_minute,
        persist_every_n: raw.persist_every_n,
    })
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
