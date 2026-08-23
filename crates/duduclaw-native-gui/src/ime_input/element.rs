// S4 — multi-line paint/layout `Element` for `ImeTextInput`.
//
// Adapted from the SAME source as `text_engine.rs` (zed-industries/zed
// `crates/gpui/examples/input.rs`'s `TextElement`/`PrepaintState`, Apache-2.0,
// pinned rev `7a7c3e1d2f03195c5fa19bc890da330ad7f3abef`) — see that file's
// module doc comment for the full attribution and the "why not gpui-
// component" rationale. The original shapes ONE line; this extends the same
// per-line shape/fill/paint recipe to N rows split on `\n`, since a chat
// composer needs Shift+Enter multi-line input.
//
// D3-b (2026-08-23) generalized it twice over, for `duduclaw-shell`'s sake:
//   - every color/metric now comes from the instance's `TextInputStyle`
//     instead of this crate's fixed `theme::` constants, because the shell
//     re-resolves them from a runtime light/dark `ShellPalette`;
//   - a masked (password) field shapes one `MASK_CHAR` per grapheme cluster
//     instead of the real text, which puts caret/selection/preedit-underline
//     positioning into the MASKED string's coordinate system. All three of
//     those conversions live in `text_engine.rs` (`mask_offset` /
//     `content_offset_from_mask` / `masked_display`, unit-tested there);
//     this file only calls them, so the arithmetic is never re-derived by
//     eye at a paint site.
//
// Honest scope cuts (S4, not oversights):
//   - No text wrapping — a row is exactly one `\n`-delimited line, shaped at
//     its natural (unwrapped) width. A very long single line simply
//     overflows/clips rather than soft-wrapping; `screens/chat.rs` caps the
//     composer's visible height and lets the container clip past that,
//     rather than implementing scroll-within-input.
//   - No cursor blink — the caret is always solid while focused (`mds_gpui/
//     skeleton.rs` already precedents "static placeholder is fine, animation
//     is not required" for this crate's scope; the same call applies here).

use std::ops::Range;

use gpui::{
    fill, point, prelude::*, px, size, App, Bounds, Element, ElementId, ElementInputHandler,
    Entity, Font, GlobalElementId, Hsla, InspectorElementId, LayoutId, PaintQuad, Pixels, Point,
    SharedString, ShapedLine, Style, TextRun, UnderlineStyle, Window,
};

use super::input_state::ImeTextInput;
use super::text_engine::TextEngine;

/// One shaped row, plus the byte range of `engine.content` it covers
/// (EXCLUDING the `\n` that terminates it, if any) — the coordinate system
/// `bounds_for_range`/`character_index_for_point`/mouse hit-testing convert
/// through. Populated by `prepaint`, handed back to the entity in `paint`
/// (same pattern as the original example's `input.last_layout`).
///
/// D3-b: `byte_range` stays in CONTENT coordinates even for a masked field
/// (where the shaped `line` holds bullets, not the content) — callers convert
/// with `TextEngine::mask_offset` / `::content_offset_from_mask` rather than
/// this struct carrying two parallel range systems that could drift apart.
pub struct PaintedRow {
    pub byte_range: Range<usize>,
    pub line: ShapedLine,
}

pub struct ImeTextElement {
    pub input: Entity<ImeTextInput>,
}

pub struct PrepaintState {
    rows: Vec<PaintedRow>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for ImeTextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

/// A row's byte offset in the coordinate system its `ShapedLine` was shaped
/// in: identity (minus the row's own start) for a normal field, the
/// bullet-string offset for a masked one.
fn display_offset(engine: &TextEngine, masked: bool, row_start: usize, content_offset: usize) -> usize {
    if masked {
        engine.mask_offset(content_offset)
    } else {
        content_offset.saturating_sub(row_start)
    }
}

impl Element for ImeTextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        // A masked or single-line field is exactly one row by construction
        // (`strip_line_breaks` keeps `\n` out of the buffer), so its height
        // never depends on content — asking `rows()` would give the same 1
        // but reads as if it might not.
        let row_count =
            if input.style.masked || !input.style.multi_line { 1 } else { input.engine.rows().len().max(1) };
        let line_height = input.style.line_height;
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = (line_height * row_count).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let engine = &input.engine;
        let text_input_style = input.style;
        let masked = text_input_style.masked;
        let is_empty = engine.content.is_empty();
        let font = window.text_style().font();
        let font_size = text_input_style.text_size;
        let line_height = text_input_style.line_height;
        let cursor = engine.cursor_offset();
        let cursor_row = if masked { 0 } else { engine.row_for_offset(cursor) };
        let selected_range = engine.selected_range.clone();
        let marked_range = engine.marked_range.clone();

        // `vec![0..0]` / `vec![0..len]` here are deliberately one-element
        // `Vec<Range<usize>>`s (a single row), NOT an attempt to collect a
        // range's elements — which is what clippy's `single_range_in_vec_init`
        // lint assumes and suggests "fixing".
        #[allow(clippy::single_range_in_vec_init)]
        let row_ranges: Vec<Range<usize>> = if is_empty {
            vec![0..0]
        } else if masked {
            // Masked fields never hold a `\n`, so the whole buffer is row 0 —
            // and every offset conversion below can assume `row_start == 0`.
            vec![0..engine.content.len()]
        } else {
            engine.rows()
        };
        let mut rows = Vec::with_capacity(row_ranges.len());
        let mut cursor_quad = None;
        let mut selection_quads = Vec::new();

