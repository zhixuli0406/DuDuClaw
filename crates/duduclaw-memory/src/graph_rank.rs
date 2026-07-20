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

/// One currently-valid triple row carrying its origin trust (D2):
/// `(memory_id, subject, object, origin_trust)`. `origin_trust` scales the
/// edge weight so a low-trust fact contributes less PPR mass — directly
/// suppressing the "single poisoned triple amplified two hops by PPR" path.
pub(crate) type WeightedTripleRow = (String, String, Option<String>, f64);

/// A currently-valid triple carrying everything the graph needs (D3.3): the
/// memory id, subject, optional predicate (edge label — attached, never
/// affecting PPR) and object, plus the origin trust that scales edge weight.
/// This is the row shape the cached graph is built from.
#[derive(Debug, Clone)]
pub(crate) struct GraphTriple {
    pub memory_id: String,
    pub subject: String,
    pub predicate: Option<String>,
    pub object: Option<String>,
    pub trust: f64,
}

/// A recorded graph edge with its predicate label (D3.3). Attached to the graph
/// for export / future predicate-aware retrieval; the PPR math never reads it,
/// so ranking stays byte-identical whether or not predicates are present.
#[derive(Debug, Clone)]
pub struct EdgeLabel {
    pub memory_id: String,
    /// Canonical (alias-resolved) subject entity string.
    pub subject: String,
    pub predicate: Option<String>,
    /// Canonical (alias-resolved) object entity string, if any.
    pub object: Option<String>,
}

/// Undirected bipartite-ish graph over interned node indices:
/// entity nodes (normalized strings) and memory nodes (row ids).
pub struct TripleGraph {
    /// Weighted adjacency lists: `(neighbor_index, edge_weight)`. A plain
    /// (unit-trust) triple contributes weight 1.0 per edge, so summing unit
    /// weights reproduces the old parallel-edge multiplicity exactly — ranking
    /// is byte-identical when every `origin_trust` is 1.0.
    adjacency: Vec<Vec<(usize, f64)>>,
    /// Normalized entity string → node index.
    entities: HashMap<String, usize>,
    /// node index → memory id (`None` for entity nodes).
    memory_of: Vec<Option<String>>,
    /// D3.2 alias seeding: normalized alias string → the canonical entity's node
    /// index (only for aliases whose canonical entity is present in the graph).
    /// Empty in the alias-free path, so seeding is byte-identical to before.
    aliases: HashMap<String, usize>,
    /// D3.3 predicate-labelled edges (attached, PPR-neutral).
    edge_labels: Vec<EdgeLabel>,
}

