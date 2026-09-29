//! `role_turns.jsonl` — one append-only row per team **role member stage**
//! (Team-as-Agent P1/WP-4, design `DESIGN-team-as-agent-2026-09.md` §3.10).
//!
//! ## Why a new ledger rather than a column
//!
//! Every attribution surface this workspace has — `token_usage`, `mistakes`,
//! playbook `rule_stats`, the eval report — keys on **agent id**. When one
//! employee's task is worked by a planner on Claude, an executor on Codex and
//! a verifier on Gemini, "which agent" no longer identifies who did what, and
//! an evolution engine that cannot tell those three apart learns rules from
//! the wrong evidence (arXiv:2607.28802). This file is the missing dimension:
//! `(task_id, round, role, runtime, model)` for every member turn, written at
//! the moment the stage ends, by the composer that knows the answer — never
//! reconstructed afterwards from prose.
//!
//! ## Conventions borrowed verbatim from `tool_calls.jsonl`
//!
//! * **Cross-process append under an advisory lock**, via
//!   [`duduclaw_security::audit_chain::append_chained_line`] — the same
//!   primitive `tool_calls.jsonl` uses, so rows from the gateway and from any
//!   other process interleave safely and each line carries a `_prev_hash`
//!   chain link.
//! * **0600 on create**, tightened on drift: a row names models, task ids and
//!   error strings.
//! * **Size-capped rotation** to `role_turns.jsonl.old`
//!   ([`ROLE_TURNS_ROTATION_MAX_BYTES`]).
//!
//! ## What is deliberately NOT here
//!
//! The design's §3.10 field list is longer than this row: `trace_id` /
//! `span_id` (OTel span plumbing), `mast_mode` (P4's attribution chain),
//! `role_shapley_phi` (offline
//! backfill), `counterfactual_checked`. Writing those fields now with values
//! nothing computes would produce a schema that *looks* populated — the exact
//! failure mode the design warns about. They arrive with the code that can
//! fill them.
//!
//! Every failure in this module is non-fatal: a stage that ran must never be
//! undone because its telemetry row could not be written.

use std::path::{Path, PathBuf};

use duduclaw_core::types::Role;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Ledger file name, under `<home>/`.
pub const ROLE_TURNS_FILE: &str = "role_turns.jsonl";

/// Rotate at 16 MB, matching [`duduclaw_security::audit::TOOL_CALLS_ROTATION_MAX_BYTES`].
/// A role turn row is small and bounded (no free-text result capture), so this
/// retains a very long history; the cap exists so the file cannot grow without
/// limit, not because rows are expected to be large.
pub const ROLE_TURNS_ROTATION_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Cap on a stored `error_type` string. A closed token is preferred (see
/// [`RoleTurnRow::error_type`]) but a runtime error string is sometimes the
/// only thing available, and it must not balloon a row.
pub const ERROR_TYPE_MAX_CHARS: usize = 200;

/// How a member stage ended. Closed set with stable tokens — never
/// `format!("{:?}")`, which has already produced one silently-wrong column in
/// this workspace (`McpOnly` → `"mcponly"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleTurnOutcome {
    /// The member ran and produced its expected product (a packet, or a
    /// verdict for the verifier).
    Completed,
    /// The member ran but produced nothing usable (no packet, empty answer).
    Empty,
    /// The stage could not run (scaffold refused, capacity, guard trip, spawn
    /// error). `error_type` says which.
    Failed,
    /// The stage was deliberately not run by the budget degrade chain. Not a
    /// failure: a `skipped` utility stage is the system working as designed.
    Skipped,
}

/// The stage boundary at which a failed member was observed. It locates the
/// symptom, not necessarily the responsible role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureEdge {
    PlannerExecutor,
    ExecutorVerifier,
    VerifierGate,
    RuntimeCli,
    HarnessPrompt,
}

impl FailureEdge {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlannerExecutor => "planner_executor",
            Self::ExecutorVerifier => "executor_verifier",
            Self::VerifierGate => "verifier_gate",
            Self::RuntimeCli => "runtime_cli",
            Self::HarnessPrompt => "harness_prompt",
        }
    }
}

impl RoleTurnOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleTurnOutcome::Completed => "completed",
            RoleTurnOutcome::Empty => "empty",
            RoleTurnOutcome::Failed => "failed",
            RoleTurnOutcome::Skipped => "skipped",
        }
    }
}

/// Token usage for one member stage, when the runtime reported any.
///
/// Every field is `Option`: a CLI-backed runtime that returns no usage block
/// leaves them `None`. `Some(0)` would claim "measured, and it was free",
/// which is a different — and false — statement.
///
/// **A stage can have several answering legs** (an openai-compat tool loop, a
/// `failover.rs` primary→fallback chain), and this type is the *stage* total —
/// see [`RoleTurnUsage::accumulate`] and [`usage_legs`](Self::usage_legs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleTurnUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_cache_read_tokens: Option<u64>,
    /// How many answering legs contributed to the totals above. `None` ⇒ no
    /// leg ever reported usage (the file's pre-2026-09-28 shape, and the shape
    /// of every stage on a runtime that reports no usage block at all).
    ///
    /// `> 1` is the signal that this stage was NOT one call: a reader
    /// comparing this row against `cost_telemetry` can see why the two counts
    /// of calls differ without inferring it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_legs: Option<u32>,
}

