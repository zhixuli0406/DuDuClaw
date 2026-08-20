// p05 — 主控台。p04 的骨架變體：同一套「左會話清單＋右 transcript＋置底
// composer」，差異是 transcript 中間插入一張審批卡（核准/駁回雙鈕＋脈絡摘
// 要），對應「用自然語言操作整台機器」時系統需要人核准高風險動作的場景
// （ActionGuard maybe-irreversible 判官、HITL ApprovalBroker）。研究文件
// 對審批動作按鈕的硬規格：Apple detail view 上限 4 顆、「避免只是打開 app
// 的按鈕」——這裡只放 2 顆（核准/駁回），沒有超標。

use gpui::{div, prelude::*, px, Context, Div};

use crate::mds_gpui::{button, ButtonVariant};
use crate::screens::prototypes::common::{avatar, noop, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

fn plain_bubble(from_user: bool, text: &'static str) -> Div {
    let content = div()
        .max_w(px(340.))
        .px_3p5()
        .py_2p5()
        .rounded(px(theme::RADIUS_XL))
        .text_size(px(theme::TEXT_SM))
        .when(from_user, |el| {
            el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
        })
        .when(!from_user, |el| {
            el.bg(theme::alpha(theme::SURFACE_RAISED, 1.0)).text_color(theme::alpha(theme::FOREGROUND, 1.0))
        })
        .child(text);
    div().w_full().flex().when(from_user, |el| el.justify_end()).when(!from_user, |el| el.justify_start()).child(content)
}

/// 內嵌審批卡——本質上是一顆特殊的 assistant 訊息，靠左對齊、但用獨立卡片
// 樣式（而不是一般泡泡）標出「這則需要你決定」。
fn approval_card() -> Div {
    div()
        .w(px(400.))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::alpha(theme::WARNING, 0.4))
        .shadow(theme::surface_shadow())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_size(px(14.)).child("⚠️"))
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::alpha(theme::WARNING, 1.0))
                        .child("需要你核准：重新啟動 LINE 通道 Bot"),
                ),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("原因：連續 3 次 webhook 逾時。重啟會中斷約 10 秒的訊息接收，不會遺失既有對話紀錄。"),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(button("p05-reject", "駁回", ButtonVariant::Secondary, false, None, noop))
                .child(button("p05-approve", "核准", ButtonVariant::Primary, false, None, noop)),
        )
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let conv_list = div()
        .id("p05-conv-list")
        .w(px(220.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .border_r_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .h_8()
                .px_2()
                .flex()
                .items_center()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("會話"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .p_2p5()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0))
                .child(avatar('主', theme::CHART_2, 32.))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child("系統維運"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child("需要你核准：重啟通道"),
                        ),
                )
                .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(theme::alpha(theme::WARNING, 1.0))),
        );

    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(theme::surface_border())
        .child(avatar('主', theme::CHART_2, 24.))
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child("系統維運主控台"),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("用自然語言操作整台機器"),
        );

    let composer = div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_3()
        .border_t_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .flex_1()
                .h(px(38.))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::input_bg())
                .border_1()
                .border_color(theme::input_border())
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("下達下一個指令…"),
        )
        .child(
            div()
                .h(px(38.))
                .px_3p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::BRAND, 1.0))
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child("送出"),
        );

    let transcript = div()
        .id("p05-transcript")
        .flex_1()
        .flex()
        .flex_col()
        .gap_3()
        .px_4()
        .py_4()
        .overflow_y_scroll()
        .child(plain_bubble(true, "LINE 通道最近一直回報 webhook 逾時，幫我看看"))
        .child(plain_bubble(false, "已檢查——過去 20 分鐘有 3 次連續逾時，其餘通道正常。建議重啟該通道的 Bot 行程。"))
        .child(div().w_full().flex().justify_start().child(approval_card()));

    let right = div().flex_1().h_full().flex().flex_col().child(header).child(transcript).child(composer);

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(div().size_full().flex().child(conv_list).child(right))
}
