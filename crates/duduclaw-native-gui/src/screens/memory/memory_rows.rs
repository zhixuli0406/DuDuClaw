// Category rail (left column) + memory list (middle column) — split out of
// `memory.rs` for the same file-size reason `plans.rs`/`plans_rows.rs` are
// split (that file's own doc comment establishes the "nested `mod
// memory_rows;` declared inside `memory.rs` itself, file at
// `screens/memory/memory_rows.rs`" shape this file follows). No behavior
// differs from an unsplit version — see `memory.rs`'s own module doc comment
// for the RPC shapes and canvas fidelity notes this rendering serves.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::theme;
use crate::RootView;

use super::{freshness_band, freshness_color, freshness_label_key, layer_label_key, summarize, MemoryCategory, MemoryEntry, MemoryState};
use crate::screens::goals::relative_time;

fn category_row(cat: MemoryCategory, count: usize, active: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let row_id: SharedString = format!("memory-cat-{}", cat.label_key()).into();
    div()
        .id(row_id)
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px_2p5()
        .py_1p5()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .bg(if active { theme::alpha(theme::SIDEBAR_ACCENT, 1.0) } else { theme::alpha(theme::SURFACE, 0.0) })
        .when(!active, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .text_size(px(theme::TEXT_XS))
        .text_color(if active {
            theme::alpha(theme::SIDEBAR_ACCENT_FOREGROUND, 1.0)
        } else {
            theme::alpha(theme::MUTED_FOREGROUND, 1.0)
        })
        .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .child(i18n::t(locale, cat.label_key()))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(count.to_string()))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MemoryState>().category = cat;
            cx.notify();
        }))
}

pub(super) fn category_rail(entries: &[MemoryEntry], category: MemoryCategory, locale: Locale, cx: &mut Context<RootView>) -> Div {
    let mut rail = div().w(px(170.)).flex_shrink_0().flex().flex_col().gap_1();
    for cat in MemoryCategory::ALL {
        let count = entries.iter().filter(|e| cat.matches(&e.layer)).count();
        rail = rail.child(category_row(cat, count, cat == category, locale, cx));
    }
    rail
}

fn memory_row(entry: &MemoryEntry, selected: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let when = relative_time(locale, &entry.timestamp, chrono::Utc::now());
    let band = freshness_band(entry.retrievability);
    let dot_color = band.map(freshness_color).unwrap_or(theme::MUTED_FOREGROUND);
    let row_id: SharedString = format!("memory-row-{}", entry.id).into();
    let id_for_click = entry.id.clone();
    let layer_key = layer_label_key(&entry.layer);

    div()
        .id(row_id)
        .flex()
        .items_start()
        .gap_2()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .bg(if selected { theme::alpha(theme::SIDEBAR_ACCENT, 1.0) } else { theme::alpha(theme::SURFACE, 1.0) })
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().mt(px(5.)).flex_shrink_0().size(px(8.)).rounded_full().bg(theme::alpha(dot_color, 1.0)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(summarize(&entry.content, 90)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .children(layer_key.map(|k| div().child(i18n::t(locale, k))))
                        .child(div().child(when))
                        .children(band.map(|b| div().child(i18n::t(locale, freshness_label_key(b))))),
                ),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MemoryState>().selected_entry = Some(id_for_click.clone());
            cx.notify();
        }))
}

pub(super) fn list_column(entries: &[MemoryEntry], category: MemoryCategory, selected: &Option<String>, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let visible: Vec<&MemoryEntry> = entries.iter().filter(|e| category.matches(&e.layer)).collect();
    let mut col = div().id("memory-list").flex_1().min_w_0().overflow_y_scroll().flex().flex_col().gap_1p5();
    if visible.is_empty() {
        return col.child(empty_state("🧠", i18n::t(locale, "memoryPage.category.empty"), None, None::<Div>));
    }
    for e in visible {
        col = col.child(memory_row(e, selected.as_deref() == Some(e.id.as_str()), locale, cx));
    }
    col
}
