//! RM-204 (v1.84.0): fit an employee prompt into ONE command-line argument
//! without losing the parts that carry the conversation.
//!
//! Grok (`-p <payload>`) and the generic print-mode CLIs put the system
//! prompt, the conversation history and the message into a single argv
//! element. Linux limits one argv element to `MAX_ARG_STRLEN` = 32 pages =
//! 131,072 bytes; a longer element makes `execve` fail with `E2BIG`. The
//! v1.83.0 rule ("cut the system prompt's tail at 64 KB") dropped exactly the
//! sections that sit at the tail — the conversation summaries, the pinned
//! instructions, the historical-context note and the knowledge-base rules.
//!
//! This module replaces it with a budgeted, section-aware reduction:
//!
//! * the whole payload must stay under [`SINGLE_ARGUMENT_PAYLOAD_BUDGET`];
//! * protected sections ([`PromptSection::is_protected`]) are never dropped;
//! * the conversation's required context — the protected sections, the most
//!   recent [`MIN_RECENT_TURNS`] turns and the current message — must fit on
//!   its own; when it does not, [`try_single_argument_payload`] returns
//!   [`PromptTooLarge`] and the runtime refuses with
//!   [`prompt_too_large_error`] (plain zh-TW, explains what to do) instead of
//!   handing `execve` an argument it rejects with an opaque OS error;
//! * reduction order: oldest conversation turns first (down to the most
//!   recent [`MIN_RECENT_TURNS`]), then the low-priority middle sections
//!   (key facts, past mistakes, playbook, recent actions, …) from their ends,
//!   then the remaining recent turns, and only then the base employee prompt
//!   from its end. Conversation history and the middle sections carry
//!   attacker-influenced text; the base prompt carries the contract and
//!   boundary rules, the untrusted-content framing and the explore-lane
//!   restrictions, so base text is never cut while any of the former remain;
//! * one warning names what was reduced (labels only, never content).
//!
//! The section structure is recorded by `channel_reply::inner` while it
//! builds the prompt ([`PromptLayout`]) and reaches the runtime through the
//! task-local [`with_layout`], verified against the exact system prompt the
//! runtime received ([`current_layout`]). Without a matching layout the
//! prompt is treated as unstructured and reduced from its middle, keeping
//! its head and its tail (where the protected sections are appended).
//!
//! A payload under the budget is returned byte-identical to v1.83.0.

use std::sync::Arc;

use super::{ConversationTurn, RuntimeContext};

/// Whole single-argument payload budget, in bytes. Below the Linux
/// per-argument limit (`MAX_ARG_STRLEN`, 131,072 bytes) with room for the
/// framing the CLIs' own wrappers may add.
pub const SINGLE_ARGUMENT_PAYLOAD_BUDGET: usize = 120_000;

/// Conversation turns (entries) kept when older turns are dropped, unless the
/// base prompt would otherwise have to be cut. Part of the required context.
pub const MIN_RECENT_TURNS: usize = 3;

/// Start of the user-facing prompt-too-large refusal, right after the
/// engine-local markers ([`is_prompt_too_large_error`] anchors on it).
pub const PROMPT_TOO_LARGE_PREFIX: &str = "這段對話必要的內容太大";

/// The conversation's required context does not fit one argument.
#[derive(Debug, Clone)]
pub struct PromptTooLarge {
    /// The most-reduced payload (still over the budget, or the required
    /// context alone is); for callers that only estimate, never for argv.
    pub best_effort: String,
}

/// The refusal a single-argument runtime returns for [`PromptTooLarge`]:
/// an engine-local spawn-gate refusal (shown to the operator as written,
/// never counted against the engine's health), eligible for the configured
/// fallback engine only when that engine has no such limit
/// (`failover::fallback_rule::apply_fallback_engine_limits`).
pub fn prompt_too_large_error(runtime: &str) -> String {
    let label = duduclaw_core::types::RuntimeType::from_id(runtime)
        .map(|runtime| runtime.spec().display_name)
        .unwrap_or(runtime);
    duduclaw_agent::mcp_spawn_gate::engine_local_spawn_gate_error(&format!(
        "{PROMPT_TOO_LARGE_PREFIX}（對話摘要、固定指示、最近幾輪對話和這則訊息加起來），{label} 無法一次送出。\
         請開始新的對話，或縮短固定指示後再試。"
    ))
}

