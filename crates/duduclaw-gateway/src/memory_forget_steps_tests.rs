//! Tests for the forget-by-source external steps (P2-B): collection, the four
//! step kinds, idempotency, boot resume, and the stray-DB merge fence.

use super::*;
use crate::memory_provenance::{test_forget, test_seed};
use crate::session::SessionManager;
use duduclaw_memory::{ApplyOutcome, PlanOutcome, SourceRef};

const AGENT: &str = "agnes";

fn sel(session: &str, messages: &[&str], upto_seq: Option<i64>) -> SelectorView {
    SelectorView {
        session: session.to_string(),
        messages: messages.iter().map(|m| m.to_string()).collect(),
        upto_seq,
        upto_time: Some(Utc::now()),
    }
}

#[test]
fn page_sources_match_by_message_watermark_and_legacy_time() {
    let s = "telegram:1";
    let msg = sel(s, &["m:5"], None);
    assert!(page_source_matches("conversation:telegram:1:m:5", &msg));
    assert!(!page_source_matches("conversation:telegram:1:m:6", &msg));
    assert!(
        !page_source_matches("conversation:telegram:10:m:5", &msg),
        "exact session only"
    );
    assert!(!page_source_matches(
        "conversation:telegram:1:2026-01-01T00:00:00Z",
        &msg
    ));

    let upto = sel(s, &[], Some(10));
    assert!(page_source_matches("conversation:telegram:1:m:10", &upto));
    assert!(
        !page_source_matches("conversation:telegram:1:m:11", &upto),
        "after the watermark"
    );
    assert!(page_source_matches(
        "conversation:telegram:1:2026-01-01T00:00:00+00:00",
        &upto
    ));
    assert!(!page_source_matches(
        "conversation:telegram:1:2999-01-01T00:00:00+00:00",
        &upto
    ));
    assert!(!page_source_matches(
        "conversation:telegram:1:garbage",
        &upto
    ));
}

#[test]
fn overlong_session_ids_are_recorded_and_matched_in_digest_form() {
    use crate::memory_provenance::{WIKI_SOURCE_MAX_CHARS, wiki_source_entry};
    let long = format!("discord:{}", "9".repeat(150));
    let e = wiki_source_entry(&long, "m:7");
    assert!(
        e.starts_with("conversation-digest:") && e.ends_with(":m:7"),
        "{e}"
    );
    assert!(e.chars().count() <= WIKI_SOURCE_MAX_CHARS);
    assert!(page_source_matches(&e, &sel(&long, &["m:7"], None)));
    assert!(!page_source_matches(&e, &sel(&long, &["m:8"], None)));
    assert!(page_source_matches(&e, &sel(&long, &[], Some(7))));
    assert!(!page_source_matches(&e, &sel(&long, &[], Some(6))));
    assert!(!page_source_matches(
        &e,
        &sel("discord:other", &["m:7"], None)
    ));
    // An overlong message key: one digest, matched by that exact message.
    let key = format!("turn:{}", "x".repeat(80));
    let e = wiki_source_entry(&long, &key);
    assert!(e.chars().count() <= WIKI_SOURCE_MAX_CHARS);
    assert!(page_source_matches(&e, &sel(&long, &[key.as_str()], None)));
    // Short ids keep the readable form.
    assert_eq!(
        wiki_source_entry("telegram:1", "m:2"),
        "conversation:telegram:1:m:2"
    );
}

/// A home with one chat: four messages, an async summary and a compression
/// row; memories from messages 1 and 2; an auto page and a review card.
struct Fixture {
    home: tempfile::TempDir,
    engine: SqliteMemoryEngine,
    session: String,
    ids: Vec<i64>,
    kept_row: String,
}

