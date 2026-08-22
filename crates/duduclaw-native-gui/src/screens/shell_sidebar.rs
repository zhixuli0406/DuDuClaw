// Column 1 of the app shell — the area rail. Split out of `shell.rs` during
// the P0-1 three-column rebuild (2026-08-19); see that file's header
// comment for the overall three-column design, the collapsible full-hide
// rationale, and the area→id mapping this file just renders (owned by
// `nav.rs`).

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::i18n;
use crate::nav::{self, NavArea};
use crate::screens::shell_row::{nav_row, row_icon, row_label};
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

/// Column 1 width — inside the HIG-surveyed 180–280px native sidebar range
/// (research doc §1.1: GNOME NavigationSplitView's own bounds), narrowed
/// from the old flat sidebar's 256px since Column 1 now only has to fit 6
/// short area labels instead of ~19 page rows.
const SIDEBAR_WIDTH: f32 = 224.0;

/// One clickable Column-1 area row. Selection persists across whichever
/// page inside the area is actually showing (Apple HIG, research doc §4:
/// "persistently highlight the current selection in each pane") — driven by
/// `nav::area_for_page(active_id)`, not by comparing ids directly, since an
/// area row's own id (`area.id`, the `area*` namespace) is never itself a
/// valid `active_page` value. Clicking navigates to the area's FIRST item
/// (`area.items[0]`) — for the one single-item area (`areaManage`) this
/// doubles as "go straight to that page", exactly the HIG single-page-area
/// case `shell.rs`'s header comment describes.
///
/// `index` becomes part of this row's gpui `ElementId` (`("area", index)`,
/// one of `ElementId`'s built-in tuple `From` impls — no `String` alloc
/// needed) specifically so it can never collide with a Column-2/footer
/// row's own `.id(item.id)`: when the selected area's first item is ALSO
/// rendered as a Column-2 row (the common case), both rows exist in the
/// same view at once and would otherwise share an id.
fn area_row(area: &NavArea, index: usize, active_id: &str, locale: crate::i18n::Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let selected = nav::area_for_page(active_id).map(|a| a.id) == Some(area.id);
    let target = area.items[0].id;

    div()
        .id(("area", index))
        .flex()
        .items_center()
        .gap_2()
        .h_8()
        .px_2()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .hover(|style| style.bg(theme::alpha(theme::SIDEBAR_ACCENT, 0.7)))
        .child(row_icon(area.badge_letter, area.badge_color))
        .child(row_label(i18n::t(locale, area.label_key), selected))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.active_page = target;
            cx.notify();
        }))
}

fn status_dot(status: WsConnState) -> Div {
    div().size_2().rounded_full().bg(theme::alpha(status.dot_color(), 1.0))
}

