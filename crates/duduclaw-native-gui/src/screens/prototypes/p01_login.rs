// p01 — 登入。OOBE 共識屏型：滿版舞台、無 app chrome、置中卡、主鈕右下（見
// `research/native-os-2026-08/oobe-first-run-reference.md` §A#3「一屏一問」
// 與置中面板/大標題慣例）。內容結構直接照抄這支 crate自己的
// `screens/login.rs`（同一套 MDS Card + 兩個輸入框 + brand 按鈕 + link 次要
// 動作），差異只有兩點，都是設計稿刻意的取捨：
//   1. 兩個輸入框畫成靜態方塊（`fake_input`），不是真的 `TextField` Entity
//      ——這支模組沒有 `&mut App` 可以建立新 Entity，而且這頁本來就不需要
//      真的可打字（task brief: 純靜態渲染）。
//   2. 主鈕保持在卡片內、靠右上緣沒有特別外推（OOBE 慣例只要求「右下」是相
//      對整個舞台/視窗而言——這裡卡片本身就置中在舞台正中央，卡片內按鈕自
//      然落在視覺右下象限）。

use gpui::{div, prelude::*, px, Context, Div};

use crate::mds_gpui::{button, ButtonVariant};
use crate::screens::prototypes::common::{noop, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

fn fake_input(placeholder: &'static str, masked: bool) -> Div {
    let display = if masked { "••••••••" } else { placeholder };
    div()
        .h(px(36.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::input_bg())
        .border_1()
        .border_color(theme::input_border())
        .text_size(px(theme::TEXT_SM))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(display)
}

fn field_label(text: &'static str) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(text)
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    stage(theme::APP_SHELL, STAGE_HEIGHT).child(
        div().size_full().flex().items_center().justify_center().child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_6()
                .w(px(360.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(div().text_size(px(32.)).child("🐾"))
                        .child(
                            div()
                                .text_size(px(theme::TEXT_BASE))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child("DuDuClaw"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::TEXT_XS))
                                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                .child("登入您的帳號"),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .p_4()
                        .rounded(px(theme::RADIUS_XL))
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::surface_border())
                        .shadow(theme::surface_shadow())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(field_label("電子郵件"))
                                .child(fake_input("admin@local", false)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(field_label("密碼"))
                                .child(fake_input("", true)),
                        )
                        .child(
                            // 主鈕右下——卡片內靠右對齊、寬度不撐滿，呼應
                            // OOBE「主按鈕右下」慣例（研究文件 A 表 #? 置中卡
                            // 內的相對位置）。
                            div().flex().justify_end().child(button(
                                "p01-submit",
                                "登入",
                                ButtonVariant::Primary,
                                false,
                                None,
                                noop,
                            )),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(theme::TEXT_SM))
                                .text_color(theme::alpha(theme::PRIMARY, 1.0))
                                .child("改用通道驗證碼登入"),
                        ),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child("首次使用會引導您建立管理員密碼"),
                ),
        ),
    )
}
