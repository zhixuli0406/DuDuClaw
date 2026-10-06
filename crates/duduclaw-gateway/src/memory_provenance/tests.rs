//! Tests for host-generated memory sources and the fenced-write audit.

use super::*;
use duduclaw_memory::FenceReason;

#[test]
fn channel_source_hashes_the_stored_text_and_validates() {
    let s = channel_message_source("telegram:1", 812, "hello", Utc::now());
    assert_eq!(s.message, "m:812");
    assert_eq!(s.seq, Some(812));
    assert_eq!(
        s.content_hash.as_deref(),
        Some(content_hash("hello").as_str())
    );
    assert!(s.validate().is_ok());
}

#[test]
fn dispatch_source_is_deterministic_and_separates_prompt_from_reply() {
    let at = Utc::now();
    let a = dispatch_run_source("cron:agnes", "ab", "c", at);
    let b = dispatch_run_source("cron:agnes", "ab", "c", at);
    let c = dispatch_run_source("cron:agnes", "a", "bc", at);
    assert_eq!(a.message, b.message);
    assert_ne!(a.message, c.message, "the prompt length is part of the key");
    assert!(a.message.starts_with("run:") && a.message.len() == 4 + 32);
    assert!(a.validate().is_ok());
    assert_eq!(a.kind, SourceKind::DispatchRun);
}

#[test]
fn footprint_source_names_the_day() {
    let s = footprint_day_source("agnes", "2026-10-05", Utc::now());
    assert_eq!(
        (s.session.as_str(), s.message.as_str()),
        ("footprint:agnes", "day:2026-10-05")
    );
    assert!(s.validate().is_ok());
}

#[test]
fn turn_sources_never_invent_a_source() {
    let none = TurnSources::default();
    assert!(none.provenance().is_none() && none.assistant_provenance().is_none());
    assert!(provenance_of(&[]).is_none());
    let user_only = TurnSources {
        user: Some(test_msg("telegram:1", 1)),
        assistant: None,
    };
    assert_eq!(user_only.all().len(), 1);
    assert!(user_only.assistant_provenance().is_none());
    assert!(user_only.user_provenance().is_some());
    assert_eq!(
        wiki_source_id(&user_only.all()).as_deref(),
        Some("conversation:telegram:1:m:1")
    );
    assert!(wiki_source_id(&[]).is_none());
}

#[test]
fn fence_errors_are_recognised_by_type_not_text() {
    let d = "0123456789abcdef0123456789abcdef";
    let fenced = FenceRefusal {
        reason: FenceReason::SourceForgotten,
        source_digest: Some(d.into()),
        parent_id: None,
    }
    .into_error();
    assert_eq!(fenced_parts(&fenced), Some(("source_forgotten", Some(d))));
    // A memory error whose text merely looks like a refusal is not one.
    let lookalike = DuDuClawError::Memory("source forgotten: source_forgotten".into());
    assert_eq!(fenced_parts(&lookalike), None);
}

#[test]
fn fenced_audit_is_capped_per_day_and_carries_no_session() {
    let home = tempfile::tempdir().unwrap();
    let refusal = FenceRefusal {
        reason: FenceReason::SourceForgotten,
        source_digest: Some("0123456789abcdef0123456789abcdef".into()),
        parent_id: None,
    };
    for _ in 0..(FENCED_AUDIT_DAILY_CAP + 3) {
        record_fenced(home.path(), "agnes", "conversation_distill", &refusal);
    }
    record_fenced(home.path(), "other", "conversation_distill", &refusal);
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    let rows: Vec<&str> = audit
        .lines()
        .filter(|l| l.contains("memory_write_fenced"))
        .collect();
    let agnes = rows.iter().filter(|l| l.contains("\"agnes\"")).count();
    assert_eq!(
        agnes as u64, FENCED_AUDIT_DAILY_CAP,
        "audit rows stop at the cap"
    );
    assert!(
        rows.iter().any(|l| l.contains("\"other\"")),
        "the cap is per employee"
    );
    assert!(
        rows.iter()
            .all(|l| l.contains("source_forgotten") && !l.contains("telegram"))
    );
    let counts: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(FENCED_COUNTER_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        counts["counts"]["agnes"],
        FENCED_AUDIT_DAILY_CAP + 3,
        "still counted"
    );
}

