// Y20-P3 (2026-08-29) — real Confirm step, replacing the P2 "開發中"
// placeholder. Summarizes exactly what's about to happen (the disk picked
// on `DiskSelect`, `flow.selected_disk()`) plus an explicit destructive-write
// warning, and gates the only way past this step — a checkbox that must be
// ticked — behind `LiveInstallFlow::can_advance` (P2 left every step's gate
// unconditionally `true`; see that method's own doc comment in `state.rs`
// for the P3 change).
//
// The task brief offered either a checkbox OR a typed-disk-name field as the
// confirmation gate; this uses only the checkbox — a single explicit click
// is the smaller surface for a step whose only job is "make the operator
// commit to this on purpose", and `step_button`'s existing `disabled`
// semantics already give the gate its enabled/disabled visual for free once
// wired to the same boolean.
//
// The actual "開始安裝" action lives in `render.rs`'s shared bottom-nav slot
// (`install_runner::start_install`), NOT a second button inside this step's
// own card. With the checkbox gating `can_advance` exactly like every other
// step's forward action, the existing bottom-nav Continue button IS the
// "開始安裝" button once relabeled (see `render.rs`'s own header comment) —
// with zero risk of two differently-wired buttons on the same screen racing
// each other.

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use duduclaw_native_gui::theme;

use crate::oobe::widgets;
use crate::palette::ShellPalette;
use crate::ShellView;

use super::super::LiveInstallFlow;

pub(super) fn render(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Div {
    let palette = flow.palette();

    let body = div().flex().flex_col().gap(px(14.)).child(target_summary(flow, palette)).child(warning_banner(palette)).child(confirm_checkbox_row(flow, cx));

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title("確認安裝 · Confirm installation", palette))
        .child(widgets::subtitle("這是不可逆的操作，請詳閱後勾選確認 · This is irreversible — please confirm below", palette))
        .child(widgets::card(body, palette))
}

fn target_summary(flow: &LiveInstallFlow, palette: ShellPalette) -> Div {
    match flow.selected_disk() {
        Some(disk) => {
            let detail = if disk.model.is_empty() { disk.size.clone() } else { format!("{}  ·  {}", disk.size, disk.model) };
            // Bug fix (DESIGN-installer-settings-integration-2026-08.md §6): same
            // undefined-width flex_col-child overflow as `warning_banner` below —
            // this div is a direct child of `body`'s flex_col with no explicit
            // width, and `disk.model` from lsblk can be an arbitrarily long
            // hardware string. `.w_full()` gives it a real width to wrap inside.
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_size(px(theme::TEXT_SM)).child(format!("目標磁碟 · Target disk：/dev/{}", disk.name)))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(detail))
        }
        None => div()
            .w_full()
            .text_size(px(theme::TEXT_SM))
            .text_color(theme::alpha(palette.destructive, 1.0))
            .child("尚未選擇磁碟，請返回上一步 · No disk selected — go back"),
    }
}

fn warning_banner(palette: ShellPalette) -> Div {
    div()
        // Bug fix (DESIGN-installer-settings-integration-2026-08.md §6): without
        // an explicit width the banner shrinks to its text's natural size inside
        // the flex-col parent, so the bilingual warning line overflows `widgets::card`
        // instead of wrapping inside the card's own bounds.
        .w_full()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.destructive, 0.12))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(palette.destructive, 1.0))
        .child("警告：該磁碟所有資料將被清除且無法復原 · Warning: all data on this disk will be permanently erased")
}

/// A hand-rolled checkbox — a small border square that fills solid + shows
/// a "✓" glyph once checked, same "plain text glyph, no icon asset" fallback
/// this crate already accepts elsewhere (`oobe::steps::network`'s own
/// "需密碼" badge documents the same tradeoff for a missing icon asset).
/// Click toggles `LiveInstallFlow::confirm_checked` directly — there is no
/// separate "submit" step; the checkbox state itself IS what `can_advance`
/// reads.
fn confirm_checkbox_row(flow: &LiveInstallFlow, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let palette = flow.palette();
    let checked = flow.confirm_checked();
    let disk_ready = flow.selected_disk().is_some();

    let on_click = cx.listener(move |view, _ev, _window, cx| {
        if let Some(flow) = view.live_install.as_mut() {
            let next = !flow.confirm_checked();
            flow.set_confirm_checked(next);
        }
        cx.notify();
    });

    let mut box_el = div()
        .w(px(18.))
        .h(px(18.))
        .flex_none()
        .rounded(px(4.))
        .border_1()
        .border_color(if checked { theme::alpha(palette.brand, 1.0) } else { palette.surface_border })
        .bg(theme::alpha(if checked { palette.brand } else { palette.surface }, 1.0))
        .flex()
        .items_center()
        .justify_center();
    if checked {
        box_el = box_el.child(div().text_size(px(12.)).text_color(theme::alpha(palette.brand_foreground, 1.0)).child("✓"));
    }

    let mut row = div()
        .id("live-install-confirm-checkbox")
        .flex()
        // items_start (not items_center): once the text below wraps to a second
        // line, center-alignment would float the checkbox to the vertical middle
        // of both lines instead of lining up with the first line's glyph.
        .items_start()
        .gap(px(10.))
        .py(px(4.))
        .child(box_el)
        .child(
            // Bug fix (DESIGN-installer-settings-integration-2026-08.md §6): flex
            // row's classic `min-width:auto` overflow — a text child with no
            // `min_w(px(0.))` refuses to shrink below its content width, so this
            // bilingual line pushes past `widgets::card`'s edge instead of wrapping.
            // Template: `settings/widgets.rs` `value_row` (`.flex_1().min_w(px(0.))`).
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(theme::TEXT_SM))
                .child("我了解此操作將清除目標磁碟上的所有資料 · I understand this erases all data on the target disk"),
        );

    if disk_ready {
        row = row.cursor_pointer().on_click(on_click);
    } else {
        // No disk to confirm against yet — same "visually inert while a
        // precondition is missing" treatment `steps::network::wifi_row`'s
        // own busy state uses, not a disappearing control.
        row = row.opacity(0.55);
    }
    row
}

#[cfg(test)]
mod tests {
    // DESIGN-installer-settings-integration-2026-08.md §6: source-scan
    // regression guard for the text-overflow fix — same "crude but
    // load-bearing" shape `live_install/render.rs`'s and `oobe/steps/
    // account.rs`'s own test modules already use for behavior a gpui
    // `cx.listener` closure can't otherwise be driven from without a live
    // window (see those files' own header comments).

    #[test]
    fn confirm_checkbox_text_has_the_min_width_zero_overflow_fix() {
        let source = include_str!("confirm.rs");
        let start = source.find("fn confirm_checkbox_row(").expect("confirm_checkbox_row not found in confirm.rs");
        let window = &source[start..(start + 2000).min(source.len())];
        assert!(
            window.contains(".flex_1()") && window.contains(".min_w(px(0.))"),
            "the checkbox row's text child must keep `.flex_1().min_w(px(0.))` — \
             without it, flex row's default `min-width: auto` refuses to shrink the \
             bilingual confirmation text below its content width, and it overflows \
             `widgets::card` instead of wrapping (DESIGN-installer-settings-\
             integration-2026-08.md §6)"
        );
    }
}
