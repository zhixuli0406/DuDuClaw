//! Host-generated memory sources for the gateway's write paths (P2-B) and the
//! `memory_write_fenced` audit event.
//!
//! Every memory write the gateway makes from a conversation carries the
//! `session_messages.id` of the turn it came from (design §3.1, §5.3). The
//! sources are built here, by the host, from ids the session store returned;
//! a model's output never supplies one. A path that has no source (the turn
//! could not be saved) writes nothing rather than inventing one.
//!
//! A write the source fence refuses is not an error: it is logged, audited
//! (`memory_write_fenced`, digests only, at most [`FENCED_AUDIT_DAILY_CAP`]
//! rows per employee per UTC day, then counted only) and skipped.

use std::path::Path;

use chrono::{DateTime, Utc};
use duduclaw_core::error::DuDuClawError;
use duduclaw_memory::lineage::{FenceRefusal, Provenance, SourceKind, SourceRef};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

/// Audit rows per employee per UTC day before `memory_write_fenced` only counts.
pub const FENCED_AUDIT_DAILY_CAP: u64 = 50;

/// Counter file (under the home) shared by every process (gateway, MCP server).
const FENCED_COUNTER_FILE: &str = "memory_write_fenced_counts.json";

/// Full sha256 hex of `text` (UTF-8 bytes).
pub fn content_hash(text: &str) -> String {
    let d = Sha256::digest(text.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The source of one stored channel message: `stored_text` must be exactly
/// what `append_message_with_id` saved, so a later rewrite is detectable.
pub fn channel_message_source(
    session_id: &str,
    message_id: i64,
    stored_text: &str,
    observed_at: DateTime<Utc>,
) -> SourceRef {
    SourceRef::channel_message(
        session_id,
        message_id,
        observed_at,
        Some(content_hash(stored_text)),
    )
}

/// The source of one dispatch run (`session` = `"{request_type}:{agent}"`):
/// `message = run:<first 16 bytes of sha256(len(prompt) ‖ prompt ‖ reply)>`.
pub fn dispatch_run_source(
    session: &str,
    prompt: &str,
    reply: &str,
    observed_at: DateTime<Utc>,
) -> SourceRef {
    let mut h = Sha256::new();
    h.update((prompt.len() as u64).to_le_bytes());
    h.update(prompt.as_bytes());
    h.update(reply.as_bytes());
    let d = h.finalize();
    let key: String = d[..16].iter().map(|b| format!("{b:02x}")).collect();
    SourceRef::other(
        SourceKind::DispatchRun,
        session,
        format!("run:{key}"),
        observed_at,
    )
}

/// A fresh run key for one dispatch / cron / goal run (32 hex characters),
/// minted before the CLI spawn so the MCP writes made during the run and the
/// run's own post-run distillation share one source (P2-B M-6).
pub fn new_dispatch_run_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The source of one dispatch run identified by its minted run key.
pub fn dispatch_run_source_for(
    session: &str,
    run_key: &str,
    observed_at: DateTime<Utc>,
) -> SourceRef {
    SourceRef::other(
        SourceKind::DispatchRun,
        session,
        format!("run:{run_key}"),
        observed_at,
    )
}

tokio::task_local! {
    /// The dispatch run in progress: `(session, run key)`. Scoped around the
    /// run's CLI future so the spawn can hand it to the MCP server.
    pub static DISPATCH_RUN: Option<(String, String)>;
}

tokio::task_local! {
    /// The bus message that started this dispatch said its upstream turn
    /// identity was incomplete (sender dropped it) or carried only half of
    /// it. Scoped by the dispatcher; handed to the spawn as
    /// [`duduclaw_core::ENV_UPSTREAM_UNKNOWN`] so the run's MCP writes record
    /// the upstream as unknown.
    pub static UPSTREAM_UNKNOWN: bool;
}

fn upstream_unknown_in_scope() -> bool {
    UPSTREAM_UNKNOWN.try_with(|u| *u).unwrap_or(false)
}

/// Put the current dispatch run (if any) into a CLI spawn's env.
pub fn inject_dispatch_run_env(cmd: &mut tokio::process::Command) {
    if let Ok(Some((session, key))) = DISPATCH_RUN.try_with(|r| r.clone()) {
        cmd.env(duduclaw_core::ENV_DISPATCH_SESSION, session);
        cmd.env(duduclaw_core::ENV_DISPATCH_RUN_ID, key);
        if upstream_unknown_in_scope() {
            cmd.env(duduclaw_core::ENV_UPSTREAM_UNKNOWN, "1");
        }
    }
}

/// The turn and run identity in scope, as `(env name, value)` pairs: the
/// same variables the Claude spawn sets (turn id, session id, triggering
/// user message, dispatch run). Other runtimes hand them to the duduclaw MCP
/// server through their own per-spawn channel (N4). Empty outside a turn or
/// run.
pub fn turn_source_env_pairs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(Some(t)) = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(|t| t.clone()) {
        out.push((duduclaw_core::ENV_TRUST_TURN_ID.to_string(), t));
    }
    if let Ok(Some(s)) = duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(|s| s.clone()) {
        out.push((duduclaw_core::ENV_TRUST_SESSION_ID.to_string(), s));
    }
    if let Ok(Some((seq, at))) = TURN_USER_MESSAGE.try_with(|m| m.clone()) {
        out.push((
            duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ.to_string(),
            seq.to_string(),
        ));
        out.push((duduclaw_core::ENV_TURN_USER_MESSAGE_AT.to_string(), at));
    }
    if let Ok(Some((session, key))) = DISPATCH_RUN.try_with(|r| r.clone()) {
        out.push((duduclaw_core::ENV_DISPATCH_SESSION.to_string(), session));
        out.push((duduclaw_core::ENV_DISPATCH_RUN_ID.to_string(), key));
        if upstream_unknown_in_scope() {
            out.push((
                duduclaw_core::ENV_UPSTREAM_UNKNOWN.to_string(),
                "1".to_string(),
            ));
        }
    }
    out
}

/// Audit event: a bus message was about to carry half an upstream turn
/// identity, so the sender attached neither half.
pub const AUDIT_UPSTREAM_DROPPED: &str = "bus_upstream_identity_dropped";
/// Audit event: a received bus message carries half an upstream turn
/// identity; the run's memory writes record the upstream as unknown.
pub const AUDIT_UPSTREAM_INCOMPLETE: &str = "bus_upstream_identity_incomplete";

/// The upstream turn identity a bus message carries: the turn id and the
/// session id together, or neither. Returns the pair to attach and, when one
/// half was present without the other, the name of the missing field.
pub fn complete_upstream_pair(
    turn: Option<String>,
    session: Option<String>,
) -> (Option<String>, Option<String>, Option<&'static str>) {
    let some = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    match (some(&turn), some(&session)) {
        (true, true) => (turn, session, None),
        (false, false) => (None, None, None),
        (true, false) => (None, None, Some("session_id")),
        (false, true) => (None, None, Some("turn_id")),
    }
}

/// The missing half of a received message's upstream identity, if exactly
/// one half is present.
pub fn incomplete_upstream(turn: Option<&str>, session: Option<&str>) -> Option<&'static str> {
    let some = |v: Option<&str>| v.is_some_and(|s| !s.trim().is_empty());
    match (some(turn), some(session)) {
        (true, false) => Some("session_id"),
        (false, true) => Some("turn_id"),
        _ => None,
    }
}

