//! Gamification growth layer (V10-T10.0).
//!
//! Turns **already-true facts** — completed tasks, acquired skills, knowledge
//! pages, routine runs, approved custom skills — into an XP/level score plus a
//! declarative achievement wall. Two hard rules keep this honest:
//!
//! 1. **The judging engine is a pure function** ([`compute_snapshot`]). Given
//!    the same [`GrowthFacts`] it always returns the same XP, level, and
//!    achievement set — recompute is byte-for-byte idempotent. It performs no
//!    IO and never invents numbers.
//! 2. **`growth.db` only stores facts, not behaviour.** XP does not feed back
//!    into any agent's reasoning. The store persists (a) achievement *unlock
//!    timestamps* (first time a threshold is crossed), (b) an XP snapshot log
//!    for auditing, and (c) a per-day daily-report cache. Every scored value is
//!    derived at read time from the existing internal surfaces (tasks store /
//!    skills registry / wiki / cost telemetry / cron); a source we cannot read
//!    honestly is surfaced as `available: false` with a documented reason — the
//!    UI shows "unavailable", never a fabricated estimate.
//!
//! The gateway handler ([`crate::handlers`]) gathers [`GrowthFacts`] from those
//! surfaces and hands them to [`compute_snapshot`]; this module owns the
//! formula, the achievement table, and the SQLite persistence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{NaiveDate, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

// ── XP weights (§6.1) ───────────────────────────────────────

/// XP granted per completed task.
pub const XP_PER_TASK: u64 = 12;
/// XP granted per acquired (installed) skill.
pub const XP_PER_SKILL: u64 = 25;
/// XP granted per knowledge (wiki) page.
pub const XP_PER_KNOWLEDGE_PAGE: u64 = 8;
/// XP granted per successful routine (cron) run.
pub const XP_PER_ROUTINE: u64 = 5;

/// XP required to reach level `L` is `L^2 * 100`, i.e. `Lv = floor(sqrt(XP/100))`.
pub const XP_LEVEL_BASE: u64 = 100;

// ── Facts snapshot ──────────────────────────────────────────

/// A read-time snapshot of the real, already-persisted facts the growth engine
/// scores. Gathered by the gateway handler from the existing internal surfaces;
/// the engine treats it as the single source of truth. Every field is a plain
/// count so the computation stays a pure function of its input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrowthFacts {
    /// Number of agents ("employees") that exist. Source: `AgentRegistry::list`.
    pub agents_count: u64,
    /// Tasks in the `done` state. Source: `TaskStore::list_tasks(Some("done"))`.
    pub tasks_completed: u64,
    /// Total wiki pages across all agent wikis + the shared wiki.
    /// Source: `duduclaw_memory::WikiStore::list_pages`.
    pub knowledge_pages: u64,
    /// Installed skills across global `skills/` + every agent `SKILLS/`.
    /// Source: `AgentRegistry::load_skills`.
    pub skills_acquired: u64,
    /// Successful scheduled runs. Source: sum of `CronTaskRow::run_count`.
    ///
    /// NOTE on semantics: the design copy says "routine *streak* achieved", but
    /// no per-routine streak history is persisted. We score the honest,
    /// available fact — the count of successful runs — rather than fabricate a
    /// streak. Documented here so the interpretation is explicit.
    pub routines_completed: u64,
    /// Custom skills that have been approved & installed (T13). Source:
    /// `custom_skill_registry` rows with `status = 'approved'`.
    pub custom_skills_approved: u64,
    /// Current trailing streak of consecutive calendar days with an EMPTY
    /// actionable inbox (pending approvals + blocked tasks + open budget
    /// incidents all zero). Source: `inbox_daily_snapshot` in `growth.db`,
    /// lazily recorded on each `growth.snapshot` call.
    ///
    /// HONESTY NOTE: no per-day inbox history existed before this column, so the
    /// streak necessarily starts accruing from the first `growth.snapshot` call
    /// onward — days prior to that have no snapshot and cannot be counted.
    pub inbox_zero_streak_days: u64,
    /// Cumulative real hours saved by approved custom skills, FLOORED to whole
    /// hours (kept integer so `GrowthFacts` stays `Eq`). Source: sum of
    /// [`crate::custom_skills::estimate_saved_hours`] over approved skills —
    /// per-use units use the real `usage_count`, per-month units accrue over
    /// months since approval. The precise fractional per-skill figure is exposed
    /// separately on the `skills.custom_list` record (`saved_hours_estimate`).
    pub custom_skill_saved_hours: u64,
}

