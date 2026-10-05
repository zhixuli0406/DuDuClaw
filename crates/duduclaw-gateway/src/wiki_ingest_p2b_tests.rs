//! P2-B: the conversation-distillation producers honour forget-by-source.
//!
//! Each producer is driven after a message was forgotten (its write must be
//! refused) and with a later message of the same chat (its write must land —
//! the watermark, not the chat, is what is forgotten).

use std::path::Path;
use std::sync::Mutex;

use super::*;
use crate::memory_provenance::{test_forget, test_msg, test_seed};

const S: &str = "telegram:p2b";
const AGENT: &str = "agnes";

type Hook = Box<dyn Fn() + Send>;
/// One page-written hook, fired only for its agent (tests run in parallel).
static PAGE_WRITTEN_HOOK: Mutex<Option<(String, Hook)>> = Mutex::new(None);

/// Called by the knowledge branch between writing a page and re-checking
/// its sources (test builds only).
pub(super) fn fire_page_written_hook(agent: &str) {
    if let Ok(g) = PAGE_WRITTEN_HOOK.lock() {
        if let Some((a, f)) = g.as_ref() {
            if a == agent {
                f();
            }
        }
    }
}

fn engine_at(db: &Path) -> SqliteMemoryEngine {
    SqliteMemoryEngine::new(db).unwrap()
}

/// Forget message `seq` of [`S`] (seeding one row so there is a target).
async fn forget_msg(db: &Path, agent: &str, seq: i64) {
    let engine = engine_at(db);
    test_seed(&engine, agent, &format!("seed {seq}"), test_msg(S, seq)).await;
    test_forget(&engine, agent, S, &[&format!("m:{seq}")]).await;
}

fn fact(content: &str) -> DistilledFact {
    DistilledFact {
        subject: None,
        predicate: None,
        object: None,
        content: content.to_string(),
        confidence: Some(0.8),
    }
}

fn prov(seq: i64) -> duduclaw_memory::lineage::Provenance {
    duduclaw_memory::lineage::Provenance::source(test_msg(S, seq))
}

async fn count_content(db: &Path, agent: &str, needle: &str) -> usize {
    engine_at(db)
        .list_recent(agent, 500)
        .await
        .unwrap()
        .iter()
        .filter(|e| e.content.contains(needle))
        .count()
}

fn audit_text(home: &Path) -> String {
    std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn distilled_facts_are_fenced_and_later_messages_pass() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    forget_msg(&db, AGENT, 10).await;
    let engine = engine_at(&db);

    let r = store_facts_protected(
        &engine,
        AGENT,
        &[fact("the customer prefers oolong tea")],
        home.path(),
        &prov(10),
    )
    .await
    .unwrap();
    assert_eq!((r.stored, r.skipped), (0, 1));
    assert_eq!(count_content(&db, AGENT, "oolong").await, 0);
    let audit = audit_text(home.path());
    assert!(audit.contains("memory_write_fenced"), "{audit}");
    assert!(
        !audit.contains("oolong") && !audit.contains(S),
        "audit carries no content or session"
    );

    let r = store_facts_protected(
        &engine,
        AGENT,
        &[fact("the customer prefers oolong tea")],
        home.path(),
        &prov(11),
    )
    .await
    .unwrap();
    assert_eq!(r.stored, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn wiki_pointer_is_fenced_and_later_messages_pass() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    forget_msg(&db, AGENT, 10).await;
    let page = "auto/sop/refunds.md";
    assert!(
        !persist_wiki_pointer(
            AGENT,
            &db,
            home.path(),
            page,
            "退款",
            "摘要",
            &[test_msg(S, 10)]
        )
        .await
    );
    assert!(
        persist_wiki_pointer(
            AGENT,
            &db,
            home.path(),
            page,
            "退款",
            "摘要",
            &[test_msg(S, 11)]
        )
        .await
    );
    // No source at all ⇒ nothing written (never a fabricated source).
    assert!(!persist_wiki_pointer(AGENT, &db, home.path(), "auto/sop/x.md", "x", "x", &[]).await);
}

fn charter() -> String {
    let mut s = String::from("嘟嘟數位股份有限公司章程\n\n");
    for n in ["一", "二", "三", "四", "五"] {
        s.push_str(&format!(
            "第{n}條　本公司依公司法規定組織之，定名為嘟嘟數位股份有限公司。\
             本條規範業務範圍、股東權利義務、以及董事會之組成與職權行使方式，\
             並就股份轉讓、盈餘分派、虧損撥補等事項訂定明確之處理原則與程序。\n"
        ));
    }
    s
}

fn auto_pages(home: &Path, agent: &str) -> Vec<String> {
    let store = duduclaw_memory::WikiStore::new(home.join("agents").join(agent).join("wiki"));
    crate::auto_wiki_page::list_auto_pages(&store)
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.path)
        .collect()
}

