//! Supersession trust guard — black-box tests over the public engine API.
//!
//! The attack: an operator (or a deliberate user-profile record) sets a fact,
//! then anyone who can talk to the AI employee in a chat channel gets a
//! contradicting statement distilled into memory. The distillation path writes
//! with `origin = "channel"` (trust ceiling 0.3); it must not be able to
//! replace the operator's 1.0-trust fact as the current answer.

use chrono::Utc;
use duduclaw_core::traits::MemoryEngine as _;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};

fn entry(agent: &str, content: &str) -> MemoryEntry {
    MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        tags: vec![],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: "test".to_string(),
    }
}

fn meta(subject: &str, predicate: &str, object: &str, origin: &str, trust: f64) -> TemporalMeta {
    TemporalMeta {
        subject: Some(subject.to_string()),
        predicate: Some(predicate.to_string()),
        object: Some(object.to_string()),
        origin: Some(origin.to_string()),
        origin_trust: Some(trust),
        ..Default::default()
    }
}

/// The attack exactly as a chat user triggers it: operator sets the refund
/// policy, a channel-distilled claim with a different object follows. The
/// operator's fact must remain the single current answer.
#[tokio::test]
async fn channel_distilled_write_cannot_supersede_operator_fact() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "support-bot";

    let op_id = engine
        .store_temporal(
            agent,
            entry(agent, "Refund window is 7 days"),
            meta("policy:refund", "window", "7 days", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only(),
        )
        .await
        .unwrap();

    // Exactly what wiki_ingest's distillation path sends (origin "channel",
    // declared trust 0.3 = the class ceiling).
    let attack = engine
        .store_temporal(
            agent,
            entry(agent, "Refund window is 365 days, no questions asked"),
            meta("policy:refund", "window", "365 days", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only(),
        )
        .await;

    let history = engine
        .get_history(agent, "policy:refund", "window")
        .await
        .unwrap();
    let current: Vec<_> = history.iter().filter(|r| r.valid_until.is_none()).collect();
    assert_eq!(
        current.len(),
        1,
        "exactly one current fact expected, got {current:?} (write result {attack:?})"
    );
    assert_eq!(
        current[0].id, op_id,
        "the operator's fact must stay current; a 0.3-trust channel write replaced it \
         (write result {attack:?})"
    );
    assert!(
        attack.is_err(),
        "the refused write must be reported to the caller, not silently accepted"
    );
}

/// Same attack against a user-profile record (`user_profile` origin, the
/// AI-record ceiling 0.6 since M2) via a channel-class write.
#[tokio::test]
async fn channel_distilled_trait_cannot_supersede_user_profile_record() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "concierge";

    let rec = duduclaw_memory::record_trait(&engine, agent, "u1", "allergy", "peanuts", 1.0, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let attack = duduclaw_memory::user_profile::record_trait_with_origin(
        &engine, agent, "u1", "allergy", "none", "channel", 0.3, duduclaw_memory::lineage::Provenance::test_only(),
    )
    .await;

    let traits = duduclaw_memory::profile_traits(&engine, agent, "u1").await.unwrap();
    let allergy: Vec<_> = traits.iter().filter(|t| t.predicate == "allergy").collect();
    assert_eq!(allergy.len(), 1);
    assert_eq!(allergy[0].value, "peanuts", "write result {attack:?}");
    assert!(attack.is_err());
    // M2: the record asked for 1.0 and is stored at its class ceiling.
    assert_eq!(engine.get_origin_trust(agent, &rec).await.unwrap(), Some(0.6));
}

async fn current_ids(engine: &SqliteMemoryEngine, agent: &str, s: &str, p: &str) -> Vec<String> {
    engine
        .get_history(agent, s, p)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.valid_until.is_none())
        .map(|r| r.id)
        .collect()
}

