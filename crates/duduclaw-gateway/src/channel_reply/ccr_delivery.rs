use super::*;

/// O-4→O-3 wiring: strip an O-4 `<system_operator_pending>` marker (if
/// present) out of `raw` and map it to an O-3 chat-artifact
/// (`os_operator::marker_to_artifact`). Called from BOTH funnel points below
/// (`build_guarded_reply_for_agent` / `build_guarded_reply_with_session`)
/// — the only two places `build_reply_with_session_inner`'s raw text is ever
/// consumed — so the tag is stripped before ANY channel sees the reply text,
/// regardless of whether that channel's caller asks for the artifact half.
/// `os_operator::strip_system_operator_pending_tag` is fail-open (returns the
/// input unchanged when no tag is present), so this is a no-op on the
/// overwhelming majority of replies that never went through O-4 at all.
pub(super) fn strip_operator_pending_marker(raw: &str) -> (String, Option<serde_json::Value>) {
    let (stripped, marker) = crate::os_operator::strip_system_operator_pending_tag(raw);
    let artifact = marker
        .as_ref()
        .and_then(crate::os_operator::marker_to_artifact);
    (stripped, artifact)
}

/// Returned when a source lease cannot be retained through reply delivery.
pub const CCR_DELIVERY_REFUSED_TEXT: &str =
    "The source changed before this reply could be sent. Please try again.";

/// Returned to callers that have nowhere to hold a delivery lease at all
/// (the legacy string-only builders — today only the ACP/A2A bridge).
///
/// The source has NOT necessarily changed here, so the wording must not claim
/// it did: retrying on the same transport would only produce the same answer
/// and the same refusal.
pub const CCR_LEASE_UNSUPPORTED_TEXT: &str =
    "此回覆用到受保護來源，目前的 ACP 通道無法保留交付租約，因此不予輸出。請改用支援租約的通道（Telegram／Slack／Discord／WebChat 等）再試一次。";

tokio::task_local! {
    /// One channel turn's CCR delivery gate. Scoped by the guarded reply
    /// builders around `build_reply_with_session_inner`, so work the inner
    /// pipeline persists or schedules BEFORE the lease is rechecked can be
    /// undone (or never started) when delivery is refused.
    ///
    /// Absent for non-channel callers, which keep the previous behaviour.
    pub(super) static CCR_TURN_DELIVERY: Arc<CcrTurnDelivery>;
}

/// WebChat keeps the live authority and persisted user-row acknowledgement
/// inside the pipeline, including early knowledge-only replies.
#[derive(Clone)]
pub(crate) struct WebChatTurnDelivery {
    pub state: Arc<crate::webchat::WebChatState>,
    pub user: String,
    pub client_msg_id: String,
    pub accepted: tokio::sync::mpsc::Sender<(String, i64)>,
    pub refused: Arc<std::sync::atomic::AtomicBool>,
}

tokio::task_local! {
    pub(crate) static WEBCHAT_TURN_DELIVERY: WebChatTurnDelivery;
}

pub(super) async fn webchat_commit_allowed(agent: &str) -> bool {
    let Ok(turn) = WEBCHAT_TURN_DELIVERY.try_with(Clone::clone) else { return true; };
    if turn.refused.load(std::sync::atomic::Ordering::SeqCst) { return false; }
    if turn.state.authorize_target(&turn.user, agent, true).await.is_err() {
        turn.refused.store(true, std::sync::atomic::Ordering::SeqCst);
        return false;
    }
    true
}

/// Persist the turn's assistant reply, bound to the session row the turn
/// started in (`generation`, see `SessionManager::turn_generation`). When a
/// user cleared that conversation during the turn the write is dropped
/// (`session::is_cleared_during_turn`); a WebChat turn is then refused as a
/// whole, so the staged-row fallback (`GuardedReply::persist_webchat`) cannot
/// write the reply into the fresh conversation either.
pub(super) async fn append_turn_assistant(
    sessions: &SessionManager, sid: &str, text: &str, tokens: u32, generation: Option<&str>,
) -> duduclaw_core::error::Result<i64> {
    let pending = WEBCHAT_TURN_DELIVERY.try_with(|_| ()).is_ok();
    let result = sessions
        .append_delivery_message_in(sid, "assistant", text, tokens, None, pending, generation)
        .await;
    if let Err(e) = &result {
        if crate::session::is_cleared_during_turn(e) {
            let _ = WEBCHAT_TURN_DELIVERY
                .try_with(|turn| turn.refused.store(true, std::sync::atomic::Ordering::SeqCst));
        }
    }
    result
}