impl RoleTurnUsage {
    pub fn is_empty(&self) -> bool {
        *self == RoleTurnUsage::default()
    }

    /// Fold one answered leg into this stage's totals.
    ///
    /// 2026-09-28 review (`review_team.md` §3 "授權／證據"): the task-local
    /// sink behind this was last-writer-wins, so a stage that failed over — or
    /// ran a multi-call tool loop — published only its LAST leg here while
    /// `cost_telemetry` recorded every call. Two ledgers, same stage, different
    /// numbers, and [`RoleTurnRow::usage`]'s own doc says "one member stage".
    ///
    /// Per-dimension rules, all of them about not inventing measurements:
    /// * `None + None = None` — nothing was measured, and `Some(0)` would
    ///   claim it was measured at zero.
    /// * `Some(a) + None = Some(a)` — a leg that reported no cache read did
    ///   not report a cache read of zero, so it neither adds nor erases.
    /// * `Some(a) + Some(b) = Some(a + b)`, saturating (a u64 overflow here
    ///   would be a corrupt reading, never a real bill).
    ///
    /// An **empty** leg is a no-op, `usage_legs` included: a runtime that
    /// reported nothing is not evidence that a leg ran.
    pub fn accumulate(&mut self, leg: RoleTurnUsage) {
        if leg.is_empty() {
            return;
        }
        fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (None, None) => None,
                (Some(x), None) => Some(x),
                (None, Some(y)) => Some(y),
                (Some(x), Some(y)) => Some(x.saturating_add(y)),
            }
        }
        self.usage_input_tokens = add(self.usage_input_tokens, leg.usage_input_tokens);
        self.usage_output_tokens = add(self.usage_output_tokens, leg.usage_output_tokens);
        self.usage_cache_read_tokens =
            add(self.usage_cache_read_tokens, leg.usage_cache_read_tokens);
        self.usage_legs = Some(
            self.usage_legs
                .unwrap_or(0)
                .saturating_add(leg.usage_legs.unwrap_or(1)),
        );
    }
}

/// One row of `role_turns.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleTurnRow {
    /// RFC3339, stamped when the row is written (stage end).
    pub timestamp: String,
    /// A2A `Task.id` — the goal task, shared with `task_iterations.task_id`.
    pub task_id: String,
    /// The employee the team belongs to. Present so a reader can aggregate by
    /// employee without joining anything.
    pub agent_id: String,
    /// Goal-loop round, shared with `task_iterations.round`.
    pub round: u32,
    pub role: Role,
    /// Ephemeral member id (`eph-…`), or a synthetic label for a stage that
    /// runs without a scaffold (the verifier, which is a utility call).
    pub member_id: String,
    /// Canonical runtime catalog id the stage was configured to run on.
    pub runtime: String,
    /// The provider that actually answered. Equal to `runtime` unless a
    /// degrade/failover substituted one — recorded separately precisely so
    /// that substitution is visible rather than inferred.
    pub provider: String,
    /// Model id as requested. `None` ⇒ the role cascaded to the employee's
    /// own `[model] preferred` and the composer did not resolve it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_model: Option<String>,
    /// The canonical runtime id that ACTUALLY answered, as reported by the
    /// execution path itself (`crate::runtime::RuntimeOutcome`) rather than
    /// by the configuration. `None` ⇒ nothing reported one (a stage that
    /// never spawned, or a path with no reporter).
    ///
    /// Live round 3 E2 is why this exists: an executor configured for codex
    /// failed to spawn, silently failed over to Claude, did the work as
    /// Claude — and this ledger recorded `runtime=codex … completed`. The
    /// `runtime` field above is the *request*; this is the *answer*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_used: Option<String>,
    /// The model that actually served the call — the substituted one after a
    /// failover, not [`Self::request_model`]. `None` like `runtime_used`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// `true` when [`Self::runtime_used`] is known and differs from
    /// [`Self::runtime`]. Deliberately runtime-level: a model substitution
    /// within the same runtime is already visible as
    /// `request_model` ≠ `response_model`, whereas a *runtime* change is the
    /// one that silently swaps which model family's blind spots the answer
    /// inherits. `false` when nothing was reported — never a guess.
    #[serde(default)]
    pub failover: bool,
    /// Requested reasoning effort (`low`/…/`max`), as the canonical
    /// lowercase token. `None` ⇒ not specified for this role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Path of the [`duduclaw_core::task_packet::TaskPacket`] this stage
    /// produced, relative to `<home>`. `None` for a stage that produces no
    /// packet (verifier) or produced none (failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_path: Option<String>,
    /// Evidence grade of this stage's observations — the
    /// [`duduclaw_core::task_packet::Fidelity`] token
    /// (`full` / `mcp_only` / `none`). Never conflated: a verifier that
    /// cannot tell `none` from `mcp_only` reads "no tool calls recorded" as
    /// "no tool calls made".
    pub observation_fidelity: String,
    pub outcome: RoleTurnOutcome,
    /// Short stable classification of a failure, or the (truncated) error
    /// text when no closed token applies. `None` on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// Written at stage end from the closed outcome and error tokens. Absent
    /// on completed/skipped stages; a location is not a blame assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_edge: Option<FailureEdge>,
    /// Unknown is deliberate whenever the evidence cannot establish a side.
    /// In particular `observation_fidelity=none` can never blame a model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_side: Option<crate::fault_attribution::FaultSide>,
    #[serde(default, flatten, skip_serializing_if = "RoleTurnUsage::is_empty")]
    pub usage: RoleTurnUsage,
    /// sha8 over the configuration that decides *what this stage is*:
    /// `runtime|model` today, `runtime|model|playbook_hash` once a whole-
    /// playbook hash is reachable from here (there is no such function in the
    /// workspace yet — see [`config_fingerprint_hard`]). A change in this
    /// value invalidates evidence collected under the old one (design R7).
    pub config_fingerprint_hard: String,
}

