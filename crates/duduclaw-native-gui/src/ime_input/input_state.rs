// S4 — the gpui `Entity` for an IME-composition-capable text input: owns a
// `TextEngine` (pure logic, see `text_engine.rs`) plus the gpui-specific glue
// a real input needs — focus, `EntityInputHandler` (IME composition),
// keyboard/mouse event handling, and the row layout `element.rs`'s paint pass
// hands back each frame for hit-testing.
//
// (Was `chat_input.rs` / `ChatInputState` until D3-b, 2026-08-23 — renamed
// because `duduclaw-shell` now drives the same widget for its OOBE fields,
// Wi-Fi PSK, lockscreen password and Launcher query. A lockscreen password
// field whose type is spelled `ChatInputState` would be an actively
// misleading name, not merely an untidy one.)
//
// Source/attribution: same as `text_engine.rs`/`element.rs` — adapted from
// zed-industries/zed's `crates/gpui/examples/input.rs` (Apache-2.0, pinned
// rev `7a7c3e1d2f03195c5fa19bc890da330ad7f3abef`), extended for multi-line
// (Shift+Enter) and a submit-on-Enter event instead of the example's
// standalone demo app shell.
//
// ── Who inserts typed text (D3-b — behaviour change, read this) ───────────
// Printable characters are inserted by the PLATFORM through
// `EntityInputHandler::replace_text_in_range`, NEVER by this file's
// `on_key_down`. That is not a stylistic preference; the pre-D3-b code
// (which appended `keystroke.key_char` itself) double-inserted every
// character the moment an input handler was installed, on BOTH targets:
//
//   - macOS (`gpui_macos/src/window.rs`, pinned rev): a key that our
//     callback leaves un-consumed (`propagate == true`, which a plain
//     `on_key_down` listener always does) falls through to
//     `[inputContext handleEvent:]` → `insertText:` → `replace_text_in_
//     range`. So: once from `key_char`, once from AppKit.
//   - Wayland (`gpui_linux/src/linux/wayland/window.rs::handle_input`): after
//     the callback, `if !result.propagate { return }` — otherwise a keystroke
//     whose modifiers are a subset of Shift and which carries a `key_char` is
//     handed to `input_handler.replace_text_in_range(None, key_char)`. Same
//     double insert.
//
// Removing the `key_char` append is therefore the FIX, not a regression, and
// it leaves the no-IME environment behaving identically from the operator's
// side (a keystroke still appends exactly one character — via the platform's
// own fallback path). Two supporting facts, both verified against the pinned
// rev rather than assumed:
//   - gpui's Linux keystroke builder excludes control characters and DEL
//     from `key_char` (`gpui_linux/src/linux/platform.rs:1111`), so
//     backspace/enter/tab/arrows can never reach that fallback and be
//     inserted as control bytes;
//   - `on_key_down` deliberately does NOT call `cx.stop_propagation()` for
//     the keys it does handle. Consumers layer their own root-level key
//     listeners on top (`duduclaw-shell`'s idle-auto-lock clock is refreshed
//     by one), and swallowing the event here would silently break them.
//
// Key handling deliberately follows this crate's OWN established
// convention (`text_field.rs`'s plain `on_key_down`, not the zed example's
// `actions!`/`KeyBinding` global-registration system) — no new global
// keybindings need to be wired into `main.rs` for this to work, and it
// keeps the widget self-contained.
//
// Honest scope cuts (documented, not silent):
//   - Up/Down move the cursor to the start of the adjacent row, not a
//     column-preserving vertical caret (needs per-row x-position tracking
//     this pass doesn't add). On a single-line field they degrade to
//     Home/End, which is the standard behaviour there anyway.
//   - No clipboard (Cmd+C/V/X) and no system character palette — `text_
//     field.rs`'s login fields don't have them either; a chat composer
//     wants them eventually, tracked as follow-up, not blocking S4.

use std::ops::Range;

use gpui::{
    App, AppContext, Bounds, Context, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, UTF16Selection, Window,
};

use super::element::{byte_offset_for_point, ImeTextElement, PaintedRow};
use super::style::TextInputStyle;
use super::text_engine::{strip_line_breaks, TextEngine};

