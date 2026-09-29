//! Token-budget enforcement pipeline (#12, 2026-05-12).
//!
//! Before #12, the 200 K cliff was diagnosed *after* the request was sent
//! (via `cost_telemetry::record` + `cost_pressure` event). Operators got a
//! warning but the next request still went through unchanged. Budget
//! enforcement closes this loop: at the request boundary, estimate the
//! total input tokens; if over the configured ceiling, walk a fixed
//! compression pipeline. If the pipeline can't bring it under, refuse
//! the request and emit a `budget_exceeded` event.
//!
//! ## Design
//!
//! - **Pure stage functions** — each stage takes `(system, history, user)`
//!   and either returns a compressed version or `None` (stage can't help
//!   further). Callers iterate stages in order until budget is met.
//! - **Cost-pressure aware** — when an agent has a hot `cost_pressure`
//!   flag (#6.3), early stages become more aggressive (e.g. TurnTrim
//!   threshold drops from 800 → 200 chars).
//! - **Token estimation** uses a CJK-aware heuristic (1.306 tokens/char
//!   for CJK text, 1 token per 3.6 chars otherwise — 2026-08 local-corpus
//!   calibration; see `estimate_tokens` below for why the old uniform
//!   1.5 chars/token guess underestimated usage by ~22%).
//!
//! ## Stages
//!
//! Stages are ordered from cheapest / least lossy to most aggressive:
//! 1. `TurnTrim` — per-turn 800/200-char tail trim (existing approach,
//!    just made explicit). Loses no semantic content for short replies.
//! 2. `DropOldestToolEchoes` — strip old tool content and mark its bytes
//!    unavailable when this history path has no durable retrieval handle.
//! 3. Async bisect summary — after the pure stages fail, the gateway may
//!    summarize only the unprotected older turns and recheck the budget.
//!
//! ## What's intentionally NOT here
//!
//! - **LlmLingua-2 bridge**: CLAUDE.md mentions this as available infra
//!   but the Python subprocess startup latency makes it a poor fit for
//!   per-request synchronous compression. Should live in #13's async
//!   summarizer instead.
//! - **Meta-token LTSC**: a separate, opt-in `[compression]` config
//!   knob since it costs decode time on the agent side. Deferred.

use tracing::{info, warn};

// ── WP5: cache-aware compression gate (2607.12161) ─────────────────────
//
// The pipeline above is purely token-budget driven — it has no notion of
// prompt-cache health. That's a problem for agents whose system prompt +
// history are already hitting a healthy cache (Anthropic `cache_control:
// ephemeral`): rewriting even a small tail of the history changes the
// bytes the cache is keyed on, which forces a full cache-prefix rebuild.
// The paper's empirical finding is that cache-rebuild cost dominates
// (~87%) the overhead in that regime, i.e. the tokens compression saves
// are smaller than the cache-miss tax it triggers. `should_skip_for_cache`
// is the deterministic gate `maybe_compress_history` (channel_reply.rs)
// consults before entering the pipeline at all.

/// Compression info threaded from `maybe_compress_history` down to the
/// eventual `cost_telemetry` record call, which happens several async
/// frames away (inside `spawn_claude_cli_with_env` / the PTY variant).
/// Mirrors the existing `CHANNEL_REPLY_AGENT_ID` / `CHANNEL_REPLY_USER_ID`
/// task-locals in `claude_runner.rs` — same problem, same fix shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompressionInfo {
    /// Whether the request that's about to be sent was actually rewritten
    /// by the compression pipeline (false for: budget disabled, cache
    /// guard skipped the pipeline, request was already under budget, or
    /// the pipeline ran but couldn't bring it under budget — in that last
    /// case the caller falls back to the original uncompressed history,
    /// so nothing compressed actually went out).
    pub compressed: bool,
    /// Comma-joined stage names that ran (e.g. `"turn_trim"` or
    /// `"turn_trim,drop_oldest_tool_echoes"`). Empty when `compressed` is
    /// false.
    pub stages: String,
}

tokio::task_local! {
    /// See [`CompressionInfo`]. Scoped by `channel_reply::maybe_compress_history`'s
    /// caller alongside `CHANNEL_REPLY_AGENT_ID`.
    pub static CHANNEL_REPLY_COMPRESSION: CompressionInfo;
}

/// Cache-aware gate thresholds. Defaults match the WP5 design doc:
/// skip compression when the agent's trailing cache efficiency is > 50%
/// and the budget overshoot is < 15%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheGuardConfig {
    pub min_eff: f64,
    pub max_overshoot: f64,
}

/// Default minimum cache efficiency for the guard to engage (50%).
pub const DEFAULT_CACHE_GUARD_MIN_EFF: f64 = 0.5;
/// Default maximum budget overshoot the guard tolerates (15%).
pub const DEFAULT_CACHE_GUARD_MAX_OVERSHOOT: f64 = 0.15;

impl Default for CacheGuardConfig {
    fn default() -> Self {
        Self {
            min_eff: DEFAULT_CACHE_GUARD_MIN_EFF,
            max_overshoot: DEFAULT_CACHE_GUARD_MAX_OVERSHOOT,
        }
    }
}

/// Read `[budget] cache_guard_min_eff` / `cache_guard_max_overshoot` from
/// `agent.toml`. Fail-safe in the same shape as
/// `prompt_audit::read_max_input_tokens`: a missing file, unparseable
/// TOML, or an absent key falls back to the module default (**gate
/// enabled** at the paper's thresholds) — only an explicit
/// `cache_guard_min_eff = 0` disables the gate. This differs from
/// `read_max_input_tokens`'s "missing ⇒ disabled" convention on purpose:
/// the gate is a safety optimization that should protect agents by
/// default, not require explicit opt-in per agent.
pub fn read_cache_guard_config(agent_dir: &std::path::Path) -> CacheGuardConfig {
    let toml_path = agent_dir.join("agent.toml");
    let raw = match std::fs::read_to_string(&toml_path) {
        Ok(r) => r,
        Err(_) => return CacheGuardConfig::default(),
    };
    let value: toml::Value = match raw.parse() {
        Ok(v) => v,
        Err(_) => return CacheGuardConfig::default(),
    };
    let budget = value.get("budget");
    let as_f64 = |v: &toml::Value| -> Option<f64> {
        v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
    };
    let min_eff = budget
        .and_then(|b| b.get("cache_guard_min_eff"))
        .and_then(as_f64)
        .unwrap_or(DEFAULT_CACHE_GUARD_MIN_EFF);
    let max_overshoot = budget
        .and_then(|b| b.get("cache_guard_max_overshoot"))
        .and_then(as_f64)
        .unwrap_or(DEFAULT_CACHE_GUARD_MAX_OVERSHOOT);
    CacheGuardConfig {
        min_eff,
        max_overshoot,
    }
}

/// How far the estimated prompt is over budget, as a ratio (`0.15` = 15%
/// over). Returns `0.0` for a zero budget (disabled budget enforcement —
/// the gate is never consulted in that case anyway, but this keeps the
/// function total instead of panicking on division by zero).
pub fn overshoot_ratio(estimated_tokens: u64, budget_tokens: u64) -> f64 {
    if budget_tokens == 0 {
        return 0.0;
    }
    (estimated_tokens as f64 / budget_tokens as f64) - 1.0
}

/// Deterministic cache-aware gate decision. `min_eff <= 0.0` means the
/// gate is disabled (config convention: `cache_guard_min_eff = 0`) and
/// always returns `false` (never skip — behave exactly like pre-WP5).
/// Otherwise skips the pipeline when the cache is already healthy
/// (`cache_eff > min_eff`) AND the overshoot is mild
/// (`overshoot < max_overshoot`) — the regime where the paper found
/// cache-rebuild cost exceeds compression's savings.
pub fn should_skip_for_cache(
    cache_eff: f64,
    overshoot: f64,
    min_eff: f64,
    max_overshoot: f64,
) -> bool {
    if min_eff <= 0.0 {
        return false;
    }
    cache_eff > min_eff && overshoot < max_overshoot
}