/// G3 write-before check: a forgotten turn files no page and no memory; a
/// later turn of the same chat files its page, recorded with `m:<seq>`.
#[tokio::test(flavor = "multi_thread")]
async fn knowledge_page_checks_its_source_before_writing() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    forget_msg(&db, AGENT, 10).await;
    let before = count_content(&db, AGENT, "").await;

    run_ingest_inner(
        &charter(),
        "好的",
        AGENT,
        "u1",
        home.path(),
        &db,
        S,
        &[test_msg(S, 10)],
        Some(Err("offline".into())),
    )
    .await;
    assert!(
        auto_pages(home.path(), AGENT).is_empty(),
        "no page from a forgotten turn"
    );
    assert_eq!(
        count_content(&db, AGENT, "").await,
        before,
        "no memory row either"
    );

    run_ingest_inner(
        &charter(),
        "好的",
        AGENT,
        "u1",
        home.path(),
        &db,
        S,
        &[test_msg(S, 11)],
        Some(Err("offline".into())),
    )
    .await;
    let pages = auto_pages(home.path(), AGENT);
    assert_eq!(pages.len(), 1, "a later message files its page");
    let store =
        duduclaw_memory::WikiStore::new(home.path().join("agents").join(AGENT).join("wiki"));
    let page = store.read_page(&pages[0]).unwrap();
    assert_eq!(page.sources, vec![format!("conversation:{S}:m:11")]);
}

