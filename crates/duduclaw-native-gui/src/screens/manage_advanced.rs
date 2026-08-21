// S5b1-A (2026-08-21) — "進階設定殼" (`/manage/advanced`, `nav.rs` id
// `manageAdvanced`). Visual authority: `commercial/design/duduclaw-s5-
// settings-pages/ManageAdvanced.dc.html` — B5 single-column boxed-list, one
// section title + 4 grouped boxed-lists (帳務與授權 / 存取與安全 / 維運 /
// 其他), the canvas's own "沿用 boxed-list 語彙而非型錄式卡牆" self-note
// (not a card grid like `web/src/pages/ManageAdvancedPage.tsx`'s `sm:grid-
// cols-2` layout — per this task's "版面禁抄 web" rule, `web` is a
// functional cross-reference only).
//
// ── Deviation from the task brief's "四群組 12 項" ───────────────────────
// The task brief describes this page as 12 rows; the APPROVED canvas itself
// (authoritative per this task's own priority rule — "視覺...拍板畫布，逐位
// 可抄") renders 14 rows across the same 4 groups: 帳務與授權 (帳務/授權/
// 經銷, 3) + 存取與安全 (安全/安全審計/治理/成員/部門, 5) + 維運 (日誌/可靠
// 性/模型用量/本地模型市集/資料搬家, 5) + 其他 (系統設定, 1). This file
// follows the canvas's literal row count, not the brief's summary number —
// an honest, flagged deviation, not a silent "fix" of the brief or a silent
// drop of 2 real canvas rows to force the count to match.
//
// ── Every row is inert this round ────────────────────────────────────────
// None of the 14 target pages exist in `nav.rs` yet — all 14 are S6-wave
// scope per `commercial/docs/TODO-native-gui-page-migration-2026-08.md`'s
// "S6 第三波" table (billing/license/distributors/security/secaudit/
// governance/users/departments/logs/reliability/inference/local-models/
// migrate/settings). Every row renders a trailing "即將推出" label instead
// of a chevron, and carries NO `on_click` handler — same "an inert-looking
// control is honest; a control that silently does nothing on click is a
// worse lie" precedent `screens/agents_detail.rs`'s read-only capability
// rows already establish for this crate.
//
// ── Icon glyphs, not hand-drawn stroke SVG ───────────────────────────────
// The canvas draws a distinct hand-drawn stroke-SVG icon per row. This
// crate's own `nav.rs` doc comment already scopes "real lucide-style
// iconography" as Phase 1b — every row here reuses that SAME established
// placeholder strategy (a single fixed glyph in a small muted badge,
// locale-independent) rather than inventing new SVG icon assets a single
// settings-index page shouldn't be the one to introduce.
//
// No RPC calls on this page at all — it is a pure static navigation index,
// same shape as `web/src/pages/ManageAdvancedPage.tsx` (functional
// reference only, per this task's "版面禁抄 web" rule).

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::i18n;
use crate::theme;
use crate::RootView;

struct AdvancedItem {
    glyph: char,
    title_key: &'static str,
    desc_key: &'static str,
}

const fn advanced_item(glyph: char, title_key: &'static str, desc_key: &'static str) -> AdvancedItem {
    AdvancedItem { glyph, title_key, desc_key }
}

struct AdvancedGroup {
    label_key: &'static str,
    items: &'static [AdvancedItem],
}

const BILLING_GROUP_ITEMS: &[AdvancedItem] = &[
    advanced_item('$', "manageAdvanced.item.billing.title", "manageAdvanced.item.billing.desc"),
    advanced_item('K', "manageAdvanced.item.license.title", "manageAdvanced.item.license.desc"),
    advanced_item('W', "manageAdvanced.item.distributors.title", "manageAdvanced.item.distributors.desc"),
];

const ACCESS_GROUP_ITEMS: &[AdvancedItem] = &[
    advanced_item('S', "manageAdvanced.item.security.title", "manageAdvanced.item.security.desc"),
    advanced_item('A', "manageAdvanced.item.secaudit.title", "manageAdvanced.item.secaudit.desc"),
    advanced_item('G', "manageAdvanced.item.governance.title", "manageAdvanced.item.governance.desc"),
    advanced_item('M', "manageAdvanced.item.users.title", "manageAdvanced.item.users.desc"),
    advanced_item('P', "manageAdvanced.item.departments.title", "manageAdvanced.item.departments.desc"),
];