/// `DUDUCLAW_IME_TRACE=1` — one line per `EntityInputHandler` callback, so an
/// operator (or a bring-up session on a machine where the candidate window
/// cannot be screenshotted) can tell "the IME never reached the widget" apart
/// from "it reached the widget and the widget mishandled it". Default OFF;
/// read live rather than cached, same shape as `duduclaw-shell`'s own
/// `DUDUCLAW_SHELL_DIAG`.
///
/// A MASKED field never prints its text — only lengths and cluster counts.
/// An unmasked field's text is already on screen, so echoing it under an
/// explicit opt-in adds no exposure; a password's is not, and no flag turns
/// that on.
fn trace_enabled() -> bool {
    std::env::var("DUDUCLAW_IME_TRACE").is_ok_and(|v| v == "1")
}

/// D9-bug7/D9-bug8 (2026-08-24) — see [`ImeTextInput::insert_committed`]'s
/// own doc comment for the full flood-guardrail write-up.
const MAX_SINGLE_LINE_CONTENT_BYTES: usize = 128;
const MAX_MULTI_LINE_CONTENT_BYTES: usize = 8192;

/// Pure predicate behind the cap — same "gpui-free logic lives in a plain
/// function, tested directly" convention `text_engine.rs`'s own module doc
/// establishes for this sibling file, and `duduclaw-comp::session_lock`'s
/// `should_swallow_unbound_locked_key` establishes for this round's other
/// half of the same fix. `insert_committed` cannot be unit-tested directly
/// (it needs a live `ImeTextInput` entity, which needs a real gpui `App`
/// context this crate has no test harness for), so the one bit of decision
/// logic that matters is isolated here instead.
fn exceeds_content_cap(current_len: usize, removed_len: usize, incoming_len: usize, cap: usize) -> bool {
    current_len.saturating_sub(removed_len).saturating_add(incoming_len) > cap
}

#[derive(Debug, Clone)]
pub enum ImeTextInputEvent {
    /// Enter (without Shift) while not mid-composition — carries the
    /// content that was in the box (already cleared from `engine` by the
    /// time this fires; see `on_key_down`). Only emitted when the instance's
    /// [`TextInputStyle::submit_on_enter`] is set.
    Submit(String),
}

pub struct ImeTextInput {
    /// `pub(super)` — `element.rs` (a sibling module under `ime_input`)
    /// reads this directly for `window.handle_input(&focus_handle, ...)`
    /// and the focus-ring/cursor-visibility check. Kept out of the public
    /// API surface past `ime_input`'s own boundary; callers outside this
    /// module go through [`Focusable::focus_handle`] instead.
    pub(super) focus_handle: FocusHandle,
    pub engine: TextEngine,
    pub placeholder: SharedString,
    /// Presentation + behaviour knobs — see [`TextInputStyle`]. `pub(super)`
    /// for `element.rs`; outside callers push updates through
    /// [`Self::set_style`] so a change always lands with a `cx.notify()`.
    pub(super) style: TextInputStyle,
    is_selecting: bool,
    last_bounds: Option<Bounds<Pixels>>,
    last_rows: Vec<PaintedRow>,
}

impl ImeTextInput {
    /// A default-styled (chat composer) instance — multi-line, submits on
    /// Enter, this crate's own theme colors.
    pub fn new(cx: &mut App, placeholder: impl Into<SharedString>) -> Entity<Self> {
        Self::with_style(cx, placeholder, TextInputStyle::default())
    }

    pub fn with_style(
        cx: &mut App,
        placeholder: impl Into<SharedString>,
        style: TextInputStyle,
    ) -> Entity<Self> {
        cx.new(|cx| Self {
            focus_handle: cx.focus_handle(),
            engine: TextEngine::new(),
            placeholder: placeholder.into(),
            style,
            is_selecting: false,
            last_bounds: None,
            last_rows: Vec::new(),
        })
    }

    /// Read-only view of the committed buffer. Preferred over touching
    /// `engine.content` from outside, and the only accessor the shell uses.
    pub fn content(&self) -> &str {
        &self.engine.content
    }

    pub fn is_empty(&self) -> bool {
        self.engine.content.is_empty()
    }

