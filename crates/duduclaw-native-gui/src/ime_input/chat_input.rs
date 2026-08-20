// S4 — the gpui `Entity` for the chat composer: owns a `TextEngine` (pure
// logic, see `text_engine.rs`) plus the gpui-specific glue a real input
// needs — focus, `EntityInputHandler` (IME composition), keyboard/mouse
// event handling, and the row layout `element.rs`'s paint pass hands back
// each frame for hit-testing.
//
// Source/attribution: same as `text_engine.rs`/`element.rs` — adapted from
// zed-industries/zed's `crates/gpui/examples/input.rs` (Apache-2.0, pinned
// rev `7a7c3e1d2f03195c5fa19bc890da330ad7f3abef`), extended for multi-line
// (Shift+Enter) and a submit-on-Enter event instead of the example's
// standalone demo app shell.
//
// Key handling deliberately follows this crate's OWN established
// convention (`text_field.rs`'s plain `on_key_down`, not the zed example's
// `actions!`/`KeyBinding` global-registration system) — no new global
// keybindings need to be wired into `main.rs` for this to work, and it
// keeps the composer self-contained.
//
// Honest scope cuts (documented, not silent):
//   - Up/Down move the cursor to the start of the adjacent row, not a
//     column-preserving vertical caret (needs per-row x-position tracking
//     this pass doesn't add).
//   - No clipboard (Cmd+C/V/X) and no system character palette — `text_
//     field.rs`'s login fields don't have them either; a chat composer
//     wants them eventually, tracked as follow-up, not blocking S4.

use std::ops::Range;

use gpui::{
    App, AppContext, Bounds, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, UTF16Selection, Window,
};

use super::element::{byte_offset_for_point, ChatInputElement, PaintedRow};
use super::text_engine::TextEngine;
use crate::theme;

#[derive(Debug, Clone)]
pub enum ChatInputEvent {
    /// Enter (without Shift) while not mid-composition — carries the
    /// content that was in the box (already cleared from `engine` by the
    /// time this fires; see `on_key_down`).
    Submit(String),
}

pub struct ChatInputState {
    /// `pub(super)` — `element.rs` (a sibling module under `ime_input`)
    /// reads this directly for `window.handle_input(&focus_handle, ...)`
    /// and the focus-ring/cursor-visibility check. Kept out of the public
    /// API surface past `ime_input`'s own boundary; callers outside this
    /// module go through [`Focusable::focus_handle`] instead.
    pub(super) focus_handle: FocusHandle,
    pub engine: TextEngine,
    pub placeholder: SharedString,
    is_selecting: bool,
    last_bounds: Option<Bounds<Pixels>>,
    last_rows: Vec<PaintedRow>,
    last_line_height: Pixels,
}

