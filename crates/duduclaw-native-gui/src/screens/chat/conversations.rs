// S4b — the chat page's left sidebar (task item #2): a real conversation
// list replacing P4a's "single live conversation only" stub, PLUS the p06
// decision this pass implements ("/conversations 併入 chat 左欄，方案 A" —
// see `commercial/design/duduclaw-s4a-pages/Conversations.dc.html`'s left
// half): search + date-grouped history folded into this one sidebar instead
// of a separate page.
//
// This file owns the sidebar's STATE (`ConversationsState`), the two RPC
// round trips (`fetch`/`fetch_history`), and the header/search-box UI. The
// per-row card, date-grouping, and search-matching logic live in
// `conversation_row.rs` (split out purely to stay under this crate's
// <800-line convention).
//
// Response shape read directly from `duduclaw-gateway/src/handlers.rs`
// (not guessed):
//   `chat.sessions.list` (`handle_chat_sessions_list`, ~line 30950) →
//     `{ "sessions": [ { "session_id", "agent_id", "title", "last_active"
//       (RFC3339), "turns", "tokens", "lineage" } ], newest-first,
//       archived excluded }`. Non-admin callers MUST pass a bound
//       `agent_id` or the RPC fails closed with "agent_id parameter is
//       required" (`check_agent_filter!` at the call site, ~line 6606) —
//       this client has no way to know the caller's role/bound-agent-id
//       (that lives on `LoginResponse.user`, which `main.rs`'s `RootView`
//       doesn't store anywhere reachable from here, and this pass's task
//       brief keeps `main.rs` off-limits — see `sidebar_rpc.rs`'s module doc
//       comment). Honest consequence: an admin login sees the full list;
//       a non-admin login's list stays empty (fails closed via `Rejected`,
//       degrades to an empty state, never panics) until a future pass
//       threads the caller's role/agent-id through.
//   `chat.sessions.history` (`handle_chat_sessions_history`, ~line 30996) →
//     `{ "session_id", "agent_id", "messages": [ { "role", "content",
//       "timestamp", "tokens" } ] }`, oldest→newest, tail-limited server-side.
//
// Notably ABSENT from `chat.sessions.list`: any last-message-content field.
// The design canvas's card second line ("小杜：要我現在去撈…") is a message
// preview this RPC does not expose — fetching every row's full history just
// for a preview snippet would be an N+1 network fetch on every list render,
// so this pass substitutes the resolved agent's display name instead (see
// `conversation_row::conversation_card`) and scopes the search filter to
// `title` + agent name only, not message content — a deliberate, disclosed
// simplification, not a bug (task brief: "取捨" — document the trade-off,
// don't hide it).
//
// "未讀點" (task item #2's canvas requirement) has no server-side signal
// either — `chat.sessions.list` carries no read/unread flag. This pass
// approximates it client-side: a row shows the dot until the user opens it
// at least once THIS APP SESSION (`ConversationsState::read_session_ids`,
// seeded empty at boot, cleared per-id on click) — labelled honestly, not
// pretended to be server truth. See `mark_read`'s doc comment.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use gpui::{div, prelude::*, px, Context, CursorStyle, Div, FocusHandle, KeyDownEvent, MouseButton, ScrollHandle, SharedString, Stateful};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::theme;
use crate::RootView;

use super::agents_picker;
use super::conversation_row::{self, ConversationSummary};
use super::sidebar_rpc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStatus {
    Idle,
    Loading,
    Ready,
    Error,
}

pub struct ConversationsState {
    pub sessions: Vec<ConversationSummary>,
    pub status: LoadStatus,
    pub search_query: String,
    /// See this module's doc comment on "未讀點" — client-local "opened at
    /// least once this app session" marker, not a server-truth read state.
    read_session_ids: HashSet<String>,
    /// `screens/chat.rs::render` only receives `&RootView` (see that file's
    /// module doc comment), so this `Cell` is the interior-mutability escape
    /// hatch that lets the one-shot initial fetch fire exactly once from a
    /// shared reference — see `chat.rs::maybe_boot_fetch`.
    boot_kicked: Cell<bool>,
    /// Stable across renders (unlike a freshly-constructed handle every
    /// frame, which would silently drop the list's scroll position on every
    /// unrelated `cx.notify()` — e.g. a streaming chunk arriving while the
    /// user is scrolling this list).
    pub scroll_handle: ScrollHandle,
    /// Lazily created on first render (needs `&App`, which
    /// `ConversationsState::new()` doesn't receive — see `sidebar_rpc.rs`'s
    /// module doc comment on why `ChatState::new()` has no `cx` this pass),
    /// then cached here so every subsequent render reuses the SAME handle
    /// (a fresh one each frame would orphan the window's focus target).
    /// This — plus the inlined `on_key_down` in `search_box` — mirrors
    /// `text_field.rs`'s plain, non-IME key-capture pattern rather than
    /// reusing its `TextField` entity type, since `TextField::new` needs
    /// `&mut App` at CONSTRUCTION time, not just at first render. Same
    /// documented IME gap as `text_field.rs`: composing CJK search terms via
    /// an OS IME candidate window will not work correctly.
    search_focus: RefCell<Option<FocusHandle>>,
}

