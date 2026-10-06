//! Multi-runtime agent execution — Claude CLI, Codex CLI, Gemini CLI.
//!
//! The `AgentRuntime` trait abstracts over different CLI-based AI agents.
//! Each runtime translates its JSONL output format into a unified `RuntimeResponse`.

pub mod antigravity;
mod antigravity_setup;
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod generic_cli;
pub mod grok;
pub mod openai_compat;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;
use tracing::{info, warn};

// ── Core trait ──────────────────────────────────────────────────

/// Unified response from any agent runtime.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeResponse {
    pub content: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub model_used: String,
    pub runtime_name: String,
}

/// A single turn in conversation history.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationTurn {
    pub role: String, // "user" | "assistant"
    pub content: String,
}

/// Context passed to a runtime execution.
#[derive(Debug, Clone)]
pub struct RuntimeContext {
    /// Agent working directory (contains SOUL.md, CLAUDE.md, etc.)
    pub agent_dir: Option<PathBuf>,
    /// System prompt built from agent's loaded files.
    pub system_prompt: String,
    /// Model name to use (e.g., "claude-sonnet-4-6", "gpt-5", "gemini-2.5-flash").
    pub model: String,
    /// Maximum output tokens.
    pub max_tokens: u32,
    /// Home directory for config/key lookup.
    pub home_dir: PathBuf,
    /// Agent ID for telemetry.
    pub agent_id: String,
    /// Preferred OpenAI-compatible provider name (e.g. "minimax", "deepseek").
    /// Used by OpenAiCompatRuntime to resolve the correct API key first.
    pub preferred_provider: Option<String>,
    /// Conversation history for multi-turn context (chronological, newest last).
    /// Excludes the current user message. Empty on first turn.
    pub conversation_history: Vec<ConversationTurn>,
    /// Agent capability restrictions (`agent.toml [capabilities]`).
    ///
    /// `Some` when the caller resolved an agent directory — runtimes MUST
    /// translate these into their CLI's enforcement flags (Claude
    /// `--allowedTools`/`--disallowedTools`, Codex `--sandbox`, Gemini
    /// `--approval-mode`/`--sandbox`) and emit a structured `warn!` when the
    /// CLI cannot fully honor them. `None` (agent-less utility calls) keeps
    /// each runtime's legacy behavior.
    pub capabilities: Option<duduclaw_core::types::CapabilitiesConfig>,
    /// Agent account pool (`agent.toml [model] account_pool`).
    ///
    /// Narrows the [`AccountRotator`] candidate set for runtimes that rotate
    /// DuDuClaw-managed accounts (today: the Claude CLI runtime). Empty for
    /// agent-less utility calls and for agents that declare no pool — rotation
    /// is then unchanged. Runtimes whose credentials do not come from the
    /// rotator (codex / gemini / antigravity host logins) ignore it.
    ///
    /// [`AccountRotator`]: duduclaw_agent::account_rotator::AccountRotator
    pub account_pool: Vec<String>,
    /// Per-call reasoning effort (`agent.toml [model] effort`, or a team role
    /// spec's `effort`). P1/WP-3.
    ///
    /// Carried UNCLAMPED — each runtime clamps with
    /// [`duduclaw_core::effort::Effort::clamp_for`] against its own ceiling
    /// and translates it into its CLI's flag (`--effort`,
    /// `-c model_reasoning_effort=`, `--reasoning-effort`). Runtimes with no
    /// effort knob (gemini, the generic print-mode CLIs) log and ignore it.
    /// `None` ⇒ every spawn argv is byte-identical to before this field
    /// existed.
    pub effort: Option<duduclaw_core::effort::Effort>,
    /// May a failed primary runtime fail over to a runtime serving a
    /// **different model family**? (P0/WP-B follow-up.)
    ///
    /// `true` (every pre-existing caller) keeps
    /// [`crate::failover::FailoverManager::execute_with_failover`] exactly as
    /// it was: any healthy fallback is tried, with a model substituted for it.
    ///
    /// `false` is for calls whose whole point is *which family answers* — the
    /// decorrelated acceptance judge (`[dispatch] judge_provider`). A live test
    /// caught the hole this closes: a hinted `codex`/`gpt-5.6-sol` judge failed
    /// to spawn, failover substituted `claude-opus-4-6`, and the verdict came
    /// back from the worker's own family with nothing in the logs saying so.
    /// Same-family failover (another Claude tier) stays allowed — only the
    /// cross-family hop is refused, and the caller then degrades explicitly
    /// and audibly instead of being quietly re-routed.
    pub allow_cross_family_failover: bool,
}

/// Streaming chunk from a runtime execution.
#[derive(Debug, Clone)]
pub enum RuntimeChunk {
    Text(String),
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// `is_error` (T10, design §9): whether the paired tool call failed.
    /// Added alongside T10's codex/gemini producers — this variant had zero
    /// constructors anywhere in the repo before T10 (verified by grep), so
    /// adding a field here breaks no existing caller.
    ToolResult {
        output: String,
        is_error: bool,
    },
    Done(RuntimeResponse),
    Error(String),
}

/// T10 consumer-side fold (design §9's "消費端": "AgentRuntime execute 路徑
/// 把 chunks 中的工具事件彙整成與 WP-A4 同形態的收集結果上浮"): pair each
/// `ToolUse` chunk with the [`RuntimeChunk::ToolResult`] that follows it (in
/// emission order — codex/gemini both emit a tool's use and its result as
/// adjacent chunks, never interleaved with a different tool's pair, so
/// positional pairing is sufficient here — unlike claude_runner.rs's WP-A4
/// collector, which needs id-based pairing because Claude CAN interleave
/// multiple concurrent tool_use blocks before any tool_result arrives). A
/// `ToolUse` with no following `ToolResult` (stream ended mid-call) is still
/// recorded, success = true (provisional — an attempted call is still
/// evidence, same convention as every other producer in this module). This
/// is what keeps `runtime/codex.rs` and `runtime/gemini.rs` from having to
/// duplicate `NativeToolEvent`-building logic, and what keeps
/// `prediction::task_observe` from needing any runtime-specific code (design
/// §8.2's stated purpose for routing through `RuntimeChunk` here).
///
/// R1: also lifts `ToolUse.input`/`ToolResult.output` into
/// `NativeToolEvent::input_text`/`result_text` (masked + capped via
/// [`native_event_input_text_from_value`]/[`native_event_result_text`]) when
/// the producer populated them — `codex.rs`/`gemini.rs` (T10) now do for at
/// least some event shapes; a producer that still emits the pre-R1
/// placeholders (`input: Value::Null`, `output: String::new()`) yields `None`
/// for both, byte-identical to before.
pub fn native_tool_events_from_chunks(chunks: &[RuntimeChunk]) -> Vec<NativeToolEvent> {
    let mut events = Vec::new();
    let mut iter = chunks.iter().peekable();
    while let Some(chunk) = iter.next() {
        if let RuntimeChunk::ToolUse { name, input } = chunk {
            let input_text = native_event_input_text_from_value(input);
            let (success, result_text) = match iter.peek() {
                Some(RuntimeChunk::ToolResult { output, is_error }) => {
                    let is_error = *is_error;
                    let result_text = native_event_result_text(output);
                    iter.next(); // consume the paired ToolResult
                    (!is_error, result_text)
                }
                _ => (true, None), // unpaired — provisional success (see doc comment)
            };
            events.push(NativeToolEvent {
                tool_name: name.clone(),
                success,
                result_text,
                input_text,
            });
        }
    }
    events
}

