// mcp_memory_quota.rs — Per-client daily write quota for MCP server (W19-P0 M1)
//
// Tracks how many memory records each external client has written today.
// Quota resets at UTC midnight. Default limit: 1,000 records / day.
//
// Thread-safe: inner state is Arc<Mutex<_>> so `DailyQuota` can be cloned
// and shared across async tasks without external wrapping.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;

// ── Constants ─────────────────────────────────────────────────────────────────

const DEFAULT_DAILY_LIMIT: u64 = 1_000;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub struct QuotaExceededError {
    /// The configured daily limit that was reached.
    pub limit: u64,
    /// How many records have been written today.
    pub count: u64,
}

impl std::fmt::Display for QuotaExceededError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Daily write quota exceeded ({} records/day)",
            self.limit
        )
    }
}

// ── DailyQuota ────────────────────────────────────────────────────────────────

/// Per-client daily write quota tracker.
///
/// # Design
/// - Keeps an in-memory `HashMap<client_id, (date, count)>`.
/// - On every `check_and_increment` call, verifies the stored date equals
///   today (UTC); if not, the counter is reset (new day).
/// - No persistence across process restarts — acceptable because quota is a
///   soft DoS guard, not a billing constraint.
#[derive(Clone, Debug)]
pub struct DailyQuota {
    /// `(NaiveDate, count)` keyed by client_id.
    state: Arc<Mutex<HashMap<String, (NaiveDate, u64)>>>,
    /// Maximum records allowed per client per UTC day.
    limit: u64,
}

impl DailyQuota {
    /// Create a quota tracker with the default limit (1 000 records/day).
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_DAILY_LIMIT)
    }

    /// Create a quota tracker with a custom limit (useful for tests).
    pub fn with_limit(limit: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            limit,
        }
    }

    /// Try to consume one quota unit for `client_id`.
    ///
    /// Returns `Ok(remaining)` when the write is within quota.
    /// Returns `Err(QuotaExceededError)` when the daily limit has been reached.
    ///
    /// The counter automatically resets when a new UTC day begins.
    pub fn check_and_increment(&self, client_id: &str) -> Result<u64, QuotaExceededError> {
        let today: NaiveDate = chrono::Utc::now().date_naive();
        let mut map = self.state.lock().expect("DailyQuota lock poisoned");

        let entry = map
            .entry(client_id.to_string())
            .or_insert((today, 0));

        // Reset if a new UTC day has started.
        if entry.0 != today {
            *entry = (today, 0);
        }

        if entry.1 >= self.limit {
            return Err(QuotaExceededError {
                limit: self.limit,
                count: entry.1,
            });
        }

        entry.1 += 1;
        let remaining = self.limit.saturating_sub(entry.1);
        Ok(remaining)
    }

    /// Return how many records `client_id` has written today (UTC).
    ///
    /// Used for diagnostics and testing; does **not** modify the counter.
    pub fn count_today(&self, client_id: &str) -> u64 {
        let today: NaiveDate = chrono::Utc::now().date_naive();
        let map = self.state.lock().expect("DailyQuota lock poisoned");
        map.get(client_id)
            .filter(|(date, _)| *date == today)
            .map(|(_, count)| *count)
            .unwrap_or(0)
    }
}

impl Default for DailyQuota {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 1: first write is allowed ────────────────────────────────────────
    #[test]
    fn first_write_is_allowed() {
        let q = DailyQuota::with_limit(10);
        assert!(q.check_and_increment("client-a").is_ok());
    }

    // ── Test 2: count_today increments after each write ───────────────────────
    #[test]
    fn count_today_reflects_writes() {
        let q = DailyQuota::with_limit(10);
        q.check_and_increment("client-b").unwrap();
        q.check_and_increment("client-b").unwrap();
        assert_eq!(q.count_today("client-b"), 2);
    }

    // ── Test 3: exactly-at-limit write succeeds, remaining = 0 ───────────────
    #[test]
    fn write_at_limit_succeeds_with_zero_remaining() {
        let q = DailyQuota::with_limit(3);
        q.check_and_increment("c").unwrap();
        q.check_and_increment("c").unwrap();
        let remaining = q.check_and_increment("c").expect("3rd of 3 should succeed");
        assert_eq!(remaining, 0, "no quota should remain after reaching the limit");
    }

    // ── Test 4: write beyond limit returns QuotaExceededError ─────────────────
    #[test]
    fn write_beyond_limit_is_rejected() {
        let q = DailyQuota::with_limit(3);
        for _ in 0..3 {
            q.check_and_increment("d").unwrap();
        }
        let err = q.check_and_increment("d").unwrap_err();
        assert_eq!(err.limit, 3, "error should carry the configured limit");
        assert_eq!(err.count, 3, "error should carry the current count");
    }

    // ── Test 5: different clients are isolated ────────────────────────────────
    #[test]
    fn clients_have_independent_quotas() {
        let q = DailyQuota::with_limit(2);
        q.check_and_increment("client-x").unwrap();
        q.check_and_increment("client-x").unwrap();
        // client-x is now exhausted
        assert!(
            q.check_and_increment("client-x").is_err(),
            "client-x should be rate-limited"
        );
        // client-y has never written — full quota still available
        assert!(
            q.check_and_increment("client-y").is_ok(),
            "client-y should be unaffected by client-x's exhaustion"
        );
    }

    // ── Test 6: remaining count decreases monotonically ───────────────────────
    #[test]
    fn remaining_decreases_monotonically() {
        let q = DailyQuota::with_limit(5);
        let r1 = q.check_and_increment("e").unwrap();
        let r2 = q.check_and_increment("e").unwrap();
        let r3 = q.check_and_increment("e").unwrap();
        assert_eq!(r1, 4);
        assert_eq!(r2, 3);
        assert_eq!(r3, 2);
    }

    // ── Test 7: brand-new client count starts at zero ─────────────────────────
    #[test]
    fn new_client_count_is_zero() {
        let q = DailyQuota::new();
        assert_eq!(q.count_today("never-seen-before"), 0);
    }

    // ── Test 8: QuotaExceededError Display contains the limit ─────────────────
    #[test]
    fn error_display_mentions_limit() {
        let err = QuotaExceededError { limit: 1_000, count: 1_000 };
        let msg = err.to_string();
        assert!(
            msg.contains("1000"),
            "Display should include the limit, got: {msg}"
        );
    }

    // ── Test 9: cloned quota shares the same counter ──────────────────────────
    #[test]
    fn cloned_quota_shares_state() {
        let q1 = DailyQuota::with_limit(5);
        let q2 = q1.clone();
        q1.check_and_increment("shared").unwrap();
        q2.check_and_increment("shared").unwrap();
        // Both increments should be visible via either handle
        assert_eq!(q1.count_today("shared"), 2);
        assert_eq!(q2.count_today("shared"), 2);
    }
}
