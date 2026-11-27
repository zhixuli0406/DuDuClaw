//! Shared admission, FIFO session lanes and explicit turn cancellation.
use super::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

mod run;
#[cfg(test)]
pub(crate) mod entry_gate_all_helpers;
mod progress;
mod subscriptions;
mod writer;
pub(super) use writer::{GuardedSocket, SocketGate};
pub(super) use subscriptions::Subscriptions;
mod identity;
pub(super) mod downstream;
pub(super) use identity::IdentityUnavailable;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod no_responder_tests;
#[cfg(test)]
mod injection_block_tests;
pub(super) use run::TurnInput;

tokio::task_local! {
    pub(crate) static TURN_ID: String;
}

/// Dispatch budget includes separators; an oversized single message runs alone unchanged.
const MAX_MERGED_CHARS: usize = 32_000;
const MERGE_SEPARATOR: &str = "\n\n---\n";

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum AdmissionReason { UserLimit, GlobalLimit, CancelQuota, StoreUnavailable, ConfigInvalid, DuplicateMessage, ConversationQueue }

#[derive(Debug, serde::Serialize)]
pub(super) struct AdmissionError {
    pub reason: AdmissionReason,
    pub limit: usize,
    pub running: usize,
    pub queued: usize,
}
impl AdmissionError {
    pub fn code(&self) -> &'static str {
        if self.reason == AdmissionReason::DuplicateMessage { "duplicate_message" } else { "turn_limit" }
    }
    pub fn log_refusal(&self, user: &str, employee: &str) {
        let record = self.record(user, employee);
        tracing::info!(event = "webchat_turn_refused", record = %record, "WebChat admission refused");
    }
    pub fn record(&self, user: &str, employee: &str) -> serde_json::Value {
        serde_json::json!({"event":"webchat_turn_refused", "user":user, "employee":employee,
            "reason":self.reason, "limit":self.limit, "running":self.running, "queued":self.queued,
            "timestamp":chrono::Utc::now().to_rfc3339()})
    }
}

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CancelOutcome { Waiting, Running }

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MergedOutcome { Done, Failed, Cancelled }

struct PendingMessage {
    credential: Option<(u64, Option<String>, Option<String>)>,
    content: String,
    attachments: Vec<ChatAttachment>,
}

#[derive(Default)]
pub(super) struct Registry {
    entries: Mutex<HashMap<String, Entry>>,
    offers: Mutex<downstream::Offers>,
    #[cfg(test)]
    quota_reads: std::sync::atomic::AtomicUsize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CancelCause { Person, CredentialRevoked, CredentialUnavailable }

struct Entry {
    user: String,
    agent: Option<String>,
    origin: Option<String>,
    sid: String,
    conv: Option<String>,
    msg_id: String,
    cancel: CancellationToken,
    delivery_cancel: CancellationToken,
    cancel_request_id: Option<String>,
    cancel_cause: CancelCause,
    delivering: bool,
    admitted: bool,
    active: bool,
    tail: Option<oneshot::Receiver<()>>,
    sequence: usize,
    pending: Option<PendingMessage>,
    merged_into: Option<String>,
    result: Arc<Mutex<MergedOutcome>>,
}

pub(super) struct Ticket {
    registry: Arc<Registry>,
    pub id: String,
    pub admission_generation: Option<String>,
    pub cancel: CancellationToken,
    pub delivery_cancel: CancellationToken,
    principal: String,
    lineage_principal: Option<String>,
    started: AtomicBool,
    predecessor: Option<oneshot::Receiver<()>>,
    finished: Option<oneshot::Sender<()>>,
    originals: Vec<(String, String, usize)>,
    merged: Vec<String>,
    result: Arc<Mutex<MergedOutcome>>,
}

/// Widget identities rotate per connection; the transport IP does not.
pub(super) fn cancel_principal(user: &str, peer_ip: std::net::IpAddr) -> String {
    if is_widget_visitor(user) { format!("ip:{peer_ip}") } else { format!("user:{user}") }
}

impl Registry {
    pub(super) fn logout_origin(&self, user: &str, origin: &str, resumed: &[(String, Option<String>)]) {
        for entry in self.entries.lock().unwrap().values_mut() {
            if entry.admitted && entry.user == user && (entry.origin.as_deref() == Some(origin)
                || resumed.iter().any(|(sid, conv)| entry.sid == *sid && entry.conv == *conv && conv.is_some())) {
                entry.cancel_cause = CancelCause::CredentialRevoked;
                entry.delivery_cancel.cancel();
                // Let a delivery already in progress finish recording its row
                // before rollback; dropping an in-flight DB write loses its id.
                if !entry.delivering { entry.cancel.cancel(); }
            }
        }
    }

