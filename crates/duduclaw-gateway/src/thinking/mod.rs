//! Thinking display: one normalised event for every runtime, one gate.
//!
//! Runtimes report thinking through [`emit`] (usually via a [`Recorder`]).
//! The channel reply pipeline installs a per-turn [`ThinkingSink`] in the
//! task-local [`THINKING_SINK`] around the whole turn
//! (`channel_reply/guarded.rs`); outside that scope (cron, dispatch, ACP
//! work outside a reply) [`emit`] is a no-op and [`policy`] reports
//! [`ThinkingPolicy::INACTIVE`], so producers leave their argv / request
//! byte-identical.
//!
//! The sink runs every event through the turn's [`gate::Gate`] (masking,
//! truncation, throttling, CONTRACT checks — see [`gate`] and [`sanitize`])
//! and forwards what survives as [`ProgressEvent::Thinking`] through the
//! turn's progress callback. Text channels drop it
//! ([`ProgressEvent::is_channel_text`]); the interim watcher only notes that
//! the employee is thinking; the WebChat socket turns it into a `thinking`
//! frame (and drops it for widget visitors).
//!
//! Nothing here stores thinking text. At the end of the turn the sink writes
//! at most one `run_steps` row with the seconds and token count only.

pub mod channel_format;
pub mod channel_policy;
pub mod channel_presenter;
pub mod channel_status;
pub mod channel_surfaces;
pub mod claude_stream;
pub mod done_meta;
pub mod event;
pub mod gate;
pub mod llm_tap;
pub mod policy;
pub mod producers;
pub mod recorder;
pub mod sanitize;
pub mod think_tags;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use done_meta::{DoneMeta, DoneMetaBuilder};
pub use event::{ThinkingEvent, ThinkingKind, ThinkingPhase};
pub use policy::{SinkLevel, ThinkingPolicy};
pub use recorder::Recorder;

use crate::channel_reply::{ProgressCallback, ProgressEvent};

tokio::task_local! {
    /// The current reply turn's thinking sink. Same idiom as
    /// `runtime::NATIVE_TOOL_COLLECTOR`: task-locals follow `.await`s within
    /// the turn's task (not `tokio::spawn`), so no function between the
    /// reply pipeline and a runtime needs to pass it along.
    pub static THINKING_SINK: Arc<ThinkingSink>;
}

/// Gateway clock, unix ms.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Report one thinking event to the current turn. No scope ⇒ no-op.
pub fn emit(ev: ThinkingEvent) {
    let _ = THINKING_SINK.try_with(|sink| sink.accept(ev));
}

/// The next block number of the current turn, or `None` outside one.
pub fn next_block_number() -> Option<usize> {
    THINKING_SINK
        .try_with(|sink| {
            sink.next_block
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        })
        .ok()
}

/// The current turn's policy, or [`ThinkingPolicy::INACTIVE`] outside one.
pub fn policy() -> ThinkingPolicy {
    THINKING_SINK
        .try_with(|sink| sink.policy())
        .unwrap_or(ThinkingPolicy::INACTIVE)
}

/// Seconds and tokens of one turn's thinking (what `run_steps` keeps).
#[derive(Debug, Default)]
struct TurnStats {
    first_ms: Option<u64>,
    last_ms: Option<u64>,
    block_tokens: HashMap<String, u64>,
    turn_tokens: Option<u64>,
}

impl TurnStats {
    fn observe(&mut self, ev: &ThinkingEvent, now: u64) {
        let start = ev.started_at.unwrap_or(now);
        self.first_ms = Some(self.first_ms.map_or(start, |f| f.min(start)));
        self.last_ms = Some(self.last_ms.map_or(now, |l| l.max(now)));
        match (ev.is_turn_usage(), ev.tokens) {
            (true, Some(t)) => self.turn_tokens = Some(t),
            (false, Some(t)) => {
                let slot = self.block_tokens.entry(ev.block_id.clone()).or_insert(0);
                *slot = (*slot).max(t);
            }
            _ => {}
        }
    }

    fn seen(&self) -> bool {
        self.first_ms.is_some()
    }

    fn duration_ms(&self) -> u64 {
        match (self.first_ms, self.last_ms) {
            (Some(f), Some(l)) => l.saturating_sub(f),
            _ => 0,
        }
    }

    fn tokens(&self) -> Option<u64> {
        self.turn_tokens
            .or_else(|| (!self.block_tokens.is_empty()).then(|| self.block_tokens.values().sum()))
    }
}

