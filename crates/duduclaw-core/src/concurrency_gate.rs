//! Cross-process **in-flight concurrency** counter for edition-differentiated
//! goal-dispatch limiting (RFC-27).
//!
//! ## Why this exists, and how it differs from `dispatch_guard`
//!
//! [`crate::dispatch_guard`] is a *rate* limiter: it bounds how many dispatch
//! events happen per rolling window (a spawn-storm brake, arXiv:2607.01641).
//! This module is its *concurrency* twin: it bounds how many dispatches are
//! **simultaneously in-flight** — a resource brake that differentiates the
//! Personal edition (a small default cap) from Enterprise (unlimited), per the
//! D1-C / D2-B decision in `09-edition-split-features.md`. The two are
//! orthogonal and both apply; the stricter wins.
//!
//! A concurrency count needs an explicit *release* that a windowed rate limiter
//! never does. So instead of aging events out of a window, every in-flight unit
//! holds a **lease** with a TTL:
//!
//! - **acquire** on admission (atomic under the file lock — no cross-process
//!   read-then-admit race),
//! - **release** when the unit reaches a terminal state,
//! - **renew** periodically while still in-flight, and
//! - **TTL reclaim** so a crashed holder's lease self-expires and the count
//!   heals without operator action.
//!
//! ## Failure posture (fail-open)
//!
//! This is a commercial resource brake, **not** a security gate. If its own
//! state file / lock cannot be used, [`try_acquire`] returns an *unguarded*
//! lease (admit) — a broken counter file must never wedge autonomous goal
//! completion. This mirrors `dispatch_guard` exactly and is deliberately the
//! opposite of the fail-closed MCP authorization gate: CLAUDE.md's "security
//! gates fail closed" convention is scoped to *security* gates. Corrupt state is
//! treated as empty.
//!
//! Reused disciplines from `dispatch_guard` (project convention #3): every
//! read-modify-write is wrapped in [`crate::with_file_lock`]; saves are atomic
//! (temp + rename); expired leases are pruned on every acquire so the file
//! stays bounded.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::types::EditionProfile;

/// State-file name under `<home>`.
const STATE_FILE: &str = "concurrency_leases.json";

/// Env override for the Personal-edition cap (mirrors
/// `DUDUCLAW_PERSONAL_MAX_AGENTS`). A non-negative integer; `0` = unlimited.
const ENV_PERSONAL_MAX_CONCURRENT: &str = "DUDUCLAW_PERSONAL_MAX_CONCURRENT";

/// Tuning for the concurrency gate. Overridable via `config.toml [dispatch]`
/// (see [`ConcurrencyGateConfig::from_home`]).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct ConcurrencyGateConfig {
    /// Personal-edition simultaneous in-flight goal-dispatch cap. `0` =
    /// unlimited. Only meaningful when the active edition is Personal; the
    /// Enterprise edition is never subject to this (see [`effective_limit`]).
    pub personal_max_concurrent: u32,
    /// Crash-recovery TTL for a lease, in seconds. A lease that is not renewed
    /// within this window is reclaimed on the next acquire. Kept far above the
    /// driver's tick cadence (which renews every held lease) so a live in-flight
    /// task never loses its slot; a *crashed* holder's slot frees within the TTL.
    pub concurrency_lease_ttl_secs: u64,
}

impl Default for ConcurrencyGateConfig {
    fn default() -> Self {
        Self {
            // 2: the smallest cap that (a) is < the goal loop's in-process
            // `max_concurrent` default (3) — otherwise the edition gate would
            // be a no-op that never fires — and (b) leaves the common single-
            // and two-goal cases untouched, so a single owner barely notices
            // while a board of 3+ concurrent goals hits a visible, honest limit.
            personal_max_concurrent: 2,
            concurrency_lease_ttl_secs: 1800,
        }
    }
}

impl ConcurrencyGateConfig {
    /// Load `[dispatch]` tuning from `<home>/config.toml`. Parsed in isolation
    /// from a generic `toml::Table`, so unrelated / malformed config elsewhere
    /// can never make this fail — absent / malformed section ⇒ built-in
    /// defaults. The `DUDUCLAW_PERSONAL_MAX_CONCURRENT` env var overrides the
    /// `personal_max_concurrent` config value when set to a valid integer.
    pub fn from_home(home_dir: &Path) -> Self {
        let mut cfg = Self::from_config_only(home_dir);
        if let Some(v) = std::env::var(ENV_PERSONAL_MAX_CONCURRENT)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
        {
            cfg.personal_max_concurrent = v;
        }
        cfg
    }

