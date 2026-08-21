// Full detail page (p12) for the "AI 員工" page's `AgentsView::Detail` mode
// — split out of `agents.rs` for the same file-size reason `goals_
// inspector.rs` is split from `goals.rs`. No behavior differs from an
// unsplit version.
//
// Visual authority: `commercial/design/duduclaw-s4a-pages/AgentDetail.dc.
// html` — breadcrumb ("AI 員工 › {name}") → hero header (64px avatar / name
// / role·department·status / 編輯 / 開啟對話) → boxed-list groups (模型與
// Runtime / 能力 / 通道, GNOME "label 左值右" pattern) → an activity shelf.
//
// ── Capabilities box is READ-ONLY this round (task brief: "能力區塊本輪唯讀
// 顯示...編輯留後輪") ─────────────────────────────────────────────────────
// The canvas draws each capability row with a toggle-switch control; this
// pass renders the SAME row shape but as a plain status badge (`badge(...)`,
// no click handler at all) — a toggle that silently does nothing on click
// would be a worse lie than a badge that honestly can't be clicked. Only a
// curated, REAL subset of `CapabilitiesConfig`'s ~15 typed fields is shown
// (browser_via_bash / computer_use / os_native / autonomy_level / a
// approval-required-tools count) — see `agents_data.rs::AgentDetailData`'s
// own doc comment for exactly which fields and why no "金流／付款操作" row
// exists (the platform has no such dedicated typed capability; inventing
// one to match the canvas literally would be a fabricated field).

use gpui::{div, prelude::*, px, Context, Div, IntoElement, SharedString, Stateful};

use crate::i18n;
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::agents::{avatar_with_status, role_label, status_label, AgentsState};
use crate::screens::agents_data::{channel_platform_label, channels_for_agent, is_main_role, AgentDetailData};
use crate::screens::dashboard::{ActivityItem, Loadable};
use crate::theme;
use crate::RootView;

const CONTENT_WIDTH: f32 = 640.0;

// ── Shared "boxed-list" primitives (local to this file — the S4a design-
// gallery's own `screens/prototypes/common.rs` has an identical `kv_row`/
// `boxed_group`/`meta_label` triplet, but that module is `mod common;`
// (private, no `pub`) inside `screens/prototypes/mod.rs` — a sibling
// module tree this file cannot reach without widening that gallery's own
// visibility. Same "local copy over widened visibility" precedent
// `agents.rs::avatar_glyph`/`avatar_color` already document.) ────────────

fn meta_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

fn kv_row(label: impl Into<SharedString>, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(38.))
        .px_3p5()
        .text_size(px(theme::TEXT_SM))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
        .child(value)
}

fn boxed_group(rows: Vec<Div>) -> Div {
    let n = rows.len();
    let mut container = div()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border());
    for (i, row) in rows.into_iter().enumerate() {
        container = container.child(if i + 1 < n { row.border_b_1().border_color(theme::border()) } else { row });
    }
    container
}

fn plain_value(text: SharedString) -> Div {
    div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(text)
}

fn bool_badge(locale: i18n::Locale, on: bool) -> Div {
    if on {
        badge(i18n::t(locale, "native.agents.detail.capOn"), BadgeVariant::Success)
    } else {
        badge(i18n::t(locale, "native.agents.detail.capOff"), BadgeVariant::Outline)
    }
}

fn breadcrumb(locale: i18n::Locale, display_name: &str, cx: &mut Context<RootView>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("agents-detail-breadcrumb-back")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.agents.title"))
                .on_click(cx.listener(|_this, _ev, _window, cx| {
                    cx.global_mut::<AgentsState>().back_to_list();
                    cx.notify();
                })),
        )
        .child(div().child("›"))
        .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(display_name.to_string())))
}

