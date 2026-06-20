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
}
