//! WM-3: **Alt-Tab window switching** — the MRU order, the selection state
//! machine, and the switcher panel's geometry.
//!
//! ## Why MRU and not z-order
//!
//! WM-2 shipped `Super+Tab` as `state::cycle_focus`: promote the bottom of the
//! z-order to the top, which really does visit every window in turn but has the
//! property nobody wants from a task switcher — pressing it twice does **not**
//! bring you back to where you were. Every mainstream desktop instead keeps a
//! most-recently-used list so that tapping the binding once flips between the
//! two windows you are actually working in, and holding it walks further back.
//! WM-3 replaces `cycle_focus` with that.
//!
//! ## Everything here is pure
//!
//! `Window`/`Space`/`Seat` never appear. The MRU list is generic over its item
//! type so the tests can use `&str`; the live code instantiates it with
//! `ObjectId`. Same split as `codrive/window_target.rs` and
//! `decor/placement.rs` — see the former's module doc for the original
//! statement of the rule.
//!
//! ## The panel
//!
//! A centred list of window titles, one row per candidate, the selected row
//! filled in brand amber. Rows are drawn by `decor::switcher`, which reuses the
//! title bar's own glyph rasteriser (`decor::text`) rather than introducing a
//! second text path. Thumbnails were considered and rejected for this round:
//! they need a per-window offscreen render pass (the machinery
//! `codrive/shadow.rs` has for the PiP) for a switcher that is on screen for a
//! fraction of a second, and the task brief explicitly allows "視窗標題列縮圖
//! 可簡化為標題文字列表".

use smithay::utils::{Logical, Point, Rectangle, Size};

/// Row height in the switcher panel, logical pixels.
pub const ROW_H: i32 = 30;

/// Padding between the panel's edge and its first/last row.
pub const PANEL_PAD: i32 = 10;

/// Preferred panel width. Shrinks on a narrow output, never grows.
pub const PANEL_W: i32 = 520;

/// Minimum gap between the panel and the output's edges.
pub const PANEL_MARGIN: i32 = 40;

/// Left padding of a row's label.
pub const ROW_PAD_LEFT: i32 = 14;

/// Right padding of a row's label.
pub const ROW_PAD_RIGHT: i32 = 14;

/// Label size in logical pixels. One notch bigger than a title bar's 13 px —
/// the switcher is read at a glance while a key is held, not scanned.
pub const LABEL_FONT_PX: f32 = 14.0;

/// Moves `item` to the front of an MRU list, inserting it if it was absent.
///
/// Called on every focus change. Deliberately unbounded: the list can only
/// contain ids of live toplevels, and [`mru_forget`] removes each one when its
/// window is destroyed, so it is bounded by the number of open windows.
pub fn mru_promote<T: PartialEq>(order: &mut Vec<T>, item: T) {
    order.retain(|existing| existing != &item);
    order.insert(0, item);
}

/// Drops `item` from an MRU list. No-op if it was not there.
pub fn mru_forget<T: PartialEq>(order: &mut Vec<T>, item: &T) {
    order.retain(|existing| existing != item);
}

