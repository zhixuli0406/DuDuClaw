//! Per-key token-bucket rate limiter (e.g. 60 requests/min per device on the
//! webhook-forwarding endpoint, or per-source-IP on registration/WS-connect).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// Bound on the number of distinct keys tracked at once, to keep an
/// ever-changing key space (arbitrary device_ids / source IPs) from growing
/// the map without limit. Sweeps the oldest-refilled entries out once
/// exceeded.
const MAX_TRACKED_KEYS: usize = 50_000;

#[derive(Clone)]
pub struct TokenBucketLimiter {
    buckets: Arc<RwLock<HashMap<String, Bucket>>>,
    capacity: f64,
    refill_per_sec: f64,
}

impl TokenBucketLimiter {
    /// `capacity` tokens, refilling at a rate that reaches full capacity
    /// again every `per` duration — e.g. `new(60, Duration::from_secs(60))`
    /// for 60 requests/minute with a burst of 60.
    pub fn new(capacity: u32, per: Duration) -> Self {
        let capacity = capacity as f64;
        let refill_per_sec = capacity / per.as_secs_f64().max(0.001);
        Self {
            buckets: Arc::new(RwLock::new(HashMap::new())),
            capacity,
            refill_per_sec,
        }
    }

    /// Attempt to consume one token for `key`. Returns `true` if allowed.
    pub async fn allow(&self, key: &str) -> bool {
        let mut guard = self.buckets.write().await;
        if guard.len() > MAX_TRACKED_KEYS {
            let cutoff = Instant::now() - Duration::from_secs(3600);
            guard.retain(|_, b| b.last_refill > cutoff);
        }
        let now = Instant::now();
        let bucket = guard.entry(key.to_string()).or_insert_with(|| Bucket {
            tokens: self.capacity,
            last_refill: now,
        });
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_within_capacity_then_denies() {
        let limiter = TokenBucketLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.allow("a").await);
        assert!(limiter.allow("a").await);
        assert!(limiter.allow("a").await);
        assert!(!limiter.allow("a").await);
    }

    #[tokio::test]
    async fn separate_keys_are_independent() {
        let limiter = TokenBucketLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.allow("a").await);
        assert!(limiter.allow("b").await);
        assert!(!limiter.allow("a").await);
    }

    #[tokio::test]
    async fn refills_over_time() {
        // Fast refill so the test doesn't need to sleep long: full capacity
        // restored every 50ms.
        let limiter = TokenBucketLimiter::new(1, Duration::from_millis(50));
        assert!(limiter.allow("a").await);
        assert!(!limiter.allow("a").await);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(limiter.allow("a").await);
    }
}
