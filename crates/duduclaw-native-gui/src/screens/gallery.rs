// Component Library — S3 dogfood page. Renders every `mds_gpui` component
// (see that module) across its variants/states on one scrollable page, so
// the native GUI's rendering can be eyeballed side-by-side against the web
// dashboard's `web/src/components/mds/` originals for token-consistency
// review (the actual pixel comparison is left to the human reviewer per the
// task brief — this page's job is only to guarantee everything renders).
//
// Entirely static: no click handler here mutates any `RootView` field.
// Buttons/tabs/dialog-close use no-op closures so every visual state
// (hover/active/selected/disabled) can be inspected without needing this
// page to own interactive state it has no other use for — see `mds_gpui::
// button`'s doc comment for the one deliberate consequence of that (no real
// focus-ring demo, shown as a static labeled swatch instead).

use gpui::{div, prelude::*, px, Context, Div, IntoElement, SharedString, Stateful};

use crate::i18n;
use crate::mds_gpui::{
    badge, button, card, card_content, card_description, card_footer, card_header, card_title,
    dialog_button_row, dialog_panel, empty_state, skeleton, table, tabs, toast, BadgeVariant,
    ButtonVariant, TabItem, ToastVariant,
};
use crate::theme;
use crate::RootView;

fn noop(_ev: &gpui::ClickEvent, _window: &mut gpui::Window, _cx: &mut gpui::App) {}

/// Section wrapper: `TEXT_XS` uppercase-ish label (MDS "meta/label" bucket,
/// same bucket `screens/login.rs`'s `field_label` uses) + the section's
/// content, separated by a hairline so the page reads as one long list of
/// component swatches rather than an undifferentiated wall.
fn section(label: SharedString, content: Div) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .pb_6()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(label),
        )
        .child(content)
}

fn row(children: Vec<Div>) -> Div {
    div().flex().flex_wrap().items_center().gap_3().children(children)
}

fn labeled_swatch(label: &'static str, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(content)
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(label),
        )
}

fn button_section() -> Div {
    let variants = [
        ("Primary", ButtonVariant::Primary),
        ("Secondary", ButtonVariant::Secondary),
        ("Ghost", ButtonVariant::Ghost),
        ("Destructive", ButtonVariant::Destructive),
    ];
    let mut swatches = Vec::with_capacity(variants.len() * 2 + 1);
    for (name, variant) in variants {
        swatches.push(labeled_swatch(
            name,
            button(format!("gallery-btn-{name}"), name, variant, false, None, noop),
        ));
    }
    swatches.push(labeled_swatch(
        "Disabled",
        button("gallery-btn-disabled", "Disabled", ButtonVariant::Primary, true, None, noop),
    ));
    // Focus-ring: static color preview, not a live keyboard-focus demo — see
    // `mds_gpui::button`'s doc comment for why this page can't wire a real
    // one (no persistent `FocusHandle` for a stateless swatch grid).
    swatches.push(labeled_swatch(
        "Focus ring (color preview)",
        div()
            .h(px(32.))
            .px_2p5()
            .flex()
            .items_center()
            .rounded(px(theme::RADIUS_LG))
            .bg(theme::alpha(theme::BRAND, 1.0))
            .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
            .text_size(px(theme::TEXT_SM))
            .border_2()
            .border_color(theme::alpha(theme::RING, 1.0))
            .child("Primary"),
    ));
    section("Button".into(), row(swatches))
}

fn card_section() -> Div {
    let c = card()
        .w(px(320.))
        .child(card_header().child(card_title("方案設定")).child(card_description("卡片描述文字放這裡")))
        .child(
            card_content()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("Card content 區塊——任何內容都可以放這裡。"),
                ),
        )
        .child(
            card_footer()
                .justify_end()
                .gap_2()
                .child(button("gallery-card-cancel", "取消", ButtonVariant::Ghost, false, None, noop))
                .child(button("gallery-card-confirm", "確認", ButtonVariant::Primary, false, None, noop)),
        );
    section("Card".into(), div().child(c))
}

fn badge_section() -> Div {
    let variants = [
        ("Default", BadgeVariant::Default),
        ("Secondary", BadgeVariant::Secondary),
        ("Success", BadgeVariant::Success),
        ("Warning", BadgeVariant::Warning),
        ("Info", BadgeVariant::Info),
        ("Destructive", BadgeVariant::Destructive),
        ("Outline", BadgeVariant::Outline),
    ];
    let swatches = variants.into_iter().map(|(name, v)| badge(name, v)).collect();
    section("Badge".into(), row(swatches))
}

