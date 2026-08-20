// p06 — 對話紀錄。這頁不是一個獨立的產品頁面設計，是把「/conversations 併
// 入 /chat 左欄」這個設計提案的兩種呈現方式並排展示，供拍板時比較（處方原
// 文：「獨立原型呈現『併入 chat 左欄』提案的展開態，標註合併理由」）。
//
// 上半＝p04 左欄的「展開態」（同一份會話清單，但寬度放大、每列補上原本兩行
// 卡列裝不下的中繼資料——AI 員工/最後訊息/更新時間/未讀），示範「不用開一
// 個新頁，把 /chat 左欄拉寬就是 /conversations」。
// 下半＝重度檢索場景的替代模式——寬表格，一行一會話，方便排序/掃視大量會
// 話（研究文件對收件匣清單的兩派做法之一：「寬表格（重度排序場景，皆可切
// 換）」，這裡借用同一個模式）。

use gpui::{div, prelude::*, px, Context, Div, SharedString};

use crate::mds_gpui::table;
use crate::screens::prototypes::common::{avatar, meta_label, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

struct Row {
    name: &'static str,
    letter: char,
    color: u32,
    last_message: &'static str,
    updated: &'static str,
    unread: bool,
}

const ROWS: &[Row] = &[
    Row { name: "小杜", letter: '杜', color: theme::BRAND, last_message: "好的，這週 retention 數字整理好了", updated: "5 分鐘前", unread: true },
    Row { name: "客服組長", letter: '客', color: theme::WARNING, last_message: "有 3 則客戶訊息待回覆", updated: "18 分鐘前", unread: true },
    Row { name: "財務助理", letter: '財', color: theme::SUCCESS, last_message: "本月結算報表已產出", updated: "1 小時前", unread: false },
    Row { name: "出貨助理", letter: '出', color: theme::INFO, last_message: "12 筆訂單狀態已更新", updated: "2 小時前", unread: false },
];

fn wide_row(r: &Row) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .h(px(48.))
        .rounded(px(theme::RADIUS_LG))
        .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(avatar(r.letter, r.color, 30.))
        .child(
            div()
                .w(px(100.))
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(r.name),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(r.last_message),
        )
        .child(
            div()
                .w(px(90.))
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(r.updated),
        )
        .child(if r.unread {
            div().size(px(8.)).rounded_full().bg(theme::alpha(theme::BRAND, 1.0))
        } else {
            div().size(px(8.))
        })
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let mut wide_rows = Vec::with_capacity(ROWS.len());
    for r in ROWS {
        wide_rows.push(wide_row(r));
    }

    let headers: Vec<SharedString> = vec!["AI 員工".into(), "最後訊息".into(), "更新時間".into(), "未讀".into()];
    let table_rows: Vec<Vec<SharedString>> = ROWS
        .iter()
        .map(|r| {
            vec![
                r.name.into(),
                r.last_message.into(),
                r.updated.into(),
                if r.unread { "●".into() } else { "".into() },
            ]
        })
        .collect();

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(
        div()
            .id("p06-scroll")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_6()
            .p_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(meta_label("提案一：卡列模式（/chat 左欄拉寬）"))
                    .child(
                        div()
                            .w(px(480.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_2()
                            .rounded(px(theme::RADIUS_XL))
                            .bg(theme::alpha(theme::SIDEBAR, 1.0))
                            .border_1()
                            .border_color(theme::sidebar_border())
                            .children(wide_rows),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(meta_label("提案二：寬表格模式（重度檢索場景切換用）"))
                    .child(table(&headers, &table_rows)),
            )
            .child(
                div()
                    .p_3()
                    .rounded(px(theme::RADIUS_LG))
                    .bg(theme::alpha(theme::INFO, 0.10))
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::INFO, 1.0))
                    .child("合併理由：獨立的 /conversations 清單頁與 /chat 左欄本質是同一份資料的兩種密度——拆成兩頁會讓使用者在切換會話時多繞一層導覽，直接把左欄做成可展開的寬版本即可兩者兼顧。"),
            ),
    )
}
