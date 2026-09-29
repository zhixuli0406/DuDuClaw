//! `[goal_loop]` / `[goal_defaults]` configuration, the restart-resume
//! reconciliation and the small pure predicates the driver reads.
//! Moved verbatim out of `goal_loop.rs` (file-size split).

use super::*;

/// L4: count DISTINCT topical signals in `lower` for one keyword list.
///
/// Two fixes over the old `list.iter().filter(|kw| lower.contains(*kw)).count()`:
///
/// 1. **Anchored matching** (project convention #2: no unanchored `contains`
///    for a routing/classification decision) — uses
///    [`duduclaw_core::word_contains_ci`] instead of raw `contains`, so e.g.
///    the OPS keyword `"send"` no longer matches inside `"sender"`.
/// 2. **Overlap de-duplication** — a single CJK phrase like `"寫程式碼"`
///    contains THREE of [`GOAL_KIND_CODING_KEYWORDS`] as substrings
///    (`"程式"`, `"程式碼"`, `"寫程式"`), all sharing the same characters.
///    Counting each independently inflates the score to 3 for what is one
///    coding-topic signal. This merges the first-occurrence byte span of
///    every matched keyword and counts each resulting overlap CLUSTER once,
///    not each keyword.
pub(super) fn count_distinct_hits(lower: &str, list: &[&str]) -> usize {
    let mut spans: Vec<(usize, usize)> = list
        .iter()
        .filter(|kw| duduclaw_core::word_contains_ci(lower, kw))
        .filter_map(|kw| lower.find(kw).map(|start| (start, start + kw.len())))
        .collect();
    if spans.is_empty() {
        return 0;
    }
    spans.sort_unstable();
    let mut clusters = 0usize;
    let mut current_end: Option<usize> = None;
    for (start, end) in spans.drain(..) {
        match current_end {
            Some(ce) if start < ce => current_end = Some(ce.max(end)),
            _ => {
                clusters += 1;
                current_end = Some(end);
            }
        }
    }
    clusters
}

/// X1 方案 2 reads this from `dispatch_engine`'s settle path so an
/// audit-derived causal claim can name the goal kind it accompanied, without
/// depending on `[task_forward_model]` (which is off by default and is the
/// only other place a `GoalKind` is computed).
pub(crate) fn derive_goal_kind(text: &str) -> GoalKind {
    let difficulty = crate::dispatch_engine::classify_goal_difficulty(text);
    let lower = text.to_lowercase();

    let hits = |list: &[&str]| count_distinct_hits(&lower, list);

    let ops = hits(&GOAL_KIND_OPS_KEYWORDS);
    let research = hits(&GOAL_KIND_RESEARCH_KEYWORDS);
    let planning = hits(&GOAL_KIND_PLANNING_KEYWORDS);
    let coding = hits(&GOAL_KIND_CODING_KEYWORDS);

    // Ops/external signals dominate regardless of difficulty — an external
    // side-effect changes the expected tool classes (Net/Exec) and artifact
    // shape (ExternalEffect) more than length/complexity does.
    if ops > 0 && ops >= research && ops >= planning && ops >= coding {
        return GoalKind::OpsOrExternal;
    }
    if coding > 0 && coding >= research && coding >= planning {
        return match difficulty {
            crate::dispatch_engine::Difficulty::Simple => GoalKind::CodingSimple,
            crate::dispatch_engine::Difficulty::Complex => GoalKind::CodingComplex,
        };
    }
    if research > 0 && research >= planning {
        return GoalKind::ResearchOrQa;
    }
    if planning > 0 {
        return GoalKind::PlanningOrDoc;
    }
    // No topical keyword hit at all: fall back on the difficulty split alone
    // (coding is the modal goal-loop workload — Complex without any other
    // signal still gets the coarse Coding bucket rather than Unknown, which
    // would leave GoalKind's statistics permanently unbucketed for the
    // common case).
    match difficulty {
        crate::dispatch_engine::Difficulty::Simple => GoalKind::Unknown,
        crate::dispatch_engine::Difficulty::Complex => GoalKind::CodingComplex,
    }
}