/// CJK codepoint class — CJK Unified Ideographs + extension A,
/// Hiragana/Katakana, Hangul, CJK compatibility ideographs, full-width
/// forms. Same ranges as the other CJK-aware estimators in the workspace
/// (`duduclaw-llm::types::estimate_tokens`, `duduclaw-llm::provenance`),
/// kept as a local copy rather than a shared import to avoid a cross-crate
/// dependency for one small range table.
fn is_cjk_char(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// Calibrated tokens-per-char for CJK text (2026-08 local-corpus
/// calibration against real tokenizer output).
const CJK_TOKENS_PER_CHAR: f64 = 1.306;

/// Calibrated chars-per-token for non-CJK text (same calibration pass).
const NON_CJK_CHARS_PER_TOKEN: f64 = 3.6;

/// Estimate the number of tokens in a chunk of text, CJK-aware.
///
/// bug3 (2026-08): the prior heuristic was a uniform `chars / 1.5`
/// regardless of script, which underestimated real usage by ~22% on this
/// deployment's mixed zh-TW/en corpus — 1.5 chars/token (0.667 tok/char)
/// sits well *below* the calibrated CJK rate (1.306 tok/char), so
/// CJK-heavy prompts were undercounted and `[budget] max_input_tokens`
/// enforcement (`enforce_budget`/`enforce_budget_traced` below) could fire
/// the compression pipeline too late — by the time the estimate crossed
/// the configured ceiling, the real request had already blown well past
/// it. Splitting the estimate by codepoint class fixes both directions:
/// CJK text now counts for *more* (1.306 tok/char) and plain ASCII/latin
/// text counts for *less* (1 token per 3.6 chars, matching real tokenizer
/// behavior far better than the old 1.5 chars/token guess for English).
pub fn estimate_tokens(text: &str) -> u64 {
    let mut cjk_chars: u64 = 0;
    let mut other_chars: u64 = 0;
    for ch in text.chars() {
        if is_cjk_char(ch) {
            cjk_chars += 1;
        } else {
            other_chars += 1;
        }
    }
    let tokens =
        (cjk_chars as f64) * CJK_TOKENS_PER_CHAR + (other_chars as f64) / NON_CJK_CHARS_PER_TOKEN;
    tokens.ceil() as u64
}

/// Estimate total tokens across system prompt, conversation history, and
/// the upcoming user message. Each history turn's role tag counts as ~3
/// tokens of overhead; we approximate by adding 4 per turn.
pub fn estimate_request_tokens(
    system_prompt: &str,
    history: &[ChatMessage<'_>],
    user_message: &str,
) -> u64 {
    let mut total = estimate_tokens(system_prompt);
    for msg in history {
        total += estimate_tokens(msg.content);
        total += 4; // role tag + structural overhead per Anthropic API
    }
    total += estimate_tokens(user_message);
    total
}

// ── Never-trim sections (P1/WP-5, arXiv:2608.29028) ─────────────────────
//
// A TaskPacket's `constraints` and `audience` are the **incompressible**
// section. The measurement behind that word: vague boundaries produced
// violation rates of 50–73%, enumerating them dropped every model under 15%,
// and an audience allowlist "nearly eliminates" leakage — but a compression
// budget taxes boundaries unilaterally (boundary survival 0.80 → 0.57),
// silently converting an explicit constraint back into an implicit one. That
// is precisely the failure the two fields exist to prevent, so no stage in
// this pipeline may touch them: not a tail trim, not a tool-echo stub, not a
// summary. When protecting them means the budget cannot be met, the pipeline
// returns [`BudgetExceeded`] (with `protected_section_tokens` set) and the
// caller sends the uncompressed request — expensive is recoverable, a
// silently dropped constraint is not.
//
// Protection level matches the `working_state` authority section, which is
// safe today only because it is rendered into the *system prompt* and this
// pipeline rewrites history alone. A packet section can land in either place,
// so history needs the explicit exemption below.

// W2-E (review finding 4, real fix): the exemption is bound to its **source**,
// not to the header text. The four headers below are ordinary markdown any
// channel user can type, and `history` carries the user's own messages — so a
// header alone now protects nothing. A protected run requires the header line
// to be followed immediately by this process's unguessable marker, which only
// `team_composer::render_packet_for_prompt` emits. See
// [`duduclaw_core::protected_section`] for the sentinel's lifecycle and the
// three deliberate fail-safe directions (no sentinel / restart / CSPRNG down
// ⇒ nothing is protected).

pub use duduclaw_core::protected_section::{
    NEVER_TRIM_SECTION_HEADERS, SECTION_HEADER_AUDIENCE, SECTION_HEADER_CONSTRAINTS,
    contains_never_trim_header_spelling, is_never_trim_header_spelling, protected_marker_line,
};

/// `true` when `header_line` opens a protected run: it is exactly one of
/// [`NEVER_TRIM_SECTION_HEADERS`] **and** `next_line` is this process's
/// protected marker.
///
/// Both halves are exact equality after trimming surrounding ASCII whitespace
/// — never a substring or prefix test (coding convention 2). A decorated
/// variant (`## 約束（勿刪）`) is deliberately NOT a header, and a correct
/// header whose next line is anything else — including a user's own prose — is
/// deliberately NOT protected. Before W2-E the second half did not exist, so
/// one typed heading bought immunity from the budget and a permanent pin in
/// the session summary.
pub fn is_never_trim_header(header_line: &str, next_line: Option<&str>) -> bool {
    duduclaw_core::protected_section::opens_protected_section(
        header_line,
        next_line,
        duduclaw_core::protected_section::process_sentinel(),
    )
}

/// `true` when `line` opens a markdown heading at level 1 or 2 — the sibling
/// boundary that closes a never-trim section. `###` and deeper are content
/// *inside* the section.
fn is_section_boundary(line: &str) -> bool {
    let line = line.trim_start();
    (line.starts_with("## ") && !line.starts_with("### ")) || line.starts_with("# ")
}

/// One run of message content, classified by whether the stages may rewrite
/// it. Segments appear in document order and concatenate back to the exact
/// original content.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub text: String,
    /// `true` ⇒ a never-trim section (header line included).
    pub protected: bool,
}

/// Split message content into protected / trimmable runs.
///
/// A protected run starts at a header line whose **next** line carries this
/// process's protected marker ([`is_never_trim_header`]) and ends at the next
/// level-1/level-2 heading ([`is_section_boundary`]) or at end of content. The
/// marker line itself is part of the protected run, so a run that survives a
/// summary write-back is still protected on the next turn.
///
/// Concatenating `text` in order reproduces `content` byte for byte, so a
/// message with no protected section yields exactly one unprotected segment
/// and every stage behaves as it did before this existed.
pub fn split_never_trim_sections(content: &str) -> Vec<Segment> {
    split_never_trim_sections_with(
        content,
        duduclaw_core::protected_section::process_sentinel(),
    )
}

/// [`split_never_trim_sections`] against an explicit sentinel — the seam tests
/// use to exercise a wrong / absent sentinel without touching process state.
pub fn split_never_trim_sections_with(content: &str, sentinel: &str) -> Vec<Segment> {
    // One pass needs a one-line lookahead, so materialize the inclusive pieces
    // first. `split_inclusive` keeps the terminators, which is what makes the
    // round-trip byte-exact.
    let raws: Vec<&str> = content.split_inclusive('\n').collect();
    let line_at = |idx: usize| -> Option<&str> {
        raws.get(idx)
            .map(|raw| raw.trim_end_matches(['\n', '\r']) as &str)
    };
    let mut segments: Vec<Segment> = Vec::new();
    let mut in_protected = false;
    for (idx, raw) in raws.iter().enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        if in_protected && is_section_boundary(line) {
            in_protected = false;
        }
        if !in_protected
            && duduclaw_core::protected_section::opens_protected_section(
                line,
                line_at(idx + 1),
                sentinel,
            )
        {
            in_protected = true;
        }
        match segments.last_mut() {
            Some(last) if last.protected == in_protected => last.text.push_str(raw),
            _ => segments.push(Segment {
                text: (*raw).to_string(),
                protected: in_protected,
            }),
        }
    }
    segments
}