impl RoleTurnRow {
    /// A row for a stage that never got to run. `runtime`/`provider` are the
    /// configured ones; fidelity is `none`, which is honest — nothing was
    /// observed.
    pub fn refused(
        task_id: &str,
        agent_id: &str,
        round: u32,
        role: Role,
        runtime: &str,
        model: Option<&str>,
        outcome: RoleTurnOutcome,
        error_type: &str,
    ) -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            task_id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            round,
            role,
            member_id: String::new(),
            runtime: runtime.to_string(),
            provider: runtime.to_string(),
            request_model: model.map(str::to_string),
            // A stage that never ran has no answering runtime to report.
            runtime_used: None,
            response_model: None,
            failover: false,
            effort: None,
            packet_path: None,
            observation_fidelity: duduclaw_core::task_packet::Fidelity::None
                .as_str()
                .to_string(),
            outcome,
            error_type: Some(duduclaw_core::truncate_chars(
                error_type,
                ERROR_TYPE_MAX_CHARS,
            )),
            failure_edge: None,
            fault_side: None,
            usage: RoleTurnUsage::default(),
            config_fingerprint_hard: config_fingerprint_hard(runtime, model),
        }
    }
}

/// sha8 of the hard configuration fingerprint for a `(runtime, model)` pair.
///
/// The design's definition is `runtime + model + playbook hash`. No
/// whole-playbook hash function is reachable from the gateway today (the AEE
/// champion snapshot hashes a playbook but does not expose it), so this
/// computes the two-part fingerprint and the doc says so rather than shipping
/// a field that silently omits a third of its definition.
pub fn config_fingerprint_hard(runtime: &str, model: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(runtime.as_bytes());
    hasher.update(b"|");
    hasher.update(model.unwrap_or("").as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)[..8].to_string()
}

/// Absolute path of the ledger for a home dir.
pub fn ledger_path(home_dir: &Path) -> PathBuf {
    home_dir.join(ROLE_TURNS_FILE)
}

fn failure_edge(row: &RoleTurnRow) -> Option<FailureEdge> {
    if matches!(
        row.outcome,
        RoleTurnOutcome::Completed | RoleTurnOutcome::Skipped
    ) {
        return None;
    }
    if row.error_type.as_deref() == Some("dispatch_error") {
        return Some(FailureEdge::RuntimeCli);
    }
    Some(match row.role {
        Role::Planner => FailureEdge::PlannerExecutor,
        Role::Executor => FailureEdge::ExecutorVerifier,
        Role::Verifier => FailureEdge::VerifierGate,
        Role::Utility => FailureEdge::HarnessPrompt,
    })
}

fn fault_side(row: &RoleTurnRow) -> Option<crate::fault_attribution::FaultSide> {
    use crate::fault_attribution::FaultSide;
    if matches!(
        row.outcome,
        RoleTurnOutcome::Completed | RoleTurnOutcome::Skipped
    ) {
        return None;
    }
    if !matches!(row.observation_fidelity.as_str(), "full" | "mcp_only") {
        return Some(FaultSide::Unknown);
    }
    // Only closed infrastructure tokens can establish environment fault.
    // Free-text runtime errors, missing packets and a failed verifier may
    // have several causes; never turn them into model-learning samples.
    match row.error_type.as_deref() {
        Some(
            "rate_limited" | "billing" | "timeout" | "binary_missing" | "spawn_error"
            | "no_accounts",
        ) => Some(FaultSide::Environment),
        _ => Some(FaultSide::Unknown),
    }
}

/// Append one row. Best-effort: a write failure warns and returns — a member
/// stage that actually ran is never rolled back because its row could not be
/// persisted.
pub fn append_row(home_dir: &Path, row: &RoleTurnRow) {
    let path = ledger_path(home_dir);
    maybe_rotate(&path);
    let mut attributed = row.clone();
    attributed.failure_edge = failure_edge(&attributed);
    attributed.fault_side = fault_side(&attributed);
    let value = match serde_json::to_value(&attributed) {
        Ok(serde_json::Value::Object(map)) => map,
        Ok(other) => {
            warn!("role_turns row did not serialize to an object ({other:?}) — row dropped");
            return;
        }
        Err(e) => {
            warn!("role_turns row serialization failed: {e} — row dropped");
            return;
        }
    };
    if let Err(e) = duduclaw_security::audit_chain::append_chained_line(&path, value, Some(0o600)) {
        warn!("failed to append role turn to {}: {e}", path.display());
    }
}

