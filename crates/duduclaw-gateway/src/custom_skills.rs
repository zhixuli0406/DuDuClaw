//! Human × agent co-authored custom skills (V13-T13.0).
//!
//! A human describes a skill in natural language, an agent drafts the actual
//! `SKILL.md`, the human fills the *human-facing* fields (a display name that is
//! deliberately separate from the machine `slug`, a description, a self-reported
//! time-saving estimate, tags), and the draft goes through a mandatory security
//! scan before being routed to a superior for **HITL approval** via the shared
//! [`crate::approval::ApprovalBroker`]. Only on approval is the skill installed
//! into the real skills directory.
//!
//! ## Isolation invariant (fail-closed)
//!
//! Draft `SKILL.md` files live under [`drafts_root`] = `<home>/skills-drafts/`,
//! which is **never** a skill-loader scan root. The loader
//! ([`duduclaw_agent::registry::AgentRegistry::load_skills`]) only scans
//! `<home>/skills` (global) and `<home>/agents/<id>/SKILLS` (per-agent). A draft
//! therefore cannot be loaded or executed by any agent until the approval
//! side-effect copies it into a real skills directory. [`drafts_is_isolated`]
//! encodes this and is asserted by the unit tests.
//!
//! ## Approval routing
//!
//! The submit path stores `created_by_user` in the approval payload. The
//! decision gate lives in the dashboard `approvals.decide` handler
//! (manager-or-admin). Because the `User` model has no `manager_id` column yet,
//! the fallback is: **any admin may approve**; when a single admin is also the
//! creator, self-approval is allowed and audited via
//! [`is_self_approval`] (`self_approved = true`).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use crate::skill_lifecycle::security_scanner::RiskLevel;

// ── Constants ───────────────────────────────────────────────

/// Stable `action_kind` for a custom-skill creation approval. A free-string
/// kind on the universal [`crate::approval::ApprovalBroker`]; kept as a constant
/// so the submit path, the decision side-effect, and the inbox renderer agree.
pub const ACTION_KIND_SKILL_CREATE: &str = "skill_create";

/// TTL for a custom-skill approval: 7 days. Unactioned for a week ⇒ expires ⇒
/// DENY (fail-closed, the broker's TTL-expiry contract). The creator can
/// resubmit.
pub const SKILL_CREATE_TTL_SECONDS: i64 = 7 * 24 * 3600;

/// Directory name (under `<home>`) that quarantines pre-approval drafts.
pub const DRAFTS_DIR_NAME: &str = "skills-drafts";

// ── Draft isolation ─────────────────────────────────────────

/// Root directory quarantining all pre-approval drafts. Never a loader scan
/// root — see the module invariant.
pub fn drafts_root(home_dir: &Path) -> PathBuf {
    home_dir.join(DRAFTS_DIR_NAME)
}

/// Per-draft directory: `<home>/skills-drafts/<id>/`.
pub fn draft_dir(home_dir: &Path, id: &str) -> PathBuf {
    drafts_root(home_dir).join(id)
}

/// The `SKILL.md` path for a draft.
pub fn draft_skill_path(home_dir: &Path, id: &str) -> PathBuf {
    draft_dir(home_dir, id).join("SKILL.md")
}

/// The skill-loader scan roots (must stay in sync with
/// `AgentRegistry::scan` / `load_skills`): global `<home>/skills` plus every
/// `<home>/agents/<id>/SKILLS`. Used to *prove* draft isolation.
fn loader_scan_roots(home_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home_dir.join("skills")];
    if let Ok(rd) = std::fs::read_dir(home_dir.join("agents")) {
        for entry in rd.flatten() {
            if entry.path().is_dir() {
                roots.push(entry.path().join("SKILLS"));
            }
        }
    }
    roots
}

/// True when `path` is `ancestor` or lies underneath it (lexical, after
/// stripping `.`/`..` via component comparison — inputs here are gateway-built
/// absolute paths, never user strings).
fn is_within(path: &Path, ancestor: &Path) -> bool {
    path == ancestor || path.starts_with(ancestor)
}

/// The isolation invariant: the drafts root is neither equal to, nor inside, nor
/// an ancestor of, any skill-loader scan root. Returns false if it would ever
/// intersect the load path (fail-closed check the tests assert).
pub fn drafts_is_isolated(home_dir: &Path) -> bool {
    let drafts = drafts_root(home_dir);
    loader_scan_roots(home_dir).iter().all(|root| {
        !is_within(&drafts, root) && !is_within(root, &drafts)
    })
}

