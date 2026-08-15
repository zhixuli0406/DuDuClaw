//! Spawn **admission queue** (H19) — bounded FIFO queuing for spawn requests
//! that are legitimate but temporarily over a concurrency cap.
//!
//! ## Why this exists (research/harness-2026-08/grok-build.md §2.7)
//!
//! Grok Build's sub-agent admission controller defaults to N concurrent
//! spawns per session; anything over that either hard-fails or FIFO-queues
//! (`queue|fail`, operator choice), a configured cap of `0` is clamped to `1`
//! ("a limit can be adjusted but never disabled"), and pending admissions are
//! invalidated once their owning session ends (the "Stop blocks late spawns"
//! rule). DuDuClaw's closest analog was `ephemeral::scaffold`'s
//! `MAX_ACTIVE_EPHEMERAL` circuit breaker: a caller that asked for a
//! perfectly valid ephemeral sub-agent while the platform was merely *busy*
//! (32 live scaffolds already outstanding, freed only by the hourly GC sweep)
//! got a hard `Err` — the request was lost, not deferred. This module is the
//! queue half of the fix: it does not decide admission itself (the caller
//! already knows its own capacity model — disk scaffolds, in-flight leases,
//! whatever), it durably holds a request that was refused *right now* so a
//! periodic drain can admit it once room exists.
//!
//! ## Relationship to sibling modules
//!
//! - [`crate::dispatch_guard`] is a *rate* limiter (rolling-window circuit
//!   breaker) — untouched by this module, different problem (frequency vs.
//!   capacity).
//! - [`crate::concurrency_gate`] is an edition-differentiated *in-flight lease*
//!   counter with its own defer-and-retry loop (goal-loop dispatch) — this
//!   module does not duplicate that; it is the general-purpose, durable,
//!   cross-process FIFO queue for callers whose capacity model is NOT a
//!   simple lease (e.g. ephemeral's disk-scaffold-count-until-GC model, where
//!   an in-process retry loop would spin uselessly for up to ~2 hours).
//!
//! ## Semantics
//!
//! - Every ticket belongs to a `class` (a short, stable label — e.g.
//!   `"ephemeral"`) so unrelated admission queues never interfere.
//! - `admission = "queue"` (default): a request that cannot be admitted right
//!   now is durably enqueued (FIFO by insertion order) instead of rejected,
//!   UNLESS the queue itself is already at `queue_max_depth` — that still
//!   rejects, with an explicit reason (an unbounded queue is its own runaway
//!   risk).
//! - `admission = "fail"`: queuing is disabled; every over-capacity request is
//!   rejected immediately — byte-identical to the pre-H19 hard-reject
//!   behavior. This is the documented escape hatch, not the default.
//! - Every queued ticket carries a **TTL**; a ticket found expired during any
//!   scan is dropped (never silently — see [`sweep_expired`] and
//!   [`dequeue_next`], both of which return what they dropped so the caller
//!   can audit-log it).
//! - Tickets may carry an optional `owner_key` (e.g. a turn/session id) so
//!   [`invalidate_owner`] can purge every ticket belonging to a session that
//!   has since ended — the "Stop blocks late spawns" analog. A queued spawn
//!   with no meaningful owner scope (`owner_key = None`) is never touched by
//!   invalidation and only ever leaves the queue via admission or TTL expiry.
//!
//! ## Failure posture
//!
//! Like `dispatch_guard` / `concurrency_gate`, this is a resource-management
//! primitive, not a security gate. State lives in
//! `<home>/spawn_admission_queue.json`; every read-modify-write is wrapped in
//! [`crate::with_file_lock`] (project convention #3). Corrupt state is treated
//! as empty. [`enqueue`] surfaces lock/FS failure as `Err` so the caller can
//! degrade to its own pre-existing hard-reject behavior (queuing a request we
//! cannot durably persist would be worse than rejecting it outright) —
//! deliberately the one place this module does NOT fail-open, because
//! "silently admitted anyway" would defeat the concurrency cap the caller is
//! trying to enforce.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::warn;

/// State-file name under `<home>`.
const STATE_FILE: &str = "spawn_admission_queue.json";

