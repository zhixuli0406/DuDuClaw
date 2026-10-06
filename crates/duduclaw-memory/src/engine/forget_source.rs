//! Forget by source (P2-B): plan, then apply.
//!
//! `plan_forget_source` only reads memory and records one row in
//! `memory_forget_plans`. Its document (canonical JSON, hashed) lists every
//! row that would go, why, and what else is lost with it — ids, digests and
//! counts only, never memory content.
//!
//! `apply_forget_plan` runs in one `BEGIN IMMEDIATE` transaction: it checks
//! the namespace's forget epoch, recomputes the plan from its frozen selector
//! and watermark, refuses (`Stale`) when the hash differs, then writes the
//! tombstones, deletes the rows (memories, FTS, archive copies, key facts),
//! cuts supersession links, drops orphaned entity embeddings, records the
//! external steps for the gateway, bumps the epoch and commits. Any failure
//! rolls everything back and leaves the plan `planned`.
//!
//! The external steps (`memory_forget_steps`) are only recorded here; running
//! them (wiki page deletion, review-card scrub, hiding messages, clearing the
//! session summary) belongs to the gateway.

use super::*;
use crate::lineage::format_ts;
use serde::Deserialize;

mod apply;
mod body;
mod body_aux;
mod turns;

/// `schema` of a plan document.
pub const PLAN_SCHEMA: &str = "duduclaw.memory.forget_plan.v1";
/// Default and hard cap of rows one plan may remove.
pub const DEFAULT_MAX_ROWS: usize = 50_000;
pub const HARD_MAX_ROWS: usize = 200_000;
/// Legacy (`derived_from` / `source_ids`) closure limits.
pub const BFS_MAX_NODES: usize = 10_000;
pub const BFS_MAX_DEPTH: usize = 32;
/// Plan lifetime.
pub const DEFAULT_TTL_MINUTES: i64 = 30;
pub const MAX_TTL_MINUTES: i64 = 1440;

/// External step kinds recorded at apply, run by the gateway.
pub const STEP_WIKI_PAGE_DELETE: &str = "wiki_page_delete";
pub const STEP_REVIEW_SCRUB: &str = "review_scrub";
pub const STEP_SESSION_HIDE: &str = "session_hide";
pub const STEP_SESSION_SUMMARY_CLEAR: &str = "session_summary_clear";

/// What the operator asked to forget.
#[derive(Debug, Clone, PartialEq)]
pub struct ForgetSelector {
    /// The source session (channel session id, `cron:<agent>`, …).
    pub session: String,
    /// Message keys (`m:812`, `run:…`). Non-empty ⇒ message scope; empty ⇒
    /// the whole session up to the watermark.
    pub messages: Vec<String>,
    /// Watermark: the session's highest `session_messages.id` at plan time.
    pub upto_seq: Option<i64>,
    /// Watermark time; `None` ⇒ the plan's creation time.
    pub upto_time: Option<DateTime<Utc>>,
}

/// Plan knobs (`None` ⇒ default).
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanOptions {
    pub max_rows: Option<usize>,
    pub ttl_minutes: Option<i64>,
}

/// A wiki page the caller found carrying a forgotten source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WikiPageRef {
    pub path: String,
    pub file_sha256: String,
}

/// A session message the caller will hide (digest only).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionMessageRef {
    pub session_digest: String,
    pub seq: i64,
}

/// What the caller (gateway / CLI) found outside `memory.db`. Part of the
/// plan hash: a change between plan and apply makes the plan stale.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExternalInputs {
    pub wiki_pages: Vec<WikiPageRef>,
    pub review_cards_matching: u64,
    pub session_messages: Vec<SessionMessageRef>,
}

impl ExternalInputs {
    fn canonical(&self) -> Self {
        let mut c = self.clone();
        c.wiki_pages.sort();
        c.wiki_pages.dedup();
        c.session_messages.sort();
        c.session_messages.dedup();
        c
    }
}

