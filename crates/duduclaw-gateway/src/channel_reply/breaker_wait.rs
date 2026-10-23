//! The reply circuit breaker's frequency limit, applied by waiting.
//!
//! `CircuitBreaker::check_inbound_as` answers [`BreakerDecision::Wait`] when
//! a conversation already had `frequency_max_replies` messages from a person
//! inside the window. The message is then held here until the window has
//! room and checked again, so a burst is answered at a bounded rate instead
//! of being dropped (B10, 2026-10: "連續發訊息會掉回覆"). A message still
//! waiting after [`MAX_TOTAL_WAIT`] (a flood: dozens at once) is refused,
//! still counted by the breaker, and the sender is told once per window.
//! Senders the channel marks as bots keep the strict trip (see
//! [`as_sender`]).

use std::future::Future;
use std::time::Duration;

use duduclaw_security::circuit_breaker::{BreakerDecision, CircuitBreakerRegistry, SenderKind};
use tracing::{info, warn};

use crate::trigger_route::texts::Lang;

/// Upper bound on the total wait for one message.
pub(super) const MAX_TOTAL_WAIT: Duration = Duration::from_secs(30);

tokio::task_local! {
    static SENDER: SenderKind;
}

/// Run a channel turn knowing who sent the message (default: a person).
/// Adapters whose platform flags bot senders wrap the turn with
/// `SenderKind::Bot`, so the breaker keeps its strict trip for them.
pub(crate) async fn as_sender<F: Future>(kind: SenderKind, turn: F) -> F::Output {
    SENDER.scope(kind, turn).await
}

fn current_sender() -> SenderKind {
    SENDER.try_with(|k| *k).unwrap_or_default()
}

/// What the breaker decided for one inbound message.
#[derive(Debug)]
pub(super) enum Admission {
    /// The breaker's own decision (never `Wait`).
    Decision(BreakerDecision),
    /// Waited [`MAX_TOTAL_WAIT`] and the window still had no room: not
    /// answered. `notify`: tell the sender (once per window).
    Refused { notify: bool },
}

/// What a refused sender is told.
pub(super) fn refused_text(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "訊息太多了，這則先不處理。請等一分鐘後再傳一次。",
        Lang::En => "Too many messages at once, so this one was not handled. Please send it again in a minute.",
        Lang::Ja => "メッセージが多すぎるため、このメッセージは処理しませんでした。1 分ほどしてからもう一度送ってください。",
    }
}

/// [`CircuitBreakerRegistry::check_inbound_as`], sleeping through
/// [`BreakerDecision::Wait`] answers for at most [`MAX_TOTAL_WAIT`].
pub(super) async fn check_inbound_waiting(
    registry: &CircuitBreakerRegistry,
    scope: &str,
    text: &str,
) -> Admission {
    check_inbound_waiting_at_most(registry, scope, text, MAX_TOTAL_WAIT).await
}

async fn check_inbound_waiting_at_most(
    registry: &CircuitBreakerRegistry,
    scope: &str,
    text: &str,
    cap: Duration,
) -> Admission {
    let sender = current_sender();
    let mut waited = Duration::ZERO;
    loop {
        let decision = registry.check_inbound_as(scope, text, sender).await;
        let BreakerDecision::Wait(wait) = decision else {
            return Admission::Decision(decision);
        };
        if waited >= cap {
            warn!(scope, ?waited, "reply rate limit: still no room after the maximum wait; message refused");
            return Admission::Refused {
                notify: registry.record_refused(scope, text).await,
            };
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
            assert!(
                matches!(d, Admission::Decision(BreakerDecision::Allow)),
                "message {i} ({text}) got {d:?}"
            );
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
            Admission::Decision(BreakerDecision::Allow)
        ));
        let cap = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let d = check_inbound_waiting_at_most(&reg, "s", "second", cap).await;
        assert!(matches!(d, Admission::Refused { notify: true }));
        let took = start.elapsed();
        assert!(took >= cap && took < Duration::from_secs(5), "{took:?}");
    }

    /// B10 review M1: a flood past the wait cap is refused (never let
    /// through), counted, and announced only once.
    #[tokio::test]
    async fn a_flood_past_the_cap_is_refused_counted_and_announced_once() {
        let reg = CircuitBreakerRegistry::new(CircuitBreakerConfig {
            hard_max_messages: 6,
            ..config(1, 600)
        });
        let cap = Duration::from_millis(20);
        assert!(matches!(
            check_inbound_waiting_at_most(&reg, "s", "m0", cap).await,
            Admission::Decision(BreakerDecision::Allow)
        ));
        let mut notices = 0;
        for i in 1..6 {
            match check_inbound_waiting_at_most(&reg, "s", &format!("m{i}"), cap).await {
                Admission::Refused { notify } => notices += usize::from(notify),
                other => panic!("message {i} let through: {other:?}"),
            }
        }
        assert_eq!(notices, 1);
        // The refused messages counted toward the hard limit.
        assert!(matches!(
            check_inbound_waiting_at_most(&reg, "s", "m6", cap).await,
            Admission::Decision(BreakerDecision::Trip(_))
        ));
    }

    /// B10 review L1: only a deliberate silence is reported as one.
    #[tokio::test]
    async fn deliberate_silence_is_reported_only_when_recorded() {
        let home = tempfile::tempdir().unwrap();
        let (_, quiet) = crate::channel_reply::track_silence(async {
            crate::channel_reply::record_silent_reply(home.path(), "webchat:x", "u", "silent_by_design: test");
        })
        .await;
        assert!(quiet);
        let (_, quiet) = crate::channel_reply::track_silence(async {}).await;
        assert!(!quiet, "an empty model answer is not a deliberate silence");
    }

    #[tokio::test]
    async fn a_bot_sender_trips_instead_of_waiting() {
        let reg = CircuitBreakerRegistry::new(config(2, 600));
        let run = async {
            for text in ["a", "b"] {
                assert!(matches!(
                    check_inbound_waiting(&reg, "s", text).await,
                    Admission::Decision(BreakerDecision::Allow)
                ));
            }
            check_inbound_waiting(&reg, "s", "c").await
        };
        let d = as_sender(SenderKind::Bot, run).await;
        assert!(matches!(d, Admission::Decision(BreakerDecision::Trip(_))));
    }
}
