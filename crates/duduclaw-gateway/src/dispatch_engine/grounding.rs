use super::*;

// ── B3: GroundedSpec production pre-check (arXiv:2606.22737) ──────────────
//
// Lifts the eval-only trace-grounding assertion (WP4 GroundEval,
// `duduclaw-cli/src/eval/assertions.rs`, `[[expect.grounded]]`) into the goal
// loop's own zero-LLM acceptance chain — a claim provably unsupported by tool
// evidence is rejected without spending a judge LLM call, exactly like the
// WP2.4 `outcome_spec` deterministic check it runs alongside. The shared
// overlap primitive lives in `duduclaw_core::grounding` (moved there in the
// same change so both crates use byte-identical matching logic).
//
// ## Evidence source is already multi-runtime
//
// Evidence is read from the same `tool_calls.jsonl` window `<tool_activity>`
// already reads ([`read_tool_activity_records`]). That trail is written by
// the MCP dispatch layer in `duduclaw-cli/src/mcp.rs`
// (`append_tool_call_with_input`), which sits BELOW the runtime abstraction:
// every runtime that calls a DuDuClaw MCP tool — Claude, Codex, Gemini,
// Antigravity, or an openai-compat backend — produces an identical
// `tool_calls.jsonl` row regardless of which CLI drove the call. No
// per-runtime branching is needed here; a runtime is transparently exactly
// as "seen" as its MCP tool usage.
//
// ## Current behavior — read before trusting this gate in production
//
// B3b activated the evidence source: `tool_calls.jsonl` rows now capture the
// tool's masked **input**, a `success` bool, AND (for most state-changing
// tools) the tool's masked **output** text (`append_tool_call_with_input`,
// `duduclaw-cli/src/mcp.rs`). This gate is therefore live, not inert — a
// `review` task whose claim is unsupported by any captured tool result CAN
// be rejected here, before the judge is ever invoked.
//
// R1 (2026-08, `wiki/reports/memory-quality/2026-08/wp-a10-live-test-2026-08-06.md`
// §6) extended the SAME evidence merge to the WP-A4/A5/T10 native-tool
// collector: `NativeToolEvent` now carries masked `result_text`/`input_text`
// when the originating runtime's own event stream captured them (Claude
// `tool_result` content blocks, codex `aggregated_output`/`mcp_tool_call
// result`, gemini `tool_result.output`, the openai-compat direct-API tool
// loop). Before R1 a native event carried only `tool_name`/`success`, so an
// honest task done entirely with native tools (Read/Write/Bash — no MCP
// call at all) could never reach `Grounded`, only perpetually `Degraded`.
// Native evidence is folded into the SAME `ToolEvidence` list the MCP
// records build and passed through the SAME `check_grounded` call — there is
// no longer a structural reason native evidence cannot ground a claim.
//
// Three cases still fall through to the judge unchanged (never reject):
// - **Read-only tools produce no audit row at all**
//   (`duduclaw_security::audit::is_readonly_tool_name` — `tasks_list`,
//   `memory_search`'s sibling `*_get`/`*_status` tools, etc. never even
//   reach `is_state_changing`). A task whose claim rests entirely on a
//   lookup, not a mutation, has NO evidence in the window → `Skip`
//   ("no tool_use in claim→review window").
// - **Self-echo tools never capture `result_text`** (Fix-2 C1a,
//   `duduclaw_core::grounding::SELF_ECHO_TOOL_NAMES` — `tasks_complete`,
//   `tasks_update`, `activity_post`, ...): their MCP response is
//   substantially the caller's own input echoed back, so capturing it as
//   evidence would let a claim "ground" against its own words. These
//   degrade to `ResultTextMissing`/skip, exactly like an ordinary
//   observability gap.
// - **Every recorded call errored** → `Degraded` ("no successful tool call
//   in window") — an execution problem, not a fabrication the judge's
//   `correctness` aspect needs a second zero-LLM pass on.
//
// Fix-2 C1b adds a second, orthogonal safeguard even on a genuinely captured
// `result_text`: a span shared with the final claim is disqualified if that
// same span also appears in the call's OWN input
// (`shares_contiguous_run_excluding_echo`) — so a tool that isn't fully
// self-echoing (e.g. mixes a genuine store-assigned id into a response that
// also restates part of the request) still can't be "grounded" purely on
// the restated part.
//
// A separate, orthogonal limitation remains: this is a literal-overlap check
// (same as the eval version). An agent that legitimately paraphrases or
// translates a tool result (e.g. summarizing an English API response in
// 繁體中文) can fail it even though the claim is well-founded — the reject
// feedback explicitly asks the agent to quote the tool's key original
// wording rather than paraphrase, to steer around this false-positive mode.