/// Tuning for the goal loop driver. Read from `config.toml [goal_loop]`.
///
/// `#[serde(default)]` at the container level means every field falls back to
/// [`GoalLoopConfig::default`] when absent, so a missing or partial section is
/// always valid.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GoalLoopConfig {
    /// Hard cap on total dispatches per task **for Complex goals** (independent
    /// of the judge's `max_retries`; both apply, stricter wins). Exceed ⇒
    /// `needs_human`. Iterative Kanban lowered this 8→5: under critique-revise
    /// feedback two rounds capture 76-95% of the gain (arXiv:2604.10508) and
    /// rounds past ~4 trend to zero (Self-Refine 2303.17651), so 5 is the
    /// evidence-backed hard ceiling. Override in `config.toml [goal_loop]`.
    pub iteration_cap: u32,
    /// D4 item 3: iteration cap for **Simple goals** (MaAS dynamic depth — a
    /// simple goal that has not converged in a few tries is unlikely to, so it
    /// escalates sooner and cheaper). The per-task effective cap is chosen by
    /// [`crate::dispatch_engine::classify_goal_difficulty`].
    pub iteration_cap_simple: u32,
    /// Iterative Kanban soft cap: once a task's `revision_round` reaches this,
    /// it is flagged `diminishing` (amber "報酬遞減" badge) but NOT blocked —
    /// only `iteration_cap` blocks. Default 3 (2604.10508: gains flatten after
    /// round 2-3). Passed to `reject_review` via the dispatch engine.
    pub soft_cap: i64,
    /// Wall-clock budget measured from the task's `created_at`, in hours.
    /// Exceed ⇒ `needs_human`.
    pub wall_clock_hours: i64,
    /// Max simultaneously in-flight goal tasks (spawn-storm guard).
    pub max_concurrent: usize,
    /// Driver tick cadence (seconds).
    pub tick_secs: u64,
    /// A dispatched task the agent has not picked up within this many seconds is
    /// considered stalled and may be re-dispatched (counts as an iteration).
    pub stalled_secs: i64,
    /// H22 (workbuddy-codebuddy §2.5): after this many minutes with no
    /// observable progress signal, an already-picked-up (`in_progress`) goal
    /// task gets ONE "已執行 X 分鐘未回報進度" notice — Activity Feed plus the
    /// launching conversation — at most once per round.
    ///
    /// Strictly a report. It never re-dispatches, escalates, or cancels
    /// anything; `stalled_secs` / `iteration_cap` / `wall_clock_hours` remain
    /// the only guards that act. `0` (or any negative value) disables it
    /// entirely, and the disabled path costs zero queries.
    pub progress_report_minutes: i64,
    /// H6 (WP-B) / WP-E (2026-08 P1 rollout): `"auto"` or `"pause"`
    /// (**default since WP-E**). Read as a raw string (not a typed enum) so
    /// an unrecognized value degrades to the safe default instead of
    /// failing `GoalLoopConfig` deserialization for the whole `[goal_loop]`
    /// section — same lenient-string convention
    /// [`AutonomyLevel::from_toml_str`] uses elsewhere in this file. Resolve
    /// via [`GoalLoopConfig::resume_on_restart`].
    pub resume_on_restart: String,
    /// H10: whether `capture_round_state` computes and injects the tool-call
    /// streak advisory (`goal_loop/signals.rs`, deepseek-harness §2.16
    /// `repeat-tool-reminder`) into the next round's `<state>` block.
    /// Default `true` — this is purely advisory text (it can never change
    /// what the agent is allowed to do, only nudge what it is told), so
    /// unlike most goal-loop gates the safe default is ON, not off. Set
    /// `false` in `config.toml [goal_loop]` to silence it entirely.
    pub tool_streak_advisory: bool,
}

impl Default for GoalLoopConfig {
    fn default() -> Self {
        Self {
            iteration_cap: 5,
            iteration_cap_simple: 3,
            soft_cap: 3,
            wall_clock_hours: 24,
            max_concurrent: 3,
            tick_secs: 30,
            stalled_secs: 600,
            // H22: 10 minutes of silence is where a human starts wondering
            // whether anything is happening at all. Deliberately the same
            // intuition as `stalled_secs` (600s) about how long "quiet" is
            // tolerable — but this one only reports, on an already-picked-up
            // task, where `stalled_secs` re-dispatches an unclaimed one.
            progress_report_minutes: 10,
            // WP-E (2026-08 P1 rollout, user-approved spec change): default
            // flipped from "auto" to "pause". H6 shipped opt-in-only first;
            // this is the deliberate follow-up so an unattended gateway
            // restart/crash-recovery no longer silently resumes driving a
            // goal nobody re-confirmed is still safe. Set back to "auto" in
            // `config.toml [goal_loop]` (or via the dashboard's Automation
            // tab) to restore the pre-WP-E behavior.
            resume_on_restart: "pause".to_string(),
            // H10: on by default — advisory-only, never behavior-changing.
            tool_streak_advisory: true,
        }
    }
}