/// G3 write-after check: a forget applied while the page was being written
/// removes the page, and no pointer row is stored.
#[tokio::test(flavor = "multi_thread")]
async fn knowledge_page_forgotten_while_written_is_removed() {
    let agent = "agnes-race";
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    {
        let engine = engine_at(&db);
        test_seed(&engine, agent, "seed 20", test_msg(S, 20)).await;
    }
    let db_for_hook = db.clone();
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fired_in_hook = fired.clone();
    *PAGE_WRITTEN_HOOK.lock().unwrap() = Some((
        agent.to_string(),
        Box::new(move || {
            fired_in_hook.store(true, std::sync::atomic::Ordering::SeqCst);
            let db = db_for_hook.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let engine = SqliteMemoryEngine::new(&db).unwrap();
                    test_forget(&engine, "agnes-race", S, &["m:20"]).await;
                });
            })
            .join()
            .unwrap();
        }),
    ));
    run_ingest_inner(
        &charter(),
        "好的",
        agent,
        "u1",
        home.path(),
        &db,
        S,
        &[test_msg(S, 20)],
        Some(Err("offline".into())),
    )
    .await;
    *PAGE_WRITTEN_HOOK.lock().unwrap() = None;
    assert!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        "the page was written"
    );
    assert!(
        auto_pages(home.path(), agent).is_empty(),
        "page removed by the post-check"
    );
    assert_eq!(
        count_content(&db, agent, "已建檔於知識庫").await,
        0,
        "no pointer"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_traits_are_fenced_and_later_messages_pass() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    forget_msg(&db, AGENT, 10).await;
    let traits = |db: &Path| {
        let db = db.to_path_buf();
        async move {
            duduclaw_memory::user_profile::profile_traits(&engine_at(&db), AGENT, "u1")
                .await
                .unwrap()
                .len()
        }
    };
    crate::profile_distill::run_profile_distill(
        "以後請稱呼我老李。",
        AGENT,
        "u1",
        &db,
        home.path(),
        &[test_msg(S, 10)],
    )
    .await;
    assert_eq!(traits(&db).await, 0);
    crate::profile_distill::run_profile_distill(
        "以後請稱呼我老李。",
        AGENT,
        "u1",
        &db,
        home.path(),
        &[test_msg(S, 11)],
    )
    .await;
    assert_eq!(traits(&db).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn decisions_capture_and_resolve_are_fenced() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    forget_msg(&db, AGENT, 10).await;
    let engine = engine_at(&db);
    let draft = crate::decision_capture::DecisionDraft {
        question: "選哪個方案？".into(),
        options: vec![("A".into(), "方案甲".into()), ("B".into(), "方案乙".into())],
    };
    let e = crate::decision_capture::persist_decision(
        &engine,
        AGENT,
        "d1",
        &draft,
        serde_json::json!({}),
        prov(10),
    )
    .await
    .unwrap_err();
    assert!(
        crate::memory_provenance::fenced_parts(&e).is_some(),
        "{e}"
    );
    assert!(
        engine
            .list_open_decisions(AGENT, 10)
            .await
            .unwrap()
            .is_empty()
    );

    crate::decision_capture::persist_decision(
        &engine,
        AGENT,
        "d2",
        &draft,
        serde_json::json!({}),
        prov(11),
    )
    .await
    .unwrap();
    // The user's choice in the forgotten message cannot resolve it…
    let e = engine
        .resolve_decision(AGENT, "d2", "A", prov(10))
        .await
        .unwrap_err();
    assert!(crate::memory_provenance::fenced_parts(&e).is_some());
    // …a later message can.
    assert!(matches!(
        engine
            .resolve_decision(AGENT, "d2", "A", prov(12))
            .await
            .unwrap(),
        duduclaw_memory::DecisionResolveOutcome::Resolved { .. }
    ));
}

/// G9/G10 shape: a turn's key fact and episodic row carry both messages, so
/// forgetting either message fences the write.
#[tokio::test(flavor = "multi_thread")]
async fn turn_sources_fence_key_facts_and_episodic_rows() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    forget_msg(&db, AGENT, 21).await;
    let engine = engine_at(&db);
    let turn = |u: i64, a: i64| crate::memory_provenance::TurnSources {
        user: Some(test_msg(S, u)),
        assistant: Some(test_msg(S, a)),
    };
    let fenced = engine
        .store_fact_outcome(
            AGENT,
            "user likes jasmine",
            "telegram",
            "p2b",
            S,
            turn(20, 21).provenance().unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(
        fenced,
        duduclaw_memory::FactWriteOutcome::Fenced(_)
    ));
    let ok = engine
        .store_fact_outcome(
            AGENT,
            "user likes jasmine",
            "telegram",
            "p2b",
            S,
            turn(22, 23).provenance().unwrap(),
        )
        .await
        .unwrap();
    assert!(matches!(ok, duduclaw_memory::FactWriteOutcome::Stored(_)));
    assert!(
        crate::memory_provenance::TurnSources::default()
            .provenance()
            .is_none()
    );
}

/// G5: a review card is not filed for rows that were forgotten after they
/// were written.
#[tokio::test(flavor = "multi_thread")]
async fn review_card_is_not_filed_for_forgotten_rows() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    let engine = engine_at(&db);
    let id = test_seed(&engine, AGENT, "burst row", test_msg(S, 30)).await;
    test_forget(&engine, AGENT, S, &["m:30"]).await;
    let outcome = QuarantineOutcome {
        origin: DISTILL_ORIGIN.into(),
        subject: "s".into(),
        reason: "burst".into(),
        snippet: "burst row".into(),
        ids: vec![id.clone()],
        disposition: "quarantined",
        held: None,
    };
    dispatch_quarantine_side_effects(AGENT, home.path(), &db, &[outcome], None).await;
    assert_eq!(
        count_review_cards_for_ids(home.path(), &[id])
            .await
            .unwrap(),
        0
    );
}

