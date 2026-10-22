//! Hybrid retrieval: the vector list, its fallback notices and the ACL.

use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{Connection, params};

use super::*;
use crate::acl::{HumanCtx, Narrowing, effective_collections};
use crate::embed::{Egress, EmbedError, Embedder, UnavailableEmbedder};
use crate::index::test_support::indexed_source;
use crate::limits::KnowledgeConfig;
use crate::store::{NewCollection, create_collection};

/// Three "concepts"; a text's vector is which concept words it contains.
/// Stands in for a real model: paraphrases with no shared characters land
/// on the same axis.
struct ConceptEmbedder {
    egress: Egress,
    fail_query: bool,
    queries: AtomicUsize,
}

const CONCEPTS: [&[&str]; 3] = [
    &["特休", "休假", "放假", "請假", "假"],
    &["薪", "錢", "加班費", "費"],
    &["鞋", "產品", "型號"],
];

fn concept_vec(text: &str) -> Vec<f32> {
    let v: Vec<f32> = CONCEPTS
        .iter()
        .map(|words| words.iter().filter(|w| text.contains(*w)).count() as f32)
        .collect();
    crate::embed::pooling::l2_normalize(v)
}

impl ConceptEmbedder {
    fn local() -> Self {
        Self {
            egress: Egress::Local,
            fail_query: false,
            queries: AtomicUsize::new(0),
        }
    }
}

impl Embedder for ConceptEmbedder {
    fn model_id(&self) -> &str {
        "test/concepts@3"
    }
    fn dim(&self) -> usize {
        3
    }
    fn max_input_tokens(&self) -> usize {
        512
    }
    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(texts.iter().map(|t| concept_vec(t)).collect())
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        if self.fail_query {
            return Err(EmbedError::Failed("雲端錯誤：HTTP 500".into()));
        }
        Ok(concept_vec(text))
    }
    fn egress(&self) -> Egress {
        self.egress.clone()
    }
}

fn embed_all(conn: &mut Connection, e: &dyn Embedder) {
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, text FROM chunks").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let vecs: Vec<(i64, Vec<f32>)> = rows
        .iter()
        .map(|(id, t)| (*id, e.embed_passages(&[t.as_str()]).unwrap().remove(0)))
        .collect();
    crate::embed::vectors::store(conn, e.model_id(), &vecs).unwrap();
}

fn setup() -> (Connection, String, String) {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let cfg = KnowledgeConfig::default();
    let hr = create_collection(
        &mut conn,
        &NewCollection {
            name: "人資".into(),
            ..Default::default()
        },
        "hr",
        &cfg,
    )
    .unwrap();
    let sales = create_collection(
        &mut conn,
        &NewCollection {
            name: "業務".into(),
            ..Default::default()
        },
        "sales",
        &cfg,
    )
    .unwrap();
    indexed_source(
        &mut conn,
        &hr.id,
        "員工手冊.pdf",
        &["新進員工第一年特休為七日。", "加班費依勞基法計算。"],
    );
    indexed_source(
        &mut conn,
        &sales.id,
        "型錄.pdf",
        &["防滑安全鞋單價一千二百元。"],
    );
    // Grant the HR user read access to nothing else.
    (conn, hr.id, sales.id)
}

fn allowed(conn: &Connection, user: &str) -> AllowedSet {
    effective_collections(
        conn,
        &HumanCtx::dashboard(user, None, false),
        None,
        &Narrowing::default(),
    )
    .unwrap()
}

#[test]
fn paraphrase_without_shared_terms_is_found_by_vectors() {
    let (mut conn, hr, _) = setup();
    let e = ConceptEmbedder::local();
    embed_all(&mut conn, &e);
    let cache = VectorCache::default();
    // 「放假」 shares no trigram and no LIKE term with 「特休為七日」.
    let kw = search(
        &conn,
        &allowed(&conn, "hr"),
        "我可以放幾天",
        5,
        &NoEmbedderAlias,
        &cache,
    )
    .unwrap();
    assert!(
        kw.hits.iter().all(|h| !h.text.contains("特休")),
        "keywords alone miss it"
    );
    let r = search(
        &conn,
        &allowed(&conn, "hr"),
        "放假可以放幾天",
        5,
        &e,
        &cache,
    )
    .unwrap();
    assert_eq!(r.mode, RetrievalMode::Hybrid);
    assert!(r.vector_unavailable.is_none());
    assert_eq!(r.coverage, Some(1.0));
    let top = &r.hits[0];
    assert!(top.text.contains("特休"), "{:?}", r.hits);
    assert_eq!(top.vec_rank, Some(1));
    assert!(top.vec_score.unwrap() > 0.4, "{:?}", top.vec_score);
    assert!(
        r.hits.iter().all(|h| h.collection_id == hr),
        "vectors never cross the ACL"
    );
}