impl GoalLoopConfig {
    /// Load `[goal_loop]` from `<home>/config.toml`. The section is parsed in
    /// isolation (from a generic `toml::Table`), so unrelated config sections
    /// can never make this fail — absent / malformed ⇒ defaults.
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("goal_loop") {
            Some(section) => section
                .clone()
                .try_into::<GoalLoopConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }

    /// H6: the parsed [`ResumeOnRestart`] policy. Unrecognized / empty /
    /// whitespace-only values fall back to [`ResumeOnRestart::Auto`] — the
    /// pre-H6, byte-identical-behavior default.
    pub fn resume_on_restart(&self) -> ResumeOnRestart {
        ResumeOnRestart::from_str_lenient(&self.resume_on_restart)
    }
}

/// H6: whether the goal loop resumes in-flight goal tasks automatically
/// after a gateway process restart, or requires human confirmation first.
///
/// Two independent harnesses (deepseek-harness's Ralph loop, grok-build's
/// `goal_tracker.rs`) converged on the same conclusion: a durable
/// autonomous loop must never resurrect itself after a process restart — an
/// unattended process crash/redeploy must not silently resume driving a
/// goal that a human has not re-confirmed is still safe to continue. See
/// H6 in `commercial/docs/DESIGN-harness-borrowings-2026-08.md`.
///
/// H6 shipped with [`GoalLoopConfig`]'s default string set to `"auto"`
/// (byte-identical to pre-H6 behavior) pending the P1 rollout decision in
/// that design doc. WP-E (2026-08, user-approved) is that rollout: the
/// config default is now `"pause"` — see [`GoalLoopConfig::default`]. This
/// enum's own `#[default]` stays [`ResumeOnRestart::Auto`] deliberately: it
/// is the fail-safe fallback [`ResumeOnRestart::from_str_lenient`] returns
/// for an unrecognized/malformed config string, which must never
/// double-negative into the *stricter* behavior on a typo — that is a
/// separate concept from "what a fresh install ships with".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResumeOnRestart {
    /// The driver picks up any non-terminal goal_mode task exactly as if
    /// the process had never stopped. Pre-H6 behavior; no longer the
    /// `GoalLoopConfig` default since WP-E, but still the safe fallback for
    /// an unparseable `resume_on_restart` string (see the enum doc above).
    #[default]
    Auto,
    /// At gateway boot, every non-terminal goal_mode task is escalated to
    /// `needs_human` (reason `gateway_restart`) instead of being silently
    /// resumed. See [`pause_inflight_on_restart`].
    Pause,
}

impl ResumeOnRestart {
    /// Unknown / empty / whitespace-only ⇒ [`ResumeOnRestart::Auto`] (the
    /// safe, behavior-preserving default) — a typo in config must never
    /// silently switch to the OTHER mode's semantics in either direction.
    fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "pause" => ResumeOnRestart::Pause,
            _ => ResumeOnRestart::Auto,
        }
    }
}

