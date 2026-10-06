//! The one dashboard approval gate in front of every state-changing operator
//! command-line action (owner decision U5).
//!
//! The terminal cannot prove who typed a command: an AI employee with Bash
//! runs as the same operating-system user. So a state-changing operator CLI
//! action only files a dashboard-only [`ApprovalBroker`] request; an Admin
//! decides it in the dashboard; the CLI then applies the action once, bound
//! to the request. Four features use this module (LINE inbox, continuous
//! responsibilities, forget by source, computer workspaces). Each supplies a
//! [`KindSpec`] (texts, validity, caps) and a [`Binding`] per request; this
//! module owns matching, merging, throttling, consumption, push caps and the
//! registry the approval inbox, the notifier and `approvals.decide` read.
//!
//! Rules (design `DESIGN-operator-cli-approval-gate-2026-10`):
//! - the binding lives in the payload's `"gate"` object; rows without it
//!   (filed before this module) are never matched and never counted;
//! - an approval counts only when decided in the dashboard (`decided_by`
//!   starts with `dashboard:`), for the same request digest, for the same
//!   state (unless the binding says the state is checked item by item when
//!   applied) and, where the kind has a window, within that window after the
//!   decision; anything else is invalidated with a fixed reason;
//! - one waiting request per (kind, action, target, digest, state); a state
//!   change invalidates the waiting card (`state_changed`) and files a new
//!   one, never rewriting the old card in place;
//! - a different digest for the same (action, target) is a separate request;
//!   at most `max_pending_per_target` of them and `max_pending_per_kind`
//!   overall may wait;
//! - consumption is one conditional UPDATE (`consume_approved`); a run that
//!   finds the row no longer approved (another run consumed it, or another
//!   run invalidated it, e.g. `state_changed`) gets [`Gate::AlreadyClaimed`]
//!   and files nothing.
//!
//! Limits: this binds the product paths. An employee with unrestricted Bash
//! that evades the file guard can still rewrite `approvals.db` directly; real
//! isolation is not granting Bash, or the task sandbox.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus};

/// The actor recorded for anything a terminal asks for.
pub const UNVERIFIED_ACTOR: &str = "本機指令列（身分未驗證）";
/// Payload key holding the binding.
pub const GATE_KEY: &str = "gate";
/// Binding format version.
pub const GATE_VERSION: i64 = 1;
/// Most requests (any target) of one kind that may wait at once.
pub const DEFAULT_MAX_PENDING_PER_KIND: usize = 20;
/// Most different requests for one (action, target) that may wait at once.
pub const DEFAULT_MAX_PENDING_PER_TARGET: usize = 3;

/// How long an approval stays usable after its decision.
#[derive(Debug, Clone, Copy)]
pub enum Validity {
    /// A fixed number of minutes.
    Fixed(i64),
    /// Minutes read from the operator's configuration on every check.
    Config(fn(&Path) -> i64),
    /// No window after the decision: the request's own TTL bounds it (a
    /// forget plan expires with its request).
    UntilRequestExpiry,
}

/// Whether a proceed consumes the approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consume {
    Once,
    /// The artifact the approval names is single-use itself (a forget plan).
    Never,
}

/// How the state fingerprint binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatePolicy {
    /// An approval granted for another state is void.
    MustMatch,
    /// Only a waiting card must match the current state; an approval stays
    /// usable and the caller re-checks every item when applying (LINE batch).
    PendingOnly,
}

/// Everything one approval kind supplies.
pub struct KindSpec {
    pub kind: &'static str,
    pub validity: Validity,
    pub consume: Consume,
    /// Whether the generic "about to auto-deny" reminder is pushed.
    pub reminders: bool,
    pub max_pending_per_target: usize,
    pub max_pending_per_kind: usize,
    /// First pushes per push scope per hour; `None` = uncapped.
    pub push_cap_per_hour: Option<usize>,
    /// Payload key read as the push scope for rows filed before the `gate`
    /// object existed.
    pub legacy_scope_key: &'static str,
    /// What `approvals.decide` answers a caller who is not a current Admin.
    pub admin_refusal: &'static str,
    /// What the dashboard answers for a decision on an expired card.
    pub expired_text: &'static str,
    /// What a channel decision is answered with; `None` = the shared text.
    pub channel_refusal: Option<&'static str>,
    /// Plain channel notice: (record, reminder, deadline phrase).
    pub notice: fn(&ApprovalRecord, bool, &str) -> String,
    /// Called (on a blocking thread) when a first push is suppressed by the
    /// cap: (home, record, pushes in the last hour).
    pub on_push_suppressed: fn(&Path, &ApprovalRecord, usize),
}

