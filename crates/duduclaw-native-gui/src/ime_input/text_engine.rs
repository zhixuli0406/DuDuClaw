// S4 — pure (gpui-free) text-editing + IME-composition engine.
//
// Adapted from `zed-industries/zed`'s own `crates/gpui/examples/input.rs`
// (Apache-2.0; pinned rev `7a7c3e1d2f03195c5fa19bc890da330ad7f3abef` — the
// EXACT commit this crate's `gpui` dependency resolves to, verified against
// `Cargo.lock`, not merely "some recent-ish version"). See `ime_input/
// mod.rs`'s module doc comment for why this is the source instead of
// `gpui-component`'s `crates/base/src/input/base/state.rs` (the file the S3
// D4 spike had flagged as the eventual vendor target).
//
// This is a faithful port of `TextInput`'s offset/range/composition methods
// (`offset_from_utf16` / `offset_to_utf16` / `range_to_utf16` /
// `range_from_utf16` / `replace_text_in_range` /
// `replace_and_mark_text_in_range` / grapheme boundary walking), pulled out
// of the original's gpui `Entity` struct into a bare `TextEngine` with NO
// gpui types at all (no `FocusHandle`, no `Context`, no `cx.notify()`) —
// purely so it's unit-testable without a live `App`/`Window` (gpui's own
// entity/window machinery needs a running application to construct even a
// `FocusHandle`). `ime_input/input_state.rs`'s `EntityInputHandler` impl is
// thin glue that delegates to these methods and adds `cx.notify()`.
//
// One deliberate behavioral difference from the original: `content` may
// contain `\n` (multi-line chat composer, Shift+Enter). Every method below
// is byte/range arithmetic over a flat `String` and works identically
// whether or not it contains newlines — multi-line-ness is purely a layout
// concern, handled in `element.rs`.

use std::borrow::Cow;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::style::MASK_CHAR;

#[derive(Debug, Default)]
pub struct TextEngine {
    pub content: String,
    /// Byte range into `content`. Empty (`start == end`) means "just a
    /// cursor, no selection".
    pub selected_range: Range<usize>,
    pub selection_reversed: bool,
    /// The IME composition (marked/underlined) span, if a composition is
    /// currently in progress. `None` outside of active IME input.
    pub marked_range: Option<Range<usize>>,
}

impl TextEngine {
    pub fn new() -> Self {
        Self { content: String::new(), selected_range: 0..0, selection_reversed: false, marked_range: None }
    }

