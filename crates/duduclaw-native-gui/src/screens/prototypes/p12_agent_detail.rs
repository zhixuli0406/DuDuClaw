// p12 — 員工詳情。header hero（頭像／名稱／角色／狀態）＋boxed-list 分組
// （模型／runtime／能力／通道，label 左值右、限寬置中——GNOME boxed lists
// 文法）＋活動 shelf。跟 p11 的差異：p11 是清單選取後的「快速預覽」，這頁是
// 點進去的「完整資料頁」，欄位齊全、限寬置中閱讀（研究文件 §1.3「內容區寬
// 度」：Windows 設定頁 1000–1100px 級限寬，這裡用同一個限寬直覺）。

use gpui::{div, prelude::*, px, Context, Div};

use crate::mds_gpui::{badge, BadgeVariant};
use crate::screens::prototypes::common::{avatar, boxed_group, kv_row, meta_label, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

fn activity_card(who_action: &'static str, when: &'static str) -> Div {
    div()
        .w(px(200.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(who_action),
        )
        .child(div().text_size(px(10.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(when))
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let model_group = boxed_group(vec![
        kv_row(
            "偏好模型",
            div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child("Claude Sonnet"),
        ),
        kv_row(
            "Runtime",
            div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child("Claude Code CLI"),
        ),
        kv_row("帳號池", badge("預設", BadgeVariant::Secondary)),
    ]);

    let capability_group = boxed_group(vec![
        kv_row("瀏覽器自動化", badge("已開啟", BadgeVariant::Success)),
        kv_row("檔案存取", badge("已開啟", BadgeVariant::Success)),
        kv_row("金流／付款操作", badge("需核准", BadgeVariant::Warning)),
        kv_row("自主目標迴圈", badge("已開啟", BadgeVariant::Success)),
    ]);

    let channel_group = boxed_group(vec![
        kv_row("LINE", badge("已連接", BadgeVariant::Success)),
        kv_row("Discord", badge("已連接", BadgeVariant::Success)),
        kv_row("Slack", badge("未設定", BadgeVariant::Outline)),
    ]);

    let hero = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .py_6()
        .child(avatar('杜', theme::BRAND, 56.))
        .child(
            div()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child("小杜"),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("客服專員 · 隸屬「客服組」"),
        )
        .child(badge("在線", BadgeVariant::Success));

    let activity_rows = vec![
        activity_card("回覆了客戶「訂單延誤」詢問", "5 分鐘前"),
        activity_card("完成「本週客訴摘要」任務", "1 小時前"),
        activity_card("被交辦新目標：客服 SOP 草稿", "昨天"),
    ];

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(
        div().id("p12-scroll").size_full().overflow_y_scroll().flex().flex_col().items_center().child(
            // 限寬置中——GNOME boxed lists／Windows 設定頁的內容寬度直覺。
            div()
                .w(px(480.))
                .flex()
                .flex_col()
                .gap_5()
                .pb_8()
                .child(hero)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(meta_label("模型與 Runtime"))
                        .child(model_group),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(meta_label("能力"))
                        .child(capability_group),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(meta_label("通道"))
                        .child(channel_group),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(meta_label("活動"))
                        .child(div().flex().gap_3().children(activity_rows)),
                ),
        ),
    )
}
