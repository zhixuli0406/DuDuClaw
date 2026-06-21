//! Budget enforcement — per-branch caps plus an aggregate cap across all branches.
//!
//! RFC-26 §3.3: branches run with independent `budget_usd` caps, but an aggregate
//! ceiling is enforced across all branches simultaneously. A branch whose next
//! charge would breach either cap is denied and transitioned to `BudgetKilled`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::branch::BranchId;

/// Outcome of attempting to charge spend against the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    /// Charge accepted; branch may continue.
    Allowed,
    /// Per-branch cap would be exceeded.
    BranchExceeded,
    /// Aggregate cap would be exceeded.
    AggregateExceeded,
}

impl Charge {
    pub fn is_allowed(self) -> bool {
        matches!(self, Charge::Allowed)
    }
}

/// Thread-safe shared budget pool. Cheap to share across branch tasks via `Arc`.
#[derive(Debug)]
pub struct Pool {
    aggregate_cap_usd: f64,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    aggregate_spent: f64,
    per_branch_cap: HashMap<BranchId, f64>,
    per_branch_spent: HashMap<BranchId, f64>,
}

impl Pool {
    /// Create a pool with an aggregate ceiling across all branches.
    pub fn new(aggregate_cap_usd: f64) -> Self {
        Pool {
            aggregate_cap_usd: aggregate_cap_usd.max(0.0),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Register a branch with its own per-branch cap before it spends.
    pub fn register(&self, id: BranchId, branch_cap_usd: f64) {
        let mut inner = self.inner.lock().expect("budget pool poisoned");
        inner.per_branch_cap.insert(id.clone(), branch_cap_usd.max(0.0));
        inner.per_branch_spent.entry(id).or_insert(0.0);
    }

    /// Attempt to charge `amount_usd` to a branch. On `Allowed` the spend is
    /// committed; on any rejection nothing is recorded (fail-closed — the caller
    /// must stop the branch).
    pub fn try_charge(&self, id: &BranchId, amount_usd: f64) -> Charge {
        let amount = amount_usd.max(0.0);
        let mut inner = self.inner.lock().expect("budget pool poisoned");

        let branch_cap = inner.per_branch_cap.get(id).copied().unwrap_or(0.0);
        let branch_spent = inner.per_branch_spent.get(id).copied().unwrap_or(0.0);

        if branch_spent + amount > branch_cap {
            return Charge::BranchExceeded;
        }
        if inner.aggregate_spent + amount > self.aggregate_cap_usd {
            return Charge::AggregateExceeded;
        }

        inner.aggregate_spent += amount;
        *inner.per_branch_spent.entry(id.clone()).or_insert(0.0) += amount;
        Charge::Allowed
    }

    /// Total committed spend across all branches.
    pub fn aggregate_spent(&self) -> f64 {
        self.inner.lock().expect("budget pool poisoned").aggregate_spent
    }

    /// Committed spend for one branch.
    pub fn branch_spent(&self, id: &BranchId) -> f64 {
        self.inner
            .lock()
            .expect("budget pool poisoned")
            .per_branch_spent
            .get(id)
            .copied()
            .unwrap_or(0.0)
    }
}

// ── Streaming-time aggregate pre-emption (RFC-26 §4.2) ──────────────────────

/// What [`LiveAggregate::observe`] decides after a branch reports new spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preempt {
    /// Combined live spend is within the aggregate cap — keep streaming.
    Ok,
    /// Combined live spend crossed the cap. Kill this branch id (the single
    /// most-expensive *in-flight* branch) to bring the aggregate back under
    /// budget while sacrificing the fewest branches.
    Kill(String),
}

/// Streaming-time aggregate budget tracker (RFC-26 §4.2).
///
/// Distinct from [`Pool`], which does post-completion charge accounting:
/// `LiveAggregate` watches the *in-flight* cumulative cost of every concurrently
/// running branch and, the moment their combined live spend crosses the
/// aggregate cap, names the most-expensive in-flight branch so the caller can
/// pre-emptively kill it mid-stream — rather than waiting for each branch to hit
/// its own per-branch cap. Cheap to share across branch tasks via `Arc`.
#[derive(Debug)]
pub struct LiveAggregate {
    cap_usd: f64,
    live: Mutex<HashMap<String, f64>>,
}

impl LiveAggregate {
    /// Create a tracker with the aggregate ceiling across all branches.
    pub fn new(cap_usd: f64) -> Self {
        LiveAggregate {
            cap_usd: cap_usd.max(0.0),
            live: Mutex::new(HashMap::new()),
        }
    }

    /// Record `branch_id`'s latest cumulative running cost and decide whether a
    /// pre-emption is needed. Fail-closed: when the combined live spend exceeds
    /// the cap, always names a victim — the max-cost in-flight branch, with ties
    /// broken deterministically by id so the choice is independent of HashMap
    /// iteration order. A NaN cost is sanitized to 0.0.
    pub fn observe(&self, branch_id: &str, running_cost_usd: f64) -> Preempt {
        let mut live = self.live.lock().expect("live aggregate poisoned");
        live.insert(branch_id.to_string(), running_cost_usd.max(0.0));
        let total: f64 = live.values().sum();
        if total <= self.cap_usd {
            return Preempt::Ok;
        }
        // Total order on (cost, id): pick the highest cost, then the lexically
        // largest id among equal costs — deterministic regardless of order.
        let victim = live
            .iter()
            .max_by(|(ka, va), (kb, vb)| {
                va.partial_cmp(vb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| ka.cmp(kb))
            })
            .map(|(k, _)| k.clone());
        match victim {
            Some(v) => Preempt::Kill(v),
            None => Preempt::Ok,
        }
    }

