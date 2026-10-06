//! Everything an ingress run sends back to LINE: the final reply (or the
//! late-reply Push), progress notices, and 📎DELIVER document notices.
//!
//! Every send revalidates first: the dispatch lease is still ours, the stop
//! switch is still on, and the route/authority snapshot still matches; a
//! snapshot that cannot be read is retried briefly and then refused as
//! `revalidation_unavailable`, never reported as a change. The final
//! answer's Push goes only to the event's own conversation (group, room or
//! 1:1 user), carries an `X-Line-Retry-Key` derived from the run, and its
//! result is written to the attempt receipt. Progress notices are secondary
//! and, as before, go to the sender's 1:1 chat (also from a group); their
//! results are tallied apart and never change the event's status. They are
//! pushed regardless of `line_late_reply`.

use super::super::*;
use super::revision::{RevisionError, line_revision};
use super::{Binding, INGRESS_BINDING, PROGRESS, RUN, record_reply_failure};
use crate::channel_ingress::REPLY_TOKEN_SECONDS;
use crate::channel_ingress::config::LateReply;

/// How the final answer of this run can go out right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryRoute {
    Reply,
    Push,
    Expired,
}

/// Per-run delivery state (task-local [`RUN`]).
#[derive(Debug, Clone)]
pub(crate) struct RunReceipt {
    /// End of the reply token's window (receipt + 60 s; never renewed).
    pub token_deadline: i64,
    pub late: LateReply,
    /// A `retry` / `rerun` run: with Push allowed it never reuses the old token.
    pub rerun: bool,
    pub outcome: Option<&'static str>,
    pub delivered_via: Vec<&'static str>,
    pub provider_receipt: Option<String>,
    pub pushes: u32,
}

impl RunReceipt {
    pub(crate) fn new(token_deadline: i64, late: LateReply, rerun: bool) -> Self {
        Self {
            token_deadline,
            late,
            rerun,
            outcome: None,
            delivered_via: Vec::new(),
            provider_receipt: None,
            pushes: 0,
        }
    }

    pub(crate) fn route(&self, now: i64) -> DeliveryRoute {
        match self.late {
            LateReply::Push if self.rerun || now > self.token_deadline => DeliveryRoute::Push,
            _ if now <= self.token_deadline => DeliveryRoute::Reply,
            _ => DeliveryRoute::Expired,
        }
    }
}

/// Reasons meaning "the provider may have it" (never retried automatically).
pub(crate) fn is_uncertain(reason: &str) -> bool {
    matches!(
        reason,
        "reply_delivery_uncertain" | "push_delivery_uncertain" | "dispatch_lease_lost"
    )
}

/// The event status a finished run ends in.
pub(crate) fn final_status(outcome: Option<&str>) -> &'static str {
    match outcome {
        None => "completed",
        Some(r) if is_uncertain(r) => "uncertain",
        Some(_) => "undelivered",
    }
}

/// The reply token's deadline for an event (review N2). LINE: a reply
/// token must be used within one minute of receiving the webhook and is
/// valid once. The deadline counts from the earlier of our receipt and the
/// event's own `timestamp`. A redelivered webhook (`deliveryContext.
/// isRedelivery`) is treated as already past it: its token may have been
/// used by the first delivery, so it is never tried and the answer follows
/// `line_late_reply` instead.
pub(crate) fn reply_token_deadline(received_at: i64, event: &LineEvent) -> i64 {
    if event
        .delivery_context
        .as_ref()
        .is_some_and(|c| c.is_redelivery)
    {
        return i64::MIN;
    }
    let occurred = event
        .timestamp
        .filter(|ms| *ms > 0)
        .map(|ms| ms / 1000)
        .unwrap_or(received_at);
    received_at
        .min(occurred)
        .saturating_add(REPLY_TOKEN_SECONDS)
}

/// Whether the event came from a redelivered webhook.
pub(crate) fn is_redelivery(event: &LineEvent) -> bool {
    event
        .delivery_context
        .as_ref()
        .is_some_and(|c| c.is_redelivery)
}

/// The answer to "may this run still send?" (review N3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Validity {
    Valid,
    /// Read and different (or the lease / stop switch says no).
    Changed,
    /// Could not be read within the retry budget.
    Unavailable,
}

impl Validity {
    /// The receipt reason for a refused final send.
    pub(crate) fn refusal(self) -> &'static str {
        match self {
            Self::Unavailable => "revalidation_unavailable",
            _ => "authorization_changed_before_delivery",
        }
    }
}