impl ChatInputState {
    pub fn new(cx: &mut App, placeholder: impl Into<SharedString>) -> Entity<Self> {
        cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
            engine: TextEngine::new(),
            placeholder: placeholder.into(),
            is_selecting: false,
            last_bounds: None,
            last_rows: Vec::new(),
            last_line_height: Pixels::ZERO,
        })
    }

    /// Called from `element.rs`'s `paint` with this frame's row layout —
    /// the only way mouse hit-testing and IME candidate-window placement
    /// (`bounds_for_range`) can work, since they need to know where text
    /// actually landed on screen, which only the paint pass computes.
    pub(super) fn set_last_layout(&mut self, rows: Vec<PaintedRow>, bounds: Bounds<Pixels>) {
        self.last_rows = rows;
        self.last_bounds = Some(bounds);
    }

    pub(super) fn note_line_height(&mut self, line_height: Pixels) {
        self.last_line_height = line_height;
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;

        match ks.key.as_str() {
            "enter" => {
                if ks.modifiers.shift {
                    self.engine.replace_text_in_range(None, "\n");
                    cx.notify();
                } else if self.engine.marked_range.is_none() {
                    // Guard against a raw "enter" ever reaching here mid-
                    // composition (see `text_engine.rs`'s module doc
                    // comment: the OS normally consumes Enter itself to
                    // confirm a candidate and this app never sees it as a
                    // key event at all — this is defence-in-depth, not the
                    // primary mechanism).
                    let content = self.engine.take_content();
                    cx.notify();
                    cx.emit(ChatInputEvent::Submit(content));
                }
                return;
            }
            "backspace" => {
                self.delete_backward(cx);
                return;
            }
            "delete" => {
                self.delete_forward(cx);
                return;
            }
            "left" => {
                self.move_horizontal(-1, ks.modifiers.shift, cx);
                return;
            }
            "right" => {
                self.move_horizontal(1, ks.modifiers.shift, cx);
                return;
            }
            "up" => {
                self.move_row(-1, cx);
                return;
            }
            "down" => {
                self.move_row(1, cx);
                return;
            }
            "home" => {
                let row = self.engine.row_for_offset(self.engine.cursor_offset());
                let start = self.engine.rows()[row].start;
                self.engine.move_to(start);
                cx.notify();
                return;
            }
            "end" => {
                let row = self.engine.row_for_offset(self.engine.cursor_offset());
                let end = self.engine.rows()[row].end;
                self.engine.move_to(end);
                cx.notify();
                return;
            }
            _ => {}
        }

        if ks.modifiers.platform && ks.key.as_str() == "a" {
            self.engine.select_all();
            cx.notify();
            return;
        }
        // Let anything else chorded with cmd/ctrl/function fall through
        // (undo, quit, window shortcuts, ...) instead of being swallowed
        // as "typed text" — same guard `text_field.rs` uses.
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        if let Some(ch) = ks.key_char.as_deref() {
            if !ch.is_empty() && ch.chars().all(|c| !c.is_control()) {
                self.engine.replace_text_in_range(None, ch);
                cx.notify();
            }
        }
        let _ = window;
    }

    fn delete_backward(&mut self, cx: &mut Context<Self>) {
        if self.engine.selected_range.is_empty() {
            let prev = self.engine.previous_boundary(self.engine.cursor_offset());
            if prev == self.engine.cursor_offset() {
                return; // already at the start — nothing to delete
            }
            self.engine.select_to(prev);
        }
        self.engine.replace_text_in_range(None, "");
        cx.notify();
    }

    fn delete_forward(&mut self, cx: &mut Context<Self>) {
        if self.engine.selected_range.is_empty() {
            let next = self.engine.next_boundary(self.engine.cursor_offset());
            if next == self.engine.cursor_offset() {
                return;
            }
            self.engine.select_to(next);
        }
        self.engine.replace_text_in_range(None, "");
        cx.notify();
    }

    fn move_horizontal(&mut self, dir: i8, extend_selection: bool, cx: &mut Context<Self>) {
        let cursor = self.engine.cursor_offset();
        let target =
            if dir < 0 { self.engine.previous_boundary(cursor) } else { self.engine.next_boundary(cursor) };

        if extend_selection {
            self.engine.select_to(target);
        } else if self.engine.selected_range.is_empty() {
            self.engine.move_to(target);
        } else {
            // Collapsing an active (non-extending) selection lands on
            // whichever edge the direction points to, matching standard
            // editor behavior — a bare arrow key never both collapses AND
            // moves by one further step.
            let edge = if dir < 0 { self.engine.selected_range.start } else { self.engine.selected_range.end };
            self.engine.move_to(edge);
        }
        cx.notify();
    }

    /// Move to the start of the row above/below the cursor's current row —
    /// see this file's module doc comment for the "no column-preserving
    /// vertical caret" scope cut.
    fn move_row(&mut self, dir: i8, cx: &mut Context<Self>) {
        let rows = self.engine.rows();
        let current = self.engine.row_for_offset(self.engine.cursor_offset());
        let target_row = if dir < 0 {
            current.saturating_sub(1)
        } else {
            (current + 1).min(rows.len().saturating_sub(1))
        };
        if let Some(range) = rows.get(target_row) {
            self.engine.move_to(range.start);
            cx.notify();
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        let offset = self.byte_offset_for_point(event.position);
        if event.modifiers.shift {
            self.engine.select_to(offset);
        } else {
            self.engine.move_to(offset);
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            let offset = self.byte_offset_for_point(event.position);
            self.engine.select_to(offset);
            cx.notify();
        }
    }

    fn byte_offset_for_point(&self, point: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else { return self.engine.content.len() };
        byte_offset_for_point(point, &bounds, self.last_line_height, &self.last_rows, self.engine.content.len())
    }
}

impl EventEmitter<ChatInputEvent> for ChatInputState {}

impl Focusable for ChatInputState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for ChatInputState {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.engine.range_from_utf16(&range_utf16);
        actual_range.replace(self.engine.range_to_utf16(&range));
        Some(self.engine.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.engine.range_to_utf16(&self.engine.selected_range),
            reversed: self.engine.selection_reversed,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.engine.marked_range.as_ref().map(|r| self.engine.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.engine.unmark_text();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.engine.replace_text_in_range(range_utf16, new_text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.engine.replace_and_mark_text_in_range(range_utf16, new_text, new_selected_range_utf16);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.engine.range_from_utf16(&range_utf16);
        let row_idx = self.engine.row_for_offset(range.start);
        let row = self.last_rows.get(row_idx)?;
        let row_top = bounds.top() + self.last_line_height * row_idx;
        let local_start = range.start.saturating_sub(row.byte_range.start);
        let local_end = range.end.saturating_sub(row.byte_range.start);
        Some(Bounds::from_corners(
            gpui::point(bounds.left() + row.line.x_for_index(local_start), row_top),
            gpui::point(bounds.left() + row.line.x_for_index(local_end), row_top + self.last_line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let offset = byte_offset_for_point(
            point,
            &bounds,
            self.last_line_height,
            &self.last_rows,
            self.engine.content.len(),
        );
        Some(self.engine.offset_to_utf16(offset))
    }
}

impl Render for ChatInputState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::prelude::*;

        // Track the render-time line height so mouse hit-testing (which
        // runs outside the paint pass, e.g. on a raw `MouseMoveEvent`) uses
        // the same value `element.rs` painted with. `window.line_height()`
        // reflects whatever `.line_height(px(..))` this div sets below.
        let line_height = gpui::px(theme::TEXT_SM * 1.4);
        self.note_line_height(line_height);

        gpui::div()
            .id("chat-input")
            .track_focus(&self.focus_handle)
            .key_context("ChatInput")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .line_height(line_height)
            .text_size(gpui::px(theme::TEXT_SM))
            .text_color(theme::alpha(theme::FOREGROUND, 1.0))
            .cursor(gpui::CursorStyle::IBeam)
            .child(ChatInputElement { input: cx.entity() })
    }
}
