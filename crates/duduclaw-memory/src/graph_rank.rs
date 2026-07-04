//! HippoRAG-lite: Personalized PageRank over the SPO triple graph.
//!
//! Implements the retrieval side of HippoRAG (arXiv:2405.14831) with zero LLM
//! cost: the knowledge graph is the existing `subject` / `predicate` / `object`
//! columns written by `store_temporal` (F1, v1.19.0) — no schema change, no new
//! dependencies. Nodes are normalized entity strings plus memory ids; edges are
//! undirected `subject ↔ memory ↔ object`. Entities mentioned in the query seed
//! a Personalized PageRank walk (damping d = 0.5, HippoRAG's value) whose
//! stationary mass over memory nodes re-ranks and augments FTS results in
//! `SqliteMemoryEngine::search`.
//!
//! Fail-safe by construction: zero triples for the agent, or a query matching
//! no graph entity, returns `None` and the caller skips graph ranking entirely,
//! leaving FTS-only behavior byte-identical to before.

use std::collections::HashMap;

use rusqlite::{params, Connection};

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::word_contains_ci;

/// HippoRAG damping factor for the personalized walk.
const DAMPING: f64 = 0.5;
/// Maximum PPR power iterations.
const MAX_ITERATIONS: usize = 20;
/// L1 convergence threshold for early termination.
const L1_EPSILON: f64 = 1e-6;
/// How many top-PPR memories are considered for graph-only augmentation.
pub(crate) const TOP_GRAPH_CANDIDATES: usize = 5;
/// How many graph-only memories may be appended to an FTS candidate set.
pub(crate) const MAX_GRAPH_APPENDS: usize = 3;

/// One currently-valid triple row: `(memory_id, subject, object)`.
pub(crate) type TripleRow = (String, String, Option<String>);

/// Undirected bipartite-ish graph over interned node indices:
/// entity nodes (normalized strings) and memory nodes (row ids).
pub struct TripleGraph {
    /// Adjacency lists; parallel edges are allowed and act as edge weights.
    adjacency: Vec<Vec<usize>>,
    /// Normalized entity string → node index.
    entities: HashMap<String, usize>,
    /// node index → memory id (`None` for entity nodes).
    memory_of: Vec<Option<String>>,
}

/// Normalize an entity string for node identity: trim + Unicode lowercase.
fn normalize_entity(raw: &str) -> String {
    raw.trim().to_lowercase()
}

impl TripleGraph {
    /// Build the graph from `(memory_id, subject, object)` rows.
    ///
    /// Rows with an empty normalized subject are skipped; a missing/empty
    /// object yields a degree-1 memory node (subject edge only).
    pub fn from_triples(rows: &[TripleRow]) -> Self {
        let mut graph = Self {
            adjacency: Vec::new(),
            entities: HashMap::new(),
            memory_of: Vec::new(),
        };
        let mut memory_index: HashMap<String, usize> = HashMap::new();

        for (memory_id, subject, object) in rows {
            let subject = normalize_entity(subject);
            if subject.is_empty() {
                continue;
            }
            let mem = graph.intern_memory(&mut memory_index, memory_id);
            let subj = graph.intern_entity(subject);
            graph.add_edge(mem, subj);

            if let Some(object) = object {
                let object = normalize_entity(object);
                if !object.is_empty() {
                    let obj = graph.intern_entity(object);
                    graph.add_edge(mem, obj);
                }
            }
        }
        graph
    }

    /// True when the graph has no nodes (no usable triples).
    pub fn is_empty(&self) -> bool {
        self.adjacency.is_empty()
    }

    /// Entity nodes whose name appears in `query` (case-insensitive
    /// whole-word; CJK-safe because CJK bytes never form ASCII word
    /// boundaries). Sorted for determinism; uniform seed mass is applied
    /// later in [`personalized_pagerank`](Self::personalized_pagerank).
    pub fn seed_nodes(&self, query: &str) -> Vec<usize> {
        let mut seeds: Vec<usize> = self
            .entities
            .iter()
            .filter(|(name, _)| word_contains_ci(query, name))
            .map(|(_, idx)| *idx)
            .collect();
        seeds.sort_unstable();
        seeds
    }