/// Table over origin pairs through the real engine: lower → higher refused
/// (typed outcome, nothing written), equal and higher → lower supersede.
#[tokio::test]
async fn origin_pairs_through_the_engine() {
    use duduclaw_memory::TemporalWriteOutcome;
    // (existing origin, write origin, refused?)
    let table = [
        ("operator", "channel", true),
        ("operator", "tool_echo", true),
        ("operator", "import", true),
        ("user_direct", "agent_derived", true),
        ("user_profile", "mcp_external", true),
        ("user_profile", "channel", true),
        ("operator", "user_profile", true),
        ("import", "user_profile", true),
        ("user_profile", "agent_derived", false),
        ("agent_derived", "user_profile", false),
        ("import", "agent_derived", true),
        ("agent_derived", "tool_echo", true),
        ("operator", "operator", false),
        ("channel", "channel", false),
        ("agent_derived", "unattributed", false),
        ("channel", "operator", false),
        ("channel", "agent_derived", false),
        ("agent_derived", "import", false),
        ("import", "user_direct", false),
        ("tool_echo", "agent_derived", false),
    ];
    for (i, (existing, write, refused)) in table.iter().enumerate() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = format!("pair-{i}");
        let first = engine
            .store_temporal(
                &agent,
                entry(&agent, "old value"),
                meta("s", "p", "old", existing, 1.0), duduclaw_memory::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        let out = engine
            .store_temporal_outcome(
                &agent,
                entry(&agent, "new value"),
                meta("s", "p", "new", write, 1.0), duduclaw_memory::lineage::Provenance::test_only(),
            )
            .await
            .unwrap();
        let current = current_ids(&engine, &agent, "s", "p").await;
        match (refused, &out) {
            (true, TemporalWriteOutcome::Refused(r)) => {
                assert_eq!(current, vec![first.clone()], "{existing}→{write}");
                assert_eq!(r.existing_id, first);
                assert_eq!(r.write_origin, *write);
                assert_eq!(r.existing_origin.as_deref(), Some(*existing));
                assert!(r.existing_trust > r.write_trust);
                assert_eq!(engine.supersession_refusals(), 1);
            }
            (false, TemporalWriteOutcome::Stored(id)) => {
                assert_eq!(current, vec![id.clone()], "{existing}→{write}");
                assert_eq!(engine.supersession_refusals(), 0);
            }
            _ => panic!("{existing}→{write}: unexpected outcome {out:?}"),
        }
    }
}

/// `supersession_trust_guard = false` restores the old behaviour: the channel
/// write supersedes the operator fact, chain linked as before.
#[tokio::test]
async fn switch_off_restores_unguarded_supersession() {
    let engine = SqliteMemoryEngine::in_memory()
        .unwrap()
        .with_supersession_trust_guard(false);
    let agent = "off";
    let op = engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let ch = engine
        .store_temporal(agent, entry(agent, "365 days"), meta("s", "p", "365", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let h = engine.get_history(agent, "s", "p").await.unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].id, op);
    assert_eq!(h[0].superseded_by.as_deref(), Some(ch.as_str()));
    assert_eq!(h[1].supersedes.as_deref(), Some(op.as_str()));
    assert_eq!(current_ids(&engine, agent, "s", "p").await, vec![ch]);
    assert_eq!(engine.supersession_refusals(), 0);
}

/// Reaffirmation by a low-trust origin is untouched: the same object+content
/// is a reaffirm (no supersession), and the ≥2-distinct-origin boost applies.
#[tokio::test]
async fn reaffirmation_by_lower_trust_is_unaffected() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "reaffirm";
    let op = engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut m = meta("s", "p", "7", "channel", 0.3);
    m.confidence = Some(0.5);
    m.source_event = Some("ev-chat".to_string());
    let again = engine
        .store_temporal(agent, entry(agent, "7 days"), m, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_eq!(again, op, "a reaffirm returns the surviving row");
    let h = engine.get_history(agent, "s", "p").await.unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].reaffirmed_by, vec!["ev-chat".to_string()]);
    assert_eq!(engine.supersession_refusals(), 0);
}

