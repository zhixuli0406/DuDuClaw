//! Night Engine memory passes — N3 schema induction + N4 recurrence-gated
//! consolidation with deterministic trust verification.
//!
//! These are the always-deterministic (zero-LLM) half of the Night Engine. The
//! gateway's `night_engine` orchestrator drives them during idle windows; the
//! LLM-backed N1/N2 passes live in the gateway crate.
//!
//! - **N3 Schema induction** (DCPM, arXiv:2606.09483): scan an agent's episodic
//!   memory, find token themes recurring across `>= min_support` distinct
//!   memories, and promote each into a `night-schema` semantic entry. The daytime
//!   writer is the existing temporal supersession chain (which DCPM independently
//!   validates); this is the nightly System-2 pass DCPM says was missing.
//! - **N4 Recurrence-gated consolidation** (RecMem, arXiv:2605.16045): only a
//!   theme that semantically recurs `>= recurrence_threshold` times triggers a
//!   consolidation (token-frugal — most themes never do). The merged statement
//!   then passes a deterministic **coverage / preservation / faithfulness** gate
//!   (TRUSTMEM, arXiv:2606.25161) before it is written; a failing merge is rolled
//!   back (never stored), so the store can't get "worse the more it tidies".
//!
//! Everything except the two engine-touching async entry points is a pure
//! function with unit tests.

use std::collections::{BTreeMap, HashSet};

use duduclaw_core::error::Result;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};

use crate::engine::{SqliteMemoryEngine, TemporalMeta};

/// A recurring token theme detected across an agent's episodic memories.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// The normalized token that anchors this theme.
    pub key: String,
    /// Number of distinct memories the theme appears in (document frequency).
    pub support: u32,
    /// Ids of the supporting memories (sorted, deduped), capped for stability.
    pub source_ids: Vec<String>,
    /// Short representative snippets from the supporting memories.
    pub snippets: Vec<String>,
}

/// Deterministic three-axis verification of a consolidation (TRUSTMEM).
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationReport {
    /// Fraction of source memories represented in the consolidated text (each
    /// source must share at least one salient token). 1.0 = every source covered.
    pub coverage: f64,
    /// Fraction of the union of source salient tokens retained in the
    /// consolidated text. Guards against over-compression that drops facts.
    pub preservation: f64,
    /// 1.0 − (fraction of consolidated salient tokens that appear in NO source
    /// and are not scaffold vocabulary). 1.0 = nothing hallucinated.
    pub faithfulness: f64,
    /// Whether all three axes clear their thresholds.
    pub passed: bool,
}

// Thresholds for the deterministic gate. Conservative on faithfulness (we never
// want to admit hallucinated content) and lenient on preservation (compression
// is the point).
const COVERAGE_MIN: f64 = 0.8;
const PRESERVATION_MIN: f64 = 0.5;
const FAITHFULNESS_MIN: f64 = 0.95;

/// Importance ceiling for night-promoted entries (N3 schemas / N4
/// consolidations). Their inputs are episodic memories, which are largely
/// channel-derived — i.e. attacker-reachable chat content. Retrieval ranks by
/// `w_importance · importance/10` and Ebbinghaus stability scales with
/// `importance/5`, so an importance above the 5.0 default would let repeated
/// injected chat lines launder themselves into top-ranked durable memory.
/// Capping at the neutral default means recurrence alone can never outrank
/// curated (operator/agent-stored, higher-importance) entries. Provenance is
/// preserved via the `night-schema` / `night-consolidated` tags and the
/// `source: episodic-recurrence` metadata field.
const NIGHT_PROMOTION_MAX_IMPORTANCE: f64 = 5.0;

/// Result of an N3 schema-induction store.
#[derive(Debug, Clone)]
pub struct InducedSchema {
    pub memory_id: String,
    pub key: String,
    pub support: u32,
}