/// Rotate the ledger when it exceeds [`ROLE_TURNS_ROTATION_MAX_BYTES`].
///
/// Unlike `tool_calls.jsonl`'s every-64-calls sampling, this checks on every
/// append: role turns arrive a handful per goal round (not per tool call), so
/// one `metadata()` syscall per row is free, and sampling would let the file
/// overshoot by however many rows a sampling window happens to hold.
///
/// A concurrent rotator's `rename` winning first yields `NotFound` here,
/// which is ignored — the next append recreates the file.
fn maybe_rotate(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= ROLE_TURNS_ROTATION_MAX_BYTES {
        return;
    }
    let backup = path.with_extension("jsonl.old");
    match std::fs::rename(path, &backup) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("failed to rotate {}: {e}", path.display()),
    }
}

/// Every row recorded for one goal task, oldest first.
///
/// Reads the most recent rotated ledger (`.jsonl.old`) **and then** the live
/// one, so a task whose rows straddle a rotation is not silently cut in half.
///
/// This used to read the live ledger only, on the reasoning that a
/// rotated-away history is out of scope for a "show me this task's role turns"
/// view. Review follow-up (`team_composer.rs:1094` + `role_turns.rs:367,389`):
/// it is not only a view. [`crate::team_composer::spawns_used_for_task`]
/// counts these rows to charge the team spawn budget, so a rotation in the
/// middle of a live task silently reset its spent budget to zero — directly
/// contradicting that function's own promise that "the budget survives a
/// gateway restart", and handing the task an unbounded number of extra rounds.
///
/// Only **one** generation back: [`maybe_rotate`] keeps exactly one `.old`
/// (each rotation overwrites it), so there is no deeper history to find. The
/// two files' hash chains restart at the boundary, which is why this is a
/// concatenation for counting and reading, never a claim that the chain is
/// continuous — nothing here verifies the chain, and any chain verification
/// stays per-file.
///
/// Unparseable lines are skipped, not guessed at; a missing file on either
/// side yields nothing from that side rather than an error.
pub fn read_rows_for_task(home_dir: &Path, task_id: &str) -> Vec<RoleTurnRow> {
    let live = ledger_path(home_dir);
    let rotated = live.with_extension("jsonl.old");
    let mut rows = Vec::new();
    // Oldest first: the rotated file holds everything that came before the
    // live one, by construction.
    for path in [rotated, live] {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        rows.extend(
            content
                .lines()
                .filter_map(|line| serde_json::from_str::<RoleTurnRow>(line).ok())
                .filter(|row: &RoleTurnRow| row.task_id == task_id),
        );
    }
    rows
}

/// The role members that ran for one `(task, round)`, oldest first, with
/// duplicates collapsed.
///
/// Live round 3 E3: the verifier and the settle path both read tool evidence
/// scoped to ONE agent id (the employee). A team's work is done by ephemeral
/// members under their own ids, so the employee's `tool_calls.jsonl` window
/// was empty and both correctly concluded "no tool activity supports this
/// claim". Callers union this with the employee id to ask about the whole
/// team instead.
///
/// Rows with an empty `member_id` are refusals that never reached a scaffold
/// and produced no audit trail, so they are skipped.
pub fn member_ids_for_task_round(home_dir: &Path, task_id: &str, round: u32) -> Vec<String> {
    dedup_member_ids(
        read_rows_for_task(home_dir, task_id)
            .into_iter()
            .filter(|r| r.round == round),
    )
}

/// [`member_ids_for_task_round`] across every round of the task.
///
/// The goal loop's in-flight `iter` counter and the settle path's
/// `revision_round + 1` are maintained separately and can diverge (a gateway
/// restart re-seeds `iter` from zero while `revision_round` persists), so a
/// settle that finds no members for its own round falls back to this.
///
/// **Not safe to widen on a team task.** The pre-fix rationale was "every
/// consumer additionally filters by the task's claim→review time window, which
/// already excludes members from other rounds" — but a team task is never
/// *claimed* (`goal_loop::try_team_dispatch` completes it as `team-composer`),
/// so its `claimed_at` is `None` and that window silently widened to
/// `created_at`, i.e. the whole task. Two widenings stacked, and round 1's
/// evidence could vouch for round 3. Team callers use
/// [`member_ids_for_task_since`] with a window anchored on the round's own
/// start instead.
pub fn member_ids_for_task(home_dir: &Path, task_id: &str) -> Vec<String> {
    dedup_member_ids(read_rows_for_task(home_dir, task_id).into_iter())
}

