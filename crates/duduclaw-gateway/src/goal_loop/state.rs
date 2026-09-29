//! Goal-loop **task state** — the structured `<state>` block round-tripped
//! through every dispatch round, the closed pause-reason classification a
//! `needs_human` exit is stamped with, and the deterministic "best round"
//! picker a budget-exhausted escalation hands back.
//!
//! Merged 2026-09-29 (audit O8) from `goal_state` / `pause_reason` /
//! `goal_budget_best_round`; each source module's design rationale is kept
//! verbatim as the section banner below it, and the old crate-root paths
//! stay available as re-exports in `lib.rs` for one release.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::task_store::{TaskIterationRow, TaskRow};

// ══════════════════════════════════════════════════════════════════════
// goal_state ════════════════════════════════════════════════════════
// ══════════════════════════════════════════════════════════════════════
// Structured `<state>` block for the goal loop dispatch prompt — **A1**
// (StateAct, arXiv:2410.02810: self-prompting + chain-of-states, zero
// retrain, 10-30% task-completion lift reported).
//
// ## Protocol (runtime-neutral plain text)
//
// Every goal-loop dispatch payload carries a `<state>` block with four
// sections: goal, confirmed facts, pending hypotheses, excluded
// approaches. [`build_state_block`] fills the first and last
// programmatically every round; the middle two round-trip through the
// agent's own reply via a fixed self-report marker
// (`<state_update>{JSON}</state_update>`, see [`parse_state_update`]) so
// ANY CLI runtime (Claude / Codex / Gemini / Antigravity / openai-compat)
// can participate — the protocol is plain text embedded in the prompt and
// the completion text, not a Claude-specific structured-output feature.
//
// ## Round 1 vs later rounds
//
// First dispatch: goal comes from the task, the other three sections are
// empty (no iteration history, no persisted snapshot yet). Every dispatch
// after that: `excluded_approaches` is derived programmatically from the
// judge-rejection history in `task_iterations` (never self-reported —
// Honest Lying arXiv:2605.29463 found self-reported failure diagnosis on
// ALFWorld correct 0% of the time vs 86% for programmatic trajectory
// extraction, so this harness never asks the agent to self-diagnose its
// own failures); `pending_hypotheses` round-trips through
// [`GoalStateSnapshot`], persisted to `tasks.goal_state_json`
// (`goal_loop.rs::GoalLoopDriver::capture_round_state`) and re-read here
// every round. A parse failure or a missed capture window keeps the
// *previous* round's snapshot — this module never fabricates content for
// a section it could not obtain (StateAct's own framing: harness fills
// what it can verify, self-report degrades to "unchanged" on any doubt).
//
// ## Honesty note: `confirmed_facts` — WP-A9 wired this (previously always empty)
//
// This field used to be permanently empty, rendered as the "尚無"
// placeholder — see the earlier revision of this comment for why (the
// source data lived in `dispatch_engine.rs`, off-limits to the work
// package that first built this module). **WP-A9**
// (`commercial/docs/design-task-forward-model-2026-08-06.md` §4.2) wires
// it: `dispatch_engine.rs`'s acceptance review now appends this round's
// zero-LLM deterministic pass signals — the WP2.4 `outcome_spec` check and
// the B3 grounding pre-check's `Grounded` conclusion — into
// `GoalStateSnapshot.confirmed_facts` via `TaskStore::set_goal_state_json`
// (see `DispatchEngine::persist_confirmed_facts`). Still never
// self-reported: only those two deterministic, programmatic checks feed
// it, never `judge_feedback` prose or the agent's own claim. Each line is
// CJK-safe truncated to ≤120 chars and the list is capped to the 6 most
// recent entries (mirrors `excluded_from_iterations`'s own caps below). An
// empty list (neither check fired this round) still renders the "尚無"
// placeholder — "degrade, don't fabricate" is unchanged, only the source
// of truth grew a real producer.