/// `true` when `content` carries at least one *protected* section — a
/// never-trim header bound to this process by its marker line.
///
/// A bare header spelling with no marker (anything a channel user can type) is
/// `false` here on purpose. When the question is instead "does this text echo
/// a never-trim heading at all" — the forgery check for text coming back from
/// a model — use [`contains_never_trim_header_spelling`].
pub fn has_never_trim_section(content: &str) -> bool {
    split_never_trim_sections(content).iter().any(|s| s.protected)
}

/// Produce model-visible transcript text and exact protected sections from
/// structured turns. A section cannot accidentally span into the next turn.
pub fn partition_turns_for_summary(turns: &[(String, String)]) -> (String, String, bool) {
    let mut transcript = String::new();
    let mut protected = String::new();
    let mut has_unprotected_text = false;
    for (index, (role, content)) in turns.iter().enumerate() {
        let mut visible = String::new();
        for segment in split_never_trim_sections(content) {
            if segment.protected {
                visible.push_str("\n[protected section preserved outside summary]\n");
                protected.push_str(&format!("[from {role} turn {}]\n", index + 1));
                protected.push_str(&segment.text);
                if !segment.text.ends_with('\n') {
                    protected.push('\n');
                }
            } else {
                has_unprotected_text |= !segment.text.trim().is_empty();
                visible.push_str(&segment.text);
            }
        }
        transcript.push_str(role);
        transcript.push_str(": ");
        transcript.push_str(&visible);
        transcript.push('\n');
    }
    (transcript, protected, has_unprotected_text)
}

/// Most never-trim tokens the budget floor will honour across one history.
///
/// Kept as defence in depth after W2-E bound the exemption to its source. The
/// original reason: a never-trim header is four literal markdown headings, so
/// any channel user could open a protected run by typing one, raise the floor
/// above the budget, and force [`compress_history_for_budget`] to refuse
/// before it ran a single stage — which, via `channel_reply`'s
/// `stages_tried.is_empty()` guard, also skipped the asynchronous summary
/// fallback. That entry point is closed (a marker line is required now), but
/// the ceiling still bounds a *legitimate* composer that renders far more
/// protected text than a packet's own field caps allow.
///
/// Sized so the composer's own maximum output is never capped: a packet's
/// `constraints` (≤12 × ≤200 chars) plus `audience` (≤16 × ≤64 chars) is at
/// most ~3.4k chars, ≈4.5k tokens at the CJK rate. The ceiling applies to the
/// **sum over the whole history**, so it also bounds an attacker who spreads
/// forged sections across many messages.
pub const NEVER_TRIM_FLOOR_MAX_TOKENS: u64 = 6_000;

/// Estimated tokens locked up by never-trim sections across `history`. This
/// is a strict lower bound on what any stage combination can leave behind,
/// because no stage may remove a protected run.
pub fn never_trim_tokens(history: &[OwnedChatMessage]) -> u64 {
    history
        .iter()
        .map(|m| {
            split_never_trim_sections(&m.content)
                .iter()
                .filter(|s| s.protected)
                .map(|s| estimate_tokens(&s.text))
                .sum::<u64>()
        })
        .sum()
}

/// Borrowed view of one chat message — kept generic so the same pipeline
/// can be driven from `channel_reply` and `claude_runner` without
/// allocating an intermediate Vec.
#[derive(Debug, Clone)]
pub struct ChatMessage<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

/// Owned variant when a stage needs to rewrite content. Stages return
/// these so the caller can either pass them onward (chained compression)
/// or extract the final result.
#[derive(Debug, Clone)]
pub struct OwnedChatMessage {
    pub role: String,
    pub content: String,
}

impl OwnedChatMessage {
    pub fn as_view(&self) -> ChatMessage<'_> {
        ChatMessage {
            role: &self.role,
            content: &self.content,
        }
    }
}

/// Verdict emitted when the pipeline can't bring the request under
/// budget. Caller logs this and aborts the request rather than sending
/// a known-over-budget call.
#[derive(Debug, Clone)]
pub struct BudgetExceeded {
    pub estimated_tokens: u64,
    pub budget_tokens: u64,
    /// Names of stages that ran; useful for debugging which compression
    /// strategies failed to free enough budget.
    pub stages_tried: Vec<&'static str>,
    /// P1/WP-5: estimated tokens held by never-trim sections
    /// ([`NEVER_TRIM_SECTION_HEADERS`]) the pipeline refused to touch.
    /// Non-zero says the residual overshoot is at least partly the
    /// incompressible section — the one reduction this pipeline is forbidden
    /// to make. `0` means the failure has nothing to do with packet
    /// boundaries (the pre-WP-5 meaning of this error).
    pub protected_section_tokens: u64,
}

/// Pipeline driver — runs `stages` in order until `estimate <= budget`
/// or all stages exhaust. Returns the final history (possibly compressed)
/// or `BudgetExceeded` if no combination of stages worked.
///
/// `stages` is `&[(&'static str, StageFn)]` where each stage is invoked
/// with the *current* history and may rewrite it. Stage functions are
/// pure: same input → same output. This keeps the pipeline testable
/// without mocking I/O.
///
/// Thin wrapper over [`enforce_budget_traced`] that drops the stage-trace
/// on success — kept byte-identical to preserve the existing call sites
/// and tests (WP5, 2607.12161: the traced sibling exists so
/// `cost_telemetry` can record *which* stages actually ran without
/// forcing every caller to consume that extra info).
#[allow(clippy::type_complexity)]
pub fn enforce_budget(
    system_prompt: &str,
    history: Vec<OwnedChatMessage>,
    user_message: &str,
    budget_tokens: u64,
    stages: &[(
        &'static str,
        fn(Vec<OwnedChatMessage>, bool) -> Vec<OwnedChatMessage>,
    )],
    cost_pressure: bool,
) -> Result<Vec<OwnedChatMessage>, BudgetExceeded> {
    enforce_budget_traced(
        system_prompt,
        history,
        user_message,
        budget_tokens,
        stages,
        cost_pressure,
    )
    .map(|(messages, _stages_ran)| messages)
}

