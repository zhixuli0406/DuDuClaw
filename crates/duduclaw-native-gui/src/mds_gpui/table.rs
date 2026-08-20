// Table — MDS data table (spec §4 Table; source of truth
// `web/src/components/mds/table.tsx`). Per the S3 task brief: header
// `MUTED_FOREGROUND` `TEXT_XS`, body row height 48px, `hover=SURFACE_HOVER`,
// no zebra striping (MDS uses a hairline row divider instead).
//
// gpui has no HTML `<table>` primitive (and none is needed for the fixed-
// column layouts this crate's pages use) — columns are equal-width `flex_1`
// cells in a `flex` row, the same layout primitive every other screen in
// this crate already uses. No virtualization: the brief only asks for the
// static header/row/hover recipe, and every page this facade currently
// serves (the Gallery dogfood grid) renders a handful of sample rows, not a
// scrolling dataset — a real virtualized data table (gpui's own
// `uniform_list`) is a `Phase 1b`-shaped follow-up if a page ever needs one.

use gpui::{div, prelude::*, px, Div, SharedString};

use crate::theme;

fn row_cells(cells: &[SharedString], header: bool) -> Vec<Div> {
    cells
        .iter()
        .map(|cell| {
            div()
                .flex_1()
                .px_2()
                .text_size(px(if header { theme::TEXT_XS } else { theme::TEXT_SM }))
                .font_weight(if header { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
                .text_color(if header {
                    theme::alpha(theme::MUTED_FOREGROUND, 1.0)
                } else {
                    theme::alpha(theme::FOREGROUND, 1.0)
                })
                .child(cell.clone())
        })
        .collect()
}

/// `headers.len()` fixes the column count; every row in `rows` should carry
/// the same number of cells (a short row simply renders fewer cells, no
/// panic — callers get a visibly malformed row rather than a crash, which
/// is the honest degrade for sample/placeholder data).
pub fn table(headers: &[SharedString], rows: &[Vec<SharedString>]) -> Div {
    let header_row = div()
        .flex()
        .items_center()
        .h(px(40.))
        .border_b_1()
        .border_color(theme::border())
        .children(row_cells(headers, true));

    let mut body = Vec::with_capacity(rows.len());
    for cells in rows {
        body.push(
            div()
                .flex()
                .items_center()
                .h(px(48.))
                .border_b_1()
                .border_color(theme::border())
                .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
                .children(row_cells(cells, false)),
        );
    }

    div().flex().flex_col().w_full().child(header_row).children(body)
}
