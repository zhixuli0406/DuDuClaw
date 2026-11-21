//! Cross-session user profile — per-user preference facts that persist and
//! supersede across sessions, rendered into an injectable `## About This User`
//! block.
//!
//! This is the sibling of the F2 Reflexion consolidation: same "accumulate
//! observations → synthesise one durable record" shape, but keyed by a user id
//! (`subject = "user:<id>"`) instead of a mistake category. Every trait rides
//! the temporal-supersession machinery in `store_temporal`, so re-recording the
//! same `predicate` for a user automatically closes out the prior value and
//! links a supersession chain — the profile is always the currently-valid set.
//!
//! The rendered block is deterministic (traits sorted by predicate), so the
//! injected system-prompt bytes are stable for a given fact set — prompt-cache
//! friendly, exactly like the ranked-wiki injection.

use crate::engine::{RowScope, SqliteMemoryEngine};
use crate::TemporalMeta;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::types::{MemoryEntry, MemoryLayer};

/// Predicate reserved for the consolidated free-text profile summary; excluded
/// from the raw-trait listing so it never recurses into itself.
const SUMMARY_PREDICATE: &str = "profile_summary";

/// Normalize a channel-supplied user id into the durable identity the profile
/// is keyed on.
///
/// WebChat mints `webchat:<owner-tag>:<per-connection-nonce>`; the trailing
/// segment is re-rolled on every page reload. Keying the profile on the raw id
/// makes write and read agree *within* one connection and lose everything
/// between connections — the gap is invisible in a single session and total
/// across sessions. The owner tag is the durable identity; the connection
/// nonce is not. Every other channel already supplies a stable id and is
/// returned unchanged.
///
/// `':'` is ASCII, so the byte index used to split is always a char boundary
/// (no raw mid-char slicing, project convention #1).
pub fn stable_user_id(user_id: &str) -> &str {
    const WEBCHAT: &str = "webchat:";
    let Some(rest) = user_id.strip_prefix(WEBCHAT) else {
        return user_id;
    };
    match rest.rfind(':') {
        Some(idx) if idx > 0 => &user_id[..WEBCHAT.len() + idx],
        _ => user_id,
    }
}

/// The `subject` value for a user's facts. Normalizing here (rather than at
/// each call site) keeps every path — record, read, history, MCP tool — keyed
/// identically by construction.
///
/// v1.84.0: this is the bare (pre-v1.84) form, used only where no trusted
/// channel is known (employee-wide tools, which may pass an already
/// channel-qualified id). Conversation paths use [`profile_subject`].
pub fn user_subject(user_id: &str) -> String {
    format!("user:{}", stable_user_id(user_id))
}

/// A channel name as it appears in a profile subject: trimmed, ASCII
/// lowercase, `[a-z0-9_-]+`. `None` for anything else (fail closed: no
/// profile subject can be built for it).
pub fn normalize_channel(channel: &str) -> Option<String> {
    let c = channel.trim().to_ascii_lowercase();
    let valid = !c.is_empty()
        && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    valid.then_some(c)
}

/// The channel of a conversation key `<channel>:<chat_id>[:<thread>][#agent:<id>]`:
/// the exact segment before the first `:` ([`normalize_channel`]), or `None`
/// when the key has no chat part.
pub fn channel_of_session(session: &str) -> Option<String> {
    let conversation = duduclaw_core::session_conversation(session.trim());
    let (channel, rest) = conversation.split_once(':')?;
    if rest.is_empty() {
        return None;
    }
    normalize_channel(channel)
}

