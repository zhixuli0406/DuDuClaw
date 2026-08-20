// Shared row-rendering primitives for `shell_sidebar.rs` (Column 1) and
// `shell_content_list.rs` (Column 2) — split out of a single `shell.rs`
// during the P0-1 three-column rebuild (2026-08-19, see `shell.rs`'s header
// comment for the overall design) purely to keep each file under this
// crate's own <300-line convention. No behavior here differs from the
// pre-split version.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n;
use crate::i18n::Locale;
use crate::nav::NavItem;
use crate::theme;
use crate::RootView;

/// The badge/icon placeholder shared by every clickable row in Columns 1
/// and 2 (area rows and page rows alike). Dark text on a bright token fill
/// keeps the letter legible against every badge color in `nav.rs`'s
/// MDS-sourced palette.
pub(super) fn row_icon(letter: char, color: u32) -> Div {
    div()
        .size_5()
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::alpha(color, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::APP_SHELL, 1.0))
        .child(letter.to_string())
}

/// The row label, shared by area rows and page rows — MDS spec §5.1 exactly
/// (active = background + weight, never a colored label).
pub(super) fn row_label(text: SharedString, selected: bool) -> Div {
    div()
        .flex_1()
        .text_size(px(theme::TEXT_SM))
        .text_color(if selected {
            theme::alpha(theme::SIDEBAR_ACCENT_FOREGROUND, 1.0)
        } else {
            theme::alpha(theme::MUTED_FOREGROUND, 1.0)
        })
        .font_weight(if selected { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
        .child(text)
}

/// One clickable Column-2/footer row — a concrete `NavItem`, selection
/// driven by `active_id == item.id`, click sets `active_page` to this exact
/// item. `Stateful<Div>` (not `impl IntoElement`) so a plain `Vec<..>` can
/// collect rows from a `for` loop — see `language_picker.rs`'s doc comment
/// for why a loop, not `.map()`, when a `&mut Context` needs to be threaded
/// through per-item click handlers.
pub(super) fn nav_row(item: NavItem, active_id: &str, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let selected = item.id == active_id;

    div()
        .id(item.id)
        .flex()
        .items_center()
        .gap_2()
        .h_8()
        .px_2()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .hover(|style| style.bg(theme::alpha(theme::SIDEBAR_ACCENT, 0.7)))
        .child(row_icon(item.badge_letter, item.badge_color))
        .child(row_label(i18n::t(locale, item.label_key), selected))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.active_page = item.id;
            cx.notify();
        }))
}
