//! Goal-loop **turn signals** — the four zero-LLM extractors that read a
//! dispatch round's output (judge feedback, `(state, action)` history, tool
//! activity, completion text) and hand the driver a comparable signal.
//!
//! Merged 2026-09-29 (audit O8) from the four single-purpose modules
//! `goal_gap_fingerprint` / `goal_visit_graph` / `goal_tool_streak` /
//! `goal_bail_detect`; each source module's design rationale is kept
//! verbatim as the section banner below it, and the old crate-root paths
//! stay available as re-exports in `lib.rs` for one release.
//!
//! Nothing here performs I/O beyond the shared `tool_calls.jsonl` reader,
//! and nothing here vetoes a dispatch — every output is either a hash to
//! compare or an advisory string for the next round's `<state>` block.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use tokio::sync::Mutex;

use crate::goal_loop::state::short_hash;
use crate::tool_activity::{ToolActivityRecord, read_tool_activity_records};

// ══════════════════════════════════════════════════════════════════════
// goal_gap_fingerprint ══════════════════════════════════════════════
// ══════════════════════════════════════════════════════════════════════
// Gap fingerprinting for the goal loop's no-progress guard — **H4**
// (`commercial/docs/DESIGN-harness-borrowings-2026-08.md` WP-B, borrowed
// from grok-build's `gap_fingerprint`, `session/goal_tracker.rs:1208`).
//
// ## Problem this replaces
//
// [`crate::goal_state::StateBlock::hash_input`] folds the LATEST judge
// rejection reason into the A2 no-progress guard's `state_hash` verbatim
// (after NFKC-normalize + whitespace-collapse — see `short_hash` /
// `normalize_for_hash`). That is still, in substance, a byte comparison: a
// judge that rewords the exact same underlying gap ("missing error
// handling in `goal_loop.rs:120`" vs. "you forgot to validate at
// `goal_loop.rs:120`") produces two different hashes, so the no-progress
// guard never fires even though the agent is provably stuck on the same
// spot — the two-round oscillation escalation silently loses its signal to
// cosmetic rewording.
//
// [`gap_fingerprint`] extracts the durable part of a rejection reason —
// `path:line` citations and backtick-quoted key tokens (function/variable
// names, error identifiers) — and normalizes it so a reworded-but-same gap
// collapses to an identical fingerprint. `path:line` extraction here
// answers "no citation" for the ANY case per docs; when a `path:line`
// citation or key token is present, a scratch/temp path segment (which
// frequently contains a random UUID/hash and would otherwise make every
// citation of the same logical file appear unique — grok's own rationale)
// is normalized to a `<scratch>` placeholder before hashing.
//
// ## Fallback contract
//
// When NEITHER a `path:line` citation NOR a backtick key token can be
// extracted, [`gap_fingerprint`] returns `None` and the caller
// (`goal_loop/state.rs::StateBlock::hash_input`) falls back to the literal
// (NFKC-normalized) feedback text — i.e. byte-identical to the pre-H4
// behavior. This keeps the change behavior-compatible for prose-only
// rejection reasons (the common case for research/analysis goals with no
// file citations at all).

/// Matches a `path:line` or `path:line:col` citation. Requires the "path"
/// half to contain a `.` followed by a short alphabetic extension (`.rs`,
/// `.ts`, `.py`, …) so ordinary sentences with a colon (times, ratios,
/// labels) are not mistaken for citations. Deliberately permissive on the
/// path character class (letters/digits/`_`/`.`/`/`/`\`/`-`) — judges cite
/// paths in many shapes (relative, absolute, Windows) and over-matching a
/// token here only ever adds one more entry to the fingerprint set, it
/// never causes a false "no citation" fallback.
static CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([A-Za-z0-9_][A-Za-z0-9_./\\-]*\.[A-Za-z]{1,10}):(\d{1,7})(?::(\d{1,5}))?")
        .expect("CITATION_RE must compile")
});