/// v1.84.0: THE profile subject of the person `user_id` on `channel`:
/// `user:<channel>:<id>`. Two people with the same raw id on different
/// platforms get different subjects. Every conversation write and read
/// builds its subject here.
///
/// WebChat ids already carry their channel (`webchat:<owner-tag>:<nonce>`,
/// see [`stable_user_id`]), and the pre-v1.84 subject `user:webchat:<owner>`
/// is already of the `user:<channel>:<id>` shape — unambiguous, so it is kept
/// byte for byte (no WebChat row needs migrating).
///
/// `None` when the channel is not a valid name or the id is blank.
pub fn profile_subject(channel: &str, user_id: &str) -> Option<String> {
    let channel = normalize_channel(channel)?;
    let id = user_id.trim();
    if id.is_empty() {
        return None;
    }
    if channel == "webchat" {
        if let Some(owner) = stable_user_id(id).strip_prefix("webchat:").filter(|o| !o.is_empty()) {
            return Some(format!("user:webchat:{owner}"));
        }
    }
    Some(format!("user:{channel}:{id}"))
}

/// Which subject(s) a profile read or write uses (v1.84.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileKey {
    /// Where traits are written and read first.
    pub subject: String,
    /// The bare pre-v1.84 subject, read only when no row of `subject` is
    /// visible to the reader — and only for a platform whose ids cannot
    /// collide across channels (decided by the caller, e.g. the gateway's
    /// `memory_privacy::channel_id_is_globally_unique`).
    pub legacy_subject: Option<String>,
}

impl ProfileKey {
    /// The pre-v1.84 keying by user id alone ([`user_subject`]), no fallback:
    /// for callers with no trusted channel.
    pub fn legacy(user_id: &str) -> Self {
        Self { subject: user_subject(user_id), legacy_subject: None }
    }

    /// The key of `user_id` on `channel` ([`profile_subject`]).
    /// `legacy_readable` = the platform's ids are globally unique, so the
    /// bare `user:<id>` rows written before v1.84 may be read as a fallback.
    pub fn for_channel(channel: &str, user_id: &str, legacy_readable: bool) -> Option<Self> {
        let subject = profile_subject(channel, user_id)?;
        let legacy = user_subject(user_id);
        let legacy_subject = (legacy_readable && legacy != subject).then_some(legacy);
        Some(Self { subject, legacy_subject })
    }
}

/// One currently-valid profile trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileTrait {
    pub predicate: String,
    /// The value — the triple `object` when present, else the memory content.
    pub value: String,
}

/// Default origin for profile writes: the `user_profile` class — a record the
/// AI employee keeps about a user. It is the AI's record, not the user's or
/// the operator's direct input, so its ceiling is the agent-derived `0.6`
/// (`crate::origin::USER_PROFILE`); an operator-approved value (`1.0`) can
/// only be replaced through review.
const DEFAULT_PROFILE_ORIGIN: &str = crate::origin::USER_PROFILE.name;

/// Record (or update) one preference trait about a user. Re-recording the same
/// `predicate` supersedes the prior value via the temporal chain.
///
/// `origin_trust` in `[0,1]` marks how much to trust the source (channel-derived
/// facts should be < 1.0); it flows through `store_temporal`'s trust clamp.
///
/// Writes are stamped with the `user_profile` origin. Callers whose provenance
/// is weaker than a deliberate profile write — notably the conversation
/// distillation pipeline — must use [`record_trait_with_origin`] instead so the
/// v1.41 trust ceiling for their own origin class applies (a distilled trait
/// must not launder itself into `user_direct` trust).
pub async fn record_trait(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    predicate: &str,
    value: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
) -> Result<String> {
    record_trait_with_origin(
        engine,
        agent_id,
        user_id,
        predicate,
        value,
        DEFAULT_PROFILE_ORIGIN,
        origin_trust,
        provenance,
    )
    .await
}

/// [`record_trait`] with an explicit origin class (see
/// `duduclaw_memory::origin`). `store_temporal` clamps `origin_trust` down to
/// that class's ceiling, so a low-trust source cannot claim profile-grade
/// trust however high a value it declares.
#[allow(clippy::too_many_arguments)]
pub async fn record_trait_with_origin(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
) -> Result<String> {
    let (entry, meta) =
        trait_write(agent_id, &user_subject(user_id), predicate, value, origin, origin_trust);
    engine.store_temporal(agent_id, entry, meta, provenance).await
}