/// H6: boot-time reconciliation for `resume_on_restart = "pause"`. Scans
/// every genuinely in-flight (`revising` / `in_progress` / `review` /
/// `blocked`) `goal_mode` task and escalates it to `needs_human`
/// (reason `gateway_restart`), reusing [`GoalLoopDriver::escalate`]'s
/// well-tested path (grant revocation, activity post, visit-graph /
/// state-capture cleanup) via a throwaway driver instance — safe because at
/// boot time no live in-memory in-flight tracking exists yet for any task,
/// so an empty `inflight` map is equivalent to a freshly-started driver's
/// real state. The existing `needs_human` channel push then happens
/// naturally on the driver's own first tick via
/// [`GoalLoopDriver::reconcile_needs_human`] — no separate notify path is
/// duplicated here.
///
/// No-op (returns `0`) when `resume_on_restart` resolves to `Auto` —
/// byte-identical to pre-H6 behavior, but note this is no longer the
/// `GoalLoopConfig` default since WP-E (see [`ResumeOnRestart`]).
/// Deliberately called ONLY from the gateway boot path, never from a
/// hot-reload respawn — see the sole caller's doc comment
/// (`MethodHandler::pause_inflight_goal_tasks_on_restart` in `handlers.rs`)
/// for why conflating the two would be wrong.
pub async fn pause_inflight_on_restart(
    store: Arc<TaskStore>,
    queue: Arc<MessageQueue>,
    home_dir: &Path,
) -> usize {
    let cfg = GoalLoopConfig::from_home(home_dir);
    if cfg.resume_on_restart() != ResumeOnRestart::Pause {
        return 0;
    }
    let driver =
        GoalLoopDriver::new(store.clone(), queue, cfg).with_home_dir(home_dir.to_path_buf());
    // Queue states a goal task holds BEFORE its first dispatch (`todo`,
    // `pending`) are deliberately NOT escalated: the user confirmed the goal
    // at creation and no round has run yet, so dispatching it after boot is
    // starting the confirmed work — not silently resuming an interrupted,
    // unconfirmed run. The documented contract (CHANGELOG / goal-loop guide)
    // promises pausing tasks "still running"; live verification 2026-08-15
    // caught the earlier all-non-terminal scan pausing queued tasks too.
    const INFLIGHT_STATUSES: &[&str] = &["revising", "in_progress", "review", "blocked"];
    let mut paused = 0usize;
    for status in INFLIGHT_STATUSES {
        let tasks = match store.tasks_in_status(status).await {
            Ok(t) => t,
            Err(e) => {
                warn!(%status, error = %e, "goal loop: resume_on_restart scan failed for this status (continuing)");
                continue;
            }
        };
        for t in tasks {
            if !t.goal_mode {
                continue;
            }
            let mut dummy_inflight: HashMap<String, InFlight> = HashMap::new();
            if let Err(e) = driver
                .escalate(
                    &mut dummy_inflight,
                    &t,
                    "gateway_restart",
                    crate::pause_reason::PauseReason::Restart,
                )
                .await
            {
                warn!(task = %t.id, error = %e, "goal loop: resume_on_restart escalate failed for this task (continuing)");
                continue;
            }
            paused += 1;
        }
    }
    if paused > 0 {
        info!(
            paused,
            "goal loop: resume_on_restart=pause escalated in-flight goal tasks to needs_human at boot"
        );
    }
    paused
}

/// Iterative Kanban default `review` WIP limit. The board flags the review
/// column amber and shows a Little's-Law wait estimate once the queue depth
/// exceeds this. Override in `config.toml [task_board] review_wip_limit`.
pub const DEFAULT_REVIEW_WIP_LIMIT: i64 = 10;

/// Read `[task_board] review_wip_limit` from `<home>/config.toml`. Absent /
/// malformed / non-positive ⇒ [`DEFAULT_REVIEW_WIP_LIMIT`] (a WIP limit ≤ 0 is
/// meaningless, so it falls back rather than disabling the guard silently).
pub fn review_wip_limit(home_dir: &Path) -> i64 {
    let path = home_dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return DEFAULT_REVIEW_WIP_LIMIT;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return DEFAULT_REVIEW_WIP_LIMIT;
    };
    table
        .get("task_board")
        .and_then(|s| s.get("review_wip_limit"))
        .and_then(|v| v.as_integer())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_REVIEW_WIP_LIMIT)
}

// ── Goal assignment form v2 (design-market-belief-loop-2026-08.md §6,
// G1) ────────────────────────────────────────────────────────────

/// Built-in baseline risk-boundary text (design §6 G1): the five-line default
/// applied to every goal-mode task whose assign form left `risk_boundary`
/// blank. Used both as the deployment default and as the fail-open fallback
/// when `config.toml [goal_defaults] baseline_boundary` is absent, malformed,
/// or unreadable — a bad/missing config must never leave a goal task with NO
/// boundary text injected.
pub const DEFAULT_BASELINE_BOUNDARY: &str = "\
- 遵循當地法規。\n\
- 資安紅線：不得外洩秘密或憑證。\n\
- 不得繞過或說服自己繞過任何硬性風控與平台護欄。\n\
- 金流與不可逆動作須經人審。\n\
- 對外公開發言須經人審。";