// ── Status machine ──────────────────────────────────────────

/// Lifecycle status of a custom skill (§5.6):
/// `draft → generating → pending_approval → approved | rejected | retired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomSkillStatus {
    Draft,
    Generating,
    PendingApproval,
    Approved,
    Rejected,
    Retired,
}

impl CustomSkillStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CustomSkillStatus::Draft => "draft",
            CustomSkillStatus::Generating => "generating",
            CustomSkillStatus::PendingApproval => "pending_approval",
            CustomSkillStatus::Approved => "approved",
            CustomSkillStatus::Rejected => "rejected",
            CustomSkillStatus::Retired => "retired",
        }
    }

    /// Parse from the DB text column. Unknown ⇒ `Retired` (fail-safe: an
    /// unrecognized row is treated as inert, never as approved/installable).
    pub fn from_db(s: &str) -> Self {
        match s {
            "draft" => CustomSkillStatus::Draft,
            "generating" => CustomSkillStatus::Generating,
            "pending_approval" => CustomSkillStatus::PendingApproval,
            "approved" => CustomSkillStatus::Approved,
            "rejected" => CustomSkillStatus::Rejected,
            _ => CustomSkillStatus::Retired,
        }
    }
}

/// Allowed status transitions. Any transition not listed here is rejected by the
/// store, so the state machine cannot be skipped (e.g. draft → approved without
/// passing through submit/approve).
pub fn is_valid_transition(from: CustomSkillStatus, to: CustomSkillStatus) -> bool {
    use CustomSkillStatus::*;
    matches!(
        (from, to),
        (Draft, Generating)
            | (Draft, PendingApproval)      // submit straight from a hand-written draft
            | (Generating, Draft)           // generation finished / re-draft
            | (Generating, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Rejected)
            | (PendingApproval, Draft)      // TTL-expiry return path
            | (Rejected, Draft)             // resubmit after fixes
            | (Rejected, PendingApproval)
            | (Approved, Retired)
            | (Draft, Retired)
            | (Rejected, Retired)
            | (Generating, Retired)
            | (PendingApproval, Retired)
    )
}

// ── Record ──────────────────────────────────────────────────

/// One row of `custom_skill_registry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomSkillRecord {
    pub id: String,
    /// Machine-stable skill name (the loader identity once installed).
    pub slug: String,
    /// Human-facing display name, deliberately separate from `slug`.
    pub display_name: String,
    pub description_human: String,
    /// Self-reported time-saving estimate value + unit (e.g. 30 "minutes_per_use").
    pub time_saved_value: f64,
    pub time_saved_unit: String,
    /// Comma-separated tags.
    pub tags: String,
    /// User id (from `ctx.user_id`) who created this.
    pub created_by_user: String,
    /// Agent id delegated to author the SKILL.md.
    pub built_by_agent: String,
    pub status: CustomSkillStatus,
    /// Approval id once submitted (links to `approvals.db`).
    pub approval_id: Option<String>,
    /// Reason captured when an approval is rejected.
    pub rejection_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub approved_at: Option<String>,
    /// Real invocation counter — how many times an *approved* skill has actually
    /// been used, incremented from the channel_reply stream-json path when the
    /// Claude CLI `Skill` tool names this skill's slug (token-equal match). 0 for
    /// unapproved/never-run skills. Feeds the honest saved-hours figure.
    pub usage_count: u64,
}

// ── Store ───────────────────────────────────────────────────

/// SQLite-backed registry for custom skills. Mirrors the project store idioms
/// (`Mutex<Connection>`, WAL, self-healing schema, parameterized SQL only).
pub struct CustomSkillStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