/// Waits between re-reads of an unreadable snapshot (about 7 s in all).
#[cfg(not(test))]
const REVALIDATION_BACKOFF_MS: [u64; 4] = [300, 1000, 2000, 4000];
#[cfg(test)]
const REVALIDATION_BACKOFF_MS: [u64; 4] = [5, 5, 5, 5];

/// Is the binding still allowed to send? A transient read failure is
/// retried a few times with growing waits; still unreadable is
/// [`Validity::Unavailable`], never reported as a change (fail closed
/// either way: nothing is sent).
pub(crate) async fn binding_validity(binding: &Binding) -> Validity {
    let Ok(event) = serde_json::from_value::<LineEvent>(binding.payload["event"].clone()) else {
        return Validity::Changed;
    };
    let state = &binding.state;
    let Some(store) = state.ingress.as_ref() else {
        return Validity::Changed;
    };
    if !durable_line_enabled(&state.home_dir).await {
        return Validity::Changed;
    }
    let mut waits = REVALIDATION_BACKOFF_MS.iter();
    loop {
        let lease = store
            .lease_active(&binding.row, chrono::Utc::now().timestamp())
            .await;
        let read = match lease {
            Ok(false) => return Validity::Changed,
            Ok(true) => line_revision(state, &event, None).await,
            Err(_) => Err(RevisionError::Unavailable("lease_unreadable")),
        };
        match read {
            Ok(rev) if rev.route == binding.revision && rev.authority == binding.authorization => {
                return Validity::Valid;
            }
            Ok(_) | Err(RevisionError::Changed(_)) => return Validity::Changed,
            Err(RevisionError::Unavailable(_)) => match waits.next() {
                Some(ms) => tokio::time::sleep(std::time::Duration::from_millis(*ms)).await,
                None => return Validity::Unavailable,
            },
        }
    }
}

/// A stable UUID for one push of one run, so a transport retry of the same
/// request is deduplicated by LINE (`X-Line-Retry-Key`).
fn retry_key(run_id: &str, n: u32) -> String {
    let d = crate::channel_ingress::digest(&[run_id, "line-push", &n.to_string()]);
    let mut bytes = [0u8; 16];
    for (b, v) in bytes.iter_mut().zip(hex::decode(d).unwrap_or_default()) {
        *b = v;
    }
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

fn request_id(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get("x-line-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| duduclaw_core::truncate_chars(v, 64).to_string())
}

fn note_delivered(via: &'static str, receipt: Option<String>) {
    let _ = RUN.try_with(|r| {
        let mut r = r.borrow_mut();
        r.delivered_via.push(via);
        if receipt.is_some() {
            r.provider_receipt = receipt;
        }
    });
}

/// Send the final answer of an ingress run. Called by `send_reply_rich`.
pub(crate) async fn deliver(
    http: &reqwest::Client,
    token: &str,
    reply_token: &str,
    messages: Vec<serde_json::Value>,
) -> bool {
    let binding = INGRESS_BINDING.try_with(Clone::clone).ok();
    if let Some(binding) = &binding {
        let validity = binding_validity(binding).await;
        if validity != Validity::Valid {
            record_reply_failure(validity.refusal());
            return false;
        }
    }
    let now = chrono::Utc::now().timestamp();
    let route = RUN
        .try_with(|r| r.borrow().route(now))
        .unwrap_or(DeliveryRoute::Reply);
    match route {
        DeliveryRoute::Expired => {
            record_reply_failure("reply_token_expired");
            false
        }
        DeliveryRoute::Reply => {
            match send_reply(http, token, reply_token, messages.clone()).await {
                ReplyResult::Delivered => true,
                ReplyResult::Failed => false,
                ReplyResult::InvalidToken => {
                    reply_token_refused(http, token, binding.as_ref(), messages).await
                }
            }
        }
        DeliveryRoute::Push => match binding {
            Some(binding) => push_to_conversation(http, token, &binding, messages).await,
            // No binding means no verified conversation to push to.
            None => {
                record_reply_failure("push_target_unavailable");
                false
            }
        },
    }
}

/// What the Reply API said.
enum ReplyResult {
    Delivered,
    /// HTTP 400 with LINE's `"Invalid reply token"` message: the token was
    /// used or expired, so nothing was delivered with it.
    InvalidToken,
    Failed,
}

/// LINE's error message for a used or expired reply token.
const INVALID_REPLY_TOKEN: &str = "Invalid reply token";

/// The Reply API refused the token (review N2). With `line_late_reply =
/// "push"` the answer goes out by Push after a fresh revalidation, with its
/// own receipt; with `"fail"` it stays undelivered.
async fn reply_token_refused(
    http: &reqwest::Client,
    token: &str,
    binding: Option<&Binding>,
    messages: Vec<serde_json::Value>,
) -> bool {
    let push = RUN
        .try_with(|r| r.borrow().late == LateReply::Push)
        .unwrap_or(false);
    let Some(binding) = binding.filter(|_| push) else {
        record_reply_failure("reply_token_invalid");
        return false;
    };
    let validity = binding_validity(binding).await;
    if validity != Validity::Valid {
        record_reply_failure(validity.refusal());
        return false;
    }
    push_to_conversation(http, token, binding, messages).await
}

async fn send_reply(
    http: &reqwest::Client,
    token: &str,
    reply_token: &str,
    messages: Vec<serde_json::Value>,
) -> ReplyResult {
    let body = serde_json::json!({"replyToken": reply_token, "messages": messages});
    match http
        .post(line_provider_url(token, "/message/reply"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            note_delivered("reply", request_id(&resp));
            ReplyResult::Delivered
        }
        Ok(resp) if resp.status() == reqwest::StatusCode::BAD_REQUEST => {
            let message = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_string));
            if message.as_deref() == Some(INVALID_REPLY_TOKEN) {
                return ReplyResult::InvalidToken;
            }
            record_reply_failure("reply_rejected");
            error!("LINE reply rejected (400); not resent automatically");
            ReplyResult::Failed
        }
        Ok(resp) => {
            let status = resp.status();
            record_reply_failure(if status.is_client_error() {
                "reply_rejected"
            } else {
                "reply_delivery_uncertain"
            });
            error!(%status, "LINE reply rejected; not resent automatically");
            ReplyResult::Failed
        }
        Err(_) => {
            record_reply_failure("reply_delivery_uncertain");
            error!("LINE reply transport failed; receipt uncertain");
            ReplyResult::Failed
        }
    }
}

