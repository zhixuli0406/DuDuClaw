// Column 2 of the app shell — the selected area's own page list. Split out
// of `shell.rs` during the P0-1 three-column rebuild (2026-08-19); see that
// file's header comment for the overall three-column design.

use gpui::{div, prelude::*, px, Context, Stateful};

use crate::i18n;
use crate::nav;
use crate::screens::shell_row::nav_row;
use crate::theme;
use crate::RootView;

/// Column 2 width — same order of magnitude as Column 1; it lists at most a
/// handful of short page names for any one area (`nav.rs::AREAS`' longest is
/// `AI 員工` with 7 items as of the S5b2-D update — see `nav.rs`'s own
/// module doc comment).
const CONTENT_LIST_WIDTH: f32 = 224.0;

/// `None` when the current page's area holds only one page (nothing to
/// disambiguate — HIG: "area 只有單頁時欄 2 可隱藏", see `shell.rs`'s header
/// comment) or when the current page is a footer item (belongs to no area
/// at all, e.g. `manage`/`componentLibrary`).
pub(super) fn render(state: &RootView, cx: &mut Context<RootView>) -> Option<Stateful<gpui::Div>> {
    // WP-S6b3-fix (2026-08-22): `nav::sidebar_active_id` maps a drill-down
    // leaf with no `nav.rs` id of its own (`knowledgeCuration`/`sharedWiki`)
    // onto its real parent row (`knowledgeHub`) — both so `area_for_page`
    // finds the owning area at all (a bare leaf id resolves to `None`) and
    // so the row loop below highlights the right row. A no-op for every
    // other page (see that function's own doc comment). Content ROUTING is
    // untouched — `shell.rs` still keys off the real, unmapped
    // `state.active_page`.
    let active_id = nav::sidebar_active_id(state.active_page);
    let area = nav::area_for_page(active_id)?;
    if area.items.len() <= 1 {
        return None;
    }
    let locale = state.locale;

    let mut rows = Vec::with_capacity(area.items.len());
    for item in area.items {
        rows.push(nav_row(*item, active_id, locale, cx));
    }

    Some(
        div()
            .id("content-list")
            .w(px(CONTENT_LIST_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded(px(theme::RADIUS_XL))
            .overflow_hidden()
            .bg(theme::alpha(theme::SIDEBAR, 1.0))
            .border_1()
            .border_color(theme::sidebar_border())
            .shadow(theme::surface_shadow())
            .child(
                // Column-2 header names the selected area — MDS spec §5.1
                // group-header scale, same bucket the old flat sidebar's
                // `工作`/`公司`/`設定` headers used.
                div()
                    .h_8()
                    .px_2()
                    .flex()
                    .items_center()
                    .text_size(px(theme::TEXT_XS))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme::alpha(theme::SIDEBAR_FOREGROUND, 0.7))
                    .child(i18n::t(locale, area.label_key)),
            )
            .children(rows),
    )
}