/// Per-turn record of everything that must not outlive a refused CCR delivery.
pub(crate) struct CcrTurnDelivery {
    /// `(session_id, row id)` of the assistant message already persisted by
    /// `build_reply_with_session_inner`.
    pub(super) assistant_message: std::sync::Mutex<Option<(String, i64)>>,
    /// Deferred work (today: conversation distillation) waiting for the
    /// delivery verdict.
    pub(super) waiters: std::sync::Mutex<Vec<tokio::sync::oneshot::Sender<bool>>>,
    /// One verdict per turn. A guarded reply publishes it when it is dropped
    /// (send finished) or as soon as a recheck loses the lease, whichever
    /// comes first — never twice.
    pub(super) settled: std::sync::atomic::AtomicBool,
    /// RM-173: the turn generation (`created_at` of the session row the turn
    /// started in), recorded once the turn ensured its session. `None`: not
    /// recorded (an early reply path) — writes are then unbound as before.
    pub(super) turn_generation: std::sync::Mutex<Option<String>>,
    cleanup: Option<DeliveryCleanup>,
    published: std::sync::atomic::AtomicBool,
}

type DeliveryCleanup = Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<Result<(), String>>>>>;

tokio::task_local! {
    static DELIVERY_CLEANUP: DeliveryCleanup;
}

impl Default for CcrTurnDelivery {
    fn default() -> Self {
        Self {
            assistant_message: Default::default(),
            waiters: Default::default(),
            settled: Default::default(),
            turn_generation: Default::default(),
            cleanup: DELIVERY_CLEANUP.try_with(Clone::clone).ok(),
            published: Default::default(),
        }
    }
}