/// The selector as frozen into the plan (watermark resolved).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrozenSelector {
    pub session: String,
    pub messages: Vec<String>,
    pub upto_seq: Option<i64>,
    pub upto_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanLimits {
    pub max_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TombstoneEntry {
    pub scope: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetEntry {
    pub store: String,
    pub id: String,
    pub content_sha256: String,
    pub layer: String,
    pub predicate: Option<String>,
    /// Digests of the forgotten sources this row carries.
    pub matched: Vec<String>,
    /// Digests of the row's other sources (lost with it).
    pub other_sources: Vec<String>,
    /// How the row was found: `origins`, `derived_from`, `source_ids`,
    /// `key_facts.source_session`, `metadata.session_id`.
    pub via: String,
    /// `true` when the forgotten source is one of the row's own (direct)
    /// sources; `false` when the row was reached because it was derived from
    /// something that came from it (inherited lineage, recorded parents).
    #[serde(default)]
    pub direct: bool,
}

/// A row kept because the forgotten source only corroborated it (B.2): only
/// that corroboration record is removed; its confidence is not rolled back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReaffirmOnlyEntry {
    pub store: String,
    pub id: String,
    pub removed_sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreId {
    pub store: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollateralEntry {
    pub source_digest: String,
    pub rows_lost: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CutEntry {
    pub deleted: String,
    pub other: String,
    /// `predecessor` (stays closed, D2) or `successor` (its back-pointer cleared).
    pub relation: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepEntry {
    pub step: String,
    pub target: String,
}

/// The recomputable part of a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanBody {
    pub tombstones: Vec<TombstoneEntry>,
    pub targets: Vec<TargetEntry>,
    pub reaffirm_only: Vec<ReaffirmOnlyEntry>,
    pub archive_ids: Vec<String>,
    pub lineage_only: Vec<StoreId>,
    pub collateral: Vec<CollateralEntry>,
    pub supersession_cuts: Vec<CutEntry>,
    pub entity_embeddings_orphaned: u64,
    pub untracked_in_namespace: u64,
    pub other_namespaces_referencing: u64,
    /// The `turn:` keys tombstoned because the forgotten messages (or the
    /// session watermark) started those turns (see `turns.rs`). Kept as keys
    /// so the review-page check can match pages stamped with only the turn.
    #[serde(default)]
    pub linked_turns: Vec<String>,
    pub wiki_pages: Vec<WikiPageRef>,
    pub review_cards_matching: u64,
    pub session_messages: Vec<SessionMessageRef>,
    pub steps: Vec<StepEntry>,
    pub not_covered: Vec<String>,
}

/// A whole plan document (what `plan_hash` covers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanDocument {
    pub schema: String,
    pub plan_id: String,
    pub db_instance_id: String,
    pub agent_id: String,
    pub created_at: String,
    pub expires_at: String,
    pub forget_epoch: i64,
    pub selector: FrozenSelector,
    pub limits: PlanLimits,
    #[serde(flatten)]
    pub body: PlanBody,
}

impl PlanDocument {
    /// Canonical JSON (keys sorted at every level, no extra whitespace).
    pub fn canonical_json(&self) -> Result<String> {
        let v = serde_json::to_value(self).map_err(|e| DuDuClawError::Memory(e.to_string()))?;
        Ok(canonical(&v))
    }

    /// The plan hash: sha256 of the canonical JSON with the informational
    /// counts ([`PlanBody::untracked_in_namespace`],
    /// [`PlanBody::other_namespaces_referencing`]) zeroed. The hash binds
    /// what an apply will do — rows and their content hashes, tombstones,
    /// steps — not counts that unrelated writes in the namespace move
    /// (decay archiving, key-fact pruning, other employees' writes), which
    /// would otherwise make a plan waiting for approval go stale. The stored
    /// JSON keeps the plan-time values; an apply reports both.
    pub fn plan_hash(&self) -> Result<String> {
        let mut d = self.clone();
        d.body.untracked_in_namespace = 0;
        d.body.other_namespaces_referencing = 0;
        Ok(crate::lineage::sha256_hex(d.canonical_json()?.as_bytes()))
    }
}

/// Serialize a JSON value with object keys sorted recursively, independent of
/// serde_json's `preserve_order` feature.
fn canonical(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String(k.clone()),
                        canonical(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(a) => {
            format!(
                "[{}]",
                a.iter().map(canonical).collect::<Vec<_>>().join(",")
            )
        }
        other => other.to_string(),
    }
}

/// A stored plan.
#[derive(Debug, Clone, PartialEq)]
pub struct ForgetPlan {
    pub plan_id: String,
    pub plan_hash: String,
    pub status: String,
    pub applied_at: Option<String>,
    pub document: PlanDocument,
}

/// Result of [`SqliteMemoryEngine::plan_forget_source`].
#[derive(Debug, Clone, PartialEq)]
pub enum PlanOutcome {
    Planned(Box<ForgetPlan>),
    /// Nothing in this namespace carries the source; nothing was recorded.
    NothingToForget {
        untracked_in_namespace: u64,
    },
    /// A limit was exceeded; nothing was recorded (no partial deletion).
    TooLarge {
        reason: String,
    },
}

/// Why an apply refused a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StaleReason {
    /// Another apply changed this namespace since the plan.
    Epoch { planned: i64, current: i64 },
    /// The recomputed plan differs (counts only, never content).
    Changed {
        targets_planned: u64,
        targets_now: u64,
        added: u64,
        removed: u64,
        changed: u64,
        /// Other parts of the plan that differ ([`PLAN_PART_NAMES`] tokens),
        /// e.g. `tombstones` or `session_messages` when no target changed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        other_parts: Vec<String>,
    },
    /// The recomputed plan exceeds a limit.
    TooLarge { reason: String },
    /// The plan was already marked stale.
    AlreadyStale,
    /// More than one of the above holds at once (each listed once).
    Several { reasons: Vec<StaleReason> },
}

impl StaleReason {
    /// The individual reasons (a [`StaleReason::Several`] flattened).
    pub fn reasons(&self) -> Vec<&StaleReason> {
        match self {
            Self::Several { reasons } => reasons.iter().flat_map(|r| r.reasons()).collect(),
            other => vec![other],
        }
    }
}

/// The plan parts a [`StaleReason::Changed`] can name besides its targets.
pub const PLAN_PART_NAMES: &[&str] = &[
    "tombstones",
    "linked_turns",
    "reaffirm_only",
    "archive_ids",
    "lineage_only",
    "collateral",
    "supersession_cuts",
    "entity_embeddings_orphaned",
    "wiki_pages",
    "review_cards_matching",
    "session_messages",
    "steps",
    "not_covered",
];

/// Counts of one successful apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub plan_id: String,
    pub agent_id: String,
    pub plan_hash: String,
    pub memories_deleted: u64,
    pub key_facts_deleted: u64,
    pub archive_deleted: u64,
    pub entity_embeddings_deleted: u64,
    pub supersession_links_cut: u64,
    pub reaffirm_lineage_removed: u64,
    pub tombstones_written: u64,
    pub steps_pending: u64,
    pub forget_epoch: i64,
    pub elapsed_ms: u64,
    /// Informational counts (not in the plan hash): at plan time and as
    /// recomputed at apply.
    pub untracked_in_namespace_planned: u64,
    pub untracked_in_namespace_at_apply: u64,
    pub other_namespaces_referencing_planned: u64,
    pub other_namespaces_referencing_at_apply: u64,
}

/// Result of [`SqliteMemoryEngine::apply_forget_plan`].
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyOutcome {
    Applied(ApplyReport),
    /// Already applied; only its pending / failed steps remain to run.
    AlreadyApplied,
    NotFound,
    Expired,
    /// The plan was made against another database.
    DbMismatch,
    Stale(StaleReason),
}

/// One external step row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgetStep {
    pub plan_id: String,
    pub step: String,
    pub target: String,
    pub status: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub updated_at: String,
}