/// Read `[goal_defaults] baseline_boundary` from `<home>/config.toml`.
/// Absent / malformed / unreadable / blank ⇒ [`DEFAULT_BASELINE_BOUNDARY`]
/// (fail-open — same "parsed in isolation, defaults on any failure" pattern
/// as [`GoalLoopConfig::from_home`] and [`review_wip_limit`], so a broken
/// unrelated config section can never take this down and a goal task is
/// never dispatched with zero boundary text). Deployment-customizable so an
/// operator can tailor the default to local regulatory / industry context.
pub fn baseline_boundary(home_dir: &Path) -> String {
    let path = home_dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return DEFAULT_BASELINE_BOUNDARY.to_string();
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return DEFAULT_BASELINE_BOUNDARY.to_string();
    };
    table
        .get("goal_defaults")
        .and_then(|s| s.get("baseline_boundary"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_BASELINE_BOUNDARY.to_string())
}

/// Resolve the effective risk-boundary text for a task: its own explicit
/// `risk_boundary` when non-blank, else the deployment baseline. Shared by
/// both G2 injection points (the goal-loop work message and the MAV judge
/// prompt) so the two never drift out of sync.
pub fn effective_risk_boundary(task_risk_boundary: Option<&str>, home_dir: &Path) -> String {
    task_risk_boundary
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| baseline_boundary(home_dir))
}

/// G3: which deadline actually fired — the escalation message tells a human
/// whether it was the global wall-clock budget or the goal's own explicit
/// `deadline_at` override, instead of one generic "goal-loop deadline" for
/// both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeadlineHit {
    /// The global `[goal_loop] wall_clock_hours` budget (from `created_at`).
    WallClock,
    /// The per-task `deadline_at` override (design §6 G3).
    TaskDeadline,
}

/// G3: resolves whether `now` has passed either the global wall-clock budget
/// (`created_at + wall_clock_hours`) or an explicit per-task `deadline_at` —
/// whichever is EARLIER wins, i.e. `deadline_at` can only *tighten* the
/// effective deadline, never loosen it past the global budget (design §6:
/// "deadline 覆蓋全域 wall-clock（取較早者）"). Pure and unit-testable without
/// constructing a [`GoalLoopDriver`]. Unparseable timestamps degrade to "does
/// not apply" for that half of the check (fail-open on the deadline only —
/// same contract the pre-G3 wall-clock-only check had; the iteration cap
/// still bounds the loop regardless).
pub(crate) fn resolve_deadline_hit(
    created_at: &str,
    deadline_at: Option<&str>,
    wall_clock_hours: i64,
    now: DateTime<Utc>,
) -> Option<DeadlineHit> {
    let wall_clock_deadline = DateTime::parse_from_rfc3339(created_at)
        .ok()
        .map(|c| c.with_timezone(&Utc) + ChronoDuration::hours(wall_clock_hours));
    let task_deadline = deadline_at
        .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
        .map(|d| d.with_timezone(&Utc));

    match (wall_clock_deadline, task_deadline) {
        (Some(wc), Some(td)) => {
            let effective = wc.min(td);
            if now < effective {
                None
            } else if td <= wc {
                Some(DeadlineHit::TaskDeadline)
            } else {
                Some(DeadlineHit::WallClock)
            }
        }
        (Some(wc), None) => (now >= wc).then_some(DeadlineHit::WallClock),
        (None, Some(td)) => (now >= td).then_some(DeadlineHit::TaskDeadline),
        (None, None) => None,
    }
}

/// Back-off before a task whose dispatch failed becomes a candidate again:
/// 60 s, 120 s, 240 s … (doubling per consecutive failure). Short enough that
/// a transient outage (llama-server still loading a model) self-heals within
/// a few minutes, long enough that a hard failure does not burn a round every
/// tick.
pub(super) fn dispatch_backoff_secs(failures: u32) -> i64 {
    60i64.saturating_mul(1i64 << failures.saturating_sub(1).min(6))
}

/// H22: pure predicate — how many whole minutes a task has gone without an
/// observable progress signal, when that exceeds the configured threshold.
///
/// `Some(elapsed_minutes)` ⇒ report; `None` ⇒ stay quiet. Disabled
/// (`threshold_minutes <= 0`) and clocks that run backwards (a `last_signal`
/// in the future — NTP correction, a hand-edited row) both return `None`:
/// the notice is a courtesy, so every ambiguous case degrades to silence
/// rather than to a wrong number in a user's chat.
pub(crate) fn no_progress_minutes(
    last_signal: DateTime<Utc>,
    now: DateTime<Utc>,
    threshold_minutes: i64,
) -> Option<i64> {
    if threshold_minutes <= 0 {
        return None;
    }
    let elapsed = (now - last_signal).num_minutes();
    (elapsed >= threshold_minutes).then_some(elapsed)
}
