//! Fault attribution — who actually caused this round to fail?
//!
//! The self-evolution loop (AEE playbook, `rule_lifecycle` credit
//! assignment, the `MistakeNotebook` → F2b reflexion consolidation) learns
//! from *failed rounds*. Every one of those consumers historically assumed
//! that a failed round means **the model got it wrong** — the only fault
//! side the pipeline could express.
//!
//! Two results say that assumption is the dominant error source in agentic
//! self-improvement:
//!
//! - **The Misattribution Gap** (arXiv:2605.22842): an attribution system
//!   blamed the model in 64/64 observed failures while the real fault lay
//!   with the grader, the environment, or the harness. A learning loop fed
//!   by such labels does not just waste rounds — it *manufactures* rules
//!   that "fix" behavior which was never broken.
//! - **Model or Harness?** (arXiv:2607.28802): the remedy is a **closed
//!   enum of fault sides decided at write time**, not a post-hoc narrative.
//!   Whatever the writer could not establish must be recorded as
//!   `Unknown`, never silently folded into "the model did it".
//!
//! This module is that write-time decision. It is deliberately:
//!
//! - **deterministic and zero-LLM** — a judge deciding who to blame is the
//!   very self-certification the papers warn against;
//! - **a closed enum** ([`FaultSide`]) with an explicit `Unknown`;
//! - **fail-open toward *not* learning** — when the observation is blind
//!   (`ObservationFidelity::None`) we return `Unknown`, and callers treat
//!   anything other than [`FaultSide::Model`] as *no evidence* rather than
//!   as evidence of success. Excluding a round costs one learning sample;
//!   mislabeling one plants a false rule that outlives the round.
//!
//! Scope: exclusion rules R1–R3 (plus the R0 fidelity guard). R4+ (fault
//! sides derived from replayed transcripts, cross-round fault clustering)
//! are deliberately out of scope for this work package.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::channel_reply::FailureReason;
use crate::prediction::task_forward::ObservationFidelity;

// ═══════════════════════════════════════════════════════════════════════
// The closed enum
// ═══════════════════════════════════════════════════════════════════════

/// Which side of the system caused this round's failure.
///
/// Closed by design (arXiv:2607.28802): a fault side that cannot be
/// established is [`FaultSide::Unknown`], never a default blame on the
/// model.
/// `Default` is `Model`, matching the `mistakes.fault_side DEFAULT 'model'`
/// column: a row written before this enum existed WAS recorded under the
/// "the model did it" assumption, and saying so is honest. New writers must
/// always pass an explicitly classified side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultSide {
    /// The model's own output is what went wrong. The ONLY side that feeds
    /// the evolution loop.
    #[default]
    Model,
    /// The scaffolding around the model failed it: a tool the agent tried
    /// to call was blocked by capability policy, or the agent's tool calls
    /// never reached a collector at all while the reply claims it used
    /// tools.
    Harness,
    /// Infrastructure: rate limit, billing exhaustion, timeout, missing
    /// binary, spawn failure, no accounts.
    Environment,
    /// The grader contradicted the deterministic evidence — the zero-LLM
    /// grounding check and the LLM judge disagree, so at least one of them
    /// is wrong and the round carries no clean signal about the model.
    Grader,
    /// Not establishable from the evidence available at write time. NEVER
    /// treat this as `Model`.
    Unknown,
}

impl FaultSide {
    /// Stable snake_case token — the SQL column value, the audit field, and
    /// the telemetry label all use this one spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Harness => "harness",
            Self::Environment => "environment",
            Self::Grader => "grader",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a stored token back. An unrecognized/legacy value reads as
    /// `Model` — matching the `DEFAULT 'model'` column and the pre-WP-C
    /// semantics of every already-written row (they WERE recorded under the
    /// "the model did it" assumption, so surfacing them as such is honest;
    /// inventing `Unknown` for them would retro-actively mute real history).
    pub fn from_token(s: &str) -> Self {
        match s {
            "harness" => Self::Harness,
            "environment" => Self::Environment,
            "grader" => Self::Grader,
            "unknown" => Self::Unknown,
            _ => Self::Model,
        }
    }