impl GrowthFacts {
    /// Base XP from countable facts (excludes one-time achievement bonuses).
    pub fn base_xp(&self) -> u64 {
        self.tasks_completed.saturating_mul(XP_PER_TASK)
            .saturating_add(self.skills_acquired.saturating_mul(XP_PER_SKILL))
            .saturating_add(self.knowledge_pages.saturating_mul(XP_PER_KNOWLEDGE_PAGE))
            .saturating_add(self.routines_completed.saturating_mul(XP_PER_ROUTINE))
    }
}

// ── Achievement definitions (declarative) ───────────────────

/// The condition that unlocks an achievement, expressed declaratively so the
/// table below reads as data. `NotImplemented` carries the reason its data
/// source is unavailable — those achievements are always `available: false`.
#[derive(Debug, Clone)]
pub enum Condition {
    AgentsAtLeast(u64),
    TasksCompletedAtLeast(u64),
    KnowledgePagesAtLeast(u64),
    SkillsAtLeast(u64),
    CustomSkillsApprovedAtLeast(u64),
    /// Consecutive days with an empty actionable inbox (L5 §14). Progress is the
    /// current trailing zero-streak; unlocks at the threshold.
    InboxZeroStreakAtLeast(u64),
    /// Cumulative whole hours saved by approved custom skills (L5 §14).
    CustomSkillSavedHoursAtLeast(u64),
    /// Unlockable in principle but its data source is not yet wired. The string
    /// documents exactly what is missing (surfaced to the UI as the reason).
    NotImplemented(&'static str),
}

impl Condition {
    /// Current progress toward this condition given the facts.
    fn current(&self, f: &GrowthFacts) -> u64 {
        match self {
            Condition::AgentsAtLeast(_) => f.agents_count,
            Condition::TasksCompletedAtLeast(_) => f.tasks_completed,
            Condition::KnowledgePagesAtLeast(_) => f.knowledge_pages,
            Condition::SkillsAtLeast(_) => f.skills_acquired,
            Condition::CustomSkillsApprovedAtLeast(_) => f.custom_skills_approved,
            Condition::InboxZeroStreakAtLeast(_) => f.inbox_zero_streak_days,
            Condition::CustomSkillSavedHoursAtLeast(_) => f.custom_skill_saved_hours,
            Condition::NotImplemented(_) => 0,
        }
    }

    /// The threshold (progress denominator) needed to unlock.
    fn denominator(&self) -> u64 {
        match self {
            Condition::AgentsAtLeast(n)
            | Condition::TasksCompletedAtLeast(n)
            | Condition::KnowledgePagesAtLeast(n)
            | Condition::SkillsAtLeast(n)
            | Condition::CustomSkillsApprovedAtLeast(n)
            | Condition::InboxZeroStreakAtLeast(n)
            | Condition::CustomSkillSavedHoursAtLeast(n) => *n,
            Condition::NotImplemented(_) => 0,
        }
    }