    pub(super) fn pending_agent(&self, user: &str, sid: &str) -> Option<String> {
        self.entries.lock().unwrap().values()
            .find(|entry| entry.admitted && entry.user == user && entry.sid == sid && !entry.cancel.is_cancelled())
            .and_then(|entry| entry.agent.clone())
    }

    /// Queued turns reserve capacity too: no hidden, unbounded queue.
    fn reserve(
        self: &Arc<Self>,
        user: &str,
        sid: &str,
        conv: Option<String>,
        msg_id: &str,
        limit: usize,
        global_limit: usize,
    ) -> Result<Ticket, AdmissionError> {
        let mut entries = self.entries.lock().unwrap();
        let counts = |global: bool| {
            let live: Vec<_> = entries.values().filter(|e| !e.cancel.is_cancelled() && e.merged_into.is_none() && (global || e.user == user)).collect();
            (live.iter().filter(|e| e.active).count(), live.iter().filter(|e| !e.active).count())
        };
        let (running, queued) = counts(false);
        let (global_running, global_queued) = counts(true);
        let refusal = |reason, limit, running, queued| AdmissionError { reason, limit, running, queued };
        if entries
            .values()
            .any(|e| e.user == user && e.sid == sid && e.msg_id == msg_id)
        {
            return Err(refusal(AdmissionReason::DuplicateMessage, limit, running, queued));
        }
        if limit == 0 || global_limit == 0 {
            return Err(refusal(AdmissionReason::ConfigInvalid, 0, running, queued));
        }
        if global_running + global_queued >= global_limit {
            return Err(refusal(AdmissionReason::GlobalLimit, global_limit, global_running, global_queued));
        }
        if running + queued >= limit {
            return Err(refusal(AdmissionReason::UserLimit, limit, running, queued));
        }
        let sequence = entries.values().map(|e| e.sequence).max().unwrap_or(0) + 1;
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let delivery_cancel = CancellationToken::new();
        let result = Arc::new(Mutex::new(MergedOutcome::Failed));
        entries.insert(
            id.clone(),
            Entry {
                user: user.into(),
                agent: None,
                origin: None,
                sid: sid.into(),
                conv,
                msg_id: msg_id.into(),
                cancel: cancel.clone(),
                delivery_cancel: delivery_cancel.clone(),
                cancel_request_id: None,
                cancel_cause: CancelCause::Person,
                delivering: false,
                admitted: false,
                active: false,
                tail: None,
                sequence, pending: None, merged_into: None, result: result.clone(),
            },
        );
        Ok(Ticket {
            registry: self.clone(),
            admission_generation: None,
            id,
            cancel,
            delivery_cancel,
            principal: format!("user:{user}"),
            lineage_principal: None,
            started: AtomicBool::new(false),
            predecessor: None,
            finished: None,
            originals: Vec::new(), merged: Vec::new(), result,
        })
    }

