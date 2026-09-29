//! Universal Human-in-the-Loop (HITL) `ApprovalBroker`.
//!
//! ONE interrupt/approval primitive — the LangGraph `interrupt()` /
//! OpenAI-SDK HITL equivalent — spanning **MCP tools**, **autopilot
//! actions**, and **bus tasks**. A caller that is about to perform a
//! sensitive action `request()`s approval (storing the exact payload to
//! re-dispatch), then either polls or `await_decision()`s. A human
//! decides through a messaging channel reply or the dashboard; on
//! approve, the caller re-reads the stored payload and re-dispatches.
//!
//! ## Why one broker (migration note)
//!
//! Of the three ad-hoc, in-process approval implementations this broker was
//! built to absorb, two are gone: `browser_router.rs` (dead code, removed
//! 2026-09) and the `duduclaw-governance` approval workflow (crate removed in
//! `b0639b96`). One remains, and wiring it is still a follow-up:
//!
//! 1. **`channel_sender.rs`** — a process-local `HashMap<user_id,
//!    oneshot::Sender<bool>>` (`wait_for_confirmation` /
//!    `resolve_confirmation`). Volatile (lost on restart), single-user,
//!    no audit trail, no cross-process visibility. Migration: keep the
//!    zh-TW reply-word matching (`is_confirmation_reply` /
//!    `is_denial_reply`) but resolve against a persisted approval id via
//!    [`ApprovalBroker::decide`] instead of an in-memory oneshot.
//!
//! ## Decision sources
//!
//! - `agent.toml [capabilities] approval_required_tools = [...]` — parsed
//!   by [`approval_required_tools`]. The MCP dispatch path (owned by
//!   another agent this wave) will call [`ApprovalBroker::request`] +
//!   [`ApprovalBroker::await_decision`] before executing a listed tool.
//! - autopilot rule `require_approval = true` in the action JSON — checked
//!   by [`rule_requires_approval`] and wired into
//!   `autopilot_engine::execute_action` (see `with_approval_broker`).
//! - dashboard RPC `approvals.list / approvals.approve / approvals.deny`
//!   (to be added in `handlers.rs` later) → [`list_pending`] / [`decide`].
//!
//! ## Fail-closed conventions
//!
//! - **TTL expiry counts as DENY.** A pending approval past its TTL is
//!   marked `expired`; [`await_decision`] returns `Expired`, which callers
//!   MUST treat as a denial (never fall through to execute).
//! - **`decide` refuses to change a terminal state.** Once
//!   approved/denied/expired, a second decision is rejected (no silent
//!   flip). The `WHERE status = 'pending'` guard also closes the
//!   two-decider race.
//! - **Store idioms mirror `events_store.rs` / `autopilot_store.rs`**:
//!   parameterized SQL only, WAL + `busy_timeout`, self-healing schema.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{info, warn};

// ── Constants ───────────────────────────────────────────────

/// Default TTL when a caller does not specify one. 1 hour.
pub const DEFAULT_TTL_SECONDS: i64 = 3600;

/// Max chars of `summary` rendered into a channel message (CJK-safe via
/// `truncate_chars`, never raw byte slicing).
const CHANNEL_SUMMARY_MAX_CHARS: usize = 500;

/// `decided_by` marker used when the TTL expiry path denies an approval.
pub const DECIDED_BY_TTL: &str = "system:ttl";

/// WP20: fraction of the TTL that must elapse before the "still waiting, about
/// to auto-deny" reminder is pushed (⅔ ⇒ the nudge lands with a third of the
/// window left).
pub const REMIND_AT_FRACTION: f64 = 2.0 / 3.0;

/// WP20: shortest TTL that earns a reminder. Below this, the nudge and the
/// auto-denial would land within seconds of each other — two notifications for
/// one non-event, and the human has no realistic window to act on the first.
/// Short-TTL approvals rely on the initial push alone.
pub const REMIND_MIN_TTL_SECONDS: i64 = 120;

/// WP20: `action_kind`s that own their channel notification already and must
/// NOT receive the generic pending-approval push (it would double-notify with
/// a second, conflicting set of buttons). `goal_kickoff` is pushed by
/// `goal_notify::notify_goal_kickoff` with its own retry bookkeeping.
const SELF_NOTIFYING_KINDS: &[&str] = &["goal_kickoff"];

/// Hard cap on the generic push so a hung channel API can never stall the
/// caller that is filing the approval.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(15);

