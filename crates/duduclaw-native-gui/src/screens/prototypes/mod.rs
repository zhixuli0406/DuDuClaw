// S4a design-gallery — registry of 13 static prototype pages + the
// list/preview viewer that shows them, reachable from the sidebar footer's
// new "設計稿" (`nav.designGallery`) entry (`screens/shell.rs`'s
// `designGallery` branch calls `render` below).
//
// Design brief for all 13 pages: `research/native-os-2026-08/
// page-type-reference-2026-08.md`'s final section ("對 S4a 13 頁設計稿的直接
// 處方"), cross-referenced with `desktop-app-conventions.md` §A/§B (web vs
// native conventions) and `oobe-first-run-reference.md` §B-1 (first-run
// flow). Each page's own file header restates its own one-line brief +
// which trade-offs it made, so the rationale travels with the code — this
// file only carries the registry-level "which page is this" metadata
// (`PrototypeSpec::caption`, one line each, echoing the brief's own wording).
//
// Every page is a pure, static render function: `fn(&RootView, &mut
// Context<RootView>) -> Div`. None of them mutate `RootView` or read
// anything from it beyond what the function signature requires them to
// accept (see `common.rs`'s module doc comment for why: this is a
// design-review artifact with fabricated sample data, not a wired-up
// surface). "Which page is selected" is the ONE piece of real state this
// gallery owns, `RootView::active_prototype` — mirrors `active_page`'s own
// shape, driven by clicking a row in the list this file renders.
//
// Not a Column-2 content list (`shell_content_list.rs`): `designGallery` is
// a `nav.rs::FOOTER_ITEMS` entry, which belongs to no `NavArea`
// (`nav::area_for_page("designGallery")` is `None`), so Column 2 already
// renders nothing for it. Per the task brief's "在內容區自建左列，選簡單的
// 做法", this file draws its own list+preview split entirely inside Column
// 3 instead of teaching the nav/area system about a 14th "area".

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::theme;
use crate::RootView;

mod common;
mod p01_login;
mod p02_onboarding;
mod p03_dashboard;
mod p04_chat;
mod p05_console;
mod p06_conversations;
mod p07_inbox;
mod p08_goals;
mod p09_tasks;
mod p10_task_detail;
mod p11_agents;
mod p12_agent_detail;
mod p13_about;

type RenderFn = fn(&RootView, &mut Context<RootView>) -> Div;

pub struct PrototypeSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub caption: &'static str,
    render: RenderFn,
}

/// The 13 pages, in the same order as the task brief's "13 頁處方摘要".
pub const PROTOTYPES: &[PrototypeSpec] = &[
    PrototypeSpec {
        id: "p01",
        title: "登入",
        caption: "依據：OOBE 共識——滿版舞台無 app chrome、置中卡、主鈕右下",
        render: p01_login::render,
    },
    PrototypeSpec {
        id: "p02",
        title: "首次引導",
        caption: "依據：裝置型 OOBE 藍本——一屏一問＋dots 進度，代表屏＝建立操作者帳號",
        render: p02_onboarding::render,
    },
    PrototypeSpec {
        id: "p03",
        title: "儀表板",
        caption: "依據：Copilot prompt-first ＋ Settings Home 互動卡，刻意不放圖表",
        render: p03_dashboard::render,
    },
    PrototypeSpec {
        id: "p04",
        title: "對話",
        caption: "依據：Apple Messages 文法——左會話清單＋右 transcript＋置底 composer",
        render: p04_chat::render,
    },
    PrototypeSpec {
        id: "p05",
        title: "主控台",
        caption: "依據：p04 骨架變體——transcript 內嵌審批卡，核准/駁回雙鈕",
        render: p05_console::render,
    },
    PrototypeSpec {
        id: "p06",
        title: "對話紀錄",
        caption: "依據：「併入對話頁左欄」提案的展開態，附寬表格切換模式示意",
        render: p06_conversations::render,
    },
    PrototypeSpec {
        id: "p07",
        title: "收件匣",
        caption: "依據：Slack Activity 文法——filter chips＋兩行卡列 feed＋展開態雙鈕",
        render: p07_inbox::render,
    },
    PrototypeSpec {
        id: "p08",
        title: "目標",
        caption: "依據：OmniFocus 三欄——智慧視圖＋狀態圈清單＋右 inspector，無 kanban",
        render: p08_goals::render,
    },
    PrototypeSpec {
        id: "p09",
        title: "任務",
        caption: "依據：同 p08 骨架，智慧視圖換成今天/指派給我/全部/封存",
        render: p09_tasks::render,
    },
    PrototypeSpec {
        id: "p10",
        title: "任務詳情",
        caption: "依據：header＋分段捲動＋右屬性 inspector，不用 web tabs",
        render: p10_task_detail::render,
    },
    PrototypeSpec {
        id: "p11",
        title: "AI 員工",
        caption: "依據：master-detail 清單——左清單＋右詳情，不用卡片網格",
        render: p11_agents::render,
    },
    PrototypeSpec {
        id: "p12",
        title: "員工詳情",
        caption: "依據：header hero＋boxed-list 分組（GNOME boxed lists）＋活動 shelf",
        render: p12_agent_detail::render,
    },
    PrototypeSpec {
        id: "p13",
        title: "關於",
        caption: "依據：原生 about 文法（macOS about panel / AdwAboutWindow），小幅面置中",
        render: p13_about::render,
    },
];