    /// Whether a mistake / failed round attributed to this side may feed
    /// the learning loop (playbook credit, F2b consolidation).
    pub fn counts_for_learning(&self) -> bool {
        matches!(self, Self::Model)
    }
}

/// The zero-LLM grounding pre-check's verdict, reduced to the three states
/// fault attribution cares about. `dispatch_engine`'s richer
/// `GroundingPrecheck` (`Skip` / `Degraded` / `Grounded` / `Reject`) maps
/// onto this: `Grounded` → `Pass`, `Reject` → `Fail`, everything else →
/// `Skip` (the check did not produce a usable verdict).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroundingVerdict {
    /// The final answer is backed by a real, non-error tool result.
    Pass,
    /// Evidence exists and contradicts the claim.
    Fail,
    /// The check did not apply, or ran on incomplete data.
    Skip,
}

// ═══════════════════════════════════════════════════════════════════════
// Inputs
// ═══════════════════════════════════════════════════════════════════════

/// Everything the write-time classifier is allowed to look at. All fields
/// are programmatic signals — nothing here is an LLM's narration of what it
/// thinks went wrong.
pub struct FaultInputs<'a> {
    /// How much of this round's tool activity was actually observable.
    /// `None` ⇒ we are blind ⇒ R0 refuses to attribute at all.
    pub fidelity: ObservationFidelity,
    /// The deterministic grounding pre-check's verdict for this round.
    pub grounding: GroundingVerdict,
    /// The acceptance judge's verdict, when a judge ran at all. `None` =
    /// no judge verdict (a zero-LLM phase decided the round, or the judge
    /// itself was unavailable).
    pub judge_passed: Option<bool>,
    /// A deterministic grounded assertion failed for this round (eval
    /// `[[expect.grounded]]`, or the production grounding reject). Paired
    /// with `judge_passed == Some(true)` this is the second half of R1:
    /// the judge blessed an answer the evidence contradicts.
    pub grounded_assertion_failed: bool,
    /// The classified infrastructure failure for this round, when one was
    /// recorded.
    pub failure_reason: Option<&'a FailureReason>,
    /// How many native tool events the runtime collector captured.
    pub native_tool_events: usize,
    /// Heuristic: does the worker's final text claim it used tools? See
    /// [`reply_claims_tool_use`].
    pub reply_claims_tool_use: bool,
    /// A capability gate (denied_tools / allowed_tools / missing grant /
    /// insufficient scope) blocked at least one tool call in this round's
    /// window. See [`capability_blocked_in_window`].
    pub capability_blocked: bool,
}

// ═══════════════════════════════════════════════════════════════════════
// The classifier
// ═══════════════════════════════════════════════════════════════════════

/// Infrastructure failure reasons that make the round the **environment's**
/// fault rather than the model's (R2).
///
/// [`FailureReason::AuthFailed`] and the three `AccountsCoolingDown*` variants
/// were previously excluded, with the note that widening R2 was "a behavior
/// decision for a follow-up". This is that follow-up (review P3): an expired
/// OAuth token and a cooling-down account pool are credentials/infrastructure
/// by any reading, and leaving them out sent those rounds down the R3/Model
/// tail where `counts_for_learning() == true` — so a credential outage fed the
/// `MistakeNotebook` and playbook credit as if the model had written a bad
/// answer. That is precisely the misattribution this module exists to stop,
/// and the omission pointed the wrong way.
fn is_environment_failure(reason: &FailureReason) -> bool {
    matches!(
        reason,
        FailureReason::RateLimited
            | FailureReason::Billing
            | FailureReason::Timeout
            | FailureReason::BinaryMissing
            | FailureReason::SpawnError
            | FailureReason::NoAccounts
            | FailureReason::AuthFailed
            | FailureReason::AccountsCoolingDownLong
            | FailureReason::AccountsCoolingDownShort
            | FailureReason::AccountsCoolingDownUnknown
    )
}