/// Audit one incomplete upstream identity (no ids or content, only which
/// field is missing and where). Never fails.
pub fn audit_upstream_identity(
    home: &Path,
    event: &str,
    agent_id: &str,
    message_id: &str,
    missing: &str,
) {
    warn!(
        agent = agent_id,
        message_id, missing, "bus message upstream turn identity incomplete ({event})"
    );
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            event,
            agent_id,
            duduclaw_security::audit::Severity::Warning,
            serde_json::json!({
                "agent_id": agent_id,
                "message_id": message_id,
                "missing": missing,
            }),
        ),
    );
}

/// One OS footprint day of `agent` (`YYYY-MM-DD`).
pub fn footprint_day_source(agent: &str, day: &str, observed_at: DateTime<Utc>) -> SourceRef {
    SourceRef::other(
        SourceKind::FootprintDay,
        format!("footprint:{agent}"),
        format!("day:{day}"),
        observed_at,
    )
}

tokio::task_local! {
    /// The user message that triggered the current channel turn:
    /// `(session_messages.id, observed_at RFC3339)`. Scoped around the turn's
    /// CLI future so the spawn can hand it to the MCP server (P2-B).
    pub static TURN_USER_MESSAGE: Option<(i64, String)>;
}

/// Put the current turn's user message (if any) into a CLI spawn's env, next
/// to `DUDUCLAW_TURN_ID`. No turn in scope ⇒ nothing is set.
pub fn inject_turn_user_message_env(cmd: &mut tokio::process::Command) {
    if let Ok(Some((seq, at))) = TURN_USER_MESSAGE.try_with(|m| m.clone()) {
        cmd.env(duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ, seq.to_string());
        cmd.env(duduclaw_core::ENV_TURN_USER_MESSAGE_AT, at);
    }
}

/// The sources of one conversation turn: the user message and the
/// assistant's reply, each present only when the session store saved it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnSources {
    pub user: Option<SourceRef>,
    pub assistant: Option<SourceRef>,
}

