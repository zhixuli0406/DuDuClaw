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