/// Classify who is at fault for this round. First rule that hits wins.
///
/// Rule order (R0 → R3, else `Model`):
/// - **R0** — `fidelity == None`: we observed nothing, so we know nothing.
///   `Unknown`.
/// - **R1** — the grader and the deterministic evidence disagree
///   (`grounding == Pass && judge_passed == Some(false)`, or
///   `judge_passed == Some(true) && grounded_assertion_failed`): `Grader`.
/// - **R2** — an infrastructure failure was recorded: `Environment`.
/// - **R3** — the reply claims tool use but no native tool event was
///   captured **while a native collector was actually running**
///   (`fidelity == Full`), or a capability gate blocked a call: `Harness`.
/// - otherwise `Model`.
pub fn classify_fault(inputs: &FaultInputs) -> FaultSide {
    classify_fault_with_reason(inputs).0
}

/// [`classify_fault`] plus the stable reason token that fired, for the
/// `fault_attributed` audit event. The token names the *rule*, not a
/// narrative — a narrative is exactly what the misattribution paper found
/// to be unreliable.
pub fn classify_fault_with_reason(inputs: &FaultInputs) -> (FaultSide, &'static str) {
    // R0 — blind observation. Must come first: every rule below reasons
    // about evidence, and with `None` fidelity there is none.
    if inputs.fidelity == ObservationFidelity::None {
        return (FaultSide::Unknown, "r0_no_observation_fidelity");
    }

    // R1 — grader vs. deterministic evidence disagreement.
    if inputs.grounding == GroundingVerdict::Pass && inputs.judge_passed == Some(false) {
        return (FaultSide::Grader, "r1_judge_rejected_grounded_answer");
    }
    if inputs.judge_passed == Some(true) && inputs.grounded_assertion_failed {
        return (FaultSide::Grader, "r1_judge_passed_ungrounded_answer");
    }

    // R2 — infrastructure.
    if let Some(reason) = inputs.failure_reason {
        if is_environment_failure(reason) {
            return (FaultSide::Environment, "r2_infrastructure_failure");
        }
    }

    // R3 — harness. Spec order: the missing-tool-event case first, then the
    // capability block.
    //
    // Review P2: `native_tool_events == 0` only *means* something when a
    // native collector actually ran, i.e. under `Fidelity::Full`. Under
    // `McpOnly` (the project's own stated main branch) and on the PTY-pool
    // path (no collector at all) zero native events is structural, not a
    // symptom — and `TOOL_USE_CLAIM_PHRASES` carries CJK entries
    // (`已執行` / `已查詢`) for which `word_contains_ci` degrades to plain
    // substring matching, so an ordinary "任務已執行完畢" reply was enough to
    // relabel a genuine model failure as `Harness` and drop it from learning.
    // The capability-block half below needs no fidelity condition: a denial
    // row IS positive evidence.
    if inputs.fidelity == ObservationFidelity::Full
        && inputs.native_tool_events == 0
        && inputs.reply_claims_tool_use
    {
        return (FaultSide::Harness, "r3_tool_claim_without_native_events");
    }
    if inputs.capability_blocked {
        return (FaultSide::Harness, "r3_capability_blocked");
    }

    (FaultSide::Model, "model")
}

// ═══════════════════════════════════════════════════════════════════════
// Cheap deterministic detectors
// ═══════════════════════════════════════════════════════════════════════