/// Cap on rendered excluded-approach lines (bounds prompt growth — the plan
/// doc's 4th hard constraint: unpruned self-evolution inevitably bloats;
/// 2606.29182 found 37.5% of "new discoveries" in an unpruned loop were
/// spurious). Keeps only the most recent N.
const MAX_EXCLUDED_LINES: usize = 6;
/// Cap on self-reported hypothesis lines carried into the next round.
const MAX_HYPOTHESIS_LINES: usize = 6;
/// Per-line truncation for an excluded-approach summary (CJK-safe chars —
/// see `duduclaw_core::truncate_chars`, never raw byte slicing).
const EXCLUDED_LINE_CHAR_CAP: usize = 120;
/// Per-line truncation for a self-reported hypothesis (CJK-safe chars).
const HYPOTHESIS_LINE_CHAR_CAP: usize = 200;

/// One round's structured state, built fresh every dispatch by
/// [`build_state_block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateBlock {
    pub goal: String,
    pub confirmed_facts: Vec<String>,
    pub pending_hypotheses: Vec<String>,
    pub excluded_approaches: Vec<String>,
    /// Prompt-only annotation injected by the A2 visit graph
    /// (`goal_loop/signals.rs`) when this round's state repeats a
    /// previously-tried, previously-failed action. Deliberately excluded
    /// from [`StateBlock::hash_input`] — folding it in would make the
    /// annotation perturb the very hash it is derived from, so a flagged
    /// round could never "settle" into a stable hash. `None` until the
    /// caller (goal_loop.rs) consults the visit graph.
    pub loop_warning: Option<String>,
    /// H5 (WP-B): prompt-only annotation set when the PREVIOUS round's
    /// completion text matched the bail-pattern panel
    /// (`goal_bail_detect::detect_bail_pattern`) — see
    /// `goal_loop.rs::GoalLoopDriver::capture_round_state`. Same rationale
    /// as `loop_warning`: excluded from [`StateBlock::hash_input`] so this
    /// advisory nudge can never perturb the A2 no-progress comparison.
    pub bail_hint: Option<String>,
    /// H10: prompt-only annotation set when the PREVIOUS round's tool
    /// activity contained a long run of identical (tool, masked-params)
    /// calls (`goal_tool_streak::detect_tool_streak`) — see
    /// `goal_loop.rs::GoalLoopDriver::capture_round_state`. Same rationale
    /// as `loop_warning`/`bail_hint`: excluded from [`StateBlock::hash_input`]
    /// so a purely-advisory nudge can never perturb the A2 no-progress
    /// comparison. Advisory only — never blocks or retries anything.
    pub tool_streak_hint: Option<String>,
}

