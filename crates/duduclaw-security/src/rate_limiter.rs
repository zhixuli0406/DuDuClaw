use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

/// Sliding-window rate limiter.
pub struct RateLimiter {
    windows: Arc<RwLock<Windows>>,
    max_keys: usize,
    default_limit: u32,
    window_duration: Duration,
}

struct Windows {
    entries: HashMap<String, SlidingWindow>,
    last_cleanup: Instant,
}

struct SlidingWindow {
    timestamps: Vec<Instant>,
    limit: u32,
}

impl SlidingWindow {
    fn new(limit: u32) -> Self {
        Self {
            timestamps: Vec::new(),
            limit,
        }
    }

    /// Remove timestamps older than `window` and return whether a new request
    /// would be within the limit.
    fn is_allowed(&mut self, window: Duration) -> bool {
        let cutoff = Instant::now() - window;
        self.timestamps.retain(|t| *t > cutoff);
        self.timestamps.len() < self.limit as usize
    }

    fn record(&mut self, window: Duration) {
        let cutoff = Instant::now() - window;
        self.timestamps.retain(|t| *t > cutoff);
        self.timestamps.push(Instant::now());
    }
}

impl RateLimiter {
    /// Create a limiter that allows `default_limit` requests per
    /// `window_duration`.
    pub fn new(default_limit: u32, window_duration: Duration) -> Self {
        Self::with_max_keys(default_limit, window_duration, 10_000)
    }

    /// A full limiter refuses new keys; existing keys keep their counters.
    pub fn with_max_keys(default_limit: u32, window_duration: Duration, max_keys: usize) -> Self {
        Self {
            windows: Arc::new(RwLock::new(Windows {
                entries: HashMap::new(), last_cleanup: Instant::now(),
            })),
            max_keys,
            default_limit,
            window_duration,
        }
    }

    fn admit_key(&self, windows: &mut Windows, key: &str) -> bool {
        if windows.entries.contains_key(key) { return true; }
        if windows.entries.len() >= self.max_keys {
            // At saturation, sweep at most once per second, never per rejected key.
            if windows.last_cleanup.elapsed() >= self.window_duration.min(Duration::from_secs(1)) {
                let cutoff = Instant::now() - self.window_duration;
                windows.entries.retain(|_, w| w.timestamps.iter().any(|t| *t > cutoff));
                windows.last_cleanup = Instant::now();
            }
            if windows.entries.len() >= self.max_keys { return false; }
        }
        windows.entries.insert(key.to_string(), SlidingWindow::new(self.default_limit));
        true
    }

    /// Returns `true` if the key has remaining capacity (does **not** record
    /// the request).
    ///
    /// **Note:** This method is not atomic with [`record`](Self::record). If you
    /// need to check-and-record in one step, use
    /// [`check_and_record`](Self::check_and_record) instead.
    pub async fn check(&self, key: &str) -> bool {
        let mut windows = self.windows.write().await;
        if !self.admit_key(&mut windows, key) { return false; }
        windows.entries.get_mut(key).unwrap().is_allowed(self.window_duration)
    }

    /// Record a request for `key`, unless the key capacity is exhausted.
    /// Prefer `check_and_record` for admission; this method has no success result.
    pub async fn record(&self, key: &str) {
        let mut windows = self.windows.write().await;
        if !self.admit_key(&mut windows, key) { return; }
        windows.entries.get_mut(key).unwrap().record(self.window_duration);
    }

    /// Atomically check whether the request is allowed and, if so, record it.
    /// Returns `true` when the request was accepted.
    pub async fn check_and_record(&self, key: &str) -> bool {
        let mut windows = self.windows.write().await;

        if !self.admit_key(&mut windows, key) { return false; }
        let window = windows.entries.get_mut(key).unwrap();
        if window.is_allowed(self.window_duration) {
            window.record(self.window_duration);
            true
        } else {
            false
        }
    }

    /// Reset the counter for `key`.
    pub async fn reset(&self, key: &str) {
        self.windows.write().await.entries.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_within_limit() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.check_and_record("a").await);
        assert!(limiter.check_and_record("a").await);
        assert!(limiter.check_and_record("a").await);
        // Fourth should be denied.
        assert!(!limiter.check_and_record("a").await);
    }

    #[tokio::test]
    async fn separate_keys_are_independent() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.check_and_record("a").await);
        assert!(limiter.check_and_record("b").await);
        assert!(!limiter.check_and_record("a").await);
    }

    #[tokio::test]
    async fn reset_clears_counter() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.check_and_record("a").await);
        assert!(!limiter.check("a").await);
        limiter.reset("a").await;
        assert!(limiter.check("a").await);
    }

    #[tokio::test]
    async fn active_keys_have_a_hard_cap_on_every_entry_point() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        for n in 0..10_000 {
            assert!(limiter.check_and_record(&format!("2001:db8::{n:x}")).await);
        }
        for n in 0..1000 {
            let key = format!("new-{n}");
            assert!(!limiter.check_and_record(&key).await);
            assert!(!limiter.check(&key).await);
            limiter.record(&key).await;
        }
        assert_eq!(limiter.windows.read().await.entries.len(), 10_000);
        assert!(limiter.check_and_record("2001:db8::0").await);
        assert!(!limiter.check_and_record("2001:db8::0").await);
    }

    #[tokio::test]
    async fn saturated_cleanup_is_throttled_and_recovers_expired_capacity() {
        let limiter = RateLimiter::with_max_keys(1, Duration::from_secs(60), 1);
        assert!(limiter.check_and_record("active").await);
        {
            let mut windows = limiter.windows.write().await;
            windows.last_cleanup = Instant::now() - Duration::from_secs(2);
        }
        assert!(!limiter.check_and_record("new").await);
        let last_cleanup = limiter.windows.read().await.last_cleanup;
        for _ in 0..10 { assert!(!limiter.check_and_record("new").await); }
        assert_eq!(limiter.windows.read().await.last_cleanup, last_cleanup);
        {
            let mut windows = limiter.windows.write().await;
            windows.last_cleanup = Instant::now() - Duration::from_secs(2);
            windows.entries.get_mut("active").unwrap().timestamps[0] = Instant::now() - Duration::from_secs(61);
        }
        assert!(limiter.check_and_record("new").await);
        assert_eq!(limiter.windows.read().await.entries.len(), 1);
    }

    #[tokio::test]
    async fn zero_key_capacity_is_closed() {
        let limiter = RateLimiter::with_max_keys(1, Duration::from_secs(60), 0);
        assert!(!limiter.check("key").await);
        assert!(!limiter.check_and_record("key").await);
        limiter.record("key").await;
        assert!(limiter.windows.read().await.entries.is_empty());
    }

}
