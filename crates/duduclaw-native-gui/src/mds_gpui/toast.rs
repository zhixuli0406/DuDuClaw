// Toast — MDS notification pill, per the S3 task brief ("右下角、semantic
// 變體"). The web MDS catalog (`web/src/components/mds/`) has no dedicated
// toast primitive of its own — `web/src/components/Toast.tsx` is a plain
// component outside that catalog — so this facade's recipe (surface-raised
// card + `floating_shadow()` + a semantic left accent bar) is this crate's
// own MDS-token-consistent design, not a port of an existing spec section.

use gpui::{div, prelude::*, px, Div, Rgba, SharedString};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastVariant {
    Default,
    Success,
    Warning,
    Destructive,
}

fn accent_color(variant: ToastVariant) -> Rgba {
    match variant {
        ToastVariant::Default => theme::alpha(theme::MUTED_FOREGROUND, 1.0),
        ToastVariant::Success => theme::alpha(theme::SUCCESS, 1.0),
        ToastVariant::Warning => theme::alpha(theme::WARNING, 1.0),
        ToastVariant::Destructive => theme::alpha(theme::DESTRUCTIVE, 1.0),
    }
}

/// A single toast card — `SURFACE_RAISED` + `floating_shadow()` +
/// `RADIUS_XL`, with a 3px semantic-colored left accent bar standing in for
/// an icon (this crate has no icon set yet, same placeholder strategy
/// `nav.rs`'s badge letters already use).
pub fn toast(variant: ToastVariant, title: impl Into<SharedString>, description: Option<SharedString>) -> Div {
    let accent = accent_color(variant);
    div()
        .w(px(320.))
        .flex()
        .gap_2p5()
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::floating_shadow())
        .child(div().w(px(3.)).flex_shrink_0().rounded(px(theme::RADIUS_SM)).bg(accent))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(theme::SURFACE_FOREGROUND, 1.0))
                        .child(title.into()),
                )
                .children(description.map(|d| {
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(d)
                })),
        )
}

/// Positions a stack of `toast(...)` cards fixed to the bottom-right corner,
/// newest at the bottom — the real-usage wrapper. Must be a top-level screen
/// child, same caller contract as `dialog::dialog_overlay`. Not yet called
/// by anything in this crate — `screens/gallery.rs` lists the 4 toast
/// variants inline instead (see this module's doc comment); kept as
/// forward-looking facade surface for the first real caller.
#[allow(dead_code)]
pub fn toast_container(toasts: Vec<Div>) -> Div {
    div()
        .absolute()
        .bottom(px(16.))
        .right(px(16.))
        .flex()
        .flex_col()
        .gap_2()
        .children(toasts)
}