fn err(e: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Memory(e.to_string())
}

impl ForgetSelector {
    /// Validate and freeze (watermark time defaults to `now`).
    fn freeze(&self, now: DateTime<Utc>) -> Result<FrozenSelector> {
        let session = self.session.trim();
        if session.is_empty() || session.len() > crate::lineage::MAX_SOURCE_KEY_BYTES {
            return Err(err("forget selector: session is empty or too long"));
        }
        if session.chars().any(char::is_control) {
            return Err(err("forget selector: session contains a control character"));
        }
        if session.starts_with(crate::lineage::SYSTEM_SESSION_PREFIX)
            || session == crate::lineage::UNTRACKED_SESSION
        {
            return Err(err(
                "forget selector: system content cannot be forgotten by source",
            ));
        }
        let mut messages: Vec<String> = Vec::new();
        for m in &self.messages {
            let m = m.trim();
            if m.is_empty()
                || m.len() > crate::lineage::MAX_SOURCE_KEY_BYTES
                || m.chars().any(char::is_control)
            {
                return Err(err("forget selector: empty or too long message key"));
            }
            if !messages.iter().any(|x| x == m) {
                messages.push(m.to_string());
            }
        }
        if messages.len() > 1000 {
            return Err(err("forget selector: at most 1000 messages per plan"));
        }
        messages.sort();
        if !messages.is_empty() {
            return Ok(FrozenSelector {
                session: session.to_string(),
                messages,
                upto_seq: None,
                upto_time: None,
            });
        }
        if self.upto_seq.is_some_and(|s| s < 0) {
            return Err(err("forget selector: negative watermark"));
        }
        // A whole import source is forgotten for good (H-3): its records'
        // observation time says nothing about whether they are new.
        if session.starts_with(crate::lineage::IMPORT_SESSION_PREFIX) {
            return Ok(FrozenSelector {
                session: session.to_string(),
                messages,
                upto_seq: None,
                upto_time: Some(crate::lineage::FOREVER_TS.to_string()),
            });
        }
        Ok(FrozenSelector {
            session: session.to_string(),
            messages,
            upto_seq: self.upto_seq,
            upto_time: Some(format_ts(self.upto_time.unwrap_or(now))),
        })
    }
}

