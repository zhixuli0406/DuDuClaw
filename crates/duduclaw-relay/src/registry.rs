//! In-process registry of live device WebSocket connections.
//!
//! One entry per `device_id`; a new authenticated connection evicts
//! (kicks) any previous one for the same device — "at most one active
//! connection per device" is enforced here, not by the client.
//!
//! **Deployment note**: this registry is per-process/in-memory, same as the
//! offline queue and the SQLite file it shares a host with. Running more
//! than one relay instance means a device's live connection lives on
//! exactly one instance while an inbound webhook can land on any instance
//! behind the load balancer, with no way for that instance to know the
//! device is connected elsewhere. See the crate README for the
//! single-instance deployment note this implies for Cloud Run.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

use crate::hook::HookFrame;

/// Bound on the outbound-frame channel per connection. A device that stops
/// draining its socket (dead peer, slow network) backs up here rather than
/// growing without limit; once full, new hooks fall back to the offline
/// queue exactly as if the device were disconnected (see `hook.rs`).
pub const OUTBOUND_CHANNEL_CAPACITY: usize = 64;

/// A message pushed to a live device connection's write loop.
#[derive(Debug, Clone)]
pub enum OutboundMessage {
    Hook(HookFrame),
}

struct ConnectionHandle {
    conn_id: Uuid,
    tx: mpsc::Sender<OutboundMessage>,
    evict_tx: Option<oneshot::Sender<()>>,
}

/// Cheap to clone — shares the underlying map via `Arc`.
#[derive(Clone, Default)]
pub struct ConnectionRegistry {
    inner: Arc<RwLock<HashMap<String, ConnectionHandle>>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a newly authenticated connection for `device_id`. Any
    /// connection currently holding that device's slot is kicked (sent a
    /// one-shot eviction signal so its read/write loop can close the socket
    /// and exit cleanly) before the new one takes over.
    ///
    /// Returns the new connection's id (used later to make cleanup
    /// idempotent-safe against races — see `deregister_if_current`) and the
    /// receiving half of the eviction signal, which the caller's main
    /// select loop should watch.
    pub async fn register(
        &self,
        device_id: &str,
        tx: mpsc::Sender<OutboundMessage>,
    ) -> (Uuid, oneshot::Receiver<()>) {
        let conn_id = Uuid::new_v4();
        let (evict_tx, evict_rx) = oneshot::channel();
        let mut guard = self.inner.write().await;
        if let Some(old) = guard.remove(device_id) {
            if let Some(kick) = old.evict_tx {
                let _ = kick.send(());
            }
        }
        guard.insert(
            device_id.to_string(),
            ConnectionHandle {
                conn_id,
                tx,
                evict_tx: Some(evict_tx),
            },
        );
        (conn_id, evict_rx)
    }

    /// Remove the registry entry for `device_id` iff it still belongs to
    /// `conn_id`. A stale connection's cleanup must never clobber a newer
    /// connection that already replaced it — callers only mark the device
    /// offline in storage when this returns `true`.
    pub async fn deregister_if_current(&self, device_id: &str, conn_id: Uuid) -> bool {
        let mut guard = self.inner.write().await;
        if guard.get(device_id).map(|h| h.conn_id) == Some(conn_id) {
            guard.remove(device_id);
            true
        } else {
            false
        }
    }

    /// Attempt to hand `msg` to the device's live connection. Returns
    /// `false` (never blocks) when the device isn't connected, or its
    /// outbound channel is full/closed — callers treat that identically to
    /// "offline" and fall back to the durable offline queue.
    pub async fn try_send(&self, device_id: &str, msg: OutboundMessage) -> bool {
        let guard = self.inner.read().await;
        match guard.get(device_id) {
            Some(h) => h.tx.try_send(msg).is_ok(),
            None => false,
        }
    }

    pub async fn is_registered(&self, device_id: &str) -> bool {
        self.inner.read().await.contains_key(device_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::HookFrame;
    use std::collections::BTreeMap;

    fn dummy_frame() -> HookFrame {
        HookFrame::new(crate::channel::Channel::Line, BTreeMap::new(), b"x")
    }

    #[tokio::test]
    async fn try_send_fails_when_no_connection() {
        let reg = ConnectionRegistry::new();
        assert!(!reg.try_send("nobody", OutboundMessage::Hook(dummy_frame())).await);
    }

    #[tokio::test]
    async fn try_send_delivers_to_registered_connection() {
        let reg = ConnectionRegistry::new();
        let (tx, mut rx) = mpsc::channel(4);
        let (_conn_id, _evict_rx) = reg.register("dev-1", tx).await;

        assert!(reg.try_send("dev-1", OutboundMessage::Hook(dummy_frame())).await);
        let received = rx.recv().await;
        assert!(matches!(received, Some(OutboundMessage::Hook(_))));
    }

    #[tokio::test]
    async fn second_registration_evicts_the_first() {
        let reg = ConnectionRegistry::new();
        let (tx1, _rx1) = mpsc::channel(4);
        let (conn_id_1, mut evict_rx_1) = reg.register("dev-1", tx1).await;

        let (tx2, _rx2) = mpsc::channel(4);
        let (conn_id_2, _evict_rx_2) = reg.register("dev-1", tx2).await;

        assert_ne!(conn_id_1, conn_id_2);
        // The first connection's eviction signal must have fired.
        assert!(evict_rx_1.try_recv().is_ok());
        assert!(reg.is_registered("dev-1").await);
    }

    #[tokio::test]
    async fn deregister_if_current_ignores_stale_conn_id() {
        let reg = ConnectionRegistry::new();
        let (tx1, _rx1) = mpsc::channel(4);
        let (conn_id_1, _evict_rx_1) = reg.register("dev-1", tx1).await;

        let (tx2, _rx2) = mpsc::channel(4);
        let (_conn_id_2, _evict_rx_2) = reg.register("dev-1", tx2).await;

        // conn_id_1 is stale now — deregistering with it must not remove
        // the newer (conn_id_2) entry.
        assert!(!reg.deregister_if_current("dev-1", conn_id_1).await);
        assert!(reg.is_registered("dev-1").await);
    }

    #[tokio::test]
    async fn deregister_if_current_removes_matching_entry() {
        let reg = ConnectionRegistry::new();
        let (tx, _rx) = mpsc::channel(4);
        let (conn_id, _evict_rx) = reg.register("dev-1", tx).await;

        assert!(reg.deregister_if_current("dev-1", conn_id).await);
        assert!(!reg.is_registered("dev-1").await);
    }
}