fn hero(locale: i18n::Locale, detail: &AgentDetailData, cx: &mut Context<RootView>) -> Div {
    let meta = {
        let role = role_label(locale, &detail.role);
        let status = status_label(locale, &detail.status);
        if detail.department.is_empty() {
            SharedString::from(format!("{role} · {status}"))
        } else {
            let dept = i18n::t1(locale, "native.agents.detail.belongsTo", "department", &detail.department);
            SharedString::from(format!("{role} · {dept} · {status}"))
        }
    };
    let open_chat_id = detail.id.clone();
    let edit_id = detail.id.clone();

    div()
        .flex()
        .items_center()
        .gap_4()
        .child(avatar_with_status(&detail.id, &detail.display_name, detail.icon.as_deref(), &detail.status, px(64.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(SharedString::from(detail.display_name.clone())),
                )
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(meta)),
        )
        .child(button(
            "agents-detail-edit",
            i18n::t(locale, "native.agents.action.edit"),
            ButtonVariant::Secondary,
            // WP-S6b2-N (2026-08-21): the follow-up pass this stub's own
            // comment pointed at has landed — `screens::edit_agent` is a
            // real page now. Real click handler below, no longer a stub.
            false,
            None,
            cx.listener(move |this, _ev, _window, cx| {
                cx.global_mut::<crate::screens::edit_agent::EditAgentState>().select_agent(edit_id.clone());
                this.active_page = "editAgent";
                cx.notify();
            }),
        ))
        .child(button(
            "agents-detail-open-chat",
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

fn model_group(locale: i18n::Locale, detail: &AgentDetailData) -> Div {
    let model = detail.model_preferred.clone().map(SharedString::from).unwrap_or_else(|| i18n::t(locale, "native.agents.detail.unset"));
    let runtime = detail.runtime_provider.clone().map(SharedString::from).unwrap_or_else(|| i18n::t(locale, "native.agents.detail.runtimeAuto"));
    let pool = detail.account_pool.clone().map(SharedString::from).unwrap_or_else(|| i18n::t(locale, "native.agents.detail.poolDefault"));
    boxed_group(vec![
        kv_row(i18n::t(locale, "native.agents.detail.preferredModel"), plain_value(model)),
        kv_row(i18n::t(locale, "native.agents.detail.runtime"), plain_value(runtime)),
        kv_row(i18n::t(locale, "native.agents.detail.accountPool"), plain_value(pool)),
    ])
}

fn capability_group(locale: i18n::Locale, detail: &AgentDetailData) -> Div {
    let mut rows = vec![
        kv_row(i18n::t(locale, "native.agents.detail.capBrowser"), bool_badge(locale, detail.browser_via_bash)),
        kv_row(i18n::t(locale, "native.agents.detail.capComputerUse"), bool_badge(locale, detail.computer_use)),
        kv_row(i18n::t(locale, "native.agents.detail.capOsNative"), bool_badge(locale, detail.os_native)),
    ];
    let autonomy_value = match &detail.autonomy_level {
        Some(level) => plain_value(autonomy_level_label(locale, level)),
        None => plain_value(i18n::t(locale, "native.agents.detail.unset")),
    };
    rows.push(kv_row(i18n::t(locale, "native.agents.detail.capAutonomy"), autonomy_value));
    if detail.approval_required_tools_count > 0 {
        rows.push(kv_row(
            i18n::t(locale, "native.agents.detail.capApprovalRequired"),
            plain_value(i18n::t1(
                locale,
                "native.agents.detail.capApprovalRequiredCount",
                "n",
                &detail.approval_required_tools_count.to_string(),
            )),
        ));
    }
    boxed_group(rows)
}

fn autonomy_level_label(locale: i18n::Locale, level: &str) -> SharedString {
    let key = match level {
        "operator" => "native.agents.detail.autonomy.operator",
        "collaborator" => "native.agents.detail.autonomy.collaborator",
        "consultant" => "native.agents.detail.autonomy.consultant",
        "approver" => "native.agents.detail.autonomy.approver",
        "observer" => "native.agents.detail.autonomy.observer",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

fn channels_group(locale: i18n::Locale, detail: &AgentDetailData, channels: &Loadable<Vec<crate::screens::agents_data::ChannelStatusItem>>) -> Div {
    match channels {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_1p5();
            for _ in 0..2 {
                wrap = wrap.child(skeleton(px(CONTENT_WIDTH - 32.), px(20.)));
            }
            wrap
        }
        // Admin-gated RPC (`channels.status`) failing on a lower-privilege
        // session is an honest "未提供", never a blocked page — see
        // `agents.rs::maybe_fetch_channels`'s own doc comment.
        Loadable::Failed(_) => div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.agents.detail.channelsUnavailable")),
        Loadable::Ready(all) => {
            let is_main = is_main_role(&detail.role);
            let mine = channels_for_agent(all, &detail.id, is_main);
            if mine.is_empty() {
                return div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(i18n::t(locale, "native.agents.detail.channelsEmpty"));
            }
            let rows = mine
                .into_iter()
                .map(|c| {
                    let label = channel_platform_label(&c.name).to_string();
                    let status = if c.connected {
                        badge(i18n::t(locale, "native.agents.detail.channelConnected"), BadgeVariant::Success)
                    } else {
                        badge(i18n::t(locale, "native.agents.detail.channelNotConnected"), BadgeVariant::Outline)
                    };
                    kv_row(label, status)
                })
                .collect();
            boxed_group(rows)
        }
    }
}

fn activity_card(item: &ActivityItem) -> Div {
    div()
        .w(px(200.))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE_RAISED, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(SharedString::from(item.summary.clone())),
        )
        .child(div().text_size(px(10.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(SharedString::from(item.when.clone())))
}

fn activity_shelf(locale: i18n::Locale, activity: &Loadable<Vec<ActivityItem>>) -> Div {
    let body: Div = match activity {
        Loadable::Loading => {
            let mut row = div().flex().gap_3();
            for _ in 0..3 {
                row = row.child(skeleton(px(200.), px(56.)).rounded(px(theme::RADIUS_LG)));
            }
            row
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
            let mut row = div().flex().gap_3();
            for item in items.iter().take(3) {
                row = row.child(activity_card(item));
            }
            row
        }
    };
    div().flex().flex_col().gap_1p5().child(meta_label(i18n::t(locale, "native.agents.activity.title"))).child(body)
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let g = cx.default_global::<AgentsState>();

    let outer = div().id("agents-detail-page").size_full().overflow_y_scroll().flex().flex_col().items_center().p_4();

    let (detail, channels, activity) = match &g.detail {
        Loadable::Ready(d) => (d.clone(), g.channels.clone(), g.activity.clone()),
        Loadable::Loading => {
            return outer.child(
                div().w(px(CONTENT_WIDTH)).flex().flex_col().gap_3().pt_8().child(skeleton(px(CONTENT_WIDTH), px(96.))).child(skeleton(px(CONTENT_WIDTH), px(140.))),
            );
        }
        Loadable::Failed(msg) => {
            return outer.child(
                div().w(px(CONTENT_WIDTH)).pt_16().child(empty_state(
                    "⚠️",
                    i18n::t1(locale, "native.home.card.errorPrefix", "message", msg),
                    None,
                    None::<Div>,
                )),
            );
        }
    };

    outer.child(
        div()
            .w(px(CONTENT_WIDTH))
            .flex()
            .flex_col()
            .gap_5()
            .pb_8()
            .child(breadcrumb(locale, &detail.display_name, cx))
            .child(hero(locale, &detail, cx))
            .child(div().flex().flex_col().gap_1p5().child(meta_label(i18n::t(locale, "native.agents.detail.modelRuntime"))).child(model_group(locale, &detail)))
            .child(div().flex().flex_col().gap_1p5().child(meta_label(i18n::t(locale, "native.agents.detail.capabilities"))).child(capability_group(locale, &detail)))
            .child(div().flex().flex_col().gap_1p5().child(meta_label(i18n::t(locale, "native.agents.detail.channels"))).child(channels_group(locale, &detail, &channels)))
            .child(activity_shelf(locale, &activity)),
    )
}
