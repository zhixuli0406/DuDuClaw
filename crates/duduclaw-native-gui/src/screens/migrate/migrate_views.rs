// Sibling of `migrate.rs` — the three per-step content panels (see that
// file's own header comment for the full RPC/canvas-fidelity rationale).
// Split out purely for this crate's own <800-line-per-file convention.
//
// Every function here takes OWNED/cloned data (never `&MigrateState`) —
// `migrate::render` clones the small snapshot it needs out of the
// `cx.global::<MigrateState>()` borrow before calling into this module,
// because a live global borrow cannot coexist with the `&mut
// Context<RootView>` these functions' own `cx.listener(...)` closures need.

use gpui::{div, prelude::*, px, Context, Div, SharedString};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, BadgeVariant, ButtonVariant};
use crate::screens::migrate::{start_scan, MigrateItemRow, MigrateResultData, MigrateState, MigrateStep};
use crate::theme;
use crate::RootView;

/// zh-TW display label for a `MigrateItem.kind` wire value — the real field
/// is an English token (`agent`/`skill`/`memory`/`session`/...,
/// `crates/duduclaw-cli/src/migrate_from/*.rs`'s own item-kind vocabulary,
/// cross-checked against `web/src/pages/MigratePage.tsx`'s `KIND_ICONS`
/// map). An unmapped kind renders verbatim rather than a raw i18n key —
/// this crate's "unwired id degrades to the raw value, never a crash or a
/// fabricated label" convention.
fn kind_label(locale: Locale, kind: &str) -> SharedString {
    let key = match kind {
        "agent" | "agents" => Some("migrate.kind.agent"),
        "skill" | "skills" => Some("migrate.kind.skill"),
        "memory" => Some("migrate.kind.memory"),
        "session" => Some("migrate.kind.session"),
        "channel" | "channel_token" => Some("migrate.kind.channel"),
        "cron" => Some("migrate.kind.cron"),
        "task" | "tasks" => Some("migrate.kind.task"),
        "model" => Some("migrate.kind.model"),
        "persona" | "soul" => Some("migrate.kind.persona"),
        "wiki" | "company" => Some("migrate.kind.wiki"),
        "api_key" => Some("migrate.kind.apiKey"),
        _ => None,
    };
    match key {
        Some(k) => i18n::t(locale, k),
        None => SharedString::from(kind.to_string()),
    }
}

fn status_badge(locale: Locale, status: &str) -> Div {
    match status {
        "imported" => badge(i18n::t(locale, "migrate.status.imported"), BadgeVariant::Success),
        "partial" => badge(i18n::t(locale, "migrate.status.partial"), BadgeVariant::Warning),
        "conflict" => badge(i18n::t(locale, "migrate.status.conflict"), BadgeVariant::Destructive),
        "skipped" => badge(i18n::t(locale, "migrate.status.skipped"), BadgeVariant::Secondary),
        other => badge(SharedString::from(other.to_string()), BadgeVariant::Outline),
    }
}

// ── Step 1: select source ────────────────────────────────────────────────

pub(super) fn select_source_view(locale: Locale, scanning: bool, error: Option<String>, cx: &mut Context<RootView>) -> Div {
    div().flex().flex_col().gap_4().child(
        div()
            .rounded(px(theme::RADIUS_XL))
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "migrate.source.label")))
                    .child(div().font_family("SF Mono").text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("~/.claude")),
            )
            .children(error.map(|e| {
                div()
                    .rounded(px(theme::RADIUS_LG))
                    .bg(theme::alpha(theme::DESTRUCTIVE, 0.10))
                    .p_3()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
                    .child(SharedString::from(e))
            }))
            .child(div().flex().justify_end().child(button(
                "migrate-start-scan",
                i18n::t(locale, if scanning { "migrate.scanning" } else { "migrate.action.startScan" }),
                ButtonVariant::Primary,
                scanning,
                None,
                cx.listener(|this, _ev, _window, cx| start_scan(this, cx)),
            ))),
    )
}

// ── Step 2: preview scan results (this task's own "展示重點") ────────────

fn summary_tile(label: SharedString, value: i64, value_color: u32) -> Div {
    div()
        .flex_1()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .px_3p5()
        .py_2p5()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(div().mt_0p5().text_size(px(22.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(value_color, 1.0)).font_family("SF Mono").child(value.to_string()))
}

fn item_header_row(locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .px_4()
        .py_2()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(div().w(px(60.)).flex_shrink_0().child(i18n::t(locale, "migrate.col.kind")))
        .child(div().flex_1().child(i18n::t(locale, "migrate.col.name")))
        .child(div().w(px(76.)).flex_shrink_0().child(i18n::t(locale, "migrate.col.status")))
        .child(div().w(px(200.)).flex_shrink_0().child(i18n::t(locale, "migrate.col.reason")))
}

fn item_row(locale: Locale, item: &MigrateItemRow, is_last: bool) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .gap_2p5()
        .px_4()
        .py_2()
        .child(div().w(px(60.)).flex_shrink_0().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(kind_label(locale, &item.kind)))
        .child(div().flex_1().min_w_0().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(item.name.clone())))
        .child(div().w(px(76.)).flex_shrink_0().child(status_badge(locale, &item.status)))
        .child(
            div()
                .w(px(200.))
                .flex_shrink_0()
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(item.reason.clone().map(SharedString::from).unwrap_or_else(|| SharedString::from("—"))),
        );
    if !is_last {
        row = row.border_b_1().border_color(theme::border());
    }
    row
}

