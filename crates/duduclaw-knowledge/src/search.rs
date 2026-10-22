//! Retrieval over an [`AllowedSet`]. Every list is filtered by the allowed
//! collection ids inside SQL, and every result is re-checked against the set
//! before it is returned.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::KbResult;
use crate::acl::AllowedSet;
use crate::embed::vector_cache::VectorCache;
use crate::embed::{EmbedError, Egress, Embedder, is_available, vectors};
use crate::fts::{like_pattern, plan_query};
use crate::locator::{Locator, LocatorKind};
use crate::rank::{cap_per_source, rrf};

/// Candidates taken from each list before fusion.
pub const LIST_LIMIT: usize = 50;
/// Rows the short-term `LIKE` fallback scans at most.
pub const MAX_LIKE_SCAN: usize = 20_000;
/// Largest result size a caller may ask for.
pub const MAX_RESULTS: usize = 20;
/// Below this many FTS candidates the two-character fallback terms join the
/// `LIKE` scan.
pub const FALLBACK_BELOW: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalMode {
    Hybrid,
    KeywordOnly,
    VectorOnly,
    /// Nothing could be searched (no allowed collection, no usable term).
    None,
}

impl RetrievalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RetrievalMode::Hybrid => "hybrid",
            RetrievalMode::KeywordOnly => "keyword_only",
            RetrievalMode::VectorOnly => "vector_only",
            RetrievalMode::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub chunk_id: i64,
    pub source_id: String,
    pub collection_id: String,
    pub title: String,
    pub locator: Locator,
    pub locator_text: String,
    pub heading_path: String,
    pub text: String,
    pub score: f32,
    pub fts_rank: Option<u32>,
    pub like_rank: Option<u32>,
    pub vec_rank: Option<u32>,
    /// Cosine similarity to the query when the vector list had this chunk.
    #[serde(default)]
    pub vec_score: Option<f32>,
    /// Flagged at index time as instruction-like text.
    pub suspect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<Hit>,
    pub mode: RetrievalMode,
    /// zh-TW reason when vector retrieval was not used.
    pub vector_unavailable: Option<String>,
    /// Share of the searched chunks that have a vector of the model in use
    /// (`None` when vectors were not used or nothing is eligible).
    #[serde(default)]
    pub coverage: Option<f32>,
    /// The `LIKE` fallback hit its scan ceiling.
    pub scan_capped: bool,
}

/// What the vector side contributed to one search.
struct VectorSide {
    /// `(chunk_id, cosine)`, best first, already inside the allowed set.
    hits: Vec<(i64, f32)>,
    unavailable: Option<String>,
    coverage: Option<f32>,
}

impl VectorSide {
    fn off(reason: String) -> Self {
        Self { hits: Vec::new(), unavailable: Some(reason), coverage: None }
    }
}

/// zh-TW: every allowed collection is kept off the cloud provider.
pub const CLOUD_EXCLUDED_REASON: &str =
    "這些集合設定為只在本機計算向量（或屬機密），使用雲端向量時只以關鍵字檢索";
/// zh-TW: vectors exist for none of the allowed passages yet.
pub const NO_VECTORS_YET_REASON: &str = "向量尚未建立完成，目前只以關鍵字檢索";

fn unavailable_reason(embedder: &dyn Embedder) -> String {
    match embedder.embed_query("") {
        Err(EmbedError::Unavailable(r) | EmbedError::Failed(r)) => r,
        Ok(_) => crate::embed::KEYWORD_ONLY_REASON.to_string(),
    }
}