impl SqliteMemoryEngine {
    /// Dry run: compute and record a forget plan for one namespace. Reads
    /// memory only; writes nothing but the plan row.
    pub async fn plan_forget_source(
        &self,
        agent_id: &str,
        selector: &ForgetSelector,
        options: PlanOptions,
        external: &ExternalInputs,
    ) -> Result<PlanOutcome> {
        if agent_id.trim().is_empty() {
            return Err(err("plan_forget_source: empty namespace"));
        }
        let now = Utc::now();
        let frozen = selector.freeze(now)?;
        let max_rows = options
            .max_rows
            .unwrap_or(DEFAULT_MAX_ROWS)
            .clamp(1, HARD_MAX_ROWS);
        let ttl = options
            .ttl_minutes
            .unwrap_or(DEFAULT_TTL_MINUTES)
            .clamp(1, MAX_TTL_MINUTES);
        let external = external.canonical();

        let conn = self.conn.lock().await;
        // A deferred read transaction: one consistent snapshot for the plan.
        conn.execute_batch("BEGIN").map_err(err)?;
        let work = (|| -> Result<PlanOutcome> {
            let computed = match body::compute(&conn, agent_id, &frozen, max_rows, &external)? {
                body::Computation::Ok(c) => c,
                body::Computation::TooLarge(reason) => return Ok(PlanOutcome::TooLarge { reason }),
            };
            if computed.is_empty() {
                return Ok(PlanOutcome::NothingToForget {
                    untracked_in_namespace: computed.body.untracked_in_namespace,
                });
            }
            let document = PlanDocument {
                schema: PLAN_SCHEMA.to_string(),
                plan_id: uuid::Uuid::new_v4().to_string(),
                db_instance_id: crate::lineage::db::db_instance_id(&conn)?,
                agent_id: agent_id.to_string(),
                created_at: format_ts(now),
                expires_at: format_ts(now + chrono::Duration::minutes(ttl)),
                forget_epoch: crate::lineage::db::forget_epoch(&conn, agent_id)?,
                selector: frozen.clone(),
                limits: PlanLimits { max_rows },
                body: computed.body,
            };
            Ok(PlanOutcome::Planned(Box::new(ForgetPlan {
                plan_id: document.plan_id.clone(),
                plan_hash: String::new(),
                status: "planned".to_string(),
                applied_at: None,
                document,
            })))
        })();
        let _ = conn.execute_batch("COMMIT");
        let mut outcome = work?;
        if let PlanOutcome::Planned(plan) = &mut outcome {
            let json = plan.document.canonical_json()?;
            plan.plan_hash = plan.document.plan_hash()?;
            conn.execute(
                "INSERT INTO memory_forget_plans
                    (plan_id, agent_id, plan_hash, plan_json, forget_epoch, db_instance_id,
                     created_at, expires_at, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'planned')",
                params![
                    plan.plan_id,
                    agent_id,
                    plan.plan_hash,
                    json,
                    plan.document.forget_epoch,
                    plan.document.db_instance_id,
                    plan.document.created_at,
                    plan.document.expires_at
                ],
            )
            .map_err(err)?;
        }
        Ok(outcome)
    }