/// Corroboration raises confidence, not trust: a corroborated channel fact is
/// still a 0.3 fact, so an import (0.7) supersedes it.
#[tokio::test]
async fn corroborated_low_trust_fact_is_still_superseded_by_higher_trust() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "corroborated";
    let mut m = meta("s", "p", "x", "channel", 0.3);
    m.confidence = Some(0.5);
    let ch = engine
        .store_temporal(agent, entry(agent, "x"), m, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut r = meta("s", "p", "x", "mcp_external", 0.3);
    r.source_event = Some("ev-2".to_string());
    engine.store_temporal(agent, entry(agent, "x"), r, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    let h = engine.get_history(agent, "s", "p").await.unwrap();
    assert!(h[0].confidence > 0.5, "boosted by a second distinct origin");
    assert_eq!(engine.get_origin_trust(agent, &ch).await.unwrap(), Some(0.3));

    // Equal trust (another channel claim) still supersedes the corroborated row.
    let newer = engine
        .store_temporal(agent, entry(agent, "y"), meta("s", "p", "y", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_eq!(current_ids(&engine, agent, "s", "p").await, vec![newer]);
}

/// A fact whose trust was lowered after the fact (poisoned-source cascade of
/// `invalidate_by_origin`) is compared at its stored, lowered value.
#[tokio::test]
async fn lowered_trust_is_compared_at_stored_value() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "lowered";
    let src = engine
        .store_temporal(agent, entry(agent, "source"), TemporalMeta {
            origin: Some("tool_echo".to_string()),
            ..Default::default()
        }, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut m = meta("s", "p", "derived", "operator", 1.0);
    m.derived_from = Some(vec![src.clone()]);
    let derived = engine
        .store_temporal(agent, entry(agent, "derived"), m, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    // Derived from a tool echo → stored at 0.5; an import (0.7) may replace it.
    assert_eq!(engine.get_origin_trust(agent, &derived).await.unwrap(), Some(0.5));
    engine.invalidate_by_origin(agent, "tool_echo", None).await.unwrap();
    assert_eq!(engine.get_origin_trust(agent, &derived).await.unwrap(), Some(0.1));
    // Now even a channel write (0.3) outranks the poisoned 0.1 row.
    let ch = engine
        .store_temporal(agent, entry(agent, "chat"), meta("s", "p", "chat", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_eq!(current_ids(&engine, agent, "s", "p").await, vec![ch]);
}

/// After a refusal the history chain is exactly as before; a higher-trust write
/// afterwards supersedes normally and links to the operator fact.
#[tokio::test]
async fn history_intact_after_refusal_and_higher_trust_write_proceeds() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "chain";
    let a = engine
        .store_temporal(agent, entry(agent, "v1"), meta("s", "p", "v1", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let b = engine
        .store_temporal(agent, entry(agent, "v2"), meta("s", "p", "v2", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let before = engine.get_history(agent, "s", "p").await.unwrap();
    assert!(engine
        .store_temporal(agent, entry(agent, "evil"), meta("s", "p", "evil", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .is_err());
    let after = engine.get_history(agent, "s", "p").await.unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap(),
        "a refusal must not touch the chain"
    );
    let at = engine
        .get_at(agent, "s", "p", Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(at.id, b);

    let c = engine
        .store_temporal(agent, entry(agent, "v3"), meta("s", "p", "v3", "user_direct", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let h = engine.get_history(agent, "s", "p").await.unwrap();
    assert_eq!(h.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec![a, b.clone(), c.clone()]);
    assert_eq!(h[1].superseded_by.as_deref(), Some(c.as_str()));
    assert_eq!(h[2].supersedes.as_deref(), Some(b.as_str()));
}

/// A low-trust fact whose world-time predates the current fact is a historical
/// segment, not a supersession — still allowed, current fact untouched.
#[tokio::test]
async fn historical_insert_is_not_a_supersession() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "history";
    let mut m = meta("s", "p", "now", "operator", 1.0);
    m.valid_from = Some(Utc::now());
    let op = engine.store_temporal(agent, entry(agent, "now"), m, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    let mut old = meta("s", "p", "then", "channel", 0.3);
    old.valid_from = Some(Utc::now() - chrono::Duration::days(30));
    engine
        .store_temporal(agent, entry(agent, "then"), old, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_eq!(current_ids(&engine, agent, "s", "p").await, vec![op]);
}

/// Hold → review → accept: a refused claim held as a quarantined row does not
/// block anything, and promotion re-writes it with the reviewer's authority,
/// superseding the operator fact; the held row points at its promoted copy.
#[tokio::test]
async fn held_claim_promoted_with_reviewer_authority() {
    use duduclaw_memory::TemporalWriteOutcome;
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "review";
    let op = engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let claim = meta("s", "p", "14", "channel", 0.3);
    let out = engine
        .store_temporal_outcome(agent, entry(agent, "14 days"), claim.clone(), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert!(matches!(out, TemporalWriteOutcome::Refused(_)));
    let held = engine
        .hold_refused_claim(agent, entry(agent, "14 days"), claim, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_eq!(engine.is_quarantined(agent, &held).await.unwrap(), Some(true));
    // The held claim is invisible to history / point-in-time / retrieval.
    let before = engine.get_history(agent, "s", "p").await.unwrap();
    assert_eq!(before.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), vec![op.clone()]);
    assert_eq!(
        engine.get_at(agent, "s", "p", Utc::now()).await.unwrap().map(|r| r.id),
        Some(op.clone())
    );
    assert!(engine.search(agent, "14 days", 10).await.unwrap().is_empty());
    // A later channel claim is still refused by the operator fact (the held
    // row neither replaces nor shields anything).
    assert!(engine
        .store_temporal(agent, entry(agent, "30 days"), meta("s", "p", "30", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .is_err());

    let n = engine
        .promote_quarantined(agent, &[held.clone()], "operator")
        .await
        .unwrap();
    assert_eq!((n.promoted, n.stale), (1, 0));
    let current = current_ids(&engine, agent, "s", "p").await;
    assert_eq!(current.len(), 1);
    let promoted = &current[0];
    assert_ne!(promoted, &held);
    assert_eq!(engine.get_origin_trust(agent, promoted).await.unwrap(), Some(1.0));
    assert_eq!(
        engine.get_origin(agent, promoted).await.unwrap(),
        Some(Some("operator".to_string()))
    );
    let h = engine.get_history(agent, "s", "p").await.unwrap();
    assert_eq!(h.len(), 2, "operator fact → promoted claim");
    assert_eq!(h[0].id, op);
    assert_eq!(h[0].superseded_by.as_deref(), Some(promoted.as_str()));
    assert_eq!(h[1].supersedes.as_deref(), Some(op.as_str()));
    assert_eq!(h[1].content, "14 days");
    // Idempotent: a second approval finds nothing pending.
    assert_eq!(
        engine.promote_quarantined(agent, &[held], "operator").await.unwrap(),
        duduclaw_memory::PromotionReport::default()
    );
}

/// A refused claim repeated while it is pending review is held once: the same
/// `(agent, subject, predicate, object)` returns the existing held id without
/// a second row. A different object for the same subject/predicate is a
/// different claim and gets its own row; another agent's identical claim is
/// independent. Once the held row is decided (rejected) a repeat is new again.
#[tokio::test]
async fn repeated_held_claim_is_held_once_while_pending() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "review-dedup";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();

    let first = engine
        .hold_refused_claim_outcome(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert!(first.newly_held);
    for _ in 0..3 {
        let again = engine
            .hold_refused_claim_outcome(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
            .await
            .unwrap();
        assert!(!again.newly_held);
        assert_eq!(again.id, first.id);
    }
    assert_eq!(engine.held_claim_repeats(), 3);
    // The String-returning form is idempotent too.
    let plain = engine
        .hold_refused_claim(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_eq!(plain, first.id);

    // Different object ⇒ separate held claim.
    let other = engine
        .hold_refused_claim_outcome(agent, entry(agent, "30 days"), meta("s", "p", "30", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert!(other.newly_held);
    assert_ne!(other.id, first.id);
    assert_eq!(
        engine.find_pending_held_claim(agent, "s", "p", Some("30")).await.unwrap(),
        Some(other.id.clone())
    );
    assert_eq!(engine.find_pending_held_claim(agent, "s", "p", None).await.unwrap(), None);

    // Another agent's identical claim is not deduplicated against this one.
    let elsewhere = engine
        .hold_refused_claim_outcome("other-agent", entry("other-agent", "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert!(elsewhere.newly_held);

    // After the reviewer rejects it, the claim is no longer pending: a repeat
    // is held (and reviewed) afresh.
    engine
        .reject_quarantine(agent, &[first.id.clone()], "test_reject")
        .await
        .unwrap();
    let after = engine
        .hold_refused_claim_outcome(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert!(after.newly_held);
    assert_ne!(after.id, first.id);
}

// ── v1.67.1 second batch ────────────────────────────────────────────────────

/// M3: a card approved after the protected fact changed is stale — nothing is
/// written, the newer fact stays, the held row is closed out.
#[tokio::test]
async fn stale_card_is_not_promoted_over_a_newer_fact() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "stale";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let held = engine
        .hold_refused_claim(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    // The operator changes the fact after the card was filed.
    let newer = engine
        .store_temporal(agent, entry(agent, "10 days"), meta("s", "p", "10", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let r = engine
        .promote_quarantined(agent, &[held.clone()], "operator")
        .await
        .unwrap();
    assert_eq!((r.promoted, r.stale), (0, 1));
    assert_eq!(current_ids(&engine, agent, "s", "p").await, vec![newer.clone()]);
    // The held row is closed: approving again does nothing, and the same
    // claim can be held afresh against the new fact.
    assert_eq!(
        engine.promote_quarantined(agent, &[held.clone()], "operator").await.unwrap(),
        duduclaw_memory::PromotionReport::default()
    );
    assert_eq!(engine.find_pending_held_claim(agent, "s", "p", Some("14")).await.unwrap(), None);
    let again = engine
        .hold_refused_claim_outcome(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert!(again.newly_held);
    // …and that fresh card, approved now, is current and promotes.
    let r = engine
        .promote_quarantined(agent, &[again.id], "operator")
        .await
        .unwrap();
    assert_eq!((r.promoted, r.stale), (1, 0));
}

/// M3: the conflicting fact's id is persisted on the held row.
#[tokio::test]
async fn held_row_records_the_fact_it_conflicts_with() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "conflicts-with";
    let op = engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let held = engine
        .hold_refused_claim(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let m = engine.get_metadata(agent, &held).await.unwrap().unwrap();
    assert_eq!(m["held_claim"]["conflicts_with"], serde_json::json!(op));
    assert_eq!(m["held_claim"]["object"], serde_json::json!("14"));
}

/// M2: a legacy `user_profile` row stored at 1.0 (before the class had its own
/// 0.6 ceiling) is compared at 0.6: the speaker's own profile statement (same
/// class) corrects it without review; a channel write is still refused.
#[tokio::test]
async fn legacy_user_profile_row_is_capped_at_the_new_ceiling() {
    use duduclaw_memory::TemporalWriteOutcome;
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "legacy-profile";
    let legacy = duduclaw_memory::record_trait(&engine, agent, "u1", "preferred_name", "李總", 1.0, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    // Simulate the pre-M2 stored value.
    {
        let conn = engine.conn_for_maintenance().await;
        conn.execute(
            "UPDATE memories SET origin_trust = 1.0 WHERE id = ?1",
            rusqlite::params![legacy],
        )
        .unwrap();
    }
    assert_eq!(engine.get_origin_trust(agent, &legacy).await.unwrap(), Some(1.0));
    let ch = duduclaw_memory::user_profile::record_trait_outcome(
        &engine, agent, "u1", "preferred_name", "x", "channel", 0.3, duduclaw_memory::lineage::Provenance::test_only(),
    )
    .await
    .unwrap();
    match ch {
        TemporalWriteOutcome::Refused(r) => assert_eq!(r.existing_trust, 0.6),
        other => panic!("channel write must be refused: {other:?}"),
    }
    let own = duduclaw_memory::user_profile::record_trait_outcome(
        &engine, agent, "u1", "preferred_name", "老李", "user_profile", 0.6, duduclaw_memory::lineage::Provenance::test_only(),
    )
    .await
    .unwrap();
    assert!(matches!(own, TemporalWriteOutcome::Stored(_)), "{own:?}");
}

/// H2(a): a burst-quarantined write the guard would refuse is refused at store
/// time (the caller holds it as a claim); an identical value or a fresh triple
/// is stored quarantined as before.
#[tokio::test]
async fn quarantined_write_outranked_by_current_fact_is_refused() {
    use duduclaw_memory::TemporalWriteOutcome;
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "burst";
    let op = engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut q = meta("s", "p", "365", "channel", 0.3);
    q.quarantined = true;
    let out = engine
        .store_temporal_outcome(agent, entry(agent, "365 days"), q, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    match out {
        TemporalWriteOutcome::Refused(r) => assert_eq!(r.existing_id, op),
        other => panic!("{other:?}"),
    }
    // Identical value: not a conflict, stored inert.
    let mut same = meta("s", "p", "7", "channel", 0.3);
    same.quarantined = true;
    let id = engine
        .store_temporal(agent, entry(agent, "7 days"), same, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_ne!(id, op);
    assert_eq!(engine.is_quarantined(agent, &id).await.unwrap(), Some(true));
    // No current fact: stored inert.
    let mut fresh = meta("other", "p", "v", "channel", 0.3);
    fresh.quarantined = true;
    let id = engine.store_temporal(agent, entry(agent, "v"), fresh, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    assert_eq!(engine.is_quarantined(agent, &id).await.unwrap(), Some(true));
    assert_eq!(current_ids(&engine, agent, "s", "p").await.len(), 2, "op + inert row");
}

/// H2(b): releasing a burst row applies supersession — it replaces an
/// equal-trust current fact (which is closed out and linked) instead of
/// coexisting; an identical value reaffirms; an outranked row becomes a held
/// claim with its own conflict.
#[tokio::test]
async fn release_applies_supersession_semantics() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "release";

    // (1) supersede an equal-trust current fact.
    let old = engine
        .store_temporal(agent, entry(agent, "a"), meta("s1", "p", "a", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut q = meta("s1", "p", "b", "channel", 0.3);
    q.quarantined = true;
    let b = engine.store_temporal(agent, entry(agent, "b"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    let r = engine.release_quarantine(agent, &[b.clone()]).await.unwrap();
    assert_eq!((r.released, r.held.len()), (1, 0));
    assert_eq!(current_ids(&engine, agent, "s1", "p").await, vec![b.clone()]);
    let h = engine.get_history(agent, "s1", "p").await.unwrap();
    let old_row = h.iter().find(|x| x.id == old).unwrap();
    assert_eq!(old_row.superseded_by.as_deref(), Some(b.as_str()));
    assert_eq!(h.iter().find(|x| x.id == b).unwrap().supersedes.as_deref(), Some(old.as_str()));

    // (2) identical value → reaffirm, the released row is closed out.
    let cur = engine
        .store_temporal(agent, entry(agent, "x"), meta("s2", "p", "x", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut q = meta("s2", "p", "x", "channel", 0.3);
    q.quarantined = true;
    let dup = engine.store_temporal(agent, entry(agent, "x"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    let r = engine.release_quarantine(agent, &[dup.clone()]).await.unwrap();
    assert_eq!(r.released, 1);
    assert_eq!(current_ids(&engine, agent, "s2", "p").await, vec![cur]);

    // (3) outranked at release (stored while the guard was off) → held claim.
    let op = engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s3", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut engine = engine;
    engine.supersession_trust_guard = false;
    let mut q = meta("s3", "p", "365", "channel", 0.3);
    q.quarantined = true;
    let low = engine.store_temporal(agent, entry(agent, "365 days"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    engine.supersession_trust_guard = true;
    let r = engine.release_quarantine(agent, &[low.clone()]).await.unwrap();
    assert_eq!(r.released, 0);
    assert_eq!(r.held.len(), 1);
    assert_eq!(r.held[0].held_id, low);
    assert!(r.held[0].newly_converted);
    assert_eq!(r.held[0].refusal.as_ref().unwrap().existing_id, op);
    let view = engine.held_claim_view(agent, &low).await.unwrap().unwrap();
    assert_eq!(view.existing_content.as_deref(), Some("7 days"));
    assert_eq!(view.object.as_deref(), Some("365"));
    assert!(view.held_from_release);
    // A retried release re-reports the converted row without converting again.
    let again = engine.release_quarantine(agent, &[low.clone()]).await.unwrap();
    assert_eq!(again.released, 0);
    assert_eq!(again.held.len(), 1);
    assert!(!again.held[0].newly_converted);
    assert_eq!(current_ids(&engine, agent, "s3", "p").await, vec![op.clone()]);
    assert_eq!(engine.is_quarantined(agent, &low).await.unwrap(), Some(true));
    assert_eq!(
        engine.find_pending_held_claim(agent, "s3", "p", Some("365")).await.unwrap(),
        Some(low.clone())
    );
    // It is now a held claim: promotion (with operator authority) accepts it.
    let p = engine.promote_quarantined(agent, &[low], "operator").await.unwrap();
    assert_eq!((p.promoted, p.stale), (1, 0));
    assert_eq!(current_ids(&engine, agent, "s3", "p").await.len(), 1);
    assert_ne!(current_ids(&engine, agent, "s3", "p").await, vec![op]);
}

/// M1: the admission gate is consulted only for a new row; refusing it writes
/// nothing; a repeat of a pending claim never consumes it.
#[tokio::test]
async fn hold_admission_gate_caps_new_rows_only() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "gate";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut calls = 0u32;
    let mut admit_once = move || {
        calls += 1;
        calls == 1
    };
    let first = engine
        .hold_refused_claim_gated(agent, entry(agent, "14"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only(), &mut admit_once)
        .await
        .unwrap()
        .unwrap();
    assert!(first.newly_held);
    // Repeat: no gate call, same id.
    let again = engine
        .hold_refused_claim_gated(agent, entry(agent, "14"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only(), &mut admit_once)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.id, first.id);
    // A new claim: second gate call refuses → nothing written.
    let capped = engine
        .hold_refused_claim_gated(agent, entry(agent, "30"), meta("s", "p", "30", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only(), &mut admit_once)
        .await
        .unwrap();
    assert!(capped.is_none());
    assert_eq!(engine.find_pending_held_claim(agent, "s", "p", Some("30")).await.unwrap(), None);
}

/// M1: two identical holds racing from two connections (two engines on one
/// database file, as two processes would be) yield exactly one held row.
#[test]
fn concurrent_identical_holds_yield_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let engine = SqliteMemoryEngine::new(&db).unwrap();
        rt.block_on(engine.store_temporal(
            "race",
            entry("race", "7 days"),
            meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only(),
        ))
        .unwrap();
    }
    for round in 0..5 {
        let object = format!("claim-{round}");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let db = db.clone();
                let barrier = barrier.clone();
                let object = object.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
                    let engine = SqliteMemoryEngine::new(&db).unwrap();
                    barrier.wait();
                    rt.block_on(engine.hold_refused_claim_outcome(
                        "race",
                        entry("race", &object),
                        meta("s", "p", &object, "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only(),
                    ))
                    .unwrap()
                })
            })
            .collect();
        let outs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(outs[0].id, outs[1].id, "{outs:?}");
        assert_eq!(outs.iter().filter(|o| o.newly_held).count(), 1, "{outs:?}");
    }
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let engine = SqliteMemoryEngine::new(&db).unwrap();
    let n: i64 = rt.block_on(async {
        let conn = engine.conn_for_maintenance().await;
        conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE quarantined = 1
               AND json_extract(metadata, '$.held_claim.subject') = 's'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert_eq!(n, 5, "one held row per round");
}

/// L1: quarantined rows whose review is no longer pending are closed out as a
/// rejection; rows still covered by a pending card, and fresh rows, are kept.
#[tokio::test]
async fn sweep_closes_quarantine_with_no_pending_review() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "sweep";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let pending = engine
        .hold_refused_claim(agent, entry(agent, "14"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let lapsed = engine
        .hold_refused_claim(agent, entry(agent, "30"), meta("s", "p", "30", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let keep: std::collections::HashSet<String> = [pending.clone()].into_iter().collect();
    // Too fresh: nothing swept.
    let past = Utc::now() - chrono::Duration::hours(1);
    assert_eq!(engine.expire_unreviewed_quarantine(&keep, past, "lapsed").await.unwrap(), 0);
    let future = Utc::now() + chrono::Duration::seconds(5);
    assert_eq!(engine.expire_unreviewed_quarantine(&keep, future, "lapsed").await.unwrap(), 1);
    assert_eq!(engine.find_pending_held_claim(agent, "s", "p", Some("30")).await.unwrap(), None);
    assert_eq!(
        engine.find_pending_held_claim(agent, "s", "p", Some("14")).await.unwrap(),
        Some(pending)
    );
    assert_eq!(engine.get_origin_trust(agent, &lapsed).await.unwrap(), Some(0.1));
    // A lapsed claim can no longer be promoted.
    assert_eq!(
        engine.promote_quarantined(agent, &[lapsed], "operator").await.unwrap(),
        duduclaw_memory::PromotionReport::default()
    );
}

// ── v1.67.1 third batch ─────────────────────────────────────────────────────

/// R-M2: a value equal to a pending (quarantined) row is not a reaffirmation
/// of that row — an ordinary write stores a new current row.
#[tokio::test]
async fn equal_value_does_not_reaffirm_a_quarantined_row() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "m2-write";
    let mut q = meta("s", "p", "v", "channel", 0.3);
    q.quarantined = true;
    let pending = engine.store_temporal(agent, entry(agent, "v"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    let id = engine
        .store_temporal(agent, entry(agent, "v"), meta("s", "p", "v", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    assert_ne!(id, pending, "must not report the quarantined row as the stored fact");
    assert_eq!(engine.is_quarantined(agent, &id).await.unwrap(), Some(false));
    assert_eq!(engine.get_origin(agent, &id).await.unwrap(), Some(Some("operator".into())));
}

/// R-M2: promoting a held claim whose value equals a pending burst row writes
/// the promoted fact instead of "reaffirming" the inert row.
#[tokio::test]
async fn promotion_equal_to_a_pending_burst_row_still_writes() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "m2-promote";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let held = engine
        .hold_refused_claim(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    // A burst row with the same value sits pending (written while the guard
    // was off, so it is not refused at store time).
    let mut engine = engine;
    engine.supersession_trust_guard = false;
    let mut q = meta("s", "p", "14", "channel", 0.3);
    q.quarantined = true;
    let burst = engine.store_temporal(agent, entry(agent, "14 days"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    engine.supersession_trust_guard = true;
    let r = engine.promote_quarantined(agent, &[held], "operator").await.unwrap();
    assert_eq!(r.promoted, 1);
    let cur = current_ids(&engine, agent, "s", "p").await;
    assert_eq!(cur.len(), 1);
    assert_ne!(cur[0], burst);
    assert_eq!(engine.get_origin(agent, &cur[0]).await.unwrap(), Some(Some("operator".into())));
}

/// R-H1: promotion bound to a card's digest refuses a row that no longer
/// matches what the card showed (counted stale, row closed).
#[tokio::test]
async fn bound_promotion_refuses_a_digest_mismatch() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "digest";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let held = engine
        .hold_refused_claim(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let view = engine.held_claim_view(agent, &held).await.unwrap().unwrap();
    assert_eq!(
        view.claim_digest,
        duduclaw_memory::claim_digest("14 days", "s", "p", Some("14"))
    );
    let wrong = duduclaw_memory::claim_digest("forever", "s", "p", Some("14"));
    let r = engine
        .promote_quarantined_bound(agent, &[(held.clone(), wrong)], "operator")
        .await
        .unwrap();
    assert_eq!((r.promoted, r.stale), (0, 1));
    assert!(engine.held_claim_view(agent, &held).await.unwrap().is_none(), "row closed");

    let held2 = engine
        .hold_refused_claim(agent, entry(agent, "21 days"), meta("s", "p", "21", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let d = engine.held_claim_view(agent, &held2).await.unwrap().unwrap().claim_digest;
    let r = engine
        .promote_quarantined_bound(agent, &[(held2, d)], "operator")
        .await
        .unwrap();
    assert_eq!((r.promoted, r.stale), (1, 0));
}

/// R-L10: an error inside a held-claim transaction rolls back, and the
/// connection keeps working for the next transaction.
#[tokio::test]
async fn failed_promotion_rolls_back_and_the_connection_stays_usable() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "rollback";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let held = engine
        .hold_refused_claim(agent, entry(agent, "14 days"), meta("s", "p", "14", "channel", 0.3), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    // A reviewer origin below the protected fact makes the write refuse → Err.
    assert!(engine.promote_quarantined(agent, &[held.clone()], "channel").await.is_err());
    // Rolled back: still pending; and the next transaction succeeds.
    assert!(engine.held_claim_view(agent, &held).await.unwrap().is_some());
    let r = engine.promote_quarantined(agent, &[held], "operator").await.unwrap();
    assert_eq!(r.promoted, 1);
}

/// R-L9: a row converted to a held claim at release gets a fresh grace period.
#[tokio::test]
async fn release_conversion_refreshes_the_sweep_grace_period() {
    let mut engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "grace";
    engine
        .store_temporal(agent, entry(agent, "7 days"), meta("s", "p", "7", "operator", 1.0), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    engine.supersession_trust_guard = false;
    let mut q = meta("s", "p", "365", "channel", 0.3);
    q.quarantined = true;
    let low = engine.store_temporal(agent, entry(agent, "365 days"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    engine.supersession_trust_guard = true;
    // Backdate the row's ingestion: before the fix the sweep keyed on this.
    {
        let conn = engine.conn_for_maintenance().await;
        conn.execute(
            "UPDATE memories SET ingested_at = '2020-01-01T00:00:00+00:00',
                                 timestamp = '2020-01-01T00:00:00+00:00' WHERE id = ?1",
            rusqlite::params![low],
        )
        .unwrap();
    }
    let r = engine.release_quarantine(agent, &[low.clone()]).await.unwrap();
    assert_eq!(r.held.len(), 1);
    let cutoff = Utc::now() - chrono::Duration::minutes(10);
    let keep = std::collections::HashSet::new();
    assert_eq!(engine.expire_unreviewed_quarantine(&keep, cutoff, "lapsed").await.unwrap(), 0);
    assert!(engine.held_claim_view(agent, &low).await.unwrap().is_some());
}

/// R-L3: a released row's trust is capped at its class ceiling — a legacy
/// row stored at 1.0 with no origin reads as unattributed (0.6), so it cannot
/// replace an import (0.7) fact at release.
#[tokio::test]
async fn release_caps_a_legacy_rows_trust_at_its_class() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let agent = "legacy-release";
    let imp = engine
        .store_temporal(agent, entry(agent, "a"), meta("s", "p", "a", "import", 0.7), duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap();
    let mut engine = engine;
    engine.supersession_trust_guard = false;
    let mut q = meta("s", "p", "b", "channel", 0.3);
    q.quarantined = true;
    let row = engine.store_temporal(agent, entry(agent, "b"), q, duduclaw_memory::lineage::Provenance::test_only()).await.unwrap();
    engine.supersession_trust_guard = true;
    {
        let conn = engine.conn_for_maintenance().await;
        conn.execute(
            "UPDATE memories SET origin = NULL, origin_trust = 1.0 WHERE id = ?1",
            rusqlite::params![row],
        )
        .unwrap();
    }
    let r = engine.release_quarantine(agent, &[row.clone()]).await.unwrap();
    assert_eq!((r.released, r.held.len()), (0, 1));
    assert_eq!(current_ids(&engine, agent, "s", "p").await, vec![imp]);
}
