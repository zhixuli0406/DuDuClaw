// Card — MDS primitive (spec §4 Card; source of truth
// `web/src/components/mds/card.tsx`). Opaque `SURFACE` panel, `surface_
// border` hairline, `surface_shadow()` elevation, `RADIUS_XL` corners —
// matching `screens/login.rs`'s pre-existing Card-shaped panel exactly (this
// module just extracts that pattern into a reusable facade + adds the
// Header/Title/Description/Content/Footer subdivisions the web version has).
//
// Composition mirrors the web API: `card()` is the outer container, the
// `card_*` helpers are children you `.child(...)` into it in order. Nothing
// here manages layout beyond what each slot's own spec prescribes — callers
// still `.child()` these together same as any other `div()` tree.

use gpui::{div, prelude::*, px, Div, SharedString};

use crate::theme;

/// Outer container — `flex flex-col gap-4 ... py-4`, opaque surface.
pub fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .py_4()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .text_size(px(theme::TEXT_SM))
        .text_color(theme::alpha(theme::SURFACE_FOREGROUND, 1.0))
}

/// `grid gap-1 px-4` — approximated as a tight flex column (gpui's `.grid()`
/// exists but this slot has no `card-action` 2-column layout need here).
pub fn card_header() -> Div {
    div().flex().flex_col().gap_1().px_4()
}

/// `text-base font-medium leading-snug`.
pub fn card_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_BASE))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
        .child(text.into())
}

/// `text-sm text-muted-foreground`.
pub fn card_description(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_SM))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

/// `px-4`.
pub fn card_content() -> Div {
    div().px_4()
}

/// `flex items-center border-t border-surface-border bg-surface-hover/70 p-4`.
pub fn card_footer() -> Div {
    div()
        .flex()
        .items_center()
        .border_t_1()
        .border_color(theme::surface_border())
        .bg(theme::alpha(theme::SURFACE_HOVER, 0.70))
        .p_4()
}