/// The switcher's candidate order: MRU first, then anything present that the
/// MRU list has never seen.
///
/// `present` is the live set in **topmost-first** order (i.e.
/// `Space::elements().rev()`, plus minimized windows). The second pass is not
/// dead code — a window that has never been focused (it mapped in the
/// background, or focus went straight to a dialog) has no MRU entry at all and
/// must still be reachable. Ordering those by z-order rather than dropping them
/// is the only honest choice; dropping them would make a window unswitchable.
pub fn switch_order<T: Clone + PartialEq>(mru: &[T], present: &[T]) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(present.len());
    for item in mru {
        if present.contains(item) {
            out.push(item.clone());
        }
    }
    for item in present {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

/// Where the selection starts when the switcher opens.
///
/// The **second** entry, so one tap of Alt-Tab flips to the previously focused
/// window — the behaviour the MRU order exists for. With a single candidate
/// there is nowhere else to go and the selection stays on it.
pub fn initial_selection(len: usize) -> usize {
    if len > 1 {
        1
    } else {
        0
    }
}

/// Advances the selection, wrapping in both directions. `len == 0` is answered
/// with `0` rather than a panic — a switcher with no candidates never opens,
/// but a divide/modulo by zero must not be one keystroke away.
pub fn next_selection(len: usize, current: usize, backwards: bool) -> usize {
    if len == 0 {
        return 0;
    }
    let current = current.min(len - 1);
    if backwards {
        (current + len - 1) % len
    } else {
        (current + 1) % len
    }
}

/// How many rows fit on this output, given the panel's own padding.
///
/// At least 1 whenever there is any vertical room at all: a switcher that
/// renders zero rows would look identical to one that failed to open.
pub fn max_rows_for(output: Rectangle<i32, Logical>) -> usize {
    let usable = output.size.h - 2 * PANEL_MARGIN - 2 * PANEL_PAD;
    if usable < ROW_H {
        return if output.size.h > 2 * PANEL_PAD + ROW_H { 1 } else { 0 };
    }
    (usable / ROW_H) as usize
}

/// The window of candidates actually drawn, as `(start, len)`.
///
/// Scrolls to keep `selected` visible, roughly centred, and never runs past
/// either end. Guarantees `start <= selected < start + len` whenever
/// `len > 0` — asserted by the tests, because a switcher that highlights a row
/// it is not drawing is worse than one that does not scroll at all.
pub fn visible_range(total: usize, selected: usize, max_rows: usize) -> (usize, usize) {
    if total == 0 || max_rows == 0 {
        return (0, 0);
    }
    if total <= max_rows {
        return (0, total);
    }
    let half = max_rows / 2;
    let start = selected.saturating_sub(half).min(total - max_rows);
    (start, max_rows)
}

/// The panel rectangle for `rows` visible rows, centred on `output`.
///
/// `output` is whatever coordinate space the caller wants the answer in — the
/// renderer passes an output-local rectangle, the tests pass whatever is
/// convenient.
pub fn panel_rect(output: Rectangle<i32, Logical>, rows: usize) -> Rectangle<i32, Logical> {
    let w = PANEL_W
        .min((output.size.w - 2 * PANEL_MARGIN).max(1))
        .min(output.size.w.max(1))
        .max(1);
    let h = (rows as i32 * ROW_H + 2 * PANEL_PAD)
        .min(output.size.h.max(1))
        .max(1);
    Rectangle::new(
        Point::from((
            output.loc.x + (output.size.w - w) / 2,
            output.loc.y + (output.size.h - h) / 2,
        )),
        Size::from((w, h)),
    )
}

/// The `index`-th visible row inside `panel` (0 = topmost drawn row).
pub fn row_rect(panel: Rectangle<i32, Logical>, index: usize) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((
            panel.loc.x + PANEL_PAD,
            panel.loc.y + PANEL_PAD + index as i32 * ROW_H,
        )),
        Size::from(((panel.size.w - 2 * PANEL_PAD).max(0), ROW_H)),
    )
}