/// How an over-capacity spawn request is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdmissionMode {
    /// Durably FIFO-queue the request (default — see module docs).
    Queue,
    /// Reject immediately. The pre-H19 behavior, kept as an explicit opt-out.
    Fail,
}

impl Default for AdmissionMode {
    fn default() -> Self {
        AdmissionMode::Queue
    }
}

/// Tuning for the admission queue. Overridable via `config.toml [dispatch]`
/// (see [`AdmissionConfig::from_home`]) — the SAME `[dispatch]` table that
/// `concurrency_gate::ConcurrencyGateConfig` and the goal-dispatch-engine
/// config independently parse; each struct only reads the keys it knows and
/// ignores the rest (established project pattern), so adding fields here
/// never conflicts with those.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct AdmissionConfig {
    /// `"queue"` (default) or `"fail"`.
    pub admission: AdmissionMode,
    /// Maximum live (unexpired) queued tickets per class. Overflow is
    /// rejected with an explicit reason even in `Queue` mode — an unbounded
    /// queue is itself a runaway-memory / runaway-disk risk. Clamped to at
    /// least 1 (see [`clamp_min_one`]) — same "never fully disabled" rule as
    /// a concurrency cap.
    pub queue_max_depth: u32,
    /// How long a queued ticket may wait before it is dropped (and logged) as
    /// expired.
    pub queue_item_ttl_secs: u64,
    /// The ephemeral-spawn concurrency cap (`ephemeral::MAX_ACTIVE_EPHEMERAL`
    /// default). `0` is clamped to `1` — see [`clamp_min_one`] — a
    /// concurrency limit "can be adjusted but never disabled"
    /// (research/harness-2026-08/grok-build.md §2.7).
    pub ephemeral_max_active: u32,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            admission: AdmissionMode::Queue,
            queue_max_depth: 64,
            queue_item_ttl_secs: 600,
            // Mirrors the pre-H19 hardcoded `MAX_ACTIVE_EPHEMERAL` constant so
            // an absent/malformed `[dispatch]` section is byte-identical to
            // legacy behavior.
            ephemeral_max_active: 32,
        }
    }
}

impl AdmissionConfig {
    /// Load `[dispatch]` admission tuning from `<home>/config.toml`. Parsed in
    /// isolation from a generic `toml::Table` so unrelated / malformed config
    /// elsewhere in the same section (e.g. dispatch-engine's `enabled` /
    /// `policy` keys, or `concurrency_gate`'s `personal_max_concurrent`)
    /// never breaks this parse — absent / malformed section ⇒ built-in
    /// defaults.
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("dispatch") {
            Some(section) => section.clone().try_into::<AdmissionConfig>().unwrap_or_default(),
            None => Self::default(),
        }
    }
}

/// Clamp a configured concurrency cap so it can never fully disable spawning
/// — `0` becomes `1`, everything else passes through unchanged. Logs a
/// warning when clamping actually happens (fail-visible: an operator who
/// typed `0` expecting "off" needs to see why the platform did not honor it).
pub fn clamp_min_one(configured: u32, context: &str) -> u32 {
    if configured == 0 {
        warn!(
            context,
            "[dispatch] concurrency cap configured as 0 — clamped to 1: \
             a limit can be adjusted but never fully disabled"
        );
        1
    } else {
        configured
    }
}

/// One durably-queued spawn request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedSpawn {
    pub ticket_id: String,
    pub class: String,
    /// Monotonic FIFO order within `class` (ties never occur — assigned under
    /// the same file lock as the insert).
    pub seq: u64,
    pub enqueued_ms: i64,
    pub expires_ms: i64,
    /// Optional caller-defined invalidation scope (e.g. a turn/session id).
    /// `None` means this ticket is never touched by [`invalidate_owner`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_key: Option<String>,
    /// Opaque caller-defined payload — everything the caller needs to replay
    /// the deferred spawn once admitted.
    pub payload: serde_json::Value,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    next_seq: u64,
    #[serde(default)]
    tickets: HashMap<String, QueuedSpawn>,
}

fn now_epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn load_state(path: &Path) -> State {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => State::default(),
    }
}

fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

