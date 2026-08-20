// S4 — Screen 4: the chat page ("新對話" in the daily nav rail).
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
// Honest scope cuts (S4, documented per this crate's convention):
//   - Plain text only — no markdown rendering (bold/code/links render as
//     raw source characters). The web dashboard's markdown pipeline
//     (`channel_format`-adjacent) is out of scope for this pass.
//   - No file attachments, no agent picker, no conversation history list /
//     resume — single live conversation only (see `chat_ws.rs`'s own
//     "honest stubs" list, which this screen inherits).
//   - `step`/`progress` frames collapse into ONE transient status line
//     (`ChatState::status`), not a persistent collapsible tool-call tree.

use gpui::{div, prelude::*, px, Context, Div, ScrollHandle, SharedString, Stateful};
use tokio::sync::mpsc as tokio_mpsc;

use crate::chat_ws::{self, ConnState};
use crate::i18n::{self, Locale};
use crate::ime_input::ChatInputState;
use crate::mds_gpui::{empty_state, skeleton};
use crate::theme;
use crate::RootView;

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
    pub input: gpui::Entity<ChatInputState>,
    pub scroll_handle: ScrollHandle,
    tx: tokio_mpsc::UnboundedSender<chat_ws::Command>,
}

impl ChatState {
    pub fn new(input: gpui::Entity<ChatInputState>, tx: tokio_mpsc::UnboundedSender<chat_ws::Command>) -> Self {
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
        }
    }

    /// Kick off (or replace) the `/ws/chat` connection with a fresh JWT —
    /// called from `main.rs::handle_session_event` right alongside the main
    /// `/ws`'s own `ConnectWs` dispatch, same eager-connect timing.
    pub fn connect(&self, jwt: String) {
        let _ = self.tx.send(chat_ws::Command::Connect { jwt });
    }

    /// Tear down the chat connection — called when the main `/ws` session
    /// itself terminates (e.g. `WsAuthFailed`), since a stale JWT there
    /// means the chat socket's JWT is stale too.
    pub fn disconnect(&self) {
        let _ = self.tx.send(chat_ws::Command::Disconnect);
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

    let bubble = div()
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
        .when(!is_user && !is_system, |el| {
            el.bg(theme::alpha(theme::SURFACE_RAISED, 1.0)).text_color(theme::alpha(theme::FOREGROUND, 1.0))
        })
        .child(msg.content.clone())
        .children(msg.tokens.map(|t| {
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

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
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

    div()
        .id("chat-page")
        .size_full()
        .flex()
        .flex_col()
        .child(header)
        .child(message_list)
        .child(composer)
}