    /// Read-only preview of the `memories` ids (live and archived) a plan
    /// with `selector` would remove, for callers that must look outside
    /// `memory.db` before planning (review cards referencing those ids).
    /// Records nothing. `None` when a limit would be exceeded. Pass an
    /// explicit `upto_time` so the preview and the plan freeze the same
    /// watermark.
    pub async fn preview_forget_memory_ids(
        &self,
        agent_id: &str,
        selector: &ForgetSelector,
        options: PlanOptions,
    ) -> Result<Option<Vec<String>>> {
        let frozen = selector.freeze(Utc::now())?;
        let max_rows = options
            .max_rows
            .unwrap_or(DEFAULT_MAX_ROWS)
            .clamp(1, HARD_MAX_ROWS);
        let conn = self.conn.lock().await;
        conn.execute_batch("BEGIN").map_err(err)?;
        let work = body::compute(
            &conn,
            agent_id,
            &frozen,
            max_rows,
            &ExternalInputs::default(),
        );
        let _ = conn.execute_batch("COMMIT");
        Ok(match work? {
            body::Computation::Ok(c) => {
                let mut ids = c.memory_ids.clone();
                ids.extend(c.archive_ids.iter().cloned());
                Some(ids)
            }
            body::Computation::TooLarge(_) => None,
        })
    }

    /// A stored plan by id.
    pub async fn get_forget_plan(&self, plan_id: &str) -> Result<Option<ForgetPlan>> {
        let conn = self.conn.lock().await;
        Self::load_plan(&conn, plan_id)
    }