/// Every operator-CLI approval kind. The inbox, the notifier, the reminder
/// sweep and `approvals.decide` all read this table.
pub static OPERATOR_CLI_KINDS: &[&KindSpec] = &[
    &crate::channel_ingress::cli_approval::SPEC,
    &crate::responsibility::operator_gate::SPEC,
    &crate::memory_forget_approval::SPEC,
    &crate::computer_workspaces::cli_approval::SPEC,
];

/// The spec of an operator-CLI kind, `None` for every other kind.
pub fn spec_for(kind: &str) -> Option<&'static KindSpec> {
    OPERATOR_CLI_KINDS.iter().copied().find(|s| s.kind == kind)
}

/// Whether the generic reminder must not be pushed for `kind`.
pub fn no_reminder(kind: &str) -> bool {
    spec_for(kind).is_some_and(|s| !s.reminders)
}

/// The validity window in minutes for `spec`, `None` for no window.
pub fn valid_minutes(spec: &KindSpec, home: &Path) -> Option<i64> {
    match spec.validity {
        Validity::Fixed(m) => Some(m),
        Validity::Config(read) => Some(read(home)),
        Validity::UntilRequestExpiry => None,
    }
}

/// `approvals.decide`: only a current Admin (role re-read from `users.db`)
/// decides an operator-CLI request.
pub fn require_admin_decider(home: &Path, ctx: &duduclaw_auth::UserContext) -> Result<(), String> {
    super::require_current_dashboard_role_in_home(home, ctx, duduclaw_auth::UserRole::Admin)
}

/// sha256 over length-prefixed parts (no separator ambiguity).
pub fn digest(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    hex::encode(h.finalize())
}

/// What one request is bound to.
#[derive(Debug, Clone)]
pub struct Binding<'a> {
    pub action: &'a str,
    pub target: &'a str,
    /// The request itself (arguments, note, plan hash), never the state.
    pub request_digest: String,
    /// The state fingerprint the request was made against.
    pub state: String,
    pub state_policy: StatePolicy,
    /// Push-cap counting key; `None` = `target`.
    pub push_scope: Option<&'a str>,
}

/// The card a new request is filed with.
pub struct Filing<'a> {
    pub agent_id: &'a str,
    pub summary: &'a str,
    /// Feature fields kept at the top level of the payload (notices read
    /// them). Must be a JSON object or `Null`.
    pub extra: Value,
    pub ttl_secs: i64,
}

/// The binding stored on a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBinding {
    pub action: String,
    pub target: String,
    pub request_digest: String,
    pub state: String,
    pub push_scope: String,
}

/// Read the binding of `rec`, `None` for a row without a current one.
pub fn binding_of(rec: &ApprovalRecord) -> Option<StoredBinding> {
    let g = rec.payload.get(GATE_KEY)?;
    if g.get("v").and_then(Value::as_i64) != Some(GATE_VERSION) {
        return None;
    }
    let s = |k: &str| g.get(k).and_then(Value::as_str).map(str::to_string);
    let target = s("target")?;
    Some(StoredBinding {
        action: s("action")?,
        push_scope: s("push_scope").unwrap_or_else(|| target.clone()),
        target,
        request_digest: s("request_digest")?,
        state: s("state")?,
    })
}

/// Why the gate invalidated an approval or a waiting card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoidReason {
    NotDashboardDecision,
    StateChanged,
    ApprovalExpired,
    Duplicate,
}

impl VoidReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotDashboardDecision => "not_dashboard_decision",
            Self::StateChanged => "state_changed",
            Self::ApprovalExpired => "approval_expired",
            Self::Duplicate => "duplicate",
        }
    }
}

/// Which cap stopped a new request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleScope {
    Target,
    Kind,
}

/// An approval claimed for this run.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub id: ApprovalId,
    pub decided_by: String,
    pub payload: Value,
    bound_state: String,
}

impl Claim {
    /// Whether the state read again right before acting is still the one
    /// the approval was bound to. Kinds whose apply path has no CAS must
    /// call this after consuming.
    pub fn still_bound(&self, current_state: &str) -> bool {
        self.bound_state == current_state
    }
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    /// An approval for exactly this request (and state) was claimed: act.
    Proceed(Claim),
    /// A request was filed now.
    Requested(ApprovalId),
    /// A request for exactly this is still waiting.
    Pending(ApprovalId),
    /// Too many requests wait; nothing was filed.
    Throttled { waiting: usize, scope: ThrottleScope },
    /// A usable approval existed but another run consumed or invalidated it
    /// first (`consume_approved` found it no longer approved).
    AlreadyClaimed(ApprovalId),
}

fn decided_by_dashboard(rec: &ApprovalRecord) -> bool {
    rec.decided_by
        .as_deref()
        .is_some_and(|by| by.starts_with("dashboard:"))
}

