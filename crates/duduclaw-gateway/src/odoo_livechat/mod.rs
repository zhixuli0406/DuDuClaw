//! Odoo website Live Chat → DuDuClaw AI employees.
//!
//! The gateway signs in to Odoo as a dedicated bot user that the admin added
//! as a Live Chat operator, keeps that user `online`, and answers the
//! sessions Odoo assigns to it. Each session is routed once to an AI
//! employee by the `[odoo_livechat]` audience rules (`duduclaw_odoo::livechat`).
//!
//! Default off (`[odoo_livechat] enabled = false`). The loop re-reads config
//! every tick, so the dashboard can switch it on, off or re-route without a
//! restart. It only runs in the process holding the gateway lock.
//!
//! Human takeover is Odoo-native: once an internal Odoo user other than the
//! bot posts in a session (or a visitor asks for a human and operators are
//! invited), the AI goes silent for that session until an admin hands it
//! back from the dashboard (`odoo.livechat.resume`) or the session ends.

mod state;
mod turn;
mod worker;

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use duduclaw_odoo::livechat::rpc::{ChatMessage, PresenceApi};
use duduclaw_odoo::OdooConnector;
use serde::Serialize;
use serde_json::{json, Value};

pub use state::{BridgeState, SessionState};
pub use worker::spawn;

/// Session-id prefix for the reply pipeline
/// (`odoolivechat:<instance tag>:<channel id>`, see [`session_key`]).
pub const SESSION_PREFIX: &str = "odoolivechat";

/// What a website visitor sees when the AI cannot answer (no account, model
/// error, timeout). The operator-facing failure text names accounts and
/// dashboard links, which a customer must never see.
pub const VISITOR_FALLBACK_MESSAGE: &str = "抱歉，目前暫時無法回覆，請稍後再試一次。";

/// Stable short tag for one Odoo instance (url + database). Part of every
/// reply-pipeline session key: `discuss.channel` ids restart from 1 in each
/// database, and two visitors must never share a conversation history.
pub fn instance_tag(url: &str, db: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{}\u{1f}{}", url.trim_end_matches('/'), db).as_bytes());
    hex::encode(digest)[..12].to_string()
}

/// Reply-pipeline session key for one live chat session.
pub fn session_key(instance: &str, channel_id: i64) -> String {
    format!("{SESSION_PREFIX}:{instance}:{channel_id}")
}

/// Whether `session_id` belongs to a public Odoo Live Chat visitor
/// (exact prefix token, never a substring test).
pub fn is_public_visitor_session(session_id: &str) -> bool {
    session_id
        .split_once(':')
        .is_some_and(|(prefix, rest)| prefix == SESSION_PREFIX && !rest.is_empty())
}

/// Who wrote a session message, from the bridge's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Author {
    /// The bot user itself (our own replies, the greeting, handoff notices).
    Bot,
    /// Another internal Odoo user — a human operator.
    Operator,
    /// A Chatbot script persona (messages from before the forward).
    ChatbotScript,
    /// The website visitor (anonymous guest or logged-in customer).
    Visitor,
}

/// Classify one message. A partner that is not internal, not a chatbot
/// persona and not the bot is the visitor; an author-less message with a
/// guest is the anonymous visitor; anything else (no author at all) is
/// treated as a script/system line, never as the visitor.
pub fn classify(
    msg: &ChatMessage,
    bot_partner_id: i64,
    internal_partners: &HashSet<i64>,
    chatbot_partners: &HashSet<i64>,
) -> Author {
    match (msg.author_partner_id, msg.author_guest_id) {
        (Some(p), _) if p == bot_partner_id => Author::Bot,
        (Some(p), _) if chatbot_partners.contains(&p) => Author::ChatbotScript,
        (Some(p), _) if internal_partners.contains(&p) => Author::Operator,
        (Some(_), _) => Author::Visitor,
        (None, Some(_)) => Author::Visitor,
        (None, None) => Author::ChatbotScript,
    }
}

/// Live status for the dashboard. Written by the worker each tick.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BridgeStatus {
    /// `off` / `connecting` / `online` / `error`.
    pub state: String,
    pub message: String,
    pub bot_name: String,
    pub bot_partner_id: Option<i64>,
    /// `bus` (Odoo 17/18) or `mail` (Odoo 19+).
    pub presence_api: Option<String>,
    pub last_poll_at: Option<String>,
    pub last_error_at: Option<String>,
}