/// [`record_trait_with_origin`] with the typed outcome of
/// [`SqliteMemoryEngine::store_temporal_outcome`]: a trait that would replace a
/// more trusted current value comes back as `Refused` instead of an `Err`, so
/// the caller can hold it for review via [`hold_trait`].
#[allow(clippy::too_many_arguments)]
pub async fn record_trait_outcome(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
) -> Result<crate::supersession_guard::TemporalWriteOutcome> {
    record_trait_outcome_for(
        engine,
        agent_id,
        &ProfileKey::legacy(user_id),
        predicate,
        value,
        origin,
        origin_trust,
        provenance,
    )
    .await
}

/// [`record_trait_outcome`] under `key.subject` (v1.84.0: the channel-
/// qualified subject of [`ProfileKey::for_channel`]).
#[allow(clippy::too_many_arguments)]
pub async fn record_trait_outcome_for(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &ProfileKey,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
) -> Result<crate::supersession_guard::TemporalWriteOutcome> {
    let (entry, meta) = trait_write(agent_id, &key.subject, predicate, value, origin, origin_trust);
    engine
        .store_temporal_outcome(agent_id, entry, meta, provenance)
        .await
}

/// [`record_trait_outcome`] for a trait the person **explicitly** asked to be
/// remembered (MCP `user_profile_record`): the row's metadata carries
/// `{"explicit": true}` so the "my memory" view can tell it apart from a
/// distilled trait (both share the `user_profile` origin class).
#[allow(clippy::too_many_arguments)]
pub async fn record_explicit_trait_outcome(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
) -> Result<crate::supersession_guard::TemporalWriteOutcome> {
    record_explicit_trait_outcome_for(
        engine,
        agent_id,
        &ProfileKey::legacy(user_id),
        predicate,
        value,
        origin,
        origin_trust,
        provenance,
        None,
    )
    .await
}

/// [`record_explicit_trait_outcome`] under `key.subject` (v1.84.0).
/// `reader` = the caller's own read scope
/// ([`SqliteMemoryEngine::store_temporal_outcome_for`]; `None` = judged from
/// the write's lineage): supersession and the trust guard only consider the
/// rows that conversation may see.
#[allow(clippy::too_many_arguments)]
pub async fn record_explicit_trait_outcome_for(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &ProfileKey,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
    reader: Option<RowScope<'_>>,
) -> Result<crate::supersession_guard::TemporalWriteOutcome> {
    let (entry, mut meta) =
        trait_write(agent_id, &key.subject, predicate, value, origin, origin_trust);
    meta.metadata = Some(serde_json::json!({ "explicit": true }));
    engine
        .store_temporal_outcome_for(agent_id, entry, meta, provenance, reader)
        .await
}

/// Hold a refused trait inert for human review
/// ([`SqliteMemoryEngine::hold_refused_claim`]). Returns the held row id.
#[allow(clippy::too_many_arguments)]
pub async fn hold_trait(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
) -> Result<String> {
    Ok(hold_trait_outcome(
        engine,
        agent_id,
        user_id,
        predicate,
        value,
        origin,
        origin_trust,
        provenance,
    )
    .await?
    .id)
}

/// [`hold_trait`], reporting whether the trait was newly held or an identical
/// one was already pending review
/// ([`SqliteMemoryEngine::hold_refused_claim_outcome`]).
#[allow(clippy::too_many_arguments)]
pub async fn hold_trait_outcome(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
) -> Result<crate::supersession_guard::HeldClaim> {
    let (entry, meta) =
        trait_write(agent_id, &user_subject(user_id), predicate, value, origin, origin_trust);
    engine
        .hold_refused_claim_outcome(agent_id, entry, meta, provenance)
        .await
}

/// [`hold_trait_outcome`] with an admission gate for NEW held rows
/// ([`SqliteMemoryEngine::hold_refused_claim_gated`]): `Ok(None)` when the
/// gate refused (nothing written).
#[allow(clippy::too_many_arguments)]
pub async fn hold_trait_gated(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
    admit: &mut (dyn FnMut() -> bool + Send),
) -> Result<Option<crate::supersession_guard::HeldClaim>> {
    hold_trait_gated_for(
        engine,
        agent_id,
        &ProfileKey::legacy(user_id),
        predicate,
        value,
        origin,
        origin_trust,
        provenance,
        admit,
    )
    .await
}