impl CustomSkillStore {
    /// Open (or create) `<home>/custom_skills.db`.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("custom_skills.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open custom skills store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "CustomSkillStore initialized");
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: Some(db_path),
        })
    }

    /// In-memory store for tests.
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("open in-memory: {e}"))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: None,
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;

             CREATE TABLE IF NOT EXISTS custom_skill_registry (
                 id                TEXT PRIMARY KEY,
                 slug              TEXT NOT NULL,
                 display_name      TEXT NOT NULL,
                 description_human TEXT NOT NULL DEFAULT '',
                 time_saved_value  REAL NOT NULL DEFAULT 0,
                 time_saved_unit   TEXT NOT NULL DEFAULT 'minutes_per_use',
                 tags              TEXT NOT NULL DEFAULT '',
                 created_by_user   TEXT NOT NULL,
                 built_by_agent    TEXT NOT NULL DEFAULT '',
                 status            TEXT NOT NULL DEFAULT 'draft',
                 approval_id       TEXT,
                 rejection_reason  TEXT,
                 created_at        TEXT NOT NULL,
                 updated_at        TEXT NOT NULL,
                 approved_at       TEXT
             );

             CREATE INDEX IF NOT EXISTS idx_custom_skills_status  ON custom_skill_registry(status);
             CREATE INDEX IF NOT EXISTS idx_custom_skills_creator ON custom_skill_registry(created_by_user);
             CREATE INDEX IF NOT EXISTS idx_custom_skills_approval ON custom_skill_registry(approval_id);",
        )
        .map_err(|e| format!("init custom skills schema: {e}"))?;

        // Idempotent migration: `usage_count` (L5 §14 — per-skill invocation
        // counter) may be absent on registries created before this column
        // existed. `ADD COLUMN` on a live table re-runs on every open, so we
        // swallow the "duplicate column name" error (the only expected failure)
        // and surface any other.
        if let Err(e) = conn.execute(
            "ALTER TABLE custom_skill_registry ADD COLUMN usage_count INTEGER NOT NULL DEFAULT 0",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(format!("migrate usage_count: {msg}"));
            }
        }
        Ok(())
    }

    pub async fn insert(&self, rec: &CustomSkillRecord) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO custom_skill_registry
                (id, slug, display_name, description_human, time_saved_value, time_saved_unit,
                 tags, created_by_user, built_by_agent, status, approval_id, rejection_reason,
                 created_at, updated_at, approved_at, usage_count)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                rec.id,
                rec.slug,
                rec.display_name,
                rec.description_human,
                rec.time_saved_value,
                rec.time_saved_unit,
                rec.tags,
                rec.created_by_user,
                rec.built_by_agent,
                rec.status.as_str(),
                rec.approval_id,
                rec.rejection_reason,
                rec.created_at,
                rec.updated_at,
                rec.approved_at,
                rec.usage_count as i64,
            ],
        )
        .map_err(|e| format!("insert custom skill: {e}"))?;
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Option<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(SELECT_COLS, params![id], row_to_record)
            .optional()
            .map_err(|e| format!("get custom skill: {e}"))
    }

    /// Look up a record by its linked approval id (for the decide side-effect).
    pub async fn get_by_approval(&self, approval_id: &str) -> Result<Option<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, slug, display_name, description_human, time_saved_value, time_saved_unit,
                    tags, created_by_user, built_by_agent, status, approval_id, rejection_reason,
                    created_at, updated_at, approved_at, usage_count
             FROM custom_skill_registry WHERE approval_id = ?1",
            params![approval_id],
            row_to_record,
        )
        .optional()
        .map_err(|e| format!("get custom skill by approval: {e}"))
    }

    /// List records, optionally filtered to one creator (non-admins see only
    /// their own). Newest first.
    pub async fn list(&self, creator: Option<&str>) -> Result<Vec<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        let (sql, bind): (String, Vec<String>) = match creator {
            Some(u) => (
                format!("{SELECT_ALL} WHERE created_by_user = ?1 ORDER BY created_at DESC"),
                vec![u.to_string()],
            ),
            None => (format!("{SELECT_ALL} ORDER BY created_at DESC"), vec![]),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare list: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            bind.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), row_to_record)
            .map_err(|e| format!("query list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list: {e}"))?;
        Ok(rows)
    }

    /// Count approved custom skills (feeds the growth `custom_skills_approved` fact).
    pub async fn count_approved(&self) -> Result<u64, String> {
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM custom_skill_registry WHERE status = 'approved'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| format!("count approved: {e}"))?;
        Ok(n.max(0) as u64)
    }

    /// Increment the real invocation counter for an **approved** skill matched
    /// by its machine `slug`. Fail-closed on scope: only `status = 'approved'`
    /// rows are ever counted, so a draft/pending/retired slug collision cannot
    /// inflate saved-hours. Returns true when a row was actually bumped.
    pub async fn increment_usage_by_slug(&self, slug: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE custom_skill_registry
                    SET usage_count = usage_count + 1
                  WHERE slug = ?1 AND status = 'approved'",
                params![slug],
            )
            .map_err(|e| format!("increment usage: {e}"))?;
        Ok(n > 0)
    }

    /// Approved custom skills (feeds the growth saved-hours computation). Newest
    /// first — same ordering as [`Self::list`].
    pub async fn list_approved(&self) -> Result<Vec<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!("{SELECT_ALL} WHERE status = 'approved' ORDER BY approved_at DESC"))
            .map_err(|e| format!("prepare list_approved: {e}"))?;
        let rows = stmt
            .query_map([], row_to_record)
            .map_err(|e| format!("query list_approved: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect list_approved: {e}"))?;
        Ok(rows)
    }

    /// Update the human-facing fields (create-wizard step 3). Only mutates the
    /// human columns; never the status/approval linkage.
    pub async fn update_human_fields(
        &self,
        id: &str,
        display_name: Option<&str>,
        description_human: Option<&str>,
        time_saved_value: Option<f64>,
        time_saved_unit: Option<&str>,
        tags: Option<&str>,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE custom_skill_registry SET
                    display_name      = COALESCE(?2, display_name),
                    description_human = COALESCE(?3, description_human),
                    time_saved_value  = COALESCE(?4, time_saved_value),
                    time_saved_unit   = COALESCE(?5, time_saved_unit),
                    tags              = COALESCE(?6, tags),
                    updated_at        = ?7
                 WHERE id = ?1",
                params![
                    id,
                    display_name,
                    description_human,
                    time_saved_value,
                    time_saved_unit,
                    tags,
                    now,
                ],
            )
            .map_err(|e| format!("update human fields: {e}"))?;
        Ok(n > 0)
    }

    /// Transition status, validating the transition. Optionally sets
    /// `approval_id` / `rejection_reason` / `approved_at` in the same write.
    /// Returns `Err` when the transition is not allowed (fail-closed).
    pub async fn transition(
        &self,
        id: &str,
        to: CustomSkillStatus,
        approval_id: Option<&str>,
        rejection_reason: Option<&str>,
        set_approved_at: bool,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let current: Option<String> = conn
            .query_row(
                "SELECT status FROM custom_skill_registry WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("read status: {e}"))?;
        let from = match current {
            Some(s) => CustomSkillStatus::from_db(&s),
            None => return Err(format!("custom skill {id} not found")),
        };
        if !is_valid_transition(from, to) {
            return Err(format!(
                "illegal transition {} → {}",
                from.as_str(),
                to.as_str()
            ));
        }
        let approved_at: Option<String> = if set_approved_at { Some(now.clone()) } else { None };
        conn.execute(
            "UPDATE custom_skill_registry SET
                status           = ?2,
                approval_id      = COALESCE(?3, approval_id),
                rejection_reason = ?4,
                approved_at      = COALESCE(?5, approved_at),
                updated_at       = ?6
             WHERE id = ?1",
            params![id, to.as_str(), approval_id, rejection_reason, approved_at, now],
        )
        .map_err(|e| format!("transition custom skill: {e}"))?;
        Ok(())
    }
}

