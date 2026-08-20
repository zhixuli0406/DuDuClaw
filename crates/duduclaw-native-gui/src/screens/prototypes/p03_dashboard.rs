// p03 — 儀表板。Copilot prompt-first（頂部大輸入框為主體，下方吐建議
// chips）＋ Windows 11 Settings Home 式互動卡（每卡＝狀態＋單一動作，≤6
// 張）＋ Steam Library Home 式「最近活動」shelf 一列。刻意不放任何圖表——
// 三種原生首頁形態裡，圖表分頁制（Task Manager/Mission Center）屬於
// /reports 頁的職責，不是首頁（見處方原文「不放圖表（圖表分頁制留給
// reports）」）。

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::mds_gpui::{button, ButtonVariant};
use crate::screens::prototypes::common::{avatar, noop, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

struct DashCard {
    icon: char,
    icon_color: u32,
    title: &'static str,
    detail: &'static str,
    action: &'static str,
}

const CARDS: &[DashCard] = &[
    DashCard { icon: '✅', icon_color: theme::WARNING, title: "待審批", detail: "3 件等你決定", action: "前往收件匣" },
    DashCard { icon: '💰', icon_color: theme::INFO, title: "本月 API 預算", detail: "已用 62%（NT$3,720 / 6,000）", action: "查看用量" },
    DashCard {
        icon: '🎯',
        icon_color: theme::SUCCESS,
        title: "目標進度",
        detail: "「每日營收日報自動化」第 3/5 輪，卡在需要你",
        action: "查看目標",
    },
    DashCard { icon: '📡', icon_color: theme::SUCCESS, title: "通道健康", detail: "8 / 8 上線", action: "查看通道" },
    DashCard { icon: '👥', icon_color: theme::WARNING, title: "AI 員工", detail: "5 位在線、1 位忙碌", action: "查看員工" },
    DashCard { icon: '📋', icon_color: theme::DESTRUCTIVE, title: "任務看板", detail: "今天到期 4 項", action: "查看任務" },
];

const SUGGESTIONS: &[&str] = &["幫我整理今天的待辦", "檢查通道健康", "產生本週報表", "新增一個目標"];

struct ActivityItem {
    who: &'static str,
    what: &'static str,
    when: &'static str,
}

const ACTIVITY: &[ActivityItem] = &[
    ActivityItem { who: "小杜", what: "完成「每日營收日報」第 2 輪", when: "5 分鐘前" },
    ActivityItem { who: "客服組長", what: "回覆了 3 則客戶訊息", when: "18 分鐘前" },
    ActivityItem { who: "財務助理", what: "產出「本月結算」報表", when: "1 小時前" },
    ActivityItem { who: "出貨助理", what: "更新了 12 筆訂單狀態", when: "2 小時前" },
];

fn chip(label: &'static str) -> Stateful<Div> {
    div()
        .id(label)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .bg(theme::alpha(theme::SECONDARY, 1.0))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::SECONDARY_FOREGROUND, 1.0))
        .cursor_pointer()
        .hover(|style| style.bg(theme::alpha(theme::SECONDARY, 0.8)))
        .child(label)
}

fn dash_card(card: &DashCard) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme::alpha(card.icon_color, 0.14))
                        .text_size(px(14.))
                        .child(card.icon.to_string()),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(card.title),
                ),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(card.detail),
        )
        .child(button(
            format!("p03-card-{}", card.title),
            card.action,
            ButtonVariant::Secondary,
            false,
            None,
            noop,
        ))
}

fn activity_card(item: &ActivityItem) -> Div {
    div()
        .w(px(220.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(avatar(item.who.chars().next().unwrap_or('•'), theme::BRAND, 22.))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(item.who),
                ),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(item.what),
        )
        .child(div().text_size(px(10.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(item.when))
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let mut card_rows = Vec::with_capacity(CARDS.len());
    for c in CARDS {
        card_rows.push(dash_card(c));
    }
    let mut chip_rows = Vec::with_capacity(SUGGESTIONS.len());
    for s in SUGGESTIONS {
        chip_rows.push(chip(s));
    }
    let mut activity_rows = Vec::with_capacity(ACTIVITY.len());
    for a in ACTIVITY {
        activity_rows.push(activity_card(a));
    }

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(
        div()
            .id("p03-scroll")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_5()
            .p_6()
            // ── Prompt-first 輸入框 ──────────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(
                        div()
                            .h(px(56.))
                            .px_4()
                            .flex()
                            .items_center()
                            .rounded(px(theme::RADIUS_XL))
                            .bg(theme::alpha(theme::SURFACE, 1.0))
                            .border_1()
                            .border_color(theme::input_border())
                            .shadow(theme::surface_shadow())
                            .text_size(px(theme::TEXT_BASE))
                            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                            .child("跟你的 AI 員工說句話……"),
                    )
                    .child(div().flex().flex_wrap().gap_2().children(chip_rows)),
            )
            // ── ≤6 張互動卡 ──────────────────────────────────────────
            .child(
                div()
                    .grid()
                    .grid_cols(3)
                    .gap_3()
                    .children(card_rows),
            )
            // ── 最近活動 shelf ───────────────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(theme::TEXT_XS))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                            .child("最近活動"),
                    )
                    .child(div().flex().gap_3().children(activity_rows)),
            ),
    )
}