/// Remove every expired ticket from `state`, returning what was removed
/// (sorted oldest-first) so the caller can audit-log the drop.
fn prune_expired_state(state: &mut State, now_ms: i64) -> Vec<QueuedSpawn> {
    let expired_ids: Vec<String> = state
        .tickets
        .values()
        .filter(|t| t.expires_ms <= now_ms)
        .map(|t| t.ticket_id.clone())
        .collect();
    let mut expired: Vec<QueuedSpawn> = expired_ids
        .into_iter()
        .filter_map(|id| state.tickets.remove(&id))
        .collect();
    expired.sort_by_key(|t| t.seq);
    expired
}

/// Count live (unexpired) tickets in `class`.
fn count_class(state: &State, class: &str, now_ms: i64) -> u32 {
    state
        .tickets
        .values()
        .filter(|t| t.class == class && t.expires_ms > now_ms)
        .count() as u32
}

/// Outcome of an [`enqueue`] attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Durably queued. `position` is 1-based FIFO rank within `class`.
    Queued { ticket_id: String, position: u32 },
    /// Rejected — either `admission = "fail"`, or the queue itself is full.
    Rejected { reason: String },
}

/// Enqueue a deferred spawn request. The caller has already determined the
/// request cannot be admitted right now; this durably holds it (unless
/// `cfg.admission == Fail`, or the queue is already at `cfg.queue_max_depth`)
/// so a later [`dequeue_next`] can admit it once capacity frees.
///
/// Lock/FS failure surfaces as `Err` — deliberately NOT fail-open (see module
/// docs): the caller should fall back to its own pre-existing hard-reject
/// rather than silently bypass the cap it is trying to enforce.
///
/// Deliberately does NOT opportunistically prune expired tickets the way
/// [`dequeue_next`] / [`sweep_expired`] do. [`count_class`] already excludes
/// expired-but-not-yet-physically-removed tickets via its own `expires_ms >
/// now_ms` filter, so admission correctness does not depend on pruning here.
/// An earlier version *did* prune-and-discard inside `enqueue`, which meant a
/// ticket that expired between two unrelated `enqueue` calls could be wiped
/// out as a silent side effect of the SECOND caller's own admission check —
/// never reaching `dequeue_next`/`sweep_expired`, so its expiry was never
/// reported anywhere (a real audit gap: "TTL 過期即丟棄並記錄" requires every
/// expiry to be observable, not just the ones a dequeue/sweep happens to catch
/// first). Reporting expired tickets is the sole job of `dequeue_next` /
/// `sweep_expired`; `enqueue` only ever adds.
pub fn enqueue(
    home_dir: &Path,
    class: &str,
    cfg: &AdmissionConfig,
    owner_key: Option<&str>,
    payload: serde_json::Value,
) -> Result<EnqueueOutcome, String> {
    if cfg.admission == AdmissionMode::Fail {
        return Ok(EnqueueOutcome::Rejected {
            reason: "admission mode is 'fail' — queuing disabled by config".to_string(),
        });
    }
    let max_depth = clamp_min_one(cfg.queue_max_depth, "queue_max_depth");
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let ttl_ms = cfg.queue_item_ttl_secs.saturating_mul(1000) as i64;
    let class = class.to_string();
    let owner_key = owner_key.map(|s| s.to_string());

    crate::with_file_lock(&path, || {
        let mut state = load_state(&path);

        let live = count_class(&state, &class, now_ms);
        if live >= max_depth {
            return Ok::<EnqueueOutcome, std::io::Error>(EnqueueOutcome::Rejected {
                reason: format!(
                    "admission queue full ({live}/{max_depth}) for class '{class}' — \
                     try again later"
                ),
            });
        }

        let ticket_id = uuid::Uuid::new_v4().to_string();
        let seq = state.next_seq;
        state.next_seq = state.next_seq.wrapping_add(1);
        state.tickets.insert(
            ticket_id.clone(),
            QueuedSpawn {
                ticket_id: ticket_id.clone(),
                class: class.clone(),
                seq,
                enqueued_ms: now_ms,
                expires_ms: now_ms + ttl_ms,
                owner_key,
                payload,
            },
        );
        save_state(&path, &state)?;
        Ok::<EnqueueOutcome, std::io::Error>(EnqueueOutcome::Queued { ticket_id, position: live + 1 })
    })
    .map_err(|e: std::io::Error| format!("spawn admission queue unavailable: {e}"))
}

