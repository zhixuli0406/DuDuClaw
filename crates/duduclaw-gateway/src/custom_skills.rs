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
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;

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

/// Per-attempt draft directory: `<home>/skills-drafts/<id>/<message_id>/`.
/// Each generation attempt writes to its own path so output from a superseded
/// (late / re-queued) attempt can never be mistaken for the current one.
/// The message id is filtered to `[A-Za-z0-9_-]` so it can't escape the dir.
pub fn draft_attempt_dir(home_dir: &Path, id: &str, message_id: &str) -> PathBuf {
    let safe: String = message_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    draft_dir(home_dir, id).join(safe)
}

/// The `SKILL.md` path for one generation attempt.
pub fn draft_attempt_skill_path(home_dir: &Path, id: &str, message_id: &str) -> PathBuf {
    draft_attempt_dir(home_dir, id, message_id).join("SKILL.md")
}

/// The draft path that counts for `rec` right now: the current attempt's file
/// when a generation was started, else the legacy un-suffixed path (drafts
/// written directly, e.g. by recording distillation).
pub fn current_draft_path(home_dir: &Path, rec: &CustomSkillRecord) -> PathBuf {
    match rec.generation_message_id.as_deref() {
        Some(m) if !m.is_empty() => draft_attempt_skill_path(home_dir, &rec.id, m),
        _ => draft_skill_path(home_dir, &rec.id),
    }
}

/// True when `path` is a non-empty file.
pub fn file_nonempty(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
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
    loader_scan_roots(home_dir)
        .iter()
        .all(|root| !is_within(&drafts, root) && !is_within(root, &drafts))
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
            | (Generating, Draft)           // generation finished / failed / re-draft
            | (Generating, Generating)      // regenerate after a stalled attempt
            | (Rejected, Generating)        // regenerate straight from a rejection
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
    ///
    /// G5 curator note: the table also carries a `last_used_at` column stamped
    /// by [`CustomSkillStore::increment_usage_by_slug`]. It is deliberately NOT
    /// a struct field (existing literal constructors live outside this module);
    /// read it via [`CustomSkillStore::registry_last_used_map`].
    pub usage_count: u64,
    /// Bus message id of the in-flight (or last) generation request. Lets the
    /// dispatcher map a failed agent reply back to this row.
    pub generation_message_id: Option<String>,
    /// RFC-3339 time the current generation attempt was queued.
    pub generation_started_at: Option<String>,
    /// Machine-readable reason code the last generation attempt failed (see the
    /// `GEN_ERR_*` constants / `dispatch:<label>`); the UI maps it to localized
    /// text. Cleared when a new attempt starts. A `draft` row with this set =
    /// "failed".
    pub generation_error: Option<String>,
}

/// Failure code: the attempt exceeded [`GENERATION_TIMEOUT_SECS`].
pub const GEN_ERR_TIMEOUT: &str = "timeout";
/// Failure code: the agent finished but wrote no draft.
pub const GEN_ERR_NO_DRAFT: &str = "no_draft";
/// Error code: a fresh generation is already running for this skill.
pub const GEN_ERR_IN_PROGRESS: &str = "in_progress";
/// Failure code: the request could not be queued.
pub const GEN_ERR_ENQUEUE: &str = "dispatch:enqueue";

/// Seconds since the current attempt was queued (server clock); `None` unless
/// the record is `generating` with a parseable start time.
pub fn generation_age_secs(rec: &CustomSkillRecord, now: DateTime<Utc>) -> Option<i64> {
    if rec.status != CustomSkillStatus::Generating {
        return None;
    }
    let ts = rec
        .generation_started_at
        .as_deref()
        .unwrap_or(rec.updated_at.as_str());
    age_secs(ts, now).map(|a| a.max(0))
}

/// Whether the server would accept a (re)generate request right now.
pub fn can_regenerate(rec: &CustomSkillRecord, now: DateTime<Utc>) -> bool {
    if !is_valid_transition(rec.status, CustomSkillStatus::Generating) {
        return false;
    }
    rec.status != CustomSkillStatus::Generating
        || generation_older_than(rec, GENERATION_STALE_SECS, now)
}

/// A generation attempt still `generating` after this long is declared failed.
pub const GENERATION_TIMEOUT_SECS: i64 = 10 * 60;
/// Regenerate is allowed while `generating` once the attempt is this old.
pub const GENERATION_STALE_SECS: i64 = 3 * 60;
/// Max stored length of `generation_error` (chars).
pub const GENERATION_ERROR_MAX_CHARS: usize = 500;

/// Collapse whitespace/control chars and cap length so an agent/CLI error is
/// safe to store and show to an end user.
pub fn sanitize_generation_error(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    duduclaw_core::truncate_chars(&cleaned, GENERATION_ERROR_MAX_CHARS)
}

/// Age in seconds of an RFC-3339 timestamp; `None` when unparseable.
fn age_secs(ts: &str, now: DateTime<Utc>) -> Option<i64> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| (now - dt.with_timezone(&Utc)).num_seconds())
}

/// True when a `generating` record has been waiting longer than `limit_secs`.
/// Legacy rows without `generation_started_at` fall back to `updated_at`.
pub fn generation_older_than(rec: &CustomSkillRecord, limit_secs: i64, now: DateTime<Utc>) -> bool {
    if rec.status != CustomSkillStatus::Generating {
        return false;
    }
    let ts = rec
        .generation_started_at
        .as_deref()
        .unwrap_or(rec.updated_at.as_str());
    age_secs(ts, now).map(|a| a >= limit_secs).unwrap_or(false)
}

