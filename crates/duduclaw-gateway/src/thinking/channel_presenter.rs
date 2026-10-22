//! Thinking display in messaging apps (one presenter per reply turn).
//!
//! A channel handler that supports it creates a [`ChannelThinking`], wraps
//! its progress callback with [`ChannelThinking::wrap`] and runs the reply
//! inside [`ChannelThinking::scope`]. The reply pipeline
//! (`channel_reply/guarded.rs`) then resolves what this sender may see
//! ([`super::channel_policy`]) and tells the presenter
//! ([`ChannelThinking::set_show`]); a channel that installs no presenter
//! keeps today's behaviour (text-less signal, interim line only).
//!
//! - Editable platforms (a [`StatusSurface`]): one status message
//!   "💭 思考中… N 秒" (+ the latest masked line for the summary audience),
//!   edited at most every [`EDIT_INTERVAL`], then edited into
//!   "💭 思考了 N 秒" (+ the summary) when the answer is ready
//!   ([`ChannelThinking::finish`]). While it shows, the interim status line
//!   is not posted (it would say the same thing twice).
//! - Other platforms: nothing while thinking (the interim line stays);
//!   [`ChannelThinking::finish`] returns a [`Footer`] the channel adds to
//!   its reply (LINE as one extra message in the same reply call).
//!
//! "N 秒" is wall-clock time from the start of the turn, the same number
//! the WebChat timer shows. Nothing is shown when the model reported no
//! thinking at all. Summary text is the gate's masked output only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use duduclaw_core::show_thinking::ShowThinking;

use super::channel_format::StatusView;
pub use super::channel_status::{PostOutcome, StatusSurface};
use super::channel_status::StatusSlot;
use super::event::{ThinkingEvent, ThinkingKind};
use crate::channel_reply::{ProgressCallback, ProgressEvent};
use crate::trigger_route::texts::Lang;

/// Minimum gap between two edits of the status message.
pub const EDIT_INTERVAL: Duration = Duration::from_secs(3);
/// The live status stops updating after this long (a turn's hard limit is
/// far shorter; this only bounds a status task that was never finished).
const MAX_LIVE_SECS: u64 = 2 * 60 * 60;
/// Longest wait for the status task when the answer is ready.
const FINISH_WAIT: Duration = Duration::from_secs(5);

tokio::task_local! {
    /// The current messaging-app turn's presenter (read by the reply
    /// pipeline to set what this sender may see).
    pub static CHANNEL_THINKING: Arc<ChannelThinking>;
}

/// The presenter of the current turn, if the channel installed one.
pub fn current() -> Option<Arc<ChannelThinking>> {
    CHANNEL_THINKING.try_with(Arc::clone).ok()
}

/// What a non-editable platform adds to its reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footer {
    pub view: StatusView,
}

impl Footer {
    /// Plain text for platforms that cannot fold the summary: capped, with
    /// mention / markup syntax neutralised so a summary can never ping or
    /// format anything.
    pub fn text(&self) -> String {
        super::channel_format::neutralize_markup(&super::channel_format::plain(
            &self.view,
            super::channel_format::PLAIN_SUMMARY_CHARS,
        ))
    }
}

/// Decides what the sender may see for a given answering employee.
pub type ShowResolver = Arc<dyn Fn(&str) -> ShowThinking + Send + Sync>;

#[derive(Default)]
struct Audience {
    /// The employee the current `show` was resolved for.
    agent: Option<String>,
    resolver: Option<ShowResolver>,
    cache: HashMap<String, ShowThinking>,
}

#[derive(Debug, Default)]
struct State {
    seen: bool,
    line: Option<String>,
    /// Latest masked snapshot per block, in first-seen order.
    blocks: Vec<(String, String)>,
    status_started: bool,
}

/// See the module docs.
pub struct ChannelThinking {
    channel: &'static str,
    is_group: bool,
    lang: Lang,
    started: Instant,
    show: Mutex<ShowThinking>,
    audience: Mutex<Audience>,
    state: Mutex<State>,
    surface: Option<Arc<dyn StatusSurface>>,
    status: Arc<tokio::sync::Mutex<StatusSlot>>,
    stop: tokio::sync::watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// `finish` or `abandon` ran (otherwise `Drop` removes the status).
    settled: AtomicBool,
}

