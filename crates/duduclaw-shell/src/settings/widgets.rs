// D4b — the settings app's shared visual primitives.
//
// Every page is built from these five shapes and nothing else, so all seven
// look like one application rather than seven. They deliberately mirror
// `overlay::pointer_settings`' card/section/notice geometry (12px radius,
// 18/16 padding, `surface` on `surface_raised`, `TEXT_XS` notices) — that
// page is the design canvas's own settings-page artboard rendered, so
// copying its metrics is what keeps this app consistent with the board
// without re-deriving it.
//
// Nothing here touches `ShellView`: these are pure element builders taking
// already-resolved values, which is what lets the pages stay thin and keeps
// the click wiring visible at the page's own call site rather than buried in
// a widget.

use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::palette::ShellPalette;

/// Severity of a one-line notice. The four states this app must be able to
/// tell apart on screen (see `settings/mod.rs`'s honesty contract) map onto
/// exactly these colours, so a page can never accidentally render "not asked
/// yet" in the same red as "the call failed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Neutral: loading, not-applicable, informational.
    Muted,
    /// Something the operator should notice but that is not an error —
    /// a service that is not installed, a setting that will not persist.
    Warning,
    /// A call failed, or a value is invalid.
    Danger,
    /// A change was applied.
    Success,
}

impl Tone {
    fn color(self, palette: ShellPalette) -> u32 {
        match self {
            Tone::Muted => palette.muted_foreground,
            Tone::Warning => palette.warning,
            Tone::Danger => palette.destructive,
            Tone::Success => palette.success,
        }
    }
}

/// The one card shape every page section uses.
pub(crate) fn card(palette: ShellPalette) -> Div {
    div()
        .bg(theme::alpha(palette.surface, 1.0))
        .border_1()
        .border_color(palette.border())
        .rounded(px(12.))
        .px(px(18.))
        .py(px(16.))
        .flex()
        .flex_col()
        .gap(px(12.))
}

/// A card's own heading, with an optional right-hand slot (a 重新整理 button,
/// a status pill).
pub(crate) fn card_header(title: &'static str, trailing: Option<AnyElement>, palette: ShellPalette) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.))
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child(title),
        );
    if let Some(trailing) = trailing {
        row = row.child(trailing);
    }
    row
}

/// One label/value line — the workhorse of 關於, 網路 status and 日期與時間.
///
/// `value` is an owned `String` because almost every one of them is read
/// from a backend at runtime; a `&'static str` variant would buy nothing but
/// a second function.
pub(crate) fn value_row(label: &'static str, value: String, palette: ShellPalette) -> Div {
    // An absent value is rendered as an em-dash rather than an empty gap:
    // "we asked and there is nothing" has to be visibly different from "this
    // row failed to render".
    let shown = if value.trim().is_empty() { "—".to_string() } else { value };
    div()
        .flex()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .w(px(112.))
                .flex_none()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child(shown),
        )
}

/// One honest one-liner. Every "loading", "not applicable", "not installed",
/// "failed" and "applied" line in this app goes through here — see `Tone`.
pub(crate) fn notice(text: String, tone: Tone, palette: ShellPalette) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(tone.color(palette), 1.0)).child(text)
}

/// `notice` for a literal, saving every call site a `.to_string()`.
pub(crate) fn notice_static(text: &'static str, tone: Tone, palette: ShellPalette) -> Div {
    notice(text.to_string(), tone, palette)
}

/// A small field label above an `OobeTextField`.
pub(crate) fn field_label(text: &'static str, palette: ShellPalette) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(text)
}

/// The two button weights this app uses. A DISABLED button is a real state,
/// not an absent button: a control whose backend cannot perform the change
/// must still be visible with its reason next to it (see `settings/mod.rs`'s
/// honesty contract), because a control that silently disappears reads as a
/// missing feature rather than an unavailable one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonWeight {
    Primary,
    Secondary,
}

pub(crate) fn button(
    id: &'static str,
    label: String,
    weight: ButtonWeight,
    enabled: bool,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let (bg, fg, border) = match (weight, enabled) {
        (ButtonWeight::Primary, true) => (palette.brand, palette.brand_foreground, palette.brand),
        (ButtonWeight::Primary, false) => (palette.muted, palette.muted_foreground, palette.muted),
        (ButtonWeight::Secondary, true) => (palette.surface_raised, palette.foreground, palette.muted),
        (ButtonWeight::Secondary, false) => (palette.surface, palette.muted_foreground, palette.muted),
    };
    let mut b = div()
        .id(id)
        .flex_none()
        .px(px(14.))
        .py(px(7.))
        .rounded(px(8.))
        .bg(theme::alpha(bg, 1.0))
        .border_1()
        .border_color(theme::alpha(border, 1.0))
        .text_size(px(theme::TEXT_XS))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(fg, 1.0))
        .child(label);
    if enabled {
        b = b.cursor_pointer().hover(|s| s.opacity(0.88)).on_click(on_click);
    } else {
        b = b.opacity(0.55);
    }
    b
}

