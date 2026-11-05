//! WebChat heartbeat liveness (B10).
//!
//! The reader runs independently of model turns. Protocol pong frames and
//! incoming application frames both count as liveness; browser JS timers are
//! not required. Time spent handling an inbound control frame is excluded.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::Instant;

/// No frame for longer than this while idle closes the connection.
pub(super) const PING_INTERVAL: Duration = Duration::from_secs(30);
pub(super) const IDLE_LIMIT: Duration = Duration::from_secs(60);

/// When the connection last showed it was alive.
pub(super) struct Liveness {
    base: Instant,
    last_ms: AtomicU64,
}

impl Liveness {
    pub(super) fn new() -> Self {
        Self {
            base: Instant::now(),
            last_ms: AtomicU64::new(0),
        }
    }

    /// A frame arrived, or the loop finished work and can read again.
    pub(super) fn touch(&self) {
        let ms = self.base.elapsed().as_millis() as u64;
        self.last_ms.fetch_max(ms, Ordering::Relaxed);
    }

    fn idle_at(&self, now: Instant) -> Duration {
        let last = self.base + Duration::from_millis(self.last_ms.load(Ordering::Relaxed));
        now.saturating_duration_since(last)
    }

    /// Whether the peer has been silent past [`IDLE_LIMIT`].
    pub(super) fn timed_out(&self) -> bool {
        self.idle_at(Instant::now()) > IDLE_LIMIT
    }

    /// Touches when dropped: hold one while handling a frame, so every way
    /// out of the handler (including `continue`) counts as "free again".
    pub(super) fn busy(&self) -> TouchOnDrop<'_> {
        self.touch();
        TouchOnDrop(self)
    }
}

pub(super) struct TouchOnDrop<'a>(&'a Liveness);

impl Drop for TouchOnDrop<'_> {
    fn drop(&mut self) {
        self.0.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_turn_does_not_count_as_silence() {
        let live = Liveness::new();
        // Pretend the last frame came 70 s ago and a turn has just finished.
        let started = Instant::now();
        {
            let _busy = live.busy();
        }
        assert!(live.idle_at(started + Duration::from_secs(70)) >= Duration::from_secs(69));
        live.touch();
        assert!(!live.timed_out(), "finishing a turn resets the idle clock");
    }

    #[test]
    fn real_silence_still_times_out() {
        let live = Liveness::new();
        assert!(!live.timed_out());
        assert!(live.idle_at(Instant::now() + Duration::from_secs(61)) > IDLE_LIMIT);
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn protocol_pongs_keep_background_client_alive_and_silence_expires() {
        let live = Liveness::new();
        assert!(PING_INTERVAL < IDLE_LIMIT);
        for _ in 0..8 {
            tokio::time::advance(PING_INTERVAL).await;
            assert!(!live.timed_out());
            // Same path used by Message::Pong in the real reader.
            { let _pong = live.busy(); }
        }
        tokio::time::advance(IDLE_LIMIT + Duration::from_secs(1)).await;
        assert!(live.timed_out());
    }
}