/// Keep the session FIFO occupied until cancellation's spawned row revocations
/// finish. Guards capture the collector at construction, before their task-local
/// scope can be dropped. Other channel callers retain their existing behavior.
pub(crate) async fn with_delivery_cleanup<F: std::future::Future>(future: F) -> (F::Output, Result<(), String>) {
    let jobs: DeliveryCleanup = Arc::new(std::sync::Mutex::new(Vec::new()));
    use futures_util::FutureExt;
    // Drop unwinding guards inside the collector scope, then join revocations
    // before rethrowing to the turn supervisor and releasing its FIFO ticket.
    let result = DELIVERY_CLEANUP.scope(jobs.clone(),
        std::panic::AssertUnwindSafe(future).catch_unwind()).await;
    let pending = std::mem::take(&mut *jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    let mut cleanup = Ok(());
    for job in pending {
        match job.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => cleanup = Err(error),
            Err(error) => cleanup = Err(error.to_string()),
        }
    }
    match result {
        Ok(result) => (result, cleanup),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

impl CcrTurnDelivery {
    pub(super) fn waits_for_turn_delivery(&self) -> bool {
        self.cleanup.is_some()
    }

    pub(super) fn subscribe(&self) -> tokio::sync::oneshot::Receiver<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(tx);
        rx
    }

    /// Synchronous half of [`Self::settle`]: publish the verdict to deferred
    /// work exactly once and hand back the assistant row that a refusal must
    /// erase. Idempotent — the second caller gets `None`.
    pub(super) fn publish(&self, delivered: bool) -> Option<(String, i64)> {
        let delivered = delivered && (!self.waits_for_turn_delivery()
            || self.published.load(std::sync::atomic::Ordering::SeqCst));
        if self
            .settled
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return None;
        }
        let waiters = std::mem::take(
            &mut *self
                .waiters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for waiter in waiters {
            // A dropped receiver is normal (the spawned task may be gone).
            let _ = waiter.send(delivered);
        }
        if delivered {
            return None;
        }
        self.assistant_message
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Publish the verdict and, when the reply was refused, erase the
    /// source-derived assistant message this turn already committed.
    pub(super) async fn settle(&self, session_mgr: &Arc<SessionManager>, delivered: bool) {
        let Some(row) = self.publish(delivered) else { return; };
        if let Some(done) = self.start_revocation(session_mgr.clone(), row) {
            // The owned job remains in the cleanup collector if this waiter is
            // dropped while waiting for a pooled database connection.
            let _ = done.await;
        }
    }

    fn start_revocation(
        &self, session_mgr: Arc<SessionManager>, (session_id, message_id): (String, i64),
    ) -> Option<tokio::sync::oneshot::Receiver<()>> {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(session_id, message_id,
                "CCR delivery refused but no runtime remains to revoke the assistant turn");
            return None;
        };
        let (done, completed) = tokio::sync::oneshot::channel();
        let task = handle.spawn(async move {
            let result = session_mgr.revoke_message(&session_id, message_id, CCR_DELIVERY_REFUSED_TEXT).await;
            let result = match result {
                Ok(true) => {
                    warn!(session_id, message_id, "CCR delivery refused — assistant turn revoked from session history");
                    Ok(())
                }
                Ok(false) => Ok(()),
                Err(error) => {
                    warn!(session_id, message_id, %error, "CCR assistant revocation failed");
                    Err(error.to_string())
                }
            };
            let _ = done.send(());
            result
        });
        if let Some(cleanup) = &self.cleanup {
            cleanup.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(task);
        }
        Some(completed)
    }

}

/// Remember the assistant row this turn just persisted so a later CCR refusal
/// can revoke it. No-op outside a guarded reply.
pub(super) fn record_turn_assistant_message(session_id: &str, message_id: i64) {
    let _ = CCR_TURN_DELIVERY.try_with(|delivery| {
        *delivery
            .assistant_message
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((session_id.to_owned(), message_id));
    });
}

/// RM-173: remember the turn generation so a later staged-reply write
/// (`GuardedReply::persist_webchat`) is bound to the row the turn started
/// in. No-op outside a guarded reply.
pub(super) fn record_turn_generation(generation: Option<&str>) {
    let _ = CCR_TURN_DELIVERY.try_with(|delivery| {
        *delivery
            .turn_generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = generation.map(str::to_owned);
    });
}

/// Subscribe to this turn's CCR delivery verdict. `None` means no gate is in
/// scope (non-channel caller) — callers then keep their previous behaviour.
pub(super) fn ccr_delivery_verdict() -> Option<tokio::sync::oneshot::Receiver<bool>> {
    CCR_TURN_DELIVERY
        .try_with(|delivery| delivery.subscribe())
        .ok()
}

/// The turn-scoped rollback handle a guarded reply keeps so a revocation
/// observed *during* delivery reaches the same undo path a build-time
/// refusal takes (`CcrTurnDelivery::settle` → `SessionManager::revoke_message`
/// plus the distillation verdict). Only attached when the turn actually holds
/// a CCR lease; a lease-free turn settles at build time as before.
#[derive(Clone)]
pub(super) struct TurnRollback {
    pub(super) delivery: Arc<CcrTurnDelivery>,
    pub(super) session_mgr: Arc<SessionManager>,
}

impl std::fmt::Debug for TurnRollback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TurnRollback")
    }
}

/// Own rollback before the raw pipeline has produced a `GuardedReply`.
/// Cancellation can happen after the assistant row was persisted but while
/// citations, thinking metadata, restoration or the contract are still awaited.
pub(super) struct TurnBuildRollback(Option<TurnRollback>);

impl TurnBuildRollback {
    pub(super) fn new(delivery: Arc<CcrTurnDelivery>, session_mgr: Arc<SessionManager>) -> Self {
        Self(Some(TurnRollback { delivery, session_mgr }))
    }

