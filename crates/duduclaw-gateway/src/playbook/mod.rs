//! Playbook — gene-shaped experience entries for the AEE evolution loop
//! (WP1.2 + WP1.3; GEP G1/G2/G3/G5/G6; `commercial/docs/DESIGN-evolution-v3-aee.md` §1).
//!
//! **Carrier decision (§1.1)**: a playbook entry is a `duduclaw-memory`
//! semantic-layer row (`source_event = `[`entry::PLAYBOOK_SOURCE_EVENT`])
//! whose gene-shaped fields live in the row's `metadata` JSON blob under the
//! [`entry::PLAYBOOK_KEY`] key, beside (never replacing) the `rule_stats`
//! key `crate::prediction::rule_lifecycle` already maintains. No new
//! storage engine, no new SQL migration.
//!
//! Module map:
//! - [`entry`] — the `PlaybookMeta` schema + constants (§1.2)
//! - [`humanize`] — W3-2 zero-LLM zh-TW rewrite layer turning a stored entry
//!   into 「經驗法則」 copy for the dashboard, `/rules`, and the daily digest
//! - [`signals`] — the `signals_match` vocabulary, `TurnSignals` assembly,
//!   and matching (§1.3)
//! - [`gene`] — lossless GEP-gene-shaped JSON export/import (§1.4, D5=B)
//! - [`delta`] — the delta op set + deterministic zero-LLM merge (§1.5, ACE)
//! - [`dedup`] — two-level (literal + optional semantic) deduplication (§1.6)
//! - [`store`] — CRUD orchestration over the existing memory engine API (no
//!   new SQL)
//! - [`select`] — injection ranking + [`select::InjectionBudget`] (§1.8, WP1.3)
//! - [`sweep`] — stale/capacity lifecycle sweep, reusing Ebbinghaus
//!   retrievability (§1.7, G5)
//!
//! What this module deliberately does NOT do (over-engineering guard, see
//! the planning doc's risk table): `strategy` steps stay empty in phase 1;
//! `Regulatory`/`Explore` categories exist only as reserved enum variants;
//! no evolver code or external gene-hub client is vendored (D5=B: local
//! schema alignment + lossless export only, no hub I/O).

pub mod assertions;
pub mod dedup;
pub mod delta;
pub mod entry;
pub mod gene;
pub mod humanize;
pub mod select;
pub mod signals;
pub mod store;
pub mod sweep;

#[cfg(test)]
mod tests;

pub use delta::{merge, validate_all, validate_delta, AppliedOp, MergeNote, MergeOutcome, PlaybookDelta, ValidationCtx};
pub use entry::{
    Application, FailureNote, PlaybookCategory, PlaybookMeta, PlaybookState,
    LEGACY_RULE_SOURCE_EVENT, PLAYBOOK_KEY, PLAYBOOK_MAX_ENTRIES, PLAYBOOK_SCHEMA_VERSION,
    PLAYBOOK_SOURCE_EVENT, SIGNALS_MAX, WILDCARD_QUOTA,
};
pub use gene::{from_gene, to_gene, EvalCaseRef};
pub use humanize::{humanize, HumanizedRule, RuleEvidence};
pub use select::{
    build_playbook_section_blocking, collect_armed_shadow, collect_armed_shadow_blocking,
    render_section, select_playbook, ArmedShadow, InjectionBudget, SelectedEntry,
};
pub use signals::TurnSignals;
pub use store::{apply_deltas, list_active};
pub use sweep::{run_playbook_sweep, spawn_playbook_sweep_loop};
