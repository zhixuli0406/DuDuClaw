// S4 / S4b — Screen 4: the chat page ("新對話" in the daily nav rail).
//
// Data flow (protocol read directly from `crates/duduclaw-gateway/src/
// webchat.rs`, mirrored in `chat_protocol.rs`/`chat_ws.rs` — see those
// files' doc comments): a dedicated `/ws/chat` connection, separate from
// the main authenticated `/ws` (`ws_status.rs`). `main.rs` connects it
// eagerly right after login (same eager-connect timing as the main `/ws`),
// so by the time a user navigates here it's normally already authenticated.
//
// This module owns [`ChatState`] (message list, connection status, the
// IME-capable composer entity, the scroll handle) as ONE cohesive field on
// `RootView` (`RootView.chat`), rather than a dozen flat fields — keeps
// `main.rs` from re-growing the way it would if S4 just tacked ten more
// `pub` fields onto `RootView` directly.
//
// ── S4b (this pass) ────────────────────────────────────────────────────
// Closes the first three of S4's honest scope cuts:
//   1. Markdown rendering for agent bubbles (`markdown.rs`).
//   2. A real left-sidebar conversation list (`conversations.rs`) — this is
//      also where the p06 decision lands ("/conversations 併入 chat 左欄，
//      方案 A": see `commercial/design/duduclaw-s4a-pages/Conversations.
//      dc.html`'s left half — no separate `/conversations` page exists or
//      is planned).
//   3. An agent picker for new conversations (`agents_picker.rs`).
// Still out of scope (deferred, not forgotten): file attachments, a
// clipboard, a `TodoWrite` progress board, voice/video calls.
//
// ── Why this pass never touches `main.rs`/`nav.rs` ────────────────────────
// A concurrent pass owns `main.rs`'s wiring and `nav.rs`'s routing this
// round. Every new piece of state this pass needs (the sidebar RPC
// connection, the agents/conversations fetch triggers) is therefore
// constructed *inside* this module's own `ChatState::new()`/`render()`
// rather than threaded in from `main.rs` — see `sidebar_rpc.rs`'s module
// doc comment for the concrete consequence (a second, simpler RPC
// connection instead of reusing `ws_status.rs`'s dormant `Command::Call`)
// and `maybe_boot_fetch`'s doc comment below for how the initial
// conversations/agents fetch fires without `main.rs`'s poll loop.

use gpui::{div, prelude::*, px, Context, Div, ScrollHandle, SharedString, Stateful};
use tokio::sync::mpsc as tokio_mpsc;

use crate::chat_ws::{self, ConnState};
use crate::i18n::{self, Locale};
use crate::ime_input::ImeTextInput;
use crate::mds_gpui::{empty_state, skeleton};
use crate::theme;
use crate::RootView;

mod agents_picker;
mod conversation_row;
mod conversations;
mod markdown;
mod sidebar_rpc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone)]
pub struct UiMessage {
    pub role: Role,
    pub content: String,
    pub tokens: Option<u32>,
}

/// All chat-page state, bundled behind `RootView.chat`. Constructed once at
/// window-open time (`main.rs`) — the `input` entity is created there
/// (needs `&mut App`, available at that point) and handed in.
pub struct ChatState {
    pub messages: Vec<UiMessage>,
    pub conn_state: ConnState,
    pub session_id: Option<String>,
    pub agent_name: Option<String>,
    pub agent_icon: Option<String>,
    pub model: Option<String>,
    /// Transient "still working" line folded from `step`/`progress` frames
    /// — cleared on `Done`/`Error`, see `chat_ws.rs`'s doc comment on why
    /// no history/tree is kept.
    pub status: Option<String>,
    pub streaming: bool,
    conv_id: String,
    pub input: gpui::Entity<ImeTextInput>,
    pub scroll_handle: ScrollHandle,
    tx: tokio_mpsc::UnboundedSender<chat_ws::Command>,
    /// S4b: the left sidebar's conversation list (task item #2).
    pub conversations: conversations::ConversationsState,
    /// S4b: the agent picker for new conversations (task item #3).
    pub agents: agents_picker::AgentsState,
    /// `None` = the server's default/main agent (byte-compatible with the
    /// pre-S4b protocol, which never sent an `agent` field at all) — see
    /// `select_agent`'s doc comment.
    pub selected_agent_id: Option<String>,
    /// S4b: the sidebar's own dedicated RPC connection — see
    /// `sidebar_rpc.rs`'s module doc comment for why this isn't
    /// `ws_status.rs`'s already-open `/ws`.
    rpc_tx: tokio_mpsc::UnboundedSender<sidebar_rpc::Command>,
}