/// One row in the left-hand prototype list.
fn prototype_row(spec: &PrototypeSpec, active_id: &str, cx: &mut Context<RootView>) -> Stateful<Div> {
    let selected = spec.id == active_id;
    let target = spec.id;
    div()
        .id(spec.id)
        .flex()
        .flex_col()
        .gap_0p5()
        .px_2p5()
        .py_2()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::SIDEBAR_ACCENT, 1.0)))
        .hover(|style| style.bg(theme::alpha(theme::SIDEBAR_ACCENT, 0.7)))
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(if selected { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
                .text_color(theme::alpha(
                    if selected { theme::SIDEBAR_ACCENT_FOREGROUND } else { theme::FOREGROUND },
                    1.0,
                ))
                .child(format!("{} · {}", spec.id.to_uppercase(), spec.title)),
        )
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.active_prototype = target;
            cx.notify();
        }))
}

/// The design-gallery viewer: a 13-row list (left) + a caption bar ＋ the
/// selected page's rendered content (right) — both nested inside `screens/
/// shell.rs`'s Column 3, see this module's header comment for why this
/// isn't a Column-2 list.
pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let active = state.active_prototype;
    let spec = PROTOTYPES.iter().find(|p| p.id == active).unwrap_or(&PROTOTYPES[0]);

    let mut rows = Vec::with_capacity(PROTOTYPES.len());
    for p in PROTOTYPES {
        rows.push(prototype_row(p, active, cx));
    }
    let list = div()
        .id("design-gallery-list")
        .w(px(200.))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1()
        .pr_3()
        .border_r_1()
        .border_color(theme::surface_border())
        .overflow_y_scroll()
        .children(rows);

    // The per-prototype caption bar (task brief: "每個原型頁頂部一條細
    // caption 帶（原型名＋『依據：XXX 文法』一句）").
    let caption_bar = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .pb_3()
        .mb_3()
        .border_b_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(format!("{} — {}", spec.id.to_uppercase(), spec.title)),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(spec.caption),
        );

    let preview = (spec.render)(state, cx);

    let right = div()
        .id("design-gallery-preview")
        .flex_1()
        .h_full()
        .pl_4()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .child(caption_bar)
        .child(preview);

    div().id("design-gallery").flex_1().w_full().h_full().flex().child(list).child(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thirteen_prototypes_registered() {
        assert_eq!(PROTOTYPES.len(), 13, "S4a design gallery must register exactly 13 prototypes");
    }

    #[test]
    fn prototype_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for p in PROTOTYPES {
            assert!(seen.insert(p.id), "duplicate prototype id: {}", p.id);
        }
    }

    #[test]
    fn prototype_ids_follow_p_nn_convention() {
        for (i, p) in PROTOTYPES.iter().enumerate() {
            let expected = format!("p{:02}", i + 1);
            assert_eq!(p.id, expected, "prototype at index {i} has id {:?}, expected {expected:?}", p.id);
        }
    }
}
