//! The decomposability gate — **default Solo, form a team only when the work
//! is actually decomposable** (Team-as-Agent design §3.2).
//!
//! # Why a gate at all
//!
//! Anthropic's own guidance: "When the work is one dependent chain, or fits
//! in a single context, the orchestrator pays for a plan, a handoff, and a
//! merge that a single model gets for free." OneFlow (arXiv:2601.12307) finds
//! a single agent looping over multiple turns matches a homogeneous workflow
//! while keeping its KV cache warm; arXiv:2609.19759 finds the multi-agent
//! advantage appears only with long horizons **and** sparse dependencies.
//! So the platform's default is the path it already has — one agent, one
//! context — and a team is a per-task exception that must be earned.
//!
//! # Shape
//!
//! Zero LLM, zero I/O, total: [`decide`] is a pure function of
//! [`GateInputs`]. It reuses the `knowledge_route.rs` template already proven
//! in this workspace — L0 hard exclusions, then an L1 signal table, then a
//! grey band that defers rather than guessing:
//!
//! | layer | rule | result |
//! |---|---|---|
//! | sandbox | the employee has `[container] sandbox_enabled = true` | Solo, ahead of `mode` |
//! | mode | `always_solo` / `always_team` | dominates everything below |
//! | L0 | channel source · plan-first pending · irreversible action in the plan · fewer than [`TEAM_GATE_MIN_BUDGET_ROUNDS`] rounds | Solo |
//! | L1 | four independent signals, ≥ [`TEAM_GATE_SIGNALS_FOR_TEAM`] hit | Team |
//! | L2 | exactly [`TEAM_GATE_GREY_BAND_SIGNALS`] hit | [`GateDecision::GreyBand`] |
//! | else | — | Solo, with an effort hint |
//!
//! [`GateDecision::GreyBand`] is not "maybe". It is an instruction to the
//! composer: run the planner once — a call the task was going to pay for
//! anyway — and re-gate on what the plan actually decomposes into. Deciding
//! from a coin flip here is how an architecture change gets credited for
//! noise.
//!
//! **Solo is not "do nothing".** A Solo decision can carry an
//! `effort_hint`, because Anthropic's own cost guidance records that tuning
//! effort beat an architecture change — the cheapest win on this path is
//! usually a knob, not a team.
//!
//! # Honesty
//!
//! Every decision carries a stable reason token, so each one can be written
//! to `prediction_log` and scored against the task's actual outcome later
//! (design §3.2, "誠實校準"). Until that calibration reaches `Supported`, the
//! dashboard must label the gate experimental — a gate that has never been
//! scored is a hypothesis, not a feature.

use crate::types::TeamGateMode;

/// Minimum remaining goal-loop rounds for a team to be worth forming. Below
/// this the plan/handoff/merge overhead cannot amortise.
pub const TEAM_GATE_MIN_BUDGET_ROUNDS: u32 = 3;

/// L1 signal hits that force a team.
pub const TEAM_GATE_SIGNALS_FOR_TEAM: u8 = 3;

/// L1 signal hits that land in the grey band (planner runs once, then
/// re-gate).
pub const TEAM_GATE_GREY_BAND_SIGNALS: u8 = 2;

/// Minimum independent work items for the "bulk" signal.
pub const TEAM_GATE_BULK_MIN_ITEMS: u32 = 4;

/// Minimum acceptance criteria for the "long horizon" signal.
pub const TEAM_GATE_LONG_HORIZON_MIN_CRITERIA: u32 = 3;

/// Where the task came from. Only `/goal`-shaped work is eligible for a team:
/// a live channel turn cannot wait for plan → execute → verify, and that is a
/// product constraint, not a capability one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskSource {
    /// `/goal` (or `tasks.goal_create`).
    GoalCommand,
    /// A plan-first goal whose plan the operator already approved.
    PlanFirstApproved,
    /// A live channel turn. Always Solo — the facade answers.
    Channel,
    /// Fired by an autopilot rule.
    Autopilot,
    /// Cron, heartbeat, anything else.
    Other,
}

impl TaskSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskSource::GoalCommand => "goal_command",
            TaskSource::PlanFirstApproved => "plan_first_approved",
            TaskSource::Channel => "channel",
            TaskSource::Autopilot => "autopilot",
            TaskSource::Other => "other",
        }
    }
}

