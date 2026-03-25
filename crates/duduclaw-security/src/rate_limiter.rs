use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

/// Sliding-window rate limiter.
pub struct RateLimiter {
    windows: Arc<RwLock<HashMap<String, SlidingWindow>>>,
    default_limit: u32,
    window_duration: Duration,
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
        Self {
            windows: Arc::new(RwLock::new(HashMap::new())),
            default_limit,
            window_duration,
        }
    }

    /// Returns `true` if the key has remaining capacity (does **not** record
    /// the request).
    ///
    /// **Note:** This method is not atomic with [`record`](Self::record). If you
    /// need to check-and-record in one step, use
    /// [`check_and_record`](Self::check_and_record) instead.
    pub async fn check(&self, key: &str) -> bool {
        let mut windows = self.windows.write().await;
        let window = windows
            .entry(key.to_string())
            .or_insert_with(|| SlidingWindow::new(self.default_limit));
        window.is_allowed(self.window_duration)
    }

    /// Record a request for `key` (unconditionally).
    pub async fn record(&self, key: &str) {
        let mut windows = self.windows.write().await;
        let window = windows
            .entry(key.to_string())
            .or_insert_with(|| SlidingWindow::new(self.default_limit));
        window.record(self.window_duration);
    }

    /// Atomically check whether the request is allowed and, if so, record it.
    /// Returns `true` when the request was accepted.
    pub async fn check_and_record(&self, key: &str) -> bool {
        let mut windows = self.windows.write().await;

        // Evict stale keys to prevent unbounded memory growth (MW-H3)
        if windows.len() > 10_000 {
            let cutoff = Instant::now() - self.window_duration;
            windows.retain(|_, w| w.timestamps.iter().any(|t| *t > cutoff));
        }

        let window = windows
            .entry(key.to_string())
            .or_insert_with(|| SlidingWindow::new(self.default_limit));
        if window.is_allowed(self.window_duration) {
            window.record(self.window_duration);
            true
        } else {
            false
        }
    }

    /// Reset the counter for `key`.
    pub async fn reset(&self, key: &str) {
        self.windows.write().await.remove(key);
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
}
