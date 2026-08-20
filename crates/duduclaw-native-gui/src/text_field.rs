// A minimal single-line editable text field entity.
//
// gpui's official pattern for a *fully* IME-integrated text input
// (`crates/gpui/examples/input.rs`, ~780 lines) implements
// `EntityInputHandler` for OS-level composition (CJK IME candidate windows,
// drag-to-select, system clipboard shortcuts, etc.) plus a hand-rolled
// `Element` for cursor/selection painting. That is real Phase-1b-or-later
// scope, not a Phase 1a login screen.
//
// This is the deliberately-smaller alternative: plain `on_key_down` capture
// (printable `key_char` appends, `backspace` pops) with no text selection,
// no IME composition, no clipboard integration. It is genuinely typeable —
// click to focus, type to edit — which is what the login screen's email/
// password fields need to look and feel real, but composing CJK text into
// it will not work correctly (no marked-text/candidate-window support).
// That gap is intentional scope, not a bug: worth flagging for whoever picks
// up the real `EntityInputHandler` version later (email/password fields are
// ASCII-shaped in practice, so it doesn't block Phase 1a's login screen).

use gpui::{
    div, prelude::*, px, App, Context, CursorStyle, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, SharedString, Window,
};

use crate::theme;

pub struct TextField {
    pub content: String,
    placeholder: SharedString,
    masked: bool,
    focus_handle: FocusHandle,
}

impl TextField {
    pub fn new(
        cx: &mut App,
        placeholder: impl Into<SharedString>,
        masked: bool,
        initial: impl Into<String>,
    ) -> gpui::Entity<Self> {
        cx.new(|cx| Self {
            content: initial.into(),
            placeholder: placeholder.into(),
            masked,
            focus_handle: cx.focus_handle(),
        })
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        // Let anything chorded with cmd/ctrl/function fall through (undo,
        // copy, quit, ...) instead of being swallowed as "typed text".
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            "backspace" => {
                self.content.pop();
                cx.notify();
            }
            _ => {
                if let Some(ch) = ks.key_char.as_deref() {
                    if !ch.is_empty() && ch.chars().all(|c| !c.is_control()) {
                        self.content.push_str(ch);
                        cx.notify();
                    }
                }
            }
        }
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let is_empty = self.content.is_empty();

        let display: SharedString = if is_empty {
            self.placeholder.clone()
        } else if self.masked {
            "•".repeat(self.content.chars().count()).into()
        } else {
            self.content.clone().into()
        };

        // MDS Input (spec §4): `h-8 rounded-lg border border-input
        // bg-transparent px-2.5 py-1 text-sm` + `dark:bg-input/30`,
        // `focus-visible:border-ring`. All five properties below are the
        // exact MDS Input recipe, not an approximation.
        div()
            .id("text-field")
            .track_focus(&self.focus_handle)
            .key_context("TextField")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    window.focus(&this.focus_handle, cx);
                }),
            )
            .w_full()
            .h(px(32.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::dark::input_bg())
            .border_1()
            .border_color(if focused {
                theme::alpha(theme::RING, 1.0).into()
            } else {
                theme::dark::input_border()
            })
            .cursor(CursorStyle::IBeam)
            .text_size(px(theme::TEXT_SM))
            .text_color(if is_empty {
                theme::alpha(theme::MUTED_FOREGROUND, 1.0)
            } else {
                theme::alpha(theme::FOREGROUND, 1.0)
            })
            .child(display)
    }
}
