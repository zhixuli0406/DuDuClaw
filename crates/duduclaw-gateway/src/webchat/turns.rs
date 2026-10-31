//! Shared admission, FIFO session lanes and explicit turn cancellation.
use super::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

mod run;
mod progress;
mod identity;
pub(super) use identity::IdentityUnavailable;
#[cfg(test)]
mod tests;
pub(super) use run::TurnInput;

tokio::task_local! {
    pub(crate) static TURN_ID: String;
}

#[derive(Default)]
pub(super) struct Registry {
    entries: Mutex<HashMap<String, Entry>>,
    #[cfg(test)]
    quota_reads: std::sync::atomic::AtomicUsize,
}

struct Entry {
    user: String,
    sid: String,
    conv: Option<String>,
    msg_id: String,
    cancel: CancellationToken,
    cancel_request_id: Option<String>,
    delivering: bool,
    admitted: bool,
    active: bool,
    tail: Option<oneshot::Receiver<()>>,
}

pub(super) struct Ticket {
    registry: Arc<Registry>,
    pub id: String,
    pub cancel: CancellationToken,
    principal: String,
    started: AtomicBool,
    predecessor: Option<oneshot::Receiver<()>>,
    finished: Option<oneshot::Sender<()>>,
}

/// Widget identities rotate per connection; the transport IP does not.
pub(super) fn cancel_principal(user: &str, peer_ip: std::net::IpAddr) -> String {
    if is_widget_visitor(user) { format!("ip:{peer_ip}") } else { format!("user:{user}") }
}

impl Registry {
    /// Queued turns reserve capacity too: no hidden, unbounded queue.
    fn reserve(
        self: &Arc<Self>,
        user: &str,
        sid: &str,
        conv: Option<String>,
        msg_id: &str,
        limit: usize,
        global_limit: usize,
    ) -> Result<Ticket, &'static str> {
        let mut entries = self.entries.lock().unwrap();
        if entries
            .values()
            .any(|e| e.user == user && e.sid == sid && e.msg_id == msg_id)
        {
            return Err("duplicate_message");
        }
        if entries.len() >= global_limit || entries.values().filter(|e| e.user == user).count() >= limit {
            return Err("turn_limit");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        entries.insert(
            id.clone(),
            Entry {
                user: user.into(),
                sid: sid.into(),
                conv,
                msg_id: msg_id.into(),
                cancel: cancel.clone(),
                cancel_request_id: None,
                delivering: false,
                admitted: false,
                active: false,
                tail: None,
            },
        );
        Ok(Ticket {
            registry: self.clone(),
            id,
            cancel,
            principal: format!("user:{user}"),
            started: AtomicBool::new(false),
            predecessor: None,
            finished: None,
        })
    }

    #[cfg(test)]
    pub(super) fn admit(
        self: &Arc<Self>, user: &str, sid: &str, conv: Option<String>, msg_id: &str,
        limit: usize, global_limit: usize,
    ) -> Result<Ticket, &'static str> {
        let mut ticket = self.reserve(user, sid, conv, msg_id, limit, global_limit)?;
        ticket.join_lane(sid);
        Ok(ticket)
    }

    /// Check durable limits before exposing an accepted ticket. Fail closed if
    /// the store is unavailable; cancellation never calls this admission gate.
    pub(super) async fn admit_persistent(
        self: &Arc<Self>, broker: Option<&crate::approval::ApprovalBroker>, principal: &str,
        user: &str, sid: &str, conv: Option<String>, msg_id: &str,
        limit: usize, global_limit: usize,
    ) -> Result<Ticket, &'static str> {
        // Reserve both caps before quota I/O; join the FIFO only once accepted.
        // A refused reservation drops its slot without touching the session lane.
        let mut ticket = self.reserve(user, sid, conv, msg_id, limit, global_limit)?;
        let broker = broker.ok_or("turn_limit")?;
        #[cfg(test)]
        self.quota_reads.fetch_add(1, Ordering::Relaxed);
        broker.check_webchat_turn_admission(principal).await.map_err(|error| {
            warn!(%error, "WebChat persistent admission refused");
            "turn_limit"
        })?;
        ticket.principal = principal.to_owned();
        ticket.join_lane(sid);
        Ok(ticket)
    }

    pub(super) fn cancel(
        &self, user: &str, sid: Option<&str>, conv: Option<&str>, msg: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<(), ()> {
        if sid.is_none() && conv.is_none() {
            return Err(());
        }
        let mut entries = self.entries.lock().unwrap();
        let mut matches: Vec<_> = entries
            .values_mut()
            .filter(|e| {
                e.admitted && e.user == user
                    && sid.is_none_or(|s| e.sid == s)
                    && conv.is_none_or(|c| e.conv.as_deref() == Some(c))
                    && msg.map_or(e.active, |m| e.msg_id == m)
            })
            .collect();
        // Without msg_id, select the active turn; a conv matching several sessions is ambiguous.
        if matches.len() != 1 || matches[0].delivering || matches[0].cancel.is_cancelled() {
            return Err(());
        }
        matches[0].cancel_request_id = request_id.map(str::to_owned);
        matches[0].cancel.cancel();
        Ok(())
    }
}