/// Traced sibling of [`enforce_budget`] — identical behavior, but the
/// success case also returns the names of the stages that actually ran
/// (empty when the fast path applied, i.e. the request was already under
/// budget). `cost_telemetry::record_attributed_with_compression` uses
/// this to persist "was this reply compressed, and by which stages" per
/// request row instead of only inferring it from the cache-efficiency
/// trend after the fact.
#[allow(clippy::type_complexity)]
pub fn enforce_budget_traced(
    system_prompt: &str,
    history: Vec<OwnedChatMessage>,
    user_message: &str,
    budget_tokens: u64,
    stages: &[(
        &'static str,
        fn(Vec<OwnedChatMessage>, bool) -> Vec<OwnedChatMessage>,
    )],
    cost_pressure: bool,
) -> Result<(Vec<OwnedChatMessage>, Vec<&'static str>), BudgetExceeded> {
    let initial = {
        let views: Vec<ChatMessage<'_>> = history.iter().map(|m| m.as_view()).collect();
        estimate_request_tokens(system_prompt, &views, user_message)
    };
    if initial <= budget_tokens {
        // Fast path — no compression needed.
        return Ok((history, Vec::new()));
    }

    // P1/WP-5: never-trim sections are a floor no stage can lower. When the
    // floor alone (system + user + every protected run) already clears the
    // budget, running the pipeline can only waste work and end in the same
    // refusal — so fail the compression step explicitly instead of trimming
    // the one thing that must not be trimmed. Per-turn structural overhead is
    // deliberately excluded from the floor so it stays a strict lower bound
    // even if a future stage drops whole messages.
    let measured_protected_tokens = never_trim_tokens(&history);
    // Review finding 4 (partial mitigation): the never-trim headers are four
    // ordinary markdown headings, and `history` includes the USER's own
    // messages on all eleven channels. One message containing a bare
    // `## Constraints` line plus a wall of text could push the floor over the
    // budget, which returned `BudgetExceeded { stages_tried: [] }` — and
    // `channel_reply`'s `!(stages_tried.is_empty() && protected_section_tokens
    // > 0)` guard then skipped even the asynchronous bisect summary, sending
    // the whole uncompressed history. Capping how much protection the floor
    // will honour means the pipeline always RUNS: past the cap the excess is
    // accounted as ordinary compressible content.
    //
    // This does NOT make the protected runs themselves trimmable (the stages
    // still respect `split_never_trim_sections`). W2-E has since landed the
    // real fix — the exemption is bound to the composer as its *source* via an
    // unguessable marker line — so user-authored text can no longer reach this
    // counter at all; the cap survives as defence in depth against a
    // legitimate emitter producing far more protected text than a packet's own
    // field caps allow.
    let protected_section_tokens = measured_protected_tokens.min(NEVER_TRIM_FLOOR_MAX_TOKENS);
    if measured_protected_tokens > NEVER_TRIM_FLOOR_MAX_TOKENS {
        warn!(
            measured_protected_tokens,
            honored = protected_section_tokens,
            cap = NEVER_TRIM_FLOOR_MAX_TOKENS,
            "budget enforcement: never-trim sections exceed the protected-floor cap — \
             the excess is treated as ordinary compressible content (protected runs are \
             source-bound since W2-E, so this means a system emitter rendered more than \
             a packet's field caps allow)"
        );
    }
    if protected_section_tokens > 0 {
        let floor = estimate_tokens(system_prompt)
            + estimate_tokens(user_message)
            + protected_section_tokens;
        if floor > budget_tokens {
            warn!(
                floor,
                protected_section_tokens,
                budget = budget_tokens,
                "budget enforcement: never-trim sections (## 約束 / ## 受眾) alone \
                 exceed the budget — refusing to compress rather than truncating \
                 an explicit constraint (arXiv:2608.29028)"
            );
            return Err(BudgetExceeded {
                estimated_tokens: initial,
                budget_tokens,
                stages_tried: Vec::new(),
                protected_section_tokens,
            });
        }
    }

    info!(
        initial_tokens = initial,
        budget = budget_tokens,
        cost_pressure,
        protected_section_tokens,
        "prompt over budget — entering compression pipeline"
    );

    let mut current = history;
    let mut stages_tried: Vec<&'static str> = Vec::new();
    for (name, stage) in stages {
        current = stage(current, cost_pressure);
        stages_tried.push(*name);
        let views: Vec<ChatMessage<'_>> = current.iter().map(|m| m.as_view()).collect();
        let after = estimate_request_tokens(system_prompt, &views, user_message);
        info!(
            stage = name,
            after_tokens = after,
            "compression stage applied"
        );
        if after <= budget_tokens {
            return Ok((current, stages_tried));
        }
    }

    let views: Vec<ChatMessage<'_>> = current.iter().map(|m| m.as_view()).collect();
    let final_estimate = estimate_request_tokens(system_prompt, &views, user_message);
    warn!(
        final_tokens = final_estimate,
        budget = budget_tokens,
        stages = ?stages_tried,
        protected_section_tokens,
        "compression pipeline failed to bring request under budget"
    );
    Err(BudgetExceeded {
        estimated_tokens: final_estimate,
        budget_tokens,
        stages_tried,
        protected_section_tokens,
    })
}

// ── Stages ──────────────────────────────────────────────────────────

/// TurnTrim — tail-trim each message to a length threshold. The
/// threshold drops to 200 chars when `cost_pressure` is set; otherwise
/// 800. Loses the *prefix* of long tool outputs (the most informative
/// part is usually the head, but we accept that tradeoff to free budget;
/// crucial messages live in the recent turns which see the soft 200/800
/// limit, not zero).
///
/// Implementation is intentionally simple: chop bytes at a char boundary,
/// add a single-line `[trimmed N chars]` marker so the model knows what
/// happened.
///
/// P1/WP-5: never-trim sections ([`NEVER_TRIM_SECTION_HEADERS`]) survive
/// verbatim. The `threshold` character budget then applies to the *trimmable*
/// runs only, spent head-first in document order, so the result can legally
/// exceed `threshold` — a protected boundary outranks the budget. A message
/// with no such section takes the byte-identical legacy path below.
pub fn turn_trim(history: Vec<OwnedChatMessage>, cost_pressure: bool) -> Vec<OwnedChatMessage> {
    let threshold = if cost_pressure { 200 } else { 800 };
    history
        .into_iter()
        .map(|msg| {
            if msg.content.chars().count() <= threshold {
                return msg;
            }
            if !has_never_trim_section(&msg.content) {
                // ── Legacy path, unchanged ──
                let take = msg.content.chars().take(threshold).collect::<String>();
                let trimmed = msg.content.chars().count() - threshold;
                return OwnedChatMessage {
                    role: msg.role,
                    content: format!("{take}\n[trimmed {trimmed} chars]"),
                };
            }
            let mut out = String::new();
            let mut remaining = threshold;
            let mut dropped = 0usize;
            for seg in split_never_trim_sections(&msg.content) {
                if seg.protected {
                    out.push_str(&seg.text);
                    continue;
                }
                let len = seg.text.chars().count();
                if len <= remaining {
                    out.push_str(&seg.text);
                    remaining -= len;
                } else {
                    out.extend(seg.text.chars().take(remaining));
                    dropped += len - remaining;
                    remaining = 0;
                }
            }
            if dropped > 0 {
                out.push_str(&format!("\n[trimmed {dropped} chars]"));
            }
            OwnedChatMessage {
                role: msg.role,
                content: out,
            }
        })
        .collect()
}

/// DropOldestToolEchoes — for messages with `role == "tool"` or
/// `role == "function"`, when older than the last 3 turns, replace the
/// content with a stub. This path has no durable retrieval handle.
///
/// P1/WP-5: a stubbed message keeps its never-trim sections
/// ([`NEVER_TRIM_SECTION_HEADERS`]) verbatim and stubs only the rest — the
/// stub's byte count is then the stripped bytes, not the whole message. A
/// message with no such section is replaced by a single honest stub.
pub fn drop_oldest_tool_echoes(
    history: Vec<OwnedChatMessage>,
    _cost_pressure: bool,
) -> Vec<OwnedChatMessage> {
    let n = history.len();
    // Keep last 3 turns verbatim; for older tool-role messages, stub.
    let keep_from = n.saturating_sub(3);
    history
        .into_iter()
        .enumerate()
        .map(|(idx, msg)| {
            let is_tool = msg.role == "tool" || msg.role == "function";
            if !(idx < keep_from && is_tool) {
                return msg;
            }
            if !has_never_trim_section(&msg.content) {
                // No durable original is available from this history path.
                let original_len = msg.content.len();
                return OwnedChatMessage {
                    role: msg.role,
                    content: format!("[tool_echo stripped — {original_len} bytes dropped; original unavailable]"),
                };
            }
            let segments = split_never_trim_sections(&msg.content);
            let stripped_len: usize = segments
                .iter()
                .filter(|s| !s.protected)
                .map(|s| s.text.len())
                .sum();
            let mut out = String::new();
            let mut stub_written = false;
            for seg in &segments {
                if seg.protected {
                    out.push_str(&seg.text);
                } else if !stub_written {
                    out.push_str(&format!(
                        "[tool_echo stripped — {stripped_len} bytes dropped; original unavailable]\n"
                    ));
                    stub_written = true;
                }
            }
            OwnedChatMessage {
                role: msg.role,
                content: out,
            }
        })
        .collect()
}

/// Legacy synchronous stage kept for callers that explicitly supply it.
/// It remains a no-op because the gateway's actual last-resort summary
/// runs asynchronously after the pure pipeline reports over budget.
///
/// Never-trim sections are handled by [`prepare_bisect_summary`] and
/// [`complete_bisect_summary`] in the async caller.
pub fn bisect_and_summarize(
    history: Vec<OwnedChatMessage>,
    _cost_pressure: bool,
) -> Vec<OwnedChatMessage> {
    // TODO(#13) — fold older half into a Haiku-generated summary.
    // Until then, no-op so the pipeline either succeeds at earlier
    // stages or honestly reports BudgetExceeded.
    history
}

/// Pure preparation for the async last-resort stage. Keep the newest three
/// messages untouched and never send protected sections to the utility model.
pub struct PendingBisectSummary {
    pub transcript: String,
    pub protected: String,
    pub has_unprotected_text: bool,
    pub older_turns: usize,
    remaining: Vec<OwnedChatMessage>,
}