/// [`hold_trait_gated`] under `key.subject` (v1.84.0).
#[allow(clippy::too_many_arguments)]
pub async fn hold_trait_gated_for(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &ProfileKey,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
    provenance: crate::lineage::Provenance,
    admit: &mut (dyn FnMut() -> bool + Send),
) -> Result<Option<crate::supersession_guard::HeldClaim>> {
    let (entry, meta) = trait_write(agent_id, &key.subject, predicate, value, origin, origin_trust);
    engine
        .hold_refused_claim_gated(agent_id, entry, meta, provenance, admit)
        .await
}

/// The entry + temporal metadata one trait write stores under `subject`.
fn trait_write(
    agent_id: &str,
    subject: &str,
    predicate: &str,
    value: &str,
    origin: &str,
    origin_trust: f64,
) -> (MemoryEntry, TemporalMeta) {
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: format!("{predicate}: {value}"),
        timestamp: chrono::Utc::now(),
        tags: vec!["user-profile".to_string()],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 6.0,
        access_count: 0,
        last_accessed: None,
        source_event: "user_profile".to_string(),
    };
    let meta = TemporalMeta {
        subject: Some(subject.to_string()),
        predicate: Some(predicate.to_string()),
        object: Some(value.to_string()),
        origin: Some(origin.to_string()),
        origin_trust: Some(origin_trust.clamp(0.0, 1.0)),
        ..Default::default()
    };
    (entry, meta)
}

/// Fetch the currently-valid profile traits for a user, sorted by predicate.
/// Excludes the consolidated summary row.
pub async fn profile_traits(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
) -> Result<Vec<ProfileTrait>> {
    Ok(newest_per_predicate(
        profile_trait_rows(engine, agent_id, user_id).await?,
    ))
}

/// v1.83.0: every currently-valid trait row [`profile_traits`] reads, with its
/// memory id, before the one-per-predicate pick (predicate ascending, newest
/// first within a predicate). A reader serving one conversation drops the
/// rows it may not return, then picks with [`newest_per_predicate`].
pub async fn profile_trait_rows(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
) -> Result<Vec<(String, ProfileTrait)>> {
    profile_trait_rows_for(engine, agent_id, &ProfileKey::legacy(user_id), RowScope::All).await
}

/// v1.84.0: [`profile_trait_rows`] for `key`, keeping only the rows of
/// `scope` (the visibility rule is part of the query). When no row of
/// `key.subject` is visible to this reader and the key allows it, the rows
/// of `key.legacy_subject` are read instead (same scope). Whether qualified
/// rows "exist" is judged on visible rows only, so another conversation's
/// rows can not change what this reader gets.
pub async fn profile_trait_rows_for(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &ProfileKey,
    scope: RowScope<'_>,
) -> Result<Vec<(String, ProfileTrait)>> {
    let rows = subject_trait_rows(engine, agent_id, &key.subject, scope).await?;
    match &key.legacy_subject {
        Some(legacy) if rows.is_empty() => subject_trait_rows(engine, agent_id, legacy, scope).await,
        _ => Ok(rows),
    }
}

/// Every currently-valid trait row of `subject` (summary excluded) within
/// `scope`, predicate ascending, newest first within a predicate.
async fn subject_trait_rows(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    subject: &str,
    scope: RowScope<'_>,
) -> Result<Vec<(String, ProfileTrait)>> {
    let conn = engine.conn_for_maintenance().await;
    let sql = format!(
        "SELECT id, predicate, object, content
         FROM memories
         WHERE agent_id = ?1 AND subject = ?2 AND valid_until IS NULL
           AND predicate IS NOT NULL AND predicate != ?3{visible}
         ORDER BY predicate ASC, COALESCE(valid_from, timestamp) DESC",
        visible = scope.and_visible("memories", "?4"),
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let session = scope.session();
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&agent_id, &subject, &SUMMARY_PREDICATE];
    if scope.is_scoped() {
        bound.push(&session);
    }
    let rows = stmt
        .query_map(bound.as_slice(), |r| {
            let id: String = r.get(0)?;
            let predicate: String = r.get(1)?;
            let object: Option<String> = r.get(2)?;
            let content: String = r.get(3)?;
            Ok((id, predicate, object, content))
        })
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, predicate, object, content) =
            row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let value = object.unwrap_or(content);
        out.push((id, ProfileTrait { predicate, value }));
    }
    Ok(out)
}