/// Whether `decided_at` lies within `minutes` before `now` (unparsable or
/// missing ⇒ outside, fail closed).
pub fn decided_within(decided_at: Option<&str>, minutes: i64, now: DateTime<Utc>) -> bool {
    decided_at
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| {
            let age = now.signed_duration_since(t.with_timezone(&Utc));
            age >= chrono::Duration::zero() && age <= chrono::Duration::minutes(minutes)
        })
}

async fn live_pending(
    broker: &ApprovalBroker,
    id: &ApprovalId,
    now: DateTime<Utc>,
) -> Result<bool, String> {
    // `get` only reads; a pending row past its TTL counts as not live through
    // `is_stale` (fail closed), without being settled here.
    Ok(broker
        .get(id)
        .await?
        .is_some_and(|r| r.status == ApprovalStatus::Pending && !r.is_stale(now)))
}

/// Check for a usable approval, or file a request (see the module doc).
/// `valid_minutes`: `None` = no window after the decision.
pub async fn gate(
    broker: &ApprovalBroker,
    spec: &KindSpec,
    bind: &Binding<'_>,
    filing: Filing<'_>,
    valid_minutes: Option<i64>,
    now: DateTime<Utc>,
) -> Result<(Gate, Vec<(ApprovalId, VoidReason)>), String> {
    let mut voided: Vec<(ApprovalId, VoidReason)> = Vec::new();
    let mut pending: Option<ApprovalId> = None;
    let mut target_waiting = 0usize;
    let mut kind_waiting = 0usize;
    for rec in broker.list_by_kind(spec.kind).await? {
        let Some(b) = binding_of(&rec) else { continue };
        let same_target = b.action == bind.action && b.target == bind.target;
        let same_request = same_target && b.request_digest == bind.request_digest;
        let same_state = b.state == bind.state;
        match rec.status {
            ApprovalStatus::Approved if same_request => {
                let why = if !decided_by_dashboard(&rec) {
                    Some(VoidReason::NotDashboardDecision)
                } else if bind.state_policy == StatePolicy::MustMatch && !same_state {
                    Some(VoidReason::StateChanged)
                } else if valid_minutes
                    .is_some_and(|m| !decided_within(rec.decided_at.as_deref(), m, now))
                {
                    Some(VoidReason::ApprovalExpired)
                } else {
                    None
                };
                if let Some(why) = why {
                    broker.invalidate_request(&rec.id, why.as_str()).await?;
                    voided.push((rec.id.clone(), why));
                    continue;
                }
                let claim = Claim {
                    id: rec.id.clone(),
                    decided_by: rec.decided_by.clone().unwrap_or_default(),
                    payload: rec.payload.clone(),
                    bound_state: b.state.clone(),
                };
                if spec.consume == Consume::Never {
                    return Ok((Gate::Proceed(claim), voided));
                }
                let reason = format!("consumed:{}", uuid::Uuid::new_v4().as_simple());
                if broker.consume_approved(&rec.id, &reason).await? {
                    return Ok((Gate::Proceed(claim), voided));
                }
                return Ok((Gate::AlreadyClaimed(rec.id), voided));
            }
            ApprovalStatus::Pending => {
                if !live_pending(broker, &rec.id, now).await? {
                    continue;
                }
                if same_request {
                    let why = if pending.is_some() {
                        Some(VoidReason::Duplicate)
                    } else if !same_state {
                        Some(VoidReason::StateChanged)
                    } else {
                        None
                    };
                    if let Some(why) = why {
                        broker.invalidate_request(&rec.id, why.as_str()).await?;
                        voided.push((rec.id.clone(), why));
                        continue;
                    }
                    pending = Some(rec.id);
                } else if same_target {
                    target_waiting += 1;
                }
                kind_waiting += 1;
            }
            _ => {}
        }
    }
    if let Some(id) = pending {
        return Ok((Gate::Pending(id), voided));
    }
    if target_waiting >= spec.max_pending_per_target {
        return Ok((
            Gate::Throttled {
                waiting: target_waiting,
                scope: ThrottleScope::Target,
            },
            voided,
        ));
    }
    if kind_waiting >= spec.max_pending_per_kind {
        return Ok((
            Gate::Throttled {
                waiting: kind_waiting,
                scope: ThrottleScope::Kind,
            },
            voided,
        ));
    }
    let id = file(broker, spec, bind, filing).await?;
    Ok((Gate::Requested(id), voided))
}