    /// The config-file-only half of [`from_home`], without the env override.
    /// Separated so the env-precedence behavior is unit-testable in isolation.
    fn from_config_only(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("dispatch") {
            Some(section) => section
                .clone()
                .try_into::<ConcurrencyGateConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }
}

/// The effective simultaneous in-flight cap for an edition.
///
/// - **Personal** → `Some(cap)` from config, unless the cap is `0` (unlimited).
/// - **Enterprise** (and any future non-personal edition) → `None` (unlimited).
///
/// Pure so the write-time gate and any startup/observability caller consult one
/// identical decision without a live runtime. `None` means "do not gate".
pub fn effective_limit(edition: EditionProfile, cfg: &ConcurrencyGateConfig) -> Option<u32> {
    if edition.is_personal() {
        (cfg.personal_max_concurrent > 0).then_some(cfg.personal_max_concurrent)
    } else {
        None
    }
}

/// A held (or no-op) slot in a concurrency class.
///
/// A **guarded** lease carries an id and occupies one slot in the state file
/// until [`release`]d or TTL-reclaimed. An **unguarded** lease carries no id and
/// makes [`release`]/[`renew`] no-ops — every "we did not actually take a slot"
/// path (unlimited edition, or a fail-open on FS error) returns one, so callers
/// have a single uniform type and never branch on "did the gate apply".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    id: Option<String>,
    class: String,
}

impl Lease {
    /// A no-op lease that occupies no slot (`release`/`renew` do nothing).
    pub fn unguarded(class: &str) -> Self {
        Self {
            id: None,
            class: class.to_string(),
        }
    }

    /// `true` when this lease actually holds a slot in the state file.
    pub fn is_guarded(&self) -> bool {
        self.id.is_some()
    }

    /// The concurrency class this lease belongs to.
    pub fn class(&self) -> &str {
        &self.class
    }
}

/// Outcome of a [`try_acquire`] attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// A slot was taken (guarded) or the gate did not apply (unguarded). Either
    /// way the caller may proceed; hold the [`Lease`] and [`release`] it on the
    /// terminal state.
    Admitted(Lease),
    /// The class is already at its cap. The caller should **defer / queue**, not
    /// hard-reject — a durable goal must not be dropped by a throughput brake
    /// (RFC-27 §6).
    AtCapacity {
        /// Live in-flight count observed for the class.
        active: u32,
        /// The cap that was hit.
        limit: u32,
    },
}

impl AcquireOutcome {
    /// `true` when the class was at capacity (dispatch should defer).
    pub fn is_at_capacity(&self) -> bool {
        matches!(self, AcquireOutcome::AtCapacity { .. })
    }
}

/// One lease record. `expires_ms` is epoch milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseRec {
    class: String,
    expires_ms: i64,
}

type State = HashMap<String, LeaseRec>;

fn now_epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn load_state(path: &Path) -> State {
    // Missing or corrupt state ⇒ empty (fresh). Never propagate an error: a
    // resource brake must not break the caller because its own file is unreadable.
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => State::new(),
    }
}

fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Atomic replace (temp + rename) so a crash mid-write cannot leave a
    // truncated JSON file the next reader would discard.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

/// Drop expired leases so the file stays bounded and reclaims crashed holders.
fn prune_expired(state: &mut State, now_ms: i64) {
    state.retain(|_, rec| rec.expires_ms > now_ms);
}

/// Count live (unexpired) leases in `class`.
fn count_class(state: &State, class: &str, now_ms: i64) -> u32 {
    state
        .values()
        .filter(|r| r.class == class && r.expires_ms > now_ms)
        .count() as u32
}

/// Attempt to take one in-flight slot in `class`.
///
/// - `limit == None` ⇒ the gate does not apply: returns `Admitted(unguarded)`
///   with **zero file I/O** (the Enterprise / unlimited path pays nothing).
/// - `limit == Some(cap)` ⇒ under [`crate::with_file_lock`], expired leases are
///   pruned, live leases in `class` are counted, and if the count is `< cap` a
///   fresh lease is inserted (`Admitted(guarded)`), else `AtCapacity`.
/// - Any lock / FS failure ⇒ **fail open**: `Admitted(unguarded)`.
pub fn try_acquire(
    home_dir: &Path,
    class: &str,
    limit: Option<u32>,
    ttl_secs: u64,
) -> AcquireOutcome {
    let Some(cap) = limit else {
        // Unlimited: never touch the file.
        return AcquireOutcome::Admitted(Lease::unguarded(class));
    };
    // A zero cap means "unlimited" too (belt-and-suspenders — `effective_limit`
    // already maps 0 → None, but guard here so a direct caller can't self-DoS).
    if cap == 0 {
        return AcquireOutcome::Admitted(Lease::unguarded(class));
    }

    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let ttl_ms = ttl_secs.saturating_mul(1000) as i64;

    let result = crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune_expired(&mut state, now_ms);

        let active = count_class(&state, class, now_ms);
        if active >= cap {
            return Ok(AcquireOutcome::AtCapacity {
                active,
                limit: cap,
            });
        }

        let id = format!("{class}-{}", uuid::Uuid::new_v4());
        state.insert(
            id.clone(),
            LeaseRec {
                class: class.to_string(),
                expires_ms: now_ms + ttl_ms,
            },
        );
        let _ = save_state(&path, &state);
        Ok(AcquireOutcome::Admitted(Lease {
            id: Some(id),
            class: class.to_string(),
        }))
    });

    // Lock acquisition failure ⇒ fail-open (availability over strictness for a
    // resource brake; a broken lock file must not wedge every dispatch).
    result.unwrap_or_else(|_| AcquireOutcome::Admitted(Lease::unguarded(class)))
}

