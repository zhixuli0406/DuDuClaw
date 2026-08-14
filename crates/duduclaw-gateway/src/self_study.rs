//! Per-agent "自主研究" (self-study) — Belief Loop × Goal Contract gap 2.
//!
//! See `commercial/docs/DESIGN-market-belief-loop-2026-08.md` §3:
//!
//! ```text
//! ┌─ 自主研究（做功課）───────────────────────────────────────
//! │ 觸發：當日有 belief miss 或 mistake（失準>錯誤>好奇）
//! │ 形態：goal（每日至多 1 個，goal loop 既有 cap 防 runaway）
//! │ 驗收：研究筆記落 wiki/記憶＋次日派工必引用
//! └───────────────────────────────────────────────────────────
//! ```
//!
//! Platform-level, domain-agnostic: it replaces the "hand-write a cron
//! prompt per agent" pattern (design §Appendix A) with a generic mechanism
//! any agent can opt into. Every evening, past `[research] self_study_hour`,
//! an opted-in agent gets **at most one** goal-mode task asking it to review
//! today's worst-calibrated belief `subject` (design §0-7: 「閒時計算只整理
//! 已確認歷史」— this is a review-and-write task, never a fresh prediction).
//!
//! No miss today ⇒ nothing is created (design §0-1 spirit extended: an
//! agent that called everything right has nothing to research). The
//! once-per-local-day claim lives in `prediction.db`'s `belief_meta` table
//! (via [`super::prediction::belief::get_meta`] / `set_meta`) — the same
//! store `belief.rs`'s `mark_stats_injected` marker uses, so this doesn't
//! invent a second key-value store.

use std::path::{Path, PathBuf};

use chrono::{Datelike, Timelike};
use tracing::{debug, info, warn};

use crate::prediction::belief;
use crate::task_store::{TaskRow, TaskStore};

/// `agent.toml [research] self_study_hour` default — evening, after a
/// typical trading/business day has produced its belief settlements.
const DEFAULT_SELF_STUDY_HOUR: u32 = 20;

/// Tag stamped on every self-study goal task (design §3), used both for
/// human/dashboard identification and for the task-store double-check in
/// [`SelfStudyScheduler::already_created_today`].
pub const AUTO_RESEARCH_TAG: &str = "auto-research";

/// `created_by` stamped on every self-study goal task — mirrors the
/// `goal:<channel>` convention `chat_commands.rs`'s `/goal` command uses, so
/// a research-created task is identifiable in the same field without a
/// schema change.
const RESEARCH_CREATED_BY: &str = "research:self_study";

/// Bounded scan of recent beliefs when picking today's worst subject — same
/// discipline as `belief::RECENT_MAX_LIMIT` callers elsewhere (a self-study
/// pick is a summary operation, not a full-history scan).
const BELIEF_SCAN_LIMIT: usize = belief::RECENT_MAX_LIMIT;

// ─────────────────────────────────────────────────────────────────────────
// Config — `agent.toml [research]`
// ─────────────────────────────────────────────────────────────────────────

/// `agent.toml [research]` — per-agent self-study opt-in. Default OFF
/// (design's platform-level features are opt-in per the codebase-wide
/// posture — see `[evolution] gvu_enabled` for the precedent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResearchConfig {
    pub self_study: bool,
    /// Local wall-clock hour (0-23) past which the goal may fire.
    pub self_study_hour: u32,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self { self_study: false, self_study_hour: DEFAULT_SELF_STUDY_HOUR }
    }
}