/// Is `error` a [`prompt_too_large_error`]? Anchored: both markers, then the
/// fixed wording.
pub fn is_prompt_too_large_error(error: &str) -> bool {
    duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(error)
        && duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(error)
            .is_some_and(|message| message.starts_with(PROMPT_TOO_LARGE_PREFIX))
}

/// Does `runtime` put the whole prompt in one command-line argument (and so
/// share this budget)? Claude and Codex read it otherwise (Codex: stdin), and
/// the API runtime has no argument at all.
pub fn runtime_has_single_argument_limit(runtime: duduclaw_core::types::RuntimeType) -> bool {
    use duduclaw_core::types::RuntimeType;
    !matches!(runtime, RuntimeType::Claude | RuntimeType::Codex | RuntimeType::OpenAiCompat)
}

/// Tail kept intact when a prompt has no verified layout (see module doc).
const UNSTRUCTURED_TAIL_BYTES: usize = 24 * 1024;

/// One labelled part of the employee system prompt, in text order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSection {
    /// The employee's base prompt (identity, soul, skills, wiki, …).
    Base,
    /// A low-priority middle section; the label names it in the log.
    Reducible(&'static str),
    /// Compression / rolling conversation summary.
    Summary,
    /// `## Pinned Task Instructions`.
    Pinned,
    /// The historical-reference note.
    HistoricalNote,
    /// The knowledge-base rules section.
    KnowledgeRules,
    /// A per-turn instruction that is short and must not be lost.
    Instruction,
}

impl PromptSection {
    pub fn is_protected(self) -> bool {
        matches!(self, Self::Summary | Self::Pinned | Self::HistoricalNote | Self::KnowledgeRules | Self::Instruction)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Base => "base employee prompt",
            Self::Reducible(label) => label,
            Self::Summary => "summary",
            Self::Pinned => "pinned instructions",
            Self::HistoricalNote => "historical note",
            Self::KnowledgeRules => "knowledge rules",
            Self::Instruction => "turn instruction",
        }
    }
}

/// Reduction order of the low-priority middle sections (labels used by
/// `channel_reply::inner`). A label not listed here is reduced last among
/// the middle sections.
pub const REDUCIBLE_ORDER: &[&str] = &[
    "key facts",
    "past mistakes",
    "playbook",
    "recent actions",
    "user profile",
    "working state",
    "open decisions",
    "preset",
];

/// The section boundaries of one system prompt, recorded while it is built
/// by appending (every boundary is a `String::len()` after an append, so a
/// char boundary). `text` is the exact prompt the boundaries describe.
#[derive(Debug, Clone, Default)]
pub struct PromptLayout {
    spans: Vec<(PromptSection, usize)>,
    text: String,
    invalid: bool,
}

impl PromptLayout {
    /// Everything appended to `prompt` since the previous mark is `kind`.
    /// Empty spans are kept (harmless). A prompt that shrank invalidates the
    /// layout: it was not built by appending.
    pub fn mark(&mut self, kind: PromptSection, prompt: &str) {
        let end = prompt.len();
        if self.spans.last().is_some_and(|(_, prev)| *prev > end) {
            self.invalid = true;
        }
        self.spans.push((kind, end));
    }

    /// The prompt was rewritten (e.g. redaction changed it): positions no
    /// longer describe it.
    pub fn invalidate(&mut self) {
        self.invalid = true;
    }

    /// Bind the layout to the final prompt text. `None` when the marks do not
    /// cover exactly that text.
    pub fn finish(mut self, prompt: &str) -> Option<Arc<Self>> {
        if self.invalid || self.spans.last().map(|(_, end)| *end) != Some(prompt.len()) {
            return None;
        }
        self.text = prompt.to_string();
        self.sections().map(|_| Arc::new(self))
    }

