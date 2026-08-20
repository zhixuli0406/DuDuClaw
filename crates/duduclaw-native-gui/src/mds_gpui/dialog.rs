// Dialog — MDS centered modal (spec §4 Dialog; source of truth
// `web/src/components/mds/dialog.tsx`). Overlay = translucent black backdrop
// + `SURFACE_RAISED` panel + `floating_shadow()` + `RADIUS_XL`, per the S3
// task brief.
//
// Split into two functions on purpose:
// - `dialog_panel(...)` — just the panel itself (title/description/content/
//   footer), no backdrop or positioning. This is what `screens/gallery.rs`
//   renders inline as a static "here's what it looks like" preview, since
//   the Gallery page has no dialog-open/closed boolean anywhere to drive a
//   real show/hide — an honest choice over faking interactivity that isn't
//   wired to anything (see this crate's S3 report for the explicit stub
//   callout).
// - `dialog_overlay(...)` — `dialog_panel(...)` composed with the full-
//   screen backdrop, for real future callers that DO have open/closed
//   state. It must be the LAST `.child(...)` of a screen's outermost
//   container (paint order on unpositioned siblings follows document order
//   in gpui, same as CSS) — no `deferred()` wrapper is needed for that case
//   alone, but a caller stacking a dialog over its own internally-scrolled
//   content should reach for `gpui::deferred(...)` around this if it turns
//   out to matter (not exercised by anything in this crate yet).

use gpui::{div, prelude::*, px, rgb, App, ClickEvent, Div, IntoElement, SharedString, Stateful};

use crate::theme;

/// True when the platform's dialog convention puts the primary/confirm
/// button FIRST (leftmost) — Windows only. Everyone else (macOS, GNOME; KDE
/// leaves it to Qt's per-platform `QDialogButtonBox`, which itself follows
/// this same macOS/Windows split) puts confirm on the RIGHT, Cancel on the
/// left. Research doc §5 / table A#6: "對話框按鈕順序兩陣營相反（硬性）—
/// macOS/GNOME 確認在右（Cancel 在左）；Windows primary 在左、最右是安全鈕"
/// — a single hardcoded order (what this crate shipped before P0-3) is
/// necessarily wrong on whichever platform it wasn't written for.
///
/// Pulled out of `dialog_button_row` as a plain `&str -> bool` function
/// (not gated behind `cfg!(target_os = ...)` directly) so the actual
/// per-platform DECISION is unit-testable for all three platform strings in
/// a single test run, regardless of which OS actually built the test
/// binary — this crate has no headless dialog-render harness (see
/// `main.rs`'s gotcha list on the lack of scriptable UI automation), so the
/// only thing that CAN be verified without a live window is this pure
/// lookup.
fn primary_button_leads_on(os: &str) -> bool {
    os == "windows"
}

/// A dialog's footer action row, ordered per the current platform's
/// convention (see `primary_button_leads_on`). Takes the two buttons
/// already built (via `mds_gpui::button`, any variant) — this helper only
/// decides which one renders first, it doesn't know or care about their
/// styling. `screens/gallery.rs`'s dialog preview is the first caller.
pub fn dialog_button_row(confirm: impl IntoElement, cancel: impl IntoElement) -> Div {
    let row = div().flex().justify_end().gap_2();
    if primary_button_leads_on(std::env::consts::OS) {
        row.child(confirm).child(cancel)
    } else {
        row.child(cancel).child(confirm)
    }
}

/// The panel alone — no backdrop, no absolute positioning. `width` lets
/// callers pick a size (MDS's own spec caps at `sm` ≈ 384px on desktop).
pub fn dialog_panel(
    width: gpui::Pixels,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    content: Option<Div>,
    footer: Option<Div>,
    on_close: Option<impl Fn(&ClickEvent, &mut gpui::Window, &mut App) + 'static>,
) -> Div {
    let mut panel = div()
        .relative()
        .w(width)
        .flex()
        .flex_col()
        .gap_4()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::floating_shadow())
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_BASE))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(theme::SURFACE_FOREGROUND, 1.0))
                        .child(title.into()),
                )
                .children(description.map(|d| {
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(d)
                })),
        )
        .children(content);

    if let Some(handler) = on_close {
        panel = panel.child(
            div()
                .id("mds-dialog-close")
                .absolute()
                .top(px(12.))
                .right(px(12.))
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS_SM))
                .cursor_pointer()
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|style| style.bg(theme::alpha(theme::MUTED, 1.0)))
                .child("×")
                .on_click(handler),
        );
    }

    panel.children(footer.map(|f| {
        f.border_t_1()
            .border_color(theme::surface_border())
            .bg(theme::alpha(theme::SURFACE_HOVER, 0.70))
    }))
}

/// `dialog_panel(...)` + full-screen backdrop, ready to be a top-level
/// screen child. See this module's doc comment for the caller contract.
// Not yet called by anything in this crate — see this module's doc comment
// (`screens/gallery.rs` uses the inline `dialog_panel(...)` preview instead,
// since it has no open/closed state to drive a real overlay). Kept as
// forward-looking facade surface for the first real caller.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn dialog_overlay(
    width: gpui::Pixels,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    content: Option<Div>,
    footer: Option<Div>,
    on_close: impl Fn(&ClickEvent, &mut gpui::Window, &mut App) + 'static + Clone,
) -> Stateful<Div> {
    div()
        .id("mds-dialog-overlay")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x000000).opacity(0.5))
        .on_click({
            let on_close = on_close.clone();
            move |ev, window, cx| on_close(ev, window, cx)
        })
        .child(
            // Stop the backdrop's own click-to-close from firing when the
            // click actually landed inside the panel — gpui has no
            // `stopPropagation` primitive wired here yet, so this relies on
            // the panel's own close button being the only way to dismiss
            // rather than swallowing the click; a click-through backdrop
            // gap is an honest known limitation, not silently patched over.
            dialog_panel(width, title, description, content, footer, Some(on_close)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_puts_primary_first() {
        assert!(primary_button_leads_on("windows"));
    }

    #[test]
    fn macos_and_linux_put_primary_last() {
        // macOS and GNOME/Linux agree: confirm on the right, Cancel on the
        // left (research doc §5) — everything that ISN'T Windows takes this
        // branch, so an unrecognized/future `os` string fails safe into the
        // macOS/GNOME-majority behavior rather than silently adopting
        // Windows' reversed order.
        assert!(!primary_button_leads_on("macos"));
        assert!(!primary_button_leads_on("linux"));
        assert!(!primary_button_leads_on("some-future-os"));
    }
}
