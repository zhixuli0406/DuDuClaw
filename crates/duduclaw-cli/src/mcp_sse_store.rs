// mcp_sse_store.rs — Per-connection SSE event store with ring buffer (W20-P1 Phase 2C)
//
// Provides `SseEventStore` which:
//   - Maintains a per-connection ring buffer (capacity = 1024 events).
//   - Supports `Last-Event-ID` based replay for reconnecting clients.
//   - Auto-evicts connections idle for longer than 10 minutes.
//   - Thread-safe: backed by `Mutex<HashMap>`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tracing::debug;

// ── Constants ─────────────────────────────────────────────────────────────────

const RING_BUFFER_CAPACITY: usize = 1024;
const IDLE_TTL: Duration = Duration::from_secs(600); // 10 minutes

// ── SseEvent ──────────────────────────────────────────────────────────────────

/// A single SSE event record stored in the ring buffer.
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// Monotonically increasing event ID, format: `evt_{u64}`.
    pub id: String,
    /// SSE event type (e.g. `tool_progress`, `tool_result`, `connected`).
    pub event: String,
    /// Raw event data (JSON string).
    pub data: String,
}

// ── ConnectionEntry ───────────────────────────────────────────────────────────

struct ConnectionEntry {
    /// Ring buffer of recent events for replay.
    buffer: VecDeque<SseEvent>,
    /// Broadcast sender to push live events to the SSE stream.
    tx: broadcast::Sender<String>,
    /// Last activity timestamp for idle TTL eviction.
    last_active: Instant,
}

impl ConnectionEntry {
    fn new(tx: broadcast::Sender<String>) -> Self {
        Self {
            buffer: VecDeque::with_capacity(RING_BUFFER_CAPACITY),
            tx,
            last_active: Instant::now(),
        }
    }

    fn push_event(&mut self, event: SseEvent) {
        // Evict oldest event when at capacity
        if self.buffer.len() >= RING_BUFFER_CAPACITY {
            self.buffer.pop_front();
        }
        // Broadcast to live subscribers (ignore send errors — receiver may have disconnected)
        let _ = self.tx.send(event.data.clone());
        self.buffer.push_back(event);
        self.last_active = Instant::now();
    }

    /// Replay events after the given `last_event_id`.
    /// Returns `Some(events)` if the ID is found; `None` if it's not in the buffer.
    fn replay_after(&self, last_event_id: &str) -> Option<Vec<SseEvent>> {
        let pos = self
            .buffer
            .iter()
            .position(|e| e.id == last_event_id)?;
        Some(self.buffer.iter().skip(pos + 1).cloned().collect())
    }
}

// ── SseEventStore ─────────────────────────────────────────────────────────────

/// Thread-safe event store for all active SSE connections.
///
/// Cheap to clone: all state is behind `Arc<Mutex<>>`.
#[derive(Clone, Default)]
pub struct SseEventStore {
    inner: Arc<Mutex<StoreInner>>,
}

#[derive(Default)]
struct StoreInner {
    connections: HashMap<String, ConnectionEntry>,
    /// Monotonic counter for event IDs.
    event_counter: u64,
}

impl StoreInner {
    fn next_event_id(&mut self) -> String {
        self.event_counter += 1;
        format!("evt_{}", self.event_counter)
    }
}