/// Keep only chunks whose source is searchable right now.
fn searchable(conn: &Connection, hits: Vec<(i64, f32)>) -> KbResult<Vec<(i64, f32)>> {
    if hits.is_empty() {
        return Ok(hits);
    }
    let ids: Vec<i64> = hits.iter().map(|h| h.0).collect();
    let mut stmt = conn.prepare_cached(
        "SELECT c.id FROM chunks c JOIN sources s ON s.id = c.source_id
          WHERE c.id IN (SELECT value FROM json_each(?1)) AND s.status IN ('embedding','ready')",
    )?;
    let ok: std::collections::HashSet<i64> = stmt
        .query_map(params![serde_json::to_string(&ids).unwrap_or_default()], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(hits.into_iter().filter(|h| ok.contains(&h.0)).collect())
}

fn vector_side(
    conn: &Connection,
    allowed: &AllowedSet,
    query: &str,
    embedder: &dyn Embedder,
    cache: &VectorCache,
) -> KbResult<VectorSide> {
    if !is_available(embedder) {
        return Ok(VectorSide::off(unavailable_reason(embedder)));
    }
    let excluded = match embedder.egress() {
        Egress::Cloud { .. } => vectors::cloud_excluded_collections(conn)?,
        Egress::Local => Default::default(),
    };
    let eligible: Vec<String> = allowed.ids().iter().filter(|c| !excluded.contains(*c)).cloned().collect();
    if eligible.is_empty() {
        return Ok(VectorSide::off(CLOUD_EXCLUDED_REASON.to_string()));
    }
    let (have, total) = vectors::coverage(conn, embedder.model_id(), Some(&eligible), &excluded)?;
    if have == 0 {
        // Nothing to compare against: do not compute (or send) the query.
        return Ok(VectorSide::off(NO_VECTORS_YET_REASON.to_string()));
    }
    let coverage = (total > 0).then(|| have as f32 / total as f32);
    let q = match embedder.embed_query(query) {
        Ok(q) => q,
        Err(EmbedError::Unavailable(r) | EmbedError::Failed(r)) => {
            tracing::warn!(reason = %r, "knowledge: query embedding failed; keyword-only for this search");
            return Ok(VectorSide::off(r));
        }
    };
    let snap = cache.snapshot(conn, embedder.model_id(), embedder.dim())?;
    let hits = searchable(conn, snap.knn(&q, allowed, &excluded, LIST_LIMIT))?;
    Ok(VectorSide { hits, unavailable: None, coverage })
}

/// A chunk row with its source title.
pub(crate) struct ChunkRow {
    pub id: i64,
    pub source_id: String,
    pub collection_id: String,
    pub text: String,
    pub heading_path: String,
    pub locator: Locator,
    pub flags: i64,
    pub title: String,
    pub ord: i64,
    pub char_start: i64,
    pub char_end: i64,
    pub segment_ord: i64,
}

pub(crate) const CHUNK_COLS: &str = "c.id, c.source_id, c.collection_id, c.text, c.heading_path, c.locator_kind, c.page_start, c.page_end, c.sheet, c.row_start, c.row_end, c.para_index, c.flags, s.title, c.ord, c.char_start, c.char_end, c.segment_ord";

pub(crate) fn row_to_chunk(r: &Row<'_>) -> rusqlite::Result<ChunkRow> {
    let kind: String = r.get(5)?;
    let heading: String = r.get(4)?;
    let mut locator = Locator::new(LocatorKind::parse(&kind).unwrap_or(LocatorKind::Paragraph));
    locator.page_start = r.get(6)?;
    locator.page_end = r.get(7)?;
    locator.sheet = r.get(8)?;
    locator.row_start = r.get(9)?;
    locator.row_end = r.get(10)?;
    locator.para_index = r.get(11)?;
    if matches!(locator.kind, LocatorKind::Section | LocatorKind::Url) && !heading.is_empty() {
        locator.heading = Some(heading.clone());
    }
    Ok(ChunkRow {
        id: r.get(0)?,
        source_id: r.get(1)?,
        collection_id: r.get(2)?,
        text: r.get(3)?,
        heading_path: heading,
        locator,
        flags: r.get(12)?,
        title: r.get(13)?,
        ord: r.get(14)?,
        char_start: r.get(15)?,
        char_end: r.get(16)?,
        segment_ord: r.get(17)?,
    })
}

pub(crate) fn load_chunk(conn: &Connection, id: i64) -> KbResult<Option<ChunkRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {CHUNK_COLS} FROM chunks c JOIN sources s ON s.id = c.source_id WHERE c.id = ?1"),
            params![id],
            row_to_chunk,
        )
        .optional()?)
}

