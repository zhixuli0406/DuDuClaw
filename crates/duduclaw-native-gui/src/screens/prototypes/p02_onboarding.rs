// p02 — 首次引導。裝置型 OOBE 藍本的代表屏（見
// `research/native-os-2026-08/oobe-first-run-reference.md` §B-1 表格）：值
// 班機式流程共 8 步（語言/網路/更新/建帳號/AI runtime/隱私/產業板模/完成），
// 這頁畫其中「唯一必填身分步」——第 4 步「建立操作者帳號」，因為 B-1 表格
// 特別點名這一步是「結構性根除 bootstrap-admin 先登入再強制改密 WS 死結
// 事故」的關鍵設計（見 memory `project_bootstrap_admin_ws_deadlock`）。
//
// 版面文法：一屏一問（Microsoft OOBE「each screen requests a specific
// action」）＋ dots 進度（KDE plasma-welcome）＋大標題/一句說明/單一表單區
// ＋主鈕右下、跳過為次要樣式同列（八款 OOBE 無一在此屏漏放 chrome）。

use gpui::{div, prelude::*, px, Context, Div};

use crate::mds_gpui::{button, ButtonVariant};
use crate::screens::prototypes::common::{noop, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

/// dots 進度——8 步，目前在第 4 步（見本檔頭註）。
fn progress_dots() -> Div {
    let mut row = div().flex().items_center().gap_2();
    for step in 1..=8 {
        let active = step == 4;
        row = row.child(
            div()
                .size(px(if active { 8. } else { 6. }))
                .rounded_full()
                .bg(theme::alpha(if active { theme::BRAND } else { theme::MUTED }, 1.0)),
        );
    }
    row
}

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

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    stage(theme::APP_SHELL, STAGE_HEIGHT).child(
        div()
            .size_full()
            .flex()
            .flex_col()
            // ── 跳過（次要樣式，同列於頂部——與主鈕呼應但明顯次要）───────
            .child(
                div().w_full().flex().justify_end().p_5().child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child("稍後設定"),
                ),
            )
            .child(
                div().flex_1().flex().items_center().justify_center().child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_8()
                        .w(px(400.))
                        // ── DuDu 吉祥物側位插圖佔位（B-4：可保留的 web 資
                        // 產——每步一表情，這步用 🐾 代表「歡迎」表情）───
                        .child(div().text_size(px(40.)).child("🐾"))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_XL))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                        .child("建立操作者帳號"),
                                )
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_SM))
                                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                        .text_center()
                                        .child("這是唯一必填的身分步驟——一步建立帳號與密碼，之後不會再被要求重新登入。"),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(fake_input("操作者帳號", false))
                                .child(fake_input("設定密碼", true))
                                .child(fake_input("再次輸入密碼", true)),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(progress_dots())
                                .child(button(
                                    "p02-next",
                                    "下一步",
                                    ButtonVariant::Primary,
                                    false,
                                    None,
                                    noop,
                                )),
                        ),
                ),
            ),
    )
}
