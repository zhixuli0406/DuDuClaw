//! Forget by source (P2-B): the half that lives outside `memory.db`.
//!
//! `duduclaw-memory` plans and applies a forget inside its own transaction and
//! records the follow-up steps it cannot do itself. This module
//!
//! - collects what a plan must see outside `memory.db` ([`collect_external`]):
//!   auto wiki pages carrying the source, review cards covering the target
//!   rows, and the session messages that will be hidden — all of it is part
//!   of the plan hash, so a change between plan and apply makes it stale;
//! - runs the recorded steps ([`run_forget_steps`]), each idempotent:
//!   `wiki_page_delete` (D5), `review_scrub` (§7.4), `session_hide` (D6:
//!   `undone_at`, the original text stays in `sessions.db`) and
//!   `session_summary_clear` (B.3 point 2);
//! - resumes unfinished steps at gateway boot ([`resume_unfinished`], D10).
//!
//! Every step only carries out what the operator already confirmed at apply;
//! nothing here decides a new deletion.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use duduclaw_memory::engine::forget_source::{
    STEP_REVIEW_SCRUB, STEP_SESSION_HIDE, STEP_SESSION_SUMMARY_CLEAR, STEP_WIKI_PAGE_DELETE,
};
use duduclaw_memory::{
    ExternalInputs, ForgetPlan, ForgetSelector, PlanOptions, SessionMessageRef, SqliteMemoryEngine,
    WikiPageRef, WikiStore, source_digest,
};
use rusqlite::{Connection, OptionalExtension, params};
use tracing::{info, warn};

/// Only auto-filed pages are ever deleted; human pages are never touched.
const AUTO_PAGE_PREFIX: &str = "auto/";

/// The selector fields the outside-memory collection depends on.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectorView {
    pub session: String,
    pub messages: Vec<String>,
    pub upto_seq: Option<i64>,
    pub upto_time: Option<DateTime<Utc>>,
}

impl SelectorView {
    pub fn from_selector(s: &ForgetSelector) -> Self {
        Self {
            session: s.session.clone(),
            messages: s.messages.clone(),
            upto_seq: s.upto_seq,
            upto_time: s.upto_time,
        }
    }

    /// The selector as frozen into a stored plan.
    pub fn from_plan(plan: &ForgetPlan) -> Self {
        let sel = &plan.document.selector;
        Self {
            session: sel.session.clone(),
            messages: sel.messages.clone(),
            upto_seq: sel.upto_seq,
            upto_time: sel
                .upto_time
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc)),
        }
    }

    fn session_scope(&self) -> bool {
        self.messages.is_empty()
    }
}

/// `sessions.db` under the home (the store the channel turns are in).
pub fn sessions_db_path(home: &Path) -> PathBuf {
    home.join("sessions.db")
}

/// The `memories` ids a stored plan removes (live targets and archive copies).
pub fn plan_memory_ids(plan: &ForgetPlan) -> Vec<String> {
    let body = &plan.document.body;
    let mut ids: Vec<String> = body
        .targets
        .iter()
        .filter(|t| t.store == "memories")
        .map(|t| t.id.clone())
        .collect();
    ids.extend(body.archive_ids.iter().cloned());
    ids
}