impl ChatState {
    pub fn new(input: gpui::Entity<ImeTextInput>, tx: tokio_mpsc::UnboundedSender<chat_ws::Command>) -> Self {
        Self {
            messages: Vec::new(),
            conn_state: ConnState::Disconnected,
            session_id: None,
            agent_name: None,
            agent_icon: None,
            model: None,
            status: None,
            streaming: false,
            conv_id: mint_conv_id(),
            input,
            scroll_handle: ScrollHandle::new(),
            tx,
            conversations: conversations::ConversationsState::new(),
            agents: agents_picker::AgentsState::new(),
            selected_agent_id: None,
            rpc_tx: sidebar_rpc::spawn(),
        }
    }

    /// Kick off (or replace) the `/ws/chat` connection with a fresh JWT —
    /// called from `main.rs::handle_session_event` right alongside the main
    /// `/ws`'s own `ConnectWs` dispatch, same eager-connect timing. Also
    /// hands the same JWT to the S4b sidebar RPC connection (see
    /// `sidebar_rpc.rs`) — `main.rs` calls this one method for both, so no
    /// `main.rs` edit was needed to wire the second connection in.
    pub fn connect(&self, jwt: String) {
        let _ = self.tx.send(chat_ws::Command::Connect { jwt: jwt.clone() });
        let _ = self.rpc_tx.send(sidebar_rpc::Command::Connect { jwt });
    }

    /// Tear down the chat connection — called when the main `/ws` session
    /// itself terminates (e.g. `WsAuthFailed`), since a stale JWT there
    /// means the chat socket's JWT is stale too (and the sidebar RPC
    /// connection's JWT, forgotten here for the same reason).
    pub fn disconnect(&self) {
        let _ = self.tx.send(chat_ws::Command::Disconnect);
        let _ = self.rpc_tx.send(sidebar_rpc::Command::Disconnect);
    }

    /// Clone of the sidebar RPC command sender — `conversations.rs`/
    /// `agents_picker.rs` (this module's own children) need it to issue
    /// their own calls; kept as a method rather than a `pub` field so
    /// `ChatState` stays the single owner of the underlying channel.
    fn rpc_tx(&self) -> tokio_mpsc::UnboundedSender<sidebar_rpc::Command> {
        self.rpc_tx.clone()
    }

    /// Switch which AI staff member a NEW conversation talks to (task item
    /// #3). Mirrors the web dashboard's `chat-store.ts::selectAgent` (read
    /// directly, not guessed): switching partners starts a fresh thread —
    /// each employee has an isolated server-side session, so keeping the
    /// old messages/session id around would either bleed A's context into
    /// B's view or make the next send read as an invalid cross-agent
    /// resume. A no-op re-selection of the already-active agent does
    /// nothing (matches the web store's own early-return).
    pub fn select_agent(&mut self, agent_id: Option<String>) {
        if agent_id == self.selected_agent_id {
            return;
        }
        self.selected_agent_id = agent_id;
        self.start_new_conversation();
    }

    /// The sidebar's "+" button (task item #2, per the design canvas) — a
    /// blank conversation with whichever agent is currently selected.
    pub fn start_new_conversation(&mut self) {
        self.messages.clear();
        self.session_id = None;
        self.status = None;
        self.streaming = false;
        self.conv_id = mint_conv_id();
    }

