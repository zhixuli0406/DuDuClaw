// Tab render functions for the "能力" rail group (技能/工具/整合) plus 一般
// — sibling of `edit_agent.rs`/`edit_agent_data.rs`/`edit_agent_tabs_b.rs`.
// See `edit_agent.rs`'s module doc comment for the page-wide scope decision
// (every row here is a REAL value, READ-ONLY display) and the RPC
// citations. Every function returns a self-contained `.flex().flex_col()
// .gap_3p5()` stack of `edit_agent::section(...)` calls — `edit_agent.rs::
// render`'s tab-body wrapper only supplies sizing, not layout.
//
// None of these 4 tabs have canvas artwork (only 大腦, in `edit_agent_tabs_
// b.rs`, does) — each section below is a direct port of the matching tab in
// `web/src/pages/agent-form/EditAgentPage.tsx`, using the SAME kv_row/
// boxed_group vocabulary the canvas's own 大腦 example establishes. Every
// field this page deliberately leaves out (because `agents.inspect` has no
// readback for it) is called out with a one-line comment at the exact point
// it was decided, not silently dropped — the "誠實偏差" table this WP's
// brief requires.

use gpui::{div, prelude::*, px, Div, SharedString};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, skeleton, BadgeVariant};
use crate::screens::agents::{role_label, status_label};
use crate::screens::agents_data::{channel_platform_label, channels_for_agent, is_main_role, ChannelStatusItem};
use crate::screens::dashboard::Loadable;
use crate::screens::edit_agent::{boxed_group, bool_badge, kv_row, plain_value, section};
use crate::screens::edit_agent_data::{ContractData, EditAgentDetail};
use crate::theme;

/// `capabilities.autonomy_level` label mapping — a second, deliberately
/// identical copy of `agents_detail.rs`'s own private (non-`pub`, so
/// unreachable from here) `autonomy_level_label` function. Same "local
/// copy over widened visibility" precedent that file's own header comment
/// already establishes for its avatar helpers.
fn autonomy_level_label(locale: Locale, level: &str) -> SharedString {
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

// ── 技能 ───────────────────────────────────────────────────────────────

pub(super) fn skills_tab(locale: Locale, detail: &EditAgentDetail) -> Div {
    let equipped_body: Div = if detail.skills.is_empty() {
        div()
            .px_3p5()
            .py_4()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.editAgent.skillsEmpty"))
    } else {
        let mut wrap = div().flex().flex_wrap().gap_1p5().p_3p5();
        for name in &detail.skills {
            wrap = wrap.child(badge(SharedString::from(name.clone()), BadgeVariant::Secondary));
        }
        wrap
    };

    // 誠實偏差: web 表單的 max_active_skills / skill_token_budget 是「寫入用
    // 預設值」欄位（送出時打包成設定，但 `agents.inspect` 從未回讀它們），本
    // 頁不臆造「目前值」，直接略過這兩個欄位。
    div()
        .flex()
        .flex_col()
        .gap_3p5()
        .child(section(i18n::t(locale, "native.editAgent.section.equippedSkills"), boxed_group(vec![equipped_body])))
        .child(section(
            i18n::t(locale, "native.editAgent.section.skillAutomation"),
            boxed_group(vec![
                kv_row(i18n::t(locale, "native.editAgent.row.skillAutoActivate"), bool_badge(locale, detail.skill_auto_activate)),
                kv_row(i18n::t(locale, "native.editAgent.row.skillSecurityScan"), bool_badge(locale, detail.skill_security_scan)),
            ]),
        ))
}

// ── 工具 ───────────────────────────────────────────────────────────────

/// A label-above-value row that grows to fit wrapped multi-line content —
/// `edit_agent::kv_row`'s fixed `h(38px)` single line would clip a long
/// `must_not`/`must_always` sentence list, so the CONTRACT section uses
/// this instead of `kv_row` for its two list rows.
fn wrap_row(label: impl Into<SharedString>, value: SharedString) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3p5()
        .py_2p5()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(value))
}

fn contract_body(locale: Locale, contract: &Loadable<ContractData>) -> Div {
    match contract {
        Loadable::Loading => div()
            .px_3p5()
            .py_4()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.editAgent.contractLoading")),
        Loadable::Failed(_) => div()
            .px_3p5()
            .py_4()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.editAgent.contractFailed")),
        Loadable::Ready(c) => {
            let must_not = if c.must_not.is_empty() { i18n::t(locale, "native.editAgent.contractEmpty") } else { SharedString::from(c.must_not.join("；")) };
            let must_always =
                if c.must_always.is_empty() { i18n::t(locale, "native.editAgent.contractEmpty") } else { SharedString::from(c.must_always.join("；")) };
            boxed_group(vec![
                wrap_row(i18n::t(locale, "native.editAgent.row.mustNot"), must_not),
                wrap_row(i18n::t(locale, "native.editAgent.row.mustAlways"), must_always),
                kv_row(i18n::t(locale, "native.editAgent.row.maxToolCalls"), plain_value(SharedString::from(c.max_tool_calls_per_turn.to_string()))),
            ])
        }
    }
}

