//! One-at-a-time, in-order processing per conversation for webhook channels.
//!
//! A webhook adapter that spawned one task per message ran several turns of
//! the same conversation at once (B10, Google Chat): later messages were
//! answered before, or without seeing, earlier ones, and a burst reached the
//! reply circuit breaker all at once. Submitting through a
//! [`ConversationQueue`] keeps each conversation's messages in arrival order
//! (the order of [`ConversationQueue::submit`] calls), one at a time;
//! different conversations still run in parallel. Each conversation queue is
//! bounded; a full queue is reported to the caller, never dropped silently.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use tokio::sync::mpsc;

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
type Senders = Arc<Mutex<HashMap<String, mpsc::Sender<Job>>>>;

/// Most messages waiting in one conversation.
pub(crate) const DEFAULT_PER_CONVERSATION: usize = 20;

/// The conversation already has the maximum number of waiting messages.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct QueueFull;

#[derive(Clone)]
pub(crate) struct ConversationQueue {
    senders: Senders,
    per_conversation: usize,
}

impl Default for ConversationQueue {
    fn default() -> Self {
        Self::new(DEFAULT_PER_CONVERSATION)
    }
}

impl ConversationQueue {
    pub(crate) fn new(per_conversation: usize) -> Self {
        Self {
            senders: Arc::new(Mutex::new(HashMap::new())),
            per_conversation: per_conversation.max(1),
        }
    }

    /// Queue `job` behind the conversation's earlier jobs. Must be called
    /// inside a Tokio runtime (starts the conversation's worker if needed).
    pub(crate) fn submit<F>(&self, key: &str, job: F) -> Result<(), QueueFull>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut job: Job = Box::pin(job);
        let mut senders = self.senders.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(tx) = senders.get(key) {
            match tx.try_send(job) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Full(_)) => return Err(QueueFull),
                // The worker is gone (it panicked outside a job): start anew.
                Err(mpsc::error::TrySendError::Closed(back)) => {
                    senders.remove(key);
                    job = back;
                }
            }
        }
        let (tx, rx) = mpsc::channel(self.per_conversation);
        tx.try_send(job).map_err(|_| QueueFull)?;
        senders.insert(key.to_string(), tx);
        tokio::spawn(run_conversation(key.to_string(), rx, self.senders.clone()));
        Ok(())
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        self.senders.lock().unwrap().len()
    }
}

/// Run one conversation's jobs in order; exit (and forget the conversation)
/// once its queue is empty. Sends happen under the same lock as the
/// emptiness check, so no job is left behind.
async fn run_conversation(key: String, mut rx: mpsc::Receiver<Job>, senders: Senders) {
    loop {
        match rx.try_recv() {
            Ok(job) => {
                // A panicking turn must not stop the conversation's later messages.
                if std::panic::AssertUnwindSafe(job).catch_unwind().await.is_err() {
                    tracing::error!(conversation = %key, "conversation job panicked; continuing with the next message");
                }
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                let mut map = senders.lock().unwrap_or_else(|p| p.into_inner());
                if rx.is_empty() {
                    map.remove(&key);
                    return;
                }
            }
            Err(mpsc::error::TryRecvError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn one_conversation_runs_in_order_one_at_a_time() {
        let q = ConversationQueue::default();
        let log = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for i in 0..5u64 {
            let (log, running) = (log.clone(), running.clone());
            q.submit("space/A", async move {
                let now = running.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(now, 0, "two turns of one conversation overlapped");
                // Earlier messages take longer: without the queue they would finish last.
                tokio::time::sleep(Duration::from_millis(50 - i * 10)).await;
                log.lock().unwrap().push(i);
                running.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            })
            .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while log.lock().unwrap().len() < 5 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn conversations_run_in_parallel_and_a_full_queue_is_reported() {
        let q = ConversationQueue::new(2);
        let gate = Arc::new(tokio::sync::Notify::new());
        let blocker = gate.clone();
        q.submit("A", async move { blocker.notified().await }).unwrap();
        // The worker may or may not have taken the first job yet.
        let mut accepted = 0;
        while q.submit("A", async {}).is_ok() {
            accepted += 1;
            assert!(accepted <= 2);
        }
        assert_eq!(q.submit("A", async {}), Err(QueueFull));
        let (tx, rx) = tokio::sync::oneshot::channel();
        q.submit("B", async move {
            let _ = tx.send(());
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("conversation B waited for A")
            .unwrap();
        gate.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while q.active() > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("idle conversations are forgotten");
    }

    #[tokio::test]
    async fn a_panicking_job_does_not_stop_later_messages() {
        let q = ConversationQueue::default();
        q.submit("A", async { panic!("boom") }).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        q.submit("A", async move {
            let _ = tx.send(());
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), rx).await.unwrap().unwrap();
    }
}