fn fts_list(conn: &Connection, allowed: &AllowedSet, expr: &str) -> KbResult<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT c.id FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid
           JOIN sources s ON s.id = c.source_id
          WHERE chunks_fts MATCH ?1
            AND c.collection_id IN (SELECT value FROM json_each(?2))
            AND s.status IN ('embedding','ready')
          ORDER BY bm25(chunks_fts, 1.0, 0.5), c.id
          LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![expr, allowed.json(), LIST_LIMIT as i64], |r| {
        r.get(0)
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn like_list(
    conn: &Connection,
    allowed: &AllowedSet,
    terms: &[String],
) -> KbResult<(Vec<i64>, bool)> {
    if terms.is_empty() {
        return Ok((Vec::new(), false));
    }
    let candidates: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks c JOIN sources s ON s.id = c.source_id
          WHERE c.collection_id IN (SELECT value FROM json_each(?1)) AND s.status IN ('embedding','ready')",
        params![allowed.json()],
        |r| r.get(0),
    )?;
    let capped = candidates as usize > MAX_LIKE_SCAN;
    if capped {
        tracing::warn!(
            candidates,
            "knowledge: short-term search scan capped at {MAX_LIKE_SCAN} rows"
        );
    }
    let conds: Vec<String> = (0..terms.len())
        .map(|i| {
            format!(
                "match_text LIKE ?{n} ESCAPE '\\' OR match_heading LIKE ?{n} ESCAPE '\\'",
                n = i + 3
            )
        })
        .collect();
    let sql = format!(
        "SELECT id, match_text, match_heading FROM (
           SELECT c.id, c.match_text, c.match_heading FROM chunks c JOIN sources s ON s.id = c.source_id
            WHERE c.collection_id IN (SELECT value FROM json_each(?1)) AND s.status IN ('embedding','ready')
            ORDER BY c.id DESC LIMIT ?2)
          WHERE {}",
        conds.join(" OR ")
    );
    let mut values: Vec<rusqlite::types::Value> =
        vec![allowed.json().into(), (MAX_LIKE_SCAN as i64).into()];
    values.extend(terms.iter().map(|t| like_pattern(t).into()));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut scored: Vec<(usize, i64)> = Vec::new();
    for row in rows {
        let (id, text, heading) = row?;
        let hay = format!("{}\n{}", text.to_lowercase(), heading.to_lowercase());
        // Ranking only (not a security decision): how many terms appear.
        let n = terms.iter().filter(|t| hay.contains(t.as_str())).count();
        scored.push((n, id));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    Ok((
        scored
            .into_iter()
            .take(LIST_LIMIT)
            .map(|(_, id)| id)
            .collect(),
        capped,
    ))
}

/// Load, re-check against `allowed`, cap per source and build hits for a
/// fused list whose third rank column (if any) is the vector rank.
fn assemble(
    conn: &Connection,
    allowed: &AllowedSet,
    fused: Vec<crate::rank::Fused>,
    vec_scores: &HashMap<i64, f32>,
    limit: usize,
) -> KbResult<Vec<Hit>> {
    let mut rows: HashMap<i64, ChunkRow> = HashMap::new();
    for f in &fused {
        if let Some(row) = load_chunk(conn, f.chunk_id)? {
            // Invariant: nothing outside the allowed set leaves this module.
            if allowed.contains(&row.collection_id) {
                rows.insert(row.id, row);
            }
        }
    }
    let source_of: HashMap<i64, String> = rows.iter().map(|(id, r)| (*id, r.source_id.clone())).collect();
    let kept = cap_per_source(fused, &source_of, limit);
    Ok(kept
        .into_iter()
        .filter_map(|f| {
            let row = rows.remove(&f.chunk_id)?;
            let rank = |i: usize| f.ranks.get(i).copied().flatten();
            Some(Hit {
                chunk_id: row.id,
                source_id: row.source_id,
                collection_id: row.collection_id,
                title: row.title,
                locator_text: row.locator.display(),
                locator: row.locator,
                heading_path: row.heading_path,
                text: row.text,
                score: f.score,
                fts_rank: rank(0),
                like_rank: rank(1),
                vec_rank: rank(2),
                vec_score: vec_scores.get(&f.chunk_id).copied(),
                suspect: row.flags & crate::index::FLAG_SUSPECT != 0,
            })
        })
        .collect())
}

