//! Security audit event log — append-only JSONL file.
//!
//! [C-2b] All security events (drift, injection, quarantine) are persisted
//! to `~/.duduclaw/security_audit.jsonl` for forensic review.
//!
//! ## Tool-call trace completeness (R4, TraceElephant arXiv:2604.22708)
//!
//! The 2026-07 trace audit found `tool_calls.jsonl` records captured only a
//! caller-authored **outcome summary** (`params_summary`, e.g.
//! `"ok: old_hash=…, size=…"`) — the tool's *input arguments* were never
//! persisted, so post-hoc forensics could see *that* a state-changing tool
//! ran but not *what it was asked to do*. [`append_tool_call_with_input`]
//! closes that gap: call sites may pass the raw args JSON and the record
//! gains two **optional** fields (`input`, `input_truncated`) — old rows
//! and old callers stay valid, and every existing consumer (the Rust
//! `action_claim_verifier`, the Python adapters) parses records as generic
//! JSON objects, so the schema remains backward-compatible.
//!
//! ### Retention / size tradeoff
//! Inputs are captured **only for state-changing tools** (read-only tools
//! are skipped by a conservative verb-token check — unknown names count as
//! state-changing so evidence is never silently dropped), values under
//! secret-looking keys are masked *before* serialization, and the
//! serialized input is capped at [`AUDIT_INPUT_MAX_CHARS`] chars
//! (CJK-safe `truncate_chars`) with an explicit `input_truncated: true`
//! marker. B3b (below) later added a second capped field, `result_text`,
//! to every record — roughly doubling average record size versus the
//! input-only estimate this note originally made. With the rotation cap
//! raised to [`TOOL_CALLS_ROTATION_MAX_BYTES`] (16 MB, up from the original
//! 5 MB — 2026-08 M4 review) the rotation cadence lands in the same
//! ballpark as before B3b, rather than shortening under the extra
//! per-record weight. The rotation cadence still shortens under heavy tool
//! traffic in general, trading history *depth* for input/result
//! *completeness*. Operators who need longer retention should archive the
//! `.jsonl.old` file, not raise the cap further.
//!
//! ## Tool-call RESULT capture (B3b, GroundEval arXiv:2606.22737)
//!
//! `duduclaw-core::grounding::check_grounded` — the B3 zero-LLM grounding
//! pre-check wired into `dispatch_engine.rs` — compares an agent's final
//! answer against `result_text` on each evidence record, but until B3b no
//! writer ever populated that field, so the gate perpetually observed
//! `ResultTextMissing` and degraded (inert-by-default). This is the fix:
//! [`append_tool_call_with_input`] gained an additional optional
//! `result_text` parameter, masked via [`mask_sensitive_text`] (a
//! free-text-oriented sibling of [`mask_sensitive_json`] — tool *results*
//! are prose/mixed text, not a JSON key tree) and capped at
//! [`AUDIT_RESULT_TEXT_MAX_CHARS`] chars. Captured for BOTH success and
//! error outcomes (an error's text is still useful context, and
//! `check_grounded` already excludes `is_error` evidence from grounding —
//! see `duduclaw_core::grounding::ToolEvidence::is_error`). Old rows and
//! old callers stay valid: `result_text`/`result_text_truncated` are
//! additive optional fields, same backward-compatibility shape as `input`.

use std::path::Path;
use std::sync::LazyLock;

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Severity level of a security event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// A single security audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: String,
    pub event_type: String,
    pub agent_id: String,
    pub severity: Severity,
    pub details: serde_json::Value,
}

impl AuditEvent {
    /// Create a new audit event with the current timestamp.
    pub fn new(
        event_type: impl Into<String>,
        agent_id: impl Into<String>,
        severity: Severity,
        details: serde_json::Value,
    ) -> Self {
        Self {
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.into(),
            agent_id: agent_id.into(),
            severity,
            details,
        }
    }
}

/// Append an audit event to the security log file.
///
/// The log is stored at `<home_dir>/security_audit.jsonl`.
/// This function is synchronous (blocking I/O) and suitable for
/// calling from both sync and async contexts via `spawn_blocking`.
pub fn append_audit_event(home_dir: &Path, event: &AuditEvent) {
    let path = home_dir.join("security_audit.jsonl");
    let json = match serde_json::to_string(event) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize audit event: {e}");
            return;
        }
    };

    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            // Use advisory file lock to prevent multi-process write corruption (MW-H2)
            if let Err(e) = duduclaw_core::platform::flock_exclusive(&f) {
                warn!("flock failed on audit log: {e}");
            }
            if let Err(e) = writeln!(f, "{json}") {
                warn!("Failed to write audit event: {e}");
            }
            // Lock automatically released when file is dropped
        }
        Err(e) => {
            warn!("Failed to open audit log {}: {e}", path.display());
        }
    }
}

/// Read recent audit events (last N entries).
///
/// Simplified: collect all lines, then slice the tail (MW-L2).
/// For very large files, consider using a reverse-line reader crate.
pub fn read_recent_events(home_dir: &Path, limit: usize) -> Vec<AuditEvent> {
    let path = home_dir.join("security_audit.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(limit);

    lines[start..]
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Count events by severity since a given timestamp.
///
/// Uses proper ISO 8601 DateTime parsing instead of string prefix
/// comparison to avoid incorrect ordering (MW-M3).
pub fn count_events_since(
    home_dir: &Path,
    since: &str,
) -> (usize, usize, usize) {
    let path = home_dir.join("security_audit.jsonl");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return (0, 0, 0),
    };

    let since_dt = chrono::DateTime::parse_from_rfc3339(since)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now() - chrono::Duration::hours(24));

    let mut info = 0usize;
    let mut warning = 0usize;
    let mut critical = 0usize;

    for line in content.lines() {
        if let Ok(event) = serde_json::from_str::<AuditEvent>(line) {
            let event_time = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .ok();
            if event_time.is_some_and(|t| t >= since_dt) {
                match event.severity {
                    Severity::Info => info += 1,
                    Severity::Warning => warning += 1,
                    Severity::Critical => critical += 1,
                }
            }
        }
    }

    (info, warning, critical)
}

// ── Convenience constructors for common events ──────────────