        for (row_idx, row_range) in row_ranges.iter().enumerate() {
            let row_text: SharedString = if is_empty {
                input.placeholder.clone()
            } else if masked {
                engine.masked_display().into()
            } else {
                engine.content[row_range.clone()].to_string().into()
            };
            let text_color: Hsla =
                if is_empty { text_input_style.placeholder } else { text_input_style.text };

            // The preedit underline is expressed in the SHAPED string's
            // coordinates, so a masked field underlines the bullets standing
            // in for the composing text — the operator still sees "this part
            // isn't committed yet" without any plaintext being painted.
            let marked_display: Option<Range<usize>> = marked_range.as_ref().map(|m| {
                display_offset(engine, masked, row_range.start, m.start)
                    ..display_offset(engine, masked, row_range.start, m.end)
            });
            let runs = build_runs(&row_text, text_color, font.clone(), marked_display.as_ref());
            let line = window.text_system().shape_line(row_text, font_size, &runs, None);

            let row_top = bounds.top() + line_height * row_idx;

            if selected_range.is_empty() && row_idx == cursor_row {
                let local = display_offset(engine, masked, row_range.start, cursor).min(line.text.len());
                let x = line.x_for_index(local);
                cursor_quad = Some(fill(
                    Bounds::new(point(bounds.left() + x, row_top), size(px(2.), line_height)),
                    text_input_style.cursor,
                ));
            }

            // Selection: the portion of `selected_range` that intersects
            // this row, if any (a drag-selection spanning multiple rows
            // paints one quad per row it touches).
            if !selected_range.is_empty() {
                let sel_start = selected_range.start.max(row_range.start);
                let sel_end = selected_range.end.min(row_range.end);
                if sel_start < sel_end {
                    let local_start =
                        display_offset(engine, masked, row_range.start, sel_start).min(line.text.len());
                    let local_end =
                        display_offset(engine, masked, row_range.start, sel_end).min(line.text.len());
                    selection_quads.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + line.x_for_index(local_start), row_top),
                            point(bounds.left() + line.x_for_index(local_end), row_top + line_height),
                        ),
                        text_input_style.selection,
                    ));
                }
            }

            rows.push(PaintedRow { byte_range: row_range.clone(), line });
        }

        PrepaintState { rows, cursor: cursor_quad, selection: selection_quads }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        // The ONE call that makes OS-level IME composition reach this widget
        // at all: without it every `CommitString`/`PreeditString` the
        // compositor delivers is silently dropped (gpui's Wayland
        // `handle_ime` early-returns when no input handler is installed), and
        // on the shell that reads as "English types fine, Chinese does
        // nothing". `Window::handle_input` no-ops unless `focus_handle` is
        // the focused one, so N painted fields still install exactly one
        // handler.
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);

        for quad in prepaint.selection.drain(..) {
            window.paint_quad(quad);
        }

        let line_height = self.input.read(cx).style.line_height;
        for (row_idx, row) in prepaint.rows.iter().enumerate() {
            let row_top = bounds.top() + line_height * row_idx;
            if let Err(e) =
                row.line.paint(point(bounds.left(), row_top), line_height, gpui::TextAlign::Left, None, window, cx)
            {
                // A shaped-line paint failure is a rendering glitch, not a
                // reason to crash the whole app — log and keep painting the
                // remaining rows (matches this crate's "never panic on a
                // rendering/wire-adjacent code path" convention). Deliberately
                // logs only the error, never the row's text: on a masked
                // field that text would be the password.
                eprintln!("[ime_input] row paint failed: {e}");
            }
        }

        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }

        let rows = std::mem::take(&mut prepaint.rows);
        self.input.update(cx, |input, _cx| {
            input.set_last_layout(rows, bounds);
        });
    }
}

/// Build the `TextRun` list for one row — plain single run, unless the
/// active IME composition (`marked_display`, already converted into THIS
/// row's shaped-string coordinates by the caller) intersects it, in which
/// case the intersecting slice gets an underline (the standard "this text is
/// still being composed" affordance every IME-aware editor shows).
fn build_runs(
    row_text: &SharedString,
    color: Hsla,
    font: Font,
    marked_display: Option<&Range<usize>>,
) -> Vec<TextRun> {
    let base = TextRun {
        len: row_text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let Some(marked) = marked_display else {
        return vec![base];
    };
    let local_start = marked.start.min(row_text.len());
    let local_end = marked.end.min(row_text.len());
    if local_start >= local_end {
        return vec![base];
    }
    vec![
        TextRun { len: local_start, ..base.clone() },
        TextRun {
            len: local_end - local_start,
            underline: Some(UnderlineStyle { color: Some(base.color), thickness: px(1.0), wavy: false }),
            ..base.clone()
        },
        TextRun { len: row_text.len() - local_end, ..base },
    ]
    .into_iter()
    .filter(|r| r.len > 0)
    .collect()
}

/// Hit-test a mouse position against the last-painted row layout — returns
/// a byte offset into `engine.content`. Used by `ImeTextInput`'s mouse
/// handlers (click-to-position, drag-to-select) and by
/// `character_index_for_point`.
///
/// On a masked field the shaped line holds bullets, so `closest_index_for_x`
/// answers in bullet coordinates and has to be converted back
/// (`content_offset_from_mask`) before it means anything to the buffer.
pub fn byte_offset_for_point(
    point: Point<Pixels>,
    bounds: &Bounds<Pixels>,
    line_height: Pixels,
    rows: &[PaintedRow],
    engine: &TextEngine,
    masked: bool,
) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let row_idx = if point.y < bounds.top() {
        0
    } else {
        (((point.y - bounds.top()) / line_height).floor() as usize).min(rows.len() - 1)
    };
    let row = &rows[row_idx];
    let local_x = point.x - bounds.left();
    let local = row.line.closest_index_for_x(local_x);
    if masked {
        engine.content_offset_from_mask(local)
    } else {
        (row.byte_range.start + local).min(engine.content.len())
    }
}