    #[cfg(test)]
    pub(super) fn admit(
        self: &Arc<Self>, user: &str, sid: &str, conv: Option<String>, msg_id: &str,
        limit: usize, global_limit: usize,
    ) -> Result<Ticket, AdmissionError> {
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
    ) -> Result<Ticket, AdmissionError> {
        // Reserve both caps before quota I/O; join the FIFO only once accepted.
        // A refused reservation drops its slot without touching the session lane.
        let mut ticket = self.reserve(user, sid, conv, msg_id, limit, global_limit)?;
        let error = |reason, bound| {
            let entries = self.entries.lock().unwrap();
            let live: Vec<_> = entries.iter().filter(|(id, e)| **id != ticket.id && e.user == user && !e.cancel.is_cancelled()).map(|(_, e)| e).collect();
            AdmissionError { reason, limit: bound, running: live.iter().filter(|e| e.active).count(), queued: live.iter().filter(|e| !e.active).count() }
        };
        let broker = broker.ok_or_else(|| error(AdmissionReason::StoreUnavailable, limit))?;
        #[cfg(test)]
        self.quota_reads.fetch_add(1, Ordering::Relaxed);
        broker.check_webchat_turn_admission(principal).await.map_err(|error| {
            warn!(%error, "WebChat persistent admission refused");
            if error == "turn_limit" {
                // The durable gate's exact closed sentinel, never a substring match.
                AdmissionError { reason: AdmissionReason::CancelQuota, limit: 100, ..self.snapshot(user) }
            } else {
                AdmissionError { reason: AdmissionReason::StoreUnavailable, limit, ..self.snapshot(user) }
            }
        })?;
        ticket.principal = principal.to_owned();
        ticket.join_lane(sid);
        Ok(ticket)
    }

    fn snapshot(&self, user: &str) -> AdmissionError {
        let entries = self.entries.lock().unwrap();
        let live: Vec<_> = entries.values().filter(|e| e.user == user && e.admitted && !e.cancel.is_cancelled() && e.merged_into.is_none()).collect();
        AdmissionError { reason: AdmissionReason::ConversationQueue, limit: 0,
            running: live.iter().filter(|e| e.active).count(), queued: live.iter().filter(|e| !e.active).count() }
    }

    pub(super) fn cancel(
        &self, user: &str, sid: Option<&str>, conv: Option<&str>, msg: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<CancelOutcome, ()> {
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
        if matches.len() != 1 || matches[0].delivering || matches[0].merged_into.is_some() || matches[0].cancel.is_cancelled() {
            return Err(());
        }
        matches[0].cancel_request_id = request_id.map(str::to_owned);
        matches[0].pending = None;
        matches[0].cancel.cancel();
        Ok(if matches[0].active { CancelOutcome::Running } else { CancelOutcome::Waiting })
    }
}

impl Ticket {
    pub(super) fn queue_notice(&self, limit: usize) -> Option<AdmissionError> {
        let entries = self.registry.entries.lock().unwrap();
        let own = entries.get(&self.id)?;
        if own.active || is_widget_visitor(&own.user) || own.sid.starts_with("odoolivechat:") { return None; }
        let user = own.user.clone();
        drop(entries);
        Some(AdmissionError { limit, ..self.registry.snapshot(&user) })
    }

    pub(super) fn stage(&self, input: &TurnInput) {
        if is_widget_visitor(&input.auth_user) || input.sid.starts_with("odoolivechat:")
            || crate::chat_commands::is_command(&input.content) { return; }
        self.registry.entries.lock().unwrap().get_mut(&self.id).unwrap().pending = Some(PendingMessage {
            credential: input.credential.as_ref().map(|c| match c {
                crate::webchat::ChatCredential::Jwt(c) => (c.credential_epoch, c.jti.clone(), None),
                crate::webchat::ChatCredential::Member(c) => (c.credential_epoch, None, c.member_key_id.clone()),
            }),
            content: input.content.clone(), attachments: input.attachments.clone(),
        });
    }