pub fn prepare_bisect_summary(
    history: &[OwnedChatMessage],
    cost_pressure: bool,
) -> Option<PendingBisectSummary> {
    if history.len() < 4 {
        return None;
    }
    let older_turns = (history.len() / 2).min(history.len() - 3);
    let turns: Vec<_> = history[..older_turns]
        .iter()
        .map(|message| (message.role.clone(), message.content.clone()))
        .collect();
    // Strip protected bytes before any lossy cap. Trimming a message first
    // could remove the newline before a header and accidentally expose it.
    let (mut transcript, protected, has_unprotected_text) = partition_turns_for_summary(&turns);
    let limit = older_turns.saturating_mul(if cost_pressure { 200 } else { 800 });
    if transcript.chars().count() > limit {
        transcript = transcript.chars().take(limit).collect();
        transcript.push_str("\n[older unprotected text omitted]");
    }
    Some(PendingBisectSummary {
        transcript,
        protected,
        has_unprotected_text,
        older_turns,
        remaining: history[older_turns..].to_vec(),
    })
}

/// Accept the utility output only when it cannot forge a protected section
/// and the final request truly fits the budget. Otherwise the caller keeps
/// its original history and reports that compression was insufficient.
pub fn complete_bisect_summary(
    pending: PendingBisectSummary,
    summary: &str,
    system_prompt: &str,
    user_message: &str,
    budget_tokens: u64,
) -> Option<Vec<OwnedChatMessage>> {
    let summary = summary.trim();
    // Forgery check on model output uses the *spelling*, not the marker: the
    // utility model never sees a sentinel (protected runs are stripped before
    // the transcript is built), so it cannot mint a real marker — but a
    // summary that parrots `## 約束` back would still teach the next reader
    // that the heading means something. Rejecting the spelling is strictly
    // more conservative and keeps the pre-W2-E behavior on this path.
    if (pending.has_unprotected_text && summary.is_empty())
        || contains_never_trim_header_spelling(summary)
    {
        return None;
    }
    let mut content = format!("[summary of earlier {} turns]", pending.older_turns);
    if !summary.is_empty() {
        content.push('\n');
        content.push_str(summary);
    }
    if !pending.protected.is_empty() {
        content.push_str("\n[verbatim protected sections]\n");
        content.push_str(&pending.protected);
    }
    let mut result = Vec::with_capacity(pending.remaining.len() + 1);
    result.push(OwnedChatMessage {
        role: "assistant".into(),
        content,
    });
    result.extend(pending.remaining);
    let views: Vec<_> = result.iter().map(OwnedChatMessage::as_view).collect();
    (estimate_request_tokens(system_prompt, &views, user_message) <= budget_tokens)
        .then_some(result)
}