    fn load_plan(conn: &Connection, plan_id: &str) -> Result<Option<ForgetPlan>> {
        let row: Option<(String, String, String, Option<String>)> = conn
            .query_row(
                "SELECT plan_hash, plan_json, status, applied_at
                 FROM memory_forget_plans WHERE plan_id = ?1",
                params![plan_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(err)?;
        let Some((plan_hash, json, status, applied_at)) = row else {
            return Ok(None);
        };
        let document: PlanDocument = serde_json::from_str(&json)
            .map_err(|e| err(format!("stored forget plan unreadable: {e}")))?;
        Ok(Some(ForgetPlan {
            plan_id: plan_id.to_string(),
            plan_hash,
            status,
            applied_at,
            document,
        }))
    }

    /// Apply a stored plan (see the module doc). `external` must be what the
    /// caller finds now, computed the same way as at plan time.
    pub async fn apply_forget_plan(
        &self,
        plan_id: &str,
        external: &ExternalInputs,
    ) -> Result<ApplyOutcome> {
        let started = std::time::Instant::now();
        let (outcome, agent) = {
            let conn = self.conn.lock().await;
            apply::apply_locked(self, &conn, plan_id, &external.canonical(), started)?
        };
        if let (ApplyOutcome::Applied(_), Some(agent)) = (&outcome, agent) {
            self.bump_graph_generation(&agent);
        }
        Ok(outcome)
    }

    /// External steps of one plan.
    pub async fn forget_steps(&self, plan_id: &str) -> Result<Vec<ForgetStep>> {
        let conn = self.conn.lock().await;
        Self::query_steps(
            &conn,
            "SELECT plan_id, step, target, status, attempts, last_error, updated_at
             FROM memory_forget_steps WHERE plan_id = ?1 ORDER BY step, target",
            Some(plan_id),
        )
    }

    /// Every step not yet done (pending or failed), any plan — for the
    /// gateway's boot-time resume.
    pub async fn unfinished_forget_steps(&self) -> Result<Vec<ForgetStep>> {
        let conn = self.conn.lock().await;
        Self::query_steps(
            &conn,
            "SELECT plan_id, step, target, status, attempts, last_error, updated_at
             FROM memory_forget_steps WHERE status != 'done' ORDER BY plan_id, step, target",
            None,
        )
    }

    fn query_steps(conn: &Connection, sql: &str, plan_id: Option<&str>) -> Result<Vec<ForgetStep>> {
        let mut stmt = conn.prepare(sql).map_err(err)?;
        let map = |r: &rusqlite::Row<'_>| {
            Ok(ForgetStep {
                plan_id: r.get(0)?,
                step: r.get(1)?,
                target: r.get(2)?,
                status: r.get(3)?,
                attempts: r.get(4)?,
                last_error: r.get(5)?,
                updated_at: r.get(6)?,
            })
        };
        let rows = match plan_id {
            Some(p) => stmt.query_map(params![p], map),
            None => stmt.query_map([], map),
        }
        .map_err(err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(err)
    }

    /// The source fence without a write: the first of `sources` that matches
    /// a tombstone of `agent_id`, as a digest-only refusal. For derived
    /// artifacts kept outside `memory.db` (auto wiki pages), which check
    /// before and after they are written. A malformed source is an `Err`.
    pub async fn source_fence_check(
        &self,
        agent_id: &str,
        sources: &[crate::lineage::SourceRef],
    ) -> Result<Option<crate::lineage::FenceRefusal>> {
        let mut rows = Vec::with_capacity(sources.len());
        for s in sources {
            s.validate()
                .map_err(|e| err(format!("invalid source: {e}")))?;
            rows.push(crate::lineage::db::OriginRow {
                kind: s.kind.as_str().to_string(),
                session: s.session.clone(),
                message: s.message.clone(),
                seq: s.seq,
                observed_at: format_ts(s.observed_at),
                hash: s.content_hash.clone(),
                role: crate::lineage::db::Role::Direct,
                via: None,
            });
        }
        let conn = self.conn.lock().await;
        crate::lineage::db::first_forgotten(&conn, agent_id, &rows)
    }

    /// The subset of `ids` that are live `memories` rows of `agent_id`
    /// (neither deleted nor forgotten). Order follows `ids`.
    pub async fn live_memory_ids(&self, agent_id: &str, ids: &[String]) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(
                "SELECT 1 FROM memories m WHERE m.id = ?1 AND m.agent_id = ?2
                 AND NOT EXISTS (SELECT 1 FROM forgotten_memories f
                                 WHERE f.memory_store = 'memories' AND f.memory_id = m.id)",
            )
            .map_err(err)?;
        let mut out = Vec::new();
        for id in ids {
            let live = stmt
                .query_row(params![id, agent_id], |_| Ok(()))
                .optional()
                .map_err(err)?
                .is_some();
            if live && !out.contains(id) {
                out.push(id.clone());
            }
        }
        Ok(out)
    }

    /// Record the result of running one external step (idempotent).
    /// `error` is truncated to 500 characters.
    pub async fn mark_forget_step(
        &self,
        plan_id: &str,
        step: &str,
        target: &str,
        result: std::result::Result<(), String>,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let now = format_ts(Utc::now());
        let (status, error) = match result {
            Ok(()) => ("done", None),
            Err(e) => (
                "failed",
                Some(duduclaw_core::truncate_chars(&e, 500).to_string()),
            ),
        };
        let n = conn
            .execute(
                "UPDATE memory_forget_steps
                 SET status = ?1, last_error = ?2, attempts = attempts + 1, updated_at = ?3
                 WHERE plan_id = ?4 AND step = ?5 AND target = ?6",
                params![status, error, now, plan_id, step, target],
            )
            .map_err(err)?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_gt34;
#[cfg(test)]
mod tests_links;
#[cfg(test)]
mod tests_races;
#[cfg(test)]
mod tests_review;
#[cfg(test)]
mod tests_turns;