    /// Stop counting a branch once it has finished or been killed, so the
    /// remaining branches' combined spend reflects only live work. Idempotent.
    pub fn finish(&self, branch_id: &str) {
        self.live.lock().expect("live aggregate poisoned").remove(branch_id);
    }

    /// Current combined live spend across in-flight branches.
    pub fn live_total(&self) -> f64 {
        self.live.lock().expect("live aggregate poisoned").values().sum()
    }

    /// The aggregate ceiling (clamped to ≥ 0 at construction).
    pub fn cap_usd(&self) -> f64 {
        self.cap_usd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_within_caps_allowed() {
        let pool = Pool::new(10.0);
        let b = BranchId::new();
        pool.register(b.clone(), 5.0);
        assert_eq!(pool.try_charge(&b, 2.0), Charge::Allowed);
        assert_eq!(pool.try_charge(&b, 2.0), Charge::Allowed);
        assert_eq!(pool.branch_spent(&b), 4.0);
        assert_eq!(pool.aggregate_spent(), 4.0);
    }

    #[test]
    fn per_branch_cap_enforced() {
        let pool = Pool::new(100.0);
        let b = BranchId::new();
        pool.register(b.clone(), 5.0);
        assert_eq!(pool.try_charge(&b, 4.0), Charge::Allowed);
        // Next charge would breach the per-branch cap of 5.0.
        assert_eq!(pool.try_charge(&b, 2.0), Charge::BranchExceeded);
        // Rejected charge is not committed.
        assert_eq!(pool.branch_spent(&b), 4.0);
    }

    #[test]
    fn aggregate_cap_enforced_across_branches() {
        let pool = Pool::new(6.0);
        let a = BranchId::new();
        let b = BranchId::new();
        pool.register(a.clone(), 100.0);
        pool.register(b.clone(), 100.0);
        assert_eq!(pool.try_charge(&a, 4.0), Charge::Allowed);
        // a+b would be 9.0 > aggregate 6.0
        assert_eq!(pool.try_charge(&b, 5.0), Charge::AggregateExceeded);
        assert_eq!(pool.try_charge(&b, 2.0), Charge::Allowed);
        assert_eq!(pool.aggregate_spent(), 6.0);
    }

    #[test]
    fn unregistered_branch_has_zero_cap() {
        let pool = Pool::new(10.0);
        let ghost = BranchId::new();
        // No register() ⇒ per-branch cap defaults to 0 ⇒ any positive charge denied.
        assert_eq!(pool.try_charge(&ghost, 0.01), Charge::BranchExceeded);
    }

    // ── LiveAggregate (streaming pre-emption) ───────────────────────────────

    #[test]
    fn live_aggregate_within_cap_is_ok() {
        let agg = LiveAggregate::new(1.0);
        assert_eq!(agg.observe("a", 0.3), Preempt::Ok);
        assert_eq!(agg.observe("b", 0.3), Preempt::Ok);
        assert!((agg.live_total() - 0.6).abs() < 1e-9);
    }

    #[test]
    fn live_aggregate_over_cap_kills_most_expensive() {
        let agg = LiveAggregate::new(1.0);
        assert_eq!(agg.observe("cheap", 0.2), Preempt::Ok);
        // cheap(0.2) + pricey(0.9) = 1.1 > 1.0 ⇒ kill the pricey one.
        assert_eq!(agg.observe("pricey", 0.9), Preempt::Kill("pricey".into()));
    }

    #[test]
    fn live_aggregate_tie_break_is_deterministic() {
        // Two equally-expensive branches over cap ⇒ lexically-largest id wins,
        // independent of insertion order.
        let agg = LiveAggregate::new(1.0);
        agg.observe("aaa", 0.6);
        assert_eq!(agg.observe("zzz", 0.6), Preempt::Kill("zzz".into()));

        let agg2 = LiveAggregate::new(1.0);
        agg2.observe("zzz", 0.6);
        assert_eq!(agg2.observe("aaa", 0.6), Preempt::Kill("zzz".into()));
    }

    #[test]
    fn live_aggregate_finish_drops_branch_from_total() {
        let agg = LiveAggregate::new(1.0);
        agg.observe("a", 0.6);
        agg.observe("b", 0.6); // total 1.2 > cap, but we kill via observe normally
        agg.finish("b");
        assert!((agg.live_total() - 0.6).abs() < 1e-9);
        // With b gone, a fresh observe of a within cap is Ok again.
        assert_eq!(agg.observe("a", 0.7), Preempt::Ok);
    }

    #[test]
    fn live_aggregate_sanitizes_nan_and_negative() {
        let agg = LiveAggregate::new(1.0);
        assert_eq!(agg.observe("a", f64::NAN), Preempt::Ok);
        assert_eq!(agg.observe("b", -5.0), Preempt::Ok);
        assert_eq!(agg.live_total(), 0.0);
    }
}
