//! Goal-loop survival table for `duduclaw weekly-report` (read-only).
//!
//! History can answer exactly one question about the retry limit: "if the
//! limit had been k, which accepted tasks would have been cut off, and how
//! many judged rounds would have been saved?" Both numbers are monotone in k,
//! so the answer is a table, not a simulation.
//!
//! Two caveats are kept visible in the output:
//! - a lower iteration cap changes whether a round runs Solo or Team, so only
//!   all-solo tasks are comparable for that knob;
//! - rows recorded before the ledger columns existed have unknown knob values.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use duduclaw_gateway::prediction::calibration::{honest_label, wilson_bounds, HonestLabel};
use duduclaw_gateway::task_store::{TaskIterationRow, TaskRow, TaskStore};
use serde::Serialize;

/// Wilson critical value for the 95% interval.
const WILSON_Z: f64 = 1.96;

/// Below this many accepted tasks the verdict names the small sample.
const SMALL_SAMPLE: u64 = 30;

/// Page size for reading the task board (the store clamps to 200).
const PAGE: i64 = 200;

/// Statuses counted as finished. `failed` is a terminal state too (zombie
/// reclaim past the requeue cap); everything else is still in flight.
const FINISHED_STATUSES: [&str; 4] = ["done", "needs_human", "cancelled", "failed"];