// ── Types ───────────────────────────────────────────────────

mod action_guard;
mod broker;
mod gates;
mod simulation;
mod store;

/// SQLite-backed persistence for approvals. Mirrors the `events_store` /
/// `autopilot_store` idioms: `Mutex<Connection>`, WAL, self-healing
/// schema, parameterized SQL only.
pub struct ApprovalStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: Option<PathBuf>,
}

/// The single HITL approval primitive. Holds the [`ApprovalStore`] and
/// exposes the request → decide → poll/await lifecycle.
#[derive(Clone)]
pub struct ApprovalBroker {
    store: std::sync::Arc<ApprovalStore>,
}

#[cfg(test)]
mod tests;

/// WP20: whether a delivered destination differs from what the record already
/// records, i.e. whether it must be written back.
///
/// This matters because a reminder doubles as the retry for a first push that
/// found nothing: the record then still has `notify_channel = None`, while the
/// reminder's buttons are live in some chat. Without the write-back, the
/// inbound handler has nothing to match the presser against and those buttons
/// authorize no one.
pub(crate) fn notify_target_changed(rec: &ApprovalRecord, channel: &str, chat_id: &str) -> bool {
    rec.notify_channel.as_deref() != Some(channel) || rec.notify_chat_id.as_deref() != Some(chat_id)
}

pub use action_guard::{ALL_ACTION_GUARD_FINDINGS, ActionGuardFinding, analyze_action_guard_findings};

use action_guard::{
    SIMULATION_MAX_RISK_POINTS, SIMULATION_NARRATIVE_MAX_CHARS, SIMULATION_RISK_POINT_MAX_CHARS,
};
#[cfg(test)]
use broker::reminder_navigate_path;
#[cfg(test)]
use simulation::{GROUNDING_MAX_SNIPPETS, GROUNDING_SNIPPET_MAX_CHARS, protected_wiki_namespaces};
pub use gates::{
    ActionGate, JudgeVerdict, approval_required_tools, irreversible_tools,
    maybe_irreversible_tools, resolve_action_gate, tool_is_irreversible,
    tool_is_maybe_irreversible, tool_requires_approval,
};
pub use simulation::{
    SimulationNarrative, auto_approve_install, pending_summary_for_channel,
    render_grounding_block, rule_requires_approval, simulation_grounding_snippets,
};

/// Opaque approval identifier (UUIDv4 string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalId(String);

impl ApprovalId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ApprovalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ApprovalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ApprovalId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Lifecycle status of an approval. Approved is the ONLY status a caller
/// may act on; every other terminal status is a denial (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Approved => "approved",
            ApprovalStatus::Denied => "denied",
            ApprovalStatus::Expired => "expired",
        }
    }

    /// Parse from the DB text column. Unknown values fail closed to
    /// `Denied` (never `Approved`) so a corrupted row never authorizes.
    pub fn from_db(s: &str) -> Self {
        match s {
            "pending" => ApprovalStatus::Pending,
            "approved" => ApprovalStatus::Approved,
            "expired" => ApprovalStatus::Expired,
            _ => ApprovalStatus::Denied,
        }
    }

    /// True for any non-pending state (approved / denied / expired).
    pub fn is_terminal(self) -> bool {
        !matches!(self, ApprovalStatus::Pending)
    }

    /// True only when the caller is authorized to proceed.
    pub fn is_granted(self) -> bool {
        matches!(self, ApprovalStatus::Approved)
    }
}

/// Where an approval decision originated. `decided_by` is stored as free
/// text; this enum standardizes the common producers for the eventual
/// wire-up (channel reply / dashboard RPC / TTL sweep / programmatic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    Channel,
    Dashboard,
    Ttl,
    Api,
}

impl DecisionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionSource::Channel => "channel",
            DecisionSource::Dashboard => "dashboard",
            DecisionSource::Ttl => DECIDED_BY_TTL,
            DecisionSource::Api => "api",
        }
    }
}

