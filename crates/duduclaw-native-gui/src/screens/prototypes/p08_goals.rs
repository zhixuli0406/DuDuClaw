// p08 — 目標。OmniFocus 三欄文法：側欄智慧視圖（需要你(1) 置頂／進行中／
// 今天／已完成）＋清單（行首狀態圈語意著色：紅橘=needs_human、藍=running、
// 灰=done；「第 x/y 輪」計數）＋右 inspector（驗收標準／輪次歷史／判官回饋
// ／approve-retry-abort）。刻意不做 kanban——處方原文與研究文件都點名「八
// 款原生 app 零款預設看板」。

use gpui::{div, prelude::*, px, Context, Div};

use crate::mds_gpui::{badge, button, BadgeVariant, ButtonVariant};
use crate::screens::prototypes::common::{
    meta_label, noop, status_dot, stage, STATUS_DONE, STATUS_NEEDS_HUMAN, STATUS_RUNNING, STAGE_HEIGHT,
};
use crate::theme;
use crate::RootView;

struct Goal {
    title: &'static str,
    status_color: u32,
    round: &'static str,
    when: &'static str,
    selected: bool,
}

const GOALS: &[Goal] = &[
    Goal { title: "每日營收日報自動化", status_color: STATUS_NEEDS_HUMAN, round: "第 3/5 輪", when: "2 小時前", selected: true },
    Goal { title: "客服 SOP 草稿整理", status_color: STATUS_RUNNING, round: "第 1/3 輪", when: "剛剛", selected: false },
    Goal { title: "月結報表產出", status_color: STATUS_DONE, round: "5/5 輪", when: "昨天", selected: false },
];

const SMART_VIEWS: &[(&str, Option<u32>)] =
    &[("需要你", Some(1)), ("進行中", None), ("今天", None), ("已完成", None)];

fn smart_view_row(label: &str, count: Option<u32>, active: bool) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(32.))
        .px_2()
        .rounded(px(theme::RADIUS_MD))
        .when(active, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(if active { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
                .text_color(theme::alpha(
                    if active { theme::SIDEBAR_ACCENT_FOREGROUND } else { theme::MUTED_FOREGROUND },
                    1.0,
                ))
                .child(label.to_string()),
        )
        .children(count.map(|c| badge(c.to_string(), BadgeVariant::Warning)))
}

fn goal_row(g: &Goal) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .h(px(52.))
        .px_3()
        .rounded(px(theme::RADIUS_LG))
        .when(g.selected, |el| el.bg(theme::alpha(theme::SURFACE_SELECTED, 1.0)))
        .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(status_dot(g.status_color, 10.))
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
                        .child(g.title),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(g.round),
                ),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(g.when))
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let mut smart_rows = Vec::with_capacity(SMART_VIEWS.len());
    for (i, (label, count)) in SMART_VIEWS.iter().enumerate() {
        smart_rows.push(smart_view_row(label, *count, i == 0));
    }
    let mut goal_rows = Vec::with_capacity(GOALS.len());
    for g in GOALS {
        goal_rows.push(goal_row(g));
    }

    let smart_view_col = div()
        .id("p08-smart-views")
        .w(px(150.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .border_r_1()
        .border_color(theme::surface_border())
        .children(smart_rows);

    let list_col = div()
        .id("p08-list")
        .w(px(280.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(theme::surface_border())
        .child(meta_label("需要你 (1)"))
        .children(goal_rows);

    // ── 右 inspector：驗收標準／輪次歷史／判官回饋／三鈕 ──────────────
    let inspector = div()
        .id("p08-inspector")
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .gap_4()
        .p_4()
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(theme::TEXT_BASE))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("每日營收日報自動化"),
                )
                .child(badge("需要你", BadgeVariant::Warning)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(meta_label("驗收標準"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("• 每日 09:00 前送出昨日營收摘要")
                        .child("• 含前一週同期對比")
                        .child("• 異常波動需附一句原因推測"),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(meta_label("輪次歷史"))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(round_row("第 1 輪", "駁回 — 缺前一週對比", STATUS_DONE))
                        .child(round_row("第 2 輪", "駁回 — 異常波動沒附原因", STATUS_DONE))
                        .child(round_row("第 3 輪", "待審 — 判官正在檢查", STATUS_NEEDS_HUMAN)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(meta_label("判官回饋"))
                .child(
                    div()
                        .p_3()
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::alpha(theme::WARNING, 0.10))
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child("內容大致完整，但「異常波動原因推測」段落引用的數字跟附件對不上，需要你確認要採用哪個版本再繼續。"),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(button("p08-approve", "核准", ButtonVariant::Primary, false, None, noop))
                .child(button("p08-retry", "重試", ButtonVariant::Secondary, false, None, noop))
                .child(button("p08-abort", "放棄", ButtonVariant::Destructive, false, None, noop)),
        );

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT)
        .child(div().size_full().flex().child(smart_view_col).child(list_col).child(inspector))
}

fn round_row(label: &'static str, detail: &'static str, color: u32) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(status_dot(color, 8.))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(label),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(detail))
}