    fn available(&self) -> bool {
        !matches!(self, Condition::NotImplemented(_))
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        match self {
            Condition::NotImplemented(r) => Some(r),
            _ => None,
        }
    }
}

/// One achievement in the declarative table.
#[derive(Debug, Clone)]
pub struct AchievementDef {
    /// Stable machine id; the frontend maps it to an i18n label.
    pub id: &'static str,
    /// One-time XP bonus awarded when unlocked.
    pub xp_reward: u64,
    /// Unlock condition.
    pub condition: Condition,
}

/// The declarative achievement table (§6.3). Order is stable — the UI renders
/// it top-to-bottom. Add rows here; the engine and store need no other change.
pub fn achievement_defs() -> Vec<AchievementDef> {
    vec![
        AchievementDef {
            id: "first_agent",
            xp_reward: 20,
            condition: Condition::AgentsAtLeast(1),
        },
        AchievementDef {
            id: "first_task_done",
            xp_reward: 20,
            condition: Condition::TasksCompletedAtLeast(1),
        },
        AchievementDef {
            id: "tasks_100",
            xp_reward: 100,
            condition: Condition::TasksCompletedAtLeast(100),
        },
        AchievementDef {
            id: "knowledge_100",
            xp_reward: 100,
            condition: Condition::KnowledgePagesAtLeast(100),
        },
        AchievementDef {
            id: "skills_10",
            xp_reward: 50,
            condition: Condition::SkillsAtLeast(10),
        },
        AchievementDef {
            id: "inbox_zero_streak_7",
            xp_reward: 70,
            // L5 §14: `inbox_daily_snapshot` now records the actionable count per
            // day (lazily, on each growth.snapshot call). The streak is the
            // trailing run of consecutive zero days — accrues from the first
            // recorded day forward (history before that is genuinely unknown).
            condition: Condition::InboxZeroStreakAtLeast(7),
        },
        AchievementDef {
            id: "custom_skill_first",
            xp_reward: 40,
            condition: Condition::CustomSkillsApprovedAtLeast(1),
        },
        AchievementDef {
            id: "custom_skill_saved_100h",
            xp_reward: 150,
            // L5 §14: `custom_skill_registry.usage_count` now records real
            // invocations (from the channel_reply Skill-tool path). Saved hours =
            // per-use estimate × usage_count (per-month estimates accrue over
            // months since approval) — a real figure, no fabrication.
            condition: Condition::CustomSkillSavedHoursAtLeast(100),
        },
    ]
}

/// The computed state of a single achievement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AchievementState {
    pub id: String,
    /// True when the condition is met (only possible when `available`).
    pub unlocked: bool,
    /// Current progress value.
    pub progress_current: u64,
    /// Progress needed to unlock (0 for unavailable achievements).
    pub progress_denominator: u64,
    /// One-time XP bonus this achievement grants.
    pub xp_reward: u64,
    /// False when the data source is not wired; the UI must show "unavailable"
    /// rather than a 0/locked state that implies it's merely not yet earned.
    pub available: bool,
    /// Why it's unavailable (only set when `available == false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

// ── Pure judging engine ─────────────────────────────────────

/// The pure result of scoring a facts snapshot. Contains everything the
/// `growth.snapshot` RPC needs except the persisted unlock timestamps (which
/// the store layers on afterward).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrowthSnapshot {
    pub xp: u64,
    pub level: u64,
    /// XP accumulated inside the current level (`xp - level^2 * 100`).
    pub xp_into_level: u64,
    /// XP span of the current level (`((level+1)^2 - level^2) * 100`).
    pub xp_for_next_level: u64,
    pub achievements: Vec<AchievementState>,
}

/// Integer square root (floor). Deterministic, no float rounding surprises.
fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    // Newton's method on integers.
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Level from total XP: `floor(sqrt(XP / XP_LEVEL_BASE))`, uncapped.
pub fn level_from_xp(xp: u64) -> u64 {
    isqrt(xp / XP_LEVEL_BASE)
}

/// XP threshold to *reach* the given level: `level^2 * XP_LEVEL_BASE`.
pub fn xp_threshold_for_level(level: u64) -> u64 {
    level.saturating_mul(level).saturating_mul(XP_LEVEL_BASE)
}