    /// `(kind, text)` in order; `None` when a boundary is not a char boundary.
    fn sections(&self) -> Option<Vec<(PromptSection, String)>> {
        let mut out = Vec::with_capacity(self.spans.len());
        let mut start = 0;
        for (kind, end) in &self.spans {
            let part = self.text.get(start..*end)?;
            out.push((*kind, part.to_string()));
            start = *end;
        }
        (start == self.text.len()).then_some(out)
    }
}

tokio::task_local! {
    static LAYOUT: Option<Arc<PromptLayout>>;
}

/// Run `future` with `layout` available to the runtimes it calls.
pub async fn with_layout<F: std::future::Future>(layout: Option<Arc<PromptLayout>>, future: F) -> F::Output {
    LAYOUT.scope(layout, future).await
}

/// The scoped layout, only when it describes exactly `system_prompt` (a
/// nested call with another prompt in the same task never borrows it).
pub fn current_layout(system_prompt: &str) -> Option<Arc<PromptLayout>> {
    LAYOUT
        .try_with(|layout| layout.clone())
        .ok()
        .flatten()
        .filter(|layout| layout.text == system_prompt)
}

/// Same framing as v1.83.0's Grok / generic CLI `build_prompt`, minus the cut.
fn assemble(system_prompt: &str, history: &[ConversationTurn], safe_prompt: &str) -> String {
    let with_history = if history.is_empty() {
        safe_prompt.to_string()
    } else {
        super::format_history_as_prompt_uncapped(history, safe_prompt)
    };
    if system_prompt.is_empty() {
        with_history
    } else {
        // Escape closing tag in the system prompt to keep the XML frame intact.
        let safe_system = system_prompt.replace("</system_instructions>", "&lt;/system_instructions&gt;");
        format!("<system_instructions>\n{safe_system}\n</system_instructions>\n\n{with_history}")
    }
}

/// No verified layout: head = base (reduced from its end), tail = protected.
fn unstructured_sections(system_prompt: &str) -> Vec<(PromptSection, String)> {
    if system_prompt.len() <= UNSTRUCTURED_TAIL_BYTES {
        return vec![(PromptSection::Base, system_prompt.to_string())];
    }
    let target = system_prompt.len() - UNSTRUCTURED_TAIL_BYTES;
    let split = system_prompt
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= target)
        .unwrap_or(system_prompt.len());
    let (head, tail) = system_prompt.split_at(split);
    vec![
        (PromptSection::Base, head.to_string()),
        (PromptSection::Instruction, tail.to_string()),
    ]
}

fn join(sections: &[(PromptSection, String)]) -> String {
    sections.iter().map(|(_, text)| text.as_str()).collect()
}

/// Build the single-argument payload for `runtime` (system instructions +
/// history + message), reduced to [`SINGLE_ARGUMENT_PAYLOAD_BUDGET`] when
/// needed. For callers that only measure the payload; a spawn uses
/// [`try_single_argument_payload`].
pub fn single_argument_payload(context: &RuntimeContext, user_prompt: &str, runtime: &str) -> String {
    try_single_argument_payload(context, user_prompt, runtime).unwrap_or_else(|too_large| too_large.best_effort)
}

/// The payload a spawn may put in one argument, or [`PromptTooLarge`] when
/// the conversation's required context alone exceeds the budget (or nothing
/// reducible is left and it still does not fit). See the module doc for the
/// order.
pub fn try_single_argument_payload(context: &RuntimeContext, user_prompt: &str, runtime: &str) -> Result<String, PromptTooLarge> {
    // Prevent argument injection: a prompt starting with '-' would be parsed as a flag.
    let safe_prompt = if user_prompt.starts_with('-') {
        format!(" {user_prompt}")
    } else {
        user_prompt.to_string()
    };
    let history = &context.conversation_history;
    let full = assemble(&context.system_prompt, history, &safe_prompt);
    if full.len() <= SINGLE_ARGUMENT_PAYLOAD_BUDGET {
        return Ok(full);
    }
    let structured = current_layout(&context.system_prompt).and_then(|layout| layout.sections());
    let layout_known = structured.is_some();
    let mut sections = structured.unwrap_or_else(|| unstructured_sections(&context.system_prompt));
    let required_bytes = required_context(&sections, history, &safe_prompt).len();
    let (payload, reduced) = reduce(&mut sections, history, &safe_prompt);
    let too_large = required_bytes > SINGLE_ARGUMENT_PAYLOAD_BUDGET || payload.len() > SINGLE_ARGUMENT_PAYLOAD_BUDGET;
    tracing::warn!(
        runtime,
        agent = %context.agent_id,
        original_bytes = full.len(),
        final_bytes = payload.len(),
        required_bytes,
        budget = SINGLE_ARGUMENT_PAYLOAD_BUDGET,
        layout_known,
        refused = too_large,
        reduced = ?reduced,
        "prompt payload reduced to fit one command-line argument"
    );
    if too_large {
        Err(PromptTooLarge { best_effort: payload })
    } else {
        Ok(payload)
    }
}

