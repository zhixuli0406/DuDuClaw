//! Knowledge for one channel reply turn, decided before the model is
//! called: whether the fixed rules section goes into the system prompt,
//! which passages go into the user message, and (in `kb_only` mode) whether
//! the turn is answered with a fixed "not found" text without a model call.
//!
//! Permissions are the same single entry point every read uses
//! (`acl::effective_collections`: employee side ∩ the turn's person ∩ the
//! sensitivity filter). Restricted collections are never injected
//! automatically. Every lookup writes one `access_log` row (`auto_inject`).

use std::path::PathBuf;
use std::sync::Arc;

use duduclaw_auth::db::UserDb;
use duduclaw_core::knowledge_agent::{AnswerMode, RetrievalSettings};
use duduclaw_knowledge::acl::{
    AllowedSet, Narrowing, agent_side_collections, effective_collections,
};
use duduclaw_knowledge::audit::{AccessEntry, log_access};
use duduclaw_knowledge::inject::{self, ChannelSettings, NumberedPassage};
use duduclaw_knowledge::requester::{RequesterIdentity, agent_ctx, human_ctx};
use duduclaw_knowledge::{KbResult, schema};
use rusqlite::Connection;
use serde_json::json;

/// What the turn gets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnKnowledge {
    /// Add [`inject::RULES_SECTION`] to the system prompt.
    pub rules: bool,
    /// The `<knowledge_passages>` block for the user message.
    pub block: Option<String>,
    /// Answer with this text and skip the model (`kb_only`).
    pub direct_reply: Option<String>,
    /// Passages injected (for logs).
    pub injected: usize,
}

/// Inputs of [`prepare`].
pub struct TurnInput {
    pub home: PathBuf,
    pub users: Option<Arc<UserDb>>,
    pub agent_id: String,
    pub agent_dir: PathBuf,
    pub requester: RequesterIdentity,
    pub query: String,
    pub turn_id: String,
}

/// Decide the turn's knowledge. Any failure means "no knowledge this turn"
/// (logged), never more access.
pub async fn prepare(input: TurnInput) -> TurnKnowledge {
    if input.agent_id.is_empty() || !super::enabled(&input.home) {
        return TurnKnowledge::default();
    }
    match super::blocking(move || prepare_blocking(&input)).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "knowledge: automatic retrieval failed; none this turn");
            TurnKnowledge::default()
        }
    }
}

/// Collections that may be injected automatically: never restricted ones.
fn injectable(conn: &Connection, allowed: &AllowedSet) -> KbResult<AllowedSet> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM collections WHERE sensitivity = 'restricted'")?;
    let restricted: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(allowed.filtered(|id| !restricted.iter().any(|r| r == id)))
}

