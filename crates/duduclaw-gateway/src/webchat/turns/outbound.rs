//! Bounded control traffic and a replaceable latest-progress slot.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::VecDeque;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(in crate::webchat) const CONTROL_CAPACITY: usize = 32;
// Reserve one slot for every allowed concurrently executing turn (config max 64).
const TERMINAL_CAPACITY: usize = 64;

struct QueuedFrame {
    frame: Outbound,
    owner: Option<Arc<AtomicU64>>,
    sequence: u64,
    // Keep staged frames charged to their lane until selected for writing.
    _permit: Option<OwnedSemaphorePermit>,
}

pub(in crate::webchat) struct Latest<T> {
    value: Mutex<Option<T>>,
    ready: tokio::sync::Notify,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self { value: Mutex::new(None), ready: tokio::sync::Notify::new() }
    }
}

impl<T> Latest<T> {
    pub(in crate::webchat) fn replace(&self, value: T) -> Option<T> {
        let previous = self.value.lock().unwrap().replace(value);
        self.ready.notify_one();
        previous
    }

    pub(in crate::webchat) fn take(&self) -> Option<T> {
        self.value.lock().unwrap().take()
    }

    pub(in crate::webchat) async fn recv(&self) -> T {
        loop {
            let ready = self.ready.notified();
            if let Some(value) = self.take() { return value; }
            ready.await;
        }
    }
}

struct ProgressFrame {
    message: Message,
    dropped: Arc<AtomicU64>,
}

#[derive(Clone)]
pub(in crate::webchat) struct OutboundSender {
    control: mpsc::Sender<QueuedFrame>,
    terminal: mpsc::Sender<QueuedFrame>,
    control_slots: Arc<Semaphore>,
    terminal_slots: Arc<Semaphore>,
    sequence: Arc<AtomicU64>,
    progress: Arc<Latest<ProgressFrame>>,
}

pub(in crate::webchat) struct OutboundReceiver {
    control: mpsc::Receiver<QueuedFrame>,
    terminal: mpsc::Receiver<QueuedFrame>,
    controls: VecDeque<QueuedFrame>,
    terminals: VecDeque<QueuedFrame>,
    progress: Arc<Latest<ProgressFrame>>,
}

pub(in crate::webchat) fn outbound_channel() -> (OutboundSender, OutboundReceiver) {
    let (tx, rx) = mpsc::channel(CONTROL_CAPACITY);
    let (terminal_tx, terminal_rx) = mpsc::channel(TERMINAL_CAPACITY);
    let progress = Arc::new(Latest::default());
    (OutboundSender { control: tx, terminal: terminal_tx,
        control_slots: Arc::new(Semaphore::new(CONTROL_CAPACITY)),
        terminal_slots: Arc::new(Semaphore::new(TERMINAL_CAPACITY)),
        sequence: Arc::new(AtomicU64::new(0)), progress: progress.clone() },
        OutboundReceiver { control: rx, terminal: terminal_rx, controls: VecDeque::new(),
            terminals: VecDeque::new(), progress })
}

impl OutboundSender {
    pub(in crate::webchat) async fn send(&self, frame: Outbound) -> Result<(), ()> {
        self.send_owned(frame, None, false).await
    }

    async fn send_owned(&self, frame: Outbound, owner: Option<Arc<AtomicU64>>, terminal: bool) -> Result<(), ()> {
        let deadline = frame.deadline;
        let enqueue = async {
            let lane = if terminal { &self.terminal } else { &self.control };
            let slots = if terminal { &self.terminal_slots } else { &self.control_slots };
            // Receiver shutdown also interrupts a sender waiting for capacity.
            let permit = Some(tokio::select! {
                _ = lane.closed() => return Err(()),
                permit = slots.clone().acquire_owned() => permit.map_err(|_| ())?,
            });
            let slot = lane.reserve().await.map_err(|_| ())?;
            // Assign order after capacity is acquired. Production sends from
            // one turn are awaited sequentially, including its cloned sinks.
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
            slot.send(QueuedFrame { frame, owner, sequence, _permit: permit });
            Ok(())
        };
        if let Some(deadline) = deadline {
            tokio::time::timeout_at(deadline, enqueue).await.map_err(|_| ())?
        } else {
            enqueue.await
        }
    }