/// The protected sections, the most recent [`MIN_RECENT_TURNS`] turns and
/// the message, framed as they would be sent.
fn required_context(sections: &[(PromptSection, String)], history: &[ConversationTurn], safe_prompt: &str) -> String {
    let protected: Vec<(PromptSection, String)> = sections.iter()
        .filter(|(kind, _)| kind.is_protected())
        .cloned()
        .collect();
    let recent = &history[history.len().saturating_sub(MIN_RECENT_TURNS)..];
    assemble(&join(&protected), recent, safe_prompt)
}

fn fits(payload: &str) -> bool {
    payload.len() <= SINGLE_ARGUMENT_PAYLOAD_BUDGET
}

/// Cut the sections at `order` (in that order, each from its end) until the
/// payload fits; records each reduced label once.
fn cut_in_order(
    order: &[usize],
    sections: &mut [(PromptSection, String)],
    history: &[ConversationTurn],
    safe_prompt: &str,
    payload: &mut String,
    reduced: &mut Vec<&'static str>,
) {
    for &index in order {
        if fits(payload) {
            break;
        }
        let before = sections[index].1.len();
        while !fits(payload) && !sections[index].1.is_empty() {
            let overflow = payload.len() - SINGLE_ARGUMENT_PAYLOAD_BUDGET;
            let keep = sections[index].1.len().saturating_sub(overflow);
            let cut = duduclaw_core::truncate_bytes(&sections[index].1, keep).to_string();
            sections[index].1 = cut;
            *payload = assemble(&join(sections), history, safe_prompt);
        }
        if sections[index].1.len() < before {
            let label = sections[index].0.label();
            if !reduced.contains(&label) {
                reduced.push(label);
            }
        }
    }
}

