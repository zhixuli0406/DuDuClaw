// Right column: selected memory's KV facts + supersession chain (F1) +
// freshness legend — split out of `memory.rs` for the same file-size reason
// `memory_rows.rs` (this module's sibling) is split. No behavior differs
// from an unsplit version — see `memory.rs`'s own module doc comment for the
// `memory.history` RPC shape and freshness-band source of truth.

use gpui::{div, prelude::*, px, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::screens::dashboard::Loadable;
use crate::theme;

use super::{freshness_color, freshness_label_key, layer_label_key, source_label_key, summarize, MemoryEntry, MemoryHistory};

fn kv_row(label: SharedString, value: SharedString) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_2()
        .py_1p5()
        .border_t_1()
        .border_color(theme::border())
        .text_size(px(theme::TEXT_XS))
        .child(div().text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value))
}

fn history_section(history: &Loadable<MemoryHistory>, locale: Locale) -> Div {
    let heading = div().mt_3().pt_3().border_t_1().border_color(theme::border()).text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "memoryPage.history.title"));

    let body = match history {
        Loadable::Loading => div().mt_2().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "memoryPage.history.loading")),
        Loadable::Failed(_) => div().mt_2().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "memoryPage.history.error")),
        Loadable::Ready(h) if h.chain.is_empty() => div().mt_2().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "memoryPage.history.empty")),
        Loadable::Ready(h) => {
            let mut col = div().mt_2().flex().flex_col().gap_1p5();
            for c in &h.chain {
                let (bg, text) = if c.is_current {
                    (theme::alpha(theme::SUCCESS, 0.12), theme::alpha(theme::SUCCESS, 1.0))
                } else {
                    (theme::alpha(theme::MUTED, 1.0), theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                };
                let status_key = if c.is_current { "memoryPage.history.current" } else { "memoryPage.history.superseded" };
                col = col.child(
                    div()
                        .p_2()
                        .rounded(px(theme::RADIUS_MD))
                        .bg(bg)
                        .text_size(px(theme::TEXT_XS))
                        .text_color(text)
                        .when(!c.is_current, |el| el.line_through())
                        .child(format!("{} · {}", i18n::t(locale, status_key), summarize(&c.content, 60))),
                );
            }
            col
        }
    };

    div().child(heading).child(body)
}

fn freshness_legend(locale: Locale) -> Div {
    let mut row = div().mt_3().pt_3().border_t_1().border_color(theme::border()).flex().flex_col().gap_1();
    row = row.child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "memoryPage.freshness.title")));
    for band in ["fresh", "stable", "fading", "archiving"] {
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(div().size(px(8.)).rounded_full().bg(theme::alpha(freshness_color(band), 1.0)))
                .child(i18n::t(locale, freshness_label_key(band))),
        );
    }
    row
}

pub(super) fn detail_column(entries: &[MemoryEntry], selected: &Option<String>, history: &Loadable<MemoryHistory>, locale: Locale) -> Stateful<Div> {
    // `.overflow_y_scroll()` lives on `StatefulInteractiveElement` (needs a
    // stable identity to track scroll position across renders) — `.id(...)`
    // first, same requirement every other scrollable column in this crate
    // already satisfies.
    let panel = div().id("memory-detail-panel").w(px(250.)).flex_shrink_0().overflow_y_scroll().rounded(px(theme::RADIUS_XL)).bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).p_3p5();

    let Some(entry) = selected.as_deref().and_then(|id| entries.iter().find(|e| e.id == id)) else {
        return panel.child(empty_state("👈", i18n::t(locale, "memoryPage.detail.empty"), None, None::<Div>));
    };

    let recorded = if entry.timestamp.is_empty() { "—".to_string() } else { entry.timestamp.clone() };
    let mut col = div()
        .flex()
        .flex_col()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(summarize(&entry.content, 160)));

    if let Some(k) = layer_label_key(&entry.layer) {
        col = col.child(kv_row(i18n::t(locale, "memoryPage.detail.kind"), i18n::t(locale, k)));
    }
    col = col
        .child(kv_row(i18n::t(locale, "memoryPage.detail.source"), i18n::t(locale, source_label_key(&entry.source_event))))
        .child(kv_row(i18n::t(locale, "memoryPage.detail.recorded"), recorded.into()))
        .child(kv_row(i18n::t(locale, "memoryPage.detail.recalled"), i18n::t1(locale, "memoryPage.detail.recalled.value", "count", &entry.access_count.to_string())));

    col = col.child(history_section(history, locale)).child(freshness_legend(locale));

    panel.child(col)
}