async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let session = "telegram:42".to_string();
    let mgr = SessionManager::new(&sessions_db_path(home.path())).unwrap();
    mgr.get_or_create(&session, AGENT).await.unwrap();
    let mut ids = Vec::new();
    for (role, text) in [
        ("user", "my name is Ann"),
        ("assistant", "hi Ann"),
        ("user", "bye"),
        ("assistant", "bye"),
    ] {
        ids.push(
            mgr.append_message_with_id(&session, role, text, 1)
                .await
                .unwrap(),
        );
    }
    mgr.set_summary(&session, "Ann introduced herself", 2)
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(sessions_db_path(home.path())).unwrap();
    conn.execute(
        "INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
         VALUES (?1, 'system', 'compressed: Ann', 0, ?2)",
        params![session, Utc::now().to_rfc3339()],
    )
    .unwrap();
    drop(conn);

    let engine = SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
    let msg = |seq: i64| SourceRef::channel_message(session.as_str(), seq, Utc::now(), None);
    let gone = test_seed(&engine, AGENT, "name is Ann", msg(ids[0])).await;
    let kept_row = test_seed(&engine, AGENT, "said bye", msg(ids[2])).await;

    let wiki = home
        .path()
        .join("agents")
        .join(AGENT)
        .join("wiki")
        .join("auto")
        .join("sop");
    std::fs::create_dir_all(&wiki).unwrap();
    std::fs::write(
        wiki.join("ann.md"),
        format!(
            "---\ntitle: Ann\ncreated: 2026-10-01T00:00:00Z\nupdated: 2026-10-01T00:00:00Z\n\
             tags: []\nrelated: []\nsources: [\"conversation:{session}:m:{}\"]\n---\n\nAnn SOP\n",
            ids[0]
        ),
    )
    .unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    broker
        .request(
            AGENT,
            crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE,
            "Ann card",
            serde_json::json!({ "quarantined_ids": [gone] }),
            3600,
        )
        .await
        .unwrap();
    Fixture {
        home,
        engine,
        session,
        ids,
        kept_row,
    }
}