impl TurnSources {
    /// Both sources (user first), possibly empty.
    pub fn all(&self) -> Vec<SourceRef> {
        self.user
            .iter()
            .chain(self.assistant.iter())
            .cloned()
            .collect()
    }

    /// Provenance from both messages; `None` when neither was saved.
    pub fn provenance(&self) -> Option<Provenance> {
        let all = self.all();
        (!all.is_empty()).then_some(Provenance::Sources(all))
    }

    /// Provenance from the assistant message only (a decision offered in
    /// the reply); `None` when it was not saved.
    pub fn assistant_provenance(&self) -> Option<Provenance> {
        self.assistant.clone().map(Provenance::source)
    }

    /// Provenance from the user message only (a choice the user made).
    pub fn user_provenance(&self) -> Option<Provenance> {
        self.user.clone().map(Provenance::source)
    }
}

/// `Sources(sources)` or `None` for an empty list (never a fabricated source).
pub fn provenance_of(sources: &[SourceRef]) -> Option<Provenance> {
    (!sources.is_empty()).then(|| Provenance::Sources(sources.to_vec()))
}

/// The fence reason and source digest of an error, when the error is a fence
/// refusal (typed: [`DuDuClawError::SourceFenced`], never parsed from text).
pub fn fenced_parts(err: &DuDuClawError) -> Option<(&str, Option<&str>)> {
    match err {
        DuDuClawError::SourceFenced {
            reason,
            source_digest,
            ..
        } => Some((reason.as_str(), source_digest.as_deref())),
        _ => None,
    }
}

/// The source fence for a write kept outside `memory.db` (an auto wiki page):
/// the first of `sources` that was forgotten in `agent_id`'s namespace.
/// `Err` when the check could not be made — callers treat that as fenced
/// (fail closed: an unverifiable derived artifact is not written).
pub async fn sources_forgotten(
    memory_db: &Path,
    home: &Path,
    agent_id: &str,
    sources: &[SourceRef],
) -> Result<Option<FenceRefusal>, String> {
    let db = memory_db.to_path_buf();
    let home = home.to_path_buf();
    let agent = agent_id.to_string();
    let sources = sources.to_vec();
    tokio::task::spawn_blocking(move || {
        let engine = crate::memory_factory::build_memory_engine(&db, &home)
            .map_err(|e| format!("open memory engine: {e}"))?;
        tokio::runtime::Handle::current()
            .block_on(engine.source_fence_check(&agent, &sources))
            .map_err(|e| format!("source fence check: {e}"))
    })
    .await
    .map_err(|e| format!("source fence task: {e}"))?
}

/// Longest `sources` entry an auto page keeps verbatim (the page writer cuts
/// longer values, which would make the entry unmatchable).
pub const WIKI_SOURCE_MAX_CHARS: usize = 120;
/// Prefix of the fixed-length form used when the plain form is too long.
pub const WIKI_SOURCE_DIGEST_PREFIX: &str = "conversation-digest:";
/// Longest message key kept beside a session digest.
const WIKI_SOURCE_DIGEST_MESSAGE_MAX: usize = 64;

/// First 32 hex chars of sha256(`text`).
pub fn short_digest(text: &str) -> String {
    content_hash(text)[..32].to_string()
}

/// The `sources` entry for `session` / `message`:
/// `conversation:<session>:<message>`, or — when that would exceed
/// [`WIKI_SOURCE_MAX_CHARS`] — `conversation-digest:<digest(session)>:<message>`
/// (a session watermark can still read the message key), or for an overlong
/// message key `conversation-digest:<digest(conversation:<session>:<message>)>`
/// (matched by message only). Never truncated.
pub fn wiki_source_entry(session: &str, message: &str) -> String {
    let plain = format!("conversation:{session}:{message}");
    // A message key has exactly one `:` (prefix and token); anything else
    // could not be read back from the right, so it is kept only as a digest.
    let one_key = message.matches(':').count() == 1;
    if one_key && plain.chars().count() <= WIKI_SOURCE_MAX_CHARS {
        return plain;
    }
    if one_key && message.len() <= WIKI_SOURCE_DIGEST_MESSAGE_MAX {
        return format!(
            "{WIKI_SOURCE_DIGEST_PREFIX}{}:{message}",
            short_digest(session)
        );
    }
    format!("{WIKI_SOURCE_DIGEST_PREFIX}{}", short_digest(&plain))
}

/// The `sources` entry an auto wiki page records for a turn (see
/// [`wiki_source_entry`]) for the first source: the user message on a
/// channel turn, the run on a dispatch turn.
pub fn wiki_source_id(sources: &[SourceRef]) -> Option<String> {
    sources
        .first()
        .map(|s| wiki_source_entry(&s.session, &s.message))
}