/// The reduction itself; returns the payload and the labels reduced.
fn reduce(
    sections: &mut [(PromptSection, String)],
    history: &[ConversationTurn],
    safe_prompt: &str,
) -> (String, Vec<&'static str>) {
    let mut reduced: Vec<&'static str> = Vec::new();

    // 1. Oldest conversation turns, keeping the most recent ones.
    let floor = history.len().saturating_sub(MIN_RECENT_TURNS);
    let mut keep_from = 0;
    let mut payload = assemble(&join(sections), history, safe_prompt);
    while keep_from < floor && !fits(&payload) {
        keep_from += 1;
        payload = assemble(&join(sections), &history[keep_from..], safe_prompt);
    }
    if keep_from > 0 {
        reduced.push("older conversation turns");
    }

    // 2. Low-priority middle sections, in REDUCIBLE_ORDER, each from its end.
    let mut middle: Vec<usize> = Vec::new();
    for label in REDUCIBLE_ORDER {
        middle.extend(sections.iter().enumerate()
            .filter(|(_, (kind, _))| *kind == PromptSection::Reducible(label))
            .map(|(index, _)| index));
    }
    let unlisted: Vec<usize> = sections.iter().enumerate()
        .filter(|(index, (kind, _))| matches!(kind, PromptSection::Reducible(_)) && !middle.contains(index))
        .map(|(index, _)| index)
        .collect();
    middle.extend(unlisted);
    let base: Vec<usize> = sections.iter().enumerate()
        .filter(|(_, (kind, _))| *kind == PromptSection::Base)
        .map(|(index, _)| index)
        .collect();

    cut_in_order(&middle, sections, &history[keep_from..], safe_prompt, &mut payload, &mut reduced);

    // 3. The remaining recent turns, oldest first, before any base text: a
    // cut base prompt never sits next to conversation history.
    let before = keep_from;
    while keep_from < history.len() && !fits(&payload) {
        keep_from += 1;
        payload = assemble(&join(sections), &history[keep_from..], safe_prompt);
    }
    if keep_from > before {
        reduced.push("recent conversation turns");
    }

    // 4. Only then the base prompt from its end.
    cut_in_order(&base, sections, &history[keep_from..], safe_prompt, &mut payload, &mut reduced);
    if !fits(&payload) {
        // Protected sections and the message alone exceed the budget;
        // nothing protected is dropped.
        reduced.push("over budget after reduction");
    }
    (payload, reduced)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn context(system_prompt: String, history: Vec<ConversationTurn>) -> RuntimeContext {
        RuntimeContext {
            agent_dir: None, system_prompt, model: String::new(), max_tokens: 64,
            home_dir: std::path::PathBuf::from("/nonexistent"), agent_id: "budget-test".into(),
            preferred_provider: None, conversation_history: history, capabilities: None,
            account_pool: vec![], effort: None, allow_cross_family_failover: false,
        }
    }

    fn turns(count: usize, size: usize) -> Vec<ConversationTurn> {
        (0..count).map(|index| ConversationTurn {
            role: if index % 2 == 0 { "user" } else { "assistant" }.into(),
            content: format!("TURN-{index}:{}", "對話".repeat(size)),
        }).collect()
    }

    /// The prompt shape `channel_reply::inner` builds, with its layout.
    pub(crate) fn layered_prompt(base_bytes: usize, middle_bytes: usize) -> (String, Option<Arc<PromptLayout>>) {
        let mut layout = PromptLayout::default();
        let mut prompt = format!("BASE-HEAD {}", "基本".repeat(base_bytes / 6));
        layout.mark(PromptSection::Base, &prompt);
        prompt.push_str(&format!("\n\n## Key Facts About This User\n{}", "事實".repeat(middle_bytes / 6)));
        layout.mark(PromptSection::Reducible("key facts"), &prompt);
        prompt.push_str(&format!("\n\n## Recent actions\n{}", "動作".repeat(middle_bytes / 6)));
        layout.mark(PromptSection::Reducible("recent actions"), &prompt);
        prompt.push_str("\n\n## Internal conversation background\n<summary_data>\nSUMMARY-MARKER\n</summary_data>");
        layout.mark(PromptSection::Summary, &prompt);
        prompt.push_str("\n\n## Pinned Task Instructions\nPINNED-MARKER");
        layout.mark(PromptSection::Pinned, &prompt);
        prompt.push_str("\n\nHISTORICAL-NOTE-MARKER");
        layout.mark(PromptSection::HistoricalNote, &prompt);
        prompt.push_str("\n\n---\n\nKNOWLEDGE-RULES-MARKER");
        layout.mark(PromptSection::KnowledgeRules, &prompt);
        let layout = layout.finish(&prompt);
        (prompt, layout)
    }

    #[test]
    fn small_payload_is_byte_identical_to_the_v183_framing() {
        let history = turns(4, 10);
        let ctx = context("system </system_instructions> x".into(), history.clone());
        let expected = format!(
            "<system_instructions>\nsystem &lt;/system_instructions&gt; x\n</system_instructions>\n\n{}",
            crate::runtime::format_history_as_prompt(&history, " -flag message"));
        assert_eq!(single_argument_payload(&ctx, "-flag message", "grok"), expected);
        let bare = context(String::new(), vec![]);
        assert_eq!(single_argument_payload(&bare, "hi", "grok"), "hi");
    }

    #[tokio::test]
    async fn oversized_prompt_keeps_protected_sections_and_recent_turns_under_budget() {
        let (prompt, layout) = layered_prompt(70_000, 40_000);
        assert!(layout.is_some());
        let history = turns(40, 300);
        let ctx = context(prompt, history.clone());
        let payload = with_layout(layout, async { single_argument_payload(&ctx, "CURRENT-MESSAGE", "grok") }).await;
        assert!(payload.len() <= SINGLE_ARGUMENT_PAYLOAD_BUDGET, "{}", payload.len());
        for marker in ["SUMMARY-MARKER", "PINNED-MARKER", "HISTORICAL-NOTE-MARKER", "KNOWLEDGE-RULES-MARKER", "CURRENT-MESSAGE", "BASE-HEAD"] {
            assert!(payload.contains(marker), "{marker} lost");
        }
        for turn in &history[history.len() - MIN_RECENT_TURNS..] {
            let head: String = turn.content.chars().take(12).collect();
            assert!(payload.contains(&head), "recent turn {head} lost");
        }
        // Oldest turns go first.
        assert!(!payload.contains("TURN-0:"));
        // Order of the protected tail is unchanged.
        let at = |m: &str| payload.find(m).unwrap();
        assert!(at("SUMMARY-MARKER") < at("PINNED-MARKER") && at("PINNED-MARKER") < at("HISTORICAL-NOTE-MARKER")
            && at("HISTORICAL-NOTE-MARKER") < at("KNOWLEDGE-RULES-MARKER"));
        assert!(std::str::from_utf8(payload.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn middle_sections_are_reduced_before_the_base_prompt() {
        let (prompt, layout) = layered_prompt(40_000, 60_000);
        let ctx = context(prompt, turns(2, 5));
        let payload = with_layout(layout, async { single_argument_payload(&ctx, "m", "generic") }).await;
        assert!(payload.len() <= SINGLE_ARGUMENT_PAYLOAD_BUDGET);
        // The base prompt was not touched: middle sections absorbed the cut.
        assert!(payload.contains(&"基本".repeat(40_000 / 6)));
        assert!(payload.contains("PINNED-MARKER"));
    }

    #[tokio::test]
    async fn a_layout_for_another_prompt_is_not_borrowed() {
        let (_prompt, layout) = layered_prompt(10, 10);
        let ctx = context(format!("{}TAIL-PINNED", "x".repeat(200_000)), vec![]);
        let payload = with_layout(layout, async { single_argument_payload(&ctx, "m", "grok") }).await;
        assert!(payload.len() <= SINGLE_ARGUMENT_PAYLOAD_BUDGET);
        // Unstructured: head and tail kept, middle cut.
        assert!(payload.contains("TAIL-PINNED") && payload.starts_with("<system_instructions>\nxxx"));
    }

    /// The required context alone (protected sections, the last three turns
    /// and the message) over the budget: a typed refusal, never an oversized
    /// argument.
    #[tokio::test]
    async fn required_context_over_budget_is_a_typed_plain_language_refusal() {
        let (prompt, layout) = layered_prompt(10_000, 10_000);
        // Three huge recent turns.
        let ctx = context(prompt.clone(), turns(3, 25_000));
        let result = with_layout(layout.clone(), async { try_single_argument_payload(&ctx, "m", "grok") }).await;
        let too_large = result.expect_err("required context exceeds the budget");
        // The measuring entry point still returns something, never panics.
        let measured = with_layout(layout.clone(), async { single_argument_payload(&ctx, "m", "grok") }).await;
        assert_eq!(measured, too_large.best_effort);
        // A huge current message alone, too.
        let ctx = context(prompt.clone(), vec![]);
        let message = "訊息".repeat(25_000);
        assert!(with_layout(layout.clone(), async { try_single_argument_payload(&ctx, &message, "grok") }).await.is_err());
        // The same prompt with ordinary turns fits.
        let ctx = context(prompt, turns(40, 300));
        assert!(with_layout(layout, async { try_single_argument_payload(&ctx, "m", "grok") }).await.is_ok());
        // The refusal: engine-local, recognisable, plain zh-TW with what to do.
        let error = prompt_too_large_error("grok");
        assert!(is_prompt_too_large_error(&error));
        assert!(duduclaw_agent::mcp_spawn_gate::is_engine_local_spawn_gate_error(&error));
        let shown = duduclaw_agent::mcp_spawn_gate::spawn_gate_user_message(&error).unwrap();
        assert!(shown.starts_with(PROMPT_TOO_LARGE_PREFIX) && shown.contains("新的對話") && shown.contains("固定指示"), "{shown}");
        assert!(!shown.contains("argv") && !shown.contains("bytes"), "{shown}");
        assert!(!is_prompt_too_large_error(&duduclaw_agent::mcp_spawn_gate::spawn_gate_error(shown)), "employee gate is not it");
        assert!(!is_prompt_too_large_error(shown));
        // Which engines share the limit.
        use duduclaw_core::types::RuntimeType;
        for (runtime, limited) in [(RuntimeType::Claude, false), (RuntimeType::Codex, false),
            (RuntimeType::OpenAiCompat, false), (RuntimeType::Grok, true), (RuntimeType::Qwen, true)] {
            assert_eq!(runtime_has_single_argument_limit(runtime), limited, "{runtime:?}");
        }
    }

    /// Reducible, attacker-influenced text (conversation history, key facts
    /// and the other middle sections) is always gone before any base-prompt
    /// text is cut: whenever any of it is still in the payload, the base
    /// prompt's rules — at its very end here, the first place a cut hits —
    /// are intact.
    #[tokio::test]
    async fn base_prompt_rules_survive_whenever_history_or_key_facts_remain() {
        const RULES: [&str; 3] = ["CONTRACT-BOUNDARY-RULES", "UNTRUSTED-CONTENT-FRAMING", "EXPLORE-LANE-RESTRICTIONS"];
        fn prompt_with_rules(base_bytes: usize, middle_bytes: usize) -> (String, Option<Arc<PromptLayout>>) {
            let mut layout = PromptLayout::default();
            let mut prompt = format!("BASE-HEAD {}\n{}", "基本".repeat(base_bytes / 6), RULES.join("\n"));
            layout.mark(PromptSection::Base, &prompt);
            prompt.push_str(&format!("\n\n## Key Facts About This User\n{}", "事實".repeat(middle_bytes / 6)));
            layout.mark(PromptSection::Reducible("key facts"), &prompt);
            prompt.push_str(&format!("\n\n## Recent actions\n{}", "動作".repeat(middle_bytes / 6)));
            layout.mark(PromptSection::Reducible("recent actions"), &prompt);
            prompt.push_str("\n\n## Pinned Task Instructions\nPINNED-MARKER");
            layout.mark(PromptSection::Pinned, &prompt);
            let layout = layout.finish(&prompt);
            (prompt, layout)
        }
        let mut checked = 0;
        for base in [10_000, 60_000, 100_000, 118_000, 140_000] {
            for middle in [0, 6_000, 40_000, 90_000] {
                for (count, size) in [(0, 0), (2, 50), (3, 3_000), (12, 2_000), (40, 300)] {
                    let (prompt, layout) = prompt_with_rules(base, middle);
                    let ctx = context(prompt, turns(count, size));
                    let Ok(payload) = with_layout(layout, async { try_single_argument_payload(&ctx, "NOW", "grok") }).await else {
                        continue;
                    };
                    checked += 1;
                    assert!(payload.len() <= SINGLE_ARGUMENT_PAYLOAD_BUDGET);
                    assert!(payload.contains("PINNED-MARKER") && payload.ends_with("NOW"));
                    let reducible_left = payload.contains("TURN-") || payload.contains("事實") || payload.contains("動作")
                        || payload.contains("## Key Facts") || payload.contains("## Recent actions");
                    if reducible_left {
                        for rule in RULES {
                            assert!(payload.contains(rule), "{rule} cut while reducible text remains (base={base} middle={middle} turns={count}x{size})");
                        }
                    }
                }
            }
        }
        assert!(checked > 40, "the sweep must exercise real reductions ({checked})");
    }

    #[test]
    fn layout_rejects_text_it_does_not_describe() {
        let mut layout = PromptLayout::default();
        layout.mark(PromptSection::Base, "abc");
        assert!(layout.clone().finish("abcd").is_none());
        assert!(layout.clone().finish("abc").is_some());
        layout.invalidate();
        assert!(layout.finish("abc").is_none());
        let mut shrunk = PromptLayout::default();
        shrunk.mark(PromptSection::Base, "abcd");
        shrunk.mark(PromptSection::Pinned, "ab");
        assert!(shrunk.finish("ab").is_none());
    }
}
