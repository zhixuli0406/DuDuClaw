//! WP2.3 §3.2 — strategy mix (GEP G4) and per-round intent selection.
//!
//! [`decide_intent`] is a **pure function with no randomness**: the mix is
//! honoured *over time* by indexing a lookup table with `round_seq % 10`
//! rather than by rolling dice each round. That makes the configured ratio a
//! testable property (`balanced_strategy_over_100_rounds_hits_5_3_2_mix`)
//! instead of a statistical hope, and it makes a re-run of round N
//! reproducible — which §2.4.2's noise-band calibration depends on.
//!
//! The other half of the design's philosophy is encoded in
//! [`IntentDecision::Skip`]: when the material an intent needs is absent, the
//! round degrades one step and, if there is nothing left to degrade to, it is
//! **skipped and recorded as skipped**. It never fabricates work to look
//! busy (honesty convention; `repair_only_with_no_material_skips_not_fabricates`).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Per-agent evolution strategy — `agent.toml [evolution] strategy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionStrategy {
    #[default]
    Balanced,
    Innovate,
    Harden,
    RepairOnly,
}

impl EvolutionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Innovate => "innovate",
            Self::Harden => "harden",
            Self::RepairOnly => "repair_only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "balanced" => Self::Balanced,
            "innovate" => Self::Innovate,
            "harden" => Self::Harden,
            "repair_only" => Self::RepairOnly,
            _ => return None,
        })
    }

    /// Read `[evolution] strategy` from the agent's `agent.toml`.
    ///
    /// Missing file / malformed TOML / absent key / unrecognised value →
    /// [`EvolutionStrategy::Balanced`]. An unrecognised value additionally
    /// `warn!`s: a typo'd strategy silently behaving as `balanced` is exactly
    /// the class of "config typo, nobody notices for three months" defect
    /// that root cause R3 was.
    ///
    /// Goes through the shared typed parse point
    /// ([`duduclaw_core::agent_toml`]); the key stays a raw `String` on the
    /// typed section so this lenient mapping (and its `warn!`) keeps running
    /// instead of a strict serde enum turning a typo into total agent loss.
    pub fn from_agent_dir(agent_dir: &Path) -> Self {
        let Some(s) = duduclaw_core::agent_toml::load(agent_dir).evolution.strategy else {
            return Self::default();
        };
        let s = s.as_str();
        match Self::parse(s) {
            Some(v) => v,
            None => {
                tracing::warn!(
                    strategy = s,
                    "AEE: unrecognised [evolution] strategy — falling back to balanced"
                );
                Self::default()
            }
        }
    }
}

/// What this round is trying to do.
///
/// `Default` is `Repair` — the conservative choice: consuming a recorded
/// failure is always defensible, inventing a new rule is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoundIntent {
    /// Consume a concrete MistakeNotebook failure.
    #[default]
    Repair,
    /// Sharpen an existing low-streak entry. Never adds.
    Optimize,
    /// Explore a new rule with no direct failure behind it. At most one add.
    Innovate,
}

impl RoundIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repair => "repair",
            Self::Optimize => "optimize",
            Self::Innovate => "innovate",
        }
    }
}

/// Why a round produced no work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// `repair_only` configured but the MistakeNotebook is empty.
    NoRepairMaterial,
    /// Nothing to optimise and nothing may be added (playbook is full).
    PlaybookAtCapacity,
    /// The per-agent GVU cooldown has time left.
    Cooldown,
}

impl SkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoRepairMaterial => "no_repair_material",
            Self::PlaybookAtCapacity => "playbook_at_capacity",
            Self::Cooldown => "cooldown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentDecision {
    Run(RoundIntent),
    Skip(SkipReason),
}

impl IntentDecision {
    pub fn intent(&self) -> Option<RoundIntent> {
        match self {
            Self::Run(i) => Some(*i),
            Self::Skip(_) => None,
        }
    }
}

/// What kicked this AEE round off. Mirrors
/// [`crate::gvu::trigger::TriggerSource`] but is decoupled from it so the
/// pure mix function has no dependency on the trigger runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AeeTrigger {
    /// A user-facing turn produced a Significant/Critical error.
    ChannelReply,
    /// A sub-agent dispatch produced a synthetic Significant/Critical error.
    SubAgentDispatch,
    /// The silence timer fired — no concrete failure, so this is the
    /// exploration window. Default: the neutral trigger that defers to the
    /// configured mix rather than overriding it.
    #[default]
    ForcedReflection,
    /// WP0.5's detector says the agent is stuck (AVO P8).
    Stagnation,
    /// ε-floor forced exploration from `prediction::router`.
    Exploration,
}