/// Normalize an entity string for node identity: trim + Unicode lowercase.
fn normalize_entity(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Resolve a normalized entity name through the alias map to its canonical form
/// (D3.2). Both keys and values in `aliases` are pre-normalized by the caller;
/// an entity with no alias entry resolves to itself.
fn resolve_alias<'a>(aliases: &'a HashMap<String, String>, name: &'a str) -> &'a str {
    aliases.get(name).map(|c| c.as_str()).unwrap_or(name)
}

impl TripleGraph {
    /// Build the graph from `(memory_id, subject, object)` rows (unit trust).
    ///
    /// Rows with an empty normalized subject are skipped; a missing/empty
    /// object yields a degree-1 memory node (subject edge only). Every edge is
    /// weight 1.0 — the pre-D2 behaviour.
    pub fn from_triples(rows: &[TripleRow]) -> Self {
        let gt: Vec<GraphTriple> = rows
            .iter()
            .map(|(m, s, o)| GraphTriple {
                memory_id: m.clone(),
                subject: s.clone(),
                predicate: None,
                object: o.clone(),
                trust: 1.0,
            })
            .collect();
        Self::from_graph_triples(&gt, &HashMap::new())
    }

    /// Build the graph from `(memory_id, subject, object, origin_trust)` rows
    /// (D2). Each edge from a memory node is weighted by that memory's
    /// `origin_trust` (clamped to `[0,1]`, a missing/negative value treated as
    /// 1.0). With every trust == 1.0 the weighted PPR is byte-identical to the
    /// unit-weight path.
    pub fn from_triples_weighted(rows: &[WeightedTripleRow]) -> Self {
        let gt: Vec<GraphTriple> = rows
            .iter()
            .map(|(m, s, o, t)| GraphTriple {
                memory_id: m.clone(),
                subject: s.clone(),
                predicate: None,
                object: o.clone(),
                trust: *t,
            })
            .collect();
        Self::from_graph_triples(&gt, &HashMap::new())
    }

    /// Build the graph from [`GraphTriple`] rows with an optional alias map
    /// (D3.2/D3.3). Entity names are alias-resolved to their canonical form
    /// before interning (so "老闆"/"李老闆" collapse to one node), predicate
    /// labels are recorded on `edge_labels` (attached, PPR-neutral), and edges
    /// are trust-weighted exactly as [`from_triples_weighted`]. An empty alias
    /// map + `None` predicates reproduce the earlier build byte-for-byte.
    pub(crate) fn from_graph_triples(
        rows: &[GraphTriple],
        aliases: &HashMap<String, String>,
    ) -> Self {
        let mut graph = Self {
            adjacency: Vec::new(),
            entities: HashMap::new(),
            memory_of: Vec::new(),
            aliases: HashMap::new(),
            edge_labels: Vec::new(),
        };
        let mut memory_index: HashMap<String, usize> = HashMap::new();

        for gt in rows {
            let subject = normalize_entity(&gt.subject);
            if subject.is_empty() {
                continue;
            }
            let subject = resolve_alias(aliases, &subject).to_string();
            let w = if gt.trust.is_finite() {
                gt.trust.clamp(0.0, 1.0)
            } else {
                1.0
            };
            let mem = graph.intern_memory(&mut memory_index, &gt.memory_id);
            let subj = graph.intern_entity(subject.clone());
            graph.add_edge(mem, subj, w);

            let object_canon = match &gt.object {
                Some(object) => {
                    let object = normalize_entity(object);
                    if object.is_empty() {
                        None
                    } else {
                        let object = resolve_alias(aliases, &object).to_string();
                        let obj = graph.intern_entity(object.clone());
                        graph.add_edge(mem, obj, w);
                        Some(object)
                    }
                }
                None => None,
            };

            graph.edge_labels.push(EdgeLabel {
                memory_id: gt.memory_id.clone(),
                subject,
                predicate: gt.predicate.clone(),
                object: object_canon,
            });
        }

        // Build the alias → canonical-node seed index: only aliases whose
        // canonical entity actually made it into the graph are useful for
        // seeding. Self-aliases and unknown canonicals are dropped.
        for (alias, canonical) in aliases {
            if alias == canonical {
                continue;
            }
            if let Some(&idx) = graph.entities.get(canonical.as_str()) {
                graph.aliases.entry(alias.clone()).or_insert(idx);
            }
        }
        graph
    }

    /// Predicate-labelled edges recorded at build time (D3.3). Used by the
    /// export surface and predicate-aware tests; PPR never reads it.
    pub fn edges(&self) -> &[EdgeLabel] {
        &self.edge_labels
    }

    /// Iterator over canonical entity names present in the graph (D3.4 embedding
    /// seeding maps query-nearest entity names back to node indices).
    pub fn entity_names(&self) -> impl Iterator<Item = &str> {
        self.entities.keys().map(|s| s.as_str())
    }

    /// Node index for a canonical entity name, if present (D3.4).
    pub fn entity_node(&self, name: &str) -> Option<usize> {
        self.entities.get(name).copied()
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
        // D3.2: a query mentioning an alias ("老闆") seeds the canonical entity
        // node ("李老闆"). Empty alias map ⇒ this loop is a no-op and the result
        // is byte-identical to the entity-only path.
        for (alias, idx) in &self.aliases {
            if word_contains_ci(query, alias) {
                seeds.push(*idx);
            }
        }
        seeds.sort_unstable();
        seeds.dedup();
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
                // Row-normalize by total outgoing edge weight. With all weights
                // 1.0 this is `neighbors.len()` and each share is
                // `DAMPING·p[i]/len` — byte-identical to the pre-D2 path.
                let total_w: f64 = neighbors.iter().map(|(_, w)| *w).sum();
                if total_w <= 0.0 {
                    continue;
                }
                for &(j, w) in neighbors {
                    next[j] += DAMPING * p[i] * w / total_w;
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

    fn add_edge(&mut self, a: usize, b: usize, weight: f64) {
        self.adjacency[a].push((b, weight));
        self.adjacency[b].push((a, weight));
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
           AND (valid_until IS NULL OR valid_until > ?2)
           AND quarantined = 0",
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
) -> Result<Vec<WeightedTripleRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, subject, object, origin_trust FROM memories
             WHERE agent_id = ?1 AND subject IS NOT NULL
               AND (valid_until IS NULL OR valid_until > ?2)
               AND quarantined = 0",
        )
        .map_err(|e| DuDuClawError::Memory(format!("graph triple load: {e}")))?;
    let rows = stmt
        .query_map(params![agent_id, now_rfc], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                // origin_trust is NOT NULL DEFAULT 1.0, but COALESCE-guard for
                // any legacy NULL so a missing value degrades to full trust.
                r.get::<_, Option<f64>>(3)?.unwrap_or(1.0),
            ))
        })
        .map_err(|e| DuDuClawError::Memory(format!("graph triple query: {e}")))?;
    let mut triples = Vec::new();
    for row in rows {
        triples.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
    }
    Ok(triples)
}