/// Column 1 — the area rail. Header (wordmark + WS status) → "+新對話" CTA
/// (unchanged from the pre-P0-1 sidebar — still its own distinctly-styled
/// action, still sets `active_page = "newChat"`) → search trigger
/// placeholder → the 6 area rows → spacer → pinned footer (`nav.rs::
/// FOOTER_ITEMS`) → edition badge, in that order top to bottom.
pub(super) fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    // WP-S6b3-fix (2026-08-22): `nav::sidebar_active_id` maps a drill-down
    // leaf with no `nav.rs` id of its own (`knowledgeCuration`/`sharedWiki`)
    // onto its real parent row (`knowledgeHub`) so Column 1's area
    // highlight — and, via the same mapped id threaded into `nav_row`
    // below, the footer rows' own highlight — keeps working across the
    // drill-down instead of going dark. A no-op for every other page (see
    // that function's own doc comment).
    let active_id = nav::sidebar_active_id(state.active_page);

    let mut area_rows = Vec::with_capacity(nav::AREAS.len());
    for (index, a) in nav::AREAS.iter().enumerate() {
        area_rows.push(area_row(a, index, active_id, locale, cx));
    }
    let mut footer_rows = Vec::with_capacity(nav::FOOTER_ITEMS.len());
    for item in nav::FOOTER_ITEMS {
        footer_rows.push(nav_row(*item, active_id, locale, cx));
    }

    div()
        .id("sidebar")
        .w(px(SIDEBAR_WIDTH))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SIDEBAR, 1.0))
        .border_1()
        .border_color(theme::sidebar_border())
        .shadow(theme::surface_shadow())
        // ── Header: wordmark + WS status dot ─────────────────────────
        // WP-C-M2: the whole block is clickable -> `screens::gateway_
        // picker`. Before this change that page was reachable ONLY via the
        // `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=gatewayPicker` debug boot override
        // (see that page's own header comment) — a real user could never
        // reach it to switch gateways at all. The connection-status dot is
        // the natural entry point: it's the one piece of chrome ALWAYS
        // visible regardless of which page is open, and it already shows
        // exactly the state ("disconnected"/"connecting"/"connected") a
        // user would click it to go investigate.
        .child(
            div()
                .id("shell-gateway-status")
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                .cursor_pointer()
                .rounded(px(theme::RADIUS_LG))
                .hover(|style| style.bg(theme::alpha(theme::MUTED, 0.5)))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "gatewayPicker";
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_size(px(18.)).child("🐾"))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(theme::TEXT_SM))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::alpha(theme::SIDEBAR_FOREGROUND, 1.0))
                                .child(i18n::t(locale, "app.name")),
                        )
                        .child(status_dot(state.ws_state)),
                )
                .child(
                    div()
                        .pl(px(26.))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(state.ws_state.short_label(locale)),
                ),
        )
        // ── Primary "new chat" action — MDS Button `brand` variant ─────
        .child(
            div()
                .id("shell-new-chat")
                .h_8()
                .px_2p5()
                .flex()
                .items_center()
                .justify_center()
                .gap_1p5()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::BRAND, 1.0))
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .cursor_pointer()
                .hover(|style| style.bg(theme::alpha(theme::BRAND, 0.90)))
                .active(|style| style.bg(theme::alpha(theme::BRAND, 0.85)))
                .child("+")
                .child(i18n::t(locale, "nav.newChat"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "newChat";
                    cx.notify();
                })),
        )
        // ── Search trigger placeholder (visual only, Phase 1b wires it,
        // MDS spec §5.1: "SearchTrigger（⌘K keycaps）") ─────────────────
        .child(
            div()
                .h_8()
                .px_2p5()
                .flex()
                .items_center()
                .gap_1p5()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::input_bg())
                .border_1()
                .border_color(theme::input_border())
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("🔍")
                .child(div().flex_1().child(i18n::t(locale, "native.search.placeholder")))
                .child(
                    div()
                        .px_1()
                        .rounded(px(theme::RADIUS_SM))
                        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child("⌘K"),
                ),
        )
        // ── The 6 top-level areas ──────────────────────────────────────
        .child(div().flex().flex_col().gap_1().children(area_rows))
        // ── Spacer pushes the footer + edition badge to the bottom ────
        .child(div().flex_1())
        // ── Pinned footer: 設定 (`manage`) then 元件庫 (`componentLibrary`)
        // — Windows `NavigationView.IsSettingsVisible` convention, never
        // inside an area (`nav.rs::FOOTER_ITEMS` doc comment) ───────────
        .child(div().flex().flex_col().gap_1().children(footer_rows))
        .child(
            div()
                .id("shell-edition-badge")
                .self_start()
                .px_2p5()
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(theme::RADIUS_4XL))
                .bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(format!("{} Lv.1 v", i18n::t(locale, "native.shell.editionBadge")))
                // WP-NG-debt: only the version NUMBER renders in the system
                // monospace face (same `SF Mono` convention `about.rs`'s own
                // version badge and `chat/markdown.rs::mono_font` already
                // establish) — split into its own child so the edition
                // label + "Lv.1 v" prefix keep the sidebar's normal UI font;
                // a nested `div()` with no `text_size`/`text_color` of its
                // own inherits both from this flex row, so only the font
                // family differs, not the size/color of this badge.
                .child(div().font_family("SF Mono").child(env!("CARGO_PKG_VERSION"))),
        )
}