/// Release a lease, freeing its slot. No-op on an unguarded lease. Best-effort:
/// a lock/FS failure is swallowed because the TTL reclaims the slot anyway.
pub fn release(home_dir: &Path, lease: &Lease) {
    let Some(id) = lease.id.clone() else {
        return; // unguarded — nothing to release
    };
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let _ = crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune_expired(&mut state, now_ms);
        state.remove(&id);
        let _ = save_state(&path, &state);
        Ok::<(), std::io::Error>(())
    });
}

/// Extend a still-in-flight lease's TTL to `now + ttl_secs`. No-op on an
/// unguarded lease. If the lease has already been pruned (e.g. TTL lapsed under
/// heavy stall), it is re-inserted so the live task keeps counting toward its
/// own cap. Best-effort on lock/FS failure.
pub fn renew(home_dir: &Path, lease: &Lease, ttl_secs: u64) {
    let Some(id) = lease.id.clone() else {
        return; // unguarded — nothing to renew
    };
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    let ttl_ms = ttl_secs.saturating_mul(1000) as i64;
    let class = lease.class.clone();
    let _ = crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune_expired(&mut state, now_ms);
        state.insert(
            id.clone(),
            LeaseRec {
                class,
                expires_ms: now_ms + ttl_ms,
            },
        );
        let _ = save_state(&path, &state);
        Ok::<(), std::io::Error>(())
    });
}