/// Matches a backtick-quoted key token, e.g. `` `foo_bar()` `` or
/// `` `TypeError` `` — the common way a judge cites a specific
/// function/variable/error identifier in prose feedback.
static KEY_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`\n]{1,80})`").expect("KEY_TOKEN_RE must compile"));

/// Hard cap on how many extracted tokens feed one fingerprint — bounds
/// pathological input (a feedback string with dozens of citations) without
/// affecting realistic judge feedback, which cites at most a handful of
/// spots.
const MAX_FINGERPRINT_TOKENS: usize = 12;

/// Segment names treated as scratch/temp markers regardless of case.
const SCRATCH_SEGMENT_NAMES: &[&str] = &["tmp", "temp", "scratch"];

/// Extract a stagnation "gap fingerprint" from one rejection-reason string:
/// `path:line[:col]` citations and backtick-quoted key tokens, normalized
/// (scratch-path segments → `<scratch>`, lowercased, deduped, sorted) so a
/// judge that rewords the same underlying gap still produces an identical
/// fingerprint. Returns `None` when no citation or key token can be
/// extracted at all — callers MUST fall back to literal-text comparison in
/// that case (see module docs' "Fallback contract").
pub fn gap_fingerprint(feedback: &str) -> Option<String> {
    let tokens = extract_tokens(feedback);
    if tokens.is_empty() {
        return None;
    }

    let mut normalized: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
    normalized.sort();
    normalized.dedup();
    Some(normalized.join("\u{1}"))
}

/// WP-4F: the human-facing twin of [`gap_fingerprint`] — the same
/// `path:line` citations and backtick key tokens, but case-preserved and in
/// original occurrence order (only [`gap_fingerprint`] needs the
/// lowercased/sorted form, for stable hash comparison). Used to render a
/// "what's still missing" list (e.g. the goal-loop budget-exhausted
/// escalation note) and, via its length, to rank rounds by how many concrete
/// gaps remain (fewer ⇒ closer to done). Deduplication is case-insensitive
/// (so `goal_loop.rs:120` and `GOAL_LOOP.rs:120` collapse to one entry) but
/// keeps the first-seen casing. Empty when no citation or key token can be
/// extracted — same fallback contract as `gap_fingerprint`.
pub fn gap_tokens(feedback: &str) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for t in extract_tokens(feedback) {
        if seen.insert(t.to_lowercase()) {
            out.push(t);
        }
    }
    out
}

/// Shared extraction pass behind [`gap_fingerprint`] / [`gap_tokens`]:
/// `path:line[:col]` citations first, then backtick-quoted key tokens,
/// scratch-path segments normalized, capped at [`MAX_FINGERPRINT_TOKENS`].
/// Case-preserved, not deduplicated — callers apply their own
/// normalization/dedup policy on top.
fn extract_tokens(feedback: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();

    for cap in CITATION_RE.captures_iter(feedback) {
        let path = normalize_scratch_path(&cap[1]);
        let line = &cap[2];
        tokens.push(match cap.get(3) {
            Some(col) => format!("{path}:{line}:{}", col.as_str()),
            None => format!("{path}:{line}"),
        });
        if tokens.len() >= MAX_FINGERPRINT_TOKENS {
            return tokens;
        }
    }

    for cap in KEY_TOKEN_RE.captures_iter(feedback) {
        let token = cap[1].trim();
        if token.is_empty() {
            continue;
        }
        tokens.push(normalize_scratch_path(token));
        if tokens.len() >= MAX_FINGERPRINT_TOKENS {
            break;
        }
    }

    tokens
}