    pub(super) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TurnBuildRollback {
    fn drop(&mut self) {
        if let Some(rollback) = self.0.take() {
            rollback.settle_on_drop(false);
        }
    }
}

impl TurnRollback {
    /// Shared synchronous verdict plus asynchronous row rollback for both
    /// raw-pipeline cancellation and a guarded reply's final delivery verdict.
    fn settle_on_drop(self, delivered: bool) {
        let Some((session_id, message_id)) = self.delivery.publish(delivered) else {
            return;
        };
        self.delivery.start_revocation(self.session_mgr, (session_id, message_id));
    }
}

/// A channel reply that owns every CCR source lease used to form its answer.
/// Keep this value alive until the final channel send has completed.
///
/// Every adapter rechecks `still_valid` immediately before each outbound
/// segment; WebChat additionally rechecks after its send. So the guarantee
/// this type actually provides is "nothing is sent after a revocation is
/// observed", not "every send is bracketed by two checks".
///
/// Revalidation is async on purpose: one guard chain opens three to four
/// SQLite connections (and wiki-backed sources also read files), which must
/// never run on a tokio worker thread. Any recheck that loses the lease also
/// rolls the turn back — adapters do not have to do anything beyond the
/// `still_valid` call they already make.
#[derive(Debug)]
pub struct GuardedReply {
    pub text: String,
    pub artifact: Option<serde_json::Value>,
    /// The employee that answered (absent when the turn never resolved one,
    /// e.g. a refused delivery).
    pub answered_by: Option<crate::trigger_route::AnsweredBy>,
    /// Knowledge-base sources the reply cites (WebChat shows them; messaging
    /// apps already carry them as a text footer).
    pub citations: Vec<duduclaw_knowledge::cite::CitationRef>,
    pub(super) guards: Vec<duduclaw_llm::CcrDeliveryGuards>,
    /// True when the lease was already lost when this value was built, so the
    /// turn's persisted side effects must be rolled back.
    pub(super) refused: bool,
    pub(super) rollback: Option<TurnRollback>,
    /// Set by the first recheck that observes a lost lease (or by a legacy
    /// conversion that withholds the answer). Latches: once the turn is known
    /// not to have been delivered, no later check may report otherwise.
    pub(super) revoked: Arc<std::sync::atomic::AtomicBool>,
}

/// Read-only source fence for the socket writer; refusal latches on the owning reply.
#[derive(Clone)]
pub(crate) struct ReplyDeliveryFence {
    cancel: Option<tokio_util::sync::CancellationToken>,
    guards: Vec<duduclaw_llm::CcrDeliveryGuards>,
    revoked: Arc<std::sync::atomic::AtomicBool>,
}

impl ReplyDeliveryFence {
    #[cfg(test)]
    pub(crate) fn test_live() -> Self {
        Self { cancel: None, guards: Vec::new(), revoked: Arc::new(std::sync::atomic::AtomicBool::new(false)) }
    }

    pub(crate) fn refuse(&self) {
        self.revoked.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn for_turn(mut self, cancel: &tokio_util::sync::CancellationToken) -> Self {
        self.cancel = Some(cancel.clone());
        self
    }

    pub(crate) fn latched_valid(&self) -> bool {
        if self.cancel.as_ref().is_some_and(|cancel| cancel.is_cancelled()) { self.refuse(); }
        !self.revoked.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) async fn valid(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.cancel.as_ref().is_some_and(|cancel| cancel.is_cancelled()) { self.refuse(); }
        if self.revoked.load(Ordering::SeqCst) { return false; }
        let guards = self.guards.clone();
        let valid = tokio::task::spawn_blocking(move || guards.iter()
            .all(duduclaw_llm::CcrDeliveryGuards::still_valid_blocking)).await.unwrap_or(false);
        if !valid { self.revoked.store(true, Ordering::SeqCst); }
        if self.cancel.as_ref().is_some_and(|cancel| cancel.is_cancelled()) { self.refuse(); }
        valid && !self.revoked.load(Ordering::SeqCst)
    }
}

impl GuardedReply {
    pub(crate) fn delivery_fence(&self) -> ReplyDeliveryFence {
        ReplyDeliveryFence { cancel: None, guards: self.guards.clone(), revoked: self.revoked.clone() }
    }