/// `1240` → `"1,240"`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The `run_steps` preview: `思考 8 秒 · 1,240 token`. (Singular on purpose:
/// the store's secret masker treats the text after a `token` key as a value,
/// so `tokens` would render as `token[REDACTED]`.)
pub fn run_step_preview(duration_ms: u64, tokens: Option<u64>) -> String {
    let secs = duration_ms.div_ceil(1000).max(1);
    match tokens {
        Some(t) => format!("思考 {secs} 秒 · {} token", thousands(t)),
        None => format!("思考 {secs} 秒"),
    }
}

/// Everything the sink needs to mask text for this turn.
pub struct SinkCtx {
    pub home: PathBuf,
    /// The employee resolved before the turn. The employee that actually
    /// runs (task-local, e.g. an `@`-called one) is preferred when known.
    pub agent_id: String,
    /// `Some` only when redaction is enabled for this gateway.
    pub redaction: Option<Arc<duduclaw_redaction::RedactionManager>>,
}

struct SinkState {
    gate: gate::Gate,
    stats: TurnStats,
}

/// One turn's thinking sink.
pub struct ThinkingSink {
    policy: ThinkingPolicy,
    forward: Option<Arc<ProgressCallback>>,
    ctx: SinkCtx,
    state: Mutex<SinkState>,
    /// Per-turn cache of outbound rules by employee id.
    rules: Mutex<HashMap<String, Arc<crate::channel_reply::OutboundRules>>>,
    /// Per-turn cache of display ceilings by employee id.
    ceilings: Mutex<HashMap<String, SinkLevel>>,
    /// Next block number of this turn: every recorder of the turn (each
    /// retry, failover attempt and tool-loop round) draws its block ids from
    /// here, so two producers never reuse an id.
    next_block: std::sync::atomic::AtomicUsize,
}

/// The employee that is actually answering right now: the reply pipeline's
/// task-local agent id, else the turn's recorded answerer.
pub(crate) fn answering_agent() -> Option<String> {
    crate::claude_runner::CHANNEL_REPLY_AGENT_ID
        .try_with(|a| a.clone())
        .ok()
        .filter(|a| !a.is_empty())
        .or_else(|| {
            crate::trigger_route::ANSWERED_BY
                .try_with(|slot| {
                    slot.lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|who| who.id.clone()))
                })
                .ok()
                .flatten()
        })
        .filter(|a| !a.is_empty())
}

impl ThinkingSink {
    pub fn new(
        policy: ThinkingPolicy,
        forward: Option<Arc<ProgressCallback>>,
        ctx: SinkCtx,
    ) -> Self {
        let redaction_on = ctx.redaction.is_some();
        Self {
            state: Mutex::new(SinkState {
                gate: gate::Gate::new(policy.level, policy.max_chars, redaction_on),
                stats: TurnStats::default(),
            }),
            policy,
            forward,
            ctx,
            rules: Mutex::new(HashMap::new()),
            next_block: std::sync::atomic::AtomicUsize::new(0),
            ceilings: Mutex::new(HashMap::new()),
        }
    }

    /// The turn's policy, narrowed to what the employee answering right now
    /// allows (an `@`-called employee with `show_thinking = "off"` shows
    /// nothing, whatever the conversation's default employee allows).
    pub fn policy(&self) -> ThinkingPolicy {
        let agent = answering_agent().unwrap_or_else(|| self.ctx.agent_id.clone());
        ThinkingPolicy {
            level: self.policy.level.min(self.cap_for(&agent)),
            ..self.policy
        }
    }

    /// The display ceiling of `agent_id`. Only WebChat turns (levels other
    /// than `Signal`) are narrowed; the employee the turn was resolved for
    /// was already applied at install. Read once per turn, failing closed.
    fn cap_for(&self, agent_id: &str) -> SinkLevel {
        if self.policy.level == SinkLevel::Signal || agent_id == self.ctx.agent_id {
            return SinkLevel::Summary;
        }
        let mut cache = self.ceilings.lock().unwrap_or_else(|p| p.into_inner());
        *cache.entry(agent_id.to_string()).or_insert_with(|| {
            policy::level_for(policy::agent_ceiling(
                &self.ctx.home.join("agents").join(agent_id),
            ))
        })
    }