// ── WP-A4/A5/T10: runtime-neutral native-tool collector ─────────────────
//
// See `commercial/docs/design-task-forward-model-2026-08-06.md` §5.3, §8.2,
// §9. Goal loop dispatch (`dispatcher.rs` → `claude_runner.rs`, and — for
// non-Claude agents — the same call chain's `runtime_dispatch::run_agent_prompt`
// → `AgentRuntime::execute`) is the ONE place native (non-MCP-audit) tool
// evidence can be captured for the A3 forward-model's `Full` fidelity. This
// struct is deliberately minimal and carries nothing provider-specific (no
// Claude `tool_use_id`, no codex `item.id`, no gemini `tool_id`) — the raw
// tool/command name plus a success flag is everything
// `prediction::tool_class::ToolClass::classify` needs, and everything the A3
// diff algorithm consumes downstream.

/// One native tool invocation observed during a single dispatch call,
/// runtime-neutral by construction.
///
/// R1 (2026-08, `wiki/reports/memory-quality/2026-08/wp-a10-live-test-2026-08-06.md`
/// §6): `result_text`/`input_text` let this evidence actually participate in
/// the B3 grounding pre-check (`dispatch_engine::grounding_precheck`) — before
/// R1 a native event carried only `tool_name`/`success`, so an honest task
/// that only used native tools (Read/Write/Bash) could never reach
/// `Grounded`, only `Degraded`. Both fields are populated ONLY through
/// [`native_event_input_text`]/[`native_event_result_text`] (or a producer
/// that already delegates to them, e.g. [`native_tool_events_from_chunks`]) —
/// those helpers mask (`duduclaw_security::audit::mask_sensitive_text`) BEFORE
/// truncating, so no unmasked tool text is ever allowed to land here. `None`
/// when the source runtime's event stream never captured any (never
/// fabricated) — the pre-R1 producers/tests that only ever set
/// `tool_name`/`success` keep that meaning unchanged.
#[derive(Debug, Clone)]
pub struct NativeToolEvent {
    pub tool_name: String,
    pub success: bool,
    /// Masked + CJK-safe-truncated tool result text, when the producer's
    /// event stream carried one. Capped at [`NATIVE_EVENT_RESULT_MAX_CHARS`].
    pub result_text: Option<String>,
    /// Masked + CJK-safe-truncated tool call input/arguments text, when the
    /// producer's event stream carried one. Capped at
    /// [`NATIVE_EVENT_INPUT_MAX_CHARS`]. Used by the B3 grounding pre-check's
    /// Fix-2 C1b self-echo subtraction
    /// (`duduclaw_core::grounding::shares_contiguous_run_excluding_echo`) —
    /// native tools have no `SELF_ECHO_TOOL_NAMES` deny-list concept, but the
    /// same "don't ground a claim on its own echoed input" logic still
    /// applies whenever input text happens to be available.
    pub input_text: Option<String>,
}

/// Char cap for [`NativeToolEvent::input_text`] — reuses the audit trail's
/// own cap (`tool_calls.jsonl`'s `input` field) so the same tool's text is
/// bounded identically regardless of which capture path recorded it.
pub const NATIVE_EVENT_INPUT_MAX_CHARS: usize = duduclaw_security::audit::AUDIT_INPUT_MAX_CHARS;
/// Char cap for [`NativeToolEvent::result_text`] — reuses the audit trail's
/// own cap (`tool_calls.jsonl`'s `result_text` field), see
/// [`NATIVE_EVENT_INPUT_MAX_CHARS`].
pub const NATIVE_EVENT_RESULT_MAX_CHARS: usize =
    duduclaw_security::audit::AUDIT_RESULT_TEXT_MAX_CHARS;

/// Mask + CJK-safe-truncate raw text before it is allowed to become a
/// [`NativeToolEvent::input_text`]. The ONLY sanctioned way to populate that
/// field — masking always runs before truncation (a secret split across the
/// truncation boundary must still be caught). An empty/all-whitespace result
/// (nothing captured, or the source text was empty) is `None`, never an
/// empty-string placeholder.
pub fn native_event_input_text(raw: &str) -> Option<String> {
    mask_and_cap(raw, NATIVE_EVENT_INPUT_MAX_CHARS)
}

/// Same contract as [`native_event_input_text`], for
/// [`NativeToolEvent::result_text`].
pub fn native_event_result_text(raw: &str) -> Option<String> {
    mask_and_cap(raw, NATIVE_EVENT_RESULT_MAX_CHARS)
}

/// [`native_event_input_text`] variant for a tool call's raw
/// `serde_json::Value` input/arguments — stringifies first (a bare JSON
/// string is used verbatim, anything else is compact-serialized), then masks
/// and caps exactly like the `&str` entry point. `Null` and an empty string
/// are both "nothing captured" ⇒ `None`.
pub fn native_event_input_text_from_value(v: &serde_json::Value) -> Option<String> {
    let text = match v {
        serde_json::Value::Null => return None,
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    native_event_input_text(&text)
}

/// Shared mask-then-truncate-then-empty-check tail for
/// [`native_event_input_text`]/[`native_event_result_text`].
fn mask_and_cap(raw: &str, max_chars: usize) -> Option<String> {
    let masked = duduclaw_security::audit::mask_sensitive_text(raw);
    let trimmed = masked.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(duduclaw_core::truncate_chars(trimmed, max_chars))
}

tokio::task_local! {
    /// Sink for [`NativeToolEvent`]s collected while ONE dispatch call runs,
    /// populated by whichever execution path actually handles it: Claude CLI
    /// stream-json parsing in `claude_runner.rs::call_claude_streaming`
    /// (WP-A4), or an `AgentRuntime::execute` implementation below —
    /// `codex.rs`/`gemini.rs` (T10) and `openai_compat.rs` (WP-A5).
    ///
    /// The caller (`dispatcher.rs`, when dispatching a goal-loop task) enters
    /// this scope before invoking the call chain and reads the accumulated
    /// events back afterward. Task-locals thread transparently through the
    /// entire async call chain within the same tokio task (ordinary
    /// function/`.await` boundaries, NOT `tokio::spawn`), so no intermediate
    /// function in the chain (`call_claude_for_agent_impl`,
    /// `runtime_dispatch::run_agent_prompt[_text]`) needs to know this
    /// collector exists.
    ///
    /// Absent scope — every non-goal-loop dispatch path: channel reply,
    /// cron, reminders, ACP — is a complete no-op. Every write site below
    /// uses `try_with`, never panics on a missing scope, and never affects
    /// the response returned to the caller (design R5: "forward model 不得
    /// 成為派工失敗原因").
    pub static NATIVE_TOOL_COLLECTOR: std::sync::Arc<std::sync::Mutex<Vec<NativeToolEvent>>>;
}

/// Best-effort bulk push into [`NATIVE_TOOL_COLLECTOR`], if a caller has
/// scoped one. A missing scope, or the mutex being poisoned by an unrelated
/// panic elsewhere, degrades to a silent no-op — collector failure must
/// never surface as a dispatch failure (design R5). No-op on an empty batch
/// (avoids taking the lock for nothing on the common non-goal-loop path).
pub fn extend_native_tool_events(events: Vec<NativeToolEvent>) {
    if events.is_empty() {
        return;
    }
    let _ = NATIVE_TOOL_COLLECTOR.try_with(|collector| {
        if let Ok(mut guard) = collector.lock() {
            guard.extend(events);
        }
    });
}

// ── Spawn-scope overrides + truthful runtime attribution ────────────────
//
// Team-as-Agent live round 3 (design `DESIGN-team-as-agent-2026-09.md` §4.3,
// findings E2/E3) produced two defects these two task-locals close:
//
// * **E3** — a role member ran with cwd = its own throwaway scaffold, wrote
//   its files there, and immediate GC deleted them; the verifier then
//   correctly found "no tool activity supports file creation".
// * **E2** — an executor member configured for codex silently failed over to
//   Claude and `role_turns.jsonl` still recorded `runtime=codex … completed`.
//   The ledger lied about which family did the work.
//
// Both are caller-scoped facts that must reach code far down the call chain
// (`runtime/codex.rs`'s `--cd`, `failover.rs`'s substitution) through
// intermediate signatures this work package must not churn
// (`runtime_dispatch::AgentPrompt`, `RuntimeContext`, every runtime module's
// `RuntimeResponse` literal). They use exactly the mechanism
// [`NATIVE_TOOL_COLLECTOR`] above already established: a task-local set by the
// dispatching scope, read with `try_with`, a complete no-op when absent.

/// Spawn-shape overrides one caller imposes on whichever runtime answers.
#[derive(Debug, Clone, Default)]
pub struct SpawnOverride {
    /// Working directory the CLI subprocess must run in, when it has to
    /// differ from [`RuntimeContext::agent_dir`].
    ///
    /// `agent_dir` stays the member's own scaffold (it is what defines the
    /// member's identity and config); this is only the **cwd**. Today the one
    /// caller is the team composer, which puts role members in the employee's
    /// workspace so their files outlive the scaffold.
    pub work_dir: Option<PathBuf>,
}

/// The `(runtime, model)` pair that ACTUALLY answered one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOutcome {
    pub runtime: RuntimeType,
    /// The model id handed to that runtime — the *substituted* one after a
    /// failover, not the one originally requested.
    pub model: String,
}