fn status_cell() -> &'static Mutex<BridgeStatus> {
    static CELL: OnceLock<Mutex<BridgeStatus>> = OnceLock::new();
    CELL.get_or_init(|| {
        Mutex::new(BridgeStatus {
            state: "off".into(),
            ..Default::default()
        })
    })
}

pub(crate) fn update_status(f: impl FnOnce(&mut BridgeStatus)) {
    if let Ok(mut s) = status_cell().lock() {
        f(&mut s);
    }
}

/// The in-memory session state shared by the worker and the RPC handlers.
/// Keyed by home so a second home (tests) never reads another's sessions.
fn shared_state() -> &'static Mutex<Option<(std::path::PathBuf, BridgeState)>> {
    static CELL: OnceLock<Mutex<Option<(std::path::PathBuf, BridgeState)>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

fn state_for<'a>(
    slot: &'a mut Option<(std::path::PathBuf, BridgeState)>,
    home: &Path,
) -> &'a mut BridgeState {
    if slot.as_ref().is_none_or(|(h, _)| h != home) {
        *slot = Some((home.to_path_buf(), BridgeState::load(home)));
    }
    &mut slot.as_mut().expect("just set").1
}

/// Run `f` on the shared state (loaded from disk on first use) and persist.
pub(crate) fn with_state<R>(home: &Path, f: impl FnOnce(&mut BridgeState) -> R) -> R {
    let mut guard = match shared_state().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let state = state_for(&mut guard, home);
    let out = f(state);
    if let Err(e) = state.save(home) {
        tracing::warn!(error = %e, "odoo livechat: failed to persist state.json");
    }
    out
}

/// Read-only view of the shared state (no write-back).
pub(crate) fn read_state<R>(home: &Path, f: impl FnOnce(&BridgeState) -> R) -> R {
    let mut guard = match shared_state().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(state_for(&mut guard, home))
}

/// What the bridge needs to mark the bot offline at shutdown.
pub(crate) struct PresenceHandle {
    pub conn: Arc<OdooConnector>,
    pub api: PresenceApi,
    pub bot_uid: i64,
}

fn presence_cell() -> &'static Mutex<Option<Arc<PresenceHandle>>> {
    static CELL: OnceLock<Mutex<Option<Arc<PresenceHandle>>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

pub(crate) fn set_presence_handle(h: Option<Arc<PresenceHandle>>) {
    if let Ok(mut g) = presence_cell().lock() {
        *g = h;
    }
}

/// Gateway shutdown: mark the bot offline so Odoo stops routing visitors to
/// a bot that is about to stop answering. Best effort; bounded by the caller.
pub async fn go_offline_on_shutdown() {
    let handle = presence_cell().lock().ok().and_then(|g| g.clone());
    if let Some(h) = handle {
        match duduclaw_odoo::livechat::rpc::set_offline(&h.conn, h.api, h.bot_uid).await {
            Ok(()) => tracing::info!("odoo livechat: bot marked offline for shutdown"),
            Err(e) => tracing::warn!(error = %e, "odoo livechat: could not mark the bot offline"),
        }
    }
}

/// `odoo.livechat.status` payload: bridge status plus open sessions.
pub fn status_json(home: &Path) -> Value {
    let status = status_cell()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    let sessions: Vec<Value> = read_state(home, |st| {
        st.sessions
            .iter()
            .rev()
            .map(|(id, s)| {
                json!({
                    "id": id,
                    "agent": s.agent,
                    "rule": s.rule,
                    "human": s.human,
                    "human_reason": s.human_reason,
                    "visitor_name": s.visitor_name,
                    "country": s.country,
                    "lang": s.lang,
                    "logged_in": s.logged_in,
                    "replies": s.replies,
                    "first_seen": s.first_seen,
                    "last_activity": s.last_activity,
                })
            })
            .collect()
    });
    json!({ "status": status, "sessions": sessions })
}

/// Hand a taken-over session back to the AI. Returns whether it existed.
pub fn resume_session(home: &Path, session_id: i64) -> bool {
    with_state(home, |st| match st.sessions.get_mut(&session_id) {
        Some(s) => {
            s.human = false;
            s.human_reason = None;
            true
        }
        None => false,
    })
}

#[cfg(test)]
mod tests;