/// Decide whether a finished dispatch means generation failed for `rec`.
///
/// Pure matching function used by the dispatcher: only dashboard-origin
/// replies to the exact message id stored on a still-`generating` row count.
/// `outcome`'s `Err` carries an already-classified reason *code*
/// (`dispatch:<label>`), never raw agent/CLI text. Returns the failure code, or
/// `None` when it is not ours / fine.
pub fn generation_failure_reason(
    rec: &CustomSkillRecord,
    origin_agent: Option<&str>,
    in_reply_to: &str,
    outcome: Result<(), &str>,
    draft_nonempty: bool,
) -> Option<String> {
    if origin_agent != Some("dashboard")
        || rec.status != CustomSkillStatus::Generating
        || rec.generation_message_id.as_deref() != Some(in_reply_to)
    {
        return None;
    }
    match outcome {
        Err(e) => Some(sanitize_generation_error(e)),
        Ok(()) if !draft_nonempty => Some(GEN_ERR_NO_DRAFT.to_string()),
        Ok(()) => None,
    }
}

// ── Curation (G5 curator lifecycle) ─────────────────────────

/// Curation status of one installed skill file. Distinct from
/// [`CustomSkillStatus`] (which is the *creation* approval lifecycle): every
/// skill file in a loader scan root gets a curation row, custom or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurationStatus {
    /// Normal — loadable, recently used or too new to judge.
    Active,
    /// Unused past the stale window — flagged, still loadable.
    Stale,
    /// Unused past the archive window — file moved to `<home>/skills-archive/`
    /// (excluded from loader scan roots, recoverable via pin/restore).
    Archived,
    /// The curator could not locate the on-disk artifact (e.g. a nested
    /// `group/<name>/SKILL.md` layout it cannot manage). Flagged once, then
    /// exempt from scheduled transitions — never archived, never re-reported
    /// as an error on every pass.
    Unmanaged,
}

impl CurationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CurationStatus::Active => "active",
            CurationStatus::Stale => "stale",
            CurationStatus::Archived => "archived",
            CurationStatus::Unmanaged => "unmanaged",
        }
    }

    /// Parse from the DB text column. Unknown ⇒ `Stale` (fail-safe: visible
    /// flag, but never triggers a destructive file move by itself).
    pub fn from_db(s: &str) -> Self {
        match s {
            "active" => CurationStatus::Active,
            "archived" => CurationStatus::Archived,
            "unmanaged" => CurationStatus::Unmanaged,
            _ => CurationStatus::Stale,
        }
    }
}

/// One row of `skill_curation`, keyed by `(skill_name, scope)` where scope is
/// `"global"` or `"agent:<id>"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCurationRecord {
    pub skill_name: String,
    pub scope: String,
    /// When the curator first saw this skill file (RFC-3339). The stale/archive
    /// clock starts here when the skill has never been used.
    pub first_seen: String,
    /// Last real invocation (RFC-3339), stamped by the usage-increment path.
    pub last_used_at: Option<String>,
    /// Pinned skills are exempt from both stale marking and archiving.
    pub pinned: bool,
    pub status: CurationStatus,
    /// Where the file was moved on archive (for recovery).
    pub archived_path: Option<String>,
    pub updated_at: String,
}

// ── Store ───────────────────────────────────────────────────

