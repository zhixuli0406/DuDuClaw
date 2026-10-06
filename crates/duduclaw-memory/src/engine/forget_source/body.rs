//! Computing a forget plan's body from the frozen selector. Used identically
//! by plan (to record) and apply (to recompute and compare), so the two can
//! only disagree when the data changed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use super::*;
use crate::lineage::db::{
    Role, STORE_KEY_FACTS, STORE_MEMORIES, tombstone_match_sql, tombstone_upto_label,
};
use crate::lineage::{sha256_hex, source_digest};

/// One tombstone to write.
#[derive(Debug, Clone)]
pub(super) struct TombstoneSpec {
    pub scope: &'static str,
    pub session: String,
    pub message: String,
    pub upto_seq: Option<i64>,
    pub upto_time: Option<String>,
}

impl TombstoneSpec {
    pub(super) fn digest(&self, agent_id: &str) -> String {
        source_digest(
            agent_id,
            &self.session,
            &self.message,
            &tombstone_upto_label(self.upto_seq, self.upto_time.as_deref()),
        )
    }
}

/// One supersession pointer to clear at apply.
#[derive(Debug, Clone)]
pub(super) struct Cut {
    pub deleted: String,
    pub other: String,
    pub predecessor: bool,
}

/// Everything apply needs besides the hashed body.
#[derive(Debug, Clone)]
pub(super) struct Computed {
    pub body: PlanBody,
    pub tombstones: Vec<TombstoneSpec>,
    pub memory_ids: Vec<String>,
    pub fact_ids: Vec<String>,
    pub archive_ids: Vec<String>,
    pub lineage_only: Vec<(String, String)>,
    /// `(store, id, session, message)` of reaffirm-only lineage rows to drop.
    pub reaffirm_rows: Vec<(String, String, String, String)>,
    pub cuts: Vec<Cut>,
    pub orphan_entities: Vec<String>,
}

impl Computed {
    pub(super) fn is_empty(&self) -> bool {
        self.memory_ids.is_empty()
            && self.fact_ids.is_empty()
            && self.archive_ids.is_empty()
            && self.lineage_only.is_empty()
            && self.reaffirm_rows.is_empty()
            && self.body.wiki_pages.is_empty()
            // H-1: a message the operator wants forgotten is a source even
            // before anything was distilled from it — the plan still writes
            // its tombstone and hides it.
            && self.body.session_messages.is_empty()
    }
}

pub(super) enum Computation {
    Ok(Box<Computed>),
    TooLarge(String),
}

fn e(x: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(x.to_string())
}

pub(super) fn tombstone_specs(sel: &FrozenSelector) -> Vec<TombstoneSpec> {
    if sel.messages.is_empty() {
        vec![TombstoneSpec {
            scope: "session_upto",
            session: sel.session.clone(),
            message: String::new(),
            upto_seq: sel.upto_seq,
            upto_time: sel.upto_time.clone(),
        }]
    } else {
        sel.messages
            .iter()
            .map(|m| TombstoneSpec {
                scope: "message",
                session: sel.session.clone(),
                message: m.clone(),
                upto_seq: None,
                upto_time: None,
            })
            .collect()
    }
}

/// Load the candidate tombstones into the temp table the match SQL reads.
fn load_candidates(conn: &Connection, agent_id: &str, specs: &[TombstoneSpec]) -> Result<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS forget_candidates (
            agent_id TEXT NOT NULL, source_session TEXT NOT NULL, scope TEXT NOT NULL,
            source_message TEXT NOT NULL, upto_seq INTEGER, upto_time TEXT);
         CREATE INDEX IF NOT EXISTS temp.idx_forget_candidates
            ON forget_candidates(agent_id, source_session, scope, source_message);
         DELETE FROM temp.forget_candidates;",
    )
    .map_err(e)?;
    for s in specs {
        conn.execute(
            "INSERT INTO temp.forget_candidates
                (agent_id, source_session, scope, source_message, upto_seq, upto_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                agent_id,
                s.session,
                s.scope,
                s.message,
                s.upto_seq,
                s.upto_time
            ],
        )
        .map_err(e)?;
    }
    Ok(())
}