/// Attribution carried by a single composer dispatch into the existing cost
/// recorder. The task id is the episode id; no global mutable role state.
#[derive(Debug, Clone)]
pub struct RoleCostAttribution {
    pub role: &'static str,
    pub episode_id: String,
}

/// A1-3 ledger: which goal task and round a dispatch belongs to, so every
/// `token_usage` row it records carries `episode_id` = task id and
/// `round` = the task's `task_iterations.round` (`revision_round + 1` at
/// dispatch). Scoped by the SQLite-queue dispatcher around a goal-loop work
/// message and by the team composer around a team round. Absent scope ⇒ both
/// columns stay NULL, exactly as before. Recording only — never read by any
/// routing or budget decision.
#[derive(Debug, Clone)]
pub struct GoalRoundAttribution {
    pub episode_id: String,
    /// `None` when the round could not be resolved (the task-id attribution
    /// is still kept).
    pub round: Option<i64>,
}

tokio::task_local! {
    /// Caller-imposed spawn overrides for this call — see [`SpawnOverride`].
    /// Absent scope ⇒ every runtime builds a byte-identical argv.
    pub static SPAWN_OVERRIDE: SpawnOverride;

    /// Sink for the [`RuntimeOutcome`] of this call, when the caller needs to
    /// record which runtime/model really answered. Absent scope ⇒ no-op.
    pub static RUNTIME_OUTCOME: std::sync::Arc<std::sync::Mutex<Option<RuntimeOutcome>>>;

    /// Usage observed on the answering leg of a team member's dispatch.
    /// This is independent of the global cost ledger so a role turn can carry
    /// its own measured tokens without inferring them from agent-level totals.
    pub static ROLE_USAGE: std::sync::Arc<std::sync::Mutex<crate::role_turns::RoleTurnUsage>>;

    pub static ROLE_COST_ATTRIBUTION: RoleCostAttribution;

    /// See [`GoalRoundAttribution`].
    pub static GOAL_ROUND_ATTRIBUTION: GoalRoundAttribution;
}

/// The caller's cwd override for this spawn, if any. `None` outside a
/// [`SPAWN_OVERRIDE`] scope — the universal case.
pub fn spawn_work_dir_override() -> Option<PathBuf> {
    SPAWN_OVERRIDE
        .try_with(|o| o.work_dir.clone())
        .ok()
        .flatten()
}

/// The working root one CLI spawn must actually run in: the caller's
/// [`SPAWN_OVERRIDE`] `work_dir` when it names a real directory, otherwise
/// `agent_dir`.
///
/// Single resolution point for every runtime module, for two reasons found in
/// the 2026-09-28 review:
///
/// * only `runtime/codex.rs` read the override at all — `gemini.rs`,
///   `antigravity.rs` and `grok.rs` used `context.agent_dir` unconditionally,
///   so a team role member on any of those three ran in its own throwaway
///   scaffold and its files were deleted by the immediate GC at
///   `finish_role_member` (design §4.3 E3, the exact defect the override was
///   introduced to fix — it was simply never wired past codex);
/// * the override was consumed without checking that it exists. A stale or
///   mistyped path produced a spawn in a nonexistent cwd instead of falling
///   back. `claude_runner.rs`'s dispatch path already validates with
///   `is_dir()` + `warn!` + fallback; this is the same rule for the CLI
///   runtimes, in one place so the four cannot drift apart again.
///
/// The override moves the **cwd only**. Identity (`DUDUCLAW_AGENT_ID`, the
/// per-agent MCP env block, `agent_dir`-scoped config) is unaffected.
pub fn resolve_spawn_work_dir(agent_dir: Option<&Path>, agent_id: &str) -> Option<PathBuf> {
    match spawn_work_dir_override() {
        Some(dir) if dir.is_dir() => Some(dir),
        Some(dir) => {
            tracing::warn!(
                agent = %agent_id,
                requested = %dir.display(),
                "spawn work_dir override does not exist — falling back to the agent directory"
            );
            agent_dir.map(PathBuf::from)
        }
        None => agent_dir.map(PathBuf::from),
    }
}

/// Best-effort record of which runtime/model actually answered. Last writer
/// wins: a failover's fallback leg overwrites the primary's (failed) attempt,
/// so the value left behind is always the one that produced the response.
/// Missing scope / poisoned mutex ⇒ silent no-op (attribution telemetry must
/// never fail a call that succeeded).
pub fn record_runtime_outcome(runtime: RuntimeType, model: &str) {
    let _ = RUNTIME_OUTCOME.try_with(|slot| {
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(RuntimeOutcome {
                runtime,
                model: model.to_string(),
            });
        }
    });
}

/// Fold ONE answering leg's token usage into this member stage's total.
///
/// Unlike [`record_runtime_outcome`] (where last-writer-wins is the right
/// answer — the final leg is the one that produced the response), usage is
/// **cumulative**: a failover's primary attempt and a tool loop's intermediate
/// calls were all really paid for. Before 2026-09-28 this was `*guard = usage`,
/// so `role_turns.jsonl` published only the last leg while `cost_telemetry`
/// recorded every call — see [`crate::role_turns::RoleTurnUsage::accumulate`]
/// for the per-dimension rules.
///
/// Missing scope / poisoned mutex ⇒ silent no-op, same as the outcome sink:
/// telemetry must never fail a call that succeeded.
pub fn record_role_usage(usage: crate::role_turns::RoleTurnUsage) {
    let _ = ROLE_USAGE.try_with(|slot| {
        if let Ok(mut guard) = slot.lock() {
            guard.accumulate(usage);
        }
    });
}