/// How wide a row's label may be. Can legitimately be `0` on a very narrow
/// output, in which case nothing is rasterised — the same contract
/// `decor::title_text_rect` has.
pub fn label_width(row: Rectangle<i32, Logical>) -> i32 {
    (row.size.w - ROW_PAD_LEFT - ROW_PAD_RIGHT).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    /// The real appliance output.
    fn appliance() -> Rectangle<i32, Logical> {
        rect(0, 0, 1280, 800)
    }

    #[test]
    fn promoting_moves_an_existing_entry_to_the_front_without_duplicating_it() {
        let mut order = vec!["a", "b", "c"];
        mru_promote(&mut order, "c");
        assert_eq!(order, vec!["c", "a", "b"]);
        assert_eq!(order.iter().filter(|x| **x == "c").count(), 1);
    }

    #[test]
    fn promoting_a_new_entry_prepends_it() {
        let mut order = vec!["a"];
        mru_promote(&mut order, "z");
        assert_eq!(order, vec!["z", "a"]);
    }

    #[test]
    fn forgetting_removes_exactly_one_entry_and_tolerates_absence() {
        let mut order = vec!["a", "b"];
        mru_forget(&mut order, &"b");
        assert_eq!(order, vec!["a"]);
        mru_forget(&mut order, &"nope");
        assert_eq!(order, vec!["a"]);
    }

    #[test]
    fn the_candidate_order_is_mru_first() {
        // Present is topmost-first; MRU says the user last used "c" then "a".
        let order = switch_order(&["c", "a", "b"], &["b", "a", "c"]);
        assert_eq!(order, vec!["c", "a", "b"]);
    }

    #[test]
    fn a_window_never_focused_is_still_reachable_after_the_mru_entries() {
        // "n" has no MRU entry at all — it must not be dropped.
        let order = switch_order(&["a"], &["n", "a", "m"]);
        assert_eq!(order, vec!["a", "n", "m"]);
    }

    #[test]
    fn stale_mru_entries_for_closed_windows_are_ignored() {
        let order = switch_order(&["gone", "a"], &["a"]);
        assert_eq!(order, vec!["a"]);
    }

    #[test]
    fn one_tap_selects_the_previously_used_window() {
        // The entire reason MRU exists: index 1, not index 0 (which is the
        // window you are already in).
        assert_eq!(initial_selection(3), 1);
        assert_eq!(initial_selection(2), 1);
    }

    #[test]
    fn a_single_candidate_selects_itself_rather_than_going_out_of_range() {
        assert_eq!(initial_selection(1), 0);
        assert_eq!(initial_selection(0), 0);
    }

    #[test]
    fn the_selection_wraps_forwards_and_backwards() {
        assert_eq!(next_selection(3, 2, false), 0);
        assert_eq!(next_selection(3, 0, true), 2);
        assert_eq!(next_selection(3, 1, false), 2);
        assert_eq!(next_selection(3, 1, true), 0);
    }

    #[test]
    fn advancing_an_empty_or_out_of_range_selection_is_not_a_panic() {
        assert_eq!(next_selection(0, 0, false), 0);
        assert_eq!(next_selection(0, 5, true), 0);
        // A stale index (candidates shrank under an open switcher).
        assert_eq!(next_selection(2, 99, false), 0);
    }

    #[test]
    fn holding_the_binding_walks_every_candidate_exactly_once_before_repeating() {
        let len = 5;
        let mut seen = vec![initial_selection(len)];
        let mut cur = seen[0];
        for _ in 1..len {
            cur = next_selection(len, cur, false);
            assert!(!seen.contains(&cur), "index {cur} visited twice");
            seen.push(cur);
        }
        assert_eq!(seen.len(), len);
    }

    #[test]
    fn every_candidate_fits_on_the_appliance_output_for_realistic_window_counts() {
        // 1280x800 with 40px margins and 10px padding: (800-80-20)/30 = 23.
        assert_eq!(max_rows_for(appliance()), 23);
    }

    #[test]
    fn a_very_short_output_still_offers_one_row() {
        let short = rect(0, 0, 640, 80);
        assert_eq!(max_rows_for(short), 1);
    }

    #[test]
    fn an_output_with_no_room_at_all_offers_no_rows() {
        assert_eq!(max_rows_for(rect(0, 0, 640, 20)), 0);
    }

    #[test]
    fn a_short_list_is_shown_whole_from_the_top() {
        assert_eq!(visible_range(4, 3, 10), (0, 4));
        assert_eq!(visible_range(0, 0, 10), (0, 0));
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_selection_visible() {
        for selected in 0..40usize {
            let (start, len) = visible_range(40, selected, 5);
            assert_eq!(len, 5);
            assert!(
                start <= selected && selected < start + len,
                "selection {selected} fell outside the drawn window {start}..{}",
                start + len
            );
            assert!(start + len <= 40, "window ran past the end at {selected}");
        }
    }

    #[test]
    fn the_panel_is_centred_on_the_output() {
        let p = panel_rect(appliance(), 3);
        assert_eq!(p.size.w, PANEL_W);
        assert_eq!(p.size.h, 3 * ROW_H + 2 * PANEL_PAD);
        assert_eq!(p.loc.x, (1280 - PANEL_W) / 2);
        assert_eq!(p.loc.y, (800 - p.size.h) / 2);
        // Centred means equal margins.
        assert_eq!(p.loc.x, 1280 - (p.loc.x + p.size.w));
    }

    #[test]
    fn the_panel_shrinks_on_a_narrow_output_and_never_grows() {
        let narrow = panel_rect(rect(0, 0, 400, 800), 3);
        assert_eq!(narrow.size.w, 400 - 2 * PANEL_MARGIN);
        assert!(narrow.loc.x >= 0);
        let wide = panel_rect(rect(0, 0, 3840, 2160), 3);
        assert_eq!(wide.size.w, PANEL_W, "the panel must not stretch on a big screen");
    }

    #[test]
    fn the_panel_never_exceeds_a_tiny_output() {
        let tiny = rect(0, 0, 60, 40);
        let p = panel_rect(tiny, 8);
        assert!(p.size.w >= 1 && p.size.h >= 1);
        assert!(p.size.w <= tiny.size.w && p.size.h <= tiny.size.h, "{p:?}");
    }

    #[test]
    fn a_second_output_keeps_its_own_origin() {
        let p = panel_rect(rect(1280, 0, 1920, 1080), 2);
        assert!(p.loc.x >= 1280, "panel landed on the wrong output: {p:?}");
    }

    #[test]
    fn rows_stack_without_gaps_and_stay_inside_the_panel() {
        let panel = panel_rect(appliance(), 4);
        let mut previous_bottom = panel.loc.y + PANEL_PAD;
        for i in 0..4 {
            let r = row_rect(panel, i);
            assert_eq!(r.loc.y, previous_bottom, "row {i} does not abut the previous one");
            assert!(r.loc.x >= panel.loc.x);
            assert!(r.loc.x + r.size.w <= panel.loc.x + panel.size.w);
            assert!(r.loc.y + r.size.h <= panel.loc.y + panel.size.h, "row {i} overflows");
            previous_bottom = r.loc.y + r.size.h;
        }
    }

    #[test]
    fn a_labels_width_is_the_row_minus_both_paddings_and_never_negative() {
        let panel = panel_rect(appliance(), 1);
        let row = row_rect(panel, 0);
        assert_eq!(label_width(row), row.size.w - ROW_PAD_LEFT - ROW_PAD_RIGHT);
        assert_eq!(label_width(rect(0, 0, 4, ROW_H)), 0);
    }
}
