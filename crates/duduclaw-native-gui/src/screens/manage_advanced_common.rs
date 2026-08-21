// WP-S6b1-J (2026-08-21) — shared breadcrumb + LicenseShell tab-strip
// primitives for this batch's 3 "進階設定" drill-down pages (`billing.rs` /
// `license.rs` / `partner_portal.rs`). Same "duplicate, don't couple two
// unrelated batches through one shared module" boundary `settings_common.rs`'s
// own header comment draws around its four "整合" pages — this module is the
// "進階設定" analogue, scoped to exactly this batch's 3 pages. NOT shared
// with `settings_common`'s four "整合" pages (different breadcrumb root,
// different owning batch) and NOT `manage_advanced.rs` itself (that page's
// own 14-row list is a sibling package's scope this round — this task's own
// brief states "manage_advanced.rs 接線歸 L 包不歸你", so its rows stay inert
// exactly as that file already renders them; this batch only supplies the
// three LANDING pages those rows will eventually point at).
//
// Reuses `settings_common::{boxed_group, kv_row}` directly (generic
// boxed-list/row primitives with no "整合"-specific content) rather than
// duplicating them a second time — same precedent `screens::mcp_keys`
// already sets by importing `settings_common::boxed_group` while carrying
// its own local breadcrumb.

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{tabs, TabItem};
use crate::theme;
use crate::RootView;

/// "進階設定 › {page_label}" — clicking "進階設定" sets `active_page` to
/// `"manageAdvanced"` (the existing drill-down index page,
/// `screens::manage_advanced`, already wired into `shell.rs`/`nav.rs` by the
/// S5b1-A wave). `id_prefix` must be unique per call site, same contract
/// `settings_common::breadcrumb`'s own doc comment states.
pub fn breadcrumb(
    id_prefix: &'static str,
    locale: Locale,
    page_label: SharedString,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    div()
        .id(id_prefix)
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id(format!("{id_prefix}-root"))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "nav.manageAdvanced"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "manageAdvanced";
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(page_label))
}

/// The two-tab strip shared by `license.rs`/`partner_portal.rs` — the
/// canvas's own "LicenseShell" (`/app/system/license`, 授權／經銷夥伴 tabs),
/// which the batch's `Main.dc.html` cover sheet explicitly draws directly
/// atop each of the two leaf pages rather than as its own artboard (see
/// "Tabs 殼容器覆蓋方式"). `active_id` is `"license"` or `"partnerPortal"`.
pub fn license_shell_tabs(locale: Locale, active_id: &'static str, cx: &mut Context<RootView>) -> Div {
    let items = vec![
        TabItem::new(
            "license",
            i18n::t(locale, "license.tabs.license"),
            cx.listener(|this, _ev, _window, cx| {
                this.active_page = "license";
                cx.notify();
            }),
        ),
        TabItem::new(
            "partnerPortal",
            i18n::t(locale, "license.tabs.partnerPortal"),
            cx.listener(|this, _ev, _window, cx| {
                this.active_page = "partnerPortal";
                cx.notify();
            }),
        ),
    ];
    tabs(items, active_id)
}
