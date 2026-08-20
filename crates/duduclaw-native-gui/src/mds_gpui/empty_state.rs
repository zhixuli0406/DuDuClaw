// EmptyState — MDS collection empty/error state (spec §4 Empty; source of
// truth `web/src/components/mds/empty.tsx`). Centered icon-circle + title +
// optional description + optional CTA, `py-16`.
//
// This crate has no icon set yet (`nav.rs`'s badge-letter placeholder
// strategy applies here too — see its doc comment), so `icon_glyph` takes a
// single-glyph string (an emoji or short text mark) instead of the web
// version's `ComponentType<...>` icon prop.

use gpui::{div, prelude::*, px, Div, IntoElement, SharedString};

use crate::theme;

pub fn empty_state(
    icon_glyph: impl Into<SharedString>,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    cta: Option<impl IntoElement>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_center()
        .py_16()
        .child(
            div()
                .mb_4()
                .size_12()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme::alpha(theme::MUTED, 1.0))
                .text_size(px(20.))
                .child(icon_glyph.into()),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(title.into()),
        )
        .children(description.map(|d| {
            div()
                .mt_1()
                .max_w(px(384.))
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(d)
        }))
        .children(cta.map(|c| div().mt_4().child(c)))
}