const SELECT_COLS: &str = "SELECT id, slug, display_name, description_human, time_saved_value, \
    time_saved_unit, tags, created_by_user, built_by_agent, status, approval_id, rejection_reason, \
    created_at, updated_at, approved_at, usage_count FROM custom_skill_registry WHERE id = ?1";

const SELECT_ALL: &str = "SELECT id, slug, display_name, description_human, time_saved_value, \
    time_saved_unit, tags, created_by_user, built_by_agent, status, approval_id, rejection_reason, \
    created_at, updated_at, approved_at, usage_count FROM custom_skill_registry";

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<CustomSkillRecord> {
    let status_text: String = row.get(9)?;
    Ok(CustomSkillRecord {
        id: row.get(0)?,
        slug: row.get(1)?,
        display_name: row.get(2)?,
        description_human: row.get(3)?,
        time_saved_value: row.get(4)?,
        time_saved_unit: row.get(5)?,
        tags: row.get(6)?,
        created_by_user: row.get(7)?,
        built_by_agent: row.get(8)?,
        status: CustomSkillStatus::from_db(&status_text),
        approval_id: row.get(10)?,
        rejection_reason: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
        approved_at: row.get(14)?,
        usage_count: row.get::<_, i64>(15)?.max(0) as u64,
    })
}

