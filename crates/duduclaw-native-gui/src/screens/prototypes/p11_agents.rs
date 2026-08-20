// p11 — AI 員工。Master-detail 清單：左清單（頭像＋名稱＋狀態點＋兩行卡
// 列）＋右詳情，selection 持久高亮（Apple HIG：「persistently highlight the
// current selection in each pane」）。刻意不用卡片網格（處方原文：「不用卡
// 片網格」）——這是這支 crate 現有 web 慣例裡最典型的一處要改的地方，網格
// 換成清單。這頁的右詳情是「快速預覽」，完整資料頁另有 p12（員工詳情）。

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::mds_gpui::{badge, button, BadgeVariant, ButtonVariant};
use crate::screens::prototypes::common::{avatar, noop, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Online,
    Busy,
    Offline,
}

fn status_color(s: Status) -> u32 {
    match s {
        Status::Online => theme::SUCCESS,
        Status::Busy => theme::WARNING,
        Status::Offline => theme::MUTED_FOREGROUND,
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Online => "在線",
        Status::Busy => "忙碌",
        Status::Offline => "離線",
    }
}

struct Employee {
    name: &'static str,
    letter: char,
    color: u32,
    role: &'static str,
    status: Status,
    selected: bool,
}

const EMPLOYEES: &[Employee] = &[
    Employee { name: "小杜", letter: '杜', color: theme::BRAND, role: "客服專員", status: Status::Online, selected: true },
    Employee { name: "客服組長", letter: '客', color: theme::WARNING, role: "客服組長", status: Status::Busy, selected: false },
    Employee { name: "財務助理", letter: '財', color: theme::SUCCESS, role: "財務／帳務", status: Status::Online, selected: false },
    Employee { name: "出貨助理", letter: '出', color: theme::INFO, role: "物流／出貨", status: Status::Online, selected: false },
    Employee { name: "資料分析師", letter: '析', color: theme::CHART_2, role: "報表與分析", status: Status::Offline, selected: false },
];

fn employee_row(e: &Employee) -> Stateful<Div> {
    div()
        .id(e.name)
        .flex()
        .items_center()
        .gap_2p5()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .when(e.selected, |el| el.bg(theme::alpha(theme::SURFACE_SELECTED, 1.0)))
        .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(
            div().relative().child(avatar(e.letter, e.color, 34.)).child(
                div()
                    .absolute()
                    .bottom(px(-1.))
                    .right(px(-1.))
                    .size(px(10.))
                    .rounded_full()
                    .border_2()
                    .border_color(theme::alpha(theme::SURFACE, 1.0))
                    .bg(theme::alpha(status_color(e.status), 1.0)),
            ),
        )
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
                        .child(e.name),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(e.role),
                ),
        )
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let mut rows = Vec::with_capacity(EMPLOYEES.len());
    for e in EMPLOYEES {
        rows.push(employee_row(e));
    }

    let list = div()
        .id("p11-list")
        .w(px(260.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(theme::surface_border())
        .children(rows);

    let selected = &EMPLOYEES[0];
    let detail = div()
        .id("p11-detail")
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .p_8()
        .overflow_y_scroll()
        .child(avatar(selected.letter, selected.color, 64.))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(selected.name),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(selected.role),
                )
                .child(badge(status_label(selected.status), BadgeVariant::Success)),
        )
        .child(
            div()
                .flex()
                .gap_6()
                .child(quick_stat("本週處理", "42 則"))
                .child(quick_stat("平均回應", "3.2 分鐘"))
                .child(quick_stat("滿意度", "96%")),
        )
        .child(button("p11-open-detail", "查看完整資料", ButtonVariant::Primary, false, None, noop));

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(div().size_full().flex().child(list).child(detail))
}

fn quick_stat(label: &'static str, value: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_0p5()
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(value),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
}