impl ChannelThinking {
    /// `channel` is the identity-store channel name (`telegram`, `line`, …).
    /// Shows nothing until the reply pipeline calls [`Self::set_show`].
    pub fn new(
        channel: &'static str,
        is_group: bool,
        lang: Lang,
        surface: Option<Arc<dyn StatusSurface>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            channel,
            is_group,
            lang,
            started: Instant::now(),
            show: Mutex::new(ShowThinking::Off),
            audience: Mutex::new(Audience::default()),
            state: Mutex::new(State::default()),
            surface,
            status: Arc::new(tokio::sync::Mutex::new(StatusSlot::default())),
            stop: tokio::sync::watch::channel(false).0,
            task: Mutex::new(None),
            settled: AtomicBool::new(false),
        })
    }

    pub fn channel(&self) -> &'static str {
        self.channel
    }

    pub fn is_group(&self) -> bool {
        self.is_group
    }

    /// What this turn's sender may see (set once per turn by the pipeline).
    pub fn set_show(&self, show: ShowThinking) {
        *self.show.lock().unwrap_or_else(|p| p.into_inner()) = show;
    }

    /// [`Self::set_show`] for `agent`, plus how to decide again when another
    /// employee ends up answering (an `@` call routed inside the pipeline).
    pub fn set_audience(&self, agent: &str, show: ShowThinking, resolver: ShowResolver) {
        self.set_show(show);
        let mut a = self.audience.lock().unwrap_or_else(|p| p.into_inner());
        a.cache.insert(agent.to_string(), show);
        a.agent = Some(agent.to_string());
        a.resolver = Some(resolver);
    }

    pub fn show(&self) -> ShowThinking {
        *self.show.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run the reply with this presenter in scope.
    pub async fn scope<T>(self: &Arc<Self>, turn: impl std::future::Future<Output = T>) -> T {
        CHANNEL_THINKING.scope(Arc::clone(self), turn).await
    }

    /// Wrap the channel's progress callback: thinking events feed the
    /// presenter (and still reach the callback, which drops them), and the
    /// interim line is held back while the status message shows.
    pub fn wrap(self: &Arc<Self>, cb: ProgressCallback) -> ProgressCallback {
        let me = Arc::clone(self);
        Box::new(move |ev| {
            match &ev {
                ProgressEvent::Thinking(t) => me.observe(t),
                ProgressEvent::Interim { .. } if me.owns_status() => return,
                _ => {}
            }
            cb(ev)
        })
    }

    /// Is the editable status message showing (so the interim line would
    /// repeat it)?
    fn owns_status(&self) -> bool {
        self.surface.is_some() && self.state().status_started
    }

    fn seconds(&self) -> u64 {
        self.started.elapsed().as_secs().max(1)
    }

    /// The employee answering now differs from the one `show` was resolved
    /// for: decide again for it (cached per employee). Text already kept is
    /// dropped when the new employee allows less than a summary.
    fn retarget(&self, agent: &str) {
        let show = {
            let mut a = self.audience.lock().unwrap_or_else(|p| p.into_inner());
            if a.agent.as_deref() == Some(agent) {
                return;
            }
            let Some(resolver) = a.resolver.clone() else {
                return;
            };
            let show = match a.cache.get(agent) {
                Some(s) => *s,
                None => {
                    let s = resolver(agent);
                    a.cache.insert(agent.to_string(), s);
                    s
                }
            };
            a.agent = Some(agent.to_string());
            show
        };
        self.set_show(show);
        if show != ShowThinking::Summary {
            let mut st = self.state();
            st.blocks.clear();
            st.line = None;
        }
    }

    /// Feed one gated thinking event.
    pub fn observe(self: &Arc<Self>, ev: &ThinkingEvent) {
        if let Some(agent) = super::answering_agent() {
            self.retarget(&agent);
        }
        let show = self.show();
        if show == ShowThinking::Off {
            return;
        }
        let start_status = {
            let mut st = self.state();
            st.seen = true;
            if show == ShowThinking::Summary && !ev.is_turn_usage() {
                if let Some(line) = ev.line.as_deref().filter(|l| !l.trim().is_empty()) {
                    st.line = Some(line.to_string());
                }
                if ev.kind == ThinkingKind::Redacted {
                    // Withheld (by the vendor or our outbound checks): drop
                    // whatever this block showed before, and its line.
                    st.blocks.retain(|(id, _)| *id != ev.block_id);
                    st.line = None;
                }
                let text = match ev.kind {
                    ThinkingKind::Redacted => None,
                    _ => ev.text.clone(),
                };
                if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
                    match st.blocks.iter_mut().find(|(id, _)| *id == ev.block_id) {
                        Some(slot) => slot.1 = text,
                        None => st.blocks.push((ev.block_id.clone(), text)),
                    }
                }
            }
            let start = self.surface.is_some() && !st.status_started;
            if start {
                st.status_started = true;
            }
            start
        };
        if start_status {
            self.start_status_task();
        }
    }

    fn view(&self, done: bool) -> StatusView {
        let show = self.show();
        let st = self.state();
        let summary = (done && show == ShowThinking::Summary && !st.blocks.is_empty()).then(|| {
            st.blocks
                .iter()
                .map(|(_, t)| t.trim())
                .collect::<Vec<_>>()
                .join("\n\n")
        });
        StatusView {
            done,
            seconds: self.seconds(),
            line: (show == ShowThinking::Summary)
                .then(|| st.line.clone())
                .flatten(),
            summary,
            lang: self.lang,
        }
    }

    fn start_status_task(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let Some(surface) = self.surface.clone() else {
            return;
        };
        // Weak: a turn that ends without `finish` / `abandon` drops the
        // presenter, and the task stops with it.
        let me = Arc::downgrade(self);
        let status = Arc::clone(&self.status);
        let mut stop = self.stop.subscribe();
        let task = handle.spawn(async move {
            let mut last: Option<StatusView> = None;
            loop {
                if *stop.borrow() {
                    break;
                }
                let Some(view) = me.upgrade().map(|p| p.view(false)) else {
                    break;
                };
                if view.seconds > MAX_LIVE_SECS {
                    break;
                }
                {
                    let mut slot = status.lock().await;
                    if slot.is_dead() {
                        break;
                    }
                    let changed = last.as_ref() != Some(&view);
                    slot.show(surface.as_ref(), &view, changed).await;
                }
                last = Some(view);
                tokio::select! {
                    _ = tokio::time::sleep(EDIT_INTERVAL) => {}
                    _ = stop.changed() => break,
                }
            }
        });
        *self.task.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
    }

    async fn stop_status_task(&self) {
        let _ = self.stop.send(true);
        let task = self.task.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(mut task) = task {
            if tokio::time::timeout(FINISH_WAIT, &mut task).await.is_err() {
                // Never let a stuck post race the final edit.
                task.abort();
                tracing::debug!(channel = self.channel, "thinking status task aborted");
            }
        }
    }

    /// The answer is ready. Editable platforms: the status message becomes
    /// "思考了 N 秒" (+ summary) and `None` is returned (also when the
    /// platform refused the status this turn). Others: the footer to add to
    /// the reply. `None` too when nothing is to be shown.
    pub async fn finish(&self) -> Option<Footer> {
        self.settled.store(true, Ordering::SeqCst);
        self.stop_status_task().await;
        if self.show() == ShowThinking::Off || !self.state().seen {
            return None;
        }
        let view = self.view(true);
        let Some(surface) = self.surface.clone() else {
            return Some(Footer { view });
        };
        self.status.lock().await.show(surface.as_ref(), &view, true).await;
        None
    }

    /// Has the status message been posted?
    pub async fn is_status_posted(&self) -> bool {
        self.status.lock().await.id.is_some()
    }

    /// The turn ended without an answer: remove the status message.
    pub async fn abandon(&self) {
        self.settled.store(true, Ordering::SeqCst);
        self.stop_status_task().await;
        let Some(surface) = self.surface.clone() else {
            return;
        };
        if let Some(id) = self.status.lock().await.id.take() {
            surface.delete(&id).await;
        }
    }
}