/// Current trailing streak of consecutive calendar days whose actionable-inbox
/// count is zero, counting back from the most recent recorded day. **Pure.**
///
/// A gap in the calendar sequence (a day with no snapshot) or any day with a
/// non-zero count ends the streak — a broken chain does not silently jump the
/// gap. Records need not be sorted or de-duplicated; the newest date wins on a
/// duplicate. Returns 0 for an empty input or when the most recent day is
/// non-zero.
pub fn inbox_zero_streak(records: &[(NaiveDate, i64)]) -> u64 {
    if records.is_empty() {
        return 0;
    }
    let mut rows: Vec<(NaiveDate, i64)> = records.to_vec();
    rows.sort_by(|a, b| b.0.cmp(&a.0)); // newest date first
    rows.dedup_by(|a, b| a.0 == b.0); // collapse duplicate dates (keeps newest-first first)

    let mut streak = 0u64;
    let mut expected = rows[0].0;
    for (date, count) in rows {
        if date != expected || count != 0 {
            break;
        }
        streak += 1;
        expected = match expected.pred_opt() {
            Some(d) => d,
            None => break, // chrono min date — cannot go earlier
        };
    }
    streak
}

/// Score a facts snapshot into XP, level, and the achievement wall. **Pure** —
/// no IO, fully determined by `facts`. Total XP = base XP (from countable
/// facts) + the one-time bonus of every *unlocked* achievement, so recompute is
/// idempotent.
pub fn compute_snapshot(facts: &GrowthFacts) -> GrowthSnapshot {
    let defs = achievement_defs();
    let mut achievements = Vec::with_capacity(defs.len());
    let mut achievement_xp: u64 = 0;

    for def in &defs {
        let available = def.condition.available();
        let denom = def.condition.denominator();
        let current = def.condition.current(facts);
        let unlocked = available && denom > 0 && current >= denom;
        if unlocked {
            achievement_xp = achievement_xp.saturating_add(def.xp_reward);
        }
        achievements.push(AchievementState {
            id: def.id.to_string(),
            unlocked,
            // Cap displayed progress at the denominator so the bar never
            // exceeds 100% (e.g. 250 completed tasks vs. a 100 threshold).
            progress_current: if denom > 0 { current.min(denom) } else { current },
            progress_denominator: denom,
            xp_reward: def.xp_reward,
            available,
            unavailable_reason: def.condition.unavailable_reason().map(|s| s.to_string()),
        });
    }

    let xp = facts.base_xp().saturating_add(achievement_xp);
    let level = level_from_xp(xp);
    let level_floor = xp_threshold_for_level(level);
    let next_floor = xp_threshold_for_level(level + 1);
    GrowthSnapshot {
        xp,
        level,
        xp_into_level: xp.saturating_sub(level_floor),
        xp_for_next_level: next_floor.saturating_sub(level_floor),
        achievements,
    }
}

// ── Persistence (growth.db) ─────────────────────────────────

/// SQLite-backed growth persistence. Mirrors the project store idioms
/// (`Mutex<Connection>`, WAL, self-healing schema, parameterized SQL only).
/// Stores only facts: achievement unlock timestamps, an XP snapshot audit log,
/// and a per-day daily-report cache.
pub struct GrowthStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

impl GrowthStore {
    /// Open (or create) `<home>/growth.db`.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        let db_path = home_dir.join("growth.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("open growth store: {e}"))?;
        Self::init_schema(&conn)?;
        info!(?db_path, "GrowthStore initialized");
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

             CREATE TABLE IF NOT EXISTS achievement_unlocks (
                 id           TEXT PRIMARY KEY,
                 unlocked_at  TEXT NOT NULL,
                 xp_awarded   INTEGER NOT NULL DEFAULT 0
             );

             CREATE TABLE IF NOT EXISTS xp_snapshots (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 computed_at  TEXT NOT NULL,
                 xp           INTEGER NOT NULL,
                 level        INTEGER NOT NULL,
                 facts_json   TEXT NOT NULL DEFAULT '{}'
             );