impl AeeTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChannelReply => "channel_reply",
            Self::SubAgentDispatch => "subagent_dispatch",
            Self::ForcedReflection => "forced_reflection",
            Self::Stagnation => "stagnation",
            Self::Exploration => "exploration",
        }
    }

    /// §3.2.2 step 3 — trigger overrides that beat the configured ratio.
    /// `None` means "no override, use the mix".
    fn override_intent(self) -> Option<RoundIntent> {
        match self {
            // A concrete failure exists — consume it.
            Self::ChannelReply | Self::SubAgentDispatch => Some(RoundIntent::Repair),
            // No concrete failure: the mix decides.
            Self::ForcedReflection => None,
            // AVO P8: when stuck, change direction rather than sanding the
            // same spot harder.
            Self::Stagnation | Self::Exploration => Some(RoundIntent::Innovate),
        }
    }
}

/// The material available this round. Grouping the counts keeps
/// [`decide_intent`]'s signature readable and makes it obvious at every call
/// site which number means what.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoundMaterial {
    /// Unresolved MistakeNotebook entries for this agent.
    pub unresolved_mistakes: usize,
    /// Active/probation entries with `success_streak < 2`.
    pub low_streak_entries: usize,
    /// Active + probation entry count (against `PLAYBOOK_MAX_ENTRIES`).
    pub active_entries: usize,
}

impl RoundMaterial {
    fn playbook_full(&self) -> bool {
        self.active_entries >= crate::playbook::PLAYBOOK_MAX_ENTRIES
    }
}

/// §3.2.2 step 1: this many unresolved mistakes makes Repair mandatory.
pub const REPAIR_PRIORITY_THRESHOLD: usize = 3;

/// The `round_seq % 10` mix table (§3.2.2 step 4).
///
/// | strategy | Repair | Optimize | Innovate |
/// |----------|--------|----------|----------|
/// | balanced | 5 | 3 | 2 |
/// | innovate | 2 | 3 | 5 |
/// | harden   | 4 | 5 | 1 |
/// | repair_only | 10 | 0 | 0 |
fn mix_lookup(strategy: EvolutionStrategy, slot: u64) -> RoundIntent {
    use RoundIntent::*;
    match strategy {
        EvolutionStrategy::Balanced => match slot {
            0..=4 => Repair,
            5..=7 => Optimize,
            _ => Innovate,
        },
        EvolutionStrategy::Innovate => match slot {
            0..=1 => Repair,
            2..=4 => Optimize,
            _ => Innovate,
        },
        EvolutionStrategy::Harden => match slot {
            0..=3 => Repair,
            4..=8 => Optimize,
            _ => Innovate,
        },
        EvolutionStrategy::RepairOnly => Repair,
    }
}

/// Deterministic intent selection (§3.2.2). No randomness, no I/O.
pub fn decide_intent(
    strategy: EvolutionStrategy,
    trigger: AeeTrigger,
    round_seq: u64,
    material: RoundMaterial,
) -> IntentDecision {
    // Step 1 — hard precondition: a backlog of unresolved mistakes outranks
    // everything except an explicitly innovation-biased agent.
    let base = if material.unresolved_mistakes >= REPAIR_PRIORITY_THRESHOLD
        && strategy != EvolutionStrategy::Innovate
    {
        RoundIntent::Repair
    } else if strategy == EvolutionStrategy::RepairOnly {
        // Step 2 — repair_only is repair, or nothing. It never fabricates an
        // optimize/innovate round to fill the slot.
        if material.unresolved_mistakes == 0 {
            return IntentDecision::Skip(SkipReason::NoRepairMaterial);
        }
        RoundIntent::Repair
    } else if let Some(forced) = trigger.override_intent() {
        // Step 3 — trigger override.
        forced
    } else {
        // Step 4 — the configured ratio.
        mix_lookup(strategy, round_seq % 10)
    };

    // Step 5 — material-absence degradation, applied in a fixed order so the
    // result stays deterministic: Repair → Optimize → Innovate → Skip.
    degrade(base, material)
}