pub(super) fn preview_view(locale: Locale, scan: Option<MigrateResultData>, cx: &mut Context<RootView>) -> Div {
    let Some(scan) = scan else {
        return div().child(empty_state("📦", i18n::t(locale, "migrate.items.empty"), None, None::<Div>));
    };

    let source_bar = div()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .px_4()
        .py_2p5()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "migrate.source.label")))
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(if scan.source.is_empty() { SharedString::from("~/.claude") } else { SharedString::from(scan.source.clone()) }),
        );

    let tiles = div()
        .flex()
        .gap_2p5()
        .child(summary_tile(i18n::t(locale, "migrate.summary.imported"), scan.summary.imported, theme::SUCCESS))
        .child(summary_tile(i18n::t(locale, "migrate.summary.partial"), scan.summary.partial, theme::WARNING))
        .child(summary_tile(i18n::t(locale, "migrate.summary.skipped"), scan.summary.skipped, theme::MUTED_FOREGROUND))
        .child(summary_tile(i18n::t(locale, "migrate.summary.conflict"), scan.summary.conflict, theme::DESTRUCTIVE));

    let items_box = if scan.items.is_empty() {
        div()
            .rounded(px(theme::RADIUS_XL))
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .child(empty_state("📦", i18n::t(locale, "migrate.items.empty"), None, None::<Div>))
    } else {
        let n = scan.items.len();
        let mut box_ = div()
            .rounded(px(theme::RADIUS_XL))
            .overflow_hidden()
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .child(
                div()
                    .text_size(px(theme::TEXT_SM))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                    .px_4()
                    .py_2p5()
                    .border_b_1()
                    .border_color(theme::border())
                    .child(i18n::t(locale, "migrate.items.title")),
            )
            .child(item_header_row(locale));
        for (i, item) in scan.items.iter().enumerate() {
            box_ = box_.child(item_row(locale, item, i + 1 == n));
        }
        box_
    };

    // Rename-on-conflict indicator — static/decorative, not a live toggle:
    // `migrate.apply` (the RPC parameter this would configure) is
    // assembled-not-wired on step 3, so a real switch here would visibly
    // control nothing. Reproduces the canvas's own "on" visual verbatim as
    // the recommended default when conflicts exist.
    let rename_hint = (scan.summary.conflict > 0).then(|| {
        div()
            .rounded(px(theme::RADIUS_XL))
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .p_3p5()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "migrate.rename.title")))
                    .child(
                        div()
                            .mt_0p5()
                            .text_size(px(11.))
                            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                            .child(i18n::tn(locale, "migrate.rename.hint", &[("n", &scan.summary.conflict.to_string())])),
                    ),
            )
            .child(
                div()
                    .w(px(34.))
                    .h(px(19.))
                    .rounded_full()
                    .bg(theme::alpha(theme::BRAND, 1.0))
                    .child(div().mt(px(2.)).ml(px(17.)).size(px(15.)).rounded_full().bg(theme::alpha(theme::SURFACE, 1.0))),
            )
    });

    let nav_row = div()
        .flex()
        .items_center()
        .justify_between()
        .pt_1()
        .child(
            div()
                .id("migrate-back-to-source")
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child("←")
                .child(i18n::t(locale, "migrate.action.backToSource"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<MigrateState>().step = MigrateStep::SelectSource;
                    cx.notify();
                })),
        )
        .child(
            div()
                .id("migrate-view-apply")
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::BRAND, 0.85)))
                .child(i18n::t(locale, "migrate.action.viewApply"))
                .child("→")
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<MigrateState>().step = MigrateStep::Apply;
                    cx.notify();
                })),
        );

    div().flex().flex_col().gap_3p5().child(source_bar).child(tiles).child(items_box).children(rename_hint).child(nav_row)
}

// ── Step 3: apply — assembled, not wired; CLI path noted per this task's
// own brief ("執行套用決策類組裝不真按＋註明 CLI 路徑") ──────────────────

pub(super) fn apply_view(locale: Locale, scan: Option<MigrateResultData>, cx: &mut Context<RootView>) -> Div {
    let recap = scan.map(|scan| {
        div()
            .rounded(px(theme::RADIUS_XL))
            .bg(theme::alpha(theme::SURFACE, 1.0))
            .border_1()
            .border_color(theme::surface_border())
            .p_4()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "migrate.verdict.label")))
            .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(scan.verdict)))
    });

    div()
        .flex()
        .flex_col()
        .gap_3p5()
        .children(recap)
        .child(
            div()
                .rounded(px(theme::RADIUS_XL))
                .bg(theme::alpha(theme::WARNING, 0.08))
                .border_1()
                .border_color(theme::alpha(theme::WARNING, 0.3))
                .p_4()
                .flex()
                .flex_col()
                .gap_2p5()
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "migrate.apply.notAvailable")))
                .child(
                    div()
                        .rounded(px(theme::RADIUS_MD))
                        .bg(theme::alpha(theme::MUTED, 0.5))
                        .px_3()
                        .py_2()
                        .font_family("SF Mono")
                        .text_size(px(11.5))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child("duduclaw migrate-from claude-code --apply"),
                )
                .child(button("migrate-apply-inert", i18n::t(locale, "migrate.action.apply"), ButtonVariant::Primary, true, None, |_ev, _window, _cx| {})),
        )
        .child(
            div()
                .id("migrate-back-to-preview")
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child("←")
                .child(i18n::t(locale, "migrate.action.backToPreview"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<MigrateState>().step = MigrateStep::Preview;
                    cx.notify();
                })),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_label_maps_known_kinds_and_falls_back_to_raw_for_unknown() {
        assert_eq!(kind_label(Locale::ZhTw, "agent"), kind_label(Locale::ZhTw, "agents"));
        assert_eq!(kind_label(Locale::ZhTw, "totally_unknown_kind").as_ref(), "totally_unknown_kind");
    }
}
