//! O2: Dynamic sub-agent synthesis — ephemeral (Instruction, Context, Tools,
//! Model) four-tuple agents (AOrchestra, arXiv:2602.03786).
//!
//! Instead of delegating only to predefined agents, an orchestrating agent can
//! synthesize a purpose-built *ephemeral* sub-agent: a transient agent
//! directory scaffolded under `<home>/agents/.ephemeral/<eph-id>` with
//! - **Instruction** → `SOUL.md` (the synthesized system-prompt fragment),
//! - **Context** → the task payload dispatched through the existing bus,
//! - **Tools** → `[capabilities] allowed_tools` restricted to the requested
//!   subset (deny-by-default; see [`check_tool_subset`] — requested tools not
//!   inside the *parent* agent's own capability envelope are rejected, so
//!   synthesis can never escalate privileges),
//! - **Model** → a *tier* (`cheap` / `standard` / `preferred`) resolved through
//!   [`crate::delegation_router::tier_model`] against the parent's configured
//!   models (copied verbatim into the scaffold) — callers never pass a raw
//!   model id (multi-model doctrine).
//!
//! The `.ephemeral/` container directory has no `agent.toml`, so the
//! [`duduclaw_agent::registry::AgentRegistry`] scan skips it entirely —
//! ephemeral agents never pollute the registry, the heartbeat scheduler, or
//! the "Your Team" roster. Dispatch happens through the normal bus →
//! `dispatcher::dispatch_to_agent` path, which detects the `eph-` id and
//! routes here ([`dispatch`]); responses flow back through the unchanged
//! delegation-callback path.
//!
//! **Garbage collection** ([`sweep`]): hooked into the dispatcher's existing
//! ~1 hour maintenance tick (the same `tick % 720` slot that runs
//! `cleanup_stale_delegation_callbacks`) — no new scheduler. A scaffold is
//! removed when (a) its `.completed` marker is older than 1 h (grace window
//! for SQLite-queue retries / response forwarding), or (b) it is older than
//! the 24 h TTL regardless of completion. Every deletion first re-verifies
//! that the canonicalized path is strictly contained under the canonicalized
//! `.ephemeral/` root (symlinks are never followed — a symlink entry is
//! unlinked itself, its target untouched), so the sweeper can never delete
//! anything outside its namespace.
//!
//! # Team role members (Team-as-Agent P1/WP-2)
//!
//! The same scaffold carries the **role members** of a team ([decision A] in
//! `commercial/docs/DESIGN-team-as-agent-2026-09.md` §3.7): one transient agent
//! per `(task, round, role)`, each pinned to its own `(runtime, model, effort)`
//! so a team's planner can run on one vendor and its verifier on another.
//! Three properties keep that from weakening anything above:
//!
//! 1. **Raw model ids stay out of agent hands.** [`scaffold_role_member`] is
//!    the only entry that accepts a raw `(runtime, model)` pair, and it is
//!    *not* reachable from the MCP `spawn_ephemeral` tool — that tool still
//!    goes through [`parse_tier`], which accepts only the three tier keywords.
//!    Only the gateway's team composer calls the role path, so "the model does
//!    not pick its own model" survives intact.
//! 2. **The pair is validated, never guessed.** The runtime must be in
//!    [`duduclaw_core::types::TEAM_ROLE_RUNTIME_ALLOWLIST`] and the model must
//!    belong to a family that runtime actually serves
//!    ([`duduclaw_core::runtime_catalog`]) — an unknown family is an error, not
//!    a best guess (goose#10731).
//! 3. **Role members are garbage-collected at the round's terminal state**, by
//!    [`finish_role_member`], with no grace window and no waiting for the 24 h
//!    TTL. Three roles × five rounds × three in-flight tasks is 45 live
//!    scaffolds against a default `ephemeral_max_active` of 32, so per-round
//!    accumulation would overflow the cap on arithmetic alone (design §3.8).
//!    [`sweep`]'s policy for *ordinary* ephemeral agents is unchanged; a role
//!    member whose round died without calling `finish_role_member` is the one
//!    addition — it is collected as soon as its `.completed` marker exists.

use std::path::{Path, PathBuf};

use duduclaw_core::effort::Effort;
use duduclaw_core::types::{CapabilitiesConfig, Role, TEAM_ROLE_RUNTIME_ALLOWLIST};

use crate::delegation_router::{ModelTier, tier_model};

/// Directory (under `<home>/agents/`) holding ephemeral agent scaffolds.
/// Leading dot + no `agent.toml` inside ⇒ invisible to the registry scan.
pub const EPHEMERAL_DIR_NAME: &str = ".ephemeral";

