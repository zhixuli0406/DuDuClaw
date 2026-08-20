// p13 — 關於。原生 about 文法（macOS「About This App」panel / GNOME
// `AdwAboutWindow`）：置中 icon＋名稱＋版本＋連結列＋授權，小幅面置中——這
// 是唯一一頁刻意用比 `STAGE_HEIGHT` 更矮的 stage（處方原文：「小幅面置
// 中」），因為真正的 about panel 本來就是一個小視窗，不是撐滿整個內容區的
// 頁面。

use gpui::{div, prelude::*, px, Context, Div};

use crate::screens::prototypes::common::stage;
use crate::theme;
use crate::RootView;

const ABOUT_STAGE_HEIGHT: f32 = 380.0;

fn link_row(label: &'static str) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::PRIMARY, 1.0))
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(label)
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    stage(theme::APP_SHELL, ABOUT_STAGE_HEIGHT).child(
        div().size_full().flex().items_center().justify_center().child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .w(px(280.))
                .child(div().text_size(px(48.)).child("🐾"))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("DuDuClaw"),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child("版本 1.62.0-dev"),
                )
                .child(
                    div()
                        .mt_2()
                        .mb_2()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .text_center()
                        .child("多模型 AI 員工平台，支援 Claude / Codex / Gemini 等多種執行後端。"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(link_row("官方網站"))
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.5)).child("·"))
                        .child(link_row("文件"))
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.5)).child("·"))
                        .child(link_row("原始碼")),
                )
                .child(
                    div()
                        .mt_4()
                        .text_size(px(10.))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.7))
                        .child("© 2026 嘟嘟數位。以 MIT／自訂授權釋出。"),
                ),
        ),
    )
}