             CREATE TABLE IF NOT EXISTS daily_reports (
                 report_date  TEXT PRIMARY KEY,
                 report_json  TEXT NOT NULL,
                 cached_at    TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS inbox_daily_snapshot (
                 date             TEXT PRIMARY KEY,
                 actionable_count INTEGER NOT NULL,
                 recorded_at      TEXT NOT NULL
             );",
        )
        .map_err(|e| format!("init growth schema: {e}"))?;
        Ok(())
    }

    /// Record the first-observed unlock of an achievement. Idempotent —
    /// `INSERT OR IGNORE` keeps the earliest timestamp. Returns true if this
    /// call was the one that inserted (i.e. a *newly* unlocked achievement).
    pub async fn record_unlock(&self, id: &str, xp_awarded: u64, now: &str) -> Result<bool, String> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO achievement_unlocks (id, unlocked_at, xp_awarded)
                 VALUES (?1, ?2, ?3)",
                params![id, now, xp_awarded as i64],
            )
            .map_err(|e| format!("record unlock: {e}"))?;
        Ok(changed > 0)
    }

    /// Map of `achievement_id -> unlocked_at` for every recorded unlock.
    pub async fn unlock_times(&self) -> Result<HashMap<String, String>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id, unlocked_at FROM achievement_unlocks")
            .map_err(|e| format!("prepare unlock_times: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("query unlock_times: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect unlock_times: {e}"))?;
        Ok(rows.into_iter().collect())
    }

    /// Append an XP snapshot to the audit log.
    pub async fn save_snapshot(
        &self,
        xp: u64,
        level: u64,
        facts_json: &str,
        now: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO xp_snapshots (computed_at, xp, level, facts_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![now, xp as i64, level as i64, facts_json],
        )
        .map_err(|e| format!("save snapshot: {e}"))?;
        Ok(())
    }

    /// Fetch a cached daily report for `report_date` (YYYY-MM-DD), if present.
    pub async fn get_daily_report(&self, report_date: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT report_json FROM daily_reports WHERE report_date = ?1",
            params![report_date],
            |r| r.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(format!("get daily report: {other}")),
        })
    }

    /// Cache a daily report (upsert on `report_date`).
    pub async fn put_daily_report(
        &self,
        report_date: &str,
        report_json: &str,
        now: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO daily_reports (report_date, report_json, cached_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(report_date) DO UPDATE SET
                 report_json = excluded.report_json,
                 cached_at   = excluded.cached_at",
            params![report_date, report_json, now],
        )
        .map_err(|e| format!("put daily report: {e}"))?;
        Ok(())
    }

    /// Lazily record the actionable-inbox count for a calendar day (YYYY-MM-DD).
    /// At most one row per day; a repeat call on the same day keeps the **minimum**
    /// count seen, so an inbox that was cleared at any point that day counts as
    /// zero (the design's "一天內清零也算達成"). `recorded_at` is left at the
    /// earliest write.
    pub async fn record_inbox_snapshot(
        &self,
        date: &str,
        actionable_count: i64,
        now: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO inbox_daily_snapshot (date, actionable_count, recorded_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(date) DO UPDATE SET
                 actionable_count =
                     MIN(inbox_daily_snapshot.actionable_count, excluded.actionable_count)",
            params![date, actionable_count, now],
        )
        .map_err(|e| format!("record inbox snapshot: {e}"))?;
        Ok(())
    }

    /// All recorded inbox snapshots as `(date_string, actionable_count)`. The
    /// caller parses dates and computes the streak via [`inbox_zero_streak`].
    pub async fn inbox_snapshots(&self) -> Result<Vec<(String, i64)>, String> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT date, actionable_count FROM inbox_daily_snapshot")
            .map_err(|e| format!("prepare inbox_snapshots: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| format!("query inbox_snapshots: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("collect inbox_snapshots: {e}"))?;
        Ok(rows)
    }

    /// Convenience: record today's snapshot then return the current zero-streak.
    pub async fn inbox_zero_streak_days(&self) -> Result<u64, String> {
        let snaps = self.inbox_snapshots().await?;
        let parsed: Vec<(NaiveDate, i64)> = snaps
            .iter()
            .filter_map(|(d, c)| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok().map(|nd| (nd, *c)))
            .collect();
        Ok(inbox_zero_streak(&parsed))
    }
}

