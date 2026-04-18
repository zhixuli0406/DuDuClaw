//! User activity detection for L5b auto-pause.
//!
//! Monitors global mouse/keyboard events via `rdev`. When the user moves
//! the mouse or presses a key, the agent's computer use session should
//! pause to avoid conflicting input.
//!
//! # Usage
//!
//! ```ignore
//! let monitor = ActivityMonitor::start();
//! // In the orchestrator loop:
//! if monitor.user_active_since(Duration::from_secs(2)) {
//!     orchestrator.control.paused.store(true, Ordering::Relaxed);
//! }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(feature = "activity-monitor")]
use tracing::{info, warn};

/// Monitors global input events and tracks the timestamp of the last user activity.
pub struct ActivityMonitor {
    /// Unix timestamp (millis) of the last detected user input.
    last_activity_ms: Arc<AtomicU64>,
    /// Handle to the listener thread (for cleanup).
    _handle: Option<std::thread::JoinHandle<()>>,
}

impl ActivityMonitor {
    /// Start listening for global input events in a background thread.
    ///
    /// Returns immediately. The monitor thread runs until the `ActivityMonitor`
    /// is dropped (though `rdev::listen` is blocking and may not cleanly stop).
    #[cfg(feature = "activity-monitor")]
    pub fn start() -> Self {
        let last_activity = Arc::new(AtomicU64::new(0));
        let last_activity_clone = Arc::clone(&last_activity);

        let handle = std::thread::Builder::new()
            .name("activity-monitor".into())
            .spawn(move || {
                let callback = move |event: rdev::Event| {
                    // Only track mouse move and key press (not release) to reduce noise
                    let is_user_input = matches!(
                        event.event_type,
                        rdev::EventType::MouseMove { .. }
                            | rdev::EventType::KeyPress(_)
                            | rdev::EventType::ButtonPress(_)
                    );
                    if is_user_input {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        last_activity_clone.store(now, Ordering::Release);
                    }
                };

                info!("Activity monitor started (rdev global listener)");
                if let Err(e) = rdev::listen(callback) {
                    warn!("rdev listener error: {e:?}");
                }
            })
            .ok();

        Self {
            last_activity_ms: last_activity,
            _handle: handle,
        }
    }

    /// Stub when `activity-monitor` feature is disabled.
    #[cfg(not(feature = "activity-monitor"))]
    pub fn start() -> Self {
        Self {
            last_activity_ms: Arc::new(AtomicU64::new(0)),
            _handle: None,
        }
    }

    /// Check if the user has been active within the given duration.
    pub fn user_active_since(&self, window: Duration) -> bool {
        let last = self.last_activity_ms.load(Ordering::Acquire);
        if last == 0 {
            return false;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let window_ms = window.as_millis() as u64;
        now.saturating_sub(last) < window_ms
    }

    /// Get the timestamp of the last user activity (millis since UNIX epoch).
    /// Returns 0 if no activity has been detected.
    pub fn last_activity_timestamp_ms(&self) -> u64 {
        self.last_activity_ms.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_activity_returns_false() {
        let monitor = ActivityMonitor {
            last_activity_ms: Arc::new(AtomicU64::new(0)),
            _handle: None,
        };
        assert!(!monitor.user_active_since(Duration::from_secs(5)));
        assert_eq!(monitor.last_activity_timestamp_ms(), 0);
    }

    #[test]
    fn recent_activity_returns_true() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let monitor = ActivityMonitor {
            last_activity_ms: Arc::new(AtomicU64::new(now)),
            _handle: None,
        };
        assert!(monitor.user_active_since(Duration::from_secs(5)));
    }

    #[test]
    fn old_activity_returns_false() {
        let old = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - 10_000; // 10 seconds ago
        let monitor = ActivityMonitor {
            last_activity_ms: Arc::new(AtomicU64::new(old)),
            _handle: None,
        };
        assert!(!monitor.user_active_since(Duration::from_secs(5)));
        assert!(monitor.user_active_since(Duration::from_secs(15)));
    }
}