/// Per-theme outcome of an N4 consolidation attempt.
#[derive(Debug, Clone)]
pub struct ConsolidationResult {
    pub key: String,
    pub support: u32,
    pub report: VerificationReport,
    /// `Some(id)` when the merge passed verification and was written; `None`
    /// when it was rolled back (verification failed).
    pub stored_id: Option<String>,
}

// ── Pure primitives ───────────────────────────────────────────

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF   // CJK Unified Ideographs
        | 0x3400..=0x4DBF // Ext A
        | 0x3040..=0x30FF // Hiragana + Katakana
    )
}

/// Minimal stopword set (English function words + common Chinese particles).
/// Kept small on purpose: over-filtering hides real themes.
fn is_stopword(tok: &str) -> bool {
    matches!(
        tok,
        "the"
            | "and"
            | "for"
            | "with"
            | "that"
            | "this"
            | "you"
            | "are"
            | "was"
            | "have"
            | "has"
            | "not"
            | "but"
            | "our"
            | "your"
            | "their"
            | "from"
            | "will"
            | "can"
            | "a"
            | "an"
            | "of"
            | "to"
            | "in"
            | "is"
            | "it"
            | "on"
            | "as"
            | "at"
            | "be"
            | "or"
            | "by"
            | "we"
            | "的"
            | "了"
            | "是"
            | "在"
            | "我"
            | "你"
            | "他"
            | "她"
            | "它"
            | "和"
            | "與"
            | "也"
            | "就"
            | "都"
            | "而"
            | "及"
            | "或"
            | "有"
    )
}

/// Tokenize content into normalized salient theme tokens (deduped per call is
/// the caller's job). ASCII words of length ≥ 3 are kept lowercased; runs of
/// CJK characters are emitted as overlapping bigrams (single Han chars are too
/// noisy to anchor a theme).
pub fn theme_tokens(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut ascii_word = String::new();
    let mut cjk_run: Vec<char> = Vec::new();

    let flush_ascii = |word: &mut String, out: &mut Vec<String>| {
        if word.chars().count() >= 3 {
            let lower = word.to_lowercase();
            if !is_stopword(&lower) {
                out.push(lower);
            }
        }
        word.clear();
    };
    let flush_cjk = |run: &mut Vec<char>, out: &mut Vec<String>| {
        if run.len() >= 2 {
            for pair in run.windows(2) {
                let bigram: String = pair.iter().collect();
                if !is_stopword(&bigram) {
                    out.push(bigram);
                }
            }
        }
        run.clear();
    };

    for c in content.chars() {
        if is_cjk(c) {
            flush_ascii(&mut ascii_word, &mut out);
            cjk_run.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk_run, &mut out);
            ascii_word.push(c);
        } else {
            flush_ascii(&mut ascii_word, &mut out);
            flush_cjk(&mut cjk_run, &mut out);
        }
    }
    flush_ascii(&mut ascii_word, &mut out);
    flush_cjk(&mut cjk_run, &mut out);
    out
}

/// The deduped salient token set of one document.
fn token_set(content: &str) -> HashSet<String> {
    theme_tokens(content).into_iter().collect()
}

/// Detect recurring themes across `entries`. A theme is a salient token present
/// in `>= min_support` distinct entries. Returned sorted by support (desc) then
/// key (asc) for stable output; at most `max_themes` are returned.
pub fn detect_themes(entries: &[MemoryEntry], min_support: u32, max_themes: usize) -> Vec<Theme> {
    // token -> (set of source ids, first-seen snippet per id)
    let mut buckets: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for e in entries {
        let snippet: String = e.content.chars().take(120).collect();
        for tok in token_set(&e.content) {
            let bucket = buckets.entry(tok).or_default();
            if !bucket.iter().any(|(id, _)| id == &e.id) {
                bucket.push((e.id.clone(), snippet.clone()));
            }
        }
    }

    let mut themes: Vec<Theme> = buckets
        .into_iter()
        .filter(|(_, srcs)| srcs.len() as u32 >= min_support)
        .map(|(key, mut srcs)| {
            srcs.sort_by(|a, b| a.0.cmp(&b.0));
            let support = srcs.len() as u32;
            let source_ids: Vec<String> = srcs.iter().map(|(id, _)| id.clone()).collect();
            let snippets: Vec<String> = srcs.iter().take(3).map(|(_, s)| s.clone()).collect();
            Theme {
                key,
                support,
                source_ids,
                snippets,
            }
        })
        .collect();

    themes.sort_by(|a, b| b.support.cmp(&a.support).then_with(|| a.key.cmp(&b.key)));
    themes.truncate(max_themes);
    themes
}