/// Conservative default overlap threshold for the production pre-check.
/// Deliberately LOWER than the offline eval default (12 chars,
/// `default_min_overlap_chars` in `duduclaw-cli/src/eval/case.rs`): this
/// gate runs unattended on every goal-mode review in production, where a
/// false-positive reject burns a whole revision round. The eval suite is
/// author-curated (a human picks `min_overlap_chars` per assertion); this
/// gate has no such per-task tuning, so it defaults to catching only
/// blatantly unsupported claims. A false negative here is not a silent
/// miss — the MAV judge's `correctness`/`completeness` aspects remain a
/// second, LLM-backed lens on the same claim.
pub(super) const DEFAULT_GROUNDING_MIN_OVERLAP_CHARS: usize = 6;

/// Tuning for the B3 grounding pre-check. Read from `config.toml [dispatch]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GroundingPrecheckConfig {
    /// Default ON (per task brief: "預設開啟但保守"). Set
    /// `[dispatch] grounding_precheck_enabled = false` to disable.
    pub(super) enabled: bool,
    /// `[dispatch] grounding_min_overlap_chars`, chars (CJK-safe char count,
    /// not bytes). Must be >= 1; a non-positive/malformed value falls back
    /// to the default rather than degrading to "everything overlaps".
    pub(super) min_overlap_chars: usize,
}

impl Default for GroundingPrecheckConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_overlap_chars: DEFAULT_GROUNDING_MIN_OVERLAP_CHARS,
        }
    }
}

impl GroundingPrecheckConfig {
    /// Isolated `toml::Table` parse — mirrors [`dispatch_engine_enabled`]'s
    /// read pattern so an unrelated/malformed `config.toml` section can
    /// never break this. Absent file/section/field ⇒ the conservative
    /// default (on, low threshold).
    pub(super) fn from_home(home_dir: &std::path::Path) -> Self {
        let default = Self::default();
        let config_path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&config_path) else {
            return default;
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return default;
        };
        let Some(section) = table.get("dispatch").and_then(|v| v.as_table()) else {
            return default;
        };
        let enabled = section
            .get("grounding_precheck_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(default.enabled);
        let min_overlap_chars = section
            .get("grounding_min_overlap_chars")
            .and_then(|v| v.as_integer())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| n > 0)
            .unwrap_or(default.min_overlap_chars);
        Self {
            enabled,
            min_overlap_chars,
        }
    }
}

