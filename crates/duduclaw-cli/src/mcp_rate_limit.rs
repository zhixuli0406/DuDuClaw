// mcp_rate_limit.rs — Token bucket rate limiter for MCP server (W19-P0 M2)
//
// Implements per-client, per-operation-type rate limiting using the token
// bucket algorithm backed by an in-memory Mutex<HashMap>.
//
// Read:        100 req/min (capacity=100, refill=100/60 tokens/s)
// Write:        20 req/min (capacity=20,  refill=20/60  tokens/s)
// HttpRequest:  60 req/min (capacity=60,  refill=1.0    tokens/s) — W20-P1 HTTP gate

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum OpType {
    Read,
    Write,
    /// Per-key HTTP request gate (W20-P1): 60 req/min, independent of Read/Write buckets.
    /// Applied at the HTTP transport layer before OpType::Read/Write checks.
    HttpRequest,
}

#[derive(Debug)]
pub struct RateLimitError {
    pub retry_after_secs: u64,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Rate limited, retry after {} seconds", self.retry_after_secs)
    }
}

// ── Internal token bucket ─────────────────────────────────────────────────────

struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Refill tokens based on elapsed time, then attempt to consume one token.
    /// Returns `Ok(())` on success, or `Err(retry_after_secs)` when empty.
    fn try_consume(&mut self) -> Result<(), u64> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            // How many seconds until at least 1 token is available
            let deficit = 1.0 - self.tokens;
            let secs = (deficit / self.refill_rate).ceil() as u64;
            Err(secs.max(1))
        }
    }
}

// ── Bucket key helper ─────────────────────────────────────────────────────────

fn bucket_key(client_id: &str, op: &OpType) -> String {
    let op_str = match op {
        OpType::Read => "r",
        OpType::Write => "w",
        OpType::HttpRequest => "h",
    };
    format!("{client_id}:{op_str}")
}

