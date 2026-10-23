//! One-at-a-time, in-order processing per conversation for webhook channels.
//!
//! A webhook adapter that spawned one task per message ran several turns of
//! the same conversation at once (B10, Google Chat): later messages were
//! answered before, or without seeing, earlier ones, and a burst reached the
//! reply circuit breaker all at once. Submitting through a
//! [`ConversationQueue`] keeps each conversation's messages in arrival order
//! (the order of [`ConversationQueue::submit`] calls), one at a time;
//! different conversations still run in parallel. Conversations can be
//! grouped (Google Chat: one queue per space and sender, grouped by space),
//! with a cap per conversation and a cap per group, so one sender cannot
//! fill the whole space. A full queue is reported to the caller, never
//! dropped silently; [`NoticeThrottle`] keeps the caller's "queue full"
//! notices from becoming a flood themselves.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use tokio::sync::mpsc;

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
type Senders = Arc<Mutex<HashMap<String, mpsc::Sender<Job>>>>;

/// Most messages waiting in one conversation.
pub(crate) const DEFAULT_PER_CONVERSATION: usize = 10;
/// Most messages waiting in one group (all its conversations together).
pub(crate) const DEFAULT_PER_GROUP: usize = 40;

/// The conversation already has the maximum number of waiting messages.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct QueueFull;

#[derive(Clone)]
pub(crate) struct ConversationQueue {
    senders: Senders,
    /// Jobs queued or running per group.
    groups: Arc<Mutex<HashMap<String, usize>>>,
    per_conversation: usize,
    per_group: usize,
}

impl Default for ConversationQueue {
    fn default() -> Self {
        Self::with_caps(DEFAULT_PER_CONVERSATION, DEFAULT_PER_GROUP)
    }
}

/// Releases one group slot when the job ends (also on panic or drop).
struct GroupSlot {
    groups: Arc<Mutex<HashMap<String, usize>>>,
    group: String,
}

impl Drop for GroupSlot {
    fn drop(&mut self) {
        let mut groups = self.groups.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(n) = groups.get_mut(&self.group) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                groups.remove(&self.group);
            }
        }
    }
}

impl ConversationQueue {
    #[cfg(test)]
    pub(crate) fn new(per_conversation: usize) -> Self {
        Self::with_caps(per_conversation, usize::MAX)
    }

    pub(crate) fn with_caps(per_conversation: usize, per_group: usize) -> Self {
        Self {
            senders: Arc::new(Mutex::new(HashMap::new())),
            groups: Arc::new(Mutex::new(HashMap::new())),
            per_conversation: per_conversation.max(1),
            per_group: per_group.max(1),
        }
    }

    /// [`Self::submit_in`] with the conversation as its own group.
    #[cfg(test)]
    pub(crate) fn submit<F>(&self, key: &str, job: F) -> Result<(), QueueFull>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.submit_in(key, key, job)
    }

    /// Queue `job` behind the conversation's earlier jobs, counting it
    /// against `group`. Must be called inside a Tokio runtime (starts the
    /// conversation's worker if needed).
    pub(crate) fn submit_in<F>(&self, group: &str, key: &str, job: F) -> Result<(), QueueFull>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let slot = {
            let mut groups = self.groups.lock().unwrap_or_else(|p| p.into_inner());
            let n = groups.entry(group.to_string()).or_insert(0);
            if *n >= self.per_group {
                return Err(QueueFull);
            }
            *n += 1;
            GroupSlot {
                groups: self.groups.clone(),
                group: group.to_string(),
            }
        };
        let mut job: Job = Box::pin(async move {
            let _slot = slot;
            job.await
        });
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

/// At most one notice per key per interval (for "queue full" replies).
#[derive(Default)]
pub(crate) struct NoticeThrottle {
    last: Mutex<HashMap<String, std::time::Instant>>,
}

/// Keys remembered by a [`NoticeThrottle`] before old ones are dropped.
const NOTICE_KEYS_CAP: usize = 4096;

impl NoticeThrottle {
    /// `true` when no key in `keys` had a notice within its interval;
    /// then all of them are stamped now.
    pub(crate) fn allow(&self, keys: &[(&str, std::time::Duration)]) -> bool {
        let now = std::time::Instant::now();
        let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        let quiet = keys.iter().all(|(k, every)| {
            last.get(*k)
                .is_none_or(|t| now.saturating_duration_since(*t) >= *every)
        });
        if quiet {
            if last.len() >= NOTICE_KEYS_CAP {
                last.clear();
            }
            for (k, _) in keys {
                last.insert((*k).to_string(), now);
            }
        }
        quiet
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
    async fn one_sender_cannot_fill_the_whole_group() {
        let q = ConversationQueue::with_caps(10, 3);
        let gate = Arc::new(tokio::sync::Notify::new());
        for i in 0..3 {
            let g = gate.clone();
            q.submit_in("space", "space|alice", async move { g.notified().await })
                .unwrap_or_else(|_| panic!("job {i}"));
        }
        assert_eq!(q.submit_in("space", "space|bob", async {}), Err(QueueFull), "group cap");
        assert!(q.submit_in("other-space", "other|bob", async {}).is_ok());
        gate.notify_waiters();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                gate.notify_waiters();
                if q.submit_in("space", "space|bob", async {}).is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("finished jobs free their group slots");
    }

    #[test]
    fn notices_are_throttled_per_key() {
        let t = NoticeThrottle::default();
        let minute = Duration::from_secs(60);
        let ten = Duration::from_secs(10);
        assert!(t.allow(&[("sender:a", minute), ("space:s", ten)]));
        assert!(!t.allow(&[("sender:a", minute), ("space:s", ten)]), "same sender");
        assert!(!t.allow(&[("sender:b", minute), ("space:s", ten)]), "same space within 10 s");
        assert!(t.allow(&[("sender:b", minute), ("space:t", ten)]));
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