/// Load the agent's currently-valid triples with predicate labels (D3.3), for
/// the cached graph. Same temporal + isolation + quarantine filters as
/// [`load_agent_triples`], plus the `predicate` column.
pub(crate) fn load_agent_graph_triples(
    conn: &Connection,
    agent_id: &str,
    now_rfc: &str,
) -> Result<Vec<GraphTriple>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, subject, predicate, object, origin_trust FROM memories
             WHERE agent_id = ?1 AND subject IS NOT NULL
               AND (valid_until IS NULL OR valid_until > ?2)
               AND quarantined = 0",
        )
        .map_err(|e| DuDuClawError::Memory(format!("graph triple load: {e}")))?;
    let rows = stmt
        .query_map(params![agent_id, now_rfc], |r| {
            Ok(GraphTriple {
                memory_id: r.get::<_, String>(0)?,
                subject: r.get::<_, String>(1)?,
                predicate: r.get::<_, Option<String>>(2)?,
                object: r.get::<_, Option<String>>(3)?,
                trust: r.get::<_, Option<f64>>(4)?.unwrap_or(1.0),
            })
        })
        .map_err(|e| DuDuClawError::Memory(format!("graph triple query: {e}")))?;
    let mut triples = Vec::new();
    for row in rows {
        triples.push(row.map_err(|e| DuDuClawError::Memory(e.to_string()))?);
    }
    Ok(triples)
}