    pub(super) async fn new(
        text: String,
        artifact: Option<serde_json::Value>,
        guards: Vec<duduclaw_llm::CcrDeliveryGuards>,
        rollback: Option<TurnRollback>,
    ) -> Self {
        let reply = Self {
            text,
            artifact,
            answered_by: None,
            citations: Vec::new(),
            guards,
            refused: false,
            rollback,
            revoked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        // Declared after `reply` so cancellation publishes refusal before
        // `reply`'s ordinary Drop could publish successful delivery.
        let mut building = TurnBuildRollback(reply.rollback.clone());
        if reply.still_valid().await {
            building.disarm();
            return reply;
        }
        // `still_valid` already ran the rollback; drop the handle so the
        // refusal value below does not publish a second verdict.
        drop(reply);
        Self::refused()
    }

    /// Attach the answering employee.
    pub(super) fn with_answered_by(mut self, who: Option<crate::trigger_route::AnsweredBy>) -> Self {
        self.answered_by = who;
        self
    }

    /// Attach the cited knowledge sources.
    pub(super) fn with_citations(mut self, cited: Vec<duduclaw_knowledge::cite::CitationRef>) -> Self {
        self.citations = cited;
        self
    }

    pub(super) fn refused() -> Self {
        Self {
            text: CCR_DELIVERY_REFUSED_TEXT.to_string(),
            artifact: None,
            answered_by: None,
            citations: Vec::new(),
            guards: Vec::new(),
            refused: true,
            rollback: None,
            revoked: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    /// Revalidate every lease off the async reactor. A lost lease is latched
    /// and rolled back here, so a caller that only checks this before each
    /// outbound segment gets the undo for free.
    pub async fn still_valid(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.revoked.load(Ordering::SeqCst) {
            return false;
        }
        let live = if self
            .guards
            .iter()
            .all(duduclaw_llm::CcrDeliveryGuards::is_empty)
        {
            true
        } else {
            let guards = self.guards.clone();
            let check =
                move || guards.iter().all(duduclaw_llm::CcrDeliveryGuards::still_valid_blocking);
            match tokio::runtime::Handle::try_current() {
                // A panicked or cancelled revalidation fails closed.
                Ok(_) => tokio::task::spawn_blocking(check).await.unwrap_or(false),
                Err(_) => check(),
            }
        };
        if !live {
            self.revoke_turn().await;
        }
        live
    }

    /// Sync revalidation for the few callbacks that cannot be async (the
    /// `office_docs::DeliveryCheck` hook). It latches a lost lease but cannot
    /// run `revoke_message().await`; the rollback then happens at the next
    /// async recheck or when this value is dropped.
    pub fn still_valid_blocking(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.revoked.load(Ordering::SeqCst) {
            return false;
        }
        let live = self
            .guards
            .iter()
            .all(duduclaw_llm::CcrDeliveryGuards::still_valid_blocking);
        if !live {
            self.revoked.store(true, Ordering::SeqCst);
        }
        live
    }

    /// Mark an undelivered reply before dropping it; Drop schedules revocation
    /// in the cleanup collector even when the turn future is cancelled.
    pub(crate) fn refuse_delivery(&self) {
        self.revoked.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Stage a hidden durable row before live fan-out. Some early reply paths
    /// (and a failed pipeline append) have no assistant row. Record those here;
    /// publication waits for the final authority fence.
    pub(crate) async fn persist_webchat(
        &self, sessions: &Arc<SessionManager>, sid: &str, text: &str,
    ) -> Result<(), ()> {
        if !self.still_valid().await { return Err(()); }
        let Some(rollback) = &self.rollback else {
            self.refuse_delivery();
            return Err(());
        };
        let existing = rollback.delivery.assistant_message.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        match existing {
            Some((stored_sid, _)) if stored_sid == sid => {}
            Some(_) => { self.refuse_delivery(); return Err(()); }
            None => {
                // RM-173: bound to the row the turn started in, like the
                // pipeline's own append — a `/new` during the turn drops
                // this write instead of landing it in the fresh conversation.
                let generation = rollback.delivery.turn_generation.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner).clone();
                let id = sessions
                    .append_delivery_message_in(sid, "assistant", text, 0, None, true, generation.as_deref())
                    .await
                    .map_err(|error| {
                        warn!(session_id = sid, %error, "WebChat durable reply commit failed");
                        self.refuse_delivery();
                    })?;
                *rollback.delivery.assistant_message.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((sid.into(), id));
            }
        }
        if !self.still_valid().await { return Err(()); }
        Ok(())
    }

    /// Complete durable delivery after live fan-out and every authority fence.
    /// A failed scrub can never reveal the staged row. Successful publication
    /// settles the turn here so later Drop cannot reinterpret its verdict.
    pub(crate) async fn publish_webchat(
        &self, sessions: &SessionManager, sid: &str, users: &duduclaw_auth::UserDb,
        generation: u64, policy: &crate::webchat::access::TargetPolicy, cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), ()> {
        let Some(rollback) = &self.rollback else { self.refuse_delivery(); return Err(()); };
        let row = rollback.delivery.assistant_message.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let Some((stored_sid, id)) = row else { self.refuse_delivery(); return Err(()); };
        if stored_sid != sid { self.refuse_delivery(); return Err(()); }
        if !self.still_valid().await { return Err(()); }
        let published = sessions.publish_webchat_message(sid, id, users, generation, policy,
            || !cancel.is_cancelled() && !self.revoked.load(std::sync::atomic::Ordering::SeqCst)).await;
        match published {
            Ok(true) => {
                rollback.delivery.published.store(true, std::sync::atomic::Ordering::SeqCst);
                rollback.delivery.publish(true);
                Ok(())
            }
            result => {
                if let Err(error) = result { warn!(session_id = sid, %error, "WebChat reply publication failed"); }
                self.refuse_delivery();
                Err(())
            }
        }
    }

    pub(super) async fn revoke_turn(&self) {
        use std::sync::atomic::Ordering;
        if self.revoked.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(rollback) = self.rollback.as_ref() {
            rollback
                .delivery
                .settle(&rollback.session_mgr, false)
                .await;
        }
    }

    /// Legacy callers have no place to retain a lease, so they receive a
    /// conservative refusal whenever this answer used protected source data.
    pub(super) fn into_legacy_parts(mut self) -> (String, Option<serde_json::Value>) {
        use std::sync::atomic::Ordering;
        if self.refused {
            self.revoked.store(true, Ordering::SeqCst);
            (CCR_DELIVERY_REFUSED_TEXT.to_string(), None)
        } else if self.guards.iter().any(|guard| !guard.is_empty()) {
            // Nothing source-derived reaches the caller, so the turn was not
            // delivered: latch it so `Drop` rolls the session row back too.
            self.revoked.store(true, Ordering::SeqCst);
            (CCR_LEASE_UNSUPPORTED_TEXT.to_string(), None)
        } else {
            (std::mem::take(&mut self.text), self.artifact.take())
        }
    }
}

impl Drop for GuardedReply {
    /// Last act of a guarded turn: publish the delivery verdict. Only a reply
    /// that actually carries a lease holds a rollback handle, so this is a
    /// no-op for every other reply.
    fn drop(&mut self) {
        let Some(rollback) = self.rollback.take() else {
            return;
        };
        let delivered = !std::thread::panicking()
            && !self.revoked.load(std::sync::atomic::Ordering::SeqCst);
        rollback.settle_on_drop(delivered);
    }
}

/// `true` when a guarded reply's source lease is gone. An unguarded caller
/// (`None`) is never "lost". Shared by every adapter so the rollback wiring
/// lives in one place instead of eleven.
pub async fn guard_lost(guarded: Option<&GuardedReply>) -> bool {
    match guarded {
        Some(reply) => !reply.still_valid().await,
        None => false,
    }
}