/// One segmented choice (DHCP vs 靜態 IP, a display scale step). Selected
/// segments are not clickable — same discipline `pointer_settings`' size
/// buttons use, so a click can never re-issue the change already in effect.
#[allow(clippy::too_many_arguments)]
pub(crate) fn segment(
    id: (&'static str, usize),
    label: String,
    selected: bool,
    enabled: bool,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let mut seg = div()
        .id(id)
        .flex_none()
        .px(px(14.))
        .py(px(7.))
        .rounded(px(8.))
        .border_1()
        .border_color(if selected { theme::alpha(palette.brand, 1.0).into() } else { palette.border() })
        .bg(theme::alpha(if selected { palette.surface_selected } else { palette.surface_raised }, 1.0))
        .text_size(px(theme::TEXT_XS))
        .font_weight(if selected { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
        .text_color(theme::alpha(if selected { palette.brand } else { palette.foreground }, 1.0))
        .child(label);
    if enabled && !selected {
        seg = seg.cursor_pointer().hover(|s| s.bg(theme::alpha(palette.surface_hover, 1.0))).on_click(on_click);
    } else if !enabled {
        seg = seg.opacity(0.55);
    }
    seg
}

/// The ON/OFF pill used for NTP. Visually the same switch ControlCenter's
/// `toggle_pill` draws; re-derived here rather than shared because that one
/// is private to `overlay::controlcenter` and takes its own palette
/// decisions from that panel's board.
pub(crate) fn toggle_pill(
    id: &'static str,
    on: bool,
    enabled: bool,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let track = if on { palette.brand } else if palette.is_dark() { 0x52525b } else { 0xd4d4d8 };
    let knob = div()
        .w(px(16.))
        .h(px(16.))
        .rounded(px(16.))
        .bg(theme::alpha(0xffffff, 1.0))
        .shadow(palette.icon_shadow(0.18, 0.32));
    let mut pill = div()
        .id(id)
        .w(px(40.))
        .h(px(22.))
        .flex_none()
        .rounded(px(22.))
        .bg(theme::alpha(track, 1.0))
        .flex()
        .items_center()
        .px(px(3.))
        .justify_end()
        .child(knob);
    if !on {
        pill = pill.justify_start();
    }
    if enabled {
        pill = pill.cursor_pointer().on_click(on_click);
    } else {
        pill = pill.opacity(0.55);
    }
    pill
}

/// A row with a title, a sub-line and a trailing control — the shape 日期與
/// 時間's NTP switch and 使用者's rows use.
pub(crate) fn control_row(title: &'static str, subtitle: String, trailing: AnyElement, palette: ShellPalette) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::alpha(palette.foreground, 1.0))
                        .child(title),
                )
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.text_faint, 1.0)).child(subtitle)),
        )
        .child(trailing)
}

/// A small status dot + text pill, used where a page needs to state a live
/// condition compactly (link up/down, update available).
pub(crate) fn status_pill(text: String, tone: Tone, palette: ShellPalette) -> Div {
    let color = tone.color(palette);
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(9.))
        .py(px(4.))
        .rounded(px(999.))
        .bg(theme::alpha(color, 0.12))
        .child(div().w(px(6.)).h(px(6.)).flex_none().rounded(px(6.)).bg(theme::alpha(color, 1.0)))
        .child(div().text_size(px(11.)).text_color(theme::alpha(color, 1.0)).child(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four honesty states must not collapse onto one colour — that is
    /// the whole reason `Tone` exists rather than a bool.
    #[test]
    fn every_tone_maps_to_a_distinct_colour_in_both_themes() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            let colors = [Tone::Muted, Tone::Warning, Tone::Danger, Tone::Success].map(|t| t.color(palette));
            let mut deduped = colors.to_vec();
            deduped.sort_unstable();
            deduped.dedup();
            assert_eq!(deduped.len(), colors.len(), "two tones share a colour: {colors:x?}");
        }
    }
}