    pub fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    pub fn move_to(&mut self, offset: usize) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
    }

    pub fn select_to(&mut self, offset: usize) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
    }

    pub fn select_all(&mut self) {
        self.move_to(0);
        self.select_to(self.content.len());
    }

    /// Grapheme-cluster-aware boundary walking (not raw byte/char stepping)
    /// — a combining-mark sequence or a multi-codepoint emoji moves the
    /// cursor as one unit, matching every real text editor's behavior.
    pub fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    pub fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    // ── UTF-8 byte offset ⇄ UTF-16 code-unit offset ──────────────────────
    // gpui's `EntityInputHandler` protocol speaks UTF-16 offsets (the OS
    // IME's native unit on both macOS/Cocoa and Windows) while `content` is
    // a UTF-8 `String` — every CJK character above the BMP boundary (rare)
    // or any character taking >1 UTF-16 code unit needs this conversion, and
    // getting it wrong is exactly the class of bug that makes CJK IME
    // composition silently corrupt text. Ported verbatim from the example.

    pub fn offset_from_utf16(&self, offset_utf16: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset_utf16 {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    /// The range actually edited by a `replace_*` call when the caller
    /// passes `None` for `range_utf16` — falls back to the active
    /// composition span, then to the current selection. Shared by both
    /// `replace_text_in_range` and `replace_and_mark_text_in_range` (same
    /// precedence in the original).
    fn effective_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone())
    }

    /// The BYTE length [`Self::effective_range`] would remove for this
    /// `range_utf16` — i.e. what a `replace_*` call is actually replacing,
    /// before the new text goes in. Exposed so a caller can compute NET
    /// growth (`content.len() - removed + incoming.len()`) rather than
    /// treating every insert as pure append — D9-bug7/D9-bug8's flood
    /// guardrail (`input_state.rs`'s `insert_committed`) needs this so a
    /// full-selection replace on already-long content is never mistaken for
    /// unbounded growth and wrongly refused.
    pub fn removed_len_for(&self, range_utf16: Option<Range<usize>>) -> usize {
        self.effective_range(range_utf16).len()
    }

    /// Final commit of an edit (either a plain keystroke insert or an IME
    /// composition's final commit) — replaces `range` with `new_text`,
    /// clears any composition, and parks the cursor right after the
    /// inserted text.
    pub fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str) {
        let range = self.effective_range(range_utf16);
        self.content = self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range = None;
    }

    /// In-progress IME composition update — replaces `range` with
    /// `new_text` and marks it (underlined, not yet committed). Called
    /// repeatedly by the OS as the user types into a candidate window
    /// (e.g. pinyin "n" → "ni" → "nih"), each call replacing the PREVIOUS
    /// marked span. An empty `new_text` clears the marked range instead of
    /// leaving a zero-length composition around.
    pub fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
    ) {
        let range = self.effective_range(range_utf16);
        self.content = self.content[0..range.start].to_owned() + new_text + &self.content[range.end..];
        self.marked_range =
            if new_text.is_empty() { None } else { Some(range.start..range.start + new_text.len()) };
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
    }

    pub fn unmark_text(&mut self) {
        self.marked_range = None;
    }

    // ── Multi-line row splitting (S4 — the original single-line example
    // has no equivalent) ──────────────────────────────────────────────────
    // Pure byte-range arithmetic; `element.rs`'s paint/layout and mouse
    // hit-testing are the only consumers. A "row" is one `\n`-delimited
    // line; the newline byte itself belongs to neither row on either side.

    /// Row boundaries as byte ranges into `content`. Always returns at
    /// least one row (an empty one for empty content), so callers never
    /// need to special-case "no rows yet".
    pub fn rows(&self) -> Vec<Range<usize>> {
        let mut rows = Vec::new();
        let mut start = 0;
        for (i, b) in self.content.bytes().enumerate() {
            if b == b'\n' {
                rows.push(start..i);
                start = i + 1;
            }
        }
        rows.push(start..self.content.len());
        rows
    }

    /// Which row index (into [`Self::rows`]) contains byte offset `offset`.
    /// An offset that lands exactly on a row's end boundary belongs to
    /// THAT row (not the next one) — e.g. the cursor sitting right before a
    /// `\n` reads as "end of the line above", matching every text editor's
    /// convention for where Home/End/click-at-EOL parks the caret.
    pub fn row_for_offset(&self, offset: usize) -> usize {
        let rows = self.rows();
        for (i, r) in rows.iter().enumerate() {
            if offset <= r.end || i == rows.len() - 1 {
                return i;
            }
        }
        rows.len().saturating_sub(1)
    }

    /// Take the full content and reset to empty — used on submit (Enter).
    pub fn take_content(&mut self) -> String {
        let taken = std::mem::take(&mut self.content);
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        taken
    }

    /// Clear without handing the content back — the shell's "cancel and
    /// start over" affordances (`OobeTextField::clear`'s original job:
    /// dropping a half-typed Wi-Fi PSK, wiping a password after an unlock
    /// attempt). Distinct from [`Self::take_content`] on purpose: a caller
    /// that must not hold a password string even briefly has no reason to
    /// receive one.
    pub fn clear(&mut self) {
        self.content.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
    }

    // ── Masked (password) display arithmetic (D3-b) ───────────────────────
    // A masked field paints one [`MASK_CHAR`] per GRAPHEME CLUSTER, so every
    // screen-space question ("where does the caret go", "which character did
    // the mouse land on", "where should the IME candidate window sit") has to
    // be asked in the masked string's coordinate system, not the content's.
    // These three functions are the whole conversion; `element.rs` and the
    // `EntityInputHandler` impl do nothing cleverer than call them.
    //
    // Grapheme clusters (not chars, not bytes) are the unit because that is
    // what the operator perceives as "one dot": a combining-mark sequence or
    // a multi-codepoint emoji must not smear into three bullets.

    pub fn grapheme_count(&self) -> usize {
        self.content.graphemes(true).count()
    }

    /// The string a masked field actually shapes and paints.
    pub fn masked_display(&self) -> String {
        MASK_CHAR.to_string().repeat(self.grapheme_count())
    }

    /// Content byte offset → masked-string byte offset.
    ///
    /// Counts the grapheme clusters that END at or before `offset`, so an
    /// offset landing mid-cluster (which an IME can produce while composing,
    /// and which `previous_boundary`'s callers can also hand in) rounds DOWN
    /// to that cluster's own start rather than claiming a bullet that isn't
    /// finished yet.
    pub fn mask_offset(&self, offset: usize) -> usize {
        let clusters = self.content.grapheme_indices(true).filter(|(i, g)| i + g.len() <= offset).count();
        clusters * MASK_CHAR.len_utf8()
    }

    /// The bullets standing in for the graphemes inside `range` — what a
    /// masked field answers `EntityInputHandler::text_for_range` with, so an
    /// external input-method process asking for surrounding context never
    /// receives a plaintext password.
    pub fn masked_slice(&self, range: &Range<usize>) -> String {
        let clusters =
            self.content.grapheme_indices(true).filter(|(i, _)| *i >= range.start && *i < range.end).count();
        MASK_CHAR.to_string().repeat(clusters)
    }

    /// Masked-string byte offset → content byte offset (the inverse of
    /// [`Self::mask_offset`], used for mouse hit-testing on a masked field).
    /// An offset past the end saturates to `content.len()` rather than
    /// panicking.
    pub fn content_offset_from_mask(&self, mask_offset: usize) -> usize {
        let nth = mask_offset / MASK_CHAR.len_utf8();
        self.content.grapheme_indices(true).nth(nth).map(|(i, _)| i).unwrap_or(self.content.len())
    }
}