/// `PRAGMA user_version` stamped once `init_schema` has fully run.
const SCHEMA_USER_VERSION: i64 = 1;

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
        static INITIALIZED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
            std::sync::OnceLock::new();
        let db_path = home_dir.join("custom_skills.db");
        // Schema init is once per process per path — but only trusted while the
        // file still exists (a deleted db must be re-initialised).
        let existed = db_path.exists();
        let conn =
            Connection::open(&db_path).map_err(|e| format!("open custom skills store: {e}"))?;
        let seen = INITIALIZED.get_or_init(Default::default);
        let already = existed
            && seen
                .lock()
                .map(|s| s.contains(&db_path))
                .unwrap_or(false);
        // A cache hit is only trusted while the file carries our schema stamp:
        // a replaced/restored db file reads back a different `user_version`.
        let already = already
            && conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .map(|v| v == SCHEMA_USER_VERSION)
                .unwrap_or(false);
        if already {
            // Per-connection setting; journal_mode=WAL persists in the file.
            conn.busy_timeout(std::time::Duration::from_millis(5000))
                .map_err(|e| format!("custom skills busy_timeout: {e}"))?;
        } else {
            Self::init_schema(&conn)?;
            if let Ok(mut s) = seen.lock() {
                s.insert(db_path.clone());
            }
            debug!(?db_path, "CustomSkillStore initialized");
        }
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

        // Idempotent migrations: only ALTER when the column is missing
        // (PRAGMA table_info), so an up-to-date db issues no failing DDL.
        //   - `usage_count` (L5 §14 — per-skill invocation counter)
        //   - `last_used_at` (G5 curator — last-invocation timestamp)
        //   - `generation_*` (RM-087 — generation failure tracking)
        let existing: std::collections::HashSet<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(custom_skill_registry)")
                .map_err(|e| format!("read custom_skill_registry columns: {e}"))?;
            stmt.query_map([], |r| r.get::<_, String>(1))
                .map_err(|e| format!("read custom_skill_registry columns: {e}"))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("read custom_skill_registry columns: {e}"))?
        };
        for (col, ddl) in [
            ("usage_count", "ALTER TABLE custom_skill_registry ADD COLUMN usage_count INTEGER NOT NULL DEFAULT 0"),
            ("last_used_at", "ALTER TABLE custom_skill_registry ADD COLUMN last_used_at TEXT"),
            ("generation_message_id", "ALTER TABLE custom_skill_registry ADD COLUMN generation_message_id TEXT"),
            ("generation_started_at", "ALTER TABLE custom_skill_registry ADD COLUMN generation_started_at TEXT"),
            ("generation_error", "ALTER TABLE custom_skill_registry ADD COLUMN generation_error TEXT"),
        ] {
            if existing.contains(col) {
                continue;
            }
            if let Err(e) = conn.execute(ddl, []) {
                let msg = e.to_string();
                // Lost a race with another process adding it — fine.
                if !msg.contains("duplicate column name") {
                    return Err(format!("migrate custom_skill_registry: {msg}"));
                }
            }
        }

        // G5 curator tables (CREATE IF NOT EXISTS ⇒ idempotent).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS skill_curation (
                 skill_name    TEXT NOT NULL,
                 scope         TEXT NOT NULL,
                 first_seen    TEXT NOT NULL,
                 last_used_at  TEXT,
                 pinned        INTEGER NOT NULL DEFAULT 0,
                 status        TEXT NOT NULL DEFAULT 'active',
                 archived_path TEXT,
                 updated_at    TEXT NOT NULL,
                 PRIMARY KEY (skill_name, scope)
             );
             CREATE INDEX IF NOT EXISTS idx_skill_curation_status ON skill_curation(status);

             CREATE TABLE IF NOT EXISTS hub_skill_signatures (
                 skill_name TEXT NOT NULL,
                 scope TEXT NOT NULL,
                 signature_status TEXT NOT NULL,
                 content_hash TEXT NOT NULL,
                 PRIMARY KEY (skill_name, scope)
             );

             CREATE TABLE IF NOT EXISTS curator_meta (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )
        .map_err(|e| format!("init skill curation schema: {e}"))?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_USER_VERSION};"))
            .map_err(|e| format!("stamp custom skills schema version: {e}"))?;
        Ok(())
    }

    pub async fn insert(&self, rec: &CustomSkillRecord) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO custom_skill_registry
                (id, slug, display_name, description_human, time_saved_value, time_saved_unit,
                 tags, created_by_user, built_by_agent, status, approval_id, rejection_reason,
                 created_at, updated_at, approved_at, usage_count,
                 generation_message_id, generation_started_at, generation_error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
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
                rec.generation_message_id,
                rec.generation_started_at,
                rec.generation_error,
            ],
        )
        .map_err(|e| format!("insert custom skill: {e}"))?;
        Ok(())
    }

    /// Look up a record by the bus message id of its generation request.
    pub async fn get_by_generation_message(
        &self,
        message_id: &str,
    ) -> Result<Option<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &SELECT_ALL.replace(
                "FROM custom_skill_registry",
                "FROM custom_skill_registry WHERE generation_message_id = ?1",
            ),
            params![message_id],
            row_to_record,
        )
        .optional()
        .map_err(|e| format!("get custom skill by generation message: {e}"))
    }

    /// Start (or restart) a generation attempt as ONE conditional UPDATE
    /// (compare-and-swap), so concurrent requests cannot both win: it succeeds
    /// only from `draft`/`rejected`, or from `generating` when the running
    /// attempt started before `stale_cutoff` (RFC-3339) — i.e. is stale.
    /// Stores the queued message id + start time and clears any previous error.
    ///
    /// `Err(GEN_ERR_IN_PROGRESS)` when a fresh attempt is running; other `Err`s
    /// for a missing row / illegal source status.
    pub async fn begin_generation(
        &self,
        id: &str,
        message_id: &str,
        started_at: &str,
        stale_cutoff: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE custom_skill_registry SET
                    status = 'generating',
                    rejection_reason = NULL,
                    generation_message_id = ?2,
                    generation_started_at = ?3,
                    generation_error = NULL,
                    updated_at = ?3
                 WHERE id = ?1
                   AND status IN ('draft', 'rejected', 'generating')
                   AND (status <> 'generating'
                        OR generation_started_at IS NULL
                        OR julianday(generation_started_at) IS NULL
                        OR julianday(generation_started_at) < julianday(?4))",
                params![id, message_id, started_at, stale_cutoff],
            )
            .map_err(|e| format!("begin generation: {e}"))?;
        if n > 0 {
            return Ok(());
        }
        // CAS lost — report why.
        let current: Option<String> = conn
            .query_row(
                "SELECT status FROM custom_skill_registry WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("read status: {e}"))?;
        match current.map(|s| CustomSkillStatus::from_db(&s)) {
            None => Err(format!("custom skill {id} not found")),
            Some(CustomSkillStatus::Generating) => Err(GEN_ERR_IN_PROGRESS.to_string()),
            Some(from) => Err(format!("cannot generate from status {}", from.as_str())),
        }
    }

    /// Record a generation failure for ONE specific attempt: only acts on a
    /// `generating` row whose `generation_message_id` is still
    /// `expected_message_id` (so a late timeout / dispatcher settle for an old
    /// attempt cannot fail a newer one). Stores the reason code and returns the
    /// row to `draft` (`draft` + `generation_error` = "failed" in the UI).
    /// Returns true when a row was changed.
    pub async fn fail_generation(
        &self,
        id: &str,
        expected_message_id: Option<&str>,
        reason: &str,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let reason = sanitize_generation_error(reason);
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE custom_skill_registry SET
                    status = 'draft',
                    generation_error = ?2,
                    updated_at = ?3
                 WHERE id = ?1 AND status = 'generating'
                   AND generation_message_id IS ?4",
                params![id, reason, now, expected_message_id],
            )
            .map_err(|e| format!("fail generation: {e}"))?;
        Ok(n > 0)
    }

    /// Clear a recorded failure because the attempt `expected_message_id`
    /// actually completed late (its draft file exists). Only acts on a `draft`
    /// row whose current attempt is that message. Returns true when changed.
    pub async fn clear_generation_error(
        &self,
        id: &str,
        expected_message_id: &str,
    ) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE custom_skill_registry SET generation_error = NULL, updated_at = ?3
                 WHERE id = ?1 AND status = 'draft' AND generation_error IS NOT NULL
                   AND generation_message_id = ?2",
                params![id, expected_message_id, now],
            )
            .map_err(|e| format!("clear generation error: {e}"))?;
        Ok(n > 0)
    }

    pub async fn get(&self, id: &str) -> Result<Option<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(SELECT_COLS, params![id], row_to_record)
            .optional()
            .map_err(|e| format!("get custom skill: {e}"))
    }

    /// Look up a record by its linked approval id (for the decide side-effect).
    pub async fn get_by_approval(
        &self,
        approval_id: &str,
    ) -> Result<Option<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, slug, display_name, description_human, time_saved_value, time_saved_unit,
                    tags, created_by_user, built_by_agent, status, approval_id, rejection_reason,
                    created_at, updated_at, approved_at, usage_count,
                    generation_message_id, generation_started_at, generation_error
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
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| format!("prepare list: {e}"))?;
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
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
    ///
    /// G5 curator: the same call stamps `last_used_at` on the registry row and
    /// on every `skill_curation` row with this skill name (any scope, any
    /// approval status — "used" is a fact about the file, not about approval).
    /// The return value keeps its original approved-row semantics.
    pub async fn increment_usage_by_slug(&self, slug: &str) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE custom_skill_registry
                    SET usage_count = usage_count + 1,
                        last_used_at = ?2
                  WHERE slug = ?1 AND status = 'approved'",
                params![slug, now],
            )
            .map_err(|e| format!("increment usage: {e}"))?;
        conn.execute(
            "UPDATE skill_curation SET last_used_at = ?2, updated_at = ?2 WHERE skill_name = ?1",
            params![slug, now],
        )
        .map_err(|e| format!("touch curation usage: {e}"))?;
        Ok(n > 0)
    }

    // ── Curation methods (G5 curator lifecycle) ─────────────

    /// Register a skill file the curator discovered. First sighting inserts an
    /// `active` row with `first_seen = now`; later sightings are no-ops.
    /// Returns true when a new row was created.
    pub async fn curation_upsert_seen(
        &self,
        skill_name: &str,
        scope: &str,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO skill_curation
                    (skill_name, scope, first_seen, pinned, status, updated_at)
                 VALUES (?1, ?2, ?3, 0, 'active', ?3)",
                params![skill_name, scope, now],
            )
            .map_err(|e| format!("curation upsert: {e}"))?;
        Ok(n > 0)
    }

    /// Install-time origin evidence is stored beside the hub's curator metadata.
    /// The content hash prevents a later local edit/reinstall from inheriting a badge.
    pub async fn record_hub_signature(
        &self,
        skill_name: &str,
        scope: &str,
        status: &duduclaw_core::publishers::SignatureStatus,
        content_hash: &str,
    ) -> Result<(), String> {
        let status = serde_json::to_string(status).map_err(|e| e.to_string())?;
        self.conn.lock().await.execute(
            "INSERT OR REPLACE INTO hub_skill_signatures
             (skill_name, scope, signature_status, content_hash) VALUES (?1, ?2, ?3, ?4)",
            params![skill_name, scope, status, content_hash],
        ).map_err(|e| format!("record hub signature: {e}"))?;
        Ok(())
    }

    /// Read origin evidence keyed by the exact layer and installed content hash.
    pub async fn hub_signatures(&self) -> Result<std::collections::HashMap<(String, String), (String, serde_json::Value)>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT skill_name, scope, content_hash, signature_status FROM hub_skill_signatures"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?,
            r.get::<_, String>(2)?, r.get::<_, String>(3)?,
        ))).map_err(|e| e.to_string())?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (name, scope, hash, status) = row.map_err(|e| e.to_string())?;
            if let Ok(status) = serde_json::from_str::<duduclaw_core::publishers::SignatureStatus>(&status) {
                out.insert((scope, name), (hash, serde_json::to_value(status).map_err(|e| e.to_string())?));
            }
        }
        Ok(out)
    }

    /// Full-row upsert — used by tests (synthetic timestamps) and by the
    /// pin/restore path. Replaces any existing `(skill_name, scope)` row.
    pub async fn curation_put(&self, rec: &SkillCurationRecord) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO skill_curation
                (skill_name, scope, first_seen, last_used_at, pinned, status, archived_path, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                rec.skill_name,
                rec.scope,
                rec.first_seen,
                rec.last_used_at,
                rec.pinned as i64,
                rec.status.as_str(),
                rec.archived_path,
                rec.updated_at,
            ],
        )
        .map_err(|e| format!("curation put: {e}"))?;
        Ok(())
    }

    pub async fn curation_get(
        &self,
        skill_name: &str,
        scope: &str,
    ) -> Result<Option<SkillCurationRecord>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            &format!("{CURATION_SELECT} WHERE skill_name = ?1 AND scope = ?2"),
            params![skill_name, scope],
            row_to_curation,
        )
        .optional()
        .map_err(|e| format!("curation get: {e}"))
    }

    /// All curation rows, ordered by scope then name (stable report order).
    pub async fn curation_list(&self) -> Result<Vec<SkillCurationRecord>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!("{CURATION_SELECT} ORDER BY scope, skill_name"))
            .map_err(|e| format!("prepare curation list: {e}"))?;
        let rows = stmt
            .query_map([], row_to_curation)
            .map_err(|e| format!("query curation list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect curation list: {e}"))?;
        Ok(rows)
    }

    /// Set curation status (and archived path when archiving / clearing).
    pub async fn curation_set_status(
        &self,
        skill_name: &str,
        scope: &str,
        status: CurationStatus,
        archived_path: Option<&str>,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE skill_curation
                    SET status = ?3, archived_path = ?4, updated_at = ?5
                  WHERE skill_name = ?1 AND scope = ?2",
                params![skill_name, scope, status.as_str(), archived_path, now],
            )
            .map_err(|e| format!("curation set status: {e}"))?;
        Ok(n > 0)
    }

    /// Toggle the pin flag. Returns true when the row exists.
    pub async fn curation_set_pinned(
        &self,
        skill_name: &str,
        scope: &str,
        pinned: bool,
        now: &str,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE skill_curation SET pinned = ?3, updated_at = ?4
                  WHERE skill_name = ?1 AND scope = ?2",
                params![skill_name, scope, pinned as i64, now],
            )
            .map_err(|e| format!("curation set pinned: {e}"))?;
        Ok(n > 0)
    }

    /// Remove a curation row (skill file vanished outside the curator's
    /// control — nothing left to track).
    pub async fn curation_remove(&self, skill_name: &str, scope: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM skill_curation WHERE skill_name = ?1 AND scope = ?2",
                params![skill_name, scope],
            )
            .map_err(|e| format!("curation remove: {e}"))?;
        Ok(n > 0)
    }

    /// `last_used_at` per approved-registry slug (auxiliary usage signal the
    /// curator merges with the curation rows' own stamps).
    pub async fn registry_last_used_map(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT slug, last_used_at FROM custom_skill_registry WHERE last_used_at IS NOT NULL")
            .map_err(|e| format!("prepare last-used map: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("query last-used map: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect last-used map: {e}"))?;
        Ok(rows.into_iter().collect())
    }

    /// Curator scratch metadata (e.g. `last_run_at`).
    pub async fn meta_get(&self, key: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT value FROM curator_meta WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("meta get: {e}"))
    }

    pub async fn meta_set(&self, key: &str, value: &str) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO curator_meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(|e| format!("meta set: {e}"))?;
        Ok(())
    }

    /// Approved custom skills (feeds the growth saved-hours computation). Newest
    /// first — same ordering as [`Self::list`].
    pub async fn list_approved(&self) -> Result<Vec<CustomSkillRecord>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "{SELECT_ALL} WHERE status = 'approved' ORDER BY approved_at DESC"
            ))
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
        let approved_at: Option<String> = if set_approved_at {
            Some(now.clone())
        } else {
            None
        };
        conn.execute(
            "UPDATE custom_skill_registry SET
                status           = ?2,
                approval_id      = COALESCE(?3, approval_id),
                rejection_reason = ?4,
                approved_at      = COALESCE(?5, approved_at),
                updated_at       = ?6
             WHERE id = ?1",
            params![
                id,
                to.as_str(),
                approval_id,
                rejection_reason,
                approved_at,
                now
            ],
        )
        .map_err(|e| format!("transition custom skill: {e}"))?;
        Ok(())
    }
}