impl SseEventStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StoreInner {
                connections: HashMap::new(),
                event_counter: 0,
            })),
        }
    }

    /// Register a new SSE connection with the given ID and broadcast sender.
    pub fn register_connection(&self, conn_id: &str, tx: broadcast::Sender<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .connections
            .insert(conn_id.to_string(), ConnectionEntry::new(tx));
        debug!(conn_id, "SSE connection registered");
    }

    /// Push an event to the connection's ring buffer and live stream.
    ///
    /// Returns the assigned event ID, or `None` if the connection does not exist.
    pub fn push_event(&self, conn_id: &str, event_type: &str, data: &str) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        let event_id = inner.next_event_id();
        let entry = inner.connections.get_mut(conn_id)?;
        let evt = SseEvent {
            id: event_id.clone(),
            event: event_type.to_string(),
            data: data.to_string(),
        };
        entry.push_event(evt);
        Some(event_id)
    }

    /// Record an event in the ring buffer (for replay) without broadcasting.
    pub fn record(&self, conn_id: &str, event: SseEvent) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.connections.get_mut(conn_id) {
            entry.push_event(event);
        }
    }

    /// Replay events after `last_event_id` for a reconnecting client.
    ///
    /// Returns:
    /// - `Ok(events)` — events after that ID (may be empty if already up to date).
    /// - `Err(())` — event ID not found in ring buffer (client missed too many events).
    pub fn replay_after(&self, conn_id: &str, last_event_id: &str) -> Result<Vec<SseEvent>, ()> {
        let inner = self.inner.lock().unwrap();
        let entry = inner.connections.get(conn_id).ok_or(())?;
        entry.replay_after(last_event_id).ok_or(())
    }

    /// Remove connections that have been idle longer than `IDLE_TTL`.
    ///
    /// Should be called periodically by a background task.
    pub fn evict_idle(&self) {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        inner.connections.retain(|conn_id, entry| {
            let keep = now.duration_since(entry.last_active) < IDLE_TTL;
            if !keep {
                debug!(conn_id, "SSE connection evicted (idle TTL)");
            }
            keep
        });
    }

    /// Return the number of active connections (for tests / monitoring).
    pub fn connection_count(&self) -> usize {
        self.inner.lock().unwrap().connections.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    fn make_store() -> SseEventStore {
        SseEventStore::new()
    }

    fn make_conn(store: &SseEventStore, id: &str) -> broadcast::Receiver<String> {
        let (tx, rx) = broadcast::channel(64);
        store.register_connection(id, tx);
        rx
    }

    // ── Test: events are stored and broadcasted ───────────────────────────────
    #[tokio::test]
    async fn push_event_stores_and_broadcasts() {
        let store = make_store();
        let mut rx = make_conn(&store, "conn1");

        let id = store.push_event("conn1", "tool_result", r#"{"ok":true}"#);
        assert!(id.is_some(), "push_event should return an event ID");

        // Should receive the broadcast
        let received = rx.try_recv().expect("Should have received broadcast");
        assert_eq!(received, r#"{"ok":true}"#);
    }

    // ── Test: replay_after returns events after given ID ─────────────────────
    #[test]
    fn replay_after_returns_missed_events() {
        let store = make_store();
        make_conn(&store, "conn2");

        let id1 = store.push_event("conn2", "progress", "data1").unwrap();
        let _id2 = store.push_event("conn2", "progress", "data2").unwrap();
        let _id3 = store.push_event("conn2", "result", "data3").unwrap();

        let replayed = store.replay_after("conn2", &id1).expect("Should find id1");
        assert_eq!(replayed.len(), 2, "Should return events after id1");
        assert_eq!(replayed[0].data, "data2");
        assert_eq!(replayed[1].data, "data3");
    }

    // ── Test: replay_after unknown ID returns Err ─────────────────────────────
    #[test]
    fn replay_after_unknown_id_returns_err() {
        let store = make_store();
        make_conn(&store, "conn3");
        store.push_event("conn3", "progress", "data1");

        let result = store.replay_after("conn3", "evt_9999999");
        assert!(result.is_err(), "Unknown ID should return Err");
    }

    // ── Test: ring buffer evicts oldest events at capacity ────────────────────
    #[test]
    fn ring_buffer_evicts_oldest_at_capacity() {
        let store = make_store();
        make_conn(&store, "conn4");

        // Fill past capacity
        for i in 0..RING_BUFFER_CAPACITY + 5 {
            store.push_event("conn4", "progress", &format!("data{i}"));
        }

        // The total buffered should not exceed capacity
        let inner = store.inner.lock().unwrap();
        let entry = inner.connections.get("conn4").unwrap();
        assert_eq!(
            entry.buffer.len(),
            RING_BUFFER_CAPACITY,
            "Buffer should not exceed capacity"
        );
    }

    // ── Test: unknown connection push_event returns None ──────────────────────
    #[test]
    fn push_event_unknown_connection_returns_none() {
        let store = make_store();
        let result = store.push_event("nonexistent", "progress", "data");
        assert!(result.is_none(), "Pushing to unknown connection should return None");
    }

    // ── Test: different connections are isolated ──────────────────────────────
    #[test]
    fn connections_are_isolated() {
        let store = make_store();
        make_conn(&store, "connA");
        make_conn(&store, "connB");

        store.push_event("connA", "progress", "data_a");
        store.push_event("connB", "progress", "data_b");

        let inner = store.inner.lock().unwrap();
        assert_eq!(inner.connections["connA"].buffer.len(), 1);
        assert_eq!(inner.connections["connB"].buffer.len(), 1);
        assert_eq!(inner.connections["connA"].buffer[0].data, "data_a");
        assert_eq!(inner.connections["connB"].buffer[0].data, "data_b");
    }

    // ── Test: evict_idle removes stale connections ────────────────────────────
    #[test]
    fn evict_idle_removes_inactive_connections() {
        let store = make_store();
        let (tx, _rx) = broadcast::channel(4);
        // Manually insert a connection with an old timestamp
        {
            let mut inner = store.inner.lock().unwrap();
            let mut entry = ConnectionEntry::new(tx);
            // Backdate last_active by 11 minutes
            entry.last_active = Instant::now() - Duration::from_secs(660);
            inner.connections.insert("stale_conn".to_string(), entry);
        }

        assert_eq!(store.connection_count(), 1);
        store.evict_idle();
        assert_eq!(store.connection_count(), 0, "Stale connection should be evicted");
    }

    // ── Test: evict_idle preserves active connections ─────────────────────────
    #[test]
    fn evict_idle_preserves_active_connections() {
        let store = make_store();
        make_conn(&store, "active_conn");
        store.push_event("active_conn", "heartbeat", "ping");

        store.evict_idle();
        assert_eq!(store.connection_count(), 1, "Active connection should be preserved");
    }

    // ── Test: event IDs are monotonically increasing ──────────────────────────
    #[test]
    fn event_ids_are_monotonic() {
        let store = make_store();
        make_conn(&store, "conn5");

        let id1 = store.push_event("conn5", "e", "d1").unwrap();
        let id2 = store.push_event("conn5", "e", "d2").unwrap();
        let id3 = store.push_event("conn5", "e", "d3").unwrap();

        // IDs should be different and incrementing
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        // Parse the numeric suffix
        let n1: u64 = id1.strip_prefix("evt_").unwrap().parse().unwrap();
        let n2: u64 = id2.strip_prefix("evt_").unwrap().parse().unwrap();
        let n3: u64 = id3.strip_prefix("evt_").unwrap().parse().unwrap();
        assert!(n1 < n2);
        assert!(n2 < n3);
    }
}