// ─────────────────────────────────────────────────────────────────────────────
// Data model
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GoalSurvival {
    pub availability: String,
    pub unavailable_reason: Option<String>,
    /// Goal tasks created inside the window (finished + still running).
    pub tasks: u64,
    /// Finished tasks (status in `counted_statuses`).
    pub finished: u64,
    pub counted_statuses: Vec<String>,
    /// Goal tasks still in flight; excluded from every table.
    pub running_excluded: u64,
    /// Finished tasks without complete, consistent frozen retry-cap evidence.
    pub unknown_knobs: u64,
    /// Finished tasks escalated to a human at least once.
    pub escalated: u64,
    pub cancelled: u64,
    /// `done` tasks with no accepted round on record; cannot be placed in a
    /// table.
    pub done_without_round: u64,
    pub strata: Vec<Stratum>,
    /// Only protected, authenticated outcome-decision receipts qualify.
    pub human_approved_strata: Vec<Stratum>,
    pub human_approval_unknown: u64,
    pub human_approved_total: u64,
    pub human_approved_without_round: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stratum {
    pub max_retries: i64,
    /// `solo` | `team` | `unknown`.
    pub mode: String,
    pub difficulty: String,
    pub manual_retry: String,
    pub tasks: u64,
    pub accepted_total: u64,
    pub rows: Vec<SurvivalRow>,
    /// `supported` | `candidate` | `indistinguishable_from_luck`.
    pub label: String,
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SurvivalRow {
    pub k: u32,
    pub accepted_at_or_before_k: u64,
    pub lost_if_cap_k: u64,
    pub rounds_saved_if_cap_k: u64,
    /// `accepted_at_or_before_k / accepted_total`; `None` when nothing was
    /// accepted.
    pub survival: Option<f64>,
    pub wilson_lo: Option<f64>,
    pub wilson_hi: Option<f64>,
}

/// One finished task reduced to what the table needs.
#[derive(Debug, Clone)]
pub struct TaskFacts {
    pub status: String,
    pub max_retries: i64,
    pub mode: &'static str,
    /// Round of the first accepted verdict.
    pub accepted_round: Option<u32>,
    /// Distinct judged round numbers (verdict present).
    pub judged_rounds: Vec<u32>,
    pub knobs_known: bool,
    pub escalated: bool,
    pub difficulty: String,
    pub manual_retry: Option<bool>,
    pub human_approved: Option<bool>,
    pub human_approved_round: Option<u32>,
}

impl GoalSurvival {
    fn empty() -> Self {
        Self {
            availability: "available".into(),
            unavailable_reason: None,
            tasks: 0,
            finished: 0,
            counted_statuses: FINISHED_STATUSES.iter().map(|s| s.to_string()).collect(),
            running_excluded: 0,
            unknown_knobs: 0,
            escalated: 0,
            cancelled: 0,
            done_without_round: 0,
            strata: Vec::new(),
            human_approved_strata: Vec::new(),
            human_approval_unknown: 0,
            human_approved_total: 0,
            human_approved_without_round: 0,
        }
    }

    pub fn unavailable(reason: String) -> Self {
        let mut out = Self::empty();
        out.availability = "unavailable".into();
        out.unavailable_reason = Some(reason);
        out
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Collection
// ─────────────────────────────────────────────────────────────────────────────

/// Read goal tasks created since `window_start` and build the table.
pub async fn collect(
    tasks: &TaskStore,
    window_start: &DateTime<Utc>,
    agent_filter: Option<&str>,
) -> Result<GoalSurvival, String> {
    let mut rows: Vec<TaskRow> = Vec::new();
    // Archived goal tasks are still history, so page through both sets.
    for archived in [false, true] {
        let mut offset = 0i64;
        loop {
            let (page, _total) = tasks
                .list_tasks_paginated(
                    None,
                    agent_filter,
                    None,
                    Some(true),
                    Some(archived),
                    PAGE,
                    offset,
                )
                .await?;
            let n = page.len() as i64;
            rows.extend(page);
            if n < PAGE {
                break;
            }
            offset += n;
        }
    }

    let mut running = 0u64;
    let mut facts = Vec::new();
    for task in rows.iter().filter(|t| created_in_window(t, window_start)) {
        if !FINISHED_STATUSES.contains(&task.status.as_str()) {
            running += 1;
            continue;
        }
        let iters = tasks.list_iterations(&task.id).await?;
        facts.push(facts_from(task, &iters));
    }
    Ok(build(facts, running))
}

fn created_in_window(task: &TaskRow, window_start: &DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(&task.created_at)
        .map(|t| t.with_timezone(&Utc) >= *window_start)
        .unwrap_or(false)
}

/// Reduce a task and its iteration rows to [`TaskFacts`].
pub fn facts_from(task: &TaskRow, iters: &[TaskIterationRow]) -> TaskFacts {
    let mut judged: Vec<u32> = iters
        .iter()
        .filter(|r| r.verdict.is_some() && r.round >= 1)
        .map(|r| r.round as u32)
        .collect();
    judged.sort_unstable();
    judged.dedup();

    let accepted_round = iters
        .iter()
        .filter(|r| r.verdict.as_deref() == Some("accepted") && r.round >= 1)
        .map(|r| r.round as u32)
        .min();

    let any_team = iters.iter().any(|r| r.team_mode.as_deref() == Some("team"));
    let all_recorded_solo = iters.iter().all(|r| r.team_mode.as_deref() == Some("solo"));
    let none_recorded = iters.iter().all(|r| r.team_mode.is_none());
    let has_spec = task.team_spec_json.is_some();
    let mode = if any_team {
        "team"
    } else if !iters.is_empty() && all_recorded_solo {
        "solo"
    } else if !has_spec && none_recorded {
        "solo"
    } else if !has_spec && !any_team {
        // Some rounds say solo, others are unrecorded, and no team was frozen.
        "solo"
    } else {
        "unknown"
    };

    // All recorded rounds must carry one consistent frozen retry cap. The
    // current task row is mutable and is not historical knob evidence.
    let caps: Option<Vec<i64>> = iters
        .iter()
        .map(|r| {
            let value: serde_json::Value = serde_json::from_str(r.knobs_json.as_deref()?).ok()?;
            value.get("max_retries")?.as_i64().filter(|v| *v >= 0)
        })
        .collect();
    let cap = caps
        .filter(|v| !v.is_empty() && v.iter().all(|n| *n == v[0]))
        .map(|v| v[0]);
    // Round gate inputs may compress redispatches; only the protected
    // ledger plus complete dispatch counts can classify historical difficulty.
    TaskFacts {
        status: task.status.clone(),
        max_retries: cap.unwrap_or(-1),
        mode,
        accepted_round,
        judged_rounds: judged,
        knobs_known: cap.is_some(),
        escalated: task.status == "needs_human"
            || iters
                .iter()
                .any(|r| r.verdict.as_deref() == Some("escalated")),
        difficulty: "unknown".into(),
        manual_retry: None,
        human_approved: None,
        human_approved_round: None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Pure aggregation
// ─────────────────────────────────────────────────────────────────────────────

/// Build the table from finished-task facts plus the running-task count.
pub fn build(facts: Vec<TaskFacts>, running: u64) -> GoalSurvival {
    let mut out = GoalSurvival::empty();
    out.running_excluded = running;
    out.finished = facts.len() as u64;
    out.tasks = out.finished + running;

    let mut groups: BTreeMap<(i64, &'static str, String, &'static str), Vec<&TaskFacts>> =
        BTreeMap::new();
    let mut approved_facts = Vec::new();
    for f in &facts {
        if f.human_approved.is_none() {
            out.human_approval_unknown += 1;
        }
        if f.human_approved == Some(true) {
            out.human_approved_total += 1;
            if f.human_approved_round.is_some() {
                let mut approved = f.clone();
                // A protected outcome receipt supplies this subset's round.
                // The judge's verdict and task revision are unchanged.
                approved.accepted_round = f.human_approved_round;
                approved_facts.push(approved);
            } else {
                out.human_approved_without_round += 1;
            }
        }
        if !f.knobs_known {
            out.unknown_knobs += 1;
        }
        if f.escalated {
            out.escalated += 1;
        }
        if f.status == "cancelled" {
            out.cancelled += 1;
        }
        if f.status == "done" && f.accepted_round.is_none() {
            out.done_without_round += 1;
            continue;
        }
        let manual = match f.manual_retry {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        groups
            .entry((f.max_retries, f.mode, f.difficulty.clone(), manual))
            .or_default()
            .push(f);
    }

    for ((max_retries, mode, difficulty, manual), members) in groups {
        out.strata
            .push(stratum(max_retries, mode, &difficulty, manual, &members));
    }
    let mut approved_groups: BTreeMap<(i64, &'static str, String, &'static str), Vec<&TaskFacts>> =
        BTreeMap::new();
    for fact in &approved_facts {
        let manual = match fact.manual_retry {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        approved_groups
            .entry((fact.max_retries, fact.mode, fact.difficulty.clone(), manual))
            .or_default()
            .push(fact);
    }
    for ((cap, mode, difficulty, manual), members) in approved_groups {
        out.human_approved_strata
            .push(stratum(cap, mode, &difficulty, manual, &members));
    }
    out
}

fn stratum(
    max_retries: i64,
    mode: &str,
    difficulty: &str,
    manual: &str,
    members: &[&TaskFacts],
) -> Stratum {
    let accepted_total = members
        .iter()
        .filter(|f| f.accepted_round.is_some())
        .count() as u64;
    let max_round = members
        .iter()
        .flat_map(|f| f.judged_rounds.iter().copied().chain(f.accepted_round))
        .max()
        .unwrap_or(0);

    let rows: Vec<SurvivalRow> = (1..=max_round)
        .map(|k| {
            let within = members
                .iter()
                .filter(|f| f.accepted_round.is_some_and(|r| r <= k))
                .count() as u64;
            let saved: u64 = members
                .iter()
                .map(|f| f.judged_rounds.iter().filter(|&&r| r > k).count() as u64)
                .sum();
            let (lo, hi) = wilson_bounds(within, accepted_total, WILSON_Z);
            SurvivalRow {
                k,
                accepted_at_or_before_k: within,
                lost_if_cap_k: accepted_total - within,
                rounds_saved_if_cap_k: saved,
                survival: (accepted_total > 0).then(|| within as f64 / accepted_total as f64),
                wilson_lo: lo.is_finite().then_some(lo),
                wilson_hi: hi.is_finite().then_some(hi),
            }
        })
        .collect();

    // The verdict reads the k = 1 interval (the harshest cap). No PSR exists
    // for this statistic, so NaN is passed on purpose: `honest_label` then
    // treats it as failing the gate and can never answer `Supported`.
    let (lo, hi) = rows
        .first()
        .map(|r| {
            (
                r.wilson_lo.unwrap_or(f64::NAN),
                r.wilson_hi.unwrap_or(f64::NAN),
            )
        })
        .unwrap_or((f64::NAN, f64::NAN));
    let label = honest_label(lo, hi, f64::NAN);
    let (label_key, verdict) = match label {
        HonestLabel::Candidate => ("candidate", "還沒有已接受的任務，無法判斷。".to_string()),
        HonestLabel::IndistinguishableFromLuck if accepted_total < SMALL_SAMPLE => (
            "indistinguishable_from_luck",
            format!("只有 {accepted_total} 件已接受的任務，資料分不出重試上限的差別與運氣的差別。"),
        ),
        HonestLabel::IndistinguishableFromLuck => (
            "indistinguishable_from_luck",
            "缺少可靠度檢定，保守判定為看不出重試上限的差別與運氣的差別。".to_string(),
        ),
        HonestLabel::Supported => ("supported", "資料支持這個結論。".to_string()),
    };

    Stratum {
        max_retries,
        mode: mode.to_string(),
        difficulty: difficulty.to_owned(),
        manual_retry: manual.to_owned(),
        tasks: members.len() as u64,
        accepted_total,
        rows,
        label: label_key.to_string(),
        verdict,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Markdown
// ─────────────────────────────────────────────────────────────────────────────

fn mode_label(mode: &str) -> &'static str {
    match mode {
        "solo" => "單人模式",
        "team" => "團隊模式",
        _ => "模式不明",
    }
}

fn retry_cap_label(cap: i64) -> String {
    if cap < 0 {
        "不明".into()
    } else {
        cap.to_string()
    }
}

fn pct(v: Option<f64>) -> String {
    v.map(|x| format!("{:.1}%", x * 100.0))
        .unwrap_or_else(|| "—".into())
}

pub fn render_markdown(g: &GoalSurvival) -> String {
    let mut out = String::new();
    out.push_str("## 目標迴圈存活表\n\n");
    if g.availability == "unavailable" {
        out.push_str("無法讀取目標任務歷史；這不是區間內任務為零。\n\n");
        return out;
    }
    if g.tasks == 0 {
        out.push_str("_（區間內沒有目標任務）_\n\n");
        return out;
    }

    out.push_str(&format!(
        "統計區間內建立的目標任務共 {} 件，其中已結束 {} 件（完成、待人處理、取消、失敗）；仍在進行的 {} 件不列入。\n\n",
        g.tasks, g.finished, g.running_excluded
    ));
    out.push_str(
        "表中每一列回答：如果重試上限當時是 k，已接受的任務有幾件會被截斷、能少跑幾輪。\
一輪判決不等於一次派工（停滯與人工重試可能重派相同輪次）；輪數是不同 logical round 的描述統計，不能當作精確派工成本或迭代上限反事實。\n\n",
    );

    if g.strata.is_empty() {
        out.push_str("_（已結束的任務都沒有可用的輪次紀錄）_\n\n");
    }
    for s in &g.strata {
        out.push_str(&format!(
            "### 重試上限 {}・{}・難度 {}・人工重試 {}（{} 件，已接受 {} 件）\n\n",
            retry_cap_label(s.max_retries),
            mode_label(&s.mode),
            s.difficulty,
            s.manual_retry,
            s.tasks,
            s.accepted_total
        ));
        if s.mode != "solo" {
            out.push_str(
                "> 調低上限會改變某一輪用單人還是團隊跑，這組的輪次無法直接換算成較低上限，數字僅供參考。\n\n",
            );
        }
        if s.rows.is_empty() {
            out.push_str("_（沒有已判決的輪次）_\n\n");
        } else {
            out.push_str("| k | 第 k 輪內接受 | 會被截斷 | 可省輪數 | 存活比例（95% 區間） |\n");
            out.push_str("|---:|---:|---:|---:|---|\n");
            for r in &s.rows {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {}（{}–{}） |\n",
                    r.k,
                    r.accepted_at_or_before_k,
                    r.lost_if_cap_k,
                    r.rounds_saved_if_cap_k,
                    pct(r.survival),
                    pct(r.wilson_lo),
                    pct(r.wilson_hi),
                ));
            }
            out.push('\n');
        }
        out.push_str(&format!("**判讀**：{}\n\n", s.verdict));
    }

    out.push_str(&format!(
        "- 旋鈕值不明的任務：{} 件（舊紀錄沒有留下當時的設定）\n",
        g.unknown_knobs
    ));
    out.push_str(&format!("- 曾轉交人工處理：{} 件\n", g.escalated));
    out.push_str(&format!(
        "- 人工成果核准證據不明：{} 件（不把未留證據當作未核准）\n",
        g.human_approval_unknown
    ));
    out.push_str(&format!("- 已取消：{} 件\n", g.cancelled));
    out.push_str(&format!(
        "- 仍在進行（未列入）：{} 件\n",
        g.running_excluded
    ));
    if g.done_without_round > 0 {
        out.push_str(&format!(
            "- 已完成但查不到接受的輪次（未列入表中）：{} 件\n",
            g.done_without_round
        ));
    }
    out.push('\n');
    out.push_str("### 經人工核准子集（較強成果標籤）\n\n");
    out.push_str(&format!("可信的人工作業成果核准共 {} 件，其中 {} 件沒有可用的核准輪次；僅用核准紀錄綁定的實際輪次計算子集存活表。\n\n",g.human_approved_total,g.human_approved_without_round));
    if g.human_approved_strata.is_empty() {
        if g.human_approved_total > 0 {
            out.push_str(
                "_（有可信人工核准紀錄，但沒有可用的核准輪次；保留核准件數，不建立存活比例）_\n\n",
            );
        } else {
            out.push_str(
                "_（沒有可信的人工作業成果核准紀錄；開跑放行及一般 activity 不算成果核准）_\n\n",
            );
        }
    } else {
        let mut subset = g.clone();
        subset.strata = g.human_approved_strata.clone();
        subset.human_approved_strata.clear();
        // Render only the subset's grouped rows, avoiding recursive headings.
        for s in &subset.strata {
            out.push_str(&format!(
                "- 重試上限 {}，{}，難度 {}，人工重試 {}：{} 件，已接受 {} 件\n",
                retry_cap_label(s.max_retries),
                mode_label(&s.mode),
                s.difficulty,
                s.manual_retry,
                s.tasks,
                s.accepted_total
            ));
            for r in &s.rows {
                out.push_str(&format!("  - k={}：第 k 輪內接受 {}，會被截斷 {}，可省 logical 輪數 {}，存活 {}（95% Wilson {}–{}）\n",r.k,r.accepted_at_or_before_k,r.lost_if_cap_k,r.rounds_saved_if_cap_k,pct(r.survival),pct(r.wilson_lo),pct(r.wilson_hi)));
            }
            out.push_str(&format!("  - 判讀：{}\n", s.verdict));
        }
        out.push('\n');
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn goal_task(id: &str, status: &str, max_retries: i64) -> TaskRow {
        let mut t = TaskRow::new(
            id.into(),
            format!("goal {id}"),
            String::new(),
            "medium".into(),
            "agnes".into(),
            "test".into(),
        );
        t.goal_mode = true;
        t.status = status.into();
        t.max_retries = max_retries;
        t
    }

    /// Insert an iteration row by raw SQL (the store has no public writer
    /// that lets a test set every ledger column).
    fn insert_iter(
        home: &std::path::Path,
        task_id: &str,
        round: i64,
        verdict: Option<&str>,
        team_mode: Option<&str>,
        knobs: Option<&str>,
    ) {
        let conn = rusqlite::Connection::open(home.join("tasks.db")).expect("open tasks.db");
        conn.execute(
            "INSERT INTO task_iterations
                (task_id, round, dispatched_at, verdict, team_mode, knobs_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                task_id,
                round,
                Utc::now().to_rfc3339(),
                verdict,
                team_mode,
                knobs
            ],
        )
        .expect("insert iteration");
    }

    async fn seed_case_one(home: &std::path::Path) -> TaskStore {
        let store = TaskStore::open(home).expect("open store");
        for i in 0..7 {
            let id = format!("a{i}");
            store
                .insert_task(&goal_task(&id, "done", 3))
                .await
                .expect("insert");
            insert_iter(
                home,
                &id,
                1,
                Some("accepted"),
                Some("solo"),
                Some(r#"{"max_retries":3}"#),
            );
        }
        store
            .insert_task(&goal_task("b0", "done", 3))
            .await
            .expect("insert");
        insert_iter(
            home,
            "b0",
            1,
            Some("rejected"),
            Some("solo"),
            Some(r#"{"max_retries":3}"#),
        );
        insert_iter(
            home,
            "b0",
            2,
            Some("accepted"),
            Some("solo"),
            Some(r#"{"max_retries":3}"#),
        );
        for i in 0..3 {
            let id = format!("c{i}");
            store
                .insert_task(&goal_task(&id, "cancelled", 3))
                .await
                .expect("insert");
            insert_iter(
                home,
                &id,
                1,
                Some("rejected"),
                Some("solo"),
                Some(r#"{"max_retries":3}"#),
            );
        }
        store
    }

    fn window() -> DateTime<Utc> {
        Utc::now() - Duration::days(7)
    }

    #[tokio::test]
    async fn survival_table_counts_accepted_round_distribution() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = seed_case_one(dir.path()).await;
        let g = collect(&store, &window(), None).await.expect("collect");

        assert_eq!(g.tasks, 11);
        assert_eq!(g.cancelled, 3);
        assert_eq!(g.strata.len(), 1);
        let s = &g.strata[0];
        assert_eq!((s.max_retries, s.mode.as_str()), (3, "solo"));
        assert_eq!(s.accepted_total, 8);
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.rows[0].accepted_at_or_before_k, 7);
        assert_eq!(s.rows[1].accepted_at_or_before_k, 8);
        assert_eq!(s.rows[0].lost_if_cap_k, 1);
        assert_eq!(s.rows[1].lost_if_cap_k, 0);
        // Round 2 exists only for b0.
        assert_eq!(s.rows[0].rounds_saved_if_cap_k, 1);
        assert_eq!(s.label, "indistinguishable_from_luck");
        assert!(s.verdict.contains("運氣"));
        assert!(render_markdown(&g).contains("分不出"));
    }

    #[tokio::test]
    async fn team_task_lands_in_team_stratum() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open store");
        store
            .insert_task(&goal_task("t1", "done", 3))
            .await
            .expect("insert");
        insert_iter(
            dir.path(),
            "t1",
            1,
            Some("rejected"),
            Some("solo"),
            Some(r#"{"max_retries":3}"#),
        );
        insert_iter(
            dir.path(),
            "t1",
            2,
            Some("accepted"),
            Some("team"),
            Some(r#"{"max_retries":3}"#),
        );
        store
            .insert_task(&goal_task("s1", "done", 3))
            .await
            .expect("insert");
        insert_iter(
            dir.path(),
            "s1",
            1,
            Some("accepted"),
            Some("solo"),
            Some(r#"{"max_retries":3}"#),
        );

        let g = collect(&store, &window(), None).await.expect("collect");
        let modes: Vec<&str> = g.strata.iter().map(|s| s.mode.as_str()).collect();
        assert_eq!(modes, vec!["solo", "team"]);
        assert_eq!(g.strata[1].accepted_total, 1);
        assert!(render_markdown(&g).contains("團隊模式"));
    }

    #[tokio::test]
    async fn null_ledger_columns_count_as_unknown_knobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open store");
        store
            .insert_task(&goal_task("old", "done", 3))
            .await
            .expect("insert");
        insert_iter(dir.path(), "old", 1, Some("accepted"), None, None);
        store
            .insert_task(&goal_task("run", "in_progress", 3))
            .await
            .expect("insert");

        let g = collect(&store, &window(), None).await.expect("collect");
        assert_eq!(g.unknown_knobs, 1);
        assert_eq!(g.running_excluded, 1);
        assert_eq!(g.finished, 1);
        // NULL team_mode and NULL team spec reads as solo.
        assert_eq!(g.strata[0].mode, "solo");
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(json["strata"][0]["difficulty"], "unknown");
        assert_eq!(json["strata"][0]["manual_retry"], "unknown");
        assert_eq!(json["human_approval_unknown"], 1);
    }

    #[tokio::test]
    async fn empty_window_says_so_and_fabricates_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TaskStore::open(dir.path()).expect("open store");
        let g = collect(&store, &window(), None).await.expect("collect");
        assert_eq!(g.tasks, 0);
        assert!(g.strata.is_empty());
        let md = render_markdown(&g);
        assert!(md.contains("區間內沒有目標任務"));
        assert!(!md.contains('|'));
        let json = serde_json::to_value(&g).expect("json");
        assert_eq!(json["tasks"], 0);
    }

    #[tokio::test]
    async fn markdown_and_json_agree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = seed_case_one(dir.path()).await;
        let g = collect(&store, &window(), None).await.expect("collect");
        let json = serde_json::to_value(&g).expect("json");
        let md = render_markdown(&g);

        let rows = json["strata"][0]["rows"].as_array().expect("rows");
        for r in rows {
            let line = format!(
                "| {} | {} | {} | {} |",
                r["k"],
                r["accepted_at_or_before_k"],
                r["lost_if_cap_k"],
                r["rounds_saved_if_cap_k"]
            );
            assert!(md.contains(&line), "missing row: {line}\n{md}");
        }
        assert_eq!(json["cancelled"], 3);
        assert!(md.contains("已取消：3 件"));
        assert_eq!(json["strata"][0]["accepted_total"], 8);
    }

    #[test]
    fn no_accepted_task_yields_candidate_label() {
        let f = TaskFacts {
            status: "cancelled".into(),
            max_retries: 3,
            mode: "solo",
            accepted_round: None,
            judged_rounds: vec![1, 2],
            knobs_known: true,
            escalated: false,
            difficulty: "unknown".into(),
            manual_retry: None,
            human_approved: None,
            human_approved_round: None,
        };
        let g = build(vec![f], 0);
        assert_eq!(g.strata[0].label, "candidate");
        assert_eq!(g.strata[0].rows[0].survival, None);
        assert_eq!(g.strata[0].rows[0].rounds_saved_if_cap_k, 1);
    }

    #[test]
    fn survival_malformed_or_changed_frozen_knobs_never_use_current_task_values() {
        let task = goal_task("history", "done", 99);
        let iteration = |knobs: Option<&str>, difficulty: Option<&str>| TaskIterationRow {
            id: 1,
            task_id: task.id.clone(),
            round: 1,
            dispatched_at: String::new(),
            submitted_at: None,
            judged_at: None,
            verdict: Some("accepted".into()),
            judge_feedback: None,
            feedback_class: None,
            verdict_json: None,
            dispatch_count: 1,
            state_hash: None,
            repeat_streak: None,
            worker_excerpt: None,
            evaluator_verdict: None,
            iter_seq: None,
            team_mode: Some("solo".into()),
            gate_inputs_json: difficulty.map(str::to_owned),
            state_block_json: None,
            knobs_json: knobs.map(str::to_owned),
            pause_reason: None,
        };
        let valid = iteration(
            Some(r#"{"max_retries":3}"#),
            Some(r#"{"goal_difficulty":"simple"}"#),
        );
        let facts = facts_from(&task, &[valid.clone()]);
        assert_eq!(facts.max_retries, 3);
        assert!(facts.knobs_known);
        assert_eq!(
            facts.difficulty, "unknown",
            "round inputs lack protected dispatch-completeness evidence"
        );
        for invalid in [
            None,
            Some("{}"),
            Some("not json"),
            Some(r#"{"max_retries":-1}"#),
        ] {
            let facts = facts_from(&task, &[valid.clone(), iteration(invalid, None)]);
            assert!(!facts.knobs_known);
            assert_eq!(facts.max_retries, -1);
            assert_eq!(facts.difficulty, "unknown");
        }
        let facts = facts_from(
            &task,
            &[
                valid,
                iteration(
                    Some(r#"{"max_retries":4}"#),
                    Some(r#"{"goal_difficulty":"complex"}"#),
                ),
            ],
        );
        assert!(!facts.knobs_known);
        assert_eq!(facts.max_retries, -1);
        assert_eq!(
            facts.difficulty, "unknown",
            "redispatch completeness cannot be reconstructed from round gate inputs"
        );
    }

    #[test]
    fn survival_unbound_human_approval_does_not_borrow_judge_rounds_or_invent_a_subset() {
        for (accepted, judged) in [(Some(2), vec![1, 2]), (None, vec![])] {
            let report = build(
                vec![TaskFacts {
                    status: "done".into(),
                    max_retries: 3,
                    mode: "solo",
                    accepted_round: accepted,
                    judged_rounds: judged,
                    knobs_known: true,
                    escalated: false,
                    difficulty: "simple".into(),
                    manual_retry: Some(false),
                    human_approved: Some(true),
                    human_approved_round: None,
                }],
                0,
            );
            assert_eq!(report.human_approved_total, 1);
            assert!(
                report.human_approved_strata.is_empty(),
                "a real approval without its actual iteration has no survival table"
            );
            let json = serde_json::to_value(&report).unwrap();
            assert_eq!(json["human_approved_without_round"], 1);
            let markdown = render_markdown(&report);
            assert!(markdown.contains("有可信人工核准紀錄，但沒有可用的核准輪次"));
            assert!(!markdown.contains("沒有可信的人工作業成果核准紀錄"));
        }
    }
}
