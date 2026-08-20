// p04 — 對話。Apple Messages 文法：左會話清單欄（兩行卡列＋未讀點）＋右
// transcript（雙側氣泡，assistant 靠左、使用者靠右）＋置底 composer。
// `/conversations`（p06）併入本頁左欄是設計提案，非本頁自身取捨——p06 自己
// 用獨立原型頁呈現那個提案的展開態，這頁維持左欄本來的樣子（緊湊、只列會
// 話摘要，不做寬表格）。
//
// 這支 crate 自己的 `screens/chat.rs` 已經是這個文法的「真實版」（單一對話
// ＋真的 WebSocket），這頁刻意加回它省略的東西：左側會話清單（chat.rs 的
// 文件註解自己承認「no conversation history list」是已知範圍縮減）。

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::screens::prototypes::common::{avatar, stage, STAGE_HEIGHT};
use crate::theme;
use crate::RootView;

struct Conversation {
    name: &'static str,
    letter: char,
    color: u32,
    preview: &'static str,
    unread: bool,
}

const CONVERSATIONS: &[Conversation] = &[
    Conversation { name: "小杜", letter: '杜', color: theme::BRAND, preview: "好的，這週 retention 數字整理好了", unread: true },
    Conversation { name: "客服組長", letter: '客', color: theme::WARNING, preview: "有 3 則客戶訊息待回覆", unread: true },
    Conversation { name: "財務助理", letter: '財', color: theme::SUCCESS, preview: "本月結算報表已產出", unread: false },
    Conversation { name: "出貨助理", letter: '出', color: theme::INFO, preview: "12 筆訂單狀態已更新", unread: false },
];

struct Bubble {
    from_user: bool,
    text: &'static str,
}

const TRANSCRIPT: &[Bubble] = &[
    Bubble { from_user: true, text: "本週的 retention 數字幫我抓一下" },
    Bubble {
        from_user: false,
        text: "好的，這週 7 日留存率是 42%，比上週的 38% 提升了 4 個百分點。主要成長來自新版推播的重新啟用流程。要我把明細整理成報表嗎？",
    },
    Bubble { from_user: true, text: "好，順便加上跟上個月同期的對比" },
];

fn conversation_row(c: &Conversation, selected: bool) -> Stateful<Div> {
    div()
        .id(c.name)
        .flex()
        .items_center()
        .gap_2p5()
        .p_2p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .hover(|style| style.bg(theme::alpha(theme::SIDEBAR_ACCENT, 0.6)))
        .child(avatar(c.letter, c.color, 32.))
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
                        .child(c.name),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(c.preview),
                ),
        )
        .when(c.unread, |el| {
            el.child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(theme::alpha(theme::BRAND, 1.0)))
        })
}

fn bubble_row(b: &Bubble) -> Div {
    let content = div()
        .max_w(px(320.))
        .px_3p5()
        .py_2p5()
        .rounded(px(theme::RADIUS_XL))
        .text_size(px(theme::TEXT_SM))
        .when(b.from_user, |el| {
            el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
        })
        .when(!b.from_user, |el| {
            el.bg(theme::alpha(theme::SURFACE_RAISED, 1.0)).text_color(theme::alpha(theme::FOREGROUND, 1.0))
        })
        .child(b.text);
    div().w_full().flex().when(b.from_user, |el| el.justify_end()).when(!b.from_user, |el| el.justify_start()).child(content)
}

pub fn render(_state: &RootView, _cx: &mut Context<RootView>) -> Div {
    let mut conv_rows = Vec::with_capacity(CONVERSATIONS.len());
    for (i, c) in CONVERSATIONS.iter().enumerate() {
        conv_rows.push(conversation_row(c, i == 0));
    }
    let mut bubble_rows = Vec::with_capacity(TRANSCRIPT.len());
    for b in TRANSCRIPT {
        bubble_rows.push(bubble_row(b));
    }

    let conv_list = div()
        .id("p04-conv-list")
        .w(px(220.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .overflow_y_scroll()
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
        .children(conv_rows);

    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(theme::surface_border())
        .child(avatar('杜', theme::BRAND, 24.))
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child("小杜"),
        )
        .child(div().size(px(8.)).rounded_full().bg(theme::alpha(theme::SUCCESS, 1.0)))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child("已連線"),
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
                .child("輸入訊息…（Enter 送出，Shift+Enter 換行）"),
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
        .id("p04-transcript")
        .flex_1()
        .flex()
        .flex_col()
        .gap_3()
        .px_4()
        .py_4()
        .overflow_y_scroll()
        .children(bubble_rows);

    let right = div().flex_1().h_full().flex().flex_col().child(header).child(transcript).child(composer);

    stage(theme::PAGE_CANVAS, STAGE_HEIGHT).child(div().size_full().flex().child(conv_list).child(right))
}