    /// Drop everything typed so far (including any in-flight composition).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.engine.clear();
        cx.notify();
    }

    /// Re-push presentation knobs. Callers whose colors depend on a runtime
    /// theme (`duduclaw-shell`'s light/dark `ShellPalette`) call this once
    /// per render pass; it is a no-op — no `cx.notify()`, so no render loop —
    /// when nothing actually changed.
    pub fn set_style(&mut self, style: TextInputStyle, cx: &mut Context<Self>) {
        if self.style == style {
            return;
        }
        self.style = style;
        cx.notify();
    }

    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        let placeholder = placeholder.into();
        if self.placeholder == placeholder {
            return;
        }
        self.placeholder = placeholder;
        cx.notify();
    }

    /// Called from `element.rs`'s `paint` with this frame's row layout —
    /// the only way mouse hit-testing and IME candidate-window placement
    /// (`bounds_for_range`) can work, since they need to know where text
    /// actually landed on screen, which only the paint pass computes.
    pub(super) fn set_last_layout(&mut self, rows: Vec<PaintedRow>, bounds: Bounds<Pixels>) {
        self.last_rows = rows;
        self.last_bounds = Some(bounds);
    }

    /// One `DUDUCLAW_IME_TRACE=1` line. See [`trace_enabled`] for the
    /// masked-field rule this enforces.
    fn trace(&self, method: &str, text: Option<&str>) {
        if !trace_enabled() {
            return;
        }
        let body = match text {
            Some(t) if !self.style.masked => format!(" text={t:?} bytes={}", t.len()),
            Some(t) => format!(" bytes={} (masked: text withheld)", t.len()),
            None => String::new(),
        };
        let content = if self.style.masked {
            format!("<{} clusters>", self.engine.grapheme_count())
        } else {
            format!("{:?}", self.engine.content)
        };
        eprintln!(
            "[ime] {method}{body} -> marked={:?} content={content} masked={}",
            self.engine.marked_range, self.style.masked
        );
    }

    /// Insert committed text, honouring the single-line contract. Every
    /// buffer-mutating entry point funnels through here so a `\n` cannot
    /// sneak into a one-line field via a paste or an IME commit.
    ///
    /// D9-bug7/D9-bug8 (2026-08-24) flood guardrail: returns `false` (and
    /// changes nothing) when accepting `new_text` would push `content` past
    /// [`MAX_SINGLE_LINE_CONTENT_BYTES`]/[`MAX_MULTI_LINE_CONTENT_BYTES`].
    /// This is NOT the root fix for the runaway-repeat bug this round
    /// root-caused on the compositor side (see `duduclaw-comp::
    /// session_lock`'s own D9-bug7 doc comment for the full write-up) — it
    /// is the backstop for whatever else can still make a client keep
    /// synthesizing an unbounded stream of identical single-character
    /// inserts. Measured directly on the W5-1 VM round: even after comp was
    /// fixed to correctly deliver the release that ends a Cmd-held chord,
    /// gpui's own Linux/Wayland backend kept redelivering the stuck-repeat
    /// keystroke LOCALLY — a client-side mechanism this crate does not own
    /// (vendored dependency) and cannot patch here. No real password,
    /// search query, or Wi-Fi PSK is anywhere near 128 bytes (WPA2's own
    /// PSK ceiling is 63 ASCII characters), and no real
    /// chat message is anywhere near 8 KiB, so this never fires on genuine
    /// input; the caller (`EntityInputHandler::replace_text_in_range`) skips
    /// `cx.notify()` on a refusal, which is what actually stops the redraw
    /// storm — an insert that changes nothing has nothing to repaint.
    fn insert_committed(&mut self, range_utf16: Option<Range<usize>>, new_text: &str) -> bool {
        let sanitized =
            if self.style.multi_line { std::borrow::Cow::Borrowed(new_text) } else { strip_line_breaks(new_text) };
        let cap = if self.style.multi_line { MAX_MULTI_LINE_CONTENT_BYTES } else { MAX_SINGLE_LINE_CONTENT_BYTES };
        // NET growth, not raw length: `removed_len_for` is whatever this
        // exact call is about to replace (the common case — a repeat's
        // synthetic keystroke — has an empty effective range, i.e. removed
        // == 0, a pure append), so a full-selection replace on already-long
        // content is never mistaken for unbounded growth and wrongly
        // refused.
        let removed = self.engine.removed_len_for(range_utf16.clone());
        if exceeds_content_cap(self.engine.content.len(), removed, sanitized.len(), cap) {
            if trace_enabled() {
                eprintln!(
                    "[ime] insert refused — would exceed the {cap}-byte cap (current={}, removed={removed}, incoming={})",
                    self.engine.content.len(),
                    sanitized.len()
                );
            }
            return false;
        }
        self.engine.replace_text_in_range(range_utf16, sanitized.as_ref());
        true
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;

        match ks.key.as_str() {
            "enter" => {
                if ks.modifiers.shift {
                    if self.style.multi_line {
                        self.engine.replace_text_in_range(None, "\n");
                        cx.notify();
                    }
                } else if self.style.submit_on_enter && self.engine.marked_range.is_none() {
                    // Guard against a raw "enter" ever reaching here mid-
                    // composition (see `text_engine.rs`'s module doc
                    // comment: the OS normally consumes Enter itself to
                    // confirm a candidate and this app never sees it as a
                    // key event at all — this is defence-in-depth, not the
                    // primary mechanism).
                    let content = self.engine.take_content();
                    cx.notify();
                    cx.emit(ImeTextInputEvent::Submit(content));
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
        }
        // Everything else — printable characters included — is deliberately
        // NOT handled here. See this file's header comment ("Who inserts
        // typed text"): the platform routes it to
        // `EntityInputHandler::replace_text_in_range` instead, and doing
        // both would insert every character twice.
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
        self.trace("delete_backward", None);
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
    /// vertical caret" scope cut. On a single-line field there is no row to
    /// move to, so Up/Down degrade to Home/End (what every single-line text
    /// field on every platform does).
    fn move_row(&mut self, dir: i8, cx: &mut Context<Self>) {
        if !self.style.multi_line {
            let target = if dir < 0 { 0 } else { self.engine.content.len() };
            self.engine.move_to(target);
            cx.notify();
            return;
        }
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

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Click-to-focus: without this, clicking a field that isn't already
        // focused would move the caret but leave keyboard focus (and
        // therefore the installed input handler) somewhere else entirely.
        window.focus(&self.focus_handle, cx);
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
        byte_offset_for_point(
            point,
            &bounds,
            self.style.line_height,
            &self.last_rows,
            &self.engine,
            self.style.masked,
        )
    }
}

impl EventEmitter<ImeTextInputEvent> for ImeTextInput {}

impl Focusable for ImeTextInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for ImeTextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.engine.range_from_utf16(&range_utf16);
        actual_range.replace(self.engine.range_to_utf16(&range));
        if self.style.masked {
            // A masked field never hands its plaintext to the OS/IME layer —
            // this is the one method an external input-method process (fcitx5
            // and, if an operator ever enabled it, a cloud-backed engine) can
            // use to read surrounding text. See `TextInputStyle::masked`.
            return Some(self.engine.masked_slice(&range));
        }
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
        self.trace("unmark_text", None);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // D9-bug7/D9-bug8: a refused (capped) insert changes nothing, so it
        // gets neither a trace line under normal tracing (the refusal
        // itself already logs, see `insert_committed`) nor a `cx.notify()`
        // — an unbounded flood of these must never turn into an unbounded
        // flood of repaints.
        if !self.insert_committed(range_utf16, new_text) {
            return;
        }
        self.trace("replace_text_in_range", Some(new_text));
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
        let sanitized = if self.style.multi_line {
            std::borrow::Cow::Borrowed(new_text)
        } else {
            strip_line_breaks(new_text)
        };
        self.engine.replace_and_mark_text_in_range(
            range_utf16,
            sanitized.as_ref(),
            new_selected_range_utf16,
        );
        self.trace("replace_and_mark_text_in_range", Some(new_text));
        cx.notify();
    }

    /// Where the IME should park its candidate window.
    ///
    /// gpui asks for a ZERO-WIDTH range at the composition's start
    /// (`gpui::compute_ime_candidate_bounds` uses `marked_range.start`, not
    /// the selection — that is PR#28429's fix for zed#21004, already inside
    /// this crate's pinned rev), so the candidate list stays put instead of
    /// sliding right as the operator types. On a masked field the x
    /// positions are looked up in the BULLET string, so the window sits over
    /// the dots — the only honest place for it.
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.engine.range_from_utf16(&range_utf16);
        let masked = self.style.masked;
        let row_idx = if masked { 0 } else { self.engine.row_for_offset(range.start) };
        let row = self.last_rows.get(row_idx)?;
        let line_height = self.style.line_height;
        let row_top = bounds.top() + line_height * row_idx;
        let (local_start, local_end) = if masked {
            (self.engine.mask_offset(range.start), self.engine.mask_offset(range.end))
        } else {
            (range.start.saturating_sub(row.byte_range.start), range.end.saturating_sub(row.byte_range.start))
        };
        let local_start = local_start.min(row.line.text.len());
        let local_end = local_end.min(row.line.text.len());
        Some(Bounds::from_corners(
            gpui::point(bounds.left() + row.line.x_for_index(local_start), row_top),
            gpui::point(bounds.left() + row.line.x_for_index(local_end), row_top + line_height),
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
            self.style.line_height,
            &self.last_rows,
            &self.engine,
            self.style.masked,
        );
        Some(self.engine.offset_to_utf16(offset))
    }
}