/// The role members of one task whose stage row was written at or after
/// `since` (RFC3339), oldest first, duplicates collapsed.
///
/// The round-scoped [`member_ids_for_task_round`] is the primary lookup; this
/// is the fallback for the counter-divergence case, narrowed by the caller's
/// real evidence window so it can never reach into another round. A `since`
/// that does not parse, or a row whose `timestamp` does not parse, is kept —
/// dropping a member because a clock string is malformed would hide evidence,
/// which is the opposite of the failure mode this guards.
pub fn member_ids_for_task_since(home_dir: &Path, task_id: &str, since: &str) -> Vec<String> {
    let Ok(cutoff) = chrono::DateTime::parse_from_rfc3339(since) else {
        return member_ids_for_task(home_dir, task_id);
    };
    let rows = read_rows_for_task(home_dir, task_id);
    dedup_member_ids(rows.into_iter().filter(|r| {
        // An unparseable row timestamp is KEPT: dropping a member because a
        // clock string is malformed would hide evidence, which is the opposite
        // of the failure mode this narrowing guards against.
        match chrono::DateTime::parse_from_rfc3339(&r.timestamp) {
            Ok(ts) => ts >= cutoff,
            Err(_) => true,
        }
    }))
}

/// The earliest ledger timestamp recorded for one `(task, round)`.
///
/// The durable, round-scoped second source for "when did this round start"
/// when `task_iterations.dispatched_at` is unavailable (a gateway that
/// restarted between dispatch and settle, a caller with no store handle).
/// Rows are stamped at **stage end**, so this is an upper bound on the round's
/// true start: a planner-bearing round yields the planner's stage end, which
/// still precedes every executor tool call. A round with no planner yields the
/// executor's own stage end, which is *after* its work — the window then reads
/// as "no evidence", the fail-closed direction, rather than reaching back into
/// an earlier round.
pub fn round_started_at(home_dir: &Path, task_id: &str, round: u32) -> Option<String> {
    read_rows_for_task(home_dir, task_id)
        .into_iter()
        .filter(|r| r.round == round)
        .map(|r| r.timestamp)
        .min()
}