    fn progress(&self, message: Message, dropped: Arc<AtomicU64>) -> Result<(), ()> {
        if self.control.is_closed() { return Err(()); }
        if let Some(previous) = self.progress.replace(ProgressFrame { message, dropped }) {
            previous.dropped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn finish_progress(&self, dropped: &Arc<AtomicU64>) {
        // Do not let this turn's obsolete progress follow its terminal frame.
        let mut slot = self.progress.value.lock().unwrap();
        if slot.as_ref().is_some_and(|frame| Arc::ptr_eq(&frame.dropped, dropped)) {
            slot.take();
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn same_owner(left: &QueuedFrame, right: &QueuedFrame) -> bool {
    match (&left.owner, &right.owner) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

impl OutboundReceiver {
    fn predecessor(&self, terminal: &QueuedFrame) -> Option<usize> {
        self.controls.iter().position(|control| {
            control.sequence < terminal.sequence && same_owner(control, terminal)
        })
    }

    pub(in crate::webchat) async fn recv(&mut self) -> Option<Outbound> {
        loop {
            // Staged terminal frames retain their permits, so draining the
            // channel never expands its bound. A ready terminal can bypass a
            // different turn's terminal that still has control predecessors.
            while let Ok(frame) = self.terminal.try_recv() { self.terminals.push_back(frame); }
            if !self.terminals.is_empty() {
                while let Ok(frame) = self.control.try_recv() { self.controls.push_back(frame); }
                let ready = self.terminals.iter().enumerate().position(|(index, terminal)| {
                    self.predecessor(terminal).is_none()
                        && !self.terminals.iter().take(index).any(|earlier| same_owner(earlier, terminal))
                });
                if let Some(index) = ready {
                    return self.terminals.remove(index).map(|queued| queued.frame);
                }
                // No terminal is ready yet. Advance one predecessor, then
                // reconsider every terminal before selecting another control.
                let index = self.predecessor(self.terminals.front().unwrap()).unwrap();
                return self.controls.remove(index).map(|queued| queued.frame);
            }
            if let Some(queued) = self.controls.pop_front() { return Some(queued.frame); }
            tokio::select! {
                biased;
                Some(frame) = self.terminal.recv() => self.terminals.push_back(frame),
                Some(frame) = self.control.recv() => return Some(frame.frame),
                progress = self.progress.recv(), if !self.control.is_closed() => return Some(Outbound { identity_notice: None,
                    message: progress.message, ack: None, deadline: None,
                }),
                else => return None,
            }
        }
    }
}

/// Reader-side controls use the same bounded writer, without owning the socket.
pub(in crate::webchat) struct SocketSender(pub(in crate::webchat) OutboundSender);
impl SocketSender {
    pub(in crate::webchat) async fn send(&mut self, message: Message) -> Result<(), ()> {
        self.0.send(Outbound { identity_notice: None, message, ack: None, deadline: None }).await
    }
}

fn is_redaction(value: &serde_json::Value) -> bool {
    value.get("type").and_then(serde_json::Value::as_str) == Some("thinking")
        && value.get("kind").and_then(serde_json::Value::as_str) == Some("redacted")
}

fn is_progress(value: &serde_json::Value) -> bool {
    !is_redaction(value) && matches!(value.get("type").and_then(serde_json::Value::as_str),
        Some("progress" | "thinking" | "step"))
}

fn is_terminal(value: &serde_json::Value) -> bool {
    matches!(value.get("type").and_then(serde_json::Value::as_str),
        Some("assistant_done" | "error" | "cancelled" | "no_reply"))
}

#[derive(Clone)]
pub(in crate::webchat) struct TurnSink {
    tx: OutboundSender,
    sid: String,
    conv: Option<String>,
    pub(in crate::webchat) msg_id: String,
    wait_ack: bool,
    pub(in crate::webchat) progress_dropped: Arc<AtomicU64>,
}

impl TurnSink {
    pub(in crate::webchat) fn new(tx: OutboundSender, sid: &str, conv: Option<String>, msg_id: &str) -> Self {
        Self { tx, sid: sid.into(), conv, msg_id: msg_id.into(), wait_ack: false,
            progress_dropped: Arc::new(AtomicU64::new(0)) }
    }

    pub(in crate::webchat) fn acknowledged(mut self) -> Self {
        self.wait_ack = true;
        self
    }

    /// This method accepts no reply content. The writer still checks revocation.
    pub(in crate::webchat) async fn identity_unavailable(
        &self, validity: crate::handlers::live_context::SocketValidity, admission: bool,
    ) {
        if validity != crate::handlers::live_context::SocketValidity::Unavailable { return; }
        self.tx.finish_progress(&self.progress_dropped);
        let notice = IdentityUnavailable::new(&self.sid, self.conv.clone(), &self.msg_id, admission);
        let frame = Outbound {
            message: notice.message(), identity_notice: Some(notice), ack: None,
            deadline: Some(tokio::time::Instant::now() + SEND_TIMEOUT),
        };
        let _ = self.tx.send_owned(frame, Some(self.progress_dropped.clone()), true).await;
    }

    pub(in crate::webchat) async fn frame(&self, frame: ChatMessage) {
        if let Ok(value) = serde_json::to_value(frame) { self.value(value).await; }
    }

    pub(in crate::webchat) async fn value(&self, value: serde_json::Value) {
        let terminal = is_terminal(&value);
        let _ = self.tx.send_owned(Outbound { identity_notice: None,
            message: self.envelope(value), ack: None, deadline: None,
        }, Some(self.progress_dropped.clone()), terminal).await;
    }

    fn envelope(&self, mut value: serde_json::Value) -> Message {
        if is_terminal(&value) {
            self.tx.finish_progress(&self.progress_dropped);
            let dropped = self.progress_dropped.load(Ordering::Relaxed);
            value["progress_dropped"] = serde_json::json!(dropped);
            if dropped > 0 {
                // The legacy summary describes only actually sent thinking.
                // Omit it if coalescing made that set incomplete.
                if let Some(fields) = value.as_object_mut() { fields.remove("thinking"); }
                warn!(session_id = %self.sid, msg_id = %self.msg_id, progress_dropped = dropped,
                    "WebChat progress coalesced before terminal delivery");
            }
        }
        if value.get("session_id").is_none() { value["session_id"] = serde_json::json!(self.sid); }
        value["conv"] = serde_json::json!(self.conv);
        value["msg_id"] = serde_json::json!(self.msg_id);
        Message::Text(value.to_string().into())
    }

    pub(in crate::webchat) async fn send(&mut self, frame: Message) -> Result<(), ()> {
        if let Message::Text(text) = frame {
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|_| ())?;
            if is_redaction(&value) { self.tx.finish_progress(&self.progress_dropped); }
            if is_progress(&value) {
                return self.tx.progress(self.envelope(value), self.progress_dropped.clone());
            }
            let terminal = is_terminal(&value);
            if !self.wait_ack || is_redaction(&value) {
                return self.tx.send_owned(Outbound { identity_notice: None,
                    message: self.envelope(value), ack: None, deadline: None,
                }, Some(self.progress_dropped.clone()), terminal).await;
            }
            let (ack, received) = oneshot::channel();
            let deadline = tokio::time::Instant::now() + SEND_TIMEOUT;
            self.tx.send_owned(Outbound {
                identity_notice: terminal.then(|| IdentityUnavailable::new(&self.sid, self.conv.clone(), &self.msg_id, false)),
                message: self.envelope(value), ack: Some(ack), deadline: Some(deadline),
            }, Some(self.progress_dropped.clone()), terminal).await?;
            if !tokio::time::timeout_at(deadline, received).await.ok()
                .and_then(Result::ok).unwrap_or(false) { return Err(()); }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn progress_flood_is_bounded_and_terminal_has_priority_after_reader_resumes() {
        let (tx, mut rx) = outbound_channel();
        let mut turn = TurnSink::new(tx.clone(), "s", None, "m").acknowledged();
        let mut other = TurnSink::new(tx.clone(), "other", None, "other");
        // Neither the writer nor the reader makes progress during this flood.
        for sequence in 0..100_000 {
            turn.send(Message::Text(serde_json::json!({
                "type":"progress", "sequence":sequence,
            }).to_string().into())).await.unwrap();
        }
        assert_eq!(tx.control.capacity(), CONTROL_CAPACITY);
        assert_eq!(turn.progress_dropped.load(Ordering::Relaxed), 99_999);
        {
            let pending = tx.progress.value.lock().unwrap();
            let Message::Text(latest) = &pending.as_ref().unwrap().message else { panic!("text"); };
            assert_eq!(serde_json::from_str::<serde_json::Value>(latest).unwrap()["sequence"], 99_999);
        }
        let terminal = tokio::spawn(async move {
            turn.send(Message::Text(serde_json::json!({"type":"assistant_done", "thinking":{"blocks":1}}).to_string().into())).await
        });
        tokio::task::yield_now().await;
        // Keep a different turn's progress queued: controls must still win.
        other.send(Message::Text(r#"{"type":"thinking"}"#.into())).await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        let frame = rx.recv().await.unwrap();
        let Message::Text(text) = &frame.message else { panic!("text"); };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["type"], "assistant_done");
        assert_eq!(value["progress_dropped"], 100_000);
        assert!(value.get("thinking").is_none());
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer = futures_util::sink::unfold(captured.clone(), |captured, message: Message| async {
            captured.lock().unwrap().push(message);
            Ok::<_, ()>(captured)
        });
        frame.deliver(&mut BoundedSocketSink(Box::pin(writer))).await.unwrap();
        assert!(terminal.await.unwrap().is_ok());
        assert_eq!(captured.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn full_control_lane_backpressures_without_dropping_controls() {
        let (tx, mut rx) = outbound_channel();
        for _ in 0..CONTROL_CAPACITY {
            tx.send(Outbound { identity_notice: None, message: Message::Ping(vec![].into()), ack: None, deadline: None }).await.unwrap();
        }
        let next = tokio::spawn(async move {
            tx.send(Outbound { identity_notice: None, message: Message::Pong(vec![].into()), ack: None, deadline: None }).await
        });
        tokio::task::yield_now().await;
        assert!(!next.is_finished());
        assert!(rx.recv().await.is_some());
        next.await.unwrap().unwrap();
        for _ in 1..CONTROL_CAPACITY { assert!(matches!(rx.recv().await.unwrap().message, Message::Ping(_))); }
        assert!(matches!(rx.recv().await.unwrap().message, Message::Pong(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn full_other_turn_control_lane_cannot_delay_terminal() {
        let (tx, mut rx) = outbound_channel();
        let other = TurnSink::new(tx.clone(), "other", None, "other");
        for sequence in 0..CONTROL_CAPACITY {
            other.value(serde_json::json!({"type":"notice", "sequence":sequence})).await;
        }
        assert_eq!(tx.control.capacity(), 0);
        let mut turn = TurnSink::new(tx, "s", None, "m").acknowledged();
        let started = tokio::time::Instant::now();
        let terminal = tokio::spawn(async move {
            turn.send(Message::Text(r#"{"type":"assistant_done"}"#.into())).await
        });
        tokio::task::yield_now().await;
        let frame = rx.recv().await.unwrap();
        let Message::Text(text) = &frame.message else { panic!("text"); };
        assert_eq!(serde_json::from_str::<serde_json::Value>(text).unwrap()["msg_id"], "m");
        let accepting = futures_util::sink::unfold((), |(), _: Message| async { Ok::<_, ()>(()) });
        frame.deliver(&mut BoundedSocketSink(Box::pin(accepting))).await.unwrap();
        terminal.await.unwrap().unwrap();
        assert_eq!(tokio::time::Instant::now(), started);
        // Staging the other controls must not free capacity or allow growth.
        assert_eq!(rx.controls.len(), CONTROL_CAPACITY);
        for sequence in 0..CONTROL_CAPACITY {
            let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text"); };
            assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["sequence"], sequence);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_waiting_for_own_controls_cannot_block_another_terminal() {
        let (tx, mut rx) = outbound_channel();
        let first = TurnSink::new(tx.clone(), "first", None, "first");
        for sequence in 0..CONTROL_CAPACITY {
            first.value(serde_json::json!({"type":"notice", "sequence":sequence})).await;
        }
        first.value(serde_json::json!({"type":"assistant_done"})).await;
        let mut second = TurnSink::new(tx.clone(), "second", None, "second").acknowledged();
        let started = tokio::time::Instant::now();
        let terminal = tokio::spawn(async move {
            second.send(Message::Text(r#"{"type":"assistant_done"}"#.into())).await
        });
        tokio::task::yield_now().await;
        let frame = rx.recv().await.unwrap();
        let Message::Text(text) = &frame.message else { panic!("text"); };
        assert_eq!(serde_json::from_str::<serde_json::Value>(text).unwrap()["msg_id"], "second");
        let accepting = futures_util::sink::unfold((), |(), _: Message| async { Ok::<_, ()>(()) });
        frame.deliver(&mut BoundedSocketSink(Box::pin(accepting))).await.unwrap();
        terminal.await.unwrap().unwrap();
        assert_eq!(tokio::time::Instant::now(), started);
        assert_eq!(rx.controls.len(), CONTROL_CAPACITY);
        assert_eq!(tx.control_slots.available_permits(), 0);
        assert_eq!(rx.terminals.len(), 1);
        assert_eq!(tx.terminal_slots.available_permits(), TERMINAL_CAPACITY - 1);
        for sequence in 0..CONTROL_CAPACITY {
            let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text"); };
            assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["sequence"], sequence);
        }
        let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text"); };
        assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["type"], "assistant_done");
        assert_eq!(tx.terminal_slots.available_permits(), TERMINAL_CAPACITY);
    }

    #[tokio::test]
    async fn terminal_keeps_own_control_order_without_draining_other_turn() {
        let (tx, mut rx) = outbound_channel();
        let turn = TurnSink::new(tx.clone(), "s", None, "m");
        let other = TurnSink::new(tx.clone(), "other", None, "other");
        for sequence in 0..CONTROL_CAPACITY {
            let owner = if sequence == 4 || sequence == 25 { &turn } else { &other };
            owner.value(serde_json::json!({"type":"notice", "sequence":sequence})).await;
        }
        turn.value(serde_json::json!({"type":"cancelled"})).await;
        for sequence in [4, 25] {
            let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text"); };
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["msg_id"], "m");
            assert_eq!(value["sequence"], sequence);
        }
        let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text"); };
        assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["type"], "cancelled");
        assert_eq!(rx.controls.len(), CONTROL_CAPACITY - 2);
        assert_eq!(tx.control_slots.available_permits(), 2);
    }

    #[tokio::test]
    async fn later_control_from_same_turn_cannot_overtake_terminal() {
        let (tx, mut rx) = outbound_channel();
        let turn = TurnSink::new(tx, "s", None, "m");
        turn.value(serde_json::json!({"type":"notice", "sequence":1})).await;
        turn.value(serde_json::json!({"type":"assistant_done"})).await;
        turn.value(serde_json::json!({"type":"notice", "sequence":2})).await;
        for expected in ["notice", "assistant_done", "notice"] {
            let Message::Text(text) = rx.recv().await.unwrap().message else { panic!("text"); };
            assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["type"], expected);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expired_frame_does_not_stop_socket_writer_or_deliver_stale_bytes() {
        let (tx, mut rx) = outbound_channel();
        // Queue a frame whose deadline expires while the writer stalls.
        let (ack, received_expired) = oneshot::channel();
        tx.send(Outbound { identity_notice: None, message: Message::Text("expired".into()), ack: Some(ack),
            deadline: Some(tokio::time::Instant::now() + SEND_TIMEOUT) }).await.unwrap();
        tokio::time::advance(SEND_TIMEOUT).await;
        tx.send(Outbound { identity_notice: None, message: Message::Text("live".into()), ack: None, deadline: None }).await.unwrap();
        drop(tx);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let captured = sent.clone();
        let accepting = futures_util::sink::unfold(captured, |captured, message: Message| async {
            captured.lock().unwrap().push(message);
            Ok::<_, ()>(captured)
        });
        let mut writer = BoundedSocketSink(Box::pin(accepting));
        while let Some(frame) = rx.recv().await {
            if frame.deliver(&mut writer).await.is_err() { break; }
        }
        assert!(!received_expired.await.unwrap());
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(matches!(&sent[0], Message::Text(text) if text.as_str() == "live"));
    }

    #[tokio::test]
    async fn ack_closed_after_precheck_before_first_send_poll_keeps_writer_usable() {
        let (ack, received) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + SEND_TIMEOUT;
        // Match deliver's successful precheck, then inject the race before its
        // post-precheck helper is first polled.
        assert!(!ack.is_closed());
        assert!(tokio::time::Instant::now() < deadline);
        drop(received);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let captured = sent.clone();
        let accepting = futures_util::sink::unfold(captured, |captured, message: Message| async {
            captured.lock().unwrap().push(message);
            Ok::<_, ()>(captured)
        });
        let mut writer = BoundedSocketSink(Box::pin(accepting));
        Outbound::deliver_acknowledged(Message::Text("stale".into()), ack, deadline, &mut writer)
            .await.unwrap();
        assert!(sent.lock().unwrap().is_empty());
        writer.send(Message::Text("live".into())).await.unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ack_closure_after_send_is_polled_still_closes_socket() {
        let (ack, received) = oneshot::channel();
        let polled = Arc::new(tokio::sync::Notify::new());
        let signal = polled.clone();
        let stalled = futures_util::sink::unfold(signal, |signal, _: Message| async move {
            signal.notify_one();
            std::future::pending::<Result<Arc<tokio::sync::Notify>, ()>>().await
        });
        let task = tokio::spawn(async move {
            let mut writer = BoundedSocketSink(Box::pin(stalled));
            Outbound { identity_notice: None, message: Message::Text("partial".into()), ack: Some(ack),
                deadline: Some(tokio::time::Instant::now() + SEND_TIMEOUT) }.deliver(&mut writer).await
        });
        polled.notified().await;
        drop(received);
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_after_precheck_before_first_send_poll_skips_without_socket_write() {
        let (ack, received) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + SEND_TIMEOUT;
        assert!(!ack.is_closed());
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::advance(SEND_TIMEOUT).await;
        // Even a ready sink must never be polled once the pre-write deadline passed.
        let sent = Arc::new(AtomicU64::new(0));
        let captured = sent.clone();
        let accepting = futures_util::sink::unfold(captured, |captured, _: Message| async {
            captured.fetch_add(1, Ordering::Relaxed);
            Ok::<_, ()>(captured)
        });
        let mut writer = BoundedSocketSink(Box::pin(accepting));
        Outbound::deliver_acknowledged(Message::Text("stale".into()), ack, deadline, &mut writer)
            .await.unwrap();
        assert!(!received.await.unwrap());
        assert_eq!(sent.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn callback_progress_slot_retains_only_latest_event() {
        let slot = Latest::default();
        let mut dropped = 0;
        for sequence in 0..100_000 {
            if slot.replace(sequence).is_some() { dropped += 1; }
        }
        assert_eq!(dropped, 99_999);
        assert_eq!(slot.recv().await, 99_999);
        assert!(slot.take().is_none());
    }
}