const OPS_GROUP_ITEMS: &[AdvancedItem] = &[
    advanced_item('L', "manageAdvanced.item.logs.title", "manageAdvanced.item.logs.desc"),
    advanced_item('R', "manageAdvanced.item.reliability.title", "manageAdvanced.item.reliability.desc"),
    advanced_item('U', "manageAdvanced.item.inference.title", "manageAdvanced.item.inference.desc"),
    advanced_item('O', "manageAdvanced.item.localModels.title", "manageAdvanced.item.localModels.desc"),
    advanced_item('I', "manageAdvanced.item.migrate.title", "manageAdvanced.item.migrate.desc"),
];

const OTHER_GROUP_ITEMS: &[AdvancedItem] =
    &[advanced_item('#', "manageAdvanced.item.settings.title", "manageAdvanced.item.settings.desc")];

const GROUPS: &[AdvancedGroup] = &[
    AdvancedGroup { label_key: "manageAdvanced.group.billing", items: BILLING_GROUP_ITEMS },
    AdvancedGroup { label_key: "manageAdvanced.group.access", items: ACCESS_GROUP_ITEMS },
    AdvancedGroup { label_key: "manageAdvanced.group.ops", items: OPS_GROUP_ITEMS },
    AdvancedGroup { label_key: "manageAdvanced.group.other", items: OTHER_GROUP_ITEMS },
];

const CONTENT_MAX_WIDTH: f32 = 700.0;

fn item_glyph_badge(glyph: char) -> Div {
    div()
        .size(px(28.))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::MUTED, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(glyph.to_string())
}

fn item_row(locale: crate::i18n::Locale, item: &AdvancedItem, is_last: bool) -> Div {
    let row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(item_glyph_badge(item.glyph))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, item.title_key)),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, item.desc_key)),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8))
                .child(i18n::t(locale, "manageAdvanced.comingSoon")),
        );
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

fn group_box(locale: crate::i18n::Locale, group: &AdvancedGroup) -> Div {
    let mut rows = div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow());
    for (idx, item) in group.items.iter().enumerate() {
        rows = rows.child(item_row(locale, item, idx == group.items.len() - 1));
    }
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .px_0p5()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, group.label_key)),
        )
        .child(rows)
}

pub fn render(state: &RootView, _cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;

    let mut group_rows = Vec::with_capacity(GROUPS.len());
    for group in GROUPS {
        group_rows.push(group_box(locale, group));
    }

    let header = div()
        .child(
            div()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "nav.manageAdvanced")),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "manageAdvanced.subtitle")),
        );

    div()
        .id("manage-advanced-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_WIDTH))
                .p_6()
                .flex()
                .flex_col()
                .gap_5()
                .child(header)
                .children(group_rows),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trip-wire for this file's own header-comment claim: the approved
    /// canvas renders 14 rows across 4 groups, not the task brief's
    /// summary "12" — pins the exact count so a future edit that silently
    /// drops/adds a row updates this test (and its own justification) too.
    #[test]
    fn canvas_row_count_is_14_across_4_groups_not_12() {
        assert_eq!(GROUPS.len(), 4);
        let total: usize = GROUPS.iter().map(|g| g.items.len()).sum();
        assert_eq!(total, 14);
        assert_eq!(BILLING_GROUP_ITEMS.len(), 3);
        assert_eq!(ACCESS_GROUP_ITEMS.len(), 5);
        assert_eq!(OPS_GROUP_ITEMS.len(), 5);
        assert_eq!(OTHER_GROUP_ITEMS.len(), 1);
    }

    #[test]
    fn every_item_has_distinct_i18n_keys() {
        let mut seen = std::collections::HashSet::new();
        for group in GROUPS {
            for item in group.items {
                assert!(seen.insert(item.title_key), "duplicate title_key: {}", item.title_key);
                assert!(seen.insert(item.desc_key), "duplicate desc_key: {}", item.desc_key);
            }
        }
    }
}