/// Strip every line separator from text destined for a single-line field.
///
/// Single-line-ness cannot be enforced at the key-handling layer alone: on a
/// masked/one-line field the newline can arrive through
/// `replace_text_in_range` (an IME commit, a paste, the Wayland `key_char`
/// fallback) without any keystroke this widget ever sees. Stripping at the
/// buffer boundary is the only place that covers all of them.
///
/// Returns `Cow::Borrowed` when there is nothing to strip, so the common
/// path allocates nothing.
pub fn strip_line_breaks(text: &str) -> Cow<'_, str> {
    if text.contains('\n') || text.contains('\r') {
        Cow::Owned(text.chars().filter(|c| *c != '\n' && *c != '\r').collect())
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_insert() {
        let mut e = TextEngine::new();
        e.replace_text_in_range(None, "hi");
        assert_eq!(e.content, "hi");
        assert_eq!(e.selected_range, 2..2);
        assert_eq!(e.marked_range, None);
    }

    #[test]
    fn shift_enter_inserts_newline_multiline_content() {
        let mut e = TextEngine::new();
        e.replace_text_in_range(None, "line one");
        e.replace_text_in_range(None, "\n");
        e.replace_text_in_range(None, "line two");
        assert_eq!(e.content, "line one\nline two");
    }

    #[test]
    fn backspace_via_select_then_replace_empty() {
        let mut e = TextEngine::new();
        e.replace_text_in_range(None, "abc");
        let prev = e.previous_boundary(e.cursor_offset());
        e.select_to(prev);
        e.replace_text_in_range(None, "");
        assert_eq!(e.content, "ab");
        assert_eq!(e.selected_range, 2..2);
    }

    /// Pinyin composition: IME builds up "n" -> "ni" -> commits "你" — the
    /// canonical multi-step marked-text sequence gpui-component's own tests
    /// exercised for the file this module replaces (see `mod.rs`'s doc
    /// comment on why this crate re-derives rather than vendors them).
    #[test]
    fn pinyin_composition_then_commit() {
        let mut e = TextEngine::new();

        // Step 1: IME marks "n".
        e.replace_and_mark_text_in_range(None, "n", None);
        assert_eq!(e.content, "n");
        assert_eq!(e.marked_range, Some(0..1));
        assert_eq!(e.selected_range, 1..1);

        // Step 2: IME extends the composition to "ni" — replaces the
        // PREVIOUS marked span (given back in UTF-16 units, as the real OS
        // IME would).
        let marked_utf16 = e.range_to_utf16(&e.marked_range.clone().unwrap());
        e.replace_and_mark_text_in_range(Some(marked_utf16), "ni", None);
        assert_eq!(e.content, "ni");
        assert_eq!(e.marked_range, Some(0..2));

        // Step 3: user picks a candidate — IME commits the final glyph and
        // the composition ends.
        let marked_utf16 = e.range_to_utf16(&e.marked_range.clone().unwrap());
        e.replace_text_in_range(Some(marked_utf16), "你");
        assert_eq!(e.content, "你");
        assert_eq!(e.marked_range, None);
        // "你" is 3 UTF-8 bytes but exactly 1 UTF-16 code unit (BMP) — the
        // cursor lands after the character either way; this assertion is
        // the one that would silently break if `replace_text_in_range` used
        // `new_text.chars().count()` (1) or a raw UTF-16 length (1) instead
        // of `new_text.len()` (3, the correct UTF-8 byte length for a
        // BYTE-indexed `selected_range`).
        assert_eq!(e.selected_range, 3..3);
    }

    /// Japanese hiragana composition: single-step marked text (typical for
    /// a romaji->kana IME converting one syllable at a time), then unmark
    /// without a further commit (the user just moves on — e.g. presses an
    /// arrow key, which most IMEs treat as "accept as-is").
    #[test]
    fn hiragana_composition_then_unmark() {
        let mut e = TextEngine::new();
        e.replace_and_mark_text_in_range(None, "あ", None);
        assert_eq!(e.content, "あ");
        // "あ" is 3 UTF-8 bytes, 1 UTF-16 code unit — the marked range in
        // UTF-16 space must be 1 unit wide even though the byte range is 3.
        assert_eq!(e.range_to_utf16(&e.marked_range.clone().unwrap()), 0..1);
        assert_eq!(e.marked_range, Some(0..3));

        e.unmark_text();
        assert_eq!(e.marked_range, None);
        // Unmarking never touches content/selection — only the composition
        // flag clears.
        assert_eq!(e.content, "あ");
    }

    /// A composition replacing a NON-empty existing selection (e.g. the
    /// user selected a character, then started composing a replacement) —
    /// exercises `effective_range`'s selection fallback branch, not just
    /// the always-empty-selection insert path every other test uses.
    #[test]
    fn composition_replaces_existing_selection() {
        let mut e = TextEngine::new();
        e.replace_text_in_range(None, "abc");
        e.selected_range = 1..2; // select "b"
        e.selection_reversed = false;

        e.replace_and_mark_text_in_range(None, "x", None);
        assert_eq!(e.content, "axc");
        assert_eq!(e.marked_range, Some(1..2));
    }

    #[test]
    fn empty_marked_text_clears_composition_instead_of_leaving_zero_width_span() {
        let mut e = TextEngine::new();
        e.replace_and_mark_text_in_range(None, "n", None);
        assert!(e.marked_range.is_some());
        e.replace_and_mark_text_in_range(Some(e.range_to_utf16(&e.marked_range.clone().unwrap())), "", None);
        assert_eq!(e.content, "");
        assert_eq!(e.marked_range, None);
    }

    #[test]
    fn offset_utf16_roundtrip_through_cjk_and_ascii_mix() {
        let mut e = TextEngine::new();
        e.content = "a你b".to_string(); // 1 + 3 + 1 = 5 bytes; utf16: 1+1+1 = 3 units
        for utf8_offset in [0, 1, 4, 5] {
            let utf16 = e.offset_to_utf16(utf8_offset);
            assert_eq!(e.offset_from_utf16(utf16), utf8_offset, "roundtrip failed at byte {utf8_offset}");
        }
    }

    #[test]
    fn rows_splits_on_newline_excluding_the_separator() {
        let mut e = TextEngine::new();
        e.content = "ab\ncd\n".to_string();
        assert_eq!(e.rows(), vec![0..2, 3..5, 6..6]);
    }

    #[test]
    fn rows_of_empty_content_is_one_empty_row() {
        let e = TextEngine::new();
        assert_eq!(e.rows(), vec![0..0]);
    }

    #[test]
    fn row_for_offset_at_boundary_belongs_to_the_row_above() {
        let mut e = TextEngine::new();
        e.content = "ab\ncd".to_string(); // rows: 0..2, 3..5
        assert_eq!(e.row_for_offset(0), 0);
        assert_eq!(e.row_for_offset(2), 0, "right before the \\n stays on row 0");
        assert_eq!(e.row_for_offset(3), 1, "right after the \\n moves to row 1");
        assert_eq!(e.row_for_offset(5), 1);
    }

    #[test]
    fn take_content_resets_everything() {
        let mut e = TextEngine::new();
        e.replace_text_in_range(None, "draft");
        let taken = e.take_content();
        assert_eq!(taken, "draft");
        assert_eq!(e.content, "");
        assert_eq!(e.selected_range, 0..0);
        assert_eq!(e.marked_range, None);
    }

    #[test]
    fn select_all_then_replace_clears_content() {
        let mut e = TextEngine::new();
        e.replace_text_in_range(None, "hello world");
        e.select_all();
        assert_eq!(e.selected_range, 0..11);
        e.replace_text_in_range(None, "");
        assert_eq!(e.content, "");
    }

    // ── D3-b: masked-field arithmetic ────────────────────────────────────

    #[test]
    fn masked_display_paints_one_bullet_per_grapheme_not_per_byte() {
        let mut e = TextEngine::new();
        // 3 graphemes, 7 UTF-8 bytes ("你" and "好" are 3 bytes each).
        e.content = "a你好".to_string();
        assert_eq!(e.grapheme_count(), 3);
        assert_eq!(e.masked_display(), "•••");
        assert_eq!(e.masked_display().len(), 9, "3 bullets x 3 UTF-8 bytes");
    }

    #[test]
    fn masked_display_of_a_combining_mark_cluster_is_one_bullet() {
        let mut e = TextEngine::new();
        e.content = "e\u{0301}".to_string(); // "é" as base + combining acute
        assert_eq!(e.grapheme_count(), 1);
        assert_eq!(e.masked_display(), "•");
    }

    #[test]
    fn mask_offset_maps_content_bytes_onto_bullet_bytes() {
        let mut e = TextEngine::new();
        e.content = "a你好".to_string(); // byte boundaries: 0, 1, 4, 7
        assert_eq!(e.mask_offset(0), 0);
        assert_eq!(e.mask_offset(1), 3, "after 'a' -> after 1 bullet");
        assert_eq!(e.mask_offset(4), 6, "after '你' -> after 2 bullets");
        assert_eq!(e.mask_offset(7), 9, "end of content -> after 3 bullets");
    }

    /// The caret can sit mid-cluster while an IME composes; that must map to
    /// the boundary BEFORE it, never to a fractional bullet.
    #[test]
    fn mask_offset_of_a_mid_cluster_byte_rounds_down_to_the_cluster_start() {
        let mut e = TextEngine::new();
        e.content = "你".to_string(); // one 3-byte cluster
        assert_eq!(e.mask_offset(1), 0);
        assert_eq!(e.mask_offset(2), 0);
        assert_eq!(e.mask_offset(3), 3);
    }

    #[test]
    fn content_offset_from_mask_is_the_inverse_of_mask_offset() {
        let mut e = TextEngine::new();
        e.content = "a你好".to_string();
        for content_offset in [0usize, 1, 4, 7] {
            let masked = e.mask_offset(content_offset);
            assert_eq!(
                e.content_offset_from_mask(masked),
                content_offset,
                "roundtrip failed at content byte {content_offset}"
            );
        }
    }

    #[test]
    fn content_offset_from_mask_saturates_past_the_end_instead_of_panicking() {
        let mut e = TextEngine::new();
        e.content = "ab".to_string();
        assert_eq!(e.content_offset_from_mask(999), 2);
    }

    #[test]
    fn masked_slice_answers_in_bullets_never_in_plaintext() {
        let mut e = TextEngine::new();
        e.content = "pa你ss".to_string(); // graphemes at bytes 0,1,2,5,6
        assert_eq!(e.masked_slice(&(0..e.content.len())), "•••••");
        assert_eq!(e.masked_slice(&(2..5)), "•", "just the CJK cluster");
        assert_eq!(e.masked_slice(&(0..0)), "");
        // The one property that actually matters: no source character ever
        // survives into the answer.
        assert!(e.masked_slice(&(0..e.content.len())).chars().all(|c| c == MASK_CHAR));
    }

    #[test]
    fn empty_content_masks_to_an_empty_string() {
        let e = TextEngine::new();
        assert_eq!(e.masked_display(), "");
        assert_eq!(e.mask_offset(0), 0);
        assert_eq!(e.content_offset_from_mask(0), 0);
    }

    #[test]
    fn clear_resets_everything_without_returning_the_content() {
        let mut e = TextEngine::new();
        e.replace_and_mark_text_in_range(None, "pw", None);
        e.clear();
        assert_eq!(e.content, "");
        assert_eq!(e.selected_range, 0..0);
        assert_eq!(e.marked_range, None);
    }

    #[test]
    fn strip_line_breaks_removes_lf_and_cr_and_borrows_when_clean() {
        assert!(matches!(strip_line_breaks("plain"), Cow::Borrowed("plain")));
        assert_eq!(strip_line_breaks("a\nb\r\nc").as_ref(), "abc");
        // CJK survives untouched — the filter is per-`char`, never per-byte.
        assert_eq!(strip_line_breaks("你\n好").as_ref(), "你好");
    }

    #[test]
    fn boundary_walking_treats_combining_marks_as_one_grapheme() {
        // "e" + a combining acute accent (U+0301) forms ONE extended
        // grapheme cluster ("é") under UAX #29 — a naive char-by-char (not
        // grapheme-aware) boundary walk would stop between the base letter
        // and its combining mark instead of treating them as one editable
        // unit, which is exactly the class of bug that corrupts composed
        // CJK/accented text under backspace.
        let mut e = TextEngine::new();
        e.content = "e\u{0301}b".to_string();
        let end = e.content.len();

        let prev = e.previous_boundary(end);
        assert_eq!(&e.content[prev..], "b", "should land right before the trailing 'b'");

        let prev2 = e.previous_boundary(prev);
        assert_eq!(prev2, 0, "should jump over the WHOLE 'é' combining-mark cluster in one step");
    }
}
