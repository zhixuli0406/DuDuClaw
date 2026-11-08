//! RM-039: conversation distillation never stores facts in an excluded
//! personal category, and the extraction prompts say so.

use std::path::Path;

use super::*;

const AGENT: &str = "agnes-privacy";

fn fact(content: &str) -> DistilledFact {
    DistilledFact {
        subject: None,
        predicate: None,
        object: None,
        content: content.to_string(),
        confidence: Some(0.8),
    }
}

fn triple(s: &str, p: &str, o: &str, content: &str) -> DistilledFact {
    DistilledFact {
        subject: Some(s.to_string()),
        predicate: Some(p.to_string()),
        object: Some(o.to_string()),
        content: content.to_string(),
        confidence: Some(0.8),
    }
}

async fn contents(db: &Path) -> Vec<String> {
    SqliteMemoryEngine::new(db)
        .unwrap()
        .list_recent(AGENT, 500)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.content)
        .collect()
}

/// No `config.toml` at all — the defaults (health, family, personal_finance,
/// salary) apply, exactly as on an existing install without the section.
#[tokio::test(flavor = "multi_thread")]
async fn persist_facts_drops_excluded_categories_by_default() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    persist_facts(
        AGENT,
        home.path(),
        &db,
        vec![
            fact("客戶要求下週交貨"),
            fact("小王上週住院開刀"),
            fact("Erin's salary is 80k a year"),
            triple("user:carol", "marital_status", "divorced", "Carol is divorced"),
            triple("user:dave", "monthly_salary", "50000", "Dave earns 50000 a month"),
            fact("他還有三百萬房貸"),
        ],
        &crate::memory_provenance::test_sources(),
    )
    .await;
    let stored = contents(&db).await;
    assert_eq!(stored, vec!["客戶要求下週交貨".to_string()], "{stored:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_can_untick_a_category() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[memory_privacy]\nexcluded_categories = [\"health\"]\n",
    )
    .unwrap();
    let db = home.path().join("memory.db");
    persist_facts(
        AGENT,
        home.path(),
        &db,
        vec![fact("小王上週住院開刀"), fact("老闆答應下季加薪")],
        &crate::memory_provenance::test_sources(),
    )
    .await;
    let stored = contents(&db).await;
    assert_eq!(stored, vec!["老闆答應下季加薪".to_string()], "{stored:?}");
}

/// A broken config can only make the filter stricter: the admin had turned
/// everything off, the file then became unparsable ⇒ defaults again.
#[tokio::test(flavor = "multi_thread")]
async fn unreadable_config_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[memory_privacy\nexcluded_categories = [").unwrap();
    let db = home.path().join("memory.db");
    persist_facts(
        AGENT,
        home.path(),
        &db,
        vec![fact("她的月薪是五萬"), fact("會議改到週三下午")],
        &crate::memory_provenance::test_sources(),
    )
    .await;
    let stored = contents(&db).await;
    assert_eq!(stored, vec!["會議改到週三下午".to_string()], "{stored:?}");
}

/// A charter-shaped document (章程 + 第…條 markers) — graded knowledge.
fn charter(extra: &str) -> String {
    let mut s = String::from("嘟嘟數位股份有限公司章程\n\n");
    for (i, n) in ["一", "二", "三", "四", "五"].iter().enumerate() {
        s.push_str(&format!(
            "第{n}條　本公司依公司法規定組織之，定名為嘟嘟數位股份有限公司。\
             本條規範第{}項業務範圍、股東權利義務、以及董事會之組成與職權行使方式，\
             並就股份轉讓、盈餘分派等事項訂定明確之處理原則與程序。\n",
            i + 1
        ));
    }
    s.push_str(extra);
    s
}

const CHARTER_LLM: &str = r#"{"facts": [{"content": "公司章程共有五條。"},
                                        {"content": "王經理上週確診，請假兩週。"}],
                             "knowledge_grade": true, "doc_type": "charter",
                             "page_title": "公司章程", "page_slug": "company-charter",
                             "summary": "本公司的組織章程。"}"#;

async fn ingest(home: &Path, text: &str) {
    run_ingest_inner(
        text,
        "好的，我記下來了。",
        AGENT,
        "u1",
        home,
        &home.join("memory.db"),
        "telegram:1:0",
        &crate::memory_provenance::test_sources(),
        Some(Ok(CHARTER_LLM.to_string())),
    )
    .await;
}