const SELECT_COLS: &str = "SELECT id, slug, display_name, description_human, time_saved_value, \
    time_saved_unit, tags, created_by_user, built_by_agent, status, approval_id, rejection_reason, \
    created_at, updated_at, approved_at, usage_count, generation_message_id, \
    generation_started_at, generation_error FROM custom_skill_registry WHERE id = ?1";

const SELECT_ALL: &str = "SELECT id, slug, display_name, description_human, time_saved_value, \
    time_saved_unit, tags, created_by_user, built_by_agent, status, approval_id, rejection_reason, \
    created_at, updated_at, approved_at, usage_count, generation_message_id, \
    generation_started_at, generation_error FROM custom_skill_registry";

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
        generation_message_id: row.get(16)?,
        generation_started_at: row.get(17)?,
        generation_error: row.get(18)?,
    })
}

const CURATION_SELECT: &str = "SELECT skill_name, scope, first_seen, last_used_at, pinned, \
    status, archived_path, updated_at FROM skill_curation";

fn row_to_curation(row: &rusqlite::Row) -> rusqlite::Result<SkillCurationRecord> {
    let status_text: String = row.get(5)?;
    Ok(SkillCurationRecord {
        skill_name: row.get(0)?,
        scope: row.get(1)?,
        first_seen: row.get(2)?,
        last_used_at: row.get(3)?,
        pinned: row.get::<_, i64>(4)? != 0,
        status: CurationStatus::from_db(&status_text),
        archived_path: row.get(6)?,
        updated_at: row.get(7)?,
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
    let Ok(dt) = DateTime::parse_from_rfc3339(ts) else {
        return 0.0;
    };
    let days = (now - dt.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
    (days / 30.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hub_signature_metadata_is_scoped_and_replaced() {
        use duduclaw_core::publishers::{SignatureState, SignatureStatus};
        let store = CustomSkillStore::open_in_memory().unwrap();
        let verified = SignatureStatus { state: SignatureState::Verified, publisher: Some("fixture".into()) };
        store.record_hub_signature("notes", "global", &verified, "hash-a").await.unwrap();
        store.record_hub_signature("notes", "agent:alice", &SignatureStatus::unsigned(None), "hash-b").await.unwrap();
        let rows = store.hub_signatures().await.unwrap();
        assert_eq!(rows[&("global".into(), "notes".into())].1["state"], "verified");
        assert_eq!(rows[&("agent:alice".into(), "notes".into())].1["state"], "unsigned");
        store.record_hub_signature("notes", "global", &SignatureStatus::unsigned(None), "hash-c").await.unwrap();
        let rows = store.hub_signatures().await.unwrap();
        assert_eq!(rows[&("global".into(), "notes".into())].0, "hash-c");
        assert_eq!(rows[&("global".into(), "notes".into())].1["state"], "unsigned");
    }

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
            generation_message_id: None,
            generation_started_at: None,
            generation_error: None,
        }
    }

    // ── RM-087: generation failure tracking ─────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn generation_columns_roundtrip_and_failure() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        store.insert(&rec("g1", "alice")).await.unwrap();
        let t0 = Utc::now().to_rfc3339();
        // Cutoff far in the past ⇒ a running attempt is never stale here.
        let never = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        store.begin_generation("g1", "msg-1", &t0, &never).await.unwrap();
        let got = store.get("g1").await.unwrap().unwrap();
        assert_eq!(got.status, CustomSkillStatus::Generating);
        assert_eq!(got.generation_message_id.as_deref(), Some("msg-1"));
        assert_eq!(got.generation_started_at.as_deref(), Some(t0.as_str()));
        assert!(got.generation_error.is_none());
        assert_eq!(
            store.get_by_generation_message("msg-1").await.unwrap().unwrap().id,
            "g1"
        );

        assert!(store.fail_generation("g1", Some("msg-1"), "boom\n  bad").await.unwrap());
        let got = store.get("g1").await.unwrap().unwrap();
        assert_eq!(got.status, CustomSkillStatus::Draft);
        assert_eq!(got.generation_error.as_deref(), Some("boom bad"));
        // Only acts on generating rows.
        assert!(!store.fail_generation("g1", Some("msg-1"), "again").await.unwrap());

        // Retry clears the error and may restart from draft.
        store.begin_generation("g1", "msg-2", &t0, &never).await.unwrap();
        assert!(store.get("g1").await.unwrap().unwrap().generation_error.is_none());
        // Illegal source states are refused (error surfaced, not dropped).
        store.insert(&rec("g2", "alice")).await.unwrap();
        store
            .transition("g2", CustomSkillStatus::Retired, None, None, false)
            .await
            .unwrap();
        assert!(store.begin_generation("g2", "m", &t0, &never).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn begin_generation_is_compare_and_swap() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        store.insert(&rec("c1", "alice")).await.unwrap();
        let now = Utc::now();
        let started = now.to_rfc3339();
        let cutoff = (now - chrono::Duration::seconds(GENERATION_STALE_SECS)).to_rfc3339();
        store.begin_generation("c1", "m1", &started, &cutoff).await.unwrap();
        // Second begin inside the stale window loses; the first attempt stays.
        let err = store.begin_generation("c1", "m2", &started, &cutoff).await.unwrap_err();
        assert_eq!(err, GEN_ERR_IN_PROGRESS);
        let got = store.get("c1").await.unwrap().unwrap();
        assert_eq!(got.generation_message_id.as_deref(), Some("m1"));
        // Once the cutoff passes the start time (attempt is stale), it wins.
        let later_cutoff = (now + chrono::Duration::seconds(5)).to_rfc3339();
        let later = (now + chrono::Duration::seconds(6)).to_rfc3339();
        store.begin_generation("c1", "m3", &later, &later_cutoff).await.unwrap();
        assert_eq!(
            store.get("c1").await.unwrap().unwrap().generation_message_id.as_deref(),
            Some("m3")
        );
        // Missing row.
        assert!(store.begin_generation("nope", "m", &started, &cutoff).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fail_generation_ignores_superseded_attempt() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        store.insert(&rec("f1", "alice")).await.unwrap();
        let now = Utc::now();
        let t = now.to_rfc3339();
        let past = (now - chrono::Duration::days(1)).to_rfc3339();
        store.begin_generation("f1", "old", &t, &past).await.unwrap();
        let future = (now + chrono::Duration::seconds(10)).to_rfc3339();
        store.begin_generation("f1", "new", &t, &future).await.unwrap();
        // A late failure for the OLD attempt is a no-op.
        assert!(!store.fail_generation("f1", Some("old"), GEN_ERR_TIMEOUT).await.unwrap());
        let got = store.get("f1").await.unwrap().unwrap();
        assert_eq!(got.status, CustomSkillStatus::Generating);
        assert!(got.generation_error.is_none());
        // The current attempt can be failed, and then cleared when it completes late.
        assert!(store.fail_generation("f1", Some("new"), GEN_ERR_TIMEOUT).await.unwrap());
        assert!(!store.clear_generation_error("f1", "old").await.unwrap());
        assert!(store.clear_generation_error("f1", "new").await.unwrap());
        assert!(store.get("f1").await.unwrap().unwrap().generation_error.is_none());
    }

    #[test]
    fn draft_path_is_per_attempt() {
        let home = Path::new("/h");
        let mut r = rec("p1", "alice");
        assert_eq!(current_draft_path(home, &r), draft_skill_path(home, "p1"));
        r.generation_message_id = Some("m-1".into());
        let a = current_draft_path(home, &r);
        r.generation_message_id = Some("m-2".into());
        let b = current_draft_path(home, &r);
        assert_ne!(a, b);
        assert!(a.starts_with(draft_dir(home, "p1")));
        // Traversal characters in a message id cannot escape the draft dir.
        assert!(draft_attempt_skill_path(home, "p1", "../../x").starts_with(draft_dir(home, "p1")));
    }

    #[test]
    fn age_and_can_regenerate_are_server_computed() {
        let now = Utc::now();
        let mut r = rec("a1", "alice");
        assert!(can_regenerate(&r, now));
        assert_eq!(generation_age_secs(&r, now), None);
        r.status = CustomSkillStatus::Generating;
        r.generation_started_at = Some((now - chrono::Duration::seconds(30)).to_rfc3339());
        assert_eq!(generation_age_secs(&r, now), Some(30));
        assert!(!can_regenerate(&r, now));
        r.generation_started_at = Some((now - chrono::Duration::seconds(GENERATION_STALE_SECS + 1)).to_rfc3339());
        assert!(can_regenerate(&r, now));
        r.status = CustomSkillStatus::Retired;
        assert!(!can_regenerate(&r, now));
    }

    #[test]
    fn open_reinitialises_a_replaced_db_file() {
        let home = std::env::temp_dir().join(format!("duduclaw-cs-swap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let a = CustomSkillStore::open(&home).unwrap();
        rt.block_on(async { a.insert(&rec("s1", "alice")).await.unwrap() });
        drop(a);
        // Replace the file with a pre-RM-087 db (the path stays cached).
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(home.join(format!("custom_skills.db{ext}")));
        }
        {
            let c = Connection::open(home.join("custom_skills.db")).unwrap();
            c.execute_batch(
                "CREATE TABLE custom_skill_registry (id TEXT PRIMARY KEY, slug TEXT NOT NULL,
                 display_name TEXT NOT NULL, description_human TEXT NOT NULL DEFAULT '',
                 time_saved_value REAL NOT NULL DEFAULT 0, time_saved_unit TEXT NOT NULL DEFAULT 'minutes_per_use',
                 tags TEXT NOT NULL DEFAULT '', created_by_user TEXT NOT NULL, built_by_agent TEXT NOT NULL DEFAULT '',
                 status TEXT NOT NULL DEFAULT 'draft', approval_id TEXT, rejection_reason TEXT,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL, approved_at TEXT);",
            )
            .unwrap();
        }
        let b = CustomSkillStore::open(&home).unwrap();
        rt.block_on(async {
            b.insert(&rec("s2", "alice")).await.unwrap();
            assert!(b.get("s2").await.unwrap().is_some());
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn generation_timeout_detection() {
        let now = Utc::now();
        let mut r = rec("t1", "alice");
        r.status = CustomSkillStatus::Generating;
        r.generation_started_at = Some((now - chrono::Duration::minutes(11)).to_rfc3339());
        assert!(generation_older_than(&r, GENERATION_TIMEOUT_SECS, now));
        r.generation_started_at = Some((now - chrono::Duration::minutes(2)).to_rfc3339());
        assert!(!generation_older_than(&r, GENERATION_TIMEOUT_SECS, now));
        assert!(!generation_older_than(&r, GENERATION_STALE_SECS, now));
        // Legacy row: falls back to updated_at.
        r.generation_started_at = None;
        r.updated_at = (now - chrono::Duration::minutes(20)).to_rfc3339();
        assert!(generation_older_than(&r, GENERATION_TIMEOUT_SECS, now));
        // Not generating ⇒ never timed out.
        r.status = CustomSkillStatus::Draft;
        assert!(!generation_older_than(&r, GENERATION_TIMEOUT_SECS, now));
    }

    #[test]
    fn generation_failure_matching() {
        let mut r = rec("m1", "alice");
        r.status = CustomSkillStatus::Generating;
        r.generation_message_id = Some("msg-1".into());
        // Error from dashboard-origin reply to our message ⇒ failure code.
        let reason =
            generation_failure_reason(&r, Some("dashboard"), "msg-1", Err("dispatch:timeout"), false)
                .unwrap();
        assert_eq!(reason, "dispatch:timeout");
        assert_eq!(
            generation_failure_reason(&r, Some("dashboard"), "msg-1", Ok(()), false).as_deref(),
            Some(GEN_ERR_NO_DRAFT)
        );
        // Ok but no draft ⇒ failure; Ok with draft ⇒ none.
        assert!(generation_failure_reason(&r, Some("dashboard"), "msg-1", Ok(()), false).is_some());
        assert!(generation_failure_reason(&r, Some("dashboard"), "msg-1", Ok(()), true).is_none());
        // Wrong message id / origin / status ⇒ not ours.
        assert!(generation_failure_reason(&r, Some("dashboard"), "other", Err("e"), false).is_none());
        assert!(generation_failure_reason(&r, Some("agnes"), "msg-1", Err("e"), false).is_none());
        assert!(generation_failure_reason(&r, None, "msg-1", Err("e"), false).is_none());
        r.status = CustomSkillStatus::Draft;
        assert!(generation_failure_reason(&r, Some("dashboard"), "msg-1", Err("e"), false).is_none());
    }

    #[test]
    fn open_runs_schema_once_and_migrates_old_db() {
        let home = std::env::temp_dir().join(format!("duduclaw-cs-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        // Pre-RM-087 db: old table without any optional columns.
        {
            let c = Connection::open(home.join("custom_skills.db")).unwrap();
            c.execute_batch(
                "CREATE TABLE custom_skill_registry (id TEXT PRIMARY KEY, slug TEXT NOT NULL,
                 display_name TEXT NOT NULL, description_human TEXT NOT NULL DEFAULT '',
                 time_saved_value REAL NOT NULL DEFAULT 0, time_saved_unit TEXT NOT NULL DEFAULT 'minutes_per_use',
                 tags TEXT NOT NULL DEFAULT '', created_by_user TEXT NOT NULL, built_by_agent TEXT NOT NULL DEFAULT '',
                 status TEXT NOT NULL DEFAULT 'draft', approval_id TEXT, rejection_reason TEXT,
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL, approved_at TEXT);",
            )
            .unwrap();
        }
        let _a = CustomSkillStore::open(&home).unwrap();
        // Second open (cached path) must still yield a usable connection.
        let b = CustomSkillStore::open(&home).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            b.insert(&rec("o1", "alice")).await.unwrap();
            assert!(b.get("o1").await.unwrap().unwrap().generation_error.is_none());
        });
        let _ = std::fs::remove_dir_all(&home);
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
        let global =
            duduclaw_agent::registry::AgentRegistry::load_skills(&home.join("skills")).await;
        assert!(global.is_empty(), "draft leaked into global skills scan");
        let agent = duduclaw_agent::registry::AgentRegistry::load_skills(
            &home.join("agents").join("alice").join("SKILLS"),
        )
        .await;
        assert!(agent.is_empty(), "draft leaked into agent SKILLS scan");

        // Positive control: the draft IS present if you scan the drafts dir
        // directly — proving the file is real and the isolation is about the
        // loader never choosing that root.
        let direct =
            duduclaw_agent::registry::AgentRegistry::load_skills(&draft_dir(&home, id)).await;
        assert_eq!(
            direct.len(),
            1,
            "control: draft should be found under its own dir"
        );

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
        assert_eq!(
            CustomSkillStatus::from_db("garbage"),
            CustomSkillStatus::Retired
        );
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
        store
            .transition("s1", CustomSkillStatus::Generating, None, None, false)
            .await
            .unwrap();
        store
            .transition(
                "s1",
                CustomSkillStatus::PendingApproval,
                Some("appr-1"),
                None,
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            store.get_by_approval("appr-1").await.unwrap().map(|r| r.id),
            Some("s1".to_string())
        );
        store
            .transition("s1", CustomSkillStatus::Approved, None, None, true)
            .await
            .unwrap();
        let got = store.get("s1").await.unwrap().unwrap();
        assert_eq!(got.status, CustomSkillStatus::Approved);
        assert!(got.approved_at.is_some());
        assert_eq!(got.approval_id.as_deref(), Some("appr-1"));
        assert_eq!(store.count_approved().await.unwrap(), 1);

        // Illegal transition is refused.
        assert!(
            store
                .transition("s2", CustomSkillStatus::Approved, None, None, true)
                .await
                .is_err()
        );
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

        // G5: the increment stamped last_used_at on the approved slug.
        let map = store.registry_last_used_map().await.unwrap();
        assert!(
            map.contains_key("daily-report"),
            "increment must stamp last_used_at"
        );
        assert!(
            !map.contains_key("draft-only"),
            "unapproved slug must not be stamped"
        );
    }

    // ── G5 curation store ────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn curation_roundtrip_and_usage_touch() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();

        // First sighting inserts; second is a no-op that preserves first_seen.
        assert!(
            store
                .curation_upsert_seen("web-scraper", "global", &now)
                .await
                .unwrap()
        );
        assert!(
            !store
                .curation_upsert_seen("web-scraper", "global", "2030-01-01T00:00:00Z")
                .await
                .unwrap()
        );
        let rec = store
            .curation_get("web-scraper", "global")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.first_seen, now);
        assert_eq!(rec.status, CurationStatus::Active);
        assert!(!rec.pinned);
        assert!(rec.last_used_at.is_none());

        // Usage increment touches curation last_used_at even without an
        // approved registry row (usage is a fact about the file).
        store.increment_usage_by_slug("web-scraper").await.unwrap();
        let rec = store
            .curation_get("web-scraper", "global")
            .await
            .unwrap()
            .unwrap();
        assert!(
            rec.last_used_at.is_some(),
            "usage must stamp curation last_used_at"
        );

        // Pin toggle + status transitions persist.
        assert!(
            store
                .curation_set_pinned("web-scraper", "global", true, &now)
                .await
                .unwrap()
        );
        assert!(
            store
                .curation_set_status("web-scraper", "global", CurationStatus::Stale, None, &now)
                .await
                .unwrap()
        );
        let rec = store
            .curation_get("web-scraper", "global")
            .await
            .unwrap()
            .unwrap();
        assert!(rec.pinned);
        assert_eq!(rec.status, CurationStatus::Stale);

        // Missing row: pin/status return false, get returns None.
        assert!(
            !store
                .curation_set_pinned("nope", "global", true, &now)
                .await
                .unwrap()
        );
        assert!(
            store
                .curation_get("nope", "global")
                .await
                .unwrap()
                .is_none()
        );

        // Remove.
        assert!(
            store
                .curation_remove("web-scraper", "global")
                .await
                .unwrap()
        );
        assert!(
            store
                .curation_get("web-scraper", "global")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn curation_status_from_db_fails_safe_to_stale() {
        assert_eq!(CurationStatus::from_db("active"), CurationStatus::Active);
        assert_eq!(
            CurationStatus::from_db("archived"),
            CurationStatus::Archived
        );
        // Unknown must never map to a state that triggers a file move.
        assert_eq!(CurationStatus::from_db("garbage"), CurationStatus::Stale);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn curator_meta_roundtrip() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        assert!(store.meta_get("last_run_at").await.unwrap().is_none());
        store
            .meta_set("last_run_at", "2026-07-11T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            store.meta_get("last_run_at").await.unwrap().as_deref(),
            Some("2026-07-11T00:00:00Z")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn update_human_fields_only_touches_human_cols() {
        let store = CustomSkillStore::open_in_memory().unwrap();
        store.insert(&rec("s1", "alice")).await.unwrap();
        store
            .update_human_fields(
                "s1",
                Some("Renamed"),
                None,
                Some(2.0),
                Some("hours_per_month"),
                None,
            )
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
