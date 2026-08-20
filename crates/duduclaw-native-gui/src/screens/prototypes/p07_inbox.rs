// p07 — 收件匣。Slack Activity 文法：單一 feed＋型別 filter chips（全部/
// 審批/通知/系統）＋頂欄右「全部已讀」常駐＋兩行卡列。一項（審批型）展開
// 呈現備註輸入框＋核准/駁回雙鈕——研究文件對審批動作的硬規格：Apple 上限 4
// 顆、破壞性動作需附脈絡，這裡兩顆按鈕都附了理由文字，沒有超標也沒有裸的
// 「刪除」。

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::mds_gpui::{badge, button, BadgeVariant, ButtonVariant};
use crate::screens::prototypes::common::{avatar, noop, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Approval,
    Notify,
    System,
    Mention,
}

struct InboxItem {
    kind: Kind,
    who: &'static str,
    letter: char,
    color: u32,
    title: &'static str,
    when: &'static str,
    unread: bool,
    expanded: bool,
}

fn kind_label(k: Kind) -> (&'static str, BadgeVariant) {
    match k {
        Kind::Approval => ("審批", BadgeVariant::Warning),
        Kind::Notify => ("通知", BadgeVariant::Info),
        Kind::System => ("系統", BadgeVariant::Secondary),
        Kind::Mention => ("提及", BadgeVariant::Default),
    }
}

const ITEMS: &[InboxItem] = &[
    InboxItem {
        kind: Kind::Approval,
        who: "小杜",
        letter: '杜',
        color: theme::BRAND,
        title: "想要重啟 LINE 通道 Bot",
        when: "2 分鐘前",
        unread: true,
        expanded: true,
    },
    InboxItem {
        kind: Kind::Notify,
        who: "系統",
        letter: '系',
        color: theme::WARNING,
        title: "本月 API 預算已達 80%",
        when: "1 小時前",
        unread: true,
        expanded: false,
    },
    InboxItem {
        kind: Kind::System,
        who: "財務助理",
        letter: '財',
        color: theme::SUCCESS,
        title: "完成「每月結算」任務",
        when: "3 小時前",
        unread: false,
        expanded: false,
    },
    InboxItem {
        kind: Kind::Mention,
        who: "客服組長",
        letter: '客',
        color: theme::INFO,
        title: "在「任務 A」中提到了你",
        when: "昨天",
        unread: false,
        expanded: false,
    },
];

fn filter_chip(label: &'static str, active: bool) -> Stateful<Div> {
    div()
        .id(label)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .when(active, |el| el.bg(theme::alpha(theme::BRAND, 1.0)))
        .when(!active, |el| el.bg(theme::alpha(theme::SECONDARY, 1.0)))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(if active { theme::BRAND_FOREGROUND } else { theme::SECONDARY_FOREGROUND }, 1.0))
        .child(label)
}

fn inbox_row(item: &InboxItem) -> Div {
    let (kind_text, kind_variant) = kind_label(item.kind);
    let header = div()
        .flex()
        .items_center()
        .gap_2p5()
        .child(avatar(item.letter, item.color, 32.))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(px(theme::TEXT_SM))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                .child(item.who),
                        )
                        .child(badge(kind_text, kind_variant)),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(item.title),
                ),
        )
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(item.when))
        .when(item.unread, |el| {
            el.child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(theme::alpha(theme::BRAND, 1.0)))
        });

    let mut card = div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(header);

    if item.expanded {
        card = card.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pl(px(42.))
                .child(
                    div()
                        .h(px(56.))
                        .px_3()
                        .py_2()
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::input_bg())
                        .border_1()
                        .border_color(theme::input_border())
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child("備註（選填）……"),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(button("p07-reject", "駁回", ButtonVariant::Secondary, false, None, noop))
                        .child(button("p07-approve", "核准", ButtonVariant::Primary, false, None, noop)),
                ),
        );
    }
    card
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let mut rows = Vec::with_capacity(ITEMS.len());
    for item in ITEMS {
        rows.push(inbox_row(item));
    }

    let toolbar = div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .gap_2()
                .child(filter_chip("全部", true))
                .child(filter_chip("審批", false))
                .child(filter_chip("通知", false))
                .child(filter_chip("系統", false)),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::PRIMARY, 1.0))
                .cursor_pointer()
                .child("全部已讀"),
        );

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(
        div()
            .id("p07-scroll")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .child(toolbar)
            .child(div().flex().flex_col().gap_2().children(rows)),
    )
}