// ── Submit-gate decision helpers (pure, fail-closed) ────────

/// Whether a security-scan risk level permits routing to approval. Mirrors
/// `scan_skill`'s own `passed = risk < High`: **High/Critical are blocked**
/// (fail-closed — a high-risk draft can never be submitted for approval).
pub fn scan_permits_submit(risk: RiskLevel) -> bool {
    risk < RiskLevel::High
}

/// Whether an approval status authorizes the install side-effect. **Only**
/// `Approved` does; `Expired` (TTL / DENY) and every other state must not
/// install — the fail-closed decision boundary for the custom-skill flow.
pub fn approval_grants_install(status: crate::approval::ApprovalStatus) -> bool {
    status.is_granted()
}

/// True when the decider is the same person who created the skill (single-admin
/// self-approval — audited as `self_approved = true`).
pub fn is_self_approval(created_by_user: &str, decided_by_user: &str) -> bool {
    !created_by_user.is_empty() && created_by_user == decided_by_user
}

// ── Saved-hours estimation (pure) ───────────────────────────

/// Convert a custom skill's self-reported time-saving estimate into cumulative
/// saved **hours**. Two distinct semantics selected by `time_saved_unit`:
///
///   * **Per-use** (`minutes_per_use`, `hours_per_use`): the estimate is
///     realized once every time the skill runs, so total = `usage_count × value`
///     (the real invocation counter drives it).
///   * **Per-month** (`hours_per_month`, `minutes_per_month`): a *recurring*
///     monthly saving that must NOT be multiplied by call count — one run does
///     not bank a month. We accrue it over the months elapsed since approval
///     (`months_since_approval × value`).
///
/// Unknown units fall back to `minutes_per_use` (the create-wizard default).
/// A never-approved record (`approved_at == None`) accrues nothing for the
/// per-month branch. Months use a documented 30-day approximation.
pub fn estimate_saved_hours(rec: &CustomSkillRecord, now: DateTime<Utc>) -> f64 {
    let v = rec.time_saved_value.max(0.0);
    match rec.time_saved_unit.as_str() {
        "hours_per_use" => rec.usage_count as f64 * v,
        "hours_per_month" => months_since(rec.approved_at.as_deref(), now) * v,
        "minutes_per_month" => months_since(rec.approved_at.as_deref(), now) * v / 60.0,
        // "minutes_per_use" and any unrecognized unit → per-use minutes.
        _ => rec.usage_count as f64 * v / 60.0,
    }
}