/// Log, audit and count one fenced write. Never fails.
pub fn record_fenced(home: &Path, agent_id: &str, producer: &str, refusal: &FenceRefusal) {
    record_fenced_parts(
        home,
        agent_id,
        producer,
        refusal.reason.as_str(),
        refusal.source_digest.as_deref(),
    );
}

/// [`record_fenced`] for an error from an error-reporting write API; returns
/// `false` (and records nothing) when the error is not a fence refusal.
pub fn record_fenced_error(
    home: &Path,
    agent_id: &str,
    producer: &str,
    err: &DuDuClawError,
) -> bool {
    match fenced_parts(err) {
        Some((reason, digest)) => {
            record_fenced_parts(home, agent_id, producer, reason, digest);
            true
        }
        None => false,
    }
}

fn record_fenced_parts(
    home: &Path,
    agent_id: &str,
    producer: &str,
    reason: &str,
    source_digest: Option<&str>,
) {
    warn!(
        agent = agent_id,
        producer, reason, "memory write refused: its source was forgotten (skipped)"
    );
    let n = bump_daily_count(home, agent_id);
    if n > FENCED_AUDIT_DAILY_CAP {
        debug!(
            agent = agent_id,
            count = n,
            "memory_write_fenced: daily audit cap reached"
        );
        return;
    }
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "memory_write_fenced",
            agent_id,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({
                "agent_id": agent_id,
                "producer": producer,
                "reason": reason,
                "source_digest": source_digest,
                "count_today": n,
            }),
        ),
    );
}

/// Increment and return today's count for `agent_id`. Only today's entries
/// are kept. When the counter cannot be read or written the refusal is still
/// audited (returns 1): losing the cap is better than losing the record.
fn bump_daily_count(home: &Path, agent_id: &str) -> u64 {
    let path = home.join(FENCED_COUNTER_FILE);
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let result = duduclaw_core::with_file_lock(&path, || {
        let mut doc: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if doc.get("day").and_then(|d| d.as_str()) != Some(today.as_str()) {
            doc = serde_json::json!({ "day": today, "counts": {} });
        }
        let counts = doc
            .get_mut("counts")
            .and_then(|c| c.as_object_mut())
            .ok_or_else(|| std::io::Error::other("counter file malformed"))?;
        let n = counts.get(agent_id).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
        counts.insert(agent_id.to_string(), serde_json::json!(n));
        let text = serde_json::to_string(&doc).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &path)?;
        Ok(n)
    });
    match result {
        Ok(n) => n,
        Err(e) => {
            debug!("memory_write_fenced counter unavailable: {e}");
            1
        }
    }
}

/// One channel-message source for tests that drive a producer end to end.
#[cfg(test)]
pub(crate) fn test_sources() -> Vec<SourceRef> {
    vec![SourceRef::channel_message(
        "test:session",
        1,
        Utc::now(),
        None,
    )]
}

/// Channel message `m:<seq>` of `session` (tests).
#[cfg(test)]
pub(crate) fn test_msg(session: &str, seq: i64) -> SourceRef {
    SourceRef::channel_message(session, seq, Utc::now(), None)
}

/// Plan and apply a forget of `messages` (keys) of `session` in `agent`'s
/// namespace; panics unless it applies (tests).
#[cfg(test)]
pub(crate) async fn test_forget(
    engine: &duduclaw_memory::SqliteMemoryEngine,
    agent: &str,
    session: &str,
    messages: &[&str],
) -> duduclaw_memory::ApplyReport {
    let sel = duduclaw_memory::ForgetSelector {
        session: session.to_string(),
        messages: messages.iter().map(|m| m.to_string()).collect(),
        upto_seq: None,
        upto_time: None,
    };
    let plan = match engine
        .plan_forget_source(agent, &sel, Default::default(), &Default::default())
        .await
        .unwrap()
    {
        duduclaw_memory::PlanOutcome::Planned(p) => p,
        other => panic!("expected a plan, got {other:?}"),
    };
    match engine
        .apply_forget_plan(&plan.plan_id, &Default::default())
        .await
        .unwrap()
    {
        duduclaw_memory::ApplyOutcome::Applied(r) => r,
        other => panic!("expected Applied, got {other:?}"),
    }
}

/// Store one plain memory carrying `source` (so a forget has a target).
#[cfg(test)]
pub(crate) async fn test_seed(
    engine: &duduclaw_memory::SqliteMemoryEngine,
    agent: &str,
    content: &str,
    source: SourceRef,
) -> String {
    let entry = duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        tags: vec![],
        embedding: None,
        layer: duduclaw_core::types::MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "p2b_test".to_string(),
    };
    engine
        .store_temporal(agent, entry, Default::default(), Provenance::source(source))
        .await
        .unwrap()
}

#[cfg(test)]
mod tests;