impl Render for ImeTextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::prelude::*;

        gpui::div()
            .id("ime-text-input")
            .track_focus(&self.focus_handle)
            .key_context("ImeTextInput")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .line_height(self.style.line_height)
            .text_size(self.style.text_size)
            .text_color(self.style.text)
            .cursor(gpui::CursorStyle::IBeam)
            .child(ImeTextElement { input: cx.entity() })
    }
}

#[cfg(test)]
mod tests {
    use super::{exceeds_content_cap, MAX_MULTI_LINE_CONTENT_BYTES, MAX_SINGLE_LINE_CONTENT_BYTES};

    // ── D9-bug7/D9-bug8 (2026-08-24): the flood guardrail ──────────────────
    // See `ImeTextInput::insert_committed`'s own doc comment for the full
    // write-up — this is the pure decision that guardrail is built on.

    #[test]
    fn an_insert_that_stays_at_or_under_the_cap_is_never_refused() {
        assert!(!exceeds_content_cap(0, 0, MAX_SINGLE_LINE_CONTENT_BYTES, MAX_SINGLE_LINE_CONTENT_BYTES));
        assert!(!exceeds_content_cap(MAX_SINGLE_LINE_CONTENT_BYTES - 1, 0, 1, MAX_SINGLE_LINE_CONTENT_BYTES));
        assert!(!exceeds_content_cap(0, 0, 0, MAX_SINGLE_LINE_CONTENT_BYTES));
    }