#[test]
fn record_fenced_error_ignores_non_fence_errors() {
    let home = tempfile::tempdir().unwrap();
    let disk = DuDuClawError::Memory("disk full".into());
    assert!(!record_fenced_error(home.path(), "agnes", "x", &disk));
    assert!(!home.path().join("security_audit.jsonl").exists());
    let fenced = FenceRefusal {
        reason: FenceReason::ParentForgotten,
        source_digest: None,
        parent_id: Some("m1".into()),
    }
    .into_error();
    assert!(record_fenced_error(home.path(), "agnes", "x", &fenced));
}

/// The fence check for a derived artifact: a forgotten source is reported,
/// a later message of the same chat is not.
#[tokio::test(flavor = "multi_thread")]
async fn sources_forgotten_reports_only_forgotten_sources() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    {
        let engine = duduclaw_memory::SqliteMemoryEngine::new(&db).unwrap();
        test_seed(&engine, "agnes", "seed", test_msg("telegram:1", 5)).await;
        test_forget(&engine, "agnes", "telegram:1", &["m:5"]).await;
    }
    let hit = sources_forgotten(&db, home.path(), "agnes", &[test_msg("telegram:1", 5)])
        .await
        .unwrap();
    assert_eq!(hit.map(|r| r.reason), Some(FenceReason::SourceForgotten));
    assert!(
        sources_forgotten(&db, home.path(), "agnes", &[test_msg("telegram:1", 6)])
            .await
            .unwrap()
            .is_none()
    );
    // Another namespace's tombstone does not apply.
    assert!(
        sources_forgotten(&db, home.path(), "bob", &[test_msg("telegram:1", 5)])
            .await
            .unwrap()
            .is_none()
    );
}

/// Revision A: a CLI spawned inside a channel turn carries the triggering
/// user message; one spawned outside a turn (dispatch) does not.
#[tokio::test]
async fn turn_user_message_reaches_the_spawn_env_only_inside_a_turn() {
    let envs = |cmd: &tokio::process::Command| -> Vec<(String, String)> {
        cmd.as_std()
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .filter(|(k, _)| k.starts_with("DUDUCLAW_TURN_USER_MESSAGE"))
            .collect()
    };
    let mut outside = tokio::process::Command::new("true");
    inject_turn_user_message_env(&mut outside);
    assert!(envs(&outside).is_empty());

    let at = duduclaw_memory::format_ts(Utc::now());
    let mut inside = tokio::process::Command::new("true");
    TURN_USER_MESSAGE
        .scope(Some((812, at.clone())), async {
            inject_turn_user_message_env(&mut inside);
        })
        .await;
    let mut got = envs(&inside);
    got.sort();
    assert_eq!(
        got,
        vec![
            (duduclaw_core::ENV_TURN_USER_MESSAGE_AT.to_string(), at),
            (
                duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ.to_string(),
                "812".to_string()
            ),
        ]
    );
}

