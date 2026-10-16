//! Bound each destination independently; the lifecycle keeps its final
//! overall timeout. Tests inject short deadlines and never send messages.

use std::future::Future;
use std::time::Duration;

pub(crate) async fn send_to_targets<T, F, Fut>(
    targets: Vec<T>,
    deadline: Duration,
    send: F,
) -> usize
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = bool>,
{
    let deliveries = targets.into_iter().map(|target| {
        let future = send(target);
        async move {
            tokio::time::timeout(deadline, future)
                .await
                .unwrap_or(false)
        }
    });
    futures_util::future::join_all(deliveries)
        .await
        .into_iter()
        .filter(|sent| *sent)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[tokio::test]
    async fn a_blocked_first_target_does_not_prevent_later_targets_starting() {
        let later_started = Arc::new(Notify::new());
        let delivered = send_to_targets(vec![0, 1], Duration::from_millis(30), |target| {
            let later_started = later_started.clone();
            async move {
                if target == 0 {
                    later_started.notified().await;
                } else {
                    later_started.notify_one();
                }
                true
            }
        })
        .await;
        assert_eq!(
            delivered, 2,
            "the first target must observe the second start before its deadline"
        );
    }

    #[tokio::test]
    async fn one_target_timeout_does_not_cancel_other_successes() {
        let completed = AtomicUsize::new(0);
        let delivered = send_to_targets(vec![0, 1, 2, 3], Duration::from_millis(30), |target| {
            let completed = &completed;
            async move {
                match target {
                    0 => std::future::pending::<bool>().await,
                    1 | 2 => {
                        tokio::task::yield_now().await;
                        completed.fetch_add(1, Ordering::SeqCst);
                        true
                    }
                    _ => false,
                }
            }
        })
        .await;
        assert_eq!(delivered, 2);
        assert_eq!(completed.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn every_target_starts_without_a_worker_queue_and_only_successes_count() {
        const TARGETS: usize = 12;
        let started = AtomicUsize::new(0);
        let all_started = Notify::new();
        let delivered = send_to_targets(
            (0..TARGETS).collect(),
            Duration::from_millis(30),
            |target| {
                let started = &started;
                let all_started = &all_started;
                async move {
                    // Register before announcing readiness so no wake is lost.
                    let ready = all_started.notified();
                    tokio::pin!(ready);
                    ready.as_mut().enable();
                    if started.fetch_add(1, Ordering::SeqCst) + 1 == TARGETS {
                        all_started.notify_waiters();
                    } else {
                        ready.await;
                    }
                    target % 3 == 0
                }
            },
        )
        .await;
        assert_eq!(started.load(Ordering::SeqCst), TARGETS);
        assert_eq!(
            delivered, 4,
            "all targets must start before their independent deadlines"
        );
    }
}