impl StateBlock {
    /// Text used ONLY to derive the state hash (A2, `goal_loop/signals.rs`).
    ///
    /// Uses the LATEST `excluded_approaches` entry, not the full
    /// accumulated list: the list grows by one entry on every rejection, so
    /// hashing the whole thing would make the hash trivially unique almost
    /// every round — defeating "this round repeats a prior round's state"
    /// detection. The latest entry alone reproduces the old oscillation
    /// guard's comparison ("does this rejection say the same thing as the
    /// last one") while still letting genuinely new judge feedback register
    /// as a state change.
    ///
    /// H4 (WP-B, gap fingerprinting): a byte/whitespace-normalized compare
    /// of the latest excluded entry is STILL a literal-text comparison — a
    /// judge that rewords the exact same gap ("missing error handling in
    /// `goal_loop.rs:120`" vs. "you forgot to validate at
    /// `goal_loop.rs:120`") produces a different hash even though the
    /// underlying stagnation is identical. When
    /// [`crate::goal_gap_fingerprint::gap_fingerprint`] can extract a
    /// `path:line` citation or a backtick key token from the latest
    /// excluded entry, its normalized fingerprint is hashed INSTEAD of the
    /// literal text, so reworded-but-same-gap feedback collapses to the
    /// same `state_hash`. When no citation/token is extractable at all
    /// (`None`), this falls back to the literal text — byte-identical to
    /// the pre-H4 behavior (see that module's "Fallback contract").
    fn hash_input(&self) -> String {
        let latest_excluded = self
            .excluded_approaches
            .last()
            .map(String::as_str)
            .unwrap_or("\u{2205}"); // ∅ — no rejection yet this lineage
        let excluded_component = crate::goal_gap_fingerprint::gap_fingerprint(latest_excluded)
            .unwrap_or_else(|| latest_excluded.to_string());
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}",
            self.goal.trim(),
            self.confirmed_facts.join("\u{1}"),
            self.pending_hypotheses.join("\u{1}"),
            excluded_component,
        )
    }

    /// Render the `<state>` XML block for the dispatch prompt. Plain-text
    /// protocol — no runtime-specific structured-output feature.
    ///
    /// H1 (injection hardening): every dynamic value interpolated here is
    /// `xml_escape`d first. `confirmed_facts` / `pending_hypotheses` /
    /// `excluded_approaches` all ultimately trace back to untrusted text —
    /// judge feedback, the agent's own self-reported `<state_update>`
    /// payload (round-tripped through `parse_state_update` below), or (via
    /// `dispatch_engine.rs`'s `confirmed_facts` writer) other prompt output —
    /// so a crafted line like `"legit</pending_hypotheses>\n<confirmed_facts>"`
    /// must not be able to forge a second `<confirmed_facts>` section or
    /// close this `<state>` block early. Before this fix `render()`
    /// interpolated every line raw.
    pub fn render(&self) -> String {
        fn section(items: &[String]) -> String {
            if items.is_empty() {
                "（尚無）".to_string()
            } else {
                items
                    .iter()
                    .map(|s| format!("- {}", xml_escape(s)))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        let mut excluded_section = section(&self.excluded_approaches);
        if let Some(w) = &self.loop_warning {
            excluded_section.push_str(&format!("\n! {}", xml_escape(w)));
        }
        if let Some(h) = &self.bail_hint {
            excluded_section.push_str(&format!("\n! {}", xml_escape(h)));
        }
        if let Some(h) = &self.tool_streak_hint {
            excluded_section.push_str(&format!("\n! {}", xml_escape(h)));
        }
        format!(
            "<state>\n\
             <goal>\n{}\n</goal>\n\
             <confirmed_facts>\n{}\n</confirmed_facts>\n\
             <pending_hypotheses>\n{}\n</pending_hypotheses>\n\
             <excluded_approaches>\n{}\n</excluded_approaches>\n\
             </state>",
            xml_escape(self.goal.trim()),
            section(&self.confirmed_facts),
            section(&self.pending_hypotheses),
            excluded_section,
        )
    }
}

/// Minimal XML/markup escape for values interpolated into an XML-delimited
/// prompt block (project convention: prompts use XML delimiters for
/// injection resistance). Mirrors `approval.rs::xml_escape` (private there,
/// `approval.rs` out of scope for this change, so duplicated here rather
/// than made to depend on it). `pub(crate)` — also reused by `goal_notify.rs`
/// (M5) and `approval_notify.rs` (L4) for the same reason rather than each
/// keeping a third private copy.
pub(crate) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// SHA-256(NFKC-normalize + whitespace-collapse) of
/// [`StateBlock::hash_input`], first 16 hex chars — the A2 visit graph's
/// state-key half.
pub fn state_hash(state: &StateBlock) -> String {
    short_hash(&state.hash_input())
}

/// Shared hashing primitive: NFKC-normalize (fullwidth/compat forms fold to
/// their canonical form so an agent's fullwidth punctuation doesn't produce
/// a spurious distinct hash), collapse whitespace runs, SHA-256, first 16
/// hex chars. `pub(crate)` so `goal_loop/signals.rs`'s `action_digest` can
/// reuse the exact same normalization instead of duplicating it.
pub(crate) fn short_hash(input: &str) -> String {
    let normalized = normalize_for_hash(input);
    let digest = Sha256::digest(normalized.as_bytes());
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .chars()
        .take(16)
        .collect()
}

fn normalize_for_hash(s: &str) -> String {
    use duduclaw_security::unicode_normalizer::UnicodeNormalizer;
    let nfkc = UnicodeNormalizer::normalize_nfkc(s);
    nfkc.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Programmatic excluded-approaches derivation from judge-rejection history
/// (`task_iterations`) — never self-reported. Truncated per-entry (CJK-safe)
/// and capped in count (most-recent-N kept) to bound prompt growth.
pub fn excluded_from_iterations(iterations: &[TaskIterationRow]) -> Vec<String> {
    let mut out: Vec<String> = iterations
        .iter()
        .filter_map(|it| it.judge_feedback.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| duduclaw_core::truncate_chars(s, EXCLUDED_LINE_CHAR_CAP))
        .collect();
    if out.len() > MAX_EXCLUDED_LINES {
        let drop = out.len() - MAX_EXCLUDED_LINES;
        out.drain(0..drop);
    }
    out
}

/// The agent's self-reported `<state_update>` payload.
#[derive(Debug, Deserialize, Serialize, Default)]
struct StateUpdatePayload {
    #[serde(default)]
    pending_hypotheses: Vec<String>,
}

/// Parse the agent's self-reported `<state_update>{...}</state_update>`
/// block out of its completion text (`result_summary`).
///
/// Returns `None` on ANY failure — missing tag, invalid JSON, wrong shape.
/// Callers MUST keep the previous round's value on `None` rather than
/// guessing: this is the StateAct "degrade, don't fabricate" rule the task
/// card calls out explicitly.
pub fn parse_state_update(text: &str) -> Option<Vec<String>> {
    const OPEN: &str = "<state_update>";
    const CLOSE: &str = "</state_update>";
    let start = text.find(OPEN)? + OPEN.len();
    let rel_end = text[start..].find(CLOSE)?;
    let body = text[start..start + rel_end].trim();
    // Parse to a generic `Value` first and require a JSON *object* before
    // converting to the typed payload. `serde`'s derived `Deserialize`
    // otherwise also accepts a bare JSON array as a valid encoding of a
    // single-defaulted-field struct (a seq-form struct, `[]` -> zero
    // fields present -> the field's `#[serde(default)]` fires) — that
    // would let a malformed `[]` body silently reset a task's tracked
    // hypotheses to empty instead of being rejected as the wrong shape.
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    if !value.is_object() {
        return None;
    }
    let payload: StateUpdatePayload = serde_json::from_value(value).ok()?;
    let mut hyps: Vec<String> = payload
        .pending_hypotheses
        .into_iter()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .map(|h| duduclaw_core::truncate_chars(&h, HYPOTHESIS_LINE_CHAR_CAP))
        // H1 double-insurance: strip raw angle brackets from the self-report
        // BEFORE it is ever persisted to `goal_state_json`. `render()` above
        // already XML-escapes every line at render time, so this is a
        // defense-in-depth belt-and-suspenders — a future render call site
        // that forgets to escape still can't have its `<state>` block
        // structure forged by a stored hypothesis string.
        .map(|h| h.replace(['<', '>'], ""))
        .filter(|h| !h.is_empty())
        .collect();
    hyps.truncate(MAX_HYPOTHESIS_LINES);
    Some(hyps)
}

/// Persisted snapshot stored in `tasks.goal_state_json` — round-trips the
/// self-reported hypotheses across dispatch ticks (and, best-effort, across
/// a gateway restart, since it lives in the durable task row unlike the
/// in-memory A2 visit graph).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GoalStateSnapshot {
    #[serde(default)]
    pub pending_hypotheses: Vec<String>,
    /// WP-A9: zero-LLM deterministic pass signals from
    /// `dispatch_engine.rs`'s acceptance review (see this module's
    /// "Honesty note" doc comment above). `#[serde(default)]` so an
    /// existing pre-WP-A9 `goal_state_json` blob (missing this key)
    /// deserializes to an empty list rather than failing.
    #[serde(default)]
    pub confirmed_facts: Vec<String>,
    /// H5 (WP-B): set by `goal_loop.rs::capture_round_state` when the
    /// PREVIOUS round's completion text matched the bail-pattern panel —
    /// surfaced to the agent on the NEXT dispatch via
    /// [`StateBlock::bail_hint`]. `#[serde(default)]` so a pre-H5
    /// `goal_state_json` blob deserializes with no hint rather than
    /// failing.
    #[serde(default)]
    pub bail_hint: Option<String>,
    /// H10: set by `goal_loop.rs::capture_round_state` when the PREVIOUS
    /// round's tool activity contained a long run of identical
    /// (tool, masked-params) calls — surfaced to the agent on the NEXT
    /// dispatch via [`StateBlock::tool_streak_hint`]. `#[serde(default)]`
    /// so a pre-H10 `goal_state_json` blob deserializes with no hint
    /// rather than failing.
    #[serde(default)]
    pub tool_streak_hint: Option<String>,
}

impl GoalStateSnapshot {
    /// Parse a stored snapshot. Missing / malformed ⇒ the zero-value
    /// snapshot (empty hypotheses) — the same "degrade, don't fabricate"
    /// rule: a corrupt column never invents hypotheses, it just forgets
    /// them, exactly like a first-round task would render.
    pub fn from_json(raw: Option<&str>) -> Self {
        raw.and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Build this round's [`StateBlock`] from the task row, iteration history,
/// and the persisted self-report snapshot. `loop_warning` is left `None` —
/// the caller (`goal_loop.rs`) fills it in after consulting the A2 visit
/// graph, kept out of this pure function so `goal_loop/state.rs` has no
/// dependency on `goal_loop/signals.rs` (one-directional module coupling).
pub fn build_state_block(
    task: &TaskRow,
    iterations: &[TaskIterationRow],
    snapshot: &GoalStateSnapshot,
) -> StateBlock {
    let mut goal = format!("{}\n{}", task.title.trim(), task.description.trim());
    if let Some(c) = task.acceptance_criteria.as_deref() {
        if !c.trim().is_empty() {
            goal.push_str(&format!("\n驗收標準: {}", c.trim()));
        }
    }
    StateBlock {
        goal,
        // WP-A9: sourced from the persisted snapshot — see module docs
        // "Honesty note" above for the deterministic-only producer.
        confirmed_facts: snapshot.confirmed_facts.clone(),
        pending_hypotheses: snapshot.pending_hypotheses.clone(),
        excluded_approaches: excluded_from_iterations(iterations),
        loop_warning: None,
        // H5: pass the persisted bail hint (if any) straight through — the
        // caller (`goal_loop.rs`) is the one that decides whether/what to
        // write here (`capture_round_state`), this pure function just
        // round-trips it, same pattern as `pending_hypotheses` above.
        bail_hint: snapshot.bail_hint.clone(),
        // H10: same round-trip pattern as `bail_hint` above.
        tool_streak_hint: snapshot.tool_streak_hint.clone(),
    }
}

// ══════════════════════════════════════════════════════════════════════
// pause_reason ══════════════════════════════════════════════════════
// ══════════════════════════════════════════════════════════════════════
// H11: structured pause-reason classification for `needs_human` goal tasks.
//
// Borrowed from grok-build's eight-state goal state machine
// (`research/harness-2026-08/grok-build.md` §2.3), **adapted rather than
// copied**: DuDuClaw deliberately does NOT add a new task `status` value —
// `needs_human` already has a dozen consumers (dashboard board columns, the
// inbox model, channel decision cards, `resolve_needs_human`'s fail-closed
// `WHERE status='needs_human'` guards, MCP task tools), and splitting it into
// eight statuses would break every one of them. Instead the *reason* a task
// parked is stored as a closed classification alongside the free-text
// `judge_feedback` that was already there.
//
// ## Why a closed set and not the free text
//
// Every escalation path already wrote a human-readable reason into
// `judge_feedback`, but three of those strings are built from LLM output
// (the two-stage evaluator's `evidence` / `next_step`) or from an arbitrary
// transport error (`judge unavailable: {e}`). Classifying by substring at
// read time would therefore be classifying *model-authored prose* — exactly
// the kind of unanchored `contains` check coding convention 2 forbids for a
// routing decision. So the class is stamped **at the call site**, where the
// trigger is known statically, and the stored string is only ever compared
// for exact equality.
//
// ## Fail-safe direction
//
// Unknown, empty, absent (every row written before this column existed), and
// unrecognised values all resolve to [`PauseReason::Unknown`], which reads as
// 「需要人工確認」 — i.e. an unclassifiable pause degrades toward *more*
// human attention, never less.

/// Closed classification of why a goal task is parked `needs_human`.
///
/// Six variants, one per family of trigger that actually exists in the
/// codebase today (see the table in the module tests). Deliberately smaller
/// than grok-build's eight states: `user_paused` has no DuDuClaw trigger (a
/// human takeover keeps the task in `needs_human` via `claim_needs_human`
/// without re-escalating it, and `takeover::is_target_paused` freezes a task
/// *without* parking it), so inventing the state would mean shipping a chip
/// nothing can ever set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    /// The loop ran but the state stopped changing — A2 oscillation
    /// (identical `state_hash` two rounds running).
    NoProgress,
    /// A hard cap fired: iteration cap, judge retry budget, the global
    /// wall clock, or a per-goal `deadline_at`.
    BudgetExhausted,
    /// Something outside the agent blocks the goal and a person must act —
    /// the two-stage evaluator's `Blocked` verdict, or an upstream
    /// dependency that terminally failed.
    BlockedNeedsDecision,
    /// The platform itself failed, not the work — e.g. the acceptance judge
    /// was unreachable (fail-safe park, never an auto-accept).
    Infra,
    /// The gateway restarted while this task was in flight and
    /// `[goal_loop] resume_on_restart = "pause"` parked it for
    /// re-confirmation instead of silently resuming.
    Restart,
    /// Not classified: rows written before this column existed, or any
    /// stored value this build does not recognise. Reads as "needs a human
    /// to look" — the safe direction.
    Unknown,
}

impl PauseReason {
    /// Stable wire/storage token. Never localise this — it is persisted in
    /// SQLite and shipped over the dashboard RPC as a key.
    pub fn as_str(self) -> &'static str {
        match self {
            PauseReason::NoProgress => "no_progress",
            PauseReason::BudgetExhausted => "budget_exhausted",
            PauseReason::BlockedNeedsDecision => "blocked_needs_decision",
            PauseReason::Infra => "infra",
            PauseReason::Restart => "restart",
            PauseReason::Unknown => "unknown",
        }
    }

    /// Resolve a stored column value back into a class. Exact (trimmed,
    /// ASCII-case-insensitive) token equality only — never substring
    /// matching, per coding convention 2. `None` / empty / unrecognised ⇒
    /// [`PauseReason::Unknown`].
    pub fn from_stored(stored: Option<&str>) -> Self {
        match stored.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "no_progress" => PauseReason::NoProgress,
            "budget_exhausted" => PauseReason::BudgetExhausted,
            "blocked_needs_decision" => PauseReason::BlockedNeedsDecision,
            "infra" => PauseReason::Infra,
            "restart" => PauseReason::Restart,
            _ => PauseReason::Unknown,
        }
    }

    /// One short zh-TW phrase for channel notices — end-user vocabulary, no
    /// internal jargon (no "iteration cap", no "oscillation", no task-store
    /// column names). The dashboard renders its own localised chip from
    /// [`Self::as_str`] instead of reusing this.
    pub fn label_zh(self) -> &'static str {
        match self {
            PauseReason::NoProgress => "卡住沒進展",
            PauseReason::BudgetExhausted => "次數或時限用盡",
            PauseReason::BlockedNeedsDecision => "等你決策",
            PauseReason::Infra => "系統問題",
            PauseReason::Restart => "系統重啟後暫停",
            PauseReason::Unknown => "需要人工確認",
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// goal_budget_best_round ════════════════════════════════════════════
// ══════════════════════════════════════════════════════════════════════
// WP-4F: attach the closest-to-done deliverable when a goal task's budget
// (iteration cap / wall clock / per-task deadline / judge retry budget)
// runs out, instead of an empty-handed `needs_human` escalation.
//
// ## Problem this replaces
//
// Before this module, every `PauseReason::BudgetExhausted` escalation wrote
// a bare trigger string ("goal-loop iteration cap", "goal-loop deadline",
// or the last round's judge feedback) into `judge_feedback` — the only
// field both the channel notification (`goal_notify::needs_human_body`) and
// the dashboard task-detail view read. A human opening that card saw "I
// couldn't finish" with no visibility into what the agent actually
// produced. AutoDesign (arXiv:2608.13560) motivates attaching the
// best-so-far candidate instead of nothing.
//
// ## Selection rule (deterministic, zero LLM)
//
// Scans the task's sealed `task_iterations` rows (verdict `rejected` or
// `escalated` — i.e. every round that did NOT result in acceptance) and
// picks, in order:
//
// 1. **The last round that reached the MAV panel** (`verdict_json` is
//    `Some`) among rejected/escalated rounds. The two-stage evaluator's own
//    per-round `candidate_complete`/`continue`/`blocked` verdict is not
//    persisted anywhere today (`dispatch_engine.rs`'s `PreDecision` is a
//    request-scoped enum, never written to `task_iterations`), so adding a
//    literal read of "the evaluator said candidate_complete" would need a
//    new column purely to mirror a value the panel-reaching itself already
//    implies in practice: a `Continue` verdict is routed straight back to
//    `revising` via `reject_review` (no panel call, no `verdict_json`); the
//    panel only ever runs when the evaluator said `candidate_complete`,
//    degraded open on its own error/timeout, or two-stage judging is
//    disabled entirely (in which case every rejected round reaches the
//    panel, so this tier degrades to "last round" — never wrong, just not
//    extra-informative). `verdict_json.is_some()` is therefore the
//    zero-new-column proxy for "this round was taken seriously as a
//    completion candidate" — see the WP-4F report for the full trade-off.
// 2. **Fewest extracted gap-fingerprint tokens**
//    ([`crate::goal_gap_fingerprint::gap_tokens`]) in the round's rejection
//    feedback, among rounds with at least one extractable token. Fewer
//    concrete citations/key tokens ⇒ closer to passing. Ties favor the
//    later round.
// 3. **The last round**, full stop — the pre-WP-4F fallback content (the
//    most recent rejection reason), now paired with that round's own
//    worker excerpt when one was captured.
//
// Zero rejected/escalated rounds (the budget ran out before any round was
// ever judged — e.g. a wall-clock deadline hit before the very first
// dispatch) ⇒ [`pick_best_round`] returns `None` and the caller keeps the
// bare pre-WP-4F escalation reason. No round is fabricated.

/// Bytes kept for a round's worker-result excerpt persisted into
/// `task_iterations.worker_excerpt` at verdict time (`reject_review_with_verdict`)
/// — bounded so a multi-KB agent reply never balloons a history row.
/// CJK-safe: callers MUST truncate with `duduclaw_core::truncate_bytes`, never
/// a raw byte slice.
pub const WORKER_EXCERPT_MAX_BYTES: usize = 500;

/// How many gap tokens are actually listed in [`compose_escalation_note`]
/// (a display budget; the underlying extraction can return up to
/// `goal_gap_fingerprint`'s own `MAX_FINGERPRINT_TOKENS`).
const DISPLAYED_GAP_TOKENS: usize = 5;

/// Chars kept from the picked round's own judge feedback when it is used as
/// the fallback "驗收意見" line (no extractable gap tokens at all).
const FALLBACK_FEEDBACK_MAX_CHARS: usize = 200;

/// One "best round" pick — the round attached to a budget-exhausted
/// escalation via [`compose_escalation_note`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BestRoundPick {
    pub round: i64,
    /// The round's own submitted output, when one was captured
    /// (`task_iterations.worker_excerpt`). `None` for rounds sealed before
    /// this column existed, or a round with no result at all.
    pub excerpt: Option<String>,
    /// That round's own rejection feedback (trimmed, unbounded — callers
    /// truncate at display time).
    pub judge_feedback: String,
    /// Extracted gap tokens from `judge_feedback` (display-capped at
    /// [`DISPLAYED_GAP_TOKENS`]), case-preserved, in occurrence order. Empty
    /// when the feedback carried no citation/key token at all.
    pub gaps: Vec<String>,
}

/// Deterministic, zero-LLM selection of the round to attach to a
/// budget-exhausted escalation. See module docs for the three-tier rule.
/// `None` when the task has no rejected/escalated round at all — the caller
/// must then keep its pre-WP-4F empty-handed escalation text, never
/// fabricate a pick.
pub fn pick_best_round(iterations: &[TaskIterationRow]) -> Option<BestRoundPick> {
    let candidates: Vec<&TaskIterationRow> = iterations
        .iter()
        .filter(|it| matches!(it.verdict.as_deref(), Some("rejected") | Some("escalated")))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Priority 1: last round that reached the MAV panel (verdict_json
    // present) among rejected/escalated rounds.
    if let Some(it) = candidates
        .iter()
        .copied()
        .filter(|it| it.verdict_json.is_some())
        .max_by_key(|it| it.round)
    {
        return Some(build_pick(it));
    }

    // Priority 2: fewest gap-fingerprint tokens, among rounds with at least
    // one extractable token. Ties favor the later round.
    let mut best: Option<(&TaskIterationRow, usize)> = None;
    for it in candidates.iter().copied() {
        let fb = it.judge_feedback.as_deref().unwrap_or("");
        let n = crate::goal_gap_fingerprint::gap_tokens(fb).len();
        if n == 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some((cur, cur_n)) => n < cur_n || (n == cur_n && it.round > cur.round),
        };
        if better {
            best = Some((it, n));
        }
    }
    if let Some((it, _)) = best {
        return Some(build_pick(it));
    }

    // Priority 3: last round, full stop.
    candidates
        .iter()
        .copied()
        .max_by_key(|it| it.round)
        .map(build_pick)
}

fn build_pick(it: &TaskIterationRow) -> BestRoundPick {
    let feedback = it
        .judge_feedback
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let gaps = crate::goal_gap_fingerprint::gap_tokens(&feedback)
        .into_iter()
        .take(DISPLAYED_GAP_TOKENS)
        .collect();
    BestRoundPick {
        round: it.round,
        excerpt: it
            .worker_excerpt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        judge_feedback: feedback,
        gaps,
    }
}

/// Compose the enriched `judge_feedback` text written on a budget-exhausted
/// escalation. `base_reason` is the escalation trigger's own short text
/// (e.g. `"goal-loop iteration cap"` or the last round's raw judge feedback)
/// — kept verbatim as the first line so any existing consumer reading the
/// reason as a leading prefix (activity feed, logs) is unaffected.
pub fn compose_escalation_note(base_reason: &str, pick: &BestRoundPick) -> String {
    let mut out = format!(
        "{base_reason}\n已附上第 {} 輪最接近完成的成果：",
        pick.round
    );
    match &pick.excerpt {
        Some(excerpt) => out.push_str(excerpt),
        None => out.push_str("（此輪未留下成果摘要）"),
    }
    if !pick.gaps.is_empty() {
        out.push_str("\n驗收時仍缺：");
        out.push_str(&pick.gaps.join("、"));
    } else if !pick.judge_feedback.is_empty() {
        out.push_str("\n驗收意見：");
        out.push_str(&duduclaw_core::truncate_chars(
            &pick.judge_feedback,
            FALLBACK_FEEDBACK_MAX_CHARS,
        ));
    }
    out
}

#[cfg(test)]
mod tests;