/// M-6: a dispatch run's key reaches its CLI spawn, and the post-run source
/// built from the same key is the source an MCP write in that run records.
#[tokio::test]
async fn dispatch_run_key_reaches_the_spawn_env_only_inside_a_run() {
    let envs = |cmd: &tokio::process::Command| -> Vec<(String, String)> {
        cmd.as_std()
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .filter(|(k, _)| k.starts_with("DUDUCLAW_DISPATCH_"))
            .collect()
    };
    let mut outside = tokio::process::Command::new("true");
    inject_dispatch_run_env(&mut outside);
    assert!(envs(&outside).is_empty());

    let key = new_dispatch_run_key();
    assert_eq!(key.len(), 32);
    assert_ne!(key, new_dispatch_run_key());
    let mut inside = tokio::process::Command::new("true");
    DISPATCH_RUN
        .scope(Some(("cron:agnes".to_string(), key.clone())), async {
            inject_dispatch_run_env(&mut inside);
        })
        .await;
    let mut got = envs(&inside);
    got.sort();
    assert_eq!(
        got,
        vec![
            (duduclaw_core::ENV_DISPATCH_RUN_ID.to_string(), key.clone()),
            (
                duduclaw_core::ENV_DISPATCH_SESSION.to_string(),
                "cron:agnes".to_string()
            ),
        ]
    );
    let s = dispatch_run_source_for("cron:agnes", &key, Utc::now());
    assert_eq!(s.kind, SourceKind::DispatchRun);
    assert_eq!(s.message, format!("run:{key}"));
    assert!(s.validate().is_ok());
}

/// Live verification issue 3: a received bus message's upstream identity is
/// incomplete when exactly one half is present; the audit names the missing
/// field and nothing else.
#[test]
fn incomplete_upstream_names_the_missing_half_and_is_audited() {
    assert_eq!(incomplete_upstream(Some("t"), Some("s")), None);
    assert_eq!(incomplete_upstream(None, None), None);
    assert_eq!(incomplete_upstream(Some("t"), None), Some("session_id"));
    assert_eq!(incomplete_upstream(Some(""), Some("s")), Some("turn_id"));
    let home = tempfile::tempdir().unwrap();
    audit_upstream_identity(
        home.path(),
        AUDIT_UPSTREAM_INCOMPLETE,
        "agnes",
        "msg-1",
        "session_id",
    );
    let log = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(log.contains(AUDIT_UPSTREAM_INCOMPLETE), "{log}");
    assert!(log.contains("\"missing\":\"session_id\""), "{log}");
    assert_eq!(
        complete_upstream_pair(Some("t".into()), None),
        (None, None, Some("session_id"))
    );
}

/// Third review F4: the sender's "upstream dropped" marker survives the
/// queue, and a dispatch in scope of it hands the run the marker variable.
#[tokio::test]
async fn upstream_unknown_marker_rides_the_queue_and_reaches_the_spawn_env() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = crate::message_queue::MessageQueue::open(tmp.path()).unwrap();
    let msg = crate::message_queue::QueueMessage {
        id: "m-1".into(),
        sender: "agnes".into(),
        target: "bob".into(),
        payload: "work".into(),
        status: crate::message_queue::MessageStatus::Pending,
        retry_count: 0,
        delegation_depth: 1,
        origin_agent: Some("agnes".into()),
        sender_agent: Some("agnes".into()),
        error: None,
        response: None,
        created_at: "2026-10-05T00:00:00Z".into(),
        acked_at: None,
        completed_at: None,
        reply_channel: None,
        turn_id: None,
        session_id: None,
        upstream_unknown: true,
    };
    queue.enqueue(&msg).await.unwrap();
    let got = queue.get_by_id("m-1").await.unwrap().unwrap();
    assert!(got.upstream_unknown);

    let has_marker = |envs: &[(String, String)]| {
        envs.iter()
            .any(|(k, v)| k == duduclaw_core::ENV_UPSTREAM_UNKNOWN && v == "1")
    };
    let with = DISPATCH_RUN
        .scope(Some(("dispatch:bob".into(), "k1".into())), async {
            UPSTREAM_UNKNOWN
                .scope(true, async { turn_source_env_pairs() })
                .await
        })
        .await;
    assert!(has_marker(&with), "{with:?}");
    let without = DISPATCH_RUN
        .scope(Some(("dispatch:bob".into(), "k1".into())), async {
            turn_source_env_pairs()
        })
        .await;
    assert!(!has_marker(&without));
}