/// v1.84.0: [`profile_traits`] for `key` within `scope`.
pub async fn profile_traits_for(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &ProfileKey,
    scope: RowScope<'_>,
) -> Result<Vec<ProfileTrait>> {
    Ok(newest_per_predicate(
        profile_trait_rows_for(engine, agent_id, key, scope).await?,
    ))
}

/// v1.84.0: [`profile_block`] for `key` within `scope`.
pub async fn profile_block_for(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &ProfileKey,
    scope: RowScope<'_>,
) -> Result<Option<String>> {
    let traits = profile_traits_for(engine, agent_id, key, scope).await?;
    Ok(render_profile_block(&traits))
}

/// One trait per predicate from [`profile_trait_rows`] (the newest valid row
/// wins — first seen given the DESC recency order). Deterministic dedup keyed
/// by predicate.
pub fn newest_per_predicate(rows: Vec<(String, ProfileTrait)>) -> Vec<ProfileTrait> {
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|(_, t)| seen.insert(t.predicate.clone()))
        .map(|(_, t)| t)
        .collect()
}

/// Render the injectable `## About This User` block from a trait set, or `None`
/// when there is nothing to inject. Deterministic (traits already predicate-
/// sorted) so the system-prompt bytes are stable across turns.
pub fn render_profile_block(traits: &[ProfileTrait]) -> Option<String> {
    if traits.is_empty() {
        return None;
    }
    let mut s = String::from("## About This User\n");
    for t in traits {
        s.push_str(&format!("- {}: {}\n", t.predicate, t.value));
    }
    Some(s)
}

/// Convenience: fetch + render in one call.
pub async fn profile_block(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
) -> Result<Option<String>> {
    let traits = profile_traits(engine, agent_id, user_id).await?;
    Ok(render_profile_block(&traits))
}

/// Consolidate the current traits into one durable summary memory when the user
/// has at least `threshold` distinct traits. Deterministic synthesis (no LLM):
/// the sorted `predicate: value` lines joined into one Semantic memory under
/// `predicate = "profile_summary"`, which auto-supersedes any prior summary.
/// Returns the new summary memory id, or `None` if below threshold.
pub async fn consolidate_profile(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
    threshold: usize,
) -> Result<Option<String>> {
    consolidate_profile_for(engine, agent_id, &ProfileKey::legacy(user_id), RowScope::All, threshold)
        .await
}