use crate::embed::NoEmbedder as NoEmbedderAlias;

#[test]
fn vector_hits_never_leave_the_allowed_set() {
    let (mut conn, hr, sales) = setup();
    let e = ConceptEmbedder::local();
    embed_all(&mut conn, &e);
    let cache = VectorCache::default();
    // The HR user asks about shoes: the only matching vector is in 業務.
    let r = search(&conn, &allowed(&conn, "hr"), "鞋子", 20, &e, &cache).unwrap();
    assert!(r.hits.iter().all(|h| h.collection_id == hr), "{:?}", r.hits);
    let r = search(&conn, &allowed(&conn, "sales"), "鞋子", 20, &e, &cache).unwrap();
    assert!(
        r.hits
            .iter()
            .any(|h| h.collection_id == sales && h.vec_rank == Some(1))
    );
}

#[test]
fn unavailable_model_falls_back_to_keywords_with_a_visible_reason() {
    let (conn, _, _) = setup();
    let e = UnavailableEmbedder::new("向量模型檔驗證失敗，請重新安裝");
    let r = search(
        &conn,
        &allowed(&conn, "hr"),
        "特休",
        5,
        &e,
        &VectorCache::default(),
    )
    .unwrap();
    assert_eq!(r.mode, RetrievalMode::KeywordOnly);
    assert_eq!(
        r.vector_unavailable.as_deref(),
        Some("向量模型檔驗證失敗，請重新安裝")
    );
    assert!(!r.hits.is_empty(), "keywords still answer");
}

#[test]
fn no_vectors_yet_means_keyword_only_and_no_query_embedding() {
    let (conn, _, _) = setup();
    let e = ConceptEmbedder::local();
    let r = search(
        &conn,
        &allowed(&conn, "hr"),
        "特休",
        5,
        &e,
        &VectorCache::default(),
    )
    .unwrap();
    assert_eq!(r.mode, RetrievalMode::KeywordOnly);
    assert_eq!(r.vector_unavailable.as_deref(), Some(NO_VECTORS_YET_REASON));
    assert_eq!(e.queries.load(Ordering::SeqCst), 0);
}

#[test]
fn a_failed_query_embedding_is_reported_not_hidden() {
    let (mut conn, _, _) = setup();
    let mut e = ConceptEmbedder::local();
    embed_all(&mut conn, &e);
    e.fail_query = true;
    let r = search(
        &conn,
        &allowed(&conn, "hr"),
        "特休",
        5,
        &e,
        &VectorCache::default(),
    )
    .unwrap();
    assert_eq!(r.mode, RetrievalMode::KeywordOnly);
    assert!(r.vector_unavailable.unwrap().contains("HTTP 500"));
}

#[test]
fn cloud_mode_never_sends_a_query_for_local_only_collections() {
    let (mut conn, hr, _) = setup();
    let local = ConceptEmbedder::local();
    embed_all(&mut conn, &local);
    conn.execute(
        "UPDATE collections SET embedding_policy = 'local_only' WHERE id = ?1",
        params![hr],
    )
    .unwrap();
    let cloud = ConceptEmbedder {
        egress: Egress::Cloud {
            provider: "openai".into(),
            host: "api.openai.com".into(),
        },
        fail_query: false,
        queries: AtomicUsize::new(0),
    };
    let r = search(
        &conn,
        &allowed(&conn, "hr"),
        "放假",
        5,
        &cloud,
        &VectorCache::default(),
    )
    .unwrap();
    assert_eq!(r.mode, RetrievalMode::KeywordOnly);
    assert_eq!(r.vector_unavailable.as_deref(), Some(CLOUD_EXCLUDED_REASON));
    assert_eq!(
        cloud.queries.load(Ordering::SeqCst),
        0,
        "the query must not leave the machine"
    );
}

#[test]
fn partial_coverage_is_reported() {
    let (mut conn, hr, _) = setup();
    indexed_source(&mut conn, &hr, "請假辦法.pdf", &["請假需於三日前提出。"]);
    let e = ConceptEmbedder::local();
    let first: (i64, String) = conn
        .query_row(
            "SELECT id, text FROM chunks WHERE collection_id = ?1 ORDER BY id LIMIT 1",
            params![hr],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let v = e.embed_passages(&[first.1.as_str()]).unwrap().remove(0);
    crate::embed::vectors::store(&mut conn, e.model_id(), &[(first.0, v)]).unwrap();
    let r = search(
        &conn,
        &allowed(&conn, "hr"),
        "特休",
        5,
        &e,
        &VectorCache::default(),
    )
    .unwrap();
    assert_eq!(r.mode, RetrievalMode::Hybrid);
    let c = r.coverage.unwrap();
    assert!(c > 0.0 && c < 1.0, "{c}");
}