/// Outcome of the B3 grounding pre-check — more granular than a bool so the
/// caller can log a degrade distinctly from a genuine pass, and so a
/// disabled/pure-text task is visibly a `Skip`, never confused with a
/// `Grounded` pass that had nothing to disprove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GroundingPrecheck {
    /// The check does not apply: disabled, or no `tool_use` evidence exists
    /// in the claim→review window at all (a pure-text task, or a task whose
    /// tool calls never reached the MCP server). Proceed to the judge
    /// unchanged.
    Skip { reason: &'static str },
    /// Evidence exists but is not usable for grounding (no successful call,
    /// or no call captured `result_text` — a read-only-tool observability
    /// gap, or a Fix-2 C1a self-echo tool that deliberately never captures
    /// one; see the module doc). Proceed to the judge unchanged; this is a
    /// fail-open quality-gate degrade, never a fail-closed reject.
    Degraded { reason: &'static str },
    /// At least one successful tool call's result shares the required
    /// contiguous run with the claimed result. Proceed to the judge
    /// unchanged. Carries the grounding tool's name so the caller can (Fix-2
    /// C1c) decline to log a `confirmed_facts` entry for evidence sourced
    /// from a self-echo tool — belt-and-suspenders alongside C1a/C1b, which
    /// already keep such evidence out of `check_grounded` in the first
    /// place.
    Grounded { tool_name: String },
    /// Evidence with result text exists and none of it backs the claim —
    /// reject before the judge is ever invoked.
    Reject { feedback: String },
}

/// Run the B3 grounding pre-check for one task's claim→review window.
/// `result` is the agent's self-reported final answer (`task.result_summary`,
/// same text the judge sees); `records` is the window's MCP tool-call
/// evidence ([`read_tool_activity_records`]).
///
/// `native` is the WP-A4/A5/T10 native-tool collector's evidence for this
/// same round (BUG-2 fix, WP-A10 §6 復驗; R1 text capture, 2026-08). Since
/// R1, a native event MAY carry `result_text`/`input_text` (captured
/// straight from the originating runtime's own event stream — see the
/// module doc) — when it does, it is merged into the SAME
/// [`duduclaw_core::grounding::ToolEvidence`] list the MCP records build and
/// can reach every outcome `check_grounded` produces, including `Grounded`.
/// A native event with no captured text (the pre-R1 shape, and still the
/// common case for producers not yet upgraded) behaves exactly as before:
/// it can only ever nudge the `Skip`/`Degraded` reason string, never
/// upgrade the outcome on its own.
pub(super) fn grounding_precheck(
    result: &str,
    records: &[ToolActivityRecord],
    native: &[NativeToolEvent],
    config: GroundingPrecheckConfig,
) -> GroundingPrecheck {
    if !config.enabled {
        return GroundingPrecheck::Skip { reason: "disabled" };
    }

    // Only a successful, non-self-echo native event counts as a real
    // "the agent used a tool" signal — mirrors the MCP-side self-echo
    // exclusion (Fix-2 C1a) and keeps a failed/no-op native call from
    // upgrading the reason string.
    let has_native_signal = native
        .iter()
        .any(|e| e.success && !duduclaw_core::grounding::is_self_echo_tool(&e.tool_name));

    if records.is_empty() && !has_native_signal {
        // No MCP tool_use evidence in the window, and no successful
        // non-self-echo native tool use either. Never reject a task for not
        // using tools it never claimed to need (requirement: "純文字任務
        // (無 tool_use)不套用").
        return GroundingPrecheck::Skip {
            reason: "no tool_use in claim→review window",
        };
    }

    let mut evidence: Vec<duduclaw_core::grounding::ToolEvidence> = records
        .iter()
        .map(|r| duduclaw_core::grounding::ToolEvidence {
            tool_name: r.tool_name.clone(),
            result_text: r.result_text.clone(),
            // Fix-2 C1b: subtract self-echoed spans (this call's own input)
            // from what counts as grounding evidence.
            input_text: r.input_text.clone(),
            is_error: !r.success,
        })
        .collect();
    // R1: native evidence (Read/Write/Bash, ...) merges in as first-class
    // grounding evidence — a `NativeToolEvent` with `result_text: None`
    // behaves identically to an MCP `ToolActivityRecord` with `result_text:
    // None` (ResultTextMissing / NoEvidence, never Grounded on its own).
    evidence.extend(
        native
            .iter()
            .map(|e| duduclaw_core::grounding::ToolEvidence {
                tool_name: e.tool_name.clone(),
                result_text: e.result_text.clone(),
                input_text: e.input_text.clone(),
                is_error: !e.success,
            }),
    );
    let evidence_count = evidence.len();

    match duduclaw_core::grounding::check_grounded(
        result,
        &evidence,
        None,
        config.min_overlap_chars,
    ) {
        // Every recorded call errored — nothing successful to ground
        // against. Degrade, not reject: an all-error tool window is an
        // execution problem the judge's `correctness` aspect already
        // scrutinizes; this gate's job is catching fabricated *success*
        // claims, not re-deriving tool failure.
        duduclaw_core::grounding::GroundingOutcome::NoEvidence => GroundingPrecheck::Degraded {
            reason: if has_native_signal {
                "no successful MCP tool call in window (native tool evidence present but also lacks captured result_text)"
            } else {
                "no successful tool call in window"
            },
        },
        // A read-only-tool observability gap, a Fix-2 C1a self-echo tool
        // that never captures output text, OR (R1) native evidence exists
        // but none of it carried result_text either. Fail-open either way.
        duduclaw_core::grounding::GroundingOutcome::ResultTextMissing => {
            GroundingPrecheck::Degraded {
                reason: if records.is_empty() {
                    // Native-only window (BUG-2's original case): the old,
                    // more specific reason string stays intact.
                    "native tool evidence present but lacks captured result_text for grounding"
                } else if has_native_signal {
                    "tool evidence lacks captured result_text (native tool evidence also present, same limitation)"
                } else {
                    "tool evidence lacks captured result_text"
                },
            }
        }
        duduclaw_core::grounding::GroundingOutcome::Grounded { tool_name } => {
            GroundingPrecheck::Grounded { tool_name }
        }
        duduclaw_core::grounding::GroundingOutcome::NotGrounded => GroundingPrecheck::Reject {
            feedback: format!(
                "零成本 grounding 前置檢查未通過（GroundEval，未進判官）：本輪任務窗口內有 {} \
                 筆成功的工具呼叫紀錄，但回覆內容與任何一筆工具結果都沒有共同的 {} 字元以上連續片段，\
                 判定為缺乏證據支持的宣稱。請在最終回覆中直接引用工具實際回傳的關鍵原文再重新提交\
                 （已知限制：若你對工具結果做了改寫、摘要或中英轉換，字面比對可能誤判——請盡量保留\
                 關鍵原文用詞，例如數字、代號、專有名詞）。",
                evidence_count, config.min_overlap_chars
            ),
        },
    }
}