async fn plan_and_apply(f: &Fixture, messages: &[String], upto_seq: Option<i64>) -> ForgetPlan {
    let selector = ForgetSelector {
        session: f.session.clone(),
        messages: messages.to_vec(),
        upto_seq,
        upto_time: Some(Utc::now()),
    };
    let ext = collect_external_for_plan(
        &f.engine,
        f.home.path(),
        AGENT,
        &selector,
        Default::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ext.wiki_pages.len(), 1, "{ext:?}");
    assert_eq!(ext.review_cards_matching, 1);
    let plan = match f
        .engine
        .plan_forget_source(AGENT, &selector, Default::default(), &ext)
        .await
        .unwrap()
    {
        PlanOutcome::Planned(p) => *p,
        other => panic!("{other:?}"),
    };
    let ext2 = collect_external_for_apply(f.home.path(), &plan)
        .await
        .unwrap();
    assert!(matches!(
        f.engine
            .apply_forget_plan(&plan.plan_id, &ext2)
            .await
            .unwrap(),
        ApplyOutcome::Applied(_)
    ));
    f.engine
        .get_forget_plan(&plan.plan_id)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn steps_remove_page_card_messages_and_summary_and_are_idempotent() {
    let f = fixture().await;
    let plan = plan_and_apply(&f, &[format!("m:{}", f.ids[0])], None).await;

    let r = run_forget_steps(&f.engine, f.home.path(), &plan.plan_id)
        .await
        .unwrap();
    assert!(r.complete(), "{r:?}");
    assert!(r.done >= 4, "{r:?}");
    // Page gone, card withdrawn, message and summaries hidden.
    assert!(
        !f.home
            .path()
            .join("agents")
            .join(AGENT)
            .join("wiki/auto/sop/ann.md")
            .exists()
    );
    assert_eq!(
        crate::wiki_ingest::count_review_cards_for_ids(f.home.path(), &plan_memory_ids(&plan))
            .await
            .unwrap(),
        1,
        "the card stays (scrubbed), it is not deleted"
    );
    let broker = crate::approval::ApprovalBroker::open(f.home.path()).unwrap();
    assert!(broker.list_pending(Some(AGENT)).await.unwrap().is_empty());
    let mgr = SessionManager::new(&sessions_db_path(f.home.path())).unwrap();
    let msgs = mgr.get_messages(&f.session).await.unwrap();
    assert!(msgs.iter().all(|m| !m.content.contains("my name is Ann")));
    assert!(
        msgs.iter().all(|m| m.role != "system"),
        "compression row hidden"
    );
    assert!(
        msgs.iter().any(|m| m.content == "bye"),
        "other messages stay"
    );
    assert_eq!(
        mgr.get_summary(&f.session).await.unwrap(),
        (String::new(), 0)
    );
    // The other message's memory is untouched.
    assert!(
        f.engine
            .get_by_id(AGENT, &f.kept_row)
            .await
            .unwrap()
            .is_some()
    );

    // Idempotent: nothing left to run.
    let again = run_forget_steps(&f.engine, f.home.path(), &plan.plan_id)
        .await
        .unwrap();
    assert_eq!(again.done + again.failed, 0);
    // Audit: steps event, no content, no raw session id.
    let audit = std::fs::read_to_string(f.home.path().join("security_audit.jsonl")).unwrap();
    let ours: Vec<&str> = audit
        .lines()
        .filter(|l| l.contains("memory_source_forget"))
        .collect();
    assert!(!ours.is_empty());
    assert!(
        ours.iter()
            .all(|l| !l.contains("Ann") && !l.contains(&f.session))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn boot_resume_runs_unfinished_steps_once() {
    let f = fixture().await;
    let plan = plan_and_apply(&f, &[], Some(f.ids[1])).await;
    assert!(!f.engine.unfinished_forget_steps().await.unwrap().is_empty());
    let (plans, failed) = resume_unfinished(&f.engine, f.home.path()).await;
    assert_eq!((plans, failed), (1, 0));
    assert!(f.engine.unfinished_forget_steps().await.unwrap().is_empty());
    assert_eq!(resume_unfinished(&f.engine, f.home.path()).await, (0, 0));
    // Session forget: every message up to the watermark is hidden.
    let mgr = SessionManager::new(&sessions_db_path(f.home.path())).unwrap();
    let left: Vec<String> = mgr
        .get_messages(&f.session)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.content)
        .collect();
    // Rows after the watermark (here the later turn and a compression row
    // written after it) stay.
    assert_eq!(
        left,
        vec![
            "bye".to_string(),
            "bye".to_string(),
            "compressed: Ann".to_string()
        ]
    );
    assert_eq!(plan.status, "applied");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_step_is_recorded_and_retried() {
    let f = fixture().await;
    let plan = plan_and_apply(&f, &[format!("m:{}", f.ids[0])], None).await;
    // sessions.db unreadable as a database ⇒ the session steps fail.
    let db = sessions_db_path(f.home.path());
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let saved = std::fs::read(&db).unwrap();
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
    }
    std::fs::write(&db, b"not a database").unwrap();
    let r = run_forget_steps(&f.engine, f.home.path(), &plan.plan_id)
        .await
        .unwrap();
    assert!(!r.complete());
    let failed: Vec<_> = f
        .engine
        .forget_steps(&plan.plan_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|s| s.status == "failed")
        .collect();
    assert!(!failed.is_empty() && failed.iter().all(|s| s.last_error.is_some()));
    std::fs::write(&db, saved).unwrap();
    let r = run_forget_steps(&f.engine, f.home.path(), &plan.plan_id)
        .await
        .unwrap();
    assert!(r.complete(), "{r:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_matching_page_between_plan_and_apply_makes_the_plan_stale() {
    let f = fixture().await;
    let selector = ForgetSelector {
        session: f.session.clone(),
        messages: vec![format!("m:{}", f.ids[0])],
        upto_seq: None,
        upto_time: Some(Utc::now()),
    };
    let ext = collect_external_for_plan(
        &f.engine,
        f.home.path(),
        AGENT,
        &selector,
        Default::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let PlanOutcome::Planned(plan) = f
        .engine
        .plan_forget_source(AGENT, &selector, Default::default(), &ext)
        .await
        .unwrap()
    else {
        panic!()
    };
    let wiki = f
        .home
        .path()
        .join("agents")
        .join(AGENT)
        .join("wiki/auto/sop");
    let page = std::fs::read_to_string(wiki.join("ann.md")).unwrap();
    std::fs::write(wiki.join("ann2.md"), page).unwrap();
    let ext2 = collect_external_for_apply(f.home.path(), &plan)
        .await
        .unwrap();
    assert!(matches!(
        f.engine
            .apply_forget_plan(&plan.plan_id, &ext2)
            .await
            .unwrap(),
        ApplyOutcome::Stale(_)
    ));
    assert!(
        f.home
            .path()
            .join("agents")
            .join(AGENT)
            .join("wiki/auto/sop/ann.md")
            .exists()
    );
}

/// G17: a stray per-agent file holding a forgotten row (by id, or by a
/// forgotten source) does not bring it back at boot; its other rows merge.
#[tokio::test(flavor = "multi_thread")]
async fn stray_db_merge_does_not_resurrect_forgotten_rows() {
    let home = tempfile::tempdir().unwrap();
    let shared = home.path().join("memory.db");
    let stray_path = home
        .path()
        .join("agents")
        .join(AGENT)
        .join("state")
        .join("memory.db");
    std::fs::create_dir_all(stray_path.parent().unwrap()).unwrap();
    let msg = |seq: i64| SourceRef::channel_message("telegram:9", seq, Utc::now(), None);
    let (forgotten_id, by_source_id, kept_id) = {
        let stray = SqliteMemoryEngine::new(&stray_path).unwrap();
        (
            test_seed(&stray, AGENT, "forgotten by id", msg(1)).await,
            test_seed(&stray, AGENT, "forgotten by source", msg(2)).await,
            test_seed(&stray, AGENT, "kept", msg(3)).await,
        )
    };
    {
        // The shared db forgot message 1 (and with it the row's id) and message 2.
        let engine = SqliteMemoryEngine::new(&shared).unwrap();
        let conn = engine.conn_for_maintenance().await;
        conn.execute(
            "INSERT INTO forgotten_memories (memory_store, memory_id, agent_id, plan_id, forgotten_at)
             VALUES ('memories', ?1, ?2, 'p', '2026-10-05T00:00:00.000000Z')",
            params![forgotten_id, AGENT],
        )
        .unwrap();
        drop(conn);
        test_seed(&engine, AGENT, "shared seed", msg(2)).await;
        test_forget(&engine, AGENT, "telegram:9", &["m:2"]).await;
    }
    {
        // The stray file also caches an embedding of a forgotten entity.
        let conn = rusqlite::Connection::open(&stray_path).unwrap();
        conn.execute(
            "INSERT INTO entity_embedding (agent_id, entity, model, vec, created_at)
             VALUES (?1, 'ann', 'ngram', x'00', '2026-10-05T00:00:00Z')",
            params![AGENT],
        )
        .unwrap();
    }
    let out = crate::memory_migrate::merge_per_agent_memory_dbs(home.path());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let engine = SqliteMemoryEngine::new(&shared).unwrap();
    assert!(
        engine
            .get_by_id(AGENT, &forgotten_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        engine
            .get_by_id(AGENT, &by_source_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(engine.get_by_id(AGENT, &kept_id).await.unwrap().is_some());
    let embeddings: i64 = engine
        .conn_for_maintenance()
        .await
        .query_row(
            "SELECT COUNT(*) FROM entity_embedding WHERE entity = 'ann'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(embeddings, 0, "a stray embedding cache is not merged");
    // The merged row keeps its lineage, so it can still be forgotten.
    test_forget(&engine, AGENT, "telegram:9", &["m:3"]).await;
    assert!(engine.get_by_id(AGENT, &kept_id).await.unwrap().is_none());
}

/// L-1: the message key is read from the right, so a session whose id
/// continues with `:m` / `:run` / `:turn` is never taken for another.
#[test]
fn nested_session_ids_are_not_confused() {
    let whole = sel("a", &[], Some(100));
    assert!(
        !page_source_matches("conversation:a:m:m:5", &whole),
        "session a:m"
    );
    assert!(
        !page_source_matches("conversation:a:run:x:m:5", &whole),
        "session a:run:x"
    );
    assert!(!page_source_matches("conversation:a:turn:t:run:9", &whole));
    assert!(page_source_matches("conversation:a:m:5", &whole));
    let nested = sel("a:m", &["m:5"], None);
    assert!(page_source_matches("conversation:a:m:m:5", &nested));
    assert!(!page_source_matches("conversation:a:m:5", &nested));
    // Keys with more than one `:` are only ever stored as a digest.
    assert!(
        crate::memory_provenance::wiki_source_entry("a", "turn:x:y")
            .starts_with("conversation-digest:")
    );
}

/// H-2: a user message's same-turn reply is found; an assistant message has
/// no reply of its own.
#[tokio::test(flavor = "multi_thread")]
async fn same_turn_replies_follow_user_messages_only() {
    let f = fixture().await;
    let db = sessions_db_path(f.home.path());
    assert_eq!(
        same_turn_replies(&db, &f.session, &[f.ids[0], f.ids[1]]).unwrap(),
        vec![(f.ids[0], f.ids[1])]
    );
    assert!(
        same_turn_replies(&db, "telegram:none", &[f.ids[0]])
            .unwrap()
            .is_empty()
    );
}

/// M3: a page rewritten since apply that no longer carries the forgotten
/// source is kept, and the step counts as done.
#[tokio::test(flavor = "multi_thread")]
async fn a_page_no_longer_matching_at_step_time_is_kept() {
    let f = fixture().await;
    let plan = plan_and_apply(&f, &[format!("m:{}", f.ids[0])], None).await;
    let page = f
        .home
        .path()
        .join("agents")
        .join(AGENT)
        .join("wiki/auto/sop/ann.md");
    std::fs::write(
        &page,
        format!(
            "---\ntitle: Ann\ncreated: 2026-10-01T00:00:00Z\nupdated: 2026-10-02T00:00:00Z\n\
             tags: []\nrelated: []\nsources: [\"conversation:{}:m:{}\"]\n---\n\nNew SOP\n",
            f.session, f.ids[2]
        ),
    )
    .unwrap();
    let r = run_forget_steps(&f.engine, f.home.path(), &plan.plan_id)
        .await
        .unwrap();
    assert!(r.complete(), "{r:?}");
    assert!(page.exists(), "page from another message is kept");
}

/// L-2: a checked delete drops the index entry even when the file is
/// already gone (a retry repairs what a failed index write left).
#[test]
fn checked_delete_repairs_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = dir.path().join("wiki");
    std::fs::create_dir_all(wiki.join("auto/sop")).unwrap();
    std::fs::write(
        wiki.join("_index.md"),
        "# Index\n- [Ann](auto/sop/ann.md)\n- [B](b.md)\n",
    )
    .unwrap();
    let store = WikiStore::new(wiki.clone());
    store.delete_page_checked("auto/sop/ann.md").unwrap();
    let idx = std::fs::read_to_string(wiki.join("_index.md")).unwrap();
    assert!(
        !idx.contains("auto/sop/ann.md") && idx.contains("b.md"),
        "{idx}"
    );
    store.delete_page_checked("auto/sop/ann.md").unwrap();
}