/// The Push target of an event: its own group, room or 1:1 user.
pub(crate) fn push_target(binding: &Binding) -> Option<String> {
    let event = serde_json::from_value::<LineEvent>(binding.payload["event"].clone()).ok()?;
    let target = line_conversation(&event);
    (target != "unknown" && !target.is_empty()).then_some(target)
}

async fn push_to_conversation(
    http: &reqwest::Client,
    token: &str,
    binding: &Binding,
    messages: Vec<serde_json::Value>,
) -> bool {
    let Some(to) = push_target(binding) else {
        record_reply_failure("push_target_unavailable");
        return false;
    };
    let n = RUN
        .try_with(|r| {
            let mut r = r.borrow_mut();
            r.pushes += 1;
            r.pushes
        })
        .unwrap_or(1);
    let key = retry_key(&binding.row.run_id, n);
    match http
        .post(line_provider_url(token, "/message/push"))
        .bearer_auth(token)
        .header("X-Line-Retry-Key", key)
        .json(&serde_json::json!({"to": to, "messages": messages}))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            note_delivered("push", request_id(&resp));
            true
        }
        // 409: this retry key was already accepted (an earlier send of the
        // same request reached LINE).
        Ok(resp) if resp.status() == reqwest::StatusCode::CONFLICT => {
            note_delivered("push", request_id(&resp));
            true
        }
        Ok(resp) => {
            let status = resp.status();
            record_reply_failure(if status.is_client_error() {
                "push_rejected"
            } else {
                "push_delivery_uncertain"
            });
            error!(%status, "LINE late-reply push refused; not resent automatically");
            false
        }
        Err(_) => {
            record_reply_failure("push_delivery_uncertain");
            error!("LINE late-reply push transport failed; receipt uncertain");
            false
        }
    }
}

/// Progress pushes of one run: secondary, tallied apart (review I-HIGH-2).
#[derive(Default)]
pub(crate) struct ProgressLedger {
    pub handles: std::sync::Mutex<Vec<tokio::task::JoinHandle<ProgressResult>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressResult {
    Sent,
    Failed,
    Skipped,
}

impl ProgressLedger {
    /// Wait (bounded) for the spawned progress pushes and summarize them.
    pub(crate) async fn settle(&self, wait: std::time::Duration) -> Option<String> {
        let handles = self
            .handles
            .lock()
            .map(|mut h| std::mem::take(&mut *h))
            .unwrap_or_default();
        if handles.is_empty() {
            return None;
        }
        let deadline = tokio::time::Instant::now() + wait;
        let (mut sent, mut failed, mut skipped, mut unknown) = (0, 0, 0, 0);
        for mut handle in handles {
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(Ok(ProgressResult::Sent)) => sent += 1,
                Ok(Ok(ProgressResult::Failed)) => failed += 1,
                Ok(Ok(ProgressResult::Skipped)) => skipped += 1,
                _ => {
                    handle.abort();
                    unknown += 1;
                }
            }
        }
        Some(format!(
            "sent={sent} failed={failed} skipped={skipped} unknown={unknown}"
        ))
    }
}