fn tabs_section() -> Div {
    let items = vec![
        TabItem::new("overview", "總覽", noop),
        TabItem::new("activity", "活動", noop),
        TabItem::new("settings", "設定", noop),
    ];
    // Static demo — "activity" is shown selected so both selected and
    // unselected label/underline states are visible at once.
    section("Tabs".into(), div().w(px(320.)).child(tabs(items, "activity")))
}

fn table_section() -> Div {
    let headers: Vec<SharedString> = vec!["員工".into(), "狀態".into(), "任務數".into()];
    let rows: Vec<Vec<SharedString>> = vec![
        vec!["小夜".into(), "在線".into(), "3".into()],
        vec!["小晨".into(), "忙碌".into(), "7".into()],
        vec!["小安".into(), "離線".into(), "0".into()],
    ];
    section("Table".into(), table(&headers, &rows))
}

fn dialog_section() -> Div {
    // Static preview — see `mds_gpui::dialog`'s doc comment for why this
    // page renders `dialog_panel(...)` inline instead of the full-screen
    // `dialog_overlay(...)` (no dialog-open state exists on this page).
    //
    // P0-3: button order now goes through `dialog_button_row` instead of a
    // single hardcoded `.child(cancel).child(confirm)` — on this machine
    // (macOS) the rendered order is unchanged (confirm right, Cancel left),
    // but a Windows build now correctly renders primary-left instead of
    // silently inheriting the macOS/GNOME convention.
    let footer = dialog_button_row(
        button("gallery-dialog-confirm", "刪除", ButtonVariant::Destructive, false, None, noop),
        button("gallery-dialog-cancel", "取消", ButtonVariant::Ghost, false, None, noop),
    );
    let panel = dialog_panel(
        px(360.),
        "確定要刪除這個項目嗎？",
        Some("此動作無法復原。".into()),
        None,
        Some(footer),
        Some(noop),
    );
    section("Dialog".into(), div().child(panel))
}

fn toast_section() -> Div {
    let variants = [
        (ToastVariant::Default, "已儲存", "設定已更新"),
        (ToastVariant::Success, "部署成功", "新版本已上線"),
        (ToastVariant::Warning, "配額即將用盡", "本月用量已達 80%"),
        (ToastVariant::Destructive, "連線失敗", "無法連線到 gateway"),
    ];
    let cards: Vec<Div> = variants
        .into_iter()
        .map(|(v, title, desc)| toast(v, title, Some(desc.into())))
        .collect();
    section("Toast".into(), div().flex().flex_col().gap_2().children(cards))
}

fn empty_state_section() -> Div {
    let cta = button("gallery-empty-cta", "新增任務", ButtonVariant::Primary, false, None, noop);
    section(
        "EmptyState".into(),
        empty_state("📭", "還沒有任務", Some("建立第一個任務開始使用任務看板。".into()), Some(cta)),
    )
}

fn skeleton_section() -> Div {
    let row_preview = div()
        .flex()
        .items_center()
        .gap_2p5()
        .child(skeleton(px(32.), px(32.)).rounded_full())
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(skeleton(px(160.), px(12.)))
                .child(skeleton(px(100.), px(12.))),
        );
    section("Skeleton".into(), row_preview)
}

/// `cx` isn't needed by this fully-static page (every interactive swatch
/// uses the no-op `noop` handler, which doesn't close over `Context<
/// RootView>` at all) — kept in the signature only so `shell.rs` can call
/// this the same way it calls every other `screens::*::render` fn.
pub fn render(state: &RootView, _cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;

    div()
        // `.overflow_y_scroll()` lives on `StatefulInteractiveElement`
        // (needs `.id(...)` first — same gotcha as `.on_click()`, see
        // main.rs's gpui gotcha list), hence the `id()` and the `Stateful
        // <Div>` return type instead of the plain `Div` every other
        // `screens::*::render` fn returns.
        //
        // `flex_1` (not `size_full`) so this pane claims the REMAINING
        // height inside `shell.rs`'s `flex_col` content area rather than
        // trying to be 100% of its own auto-sized box — that bounded height
        // is what makes `overflow_y_scroll()` below actually clip+scroll
        // instead of just growing the whole content area unboundedly.
        .id("gallery-content")
        .flex_1()
        .w_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_6()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.gallery.title")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.gallery.subtitle")),
                ),
        )
        .child(button_section())
        .child(card_section())
        .child(badge_section())
        .child(tabs_section())
        .child(table_section())
        .child(dialog_section())
        .child(toast_section())
        .child(empty_state_section())
        .child(skeleton_section())
}
