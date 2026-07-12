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
//! marker. With the existing 5 MB rotation ([`maybe_rotate_tool_calls`])
//! the worst case is ~1.2k full-size records per rotation window — the
//! rotation cadence shortens under heavy tool traffic, trading history
//! *depth* for input *completeness*. Operators who need longer retention
//! should archive the `.jsonl.old` file, not raise the cap.

use std::path::Path;

use chrono::Utc;
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
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
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
    "glpat-",
];

fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k))
}

fn is_sensitive_value(v: &str) -> bool {
    SENSITIVE_VALUE_PREFIXES.iter().any(|p| v.starts_with(p))
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
/// **input arguments** (R4 — record full inputs, not just outcomes).
///
/// Behavior:
/// - `input = None` or a read-only tool name ⇒ byte-identical record shape
///   to [`append_tool_call`] (no new fields).
/// - Otherwise the record gains `input` (masked via
///   [`mask_sensitive_json`], serialized, capped at
///   [`AUDIT_INPUT_MAX_CHARS`] chars) and `input_truncated: bool`.
///
/// Old consumers keep working: both fields are additive and optional.
pub fn append_tool_call_with_input(
    home_dir: &Path,
    agent_id: &str,
    tool_name: &str,
    params_summary: &str,
    success: bool,
    input: Option<&serde_json::Value>,
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
    append_tool_call_with_extras(home_dir, agent_id, tool_name, params_summary, success, &extras)
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

/// Rotate `tool_calls.jsonl` if it exceeds 5 MB.
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

    const MAX_SIZE: u64 = 5 * 1024 * 1024; // 5 MB
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_SIZE {
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
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, Some(&input));
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
        append_tool_call_with_input(&home, "agnes", "memory_search", "ok", true, Some(&input));
        append_tool_call_with_input(&home, "agnes", "tasks_create", "ok", true, None);
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
        append_tool_call_with_input(&home, "agnes", "shared_wiki_write", "ok", true, Some(&input));
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
}