/// Whether one auto page `sources` entry was produced by a forgotten source.
///
/// Entries are `conversation:<session>:<message key>` (`m:<seq>`, `run:<key>`;
/// written since P2-B) or the older `conversation:<session>:<RFC3339 time>`.
/// A message key is always the last two `:`-separated parts (a known prefix
/// and one token), so it is read from the right and the session is
/// everything before it — a session id containing `:` cannot be mistaken
/// for another (L-1). The older time form has no message, so it is matched
/// only by a session watermark (design §3.4), as before.
pub fn page_source_matches(entry: &str, sel: &SelectorView) -> bool {
    use crate::memory_provenance::{WIKI_SOURCE_DIGEST_PREFIX, short_digest, wiki_source_entry};
    if let Some(d) = entry.strip_prefix(WIKI_SOURCE_DIGEST_PREFIX) {
        // Fixed-length form for an overlong session id (P2-B): the session is
        // a digest, the message key is kept, so the same rules apply.
        if let Some((digest, key)) = split_message_key(d) {
            return digest == short_digest(&sel.session) && key_matches(&key, sel);
        }
        // Whole-entry digest (overlong message key): only an exact message
        // can be recognised.
        return !sel.session_scope()
            && sel
                .messages
                .iter()
                .any(|m| wiki_source_entry(&sel.session, m) == entry);
    }
    let Some(body) = entry.strip_prefix("conversation:") else {
        return false;
    };
    if let Some((session, key)) = split_message_key(body) {
        return session == sel.session && key_matches(&key, sel);
    }
    // Older `<RFC3339 time>` form.
    let prefix = format!("{}:", sel.session);
    match body.strip_prefix(prefix.as_str()) {
        Some(rest) if sel.session_scope() => {
            match (DateTime::parse_from_rfc3339(rest).ok(), sel.upto_time) {
                (Some(t), Some(upto)) => t.with_timezone(&Utc) <= upto,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Message key prefixes a page source can end with.
const KEY_PREFIXES: &[&str] = &["m", "run", "turn", "item", "day", "call"];

/// `(<head>, <prefix>:<token>)` when `body` ends with a message key.
fn split_message_key(body: &str) -> Option<(&str, String)> {
    let (head, token) = body.rsplit_once(':')?;
    let (session, prefix) = head.rsplit_once(':')?;
    if token.is_empty() || session.is_empty() || !KEY_PREFIXES.contains(&prefix) {
        return None;
    }
    Some((session, format!("{prefix}:{token}")))
}

/// A message key against the selector.
fn key_matches(key: &str, sel: &SelectorView) -> bool {
    if !sel.session_scope() {
        return sel.messages.iter().any(|m| m == key);
    }
    if let Some(seq) = key.strip_prefix("m:") {
        return match seq.parse::<i64>() {
            Ok(seq) => sel.upto_seq.is_none_or(|upto| seq <= upto),
            Err(_) => false,
        };
    }
    // A run/turn key exists only for content already written, i.e. no later
    // than the plan's watermark time.
    true
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(duduclaw_memory::lineage::sha256_hex(&bytes))
}

/// Auto pages of `agent_id` carrying a source the selector forgets.
pub fn matching_wiki_pages(
    home: &Path,
    agent_id: &str,
    sel: &SelectorView,
) -> Result<Vec<WikiPageRef>, String> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Ok(Vec::new());
    }
    let wiki_dir = home.join("agents").join(agent_id).join("wiki");
    if !wiki_dir.join("auto").is_dir() {
        return Ok(Vec::new());
    }
    let store = WikiStore::new(wiki_dir.clone());
    let mut out = Vec::new();
    for meta in store.list_pages().map_err(|e| format!("list wiki: {e}"))? {
        if !meta.path.starts_with(AUTO_PAGE_PREFIX) {
            continue;
        }
        // Fail closed: an auto page that cannot be read cannot be cleared.
        let page = store
            .read_page(&meta.path)
            .map_err(|e| format!("read wiki page {}: {e}", meta.path))?;
        if page.sources.iter().any(|s| page_source_matches(s, sel)) {
            out.push(WikiPageRef {
                file_sha256: file_sha256(&wiki_dir.join(&meta.path))?,
                path: meta.path,
            });
        }
    }
    out.sort();
    Ok(out)
}

/// The session messages a plan will hide (ids only, session as a digest).
/// Messages already hidden are not listed (N5): forgetting a source again
/// after its messages were hidden finds nothing new to hide.
pub fn matching_session_messages(
    sessions_db: &Path,
    agent_id: &str,
    sel: &SelectorView,
) -> Result<Vec<SessionMessageRef>, String> {
    if !sessions_db.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open(sessions_db).map_err(|e| format!("open sessions.db: {e}"))?;
    let session_digest = source_digest(agent_id, &sel.session, "", "");
    let mut seqs: Vec<i64> = Vec::new();
    if sel.session_scope() {
        if let Some(upto) = sel.upto_seq {
            let mut stmt = conn
                .prepare(
                    "SELECT id FROM session_messages
                     WHERE session_id = ?1 AND id <= ?2 AND undone_at IS NULL",
                )
                .map_err(|e| format!("session messages: {e}"))?;
            let rows = stmt
                .query_map(params![sel.session, upto], |r| r.get::<_, i64>(0))
                .map_err(|e| format!("session messages: {e}"))?;
            for r in rows {
                seqs.push(r.map_err(|e| format!("session messages: {e}"))?);
            }
        }
    } else {
        for m in &sel.messages {
            let Some(seq) = m.strip_prefix("m:").and_then(|s| s.parse::<i64>().ok()) else {
                continue;
            };
            let found = conn
                .query_row(
                    "SELECT 1 FROM session_messages
                     WHERE session_id = ?1 AND id = ?2 AND undone_at IS NULL",
                    params![sel.session, seq],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| format!("session message: {e}"))?;
            if found.is_some() {
                seqs.push(seq);
            }
        }
    }
    seqs.sort_unstable();
    seqs.dedup();
    Ok(seqs
        .into_iter()
        .map(|seq| SessionMessageRef {
            session_digest: session_digest.clone(),
            seq,
        })
        .collect())
}

/// The assistant replies that belong to each user message in `seqs` (H-2,
/// N6): after the user message, any further user messages are skipped (the
/// user sent several before an answer came), then every assistant message up
/// to the next user message counts. Interleaved turns can therefore pull in
/// a reply to a neighbouring message too — more is forgotten, never less.
/// System rows are passed over. Returned as `(user seq, reply seq)`; a row
/// that is not a user message gives none.
pub fn same_turn_replies(
    sessions_db: &Path,
    session: &str,
    seqs: &[i64],
) -> Result<Vec<(i64, i64)>, String> {
    if seqs.is_empty() || !sessions_db.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open(sessions_db).map_err(|e| format!("open sessions.db: {e}"))?;
    let mut out = Vec::new();
    let mut next = conn
        .prepare(
            "SELECT id, role FROM session_messages
             WHERE session_id = ?1 AND id > ?2 ORDER BY id LIMIT 64",
        )
        .map_err(|e| format!("session messages: {e}"))?;
    for &seq in seqs {
        let role: Option<String> = conn
            .query_row(
                "SELECT role FROM session_messages WHERE session_id = ?1 AND id = ?2",
                params![session, seq],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("session message: {e}"))?;
        if role.as_deref() != Some("user") {
            continue;
        }
        let rows = next
            .query_map(params![session, seq], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| format!("session messages: {e}"))?;
        let mut answering = false;
        for row in rows {
            let (id, role) = row.map_err(|e| format!("session messages: {e}"))?;
            match role.as_str() {
                "assistant" => {
                    answering = true;
                    out.push((seq, id));
                }
                "user" if answering => break,
                _ => {}
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// A human label for one stored message: `(role, time, seq)` (M-3).
pub fn message_label(sessions_db: &Path, session: &str, seq: i64) -> Option<(String, String)> {
    let conn = Connection::open(sessions_db).ok()?;
    conn.query_row(
        "SELECT role, timestamp FROM session_messages WHERE session_id = ?1 AND id = ?2",
        params![session, seq],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .optional()
    .ok()
    .flatten()
}

/// Everything outside `memory.db` a plan for `sel` covers. `memory_ids` are
/// the rows the plan removes (a preview at plan time, the stored plan's
/// targets at apply), used to count the review cards that reference them.
pub async fn collect_external(
    home: &Path,
    agent_id: &str,
    sel: &SelectorView,
    memory_ids: &[String],
) -> Result<ExternalInputs, String> {
    Ok(ExternalInputs {
        wiki_pages: matching_wiki_pages(home, agent_id, sel)?,
        review_cards_matching: crate::wiki_ingest::count_review_cards_for_ids(home, memory_ids)
            .await?,
        session_messages: matching_session_messages(&sessions_db_path(home), agent_id, sel)?,
    })
}

/// [`collect_external`] for a new plan: previews the target rows first.
/// `Ok(None)` when the preview exceeds a limit (the plan will say so).
pub async fn collect_external_for_plan(
    engine: &SqliteMemoryEngine,
    home: &Path,
    agent_id: &str,
    selector: &ForgetSelector,
    options: PlanOptions,
) -> Result<Option<ExternalInputs>, String> {
    let Some(ids) = engine
        .preview_forget_memory_ids(agent_id, selector, options)
        .await
        .map_err(|e| format!("preview: {e}"))?
    else {
        return Ok(None);
    };
    collect_external(home, agent_id, &SelectorView::from_selector(selector), &ids)
        .await
        .map(Some)
}

/// [`collect_external`] for applying a stored plan.
pub async fn collect_external_for_apply(
    home: &Path,
    plan: &ForgetPlan,
) -> Result<ExternalInputs, String> {
    collect_external(
        home,
        &plan.document.agent_id,
        &SelectorView::from_plan(plan),
        &plan_memory_ids(plan),
    )
    .await
}

/// Result of one run of a plan's steps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepsReport {
    pub done: u64,
    pub failed: u64,
    /// Step kind → (done, failed).
    pub by_step: BTreeMap<String, (u64, u64)>,
}

impl StepsReport {
    pub fn complete(&self) -> bool {
        self.failed == 0
    }
}

/// Run every not-yet-done step of `plan_id` and record each result. Steps
/// are idempotent, so a re-run after a crash or failure is safe.
pub async fn run_forget_steps(
    engine: &SqliteMemoryEngine,
    home: &Path,
    plan_id: &str,
) -> Result<StepsReport, String> {
    let plan = engine
        .get_forget_plan(plan_id)
        .await
        .map_err(|e| format!("read plan: {e}"))?
        .ok_or_else(|| format!("forget plan {plan_id} not found"))?;
    if plan.status != "applied" {
        return Err(format!(
            "forget plan {plan_id} is {}, not applied",
            plan.status
        ));
    }
    let steps = engine
        .forget_steps(plan_id)
        .await
        .map_err(|e| format!("read steps: {e}"))?;
    let mut report = StepsReport::default();
    for step in steps.iter().filter(|s| s.status != "done") {
        let result = run_one(home, &plan, &step.step, &step.target).await;
        let ok = result.is_ok();
        if let Err(e) = &result {
            warn!(plan_id, step = %step.step, "forget step failed: {e}");
        }
        engine
            .mark_forget_step(plan_id, &step.step, &step.target, result)
            .await
            .map_err(|e| format!("record step: {e}"))?;
        let entry = report.by_step.entry(step.step.clone()).or_default();
        if ok {
            report.done += 1;
            entry.0 += 1;
        } else {
            report.failed += 1;
            entry.1 += 1;
        }
    }
    audit_steps(home, &plan, &report);
    Ok(report)
}

async fn run_one(home: &Path, plan: &ForgetPlan, step: &str, target: &str) -> Result<(), String> {
    let agent = plan.document.agent_id.as_str();
    let sel = SelectorView::from_plan(plan);
    match step {
        STEP_WIKI_PAGE_DELETE => delete_wiki_page(home, agent, target, &sel),
        STEP_REVIEW_SCRUB => {
            crate::wiki_ingest::scrub_review_store_for_forgotten(home, &plan_memory_ids(plan))
                .await
                .map(|_| ())
        }
        STEP_SESSION_HIDE => hide_messages(&sessions_db_path(home), &sel, target),
        STEP_SESSION_SUMMARY_CLEAR => clear_session_summary(&sessions_db_path(home), &sel),
        other => Err(format!("unknown forget step {other}")),
    }
}

/// Delete one auto page — only if it still carries a forgotten source (M3):
/// a page rewritten since apply from other conversations only is kept and
/// the step is done. A page already gone still has its index entry dropped;
/// an index failure fails the step so it is retried (L-2).
fn delete_wiki_page(
    home: &Path,
    agent: &str,
    path: &str,
    sel: &SelectorView,
) -> Result<(), String> {
    if !path.starts_with(AUTO_PAGE_PREFIX) || !duduclaw_core::is_valid_agent_id(agent) {
        return Err("refused: only an employee's auto/ pages are deleted".into());
    }
    let wiki_dir = home.join("agents").join(agent).join("wiki");
    let store = WikiStore::new(wiki_dir.clone());
    if wiki_dir.join(path).exists() {
        let page = store
            .read_page(path)
            .map_err(|e| format!("read page before delete: {e}"))?;
        if !page.sources.iter().any(|s| page_source_matches(s, sel)) {
            info!(
                page = path,
                "forget step: page no longer carries a forgotten source, kept"
            );
            return Ok(());
        }
    }
    store
        .delete_page_checked(path)
        .map_err(|e| format!("delete page: {e}"))
}

/// Hide (`undone_at`) the forgotten messages. `target` is `upto` (every
/// message up to the plan's watermark) or one message key; keys that are not
/// channel messages (`run:…`) have nothing to hide.
fn hide_messages(sessions_db: &Path, sel: &SelectorView, target: &str) -> Result<(), String> {
    if !sessions_db.exists() {
        return Ok(());
    }
    let conn = Connection::open(sessions_db).map_err(|e| format!("open sessions.db: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("busy_timeout: {e}"))?;
    let now = Utc::now().to_rfc3339();
    if target == "upto" {
        if let Some(upto) = sel.upto_seq {
            conn.execute(
                "UPDATE session_messages SET undone_at = ?1
                 WHERE session_id = ?2 AND id <= ?3 AND undone_at IS NULL",
                params![now, sel.session, upto],
            )
            .map_err(|e| format!("hide messages: {e}"))?;
        }
        return Ok(());
    }
    if let Some(seq) = target
        .strip_prefix("m:")
        .and_then(|s| s.parse::<i64>().ok())
    {
        conn.execute(
            "UPDATE session_messages SET undone_at = ?1
             WHERE session_id = ?2 AND id = ?3 AND undone_at IS NULL",
            params![now, sel.session, seq],
        )
        .map_err(|e| format!("hide message: {e}"))?;
    }
    Ok(())
}

/// Clear the session's compressed context: the async summary
/// (`summary_of_prior`), the compression summary column, and the compression
/// summary rows (`role = 'system'`, hidden like forgotten messages). Whether
/// a forgotten message was folded into a summary cannot be told, so it is
/// always cleared (B.3 point 2): up to the watermark for a session forget,
/// all of them for a message forget.
fn clear_session_summary(sessions_db: &Path, sel: &SelectorView) -> Result<(), String> {
    if !sessions_db.exists() {
        return Ok(());
    }
    let conn = Connection::open(sessions_db).map_err(|e| format!("open sessions.db: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("busy_timeout: {e}"))?;
    let now = Utc::now().to_rfc3339();
    let has_col = |col: &str| -> bool {
        conn.prepare("SELECT name FROM pragma_table_info('sessions') WHERE name = ?1")
            .and_then(|mut s| s.query_row([col], |_| Ok(())).optional())
            .map(|r| r.is_some())
            .unwrap_or(false)
    };
    conn.execute(
        "UPDATE sessions SET summary = '' WHERE id = ?1",
        params![sel.session],
    )
    .map_err(|e| format!("clear summary: {e}"))?;
    if has_col("summary_of_prior") {
        conn.execute(
            "UPDATE sessions SET summary_of_prior = NULL, summarized_through_turn = 0
             WHERE id = ?1",
            params![sel.session],
        )
        .map_err(|e| format!("clear prior summary: {e}"))?;
    }
    let upto = if sel.session_scope() {
        sel.upto_seq
    } else {
        None
    };
    match upto {
        Some(upto) => conn.execute(
            "UPDATE session_messages SET undone_at = ?1
             WHERE session_id = ?2 AND role = 'system' AND id <= ?3 AND undone_at IS NULL",
            params![now, sel.session, upto],
        ),
        None => conn.execute(
            "UPDATE session_messages SET undone_at = ?1
             WHERE session_id = ?2 AND role = 'system' AND undone_at IS NULL",
            params![now, sel.session],
        ),
    }
    .map_err(|e| format!("hide summary rows: {e}"))?;
    Ok(())
}

fn audit_steps(home: &Path, plan: &ForgetPlan, report: &StepsReport) {
    let by_step: serde_json::Map<String, serde_json::Value> = report
        .by_step
        .iter()
        .map(|(k, (d, f))| (k.clone(), serde_json::json!({ "done": d, "failed": f })))
        .collect();
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "memory_source_forget_steps",
            &plan.document.agent_id,
            if report.failed == 0 {
                duduclaw_security::audit::Severity::Info
            } else {
                duduclaw_security::audit::Severity::Warning
            },
            serde_json::json!({
                "plan_id": plan.plan_id,
                "agent_id": plan.document.agent_id,
                "done": report.done,
                "failed": report.failed,
                "steps": by_step,
            }),
        ),
    );
}

/// Boot-time resume (D10): run every applied plan's pending or failed steps.
/// Returns `(plans, failed steps)`. Never panics; errors are logged.
pub async fn resume_unfinished(engine: &SqliteMemoryEngine, home: &Path) -> (usize, u64) {
    let steps = match engine.unfinished_forget_steps().await {
        Ok(s) => s,
        Err(e) => {
            warn!("forget steps: cannot list unfinished steps: {e}");
            return (0, 0);
        }
    };
    let mut plans: Vec<String> = steps.into_iter().map(|s| s.plan_id).collect();
    plans.sort();
    plans.dedup();
    let mut failed = 0u64;
    for plan_id in &plans {
        match run_forget_steps(engine, home, plan_id).await {
            Ok(r) => {
                failed += r.failed;
                info!(plan_id = %plan_id, done = r.done, failed = r.failed, "forget steps resumed");
            }
            Err(e) => {
                failed += 1;
                warn!(plan_id = %plan_id, "forget steps resume failed: {e}");
            }
        }
    }
    (plans.len(), failed)
}

/// [`resume_unfinished`] against `<home>/memory.db` on a blocking thread
/// (the engine is `!Send`). For the gateway's boot sequence.
pub async fn resume_unfinished_at_boot(home: &Path) {
    let db = home.join("memory.db");
    if !db.exists() {
        return;
    }
    let home = home.to_path_buf();
    let r = tokio::task::spawn_blocking(move || {
        let engine = match SqliteMemoryEngine::new(&db) {
            Ok(e) => e,
            Err(e) => {
                warn!("forget steps: open memory.db failed: {e}");
                return (0, 0);
            }
        };
        tokio::runtime::Handle::current().block_on(resume_unfinished(&engine, &home))
    })
    .await;
    match r {
        Ok((plans, failed)) if plans > 0 => {
            info!(plans, failed, "forget-by-source steps resumed at boot")
        }
        Ok(_) => {}
        Err(e) => warn!("forget steps resume task failed: {e}"),
    }
}

#[cfg(test)]
#[path = "memory_forget_steps_tests.rs"]
mod tests;