// ── RateLimiter ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RateLimiter {
    state: Arc<Mutex<HashMap<String, TokenBucket>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check whether `client_id` is within their rate limit for `op`.
    ///
    /// Returns `Ok(())` if the request is allowed, or
    /// `Err(RateLimitError { retry_after_secs })` if the bucket is exhausted.
    pub fn check(&self, client_id: &str, op: OpType) -> Result<(), RateLimitError> {
        let key = bucket_key(client_id, &op);
        let (capacity, refill_rate) = match op {
            OpType::Read => (100.0_f64, 100.0 / 60.0),
            OpType::Write => (20.0_f64, 20.0 / 60.0),
            // HTTP gate: 60 req/min, refill 1 token/s
            OpType::HttpRequest => (60.0_f64, 1.0_f64),
        };

        let mut map = self.state.lock().unwrap();
        let bucket = map
            .entry(key)
            .or_insert_with(|| TokenBucket::new(capacity, refill_rate));

        bucket.try_consume().map_err(|secs| RateLimitError {
            retry_after_secs: secs,
        })
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 1: Read bucket allows 100 requests, blocks the 101st ────────────
    #[test]
    fn read_bucket_allows_100_then_rejects() {
        let limiter = RateLimiter::new();
        let client = "test-client-read";

        // First 100 reads must pass
        for i in 1..=100 {
            assert!(
                limiter.check(client, OpType::Read).is_ok(),
                "request {i} should be allowed"
            );
        }

        // 101st must be rejected
        let result = limiter.check(client, OpType::Read);
        assert!(
            result.is_err(),
            "101st read request should be rate-limited"
        );
    }

    // ── Test 2: Write bucket allows 20 requests, blocks the 21st ─────────────
    #[test]
    fn write_bucket_allows_20_then_rejects() {
        let limiter = RateLimiter::new();
        let client = "test-client-write";

        // First 20 writes must pass
        for i in 1..=20 {
            assert!(
                limiter.check(client, OpType::Write).is_ok(),
                "write request {i} should be allowed"
            );
        }

        // 21st must be rejected
        let result = limiter.check(client, OpType::Write);
        assert!(
            result.is_err(),
            "21st write request should be rate-limited"
        );
    }

    // ── Test 3: Different clients are isolated ────────────────────────────────
    #[test]
    fn different_clients_are_isolated() {
        let limiter = RateLimiter::new();

        // Exhaust client_a's read bucket
        for _ in 0..100 {
            let _ = limiter.check("client_a", OpType::Read);
        }
        // client_a should now be blocked
        assert!(
            limiter.check("client_a", OpType::Read).is_err(),
            "client_a should be rate-limited after 100 reads"
        );

        // client_b has never been called — should still pass
        assert!(
            limiter.check("client_b", OpType::Read).is_ok(),
            "client_b should not be affected by client_a's exhaustion"
        );
    }

    // ── Test 4: retry_after_secs is positive ──────────────────────────────────
    #[test]
    fn retry_after_secs_is_positive() {
        let limiter = RateLimiter::new();
        let client = "test-client-retry";

        // Exhaust the write bucket
        for _ in 0..20 {
            let _ = limiter.check(client, OpType::Write);
        }

        let err = limiter
            .check(client, OpType::Write)
            .expect_err("should be rate-limited");

        assert!(
            err.retry_after_secs > 0,
            "retry_after_secs must be > 0, got {}",
            err.retry_after_secs
        );
    }

    // ── Test 5: error Display is non-empty ────────────────────────────────────
    #[test]
    fn rate_limit_error_display_non_empty() {
        let err = RateLimitError { retry_after_secs: 42 };
        let msg = err.to_string();
        assert!(!msg.is_empty(), "Display should not be empty");
        assert!(
            msg.contains("42"),
            "Display should include retry_after_secs value"
        );
    }

    // ── Test 6: Read and Write buckets are independent per client ─────────────
    #[test]
    fn read_and_write_buckets_are_independent() {
        let limiter = RateLimiter::new();
        let client = "test-client-rw";

        // Exhaust the write bucket
        for _ in 0..20 {
            let _ = limiter.check(client, OpType::Write);
        }
        assert!(
            limiter.check(client, OpType::Write).is_err(),
            "Write bucket should be exhausted"
        );

        // Read bucket should still be full (80 tokens remain untouched)
        assert!(
            limiter.check(client, OpType::Read).is_ok(),
            "Read bucket should be unaffected by write bucket exhaustion"
        );
    }

    // ── Test 7: HttpRequest gate allows 60 requests, blocks the 61st ─────────
    #[test]
    fn http_gate_allows_60_then_rejects() {
        let limiter = RateLimiter::new();
        let client = "test-client-http";

        // First 60 HTTP requests must pass
        for i in 1..=60 {
            assert!(
                limiter.check(client, OpType::HttpRequest).is_ok(),
                "HTTP request {i} should be allowed"
            );
        }

        // 61st must be rejected
        let result = limiter.check(client, OpType::HttpRequest);
        assert!(
            result.is_err(),
            "61st HTTP request should be rate-limited"
        );
        let err = result.unwrap_err();
        assert!(err.retry_after_secs > 0, "retry_after_secs must be > 0");
    }

    // ── Test 8: HttpRequest gate is independent from Read/Write buckets ───────
    #[test]
    fn http_gate_independent_from_read_write() {
        let limiter = RateLimiter::new();
        let client = "test-client-http-isolation";

        // Exhaust HTTP gate
        for _ in 0..60 {
            let _ = limiter.check(client, OpType::HttpRequest);
        }
        assert!(
            limiter.check(client, OpType::HttpRequest).is_err(),
            "HTTP gate should be exhausted after 60 requests"
        );

        // Read and Write buckets must still be available
        assert!(
            limiter.check(client, OpType::Read).is_ok(),
            "Read bucket must be unaffected by HTTP gate exhaustion"
        );
        assert!(
            limiter.check(client, OpType::Write).is_ok(),
            "Write bucket must be unaffected by HTTP gate exhaustion"
        );
    }

    // ── Test 9: HttpRequest gate uses separate key per client ─────────────────
    #[test]
    fn http_gate_separate_per_client() {
        let limiter = RateLimiter::new();

        // Exhaust client_x HTTP gate
        for _ in 0..60 {
            let _ = limiter.check("client_x", OpType::HttpRequest);
        }
        assert!(
            limiter.check("client_x", OpType::HttpRequest).is_err(),
            "client_x HTTP gate should be exhausted"
        );

        // client_y is unaffected
        assert!(
            limiter.check("client_y", OpType::HttpRequest).is_ok(),
            "client_y HTTP gate should be independent of client_x"
        );
    }
}