/// Result of a [`dequeue_next`] call.
#[derive(Debug, Default)]
pub struct DequeueResult {
    /// The oldest live ticket in `class`, if any remained after pruning.
    pub ticket: Option<QueuedSpawn>,
    /// Tickets that were found expired during this scan (oldest-first) —
    /// distinct from `ticket`, always worth audit-logging as drops.
    pub expired: Vec<QueuedSpawn>,
}

/// Pop the oldest live (unexpired) ticket for `class`, FIFO by insertion
/// order. Expired tickets encountered during the scan are pruned and
/// returned separately (never silently dropped — see module docs).
///
/// Fail-open on lock/FS failure (returns an empty result): a broken state
/// file must not wedge the drain loop that calls this — the request just
/// stays queued (on disk, if the file is merely transiently unavailable) and
/// is retried on the next drain tick.
pub fn dequeue_next(home_dir: &Path, class: &str) -> DequeueResult {
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let class = class.to_string();

    crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        let expired = prune_expired_state(&mut state, now_ms);

        let oldest_id = state
            .tickets
            .values()
            .filter(|t| t.class == class)
            .min_by_key(|t| t.seq)
            .map(|t| t.ticket_id.clone());

        let ticket = oldest_id.and_then(|id| state.tickets.remove(&id));
        save_state(&path, &state)?;
        Ok::<DequeueResult, std::io::Error>(DequeueResult { ticket, expired })
    })
    .unwrap_or_default()
}

/// Prune every expired ticket in `class` without popping a live one —
/// distinct from [`dequeue_next`], meant for a periodic sweep that reports
/// expiries even when there is no free capacity to admit anything (a queue
/// that is perpetually full must still shed dead entries and log them).
pub fn sweep_expired(home_dir: &Path, class: &str) -> Vec<QueuedSpawn> {
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let class = class.to_string();

    crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        let all_expired = prune_expired_state(&mut state, now_ms);
        save_state(&path, &state)?;
        Ok::<Vec<QueuedSpawn>, std::io::Error>(
            all_expired.into_iter().filter(|t| t.class == class).collect(),
        )
    })
    .unwrap_or_default()
}

/// Remove every live ticket in `class` whose `owner_key == owner` — the
/// "terminal state invalidates its queued spawns" rule (Grok's Stop-blocks-
/// late-spawns analog). Returns the removed tickets (oldest-first) so the
/// caller can audit-log the invalidation. Tickets with `owner_key: None` are
/// never touched (best-effort scoping — see module docs). Also opportunistic-
/// ally prunes expired tickets as a side effect (silently — [`sweep_expired`]
/// is the authoritative expiry-audit path).
pub fn invalidate_owner(home_dir: &Path, class: &str, owner: &str) -> Vec<QueuedSpawn> {
    if owner.is_empty() {
        return Vec::new();
    }
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let class = class.to_string();
    let owner = owner.to_string();

    crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune_expired_state(&mut state, now_ms);

        let invalidate_ids: Vec<String> = state
            .tickets
            .values()
            .filter(|t| t.class == class && t.owner_key.as_deref() == Some(owner.as_str()))
            .map(|t| t.ticket_id.clone())
            .collect();
        let mut removed: Vec<QueuedSpawn> = invalidate_ids
            .into_iter()
            .filter_map(|id| state.tickets.remove(&id))
            .collect();
        removed.sort_by_key(|t| t.seq);
        save_state(&path, &state)?;
        Ok::<Vec<QueuedSpawn>, std::io::Error>(removed)
    })
    .unwrap_or_default()
}