/// Everything the gate reads. `Option` fields are "not measured" — never
/// zero, never a guess: an unmeasured signal simply does not fire, which
/// biases the gate toward Solo. That direction is deliberate; the expensive
/// mistake is forming a team for work one agent would have finished.
#[derive(Debug, Clone, PartialEq)]
pub struct GateInputs {
    pub source: TaskSource,
    /// A plan-first goal is still waiting for its human decision.
    pub plan_first_pending: bool,
    /// The plan contains at least one action ActionGuard classes as
    /// irreversible. A team multiplies the actors; irreversibility is exactly
    /// when that is the wrong trade.
    pub irreversible_in_plan: bool,
    /// Goal-loop rounds still available.
    pub budget_rounds: u32,
    /// ① bulk — estimated work items that could run independently.
    pub independent_items: Option<u32>,
    /// ① bulk — hub nodes in the dependency graph. `Some(0)` (measured, and
    /// there are none) is what makes the items independent; `None` means the
    /// graph was never built, so the signal cannot fire.
    pub dependency_hubs: Option<u32>,
    /// ② context — estimated prompt tokens for the whole task.
    pub estimated_input_tokens: Option<u64>,
    /// ② context — the single-agent context window to compare against.
    pub context_window_tokens: Option<u64>,
    /// ③ capability gap between the executor and planner candidates, in
    /// percentage points, from the role×model matrix.
    pub capability_gap_pp: Option<f32>,
    /// ③ the matrix's declared minimum detectable effect, also in percentage
    /// points. A gap below the MDE is noise (Miller, arXiv:2411.00640) and
    /// must not be treated as a capability difference.
    pub declared_mde_pp: Option<f32>,
    /// ④ long horizon — frozen acceptance criteria count.
    pub acceptance_criteria_count: u32,
    /// ④ long horizon — the task produces artifacts (files, not just prose).
    pub produces_artifacts: bool,
    /// Resolved `[team] gate`.
    pub mode: TeamGateMode,
    /// The employee has `agent.toml [container] sandbox_enabled = true`. Role
    /// members run the employee's CLI on the host with the full platform tool
    /// surface, which is exactly what the sandbox exists to prevent, so this
    /// employee never forms a team: the round runs Solo through the sandboxed
    /// dispatch path. Checked ahead of `mode`, so the testing-only
    /// `always_team` cannot override it.
    pub sandbox_enabled: bool,
}

impl Default for GateInputs {
    /// A task about which nothing is known: `Other` source, no budget, no
    /// measurements. Decides Solo, which is the right answer for "no
    /// information".
    fn default() -> Self {
        Self {
            source: TaskSource::Other,
            plan_first_pending: false,
            irreversible_in_plan: false,
            budget_rounds: 0,
            independent_items: None,
            dependency_hubs: None,
            estimated_input_tokens: None,
            context_window_tokens: None,
            capability_gap_pp: None,
            declared_mde_pp: None,
            acceptance_criteria_count: 0,
            produces_artifacts: false,
            mode: TeamGateMode::Auto,
            sandbox_enabled: false,
        }
    }
}

/// Which of the four L1 signals fired. Carried out of the gate so the
/// calibration log records *why*, not just *what* — a gate whose reasons are
/// not recorded cannot be scored, and an unscored gate cannot be improved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateSignals {
    /// ① ≥ [`TEAM_GATE_BULK_MIN_ITEMS`] independent items, zero dependency
    /// hubs.
    pub bulk: bool,
    /// ② the task does not fit one context window.
    pub context_overflow: bool,
    /// ③ the executor/planner capability gap clears the declared MDE.
    pub capability_gap: bool,
    /// ④ ≥ [`TEAM_GATE_LONG_HORIZON_MIN_CRITERIA`] acceptance criteria and
    /// real artifacts.
    pub long_horizon: bool,
}

impl GateSignals {
    pub fn count(&self) -> u8 {
        u8::from(self.bulk)
            + u8::from(self.context_overflow)
            + u8::from(self.capability_gap)
            + u8::from(self.long_horizon)
    }