impl Ticket {
    fn join_lane(&mut self, sid: &str) {
        let mut entries = self.registry.entries.lock().unwrap();
        self.predecessor = entries.values_mut()
            .find(|e| e.sid == sid && e.tail.is_some()).and_then(|e| e.tail.take());
        let (finished, tail) = oneshot::channel();
        self.finished = Some(finished);
        let entry = entries.get_mut(&self.id).unwrap();
        entry.admitted = true;
        entry.active = self.predecessor.is_none();
        entry.tail = Some(tail);
    }

    /// Serialize credential cancellation with the delivery fence. Once delivery
    /// starts, its awaited validity checks own refusal and archive rollback.
    fn cancel_unless_delivering(&self) -> bool {
        let entries = self.registry.entries.lock().unwrap();
        let entry = entries.get(&self.id).unwrap();
        if entry.delivering {
            return false;
        }
        entry.cancel.cancel();
        true
    }

    pub(super) fn begin_delivery(&self) -> bool {
        let mut entries = self.registry.entries.lock().unwrap();
        let entry = entries.get_mut(&self.id).unwrap();
        if entry.cancel.is_cancelled() {
            return false;
        }
        entry.delivering = true;
        true
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.registry.entries.lock().unwrap().remove(&self.id);
        if let Some(finished) = self.finished.take() {
            let _ = finished.send(());
        }
    }
}

/// Invalid/unreadable configuration denies admission; an absent key defaults to 3.
pub(super) fn configured_limit(home: &std::path::Path) -> usize {
    configured_bound(home, "max_in_flight_turns", 3)
}

pub(super) fn configured_global_limit(home: &std::path::Path) -> usize {
    configured_bound(home, "max_global_turns", 10)
}

fn configured_bound(home: &std::path::Path, key: &str, default: usize) -> usize {
    let text = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return default,
        Err(_) => return 0,
    };
    let Ok(config) = text.parse::<toml::Value>() else {
        return 0;
    };
    let section = match config.get("webchat") {
        None => return default,
        Some(section) if section.is_table() => section,
        Some(_) => return 0,
    };
    match section.get(key) {
        None => default,
        Some(value) => value
            .as_integer()
            .filter(|n| (1..=64).contains(n))
            .map_or(0, |n| n as usize),
    }
}

/// Match the authenticated socket heartbeat cadence, even after disconnect.
const CREDENTIAL_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// A refused turn must release its ticket even if the writer withholds output.
pub(super) async fn credential_allows_turn(
    validity: crate::handlers::live_context::SocketValidity,
    sink: &TurnSink,
) -> bool {
    use crate::handlers::live_context::SocketValidity;
    match validity {
        SocketValidity::Valid => true,
        SocketValidity::Revoked => false,
        SocketValidity::Unavailable => {
            sink.identity_unavailable(validity, true).await;
            false
        }
    }
}