/// `review_scrub`: a card covering forgotten rows is withdrawn and its text
/// replaced; a second run changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn review_cards_of_forgotten_rows_are_withdrawn_and_scrubbed() {
    let home = tempfile::tempdir().unwrap();
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let card = broker
        .request(
            AGENT,
            ACTION_KIND_KNOWLEDGE_QUARANTINE,
            "secret summary",
            serde_json::json!({ "quarantined_ids": ["m-1"], "snippet": "secret" }),
            3600,
        )
        .await
        .unwrap();
    assert_eq!(
        count_review_cards_for_ids(home.path(), &["m-1".into()])
            .await
            .unwrap(),
        1
    );
    let r = scrub_review_store_for_forgotten(home.path(), &["m-1".into()])
        .await
        .unwrap();
    assert_eq!((r.withdrawn, r.scrubbed), (1, 1));
    let rec = broker.get(&card).await.unwrap().unwrap();
    assert_ne!(rec.status, crate::approval::ApprovalStatus::Pending);
    assert_eq!(rec.decided_by.as_deref(), Some(DECIDED_BY_FORGET_SOURCE));
    assert!(!rec.summary.contains("secret") && !rec.payload.to_string().contains("secret"));
    let again = scrub_review_store_for_forgotten(home.path(), &["m-1".into()])
        .await
        .unwrap();
    assert_eq!(again.withdrawn, 0);
}

/// An auto page from a chat whose session id is too long for a plain source
/// entry records the digest form, and is still deleted when that message is
/// forgotten.
#[tokio::test(flavor = "multi_thread")]
async fn page_of_an_overlong_session_is_deleted_when_forgotten() {
    use crate::memory_forget_steps as steps;
    let agent = "agnes-long";
    let long = format!("discord:{}", "7".repeat(150));
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    let src = duduclaw_memory::SourceRef::channel_message(long.as_str(), 11, Utc::now(), None);
    run_ingest_inner(
        &charter(),
        "好的",
        agent,
        "u1",
        home.path(),
        &db,
        &long,
        &[src],
        Some(Err("offline".into())),
    )
    .await;
    let pages = auto_pages(home.path(), agent);
    assert_eq!(pages.len(), 1);
    let store =
        duduclaw_memory::WikiStore::new(home.path().join("agents").join(agent).join("wiki"));
    let entry = crate::memory_provenance::wiki_source_entry(&long, "m:11");
    assert!(entry.starts_with("conversation-digest:"));
    assert_eq!(store.read_page(&pages[0]).unwrap().sources, vec![entry]);

    let engine = engine_at(&db);
    let selector = duduclaw_memory::ForgetSelector {
        session: long.clone(),
        messages: vec!["m:11".into()],
        upto_seq: None,
        upto_time: Some(Utc::now()),
    };
    let ext = steps::collect_external_for_plan(
        &engine,
        home.path(),
        agent,
        &selector,
        Default::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ext.wiki_pages.len(), 1);
    let duduclaw_memory::PlanOutcome::Planned(plan) = engine
        .plan_forget_source(agent, &selector, Default::default(), &ext)
        .await
        .unwrap()
    else {
        panic!("no plan")
    };
    let ext = steps::collect_external_for_apply(home.path(), &plan)
        .await
        .unwrap();
    assert!(matches!(
        engine.apply_forget_plan(&plan.plan_id, &ext).await.unwrap(),
        duduclaw_memory::ApplyOutcome::Applied(_)
    ));
    assert!(
        steps::run_forget_steps(&engine, home.path(), &plan.plan_id)
            .await
            .unwrap()
            .complete()
    );
    assert!(auto_pages(home.path(), agent).is_empty());
}