pub(super) fn tools_tab(locale: Locale, detail: &EditAgentDetail, contract: &Loadable<ContractData>) -> Div {
    let autonomy_value = match &detail.autonomy_level {
        Some(level) => plain_value(autonomy_level_label(locale, level)),
        None => plain_value(i18n::t(locale, "native.editAgent.unset")),
    };
    let allowed_value = if detail.allowed_tools_count == 0 {
        plain_value(i18n::t(locale, "native.editAgent.allowedToolsUnrestricted"))
    } else {
        plain_value(i18n::t1(locale, "native.agents.detail.capApprovalRequiredCount", "n", &detail.allowed_tools_count.to_string()))
    };

    div()
        .flex()
        .flex_col()
        .gap_3p5()
        .child(section(
            i18n::t(locale, "native.editAgent.section.permissions"),
            boxed_group(vec![
                kv_row(i18n::t(locale, "native.editAgent.row.canSendCrossAgent"), bool_badge(locale, detail.can_send_cross_agent)),
                kv_row(i18n::t(locale, "native.editAgent.row.canScheduleTasks"), bool_badge(locale, detail.can_schedule_tasks)),
                kv_row(i18n::t(locale, "native.editAgent.row.canCreateAgents"), bool_badge(locale, detail.can_create_agents)),
                kv_row(i18n::t(locale, "native.editAgent.row.canModifyOwnSoul"), bool_badge(locale, detail.can_modify_own_soul)),
            ]),
        ))
        .child(section(
            i18n::t(locale, "native.editAgent.section.toolAccess"),
            boxed_group(vec![
                kv_row(i18n::t(locale, "native.editAgent.row.autonomyLevel"), autonomy_value),
                kv_row(
                    i18n::t(locale, "native.editAgent.row.deniedToolsCount"),
                    plain_value(i18n::t1(locale, "native.agents.detail.capApprovalRequiredCount", "n", &detail.denied_tools_count.to_string())),
                ),
                kv_row(i18n::t(locale, "native.editAgent.row.allowedToolsCount"), allowed_value),
            ]),
        ))
        // CONTRACT.toml is lazily fetched only once this tab is opened —
        // see `edit_agent.rs::maybe_fetch_contract`'s own doc comment.
        .child(section(i18n::t(locale, "native.editAgent.section.contract"), contract_body(locale, contract)))
}

// ── 整合 ───────────────────────────────────────────────────────────────

/// Same shape `agents_detail.rs::channels_group` already establishes —
/// duplicated locally rather than imported (that function is private to
/// its own file, and this page reads a different `Loadable` field on a
/// different `Global`).
fn channels_body(locale: Locale, detail: &EditAgentDetail, channels: &Loadable<Vec<ChannelStatusItem>>) -> Div {
    match channels {
        Loadable::Loading => {
            let mut wrap = div().flex().flex_col().gap_1p5().p_3p5();
            for _ in 0..2 {
                wrap = wrap.child(skeleton(px(400.), px(20.)));
            }
            wrap
        }
        Loadable::Failed(_) => div()
            .px_3p5()
            .py_4()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t(locale, "native.agents.detail.channelsUnavailable")),
        Loadable::Ready(all) => {
            let is_main = is_main_role(&detail.role);
            let mine = channels_for_agent(all, &detail.id, is_main);
            if mine.is_empty() {
                return div()
                    .px_3p5()
                    .py_4()
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

pub(super) fn integration_tab(locale: Locale, detail: &EditAgentDetail, channels: &Loadable<Vec<ChannelStatusItem>>) -> Div {
    // 誠實偏差: web 表單的 Odoo 每員工覆寫（url/db/憑證）是純寫入表單，
    // `agents.inspect` 完全沒有對應欄位可回讀（也不該回讀 —— 那些欄位含憑
    // 證）。本頁不臆造「目前設定」，只留一行誠實說明，實際設定管理仍在網頁
    // 版儀表板。
    div()
        .flex()
        .flex_col()
        .gap_3p5()
        .child(section(i18n::t(locale, "native.agents.detail.channels"), channels_body(locale, detail, channels)))
        .child(section(
            i18n::t(locale, "native.editAgent.section.odoo"),
            boxed_group(vec![div()
                .px_3p5()
                .py_3()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "native.editAgent.odooNote"))]),
        ))
}

// ── 一般 ───────────────────────────────────────────────────────────────

/// Full 1:1 field coverage — every value `agents.inspect` returns for this
/// tab's identity fields has a row here (no omissions to document).
pub(super) fn general_tab(locale: Locale, detail: &EditAgentDetail) -> Div {
    let unset = || i18n::t(locale, "native.editAgent.unset");
    let icon_val = detail.icon.clone().map(SharedString::from).unwrap_or_else(unset);
    let trigger_val = detail.trigger.clone().map(SharedString::from).unwrap_or_else(unset);
    let reports_to_val = detail.reports_to.clone().map(SharedString::from).unwrap_or_else(unset);
    let department_val = if detail.department.is_empty() { unset() } else { SharedString::from(detail.department.clone()) };

    div().flex().flex_col().gap_3p5().child(section(
        i18n::t(locale, "native.editAgent.section.general"),
        boxed_group(vec![
            kv_row(i18n::t(locale, "native.editAgent.row.displayName"), plain_value(SharedString::from(detail.display_name.clone()))),
            kv_row(i18n::t(locale, "native.editAgent.row.icon"), plain_value(icon_val)),
            kv_row(i18n::t(locale, "native.editAgent.row.role"), plain_value(role_label(locale, &detail.role))),
            kv_row(i18n::t(locale, "native.editAgent.row.trigger"), plain_value(trigger_val)),
            kv_row(i18n::t(locale, "native.editAgent.row.reportsTo"), plain_value(reports_to_val)),
            kv_row(i18n::t(locale, "native.editAgent.row.department"), plain_value(department_val)),
            kv_row(i18n::t(locale, "native.editAgent.row.status"), plain_value(status_label(locale, &detail.status))),
        ]),
    ))
}