/// Abstract runtime for executing agent tasks.
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Human-readable name of this runtime.
    fn name(&self) -> &str;

    /// Execute a prompt and return the response.
    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String>;

    /// Check if this runtime is available (CLI installed, API key configured, etc.)
    async fn is_available(&self) -> bool;
}

// ── Runtime type enum ───────────────────────────────────────────

// Re-export RuntimeType from core
pub use duduclaw_core::types::RuntimeType;

// ── Registry ────────────────────────────────────────────────────

/// Catalog ids that have a hand-written runtime module in this directory.
///
/// Everything else in `runtime_catalog` is driven by [`generic_cli`]. Kept at
/// module scope so `bespoke_ids_match_the_modules_that_exist` can prove the two
/// stay in step: a new bespoke module that is not listed here would never be
/// reached (the generic driver would shadow it), and a listed id with no module
/// would silently drop that runtime from the registry entirely.
pub(crate) const BESPOKE_RUNTIME_IDS: &[&str] =
    &["claude", "codex", "gemini", "antigravity", "grok"];

/// Registry of available runtimes, auto-detected at startup.
pub struct RuntimeRegistry {
    runtimes: HashMap<RuntimeType, Box<dyn AgentRuntime>>,
}

impl RuntimeRegistry {
    /// Create a new registry and auto-detect available runtimes.
    pub async fn new(home_dir: &Path) -> Self {
        let mut runtimes: HashMap<RuntimeType, Box<dyn AgentRuntime>> = HashMap::new();

        // Claude is always available (it's the core)
        runtimes.insert(
            RuntimeType::Claude,
            Box::new(claude::ClaudeRuntime::new(home_dir.to_path_buf())),
        );

        // Codex: check if `codex` CLI is installed
        let codex = codex::CodexRuntime::new();
        if codex.is_available().await {
            info!("Codex CLI detected — registering CodexRuntime");
            runtimes.insert(RuntimeType::Codex, Box::new(codex));
        }

        // Gemini: check if `gemini` CLI is installed
        let gemini = gemini::GeminiRuntime::new();
        if gemini.is_available().await {
            info!("Gemini CLI detected — registering GeminiRuntime");
            runtimes.insert(RuntimeType::Gemini, Box::new(gemini));
        }

        // Antigravity (`agy`): the 2026-06-18 successor to the personal Gemini CLI.
        let antigravity = antigravity::AntigravityRuntime::new();
        if antigravity.is_available().await {
            info!("Antigravity CLI (agy) detected — registering AntigravityRuntime");
            runtimes.insert(RuntimeType::Antigravity, Box::new(antigravity));
        }

        // Grok (`grok` / third-party `grok-cli`): check if the CLI is installed.
        let grok = grok::GrokRuntime::new();
        if grok.is_available().await {
            info!("Grok CLI detected — registering GrokRuntime");
            runtimes.insert(RuntimeType::Grok, Box::new(grok));
        }

        // WP-B: every remaining catalog runtime is a print-mode CLI driven by
        // the ONE generic implementation (`generic_cli.rs`) from its
        // `runtime_catalog` entry — Qwen Code, Kimi Code, GitHub Copilot CLI,
        // Kiro CLI, Cursor, Mistral Vibe, OpenCode today, and anything added to
        // the catalog tomorrow without touching this function.
        //
        // The five above keep bespoke modules because each has real per-vendor
        // wiring the generic driver has no place for (account rotation, MCP
        // config injection, capability→sandbox-flag translation, PTY recovery).
        // They are excluded by NAME rather than by `runtimes.contains_key`: a
        // bespoke runtime whose own availability probe said "no" must stay
        // absent so failover fires, not be quietly replaced by a generic driver
        // that would hide the broken install.
        //
        // Registration is gated on the binary actually being present, exactly
        // like the bespoke ones: a runtime that is configured but not installed
        // must fail over, not spawn a missing program.
        let user_home = std::path::PathBuf::from(duduclaw_core::platform::home_dir());
        for spec in duduclaw_core::runtime_catalog::cli_specs() {
            if BESPOKE_RUNTIME_IDS.contains(&spec.id) {
                continue;
            }
            let Some(rt) = RuntimeType::from_id(spec.id) else {
                continue;
            };
            if let Some(runtime) = generic_cli::GenericCliRuntime::detect(spec, &user_home) {
                info!(
                    runtime = spec.id,
                    output = spec.headless.output.as_str(),
                    verified = spec.verified,
                    "{} detected — registering generic print-mode runtime",
                    spec.display_name
                );
                runtimes.insert(rt, Box::new(runtime));
            }
        }

        // OpenAI-compatible: always available if API key is configured
        runtimes.insert(
            RuntimeType::OpenAiCompat,
            Box::new(openai_compat::OpenAiCompatRuntime::new()),
        );

        Self { runtimes }
    }

    /// Test-only constructor: build a registry from explicit stub runtimes so
    /// failover behavior can be exercised without real CLIs/APIs.
    #[cfg(test)]
    pub(crate) fn with_runtimes(runtimes: HashMap<RuntimeType, Box<dyn AgentRuntime>>) -> Self {
        Self { runtimes }
    }

    /// Get a runtime by type.
    pub fn get(&self, runtime_type: &RuntimeType) -> Option<&dyn AgentRuntime> {
        self.runtimes.get(runtime_type).map(|r| r.as_ref())
    }

    /// Get the runtime for an agent config, with fallback.
    pub fn select(
        &self,
        primary: &RuntimeType,
        fallback: Option<&RuntimeType>,
    ) -> Option<&dyn AgentRuntime> {
        self.get(primary).or_else(|| {
            if let Some(fb) = fallback {
                let rt = self.get(fb);
                if rt.is_some() {
                    warn!(
                        primary = ?primary,
                        fallback = ?fb,
                        "Primary runtime unavailable, using fallback"
                    );
                }
                rt
            } else {
                None
            }
        })
    }

    /// List all available runtime types.
    pub fn available(&self) -> Vec<(&RuntimeType, &str)> {
        self.runtimes.iter().map(|(t, r)| (t, r.name())).collect()
    }
}

// ── Helpers ─────────────────────────────────────────────────────

/// Load `agent.toml [capabilities]` for an agent directory.
///
/// Returns `None` only when `agent.toml` itself is missing (synthetic /
/// test agent ids) — callers then keep their legacy capability-less
/// behavior. When the file exists but `[capabilities]` is absent OR fails
/// to deserialize, this returns `Some(CapabilitiesConfig::default())`
/// (deny-by-default: `computer_use = false`, `browser_via_bash = false`)
/// with a warn on the malformed case — security gates fail closed.
pub fn load_agent_capabilities(
    agent_dir: &Path,
) -> Option<duduclaw_core::types::CapabilitiesConfig> {
    let path = agent_dir.join("agent.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let parsed = match text.parse::<toml::Value>() {
        Ok(v) => v,
        Err(e) => {
            warn!(
                agent_dir = %agent_dir.display(),
                error = %e,
                "agent.toml parse failed — applying default (deny-by-default) capabilities"
            );
            return Some(duduclaw_core::types::CapabilitiesConfig::default());
        }
    };
    let caps = match parsed.get("capabilities") {
        None => duduclaw_core::types::CapabilitiesConfig::default(),
        Some(section) => match section.clone().try_into() {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    agent_dir = %agent_dir.display(),
                    error = %e,
                    "[capabilities] section malformed — applying default (deny-by-default) capabilities"
                );
                duduclaw_core::types::CapabilitiesConfig::default()
            }
        },
    };
    Some(caps)
}