/// Vector list only (no keyword lists) — for `duduclaw knowledge eval
/// --modes vector`, to see what the model alone contributes.
pub fn vector_only(
    conn: &Connection,
    allowed: &AllowedSet,
    query: &str,
    limit: usize,
    embedder: &dyn Embedder,
    cache: &VectorCache,
) -> KbResult<SearchResponse> {
    let limit = limit.clamp(1, MAX_RESULTS);
    if allowed.is_empty() || query.trim().is_empty() {
        return Ok(SearchResponse { hits: Vec::new(), mode: RetrievalMode::None, vector_unavailable: None, coverage: None, scan_capped: false });
    }
    let vec = vector_side(conn, allowed, query, embedder, cache)?;
    if vec.unavailable.is_some() {
        return Ok(SearchResponse { hits: Vec::new(), mode: RetrievalMode::None, vector_unavailable: vec.unavailable, coverage: None, scan_capped: false });
    }
    let scores: HashMap<i64, f32> = vec.hits.iter().copied().collect();
    let ids: Vec<i64> = vec.hits.iter().map(|h| h.0).collect();
    let hits = assemble(conn, allowed, rrf(&[Vec::new(), Vec::new(), ids]), &scores, limit)?;
    Ok(SearchResponse { hits, mode: RetrievalMode::VectorOnly, vector_unavailable: None, coverage: vec.coverage, scan_capped: false })
}

