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

use crate::engine::SqliteMemoryEngine;
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
pub fn user_subject(user_id: &str) -> String {
    format!("user:{}", stable_user_id(user_id))
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
    let (entry, meta) = trait_write(agent_id, user_id, predicate, value, origin, origin_trust);
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
    let (entry, meta) = trait_write(agent_id, user_id, predicate, value, origin, origin_trust);
    engine
        .store_temporal_outcome(agent_id, entry, meta, provenance)
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
    let (entry, meta) = trait_write(agent_id, user_id, predicate, value, origin, origin_trust);
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
    let (entry, meta) = trait_write(agent_id, user_id, predicate, value, origin, origin_trust);
    engine
        .hold_refused_claim_gated(agent_id, entry, meta, provenance, admit)
        .await
}

/// The entry + temporal metadata one trait write stores.
fn trait_write(
    agent_id: &str,
    user_id: &str,
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
        subject: Some(user_subject(user_id)),
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
    let subject = user_subject(user_id);
    let conn = engine.conn_for_maintenance().await;
    let mut stmt = conn
        .prepare(
            "SELECT predicate, object, content
             FROM memories
             WHERE agent_id = ?1 AND subject = ?2 AND valid_until IS NULL
               AND predicate IS NOT NULL AND predicate != ?3
             ORDER BY predicate ASC, COALESCE(valid_from, timestamp) DESC",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let rows = stmt
        .query_map(
            rusqlite::params![agent_id, subject, SUMMARY_PREDICATE],
            |r| {
                let predicate: String = r.get(0)?;
                let object: Option<String> = r.get(1)?;
                let content: String = r.get(2)?;
                Ok((predicate, object, content))
            },
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;

    // One trait per predicate (the newest valid row wins — first seen given the
    // DESC recency order). Deterministic dedup keyed by predicate.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let (predicate, object, content) = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        if !seen.insert(predicate.clone()) {
            continue;
        }
        let value = object.unwrap_or(content);
        out.push(ProfileTrait { predicate, value });
    }
    Ok(out)
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
    let traits = profile_traits(engine, agent_id, user_id).await?;
    if traits.len() < threshold {
        return Ok(None);
    }
    let summary = traits
        .iter()
        .map(|t| format!("{}: {}", t.predicate, t.value))
        .collect::<Vec<_>>()
        .join("; ");
    // The summary is derived from the current trait rows: `derived_from`
    // makes the engine clamp its trust to the least trusted of them, so a
    // summary of distilled traits never outranks the traits themselves.
    let source_ids = current_trait_ids(engine, agent_id, user_id).await?;

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
        subject: Some(user_subject(user_id)),
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

/// Ids of the currently valid trait rows [`profile_traits`] reads (summary
/// excluded), for the summary's `derived_from`.
async fn current_trait_ids(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    user_id: &str,
) -> Result<Vec<String>> {
    let subject = user_subject(user_id);
    let conn = engine.conn_for_maintenance().await;
    let mut stmt = conn
        .prepare(
            "SELECT id FROM memories
             WHERE agent_id = ?1 AND subject = ?2 AND valid_until IS NULL
               AND predicate IS NOT NULL AND predicate != ?3",
        )
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let rows = stmt
        .query_map(rusqlite::params![agent_id, subject, SUMMARY_PREDICATE], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|e| DuDuClawError::Memory(e.to_string()))?;
    let mut ids = Vec::new();
    for r in rows {
        ids.push(r.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
    }
    Ok(ids)
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

    #[tokio::test]
    async fn cross_agent_isolation() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        record_trait(&engine, "a1", "u1", "prefers", "tea", 1.0, crate::lineage::Provenance::test_only())
            .await
            .unwrap();
        assert!(profile_block(&engine, "a2", "u1").await.unwrap().is_none());
    }
}