/// Live (unexpired) queue depth for `class` (observability / tests). Prunes
/// expired tickets as a side effect so a stale file does not over-report.
pub fn queue_depth(home_dir: &Path, class: &str) -> u32 {
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let class = class.to_string();
    crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune_expired_state(&mut state, now_ms);
        let n = count_class(&state, &class, now_ms);
        save_state(&path, &state)?;
        Ok::<u32, std::io::Error>(n)
    })
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(admission: AdmissionMode, max_depth: u32, ttl_secs: u64) -> AdmissionConfig {
        AdmissionConfig {
            admission,
            queue_max_depth: max_depth,
            queue_item_ttl_secs: ttl_secs,
            ..AdmissionConfig::default()
        }
    }

    #[test]
    fn clamp_zero_to_one_and_passthrough() {
        assert_eq!(clamp_min_one(0, "x"), 1);
        assert_eq!(clamp_min_one(1, "x"), 1);
        assert_eq!(clamp_min_one(5, "x"), 5);
    }

    #[test]
    fn admission_mode_parses_from_toml_lowercase() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[dispatch]\nadmission = \"fail\"\n")
            .unwrap();
        let c = AdmissionConfig::from_home(dir.path());
        assert_eq!(c.admission, AdmissionMode::Fail);

        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join("config.toml"), "[dispatch]\nadmission = \"queue\"\n")
            .unwrap();
        assert_eq!(AdmissionConfig::from_home(dir2.path()).admission, AdmissionMode::Queue);

        // Absent config ⇒ default is Queue (the H19 behavior change).
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(AdmissionConfig::from_home(empty.path()).admission, AdmissionMode::Queue);
    }

    #[test]
    fn coexists_with_unrelated_dispatch_keys() {
        // The same [dispatch] table also carries dispatch-engine's `enabled`
        // and concurrency_gate's `personal_max_concurrent` in production —
        // this struct must ignore both and still parse its own fields.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch]\nenabled = true\npolicy = \"fixed_hierarchy\"\n\
             personal_max_concurrent = 2\nadmission = \"fail\"\nqueue_max_depth = 10\n",
        )
        .unwrap();
        let c = AdmissionConfig::from_home(dir.path());
        assert_eq!(c.admission, AdmissionMode::Fail);
        assert_eq!(c.queue_max_depth, 10);
    }

    #[test]
    fn enqueue_then_fifo_dequeue() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(AdmissionMode::Queue, 10, 600);
        let a = enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({"n": 1})).unwrap();
        let b = enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({"n": 2})).unwrap();
        let (a_id, a_pos) = match a {
            EnqueueOutcome::Queued { ticket_id, position } => (ticket_id, position),
            other => panic!("expected Queued, got {other:?}"),
        };
        let (_b_id, b_pos) = match b {
            EnqueueOutcome::Queued { ticket_id, position } => (ticket_id, position),
            other => panic!("expected Queued, got {other:?}"),
        };
        assert_eq!(a_pos, 1);
        assert_eq!(b_pos, 2);
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 2);

        // FIFO: the first-enqueued ticket comes out first.
        let d1 = dequeue_next(dir.path(), "ephemeral");
        assert_eq!(d1.ticket.as_ref().unwrap().ticket_id, a_id);
        assert!(d1.expired.is_empty());
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 1);

        let d2 = dequeue_next(dir.path(), "ephemeral");
        assert_eq!(d2.ticket.unwrap().payload, serde_json::json!({"n": 2}));
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 0);

        // Nothing left.
        let d3 = dequeue_next(dir.path(), "ephemeral");
        assert!(d3.ticket.is_none());
    }

    #[test]
    fn queue_full_is_rejected_with_clear_reason() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(AdmissionMode::Queue, 2, 600);
        assert!(matches!(
            enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap(),
            EnqueueOutcome::Queued { .. }
        ));
        assert!(matches!(
            enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap(),
            EnqueueOutcome::Queued { .. }
        ));
        // Third overflows the depth-2 queue.
        match enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap() {
            EnqueueOutcome::Rejected { reason } => {
                assert!(reason.contains("full"), "got: {reason}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 2);
    }

    #[test]
    fn admission_fail_never_queues() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(AdmissionMode::Fail, 10, 600);
        match enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap() {
            EnqueueOutcome::Rejected { reason } => {
                assert!(reason.contains("'fail'"), "got: {reason}");
            }
            other => panic!("expected Rejected under admission=fail, got {other:?}"),
        }
        assert_eq!(
            queue_depth(dir.path(), "ephemeral"),
            0,
            "admission=fail must never persist a ticket (fallback to legacy hard-reject)"
        );
    }

    #[test]
    fn expired_tickets_are_dropped_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        // TTL 0 ⇒ already expired the instant it is written.
        let c_expiring = cfg(AdmissionMode::Queue, 10, 0);
        let a =
            enqueue(dir.path(), "ephemeral", &c_expiring, None, serde_json::json!({"n": "a"}))
                .unwrap();
        assert!(matches!(a, EnqueueOutcome::Queued { .. }));

        // A fresh, non-expiring ticket enqueued after.
        let c_live = cfg(AdmissionMode::Queue, 10, 600);
        let b = enqueue(dir.path(), "ephemeral", &c_live, None, serde_json::json!({"n": "b"}))
            .unwrap();
        assert!(matches!(b, EnqueueOutcome::Queued { .. }));

        // dequeue_next must report the expired one as dropped, and return the
        // live one as the actual admitted ticket.
        let d = dequeue_next(dir.path(), "ephemeral");
        assert_eq!(d.expired.len(), 1);
        assert_eq!(d.expired[0].payload, serde_json::json!({"n": "a"}));
        assert_eq!(d.ticket.unwrap().payload, serde_json::json!({"n": "b"}));
    }

    #[test]
    fn sweep_expired_reports_without_requiring_a_dequeue() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(AdmissionMode::Queue, 10, 0);
        enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap();
        enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap();

        let swept = sweep_expired(dir.path(), "ephemeral");
        assert_eq!(swept.len(), 2);
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 0);

        // Idempotent — nothing left to sweep.
        assert!(sweep_expired(dir.path(), "ephemeral").is_empty());
    }

    #[test]
    fn invalidate_owner_removes_only_matching_tickets() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(AdmissionMode::Queue, 10, 600);
        enqueue(dir.path(), "ephemeral", &c, Some("turn-1"), serde_json::json!({"n": 1}))
            .unwrap();
        enqueue(dir.path(), "ephemeral", &c, Some("turn-2"), serde_json::json!({"n": 2}))
            .unwrap();
        enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({"n": 3})).unwrap();
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 3);

        let removed = invalidate_owner(dir.path(), "ephemeral", "turn-1");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].payload, serde_json::json!({"n": 1}));
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 2);

        // Invalidating an owner with nothing queued is a safe no-op.
        assert!(invalidate_owner(dir.path(), "ephemeral", "turn-nonexistent").is_empty());
        // Empty owner key is a safe no-op (never mass-invalidate unscoped tickets).
        assert!(invalidate_owner(dir.path(), "ephemeral", "").is_empty());
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 2);
    }

    #[test]
    fn classes_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(AdmissionMode::Queue, 1, 600);
        assert!(matches!(
            enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap(),
            EnqueueOutcome::Queued { .. }
        ));
        // A different class has its own budget even though "ephemeral" is full.
        assert!(matches!(
            enqueue(dir.path(), "other", &c, None, serde_json::json!({})).unwrap(),
            EnqueueOutcome::Queued { .. }
        ));
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 1);
        assert_eq!(queue_depth(dir.path(), "other"), 1);
        // dequeue on "other" must not touch "ephemeral"'s ticket.
        let d = dequeue_next(dir.path(), "other");
        assert!(d.ticket.is_some());
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 1);
    }

    #[test]
    fn corrupt_state_is_treated_as_empty_fail_safe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), b"{not json").unwrap();
        let c = cfg(AdmissionMode::Queue, 10, 600);
        // Corrupt file ⇒ fresh (empty) ⇒ still enqueues successfully.
        assert!(matches!(
            enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({})).unwrap(),
            EnqueueOutcome::Queued { .. }
        ));
    }

    #[test]
    fn cross_process_share_one_queue() {
        // Two independent enqueue calls against the SAME home dir simulate
        // two processes (e.g. the CLI MCP server and the gateway) sharing the
        // durable queue file.
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(AdmissionMode::Queue, 10, 600);
        enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({"n": 1})).unwrap();
        enqueue(dir.path(), "ephemeral", &c, None, serde_json::json!({"n": 2})).unwrap();
        assert_eq!(queue_depth(dir.path(), "ephemeral"), 2);
    }
}