/// Every ephemeral agent id starts with this prefix.
pub const EPHEMERAL_ID_PREFIX: &str = "eph-";

/// Hard TTL: scaffolds older than this are swept even if never completed.
pub const EPHEMERAL_TTL_HOURS: i64 = 24;

/// Grace window after completion before the scaffold is removed (lets the
/// SQLite stale-message sweeper retry and response forwarding settle).
pub const COMPLETED_GRACE_SECS: i64 = 3600;

/// Circuit breaker: refuse to synthesize beyond this many live scaffolds.
/// This is the DEFAULT cap; the effective cap is `[dispatch]
/// ephemeral_max_active` (see `duduclaw_core::spawn_admission`), which
/// defaults to this exact value so an absent/malformed config section is
/// byte-identical to the pre-H19 hardcoded behavior.
pub const MAX_ACTIVE_EPHEMERAL: usize = 32;

/// Admission-queue class label for ephemeral spawn requests (H19). Shared
/// between [`scaffold`]'s capacity check, [`drain_admission_queue`], and the
/// MCP `spawn_ephemeral` tool's queue-vs-fail branch.
pub const EPHEMERAL_ADMISSION_CLASS: &str = "ephemeral";

/// Stable, anchored prefix of the capacity-exceeded error returned by
/// [`scaffold`]. Callers use `starts_with` against this constant (never a
/// generic substring match on arbitrary error text — project convention #2)
/// to distinguish "temporarily out of capacity, safe to queue" from every
/// other rejection reason (privilege escalation, malformed parent, …), which
/// must NEVER be queued since retrying them can never succeed.
pub const EPHEMERAL_CAPACITY_ERROR_PREFIX: &str = "ephemeral agent limit reached";

/// `agent.toml` section that marks a scaffold as a **team role member** and
/// records which `(task, round, role)` it belongs to.
///
/// A dedicated section rather than `[agent] role`: [`AgentRole`] is an
/// *employee's* org role and has a `planner` variant but no `executor` /
/// `verifier` / `utility` ones, so three of the four team roles would have no
/// faithful label and the fourth would silently collide with an org concept
/// that drives delegation. `[agent] role` therefore stays `worker` for every
/// scaffold, exactly as before, and the team dimension lives here where nothing
/// else reads it by accident.
///
/// [`AgentRole`]: duduclaw_core::types::AgentRole
pub const ROLE_MEMBER_SECTION: &str = "team_member";

/// Longest parent id fragment carried inside a role-member id. The rest of the
/// id (prefix, round, role, random suffix) is bounded, so this is what keeps
/// the total inside [`duduclaw_core::is_valid_agent_id`]'s 64-character limit —
/// see [`new_role_member_id`].
const ROLE_MEMBER_PARENT_FRAGMENT_MAX: usize = 24;

/// Highest round number a role-member id can encode. Beyond this the id's
/// length budget is no longer provable, so the mint refuses instead of
/// truncating a number (a silently-wrong round in an id is worse than an
/// error — it is what the whole per-round attribution is keyed on).
const ROLE_MEMBER_ROUND_MAX: u32 = 9_999;

mod dispatch;
mod gc;
mod ids;
mod role_member;
mod scaffold;

#[cfg(test)]
mod tests;

/// What distinguishes one [`scaffold_with`] call from another: the id to mint
/// the directory under (`None` ⇒ a plain random [`new_ephemeral_id`]) and the
/// role-member overrides, when this scaffold is a team role member.
struct ScaffoldPlan<'a> {
    agent_id: Option<String>,
    role: Option<&'a RoleMemberSpec>,
}

pub use dispatch::{
    dispatch, dispatch_with, is_due_for_gc, read_meta, resolve_agent_dir, resolve_tier_model_for_dir,
};
pub use gc::{AdmissionDrainSummary, drain_admission_queue, sweep};
pub use ids::{
    EphemeralMeta, RoleMemberOutcome, RoleMemberRecord, RoleMemberSpec, TEAM_INTRINSIC_TOOLS,
    check_tool_subset, check_tool_subset_with_intrinsics, ephemeral_root, is_ephemeral_id,
    new_ephemeral_id, new_role_member_id, parse_tier,
};
pub use role_member::{
    RoleMemberAdmitted, admit_role_member, finish_role_member, has_role_member_marker,
    is_role_member, read_role_member, role_member_from_payload, scaffold_role_member,
};
pub use scaffold::{EphemeralSpawnSpec, ScaffoldResult, parent_capabilities, scaffold};

use ids::stable_role_soul;
#[cfg(test)]
use role_member::role_member_payload;
use scaffold::{active_count, scaffold_with, validate_role_runtime_model};
