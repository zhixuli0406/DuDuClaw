//! Plan-body helpers split out of `body.rs`: entity embeddings orphaned by a
//! plan and the namespace's untracked-row count.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::*;

fn e(x: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(x.to_string())
}

pub(super) fn orphan_entities(
    conn: &Connection,
    agent_id: &str,
    memory_ids: &BTreeMap<String, String>,
    targets: &HashSet<&str>,
) -> Result<Vec<String>> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }
    let aliases = crate::graph_rank::load_alias_map(conn, agent_id)?;
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    let mut stmt = conn
        .prepare_cached("SELECT subject, object FROM memories WHERE id = ?1 AND agent_id = ?2")
        .map_err(e)?;
    for id in memory_ids.keys() {
        let so: Option<(Option<String>, Option<String>)> = stmt
            .query_row(params![id, agent_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(e)?;
        if let Some((s, o)) = so {
            for raw in [s, o].into_iter().flatten() {
                let name = crate::graph_rank::canonical_entity(&raw, &aliases);
                if !name.is_empty() {
                    candidates.insert(name);
                }
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let now = Utc::now().to_rfc3339();
    let surviving: Vec<crate::graph_rank::GraphTriple> =
        crate::graph_rank::load_agent_graph_triples(conn, agent_id, &now)?
            .into_iter()
            .filter(|t| !targets.contains(t.memory_id.as_str()))
            .collect();
    let graph = crate::graph_rank::TripleGraph::from_graph_triples(&surviving, &aliases);
    let alive: HashSet<&str> = graph.entity_names().collect();
    let mut has = conn
        .prepare_cached(
            "SELECT 1 FROM entity_embedding WHERE agent_id = ?1 AND entity = ?2 LIMIT 1",
        )
        .map_err(e)?;
    let mut out = Vec::new();
    for c in candidates {
        if alive.contains(c.as_str()) {
            continue;
        }
        if has
            .query_row(params![agent_id, c], |_| Ok(()))
            .optional()
            .map_err(e)?
            .is_some()
        {
            out.push(c);
        }
    }
    Ok(out)
}

pub(super) fn untracked_count(
    conn: &Connection,
    agent_id: &str,
    memory_ids: &BTreeMap<String, String>,
    fact_ids: &BTreeMap<String, String>,
) -> Result<u64> {
    // Rows with no lineage at all (written before the feature), plus rows
    // whose dispatch run was handed an incomplete upstream identity (their
    // upstream conversation is unknown, so forgetting it cannot reach them).
    let mut n = 0u64;
    for (sql, targets) in [
        (
            "SELECT m.id FROM memories m WHERE m.agent_id = ?1
               AND (NOT EXISTS (SELECT 1 FROM memory_origins o
                                WHERE o.memory_store = 'memories' AND o.memory_id = m.id)
                    OR EXISTS (SELECT 1 FROM memory_origins o
                               WHERE o.memory_store = 'memories' AND o.memory_id = m.id
                                 AND o.source_kind = ?2))",
            memory_ids,
        ),
        (
            "SELECT k.id FROM key_facts k WHERE k.agent_id = ?1
               AND (NOT EXISTS (SELECT 1 FROM memory_origins o
                                WHERE o.memory_store = 'key_facts' AND o.memory_id = k.id)
                    OR EXISTS (SELECT 1 FROM memory_origins o
                               WHERE o.memory_store = 'key_facts' AND o.memory_id = k.id
                                 AND o.source_kind = ?2))",
            fact_ids,
        ),
    ] {
        let mut stmt = conn.prepare(sql).map_err(e)?;
        let ids = stmt
            .query_map(
                params![agent_id, crate::lineage::UPSTREAM_UNKNOWN_KIND],
                |r| r.get::<_, String>(0),
            )
            .map_err(e)?;
        for id in ids {
            if !targets.contains_key(&id.map_err(e)?) {
                n += 1;
            }
        }
    }
    Ok(n)
}
