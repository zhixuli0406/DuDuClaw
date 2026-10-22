//! The reply circuit breaker's frequency limit, applied by waiting.
//!
//! `CircuitBreaker::check_inbound` answers [`BreakerDecision::Wait`] when a
//! conversation already had `frequency_max_replies` messages inside the
//! window. The message is then held here until the window has room and
//! checked again, so a burst is answered at a bounded rate instead of being
//! dropped (B10, 2026-10: "連續發訊息會掉回覆").

use std::time::Duration;

use duduclaw_security::circuit_breaker::{BreakerDecision, CircuitBreakerRegistry};
use tracing::info;

/// Upper bound on the total wait for one message. Past it the message runs
/// anyway: the limit shapes the reply rate, it never drops a message.
pub(super) const MAX_TOTAL_WAIT: Duration = Duration::from_secs(30);

/// [`CircuitBreakerRegistry::check_inbound`], sleeping through
/// [`BreakerDecision::Wait`] answers. Never returns `Wait` unless the total
/// wait reached [`MAX_TOTAL_WAIT`] (the caller then treats it as allowed).
pub(super) async fn check_inbound_waiting(
    registry: &CircuitBreakerRegistry,
    scope: &str,
    text: &str,
) -> BreakerDecision {
    check_inbound_waiting_at_most(registry, scope, text, MAX_TOTAL_WAIT).await
}

async fn check_inbound_waiting_at_most(
    registry: &CircuitBreakerRegistry,
    scope: &str,
    text: &str,
    cap: Duration,
) -> BreakerDecision {
    let mut waited = Duration::ZERO;
    loop {
        let decision = registry.check_inbound(scope, text).await;
        let BreakerDecision::Wait(wait) = decision else {
            return decision;
        };
        if waited >= cap {
            info!(scope, ?waited, "reply rate limit: waited the maximum, answering now");
            return BreakerDecision::Wait(wait);
        }
        let step = wait.min(cap - waited);
        info!(scope, ?step, "reply rate limit: holding this message until the window has room");
        tokio::time::sleep(step).await;
        waited += step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_security::killswitch::CircuitBreakerConfig;

    fn config(max: u32, window: u64) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            frequency_window_secs: window,
            frequency_max_replies: max,
            ..CircuitBreakerConfig::default()
        }
    }

    /// B10 regression: seven quick messages over the limit (5 per window)
    /// used to trip the breaker on the 6th and drop the 6th and 7th. Now
    /// every one is allowed; the 6th waits for the window. (Real time: the
    /// breaker reads `std::time::Instant`, so the window is kept short.)
    #[tokio::test]
    async fn a_burst_over_the_limit_is_answered_after_a_wait_not_dropped() {
        let reg = CircuitBreakerRegistry::new(config(5, 1));
        let start = std::time::Instant::now();
        let burst = ["嗨", "我想問", "你們有賣", "藍色的", "M號", "還有", "黑色嗎"];
        for (i, text) in burst.iter().enumerate() {
            let d = check_inbound_waiting(&reg, "line:U1", text).await;
            assert!(matches!(d, BreakerDecision::Allow), "message {i} ({text}) got {d:?}");
        }
        assert!(start.elapsed() >= Duration::from_millis(900), "the 6th message waited");
        assert_eq!(
            reg.state("line:U1").await,
            duduclaw_security::circuit_breaker::BreakerState::Closed
        );
    }

    #[tokio::test]
    async fn the_total_wait_is_bounded() {
        // A window far longer than the cap: the message still runs.
        let reg = CircuitBreakerRegistry::new(config(1, 600));
        assert!(matches!(
            check_inbound_waiting(&reg, "s", "first").await,
            BreakerDecision::Allow
        ));
        let cap = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let d = check_inbound_waiting_at_most(&reg, "s", "second", cap).await;
        assert!(matches!(d, BreakerDecision::Wait(_)));
        let took = start.elapsed();
        assert!(took >= cap && took < Duration::from_secs(5), "{took:?}");
    }
}