impl ConversationsState {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            status: LoadStatus::Idle,
            search_query: String::new(),
            read_session_ids: HashSet::new(),
            boot_kicked: Cell::new(false),
            scroll_handle: ScrollHandle::new(),
            search_focus: RefCell::new(None),
        }
    }

    pub fn should_boot_fetch(&self) -> bool {
        !self.boot_kicked.get()
    }

    pub fn mark_boot_kicked(&self) {
        self.boot_kicked.set(true);
    }

    pub fn set_loading(&mut self) {
        self.status = LoadStatus::Loading;
    }

    pub fn apply_loaded(&mut self, sessions: Vec<ConversationSummary>) {
        self.sessions = sessions;
        self.status = LoadStatus::Ready;
    }

    pub fn apply_error(&mut self) {
        self.status = LoadStatus::Error;
    }

    /// Called when a row is clicked — clears its unread dot for the rest of
    /// this app session (see this module's doc comment on the client-local
    /// "unread" approximation).
    pub fn mark_read(&mut self, session_id: &str) {
        self.read_session_ids.insert(session_id.to_string());
    }

    /// `pub(super)` — read from `conversation_row.rs`'s card renderer (a
    /// sibling module under `chat`), not exposed outside this crate's chat
    /// subtree.
    pub(super) fn is_unread(&self, session_id: &str, currently_open: Option<&str>) -> bool {
        currently_open != Some(session_id) && !self.read_session_ids.contains(session_id)
    }
}

impl Default for ConversationsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Kick off the initial `chat.sessions.list` fetch — see this module's doc
/// comment for the admin-vs-non-admin scoping honesty note. Called from
/// `chat.rs::maybe_boot_fetch`.
pub fn fetch(rpc_tx: tokio_mpsc::UnboundedSender<sidebar_rpc::Command>, cx: &mut Context<RootView>) {
    let rx = sidebar_rpc::call(&rpc_tx, "chat.sessions.list", serde_json::json!({ "limit": 100 }));
    cx.spawn(async move |view, cx| {
        // Flip to `Loading` before awaiting the RPC round trip — `render()`
        // (which called `mark_boot_kicked` synchronously) only has `&RootView`
        // and cannot mutate `status` itself, so this spawned task's own
        // first tick is what the skeleton state in
        // `conversation_row::conversation_list` actually depends on.
        let _ = view.update(cx, |view, cx| {
            view.chat.conversations.set_loading();
            cx.notify();
        });
        let outcome = rx.await;
        let _ = view.update(cx, |view, cx| {
            match outcome {
                Ok(Ok(payload)) => view.chat.conversations.apply_loaded(conversation_row::parse_sessions(&payload)),
                Ok(Err(e)) => {
                    eprintln!("[chat] chat.sessions.list failed: {e:?}");
                    view.chat.conversations.apply_error();
                }
                Err(_) => {
                    eprintln!("[chat] chat.sessions.list: sidebar-rpc thread gone");
                    view.chat.conversations.apply_error();
                }
            }
            cx.notify();
        });
    })
    .detach();
}