/// File a request with `bind` stored in the payload's `gate` object. Callers
/// normally go through [`gate`], which applies the caps first.
pub async fn file(
    broker: &ApprovalBroker,
    spec: &KindSpec,
    bind: &Binding<'_>,
    filing: Filing<'_>,
) -> Result<ApprovalId, String> {
    let mut payload = match filing.extra {
        Value::Object(map) => Value::Object(map),
        Value::Null => json!({}),
        _ => return Err("operator gate: extra payload must be an object".into()),
    };
    payload[GATE_KEY] = json!({
        "v": GATE_VERSION,
        "action": bind.action,
        "target": bind.target,
        "request_digest": bind.request_digest,
        "state": bind.state,
        "push_scope": bind.push_scope.unwrap_or(bind.target),
    });
    if payload.get("requested_by").is_none() {
        payload["requested_by"] = json!(UNVERIFIED_ACTOR);
    }
    broker
        .request(filing.agent_id, spec.kind, filing.summary, payload, filing.ttl_secs)
        .await
}

/// Where one request stands, read-only (the newest row for the action and
/// target decides). Used where the approved artifact is single-use itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approved { id: ApprovalId },
    Pending { id: ApprovalId },
    /// Denied, withdrawn, expired, invalidated or decided outside the
    /// dashboard (`status` says which).
    Refused { id: ApprovalId, status: String },
    /// Approved, but for another request digest.
    Mismatch { id: ApprovalId },
    Missing,
}

/// The read-only verdict for (action, target, digest).
pub async fn verdict(
    broker: &ApprovalBroker,
    spec: &KindSpec,
    action: &str,
    target: &str,
    request_digest: &str,
    valid_minutes: Option<i64>,
    now: DateTime<Utc>,
) -> Result<Verdict, String> {
    let newest = broker
        .list_by_kind(spec.kind)
        .await?
        .into_iter()
        .filter_map(|r| binding_of(&r).map(|b| (r, b)))
        .filter(|(_, b)| b.action == action && b.target == target)
        .max_by(|a, b| a.0.created_at.cmp(&b.0.created_at));
    let Some((rec, b)) = newest else {
        return Ok(Verdict::Missing);
    };
    let id = rec.id.clone();
    Ok(match rec.status {
        ApprovalStatus::Approved if !decided_by_dashboard(&rec) => Verdict::Refused {
            id,
            status: VoidReason::NotDashboardDecision.as_str().into(),
        },
        ApprovalStatus::Approved if b.request_digest != request_digest => Verdict::Mismatch { id },
        ApprovalStatus::Approved
            if valid_minutes
                .is_some_and(|m| !decided_within(rec.decided_at.as_deref(), m, now)) =>
        {
            Verdict::Refused {
                id,
                status: VoidReason::ApprovalExpired.as_str().into(),
            }
        }
        ApprovalStatus::Approved => Verdict::Approved { id },
        ApprovalStatus::Pending if rec.is_stale(now) => Verdict::Refused {
            id,
            status: "expired".into(),
        },
        ApprovalStatus::Pending => Verdict::Pending { id },
        other => Verdict::Refused {
            id,
            status: other.as_str().into(),
        },
    })
}

fn push_scope_of(spec: &KindSpec, rec: &ApprovalRecord) -> Option<String> {
    match binding_of(rec) {
        Some(b) => Some(b.push_scope),
        None => rec
            .payload
            .get(spec.legacy_scope_key)
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// Whether one more first push for `rec` fits its kind's hourly cap. Kinds
/// outside the registry, or without a cap, are always allowed. Past the cap
/// the kind's suppression hook runs and the request stays in the dashboard
/// inbox only. Anything unreadable refuses the push (fail closed).
pub async fn push_allowed(home: &Path, rec: &ApprovalRecord) -> bool {
    let Some(spec) = spec_for(&rec.action_kind) else {
        return true;
    };
    let Some(cap) = spec.push_cap_per_hour else {
        return true;
    };
    let Some(scope) = push_scope_of(spec, rec) else {
        return false;
    };
    let Ok(broker) = ApprovalBroker::open(home) else {
        return false;
    };
    let Ok(all) = broker.list_by_kind(spec.kind).await else {
        return false;
    };
    let hour_ago = Utc::now() - chrono::Duration::hours(1);
    let pushed = all
        .iter()
        .filter(|r| r.id != rec.id && r.notify_channel.is_some())
        .filter(|r| push_scope_of(spec, r).as_deref() == Some(scope.as_str()))
        .filter(|r| {
            DateTime::parse_from_rfc3339(&r.created_at)
                .is_ok_and(|t| t.with_timezone(&Utc) >= hour_ago)
        })
        .count();
    if pushed < cap {
        return true;
    }
    let hook = spec.on_push_suppressed;
    let home = home.to_path_buf();
    let rec = rec.clone();
    let _ = tokio::task::spawn_blocking(move || hook(&home, &rec, pushed)).await;
    false
}

#[cfg(test)]
#[path = "operator_cli_gate_tests.rs"]
mod tests;