    /// Load a past conversation's transcript into the view — called by
    /// `conversations::fetch_history` once `chat.sessions.history` resolves.
    /// Payload shape read directly from `handle_chat_sessions_history`
    /// (`duduclaw-gateway/src/handlers.rs`, ~line 30996): `{ "session_id",
    /// "agent_id", "messages": [ { "role", "content", "timestamp",
    /// "tokens" } ] }`, oldest→newest. A malformed/missing field degrades to
    /// an empty value per-message rather than dropping the whole load.
    pub fn apply_history(&mut self, payload: &serde_json::Value) {
        let session_id = payload.get("session_id").and_then(|v| v.as_str()).map(str::to_string);
        let messages = payload
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|m| {
                        let role = match m.get("role").and_then(|v| v.as_str()).unwrap_or("") {
                            "user" => Role::User,
                            "assistant" => Role::Assistant,
                            _ => Role::System,
                        };
                        let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let tokens = m.get("tokens").and_then(|v| v.as_u64()).map(|t| t as u32);
                        UiMessage { role, content, tokens }
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.session_id = session_id;
        self.messages = messages;
        self.status = None;
        self.streaming = false;
        self.scroll_handle.scroll_to_bottom();
    }

    /// Localized system-bubble error line — used by `conversations.rs`'s
    /// fetch failure paths, which run inside a `cx.spawn` block that has a
    /// `Locale` (from `RootView.locale`, `Copy`) but no direct route to call
    /// `i18n::t` before reaching into `ChatState` itself.
    pub fn push_system_error(&mut self, locale: Locale, i18n_key: &str) {
        self.push_system(format!("⚠️ {}", i18n::t(locale, i18n_key)));
    }

    /// Apply one event from the background chat manager (`chat_ws.rs`).
    /// Called from `main.rs::handle_chat_event`, which fires `cx.notify()`
    /// itself right after — mirrors `ws_status.rs`'s `handle_session_event`
    /// shape, so this method stays plain `&mut self` mutation.
    pub fn apply(&mut self, event: chat_ws::Event) {
        match event {
            chat_ws::Event::ConnState(s) => self.conn_state = s,
            chat_ws::Event::AuthFailed => {
                self.conn_state = ConnState::Disconnected;
                self.push_system("⚠️ 聊天連線驗證失敗，請重新登入後再試一次。".to_string());
            }
            chat_ws::Event::SessionInfo { session_id, agent_name, agent_icon, model } => {
                self.session_id = Some(session_id);
                self.agent_name = Some(agent_name);
                self.agent_icon = Some(agent_icon);
                self.model = Some(model);
            }
            chat_ws::Event::Chunk { content } => {
                self.streaming = true;
                self.status = None;
                match self.messages.last_mut() {
                    Some(m) if m.role == Role::Assistant && m.tokens.is_none() => m.content.push_str(&content),
                    _ => self.messages.push(UiMessage { role: Role::Assistant, content, tokens: None }),
                }
                self.scroll_handle.scroll_to_bottom();
            }
            chat_ws::Event::Status { text } => {
                self.status = Some(text);
                self.scroll_handle.scroll_to_bottom();
            }
            chat_ws::Event::Done { content, tokens_used, model } => {
                self.streaming = false;
                self.status = None;
                match self.messages.last_mut() {
                    Some(m) if m.role == Role::Assistant && m.tokens.is_none() => {
                        m.content = content;
                        m.tokens = Some(tokens_used);
                    }
                    _ => self.messages.push(UiMessage { role: Role::Assistant, content, tokens: Some(tokens_used) }),
                }
                if let Some(model) = model {
                    self.model = Some(model);
                }
                self.scroll_handle.scroll_to_bottom();
            }
            chat_ws::Event::Error { message, .. } => {
                self.streaming = false;
                self.status = None;
                self.push_system(format!("⚠️ {message}"));
            }
        }
    }

    fn push_system(&mut self, content: String) {
        self.messages.push(UiMessage { role: Role::System, content, tokens: None });
        self.scroll_handle.scroll_to_bottom();
    }

    /// Send one user turn — called both from the composer's Enter-to-submit
    /// event subscription and from the send button's click handler (see
    /// `render` below), so this is the single place that actually talks to
    /// `chat_ws.rs`.
    pub fn submit(&mut self, content: String) {
        let content = content.trim().to_string();
        if content.is_empty() {
            return;
        }
        if self.conn_state != ConnState::Authenticated {
            self.push_system("⚠️ 尚未連線到伺服器，請稍候片刻再送出。".to_string());
            return;
        }
        self.messages.push(UiMessage { role: Role::User, content: content.clone(), tokens: None });
        self.streaming = true;
        self.scroll_handle.scroll_to_bottom();
        let _ = self.tx.send(chat_ws::Command::Send {
            content,
            session_id: self.session_id.clone(),
            agent: self.selected_agent_id.clone(),
            conv: Some(self.conv_id.clone()),
        });
    }
}

/// `[A-Za-z0-9_-]`-only, well under the gateway's 64-char cap
/// (`webchat.rs::sanitize_conv_nonce`) — see that function's doc comment
/// for why the charset matters (it's interpolated into a session id).
fn mint_conv_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("n-{nanos:x}")
}