/// Load the agent's entity-alias map (D3.2) as `normalized_alias →
/// normalized_canonical`. Both sides are re-normalized defensively so a graph
/// built from this map matches the entity node identities exactly. Absent /
/// empty ⇒ empty map ⇒ byte-identical (alias-free) graph.
pub(crate) fn load_alias_map(
    conn: &Connection,
    agent_id: &str,
) -> Result<HashMap<String, String>> {
    let mut stmt = conn
        .prepare(
            "SELECT alias, canonical FROM entity_alias WHERE agent_id = ?1",
        )
        .map_err(|e| DuDuClawError::Memory(format!("alias load: {e}")))?;
    let rows = stmt
        .query_map(params![agent_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| DuDuClawError::Memory(format!("alias query: {e}")))?;
    let mut map = HashMap::new();
    for row in rows {
        let (alias, canonical) = row.map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        let alias = normalize_entity(&alias);
        let canonical = normalize_entity(&canonical);
        if alias.is_empty() || canonical.is_empty() || alias == canonical {
            continue;
        }
        map.insert(alias, canonical);
    }
    Ok(map)
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
    let graph = TripleGraph::from_triples_weighted(&triples);
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

    // ── D2 trust-weighted edges ───────────────────────────────────────────

    fn wtriple(mem: &str, s: &str, o: &str, t: f64) -> WeightedTripleRow {
        (mem.to_string(), s.to_string(), Some(o.to_string()), t)
    }

    /// Byte-identical guarantee: an all-trust-1.0 weighted graph produces the
    /// exact same PPR mass vector as the unweighted (pre-D2) `from_triples`.
    #[test]
    fn unit_trust_is_byte_identical_to_unweighted() {
        let unweighted = TripleGraph::from_triples(&[
            triple("m1", "alice", "bob"),
            triple("m2", "bob", "project-x"),
            triple("m3", "alice", "carol"),
        ]);
        let weighted = TripleGraph::from_triples_weighted(&[
            wtriple("m1", "alice", "bob", 1.0),
            wtriple("m2", "bob", "project-x", 1.0),
            wtriple("m3", "alice", "carol", 1.0),
        ]);
        let seeds_u = unweighted.seed_nodes("what is alice working on");
        let seeds_w = weighted.seed_nodes("what is alice working on");
        let mass_u = unweighted.personalized_pagerank(&seeds_u);
        let mass_w = weighted.personalized_pagerank(&seeds_w);
        assert_eq!(mass_u.len(), mass_w.len());
        for (a, b) in mass_u.iter().zip(&mass_w) {
            // Bit-identical: multiplying by 1.0 and dividing by the summed unit
            // weights reproduces the exact same float ops.
            assert_eq!(a.to_bits(), b.to_bits(), "unit-trust mass must be byte-identical");
        }
    }

    /// A low-trust triple contributes less PPR mass than a full-trust one, so
    /// the poisoned memory ranks below a clean two-hop memory it would
    /// otherwise tie or beat.
    #[test]
    fn low_trust_edge_suppresses_ppr_mass() {
        // alice —[m1, trust 1.0]— bob ; alice —[m2, trust 0.05]— eve
        // Both m1 and m2 are one hop from the alice seed, but m2's low trust
        // shrinks its edge weight so it receives far less mass.
        let graph = TripleGraph::from_triples_weighted(&[
            wtriple("m1", "alice", "bob", 1.0),
            wtriple("m2", "alice", "eve", 0.05),
        ]);
        let seeds = graph.seed_nodes("alice");
        let ranked = graph.ranked_memories(&graph.personalized_pagerank(&seeds));
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, "m1", "full-trust memory ranks first");
        assert_eq!(ranked[1].0, "m2");
        assert!(
            ranked[1].1 < ranked[0].1,
            "low-trust memory must receive strictly less normalized mass"
        );
    }

    // ── D3.2 entity alias merging ─────────────────────────────────────────

    fn gtriple(mem: &str, s: &str, p: &str, o: &str, t: f64) -> GraphTriple {
        GraphTriple {
            memory_id: mem.to_string(),
            subject: s.to_string(),
            predicate: Some(p.to_string()),
            object: Some(o.to_string()),
            trust: t,
        }
    }

    /// Byte-identical guarantee: `from_graph_triples` with an empty alias map
    /// and no predicate labels produces the exact same PPR mass as the
    /// alias-free `from_triples_weighted` path.
    #[test]
    fn alias_free_build_is_byte_identical() {
        let weighted = TripleGraph::from_triples_weighted(&[
            wtriple("m1", "alice", "bob", 1.0),
            wtriple("m2", "bob", "project-x", 0.7),
        ]);
        let plain: Vec<GraphTriple> = vec![
            GraphTriple {
                memory_id: "m1".into(),
                subject: "alice".into(),
                predicate: None,
                object: Some("bob".into()),
                trust: 1.0,
            },
            GraphTriple {
                memory_id: "m2".into(),
                subject: "bob".into(),
                predicate: None,
                object: Some("project-x".into()),
                trust: 0.7,
            },
        ];
        let graphed = TripleGraph::from_graph_triples(&plain, &HashMap::new());
        let q = "what is alice doing";
        let mu = weighted.personalized_pagerank(&weighted.seed_nodes(q));
        let mg = graphed.personalized_pagerank(&graphed.seed_nodes(q));
        assert_eq!(mu.len(), mg.len());
        for (a, b) in mu.iter().zip(&mg) {
            assert_eq!(a.to_bits(), b.to_bits(), "alias-free build must be byte-identical");
        }
    }

    /// An alias in the query seeds the canonical entity's node, so the fact
    /// stored under the canonical name is retrieved. CJK case: "老闆" → "李老闆".
    #[test]
    fn alias_query_seeds_canonical_entity() {
        let rows = vec![gtriple("m1", "李老闆", "likes", "tea", 1.0)];
        let mut aliases = HashMap::new();
        aliases.insert("老闆".to_string(), "李老闆".to_string());
        let graph = TripleGraph::from_graph_triples(&rows, &aliases);

        // Without aliases the bare "老闆" query would miss the canonical node.
        let bare = TripleGraph::from_graph_triples(&rows, &HashMap::new());
        assert!(bare.seed_nodes("老闆喜歡喝什麼").is_empty());

        let seeds = graph.seed_nodes("老闆喜歡喝什麼");
        assert_eq!(seeds.len(), 1, "alias must seed the canonical entity node");
        let ranked = graph.ranked_memories(&graph.personalized_pagerank(&seeds));
        assert_eq!(ranked[0].0, "m1");
    }

    /// Aliases collapse two surface forms onto one node (higher seeding hit
    /// rate) — a triple on the alias and one on the canonical share a node.
    #[test]
    fn alias_and_canonical_collapse_to_one_node() {
        let rows = vec![
            gtriple("m1", "老闆", "owns", "shop", 1.0),
            gtriple("m2", "李老闆", "lives_in", "taipei", 1.0),
        ];
        let mut aliases = HashMap::new();
        aliases.insert("老闆".to_string(), "李老闆".to_string());
        let graph = TripleGraph::from_graph_triples(&rows, &aliases);
        // Query the canonical form → both memories reachable through the shared
        // canonical node.
        let seeds = graph.seed_nodes("李老闆");
        let ranked = graph.ranked_memories(&graph.personalized_pagerank(&seeds));
        let ids: Vec<&str> = ranked.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"m1") && ids.contains(&"m2"), "collapsed node reaches both facts");
    }

    // ── D3.3 predicate edge labels ────────────────────────────────────────

    /// Predicate labels are attached to edges without changing PPR mass.
    #[test]
    fn predicate_labels_attached_without_changing_ppr() {
        let labelled = vec![
            gtriple("m1", "alice", "works_on", "bob", 1.0),
            gtriple("m2", "bob", "part_of", "project-x", 1.0),
        ];
        let graph = TripleGraph::from_graph_triples(&labelled, &HashMap::new());
        // Same topology, no predicates.
        let plain = TripleGraph::from_triples(&[
            triple("m1", "alice", "bob"),
            triple("m2", "bob", "project-x"),
        ]);
        let q = "alice";
        let ml = graph.personalized_pagerank(&graph.seed_nodes(q));
        let mp = plain.personalized_pagerank(&plain.seed_nodes(q));
        for (a, b) in ml.iter().zip(&mp) {
            assert_eq!(a.to_bits(), b.to_bits(), "predicates must not affect PPR");
        }
        // Predicate labels are carried for export.
        let preds: Vec<&str> = graph
            .edges()
            .iter()
            .filter_map(|e| e.predicate.as_deref())
            .collect();
        assert!(preds.contains(&"works_on") && preds.contains(&"part_of"));
    }
}