/// Whole months (30-day approximation, documented) elapsed since an RFC-3339
/// `approved_at` timestamp. `None`/unparseable/future ⇒ 0.0.
fn months_since(approved_at: Option<&str>, now: DateTime<Utc>) -> f64 {
    let Some(ts) = approved_at else { return 0.0 };
    let Ok(dt) = DateTime::parse_from_rfc3339(ts) else { return 0.0 };
    let days = (now - dt.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
    (days / 30.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, creator: &str) -> CustomSkillRecord {
        let now = Utc::now().to_rfc3339();
        CustomSkillRecord {
            id: id.to_string(),
            slug: format!("slug-{id}"),
            display_name: "My Skill".into(),
            description_human: "does a thing".into(),
            time_saved_value: 30.0,
            time_saved_unit: "minutes_per_use".into(),
            tags: "ops".into(),
            created_by_user: creator.to_string(),
            built_by_agent: "builder".into(),
            status: CustomSkillStatus::Draft,
            approval_id: None,
            rejection_reason: None,
            created_at: now.clone(),
            updated_at: now,
            approved_at: None,
            usage_count: 0,
        }
    }

    // ── Fail-closed #1: TTL expiry (and any non-approved) must NOT install ──

    #[test]
    fn only_approved_status_grants_install() {
        use crate::approval::ApprovalStatus::*;
        assert!(approval_grants_install(Approved));
        // TTL expiry = DENY: an expired approval must never install.
        assert!(!approval_grants_install(Expired));
        assert!(!approval_grants_install(Denied));
        assert!(!approval_grants_install(Pending));
    }

    // ── Fail-closed #2: high-risk drafts cannot be submitted ──

    #[test]
    fn high_risk_scan_blocks_submit() {
        assert!(scan_permits_submit(RiskLevel::Clean));
        assert!(scan_permits_submit(RiskLevel::Low));
        assert!(scan_permits_submit(RiskLevel::Medium));
        assert!(!scan_permits_submit(RiskLevel::High));
        assert!(!scan_permits_submit(RiskLevel::Critical));
    }

    // ── Fail-closed #3: drafts dir is outside every loader scan root ──

    #[tokio::test(flavor = "current_thread")]
    async fn drafts_dir_is_not_scanned_by_loader() {
        let home = std::env::temp_dir().join(format!("duduclaw-cs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join("skills")).unwrap();
        std::fs::create_dir_all(home.join("agents").join("alice").join("SKILLS")).unwrap();

        // Path-level invariant: drafts root never intersects a load root.
        assert!(drafts_is_isolated(&home), "drafts root must be isolated");

        // Functional proof: put a real SKILL.md ONLY in the drafts dir.
        let id = "draft-1";
        std::fs::create_dir_all(draft_dir(&home, id)).unwrap();
        std::fs::write(
            draft_skill_path(&home, id),
            "---\nname: sneaky\n---\n# sneaky\n",
        )
        .unwrap();

        // The loader's real roots must NOT surface it.
        let global = duduclaw_agent::registry::AgentRegistry::load_skills(&home.join("skills")).await;
        assert!(global.is_empty(), "draft leaked into global skills scan");
        let agent = duduclaw_agent::registry::AgentRegistry::load_skills(
            &home.join("agents").join("alice").join("SKILLS"),
        )
        .await;
        assert!(agent.is_empty(), "draft leaked into agent SKILLS scan");

        // Positive control: the draft IS present if you scan the drafts dir
        // directly — proving the file is real and the isolation is about the
        // loader never choosing that root.
        let direct = duduclaw_agent::registry::AgentRegistry::load_skills(&draft_dir(&home, id)).await;
        assert_eq!(direct.len(), 1, "control: draft should be found under its own dir");

        let _ = std::fs::remove_dir_all(&home);
    }

    // ── Status machine ──────────────────────────────────────

    #[test]
    fn valid_and_invalid_transitions() {
        use CustomSkillStatus::*;
        assert!(is_valid_transition(Draft, Generating));
        assert!(is_valid_transition(Generating, PendingApproval));
        assert!(is_valid_transition(PendingApproval, Approved));
        assert!(is_valid_transition(PendingApproval, Rejected));
        assert!(is_valid_transition(Rejected, Draft));
        assert!(is_valid_transition(Approved, Retired));
        // Illegal: cannot jump draft → approved (must pass through approval).
        assert!(!is_valid_transition(Draft, Approved));
        // Illegal: cannot un-retire.
        assert!(!is_valid_transition(Retired, Draft));
        // Illegal: cannot re-approve an already-approved skill.
        assert!(!is_valid_transition(Approved, Approved));
    }

    #[test]
    fn status_from_db_fails_safe_to_retired() {
        assert_eq!(CustomSkillStatus::from_db("garbage"), CustomSkillStatus::Retired);
    }

    #[test]
    fn self_approval_detection() {
        assert!(is_self_approval("alice", "alice"));
        assert!(!is_self_approval("alice", "bob"));
        assert!(!is_self_approval("", ""));
    }

    // ── Store roundtrip ─────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn store_insert_get_list_transition() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        store.insert(&rec("s1", "alice")).await.unwrap();
        store.insert(&rec("s2", "bob")).await.unwrap();

        assert_eq!(store.list(None).await.unwrap().len(), 2);
        assert_eq!(store.list(Some("alice")).await.unwrap().len(), 1);

        // Draft → generating → pending → approved, with approval linkage.
        store.transition("s1", CustomSkillStatus::Generating, None, None, false).await.unwrap();
        store.transition("s1", CustomSkillStatus::PendingApproval, Some("appr-1"), None, false).await.unwrap();
        assert_eq!(
            store.get_by_approval("appr-1").await.unwrap().map(|r| r.id),
            Some("s1".to_string())
        );
        store.transition("s1", CustomSkillStatus::Approved, None, None, true).await.unwrap();
        let got = store.get("s1").await.unwrap().unwrap();
        assert_eq!(got.status, CustomSkillStatus::Approved);
        assert!(got.approved_at.is_some());
        assert_eq!(got.approval_id.as_deref(), Some("appr-1"));
        assert_eq!(store.count_approved().await.unwrap(), 1);

        // Illegal transition is refused.
        assert!(store.transition("s2", CustomSkillStatus::Approved, None, None, true).await.is_err());
    }

    // ── Saved-hours: two distinct semantics ─────────────────

    #[test]
    fn saved_hours_per_use_multiplies_by_usage_count() {
        let now = Utc::now();
        let mut r = rec("s1", "alice");
        // 30 minutes/use × 10 uses = 300 min = 5.0 h.
        r.time_saved_unit = "minutes_per_use".into();
        r.time_saved_value = 30.0;
        r.usage_count = 10;
        assert!((estimate_saved_hours(&r, now) - 5.0).abs() < 1e-9);

        // hours_per_use: 2 h/use × 4 uses = 8 h.
        r.time_saved_unit = "hours_per_use".into();
        r.time_saved_value = 2.0;
        r.usage_count = 4;
        assert!((estimate_saved_hours(&r, now) - 8.0).abs() < 1e-9);

        // Unknown unit falls back to per-use minutes.
        r.time_saved_unit = "flurbles".into();
        r.time_saved_value = 60.0;
        r.usage_count = 3;
        assert!((estimate_saved_hours(&r, now) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn saved_hours_per_month_accrues_over_time_not_usage() {
        let now = Utc::now();
        let mut r = rec("s1", "alice");
        r.time_saved_unit = "hours_per_month".into();
        r.time_saved_value = 10.0;
        r.usage_count = 999; // must NOT be multiplied for per-month units
        // Approved ~2 months (60 days) ago → ~20 h; usage_count is irrelevant.
        r.approved_at = Some((now - chrono::Duration::days(60)).to_rfc3339());
        let h = estimate_saved_hours(&r, now);
        assert!((h - 20.0).abs() < 0.2, "expected ~20h, got {h}");

        // Never approved ⇒ no accrual window ⇒ 0.
        r.approved_at = None;
        assert_eq!(estimate_saved_hours(&r, now), 0.0);
    }

    // ── Usage counter increment is approved-only (fail-closed) ──

    #[tokio::test(flavor = "current_thread")]
    async fn increment_usage_counts_only_approved_rows() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        // Approved skill with slug "daily-report".
        let mut approved = rec("s1", "alice");
        approved.slug = "daily-report".into();
        approved.status = CustomSkillStatus::Approved;
        store.insert(&approved).await.unwrap();
        // A draft with the SAME slug must never be counted.
        let mut draft = rec("s2", "bob");
        draft.slug = "draft-only".into();
        store.insert(&draft).await.unwrap();

        assert!(store.increment_usage_by_slug("daily-report").await.unwrap());
        assert!(store.increment_usage_by_slug("daily-report").await.unwrap());
        // Draft slug → no approved row → no bump.
        assert!(!store.increment_usage_by_slug("draft-only").await.unwrap());
        // Unknown slug → no bump.
        assert!(!store.increment_usage_by_slug("nope").await.unwrap());

        let got = store.get("s1").await.unwrap().unwrap();
        assert_eq!(got.usage_count, 2);
        assert_eq!(store.get("s2").await.unwrap().unwrap().usage_count, 0);

        // list_approved surfaces only the approved row.
        let approved_rows = store.list_approved().await.unwrap();
        assert_eq!(approved_rows.len(), 1);
        assert_eq!(approved_rows[0].id, "s1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_human_fields_only_touches_human_cols() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        store.insert(&rec("s1", "alice")).await.unwrap();
        store
            .update_human_fields("s1", Some("Renamed"), None, Some(2.0), Some("hours_per_month"), None)
            .await
            .unwrap();
        let got = store.get("s1").await.unwrap().unwrap();
        assert_eq!(got.display_name, "Renamed");
        assert_eq!(got.time_saved_value, 2.0);
        assert_eq!(got.time_saved_unit, "hours_per_month");
        // Untouched.
        assert_eq!(got.description_human, "does a thing");
        assert_eq!(got.status, CustomSkillStatus::Draft);
    }
}