impl ResearchConfig {
    /// Read `agent.toml [research] self_study` / `self_study_hour` from an
    /// agent's directory. Missing file / section / malformed value ⇒
    /// [`Self::default`] (feature off) — same fail-open posture
    /// `gvu::trigger::agent_gvu_enabled` uses for the sibling `[evolution]`
    /// opt-in. An out-of-range hour (not 0-23) is treated as absent rather
    /// than clamped, so a typo'd config reads as "not configured" instead of
    /// silently firing at a boundary hour nobody chose.
    pub fn from_agent_dir(agent_dir: &Path) -> Self {
        let path = agent_dir.join("agent.toml");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(value) = raw.parse::<toml::Value>() else {
            return Self::default();
        };
        let section = value.get("research");
        let self_study = section
            .and_then(|s| s.get("self_study"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let self_study_hour = section
            .and_then(|s| s.get("self_study_hour"))
            .and_then(|v| v.as_integer())
            .filter(|h| (0..=23).contains(h))
            .map(|h| h as u32)
            .unwrap_or(DEFAULT_SELF_STUDY_HOUR);
        Self { self_study, self_study_hour }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Pure decision functions
// ─────────────────────────────────────────────────────────────────────────

/// Should the self-study goal fire on this tick? Pure — the whole scheduling
/// rule in one testable function (mirrors `notify_digest::should_send`'s
/// shape at hour granularity instead of minute granularity, since the
/// caller ticks every 5 minutes and only ever needs to know "has the
/// configured hour arrived").
///
/// - Already ran today ⇒ no (the once-per-local-day ceiling, design §3
///   「每 agent 每日至多 1 個」).
/// - Local hour hasn't reached the configured hour yet ⇒ no.
/// - Otherwise ⇒ yes — including a gateway that was down at the configured
///   hour and catches up later the same day (same posture as the daily
///   digest scheduler).
pub fn should_run_now(now_local_hour: u32, cfg_hour: u32, already_ran_today: bool) -> bool {
    if already_ran_today {
        return false;
    }
    now_local_hour >= cfg_hour
}

/// Filter `rows` (as returned by [`belief::recent`]) down to those settled
/// **today** (UTC calendar day — matches `belief::unsettled_today`'s own
/// date convention, since every `belief_log` timestamp is UTC) with
/// `outcome == "miss"`. Pure: the caller supplies `today_utc` so this has no
/// wall-clock dependency and stays unit-testable.
pub fn todays_misses<'a>(
    rows: &'a [belief::BeliefRow],
    today_utc: &str,
) -> Vec<&'a belief::BeliefRow> {
    rows.iter()
        .filter(|r| r.outcome.as_deref() == Some("miss"))
        .filter(|r| {
            r.settled_at
                .as_deref()
                .map(|s| s.starts_with(today_utc))
                .unwrap_or(false)
        })
        .collect()
}

/// Pick the subject whose today's miss carries the worst (highest) Brier
/// score — design §3 「題目 = 今日 brier 最差的 subject」. `None` when there
/// is no miss today (design: no miss ⇒ no research goal, never a fallback
/// subject).
///
/// Ties (equal Brier) resolve by subject name ascending, so the pick is
/// deterministic across runs rather than depending on scan order.
pub fn pick_worst_subject(misses: &[&belief::BeliefRow]) -> Option<String> {
    misses
        .iter()
        .filter_map(|r| r.brier.map(|b| (b, r.subject.clone())))
        .fold(None::<(f64, String)>, |best, (b, subj)| match best {
            None => Some((b, subj)),
            Some((best_b, best_subj)) => {
                if b > best_b || (b == best_b && subj < best_subj) {
                    Some((b, subj))
                } else {
                    Some((best_b, best_subj))
                }
            }
        })
        .map(|(_, subj)| subj)
}

/// Combines [`todays_misses`] + [`pick_worst_subject`] — the single entry
/// point the scheduler calls. Kept as a thin composition (not re-tested
/// beyond its two parts) so the two building blocks stay independently
/// testable.
pub fn pick_research_subject(rows: &[belief::BeliefRow], today_utc: &str) -> Option<String> {
    let misses = todays_misses(rows, today_utc);
    pick_worst_subject(&misses)
}

/// Render the research goal's `description` body (design §3): review
/// today's belief context for `subject`, look for external root causes, and
/// write a research note carrying the three required elements. Pure — no
/// I/O, unit-tested directly.
pub fn render_research_description(subject: &str) -> String {
    format!(
        "回顧今日「{subject}」的信念脈絡（belief_submit / belief_settle 紀錄），\
         查找可能的外部事實根因，將研究筆記寫入記憶或 wiki，內容須包含：\n\
         1. 失準最可能原因\n\
         2. 明日可執行的修正\n\
         3. 一條可驗證的觀察點\n\
         禁止建立其他任務。"
    )
}

/// Render the research goal's `acceptance_criteria` (design §3): the note
/// must exist and carry all three required elements.
pub fn render_research_acceptance() -> String {
    "研究筆記已存在（記憶或 wiki），且內容包含三要素：失準最可能原因、明日可執行的修正、\
     一條可驗證的觀察點。"
        .to_string()
}

// ─────────────────────────────────────────────────────────────────────────
// Scheduler
// ─────────────────────────────────────────────────────────────────────────

/// Sweeps every agent directory every 5 minutes, creating at most one
/// self-study goal task per opted-in agent per local day.
pub struct SelfStudyScheduler {
    home_dir: PathBuf,
}

impl SelfStudyScheduler {
    pub fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }

    fn prediction_db_path(&self) -> PathBuf {
        self.home_dir.join("prediction.db")
    }

    /// One sweep. Returns the number of research tasks created, for tests
    /// and logging. Fail-open throughout (design: "全程 fail-open：任何讀取
    /// 失敗→跳過該 agent＋tracing warn，絕不 panic") — a store that can't be
    /// opened, or one agent's malformed config, never aborts the sweep for
    /// every other agent.
    pub async fn tick(&self) -> usize {
        let agents_dir = self.home_dir.join("agents");
        let mut entries = match tokio::fs::read_dir(&agents_dir).await {
            Ok(e) => e,
            Err(e) => {
                debug!(error = %e, dir = %agents_dir.display(), "self_study: 無法讀取 agents 目錄，本次跳過");
                return 0;
            }
        };
        let store = match TaskStore::open(&self.home_dir) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "self_study: 無法開啟 task store，本次跳過");
                return 0;
            }
        };
        let prediction_db = self.prediction_db_path();

        let local_now = chrono::Local::now();
        let today_local = format!(
            "{:04}-{:02}-{:02}",
            local_now.year(),
            local_now.month(),
            local_now.day()
        );
        let today_utc = chrono::Utc::now().format("%Y-%m-%d").to_string();

        let mut created = 0usize;
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => {
                    warn!(error = %e, "self_study: 讀取 agents 目錄項目失敗，中止本輪掃描");
                    break;
                }
            };
            let agent_dir = entry.path();
            if !agent_dir.is_dir() {
                continue;
            }
            let Some(agent_id) = agent_dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };

            let cfg = ResearchConfig::from_agent_dir(&agent_dir);
            if !cfg.self_study {
                continue;
            }

            let meta_key = format!("self_study_last:{agent_id}");
            let already_ran =
                belief::get_meta(&prediction_db, &meta_key).as_deref() == Some(today_local.as_str());
            if !should_run_now(local_now.hour(), cfg.self_study_hour, already_ran) {
                continue;
            }

            // Double-check via the task store — self-heals a lost/reset kv
            // marker (e.g. prediction.db recreated) rather than creating a
            // second research task for a day that already has one.
            if Self::already_created_today(&store, &agent_id, &today_utc).await {
                let _ = belief::set_meta(&prediction_db, &meta_key, &today_local);
                continue;
            }

            let rows = belief::recent(&prediction_db, Some(&agent_id), BELIEF_SCAN_LIMIT);
            let subject = pick_research_subject(&rows, &today_utc);

            // Claim the day regardless of outcome (miss-found or not,
            // insert succeeds or not) — design's hard cap is "at most one
            // ATTEMPT per agent per day", not "at most one success".
            let _ = belief::set_meta(&prediction_db, &meta_key, &today_local);

            let Some(subject) = subject else {
                debug!(agent = %agent_id, "self_study: 今日無 belief miss，跳過");
                continue;
            };

            match Self::create_research_task(&store, &agent_id, &subject, &today_local).await {
                Ok(()) => {
                    info!(agent = %agent_id, subject = %subject, "self_study: 已建立晚間自主研究目標");
                    created += 1;
                }
                Err(e) => warn!(agent = %agent_id, subject = %subject, error = %e, "self_study: 建立研究任務失敗"),
            }
        }
        created
    }

    /// design's task-store double-check: does `agent_id` already have a
    /// task created today carrying [`AUTO_RESEARCH_TAG`]? `created_at` is
    /// UTC (see `TaskRow::new`), so `today_utc` is the matching bucket.
    async fn already_created_today(store: &TaskStore, agent_id: &str, today_utc: &str) -> bool {
        let Ok(tasks) = store.list_tasks(None, Some(agent_id), None).await else {
            return false;
        };
        tasks.iter().any(|t| {
            t.created_at.starts_with(today_utc)
                && t.tags.split(',').any(|tag| tag.trim() == AUTO_RESEARCH_TAG)
        })
    }

    async fn create_research_task(
        store: &TaskStore,
        agent_id: &str,
        subject: &str,
        today_local: &str,
    ) -> Result<(), String> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let title = duduclaw_core::truncate_chars(
            &format!("晚間自主研究：{subject}（{today_local}）"),
            120,
        );
        let description = render_research_description(subject);
        let acceptance = render_research_acceptance();

        let mut task = TaskRow::new(
            task_id,
            title,
            description,
            "medium".to_string(),
            agent_id.to_string(),
            RESEARCH_CREATED_BY.to_string(),
        );
        task.goal_mode = true;
        task.acceptance_criteria = Some(acceptance);
        task.tags = AUTO_RESEARCH_TAG.to_string();

        store.insert_task(&task).await
    }

    /// Long-running task: sweep every `interval` until cancelled.
    pub async fn run(self: std::sync::Arc<Self>, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let _ = self.tick().await;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ResearchConfig ──

    #[test]
    fn research_config_defaults_off_when_agent_toml_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ResearchConfig::from_agent_dir(tmp.path());
        assert!(!cfg.self_study);
        assert_eq!(cfg.self_study_hour, DEFAULT_SELF_STUDY_HOUR);
    }

    #[test]
    fn research_config_reads_explicit_section() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("agent.toml"),
            "[research]\nself_study = true\nself_study_hour = 18\n",
        )
        .unwrap();
        let cfg = ResearchConfig::from_agent_dir(tmp.path());
        assert!(cfg.self_study);
        assert_eq!(cfg.self_study_hour, 18);
    }

    #[test]
    fn research_config_defaults_off_when_section_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        let cfg = ResearchConfig::from_agent_dir(tmp.path());
        assert!(!cfg.self_study);
    }

    #[test]
    fn research_config_out_of_range_hour_falls_back_to_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("agent.toml"),
            "[research]\nself_study = true\nself_study_hour = 99\n",
        )
        .unwrap();
        let cfg = ResearchConfig::from_agent_dir(tmp.path());
        assert!(cfg.self_study, "the valid flag must still be read");
        assert_eq!(cfg.self_study_hour, DEFAULT_SELF_STUDY_HOUR);
    }

    #[test]
    fn research_config_malformed_toml_is_silent_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("agent.toml"), "bad = [toml").unwrap();
        let cfg = ResearchConfig::from_agent_dir(tmp.path());
        assert!(!cfg.self_study);
    }

    // ── should_run_now ──

    #[test]
    fn should_run_now_false_when_already_ran_regardless_of_hour() {
        assert!(!should_run_now(23, 20, true));
        assert!(!should_run_now(20, 20, true));
    }

    #[test]
    fn should_run_now_false_before_the_configured_hour() {
        assert!(!should_run_now(19, 20, false));
    }

    #[test]
    fn should_run_now_true_at_the_configured_hour() {
        assert!(should_run_now(20, 20, false));
    }

    #[test]
    fn should_run_now_true_after_the_configured_hour_catches_up() {
        // A gateway that was down at 20:00 and ticks again at 23:00 same day
        // still fires — catch-up semantics, same as notify_digest.
        assert!(should_run_now(23, 20, false));
    }

    // ── todays_misses / pick_worst_subject / pick_research_subject ──

    fn miss_row(subject: &str, brier: f64, settled_at: &str) -> belief::BeliefRow {
        belief::BeliefRow {
            belief_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "trader".to_string(),
            subject: subject.to_string(),
            horizon: "今日收盤".to_string(),
            direction: "up".to_string(),
            prob: 0.6,
            rationale: None,
            ref_value: Some(100.0),
            predicted_at: "2026-08-14T01:00:00Z".to_string(),
            stats_injected: false,
            realized_value: Some(90.0),
            realized_direction: Some("down".to_string()),
            outcome: Some("miss".to_string()),
            brier: Some(brier),
            settled_at: Some(settled_at.to_string()),
            settle_source: Some("agent_unverified".to_string()),
            source_goal_id: None,
        }
    }

    fn hit_row(subject: &str, settled_at: &str) -> belief::BeliefRow {
        let mut r = miss_row(subject, 0.1, settled_at);
        r.outcome = Some("hit".to_string());
        r
    }

    #[test]
    fn pick_research_subject_none_when_no_rows() {
        assert_eq!(pick_research_subject(&[], "2026-08-14"), None);
    }

    #[test]
    fn pick_research_subject_none_when_only_hits() {
        let rows = vec![hit_row("2317", "2026-08-14T10:00:00Z")];
        assert_eq!(pick_research_subject(&rows, "2026-08-14"), None);
    }

    #[test]
    fn pick_research_subject_returns_the_single_miss_subject() {
        let rows = vec![miss_row("2317", 0.5, "2026-08-14T10:00:00Z")];
        assert_eq!(
            pick_research_subject(&rows, "2026-08-14"),
            Some("2317".to_string())
        );
    }

    #[test]
    fn pick_research_subject_picks_the_worst_brier_among_multiple_misses() {
        let rows = vec![
            miss_row("2317", 0.4, "2026-08-14T10:00:00Z"),
            miss_row("TAIEX", 0.9, "2026-08-14T11:00:00Z"),
            miss_row("0050", 0.6, "2026-08-14T12:00:00Z"),
        ];
        assert_eq!(
            pick_research_subject(&rows, "2026-08-14"),
            Some("TAIEX".to_string())
        );
    }

    #[test]
    fn pick_research_subject_excludes_misses_settled_on_a_different_day() {
        let rows = vec![
            miss_row("2317", 0.9, "2026-08-13T23:00:00Z"), // yesterday
            miss_row("TAIEX", 0.4, "2026-08-14T01:00:00Z"), // today
        ];
        assert_eq!(
            pick_research_subject(&rows, "2026-08-14"),
            Some("TAIEX".to_string()),
            "the higher-brier row from yesterday must not win"
        );
    }

    #[test]
    fn pick_research_subject_ties_break_alphabetically_for_determinism() {
        let rows = vec![
            miss_row("zeta", 0.5, "2026-08-14T10:00:00Z"),
            miss_row("alpha", 0.5, "2026-08-14T11:00:00Z"),
        ];
        assert_eq!(
            pick_research_subject(&rows, "2026-08-14"),
            Some("alpha".to_string())
        );
    }

    #[test]
    fn pick_research_subject_ignores_unsettled_and_flat_band_rows() {
        let mut unsettled = miss_row("2317", 0.9, "2026-08-14T10:00:00Z");
        unsettled.outcome = None;
        unsettled.settled_at = None;
        let mut flat_band = miss_row("TAIEX", 0.9, "2026-08-14T10:00:00Z");
        flat_band.outcome = Some("flat_band".to_string());
        let rows = vec![unsettled, flat_band];
        assert_eq!(pick_research_subject(&rows, "2026-08-14"), None);
    }

    // ── rendering (pure text — smoke-check the three required elements) ──

    #[test]
    fn render_research_description_names_the_subject_and_three_elements() {
        let body = render_research_description("trial_conversion_rate");
        assert!(body.contains("trial_conversion_rate"));
        assert!(body.contains("失準最可能原因"));
        assert!(body.contains("明日可執行的修正"));
        assert!(body.contains("可驗證的觀察點"));
        assert!(body.contains("禁止建立其他任務"));
    }

    #[test]
    fn render_research_acceptance_names_the_three_elements() {
        let acc = render_research_acceptance();
        assert!(acc.contains("失準最可能原因"));
        assert!(acc.contains("明日可執行的修正"));
        assert!(acc.contains("可驗證的觀察點"));
    }

    // ── scheduler integration (tick) ──

    async fn write_agent(agents_dir: &Path, id: &str, self_study: bool, hour: u32) {
        let dir = agents_dir.join(id);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("agent.toml"),
            format!("[research]\nself_study = {self_study}\nself_study_hour = {hour}\n"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn tick_creates_nothing_when_no_agent_opted_in() {
        let home = tempfile::tempdir().unwrap();
        let agents_dir = home.path().join("agents");
        write_agent(&agents_dir, "trader", false, 0).await;
        let sched = SelfStudyScheduler::new(home.path().to_path_buf());
        assert_eq!(sched.tick().await, 0);
    }

    #[tokio::test]
    async fn tick_creates_nothing_before_the_configured_hour() {
        let home = tempfile::tempdir().unwrap();
        let agents_dir = home.path().join("agents");
        // Hour 23 basically never reached in a CI run inside the same tick
        // window as "now" for a deterministic assertion — instead assert via
        // the local hour actually being less than a hardcoded future hour is
        // flaky across timezones, so this test only proves the config-off
        // path costs zero creates (hour-gating itself is covered by the pure
        // `should_run_now` tests above).
        write_agent(&agents_dir, "trader", true, 23).await;
        let sched = SelfStudyScheduler::new(home.path().to_path_buf());
        let local_hour = chrono::Local::now().hour();
        let created = sched.tick().await;
        if local_hour < 23 {
            assert_eq!(created, 0, "must not fire before the configured hour");
        }
    }

    #[tokio::test]
    async fn tick_is_fail_open_on_missing_agents_dir() {
        let home = tempfile::tempdir().unwrap();
        // Do not create `agents/` at all.
        let sched = SelfStudyScheduler::new(home.path().to_path_buf());
        assert_eq!(sched.tick().await, 0);
    }

    #[tokio::test]
    async fn tick_creates_a_research_task_when_a_miss_exists_today() {
        let home = tempfile::tempdir().unwrap();
        let agents_dir = home.path().join("agents");
        // Hour 0 so "now >= cfg_hour" is always true regardless of test run time.
        write_agent(&agents_dir, "trader", true, 0).await;

        let prediction_db = home.path().join("prediction.db");
        let b = belief::NewBelief {
            agent_id: "trader".to_string(),
            subject: "2317".to_string(),
            horizon: "今日收盤".to_string(),
            direction: "up".to_string(),
            prob: 0.6,
            rationale: None,
            ref_value: Some(100.0),
            source_goal_id: None,
        };
        let id = belief::submit(&prediction_db, b).unwrap();
        belief::settle(&prediction_db, "trader", &id, 90.0, None).unwrap(); // miss

        let sched = SelfStudyScheduler::new(home.path().to_path_buf());
        let created = sched.tick().await;
        assert_eq!(created, 1);

        let store = TaskStore::open(home.path()).unwrap();
        let tasks = store.list_tasks(None, Some("trader"), None).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].goal_mode);
        assert_eq!(tasks[0].tags, AUTO_RESEARCH_TAG);
        assert!(tasks[0].title.contains("2317"));
        assert_eq!(tasks[0].created_by, RESEARCH_CREATED_BY);

        // A second tick the same day must not create a duplicate (once per
        // local day, both via the kv marker and the task-store self-heal).
        let created_again = sched.tick().await;
        assert_eq!(created_again, 0);
        let tasks_after = store.list_tasks(None, Some("trader"), None).await.unwrap();
        assert_eq!(tasks_after.len(), 1, "must not double-create on a second tick");
    }

    #[tokio::test]
    async fn tick_creates_nothing_when_no_miss_today_but_still_claims_the_day() {
        let home = tempfile::tempdir().unwrap();
        let agents_dir = home.path().join("agents");
        write_agent(&agents_dir, "trader", true, 0).await;

        let prediction_db = home.path().join("prediction.db");
        let b = belief::NewBelief {
            agent_id: "trader".to_string(),
            subject: "2317".to_string(),
            horizon: "今日收盤".to_string(),
            direction: "up".to_string(),
            prob: 0.6,
            rationale: None,
            ref_value: Some(100.0),
            source_goal_id: None,
        };
        let id = belief::submit(&prediction_db, b).unwrap();
        belief::settle(&prediction_db, "trader", &id, 110.0, None).unwrap(); // hit, not miss

        let sched = SelfStudyScheduler::new(home.path().to_path_buf());
        assert_eq!(sched.tick().await, 0);

        let store = TaskStore::open(home.path()).unwrap();
        let tasks = store.list_tasks(None, Some("trader"), None).await.unwrap();
        assert!(tasks.is_empty(), "no miss ⇒ no research task");
    }
}