/// Read an agent's `[model] account_pool` from `agent.toml`.
///
/// Sibling of [`load_agent_capabilities`] — deliberately a lightweight direct
/// read rather than a full `AgentConfig` parse, because the choke-point runs on
/// paths that never loaded the registry. Missing file / unreadable / malformed
/// section ⇒ empty pool ⇒ rotation unchanged (fail-open, matching the
/// rotator's own stale-pool semantics).
pub fn load_agent_account_pool(agent_dir: &Path) -> Vec<String> {
    let path = agent_dir.join("agent.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(parsed) = text.parse::<toml::Value>() else {
        warn!(
            agent_dir = %agent_dir.display(),
            "agent.toml parse failed — ignoring [model] account_pool (full account set)"
        );
        return Vec::new();
    };
    parsed
        .get("model")
        .and_then(|m| m.get("account_pool"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Build the `duduclaw` MCP server definition for a non-Claude CLI's native
/// MCP config (Codex `config.toml [mcp_servers]`, Gemini/Antigravity
/// `settings.json mcpServers`). Mirrors
/// `duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path` for Claude:
/// absolute `duduclaw` binary + `mcp-server` arg + `DUDUCLAW_AGENT_ID` env
/// (the MCP subprocess self-identifies through it — without it every call
/// falls back to `default_agent` and supervisor authorization breaks), plus
/// this instance's `DUDUCLAW_HOME` / `DUDUCLAW_PORT` / `DUDUCLAW_INSTANCE`
/// overrides when set (multi-instance isolation, Plan A).
///
/// Returns `None` when the duduclaw binary cannot be resolved to an absolute
/// path — registering a PATH-relative command would break for CLI subprocesses
/// launched without PATH inheritance.
pub fn duduclaw_mcp_server_json(agent_id: &str) -> Option<serde_json::Value> {
    duduclaw_mcp_server_json_for_home(agent_id, &duduclaw_core::duduclaw_home())
}

/// MCP definition for a runtime call whose home may be an isolated eval arm.
/// The supplied home, rather than ambient process state, owns identity and
/// task writes. Credentials still flow through the existing internal key path.
pub fn duduclaw_mcp_server_json_for_home(
    agent_id: &str,
    home_dir: &Path,
) -> Option<serde_json::Value> {
    let bin = duduclaw_core::resolve_duduclaw_bin();
    if !bin.is_absolute() {
        return None;
    }
    let mut env = serde_json::Map::new();
    // Identity pair: `DUDUCLAW_AGENT_ID` plus, when `<home>/identity.key`
    // exists, the WP21 debt ⑧ `DUDUCLAW_AGENT_TOKEN` that proves DuDuClaw
    // issued that id. No key ⇒ id only, i.e. the pre-WP21 env block verbatim.
    for (k, v) in duduclaw_core::agent_identity_env_vars(home_dir, agent_id) {
        env.insert(k, serde_json::Value::String(v));
    }
    // Shared forward set (home/port/instance + MCP auth). CLIs like Grok spawn
    // MCP children with ONLY this declared env block — a missing
    // DUDUCLAW_MCP_API_KEY here means the server dies at boot (M6 fail-closed)
    // and the agent silently loses every duduclaw tool.
    for (k, v) in duduclaw_core::mcp_forward_env_vars() {
        env.insert(k, serde_json::Value::String(v));
    }
    env.insert(
        "DUDUCLAW_HOME".to_string(),
        serde_json::Value::String(home_dir.to_string_lossy().to_string()),
    );
    if let Some((k, v)) = round_task_env() {
        env.insert(k, serde_json::Value::String(v));
    }
    Some(serde_json::json!({
        "command": bin.to_string_lossy(),
        "args": ["mcp-server"],
        "env": serde_json::Value::Object(env),
    }))
}

/// `(DUDUCLAW_TASK_ID, task)` when this call runs inside a goal round's (or
/// heartbeat wake-up's) cost attribution scope: the one host-provided "task
/// this round runs for" (F5-D [`duduclaw_core::ENV_TASK_ID`]). Every runtime
/// passes it to its MCP server from here, so approval cards (F5-D) and the
/// parent of tasks created during a responsibility run (P2-A H-2) read the
/// same value. Same validity rule as `duduclaw_core::host_task_id`.
pub fn round_task_env() -> Option<(String, String)> {
    GOAL_ROUND_ATTRIBUTION
        .try_with(|g| g.episode_id.clone())
        .ok()
        .filter(|id| duduclaw_core::is_valid_agent_id(id))
        .map(|id| (duduclaw_core::ENV_TASK_ID.to_string(), id))
}

/// Format conversation history as an XML-delimited prompt prefix.
///
/// Used by CLI-based runtimes (Gemini, Codex) that lack native multi-turn
/// support, and as a fallback for Claude CLI when `--resume` is unavailable.
///
/// NOTE: The canonical implementation with turn trimming lives in
/// `channel_reply.rs`. This version is for the AgentRuntime trait path.
pub fn format_history_as_prompt(history: &[ConversationTurn], current_message: &str) -> String {
    if history.is_empty() {
        return current_message.to_string();
    }
    let mut buf = String::with_capacity(history.len() * 200 + current_message.len() + 64);
    buf.push_str("<conversation_history>\n");
    for turn in history {
        // Escape closing tags in content to prevent XML structure corruption
        let safe_content = turn
            .content
            .replace("</user>", "&lt;/user&gt;")
            .replace("</assistant>", "&lt;/assistant&gt;");
        buf.push('<');
        buf.push_str(&turn.role);
        buf.push('>');
        buf.push_str(&safe_content);
        buf.push_str("</");
        buf.push_str(&turn.role);
        buf.push_str(">\n");
    }
    buf.push_str("</conversation_history>\n\n");
    buf.push_str(current_message);
    buf
}

// ── Native OS sandbox wiring ────────────────────────────────────

/// Apply the opt-in native OS sandbox (`duduclaw-sandbox`) to a to-be-spawned
/// agent CLI command, in place, just before spawn.
///
/// No-op unless the agent's `[capabilities] native_sandbox = true`. When
/// enabled, the child is confined by a native OS primitive (macOS Seatbelt /
/// Linux Landlock) scoped by [`sandbox_level_for`] + the agent directory, on top
/// of the CLI-flag sandbox. **Fail-closed** (I5): if confinement is required but
/// cannot be applied (refused, unsupported OS/kernel, missing agent dir, or an
/// error), this returns `Err` and the caller MUST NOT spawn.
///
/// `agent_dir` is the workspace root that becomes the writable scope for
/// `WorkspaceWrite`. `runtime_name` is used only for structured logging.
pub(crate) fn apply_native_sandbox(
    cmd: &mut tokio::process::Command,
    caps: Option<&duduclaw_core::types::CapabilitiesConfig>,
    agent_dir: Option<&Path>,
    runtime_name: &str,
) -> Result<(), String> {
    use duduclaw_core::types::sandbox_level_for;
    use duduclaw_sandbox::{Confinement, SandboxSpec, platform_sandbox};

    let want = caps.map(|c| c.native_sandbox).unwrap_or(false);
    if !want {
        return Ok(());
    }

    // A required sandbox with no workspace root cannot be scoped → fail-closed.
    let Some(dir) = agent_dir else {
        return Err(format!(
            "native_sandbox required for runtime '{runtime_name}' but no agent directory is set — refusing to spawn"
        ));
    };

    let level = sandbox_level_for(caps);
    let spec = SandboxSpec::from_level(level, dir);
    let sandbox = platform_sandbox();

    match sandbox.confine(cmd.as_std_mut(), &spec) {
        Ok(Confinement::Applied) => {
            info!(
                runtime = runtime_name,
                ?level,
                availability = ?sandbox.availability(),
                "native OS sandbox applied to agent subprocess"
            );
            Ok(())
        }
        Ok(Confinement::Skipped) => {
            // FullAccess grant — intentional, no confinement.
            info!(
                runtime = runtime_name,
                "native OS sandbox skipped (FullAccess capability grant)"
            );
            Ok(())
        }
        Ok(Confinement::Refused) => Err(format!(
            "native_sandbox required for runtime '{runtime_name}' but the platform primitive refused (availability: {:?}) — refusing to spawn (fail-closed)",
            sandbox.availability()
        )),
        Err(e) => Err(format!(
            "native_sandbox required for runtime '{runtime_name}' but confinement failed: {e} — refusing to spawn (fail-closed)"
        )),
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::CapabilitiesConfig;

    /// Test helper: a text-less `NativeToolEvent` (pre-R1 shape) — most
    /// existing tests only care about `tool_name`/`success`.
    fn native(tool_name: &str, success: bool) -> NativeToolEvent {
        NativeToolEvent {
            tool_name: tool_name.to_string(),
            success,
            result_text: None,
            input_text: None,
        }
    }

    #[test]
    fn native_sandbox_noop_when_disabled() {
        // Default caps → native_sandbox = false → helper is a no-op success even
        // with a valid agent dir.
        let caps = CapabilitiesConfig::default();
        let mut cmd = tokio::process::Command::new("true");
        assert!(
            apply_native_sandbox(&mut cmd, Some(&caps), Some(Path::new("/tmp")), "test").is_ok()
        );
    }

    #[test]
    fn native_sandbox_noop_when_caps_absent() {
        let mut cmd = tokio::process::Command::new("true");
        assert!(apply_native_sandbox(&mut cmd, None, Some(Path::new("/tmp")), "test").is_ok());
    }

    #[test]
    fn native_sandbox_fail_closed_without_agent_dir() {
        // Required sandbox but no workspace root to scope → refuse (fail-closed).
        let caps = CapabilitiesConfig {
            native_sandbox: true,
            ..Default::default()
        };
        let mut cmd = tokio::process::Command::new("true");
        assert!(apply_native_sandbox(&mut cmd, Some(&caps), None, "test").is_err());
    }

    #[test]
    fn native_sandbox_skipped_on_full_access() {
        // computer_use grants FullAccess → SandboxSpec is unconfined → confine
        // returns Skipped → helper returns Ok (intentional escape hatch).
        let caps = CapabilitiesConfig {
            native_sandbox: true,
            computer_use: true,
            ..Default::default()
        };
        let mut cmd = tokio::process::Command::new("true");
        assert!(
            apply_native_sandbox(&mut cmd, Some(&caps), Some(Path::new("/tmp")), "test").is_ok()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_sandbox_applies_on_macos() {
        // On this macOS box the Seatbelt primitive is Enforcing → a required
        // sandbox with a workspace root confines successfully.
        let caps = CapabilitiesConfig {
            native_sandbox: true,
            ..Default::default()
        };
        let mut cmd = tokio::process::Command::new("true");
        assert!(
            apply_native_sandbox(&mut cmd, Some(&caps), Some(Path::new("/tmp")), "test").is_ok()
        );
    }

    #[tokio::test]
    async fn native_tool_collector_noop_without_scope() {
        // No `NATIVE_TOOL_COLLECTOR::scope` entered — must not panic, must
        // silently drop the events (design R5: absent scope is a no-op).
        extend_native_tool_events(vec![native("Bash", true)]);
    }

    #[tokio::test]
    async fn native_tool_collector_accumulates_when_scoped() {
        let collector = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        NATIVE_TOOL_COLLECTOR
            .scope(collector.clone(), async {
                extend_native_tool_events(vec![native("Bash", true), native("Read", false)]);
                extend_native_tool_events(vec![native("Write", true)]);
            })
            .await;
        let events = collector.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].tool_name, "Bash");
        assert!(events[0].success);
        assert_eq!(events[1].tool_name, "Read");
        assert!(!events[1].success);
    }

    #[tokio::test]
    async fn native_tool_collector_empty_batch_is_noop() {
        // Empty batches must not even take the lock (dead code would panic
        // if this somehow poisoned an unscoped run) — asserts no panic and
        // no scope required for a zero-event flush.
        extend_native_tool_events(vec![]);
    }

    // ── T10: native_tool_events_from_chunks ─────────────────────────────

    fn tool_use(name: &str) -> RuntimeChunk {
        RuntimeChunk::ToolUse {
            name: name.to_string(),
            input: serde_json::json!({}),
        }
    }
    fn tool_result(is_error: bool) -> RuntimeChunk {
        RuntimeChunk::ToolResult {
            output: String::new(),
            is_error,
        }
    }

    #[test]
    fn native_tool_events_from_chunks_pairs_success() {
        let chunks = vec![tool_use("shell"), tool_result(false)];
        let events = native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "shell");
        assert!(events[0].success);
    }

    #[test]
    fn native_tool_events_from_chunks_pairs_failure() {
        let chunks = vec![tool_use("shell"), tool_result(true)];
        let events = native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert!(!events[0].success);
    }

    #[test]
    fn native_tool_events_from_chunks_unpaired_tool_use_is_provisional_success() {
        let chunks = vec![tool_use("shell")];
        let events = native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert!(events[0].success);
    }

    #[test]
    fn native_tool_events_from_chunks_multiple_pairs_in_order() {
        let chunks = vec![
            tool_use("tasks_create"),
            tool_result(false),
            tool_use("shell"),
            tool_result(true),
        ];
        let events = native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].tool_name, "tasks_create");
        assert!(events[0].success);
        assert_eq!(events[1].tool_name, "shell");
        assert!(!events[1].success);
    }

    #[test]
    fn native_tool_events_from_chunks_ignores_text_and_done() {
        let chunks = vec![
            RuntimeChunk::Text("hello".to_string()),
            tool_use("shell"),
            tool_result(false),
            RuntimeChunk::Done(RuntimeResponse {
                content: "done".to_string(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                model_used: "m".to_string(),
                runtime_name: "codex".to_string(),
            }),
        ];
        let events = native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn native_tool_events_from_chunks_empty_is_empty() {
        assert!(native_tool_events_from_chunks(&[]).is_empty());
    }

    // ── R1: native_tool_events_from_chunks carries masked text ──────────

    #[test]
    fn native_tool_events_from_chunks_captures_result_and_input_text() {
        let chunks = vec![
            RuntimeChunk::ToolUse {
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "cat report.md"}),
            },
            RuntimeChunk::ToolResult {
                output: "quarterly revenue: 1.2M".to_string(),
                is_error: false,
            },
        ];
        let events = native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].result_text.as_deref(),
            Some("quarterly revenue: 1.2M")
        );
        assert!(
            events[0]
                .input_text
                .as_deref()
                .unwrap()
                .contains("cat report.md")
        );
    }

    #[test]
    fn native_tool_events_from_chunks_masks_secrets_in_result_text() {
        let chunks = vec![
            RuntimeChunk::ToolUse {
                name: "Bash".to_string(),
                input: serde_json::Value::Null,
            },
            RuntimeChunk::ToolResult {
                output: "token: sk-ant-api03-verysecretvalue1234567890".to_string(),
                is_error: false,
            },
        ];
        let events = native_tool_events_from_chunks(&chunks);
        let result_text = events[0].result_text.as_deref().unwrap();
        assert!(
            !result_text.contains("sk-ant-api03-verysecretvalue1234567890"),
            "secret leaked into NativeToolEvent.result_text: {result_text}"
        );
    }

    #[test]
    fn native_tool_events_from_chunks_null_input_and_empty_output_stay_none() {
        // The pre-R1 placeholder shape (Value::Null / empty String) — every
        // producer that hasn't been upgraded to capture text yet must still
        // yield `None`, never a fabricated empty-string placeholder.
        let chunks = vec![
            RuntimeChunk::ToolUse {
                name: "Bash".to_string(),
                input: serde_json::Value::Null,
            },
            RuntimeChunk::ToolResult {
                output: String::new(),
                is_error: false,
            },
        ];
        let events = native_tool_events_from_chunks(&chunks);
        assert!(events[0].result_text.is_none());
        assert!(events[0].input_text.is_none());
    }

    #[test]
    fn native_tool_events_from_chunks_unpaired_tool_use_has_no_result_text() {
        let chunks = vec![RuntimeChunk::ToolUse {
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        }];
        let events = native_tool_events_from_chunks(&chunks);
        assert!(events[0].success);
        assert!(events[0].result_text.is_none());
        assert!(events[0].input_text.is_some());
    }

    #[test]
    fn test_runtime_type_default() {
        assert_eq!(RuntimeType::default(), RuntimeType::Claude);
    }

    // ── WP-B: catalog ↔ registry coverage ───────────────────────────────

    /// Every catalog CLI must be *reachable*: either a bespoke module claims
    /// it, or the generic print-mode driver can build a command line for it.
    /// A runtime that is in neither set would be configurable and detectable
    /// but never executable.
    #[test]
    fn every_catalog_cli_is_either_bespoke_or_generic_drivable() {
        for spec in duduclaw_core::runtime_catalog::cli_specs() {
            if BESPOKE_RUNTIME_IDS.contains(&spec.id) {
                continue;
            }
            let args = generic_cli::build_args(spec, "PROMPT", "some-model");
            assert!(
                !args.is_empty(),
                "`{}` is not bespoke and has no generic argv",
                spec.id
            );
            assert!(
                spec.headless.prompt_via_stdin() || args.iter().any(|a| a == "PROMPT"),
                "`{}` neither takes the prompt as an argument nor on stdin",
                spec.id
            );
            assert!(
                RuntimeType::from_id(spec.id).is_some(),
                "`{}` has no RuntimeType, so the registry could never key it",
                spec.id
            );
        }
    }

    /// The bespoke list must name only runtimes that actually have a module
    /// here — a stale entry would silently remove that runtime from the
    /// registry (the generic loop skips it, and no bespoke module registers it).
    #[test]
    fn bespoke_ids_match_the_modules_that_exist() {
        let mut expected: Vec<&str> = vec!["claude", "codex", "gemini", "antigravity", "grok"];
        expected.sort_unstable();
        let mut got: Vec<&str> = BESPOKE_RUNTIME_IDS.to_vec();
        got.sort_unstable();
        assert_eq!(got, expected);
        for id in BESPOKE_RUNTIME_IDS {
            assert!(
                duduclaw_core::runtime_catalog::spec_for(id).is_some(),
                "bespoke id `{id}` is not in the catalog"
            );
        }
    }

    #[test]
    fn test_runtime_type_serde() {
        let json = serde_json::to_string(&RuntimeType::Codex).unwrap();
        assert_eq!(json, r#""codex""#);

        let parsed: RuntimeType = serde_json::from_str::<RuntimeType>(r#""gemini""#).unwrap();
        assert_eq!(parsed, RuntimeType::Gemini);
    }

    #[test]
    fn load_capabilities_none_when_agent_toml_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_agent_capabilities(dir.path()).is_none());
    }

    #[test]
    fn load_capabilities_defaults_when_section_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        let caps = load_agent_capabilities(dir.path()).expect("file exists");
        assert!(!caps.computer_use, "deny-by-default");
        assert!(!caps.browser_via_bash);
        assert!(caps.allowed_tools.is_empty());
    }

    #[test]
    fn load_capabilities_parses_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[capabilities]\ncomputer_use = true\nallowed_tools = [\"Read\"]\n",
        )
        .unwrap();
        let caps = load_agent_capabilities(dir.path()).unwrap();
        assert!(caps.computer_use);
        assert_eq!(caps.allowed_tools, vec!["Read".to_string()]);
    }

    #[test]
    fn load_capabilities_fails_closed_on_malformed_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            // computer_use has the wrong type — the section must fail closed
            // to the deny-by-default config, not silently grant anything.
            "[capabilities]\ncomputer_use = \"yes please\"\n",
        )
        .unwrap();
        let caps = load_agent_capabilities(dir.path()).unwrap();
        assert!(!caps.computer_use, "malformed section must fail closed");
    }

    #[test]
    fn duduclaw_mcp_server_json_carries_agent_id() {
        // resolve_duduclaw_bin falls back to current_exe (absolute under
        // cargo test); when unresolvable this correctly yields None.
        let Some(def) = duduclaw_mcp_server_json("agnes") else {
            return;
        };
        assert_eq!(def["args"][0], "mcp-server");
        assert_eq!(def["env"][duduclaw_core::ENV_AGENT_ID], "agnes");
        assert!(std::path::Path::new(def["command"].as_str().unwrap()).is_absolute());
    }

    #[test]
    fn duduclaw_mcp_server_uses_explicit_eval_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("eval-home");
        std::fs::create_dir_all(&home).unwrap();
        let key = duduclaw_core::ensure_identity_key(&home).unwrap();
        let def = duduclaw_mcp_server_json_for_home("eph-test-r1-planner-123456", &home)
            .expect("test binary resolves to an absolute path");
        assert_eq!(def["env"]["DUDUCLAW_HOME"], home.to_string_lossy().as_ref());
        let token = def["env"][duduclaw_core::ENV_AGENT_TOKEN]
            .as_str()
            .expect("identity token");
        assert!(duduclaw_core::verify_identity_token(
            &key,
            "eph-test-r1-planner-123456",
            token
        ));
    }

    // ── Spawn overrides + truthful runtime attribution ──────────────────

    #[tokio::test]
    async fn spawn_work_dir_override_is_none_without_a_scope() {
        assert!(spawn_work_dir_override().is_none());
    }

    #[tokio::test]
    async fn spawn_work_dir_override_is_visible_inside_the_scope() {
        let dir = PathBuf::from("/tmp/duduclaw-test/agents/agnes");
        SPAWN_OVERRIDE
            .scope(
                SpawnOverride {
                    work_dir: Some(dir.clone()),
                },
                async {
                    assert_eq!(spawn_work_dir_override(), Some(dir.clone()));
                },
            )
            .await;
        // And it does not leak out.
        assert!(spawn_work_dir_override().is_none());
    }

    /// Regression (2026-09-28 review): the override was consumed without
    /// checking it exists, so a stale/mistyped `work_dir` produced a spawn in a
    /// nonexistent cwd instead of falling back to the agent directory the way
    /// `claude_runner.rs`'s dispatch path already did.
    #[tokio::test]
    async fn resolve_spawn_work_dir_falls_back_when_the_override_is_not_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // A path that does not exist at all.
        let missing = tmp.path().join("nope/never/created");
        let got = SPAWN_OVERRIDE
            .scope(
                SpawnOverride {
                    work_dir: Some(missing),
                },
                async { resolve_spawn_work_dir(Some(agent_dir.as_path()), "agnes") },
            )
            .await;
        assert_eq!(got.as_deref(), Some(agent_dir.as_path()));

        // A path that exists but is a FILE — `is_dir()`, not `exists()`.
        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        let got = SPAWN_OVERRIDE
            .scope(
                SpawnOverride {
                    work_dir: Some(file),
                },
                async { resolve_spawn_work_dir(Some(agent_dir.as_path()), "agnes") },
            )
            .await;
        assert_eq!(got.as_deref(), Some(agent_dir.as_path()));
    }

    /// Regression (2026-09-28 review): only `codex.rs` ever read
    /// `SPAWN_OVERRIDE.work_dir`. `gemini.rs`, `antigravity.rs` and `grok.rs`
    /// spawned in `context.agent_dir` unconditionally, so a team role member on
    /// any of those three wrote into its own throwaway scaffold and the
    /// immediate GC at `finish_role_member` deleted the work — design §4.3 E3,
    /// re-opened on three of the four CLI runtimes.
    ///
    /// The behavioral half of this lock is the `resolve_spawn_work_dir` tests
    /// above; this half checks the *wiring*, because the wiring lives inside
    /// `execute()` where asserting it would mean spawning a real CLI. A
    /// compile-time `include_str!` keeps it deterministic and I/O-free.
    #[test]
    fn every_cli_runtime_resolves_its_cwd_through_the_shared_override_helper() {
        const CALL: &str =
            "super::resolve_spawn_work_dir(context.agent_dir.as_deref(), &context.agent_id)";
        for (name, src) in [
            ("codex.rs", include_str!("codex.rs")),
            ("gemini.rs", include_str!("gemini.rs")),
            ("antigravity.rs", include_str!("antigravity.rs")),
            ("grok.rs", include_str!("grok.rs")),
        ] {
            assert!(
                src.contains(CALL),
                "{name} no longer resolves its working root through \
                 resolve_spawn_work_dir — a role member's files will be GC'd"
            );
        }
    }

    #[tokio::test]
    async fn resolve_spawn_work_dir_honours_a_real_override_and_is_inert_without_one() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agent");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();

        let got = SPAWN_OVERRIDE
            .scope(
                SpawnOverride {
                    work_dir: Some(workspace.clone()),
                },
                async { resolve_spawn_work_dir(Some(agent_dir.as_path()), "agnes") },
            )
            .await;
        assert_eq!(got.as_deref(), Some(workspace.as_path()));

        // No scope ⇒ every runtime resolves exactly what it resolved before
        // the override existed.
        assert_eq!(
            resolve_spawn_work_dir(Some(agent_dir.as_path()), "agnes").as_deref(),
            Some(agent_dir.as_path())
        );
        assert!(resolve_spawn_work_dir(None, "agnes").is_none());
    }

    #[tokio::test]
    async fn record_runtime_outcome_is_a_noop_without_a_scope() {
        // Must not panic — attribution telemetry never fails a call.
        record_runtime_outcome(RuntimeType::Claude, "claude-opus-4-6");
    }

    /// Regression (2026-09-28 review, `review_team.md` §3 "授權／證據"):
    /// `record_role_usage` was last-writer-wins, so a member stage that ran an
    /// openai-compat tool loop or a multi-leg `failover.rs` chain recorded only
    /// its LAST leg in `role_turns.jsonl` while `cost_telemetry` recorded every
    /// one — two ledgers disagreeing about the same stage, against
    /// `RoleTurnRow::usage`'s own "one member stage" contract.
    ///
    /// NOTE: this test previously asserted the last-writer-wins behaviour
    /// (`role_usage_is_scoped_to_one_member_and_last_answer_wins`). It encoded
    /// the defect itself, so it is rewritten rather than kept.
    #[tokio::test]
    async fn role_usage_accumulates_every_leg_of_one_member_stage() {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(
            crate::role_turns::RoleTurnUsage::default(),
        ));
        ROLE_USAGE
            .scope(slot.clone(), async {
                // Leg 1: the primary attempt reports no cache read at all.
                record_role_usage(crate::role_turns::RoleTurnUsage {
                    usage_input_tokens: Some(10),
                    usage_output_tokens: Some(2),
                    ..Default::default()
                });
                // Leg 2: the fallback answers, and its tokens are ALSO spent.
                record_role_usage(crate::role_turns::RoleTurnUsage {
                    usage_input_tokens: Some(20),
                    usage_output_tokens: Some(3),
                    usage_cache_read_tokens: Some(15),
                    ..Default::default()
                });
                // An empty record is not a leg: a runtime that reported no
                // usage must not inflate the leg count.
                record_role_usage(crate::role_turns::RoleTurnUsage::default());
            })
            .await;
        let got = *slot.lock().unwrap();
        assert_eq!(got.usage_input_tokens, Some(30));
        assert_eq!(got.usage_output_tokens, Some(5));
        // A dimension only one leg measured keeps that leg's number — never
        // `Some(0)` for the leg that reported nothing.
        assert_eq!(got.usage_cache_read_tokens, Some(15));
        // Two legs reported; the empty third did not.
        assert_eq!(got.usage_legs, Some(2));

        // Outside the scope it is still a silent no-op.
        record_role_usage(crate::role_turns::RoleTurnUsage {
            usage_input_tokens: Some(999),
            ..Default::default()
        });
        assert_eq!(slot.lock().unwrap().usage_input_tokens, Some(30));
    }

    #[tokio::test]
    async fn record_runtime_outcome_keeps_the_last_writer() {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        RUNTIME_OUTCOME
            .scope(slot.clone(), async {
                // Primary leg attempts codex…
                record_runtime_outcome(RuntimeType::Codex, "gpt-5.6-sol");
                // …fails, fallback leg answers as Claude on a substituted model.
                record_runtime_outcome(RuntimeType::Claude, "claude-opus-4-6");
            })
            .await;
        let got = slot
            .lock()
            .unwrap()
            .clone()
            .expect("an outcome was recorded");
        assert_eq!(got.runtime, RuntimeType::Claude);
        assert_eq!(got.model, "claude-opus-4-6");
    }
}