/// Phrases that mark a reply as *claiming* it performed a tool action.
///
/// **This is a heuristic, and is documented as one.** It exists solely as
/// one half of R3's conjunction (`no native tool events` AND `the reply
/// says it used tools`) — on its own it decides nothing, and a miss
/// degrades to `Model`, i.e. to today's behavior. Kept deliberately small:
/// a longer list buys recall at the cost of false "harness" exclusions,
/// which silently starve the learning loop.
///
/// Matched via `duduclaw_core::word_contains_ci`, which requires ASCII
/// word boundaries on both sides for the English phrases (so `"i ran"`
/// cannot match inside `"chai ran"`), and degrades to plain — and, thanks
/// to UTF-8 self-synchronization, char-boundary-safe — substring matching
/// for the CJK phrases, which have no word delimiters.
const TOOL_USE_CLAIM_PHRASES: &[&str] = &[
    // English
    "i ran",
    "i executed",
    "i called the tool",
    "i queried",
    "i fetched",
    // zh-TW
    "我執行了",
    "已執行",
    "已查詢",
    "我查詢了",
    "呼叫了工具",
    "已呼叫",
    // zh-CN (same claim, simplified)
    "我执行了",
    "已查询",
];

/// Whether the worker's final text claims it performed a tool action.
/// Heuristic — see [`TOOL_USE_CLAIM_PHRASES`].
pub fn reply_claims_tool_use(final_text: &str) -> bool {
    TOOL_USE_CLAIM_PHRASES
        .iter()
        .any(|p| duduclaw_core::word_contains_ci(final_text, p))
}

/// `error_class` tokens written by the MCP dispatch gate when a tool call
/// is refused by capability policy (`duduclaw-cli/src/mcp_dispatch.rs` —
/// `audit_dispatch_denial`). A refused call is a harness fault: the agent
/// tried, the platform said no.
pub const CAPABILITY_DENIAL_ERROR_CLASSES: &[&str] = &[
    "denied_tools",
    "allowed_tools",
    "capability_grant_missing",
    "insufficient_scope",
];

/// Whether any tool call by `agent_id` since `since` was refused by a
/// capability gate. Reads the same `tool_calls.jsonl` audit trail every
/// other evidence consumer uses (shared lock, no new sink). A missing /
/// unreadable file reads as "no denial" — fail-open toward today's
/// behavior, never toward a fabricated exclusion.
pub fn capability_blocked_in_window(home_dir: &Path, agent_id: &str, since: &str) -> bool {
    duduclaw_security::audit::read_tool_calls_since(home_dir, agent_id, since)
        .iter()
        .any(|row| {
            row.get("error_class")
                .and_then(|v| v.as_str())
                .is_some_and(|c| CAPABILITY_DENIAL_ERROR_CLASSES.contains(&c))
        })
}

// ═══════════════════════════════════════════════════════════════════════
// Config + telemetry
// ═══════════════════════════════════════════════════════════════════════

/// Default for `config.toml [evolution] fault_attribution`.
pub const DEFAULT_FAULT_ATTRIBUTION_ENABLED: bool = true;

/// Read `config.toml [evolution] fault_attribution` (default **true**).
///
/// When `false`, every call site must behave byte-identically to the
/// pre-WP-C pipeline. Isolated `toml::Table` parse — mirrors
/// `dispatch_engine::GroundingPrecheckConfig::from_home` so an unrelated
/// malformed section can never break this.
pub fn enabled_from_home(home_dir: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return DEFAULT_FAULT_ATTRIBUTION_ENABLED;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return DEFAULT_FAULT_ATTRIBUTION_ENABLED;
    };
    table
        .get("evolution")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("fault_attribution"))
        .and_then(|v| v.as_bool())
        .unwrap_or(DEFAULT_FAULT_ATTRIBUTION_ENABLED)
}

/// Audit + activity-feed the attribution decision, reusing the existing
/// security-audit sink (`security_audit.jsonl` via
/// `security_autopilot::audit_and_emit`) that the judge seam already writes
/// its own degrade events to. No new sink is introduced.
///
/// Only worth writing for a non-`Model` side: `Model` is the status quo and
/// would drown the trail.
pub fn log_fault_attributed(
    home_dir: &Path,
    agent_id: &str,
    task_id: &str,
    round: u32,
    side: FaultSide,
    reason: &str,
) {
    let event = duduclaw_security::audit::AuditEvent::new(
        "fault_attributed",
        agent_id,
        duduclaw_security::audit::Severity::Info,
        serde_json::json!({
            "task_id": task_id,
            "round": round,
            "fault_side": side.as_str(),
            "reason": reason,
        }),
    );
    crate::security_autopilot::audit_and_emit(home_dir, &event);
}