impl Drop for ChannelThinking {
    /// A handler that was cancelled or panicked between `scope` and
    /// `finish` / `abandon`: best-effort removal of a status message that
    /// would otherwise stay frozen at "思考中… N 秒".
    fn drop(&mut self) {
        if self.settled.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.stop.send(true);
        if let Some(task) = self.task.lock().unwrap_or_else(|p| p.into_inner()).take() {
            task.abort();
        }
        let (Some(surface), Ok(handle)) =
            (self.surface.clone(), tokio::runtime::Handle::try_current())
        else {
            return;
        };
        let id = match self.status.try_lock() {
            Ok(mut slot) => slot.id.take(),
            Err(_) => None,
        };
        if let Some(id) = id {
            handle.spawn(async move { surface.delete(&id).await });
        }
    }
}

/// Add the thinking footer to a non-editable platform's reply, above the
/// product footer when there is one.
pub fn with_footer(reply: String, footer: Option<&Footer>) -> String {
    let Some(footer) = footer else {
        return reply;
    };
    if reply.trim().is_empty() {
        return reply;
    }
    let line = footer.text();
    let product = crate::channel_reply::product_footer_suffix();
    match reply.strip_suffix(product.as_str()) {
        Some(body) => format!("{}\n\n{line}{product}", body.trim_end()),
        None => format!("{reply}\n\n{line}"),
    }
}

/// LINE's per-request message limit (reply and push alike).
pub const LINE_MAX_MESSAGES: usize = 5;

/// LINE: the thinking footer as one small text message.
pub fn line_footer_message(footer: &Footer) -> serde_json::Value {
    serde_json::json!({"type": "text", "text": footer.text()})
}

/// LINE Reply call: `extra` (the thinking message) placed before the answer,
/// unless the answer already uses all [`LINE_MAX_MESSAGES`] slots or is
/// empty. Only the Reply path calls this; a pushed answer never carries it.
pub fn with_reply_extra(
    mut messages: Vec<serde_json::Value>,
    extra: Option<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let Some(extra) = extra else {
        return messages;
    };
    if messages.is_empty() || messages.len() >= LINE_MAX_MESSAGES {
        return messages;
    }
    messages.insert(0, extra);
    messages
}

#[cfg(test)]
#[path = "channel_presenter_tests.rs"]
mod tests;