/// Log a SOUL.md drift detection event.
pub fn log_soul_drift(home_dir: &Path, agent_id: &str, expected: &str, actual: &str) {
    let event = AuditEvent::new(
        "soul_drift",
        agent_id,
        Severity::Critical,
        serde_json::json!({
            "expected_hash": expected,
            "actual_hash": actual,
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log a prompt injection detection event.
pub fn log_injection_detected(
    home_dir: &Path,
    agent_id: &str,
    risk_score: u32,
    matched_rules: &[String],
    blocked: bool,
) {
    let severity = if blocked {
        Severity::Critical
    } else {
        Severity::Warning
    };
    let event = AuditEvent::new(
        "prompt_injection",
        agent_id,
        severity,
        serde_json::json!({
            "risk_score": risk_score,
            "matched_rules": matched_rules,
            "blocked": blocked,
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log a CONTRACT.toml `must_not` violation that blocked an outgoing reply (P2-3).
pub fn log_contract_violation(home_dir: &Path, agent_id: &str, violated_rules: &[String]) {
    let event = AuditEvent::new(
        "contract_violation",
        agent_id,
        Severity::Critical,
        serde_json::json!({
            "violated_rules": violated_rules,
            "action": "reply_blocked",
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log that a spawn granted an agent-CLI subprocess access to one or more of
/// the operator's git/SSH/GPG identity env vars (WP-10A, 2026-08): per-agent
/// `agent.toml [capabilities] git_credentials = true` opt-in, layered on top
/// of the WP-8B env-scrub allowlist (`duduclaw_core::spawn_env`). Since this
/// hands the agent the operator's own push/signing identity, every spawn
/// that actually carries one of the four names
/// (`SSH_AUTH_SOCK`/`SSH_AGENT_PID`/`GPG_TTY`/`GNUPGHOME`) must be traceable
/// to which agent received it. `env_names` carries only the env var *names*
/// that were added — never values, matching every other audit record in
/// this module. Call sites are expected to skip this call entirely when
/// `env_names` is empty (nothing was granted, nothing to log).
pub fn log_git_credentials_granted(home_dir: &Path, agent_id: &str, env_names: &[&str]) {
    let event = AuditEvent::new(
        "git_credentials_env_granted",
        agent_id,
        Severity::Warning,
        serde_json::json!({
            "env_names": env_names,
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log a skill quarantine event.
pub fn log_skill_quarantined(home_dir: &Path, agent_id: &str, skill_name: &str, reason: &str) {
    let event = AuditEvent::new(
        "skill_quarantined",
        agent_id,
        Severity::Warning,
        serde_json::json!({
            "skill_name": skill_name,
            "reason": reason,
        }),
    );
    append_audit_event(home_dir, &event);
}

// ── Tool call audit trail ─────────────────────────────────────

/// Log a successful MCP tool call for post-action audit verification.
///
/// Written to `tool_calls.jsonl` (separate from security_audit.jsonl)
/// so the action claim verifier can cross-reference agent outputs.
pub fn append_tool_call(
    home_dir: &Path,
    agent_id: &str,
    tool_name: &str,
    params_summary: &str,
    success: bool,
) {
    append_tool_call_with_extras(home_dir, agent_id, tool_name, params_summary, success, &[])
}

/// Variant of [`append_tool_call`] that attaches additional fields to the
/// audit record. Used by `shared_wiki_write` to record `claimed_authors_in_content`
/// and `matches_caller` (RFC-22 Decision 4-D, Phase 3 W2) so post-hoc audit
/// can detect when an agent wrote a wiki page that *claims* multi-agent
/// authorship but only one caller actually invoked the tool — e.g. the
/// 5/5 trace where agnes wrote a "## DuDuClaw PM 觀點" section after the
/// pm spawn failed.
///
/// Extras are attached as top-level JSON fields. They MUST NOT collide with
/// the canonical fields (`timestamp`, `agent_id`, `tool_name`, `params_summary`,
/// `success`); when collision occurs the canonical field wins.
pub fn append_tool_call_with_extras(
    home_dir: &Path,
    agent_id: &str,
    tool_name: &str,
    params_summary: &str,
    success: bool,
    extras: &[(&str, serde_json::Value)],
) {
    const RESERVED: &[&str] = &[
        "timestamp",
        "agent_id",
        "tool_name",
        "params_summary",
        "success",
    ];
    let path = home_dir.join("tool_calls.jsonl");
    maybe_rotate_tool_calls(&path);
    let mut map = serde_json::Map::new();
    map.insert("timestamp".into(), Utc::now().to_rfc3339().into());
    map.insert("agent_id".into(), agent_id.into());
    map.insert("tool_name".into(), tool_name.into());
    map.insert("params_summary".into(), params_summary.into());
    map.insert("success".into(), success.into());
    for (key, value) in extras {
        if RESERVED.contains(key) {
            warn!(
                "tool_call extra field '{key}' collides with canonical name; ignored"
            );
            continue;
        }
        map.insert((*key).to_string(), value.clone());
    }
    let record = serde_json::Value::Object(map);
    let json = match serde_json::to_string(&record) {
        Ok(j) => j,
        Err(e) => {
            warn!("Failed to serialize tool call record: {e}");
            return;
        }
    };

    use std::io::Write;
    // 0600 on create: since result_text landed, rows can carry business
    // data (e.g. Odoo reads), so the file must not be world/group-readable.
    // mode() only applies at creation — pre-existing files keep their bits,
    // hence the explicit tighten below for upgraded installs.
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(&path) {
        Ok(mut f) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = f.metadata() {
                    if meta.permissions().mode() & 0o077 != 0 {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o600);
                        if let Err(e) = f.set_permissions(perms) {
                            warn!("failed to tighten tool_calls.jsonl to 0600: {e}");
                        }
                    }
                }
            }
            // Warn (not silently swallow) like the security_audit.jsonl
            // sibling path — a failed lock means concurrent writers may
            // interleave lines (2026-07 MED).
            if let Err(e) = duduclaw_core::platform::flock_exclusive(&f) {
                warn!("flock failed on tool_calls.jsonl: {e}");
            }
            if let Err(e) = writeln!(f, "{json}") {
                warn!("Failed to write tool call record: {e}");
            }
        }
        Err(e) => {
            warn!("Failed to open tool_calls.jsonl: {e}");
        }
    }
}

// ── R4: input capture (TraceElephant) ─────────────────────────

/// Cap on the serialized (masked) input stored per tool-call record.
/// Chars, not bytes — truncation is CJK-safe via `truncate_chars`.
pub const AUDIT_INPUT_MAX_CHARS: usize = 4096;

/// Maximum JSON nesting depth walked by the masker; deeper values are
/// replaced wholesale (defensive bound against pathological inputs).
const MASK_MAX_DEPTH: usize = 16;

/// Key names whose values are always masked (case-insensitive exact match
/// on the key, never substring — project convention 2).
const SENSITIVE_KEYS: &[&str] = &[
    "token",
    "access_token",
    "refresh_token",
    "id_token",
    "secret",
    "client_secret",
    "corpsecret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "authorization",
    "auth",
    "credential",
    "credentials",
    "cookie",
    "session_key",
    "private_key",
    "signing_key",
    "webhook_secret",
];

/// Value prefixes that mark a string as a credential regardless of its key
/// (well-known secret formats). Anchored `starts_with`, never substring.
const SENSITIVE_VALUE_PREFIXES: &[&str] = &[
    "sk-ant-",
    "sk-proj-",
    "xoxb-",
    "xoxp-",
    "xapp-",
    "ghp_",
    "gho_",
    "github_pat_",
    "AKIA",
    "Bearer ",
    "Basic ",
    "Token ",
    "glpat-",
];

fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k))
}

fn is_sensitive_value(v: &str) -> bool {
    SENSITIVE_VALUE_PREFIXES.iter().any(|p| v.starts_with(p))
}

/// Build a regex alternation of [`SENSITIVE_KEYS`], tolerating `-`/`_`
/// interchange: header spellings like `x-api-key` / `Api-Key` must match the
/// `api_key` entry even though the constant list only spells it with an
/// underscore (2026-08 H2/H3 review PoC: `x-api-key: sk-live-abc123` was
/// previously unmasked).
fn sensitive_keys_pattern() -> String {
    SENSITIVE_KEYS
        .iter()
        .map(|k| regex::escape(k).replace('_', "[-_]"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Recursively mask secret-looking values inside a JSON tree. Returns a new
/// value (never mutates the input). Masking happens **before** the size cap
/// so a truncated record can never end mid-secret.
pub fn mask_sensitive_json(v: &serde_json::Value) -> serde_json::Value {
    mask_at_depth(v, 0)
}

fn mask_at_depth(v: &serde_json::Value, depth: usize) -> serde_json::Value {
    if depth > MASK_MAX_DEPTH {
        return serde_json::Value::String("***depth-capped***".into());
    }
    match v {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, val) in map {
                if is_sensitive_key(k) {
                    out.insert(k.clone(), serde_json::Value::String("***".into()));
                } else {
                    out.insert(k.clone(), mask_at_depth(val, depth + 1));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items.iter().map(|i| mask_at_depth(i, depth + 1)).collect(),
        ),
        serde_json::Value::String(s) if is_sensitive_value(s) => {
            // Keep a short, CJK-safe prefix for correlation, mask the rest.
            let head = duduclaw_core::truncate_chars(s, 8);
            serde_json::Value::String(format!("{head}***"))
        }
        other => other.clone(),
    }
}

// ── B3b: tool RESULT text capture (GroundEval evidence source) ────────────
//
// `mask_sensitive_json` above walks a structured JSON *input* tree by key.
// A tool's RESULT is free text — sometimes a serialized JSON object dumped
// into the MCP `text` block (many handlers `serde_json::to_string(_pretty)`
// a struct), sometimes prose, sometimes both — so it has no key tree to
// walk. These three patterns scan the rendered text for secret-shaped
// substrings instead, most-specific-first so an already-masked span is
// never re-matched by a looser later pass.

/// Cap on the serialized (masked) tool RESULT text stored per tool-call
/// record. Chars, not bytes — CJK-safe via `truncate_chars`. Deliberately
/// smaller than [`AUDIT_INPUT_MAX_CHARS`]: this exists purely as B3
/// grounding-precheck evidence (GroundEval, arXiv:2606.22737) — a
/// contiguous-run overlap check needs the tool's key wording, not its full
/// payload.
pub const AUDIT_RESULT_TEXT_MAX_CHARS: usize = 2000;

/// Matches `"<KEY>":"<value>"` (optionally spaced) as produced when a tool
/// result embeds a JSON object as its text. Case-insensitive on the key
/// against [`SENSITIVE_KEYS`]; the value group tolerates escaped quotes.
static SENSITIVE_TEXT_JSON_KV_RE: LazyLock<Regex> = LazyLock::new(|| {
    let keys = sensitive_keys_pattern();
    Regex::new(&format!(r#"(?i)"({keys})"\s*:\s*"((?:[^"\\]|\\.)*)""#))
        .expect("static SENSITIVE_TEXT_JSON_KV_RE must compile")
});

/// Matches plain `<key>: <value>` / `<key>=<value>` text (not JSON-quoted),
/// e.g. a handler's narrated summary line, and consumes the **rest of the
/// line** as the value (H2 review PoC: `password: correct horse battery
/// staple` previously only masked the first whitespace-delimited token,
/// leaving the rest of the passphrase in the clear — over-masking the whole
/// line is the accepted tradeoff, project security convention 4).
///
/// The left boundary is deliberately NOT `\b`: Rust's `regex` crate treats
/// CJK ideographs as Unicode word characters, so `\b` never fires at a
/// CJK↔ASCII seam (H3 PoC: `密碼password: hunter2` has no boundary between
/// `碼` and `p`, so the old `\bpassword\b` never matched). Instead the left
/// side is a capturing group that matches start-of-string or any non-ASCII
/// -word-char — including CJK — and the replacement re-emits that captured
/// character so nothing is eaten from the surrounding text. The right side
/// keeps `\b` (only ASCII suffix collisions like `mypassword`/`token_count`
/// are a concern there, and Unicode `\b` still filters those correctly).
static SENSITIVE_TEXT_PLAIN_KV_RE: LazyLock<Regex> = LazyLock::new(|| {
    let keys = sensitive_keys_pattern();
    Regex::new(&format!(r#"(?i)(^|[^A-Za-z0-9_])({keys})\b\s*[:=]\s*([^\n]+)"#))
        .expect("static SENSITIVE_TEXT_PLAIN_KV_RE must compile")
});

/// Matches a bare [`SENSITIVE_VALUE_PREFIXES`] token wherever it appears in
/// free text (unlike [`is_sensitive_value`], not anchored to a whole-string
/// match) — catches a leaked credential pasted mid-sentence into a tool's
/// narrated result. Case-insensitive (H2 PoC: `bearer eyJ...` lowercase was
/// previously unmatched since the prefix list only spells `Bearer `).
static SENSITIVE_TEXT_VALUE_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    let alts: Vec<String> = SENSITIVE_VALUE_PREFIXES
        .iter()
        .map(|p| regex::escape(p))
        .collect();
    Regex::new(&format!(r#"(?i)(?:{})[^\s"')\]}}]*"#, alts.join("|")))
        .expect("static SENSITIVE_TEXT_VALUE_TOKEN_RE must compile")
});

/// Matches a `<scheme>://<user>:<password>@` connection-string credential
/// segment (postgres/mongodb/redis/amqp/mysql/…) and masks only the password
/// half, keeping `scheme://user:` and the trailing `@` intact for context
/// (H3 PoC: `postgres://admin:Sup3rS3cret@db.internal:5432/prod`).
static SENSITIVE_TEXT_CONN_STRING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)([a-z][a-z0-9+.-]*://[^/\s:@]+:)[^@\s]+(@)"#)
        .expect("static SENSITIVE_TEXT_CONN_STRING_RE must compile")
});

/// Matches a Telegram Bot API URL path segment (`/bot<id>:<token>`), masking
/// only the token half (H3 PoC:
/// `https://api.telegram.org/bot7123456789:AAH9xQabc/sendMessage`).
static SENSITIVE_TEXT_TELEGRAM_BOT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(/bot\d+:)[A-Za-z0-9_-]+"#)
        .expect("static SENSITIVE_TEXT_TELEGRAM_BOT_RE must compile")
});

/// Matches a Slack Incoming Webhook URL, masking the secret path segment
/// after `services/` (H3: `hooks.slack.com/services/...` leaks the webhook
/// credential in the URL path itself).
static SENSITIVE_TEXT_SLACK_WEBHOOK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(hooks\.slack\.com/services/)[A-Za-z0-9/]+"#)
        .expect("static SENSITIVE_TEXT_SLACK_WEBHOOK_RE must compile")
});

/// Best-effort masking for free-text tool **result** output (as opposed to
/// [`mask_sensitive_json`], which walks a structured JSON *input* tree).
/// Six passes, in this specific order: JSON key/value, bare
/// credential-prefixed tokens, connection-string passwords, Telegram bot
/// URLs, Slack webhook URLs, then plain `key: value`.
///
/// The prefix pass deliberately runs BEFORE the plain-kv pass, not after:
/// [`SENSITIVE_VALUE_PREFIXES`] includes multi-word prefixes like
/// `"Bearer "`, so a value like `authorization: Bearer xyz123` must be
/// caught by the prefix pass's whitespace-tolerant match on `Bearer xyz123`
/// as a whole. The plain-kv pass now consumes the rest of the line as its
/// value (H2 fix — see its own doc comment), so running it first would no
/// longer strand an orphaned token either way, but the prefix/conn-string/
/// URL passes stay first on principle: they identify a credential by its
/// *shape*, independent of any recognized key name, so they should get the
/// first look at the raw text.
///
/// Bias: over-masking is acceptable, under-masking is not (project security
/// convention 4 — gates fail closed). This is a heuristic scan, not a
/// guarantee — it shares the same key/prefix allowlists as
/// [`mask_sensitive_json`] (now `-`/`_`-tolerant, see [`sensitive_keys_pattern`]).
pub fn mask_sensitive_text(text: &str) -> String {
    let masked = SENSITIVE_TEXT_JSON_KV_RE.replace_all(text, |caps: &regex::Captures| {
        format!("\"{}\":\"***\"", &caps[1])
    });
    let masked = SENSITIVE_TEXT_VALUE_TOKEN_RE.replace_all(&masked, "***");
    let masked = SENSITIVE_TEXT_CONN_STRING_RE.replace_all(&masked, "${1}***${2}");
    let masked = SENSITIVE_TEXT_TELEGRAM_BOT_RE.replace_all(&masked, "${1}***");
    let masked = SENSITIVE_TEXT_SLACK_WEBHOOK_RE.replace_all(&masked, "${1}***");
    let masked = SENSITIVE_TEXT_PLAIN_KV_RE.replace_all(&masked, |caps: &regex::Captures| {
        format!("{}{}: ***", &caps[1], &caps[2])
    });
    masked.into_owned()
}

/// Read-only verb tokens: a tool name whose `_`-split tokens include one of
/// these is treated as read-only and its input is **not** captured (it left
/// no state change to reconstruct). Token equality, never substring —
/// `tasks_list` matches via its `list` token, `enlist_agent` does not.
/// Conservative bias: unknown names count as state-changing (capture more,
/// masked — audit completeness wins).
const READONLY_VERB_TOKENS: &[&str] = &[
    "list", "get", "read", "search", "status", "stats", "ls", "info", "recent", "summary",
];

/// `true` when every heuristic agrees the tool only reads state.
pub fn is_readonly_tool_name(name: &str) -> bool {
    name.split('_')
        .any(|tok| READONLY_VERB_TOKENS.iter().any(|v| tok.eq_ignore_ascii_case(v)))
}

/// Variant of [`append_tool_call`] that additionally captures the tool's
/// **input arguments** and **result text** (R4 — record full inputs, not
/// just outcomes; B3b — record output text too, so the B3 grounding
/// pre-check in `dispatch_engine.rs` has evidence to compare a claim
/// against instead of perpetually observing `ResultTextMissing`).
///
/// Behavior:
/// - `input = None` or a read-only tool name ⇒ no `input`/`input_truncated`
///   fields (same skip rule as before B3b).
/// - Otherwise the record gains `input` (masked via [`mask_sensitive_json`],
///   serialized, capped at [`AUDIT_INPUT_MAX_CHARS`] chars) and
///   `input_truncated: bool`.
/// - `result_text = None` or an empty/all-whitespace string ⇒ no
///   `result_text`/`result_text_truncated` fields. Otherwise the record
///   gains `result_text` (masked via [`mask_sensitive_text`], capped at
///   [`AUDIT_RESULT_TEXT_MAX_CHARS`] chars) and, only when truncation
///   actually happened, `result_text_truncated: true`. Captured
///   unconditionally on both success AND error (an error tool's text is
///   still useful context — `check_grounded` already excludes
///   `is_error` evidence from grounding, so this never lets a failed call
///   masquerade as supporting evidence).
///
/// Old consumers keep working: every field here is additive and optional.
pub fn append_tool_call_with_input(
    home_dir: &Path,
    agent_id: &str,
    tool_name: &str,
    params_summary: &str,
    success: bool,
    input: Option<&serde_json::Value>,
    result_text: Option<&str>,
) {
    let mut extras: Vec<(&str, serde_json::Value)> = Vec::new();
    if let Some(raw) = input {
        if !is_readonly_tool_name(tool_name) {
            let masked = mask_sensitive_json(raw);
            let serialized = masked.to_string();
            let truncated = serialized.chars().count() > AUDIT_INPUT_MAX_CHARS;
            let rendered = if truncated {
                duduclaw_core::truncate_chars(&serialized, AUDIT_INPUT_MAX_CHARS)
            } else {
                serialized
            };
            extras.push(("input", serde_json::Value::String(rendered)));
            extras.push(("input_truncated", serde_json::Value::Bool(truncated)));
        }
    }
    if let Some(raw_result) = result_text {
        let masked = mask_sensitive_text(raw_result);
        if !masked.trim().is_empty() {
            let truncated = masked.chars().count() > AUDIT_RESULT_TEXT_MAX_CHARS;
            let rendered = if truncated {
                duduclaw_core::truncate_chars(&masked, AUDIT_RESULT_TEXT_MAX_CHARS)
            } else {
                masked
            };
            extras.push(("result_text", serde_json::Value::String(rendered)));
            if truncated {
                extras.push(("result_text_truncated", serde_json::Value::Bool(true)));
            }
        }
    }
    append_tool_call_with_extras(home_dir, agent_id, tool_name, params_summary, success, &extras)
}

/// Log an MCP dispatch-gate REJECTION (WP-H2 §1.3, Gap (c)).
///
/// Every guard in `McpDispatcher::dispatch_tool_call` (scope check, the
/// `denied_tools`/`allowed_tools` capability gate, the PORTICO task-scoped
/// grant gate, …) previously returned a JSON-RPC error straight to the
/// caller without ever writing to `tool_calls.jsonl` — "the tool ran" was
/// recorded, but "the tool was BLOCKED" left no trace at all. That is the
/// same failure-leaves-no-trace defect class this project has hit before
/// (OTP silent-fail, the WP-A10 BUG-1 attribution gap): evidence consumers
/// that read this file (`recent_actions.rs`'s self-action feed, the
/// dashboard's change tab, the goal-loop MAV judge's audit digest) could
/// never see a blocked attempt, so an agent asked "did you try X?" would
/// truthfully-but-misleadingly say "no" about a call it DID attempt and was
/// denied.
///
/// Always written (never gated by `is_state_changing` — a rejected call
/// changed nothing, but the attempt itself is exactly the kind of self-action
/// this audit trail exists to answer questions about).
///
/// `error_class` is a short, machine-stable token — e.g. `"insufficient_scope"`,
/// `"capability_grant_missing"`, `"denied_tools"`, `"allowed_tools"` — chosen
/// to read consistently with the credential-resolution line's
/// `describe().last_resolve.error_class` field (commercial/docs/
/// DESIGN-credentials-doctrine-2026-08.md §4.3's note that both lines should
/// share one field-naming convention for "failures must leave a trace").
///
/// `input`, when provided, is masked via [`mask_sensitive_json`] and capped
/// the same way [`append_tool_call_with_input`] caps a successful call's
/// input — unlike that function, this one does NOT skip capture for
/// read-only tool names: visibility into *why a call was blocked* matters
/// regardless of whether the tool itself would have mutated state.
pub fn append_tool_call_denied(
    home_dir: &Path,
    agent_id: &str,
    tool_name: &str,
    error_class: &str,
    detail: &str,
    input: Option<&serde_json::Value>,
) {
    let mut extras: Vec<(&str, serde_json::Value)> =
        vec![("error_class", serde_json::Value::String(error_class.to_string()))];
    if let Some(raw) = input {
        let masked = mask_sensitive_json(raw);
        let serialized = masked.to_string();
        let truncated = serialized.chars().count() > AUDIT_INPUT_MAX_CHARS;
        let rendered = if truncated {
            duduclaw_core::truncate_chars(&serialized, AUDIT_INPUT_MAX_CHARS)
        } else {
            serialized
        };
        extras.push(("input", serde_json::Value::String(rendered)));
        extras.push(("input_truncated", serde_json::Value::Bool(truncated)));
    }
    let params_summary = format!("denied ({error_class}): {}", duduclaw_core::truncate_chars(detail, 200));
    append_tool_call_with_extras(home_dir, agent_id, tool_name, &params_summary, false, &extras);
}

/// Read tool call records for a specific agent within a time window.
///
/// Uses `flock(LOCK_SH)` to prevent reading partially-written lines
/// while `append_tool_call()` holds `LOCK_EX`.
pub fn read_tool_calls_since(
    home_dir: &Path,
    agent_id: &str,
    since: &str,
) -> Vec<serde_json::Value> {
    let path = home_dir.join("tool_calls.jsonl");
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    // Acquire shared lock to prevent reading during a concurrent write
    let _ = duduclaw_core::platform::flock_shared(&file);

    use std::io::Read;
    let mut content = String::new();
    let mut reader = std::io::BufReader::new(file);
    if reader.read_to_string(&mut content).is_err() {
        return Vec::new();
    }

    // Fallback to 0 seconds ago (empty window) if `since` is unparseable,
    // to avoid accidentally including old records.
    // Apply 2-second grace period to handle clock precision issues between
    // the dispatcher recording dispatch_start and the MCP server recording
    // tool call timestamps (review round 2).
    let since_dt = chrono::DateTime::parse_from_rfc3339(since)
        .map(|dt| dt.with_timezone(&chrono::Utc) - chrono::Duration::seconds(2))
        .unwrap_or_else(|_| chrono::Utc::now());

    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|record| {
            let matches_agent = record
                .get("agent_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == agent_id);
            let after_since = record
                .get("timestamp")
                .and_then(|v| v.as_str())
                .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                .is_some_and(|dt| dt.with_timezone(&chrono::Utc) >= since_dt);
            matches_agent && after_since
        })
        .collect()
}

/// Rotation threshold for `tool_calls.jsonl`, in bytes.
///
/// Raised from 5 MB to 16 MB (2026-08 M4 review): B3b's `result_text`
/// capture added a second capped free-text field to every record, pushing
/// average record size up roughly 50-100% versus the input-only design this
/// cap was originally sized for — the old 5 MB threshold was rotating (and
/// thus losing) the audit trail well ahead of the original retention
/// intent. 16 MB restores a comparable retention window; per-record size is
/// still independently bounded by [`AUDIT_INPUT_MAX_CHARS`] and
/// [`AUDIT_RESULT_TEXT_MAX_CHARS`], so this only affects *how many* records
/// accumulate before rotation, not how large any single one can get.
pub const TOOL_CALLS_ROTATION_MAX_BYTES: u64 = 16 * 1024 * 1024; // 16 MB

/// Rotate `tool_calls.jsonl` if it exceeds [`TOOL_CALLS_ROTATION_MAX_BYTES`].
///
/// Renames the current file to `.jsonl.old` (overwriting any previous backup)
/// and starts a fresh file. Only checks file size every 64 calls to avoid
/// a `metadata()` syscall on every tool call (review R3-L1).
/// Concurrent callers may both attempt `rename` — the loser gets ENOENT
/// which is silently ignored since a fresh file will be created on the
/// next `append` (review R3-L4).
fn maybe_rotate_tool_calls(path: &std::path::Path) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static CALL_COUNT: AtomicU32 = AtomicU32::new(0);

    // Check every 64 calls (~1 metadata syscall per 64 tool invocations)
    if !CALL_COUNT.fetch_add(1, Ordering::Relaxed).is_multiple_of(64) {
        return;
    }

    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > TOOL_CALLS_ROTATION_MAX_BYTES {
            let backup = path.with_extension("jsonl.old");
            // Ignore ENOENT: a concurrent caller may have already rotated.
            match std::fs::rename(path, &backup) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!("Failed to rotate tool_calls.jsonl: {e}"),
            }
        }
}

/// Log a tool hallucination detection event.
pub fn log_tool_hallucination(
    home_dir: &Path,
    agent_id: &str,
    claimed_action: &str,
    expected_tool: &str,
) {
    let event = AuditEvent::new(
        "tool_hallucination",
        agent_id,
        Severity::Critical,
        serde_json::json!({
            "claimed_action": claimed_action,
            "expected_tool": expected_tool,
            "explanation": "Agent claimed to perform an action without calling the corresponding MCP tool",
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log an OS ground-truth reconciliation discrepancy (P3-3).
///
/// `unaccounted_count` = observed OS events (writes outside the workspace roots
/// or outbound connections) with no tool call to explain them — a possible
/// sandbox escape / hidden side effect. `missing_count` = successful tool calls
/// that claimed a footprint-leaving effect yet left no observed footprint — a
/// possible false success. Always Critical: any discrepancy is worth forensic
/// attention.
pub fn log_os_discrepancy(
    home_dir: &Path,
    agent_id: &str,
    unaccounted_count: usize,
    missing_count: usize,
) {
    let event = AuditEvent::new(
        "os_reconcile_discrepancy",
        agent_id,
        Severity::Critical,
        serde_json::json!({
            "unaccounted_count": unaccounted_count,
            "missing_count": missing_count,
            "explanation": "OS ground-truth reconciliation found agent side effects \
                            with no matching tool call (unaccounted) and/or tool calls \
                            with no matching OS footprint (missing)",
        }),
    );
    append_audit_event(home_dir, &event);
}

// ── Killswitch / Safety Filter audit events ───────────────────

/// Log a safety word trigger event.
pub fn log_safety_word(
    home_dir: &Path,
    agent_id: &str,
    scope: &str,
    user_id: &str,
    action: &str,
) {
    let event = AuditEvent::new(
        "safety_word_triggered",
        agent_id,
        Severity::Critical,
        serde_json::json!({
            "scope": scope,
            "user_id": user_id,
            "action": action,
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log a circuit breaker trip event.
pub fn log_circuit_breaker_trip(
    home_dir: &Path,
    agent_id: &str,
    scope: &str,
    reason: &str,
) {
    let event = AuditEvent::new(
        "circuit_breaker_tripped",
        agent_id,
        Severity::Warning,
        serde_json::json!({
            "scope": scope,
            "reason": reason,
        }),
    );
    append_audit_event(home_dir, &event);
}

/// Log a failsafe level change event.
pub fn log_failsafe_change(
    home_dir: &Path,
    agent_id: &str,
    scope: &str,
    from_level: &str,
    to_level: &str,
    reason: &str,
) {
    let severity = if to_level.contains("L4") || to_level.contains("L3") {
        Severity::Critical
    } else {
        Severity::Warning
    };
    let event = AuditEvent::new(
        "failsafe_level_changed",
        agent_id,
        severity,
        serde_json::json!({
            "scope": scope,
            "from": from_level,
            "to": to_level,
            "reason": reason,
        }),
    );
    append_audit_event(home_dir, &event);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_home() -> std::path::PathBuf {
        // No uuid dep in this crate — pid + monotonic counter + nanos is
        // unique enough for a test scratch dir.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "dudu-audit-{}-{}-{nanos}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn read_last_record(home: &std::path::Path) -> serde_json::Value {
        let body = std::fs::read_to_string(home.join("tool_calls.jsonl")).unwrap();
        serde_json::from_str(body.lines().last().unwrap()).unwrap()
    }

    // ── Masking ─────────────────────────────────────────

    #[test]
    fn mask_replaces_sensitive_keys_recursively() {
        let v = serde_json::json!({
            "title": "deploy",
            "api_key": "sk-live-abcdef",
            "nested": { "PASSWORD": "hunter2", "note": "ok" },
            "list": [ { "client_secret": "s3cr3t" } ],
        });
        let m = mask_sensitive_json(&v);
        assert_eq!(m["title"], "deploy");
        assert_eq!(m["api_key"], "***");
        assert_eq!(m["nested"]["PASSWORD"], "***", "case-insensitive key match");
        assert_eq!(m["nested"]["note"], "ok");
        assert_eq!(m["list"][0]["client_secret"], "***");
    }

    #[test]
    fn mask_detects_secret_value_prefixes() {
        let v = serde_json::json!({
            "content": "sk-ant-api03-verylongsecrettoken",
            "header": "Bearer eyJhbGciOi...",
            "plain": "sk8er boy", // no match — anchored prefix only
        });
        let m = mask_sensitive_json(&v);
        let c = m["content"].as_str().unwrap();
        assert!(c.ends_with("***") && !c.contains("verylongsecrettoken"));
        assert!(m["header"].as_str().unwrap().ends_with("***"));
        assert_eq!(m["plain"], "sk8er boy");
    }

    #[test]
    fn mask_key_match_is_exact_not_substring() {
        // `token_count` must NOT be masked (only exact key `token` is).
        let v = serde_json::json!({ "token_count": 42, "token": "abc" });
        let m = mask_sensitive_json(&v);
        assert_eq!(m["token_count"], 42);
        assert_eq!(m["token"], "***");
    }

    #[test]
    fn mask_depth_cap_never_recurses_forever() {
        let mut v = serde_json::json!("leaf");
        for _ in 0..40 {
            v = serde_json::json!({ "inner": v });
        }
        let m = mask_sensitive_json(&v);
        assert!(m.to_string().contains("depth-capped"));
    }

    #[test]
    fn mask_is_cjk_safe_on_prefixed_values() {
        // Multi-byte content behind a secret prefix must not panic on the
        // 8-char correlation head.
        let v = serde_json::json!({ "content": "Bearer 憑證繁體中文金鑰內容" });
        let m = mask_sensitive_json(&v);
        assert!(m["content"].as_str().unwrap().ends_with("***"));
    }

    // ── B3b: result-text masking ─────────────────────────

    #[test]
    fn mask_text_masks_json_kv_pairs_embedded_in_free_text() {
        let text = r#"Connected. {"api_key":"sk-live-abcdef","status":"ok"}"#;
        let masked = mask_sensitive_text(text);
        assert!(masked.contains(r#""api_key":"***""#));
        assert!(!masked.contains("sk-live-abcdef"));
        assert!(masked.contains(r#""status":"ok""#), "non-secret key untouched");
    }

    #[test]
    fn mask_text_masks_plain_key_value_lines() {
        let text = "Login succeeded.\ntoken: abc.def.ghi\nuser: agnes";
        let masked = mask_sensitive_text(text);
        assert!(masked.contains("token: ***"));
        assert!(!masked.contains("abc.def.ghi"));
        assert!(masked.contains("user: agnes"), "non-secret key untouched");
    }

    #[test]
    fn mask_text_masks_bare_value_prefix_tokens_mid_sentence() {
        let text = "here is the token you asked for: sk-ant-api03-verylongsecrettoken, keep it safe";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("verylongsecrettoken"));
        assert!(masked.contains("***"));
    }

    #[test]
    fn mask_text_key_match_is_word_boundary_not_substring() {
        // `author` must survive even though it contains the letters of `auth`.
        let text = "author: agnes, authorization: Bearer xyz123";
        let masked = mask_sensitive_text(text);
        assert!(masked.contains("author: agnes"), "author must not be masked");
        assert!(!masked.contains("xyz123"));
    }

    #[test]
    fn mask_text_is_cjk_safe() {
        let text = "查詢結果：password: 憑證繁體中文密碼內容 完成";
        let masked = mask_sensitive_text(text); // must not panic on multi-byte content
        assert!(masked.contains("password: ***"));
        assert!(!masked.contains("憑證繁體中文密碼內容"));
    }

    #[test]
    fn mask_text_leaves_plain_prose_untouched() {
        let text = "Refund policy: 30 days from purchase, receipt required.";
        assert_eq!(mask_sensitive_text(text), text);
    }

    // ── 2026-08 H2/H3 review PoCs ─────────────────────────

    #[test]
    fn mask_text_masks_basic_auth_header() {
        // H2 PoC: SENSITIVE_VALUE_PREFIXES previously had no "Basic "/"Token "
        // entries, so an Authorization: Basic header leaked the base64
        // credential in full.
        let text = "Authorization: Basic YWRtaW46c3VwZXJzZWNyZXQ=";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("YWRtaW46c3VwZXJzZWNyZXQ="));
        assert!(masked.contains("***"));
    }

    #[test]
    fn mask_text_masks_lowercase_bearer_token() {
        // H2 PoC: SENSITIVE_TEXT_VALUE_TOKEN_RE had no `(?i)` flag, so a
        // lowercase `bearer` prefix (as some clients/log lines render it)
        // sailed through unmasked.
        let text = "authorization: bearer eyJhbGciOiJIUzI1NiJ9.xxx.yyy";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("eyJhbGciOiJIUzI1NiJ9.xxx.yyy"));
        assert!(masked.contains("***"));
    }

    #[test]
    fn mask_text_masks_full_multiword_passphrase() {
        // H2 PoC: the plain-kv pass previously only consumed the first
        // whitespace-delimited token, leaving the rest of a multi-word
        // secret (e.g. a diceware passphrase) in the clear.
        let text = "password: correct horse battery staple";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("correct horse battery staple"));
        assert!(!masked.contains("horse"), "must not leak any word of the passphrase");
        assert!(masked.contains("password: ***"));
    }

    #[test]
    fn mask_text_masks_connection_string_password() {
        // H3 PoC: DB/queue connection-string passwords (postgres, mongodb,
        // redis, amqp, ...) were not recognized as a secret shape at all.
        let text = "postgres://admin:Sup3rS3cret@db.internal:5432/prod";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("Sup3rS3cret"));
        assert!(masked.contains("postgres://admin:***@db.internal:5432/prod"));
    }

    #[test]
    fn mask_text_masks_telegram_bot_url_token() {
        // H3 PoC: a Telegram Bot API URL embeds the bot token directly in
        // the path — no recognized key name, no recognized value prefix.
        let text = "https://api.telegram.org/bot7123456789:AAH9xQabc/sendMessage";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("AAH9xQabc"));
        assert!(masked.contains("/bot7123456789:***/sendMessage"));
    }

    #[test]
    fn mask_text_masks_slack_webhook_path() {
        // H3: Slack incoming-webhook URLs carry the credential in the URL
        // path itself.
        let text = "posting to https://hooks.slack.com/services/T00/B00/XXXXXXXXXXXXXXXXXXXXXXXX ok";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("T00/B00/XXXXXXXXXXXXXXXXXXXXXXXX"));
        assert!(masked.contains("hooks.slack.com/services/***"));
    }

    #[test]
    fn mask_text_masks_hyphenated_key_spelling() {
        // H3 PoC: SENSITIVE_KEYS spells this "api_key" (underscore); a
        // hyphenated header spelling like `x-api-key` previously escaped
        // both the JSON-kv and plain-kv passes entirely.
        let text = "x-api-key: sk-live-abc123";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("sk-live-abc123"));
        assert!(masked.contains("x-api-key: ***"));
    }

    #[test]
    fn mask_text_masks_key_immediately_after_cjk_text() {
        // H3 PoC: Rust's `regex` crate treats CJK ideographs as Unicode
        // word characters, so a plain `\b`-anchored key regex never fires
        // at a CJK↔ASCII seam — `密碼password: hunter2` previously left the
        // secret fully exposed because `\bpassword\b` never matched.
        let text = "使用者密碼password: hunter2secret";
        let masked = mask_sensitive_text(text);
        assert!(!masked.contains("hunter2secret"));
        assert!(masked.contains("password: ***"));
        assert!(masked.contains("使用者密碼"), "CJK prefix text must survive untouched");
    }

    // ── B3b: result-text capture records ─────────────────

    #[test]
    fn result_text_captured_and_masked_for_success() {
        let home = fresh_home();
        append_tool_call_with_input(
            &home,
            "agnes",
            "memory_search",
            "ok",
            true,
            None,
            Some(r#"Refund policy: 30 days. {"api_key":"sk-live-xyz"}"#),
        );
        let rec = read_last_record(&home);
        let stored = rec["result_text"].as_str().unwrap();
        assert!(stored.contains("Refund policy: 30 days"));
        assert!(!stored.contains("sk-live-xyz"), "secret in result must be masked");
        assert!(rec.get("result_text_truncated").is_none(), "no marker when untruncated");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn result_text_captured_for_error_outcome_too() {
        let home = fresh_home();
        append_tool_call_with_input(
            &home,
            "agnes",
            "odoo_partner_search",
            "ok",
            false,
            None,
            Some("Odoo error: permission denied"),
        );
        let rec = read_last_record(&home);
        assert_eq!(rec["success"], false);
        assert_eq!(rec["result_text"], "Odoo error: permission denied");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn result_text_skipped_when_none_or_empty() {
        let home = fresh_home();
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, None, None);
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, None, Some("   "));
        let body = std::fs::read_to_string(home.join("tool_calls.jsonl")).unwrap();
        for line in body.lines() {
            let rec: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(rec.get("result_text").is_none(), "no result_text expected: {line}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn oversized_result_text_truncated_with_marker_cjk_safe() {
        let home = fresh_home();
        let big = "繁體中文結果".repeat(1000); // > AUDIT_RESULT_TEXT_MAX_CHARS
        append_tool_call_with_input(&home, "agnes", "memory_search", "ok", true, None, Some(&big));
        let rec = read_last_record(&home);
        assert_eq!(rec["result_text_truncated"], true);
        assert!(
            rec["result_text"].as_str().unwrap().chars().count() <= AUDIT_RESULT_TEXT_MAX_CHARS
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Gate-activation check (B3b task requirement #3): before this change,
    /// every `tool_calls.jsonl` row lacked `result_text`, so
    /// `duduclaw_core::grounding::check_grounded` could only ever observe
    /// `ResultTextMissing` for production evidence — the B3 pre-check in
    /// `dispatch_engine.rs` was permanently degraded/inert. This test drives
    /// the real writer (`append_tool_call_with_input`) then feeds the
    /// persisted row straight into the shared `duduclaw-core` grounding
    /// primitive (the same one `dispatch_engine::grounding_precheck` wraps)
    /// to prove the evidence source is now live, without needing to touch
    /// `dispatch_engine.rs` itself.
    #[test]
    fn result_text_activates_grounding_evidence_end_to_end() {
        use duduclaw_core::grounding::{check_grounded, GroundingOutcome, ToolEvidence};

        let home = fresh_home();
        append_tool_call_with_input(
            &home,
            "agnes",
            "memory_search",
            "ok",
            true,
            None,
            Some("Refund policy: 30 days from purchase, receipt required."),
        );
        let rec = read_last_record(&home);

        let evidence = vec![ToolEvidence {
            tool_name: rec["tool_name"].as_str().unwrap().to_string(),
            result_text: rec["result_text"].as_str().map(String::from),
            // Fix-2 C1b added this field to the shared struct; unset here
            // keeps this pre-existing audit.rs test's behavior unchanged
            // (mechanical fixup, no audit.rs production logic touched).
            input_text: None,
            is_error: !rec["success"].as_bool().unwrap(),
        }];

        // Claim overlaps the tool evidence → Grounded, not ResultTextMissing.
        let grounded = check_grounded(
            "Refund policy: 30 days from purchase, receipt required.",
            &evidence,
            Some("memory_search"),
            12,
        );
        assert_eq!(
            grounded,
            GroundingOutcome::Grounded {
                tool_name: "memory_search".to_string()
            }
        );

        // Claim does NOT overlap the same evidence → NotGrounded (still not
        // ResultTextMissing — the gate genuinely evaluates now).
        let not_grounded = check_grounded(
            "I handled the request successfully.",
            &evidence,
            Some("memory_search"),
            12,
        );
        assert_eq!(not_grounded, GroundingOutcome::NotGrounded);
        let _ = std::fs::remove_dir_all(&home);
    }

    // ── File permissions ──────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn tool_calls_jsonl_is_created_and_kept_at_0600() {
        use std::os::unix::fs::PermissionsExt;

        let home = fresh_home();
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, None, None);
        let path = home.join("tool_calls.jsonl");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "fresh file must be 0600, got {mode:#o}");

        // Upgraded installs: a pre-existing world-readable file gets
        // tightened on the next append, not left as-is.
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, None, None);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "existing file must be tightened, got {mode:#o}");
        let _ = std::fs::remove_dir_all(&home);
    }

    // ── M4: rotation cap ──────────────────────────────────

    #[test]
    fn rotation_cap_raised_to_16mb() {
        // M4 review: result_text (B3b) roughly doubled average record size
        // under the old 5 MB cap, over-shortening retention. Locks in the
        // raised cap so a future edit can't silently shrink it back down.
        assert_eq!(TOOL_CALLS_ROTATION_MAX_BYTES, 16 * 1024 * 1024);
    }

    // ── Read-only heuristic ─────────────────────────────

    #[test]
    fn readonly_tool_names_by_verb_token() {
        assert!(is_readonly_tool_name("tasks_list"));
        assert!(is_readonly_tool_name("memory_search"));
        assert!(is_readonly_tool_name("shared_wiki_read"));
        assert!(is_readonly_tool_name("cost_summary"));
        assert!(is_readonly_tool_name("inference_status"));
        // State-changing (and unknown-verb) names capture input.
        assert!(!is_readonly_tool_name("agent_update_soul"));
        assert!(!is_readonly_tool_name("tasks_create"));
        assert!(!is_readonly_tool_name("shared_wiki_write"));
        assert!(!is_readonly_tool_name("totally_new_tool"));
        // Token equality, not substring: `enlist` ≠ `list`.
        assert!(!is_readonly_tool_name("enlist_agent"));
    }

    // ── Input capture records ───────────────────────────

    #[test]
    fn input_captured_masked_for_state_changing_tool() {
        let home = fresh_home();
        let input = serde_json::json!({ "title": "發布", "api_key": "sk-live-xyz" });
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, Some(&input), None);
        let rec = read_last_record(&home);
        assert_eq!(rec["tool_name"], "tasks_create");
        assert_eq!(rec["success"], true);
        let stored = rec["input"].as_str().unwrap();
        assert!(stored.contains("發布"));
        assert!(!stored.contains("sk-live-xyz"), "secret must be masked");
        assert_eq!(rec["input_truncated"], false);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn input_skipped_for_readonly_tool_and_none() {
        let home = fresh_home();
        let input = serde_json::json!({ "query": "q" });
        append_tool_call_with_input(&home, "agnes", "memory_search", "ok", true, Some(&input), None);
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, None, None);
        let body = std::fs::read_to_string(home.join("tool_calls.jsonl")).unwrap();
        for line in body.lines() {
            let rec: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(rec.get("input").is_none(), "no input field expected: {line}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn oversized_input_truncated_with_marker_cjk_safe() {
        let home = fresh_home();
        // > AUDIT_INPUT_MAX_CHARS of multi-byte content.
        let big = "繁體中文稽核".repeat(1500);
        let input = serde_json::json!({ "content": big });
        append_tool_call_with_input(&home, "agnes", "shared_wiki_write", "ok", true, Some(&input), None);
        let rec = read_last_record(&home);
        assert_eq!(rec["input_truncated"], true);
        assert!(rec["input"].as_str().unwrap().chars().count() <= AUDIT_INPUT_MAX_CHARS);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_rows_and_new_rows_coexist() {
        // Backward compatibility: a pre-R4 row (no input fields) and a new
        // row parse through the same consumer path.
        let home = fresh_home();
        append_tool_call(&home, "agnes", "agent_update_soul", "ok: hash=abc", true);
        append_tool_call_with_input(
            &home,
            "agnes",
            "agent_update_soul",
            "ok: hash=def",
            true,
            Some(&serde_json::json!({ "content": "soul text" })),
            None,
        );
        let since = "2000-01-01T00:00:00Z";
        let rows = read_tool_calls_since(&home, "agnes", since);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].get("input").is_none());
        assert!(rows[1].get("input").is_some());
        // Canonical fields present on both shapes.
        for r in &rows {
            assert!(r.get("timestamp").is_some());
            assert!(r.get("params_summary").is_some());
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    // ── WP-10A: git/SSH/GPG credential env grant audit ─────────────────

    #[test]
    fn git_credentials_granted_is_audited_with_names_only_never_values() {
        let home = fresh_home();
        log_git_credentials_granted(&home, "agnes", &["SSH_AUTH_SOCK", "GNUPGHOME"]);

        let events = read_recent_events(&home, 10);
        assert_eq!(events.len(), 1, "exactly one audit row for the grant");
        let e = &events[0];
        assert_eq!(e.event_type, "git_credentials_env_granted");
        assert_eq!(e.agent_id, "agnes");
        assert!(
            matches!(e.severity, Severity::Warning),
            "handing over the operator's push/sign identity must be at least Warning"
        );
        let names: Vec<&str> = e.details["env_names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["SSH_AUTH_SOCK", "GNUPGHOME"]);

        // Never a value anywhere in the record — only the two known names.
        let raw = serde_json::to_string(e).unwrap();
        assert!(!raw.contains("/tmp"), "no path/value should leak into the audit record");

        let _ = std::fs::remove_dir_all(&home);
    }
}