    /// Stable tokens for the signals that fired, in declaration order.
    pub fn hit_codes(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.bulk {
            v.push("bulk");
        }
        if self.context_overflow {
            v.push("context_overflow");
        }
        if self.capability_gap {
            v.push("capability_gap");
        }
        if self.long_horizon {
            v.push("long_horizon");
        }
        v
    }
}

/// What the gate decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Run the existing single-agent path — byte-identical to today.
    Solo {
        reason: &'static str,
        /// Optional `[team.roles.*] effort` suggestion for that single agent
        /// (design D12). `Some("low")` on obviously small work.
        effort_hint: Option<&'static str>,
    },
    /// Form a team.
    Team {
        reason: &'static str,
        signals_hit: u8,
    },
    /// Run the planner once, then call [`decide`] again with the plan's real
    /// decomposition. Never a coin flip.
    GreyBand { signals_hit: u8 },
}

impl GateDecision {
    /// Stable token naming the branch taken.
    pub fn code(&self) -> &'static str {
        match self {
            GateDecision::Solo { .. } => "solo",
            GateDecision::Team { .. } => "team",
            GateDecision::GreyBand { .. } => "grey_band",
        }
    }

    /// Stable token naming *why*. `GreyBand`'s reason is the branch itself.
    pub fn reason(&self) -> &'static str {
        match self {
            GateDecision::Solo { reason, .. } | GateDecision::Team { reason, .. } => reason,
            GateDecision::GreyBand { .. } => "grey_band_two_signals",
        }
    }

    pub fn is_team(&self) -> bool {
        matches!(self, GateDecision::Team { .. })
    }

    pub fn is_solo(&self) -> bool {
        matches!(self, GateDecision::Solo { .. })
    }
}

/// Evaluate the four L1 signals. Separate from [`decide`] so the composer can
/// log the signal vector even when an L0 rule short-circuited the decision.
///
/// Float comparison note: a `NaN` in either percentage-point field makes the
/// `>=` false, so the capability signal does not fire. That is the safe
/// direction (toward Solo) and is asserted by test.
pub fn evaluate_signals(input: &GateInputs) -> GateSignals {
    let bulk = matches!(input.independent_items, Some(n) if n >= TEAM_GATE_BULK_MIN_ITEMS)
        && input.dependency_hubs == Some(0);

    let context_overflow = match (input.estimated_input_tokens, input.context_window_tokens) {
        (Some(need), Some(window)) => need > window,
        _ => false,
    };

    let capability_gap = match (input.capability_gap_pp, input.declared_mde_pp) {
        (Some(gap), Some(mde)) => gap >= mde,
        _ => false,
    };

    let long_horizon = input.acceptance_criteria_count >= TEAM_GATE_LONG_HORIZON_MIN_CRITERIA
        && input.produces_artifacts;

    GateSignals {
        bulk,
        context_overflow,
        capability_gap,
        long_horizon,
    }
}