fn conn_dot_color(state: ConnState) -> u32 {
    match state {
        ConnState::Authenticated => theme::SUCCESS,
        ConnState::Disconnected | ConnState::Connecting | ConnState::Connected => theme::WARNING,
    }
}

fn conn_label(locale: Locale, state: ConnState) -> SharedString {
    match state {
        ConnState::Disconnected => i18n::t(locale, "native.chat.reconnecting"),
        ConnState::Connecting | ConnState::Connected => i18n::t(locale, "native.chat.connecting"),
        ConnState::Authenticated => i18n::t(locale, "native.chat.connected"),
    }
}

fn message_bubble(locale: Locale, msg: &UiMessage) -> Div {
    let is_user = msg.role == Role::User;
    let is_system = msg.role == Role::System;
    // S4b task item #1: markdown rendering, agent bubbles only — user/system
    // text is shown verbatim (a user's own typed message has no markdown
    // source to render, and system lines are this crate's own short status
    // strings, not agent output).
    let is_assistant = !is_user && !is_system;

    let mut bubble = div()
        .max_w(px(560.))
        .px_3p5()
        .py_2p5()
        .rounded(px(theme::RADIUS_XL))
        .text_size(px(theme::TEXT_SM))
        .when(is_user, |el| {
            el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
        })
        .when(is_system, |el| {
            el.bg(theme::alpha(theme::WARNING, 0.12)).text_color(theme::alpha(theme::WARNING, 1.0))
        })
        .when(is_assistant, |el| {
            el.bg(theme::alpha(theme::SURFACE_RAISED, 1.0)).text_color(theme::alpha(theme::FOREGROUND, 1.0))
        });
    bubble = if is_assistant {
        bubble.child(markdown::render_markdown(&msg.content))
    } else {
        bubble.child(msg.content.clone())
    };
    let bubble = bubble.children(msg.tokens.map(|t| {
            div()
                .mt_1()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(
                    if is_user { theme::BRAND_FOREGROUND } else { theme::MUTED_FOREGROUND },
                    0.7,
                ))
                .child(i18n::t1(locale, "native.chat.tokens", "n", &t.to_string()))
        }));

    div().w_full().flex().when(is_user, |el| el.justify_end()).when(!is_user, |el| el.justify_start()).child(bubble)
}