/// A lineage row as read for one memory row.
struct LineageKey {
    session: String,
    message: String,
    role: Role,
}

#[derive(Default)]
struct Found {
    /// (store, id) → (via, matched keys with their roles)
    rows: BTreeMap<(String, String), (String, Vec<LineageKey>)>,
}

pub(super) fn compute(
    conn: &Connection,
    agent_id: &str,
    sel: &FrozenSelector,
    max_rows: usize,
    external: &ExternalInputs,
) -> Result<Computation> {
    let mut specs = tombstone_specs(sel);
    load_candidates(conn, agent_id, &specs)?;
    // The turns of the forgotten messages (or of the whole conversation) are
    // the same source: their `mcp_turn` keys get tombstones too, so a later
    // write that names only the turn is fenced as well.
    let turns = super::turns::linked_turn_specs(conn, agent_id, sel)?;
    let linked_turns: Vec<String> = turns.iter().map(|t| t.message.clone()).collect();
    if !turns.is_empty() {
        specs.extend(turns);
        load_candidates(conn, agent_id, &specs)?;
    }

    // 1. Rows whose lineage matches a candidate tombstone.
    let mut found = Found::default();
    {
        let sql = format!(
            "SELECT o.memory_store, o.memory_id, o.role, o.source_session, o.source_message
             FROM memory_origins o
             WHERE o.agent_id = ?1 AND o.source_session = ?2 AND {}
             ORDER BY o.memory_store, o.memory_id",
            tombstone_match_sql(
                "temp.forget_candidates",
                "o.agent_id",
                "o.source_session",
                "o.source_message",
                "o.source_seq",
                "o.source_observed_at"
            )
        );
        let mut stmt = conn.prepare(&sql).map_err(e)?;
        let it = stmt
            .query_map(params![agent_id, sel.session], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(e)?;
        for row in it {
            let (store, id, role, session, message) = row.map_err(e)?;
            let entry = found
                .rows
                .entry((store, id))
                .or_insert_with(|| ("origins".to_string(), Vec::new()));
            entry.1.push(LineageKey {
                session,
                message,
                role: Role::parse(&role),
            });
        }
    }

    let has_archive = crate::lineage::db::table_exists(conn, "main", "memories_archive")?;
    let mut memory_ids: BTreeMap<String, String> = BTreeMap::new(); // id → via
    let mut fact_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut archive_ids: BTreeSet<String> = BTreeSet::new();
    let mut lineage_only: BTreeSet<(String, String)> = BTreeSet::new();
    let mut reaffirm_rows: Vec<(String, String, String, String)> = Vec::new();
    let mut reaffirm_only: Vec<ReaffirmOnlyEntry> = Vec::new();
    let mut matched_keys: HashMap<(String, String), HashSet<(String, String)>> = HashMap::new();
    let mut direct_rows: HashSet<(String, String)> = HashSet::new();

    for ((store, id), (via, keys)) in &found.rows {
        let deleting = keys.iter().any(|k| k.role != Role::Reaffirm);
        let exists_live = row_exists(conn, store, id, agent_id)?;
        if keys.iter().any(|k| k.role == Role::Direct) {
            direct_rows.insert((store.clone(), id.clone()));
        }
        if deleting {
            matched_keys.insert(
                (store.clone(), id.clone()),
                keys.iter()
                    .map(|k| (k.session.clone(), k.message.clone()))
                    .collect(),
            );
            if exists_live {
                if store == STORE_MEMORIES {
                    memory_ids.insert(id.clone(), via.clone());
                } else {
                    fact_ids.insert(id.clone(), via.clone());
                }
            } else if store == STORE_MEMORIES && has_archive && archived(conn, id, agent_id)? {
                archive_ids.insert(id.clone());
            } else {
                lineage_only.insert((store.clone(), id.clone()));
            }
        } else if exists_live {
            let mut digests: Vec<String> = keys
                .iter()
                .map(|k| source_digest(agent_id, &k.session, &k.message, ""))
                .collect();
            digests.sort();
            digests.dedup();
            for k in keys {
                reaffirm_rows.push((
                    store.clone(),
                    id.clone(),
                    k.session.clone(),
                    k.message.clone(),
                ));
            }
            reaffirm_only.push(ReaffirmOnlyEntry {
                store: store.clone(),
                id: id.clone(),
                removed_sources: digests,
            });
        }
    }

    // 2. Legacy rows by recorded session (session watermark only, D8).
    if let Some(upto) = sel.upto_time.as_deref().and_then(parse_rfc3339) {
        legacy_session_rows(
            conn,
            agent_id,
            &sel.session,
            upto,
            &mut memory_ids,
            &mut fact_ids,
        )?;
    }

    let total = memory_ids.len() + fact_ids.len() + archive_ids.len() + lineage_only.len();
    if total > max_rows {
        return Ok(Computation::TooLarge(format!(
            "{total} rows exceed the limit of {max_rows}"
        )));
    }

    // 3. Legacy closure over derived_from / metadata.source_ids.
    let mut roots: Vec<String> = memory_ids.keys().cloned().collect();
    roots.extend(archive_ids.iter().cloned());
    roots.extend(
        lineage_only
            .iter()
            .filter(|(s, _)| s == STORE_MEMORIES)
            .map(|(_, i)| i.clone()),
    );
    if let Some(reason) = legacy_closure(conn, agent_id, &roots, &mut memory_ids)? {
        return Ok(Computation::TooLarge(reason));
    }
    let total = memory_ids.len() + fact_ids.len() + archive_ids.len() + lineage_only.len();
    if total > max_rows {
        return Ok(Computation::TooLarge(format!(
            "{total} rows exceed the limit of {max_rows}"
        )));
    }

    // 4. Per-target details and collateral.
    let mut targets: Vec<TargetEntry> = Vec::new();
    let mut collateral: BTreeMap<String, u64> = BTreeMap::new();
    for (store, ids) in [(STORE_MEMORIES, &memory_ids), (STORE_KEY_FACTS, &fact_ids)] {
        for (id, via) in ids.iter() {
            let key = (store.to_string(), id.clone());
            // Found through its own source: a direct lineage row, or the
            // conversation the legacy row recorded itself.
            let direct = match via.as_str() {
                "origins" => direct_rows.contains(&key),
                "key_facts.source_session" | "metadata.session_id" => true,
                _ => false,
            };
            let t = target_entry(
                conn,
                agent_id,
                store,
                id,
                via,
                direct,
                matched_keys.get(&key),
            )?;
            for d in &t.other_sources {
                *collateral.entry(d.clone()).or_default() += 1;
            }
            targets.push(t);
        }
    }

    // 5. Supersession links that would point at a deleted row.
    let target_set: HashSet<&str> = memory_ids.keys().map(String::as_str).collect();
    let cuts = supersession_cuts(conn, agent_id, &memory_ids, &target_set)?;

    // 6. Entity embeddings left without any surviving triple.
    let orphan_entities =
        super::body_aux::orphan_entities(conn, agent_id, &memory_ids, &target_set)?;

    // 7. Counts shown to the operator.
    let untracked = super::body_aux::untracked_count(conn, agent_id, &memory_ids, &fact_ids)?;
    let other_ns: u64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT memory_store || ':' || memory_id) FROM memory_origins
             WHERE source_session = ?1 AND agent_id != ?2",
            params![sel.session, agent_id],
            |r| r.get::<_, i64>(0),
        )
        .map_err(e)? as u64;

    let mut steps: Vec<StepEntry> = Vec::new();
    for p in &external.wiki_pages {
        steps.push(StepEntry {
            step: STEP_WIKI_PAGE_DELETE.into(),
            target: p.path.clone(),
        });
    }
    if !memory_ids.is_empty() || !archive_ids.is_empty() {
        steps.push(StepEntry {
            step: STEP_REVIEW_SCRUB.into(),
            target: "targets".into(),
        });
    }
    if sel.messages.is_empty() {
        steps.push(StepEntry {
            step: STEP_SESSION_HIDE.into(),
            target: "upto".into(),
        });
    } else {
        for m in &sel.messages {
            steps.push(StepEntry {
                step: STEP_SESSION_HIDE.into(),
                target: m.clone(),
            });
        }
    }
    steps.push(StepEntry {
        step: STEP_SESSION_SUMMARY_CLEAR.into(),
        target: "session".into(),
    });

    let body = PlanBody {
        tombstones: specs
            .iter()
            .map(|s| TombstoneEntry {
                scope: s.scope.to_string(),
                digest: s.digest(agent_id),
            })
            .collect(),
        targets,
        reaffirm_only,
        archive_ids: archive_ids.iter().cloned().collect(),
        lineage_only: lineage_only
            .iter()
            .map(|(s, i)| StoreId {
                store: s.clone(),
                id: i.clone(),
            })
            .collect(),
        collateral: collateral
            .into_iter()
            .map(|(d, n)| CollateralEntry {
                source_digest: d,
                rows_lost: n,
            })
            .collect(),
        supersession_cuts: cuts
            .iter()
            .map(|c| CutEntry {
                deleted: c.deleted.clone(),
                other: c.other.clone(),
                relation: if c.predecessor {
                    "predecessor"
                } else {
                    "successor"
                }
                .into(),
                action: if c.predecessor {
                    "leave_closed"
                } else {
                    "clear_pointer"
                }
                .into(),
            })
            .collect(),
        entity_embeddings_orphaned: orphan_entities.len() as u64,
        untracked_in_namespace: untracked,
        linked_turns,
        other_namespaces_referencing: other_ns,
        wiki_pages: external.wiki_pages.clone(),
        review_cards_matching: external.review_cards_matching,
        session_messages: external.session_messages.clone(),
        steps,
        not_covered: [
            "transcript_text",
            "backups",
            "tool_calls_log",
            "mistake_notebook",
            "shared_wiki_copies",
            "night_cache",
            "causal_claims",
            "entity_alias",
            "employee_wiki_pages",
            "task_board",
            "working_state",
            "agent_mail",
            "goal_state_and_judge_feedback",
            "handoff_copies",
            "sub_agent_replies",
            "other_approval_cards",
            "runtime_cli_transcripts",
            "gemini_cli_runtime_mcp_writes",
            "grok_runtime_mcp_writes_unverified",
            "imported_file_at_another_path",
            "host_sources_removed_by_direct_edit",
            "rl_trajectories",
            "local_first_inference_tool_writes",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    };
    Ok(Computation::Ok(Box::new(Computed {
        body,
        tombstones: specs,
        memory_ids: memory_ids.into_keys().collect(),
        fact_ids: fact_ids.into_keys().collect(),
        archive_ids: archive_ids.into_iter().collect(),
        lineage_only: lineage_only.into_iter().collect(),
        reaffirm_rows,
        cuts,
        orphan_entities,
    })))
}