/// The knowledge route used to file the user's text verbatim into `auto/`
/// (plus a pointer row) without the gate. A document naming a colleague's
/// diagnosis files nothing; its facts take the memory path, gated there.
#[tokio::test(flavor = "multi_thread")]
async fn knowledge_page_with_sensitive_text_is_not_filed() {
    let auto = |h: &Path| h.join("agents").join(AGENT).join("wiki").join("auto");

    // Control: the same document without the sensitive line is filed.
    let clean = tempfile::tempdir().unwrap();
    ingest(clean.path(), &charter("")).await;
    assert!(auto(clean.path()).exists(), "fixture must reach the knowledge route");

    let home = tempfile::tempdir().unwrap();
    ingest(home.path(), &charter("附註：王經理上週確診，請假兩週。\n")).await;
    assert!(!auto(home.path()).exists(), "no auto page may be written");
    let rows = SqliteMemoryEngine::new(&home.path().join("memory.db"))
        .unwrap()
        .list_recent(AGENT, 500)
        .await
        .unwrap();
    assert!(
        rows.iter().all(|e| !e.tags.contains(&WIKI_POINTER_TAG.to_string())),
        "no pointer row"
    );
    let stored: Vec<String> = rows
        .into_iter()
        .filter(|e| e.source_event == DISTILL_SOURCE_EVENT)
        .map(|e| e.content)
        .collect();
    assert_eq!(stored, vec!["公司章程共有五條。".to_string()], "{stored:?}");
}

async fn put_pending(db: &Path, subject: &str, pred: &str, obj: &str) -> String {
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: AGENT.to_string(),
        content: format!("{pred}: {obj}"),
        timestamp: Utc::now(),
        tags: vec![],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: DISTILL_SOURCE_EVENT.to_string(),
    };
    let meta = TemporalMeta {
        subject: Some(subject.to_string()),
        predicate: Some(pred.to_string()),
        object: Some(obj.to_string()),
        quarantined: true,
        ..TemporalMeta::default()
    };
    SqliteMemoryEngine::new(db)
        .unwrap()
        .store_temporal(AGENT, entry, meta, duduclaw_memory::lineage::Provenance::test_only())
        .await
        .unwrap()
}

async fn row_state(db: &Path, id: &str) -> (i64, Option<String>) {
    let engine = SqliteMemoryEngine::new(db).unwrap();
    let conn = engine.conn_for_maintenance().await;
    conn.query_row(
        "SELECT quarantined, invalidated_by_event FROM memories WHERE id = ?1",
        rusqlite::params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

/// An admin approving a quarantined batch must not activate a row the
/// privacy gate refuses now — neither an excluded category nor a fact its
/// subject deleted in 「我的記憶」 after it was quarantined.
#[tokio::test(flavor = "multi_thread")]
async fn quarantine_approval_runs_the_privacy_gate() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("memory.db");
    let ok = put_pending(&db, "user:carol", "prefers", "烏龍茶").await;
    let sensitive = put_pending(&db, "user:carol", "health_note", "上週住院").await;
    let forgotten = put_pending(&db, "user:carol", "preferred_name", "小卡").await;
    crate::memory_privacy::forgotten::record(
        home.path(),
        &crate::memory_privacy::forgotten::key("user:carol", "preferred_name", "小卡"),
    )
    .unwrap();

    let report = apply_quarantine_decision(
        home.path().to_path_buf(),
        db.clone(),
        AGENT.to_string(),
        vec![ok.clone(), sensitive.clone(), forgotten.clone()],
        true,
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(report.released, 1, "{report:?}");
    assert_eq!(report.privacy_blocked.len(), 2, "{report:?}");
    assert_eq!(
        report.side_effect(true, false)["quarantine_privacy_blocked"],
        serde_json::json!(2)
    );

    assert_eq!(row_state(&db, &ok).await.0, 0, "the clean row is released");
    let (q, why) = row_state(&db, &sensitive).await;
    assert_eq!((q, why.as_deref()), (1, Some("privacy_excluded:health")));
    let (q, why) = row_state(&db, &forgotten).await;
    assert_eq!((q, why.as_deref()), (1, Some("privacy_forgotten")));
    let engine = SqliteMemoryEngine::new(&db).unwrap();
    assert!(engine
        .get_at(AGENT, "user:carol", "preferred_name", Utc::now())
        .await
        .unwrap()
        .is_none(), "a blocked row never becomes the current value");
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("memory_privacy_approval_blocked"), "{audit}");
    assert!(!audit.contains("住院") && !audit.contains("小卡"), "audit carries no content");
}

#[test]
fn ingest_prompts_carry_the_privacy_instruction() {
    let line = crate::memory_privacy::PrivacyGate::defaults()
        .prompt_instruction()
        .unwrap();
    let plain = build_cloud_ingest_prompt("u", "a");
    let with = build_cloud_ingest_prompt_with_privacy("u", "a", Some(&line));
    assert!(!plain.contains("PRIVACY"));
    assert!(with.contains(&line) && with.contains("請勿擷取"));
    assert_eq!(build_cloud_ingest_prompt_with_privacy("u", "a", None), plain);

    let kplain = build_cloud_ingest_prompt_with_knowledge("u", "a");
    let kwith = build_cloud_ingest_prompt_with_knowledge_and_privacy("u", "a", Some(&line));
    assert!(kwith.contains(&line));
    assert_eq!(
        build_cloud_ingest_prompt_with_knowledge_and_privacy("u", "a", None),
        kplain
    );
}