/// v1.84.0: [`consolidate_profile`] for `key`, built only from the trait
/// rows of `scope`. In a conversation the summary is computed from that
/// conversation's traits alone (never another conversation's); it inherits
/// their lineage, so it is itself a row of that conversation and supersedes
/// only that conversation's previous summary.
pub async fn consolidate_profile_for(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &ProfileKey,
    scope: RowScope<'_>,
    threshold: usize,
) -> Result<Option<String>> {
    let rows = profile_trait_rows_for(engine, agent_id, key, scope).await?;
    // The summary is derived from the current trait rows: `derived_from`
    // makes the engine clamp its trust to the least trusted of them, so a
    // summary of distilled traits never outranks the traits themselves.
    let source_ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    let traits = newest_per_predicate(rows);
    if traits.len() < threshold || source_ids.is_empty() {
        return Ok(None);
    }
    let summary = traits
        .iter()
        .map(|t| format!("{}: {}", t.predicate, t.value))
        .collect::<Vec<_>>()
        .join("; ");

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: format!("User profile — {summary}"),
        timestamp: chrono::Utc::now(),
        tags: vec!["user-profile".to_string(), "profile-summary".to_string()],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 7.0,
        access_count: 0,
        last_accessed: None,
        source_event: "user_profile_consolidation".to_string(),
    };
    let meta = TemporalMeta {
        subject: Some(key.subject.clone()),
        predicate: Some(SUMMARY_PREDICATE.to_string()),
        object: Some(summary),
        confidence: Some(0.9),
        origin: Some(DEFAULT_PROFILE_ORIGIN.to_string()),
        derived_from: Some(source_ids.clone()),
        ..Default::default()
    };
    // P2-B: the summary inherits every source of the traits it summarizes.
    let provenance = crate::lineage::Provenance::derived(source_ids);
    match engine
        .store_temporal_outcome(agent_id, entry, meta, provenance)
        .await?
    {
        crate::supersession_guard::TemporalWriteOutcome::Stored(id) => Ok(Some(id)),
        // A trait it was built from was forgotten (or deleted) after it was
        // read: no summary this time.
        crate::supersession_guard::TemporalWriteOutcome::Fenced(r) => {
            tracing::warn!(agent = agent_id, "profile summary not written: {r}");
            Ok(None)
        }
        // An older summary is more trusted than the traits it is rebuilt from
        // now (e.g. a trait was re-recorded at lower trust): keep it rather
        // than fail the caller; the next consolidation retries.
        crate::supersession_guard::TemporalWriteOutcome::Refused(r) => {
            tracing::warn!(agent = agent_id, "profile summary not replaced: {r}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::SqliteMemoryEngine;

    #[tokio::test]
    async fn record_supersede_and_render() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        record_trait(&engine, "a", "u1", "prefers", "tea", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        record_trait(&engine, "a", "u1", "timezone", "Asia/Taipei", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        // Supersede the first trait.
        record_trait(&engine, "a", "u1", "prefers", "coffee", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();

        let traits = profile_traits(&engine, "a", "u1").await.unwrap();
        assert_eq!(traits.len(), 2, "one row per predicate after supersession");
        let prefers = traits.iter().find(|t| t.predicate == "prefers").unwrap();
        assert_eq!(prefers.value, "coffee", "latest value wins");

        let block = profile_block(&engine, "a", "u1").await.unwrap().unwrap();
        // Deterministic: predicate-sorted → "prefers" before "timezone".
        assert_eq!(
            block,
            "## About This User\n- prefers: coffee\n- timezone: Asia/Taipei\n"
        );
    }

    #[tokio::test]
    async fn empty_profile_no_block() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        assert!(profile_block(&engine, "a", "nobody")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn consolidation_respects_threshold_and_supersedes() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        record_trait(&engine, "a", "u1", "prefers", "tea", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        // Below threshold(3) → no summary.
        assert!(consolidate_profile(&engine, "a", "u1", 3)
            .await
            .unwrap()
            .is_none());

        record_trait(&engine, "a", "u1", "timezone", "Asia/Taipei", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        record_trait(&engine, "a", "u1", "language", "zh-TW", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        let first = consolidate_profile(&engine, "a", "u1", 3).await.unwrap();
        assert!(first.is_some(), "at threshold → summary written");

        // Re-consolidating with identical traits reaffirms the existing summary
        // (D1: same subject/predicate/object + content are re-observed, not
        // changed) instead of churning a new row — the id is stable and there is
        // still exactly one valid summary. (Pre-D1 this always superseded and
        // minted a new id; reaffirm is the intended anti-bloat behavior.)
        let second = consolidate_profile(&engine, "a", "u1", 3).await.unwrap();
        assert_eq!(first, second, "identical re-consolidation reaffirms (stable id)");

        let history = engine
            .get_history("a", &user_subject("u1"), SUMMARY_PREDICATE)
            .await
            .unwrap();
        let valid = history.iter().filter(|r| r.valid_until.is_none()).count();
        assert_eq!(valid, 1, "exactly one currently-valid summary");
    }

    #[test]
    fn stable_user_id_drops_only_the_webchat_connection_nonce() {
        // WebChat: the trailing per-connection nonce is dropped…
        assert_eq!(stable_user_id("webchat:owner-alice:1a2b3c4d"), "webchat:owner-alice");
        // …including when the owner tag itself contains colons.
        assert_eq!(stable_user_id("webchat:user:alice:1a2b3c4d"), "webchat:user:alice");
        // Nothing to drop → unchanged.
        assert_eq!(stable_user_id("webchat:alice"), "webchat:alice");
        // Every other channel supplies an already-stable id.
        for id in ["u123", "telegram_88991", "U04ABCDEF", "anonymous"] {
            assert_eq!(stable_user_id(id), id);
        }
    }

    #[tokio::test]
    async fn webchat_profile_survives_a_reconnect() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        // Recorded on one WebSocket connection…
        record_trait(&engine, "a", "webchat:alice:aaaa1111", "prefers", "tea", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        // …must still be visible after a page reload mints a new nonce.
        let block = profile_block(&engine, "a", "webchat:alice:bbbb2222")
            .await
            .unwrap()
            .expect("profile must survive the reconnect");
        assert!(block.contains("tea"), "block: {block}");
    }

    #[test]
    fn profile_subject_carries_the_channel() {
        assert_eq!(profile_subject("telegram", "998").as_deref(), Some("user:telegram:998"));
        assert_eq!(profile_subject(" Telegram ", " 998 ").as_deref(), Some("user:telegram:998"));
        assert_ne!(profile_subject("telegram", "998"), profile_subject("wecom", "998"));
        // WebChat keeps its already-qualified pre-v1.84 form byte for byte.
        assert_eq!(
            profile_subject("webchat", "webchat:owner-alice:1a2b").as_deref(),
            Some(user_subject("webchat:owner-alice:1a2b").as_str())
        );
        assert_eq!(profile_subject("webchat", "owner-alice").as_deref(), Some("user:webchat:owner-alice"));
        for (channel, id) in [("", "998"), ("tele gram", "998"), ("tg:x", "998"), ("telegram", "  ")] {
            assert!(profile_subject(channel, id).is_none(), "{channel:?} {id:?}");
        }
        assert_eq!(channel_of_session("telegram:998:thread#agent:a1").as_deref(), Some("telegram"));
        assert_eq!(channel_of_session("LINE:U1").as_deref(), Some("line"));
        assert!(channel_of_session("telegram").is_none());
        assert!(channel_of_session("telegram:").is_none());
        // The legacy bare subject is a fallback only when allowed.
        let key = ProfileKey::for_channel("line", "U1", true).unwrap();
        assert_eq!(key.legacy_subject.as_deref(), Some("user:U1"));
        assert!(ProfileKey::for_channel("telegram", "998", false).unwrap().legacy_subject.is_none());
        assert!(ProfileKey::for_channel("webchat", "webchat:o:n", true).unwrap().legacy_subject.is_none());
    }

    fn from(session: &str, seq: i64) -> crate::lineage::Provenance {
        crate::lineage::Provenance::source(crate::lineage::SourceRef::channel_message(
            session,
            seq,
            chrono::Utc::now(),
            None,
        ))
    }

    /// Item 4: two people with the same raw id on different platforms get
    /// different profiles; a legacy bare row is read only when the platform's
    /// ids are globally unique and nothing qualified is visible.
    #[tokio::test]
    async fn same_raw_id_on_two_platforms_is_two_profiles() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let tg = ProfileKey::for_channel("telegram", "998", false).unwrap();
        let wc = ProfileKey::for_channel("wecom", "998", false).unwrap();
        record_trait_outcome_for(&engine, "a", &tg, "prefers", "tea", "user_profile", 1.0, from("telegram:998", 1))
            .await
            .unwrap();
        record_trait_outcome_for(&engine, "a", &wc, "prefers", "coffee", "user_profile", 1.0, from("wecom:998", 2))
            .await
            .unwrap();
        let value = |t: Vec<ProfileTrait>| t.into_iter().map(|t| t.value).collect::<Vec<_>>();
        assert_eq!(value(profile_traits_for(&engine, "a", &tg, RowScope::All).await.unwrap()), vec!["tea"]);
        assert_eq!(value(profile_traits_for(&engine, "a", &wc, RowScope::All).await.unwrap()), vec!["coffee"]);

        // Legacy bare rows (pre-v1.84, e.g. untouched by the migration).
        record_trait(&engine, "a", "U1", "timezone", "Asia/Taipei", 1.0, from("line:U1", 3)).await.unwrap();
        record_trait(&engine, "a", "777", "timezone", "UTC", 1.0, from("telegram:777", 4)).await.unwrap();
        let line = ProfileKey::for_channel("line", "U1", true).unwrap();
        assert_eq!(value(profile_traits_for(&engine, "a", &line, RowScope::All).await.unwrap()), vec!["Asia/Taipei"]);
        let in_line_chat = RowScope::conversation(Some("line:U1"));
        assert_eq!(value(profile_traits_for(&engine, "a", &line, in_line_chat).await.unwrap()), vec!["Asia/Taipei"]);
        let telegram = ProfileKey::for_channel("telegram", "777", false).unwrap();
        assert!(profile_traits_for(&engine, "a", &telegram, RowScope::All).await.unwrap().is_empty());
        // Once a qualified row is visible, the legacy rows are not read.
        record_trait_outcome_for(&engine, "a", &line, "prefers", "oolong", "user_profile", 1.0, from("line:U1", 5))
            .await
            .unwrap();
        assert_eq!(value(profile_traits_for(&engine, "a", &line, in_line_chat).await.unwrap()), vec!["oolong"]);
    }

    /// Item 5: each conversation keeps its own value for a trait, and the
    /// summary consolidated in a conversation uses only that conversation's
    /// traits and replaces only that conversation's summary.
    #[tokio::test]
    async fn traits_and_summaries_stay_in_their_conversation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let key = ProfileKey::for_channel("telegram", "alice", false).unwrap();
        let (a, b) = (RowScope::conversation(Some("telegram:A")), RowScope::conversation(Some("telegram:B")));
        record_trait_outcome_for(&engine, "a", &key, "prefers", "tea", "user_profile", 1.0, from("telegram:A", 1))
            .await
            .unwrap();
        record_trait_outcome_for(&engine, "a", &key, "prefers", "coffee", "user_profile", 1.0, from("telegram:B", 2))
            .await
            .unwrap();
        let value = |t: Vec<ProfileTrait>| t.into_iter().map(|t| t.value).collect::<Vec<_>>();
        assert_eq!(value(profile_traits_for(&engine, "a", &key, a).await.unwrap()), vec!["tea"]);
        assert_eq!(value(profile_traits_for(&engine, "a", &key, b).await.unwrap()), vec!["coffee"]);

        let summary_a = consolidate_profile_for(&engine, "a", &key, a, 1).await.unwrap().unwrap();
        let summary_b = consolidate_profile_for(&engine, "a", &key, b, 1).await.unwrap().unwrap();
        assert_ne!(summary_a, summary_b);
        let history = engine.get_history("a", &key.subject, SUMMARY_PREDICATE).await.unwrap();
        let valid: Vec<&str> = history.iter().filter(|r| r.valid_until.is_none()).map(|r| r.content.as_str()).collect();
        assert_eq!(valid.len(), 2, "B's summary does not replace A's: {valid:?}");
        let a_summary = history.iter().find(|r| r.id == summary_a).unwrap();
        assert!(a_summary.content.contains("tea") && !a_summary.content.contains("coffee"), "{}", a_summary.content);
        let hidden = engine
            .ids_hidden_from_conversation("a", &[summary_a.clone(), summary_b.clone()], Some("telegram:A"))
            .await
            .unwrap();
        assert!(!hidden.contains(&summary_a) && hidden.contains(&summary_b));
    }

    #[tokio::test]
    async fn cross_agent_isolation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        record_trait(&engine, "a1", "u1", "prefers", "tea", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        assert!(profile_block(&engine, "a2", "u1").await.unwrap().is_none());
    }
}