    /// Claim only a consecutive FIFO prefix. Claimed messages can no longer be
    /// cancelled separately: they are now part of this dispatched turn.
    fn merge_waiting(&mut self, input: &mut TurnInput) -> bool {
        let mut entries = self.registry.entries.lock().unwrap();
        let own = entries.get_mut(&self.id).unwrap();
        if own.merged_into.is_some() { return false; }
        let was_waiting = !own.active;
        own.active = true;
        let own_pending = own.pending.take();
        if !was_waiting || own_pending.is_none() || self.cancel.is_cancelled() { return true; }
        let mut chars = input.content.chars().count();
        self.originals.push((own.msg_id.clone(), input.content.clone(), input.attachments.len()));
        let agent = own.agent.clone();
        let credential = own_pending.unwrap().credential;
        let mut candidates: Vec<_> = entries.iter_mut().filter(|(id, e)| **id != self.id
            && e.admitted && e.sid == input.sid && !e.active && !e.cancel.is_cancelled() && e.merged_into.is_none()).collect();
        candidates.sort_by_key(|(_, e)| e.sequence);
        for (_, entry) in candidates {
            if entry.user != input.auth_user || entry.agent != agent { break; }
            let Some(message) = entry.pending.as_ref() else { break; };
            if message.credential != credential { break; }
            let size = message.content.chars().count() + MERGE_SEPARATOR.chars().count();
            if chars + size > MAX_MERGED_CHARS
                || input.attachments.len() + message.attachments.len() > super::uploads::MAX_FILES { break; }
            let message = entry.pending.take().unwrap();
            // truncate_chars preserves all characters here; the budget was checked above.
            input.content.push_str(MERGE_SEPARATOR);
            input.content.push_str(&duduclaw_core::truncate_chars(&message.content, MAX_MERGED_CHARS));
            chars += size;
            self.originals.push((entry.msg_id.clone(), message.content, message.attachments.len()));
            input.attachments.extend(message.attachments);
            self.merged.push(entry.msg_id.clone());
            entry.merged_into = Some(self.id.clone());
            entry.result = self.result.clone();
        }
        true
    }

    pub(super) fn set_origin(&self, agent: &str, origin: &str) {
        let mut entries = self.registry.entries.lock().unwrap();
        let entry = entries.get_mut(&self.id).unwrap();
        entry.agent = Some(agent.into());
        entry.origin = Some(origin.into());
    }

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