/// The per-round signals a settle site collects *before* it knows the
/// observation fidelity (which only exists after `observe_round`). Owned
/// so it can be handed across an async boundary without a lifetime.
#[derive(Debug, Clone, Default)]
pub struct FaultContext {
    pub grounding: Option<GroundingVerdict>,
    pub judge_passed: Option<bool>,
    pub reply_claims_tool_use: bool,
    pub failure_reason: Option<FailureReason>,
}

impl FaultContext {
    /// Complete the context with the settle-time signals and classify.
    pub fn classify(
        &self,
        fidelity: ObservationFidelity,
        native_tool_events: usize,
        capability_blocked: bool,
    ) -> (FaultSide, &'static str) {
        let grounding = self.grounding.unwrap_or(GroundingVerdict::Skip);
        let inputs = FaultInputs {
            fidelity,
            grounding,
            judge_passed: self.judge_passed,
            // A production grounding reject IS the deterministic assertion
            // failure R1's second clause looks for.
            grounded_assertion_failed: grounding == GroundingVerdict::Fail,
            failure_reason: self.failure_reason.as_ref(),
            native_tool_events,
            reply_claims_tool_use: self.reply_claims_tool_use,
            capability_blocked,
        };
        classify_fault_with_reason(&inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baseline: everything observable, nothing wrong with the scaffolding.
    fn base() -> FaultInputs<'static> {
        FaultInputs {
            fidelity: ObservationFidelity::Full,
            grounding: GroundingVerdict::Skip,
            judge_passed: None,
            grounded_assertion_failed: false,
            failure_reason: None,
            native_tool_events: 1,
            reply_claims_tool_use: false,
            capability_blocked: false,
        }
    }

    // ── table-driven rule coverage ──────────────────────────────────────

    #[test]
    fn classify_fault_rule_table() {
        struct Case {
            name: &'static str,
            mutate: fn(&mut FaultInputs<'static>),
            expect: FaultSide,
            expect_reason: &'static str,
        }

        let cases = &[
            Case {
                name: "R0 blind observation wins over everything",
                mutate: |i| {
                    i.fidelity = ObservationFidelity::None;
                    // Every other rule would also fire — R0 must still win.
                    i.grounding = GroundingVerdict::Pass;
                    i.judge_passed = Some(false);
                    i.capability_blocked = true;
                },
                expect: FaultSide::Unknown,
                expect_reason: "r0_no_observation_fidelity",
            },
            Case {
                name: "R1a judge rejected a grounded answer",
                mutate: |i| {
                    i.grounding = GroundingVerdict::Pass;
                    i.judge_passed = Some(false);
                },
                expect: FaultSide::Grader,
                expect_reason: "r1_judge_rejected_grounded_answer",
            },
            Case {
                name: "R1b judge passed an answer a grounded assertion failed",
                mutate: |i| {
                    i.judge_passed = Some(true);
                    i.grounded_assertion_failed = true;
                },
                expect: FaultSide::Grader,
                expect_reason: "r1_judge_passed_ungrounded_answer",
            },
            Case {
                name: "R1 beats R2 (order matters)",
                mutate: |i| {
                    i.grounding = GroundingVerdict::Pass;
                    i.judge_passed = Some(false);
                    i.failure_reason = Some(&FailureReason::Timeout);
                },
                expect: FaultSide::Grader,
                expect_reason: "r1_judge_rejected_grounded_answer",
            },
            Case {
                name: "R2 rate limited",
                mutate: |i| i.failure_reason = Some(&FailureReason::RateLimited),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 billing",
                mutate: |i| i.failure_reason = Some(&FailureReason::Billing),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 timeout",
                mutate: |i| i.failure_reason = Some(&FailureReason::Timeout),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 binary missing",
                mutate: |i| i.failure_reason = Some(&FailureReason::BinaryMissing),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 spawn error",
                mutate: |i| i.failure_reason = Some(&FailureReason::SpawnError),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 no accounts",
                mutate: |i| i.failure_reason = Some(&FailureReason::NoAccounts),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 beats R3",
                mutate: |i| {
                    i.failure_reason = Some(&FailureReason::Timeout);
                    i.capability_blocked = true;
                },
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "EmptyResponse is NOT in the R2 set",
                mutate: |i| i.failure_reason = Some(&FailureReason::EmptyResponse),
                expect: FaultSide::Model,
                expect_reason: "model",
            },
            Case {
                name: "Unknown failure reason is NOT in the R2 set",
                mutate: |i| i.failure_reason = Some(&FailureReason::Unknown),
                expect: FaultSide::Model,
                expect_reason: "model",
            },
            Case {
                name: "R3 tool claim with zero native events",
                mutate: |i| {
                    i.native_tool_events = 0;
                    i.reply_claims_tool_use = true;
                },
                expect: FaultSide::Harness,
                expect_reason: "r3_tool_claim_without_native_events",
            },
            Case {
                name: "R3 capability blocked",
                mutate: |i| i.capability_blocked = true,
                expect: FaultSide::Harness,
                expect_reason: "r3_capability_blocked",
            },
            Case {
                name: "zero native events without a tool claim is NOT R3",
                mutate: |i| i.native_tool_events = 0,
                expect: FaultSide::Model,
                expect_reason: "model",
            },
            Case {
                name: "tool claim WITH native events is NOT R3",
                mutate: |i| {
                    i.native_tool_events = 3;
                    i.reply_claims_tool_use = true;
                },
                expect: FaultSide::Model,
                expect_reason: "model",
            },
            Case {
                name: "an ordinary judge rejection is the model's fault",
                mutate: |i| {
                    i.grounding = GroundingVerdict::Skip;
                    i.judge_passed = Some(false);
                },
                expect: FaultSide::Model,
                expect_reason: "model",
            },
            Case {
                name: "a grounding reject with no judge verdict is the model's fault",
                mutate: |i| {
                    i.grounding = GroundingVerdict::Fail;
                    i.grounded_assertion_failed = true;
                    i.judge_passed = None;
                },
                expect: FaultSide::Model,
                expect_reason: "model",
            },
            Case {
                name: "McpOnly fidelity still attributes (it is the main branch)",
                mutate: |i| {
                    i.fidelity = ObservationFidelity::McpOnly;
                    i.capability_blocked = true;
                },
                expect: FaultSide::Harness,
                expect_reason: "r3_capability_blocked",
            },
            // ── review P3: credentials/infrastructure belong to R2 ──
            Case {
                name: "R2 now covers AuthFailed",
                mutate: |i| i.failure_reason = Some(&FailureReason::AuthFailed),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 now covers AccountsCoolingDownLong",
                mutate: |i| i.failure_reason = Some(&FailureReason::AccountsCoolingDownLong),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 now covers AccountsCoolingDownShort",
                mutate: |i| i.failure_reason = Some(&FailureReason::AccountsCoolingDownShort),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            Case {
                name: "R2 now covers AccountsCoolingDownUnknown",
                mutate: |i| i.failure_reason = Some(&FailureReason::AccountsCoolingDownUnknown),
                expect: FaultSide::Environment,
                expect_reason: "r2_infrastructure_failure",
            },
            // ── review P2: R3's missing-event half needs a collector ──
            Case {
                name: "McpOnly + zero native events + a tool claim is NOT R3",
                mutate: |i| {
                    i.fidelity = ObservationFidelity::McpOnly;
                    i.native_tool_events = 0;
                    i.reply_claims_tool_use = true;
                },
                expect: FaultSide::Model,
                expect_reason: "model",
            },
            Case {
                name: "McpOnly + a real capability block is STILL R3",
                mutate: |i| {
                    i.fidelity = ObservationFidelity::McpOnly;
                    i.native_tool_events = 0;
                    i.reply_claims_tool_use = true;
                    i.capability_blocked = true;
                },
                expect: FaultSide::Harness,
                expect_reason: "r3_capability_blocked",
            },
        ];

        for c in cases {
            let mut inputs = base();
            (c.mutate)(&mut inputs);
            let (side, reason) = classify_fault_with_reason(&inputs);
            assert_eq!(side, c.expect, "case `{}`: wrong side", c.name);
            assert_eq!(reason, c.expect_reason, "case `{}`: wrong reason", c.name);
            assert_eq!(
                classify_fault(&inputs),
                c.expect,
                "case `{}`: classify_fault disagrees with classify_fault_with_reason",
                c.name
            );
        }
    }

    #[test]
    fn clean_round_is_the_models_own() {
        assert_eq!(classify_fault(&base()), FaultSide::Model);
        assert!(FaultSide::Model.counts_for_learning());
    }

    /// Review P2 regression, stated as the failure it fixes: on the project's
    /// self-declared main branch (`McpOnly`, no native collector) zero native
    /// events is STRUCTURAL, and `TOOL_USE_CLAIM_PHRASES` carries CJK entries
    /// for which `word_contains_ci` degrades to plain substring matching — so
    /// one ordinary reply was enough to relabel a genuine model failure as
    /// `Harness` and drop the round from learning.
    #[test]
    fn an_ordinary_cjk_completion_reply_no_longer_becomes_a_harness_fault() {
        assert!(
            reply_claims_tool_use("任務已執行完畢,報表放在 notes/a.md。"),
            "the heuristic still fires — the fix is the fidelity guard, not the phrase list"
        );
        let mut inputs = base();
        inputs.fidelity = ObservationFidelity::McpOnly;
        inputs.native_tool_events = 0;
        inputs.reply_claims_tool_use = true;
        let (side, reason) = classify_fault_with_reason(&inputs);
        assert_eq!(side, FaultSide::Model, "reason was {reason}");
        assert!(
            side.counts_for_learning(),
            "a real model failure must stay in the learning loop"
        );

        // Under `Full` the signal means something and R3 still fires.
        inputs.fidelity = ObservationFidelity::Full;
        assert_eq!(
            classify_fault_with_reason(&inputs),
            (FaultSide::Harness, "r3_tool_claim_without_native_events")
        );
    }

    /// Review P3 regression: a credential outage must not be learned from as
    /// though the model wrote a bad answer.
    #[test]
    fn a_credential_outage_is_the_environments_fault_not_the_models() {
        for reason in [
            FailureReason::AuthFailed,
            FailureReason::AccountsCoolingDownLong,
            FailureReason::AccountsCoolingDownShort,
            FailureReason::AccountsCoolingDownUnknown,
        ] {
            let mut inputs = base();
            inputs.failure_reason = Some(&reason);
            let side = classify_fault(&inputs);
            assert_eq!(
                side,
                FaultSide::Environment,
                "{} must be environmental",
                reason.as_str()
            );
            assert!(!side.counts_for_learning());
        }
    }

    #[test]
    fn only_model_counts_for_learning() {
        for side in [
            FaultSide::Harness,
            FaultSide::Environment,
            FaultSide::Grader,
            FaultSide::Unknown,
        ] {
            assert!(
                !side.counts_for_learning(),
                "{} must not feed the learning loop",
                side.as_str()
            );
        }
    }

    // ── token round-trip ────────────────────────────────────────────────

    #[test]
    fn fault_side_tokens_round_trip() {
        for side in [
            FaultSide::Model,
            FaultSide::Harness,
            FaultSide::Environment,
            FaultSide::Grader,
            FaultSide::Unknown,
        ] {
            assert_eq!(FaultSide::from_token(side.as_str()), side);
        }
        // Legacy / unrecognized rows read as `model` (the DEFAULT column
        // value and the pre-WP-C semantics of every existing row).
        assert_eq!(FaultSide::from_token(""), FaultSide::Model);
        assert_eq!(FaultSide::from_token("garbage"), FaultSide::Model);
    }

    // ── tool-use claim detector ─────────────────────────────────────────

    #[test]
    fn tool_use_claim_detector_matches_zh_and_en() {
        for text in [
            "I ran the query and here is the result",
            "I executed the script.",
            "我執行了查詢，結果如下",
            "已查詢資料庫，共 12 筆",
            "呼叫了工具取得報價",
            "我执行了脚本",
        ] {
            assert!(reply_claims_tool_use(text), "should match: {text}");
        }
    }

    #[test]
    fn tool_use_claim_detector_ignores_plain_text() {
        for text in [
            "Here is a summary of the plan.",
            "這是我的建議，沒有實際動作。",
            // `i ran` must not match inside a longer ASCII word.
            "the chairan committee",
            "",
        ] {
            assert!(!reply_claims_tool_use(text), "should NOT match: {text}");
        }
    }

    // ── config ──────────────────────────────────────────────────────────

    #[test]
    fn config_defaults_to_enabled() {
        let dir = tempfile::tempdir().unwrap();
        // No config.toml at all.
        assert!(enabled_from_home(dir.path()));
        // Malformed file.
        std::fs::write(dir.path().join("config.toml"), "not = = toml").unwrap();
        assert!(enabled_from_home(dir.path()));
        // Section present, key absent.
        std::fs::write(
            dir.path().join("config.toml"),
            "[evolution]\nenabled = true\n",
        )
        .unwrap();
        assert!(enabled_from_home(dir.path()));
    }

    #[test]
    fn config_opt_out_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[evolution]\nfault_attribution = false\n",
        )
        .unwrap();
        assert!(!enabled_from_home(dir.path()));
    }

    // ── capability-denial scan ──────────────────────────────────────────

    #[test]
    fn capability_blocked_reads_the_audit_trail() {
        let dir = tempfile::tempdir().unwrap();
        let since = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();

        // No audit file yet ⇒ no denial (fail-open).
        assert!(!capability_blocked_in_window(dir.path(), "agent-1", &since));

        duduclaw_security::audit::append_tool_call(
            dir.path(),
            "agent-1",
            "tasks_create",
            "{}",
            true,
        );
        assert!(!capability_blocked_in_window(dir.path(), "agent-1", &since));

        duduclaw_security::audit::append_tool_call_denied(
            dir.path(),
            "agent-1",
            "db_query",
            "denied_tools",
            "blocked by agent.toml [capabilities] denied_tools",
            None,
        );
        assert!(capability_blocked_in_window(dir.path(), "agent-1", &since));
        // Another agent's denial must not leak across.
        assert!(!capability_blocked_in_window(dir.path(), "agent-2", &since));
    }

    // ── FaultContext ────────────────────────────────────────────────────

    #[test]
    fn fault_context_maps_grounding_fail_to_assertion_failure() {
        let ctx = FaultContext {
            grounding: Some(GroundingVerdict::Fail),
            judge_passed: Some(true),
            ..Default::default()
        };
        let (side, reason) = ctx.classify(ObservationFidelity::Full, 2, false);
        assert_eq!(side, FaultSide::Grader);
        assert_eq!(reason, "r1_judge_passed_ungrounded_answer");
    }

    #[test]
    fn fault_context_default_is_a_model_round() {
        let ctx = FaultContext::default();
        let (side, _) = ctx.classify(ObservationFidelity::McpOnly, 1, false);
        assert_eq!(side, FaultSide::Model);
    }

    #[test]
    fn fault_context_blind_fidelity_is_unknown() {
        let ctx = FaultContext::default();
        let (side, reason) = ctx.classify(ObservationFidelity::None, 0, false);
        assert_eq!(side, FaultSide::Unknown);
        assert_eq!(reason, "r0_no_observation_fidelity");
    }
}