/// This future is owned by the turn: no detached watcher task or join delay.
async fn watch_credential<C, F>(ticket: &Ticket, mut check: C)
where
    C: FnMut() -> F,
    F: std::future::Future<Output = crate::handlers::live_context::SocketValidity>,
{
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + CREDENTIAL_CHECK_INTERVAL, CREDENTIAL_CHECK_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut recovery = crate::handlers::live_context::SocketRecovery::default();
    loop {
        interval.tick().await;
        recovery.observe(check().await, true);
        if recovery.closed() {
            if ticket.cancel_unless_delivering() {
                return;
            }
            // Returning would win credential_watched's select and drop delivery.
            // Leave the work alive to finish its own refusal/rollback path.
            std::future::pending::<()>().await;
        }
    }
}

async fn credential_watched<F, W>(future: F, watcher: W)
where
    F: std::future::Future<Output = ()>,
    W: std::future::Future<Output = ()>,
{
    tokio::select! {
        biased;
        () = watcher => {},
        () = future => {},
    }
}

pub(super) const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Every socket write, including ping/auth frames, shares the same deadline.
pub(super) struct BoundedSocketSink<S>(pub S);
impl<S: futures_util::Sink<Message> + Unpin> BoundedSocketSink<S> {
    pub(super) async fn send(&mut self, message: Message) -> Result<(), ()> {
        tokio::time::timeout(SEND_TIMEOUT, self.0.send(message)).await
            .map_err(|_| ())?.map_err(|_| ())
    }
}

/// Only the socket writer owns the sink. Detached turns enqueue additive envelopes.
pub(super) struct Outbound {
    pub message: Message,
    pub ack: Option<oneshot::Sender<bool>>,
    pub deadline: Option<tokio::time::Instant>,
    pub identity_notice: Option<IdentityUnavailable>,
}

impl Outbound {
    /// Recheck queued frames at the writer, after all queueing delays. Rejected
    /// terminal acknowledgements make the turn roll back its owned archives.
    pub(super) async fn deliver_authorized<S: futures_util::Sink<Message> + Unpin>(
        self,
        validity: crate::handlers::live_context::SocketValidity,
        recovery: &mut crate::handlers::live_context::SocketRecovery,
        sink: &mut BoundedSocketSink<S>,
    ) -> Result<(), ()> {
        if !recovery.observe(validity, false) {
            let notice_live = self.deadline.is_none_or(|deadline| tokio::time::Instant::now() < deadline)
                && self.ack.as_ref().is_none_or(|ack| !ack.is_closed());
            if let Some(ack) = self.ack { let _ = ack.send(false); }
            if recovery.closed() {
                let _ = sink.send(Message::Close(None)).await;
                return Err(());
            }
            // Only typed, content-free refusal metadata can cross a temporary
            // identity outage. Never serialize or forward the queued reply here.
            if notice_live {
                if let Some(notice) = self.identity_notice {
                    return sink.send(notice.message()).await;
                }
            }
            return Ok(());
        }
        self.deliver(sink).await
    }

    pub(super) async fn deliver<S: futures_util::Sink<Message> + Unpin>(
        self, sink: &mut BoundedSocketSink<S>,
    ) -> Result<(), ()> {
        if self.identity_notice.is_some()
            && self.deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            if let Some(ack) = self.ack { let _ = ack.send(false); }
            return Ok(());
        }
        let Some(ack) = self.ack else { return sink.send(self.message).await; };
        let Some(deadline) = self.deadline else {
            let _ = ack.send(false);
            return Ok(());
        };
        // Expiration before a write only fails this turn's delivery. The socket
        // remains usable because no bytes from this frame have been written.
        if ack.is_closed() || tokio::time::Instant::now() >= deadline {
            let _ = ack.send(false);
            return Ok(());
        }
        Self::deliver_acknowledged(self.message, ack, deadline, sink).await
    }

    // Separate the post-precheck stage so tests can deterministically close the
    // receiver in the gap before select first polls without production sleeps.
    async fn deliver_acknowledged<S: futures_util::Sink<Message> + Unpin>(
        message: Message,
        mut ack: oneshot::Sender<bool>,
        deadline: tokio::time::Instant,
        sink: &mut BoundedSocketSink<S>,
    ) -> Result<(), ()> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let send_started = AtomicBool::new(false);
        let result = {
            let send = sink.0.send(message);
            tokio::pin!(send);
            let tracked_send = std::future::poll_fn(|cx| {
                // Timeout may poll its inner future even when its timer has
                // elapsed. Refuse that first poll before touching the socket.
                if !send_started.load(Ordering::Relaxed) && tokio::time::Instant::now() >= deadline {
                    return std::task::Poll::Ready(Err(()));
                }
                send_started.store(true, Ordering::Relaxed);
                std::future::Future::poll(send.as_mut(), cx).map(|result| result.map_err(|_| ()))
            });
            let write_deadline = deadline.min(tokio::time::Instant::now() + SEND_TIMEOUT);
            tokio::select! {
                biased;
                _ = ack.closed() => Err(()),
                sent = tokio::time::timeout_at(write_deadline, tracked_send) => sent.unwrap_or(Err(())),
            }
        };
        let _ = ack.send(result.is_ok());
        // Failure after polling the socket can leave a partial frame. A closed
        // receiver or deadline before that poll only fails this turn's delivery.
        if send_started.load(Ordering::Relaxed) { result } else { Ok(()) }
    }
}

