// Master list column (p11's left column) for the "AI 員工" page — split out
// of `agents.rs` purely to keep that file under this crate's own file-size
// convention, same reason `goals.rs` has sibling `goals_inspector.rs` (see
// that pair's own doc comments for the precedent). No behavior differs from
// an unsplit version. Composes the master list (this file) + the right
// mini-summary panel (`agents_summary::render`) into the full master-detail
// page — the top-level `agents::render` dispatch calls THIS file for
// `AgentsView::List`, not `agents_summary` directly.
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/Agents.dc.html`'s
// left column — header ("AI 員工 N 位" + "新員工") + a plain row LIST (avatar
// + status dot + two-line name/role-department, selected row highlighted) —
// deliberately NOT a card grid, per the task brief's own instruction.

use gpui::{div, prelude::*, px, Context, SharedString, Stateful};

use crate::i18n;
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::screens::agents::{avatar_with_status, role_label, AgentsState};
use crate::screens::agents_data::AgentListItem;
use crate::screens::agents_summary;
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::RootView;

const LIST_WIDTH: f32 = 320.0;

fn agent_row(agent: &AgentListItem, selected: bool, locale: i18n::Locale, cx: &mut Context<RootView>) -> Stateful<gpui::Div> {
    let id: SharedString = format!("agents-row-{}", agent.id).into();
    let role = role_label(locale, &agent.role);
    let second_line: SharedString = if agent.department.is_empty() {
        role
    } else {
        format!("{role} · {}", agent.department).into()
    };
    let agent_id = agent.id.clone();

    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2p5()
        .px_2p5()
        .py_2()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::SURFACE_SELECTED, 1.0)))
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .child(avatar_with_status(&agent.id, &agent.display_name, agent.icon.as_deref(), &agent.status, px(36.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .overflow_hidden()
                        .child(SharedString::from(agent.display_name.clone())),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .overflow_hidden()
                        .child(second_line),
                ),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<AgentsState>().select_agent(agent_id.clone());
            cx.notify();
        }))
}

fn header(count: usize, locale: i18n::Locale, cx: &mut Context<RootView>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_1()
        .pb_1()
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .text_size(px(theme::TEXT_BASE))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, "native.agents.title")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t1(locale, "native.agents.count", "n", &count.to_string())),
                ),
        )
        .child(
            // "新員工" — WP-S6b2-N wires this real: navigates to the
            // `createAgent` page (`screens/create_agent.rs`'s own module doc
            // comment covers RPC shapes and the assembled-vs-wired boundary
            // for that page's own submit button). Previously an honest
            // `disabled: true` stub ("agent creation is a multi-step wizard
            // on the web dashboard; not ported here yet") — that page now
            // exists, so the stub is gone.
            button(
                "agents-new",
                i18n::t(locale, "native.agents.new"),
                ButtonVariant::Primary,
                false,
                None,
                cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "createAgent";
                    cx.notify();
                }),
            ),
        )
}

fn master_list(state: &RootView, cx: &mut Context<RootView>) -> Stateful<gpui::Div> {
    let locale = state.locale;
    let g = cx.default_global::<AgentsState>();
    let selected_id = g.selected_id.clone();
    // Snapshot the whole `Loadable` up front (owned, not borrowed) so every
    // branch below can freely call functions that need `cx` mutably (`header`
    // and `agent_row` both attach `cx.listener` click handlers) — matches
    // `goals.rs::task_list`'s own "clone the snapshot, then match on the
    // owned value" shape, avoiding the double-mutable-borrow a `match &cx.
    // default_global::<T>().field { ... }` directly would create once any
    // arm needs `cx` again.
    let list_snapshot = g.list.clone();

    let frame = div()
        .id("agents-list")
        .w(px(LIST_WIDTH))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SIDEBAR, 1.0))
        .border_1()
        .border_color(theme::sidebar_border())
        .shadow(theme::surface_shadow());

    match list_snapshot {
        Loadable::Loading => {
            let mut body = div().id("agents-list-body").flex_1().overflow_y_scroll().flex().flex_col().gap_1p5().px_1();
            for _ in 0..4 {
                body = body.child(skeleton(px(LIST_WIDTH - 40.), px(48.)).rounded(px(theme::RADIUS_MD)));
            }
            frame.child(header(0, locale, cx)).child(body)
        }
        Loadable::Failed(msg) => frame.child(header(0, locale, cx)).child(
            div().flex_1().flex().items_center().justify_center().child(empty_state(
                "⚠️",
                i18n::t1(locale, "native.home.card.errorPrefix", "message", &msg),
                None,
                None::<gpui::Div>,
            )),
        ),
        Loadable::Ready(agents) if agents.is_empty() => frame.child(header(0, locale, cx)).child(
            div().flex_1().flex().items_center().justify_center().child(empty_state(
                "🐾",
                i18n::t(locale, "native.agents.empty"),
                None,
                None::<gpui::Div>,
            )),
        ),
        Loadable::Ready(agents) => {
            let mut body = div().id("agents-list-body").flex_1().overflow_y_scroll().flex().flex_col().gap_0p5();
            for agent in &agents {
                let selected = selected_id.as_deref() == Some(agent.id.as_str());
                body = body.child(agent_row(agent, selected, locale, cx));
            }
            frame.child(header(agents.len(), locale, cx)).child(body)
        }
    }
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<gpui::Div> {
    div()
        .id("agents-page")
        .size_full()
        .flex()
        .gap_3()
        .child(master_list(state, cx))
        .child(agents_summary::render(state, cx))
}