pub(crate) fn prepare_blocking(input: &TurnInput) -> KbResult<TurnKnowledge> {
    if !schema::db_path(&input.home).exists() {
        return Ok(TurnKnowledge::default());
    }
    let (scope, department, settings) =
        duduclaw_core::knowledge_agent::load_agent_knowledge_settings(&input.agent_dir);
    if scope.is_off() {
        return Ok(TurnKnowledge::default());
    }
    let conn = schema::open_home(&input.home)?;
    let agent = agent_ctx(&input.agent_id, department.as_deref(), &scope);
    if agent_side_collections(&conn, &agent)?.is_empty() {
        return Ok(TurnKnowledge::default());
    }
    let global = ChannelSettings::from_home(&input.home);
    let human = human_ctx(
        &input.requester,
        super::user_directory(&input.home, input.users.as_ref()).as_ref(),
    );
    let allowed = effective_collections(&conn, &human, Some(&agent), &Narrowing::default())?;
    let allowed = injectable(&conn, &allowed)?;
    let mut out = TurnKnowledge {
        rules: true,
        ..Default::default()
    };
    let kb_only = settings.answer_mode == AnswerMode::KbOnly;
    let mut entry = access_entry(input);
    entry.allowed = allowed.ids().to_vec();
    if allowed.is_empty() {
        if kb_only {
            out.direct_reply = Some(inject::KB_ONLY_NO_COLLECTIONS.to_string());
            entry.outcome = "no_match".into();
            entry.mode = Some("none".into());
            log(&conn, &entry);
        }
        return Ok(out);
    }
    if !kb_only
        && (!inject::wants_retrieval(&global, &settings) || inject::skip_query(&input.query))
    {
        return Ok(out);
    }
    let picked = search(
        &conn,
        &allowed,
        &input.query,
        &global,
        &settings,
        &mut entry,
    )?;
    if picked.is_empty() {
        entry.outcome = "no_match".into();
        log(&conn, &entry);
        if kb_only {
            out.direct_reply = Some(inject::KB_ONLY_NO_MATCH.to_string());
        } else {
            out.block = Some(inject::render_block("keyword_only", &[]));
        }
        return Ok(out);
    }
    let numbers = super::ledger::number(&input.agent_id, &input.turn_id, &picked, &input.query);
    let numbered: Vec<NumberedPassage<'_>> = picked
        .iter()
        .zip(&numbers)
        .enumerate()
        .map(|(i, (hit, n))| NumberedPassage {
            n: n.unwrap_or(i as u32 + 1),
            hit,
        })
        .collect();
    entry.results = numbered
        .iter()
        .enumerate()
        .map(|(i, p)| json!({"chunk_id": p.hit.chunk_id, "source_id": p.hit.source_id, "n": p.n, "rank": i + 1}))
        .collect();
    log(&conn, &entry);
    out.injected = numbered.len();
    out.block = Some(inject::render_block(
        entry.mode.as_deref().unwrap_or("keyword_only"),
        &numbered,
    ));
    Ok(out)
}

fn search(
    conn: &Connection,
    allowed: &AllowedSet,
    query: &str,
    global: &ChannelSettings,
    settings: &RetrievalSettings,
    entry: &mut AccessEntry,
) -> KbResult<Vec<duduclaw_knowledge::search::Hit>> {
    let query = duduclaw_core::truncate_chars(query.trim(), super::route::MAX_QUERY_CHARS);
    entry.query = Some(query.clone());
    if inject::skip_query(&query) {
        entry.mode = Some("none".into());
        return Ok(Vec::new());
    }
    let r = duduclaw_knowledge::search::search(
        conn,
        allowed,
        &query,
        settings.top_k.max(1) * 2,
        &duduclaw_knowledge::embed::NoEmbedder,
    )?;
    entry.mode = Some(r.mode.as_str().to_string());
    Ok(inject::select(
        &r.hits,
        settings.top_k,
        inject::budget(global, settings),
    ))
}

fn access_entry(input: &TurnInput) -> AccessEntry {
    AccessEntry {
        op: "auto_inject".into(),
        agent_id: Some(input.agent_id.clone()),
        requester_kind: input.requester.kind().to_string(),
        requester_id: input.requester.audit_id(),
        channel: input.requester.channel().map(str::to_string),
        turn_id: Some(duduclaw_core::truncate_chars(&input.turn_id, 128)),
        outcome: "ok".into(),
        ..Default::default()
    }
}

fn log(conn: &Connection, entry: &AccessEntry) {
    if let Err(e) = log_access(conn, entry) {
        tracing::warn!(error = %e, "knowledge: access log write failed");
    }
}

/// The system prompt with the rules section appended when `rules`.
pub fn with_rules(system_prompt: String, rules: bool) -> String {
    if rules {
        format!("{system_prompt}\n\n---\n\n{}", inject::RULES_SECTION)
    } else {
        system_prompt
    }
}

/// The user message with the passages block in front of the person's text
/// (after any `<task_recap>`, which the caller keeps first).
pub fn with_passages(user_text: &str, block: Option<&str>) -> String {
    match block {
        Some(b) => format!("{b}\n\n{user_text}"),
        None => user_text.to_string(),
    }
}

/// Test helper: the agent dir layout the gateway resolves.
#[cfg(test)]
pub(crate) fn agent_dir_for(home: &std::path::Path, agent: &str) -> PathBuf {
    home.join("agents").join(agent)
}

#[cfg(test)]
#[path = "auto_retrieve_tests.rs"]
mod tests;