mod outbound;
use outbound::Latest;
pub(super) use outbound::TurnSink;
pub(super) use outbound::{outbound_channel, SocketSender};

pub(super) async fn execute(input: TurnInput, sink: TurnSink, ticket: Ticket) {
    supervise(&sink, execute_inner(input, sink.clone(), ticket)).await;
}

async fn supervise<F: std::future::Future<Output = ()>>(sink: &TurnSink, future: F) {
    use futures_util::FutureExt;
    if std::panic::AssertUnwindSafe(future)
        .catch_unwind().await.is_err() {
        sink.frame(ChatMessage::error_coded("Internal turn error", "internal_error")).await;
    }
}

/// Match admission: explicit selections need access; resumes and every `@`
/// call also need operational/may_call checks, even when the agent is unchanged.
/// Cancellation bypasses denial so the normal cancellation cleanup records it.
pub(super) async fn recheck_turn_access<A, R>(
    cancel: &CancellationToken,
    resume_acl: bool,
    access: A,
    resume: R,
) -> bool
where
    A: std::future::Future<Output = bool>,
    R: std::future::Future<Output = bool>,
{
    tokio::select! {
        biased;
        _ = cancel.cancelled() => true,
        allowed = async { access.await && (!resume_acl || resume.await) } => allowed,
    }
}