/// Search `allowed` for `query`, returning at most `limit` hits.
///
/// Keyword lists (FTS, short-term `LIKE`) always run; the vector list runs
/// when `embedder` is usable and the allowed collections have vectors of
/// its model. A vector side that cannot run is reported in
/// `vector_unavailable`, never skipped silently.
pub fn search(
    conn: &Connection,
    allowed: &AllowedSet,
    query: &str,
    limit: usize,
    embedder: &dyn Embedder,
    cache: &VectorCache,
) -> KbResult<SearchResponse> {
    let limit = limit.clamp(1, MAX_RESULTS);
    let plan = plan_query(query);
    if allowed.is_empty() || query.trim().is_empty() {
        let reason = (!is_available(embedder)).then(|| unavailable_reason(embedder));
        return Ok(SearchResponse {
            hits: Vec::new(),
            mode: RetrievalMode::None,
            vector_unavailable: reason,
            coverage: None,
            scan_capped: false,
        });
    }
    let vec = vector_side(conn, allowed, query, embedder, cache)?;
    if plan.is_empty() && vec.unavailable.is_some() {
        return Ok(SearchResponse {
            hits: Vec::new(),
            mode: RetrievalMode::None,
            vector_unavailable: vec.unavailable,
            coverage: None,
            scan_capped: false,
        });
    }
    let fts = match plan.fts_match.as_deref() {
        Some(expr) => fts_list(conn, allowed, expr)?,
        None => Vec::new(),
    };
    let mut like_terms = plan.like_terms.clone();
    if fts.len() < FALLBACK_BELOW {
        like_terms.extend(plan.fallback_terms.iter().cloned());
    }
    let (like, scan_capped) = like_list(conn, allowed, &like_terms)?;
    let vec_scores: HashMap<i64, f32> = vec.hits.iter().copied().collect();
    let vec_ids: Vec<i64> = vec.hits.iter().map(|h| h.0).collect();
    let hits = assemble(conn, allowed, rrf(&[fts, like, vec_ids]), &vec_scores, limit)?;
    let mode = match (vec.unavailable.is_none(), plan.is_empty()) {
        (true, false) => RetrievalMode::Hybrid,
        (true, true) => RetrievalMode::VectorOnly,
        (false, _) => RetrievalMode::KeywordOnly,
    };
    Ok(SearchResponse {
        hits,
        mode,
        vector_unavailable: vec.unavailable,
        coverage: vec.coverage,
        scan_capped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{HumanCtx, Narrowing, effective_collections};
    use crate::embed::NoEmbedder;
    use crate::index::test_support::indexed_source;
    use crate::limits::KnowledgeConfig;
    use crate::store::{NewCollection, create_collection};

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
            &[
                "新進員工第一年特休假為七日。",
                "請假需於三日前提出申請。",
                "加班費依勞基法計算。",
            ],
        );
        indexed_source(
            &mut conn,
            &sales.id,
            "報價單.pdf",
            &["產品 A-1234 單價 NT$1,200。", "HR 系統授權費另計。"],
        );
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
    fn trigram_and_two_char_terms_find_passages() {
        let (conn, _, _) = setup();
        let set = allowed(&conn, "hr");
        let r = search(&conn, &set, "第一年特休幾天", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert_eq!(r.mode, RetrievalMode::KeywordOnly);
        assert_eq!(
            r.vector_unavailable.as_deref(),
            Some(crate::embed::KEYWORD_ONLY_REASON)
        );
        assert!(r.hits[0].text.contains("特休假為七日"));
        // Short pages join the next page's first chunk: 「第 1–3 頁」.
        assert!(
            r.hits[0].locator_text.starts_with("第 1"),
            "{}",
            r.hits[0].locator_text
        );
        let r = search(&conn, &set, "請假", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert!(r.hits[0].text.contains("請假需於三日前"));
        assert_eq!(r.hits[0].like_rank, Some(1));
        // No trigram of 「加班費率」 beyond 「加班費」 exists; particles split runs.
        let r = search(&conn, &set, "加班的費用", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert!(r.hits[0].text.contains("加班費"), "{:?}", r.hits);
        // No trigram of 「特休天數」 is in the text; the bigram fallback finds 「特休」.
        let r = search(&conn, &set, "特休天數", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert!(
            r.hits
                .first()
                .is_some_and(|h| h.text.contains("特休假") && h.fts_rank.is_none()),
            "{:?}",
            r.hits
        );
    }

    #[test]
    fn results_never_leave_the_allowed_set() {
        let (conn, hr, sales) = setup();
        let set = allowed(&conn, "hr");
        assert!(set.contains(&hr) && !set.contains(&sales));
        for q in ["A-1234", "HR", "單價", "授權費另計", "特休"] {
            let r = search(&conn, &set, q, 20, &NoEmbedder, &VectorCache::default()).unwrap();
            assert!(r.hits.iter().all(|h| h.collection_id == hr), "{q} leaked");
        }
        let r = search(&conn, &allowed(&conn, "sales"), "A-1234", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert!(r.hits[0].text.contains("A-1234"));
        let r = search(&conn, &allowed(&conn, "sales"), "HR", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert!(r.hits[0].text.contains("HR 系統"));
    }

    #[test]
    fn empty_allowed_set_or_query_returns_none_mode() {
        let (conn, _, _) = setup();
        let r = search(&conn, &AllowedSet::default(), "特休", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert_eq!(r.mode, RetrievalMode::None);
        assert!(r.hits.is_empty());
        let r = search(&conn, &allowed(&conn, "hr"), "的", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert_eq!(r.mode, RetrievalMode::None);
    }

    #[test]
    fn fts_syntax_in_query_is_harmless() {
        let (conn, _, _) = setup();
        let set = allowed(&conn, "hr");
        for q in [
            "\"",
            "NEAR(特休 請假)",
            "特休*",
            "-請假",
            "text:特休",
            "a OR b AND",
        ] {
            search(&conn, &set, q, 5, &NoEmbedder, &VectorCache::default()).unwrap();
        }
    }

    #[test]
    fn display_keeps_full_width_and_matching_folds_it() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let c = create_collection(
            &mut conn,
            &NewCollection {
                name: "型錄".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        indexed_source(
            &mut conn,
            &c.id,
            "型錄.pdf",
            &["產品Ａ－１２３４，單價⼀千元！ＨＲ部門負責。"],
        );
        let set = allowed(&conn, "u");
        for q in ["A-1234", "ａ－１２３４", "hr", "一千元"] {
            let r = search(&conn, &set, q, 5, &NoEmbedder, &VectorCache::default()).unwrap();
            let hit = r.hits.first().unwrap_or_else(|| panic!("{q} not found"));
            assert_eq!(
                hit.text, "產品Ａ－１２３４，單價⼀千元！ＨＲ部門負責。",
                "display text is the original"
            );
        }
    }

    #[test]
    fn non_ready_sources_are_not_searched() {
        let (conn, hr, _) = setup();
        conn.execute(
            "UPDATE sources SET status = 'indexing' WHERE collection_id = ?1",
            params![hr],
        )
        .unwrap();
        let r = search(&conn, &allowed(&conn, "hr"), "特休假", 5, &NoEmbedder, &VectorCache::default()).unwrap();
        assert!(r.hits.is_empty());
    }
}

#[cfg(test)]
#[path = "search_vec_tests.rs"]
mod vec_tests;