/// The default synchronous stages used by the gateway. The async summary
/// fallback is invoked by the caller only if these fail.
#[allow(clippy::type_complexity)]
pub fn default_pipeline() -> &'static [(
    &'static str,
    fn(Vec<OwnedChatMessage>, bool) -> Vec<OwnedChatMessage>,
)] {
    &[
        ("turn_trim", turn_trim),
        ("drop_oldest_tool_echoes", drop_oldest_tool_echoes),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> OwnedChatMessage {
        OwnedChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    // ── Token estimation ──

    #[test]
    fn estimate_tokens_ascii() {
        // "hello" is 5 non-CJK chars → ceil(5/3.6) = 2 tokens (calibrated
        // 3.6 chars/token for non-CJK text — corrects the old uniform
        // chars/1.5 heuristic, which overestimated plain ASCII).
        assert_eq!(estimate_tokens("hello"), 2);
    }

    #[test]
    fn estimate_tokens_cjk() {
        // 4 CJK chars → ceil(4 * 1.306) = 6 tokens (calibrated 1.306
        // tokens/char for CJK text — corrects the old uniform chars/1.5
        // heuristic, which underestimated CJK-heavy prompts, the root
        // cause of bug3's ~22% overall underestimate on mixed corpora).
        assert_eq!(estimate_tokens("你好世界"), 6);
    }

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimate_tokens_pure_cjk_matches_calibration() {
        // 1000 CJK chars at 1.306 tok/char should land at (near-)exactly
        // 1306 tokens — the calibrated corpus value, not an approximation.
        let text: String = "嘟".repeat(1000);
        let tokens = estimate_tokens(&text);
        assert!((tokens as i64 - 1306).abs() <= 2, "got {tokens}");
    }

    #[test]
    fn estimate_tokens_pure_ascii_matches_calibration() {
        // 3600 ASCII chars at 3.6 chars/token should land at (near-)exactly
        // 1000 tokens.
        let text = "a".repeat(3600);
        let tokens = estimate_tokens(&text);
        assert!((tokens as i64 - 1000).abs() <= 2, "got {tokens}");
    }

    #[test]
    fn estimate_tokens_mixed_cjk_and_ascii_blends_rates() {
        // 500 CJK chars (≈653 tok) + 1800 ASCII chars (≈500 tok) ≈ 1153 tok.
        // Confirms the two rates are applied per-class, not averaged.
        let text = format!("{}{}", "你".repeat(500), "a".repeat(1800));
        let tokens = estimate_tokens(&text);
        assert!((tokens as i64 - 1153).abs() <= 2, "got {tokens}");
    }

    #[test]
    fn estimate_request_tokens_combines_system_history_user() {
        let history = vec![
            ChatMessage {
                role: "user",
                content: "hi",
            },
            ChatMessage {
                role: "assistant",
                content: "hello",
            },
        ];
        let total = estimate_request_tokens("system prompt", &history, "user msg");
        // All ASCII: "system prompt"(13→4) + "hi"(2→1)+4 + "hello"(5→2)+4
        // + "user msg"(8→3) = 18 under the calibrated non-CJK rate.
        // Just assert > 0 and reasonable bound.
        assert!(total > 10 && total < 100, "got {total}");
    }

    // ── Fast path ──

    #[test]
    fn enforce_budget_fast_path_when_under_budget() {
        let history = vec![msg("user", "hi"), msg("assistant", "hello")];
        let result = enforce_budget(
            "tiny system",
            history.clone(),
            "tiny user",
            10_000,
            default_pipeline(),
            false,
        );
        let out = result.expect("should succeed under budget");
        // Fast path returns history unchanged.
        assert_eq!(out.len(), history.len());
        assert_eq!(out[0].content, "hi");
    }

    // ── TurnTrim ──

    #[test]
    fn turn_trim_passes_through_short_messages() {
        let history = vec![msg("user", "short")];
        let out = turn_trim(history, false);
        assert_eq!(out[0].content, "short");
    }

    #[test]
    fn turn_trim_clips_long_message_at_800_chars_default() {
        let long = "x".repeat(2000);
        let history = vec![msg("assistant", &long)];
        let out = turn_trim(history, false);
        // 800 chars + trim marker.
        assert!(out[0].content.starts_with("xxxxxxxxxxxxxxxxxxxx"));
        assert!(out[0].content.contains("[trimmed 1200 chars]"));
        assert!(out[0].content.chars().count() < 850);
    }

    #[test]
    fn turn_trim_clips_more_aggressively_under_cost_pressure() {
        let long = "x".repeat(2000);
        let history = vec![msg("assistant", &long)];
        let out = turn_trim(history, /* cost_pressure */ true);
        assert!(out[0].content.contains("[trimmed 1800 chars]"));
        assert!(out[0].content.chars().count() < 250);
    }

    // ── DropOldestToolEchoes ──

    #[test]
    fn drop_old_tool_echoes_keeps_recent_turns_verbatim() {
        let history = vec![
            msg("tool", "old tool result aaaaa"),
            msg("user", "..."),
            msg("assistant", "..."),
            msg("tool", "recent tool result"),
        ];
        let out = drop_oldest_tool_echoes(history, false);
        // Recent (last 3) untouched.
        assert_eq!(out[3].content, "recent tool result");
        // Old tool result stubbed.
        assert!(out[0].content.contains("[tool_echo stripped"));
    }

    #[test]
    fn drop_old_tool_echoes_does_not_touch_non_tool_roles() {
        let history = vec![
            msg("user", "old user message — full content preserved"),
            msg("assistant", "old assistant reply"),
            msg("user", "fresh"),
            msg("assistant", "fresh"),
            msg("user", "fresh"),
        ];
        let out = drop_oldest_tool_echoes(history, false);
        // First two are old AND non-tool → must NOT be stubbed.
        assert!(out[0].content.contains("old user message"));
        assert!(out[1].content.contains("old assistant reply"));
    }

    // ── Pipeline integration ──

    #[test]
    fn pipeline_compresses_when_over_budget_via_turn_trim() {
        let huge = "x".repeat(50_000);
        let history = vec![msg("assistant", &huge)];
        // System + huge user assistant = way over 1000 tokens. TurnTrim
        // alone should bring it under 1000.
        let result = enforce_budget(
            "system",
            history,
            "ask question",
            1_000,
            default_pipeline(),
            false,
        );
        let out = result.expect("turn_trim should suffice");
        assert!(out[0].content.contains("[trimmed"));
    }

    #[test]
    fn pipeline_reports_exceeded_when_no_stage_helps() {
        // Force a budget so small even an empty body exceeds it (system
        // prompt alone is >5 tokens).
        let history = vec![msg("user", "hi")];
        let result = enforce_budget(
            &"x".repeat(10_000), // huge un-trimmable system prompt
            history,
            "u",
            10,
            default_pipeline(),
            false,
        );
        match result {
            Err(BudgetExceeded {
                estimated_tokens,
                budget_tokens,
                stages_tried,
                protected_section_tokens,
            }) => {
                assert!(estimated_tokens > budget_tokens);
                assert_eq!(budget_tokens, 10);
                assert!(stages_tried.contains(&"turn_trim"));
                assert_eq!(protected_section_tokens, 0);
            }
            Ok(_) => panic!("expected BudgetExceeded with un-trimmable system prompt"),
        }
    }

    #[test]
    fn pipeline_uses_cost_pressure_to_compress_harder() {
        // Compose a history that's over budget pre-compression (bug3
        // calibration: non-CJK is 3.6 chars/token, so this needs ~3600
        // ASCII chars to land at ~1000 tokens — 1500 chars, the pre-bug3
        // fixture size, is only ~417 tokens and no longer crosses the 600
        // budget at all under the corrected rate, which made this test
        // fail closed on `enforce_budget`'s fast path instead of exercising
        // TurnTrim).
        let mid = "x".repeat(3_600); // ~1000 tokens
        let history = vec![msg("assistant", &mid)];

        // Without cost pressure: turn_trim caps at 800 chars → ~229
        // tokens. With pressure: 200 chars → ~62 tokens. Both fit under
        // the 600 budget, so both succeed — but pressure trims harder.
        let normal = enforce_budget("s", history.clone(), "u", 600, default_pipeline(), false);
        let pressured = enforce_budget("s", history, "u", 600, default_pipeline(), true);
        // Both should succeed at this budget, but pressured version
        // produces shorter content.
        let normal_len = normal.unwrap()[0].content.len();
        let pressured_len = pressured.unwrap()[0].content.len();
        assert!(
            pressured_len < normal_len,
            "cost_pressure should produce shorter trim ({normal_len} vs {pressured_len})"
        );
    }

    // ── enforce_budget_traced ──

    #[test]
    fn enforce_budget_traced_fast_path_returns_empty_stages() {
        let history = vec![msg("user", "hi")];
        let (out, stages) =
            enforce_budget_traced("s", history, "u", 10_000, default_pipeline(), false)
                .expect("under budget");
        assert!(stages.is_empty());
        assert_eq!(out[0].content, "hi");
    }

    #[test]
    fn enforce_budget_traced_reports_stages_that_ran() {
        let huge = "x".repeat(50_000);
        let history = vec![msg("assistant", &huge)];
        let (out, stages) = enforce_budget_traced(
            "system",
            history,
            "ask question",
            1_000,
            default_pipeline(),
            false,
        )
        .expect("turn_trim should suffice");
        assert_eq!(stages, vec!["turn_trim"]);
        assert!(out[0].content.contains("[trimmed"));
    }

    #[test]
    fn enforce_budget_and_traced_agree_on_success_payload() {
        // `enforce_budget` must stay byte-identical to the traced sibling
        // minus the stage list — regression guard for the delegation.
        let huge = "x".repeat(50_000);
        let plain = enforce_budget(
            "system",
            vec![msg("assistant", &huge)],
            "ask question",
            1_000,
            default_pipeline(),
            false,
        )
        .unwrap();
        let (traced, _stages) = enforce_budget_traced(
            "system",
            vec![msg("assistant", &huge)],
            "ask question",
            1_000,
            default_pipeline(),
            false,
        )
        .unwrap();
        assert_eq!(plain[0].content, traced[0].content);
    }

    // ── WP5: cache-aware compression gate ──

    #[test]
    fn should_skip_for_cache_disabled_when_min_eff_zero() {
        // min_eff=0 is the documented "gate disabled" config value —
        // must never skip regardless of how healthy the cache looks.
        assert!(!should_skip_for_cache(0.99, 0.0, 0.0, 0.15));
    }

    #[test]
    fn should_skip_for_cache_skips_when_hot_and_mild_overshoot() {
        assert!(should_skip_for_cache(0.6, 0.1, 0.5, 0.15));
    }

    #[test]
    fn should_skip_for_cache_does_not_skip_when_cache_cold() {
        // Cache efficiency below threshold — compression should still run.
        assert!(!should_skip_for_cache(0.2, 0.1, 0.5, 0.15));
    }

    #[test]
    fn should_skip_for_cache_does_not_skip_when_overshoot_large() {
        // Cache is healthy but the request is way over budget — still
        // compress, since the token savings likely outweigh a cache miss.
        assert!(!should_skip_for_cache(0.9, 0.5, 0.5, 0.15));
    }

    #[test]
    fn should_skip_for_cache_boundary_is_exclusive() {
        // Exactly at the thresholds should NOT skip (strict `>` / `<`).
        assert!(!should_skip_for_cache(0.5, 0.15, 0.5, 0.15));
    }

    #[test]
    fn overshoot_ratio_computes_fraction_over_budget() {
        // 1150 / 1000 - 1 = 0.15
        assert!((overshoot_ratio(1150, 1000) - 0.15).abs() < 1e-9);
    }

    #[test]
    fn overshoot_ratio_negative_when_under_budget() {
        assert!(overshoot_ratio(500, 1000) < 0.0);
    }

    #[test]
    fn overshoot_ratio_zero_budget_is_total_not_panicking() {
        assert_eq!(overshoot_ratio(500, 0), 0.0);
    }

    // ── read_cache_guard_config (fail-safe) ──

    #[test]
    fn read_cache_guard_config_defaults_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg, CacheGuardConfig::default());
    }

    #[test]
    fn read_cache_guard_config_defaults_when_toml_malformed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "not valid toml =====").unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg, CacheGuardConfig::default());
    }

    #[test]
    fn read_cache_guard_config_reads_explicit_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[budget]\ncache_guard_min_eff = 0.4\ncache_guard_max_overshoot = 0.2\n",
        )
        .unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg.min_eff, 0.4);
        assert_eq!(cfg.max_overshoot, 0.2);
    }

    #[test]
    fn read_cache_guard_config_explicit_zero_disables_gate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[budget]\ncache_guard_min_eff = 0\n",
        )
        .unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg.min_eff, 0.0);
        // Downstream gate check confirms this actually disables it.
        assert!(!should_skip_for_cache(
            0.9,
            0.0,
            cfg.min_eff,
            cfg.max_overshoot
        ));
    }

    #[test]
    fn read_cache_guard_config_missing_budget_section_uses_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "[other]\nfoo = 1\n").unwrap();
        let cfg = read_cache_guard_config(dir.path());
        assert_eq!(cfg, CacheGuardConfig::default());
    }

    // ── Never-trim sections (P1/WP-5) ─────────────────────────────────

    /// This process's protected marker — what
    /// `team_composer::render_packet_for_prompt` emits under each never-trim
    /// header. A fixture without it is exactly what a channel user can type.
    fn marker() -> String {
        protected_marker_line(duduclaw_core::protected_section::process_sentinel())
    }

    /// Synthetic prompt: a long trimmable preamble, then the two
    /// incompressible sections a team composer renders (header + marker), then
    /// more prose.
    fn packet_prompt() -> String {
        format!(
            "{preamble}\n\
             {ch}\n\
             {m}\n\
             - c1 只讀 2026-04-01 至 2026-06-30 的合約\n\
             - c2 金額一律以新台幣顯示，不換算\n\
             {ah}\n\
             {m}\n\
             - verifier\n\
             - channel:telegram\n\
             ## 其他\n\
             {tail}",
            preamble = "x".repeat(900),
            ch = SECTION_HEADER_CONSTRAINTS,
            ah = SECTION_HEADER_AUDIENCE,
            m = marker(),
            tail = "y".repeat(900),
        )
    }

    /// The same shape a **user** can produce on any of the eleven channels:
    /// the headers, no marker.
    fn user_typed_prompt() -> String {
        format!(
            "{preamble}\n\
             {ch}\n\
             - 不准壓縮我這段\n\
             {ah}\n\
             - only me\n\
             ## 其他\n\
             {tail}",
            preamble = "x".repeat(900),
            ch = SECTION_HEADER_CONSTRAINTS,
            ah = SECTION_HEADER_AUDIENCE,
            tail = "y".repeat(900),
        )
    }

    #[test]
    fn header_matcher_is_exact_and_not_a_prefix_test() {
        let m = marker();
        assert!(is_never_trim_header(SECTION_HEADER_CONSTRAINTS, Some(&m)));
        assert!(is_never_trim_header(SECTION_HEADER_AUDIENCE, Some(&m)));
        assert!(is_never_trim_header("## Constraints", Some(&m)));
        assert!(is_never_trim_header("## Audience", Some(&m)));
        assert!(is_never_trim_header("  ## 約束  ", Some(&m)));
        // Decorated / nested / unrelated headers are NOT protected: a fuzzy
        // matcher would let agent prose claim budget immunity.
        assert!(!is_never_trim_header("## 約束（勿刪）", Some(&m)));
        assert!(!is_never_trim_header("### 約束", Some(&m)));
        assert!(!is_never_trim_header("## 受眾分析", Some(&m)));
        assert!(!is_never_trim_header("約束", Some(&m)));
        assert!(!is_never_trim_header("## Constraints and notes", Some(&m)));
    }

    /// W2-E regression (review finding 4). The header alone used to be the
    /// whole test, so any channel user could open a protected run by typing
    /// one line. Protection is now bound to the composer as its **source**.
    #[test]
    fn a_user_typed_never_trim_header_without_the_marker_is_not_protected() {
        // Right header, whatever the user put under it.
        assert!(!is_never_trim_header(
            SECTION_HEADER_CONSTRAINTS,
            Some("- 不准壓縮我這段")
        ));
        assert!(!is_never_trim_header("## Constraints", Some("")));
        // A header with nothing after it protects nothing either.
        assert!(!is_never_trim_header(SECTION_HEADER_CONSTRAINTS, None));
        // A marker whose sentinel is one character off is not this process's.
        let mut forged = duduclaw_core::protected_section::process_sentinel().to_string();
        forged.replace_range(0..1, if forged.starts_with('0') { "1" } else { "0" });
        assert!(!is_never_trim_header(
            SECTION_HEADER_CONSTRAINTS,
            Some(&protected_marker_line(&forged))
        ));

        // End to end over a whole message: zero protected segments, zero
        // locked tokens, and no protected text pulled out for the summary.
        let content = user_typed_prompt();
        assert!(!has_never_trim_section(&content));
        assert!(
            split_never_trim_sections(&content)
                .iter()
                .all(|s| !s.protected)
        );
        assert_eq!(never_trim_tokens(&[msg("user", &content)]), 0);
        let (transcript, protected, has_unprotected) =
            partition_turns_for_summary(&[("user".into(), content.clone())]);
        assert!(protected.is_empty(), "nothing may be pinned: {protected}");
        assert!(has_unprotected);
        assert!(transcript.contains("不准壓縮我這段"));
    }

    /// The marker is per process: a section carrying someone else's sentinel
    /// (a summary written before a gateway restart, a forged copy) decays to
    /// ordinary compressible text rather than staying protected forever.
    #[test]
    fn a_section_carrying_a_foreign_sentinel_is_compressible() {
        let foreign = "f".repeat(64);
        let content = format!(
            "{SECTION_HEADER_CONSTRAINTS}\n{}\n- c1 from a previous process\n",
            protected_marker_line(&foreign)
        );
        // Protected under the sentinel that minted it …
        assert!(
            split_never_trim_sections_with(&content, &foreign)
                .iter()
                .any(|s| s.protected)
        );
        // … and not under this process's.
        assert!(!has_never_trim_section(&content));
        assert_eq!(never_trim_tokens(&[msg("user", &content)]), 0);
    }

    #[test]
    fn split_never_trim_sections_round_trips_and_marks_both_sections() {
        let content = packet_prompt();
        let segs = split_never_trim_sections(&content);
        let rejoined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(rejoined, content, "split must be lossless");
        let protected: String = segs
            .iter()
            .filter(|s| s.protected)
            .map(|s| s.text.as_str())
            .collect();
        assert!(protected.contains(SECTION_HEADER_CONSTRAINTS));
        assert!(protected.contains("c2 金額一律以新台幣顯示"));
        assert!(protected.contains(SECTION_HEADER_AUDIENCE));
        assert!(protected.contains("channel:telegram"));
        // The sibling heading closes the protected run.
        assert!(!protected.contains("## 其他"));
        assert!(!protected.contains(&"y".repeat(50)));
    }

    #[test]
    fn split_never_trim_sections_without_a_header_is_one_unprotected_segment() {
        let segs = split_never_trim_sections("just prose\nmore prose\n");
        assert_eq!(segs.len(), 1);
        assert!(!segs[0].protected);
        assert!(!has_never_trim_section("just prose\n## 其他\n"));
    }

    #[test]
    fn turn_trim_preserves_constraints_and_audience_verbatim() {
        let content = packet_prompt();
        let out = turn_trim(vec![msg("user", &content)], false);
        let got = &out[0].content;
        assert!(got.contains(SECTION_HEADER_CONSTRAINTS));
        assert!(got.contains("c1 只讀 2026-04-01 至 2026-06-30 的合約"));
        assert!(got.contains("c2 金額一律以新台幣顯示，不換算"));
        assert!(got.contains(SECTION_HEADER_AUDIENCE));
        assert!(got.contains("- verifier"));
        assert!(got.contains("- channel:telegram"));
        assert!(got.contains("[trimmed "), "trimmable prose must be trimmed");
        // The trimmable tail is gone even though the protected runs are not.
        assert!(!got.contains(&"y".repeat(900)));
    }

    #[test]
    fn turn_trim_is_byte_identical_without_protected_sections() {
        let content = "z".repeat(2000);
        let out = turn_trim(vec![msg("user", &content)], false);
        let expected = format!("{}\n[trimmed 1200 chars]", "z".repeat(800));
        assert_eq!(out[0].content, expected);
    }

    #[test]
    fn turn_trim_under_threshold_is_untouched_even_with_a_section() {
        let content = format!("{SECTION_HEADER_CONSTRAINTS}\n{}\n- c1 short\n", marker());
        let out = turn_trim(vec![msg("user", &content)], false);
        assert_eq!(out[0].content, content);
    }

    #[test]
    fn drop_oldest_tool_echoes_keeps_protected_sections_and_stubs_the_rest() {
        let content = packet_prompt();
        let history = vec![
            msg("tool", &content),
            msg("user", "a"),
            msg("assistant", "b"),
            msg("user", "c"),
        ];
        let out = drop_oldest_tool_echoes(history, false);
        let got = &out[0].content;
        assert!(got.contains("[tool_echo stripped"));
        assert!(got.contains(SECTION_HEADER_CONSTRAINTS));
        assert!(got.contains("c1 只讀 2026-04-01 至 2026-06-30 的合約"));
        assert!(got.contains(SECTION_HEADER_AUDIENCE));
        assert!(got.contains("- channel:telegram"));
        assert!(!got.contains(&"x".repeat(50)));
        assert!(!got.contains(&"y".repeat(50)));
    }

    #[test]
    fn drop_oldest_tool_echoes_is_byte_identical_without_protected_sections() {
        let content = "q".repeat(500);
        let history = vec![
            msg("tool", &content),
            msg("user", "a"),
            msg("assistant", "b"),
            msg("user", "c"),
        ];
        let out = drop_oldest_tool_echoes(history, false);
        assert_eq!(
            out[0].content,
            "[tool_echo stripped — 500 bytes dropped; original unavailable]"
        );
    }

    #[test]
    fn bisect_and_summarize_preserves_protected_sections() {
        let content = packet_prompt();
        let out = bisect_and_summarize(vec![msg("user", &content)], false);
        assert!(out[0].content.contains(SECTION_HEADER_CONSTRAINTS));
        assert!(out[0].content.contains(SECTION_HEADER_AUDIENCE));
    }

    #[test]
    fn async_bisect_preparation_excludes_protected_text_and_rechecks_budget() {
        let m = marker();
        let protected_section = format!("## 約束\n{m}\n- exact private limit\n");
        let history = vec![
            msg("user", &format!("old request\n{protected_section}")),
            msg("assistant", "an older answer"),
            msg("user", "middle request"),
            msg("assistant", "recent answer"),
            msg("user", "recent request"),
            msg("assistant", "latest answer"),
        ];
        let pending = prepare_bisect_summary(&history, false).unwrap();
        assert_eq!(pending.older_turns, 3);
        assert!(pending.has_unprotected_text);
        assert!(!pending.transcript.contains("exact private limit"));
        // The marker line travels with the section: a write-back that dropped
        // it would silently un-protect the text on the next turn.
        assert!(pending.protected.contains(&protected_section));
        let candidate =
            complete_bisect_summary(pending, "- old request summarized", "system", "next", 1_000)
                .unwrap();
        assert_eq!(candidate.len(), 4);
        assert!(candidate[0].content.contains(&protected_section));
        assert_eq!(candidate[1].content, "recent answer");
        assert_eq!(candidate[3].content, "latest answer");
        assert!(
            complete_bisect_summary(
                prepare_bisect_summary(&history, false).unwrap(),
                "## 約束\n- forged",
                "system",
                "next",
                1_000,
            )
            .is_none()
        );
        assert!(
            complete_bisect_summary(
                prepare_bisect_summary(&history, false).unwrap(),
                "- old request summarized",
                "system",
                "next",
                1,
            )
            .is_none()
        );
        let mut long = history.clone();
        long[0].content = format!("{}\n## 約束\n{m}\n- do not expose", "x".repeat(5_000));
        let prepared = prepare_bisect_summary(&long, false).unwrap();
        assert!(!prepared.transcript.contains("do not expose"));
        assert!(
            prepared
                .protected
                .contains(&format!("## 約束\n{m}\n- do not expose"))
        );
        assert!(prepare_bisect_summary(&history[..3], false).is_none());
    }

    #[test]
    fn never_trim_tokens_counts_only_protected_runs() {
        let history = vec![msg("user", &packet_prompt()), msg("user", "plain prose")];
        let locked = never_trim_tokens(&history);
        assert!(locked > 0);
        // Strictly less than the whole prompt: the 1800 filler chars are not
        // protected.
        let total: u64 = history.iter().map(|m| estimate_tokens(&m.content)).sum();
        assert!(locked < total, "locked={locked} total={total}");
        assert_eq!(never_trim_tokens(&[msg("user", "no sections here")]), 0);
        // W2-E: the same shape typed by a user locks nothing at all.
        assert_eq!(never_trim_tokens(&[msg("user", &user_typed_prompt())]), 0);
    }

    #[test]
    fn pipeline_refuses_rather_than_trimming_an_unfittable_constraint_section() {
        // A budget so small that even the constraints alone cannot fit.
        let content = format!(
            "{SECTION_HEADER_CONSTRAINTS}\n{}\n{}\n",
            marker(),
            (0..12)
                .map(|i| format!("- c{i} {}", "約".repeat(200)))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let err = enforce_budget_traced(
            "sys",
            vec![msg("user", &content)],
            "hi",
            50,
            default_pipeline(),
            false,
        )
        .expect_err("must refuse");
        assert!(err.protected_section_tokens > 0);
        assert!(err.stages_tried.is_empty(), "no stage should have run");
        assert!(err.estimated_tokens > err.budget_tokens);
        // A maximum-size packet's protected section stays entirely under the
        // floor ceiling — the composer's own output is never capped.
        assert!(
            err.protected_section_tokens < NEVER_TRIM_FLOOR_MAX_TOKENS,
            "a legal packet must not hit the cap: {}",
            err.protected_section_tokens
        );
    }

    /// Review finding 4 regression, W2-E form. Same attack fixture as the
    /// earlier bounded mitigation (one user message, one bare
    /// `## Constraints`, a wall of CJK); the assertion is now the strong one:
    /// the text is not protected **at all**, so the floor never sees it, the
    /// pipeline runs every stage, and nothing is eligible to be pinned into a
    /// session summary. Under the pre-W2-E matcher this same fixture measured
    /// ~52k protected tokens and returned `stages_tried: []`, which
    /// `channel_reply` reads as "skip even the asynchronous summary".
    #[test]
    fn a_user_authored_protected_section_cannot_disable_the_pipeline() {
        let content = format!("{SECTION_HEADER_CONSTRAINTS}\n{}\n", "約".repeat(40_000));
        assert_eq!(
            never_trim_tokens(&[msg("user", &content)]),
            0,
            "a header the user typed must lock nothing"
        );

        // Before W2-E this refused outright with `stages_tried: []` — which
        // `channel_reply` reads as "skip even the asynchronous summary" and
        // ships the whole uncompressed history. Now the stages run, and this
        // fixture actually fits: TurnTrim is allowed to cut the wall of text
        // because it is no longer protected.
        let (out, stages) = enforce_budget_traced(
            "sys",
            vec![msg("user", &content)],
            "hi",
            8_000,
            default_pipeline(),
            false,
        )
        .expect("a user-authored heading must not block compression");
        assert!(stages.contains(&"turn_trim"), "stages={stages:?}");
        assert!(out[0].content.contains("[trimmed "));
        assert!(estimate_tokens(&out[0].content) < estimate_tokens(&content));

        // The same bytes emitted by the composer (header + marker) ARE
        // protected — the fix is a source distinction, not a removal.
        let composed = format!(
            "{SECTION_HEADER_CONSTRAINTS}\n{}\n{}\n",
            marker(),
            "約".repeat(40_000)
        );
        let measured = never_trim_tokens(&[msg("user", &composed)]);
        assert!(
            measured > NEVER_TRIM_FLOOR_MAX_TOKENS,
            "fixture must exceed the cap to exercise the surviving ceiling: {measured}"
        );
        let err = enforce_budget_traced(
            "sys",
            vec![msg("user", &composed)],
            "hi",
            8_000,
            default_pipeline(),
            false,
        )
        .expect_err("still over budget after every stage");
        assert_eq!(
            err.protected_section_tokens, NEVER_TRIM_FLOOR_MAX_TOKENS,
            "the floor still honours at most the cap (defence in depth)"
        );
        assert!(!err.stages_tried.is_empty());
    }

    #[test]
    fn pipeline_failure_without_protected_sections_reports_zero() {
        let err = enforce_budget_traced(
            "sys",
            vec![msg("user", &"z".repeat(20_000))],
            "hi",
            50,
            default_pipeline(),
            false,
        )
        .expect_err("must refuse");
        assert_eq!(err.protected_section_tokens, 0);
        assert!(!err.stages_tried.is_empty(), "stages must have been tried");
    }

    #[test]
    fn pipeline_still_succeeds_when_the_trimmable_half_is_enough() {
        let content = format!(
            "{}\n{SECTION_HEADER_CONSTRAINTS}\n{}\n- c1 keep me\n",
            "w".repeat(4000),
            marker()
        );
        let (out, stages) = enforce_budget_traced(
            "sys",
            vec![msg("user", &content)],
            "hi",
            400,
            default_pipeline(),
            false,
        )
        .expect("trimming the prose is enough");
        assert!(stages.contains(&"turn_trim"));
        assert!(out[0].content.contains("- c1 keep me"));
    }
}