/// Deterministically synthesize a schema entry body from a theme (N3 output).
pub fn synthesize_schema(theme: &Theme) -> String {
    let bullets = theme
        .snippets
        .iter()
        .map(|s| format!("- {}", s.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Recurring theme \"{}\" observed across {} episodic memories. \
         Representative observations:\n{}",
        theme.key, theme.support, bullets
    )
}

/// The RecMem recurrence gate: consolidation only fires when a theme recurs at
/// least `threshold` times.
pub fn recurrence_gate(support: u32, threshold: u32) -> bool {
    support >= threshold
}

/// Deterministically merge recurrent source contents into one consolidated
/// statement (N4). Distinct source lessons are deduped and bulleted; no new
/// facts are introduced (faithfulness is verifiable downstream).
pub fn consolidate_sources(key: &str, contents: &[String]) -> String {
    let mut seen: Vec<String> = Vec::new();
    for c in contents {
        let trimmed = c.trim();
        if !trimmed.is_empty() && !seen.iter().any(|s| s == trimmed) {
            seen.push(trimmed.to_string());
        }
    }
    let bullets = seen
        .iter()
        .take(6)
        .map(|s| format!("- {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Consolidated knowledge about \"{}\" ({} recurring observations):\n{}",
        key,
        contents.len(),
        bullets
    )
}

/// Scaffold vocabulary the consolidation template itself introduces — these
/// tokens are allowed to appear without a source and never count as
/// hallucination.
fn scaffold_tokens() -> HashSet<String> {
    [
        "consolidated",
        "knowledge",
        "about",
        "recurring",
        "observations",
        "observation",
        "representative",
        "theme",
        "across",
        "episodic",
        "memories",
        "apply",
        "care",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Deterministic three-axis verification (TRUSTMEM). All inputs are salient
/// token sets so the checks are stable across whitespace/punctuation churn.
pub fn verify_consolidation(sources: &[&str], consolidated: &str) -> VerificationReport {
    let scaffold = scaffold_tokens();
    let consolidated_tokens = token_set(consolidated);

    // Per-source token sets.
    let source_sets: Vec<HashSet<String>> = sources.iter().map(|s| token_set(s)).collect();
    let union: HashSet<String> = source_sets.iter().flatten().cloned().collect();

    // Coverage: fraction of sources sharing ≥1 token with the consolidation.
    let covered = source_sets
        .iter()
        .filter(|s| !s.is_empty() && s.intersection(&consolidated_tokens).next().is_some())
        .count();
    let considered = source_sets.iter().filter(|s| !s.is_empty()).count();
    let coverage = if considered == 0 {
        1.0
    } else {
        covered as f64 / considered as f64
    };

    // Preservation: fraction of the source token union retained.
    let retained = union.intersection(&consolidated_tokens).count();
    let preservation = if union.is_empty() {
        1.0
    } else {
        retained as f64 / union.len() as f64
    };

    // Faithfulness: consolidated tokens that are in no source and not scaffold.
    let mut novel = 0usize;
    let mut checked = 0usize;
    for t in &consolidated_tokens {
        if scaffold.contains(t) {
            continue;
        }
        checked += 1;
        if !union.contains(t) {
            novel += 1;
        }
    }
    let faithfulness = if checked == 0 {
        1.0
    } else {
        1.0 - (novel as f64 / checked as f64)
    };

    let passed = coverage >= COVERAGE_MIN
        && preservation >= PRESERVATION_MIN
        && faithfulness >= FAITHFULNESS_MIN;

    VerificationReport {
        coverage,
        preservation,
        faithfulness,
        passed,
    }
}

// ── Engine-touching entry points ──────────────────────────────

fn episodic(entries: Vec<MemoryEntry>) -> Vec<MemoryEntry> {
    entries
        .into_iter()
        .filter(|e| e.layer == MemoryLayer::Episodic)
        .collect()
}

/// N3: induce schemas from an agent's recent episodic memory and persist each as
/// a `night-schema` semantic entry (superseding a prior schema for the same
/// theme key via the temporal chain). Returns the schemas stored this pass.
///
/// `context_window` bounds how many recent memories are scanned; `min_support`
/// is the DCPM promotion threshold. Deterministic — no LLM.
pub async fn induce_schema(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    context_window: usize,
    min_support: u32,
    max_schemas: usize,
) -> Result<Vec<InducedSchema>> {
    let recent = engine.list_recent(agent_id, context_window).await?;
    let eps = episodic(recent);
    let themes = detect_themes(&eps, min_support, max_schemas);

    let mut stored = Vec::new();
    for theme in themes {
        let body = synthesize_schema(&theme);
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            content: body,
            timestamp: chrono::Utc::now(),
            tags: vec!["night-schema".to_string(), format!("theme:{}", theme.key)],
            embedding: None,
            layer: MemoryLayer::Semantic,
            // Capped: episodic recurrence must not outrank curated entries.
            importance: NIGHT_PROMOTION_MAX_IMPORTANCE,
            access_count: 0,
            last_accessed: None,
            source_event: "night_schema_induction".to_string(),
        };
        // Supersession keyed by theme so a re-run refreshes rather than duplicates.
        let meta = TemporalMeta {
            subject: Some(format!("schema:{}", theme.key)),
            predicate: Some("night_induced".to_string()),
            object: None,
            confidence: Some(0.8),
            // WP1: night induction is the agent's own self-consolidation.
            origin: Some(crate::origin::AGENT_DERIVED.name.to_string()),
            metadata: Some(serde_json::json!({
                "support": theme.support,
                "source_ids": theme.source_ids,
                // Provenance: promoted from (channel-derived) episodic content.
                "source": "episodic-recurrence",
            })),
            ..Default::default()
        };
        let id = engine.store_temporal(agent_id, entry, meta).await?;
        stored.push(InducedSchema {
            memory_id: id,
            key: theme.key,
            support: theme.support,
        });
    }
    Ok(stored)
}

/// N4: for each recurrent theme (support ≥ `recurrence_threshold`), deterministically
/// consolidate its source memories, run the three-axis trust gate, and store the
/// result as a `night-consolidated` semantic entry **only if it passes** (rollback
/// otherwise). Returns one [`ConsolidationResult`] per theme attempted.
///
/// Deterministic — no LLM. The recurrence gate keeps this token-frugal (RecMem);
/// the verification gate keeps the store from degrading (TRUSTMEM).
pub async fn consolidate_recurrent(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    context_window: usize,
    recurrence_threshold: u32,
    max_themes: usize,
) -> Result<Vec<ConsolidationResult>> {
    let recent = engine.list_recent(agent_id, context_window).await?;
    let eps = episodic(recent);
    // Detect themes at the recurrence threshold (the gate).
    let themes = detect_themes(&eps, recurrence_threshold, max_themes);

    // Index source content by id for cheap lookup.
    let mut by_id: BTreeMap<&str, &str> = BTreeMap::new();
    for e in &eps {
        by_id.insert(e.id.as_str(), e.content.as_str());
    }

    let mut results = Vec::new();
    for theme in themes {
        if !recurrence_gate(theme.support, recurrence_threshold) {
            continue; // defensive; detect_themes already enforced this
        }
        let contents: Vec<String> = theme
            .source_ids
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).map(|s| s.to_string()))
            .collect();
        let source_refs: Vec<&str> = contents.iter().map(|s| s.as_str()).collect();
        let consolidated = consolidate_sources(&theme.key, &contents);
        let report = verify_consolidation(&source_refs, &consolidated);

        let stored_id = if report.passed {
            let entry = MemoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                agent_id: agent_id.to_string(),
                content: consolidated,
                timestamp: chrono::Utc::now(),
                tags: vec![
                    "night-consolidated".to_string(),
                    format!("theme:{}", theme.key),
                ],
                embedding: None,
                layer: MemoryLayer::Semantic,
                // Capped: episodic recurrence must not outrank curated entries.
                importance: NIGHT_PROMOTION_MAX_IMPORTANCE,
                access_count: 0,
                last_accessed: None,
                source_event: "night_consolidation".to_string(),
            };
            let meta = TemporalMeta {
                subject: Some(format!("consolidated:{}", theme.key)),
                predicate: Some("night_consolidated".to_string()),
                object: None,
                confidence: Some(0.85),
                // WP1: night consolidation is the agent's own self-consolidation.
                origin: Some(crate::origin::AGENT_DERIVED.name.to_string()),
                metadata: Some(serde_json::json!({
                    "support": theme.support,
                    "source_ids": theme.source_ids,
                    "coverage": report.coverage,
                    "preservation": report.preservation,
                    "faithfulness": report.faithfulness,
                    // Provenance: promoted from (channel-derived) episodic content.
                    "source": "episodic-recurrence",
                })),
                ..Default::default()
            };
            Some(engine.store_temporal(agent_id, entry, meta).await?)
        } else {
            tracing::debug!(
                agent = agent_id,
                key = %theme.key,
                coverage = report.coverage,
                preservation = report.preservation,
                faithfulness = report.faithfulness,
                "night consolidation rolled back: verification failed"
            );
            None
        };

        results.push(ConsolidationResult {
            key: theme.key,
            support: theme.support,
            report,
            stored_id,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::traits::MemoryEngine;

    fn ep(agent: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Episodic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: String::new(),
        }
    }

    // ── tokenizer ──
    #[test]
    fn tokenizer_keeps_ascii_words_and_cjk_bigrams() {
        let toks = theme_tokens("Deploy the gateway 部署閘道");
        assert!(toks.contains(&"deploy".to_string()));
        assert!(toks.contains(&"gateway".to_string()));
        // "the" is a stopword, "部署" and "署閘" bigrams present.
        assert!(!toks.contains(&"the".to_string()));
        assert!(toks.contains(&"部署".to_string()));
    }

    #[test]
    fn tokenizer_drops_short_and_stopwords() {
        let toks = theme_tokens("we go to it");
        assert!(toks.is_empty(), "all tokens short or stopwords: {toks:?}");
    }

    // ── theme detection ──
    #[test]
    fn detect_themes_requires_min_support() {
        let entries = vec![
            ep("a", "deploy the gateway to production"),
            ep("a", "the gateway deploy failed again"),
            ep("a", "checking gateway logs after deploy"),
            ep("a", "unrelated note about lunch"),
        ];
        let themes = detect_themes(&entries, 3, 10);
        let keys: Vec<&str> = themes.iter().map(|t| t.key.as_str()).collect();
        assert!(keys.contains(&"gateway"), "gateway recurs 3x: {keys:?}");
        assert!(keys.contains(&"deploy"), "deploy recurs 3x: {keys:?}");
        assert!(!keys.contains(&"lunch"), "lunch appears once");
    }

    #[test]
    fn detect_themes_sorted_and_capped() {
        let entries = vec![
            ep("a", "alpha alpha context"),
            ep("a", "alpha beta context"),
            ep("a", "alpha beta context"),
        ];
        // alpha: 3 docs, beta: 2, context: 3
        let themes = detect_themes(&entries, 2, 2);
        assert_eq!(themes.len(), 2, "capped at 2");
        // Highest support first.
        assert!(themes[0].support >= themes[1].support);
    }

    #[test]
    fn recurrence_gate_boundary() {
        assert!(!recurrence_gate(2, 3));
        assert!(recurrence_gate(3, 3));
        assert!(recurrence_gate(4, 3));
    }

    // ── verification ──
    #[test]
    fn verify_passes_for_faithful_merge() {
        let sources = [
            "gateway deploy needs the api token configured",
            "gateway deploy failed without api token",
        ];
        let consolidated = consolidate_sources("gateway", &sources.map(String::from));
        let report = verify_consolidation(&sources, &consolidated);
        assert!(report.passed, "faithful merge should pass: {report:?}");
        assert!(report.faithfulness >= FAITHFULNESS_MIN);
    }

    #[test]
    fn verify_flags_hallucination() {
        let sources = ["gateway deploy needs token"];
        // Consolidated invents an unrelated claim with many novel tokens.
        let consolidated =
            "Consolidated: kubernetes helm chart production rollback nginx ingress certificate";
        let report = verify_consolidation(&sources, consolidated);
        assert!(!report.passed, "hallucinated merge must fail: {report:?}");
        assert!(report.faithfulness < FAITHFULNESS_MIN);
    }

    #[test]
    fn verify_flags_missing_coverage() {
        let sources = [
            "alpha token deploy",
            "beta gateway config",
            "gamma memory engine",
        ];
        // Consolidation only mentions the first source's tokens.
        let consolidated = "Consolidated knowledge alpha token deploy";
        let report = verify_consolidation(&sources, consolidated);
        assert!(
            report.coverage < COVERAGE_MIN,
            "only 1/3 covered: {report:?}"
        );
        assert!(!report.passed);
    }

    // ── engine integration ──
    #[tokio::test]
    async fn induce_schema_stores_night_schema_semantic() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for c in [
            "deploy the gateway to production successfully",
            "gateway deploy pipeline finished",
            "gateway deploy verified in staging",
        ] {
            engine.store("agent-x", ep("agent-x", c)).await.unwrap();
        }
        let stored = induce_schema(&engine, "agent-x", 100, 3, 10).await.unwrap();
        assert!(!stored.is_empty(), "should induce at least one schema");
        // Stored as searchable semantic memory.
        let hits = engine
            .search("agent-x", "Recurring theme", 10)
            .await
            .unwrap();
        assert!(hits.iter().any(
            |m| m.layer == MemoryLayer::Semantic && m.source_event == "night_schema_induction"
        ));
    }

    #[tokio::test]
    async fn induce_schema_supersedes_on_rerun() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for c in [
            "gateway deploy alpha",
            "gateway deploy beta",
            "gateway deploy gamma",
        ] {
            engine.store("agent-y", ep("agent-y", c)).await.unwrap();
        }
        let first = induce_schema(&engine, "agent-y", 100, 3, 10).await.unwrap();
        let second = induce_schema(&engine, "agent-y", 100, 3, 10).await.unwrap();
        assert!(!first.is_empty() && !second.is_empty());
        // Only currently-valid schema rows are returned by search — supersession
        // means the "gateway" schema count stays at 1 valid row, not 2.
        let hits = engine
            .search_layer(
                "agent-y",
                "Recurring theme gateway",
                &MemoryLayer::Semantic,
                20,
            )
            .await
            .unwrap();
        let gateway_rows = hits
            .iter()
            .filter(|m| m.tags.iter().any(|t| t == "theme:gateway"))
            .count();
        assert_eq!(
            gateway_rows, 1,
            "supersession keeps one valid schema per theme"
        );
    }

    #[tokio::test]
    async fn consolidate_recurrent_stores_when_verified() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        for c in [
            "gateway deploy needs api token configured",
            "gateway deploy needs api token in env",
            "gateway deploy needs api token before start",
        ] {
            engine.store("agent-z", ep("agent-z", c)).await.unwrap();
        }
        let results = consolidate_recurrent(&engine, "agent-z", 100, 3, 10)
            .await
            .unwrap();
        assert!(!results.is_empty());
        let stored = results.iter().find(|r| r.stored_id.is_some());
        assert!(
            stored.is_some(),
            "verified consolidation should store: {results:?}"
        );
        let hits = engine
            .search("agent-z", "Consolidated knowledge", 10)
            .await
            .unwrap();
        assert!(hits.iter().any(|m| m.source_event == "night_consolidation"));
    }

    #[tokio::test]
    async fn consolidate_recurrent_rolls_back_when_verification_fails() {
        // End-to-end rollback: a theme whose later sources are lexically
        // divergent balloons the source-token union, so the first-6-only summary
        // retains < PRESERVATION_MIN of it → verification fails → NOTHING is
        // stored (the "never write on failed verify" contract, TRUSTMEM).
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-rollback";
        // 14 sources share only the "telemetry" key; every other token is unique.
        // The summary is built from just 6 of them (`consolidate_sources` cap),
        // so it can retain at most ~31/71 of the token union — below
        // PRESERVATION_MIN (0.5) no matter which 6 the id-order happens to pick.
        for i in 0..14 {
            let c = format!("telemetry alpha{i} bravo{i} charlie{i} delta{i} echo{i}");
            engine.store(agent, ep(agent, &c)).await.unwrap();
        }
        let results = consolidate_recurrent(&engine, agent, 100, 3, 10)
            .await
            .unwrap();
        let tele = results
            .iter()
            .find(|r| r.key == "telemetry")
            .expect("telemetry theme should be detected");
        assert!(
            tele.stored_id.is_none(),
            "divergent-source theme must roll back, not store: {tele:?}"
        );
        // DB-level proof: no night-consolidated entry carries the telemetry theme tag.
        let hits = engine.search(agent, "telemetry", 20).await.unwrap();
        assert!(
            !hits
                .iter()
                .any(|m| m.source_event == "night_consolidation"
                    && m.tags.iter().any(|t| t == "theme:telemetry")),
            "rolled-back consolidation must leave no telemetry entry in the store"
        );
    }

    /// MED-C: night-promoted entries (schemas + consolidations) are capped at
    /// the neutral importance so repeated (channel-derived) episodic content
    /// can never launder itself into top-ranked durable memory. Provenance is
    /// carried by the night tags.
    #[tokio::test]
    async fn night_promotions_are_importance_capped_and_tagged() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-cap";
        for c in [
            "gateway deploy needs api token configured",
            "gateway deploy needs api token in env",
            "gateway deploy needs api token before start",
        ] {
            engine.store(agent, ep(agent, c)).await.unwrap();
        }
        induce_schema(&engine, agent, 100, 3, 10).await.unwrap();
        consolidate_recurrent(&engine, agent, 100, 3, 10).await.unwrap();

        let hits = engine.search(agent, "gateway", 50).await.unwrap();
        let promoted: Vec<_> = hits
            .iter()
            .filter(|m| {
                m.source_event == "night_schema_induction"
                    || m.source_event == "night_consolidation"
            })
            .collect();
        assert!(!promoted.is_empty(), "night passes must have stored entries");
        for m in promoted {
            assert!(
                m.importance <= NIGHT_PROMOTION_MAX_IMPORTANCE,
                "night-promoted entry must be importance-capped, got {} on {:?}",
                m.importance,
                m.tags
            );
            assert!(
                m.tags
                    .iter()
                    .any(|t| t == "night-schema" || t == "night-consolidated"),
                "night-promoted entry must carry a provenance tag: {:?}",
                m.tags
            );
        }
    }

    #[tokio::test]
    async fn consolidate_recurrent_skips_below_threshold() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        // Only 2 occurrences of the theme — below default threshold 3.
        for c in [
            "gateway deploy one",
            "gateway deploy two",
            "unrelated apple pie",
        ] {
            engine.store("agent-w", ep("agent-w", c)).await.unwrap();
        }
        let results = consolidate_recurrent(&engine, "agent-w", 100, 3, 10)
            .await
            .unwrap();
        assert!(
            results.iter().all(|r| r.key != "gateway"),
            "2 < 3 must not consolidate gateway: {results:?}"
        );
    }
}
