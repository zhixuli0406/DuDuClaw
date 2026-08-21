// Right mini-summary panel (p11's right column) for the "AI 員工" LIST view
// — split out of `agents.rs`/`agents_list.rs` for the same file-size reason
// `goals_inspector.rs` is split from `goals.rs`. No behavior differs from an
// unsplit version.
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Agents.dc.html`'s
// right column — mini profile (avatar/name/role·department·status) + 交辦/
// 開啟對話 buttons + 3 stat cards (今日完成/進行中/等你決定) + 近期活動 shelf
// + "開啟完整檔案（模型、能力、通道）" link into the p12 detail view
// (`AgentsState::open_detail`, see `agents.rs`'s own `AgentsView` doc
// comment for why this is an internal view switch, not a second nav id).

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n;
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::screens::agents::{avatar_with_status, role_label, status_label, AgentsState};
use crate::screens::agents_data::{AgentListItem, AgentTaskStats};
use crate::screens::dashboard::{ActivityItem, Loadable};
use crate::theme;
use crate::RootView;

/// `role · department · status` — the exact 3-segment meta line
/// `AgentDetail.dc.html`'s hero uses ("綜合助理 · 隸屬「總管理處」 · 執行中"),
/// department segment omitted when empty (same "don't fabricate a value the
/// server left unset" rule `goals_inspector.rs::meta_line` already applies
/// to its own optional `channel` segment).
fn meta_line(locale: i18n::Locale, agent: &AgentListItem) -> SharedString {
    let role = role_label(locale, &agent.role);
    let status = status_label(locale, &agent.status);
    if agent.department.is_empty() {
        format!("{role} · {status}").into()
    } else {
        let dept = i18n::t1(locale, "native.agents.detail.belongsTo", "department", &agent.department);
        format!("{role} · {dept} · {status}").into()
    }
}

fn stat_card(label: SharedString, value: SharedString, value_color: Option<u32>) -> Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(
            div()
                .text_size(px(theme::TEXT_BASE))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(value_color.unwrap_or(theme::FOREGROUND), 1.0))
                .child(value),
        )
}

fn stats_row(locale: i18n::Locale, stats: &Loadable<AgentTaskStats>) -> Div {
    let row = div().flex().gap_2p5();
    match stats {
        Loadable::Ready(s) => row
            .child(stat_card(
                i18n::t(locale, "native.agents.stat.todayDone"),
                i18n::t1(locale, "native.agents.stat.countSuffix", "n", &s.today_done.to_string()),
                None,
            ))
            .child(stat_card(
                i18n::t(locale, "native.agents.stat.inProgress"),
                i18n::t1(locale, "native.agents.stat.countSuffix", "n", &s.in_progress.to_string()),
                None,
            ))
            .child(stat_card(
                i18n::t(locale, "native.agents.stat.needsHuman"),
                i18n::t1(locale, "native.agents.stat.countSuffix", "n", &s.needs_human.to_string()),
                (s.needs_human > 0).then_some(theme::WARNING),
            )),
        Loadable::Failed(_) => row.child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.agents.stat.unavailable")),
        ),
        Loadable::Loading => row
            .child(skeleton(px(80.), px(48.)).rounded(px(theme::RADIUS_LG)))
            .child(skeleton(px(80.), px(48.)).rounded(px(theme::RADIUS_LG)))
            .child(skeleton(px(80.), px(48.)).rounded(px(theme::RADIUS_LG))),
    }
}

fn activity_row(item: &ActivityItem) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .overflow_hidden()
                .child(SharedString::from(item.summary.clone())),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(10.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(item.when.clone())),
        )
}

fn activity_box(locale: i18n::Locale, activity: &Loadable<Vec<ActivityItem>>) -> Div {
    let body: Div = match activity {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_1p5();
            for _ in 0..3 {
                wrap = wrap.child(skeleton(px(260.), px(12.)));
            }
            wrap
        }
        Loadable::Failed(msg) => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
            .child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
        Loadable::Ready(items) if items.is_empty() => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.agents.activity.empty")),
        Loadable::Ready(items) => {
            let mut wrap = div().flex().flex_col().gap_2();
            for item in items.iter().take(5) {
                wrap = wrap.child(activity_row(item));
            }
            wrap
        }
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.agents.activity.title")),
        )
        .child(body)
}

fn profile_header(locale: i18n::Locale, agent: &AgentListItem, cx: &mut Context<RootView>) -> Div {
    let open_chat_id = agent.id.clone();
    div()
        .flex()
        .items_center()
        .gap_3p5()
        .child(avatar_with_status(&agent.id, &agent.display_name, agent.icon.as_deref(), &agent.status, px(56.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(SharedString::from(agent.display_name.clone())),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(meta_line(locale, agent)),
                ),
        )
        .child(button(
            "agents-summary-assign",
            i18n::t(locale, "native.agents.action.assign"),
            ButtonVariant::Secondary,
            true, // honest stub — see `agents_list.rs::header`'s own disabled-button doc comment
            None,
            cx.listener(|_, _, _, _| {}),
        ))
        .child(button(
            "agents-summary-open-chat",
            i18n::t(locale, "native.agents.action.openChat"),
            ButtonVariant::Primary,
            false,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                this.active_page = "newChat";
                this.chat.select_agent(Some(open_chat_id.clone()));
                cx.notify();
            }),
        ))
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let g = cx.default_global::<AgentsState>();

    let selected: Option<AgentListItem> = match (&g.list, &g.selected_id) {
        (Loadable::Ready(list), Some(id)) => list.iter().find(|a| &a.id == id).cloned(),
        _ => None,
    };

    let panel = div()
        .id("agents-summary")
        .flex_1()
        .h_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow());

    let Some(agent) = selected else {
        return panel.child(
            div().flex_1().flex().items_center().justify_center().child(empty_state(
                "🐾",
                i18n::t(locale, "native.agents.summary.emptyTitle"),
                Some(i18n::t(locale, "native.agents.summary.emptyDesc")),
                None::<Div>,
            )),
        );
    };

    let stats = g.stats.clone();
    let activity = g.activity.clone();

    panel
        .child(profile_header(locale, &agent, cx))
        .child(stats_row(locale, &stats))
        .child(activity_box(locale, &activity))
        .child(
            div()
                .id("agents-summary-open-detail")
                .cursor_pointer()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .hover(|s| s.text_color(theme::alpha(theme::BRAND, 0.8)))
                .child(i18n::t(locale, "native.agents.summary.openFullProfile"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<AgentsState>().open_detail();
                    cx.notify();
                })),
        )
}