    /// Gate one producer event and forward what survives.
    pub fn accept(&self, ev: ThinkingEvent) {
        let now = now_ms();
        let agent_id = answering_agent().unwrap_or_else(|| self.ctx.agent_id.clone());
        let cap = self.cap_for(&agent_id);
        let engine = self.ctx.redaction.as_ref().map(|m| m.engine().as_ref());
        let sanitize_ctx = sanitize::SanitizeCtx {
            rules: self.rules_for(&agent_id),
            engine,
        };
        let out = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.stats.observe(&ev, now);
            state.gate.process_capped(ev, now, &sanitize_ctx, cap)
        };
        if let Some(block) = out.newly_blocked {
            // Rule names only — never the thinking text. Unlike a blocked
            // reply this raises no security event: the reply itself goes on.
            let rules: Vec<String> = block
                .audit_rules()
                .into_iter()
                .map(|r| format!("thinking: {r}"))
                .collect();
            tracing::warn!(agent = %agent_id, ?rules, "thinking text withheld by outbound checks");
            duduclaw_security::audit::log_contract_violation(&self.ctx.home, &agent_id, &rules);
        }
        if let Some(cb) = &self.forward {
            for e in out.events {
                cb(ProgressEvent::Thinking(e));
            }
        }
    }

    /// The employee's outbound rules, read from disk once per turn and only
    /// when text can be shown (the other levels never run the checks).
    fn rules_for(&self, agent_id: &str) -> Arc<crate::channel_reply::OutboundRules> {
        if self.policy.level.min(self.cap_for(agent_id)) != SinkLevel::Summary {
            return Arc::default();
        }
        let mut cache = self.rules.lock().unwrap_or_else(|p| p.into_inner());
        Arc::clone(cache.entry(agent_id.to_string()).or_insert_with(|| {
            Arc::new(crate::channel_reply::OutboundRules::load(
                &self.ctx.home,
                agent_id,
            ))
        }))
    }

    /// Did this turn report any thinking?
    pub fn saw_any(&self) -> bool {
        self.state.lock().map(|s| s.stats.seen()).unwrap_or(false)
    }

    /// The `run_steps` preview for this turn, if it thought at all.
    pub fn preview(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        state
            .stats
            .seen()
            .then(|| run_step_preview(state.stats.duration_ms(), state.stats.tokens()))
    }

    /// End of turn: write at most one `run_steps` row (seconds and tokens
    /// only), stamped when the thinking began so it lands inside the run's
    /// window. Best effort, like every `run_steps` write.
    pub async fn finish(&self, agent_id: &str, session_key: &str) {
        let (Some(preview), Some(first_ms)) = (
            self.preview(),
            self.state.lock().ok().and_then(|s| s.stats.first_ms),
        ) else {
            return;
        };
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(first_ms as i64)
            .unwrap_or_else(chrono::Utc::now);
        let home = self.ctx.home.clone();
        let agent_id = agent_id.to_string();
        let session_key = session_key.to_string();
        // SQLite I/O off the async worker.
        let write = tokio::task::spawn_blocking(move || {
            let Some(store) = crate::run_steps::shared_store(&home) else {
                return;
            };
            store.append_at_best_effort(
                &agent_id,
                &session_key,
                crate::run_steps::KIND_THINKING,
                "thinking",
                &preview,
                0,
                ts,
            );
        });
        if let Err(e) = write.await {
            tracing::debug!(error = %e, "thinking run_steps write task failed (dropped)");
        }
    }
}

/// The redaction manager to mask thinking text with, or `None` when
/// redaction is not in effect (no manager, globally disabled, the emergency
/// force-disable flag, or `DUDUCLAW_REDACTION=off`). Raw open-model reasoning
/// is only shown when this is `Some`.
pub fn redaction_for_display(
    manager: Option<&Arc<duduclaw_redaction::RedactionManager>>,
    home: &std::path::Path,
) -> Option<Arc<duduclaw_redaction::RedactionManager>> {
    let manager = manager?;
    if !manager.config_enabled()
        || crate::redaction_integration::force_disable_active(home)
        || duduclaw_redaction::EnvSetting::from_env() == duduclaw_redaction::EnvSetting::Off
    {
        return None;
    }
    Some(Arc::clone(manager))
}

/// Wrap a turn's progress callback with a thinking sink, or `None` when no
/// sink may be installed (public visitors). Returns the callback to hand to
/// the reply pipeline (same events, unchanged) and the sink.
pub fn install(
    policy: Option<ThinkingPolicy>,
    on_progress: Option<ProgressCallback>,
    ctx: SinkCtx,
) -> (Option<ProgressCallback>, Option<Arc<ThinkingSink>>) {
    let Some(policy) = policy else {
        return (on_progress, None);
    };
    let (inner, forward) = match on_progress {
        Some(cb) => {
            let shared: Arc<ProgressCallback> = Arc::new(cb);
            let for_inner = Arc::clone(&shared);
            let inner: ProgressCallback = Box::new(move |ev| for_inner(ev));
            (Some(inner), Some(shared))
        }
        None => (None, None),
    };
    let sink = Arc::new(ThinkingSink::new(policy, forward, ctx));
    (inner, Some(sink))
}

/// Run `turn` with `sink` in scope (or plainly when there is none).
pub async fn scope_turn<T>(
    sink: Option<Arc<ThinkingSink>>,
    turn: impl std::future::Future<Output = T>,
) -> T {
    match sink {
        Some(sink) => THINKING_SINK.scope(sink, turn).await,
        None => turn.await,
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