/// One-shot initial fetch for the sidebar's conversation list + agent chips
/// — fired from the top of `render` (the only place in this module that has
/// a live `cx: &mut Context<RootView>`; see this file's module doc comment
/// on why `main.rs`'s poll loop isn't an option this pass). Gated by two
/// `Cell<bool>` latches (`ConversationsState`/`AgentsState::should_boot_
/// fetch`) so re-renders — which happen constantly while streaming — don't
/// re-issue the fetch every frame; interior mutability is what lets those
/// latches flip from a `&RootView` (render only reads state, see the
/// existing S4 comment on `self` reborrowing below).
///
/// Gated on `chat.session_id.is_some()`: that field is only ever set once
/// the `/ws/chat` handshake's `session_info` frame arrives, which happens
/// at the same moment `ChatState::connect` already handed the sidebar RPC
/// connection its JWT — using it as the trigger condition needs no new
/// "have we connected" bookkeeping. Honest limitation: this latches once
/// per app run, so a relogin after a `WsAuthFailed` kick-back will not
/// automatically re-fetch — S4 has no logout button either, so this isn't a
/// regression, just an acknowledged gap for whenever one is added.
fn maybe_boot_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.chat.session_id.is_none() {
        return;
    }
    if state.chat.conversations.should_boot_fetch() {
        state.chat.conversations.mark_boot_kicked();
        conversations::fetch(state.chat.rpc_tx(), cx);
    }
    if state.chat.agents.should_boot_fetch() {
        state.chat.agents.mark_boot_kicked();
        agents_picker::fetch(state.chat.rpc_tx(), cx);
    }
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_boot_fetch(state, cx);

    let locale = state.locale;
    let chat = &state.chat;

    let header_title =
        chat.agent_name.clone().map(SharedString::from).unwrap_or_else(|| i18n::t(locale, "app.name"));
    let header_icon = chat.agent_icon.clone().unwrap_or_else(|| "🐾".to_string());

    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .pb_3()
        .border_b_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(18.)).child(header_icon))
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(header_title),
        )
        .child(div().size_2().rounded_full().bg(theme::alpha(conn_dot_color(chat.conn_state), 1.0)))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(conn_label(locale, chat.conn_state)),
        );

    // Both arms must return the same concrete type for the `if`/`else` to
    // unify (see main.rs's gpui gotcha list) — `.overflow_y_scroll()` below
    // requires `.id(...)` first (`StatefulInteractiveElement`), which
    // changes the type from `Div` to `Stateful<Div>`, so the empty-state
    // arm gets a (functionally unused) `.id(...)` too just to match.
    let message_list: Stateful<Div> = if chat.messages.is_empty() {
        div().id("chat-messages-empty").flex_1().flex().items_center().justify_center().child(empty_state(
            "💬",
            i18n::t(locale, "native.chat.emptyTitle"),
            Some(i18n::t(locale, "native.chat.emptyDesc")),
            None::<Div>,
        ))
    } else {
        let mut rows = Vec::with_capacity(chat.messages.len() + 1);
        for msg in &chat.messages {
            rows.push(message_bubble(locale, msg));
        }
        if let Some(status) = &chat.status {
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(skeleton(px(14.), px(14.)).rounded_full())
                    .child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                            .child(status.clone()),
                    ),
            );
        }
        div()
            .id("chat-messages")
            .flex_1()
            .track_scroll(&chat.scroll_handle)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .py_3()
            .children(rows)
    };

    let can_send = chat.conn_state == ConnState::Authenticated;
    let composer = div()
        .flex()
        .items_end()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex_1()
                .max_h(px(160.))
                .overflow_hidden()
                .px_3()
                .py_2()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::dark::input_bg())
                .border_1()
                .border_color(theme::dark::input_border())
                .child(chat.input.clone()),
        )
        .child(
            div()
                .id("chat-send")
                .h_9()
                .px_3p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS_LG))
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .when(can_send, |el| {
                    el.bg(theme::alpha(theme::BRAND, 1.0))
                        .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::alpha(theme::BRAND, 0.90)))
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            let content = this
                                .chat
                                .input
                                .update(cx, |input, cx| {
                                    let taken = input.engine.take_content();
                                    cx.notify();
                                    taken
                                });
                            this.chat.submit(content);
                            cx.notify();
                        }))
                })
                .when(!can_send, |el| {
                    el.bg(theme::alpha(theme::MUTED, 0.4)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                })
                .child(i18n::t(locale, "native.chat.send")),
        );

    // S4b: the transcript/composer column (S4's original single-column page
    // content, unchanged in substance) now sits to the RIGHT of the
    // conversations sidebar instead of being the whole page — see
    // `conversations::render_sidebar` and this file's module doc comment on
    // the p06 "併入 chat 左欄" decision.
    let main_column = div()
        .id("chat-main-column")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(message_list)
        .child(composer);

    div()
        .id("chat-page")
        .size_full()
        .flex()
        .child(conversations::render_sidebar(state, cx))
        .child(main_column)
}