/// Live in-flight count for `class` (for observability / tests). Prunes expired
/// leases as a side effect so a stale file does not over-report.
pub fn active_count(home_dir: &Path, class: &str) -> u32 {
    let path = home_dir.join(STATE_FILE);
    let now_ms = now_epoch_ms();
    crate::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune_expired(&mut state, now_ms);
        let n = count_class(&state, class, now_ms);
        let _ = save_state(&path, &state);
        Ok::<u32, std::io::Error>(n)
    })
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_limit_personal_vs_enterprise() {
        let cfg = ConcurrencyGateConfig {
            personal_max_concurrent: 2,
            concurrency_lease_ttl_secs: 1800,
        };
        assert_eq!(effective_limit(EditionProfile::Personal, &cfg), Some(2));
        assert_eq!(effective_limit(EditionProfile::Enterprise, &cfg), None);
        // Personal cap 0 ⇒ unlimited (the self-host escape hatch).
        let unlimited = ConcurrencyGateConfig {
            personal_max_concurrent: 0,
            ..cfg
        };
        assert_eq!(effective_limit(EditionProfile::Personal, &unlimited), None);
    }

    #[test]
    fn admits_under_cap_then_at_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let limit = Some(2);
        let ttl = 1800;
        let a = try_acquire(dir.path(), "goal", limit, ttl);
        let b = try_acquire(dir.path(), "goal", limit, ttl);
        assert!(matches!(a, AcquireOutcome::Admitted(ref l) if l.is_guarded()));
        assert!(matches!(b, AcquireOutcome::Admitted(ref l) if l.is_guarded()));
        // Third exceeds the cap of 2.
        let c = try_acquire(dir.path(), "goal", limit, ttl);
        assert!(c.is_at_capacity(), "3rd acquire must hit the cap");
        if let AcquireOutcome::AtCapacity { active, limit } = c {
            assert_eq!(active, 2);
            assert_eq!(limit, 2);
        }
        assert_eq!(active_count(dir.path(), "goal"), 2);
    }

    #[test]
    fn release_frees_a_slot() {
        let dir = tempfile::tempdir().unwrap();
        let limit = Some(1);
        let a = try_acquire(dir.path(), "goal", limit, 1800);
        let AcquireOutcome::Admitted(lease_a) = a else {
            panic!("first acquire must be admitted");
        };
        // At cap now.
        assert!(try_acquire(dir.path(), "goal", limit, 1800).is_at_capacity());
        // Release frees the single slot.
        release(dir.path(), &lease_a);
        assert_eq!(active_count(dir.path(), "goal"), 0);
        assert!(matches!(
            try_acquire(dir.path(), "goal", limit, 1800),
            AcquireOutcome::Admitted(_)
        ));
    }

    #[test]
    fn expired_leases_are_reclaimed_on_next_acquire() {
        let dir = tempfile::tempdir().unwrap();
        let limit = Some(1);
        // TTL 0 ⇒ the lease is already expired the instant it is written.
        let a = try_acquire(dir.path(), "goal", limit, 0);
        assert!(matches!(a, AcquireOutcome::Admitted(_)));
        // Next acquire prunes the expired lease and admits (crash recovery).
        let b = try_acquire(dir.path(), "goal", limit, 1800);
        assert!(
            matches!(b, AcquireOutcome::Admitted(ref l) if l.is_guarded()),
            "an expired holder's slot must be reclaimed"
        );
    }

    #[test]
    fn renew_keeps_a_lease_alive_past_its_original_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let limit = Some(1);
        // Short-lived lease.
        let AcquireOutcome::Admitted(lease) = try_acquire(dir.path(), "goal", limit, 0) else {
            panic!("acquire must be admitted");
        };
        // Without renew it would be reclaimed; renew re-establishes a live slot.
        renew(dir.path(), &lease, 1800);
        assert_eq!(active_count(dir.path(), "goal"), 1);
        // The single slot is now held by the renewed lease.
        assert!(try_acquire(dir.path(), "goal", limit, 1800).is_at_capacity());
    }

    #[test]
    fn unlimited_never_touches_the_file() {
        let dir = tempfile::tempdir().unwrap();
        // limit None ⇒ unguarded, no file written.
        let a = try_acquire(dir.path(), "goal", None, 1800);
        assert!(matches!(a, AcquireOutcome::Admitted(ref l) if !l.is_guarded()));
        assert!(
            !dir.path().join(STATE_FILE).exists(),
            "unlimited path must not create the state file"
        );
        // Release/renew on an unguarded lease are no-ops (and still no file).
        if let AcquireOutcome::Admitted(lease) = a {
            release(dir.path(), &lease);
            renew(dir.path(), &lease, 1800);
        }
        assert!(!dir.path().join(STATE_FILE).exists());
    }

    #[test]
    fn cap_zero_is_treated_as_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let a = try_acquire(dir.path(), "goal", Some(0), 1800);
        assert!(matches!(a, AcquireOutcome::Admitted(ref l) if !l.is_guarded()));
    }

    #[test]
    fn corrupt_state_is_treated_as_empty_fail_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), b"{not json").unwrap();
        // Corrupt file ⇒ fresh (empty) ⇒ admit.
        let a = try_acquire(dir.path(), "goal", Some(1), 1800);
        assert!(matches!(a, AcquireOutcome::Admitted(ref l) if l.is_guarded()));
    }

    #[test]
    fn classes_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let limit = Some(1);
        assert!(matches!(
            try_acquire(dir.path(), "goal", limit, 1800),
            AcquireOutcome::Admitted(_)
        ));
        // A different class has its own budget.
        assert!(matches!(
            try_acquire(dir.path(), "other", limit, 1800),
            AcquireOutcome::Admitted(_)
        ));
        // But the goal class is now at cap.
        assert!(try_acquire(dir.path(), "goal", limit, 1800).is_at_capacity());
    }

    #[test]
    fn cross_process_share_one_count() {
        // Two independent acquire calls against the SAME home dir simulate two
        // processes; the second sees the first's lease through the shared file.
        let dir = tempfile::tempdir().unwrap();
        let limit = Some(1);
        let first = try_acquire(dir.path(), "goal", limit, 1800);
        assert!(matches!(first, AcquireOutcome::Admitted(ref l) if l.is_guarded()));
        // "Second process" reads the same file and is refused.
        let second = try_acquire(dir.path(), "goal", limit, 1800);
        assert!(
            second.is_at_capacity(),
            "a second process must observe the first's in-flight lease"
        );
    }

    #[test]
    fn config_from_home_and_env_override() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch]\npersonal_max_concurrent = 4\n",
        )
        .unwrap();
        // Config-only read picks up the section; TTL keeps its default.
        let c = ConcurrencyGateConfig::from_config_only(dir.path());
        assert_eq!(c.personal_max_concurrent, 4);
        assert_eq!(c.concurrency_lease_ttl_secs, 1800);

        // Absent file ⇒ all defaults.
        let empty = tempfile::tempdir().unwrap();
        let d = ConcurrencyGateConfig::from_config_only(empty.path());
        assert_eq!(d.personal_max_concurrent, 2);
    }
}
