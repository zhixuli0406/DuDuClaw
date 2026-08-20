// p09 — 任務。同 p08 的三欄骨架，智慧視圖換成「今天／指派給我／全部／封
// 存」；kanban 降為單一專案 opt-in 備選，這一版直接不做（處方原文：「kanban
// 降為單一專案 opt-in 備選（第一版不做）」）。跟 p08 的差異：這裡的列沒有
// 「第 x/y 輪」（那是 goal 專屬的判官迴圈概念），改成指派對象頭像＋截止日
// badge；右 inspector 也換成任務屬性（狀態/優先度/指派/截止/標籤），不是
// goal 的驗收標準/輪次歷史。

use gpui::{div, prelude::*, px, Context, Div};

use crate::mds_gpui::{badge, BadgeVariant};
use crate::screens::prototypes::common::{
    avatar, boxed_group, kv_row, meta_label, status_dot, stage, STATUS_BLOCKED, STATUS_DONE, STATUS_RUNNING,
    STAGE_HEIGHT,
};
use crate::theme;
use crate::RootView;

struct Task {
    title: &'static str,
    status_color: u32,
    assignee: char,
    assignee_color: u32,
    due: &'static str,
    due_variant: BadgeVariant,
    selected: bool,
}

const TASKS: &[Task] = &[
    Task { title: "整理本週客訴摘要", status_color: STATUS_RUNNING, assignee: '客', assignee_color: theme::WARNING, due: "今天", due_variant: BadgeVariant::Warning, selected: true },
    Task { title: "核對出貨單金額", status_color: STATUS_BLOCKED, assignee: '出', assignee_color: theme::INFO, due: "逾期 1 天", due_variant: BadgeVariant::Destructive, selected: false },
    Task { title: "更新常見問答文件", status_color: STATUS_DONE, assignee: '小', assignee_color: theme::BRAND, due: "已完成", due_variant: BadgeVariant::Secondary, selected: false },
];

const SMART_VIEWS: &[&str] = &["今天", "指派給我", "全部", "封存"];

fn smart_view_row(label: &str, active: bool) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(32.))
        .px_2()
        .rounded(px(theme::RADIUS_MD))
        .when(active, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .text_size(px(theme::TEXT_SM))
        .font_weight(if active { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
        .text_color(theme::alpha(
            if active { theme::SIDEBAR_ACCENT_FOREGROUND } else { theme::MUTED_FOREGROUND },
            1.0,
        ))
        .child(label.to_string())
}

fn task_row(t: &Task) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .h(px(52.))
        .px_3()
        .rounded(px(theme::RADIUS_LG))
        .when(t.selected, |el| el.bg(theme::alpha(theme::SURFACE_SELECTED, 1.0)))
        .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(status_dot(t.status_color, 10.))
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(t.title),
        )
        .child(avatar(t.assignee, t.assignee_color, 22.))
        .child(badge(t.due, t.due_variant))
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let mut smart_rows = Vec::with_capacity(SMART_VIEWS.len());
    for (i, label) in SMART_VIEWS.iter().enumerate() {
        smart_rows.push(smart_view_row(label, i == 0));
    }
    let mut task_rows = Vec::with_capacity(TASKS.len());
    for t in TASKS {
        task_rows.push(task_row(t));
    }

    let smart_view_col = div()
        .id("p09-smart-views")
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
        .id("p09-list")
        .w(px(300.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(theme::surface_border())
        .child(meta_label("今天 (3)"))
        .children(task_rows);

    let inspector = div()
        .id("p09-inspector")
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
                        .child("整理本週客訴摘要"),
                )
                .child(badge("進行中", BadgeVariant::Info)),
        )
        .child(boxed_group(vec![
            kv_row("狀態", badge("進行中", BadgeVariant::Info)),
            kv_row("優先度", badge("高", BadgeVariant::Warning)),
            kv_row(
                "指派給",
                div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child("客服組長"),
            ),
            kv_row(
                "截止日",
                div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child("今天 18:00"),
            ),
            kv_row(
                "標籤",
                div().flex().gap_1().child(badge("客服", BadgeVariant::Secondary)).child(badge("本週", BadgeVariant::Secondary)),
            ),
        ]));

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT)
        .child(div().size_full().flex().child(smart_view_col).child(list_col).child(inspector))
}