    #[test]
    fn an_insert_that_would_push_one_byte_past_the_cap_is_refused() {
        assert!(exceeds_content_cap(MAX_SINGLE_LINE_CONTENT_BYTES, 0, 1, MAX_SINGLE_LINE_CONTENT_BYTES));
        assert!(exceeds_content_cap(MAX_SINGLE_LINE_CONTENT_BYTES - 1, 0, 2, MAX_SINGLE_LINE_CONTENT_BYTES));
    }

    /// The exact shape a stuck client-side key repeat produces: content
    /// already near/at the cap, one more single-grapheme insert arriving.
    /// This is what actually bounds an otherwise-unbounded flood — it must
    /// start refusing and then STAY refused for every subsequent call, not
    /// just the one that first crosses the line.
    #[test]
    fn a_runaway_single_character_flood_is_capped_and_stays_capped() {
        let mut len = 0usize;
        let mut refusals = 0u32;
        for _ in 0..(MAX_SINGLE_LINE_CONTENT_BYTES + 2_000) {
            // A repeat's synthetic keystroke always lands at the cursor
            // with an empty selection — removed == 0, a pure append.
            if exceeds_content_cap(len, 0, 1, MAX_SINGLE_LINE_CONTENT_BYTES) {
                refusals += 1;
            } else {
                len += 1;
            }
        }
        assert_eq!(len, MAX_SINGLE_LINE_CONTENT_BYTES, "growth must stop exactly at the cap, never past it");
        assert_eq!(refusals, 2_000, "every call past the cap must be refused, not just the first");
    }

    #[test]
    fn the_multi_line_cap_is_far_more_generous_than_the_single_line_one() {
        // A real chat message can legitimately run long; a password/search/
        // PSK field cannot — the two caps must not accidentally converge.
        assert!(MAX_MULTI_LINE_CONTENT_BYTES > MAX_SINGLE_LINE_CONTENT_BYTES * 4);
    }

    #[test]
    fn a_shrinking_or_steady_replace_is_never_refused_regardless_of_current_length() {
        // An empty incoming string, replacing a range at least as long as
        // itself, must never be blocked by the cap no matter how long the
        // existing content is — the cap only ever refuses NET GROWTH.
        let huge = MAX_MULTI_LINE_CONTENT_BYTES * 2;
        assert!(!exceeds_content_cap(huge, huge, 0, MAX_SINGLE_LINE_CONTENT_BYTES));
    }

    /// The exact scenario that motivated tracking `removed_len` at all: a
    /// full-selection replace on content already sitting AT the cap must
    /// not be refused just because the OLD length alone already meets it —
    /// the replacement's own net result ("hi", 2 bytes) is what matters.
    #[test]
    fn a_full_selection_replace_on_at_cap_content_is_judged_by_its_net_result() {
        assert!(!exceeds_content_cap(
            MAX_SINGLE_LINE_CONTENT_BYTES,
            MAX_SINGLE_LINE_CONTENT_BYTES,
            2,
            MAX_SINGLE_LINE_CONTENT_BYTES
        ));
    }
}
