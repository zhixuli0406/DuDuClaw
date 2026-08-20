// p10 — 任務詳情。header（狀態圈＋標題＋meta）＋分段捲動（描述／產物／過程
// timeline）＋右屬性 inspector；刻意不用 web 的 tabs 分頁（處方原文：「不用
// web tabs」）——三段內容用一條長捲動頁面呈現，跟 macOS Split view 的
// detail pane 慣例一致（選取→內容區直接顯示，不用再點一次分頁）。

use gpui::{div, prelude::*, px, Context, Div};

use crate::mds_gpui::{badge, BadgeVariant};
use crate::screens::prototypes::common::{
    boxed_group, kv_row, meta_label, status_dot, stage, STATUS_RUNNING, STAGE_HEIGHT,
};
use crate::theme;
use crate::RootView;

fn artifact_chip(name: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(30.))
        .px_2p5()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
        .child("📄")
        .child(name)
}

fn timeline_entry(time: &'static str, text: &'static str) -> Div {
    div()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(70.))
                .flex_shrink_0()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(time),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(text),
        )
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let header = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_5()
        .border_b_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(status_dot(STATUS_RUNNING, 14.))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("整理本週客訴摘要"),
                ),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("指派給 客服組長 · 建立於 2 天前 · 優先度 高"),
        );

    let body = div()
        .id("p10-body")
        .flex_1()
        .flex()
        .flex_col()
        .gap_5()
        .p_5()
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(meta_label("描述"))
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("彙整本週所有客訴管道（LINE/Email/電話紀錄）成一份摘要，標出重複出現 3 次以上的問題類型，交給產品組排優先度。"),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(meta_label("產物"))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(artifact_chip("客訴摘要_W33.md"))
                        .child(artifact_chip("原始紀錄彙整.csv")),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(meta_label("過程"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2p5()
                        .child(timeline_entry("09:12", "任務建立，指派給客服組長"))
                        .child(timeline_entry("09:40", "開始彙整 LINE 對話紀錄"))
                        .child(timeline_entry("10:05", "抓到 27 筆客訴，分類為 6 類問題"))
                        .child(timeline_entry("10:22", "產出摘要草稿，等待覆核")),
                ),
        );

    let inspector = div()
        .id("p10-inspector")
        .w(px(260.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .overflow_y_scroll()
        .border_l_1()
        .border_color(theme::surface_border())
        .child(meta_label("屬性"))
        .child(boxed_group(vec![
            kv_row("狀態", badge("進行中", BadgeVariant::Info)),
            kv_row("優先度", badge("高", BadgeVariant::Warning)),
            kv_row(
                "截止日",
                div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child("今天 18:00"),
            ),
            kv_row(
                "標籤",
                div().flex().gap_1().child(badge("客服", BadgeVariant::Secondary)).child(badge("本週", BadgeVariant::Secondary)),
            ),
        ]));

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().flex().child(body).child(inspector)),
    )
}