/// The gate.
///
/// Evaluation order is load-bearing: the sandbox rule first (an isolation
/// boundary is not a tuning knob, so no mode may form a team around it), then
/// `mode` (an operator override must be an override), then L0 exclusions, then
/// the L1 count.
pub fn decide(input: &GateInputs) -> GateDecision {
    // ── Isolation boundary ──────────────────────────────────────────────
    if input.sandbox_enabled {
        return GateDecision::Solo {
            reason: "sandbox_enabled",
            effort_hint: None,
        };
    }

    // ── Operator override ───────────────────────────────────────────────
    match input.mode {
        TeamGateMode::AlwaysSolo => {
            return GateDecision::Solo {
                reason: "mode_always_solo",
                effort_hint: None,
            };
        }
        TeamGateMode::AlwaysTeam => {
            // Deliberately ahead of L0, which is exactly why this mode is
            // documented as testing-only: it can form a team for a task
            // holding an irreversible action.
            return GateDecision::Team {
                reason: "mode_always_team",
                signals_hit: evaluate_signals(input).count(),
            };
        }
        TeamGateMode::Auto => {}
    }

    // ── L0 hard exclusions ──────────────────────────────────────────────
    if input.source == TaskSource::Channel {
        return GateDecision::Solo {
            reason: "channel_source",
            effort_hint: None,
        };
    }
    if input.plan_first_pending {
        return GateDecision::Solo {
            reason: "plan_first_pending",
            effort_hint: None,
        };
    }
    if input.irreversible_in_plan {
        return GateDecision::Solo {
            reason: "irreversible_in_plan",
            effort_hint: None,
        };
    }
    if input.budget_rounds < TEAM_GATE_MIN_BUDGET_ROUNDS {
        return GateDecision::Solo {
            reason: "budget_rounds_below_min",
            effort_hint: None,
        };
    }

    // ── L1 signals ──────────────────────────────────────────────────────
    let signals = evaluate_signals(input);
    let hits = signals.count();

    if hits >= TEAM_GATE_SIGNALS_FOR_TEAM {
        return GateDecision::Team {
            reason: "decomposable_signals",
            signals_hit: hits,
        };
    }
    if hits == TEAM_GATE_GREY_BAND_SIGNALS {
        return GateDecision::GreyBand { signals_hit: hits };
    }

    // ── Solo, with an effort suggestion when the work is plainly small ──
    let small = matches!(input.independent_items, Some(n) if n <= 1) && !input.produces_artifacts;
    GateDecision::Solo {
        reason: "insufficient_signals",
        effort_hint: if small { Some("low") } else { None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A goal task that is eligible (passes L0) and hits zero signals.
    fn eligible() -> GateInputs {
        GateInputs {
            source: TaskSource::GoalCommand,
            budget_rounds: 5,
            ..GateInputs::default()
        }
    }

    /// Turn on exactly `n` of the four signals, in declaration order.
    fn with_signals(n: u8) -> GateInputs {
        let mut i = eligible();
        if n >= 1 {
            i.independent_items = Some(TEAM_GATE_BULK_MIN_ITEMS);
            i.dependency_hubs = Some(0);
        }
        if n >= 2 {
            i.estimated_input_tokens = Some(300_000);
            i.context_window_tokens = Some(200_000);
        }
        if n >= 3 {
            i.capability_gap_pp = Some(20.0);
            i.declared_mde_pp = Some(13.0);
        }
        if n >= 4 {
            i.acceptance_criteria_count = TEAM_GATE_LONG_HORIZON_MIN_CRITERIA;
            i.produces_artifacts = true;
        }
        i
    }

    // ── table-driven ────────────────────────────────────────────────────

    #[test]
    fn decision_table() {
        struct Row {
            name: &'static str,
            input: GateInputs,
            code: &'static str,
            reason: &'static str,
        }

        let rows = vec![
            // ── mode overrides dominate ──────────────────────────────────
            Row {
                name: "always_solo beats every signal",
                input: GateInputs {
                    mode: TeamGateMode::AlwaysSolo,
                    ..with_signals(4)
                },
                code: "solo",
                reason: "mode_always_solo",
            },
            Row {
                name: "always_team beats a channel source",
                input: GateInputs {
                    mode: TeamGateMode::AlwaysTeam,
                    source: TaskSource::Channel,
                    ..eligible()
                },
                code: "team",
                reason: "mode_always_team",
            },
            Row {
                name: "always_team beats an irreversible plan (testing-only mode)",
                input: GateInputs {
                    mode: TeamGateMode::AlwaysTeam,
                    irreversible_in_plan: true,
                    ..eligible()
                },
                code: "team",
                reason: "mode_always_team",
            },
            // ── sandbox-enabled employee never forms a team ─────────────
            Row {
                name: "sandbox_enabled is solo even with every signal",
                input: GateInputs {
                    sandbox_enabled: true,
                    ..with_signals(4)
                },
                code: "solo",
                reason: "sandbox_enabled",
            },
            Row {
                name: "sandbox_enabled beats always_team",
                input: GateInputs {
                    sandbox_enabled: true,
                    mode: TeamGateMode::AlwaysTeam,
                    ..with_signals(4)
                },
                code: "solo",
                reason: "sandbox_enabled",
            },
            Row {
                name: "sandbox_enabled under always_solo reports the sandbox",
                input: GateInputs {
                    sandbox_enabled: true,
                    mode: TeamGateMode::AlwaysSolo,
                    ..eligible()
                },
                code: "solo",
                reason: "sandbox_enabled",
            },
            Row {
                name: "sandbox_enabled skips the grey band too",
                input: GateInputs {
                    sandbox_enabled: true,
                    ..with_signals(2)
                },
                code: "solo",
                reason: "sandbox_enabled",
            },
            // ── L0 ───────────────────────────────────────────────────────
            Row {
                name: "live channel turn is always solo",
                input: GateInputs {
                    source: TaskSource::Channel,
                    ..with_signals(4)
                },
                code: "solo",
                reason: "channel_source",
            },
            Row {
                name: "plan-first still awaiting approval",
                input: GateInputs {
                    plan_first_pending: true,
                    ..with_signals(4)
                },
                code: "solo",
                reason: "plan_first_pending",
            },
            Row {
                name: "irreversible action in the plan",
                input: GateInputs {
                    irreversible_in_plan: true,
                    ..with_signals(4)
                },
                code: "solo",
                reason: "irreversible_in_plan",
            },
            Row {
                name: "budget below the minimum rounds",
                input: GateInputs {
                    budget_rounds: TEAM_GATE_MIN_BUDGET_ROUNDS - 1,
                    ..with_signals(4)
                },
                code: "solo",
                reason: "budget_rounds_below_min",
            },
            Row {
                name: "budget exactly at the minimum passes L0",
                input: GateInputs {
                    budget_rounds: TEAM_GATE_MIN_BUDGET_ROUNDS,
                    ..with_signals(3)
                },
                code: "team",
                reason: "decomposable_signals",
            },
            // ── L1 counts ────────────────────────────────────────────────
            Row {
                name: "zero signals",
                input: with_signals(0),
                code: "solo",
                reason: "insufficient_signals",
            },
            Row {
                name: "one signal",
                input: with_signals(1),
                code: "solo",
                reason: "insufficient_signals",
            },
            Row {
                name: "two signals land in the grey band",
                input: with_signals(2),
                code: "grey_band",
                reason: "grey_band_two_signals",
            },
            Row {
                name: "three signals form a team",
                input: with_signals(3),
                code: "team",
                reason: "decomposable_signals",
            },
            Row {
                name: "four signals form a team",
                input: with_signals(4),
                code: "team",
                reason: "decomposable_signals",
            },
            // ── signal edge cases ────────────────────────────────────────
            Row {
                name: "bulk needs a measured hub count, not an absent one",
                input: GateInputs {
                    independent_items: Some(12),
                    dependency_hubs: None,
                    ..with_signals(3)
                },
                code: "grey_band",
                reason: "grey_band_two_signals",
            },
            Row {
                name: "many items but a dependency hub is not bulk",
                input: GateInputs {
                    independent_items: Some(40),
                    dependency_hubs: Some(1),
                    ..eligible()
                },
                code: "solo",
                reason: "insufficient_signals",
            },
            Row {
                name: "capability gap below the declared MDE is noise",
                input: GateInputs {
                    capability_gap_pp: Some(9.0),
                    declared_mde_pp: Some(13.0),
                    ..with_signals(3)
                },
                code: "grey_band",
                reason: "grey_band_two_signals",
            },
            Row {
                name: "capability gap exactly at the MDE counts",
                input: GateInputs {
                    capability_gap_pp: Some(13.0),
                    declared_mde_pp: Some(13.0),
                    ..with_signals(2)
                },
                code: "team",
                reason: "decomposable_signals",
            },
            Row {
                name: "acceptance criteria without artifacts is not long-horizon",
                input: GateInputs {
                    acceptance_criteria_count: 9,
                    produces_artifacts: false,
                    ..with_signals(2)
                },
                code: "grey_band",
                reason: "grey_band_two_signals",
            },
            Row {
                name: "the same criteria WITH artifacts is the third signal",
                input: GateInputs {
                    acceptance_criteria_count: 9,
                    produces_artifacts: true,
                    ..with_signals(2)
                },
                code: "team",
                reason: "decomposable_signals",
            },
            Row {
                name: "context fitting the window is not an overflow",
                input: GateInputs {
                    estimated_input_tokens: Some(200_000),
                    context_window_tokens: Some(200_000),
                    ..eligible()
                },
                code: "solo",
                reason: "insufficient_signals",
            },
            Row {
                name: "autopilot source is eligible",
                input: GateInputs {
                    source: TaskSource::Autopilot,
                    ..with_signals(3)
                },
                code: "team",
                reason: "decomposable_signals",
            },
            Row {
                name: "plan-first approved is eligible",
                input: GateInputs {
                    source: TaskSource::PlanFirstApproved,
                    ..with_signals(3)
                },
                code: "team",
                reason: "decomposable_signals",
            },
            Row {
                name: "an unknown task decides solo",
                input: GateInputs::default(),
                code: "solo",
                reason: "budget_rounds_below_min",
            },
        ];

        assert!(rows.len() >= 15, "the table is the specification");
        for row in rows {
            let got = decide(&row.input);
            assert_eq!(got.code(), row.code, "{}: {got:?}", row.name);
            assert_eq!(got.reason(), row.reason, "{}: {got:?}", row.name);
        }
    }

    // ── property-style ──────────────────────────────────────────────────

    #[test]
    fn sandbox_enabled_is_solo_for_every_mode_and_input_shape() {
        for mode in [TeamGateMode::Auto, TeamGateMode::AlwaysSolo, TeamGateMode::AlwaysTeam] {
            for source in [TaskSource::GoalCommand, TaskSource::Autopilot, TaskSource::Other] {
                for signals in 0..=4u8 {
                    let input = GateInputs {
                        sandbox_enabled: true,
                        mode,
                        source,
                        budget_rounds: 99,
                        ..with_signals(signals)
                    };
                    let got = decide(&input);
                    assert!(got.is_solo(), "{input:?} → {got:?}");
                    assert_eq!(got.reason(), "sandbox_enabled");
                }
            }
        }
    }

    #[test]
    fn always_solo_dominates_every_input_shape() {
        for source in [
            TaskSource::GoalCommand,
            TaskSource::PlanFirstApproved,
            TaskSource::Channel,
            TaskSource::Autopilot,
            TaskSource::Other,
        ] {
            for signals in 0..=4u8 {
                for plan_first_pending in [false, true] {
                    for irreversible_in_plan in [false, true] {
                        for budget_rounds in [0u32, 3, 99] {
                            let input = GateInputs {
                                mode: TeamGateMode::AlwaysSolo,
                                source,
                                plan_first_pending,
                                irreversible_in_plan,
                                budget_rounds,
                                ..with_signals(signals)
                            };
                            let got = decide(&input);
                            assert!(got.is_solo(), "{input:?} → {got:?}");
                            assert_eq!(got.reason(), "mode_always_solo");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn always_team_dominates_every_input_shape() {
        for source in [
            TaskSource::GoalCommand,
            TaskSource::PlanFirstApproved,
            TaskSource::Channel,
            TaskSource::Autopilot,
            TaskSource::Other,
        ] {
            for signals in 0..=4u8 {
                for plan_first_pending in [false, true] {
                    for irreversible_in_plan in [false, true] {
                        for budget_rounds in [0u32, 3, 99] {
                            let input = GateInputs {
                                mode: TeamGateMode::AlwaysTeam,
                                source,
                                plan_first_pending,
                                irreversible_in_plan,
                                budget_rounds,
                                ..with_signals(signals)
                            };
                            let got = decide(&input);
                            assert!(got.is_team(), "{input:?} → {got:?}");
                            assert_eq!(got.reason(), "mode_always_team");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn auto_mode_never_forms_a_team_on_an_l0_exclusion() {
        for (name, input) in [
            (
                "channel",
                GateInputs {
                    source: TaskSource::Channel,
                    ..with_signals(4)
                },
            ),
            (
                "plan_first",
                GateInputs {
                    plan_first_pending: true,
                    ..with_signals(4)
                },
            ),
            (
                "irreversible",
                GateInputs {
                    irreversible_in_plan: true,
                    ..with_signals(4)
                },
            ),
            (
                "budget",
                GateInputs {
                    budget_rounds: 0,
                    ..with_signals(4)
                },
            ),
        ] {
            let got = decide(&input);
            assert!(got.is_solo(), "{name} → {got:?}");
        }
    }

    // ── effort hint ─────────────────────────────────────────────────────

    #[test]
    fn small_work_gets_a_low_effort_hint() {
        let input = GateInputs {
            independent_items: Some(1),
            produces_artifacts: false,
            ..eligible()
        };
        assert_eq!(
            decide(&input),
            GateDecision::Solo {
                reason: "insufficient_signals",
                effort_hint: Some("low"),
            }
        );

        // Artifacts mean real output — no hint.
        let input = GateInputs {
            independent_items: Some(1),
            produces_artifacts: true,
            ..eligible()
        };
        assert!(decide(&input).is_solo());
        assert!(matches!(
            decide(&input),
            GateDecision::Solo {
                effort_hint: None,
                ..
            }
        ));

        // Unmeasured item count is not "small"; it is unknown.
        let input = GateInputs {
            independent_items: None,
            ..eligible()
        };
        assert!(matches!(
            decide(&input),
            GateDecision::Solo {
                effort_hint: None,
                ..
            }
        ));

        // L0 exclusions never carry a hint — the decision was not about size.
        let input = GateInputs {
            source: TaskSource::Channel,
            independent_items: Some(0),
            ..eligible()
        };
        assert!(matches!(
            decide(&input),
            GateDecision::Solo {
                effort_hint: None,
                ..
            }
        ));
    }

    // ── signals ─────────────────────────────────────────────────────────

    #[test]
    fn signal_counting_and_codes() {
        for n in 0..=4u8 {
            let s = evaluate_signals(&with_signals(n));
            assert_eq!(s.count(), n, "{n} signals");
            assert_eq!(s.hit_codes().len() as u8, n);
        }
        let all = evaluate_signals(&with_signals(4));
        assert_eq!(
            all.hit_codes(),
            vec!["bulk", "context_overflow", "capability_gap", "long_horizon"]
        );
        assert_eq!(GateSignals::default().count(), 0);
    }

    #[test]
    fn nan_percentage_points_do_not_fire_the_capability_signal() {
        for (gap, mde) in [
            (f32::NAN, 13.0f32),
            (20.0f32, f32::NAN),
            (f32::NAN, f32::NAN),
        ] {
            let input = GateInputs {
                capability_gap_pp: Some(gap),
                declared_mde_pp: Some(mde),
                ..eligible()
            };
            assert!(
                !evaluate_signals(&input).capability_gap,
                "NaN must fail closed toward Solo"
            );
        }
    }

    #[test]
    fn a_measured_zero_gap_is_not_a_signal() {
        let input = GateInputs {
            capability_gap_pp: Some(0.0),
            declared_mde_pp: Some(13.0),
            ..eligible()
        };
        assert!(!evaluate_signals(&input).capability_gap);
    }

    #[test]
    fn signals_are_reported_even_when_l0_short_circuits() {
        // The composer logs the vector regardless of the branch taken, so a
        // later calibration pass can ask "would this have been a team?".
        let input = GateInputs {
            source: TaskSource::Channel,
            ..with_signals(4)
        };
        assert_eq!(evaluate_signals(&input).count(), 4);
        assert!(decide(&input).is_solo());
    }

    #[test]
    fn decision_codes_are_stable_and_distinct() {
        let solo = GateDecision::Solo {
            reason: "insufficient_signals",
            effort_hint: None,
        };
        let team = GateDecision::Team {
            reason: "decomposable_signals",
            signals_hit: 3,
        };
        let grey = GateDecision::GreyBand { signals_hit: 2 };
        assert_eq!(solo.code(), "solo");
        assert_eq!(team.code(), "team");
        assert_eq!(grey.code(), "grey_band");
        assert!(!solo.is_team() && team.is_team() && !grey.is_team());
        assert!(solo.is_solo() && !team.is_solo() && !grey.is_solo());
    }

    #[test]
    fn task_source_tokens_are_stable() {
        assert_eq!(TaskSource::GoalCommand.as_str(), "goal_command");
        assert_eq!(
            TaskSource::PlanFirstApproved.as_str(),
            "plan_first_approved"
        );
        assert_eq!(TaskSource::Channel.as_str(), "channel");
        assert_eq!(TaskSource::Autopilot.as_str(), "autopilot");
        assert_eq!(TaskSource::Other.as_str(), "other");
    }
}
