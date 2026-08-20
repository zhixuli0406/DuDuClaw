//! Bounded, per-device FIFO queue for webhook frames that arrive while a
//! device is offline (or its live connection is momentarily backed up).
//! Flushed to the device's WebSocket connection, in order, as soon as it
//! (re)connects — see `device_ws.rs`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::hook::HookFrame;

/// Maximum number of queued frames retained per device. Oldest entries are
/// dropped first once the cap is reached — webhook senders like LINE retry
/// on failure, so losing the very oldest of a long backlog is preferable to
/// unbounded memory growth for a device that never reconnects.
pub const MAX_QUEUED_PER_DEVICE: usize = 256;

#[derive(Clone, Default)]
pub struct OfflineQueue {
    inner: Arc<RwLock<HashMap<String, VecDeque<HookFrame>>>>,
}

impl OfflineQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a frame for `device_id`, evicting the oldest queued frame if
    /// already at capacity.
    pub async fn push(&self, device_id: &str, frame: HookFrame) {
        let mut guard = self.inner.write().await;
        let q = guard.entry(device_id.to_string()).or_default();
        if q.len() >= MAX_QUEUED_PER_DEVICE {
            q.pop_front();
        }
        q.push_back(frame);
    }

    /// Drain all queued frames for `device_id`, in FIFO order, removing
    /// them from the queue.
    pub async fn drain(&self, device_id: &str) -> Vec<HookFrame> {
        let mut guard = self.inner.write().await;
        guard
            .remove(device_id)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
    }

    /// Current queue depth for `device_id`. Used by ops/tests; not part of
    /// any security-relevant decision.
    pub async fn len(&self, device_id: &str) -> usize {
        self.inner
            .read()
            .await
            .get(device_id)
            .map(|q| q.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn frame(tag: &str) -> HookFrame {
        let mut h = HookFrame::new(crate::channel::Channel::Line, BTreeMap::new(), tag.as_bytes());
        h.id = tag.to_string(); // deterministic id for ordering assertions
        h
    }

    #[tokio::test]
    async fn drain_preserves_fifo_order() {
        let q = OfflineQueue::new();
        q.push("dev-1", frame("a")).await;
        q.push("dev-1", frame("b")).await;
        q.push("dev-1", frame("c")).await;
        let drained = q.drain("dev-1").await;
        let ids: Vec<_> = drained.iter().map(|f| f.id.clone()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn drain_empties_the_queue() {
        let q = OfflineQueue::new();
        q.push("dev-1", frame("a")).await;
        let _ = q.drain("dev-1").await;
        assert_eq!(q.len("dev-1").await, 0);
        assert!(q.drain("dev-1").await.is_empty());
    }

    #[tokio::test]
    async fn caps_at_max_and_drops_oldest_first() {
        let q = OfflineQueue::new();
        for i in 0..(MAX_QUEUED_PER_DEVICE + 5) {
            q.push("dev-1", frame(&i.to_string())).await;
        }
        assert_eq!(q.len("dev-1").await, MAX_QUEUED_PER_DEVICE);
        let drained = q.drain("dev-1").await;
        // The oldest 5 (ids "0".."4") must have been evicted.
        assert_eq!(drained.first().unwrap().id, "5");
        assert_eq!(drained.last().unwrap().id, (MAX_QUEUED_PER_DEVICE + 4).to_string());
    }

    #[tokio::test]
    async fn devices_are_isolated() {
        let q = OfflineQueue::new();
        q.push("dev-1", frame("a")).await;
        q.push("dev-2", frame("b")).await;
        assert_eq!(q.len("dev-1").await, 1);
        assert_eq!(q.len("dev-2").await, 1);
        let drained_1 = q.drain("dev-1").await;
        assert_eq!(drained_1.len(), 1);
        assert_eq!(q.len("dev-2").await, 1);
    }
}