/// Build the full `growth.snapshot` payload: score the facts (pure), persist
/// any newly-unlocked achievements, then merge in unlock timestamps. Returns
/// `(snapshot, unlock_times)` so the handler can shape the wire JSON.
pub async fn snapshot_with_store(
    store: &GrowthStore,
    facts: &GrowthFacts,
) -> Result<(GrowthSnapshot, HashMap<String, String>), String> {
    let snap = compute_snapshot(facts);
    let now = Utc::now().to_rfc3339();
    for a in &snap.achievements {
        if a.unlocked {
            let _ = store.record_unlock(&a.id, a.xp_reward, &now).await?;
        }
    }
    let facts_json = serde_json::to_string(facts).unwrap_or_else(|_| "{}".to_string());
    let _ = store.save_snapshot(snap.xp, snap.level, &facts_json, &now).await;
    let times = store.unlock_times().await?;
    Ok((snap, times))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(tasks: u64, skills: u64, knowledge: u64, routines: u64) -> GrowthFacts {
        GrowthFacts {
            agents_count: 0,
            tasks_completed: tasks,
            knowledge_pages: knowledge,
            skills_acquired: skills,
            routines_completed: routines,
            custom_skills_approved: 0,
            inbox_zero_streak_days: 0,
            custom_skill_saved_hours: 0,
        }
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    // ── XP formula ──────────────────────────────────────────

    #[test]
    fn base_xp_weights_are_exact() {
        // 3 tasks (36) + 2 skills (50) + 5 pages (40) + 4 routines (20) = 146
        let f = facts(3, 2, 5, 4);
        assert_eq!(f.base_xp(), 3 * 12 + 2 * 25 + 5 * 8 + 4 * 5);
        assert_eq!(f.base_xp(), 146);
    }

    #[test]
    fn total_xp_includes_unlocked_achievement_bonus() {
        // 1 task done → base 12, unlocks first_task_done (+20) = 32.
        let snap = compute_snapshot(&facts(1, 0, 0, 0));
        assert_eq!(snap.xp, 12 + 20);
    }

    // ── Level curve ─────────────────────────────────────────

    #[test]
    fn level_curve_matches_floor_sqrt() {
        assert_eq!(level_from_xp(0), 0);
        assert_eq!(level_from_xp(99), 0);
        assert_eq!(level_from_xp(100), 1);
        assert_eq!(level_from_xp(399), 1);
        assert_eq!(level_from_xp(400), 2);
        assert_eq!(level_from_xp(900), 3);
        assert_eq!(level_from_xp(10_000), 10);
    }

    #[test]
    fn xp_into_level_and_span_are_consistent() {
        // xp = 500 → level 2 (threshold 400), next level 3 (threshold 900).
        let snap = GrowthSnapshot {
            xp: 500,
            level: level_from_xp(500),
            xp_into_level: 500 - xp_threshold_for_level(2),
            xp_for_next_level: xp_threshold_for_level(3) - xp_threshold_for_level(2),
            achievements: vec![],
        };
        assert_eq!(snap.level, 2);
        assert_eq!(snap.xp_into_level, 100);
        assert_eq!(snap.xp_for_next_level, 500);
    }

    // ── Achievement judgments (≥3) ──────────────────────────

    fn find<'a>(snap: &'a GrowthSnapshot, id: &str) -> &'a AchievementState {
        snap.achievements.iter().find(|a| a.id == id).expect("achievement present")
    }

    #[test]
    fn first_task_done_locks_then_unlocks() {
        let locked = compute_snapshot(&facts(0, 0, 0, 0));
        assert!(!find(&locked, "first_task_done").unlocked);
        let unlocked = compute_snapshot(&facts(1, 0, 0, 0));
        assert!(find(&unlocked, "first_task_done").unlocked);
    }

    #[test]
    fn tasks_100_progress_caps_at_denominator() {
        let snap = compute_snapshot(&facts(250, 0, 0, 0));
        let a = find(&snap, "tasks_100");
        assert!(a.unlocked);
        assert_eq!(a.progress_denominator, 100);
        // 250 completed but progress display capped at 100.
        assert_eq!(a.progress_current, 100);
    }

    #[test]
    fn skills_10_and_knowledge_100_thresholds() {
        let snap = compute_snapshot(&GrowthFacts {
            skills_acquired: 10,
            knowledge_pages: 99,
            ..Default::default()
        });
        assert!(find(&snap, "skills_10").unlocked);
        assert!(!find(&snap, "knowledge_100").unlocked);
        assert_eq!(find(&snap, "knowledge_100").progress_current, 99);
    }

    #[test]
    fn custom_skill_first_unlocks_on_first_approved() {
        let snap = compute_snapshot(&GrowthFacts {
            custom_skills_approved: 1,
            ..Default::default()
        });
        assert!(find(&snap, "custom_skill_first").unlocked);
    }

    #[test]
    fn all_achievements_are_now_available_none_deferred() {
        // L5 §14 wired the last two data sources — nothing is NotImplemented now.
        let snap = compute_snapshot(&GrowthFacts::default());
        for a in &snap.achievements {
            assert!(a.available, "{} must be available (no deferred left)", a.id);
            assert!(a.unavailable_reason.is_none(), "{} carries no gap reason", a.id);
        }
    }

    #[test]
    fn inbox_streak_and_saved_hours_gate_on_their_facts() {
        // Below threshold: locked but available with progress.
        let low = compute_snapshot(&GrowthFacts {
            inbox_zero_streak_days: 3,
            custom_skill_saved_hours: 40,
            ..Default::default()
        });
        let inbox = find(&low, "inbox_zero_streak_7");
        assert!(inbox.available && !inbox.unlocked);
        assert_eq!(inbox.progress_current, 3);
        assert_eq!(inbox.progress_denominator, 7);
        let saved = find(&low, "custom_skill_saved_100h");
        assert!(saved.available && !saved.unlocked);
        assert_eq!(saved.progress_current, 40);
        assert_eq!(saved.progress_denominator, 100);

        // At/over threshold: unlocked, progress capped at denominator.
        let high = compute_snapshot(&GrowthFacts {
            inbox_zero_streak_days: 9,
            custom_skill_saved_hours: 250,
            ..Default::default()
        });
        assert!(find(&high, "inbox_zero_streak_7").unlocked);
        assert_eq!(find(&high, "inbox_zero_streak_7").progress_current, 7);
        assert!(find(&high, "custom_skill_saved_100h").unlocked);
        assert_eq!(find(&high, "custom_skill_saved_100h").progress_current, 100);
    }

    // ── Inbox zero-streak pure fn ───────────────────────────

    #[test]
    fn streak_counts_consecutive_trailing_zero_days() {
        // 5 consecutive zero days ending 2026-07-10.
        let recs = [
            (day("2026-07-06"), 0),
            (day("2026-07-07"), 0),
            (day("2026-07-08"), 0),
            (day("2026-07-09"), 0),
            (day("2026-07-10"), 0),
        ];
        assert_eq!(inbox_zero_streak(&recs), 5);
    }

    #[test]
    fn streak_breaks_on_gap_and_on_nonzero() {
        // Gap: 07-08 missing → chain from 07-10 stops after 2 days.
        let gapped = [
            (day("2026-07-06"), 0),
            (day("2026-07-07"), 0),
            (day("2026-07-09"), 0),
            (day("2026-07-10"), 0),
        ];
        assert_eq!(inbox_zero_streak(&gapped), 2);

        // Most recent day non-zero → streak 0 regardless of history.
        let dirty_today = [(day("2026-07-09"), 0), (day("2026-07-10"), 3)];
        assert_eq!(inbox_zero_streak(&dirty_today), 0);

        // Non-zero mid-chain ends it.
        let mid = [
            (day("2026-07-08"), 0),
            (day("2026-07-09"), 2),
            (day("2026-07-10"), 0),
        ];
        assert_eq!(inbox_zero_streak(&mid), 1);
    }

    #[test]
    fn streak_handles_cross_month_boundary() {
        // 06-30 → 07-01 → 07-02 all zero: contiguous across the month edge.
        let cross = [
            (day("2026-06-30"), 0),
            (day("2026-07-01"), 0),
            (day("2026-07-02"), 0),
        ];
        assert_eq!(inbox_zero_streak(&cross), 3);
    }

    #[test]
    fn streak_empty_and_unsorted_inputs() {
        assert_eq!(inbox_zero_streak(&[]), 0);
        // Unsorted input still resolves newest-first internally.
        let unsorted = [
            (day("2026-07-10"), 0),
            (day("2026-07-08"), 0),
            (day("2026-07-09"), 0),
        ];
        assert_eq!(inbox_zero_streak(&unsorted), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inbox_snapshot_takes_daily_min_and_computes_streak() {
        let store = GrowthStore::open_in_memory().unwrap();
        // Day 1 recorded with 5 actionable, then re-recorded as 0 (cleared) →
        // MIN keeps 0.
        store.record_inbox_snapshot("2026-07-09", 5, "2026-07-09T01:00:00+00:00").await.unwrap();
        store.record_inbox_snapshot("2026-07-09", 0, "2026-07-09T20:00:00+00:00").await.unwrap();
        store.record_inbox_snapshot("2026-07-10", 0, "2026-07-10T09:00:00+00:00").await.unwrap();
        // A later same-day re-record with a HIGHER count must not overwrite the min.
        store.record_inbox_snapshot("2026-07-10", 4, "2026-07-10T23:00:00+00:00").await.unwrap();
        let snaps = store.inbox_snapshots().await.unwrap();
        let map: HashMap<String, i64> = snaps.into_iter().collect();
        assert_eq!(map.get("2026-07-09"), Some(&0));
        assert_eq!(map.get("2026-07-10"), Some(&0));
        assert_eq!(store.inbox_zero_streak_days().await.unwrap(), 2);
    }

    // ── Recompute idempotency ───────────────────────────────

    #[test]
    fn recompute_is_idempotent() {
        let f = facts(17, 3, 42, 9);
        let a = compute_snapshot(&f);
        let b = compute_snapshot(&f);
        assert_eq!(a, b, "same facts must yield byte-identical snapshot");
    }

    // ── Store ───────────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn store_unlock_is_idempotent_and_timestamped() {
        let store = GrowthStore::open_in_memory().unwrap();
        let t1 = "2026-07-10T00:00:00+00:00";
        let t2 = "2026-07-11T00:00:00+00:00";
        assert!(store.record_unlock("first_task_done", 20, t1).await.unwrap());
        // Second record keeps the earliest timestamp and reports "not new".
        assert!(!store.record_unlock("first_task_done", 20, t2).await.unwrap());
        let times = store.unlock_times().await.unwrap();
        assert_eq!(times.get("first_task_done").map(String::as_str), Some(t1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_with_store_records_and_merges_unlocks() {
        let store = GrowthStore::open_in_memory().unwrap();
        let (snap, times) = snapshot_with_store(&store, &facts(1, 0, 0, 0)).await.unwrap();
        assert!(find(&snap, "first_task_done").unlocked);
        assert!(times.contains_key("first_task_done"));
        // Re-running with the same facts is stable and does not duplicate.
        let (snap2, times2) = snapshot_with_store(&store, &facts(1, 0, 0, 0)).await.unwrap();
        assert_eq!(snap, snap2);
        assert_eq!(times.len(), times2.len());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn daily_report_cache_roundtrips() {
        let store = GrowthStore::open_in_memory().unwrap();
        assert!(store.get_daily_report("2026-07-09").await.unwrap().is_none());
        store
            .put_daily_report("2026-07-09", r#"{"tasks":3}"#, "2026-07-10T00:00:00+00:00")
            .await
            .unwrap();
        assert_eq!(
            store.get_daily_report("2026-07-09").await.unwrap().as_deref(),
            Some(r#"{"tasks":3}"#)
        );
    }
}