/// Progress notices for an ingress run (existing behaviour: a push to the
/// sender at most once a minute). Each send revalidates; its result goes to
/// the run's [`ProgressLedger`] only.
pub(crate) fn line_progress_callback(
    state: &LineState,
    event: &LineEvent,
    token: &str,
) -> Option<crate::channel_reply::ProgressCallback> {
    let uid = event.source.as_ref()?.user_id.clone()?;
    let binding = INGRESS_BINDING.try_with(Clone::clone).ok()?;
    let ledger = PROGRESS.try_with(Clone::clone).ok()?;
    let http = state.http.clone();
    let token = token.to_string();
    let counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let last = Arc::new(std::sync::Mutex::new(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .unwrap_or_else(std::time::Instant::now),
    ));
    Some(Box::new(move |event| {
        if !should_forward_line_progress_event(&event) {
            return;
        }
        let Ok(mut last) = last.lock() else { return };
        if last.elapsed().as_secs() < 60 {
            return;
        }
        *last = std::time::Instant::now();
        drop(last);
        let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (http, token, uid, binding, text) = (
            http.clone(),
            token.clone(),
            uid.clone(),
            binding.clone(),
            event.to_display(),
        );
        let handle = tokio::spawn(async move {
            if binding_validity(&binding).await != Validity::Valid {
                return ProgressResult::Skipped;
            }
            let key = retry_key(&format!("{}/progress", binding.row.run_id), n);
            match http
                .post(line_provider_url(&token, "/message/push"))
                .bearer_auth(&token)
                .header("X-Line-Retry-Key", key)
                .json(&serde_json::json!({"to":uid,"messages":[{"type":"text","text":text}]}))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => ProgressResult::Sent,
                _ => {
                    warn!("LINE progress delivery failed (secondary; event status unaffected)");
                    ProgressResult::Failed
                }
            }
        });
        if let Ok(mut handles) = ledger.handles.lock() {
            handles.push(handle);
        } else {
            handle.abort();
        }
    }))
}

/// 📎DELIVER on LINE: the platform has no bot file API, so the "file is
/// ready, see the dashboard" notice is collected here and appended to the
/// run's answer, which then goes out through [`deliver`] with the same
/// revalidation and receipt as every reply (review I-MEDIUM-3). Nothing is
/// pushed from here.
#[derive(Default)]
pub(crate) struct LineNoticeCollector {
    notices: std::sync::Mutex<Vec<String>>,
}

impl LineNoticeCollector {
    pub(crate) fn take(&self) -> Vec<String> {
        self.notices
            .lock()
            .map(|mut n| std::mem::take(&mut *n))
            .unwrap_or_default()
    }

    /// The answer with any collected notices appended.
    pub(crate) fn append_to(&self, reply: String) -> String {
        let notices = self.take();
        if notices.is_empty() {
            return reply;
        }
        let joined = notices.join("\n");
        if reply.trim().is_empty() {
            joined
        } else {
            format!("{reply}\n\n{joined}")
        }
    }
}

#[async_trait::async_trait]
impl crate::channel_sender::ChannelSender for LineNoticeCollector {
    async fn send_text(&self, text: &str) -> Result<(), crate::channel_sender::ChannelSendError> {
        self.notices
            .lock()
            .map_err(|_| crate::channel_sender::ChannelSendError("notice buffer".into()))?
            .push(text.to_string());
        Ok(())
    }

    async fn send_photo(
        &self,
        _png_data: &[u8],
        _caption: &str,
    ) -> Result<(), crate::channel_sender::ChannelSendError> {
        Err(crate::channel_sender::ChannelSendError(
            "LINE ingress runs do not push photos".into(),
        ))
    }

    async fn request_confirmation(
        &self,
        _prompt: &str,
        _screenshot: Option<&[u8]>,
        _timeout_secs: u64,
    ) -> Result<bool, crate::channel_sender::ChannelSendError> {
        Ok(false)
    }

    fn channel_type(&self) -> &'static str {
        "line"
    }
}