fn degrade(mut intent: RoundIntent, material: RoundMaterial) -> IntentDecision {
    if intent == RoundIntent::Repair && material.unresolved_mistakes == 0 {
        intent = RoundIntent::Optimize;
    }
    if intent == RoundIntent::Optimize && material.low_streak_entries == 0 {
        intent = RoundIntent::Innovate;
    }
    if intent == RoundIntent::Innovate && material.playbook_full() {
        // Adding to a full playbook is exactly how a playbook turns back into
        // the always-on SOUL.md this whole design replaced.
        return IntentDecision::Skip(SkipReason::PlaybookAtCapacity);
    }
    IntentDecision::Run(intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── R5: `[evolution] strategy` direction, pinned ─────────────────────
    //
    // absent / malformed / wrong-typed / unrecognised ⇒ Balanced. The value
    // deliberately stays a raw String on the typed section: a strict serde
    // enum would turn `strategy = "hardern"` into an `AgentConfig` parse
    // error, and `AgentRegistry::load_agent` treats that as fatal — the agent
    // would vanish from the registry over a typo. Here it warns and runs
    // balanced.

    #[test]
    fn default_direction_strategy_falls_back_to_balanced_never_errors() {
        for body in [
            "",                                     // empty file
            "[evolution]\n",                        // section, no key
            "[evolution]\ngvu_enabled = true\n",    // sibling key only
            "[evolution]\nstrategy = \"hardern\"\n", // typo ⇒ warn + balanced
            "[evolution]\nstrategy = \"\"\n",       // blank
            "[evolution]\nstrategy = 42\n",         // wrong type
            "evolution = \"scalar\"\n",             // wrong-typed section
            "not toml [[[",                         // malformed file
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("agent.toml"), body).unwrap();
            assert_eq!(
                EvolutionStrategy::from_agent_dir(dir.path()),
                EvolutionStrategy::Balanced,
                "for {body:?}"
            );
        }

        // Missing file entirely — same direction.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            EvolutionStrategy::from_agent_dir(empty.path()),
            EvolutionStrategy::Balanced
        );

        // A recognised value still reaches the engine.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("agent.toml"),
            "[evolution]\nstrategy = \"repair_only\"\n",
        )
        .unwrap();
        assert_eq!(
            EvolutionStrategy::from_agent_dir(dir.path()),
            EvolutionStrategy::RepairOnly
        );
    }

    fn rich() -> RoundMaterial {
        RoundMaterial { unresolved_mistakes: 1, low_streak_entries: 1, active_entries: 0 }
    }

    #[test]
    fn balanced_strategy_over_100_rounds_hits_5_3_2_mix() {
        // Neutral trigger (no override) and material for every intent, so the
        // table alone decides.
        let mut counts = (0usize, 0usize, 0usize);
        for seq in 0..100u64 {
            match decide_intent(
                EvolutionStrategy::Balanced,
                AeeTrigger::ForcedReflection,
                seq,
                rich(),
            ) {
                IntentDecision::Run(RoundIntent::Repair) => counts.0 += 1,
                IntentDecision::Run(RoundIntent::Optimize) => counts.1 += 1,
                IntentDecision::Run(RoundIntent::Innovate) => counts.2 += 1,
                IntentDecision::Skip(r) => panic!("unexpected skip: {:?}", r),
            }
        }
        assert_eq!(counts, (50, 30, 20), "balanced must be exactly 5:3:2 over 100 rounds");
    }

    #[test]
    fn every_strategy_honours_its_documented_ratio() {
        let tally = |s: EvolutionStrategy| {
            let mut c = (0, 0, 0);
            for seq in 0..10u64 {
                match mix_lookup(s, seq) {
                    RoundIntent::Repair => c.0 += 1,
                    RoundIntent::Optimize => c.1 += 1,
                    RoundIntent::Innovate => c.2 += 1,
                }
            }
            c
        };
        assert_eq!(tally(EvolutionStrategy::Balanced), (5, 3, 2));
        assert_eq!(tally(EvolutionStrategy::Innovate), (2, 3, 5));
        assert_eq!(tally(EvolutionStrategy::Harden), (4, 5, 1));
        assert_eq!(tally(EvolutionStrategy::RepairOnly), (10, 0, 0));
    }

    #[test]
    fn repair_only_with_no_material_skips_not_fabricates() {
        let barren = RoundMaterial { unresolved_mistakes: 0, low_streak_entries: 9, active_entries: 0 };
        assert_eq!(
            decide_intent(EvolutionStrategy::RepairOnly, AeeTrigger::ForcedReflection, 7, barren),
            IntentDecision::Skip(SkipReason::NoRepairMaterial)
        );
    }

    #[test]
    fn mistake_backlog_outranks_the_mix_but_not_an_innovate_agent() {
        let backlog = RoundMaterial { unresolved_mistakes: 5, low_streak_entries: 1, active_entries: 0 };
        // Slot 9 would be Innovate under balanced — the backlog wins.
        assert_eq!(
            decide_intent(EvolutionStrategy::Balanced, AeeTrigger::ForcedReflection, 9, backlog),
            IntentDecision::Run(RoundIntent::Repair)
        );
        // …but an explicitly innovation-biased agent keeps exploring.
        assert_eq!(
            decide_intent(EvolutionStrategy::Innovate, AeeTrigger::ForcedReflection, 9, backlog),
            IntentDecision::Run(RoundIntent::Innovate)
        );
    }

    #[test]
    fn stagnation_forces_innovate_avo_p8() {
        let backlog = RoundMaterial { unresolved_mistakes: 0, low_streak_entries: 4, active_entries: 0 };
        assert_eq!(
            decide_intent(EvolutionStrategy::Harden, AeeTrigger::Stagnation, 0, backlog),
            IntentDecision::Run(RoundIntent::Innovate),
            "when stuck, change direction — do not keep sanding the same spot"
        );
    }

    #[test]
    fn concrete_failure_triggers_force_repair() {
        for t in [AeeTrigger::ChannelReply, AeeTrigger::SubAgentDispatch] {
            assert_eq!(
                decide_intent(EvolutionStrategy::Balanced, t, 9, rich()),
                IntentDecision::Run(RoundIntent::Repair)
            );
        }
    }

    #[test]
    fn degradation_chain_repair_to_optimize_to_innovate_to_skip() {
        // No mistakes → Repair degrades to Optimize.
        let m1 = RoundMaterial { unresolved_mistakes: 0, low_streak_entries: 2, active_entries: 0 };
        assert_eq!(
            decide_intent(EvolutionStrategy::Balanced, AeeTrigger::ForcedReflection, 0, m1),
            IntentDecision::Run(RoundIntent::Optimize)
        );
        // …and no low-streak entries either → Innovate.
        let m2 = RoundMaterial { unresolved_mistakes: 0, low_streak_entries: 0, active_entries: 0 };
        assert_eq!(
            decide_intent(EvolutionStrategy::Balanced, AeeTrigger::ForcedReflection, 0, m2),
            IntentDecision::Run(RoundIntent::Innovate)
        );
        // …and the playbook is full → skip, honestly recorded.
        let m3 = RoundMaterial {
            unresolved_mistakes: 0,
            low_streak_entries: 0,
            active_entries: crate::playbook::PLAYBOOK_MAX_ENTRIES,
        };
        assert_eq!(
            decide_intent(EvolutionStrategy::Balanced, AeeTrigger::ForcedReflection, 0, m3),
            IntentDecision::Skip(SkipReason::PlaybookAtCapacity)
        );
    }

    #[test]
    fn strategy_reads_agent_toml_and_falls_back_loudly() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent.toml"), "[evolution]\nstrategy = \"harden\"\n").unwrap();
        assert_eq!(EvolutionStrategy::from_agent_dir(dir.path()), EvolutionStrategy::Harden);

        std::fs::write(dir.path().join("agent.toml"), "[evolution]\nstrategy = \"hardn\"\n").unwrap();
        assert_eq!(EvolutionStrategy::from_agent_dir(dir.path()), EvolutionStrategy::Balanced);

        let empty = tempfile::tempdir().unwrap();
        assert_eq!(EvolutionStrategy::from_agent_dir(empty.path()), EvolutionStrategy::Balanced);
    }

    #[test]
    fn strategy_round_trips_through_as_str() {
        for s in [
            EvolutionStrategy::Balanced,
            EvolutionStrategy::Innovate,
            EvolutionStrategy::Harden,
            EvolutionStrategy::RepairOnly,
        ] {
            assert_eq!(EvolutionStrategy::parse(s.as_str()), Some(s));
        }
    }
}