/// Load one conversation's transcript and switch the chat view onto it —
/// called when a sidebar row is clicked (`conversation_row.rs`'s
/// `conversation_card`). Mirrors the web dashboard's `conversations-
/// store.ts::resume` (read directly, not guessed): set the session id to
/// the picked one, replace the visible messages, keep composing on that
/// same session for the next turn.
pub fn fetch_history(
    rpc_tx: tokio_mpsc::UnboundedSender<sidebar_rpc::Command>,
    session_id: String,
    cx: &mut Context<RootView>,
) {
    let rx = sidebar_rpc::call(&rpc_tx, "chat.sessions.history", serde_json::json!({ "session_id": session_id }));
    cx.spawn(async move |view, cx| {
        let outcome = rx.await;
        let _ = view.update(cx, |view, cx| {
            let locale = view.locale;
            match outcome {
                Ok(Ok(payload)) => view.chat.apply_history(&payload),
                Ok(Err(e)) => {
                    eprintln!("[chat] chat.sessions.history failed: {e:?}");
                    view.chat.push_system_error(locale, "native.chat.conversations.historyLoadError");
                }
                Err(_) => {
                    eprintln!("[chat] chat.sessions.history: sidebar-rpc thread gone");
                    view.chat.push_system_error(locale, "native.chat.conversations.historyLoadError");
                }
            }
            cx.notify();
        });
    })
    .detach();
}

fn ensure_focus_handle(state: &ConversationsState, cx: &gpui::App) -> FocusHandle {
    if let Some(existing) = state.search_focus.borrow().clone() {
        return existing;
    }
    let handle = cx.focus_handle();
    *state.search_focus.borrow_mut() = Some(handle.clone());
    handle
}

fn sidebar_header(locale: Locale, cx: &mut Context<RootView>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .pt_3()
        .pb_2()
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.chat.conversations.title")),
        )
        .child(
            div()
                .id("chat-conv-new")
                .size_6()
                .rounded(px(theme::RADIUS_MD))
                .bg(theme::alpha(theme::BRAND, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme::alpha(theme::BRAND, 0.85)))
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .text_size(px(14.))
                .font_weight(gpui::FontWeight::BOLD)
                .child("+")
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.chat.start_new_conversation();
                    cx.notify();
                })),
        )
}

fn search_box(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let handle = ensure_focus_handle(&state.chat.conversations, cx);
    let handle_for_click = handle.clone();
    let query = state.chat.conversations.search_query.clone();
    let is_empty = query.is_empty();

    div()
        .id("chat-conv-search")
        .track_focus(&handle)
        .key_context("ChatConvSearch")
        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
            let ks = &event.keystroke;
            if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
                return;
            }
            match ks.key.as_str() {
                "backspace" => {
                    this.chat.conversations.search_query.pop();
                    cx.notify();
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_deref() {
                        if !ch.is_empty() && ch.chars().all(|c| !c.is_control()) {
                            this.chat.conversations.search_query.push_str(ch);
                            cx.notify();
                        }
                    }
                }
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_this, _ev, window, cx| {
                window.focus(&handle_for_click, cx);
            }),
        )
        .mx_2()
        .mb_2()
        .flex()
        .items_center()
        .px_2()
        .py_1p5()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::MUTED, 0.4))
        .cursor(CursorStyle::IBeam)
        .text_size(px(theme::TEXT_XS))
        .text_color(if is_empty {
            theme::alpha(theme::MUTED_FOREGROUND, 1.0)
        } else {
            theme::alpha(theme::FOREGROUND, 1.0)
        })
        .child(SharedString::from(if is_empty {
            i18n::t(locale, "native.chat.conversations.searchPlaceholder").to_string()
        } else {
            query
        }))
}

/// The full left sidebar: header (+ "new conversation"), agent chip picker,
/// search box, grouped conversation list. `screens/chat.rs::render` embeds
/// this to the left of the existing transcript/composer column.
pub fn render_sidebar(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    div()
        .w(px(264.))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(theme::surface_border())
        .child(sidebar_header(locale, cx))
        .child(agents_picker::render_chips(state, cx))
        .child(search_box(state, cx))
        .child(conversation_row::conversation_list(state, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_and_mark_read_roundtrip() {
        let mut state = ConversationsState::new();
        assert!(state.is_unread("webchat:a", None));
        state.mark_read("webchat:a");
        assert!(!state.is_unread("webchat:a", None));
        // The currently-open session is never shown as unread even if it
        // was never explicitly marked.
        assert!(!state.is_unread("webchat:b", Some("webchat:b")));
    }

    #[test]
    fn boot_kick_guard_fires_once() {
        let state = ConversationsState::new();
        assert!(state.should_boot_fetch());
        state.mark_boot_kicked();
        assert!(!state.should_boot_fetch());
    }
}