    fn cancel_for_cause(&self, cause: CancelCause) -> bool {
        let mut entries = self.registry.entries.lock().unwrap();
        let entry = entries.get_mut(&self.id).unwrap();
        if entry.delivering {
            return false;
        }
        entry.cancel_cause = cause;
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
        if self.cancel.is_cancelled() {
            if self.predecessor.is_some() && self.finished.is_some() {
                let previous = self.predecessor.take().unwrap();
                let finished = self.finished.take().unwrap();
                // Retain only the FIFO link until its predecessor finishes. It
                // holds no payload or admission capacity. Keeping its tail in
                // the registry also fences messages arriving AFTER cancellation.
                let registry = self.registry.clone();
                let id = self.id.clone();
                tokio::spawn(async move {
                    let _ = previous.await;
                    registry.entries.lock().unwrap().remove(&id);
                    let _ = finished.send(());
                });
                return;
            }
        }
        self.registry.entries.lock().unwrap().remove(&self.id);
        if let Some(finished) = self.finished.take() { let _ = finished.send(()); }
    }
}

/// Invalid/unreadable configuration denies admission; an absent key defaults to 3.
pub(super) fn configured_limit(home: &std::path::Path) -> usize {
    configured_bound(home, "max_in_flight_turns", 3)
}

pub(crate) fn configured_global_limit(home: &std::path::Path) -> usize {
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
        let validity = check().await;
        recovery.observe(validity, true);
        if recovery.closed() {
            let cause = if validity == crate::handlers::live_context::SocketValidity::Revoked {
                CancelCause::CredentialRevoked
            } else { CancelCause::CredentialUnavailable };
            if ticket.cancel_for_cause(cause) {
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
    pub route: Option<subscriptions::Route>,
    pub message: Message,
    pub ack: Option<oneshot::Sender<bool>>,
    pub deadline: Option<tokio::time::Instant>,
    pub identity_notice: Option<IdentityUnavailable>,
}

impl Outbound {
    /// Prepare live authority, then let GuardedSocket enforce its generation
    /// below every buffered adapter. Backpressure never holds the ACL lease.
    pub(super) async fn deliver_live<S: futures_util::Sink<Message> + Unpin>(
        self, state: &WebChatState, credential: Option<&crate::webchat::ChatCredential>,
        recovery: &mut crate::handlers::live_context::SocketRecovery,
        sink: &mut BoundedSocketSink<S>, gate: &SocketGate,
    ) -> Result<(), ()> {
        use std::pin::Pin;
        let Some(route) = self.route.as_ref() else {
            let validity = state.chat_credential_validity(credential, true).await;
            return self.deliver_authorized(validity, recovery, sink).await;
        };
        if !route.live() || !route.authorized(state).await {
            if let Some(ack) = self.ack { let _ = ack.send(false); }
            return Ok(());
        }
        let deadline = self.deadline.unwrap_or_else(|| tokio::time::Instant::now() + SEND_TIMEOUT);
        if self.ack.as_ref().is_some_and(|ack| ack.is_closed())
            || tokio::time::Instant::now() >= deadline {
            if let Some(ack) = self.ack { let _ = ack.send(false); }
            return Ok(());
        }
        let ready = std::future::poll_fn(|cx| Pin::new(&mut sink.0).poll_ready(cx));
        if !matches!(tokio::time::timeout_at(deadline, ready).await, Ok(Ok(()))) {
            if let Some(ack) = self.ack { let _ = ack.send(false); }
            return Err(());
        }
        // Capture the generation BEFORE all final authority reads. Any write
        // during those reads invalidates the delivery, even if it grants access.
        let check = async {
            let db = state.user_db.clone();
            let generation = tokio::task::spawn_blocking(move || db.acl_generation())
                .await.ok().and_then(Result::ok);
            let validity = state.chat_credential_validity(credential, true).await;
            if generation.is_none() {
                route.source.refuse();
                return None;
            }
            if !recovery.observe(validity, false) || !route.authorized(state).await { return None; }
            let policy = state.turn_policy(&route.user, route.agent.as_deref(), true).await.ok()?;
            Some((generation?, policy))
        };
        let generation = tokio::time::timeout_at(deadline, check).await.ok().flatten();
        let Some((generation, policy)) = generation else {
            if let Some(ack) = self.ack { let _ = ack.send(false); }
            return if recovery.closed() { Err(()) } else { Ok(()) };
        };
        *gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(writer::WritePermit {
            users: state.user_db.clone(), generation, policy, route: route.clone(), deadline,
            home: state.ctx.home_dir.clone(), receiving_credential: credential.cloned(),
            contended: false,
        });
        // SplitSink::start_send only buffers a slot. The permit is enforced by
        // GuardedSocket below that adapter, when its flush reaches the socket.
        let sent = tokio::time::timeout_at(deadline, sink.0.send(self.message)).await;
        let contended = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
            .is_some_and(|permit| permit.contended);
        let result = if matches!(sent, Ok(Ok(()))) { Ok(()) } else { Err(()) };
        if let Some(ack) = self.ack { let _ = ack.send(result.is_ok()); }
        // The caller stops this socket's writer on Err, dropping every other
        // turn's queued frame. A frame withheld only for an exhausted wait
        // budget wrote no byte and left SplitSink's slot empty, so only this
        // turn fails (its ack above is false); the socket stays in service.
        if contended { return Ok(()); }
        result
    }

    /// Recheck queued frames at the writer, after all queueing delays. Rejected
    /// terminal acknowledgements make the turn roll back its owned archives.
    pub(super) async fn deliver_authorized<S: futures_util::Sink<Message> + Unpin>(
        self,
        validity: crate::handlers::live_context::SocketValidity,
        recovery: &mut crate::handlers::live_context::SocketRecovery,
        sink: &mut BoundedSocketSink<S>,
    ) -> Result<(), ()> {
        if self.route.as_ref().is_some_and(|route| !route.live()) {
            if let Some(ack) = self.ack { let _ = ack.send(false); }
            return Ok(());
        }
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

pub(super) async fn execute(input: TurnInput, sink: TurnSink, mut ticket: Ticket) {
    let state = input.state.clone();
    let sid = input.sid.clone();
    let turn_id = ticket.id.clone();
    let admission_generation = ticket.admission_generation.clone();
    // Keep the FIFO ticket alive through panic notice persistence.
    crate::channel_reply::FAILURE_SESSION.scope(Arc::new(std::sync::Mutex::new(None)), async {
        supervise(&sink, execute_inner(input, sink.clone(), &mut ticket), || async {
            persist_admitted_failure(&state.ctx.session_manager, &state.ctx.home_dir, &sid, &turn_id, crate::channel_reply::TurnFailureReason::Internal, admission_generation.as_deref()).await;
        }).await;
    }).await;
    *ticket.result.lock().unwrap() = sink.outcome();
}

async fn persist_failure(
    sessions: &crate::session::SessionManager, home: &std::path::Path, _sid: &str, turn: &str,
    reason: crate::channel_reply::TurnFailureReason,
) {
    persist_admitted_failure(sessions, home, _sid, turn, reason, None).await;
}

async fn persist_admitted_failure(
    sessions: &crate::session::SessionManager, home: &std::path::Path, admitted_sid: &str, turn: &str,
    reason: crate::channel_reply::TurnFailureReason, admission_generation: Option<&str>,
) {
    // No fallback to the current session: before binding there is no stored
    // question for this turn. In particular /new must not receive an old error.
    let Some((sid, generation, agent)) = crate::channel_reply::failure_session() else { return; };
    if sid == admitted_sid && admission_generation.is_some_and(|expected| expected != generation) { return; }
    let sid = sid.as_str();
    if sessions.session_agent(sid).await.ok().flatten().as_deref() != Some(agent.as_str()) { return; }
    let language = crate::prompt_identity::resolve_fixed_message_language_for_id(home, &agent).await;
    if let Err(error) = sessions.record_turn_failure(sid, turn, reason, language.as_deref(), Some(&generation)).await {
        warn!(%error, "WebChat failure notice persistence failed");
    }
}

async fn supervise<F, P, PF>(sink: &TurnSink, future: F, persist: P)
where
    F: std::future::Future<Output = ()>,
    P: FnOnce() -> PF,
    PF: std::future::Future<Output = ()>,
{
    use futures_util::FutureExt;
    if std::panic::AssertUnwindSafe(future)
        .catch_unwind().await.is_err() {
        persist().await;
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

async fn execute_inner(mut input: TurnInput, sink: TurnSink, ticket: &mut Ticket) {
    ticket.lineage_principal = Some(downstream::lineage_principal(&input.state.ctx.home_dir, &input.auth_user, &input.sid));
    if let Some(mut previous) = ticket.predecessor.take() {
        let cancelled = tokio::select! {
            biased;
            _ = ticket.cancel.cancelled() => true,
            _ = &mut previous => false,
        };
        if cancelled {
            ticket.predecessor = Some(previous);
            let agent = input.effective_agent.as_deref().unwrap_or(&input.agent_id);
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(input.auth_user.clone(),
                finish_cancel(&input.state.ctx.home_dir, input.state.approvals.as_ref(),
                    &input.state.ctx.session_manager, &input.sid, agent, ticket, &sink, Ok(())),
            ).await;
            return;
        }
    }
    if !ticket.merge_waiting(&mut input) {
        let outcome = *ticket.registry.entries.lock().unwrap().get(&ticket.id).unwrap().result.lock().unwrap();
        sink.value(serde_json::json!({"type":"turn_consumed", "outcome":outcome})).await;
        return;
    }
    if !ticket.merged.is_empty() {
        sink.value(serde_json::json!({"type":"turn_merged", "merged_msg_ids":ticket.merged})).await;
    }
    let state = input.state.clone();
    let sid = input.sid.clone();
    let auth_user = input.auth_user.clone();
    let agent = input
        .effective_agent
        .as_deref()
        .unwrap_or(&input.agent_id)
        .to_owned();
    if !credential_allows_turn(
        state.chat_credential_validity(input.credential.as_ref(), true).await, &sink,
    ).await {
        return;
    }
    // No employee is named and none resolves: only the no-responder notice
    // can answer, so the turn is authorised without a target.
    let target = (!state.no_responder_turn(&agent, &sid, &input.user_id).await).then_some(agent.as_str());
    let allowed = recheck_turn_access(
        &ticket.cancel,
        input.resume_acl,
        async {
            if state.authorize_turn(&input.auth_user, target, true).await.is_err() {
                return false;
            }
            match &input.requested_agent {
                Some(agent) => state.agent_access_allowed(&input.auth_user, agent).await,
                None => true,
            }
        },
        state.may_resume_into(&input.auth_user, &agent, &sid, &input.user_id),
    ).await;
    if !ticket.cancel.is_cancelled() && !allowed {
        sink.frame(state.target_error(&auth_user, ChatMessage::error_coded(
            "你已無法與此 AI 員工對話，請開啟新對話或聯絡管理員。", "agent_forbidden",
        )).await).await;
        return;
    }
    // Keep the large model/reply future out of the nested cancellation state
    // machines, including on default-size Tokio worker stacks.
    let credential = input.credential.clone();
    let work = Box::pin(run::run(input, sink.clone(), &ticket));
    let watcher = async {
        if credential.is_some() {
            watch_credential(&ticket, || state.chat_credential_validity(credential.as_ref(), true)).await;
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
    let (request_id, cause, user, active) = {
        let entries = ticket.registry.entries.lock().unwrap();
        let entry = entries.get(&ticket.id).unwrap();
        (entry.cancel_request_id.clone(), entry.cancel_cause, entry.user.clone(), entry.active)
    };
    if result.is_ok() && ticket.started.load(Ordering::Acquire) {
        let reason = if cause == CancelCause::Person {
            crate::channel_reply::TurnFailureReason::Cancelled
        } else {
            crate::channel_reply::TurnFailureReason::SafetyGate
        };
        persist_admitted_failure(sessions, home, sid, &ticket.id, reason, ticket.admission_generation.as_deref()).await;
    }
    let lineage_principal = ticket.lineage_principal.clone()
        .unwrap_or_else(|| downstream::lineage_principal(home, &user, sid));
    let offer = if cause == CancelCause::CredentialRevoked {
        crate::downstream_runs::stop(home, sid, &ticket.id, &lineage_principal,
            crate::downstream_runs::STOP_WAIT, "credential_revoked").await;
        None
    } else if cause == CancelCause::Person && !is_widget_visitor(&user) {
        let count = crate::downstream_runs::count(home, sid, &ticket.id, &lineage_principal);
        (count > 0).then(|| ticket.registry.offer(&user, &lineage_principal, sid, &ticket.id)
            .map(|reference| (count, reference))).flatten()
    } else { None };
    match (result, withdrawal, cleanup) {
        (Ok(()), Ok((withdrawal, delegations_cancelled)), Ok(())) => {
            let mut frame = serde_json::json!({"type":"error", "message":"Turn cancelled", "code":"cancelled",
                "cancel_outcome":if active { CancelOutcome::Running } else { CancelOutcome::Waiting },
                "approved_before_cancel":withdrawal, "delegations_cancelled":delegations_cancelled, "request_id":request_id});
            if let Some((count, reference)) = offer {
                frame["downstream_running"] = count.into();
                frame["downstream_ref"] = reference.into();
            }
            sink.value(frame).await;
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