    /// Personalized PageRank: `p ← (1−d)·s + d·Aᵀp` with row-normalized
    /// adjacency, damping `d = 0.5` (HippoRAG), max 20 iterations or until
    /// the L1 delta drops below 1e-6. `s` is uniform over `seeds`.
    pub fn personalized_pagerank(&self, seeds: &[usize]) -> Vec<f64> {
        let n = self.adjacency.len();
        if n == 0 || seeds.is_empty() {
            return vec![0.0; n];
        }
        let seed_mass = 1.0 / seeds.len() as f64;
        let mut restart = vec![0.0; n];
        for &s in seeds {
            if let Some(slot) = restart.get_mut(s) {
                *slot += seed_mass;
            }
        }

        let mut p = restart.clone();
        for _ in 0..MAX_ITERATIONS {
            let mut next: Vec<f64> = restart.iter().map(|r| (1.0 - DAMPING) * r).collect();
            for (i, neighbors) in self.adjacency.iter().enumerate() {
                if neighbors.is_empty() || p[i] == 0.0 {
                    continue;
                }
                let share = DAMPING * p[i] / neighbors.len() as f64;
                for &j in neighbors {
                    next[j] += share;
                }
            }
            let delta: f64 = p.iter().zip(&next).map(|(a, b)| (a - b).abs()).sum();
            p = next;
            if delta < L1_EPSILON {
                break;
            }
        }
        p
    }

    /// Extract memory-node scores from a PPR mass vector, sorted descending
    /// and normalized so the top memory scores 1.0. Ties break by id for
    /// determinism; zero-mass memories are omitted.
    pub fn ranked_memories(&self, mass: &[f64]) -> Vec<(String, f64)> {
        let mut out: Vec<(String, f64)> = self
            .memory_of
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let score = mass.get(i).copied().unwrap_or(0.0);
                m.as_ref()
                    .filter(|_| score > 0.0)
                    .map(|id| (id.clone(), score))
            })
            .collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        if let Some(&(_, max)) = out.first() {
            if max > 0.0 {
                return out.into_iter().map(|(id, s)| (id, s / max)).collect();
            }
        }
        out
    }

    fn intern_entity(&mut self, name: String) -> usize {
        if let Some(&idx) = self.entities.get(&name) {
            return idx;
        }
        let idx = self.new_node(None);
        self.entities.insert(name, idx);
        idx
    }

    fn intern_memory(&mut self, index: &mut HashMap<String, usize>, id: &str) -> usize {
        if let Some(&idx) = index.get(id) {
            return idx;
        }
        let idx = self.new_node(Some(id.to_string()));
        index.insert(id.to_string(), idx);
        idx
    }

    fn new_node(&mut self, memory_id: Option<String>) -> usize {
        let idx = self.adjacency.len();
        self.adjacency.push(Vec::new());
        self.memory_of.push(memory_id);
        idx
    }

    fn add_edge(&mut self, a: usize, b: usize) {
        self.adjacency[a].push(b);
        self.adjacency[b].push(a);
    }
}

/// Cheap gate: count of currently-valid triples for the agent (subject set,
/// not expired/superseded at `now_rfc`).
pub(crate) fn count_agent_triples(
    conn: &Connection,
    agent_id: &str,
    now_rfc: &str,
) -> Result<u64> {
    conn.query_row(
        "SELECT COUNT(*) FROM memories
         WHERE agent_id = ?1 AND subject IS NOT NULL
           AND (valid_until IS NULL OR valid_until > ?2)",
        params![agent_id, now_rfc],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as u64)
    .map_err(|e| DuDuClawError::Memory(format!("graph triple count: {e}")))
}

/// Load the agent's currently-valid triples (temporal validity filter +
/// agent isolation applied in SQL, matching `search()` semantics).
pub(crate) fn load_agent_triples(
    conn: &Connection,
    agent_id: &str,
    now_rfc: &str,
) -> Result<Vec<TripleRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, subject, object FROM memories
             WHERE agent_id = ?1 AND subject IS NOT NULL
               AND (valid_until IS NULL OR valid_until > ?2)",
        )
        .map_err(|e| DuDuClawError::Memory(format!("graph triple load: {e}")))?;
    let rows = stmt
        .query_map(params![agent_id, now_rfc], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|e| DuDuClawError::Memory(format!("graph triple query: {e}")))?;
    let mut triples = Vec::new();
    for row in rows {
        triples.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
    }
    Ok(triples)
}