fn dedup_member_ids(rows: impl Iterator<Item = RoleTurnRow>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let id = row.member_id.trim();
        if id.is_empty() {
            continue;
        }
        if seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(task: &str, round: u32, role: Role) -> RoleTurnRow {
        RoleTurnRow {
            timestamp: "2026-09-24T10:00:00Z".to_string(),
            task_id: task.to_string(),
            agent_id: "agnes".to_string(),
            round,
            role,
            member_id: "eph-abc123".to_string(),
            runtime: "codex".to_string(),
            provider: "codex".to_string(),
            request_model: Some("gpt-5.5".to_string()),
            runtime_used: Some("codex".to_string()),
            response_model: Some("gpt-5.5".to_string()),
            failover: false,
            effort: Some("medium".to_string()),
            packet_path: Some("team_packets/t1/r1/planner-to-executor.json".to_string()),
            observation_fidelity: "mcp_only".to_string(),
            outcome: RoleTurnOutcome::Completed,
            error_type: None,
            failure_edge: None,
            fault_side: None,
            usage: RoleTurnUsage {
                usage_input_tokens: Some(1200),
                usage_output_tokens: Some(340),
                usage_cache_read_tokens: None,
                usage_legs: Some(1),
            },
            config_fingerprint_hard: config_fingerprint_hard("codex", Some("gpt-5.5")),
        }
    }

    #[test]
    fn row_shape_round_trips_and_uses_stable_tokens() {
        let r = row("t1", 1, Role::Executor);
        let json = serde_json::to_value(&r).unwrap();
        // Role and outcome are lowercase tokens, usage is flattened.
        assert_eq!(json["role"], "executor");
        assert_eq!(json["outcome"], "completed");
        assert_eq!(json["usage_input_tokens"], 1200);
        assert!(
            json.get("usage_cache_read_tokens").is_none(),
            "an unmeasured usage field must be absent, not 0"
        );
        let back: RoleTurnRow = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn failed_role_rows_record_edge_without_guessing_model_blame() {
        let mut r = row("t1", 1, Role::Executor);
        r.outcome = RoleTurnOutcome::Empty;
        r.error_type = Some("missing_packet".into());
        assert_eq!(failure_edge(&r), Some(FailureEdge::ExecutorVerifier));
        assert_eq!(
            fault_side(&r),
            Some(crate::fault_attribution::FaultSide::Unknown)
        );

        r.outcome = RoleTurnOutcome::Failed;
        r.error_type = Some("timeout".into());
        assert_eq!(
            fault_side(&r),
            Some(crate::fault_attribution::FaultSide::Environment)
        );
        r.observation_fidelity = "none".into();
        assert_eq!(
            fault_side(&r),
            Some(crate::fault_attribution::FaultSide::Unknown)
        );
        r.observation_fidelity = "future_fidelity".into();
        assert_eq!(
            fault_side(&r),
            Some(crate::fault_attribution::FaultSide::Unknown)
        );

        let home = tempfile::tempdir().unwrap();
        append_row(home.path(), &r);
        let stored = read_rows_for_task(home.path(), "t1");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].failure_edge, Some(FailureEdge::ExecutorVerifier));
        assert_eq!(
            stored[0].fault_side,
            Some(crate::fault_attribution::FaultSide::Unknown)
        );
    }

    #[test]
    fn absent_usage_is_omitted_entirely() {
        let mut r = row("t1", 1, Role::Verifier);
        r.usage = RoleTurnUsage::default();
        let json = serde_json::to_value(&r).unwrap();
        for k in [
            "usage_input_tokens",
            "usage_output_tokens",
            "usage_cache_read_tokens",
            "usage_legs",
        ] {
            assert!(json.get(k).is_none(), "{k} must be omitted when unmeasured");
        }
    }

    /// Regression (2026-09-28 review, `review_team.md` §3 "授權／證據"): a
    /// member stage with several answering legs published only its last leg's
    /// usage, disagreeing with `cost_telemetry` about the same stage.
    #[test]
    fn usage_accumulates_across_legs_without_inventing_measurements() {
        let mut total = RoleTurnUsage::default();
        // Leg 1 — primary attempt, no cache-read block at all.
        total.accumulate(RoleTurnUsage {
            usage_input_tokens: Some(1000),
            usage_output_tokens: Some(200),
            ..Default::default()
        });
        // Leg 2 — the fallback that actually answered.
        total.accumulate(RoleTurnUsage {
            usage_input_tokens: Some(500),
            usage_output_tokens: Some(50),
            usage_cache_read_tokens: Some(900),
            ..Default::default()
        });
        assert_eq!(total.usage_input_tokens, Some(1500));
        assert_eq!(total.usage_output_tokens, Some(250));
        // Measured by one leg only ⇒ that leg's number, never `Some(0)` for
        // the leg that never reported the dimension.
        assert_eq!(total.usage_cache_read_tokens, Some(900));
        assert_eq!(total.usage_legs, Some(2));

        // An empty leg is not a leg.
        total.accumulate(RoleTurnUsage::default());
        assert_eq!(total.usage_legs, Some(2));
        assert_eq!(total.usage_input_tokens, Some(1500));

        // Nothing measured anywhere stays wholly absent on the wire.
        let mut none = RoleTurnUsage::default();
        none.accumulate(RoleTurnUsage::default());
        assert!(none.is_empty());

        // Saturating, never wrapping: a corrupt reading cannot become a small
        // plausible-looking number.
        let mut big = RoleTurnUsage {
            usage_input_tokens: Some(u64::MAX),
            ..Default::default()
        };
        big.accumulate(RoleTurnUsage {
            usage_input_tokens: Some(10),
            ..Default::default()
        });
        assert_eq!(big.usage_input_tokens, Some(u64::MAX));
    }

    /// A row from before `usage_legs` existed still parses, and its absence
    /// stays an absence rather than becoming `0` legs.
    #[test]
    fn a_pre_usage_legs_row_still_parses() {
        let mut json = serde_json::to_value(row("t1", 1, Role::Executor)).unwrap();
        json.as_object_mut().unwrap().remove("usage_legs");
        let back: RoleTurnRow = serde_json::from_value(json).unwrap();
        assert_eq!(back.usage.usage_legs, None);
        assert_eq!(back.usage.usage_input_tokens, Some(1200));
    }

    #[test]
    fn every_outcome_token_is_snake_case_and_distinct() {
        let all = [
            RoleTurnOutcome::Completed,
            RoleTurnOutcome::Empty,
            RoleTurnOutcome::Failed,
            RoleTurnOutcome::Skipped,
        ];
        let tokens: Vec<&str> = all.iter().map(|o| o.as_str()).collect();
        assert_eq!(tokens, ["completed", "empty", "failed", "skipped"]);
        for (o, t) in all.iter().zip(tokens) {
            assert_eq!(
                serde_json::to_value(o).unwrap(),
                serde_json::Value::String(t.to_string()),
                "serde token must equal as_str()"
            );
        }
    }

    #[test]
    fn refused_row_is_honest_about_having_observed_nothing() {
        let r = RoleTurnRow::refused(
            "t1",
            "agnes",
            2,
            Role::Planner,
            "claude",
            Some("claude-fable-5-1"),
            RoleTurnOutcome::Failed,
            "capacity",
        );
        assert_eq!(r.observation_fidelity, "none");
        assert!(r.packet_path.is_none());
        assert!(r.usage.is_empty());
        assert_eq!(r.error_type.as_deref(), Some("capacity"));
        assert_eq!(r.provider, r.runtime);
    }

    #[test]
    fn refused_row_truncates_a_long_error_cjk_safely() {
        let long = "資料庫連線失敗".repeat(200);
        let r = RoleTurnRow::refused(
            "t1",
            "agnes",
            1,
            Role::Executor,
            "codex",
            None,
            RoleTurnOutcome::Failed,
            &long,
        );
        let stored = r.error_type.unwrap();
        assert_eq!(stored.chars().count(), ERROR_TYPE_MAX_CHARS);
    }

    #[test]
    fn fingerprint_is_8_hex_and_separates_runtime_from_model() {
        let a = config_fingerprint_hard("codex", Some("gpt-5.5"));
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Changing either half changes the fingerprint, and the `|` separator
        // keeps ("ab", "c") distinct from ("a", "bc").
        assert_ne!(a, config_fingerprint_hard("codex", Some("gpt-5.6")));
        assert_ne!(a, config_fingerprint_hard("claude", Some("gpt-5.5")));
        assert_ne!(
            config_fingerprint_hard("ab", Some("c")),
            config_fingerprint_hard("a", Some("bc"))
        );
    }

    #[test]
    fn append_then_read_filters_by_task_and_preserves_order() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        append_row(home, &row("t1", 1, Role::Planner));
        append_row(home, &row("t2", 1, Role::Planner));
        append_row(home, &row("t1", 1, Role::Executor));
        append_row(home, &row("t1", 2, Role::Verifier));

        let rows = read_rows_for_task(home, "t1");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|r| r.role).collect::<Vec<_>>(),
            vec![Role::Planner, Role::Executor, Role::Verifier],
            "rows must read back oldest-first"
        );
        assert_eq!(read_rows_for_task(home, "t2").len(), 1);
        assert!(read_rows_for_task(home, "nope").is_empty());
    }

    /// Regression (review `role_turns.rs:367,389`): the ledger rotates at
    /// 16 MiB and the reader looked only at the live file, so a task whose
    /// rows straddled a rotation read back half its history — and, since
    /// `team_composer::spawns_used_for_task` counts these rows, silently got
    /// its spent spawn budget reset to zero mid-task.
    #[test]
    fn regression_rows_in_the_rotated_ledger_are_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        append_row(home, &row("t1", 1, Role::Planner));
        append_row(home, &row("t1", 1, Role::Executor));

        // Exactly what `maybe_rotate` does at the size cap.
        let live = ledger_path(home);
        std::fs::rename(&live, live.with_extension("jsonl.old")).unwrap();
        append_row(home, &row("t1", 2, Role::Verifier));

        let rows = read_rows_for_task(home, "t1");
        assert_eq!(
            rows.iter().map(|r| r.role).collect::<Vec<_>>(),
            vec![Role::Planner, Role::Executor, Role::Verifier],
            "the rotated generation comes first, then the live one"
        );
        assert_eq!(
            member_ids_for_task_round(home, "t1", 1),
            vec!["eph-abc123".to_string()],
            "round 1 lives entirely in the rotated file — the evidence union \
             that reads it must not come back empty"
        );
    }

    #[test]
    fn read_on_a_missing_ledger_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_rows_for_task(dir.path(), "t1").is_empty());
    }

    #[test]
    fn a_corrupt_line_is_skipped_without_losing_the_good_ones() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        append_row(home, &row("t1", 1, Role::Planner));
        // A truncated / foreign line, as a crashed writer or a foreign tool
        // might leave behind.
        std::fs::write(
            ledger_path(home),
            format!(
                "{}\n{{\"task_id\":\"t1\",\n",
                std::fs::read_to_string(ledger_path(home)).unwrap().trim()
            ),
        )
        .unwrap();
        append_row(home, &row("t1", 1, Role::Executor));
        let rows = read_rows_for_task(home, "t1");
        assert_eq!(rows.len(), 2, "only the malformed middle line is skipped");
    }

    #[cfg(unix)]
    #[test]
    fn ledger_is_created_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        append_row(dir.path(), &row("t1", 1, Role::Planner));
        let mode = std::fs::metadata(ledger_path(dir.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "got {mode:#o}");
    }

    #[test]
    fn rotation_moves_the_file_aside_once_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = ledger_path(home);
        std::fs::write(
            &path,
            vec![b'x'; (ROLE_TURNS_ROTATION_MAX_BYTES + 1) as usize],
        )
        .unwrap();
        append_row(home, &row("t1", 1, Role::Planner));
        assert!(path.with_extension("jsonl.old").exists());
        // The fresh file holds exactly the one new row.
        assert_eq!(read_rows_for_task(home, "t1").len(), 1);
    }

    /// Live round 3 E2: the row must be able to say "codex was requested,
    /// Claude answered" — before this it could only repeat the request.
    #[test]
    fn row_carries_the_runtime_that_actually_answered() {
        let mut r = row("t1", 1, Role::Executor);
        r.runtime = "codex".to_string();
        r.request_model = Some("gpt-5.6-sol".to_string());
        r.provider = "claude".to_string();
        r.runtime_used = Some("claude".to_string());
        r.response_model = Some("claude-opus-4-6".to_string());
        r.failover = true;
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["runtime"], "codex");
        assert_eq!(json["request_model"], "gpt-5.6-sol");
        assert_eq!(json["runtime_used"], "claude");
        assert_eq!(json["response_model"], "claude-opus-4-6");
        assert_eq!(json["failover"], true);
        let back: RoleTurnRow = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn refused_row_reports_no_answering_runtime_rather_than_guessing() {
        let r = RoleTurnRow::refused(
            "t1",
            "agnes",
            1,
            Role::Verifier,
            "gemini",
            Some("gemini-3.7-flash"),
            RoleTurnOutcome::Failed,
            "scaffold_refused",
        );
        assert_eq!(r.runtime_used, None);
        assert_eq!(r.response_model, None);
        assert!(!r.failover);
        // Absent fields stay out of the serialized row entirely.
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("runtime_used").is_none());
        assert!(json.get("response_model").is_none());
    }

    /// A pre-E2 row (no `runtime_used`/`response_model`/`failover` keys) must
    /// still deserialize — the ledger is append-only and history predates the
    /// fields.
    #[test]
    fn legacy_rows_without_the_new_fields_still_parse() {
        let legacy = serde_json::json!({
            "timestamp": "2026-09-24T10:00:00Z",
            "task_id": "t1",
            "agent_id": "agnes",
            "round": 1,
            "role": "planner",
            "member_id": "eph-x",
            "runtime": "claude",
            "provider": "claude",
            "observation_fidelity": "none",
            "outcome": "completed",
            "config_fingerprint_hard": "deadbeef",
        });
        let row: RoleTurnRow = serde_json::from_value(legacy).unwrap();
        assert_eq!(row.runtime_used, None);
        assert!(!row.failover);
    }

    #[test]
    fn member_ids_are_round_scoped_deduped_and_skip_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let mut r1 = row("t1", 1, Role::Planner);
        r1.member_id = "eph-a".to_string();
        let mut r2 = row("t1", 1, Role::Executor);
        r2.member_id = "eph-b".to_string();
        // Same member appearing twice in a round (initial pass + repair).
        let mut r3 = row("t1", 1, Role::Executor);
        r3.member_id = "eph-b".to_string();
        let mut r4 = row("t1", 2, Role::Executor);
        r4.member_id = "eph-c".to_string();
        // A refusal that never reached a scaffold has no audit trail.
        let r5 = RoleTurnRow::refused(
            "t1",
            "agnes",
            1,
            Role::Verifier,
            "gemini",
            None,
            RoleTurnOutcome::Failed,
            "model_unresolved",
        );
        // Another task's member must never leak in.
        let mut r6 = row("t2", 1, Role::Planner);
        r6.member_id = "eph-other".to_string();
        for r in [&r1, &r2, &r3, &r4, &r5, &r6] {
            append_row(home, r);
        }

        assert_eq!(
            member_ids_for_task_round(home, "t1", 1),
            vec!["eph-a".to_string(), "eph-b".to_string()]
        );
        assert_eq!(
            member_ids_for_task_round(home, "t1", 2),
            vec!["eph-c".to_string()]
        );
        assert!(member_ids_for_task_round(home, "t1", 9).is_empty());
        assert_eq!(
            member_ids_for_task(home, "t1"),
            vec![
                "eph-a".to_string(),
                "eph-b".to_string(),
                "eph-c".to_string()
            ]
        );
    }

    /// Regression (review finding 14): a team task is never claimed, so the
    /// whole-task member fallback used to be paired with a `created_at`
    /// window and round 1's members vouched for round 3. The window-scoped
    /// fallback must not reach behind its own `since`.
    #[test]
    fn member_ids_since_excludes_earlier_rounds_members() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let mut early = row("t1", 1, Role::Executor);
        early.member_id = "eph-round1".to_string();
        early.timestamp = "2026-09-24T10:00:00Z".to_string();
        let mut late = row("t1", 3, Role::Executor);
        late.member_id = "eph-round3".to_string();
        late.timestamp = "2026-09-24T12:00:00Z".to_string();
        append_row(home, &early);
        append_row(home, &late);

        assert_eq!(
            member_ids_for_task_since(home, "t1", "2026-09-24T11:00:00Z"),
            vec!["eph-round3".to_string()],
            "round 1's member must not appear inside round 3's window"
        );
        // The un-narrowed reader is what the defect looked like.
        assert_eq!(
            member_ids_for_task(home, "t1"),
            vec!["eph-round1".to_string(), "eph-round3".to_string()]
        );
        // An unparseable cutoff must not silently hide evidence.
        assert_eq!(
            member_ids_for_task_since(home, "t1", "not-a-timestamp"),
            vec!["eph-round1".to_string(), "eph-round3".to_string()]
        );
    }

    /// Regression (review finding 1): the round's own start must be derivable
    /// from the durable ledger, since `tasks.claimed_at` is always `None` for
    /// a team task.
    #[test]
    fn round_started_at_is_the_earliest_row_of_that_round_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let mut r1 = row("t1", 1, Role::Planner);
        r1.timestamp = "2026-09-24T10:00:00Z".to_string();
        let mut r2 = row("t1", 2, Role::Planner);
        r2.member_id = "eph-b".to_string();
        r2.timestamp = "2026-09-24T11:00:00Z".to_string();
        let mut r3 = row("t1", 2, Role::Executor);
        r3.member_id = "eph-c".to_string();
        r3.timestamp = "2026-09-24T11:30:00Z".to_string();
        for r in [&r1, &r2, &r3] {
            append_row(home, r);
        }

        assert_eq!(
            round_started_at(home, "t1", 2).as_deref(),
            Some("2026-09-24T11:00:00Z")
        );
        assert_eq!(round_started_at(home, "t1", 9), None);
    }
}