fn row_exists(conn: &Connection, store: &str, id: &str, agent_id: &str) -> Result<bool> {
    let sql = if store == STORE_MEMORIES {
        "SELECT 1 FROM memories WHERE id = ?1 AND agent_id = ?2"
    } else {
        "SELECT 1 FROM key_facts WHERE id = ?1 AND agent_id = ?2"
    };
    conn.prepare_cached(sql)
        .and_then(|mut s| s.query_row(params![id, agent_id], |_| Ok(())).optional())
        .map(|o| o.is_some())
        .map_err(e)
}

fn archived(conn: &Connection, id: &str, agent_id: &str) -> Result<bool> {
    conn.prepare_cached("SELECT 1 FROM memories_archive WHERE id = ?1 AND agent_id = ?2")
        .and_then(|mut s| s.query_row(params![id, agent_id], |_| Ok(())).optional())
        .map(|o| o.is_some())
        .map_err(e)
}

/// Rows without lineage that recorded this session (key facts'
/// `source_session`, decision rows' `metadata.session_id`) at or before the
/// watermark time.
fn legacy_session_rows(
    conn: &Connection,
    agent_id: &str,
    session: &str,
    upto: DateTime<Utc>,
    memory_ids: &mut BTreeMap<String, String>,
    fact_ids: &mut BTreeMap<String, String>,
) -> Result<()> {
    let mut stmt = conn
        .prepare(
            "SELECT k.id, k.timestamp FROM key_facts k
             WHERE k.agent_id = ?1 AND k.source_session = ?2
               AND NOT EXISTS (SELECT 1 FROM memory_origins o
                               WHERE o.memory_store = 'key_facts' AND o.memory_id = k.id)",
        )
        .map_err(e)?;
    let rows = stmt
        .query_map(params![agent_id, session], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(e)?;
    for row in rows {
        let (id, ts) = row.map_err(e)?;
        if parse_rfc3339(&ts).is_some_and(|t| t <= upto) {
            fact_ids
                .entry(id)
                .or_insert_with(|| "key_facts.source_session".into());
        }
    }
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.timestamp FROM memories m
             WHERE m.agent_id = ?1 AND json_valid(m.metadata)
               AND json_extract(m.metadata, '$.session_id') = ?2
               AND NOT EXISTS (SELECT 1 FROM memory_origins o
                               WHERE o.memory_store = 'memories' AND o.memory_id = m.id)",
        )
        .map_err(e)?;
    let rows = stmt
        .query_map(params![agent_id, session], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(e)?;
    for row in rows {
        let (id, ts) = row.map_err(e)?;
        if parse_rfc3339(&ts).is_some_and(|t| t <= upto) {
            memory_ids
                .entry(id)
                .or_insert_with(|| "metadata.session_id".into());
        }
    }
    Ok(())
}

/// Breadth-first closure over recorded parent ids (`derived_from`, night
/// `metadata.source_ids`). Returns a too-large reason when a limit is hit.
fn legacy_closure(
    conn: &Connection,
    agent_id: &str,
    roots: &[String],
    memory_ids: &mut BTreeMap<String, String>,
) -> Result<Option<String>> {
    let mut children: HashMap<String, Vec<(String, &'static str)>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, derived_from,
                        CASE WHEN json_valid(metadata) AND json_type(metadata, '$.source_ids') = 'array'
                             THEN json_extract(metadata, '$.source_ids') END
                 FROM memories
                 WHERE agent_id = ?1 AND (derived_from IS NOT NULL
                       OR (json_valid(metadata) AND json_type(metadata, '$.source_ids') = 'array'))",
            )
            .map_err(e)?;
        let rows = stmt
            .query_map(params![agent_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(e)?;
        for row in rows {
            let (id, derived, source_ids) = row.map_err(e)?;
            for (raw, via) in [(derived, "derived_from"), (source_ids, "source_ids")] {
                let parents: Vec<String> = raw
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                for p in parents {
                    children.entry(p).or_default().push((id.clone(), via));
                }
            }
        }
    }
    let mut visited: HashSet<String> = roots.iter().cloned().collect();
    let mut queue: VecDeque<(String, usize)> = roots.iter().map(|r| (r.clone(), 0)).collect();
    let mut discovered = 0usize;
    while let Some((node, depth)) = queue.pop_front() {
        let Some(kids) = children.get(&node) else {
            continue;
        };
        for (kid, via) in kids {
            if !visited.insert(kid.clone()) {
                continue;
            }
            if depth + 1 > BFS_MAX_DEPTH {
                return Ok(Some(format!(
                    "recorded-parent chain deeper than {BFS_MAX_DEPTH}"
                )));
            }
            discovered += 1;
            if discovered > BFS_MAX_NODES {
                return Ok(Some(format!(
                    "more than {BFS_MAX_NODES} rows reached through recorded parents"
                )));
            }
            memory_ids
                .entry(kid.clone())
                .or_insert_with(|| via.to_string());
            queue.push_back((kid.clone(), depth + 1));
        }
    }
    Ok(None)
}

fn target_entry(
    conn: &Connection,
    agent_id: &str,
    store: &str,
    id: &str,
    via: &str,
    direct: bool,
    matched: Option<&HashSet<(String, String)>>,
) -> Result<TargetEntry> {
    let (content, layer, predicate): (String, String, Option<String>) = if store == STORE_MEMORIES {
        conn.prepare_cached(
            "SELECT content, layer, predicate FROM memories WHERE id = ?1 AND agent_id = ?2",
        )
        .and_then(|mut s| {
            s.query_row(params![id, agent_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
        })
        .map_err(e)?
    } else {
        let fact: String = conn
            .prepare_cached("SELECT fact FROM key_facts WHERE id = ?1 AND agent_id = ?2")
            .and_then(|mut s| s.query_row(params![id, agent_id], |r| r.get(0)))
            .map_err(e)?;
        (fact, "key_fact".to_string(), None)
    };
    let mut stmt = conn
        .prepare_cached(
            "SELECT source_session, source_message FROM memory_origins
             WHERE memory_store = ?1 AND memory_id = ?2 AND source_kind <> ?3",
        )
        .map_err(e)?;
    // The upstream-unknown marker is not a source whose content is lost.
    let keys: Vec<(String, String)> = stmt
        .query_map(
            params![store, id, crate::lineage::UPSTREAM_UNKNOWN_KIND],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(e)?
        .collect::<std::result::Result<_, _>>()
        .map_err(e)?;
    let mut matched_d = Vec::new();
    let mut other_d = Vec::new();
    for (s, m) in keys {
        let d = source_digest(agent_id, &s, &m, "");
        if matched.is_some_and(|set| set.contains(&(s.clone(), m.clone()))) {
            matched_d.push(d);
        } else {
            other_d.push(d);
        }
    }
    matched_d.sort();
    matched_d.dedup();
    other_d.sort();
    other_d.dedup();
    Ok(TargetEntry {
        store: store.to_string(),
        id: id.to_string(),
        content_sha256: sha256_hex(content.as_bytes()),
        layer,
        predicate,
        matched: matched_d,
        other_sources: other_d,
        via: via.to_string(),
        direct,
    })
}

fn supersession_cuts(
    conn: &Connection,
    agent_id: &str,
    memory_ids: &BTreeMap<String, String>,
    targets: &HashSet<&str>,
) -> Result<Vec<Cut>> {
    // One scan per pointer column (not one per target): a large plan stays
    // linear in the namespace size.
    let mut cuts = Vec::new();
    for (col, predecessor) in [("superseded_by", true), ("supersedes", false)] {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, {col} FROM memories WHERE agent_id = ?1 AND {col} IS NOT NULL ORDER BY id"
            ))
            .map_err(e)?;
        let rows = stmt
            .query_map(params![agent_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(e)?;
        for row in rows {
            let (other, pointed) = row.map_err(e)?;
            if memory_ids.contains_key(&pointed) && !targets.contains(other.as_str()) {
                cuts.push(Cut {
                    deleted: pointed,
                    other,
                    predecessor,
                });
            }
        }
    }
    cuts.sort_by(|a, b| {
        (&a.deleted, &a.other, a.predecessor).cmp(&(&b.deleted, &b.other, b.predecessor))
    });
    Ok(cuts)
}