/// Compute normalized PPR scores over the agent's memory nodes for `query`.
///
/// Returns `Ok(None)` — meaning "skip graph ranking, FTS-only" — when the
/// agent has zero valid triples (COUNT gate, no graph build) or when no
/// entity in the graph appears in the query (no seeds).
pub(crate) fn graph_rank_scores(
    conn: &Connection,
    agent_id: &str,
    query: &str,
    now_rfc: &str,
) -> Result<Option<Vec<(String, f64)>>> {
    if count_agent_triples(conn, agent_id, now_rfc)? == 0 {
        return Ok(None);
    }
    let triples = load_agent_triples(conn, agent_id, now_rfc)?;
    let graph = TripleGraph::from_triples(&triples);
    if graph.is_empty() {
        return Ok(None);
    }
    let seeds = graph.seed_nodes(query);
    if seeds.is_empty() {
        return Ok(None);
    }
    let mass = graph.personalized_pagerank(&seeds);
    Ok(Some(graph.ranked_memories(&mass)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triple(mem: &str, s: &str, o: &str) -> TripleRow {
        (mem.to_string(), s.to_string(), Some(o.to_string()))
    }

    /// alice —[m1]— bob —[m2]— project-x: seeding on "alice" must give both
    /// memories positive mass, with the one-hop memory ranked above two-hop.
    #[test]
    fn two_hop_mass_flows_through_shared_entity() {
        let graph = TripleGraph::from_triples(&[
            triple("m1", "alice", "bob"),
            triple("m2", "bob", "project-x"),
        ]);
        let seeds = graph.seed_nodes("what is alice working on");
        assert_eq!(seeds.len(), 1, "only the alice entity should seed");

        let mass = graph.personalized_pagerank(&seeds);
        let ranked = graph.ranked_memories(&mass);
        assert_eq!(ranked.len(), 2, "both memories reachable from alice");
        assert_eq!(ranked[0].0, "m1", "direct memory ranks first");
        assert_eq!(ranked[1].0, "m2", "two-hop memory still receives mass");
        assert!((ranked[0].1 - 1.0).abs() < 1e-12, "top score normalized to 1.0");
        assert!(ranked[1].1 > 0.0 && ranked[1].1 < 1.0);
    }

    /// Whole-word seeding: entity "hi" must not seed from "this"; entities
    /// match case-insensitively.
    #[test]
    fn seeding_is_whole_word_and_case_insensitive() {
        let graph = TripleGraph::from_triples(&[triple("m1", "Hi", "Bob")]);
        assert!(graph.seed_nodes("this is unrelated").is_empty());
        assert_eq!(graph.seed_nodes("say HI to everyone").len(), 1);
    }

    /// CJK entities seed via byte-boundary safety (CJK bytes are never ASCII
    /// alphanumerics, so word boundaries always hold).
    #[test]
    fn seeding_matches_cjk_entities() {
        let graph = TripleGraph::from_triples(&[triple("m1", "小明", "茶")]);
        let seeds = graph.seed_nodes("小明喜歡什麼");
        assert_eq!(seeds.len(), 1, "CJK entity must seed from CJK query");
    }

    /// Empty rows / empty subjects produce an empty graph and no seeds.
    #[test]
    fn empty_and_blank_subject_rows_are_skipped() {
        let graph = TripleGraph::from_triples(&[("m1".to_string(), "   ".to_string(), None)]);
        assert!(graph.is_empty());
        assert!(TripleGraph::from_triples(&[]).is_empty());
    }

    /// A triple without an object still contributes a subject↔memory edge.
    #[test]
    fn object_less_triple_still_reachable() {
        let graph =
            TripleGraph::from_triples(&[("m1".to_string(), "carol".to_string(), None)]);
        let seeds = graph.seed_nodes("carol");
        let ranked = graph.ranked_memories(&graph.personalized_pagerank(&seeds));
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].0, "m1");
    }

    /// Disconnected components receive zero mass (no leakage across the graph).
    #[test]
    fn disconnected_component_gets_no_mass() {
        let graph = TripleGraph::from_triples(&[
            triple("m1", "alice", "bob"),
            triple("m2", "carol", "tea"),
        ]);
        let seeds = graph.seed_nodes("alice");
        let ranked = graph.ranked_memories(&graph.personalized_pagerank(&seeds));
        assert_eq!(ranked.len(), 1, "carol's memory must receive zero mass");
        assert_eq!(ranked[0].0, "m1");
    }

    /// No seeds → all-zero mass → empty ranking (caller skips).
    #[test]
    fn no_seeds_yields_empty_ranking() {
        let graph = TripleGraph::from_triples(&[triple("m1", "alice", "bob")]);
        assert!(graph.seed_nodes("completely unrelated query").is_empty());
        let mass = graph.personalized_pagerank(&[]);
        assert!(graph.ranked_memories(&mass).is_empty());
    }
}