async fn execute_inner(input: TurnInput, sink: TurnSink, mut ticket: Ticket) {
    // Even a cancelled waiter keeps its FIFO link until its predecessor ends.
    if let Some(previous) = ticket.predecessor.take() {
        let _ = previous.await;
    }
    ticket
        .registry
        .entries
        .lock()
        .unwrap()
        .get_mut(&ticket.id)
        .unwrap()
        .active = true;
    let state = input.state.clone();
    let sid = input.sid.clone();
    let auth_user = input.auth_user.clone();
    let agent = input
        .effective_agent
        .as_deref()
        .unwrap_or(&input.agent_id)
        .to_owned();
    if !credential_allows_turn(
        state.credential_validity(input.credential.as_ref(), true).await, &sink,
    ).await {
        return;
    }
    let allowed = recheck_turn_access(
        &ticket.cancel,
        input.resume_acl,
        async {
            match &input.requested_agent {
                Some(agent) => state.agent_access_allowed(&input.auth_user, agent).await,
                None => true,
            }
        },
        state.may_resume_into(&input.auth_user, &agent, &sid, &input.user_id),
    ).await;
    if !ticket.cancel.is_cancelled() && !allowed {
        sink.frame(ChatMessage::error_coded("Agent access revoked", "agent_forbidden")).await;
        return;
    }
    // Keep the large model/reply future out of the nested cancellation state
    // machines, including on default-size Tokio worker stacks.
    let credential = input.credential.clone();
    let work = Box::pin(run::run(input, sink.clone(), &ticket));
    let watcher = async {
        if credential.is_some() {
            watch_credential(&ticket, || state.credential_validity(credential.as_ref(), true)).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let (cancelled, cleanup) = run_cancellable(
        &sid, &ticket, credential_watched(work, watcher),
    ).await;
    if cancelled {
        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL
            .scope(
                auth_user,
                finish_cancel(
                    &state.ctx.home_dir,
                    state.approvals.as_ref(),
                    &state.ctx.session_manager,
                    &sid,
                    &agent,
                    &ticket,
                    &sink,
                    cleanup,
                ),
            )
            .await;
    } else if let Err(error) = cleanup {
        warn!(%error, "WebChat delivery cleanup failed");
        // A notice is ignored by older dashboards; never append an error bubble
        // after assistant_done, whose delivery may already have succeeded.
        sink.value(serde_json::json!({"type":"notice", "code":"delivery_cleanup_failed"})).await;
    }
}

async fn run_cancellable<F: std::future::Future<Output = ()>>(
    sid: &str,
    ticket: &Ticket,
    future: F,
) -> (bool, Result<(), String>) {
    crate::channel_reply::with_delivery_cleanup(async {
        let work = async {
            // Set only on the first poll of work, after the biased cancel arm.
            // Queued or ACL-waiting turns cancelled before this point cannot
            // have raised approvals and must not allocate a durable fence.
            ticket.started.store(true, Ordering::Release);
            crate::approval::scope_turn(
                sid, &ticket.id, TURN_ID.scope(ticket.id.clone(), Box::pin(future)),
            ).await
        };
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = ticket.cancel.cancelled() => true,
            () = &mut work => ticket.cancel.is_cancelled(),
        }
        // The scoped future is dropped before cancellation cleanup starts.
    })
    .await
}

async fn finish_cancel(
    home: &std::path::Path,
    broker: Option<&crate::approval::ApprovalBroker>,
    sessions: &crate::session::SessionManager,
    sid: &str,
    agent: &str,
    ticket: &Ticket,
    sink: &TurnSink,
    cleanup: Result<(), String>,
) {
    let withdrawal = if ticket.started.load(Ordering::Acquire) {
        match broker {
            Some(broker) => broker.withdraw_turn_for_principal(sid, &ticket.id, Some(&ticket.principal))
                .await.map(|result| result.approved_before_cancel),
            None => Err("WebChat approval store unavailable".into()),
        }
    } else {
        Ok(Vec::new())
    };
    // The fence must commit before clearing the queue. Dispatch rechecks it
    // for a late enqueue after this UPDATE, before handing work to a runtime.
    let withdrawal = match withdrawal {
        Ok(result) if ticket.started.load(Ordering::Acquire) => {
            let home = home.to_path_buf();
            let queue = tokio::task::spawn_blocking(move || crate::message_queue::MessageQueue::open(&home))
                .await.map_err(|e| format!("open delegation queue task: {e}")).and_then(|result| result);
            match queue {
                Ok(queue) => queue.cancel_pending_turn(sid, &ticket.id).await
                    .map(|count| (result, count)),
                Err(error) => Err(error),
            }
        }
        Ok(result) => Ok((result, 0)),
        Err(error) => Err(error),
    };
    let result = sessions.record_webchat_cancel(sid, agent, &ticket.id, &sink.msg_id).await;
    let request_id = ticket.registry.entries.lock().unwrap()
        .get(&ticket.id).and_then(|entry| entry.cancel_request_id.clone());
    match (result, withdrawal, cleanup) {
        (Ok(()), Ok((withdrawal, delegations_cancelled)), Ok(())) => {
            sink.value(serde_json::json!({"type":"error", "message":"Turn cancelled", "code":"cancelled",
                "approved_before_cancel":withdrawal, "delegations_cancelled":delegations_cancelled, "request_id":request_id})).await;
        }
        (result, withdrawal, cleanup) => {
            warn!(
                ?result,
                ?withdrawal,
                ?cleanup,
                "WebChat cancellation cleanup failed"
            );
            sink.frame(ChatMessage::error_coded(
                "Turn stopped but cancellation cleanup failed",
                "cancel_failed",
            )).await;
        }
    }
}

#[cfg(test)]
mod socket_tests;