/// Replace scratch/temp-looking path segments with a stable `<scratch>`
/// placeholder so two citations of "the same logical scratch file" under
/// different random temp directories collapse to the same fingerprint —
/// otherwise a UUID or content-hash directory name would make the
/// fingerprint unique on every single rejection, defeating the whole
/// purpose (grok's own documented rationale for the same normalization
/// step). Non-scratch segments are passed through unchanged (case folding
/// happens once, later, over the whole fingerprint).
fn normalize_scratch_path(path: &str) -> String {
    path.split(['/', '\\'])
        .map(|seg| {
            if is_scratch_segment(seg) {
                "<scratch>"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn is_scratch_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    let lower = seg.to_ascii_lowercase();
    if SCRATCH_SEGMENT_NAMES.contains(&lower.as_str()) {
        return true;
    }
    is_uuid_like(seg) || is_hex_hash_like(seg)
}

/// `8-4-4-4-12` hex UUID shape (the classic random-temp-dir-name pattern).
fn is_uuid_like(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8usize, 4, 4, 4, 12]
            .iter()
            .zip(parts.iter())
            .all(|(&len, p)| p.len() == len && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A bare hex-looking segment of meaningful length (content-hash temp dir
/// names, e.g. `9f8c1e2a3b4d5e6f`). 8+ hex chars is a deliberately
/// conservative floor — short real words practically never satisfy
/// "all-hex" (most contain a non-hex letter like `g`/`l`/`o`), so this
/// rarely false-positives on genuine path segments.
fn is_hex_hash_like(s: &str) -> bool {
    s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

// ══════════════════════════════════════════════════════════════════════
// goal_visit_graph ══════════════════════════════════════════════════
// ══════════════════════════════════════════════════════════════════════
// `(state_hash, action_digest)` visit graph — **A2**, structural loop
// detection for the goal loop (Graph-Based Exploration, arXiv:2512.24156:
// explicit exploration bookkeeping makes a repeated attempt structurally
// visible instead of relying on judge-feedback text similarity — reported
// third place on the private ARC-AGI-3 leaderboard using this primitive
// alone, no LLM cost).
//
// ## What this replaces
//
// The pre-A2 goal loop escalated to `needs_human` when two consecutive
// judge rejections carried byte-identical `judge_feedback` text
// (`goal_loop.rs`'s old `normalize_feedback` comparison). That signal is
// narrow: two rounds that fail for the same underlying reason but get
// worded slightly differently by the judge never trip it, and it forgets
// everything except the single most recent rejection. This module gives
// the driver an explicit, cumulative record of "have we been here before"
// instead — see `goal_loop.rs::tick_once`'s per-candidate loop for the
// call sites (`peek_streak` / `commit_dispatch` replace the old two-round
// text comparison one-for-one; the external contract — the
// `goal_loop.oscillation` activity event type and `"goal-loop no-progress
// oscillation"` judge_feedback text — is kept byte-identical, because
// `topology_evolution.rs` (D5) queries that exact event-type string for
// its own analytics and is out of scope for this change).
//
// ## Persistence: in-memory only, and why
//
// Held as a plain in-memory `HashMap` behind a `tokio::Mutex`, matching
// the existing `GoalLoopDriver::inflight` pattern (iteration counters,
// kickoff state, notify-retry counters — none of that crosses a gateway
// restart either). Two options were weighed:
//
// 1. **In-memory `HashMap`** (chosen): zero schema/IO cost; a restart
//    clears the visit history the same way it already clears `InFlight`'s
//    iteration counters — a tolerated, pre-existing behavior of this
//    driver, not a new weakness introduced here.
// 2. **Persist to a task-row column**: durable across restarts, but the
//    task card is explicit that cross-process sharing is not a
//    requirement — a single gateway process is the only reader/writer of
//    this graph, and a restart mid-goal already resets other in-memory
//    driver state today. Paying for a column + JSON (de)serialization on
//    every round to survive an event (mid-goal gateway restart) the driver
//    already tolerates losing state across is cost without behavioral
//    benefit.
//
// (1) is strictly cheaper and consistent with the driver's existing state
// model, so it is what is implemented. If cross-restart survival becomes a
// real requirement later, promoting this to a task-row column (mirroring
// `goal_loop/state.rs`'s `GoalStateSnapshot`, which IS persisted because its
// content is user/agent-visible and worth keeping) is an isolated
// follow-up — this module's public API (state_hash in, counts out) would
// not need to change shape.

/// Per-task visit bookkeeping.
#[derive(Debug, Clone, Default)]
struct TaskVisits {
    /// `(state_hash, action_digest) -> times this exact pair has been seen`.
    pair_counts: HashMap<(String, String), u32>,
    /// `state_hash` committed at the most recent actual dispatch.
    last_dispatch_state_hash: Option<String>,
    /// Consecutive actual dispatches whose `state_hash` equalled the
    /// previous one (see `commit_dispatch`).
    unchanged_streak: u32,
}

/// The goal loop's `(state_hash, action)` visit graph. One instance is
/// shared (behind `Arc`) by a `GoalLoopDriver` for the gateway process's
/// lifetime.
#[derive(Debug, Default)]
pub struct GoalVisitGraph {
    tasks: Mutex<HashMap<String, TaskVisits>>,
}

impl GoalVisitGraph {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// What this task's unchanged-streak WOULD become if it were dispatched
    /// right now with `state_hash` — read-only, does not mutate. Call
    /// BEFORE deciding whether to escalate (mirrors the pre-A2 oscillation
    /// guard's placement: read state before the commit that follows a
    /// successful dispatch decision).
    pub async fn peek_streak(&self, task_id: &str, state_hash: &str) -> u32 {
        let tasks = self.tasks.lock().await;
        match tasks.get(task_id) {
            Some(v) if v.last_dispatch_state_hash.as_deref() == Some(state_hash) => {
                v.unchanged_streak + 1
            }
            _ => 1,
        }
    }

    /// Commit an actual dispatch: record `state_hash` as this task's latest
    /// dispatched state, updating the unchanged-streak accordingly. Call
    /// exactly once per real dispatch, after the decision to dispatch this
    /// tick is final (not on a tick that merely considered and deferred).
    /// Returns the new streak value.
    pub async fn commit_dispatch(&self, task_id: &str, state_hash: &str) -> u32 {
        let mut tasks = self.tasks.lock().await;
        let entry = tasks.entry(task_id.to_string()).or_default();
        entry.unchanged_streak = if entry.last_dispatch_state_hash.as_deref() == Some(state_hash) {
            entry.unchanged_streak + 1
        } else {
            1
        };
        entry.last_dispatch_state_hash = Some(state_hash.to_string());
        entry.unchanged_streak
    }

    /// Whether `state_hash` already has SOME recorded action repeated ≥2
    /// times for this task — used to annotate the dispatch prompt's
    /// excluded-approaches section ("this exact thing was already tried
    /// from this exact state and failed at least twice; don't repeat it").
    pub async fn has_repeated_action(&self, task_id: &str, state_hash: &str) -> bool {
        let tasks = self.tasks.lock().await;
        tasks
            .get(task_id)
            .map(|v| {
                v.pair_counts
                    .iter()
                    .any(|((sh, _), count)| sh == state_hash && *count >= 2)
            })
            .unwrap_or(false)
    }

    /// Record one completed round's `(state_hash, action_digest)` pair,
    /// returning the pair's new visit count. Call once per round, after the
    /// round's outcome (the agent's completion text / tool activity) is
    /// known — see `goal_loop.rs::GoalLoopDriver::capture_round_state`.
    pub async fn record_round(&self, task_id: &str, state_hash: &str, action_digest: &str) -> u32 {
        let mut tasks = self.tasks.lock().await;
        let entry = tasks.entry(task_id.to_string()).or_default();
        let key = (state_hash.to_string(), action_digest.to_string());
        let count = entry.pair_counts.entry(key).or_insert(0);
        *count += 1;
        *count
    }

    /// Drop all tracking for `task_id` — call when the task reaches a
    /// terminal state (A2 lifecycle requirement: clean up at task end so
    /// the in-memory map does not grow unbounded across a long-running
    /// gateway's lifetime).
    pub async fn clear_task(&self, task_id: &str) {
        self.tasks.lock().await.remove(task_id);
    }

    #[cfg(test)]
    async fn task_count(&self) -> usize {
        self.tasks.lock().await.len()
    }
}

/// Action digest for one round: the set of distinct tool categories used
/// (from the shared `tool_calls.jsonl` audit trail) plus a normalized hash
/// of the round's final reply text head. Combined with [`crate::goal_state::state_hash`]
/// this forms the A2 visit-graph key.
///
/// Read-only best-effort scan of `tool_calls.jsonl` via the shared
/// `crate::tool_activity::read_tool_activity_records` helper (WP-A3
/// extracted this out of `dispatch_engine.rs` into its own `pub(crate)`
/// module — this function was written before that extraction landed and
/// now reuses it instead of keeping its own duplicate reader).
pub fn action_digest(
    home_dir: &Path,
    agent_id: &str,
    since: &str,
    until: &str,
    result_text: &str,
) -> String {
    let tools = tool_categories(home_dir, agent_id, since, until);
    let head = duduclaw_core::truncate_chars(result_text.trim(), 200);
    let combined = format!("{}\u{1}{head}", tools.join(","));
    crate::goal_state::short_hash(&combined)
}

/// Distinct tool names invoked by `agent_id` in `[since, until]`, sorted.
/// Missing/unreadable/unparseable audit file ⇒ empty (never fails the
/// caller over an observability gap — inherited from
/// `crate::tool_activity::read_tool_activity_records`'s fail-open
/// contract).
fn tool_categories(home_dir: &Path, agent_id: &str, since: &str, until: &str) -> Vec<String> {
    let records =
        crate::tool_activity::read_tool_activity_records(home_dir, agent_id, since, until);
    let set: std::collections::BTreeSet<String> =
        records.into_iter().map(|r| r.tool_name).collect();
    set.into_iter().collect()
}

// ══════════════════════════════════════════════════════════════════════
// goal_tool_streak ══════════════════════════════════════════════════
// ══════════════════════════════════════════════════════════════════════
// Tool-call streak advisory for the goal loop — **H10**
// (`research/harness-2026-08/deepseek-harness.md` §2.16
// `repeat-tool-reminder`, alongside grok-build's §2.2 "action
// stationarity" framing).
//
// ## What this catches that A2 (`goal_loop/signals.rs`) doesn't
//
// A2 flags a round that repeats a PRIOR round's whole `(state, action)`
// pair — it needs at least two full dispatch/judge cycles to notice.
// This module looks INSIDE a single round's tool activity: an agent that
// calls the exact same tool with the exact same (masked) arguments three,
// five, eight times in a row within one round is stuck well before the
// judge ever sees a result. Zero LLM cost, purely advisory — dsh's own
// framing is "the decision stays with the model", so this only ever
// injects a text hint into the NEXT dispatch round's `<state>` block
// (`goal_state::StateBlock::tool_streak_hint`); it never blocks, retries,
// or vetoes a dispatch.
//
// ## Signal source
//
// Reuses [`crate::tool_activity::read_tool_activity_records`] — the same
// `tool_calls.jsonl` tail-scoped-by-`(agent_id, since, until)` reader every
// other goal-loop evidence consumer (`goal_visit_graph::action_digest`,
// the A3 forward model) already shares, so there is no second audit-log
// parser. Records come back in file order, which is chronological (the
// audit log is append-only — see `duduclaw_security::audit`).
//
// ## Normalized parameter signature
//
// `ToolActivityRecord::input_text` is the audit writer's own MASKED input
// capture (secrets already redacted upstream). This module folds it
// through [`crate::goal_state::short_hash`] — the same CJK-safe
// NFKC-normalize + whitespace-collapse + SHA-256-prefix primitive the A2
// visit graph's `state_hash` already uses — so two calls whose masked
// input differs only by incidental whitespace/fullwidth-punctuation still
// count as "the same call", while a genuinely different argument breaks
// the streak. A call with no captured input text (writer never recorded
// one) normalizes to the empty string, which is still a stable, comparable
// signature — repeated argument-less calls to the same tool (e.g. a
// polling tool with no params) legitimately form a streak too.
//
// ## Threshold ladder
//
// `[3, 5, 8]` — identical to dsh's `repeat-tool-reminder`. Each tier's text
// is stricter than the last but always advisory: 3 asks the agent to
// re-read its last result before calling again, 5 suggests changing
// approach, 8 strongly suggests converging or asking for human help via
// `tasks_block`.

/// Escalating advisory thresholds — mirrors dsh's `[3, 5, 8]` ladder.
pub const THRESHOLDS: [u32; 3] = [3, 5, 8];

/// One round's longest same-tool/same-params streak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreakHit {
    pub tool_name: String,
    pub len: u32,
}

/// Longest run of consecutive calls sharing the same `(tool_name,
/// normalized_input_signature)` pair, scanning `records` in order (assumed
/// chronological — true for anything sourced from the append-only
/// `tool_calls.jsonl`). Ties are broken in favor of the LATEST run: the most
/// recent repeated behavior is the one most relevant to advise on for the
/// NEXT round, not necessarily the first one that happened to be longest.
///
/// Returns `None` for an empty input (nothing to report — never fabricates
/// a streak of length 0).
pub fn longest_streak(records: &[ToolActivityRecord]) -> Option<StreakHit> {
    let mut best: Option<StreakHit> = None;

    let mut current_tool: Option<&str> = None;
    let mut current_sig: Option<String> = None;
    let mut current_len: u32 = 0;

    for record in records {
        let sig = short_hash(record.input_text.as_deref().unwrap_or("").trim());
        let same_as_current = current_tool == Some(record.tool_name.as_str())
            && current_sig.as_deref() == Some(sig.as_str());
        if same_as_current {
            current_len += 1;
        } else {
            current_tool = Some(&record.tool_name);
            current_sig = Some(sig);
            current_len = 1;
        }
        let ge_best = best.as_ref().map(|b| current_len >= b.len).unwrap_or(true);
        if ge_best {
            best = Some(StreakHit {
                tool_name: record.tool_name.clone(),
                len: current_len,
            });
        }
    }

    best
}

/// Escalating zh-TW advisory text for one streak hit, or `None` when the
/// streak has not yet crossed the lowest threshold (3) — advisory-only, no
/// injection at all below that bar.
pub fn advisory_text(hit: &StreakHit) -> Option<String> {
    // Highest threshold reached, not exact match — a streak of 6 is still
    // "past 5", not "waiting for 8".
    let tier = THRESHOLDS.iter().copied().filter(|&t| hit.len >= t).max()?;
    let text = match tier {
        3 => format!(
            "已連續 {} 次呼叫同一工具「{}」且參數相同,建議先重讀上一次的執行結果,\
             確認是否已經取得所需資訊,避免重複呼叫浪費輪次。",
            hit.len, hit.tool_name
        ),
        5 => format!(
            "已連續 {} 次呼叫同一工具「{}」且參數相同,目前的做法可能沒有進展,\
             建議換一個方法或角度切入,而不是繼續重複同樣的呼叫。",
            hit.len, hit.tool_name
        ),
        _ => format!(
            "已連續 {} 次呼叫同一工具「{}」且參數相同,強烈建議停止重複嘗試:\
             直接收斂目前已取得的結果回報,或改用 tasks_block 說明受阻原因並求助。",
            hit.len, hit.tool_name
        ),
    };
    Some(text)
}

/// Read `tool_calls.jsonl` for `agent_id` in `[since, until]` and compute
/// this round's longest same-tool/same-params streak. Missing/unreadable/
/// unparseable audit file ⇒ `None` (never fails the caller over an
/// observability gap — same fail-open contract as
/// `tool_activity::read_tool_activity_records` and
/// `goal_visit_graph::action_digest`).
pub fn detect_tool_streak(
    home_dir: &Path,
    agent_id: &str,
    since: &str,
    until: &str,
) -> Option<StreakHit> {
    let records = read_tool_activity_records(home_dir, agent_id, since, until);
    longest_streak(&records)
}

// ══════════════════════════════════════════════════════════════════════
// goal_bail_detect ══════════════════════════════════════════════════
// ══════════════════════════════════════════════════════════════════════
// Premature-stop ("bail") detection for the goal loop — **H5**
// (`commercial/docs/DESIGN-harness-borrowings-2026-08.md` WP-B, borrowed
// from grok-build's `goal_stop_detector.rs`, nine `^`-anchored regexes
// compared only against the turn's last non-empty paragraph).
//
// ## Design
//
// An agent working an autonomous goal round sometimes ends its turn with
// language that *sounds* like a wrap-up but is not judge-verified
// completion: "I'll continue this later", a self-signed `VERDICT: PASS`,
// deflecting the decision back to a human that is not actually in the
// loop, and so on. None of these are failures the MAV judge would
// necessarily catch by content alone (the claimed work may even be
// correct) — they are a *process* smell worth surfacing as telemetry and
// as a hint for the next round, independent of whatever the judge decides.
//
// [`detect_bail_pattern`] runs a fixed panel of nine named zh+en anchored
// patterns against ONLY the last non-empty paragraph of the agent's
// completion text (`^`-anchored, mirroring grok's own anti-false-positive
// design — a stray mid-paragraph mention like "I don't want to give up on
// this" must not trigger `giving_up`). Patterns are deliberately narrow:
// per the source design, a broad "deferral" style was intentionally
// excluded because its false-positive rate would drown the signal.
//
// Each pattern is a source-level constant with its own regression test —
// see the `tests` module below — so a future edit to the phrasing can't
// silently regress recall for an existing pattern without a red test.

/// One named bail pattern: `(name, regex source)`. `name` is the stable
/// telemetry label (Prometheus label value + activity metadata) — never
/// renamed without also updating any dashboard query against it.
///
/// Each regex is `(?i)^...` — case-insensitive, anchored to the START of
/// the (already-trimmed) last non-empty paragraph. A short, common
/// lead-in (punctuation/whitespace) is tolerated by `^\s*` where natural
/// phrasing calls for it; the anchor's job is to reject the pattern
/// appearing buried mid-paragraph, not to require the literal first
/// character to match.
// NOTE: every pattern is written on a SINGLE line deliberately. Raw string
// literals (`r"..."`) do not support Rust's `\<newline>` line-continuation
// (that only applies to normal, non-raw strings) and regex's verbose `(?x)`
// mode would strip the literal spaces inside multi-word phrases like
// "check back later" — so a multi-line raw string here would silently
// embed literal newline/indentation characters into the compiled pattern
// instead of just being source formatting. Single-line keeps the compiled
// pattern exactly what it looks like.
const BAIL_PATTERNS_SRC: &[(&str, &str)] = &[
    // 1. unable_to_proceed — agent states it literally cannot continue.
    (
        "unable_to_proceed",
        r"(?i)^\s*(i\s*(?:am|'m)?\s*(?:currently\s+)?unable to (?:proceed|continue|complete this)\b|i\s*can(?:not|'t)\s*(?:proceed|continue)(?:\s+(?:with this|further))?\b|(?:我)?(?:目前)?無法(?:繼續|再繼續|完成)(?:這(?:個|項)?)?(?:任務|工作)?)",
    ),
    // 2. giving_up — agent explicitly gives up.
    (
        "giving_up",
        r"(?i)^\s*(i\s*(?:am|'m)?\s*giving up\b|i give up\b|我(?:決定)?放棄(?:了)?(?:這個)?(?:任務)?)",
    ),
    // 3. stopping_here — agent declares an unprompted stop point.
    (
        "stopping_here",
        r"(?i)^\s*(i(?:'ll| will)? stop here\b|stopping here\b|我(?:先)?(?:做|停)到這裡(?:好了)?|到此為止)",
    ),
    // 4. agents_in_flight — deferring because of other (sub)agents still working.
    (
        "agents_in_flight",
        r"(?i)^\s*(waiting for (?:the )?other agents?\b|another agent is (?:still )?(?:working|running)\b|等(?:其他|另一個)\s*agent(?:s)?\s*(?:完成|處理完)|其他(?:任務|代理)(?:仍在|還在)(?:執行|進行)中)",
    ),
    // 5. check_back_later — asks the human to come back later.
    (
        "check_back_later",
        r"(?i)^\s*(please check back later\b|i(?:'ll| will) continue (?:this )?later\b|check back (?:with me )?later\b|請(?:你)?(?:稍後|晚點|等等)再(?:來)?(?:查看|回來|試)|晚點(?:再)?回來)",
    ),
    // 6. verdict_line — agent self-signs a VERDICT line that should only
    //    ever come from the judge.
    (
        "verdict_line",
        r"(?i)^\s*verdict\s*[:：]\s*(?:pass|complete|done|accepted)\b",
    ),
    // 7. commit_push_pr — treats "committed/pushed/opened a PR" as the
    //    finish line instead of actual verified completion.
    (
        "commit_push_pr",
        r"(?i)^\s*(i(?:'ve| have)? committed (?:and )?pushed\b|changes (?:have been )?committed and pushed\b|(?:i(?:'ve| have)? )?opened a pr\b|已(?:經)?\s*commit\s*(?:並|和)?\s*push|已(?:提交|推送)(?:了)?\s*(?:pr|變更|程式碼))",
    ),
    // 8. ready_for_review — announces done and hands off to a reviewer
    //    without further verification, unprompted.
    (
        "ready_for_review",
        r"(?i)^\s*(this is ready for (?:your )?review\b|ready for (?:your )?review\b|please review(?: this)?\b|(?:已(?:經)?完成)?[,，]?\s*請(?:你)?(?:幫忙)?審(?:核|查)(?:一下)?)",
    ),
    // 9. please_deflection — deflects the "should I continue" decision back
    //    to a human who is not actually present in an autonomous loop.
    (
        "please_deflection",
        r"(?i)^\s*(let me know if you(?:'d| would) like me to continue\b|please let me know how (?:you'd like me )?to proceed\b|shall i continue\??|請(?:告訴我|問)(?:是否)?要(?:我)?繼續(?:嗎)?[?？]?|如(?:有)?需要(?:的話)?我可以繼續)",
    ),
];

static BAIL_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    BAIL_PATTERNS_SRC
        .iter()
        .map(|(name, src)| {
            (
                *name,
                Regex::new(src)
                    .unwrap_or_else(|e| panic!("bail pattern {name:?} must compile: {e}")),
            )
        })
        .collect()
});

/// All pattern names, in panel order — used by telemetry render code and
/// tests that want to assert coverage over the full panel.
pub fn pattern_names() -> impl Iterator<Item = &'static str> {
    BAIL_PATTERNS_SRC.iter().map(|(name, _)| *name)
}

/// The last non-empty, trimmed paragraph of `text` (paragraphs split on a
/// blank line). Falls back to the whole trimmed text when there is no blank
/// line separator (a single-paragraph completion is common). Never panics
/// on empty input — returns `""`.
pub fn last_nonempty_paragraph(text: &str) -> &str {
    // `str::split` on a `&str` pattern is a forward-only `Split` (its
    // `Searcher` is not `DoubleEndedSearcher`), so `Map`/`Filter` over it
    // cannot implement `DoubleEndedIterator` and `.next_back()` does not
    // type-check here. `.last()` is the semantically-identical fix (same
    // "last paragraph satisfying the filter" result, just a forward scan) —
    // trivial cost for a single completion's paragraph list.
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or("")
}

/// Match the fixed bail-pattern panel against the last non-empty paragraph
/// of an agent's completion text. Returns the FIRST matching pattern's
/// stable name (panel order), or `None` if nothing matched (the overwhelming
/// common case — most completions are not a premature stop).
pub fn detect_bail_pattern(text: &str) -> Option<&'static str> {
    let last = last_nonempty_paragraph(text);
    if last.is_empty() {
        return None;
    }
    BAIL_PATTERNS
        .iter()
        .find(|(_, re)| re.is_match(last))
        .map(|(name, _)| *name)
}

#[cfg(test)]
mod tests_text;
#[cfg(test)]
mod tests_activity;