/// One row of the `approvals` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: ApprovalId,
    pub agent_id: String,
    /// "mcp_tool" | "autopilot_action" | "bus_task" | "browser_action" | ...
    pub action_kind: String,
    /// Human-readable summary of what is being approved.
    pub summary: String,
    /// The exact thing to re-dispatch on approval (opaque JSON).
    pub payload: Value,
    pub status: ApprovalStatus,
    pub created_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    pub ttl_seconds: i64,
    /// WP20: the channel the pending-approval push was actually delivered to
    /// (`telegram` / `slack` / …). `None` = never pushed (no destination, or a
    /// kind that owns its own notification). Persisted so the TTL reminder and
    /// the inbound button handler can both reason about "where did this go".
    #[serde(default)]
    pub notify_channel: Option<String>,
    /// WP20: the chat/user id the push was delivered to, paired with
    /// [`Self::notify_channel`].
    #[serde(default)]
    pub notify_chat_id: Option<String>,
    /// WP20: when the "about to expire" reminder was sent (RFC3339). `None` =
    /// not yet reminded; the column doubles as the race-safe once-only guard.
    #[serde(default)]
    pub reminded_at: Option<String>,
    /// D1 (WebDreamer arXiv:2411.06559): the ActionGuard judge's structured
    /// "what will the world look like after this call runs" simulation,
    /// stored as `{"world_state_change": "...", "risk_points": [...]}`
    /// ([`SimulationNarrative::to_json`]). `None` for every approval kind
    /// that never ran the maybe-irreversible judge (the overwhelming
    /// majority) — purely additive, never required. Downstream notifiers
    /// (D2, `approval_notify::approval_body` / `goal_notify`) render this as
    /// a forward-trajectory line above the approve/deny buttons.
    #[serde(default)]
    pub simulation: Option<Value>,
}

impl ApprovalRecord {
    /// The instant this approval expires (created_at + ttl). `None` if
    /// `created_at` is unparseable — treated as "already expired" by
    /// [`is_stale`] (fail-closed).
    fn expires_at(&self) -> Option<DateTime<Utc>> {
        let created = DateTime::parse_from_rfc3339(&self.created_at).ok()?;
        Some(created.with_timezone(&Utc) + chrono::Duration::seconds(self.ttl_seconds))
    }

    /// True if pending and past its TTL (or has an unparseable timestamp).
    fn is_stale(&self, now: DateTime<Utc>) -> bool {
        if self.status != ApprovalStatus::Pending {
            return false;
        }
        match self.expires_at() {
            Some(exp) => now >= exp,
            None => true, // unparseable created_at ⇒ fail closed
        }
    }

    /// The RFC3339 instant this approval expires, for rendering a human
    /// deadline in the channel message. `None` on an unparseable timestamp.
    pub fn deadline_rfc3339(&self) -> Option<String> {
        self.expires_at().map(|t| t.to_rfc3339())
    }

    /// The instant this approval expires, as a Unix epoch (seconds). Lets a
    /// dashboard client compute a live countdown with plain arithmetic instead
    /// of parsing RFC3339 client-side. `None` on an unparseable timestamp
    /// (mirrors [`Self::deadline_rfc3339`]).
    pub fn expires_at_epoch(&self) -> Option<i64> {
        self.expires_at().map(|t| t.timestamp())
    }

    /// WP20: true when the pending approval has burned through
    /// [`REMIND_AT_FRACTION`] of its TTL and has not been reminded yet — the
    /// "about to auto-deny" nudge is due.
    ///
    /// Deliberately NOT a new background loop: evaluated on the paths that
    /// already touch a pending row (`poll` — which `await_decision` drives every
    /// couple of seconds — and the `expire_stale` sweep).
    pub(crate) fn reminder_due(&self, now: DateTime<Utc>) -> bool {
        if self.status != ApprovalStatus::Pending || self.reminded_at.is_some() {
            return false;
        }
        // Too short a window for a nudge to be actionable — see
        // [`REMIND_MIN_TTL_SECONDS`].
        if self.ttl_seconds < REMIND_MIN_TTL_SECONDS {
            return false;
        }
        let Ok(created) = DateTime::parse_from_rfc3339(&self.created_at) else {
            return false; // unparseable ⇒ is_stale already denies it; no nudge
        };
        let created = created.with_timezone(&Utc);
        let elapsed = (now - created).num_milliseconds();
        let ttl_ms = self.ttl_seconds.saturating_mul(1000);
        if ttl_ms <= 0 {
            return false;
        }
        // Due once REMIND_AT_FRACTION of the window has elapsed, but not after
        // it has already expired (that path is a denial, not a reminder).
        elapsed >= (ttl_ms as f64 * REMIND_AT_FRACTION) as i64 && elapsed < ttl_ms
    }
}

// ── Store ───────────────────────────────────────────────────
