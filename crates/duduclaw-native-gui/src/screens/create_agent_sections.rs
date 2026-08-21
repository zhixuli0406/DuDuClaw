// Section-level rendering for `create_agent.rs` (WP-S6b2-N, "新增員工") —
// split out purely to keep `create_agent.rs` under this crate's own
// <800-line file-size convention (the page's render logic alone, with
// `CreateAgentState` + fetch orchestration, ran to ~890 lines before this
// split). Same reason a single page gets split across multiple sibling
// files elsewhere in this crate (`agents.rs`/`agents_list.rs`/`agents_
// detail.rs`/`agents_summary.rs` all being ONE page; `goals.rs`/`goals_
// inspector.rs` another) — no behavior differs from an unsplit version.
// Declared as a private sibling (`mod create_agent_sections;`, no `pub`) in
// `screens/mod.rs`, same visibility shape `agents_data`/`agents_detail`
// already establish for their own page.
//
// Entry points `create_agent.rs::render` calls directly (`breadcrumb`/
// `header_row`/`template_section`/`basics_section`/`org_section`/`soul_and_
// advanced_section`) are `pub(super)` — visible throughout `crate::screens`,
// the same "sibling module reach" convention `goals.rs::spawn_goal_call`
// already establishes. Everything else here (the local `kv_row`/`boxed_
// group` primitives, `template_card`, `field_row`, `cycle_chip`, …) is
// module-private — only this file's own entry points use them.

use gpui::{div, prelude::*, px, Context, Div, Entity, IntoElement, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::agents_data::AgentListItem;
use crate::screens::create_agent::{select_role, toggle_advanced, CreateAgentState};
use crate::screens::create_agent_data::{self, DepartmentItem, TemplateRoleDetail, TemplateRosterData};
use crate::screens::dashboard::Loadable;
use crate::text_field::TextField;
use crate::theme;
use crate::RootView;

const CONTENT_WIDTH: f32 = 680.0;

// ── Shared "boxed-list" primitives — local to this file, NOT shared with
// `agents_detail.rs`/other pages, per this crate's own established
// "duplicate the kv_row/boxed_group pair locally rather than widen a
// sibling module's visibility" convention (see `agents_detail.rs`'s and
// `settings_common.rs`'s own header comments for the precedent and its one
// documented exception, which this page is not part of). ─────────────────

fn kv_row(label: impl Into<SharedString>, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(44.))
        .px_3p5()
        .text_size(px(theme::TEXT_SM))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
        .child(value)
}

/// Same shape as `kv_row` but with an optional second caption line under the
/// label — the 直屬上級 row's "留白則沿用範本內建的匯報對象" instructional
/// caption needs this; plain `kv_row` has no slot for it.
fn kv_row_captioned(label: impl Into<SharedString>, caption: Option<SharedString>, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .py_2p5()
        .px_3p5()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label.into()))
                .children(caption.map(|c| div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.75)).child(c))),
        )
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

fn section_label(text: SharedString) -> Div {
    div()
        .px_0p5()
        .pb_1()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text)
}

// ── Header ─────────────────────────────────────────────────────────────

pub(super) fn breadcrumb(locale: Locale, cx: &mut Context<RootView>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id("create-agent-breadcrumb-root")
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.agents.title"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "agents";
                    cx.notify();
                })),
        )
        .child(div().child("›"))
        .child(div().text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "createAgent.breadcrumb.current")))
}

pub(super) fn header_row(locale: Locale, cx: &mut Context<RootView>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(
                    div()
                        .size(px(34.))
                        .flex_shrink_0()
                        .rounded(px(theme::RADIUS_MD))
                        .bg(theme::alpha(theme::MUTED, 0.6))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(16.))
                        .child("👥"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(theme::TEXT_BASE)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "createAgent.title")))
                        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "createAgent.subtitle"))),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(button(
                    "create-agent-cancel",
                    i18n::t(locale, "createAgent.action.cancel"),
                    ButtonVariant::Secondary,
                    false,
                    None,
                    cx.listener(|this, _ev, _window, cx| {
                        this.active_page = "agents";
                        cx.notify();
                    }),
                ))
                // ── ASSEMBLED, NOT WIRED ────────────────────────────────
                // This is the one write-path button on this page (see
                // `create_agent.rs`'s own module doc comment for the
                // crate-wide convention it follows). It would call
                // `templates.create_agent` (params: role_id/industry?/name/
                // display_name/trigger/reports_to?/department?/soul_md/
                // contract_toml?/agent_toml?, handlers.rs:8114) when
                // `selected_role_id` is `Some`, or `agents.create` (params:
                // name/display_name/role/trigger/model_preferred?/
                // reports_to?/department?, handlers.rs:7525) when it's
                // `None` (空白開始). Both are `require_admin!()`-gated
                // writes with lasting side effects — a new agent directory,
                // an `org.toml` entry, an installed file-guard hook — and
                // this pass has no live authenticated gateway session to
                // verify the write actually lands correctly against, so it
                // stays an honest, real-looking, unwired button rather than
                // plumbing calling code nobody has run once.
                .child(button(
                    "create-agent-submit",
                    i18n::t(locale, "createAgent.action.submit"),
                    ButtonVariant::Primary,
                    false,
                    None,
                    cx.listener(|_, _, _, _| {}),
                )),
        )
}

// ── Template picker ────────────────────────────────────────────────────

fn template_card(id: SharedString, title: SharedString, summary: SharedString, selected: bool, created: bool, on_select: Option<Option<String>>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let mut card = div()
        .id(id)
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .flex_1()
        .min_w(px(300.));

    card = if selected {
        card.border_2().border_color(theme::alpha(theme::BRAND, 0.5))
    } else {
        card.border_1().border_color(theme::surface_border())
    };

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title))
        .children(if selected {
            Some(div().text_size(px(13.)).text_color(theme::alpha(theme::BRAND, 1.0)).child("✓"))
        } else {
            None
        });

    card = card.child(header).child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(summary));

    if created {
        card = card.opacity(0.55).cursor(gpui::CursorStyle::OperationNotAllowed);
    } else if let Some(role_id) = on_select {
        card = card
            .cursor_pointer()
            .hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
            .on_click(cx.listener(move |_this, _ev, _window, cx| {
                select_role(cx, role_id.clone());
                cx.notify();
            }));
    }
    card
}

pub(super) fn template_section(locale: Locale, roster: &Loadable<Option<TemplateRosterData>>, selected_role_id: &Option<String>, cx: &mut Context<RootView>) -> Div {
    let mut grid = div().flex().flex_wrap().gap_2p5();

    match roster {
        Loadable::Loading => {
            for _ in 0..4 {
                grid = grid.child(skeleton(px(300.), px(64.)).flex_1().min_w(px(300.)));
            }
        }
        Loadable::Failed(_) | Loadable::Ready(None) => {
            // Silent fallback (see `roster` field's own doc comment) — only
            // the always-available "空白開始" card renders.
            grid = grid.child(template_card(
                "create-agent-template-blank".into(),
                i18n::t(locale, "createAgent.template.blankTitle"),
                i18n::t(locale, "createAgent.template.blankSummary"),
                selected_role_id.is_none(),
                false,
                Some(None),
                cx,
            ));
        }
        Loadable::Ready(Some(roster)) => {
            grid = grid.child(template_card(
                "create-agent-template-blank".into(),
                i18n::t(locale, "createAgent.template.blankTitle"),
                i18n::t(locale, "createAgent.template.blankSummary"),
                selected_role_id.is_none(),
                false,
                Some(None),
                cx,
            ));
            let mut roles = roster.roles.clone();
            roles.sort_by_key(|r| create_agent_data::kind_sort_key(&r.kind));
            for role in roles {
                let selected = selected_role_id.as_deref() == Some(role.role_id.as_str());
                let title: SharedString = role.display_name.clone().into();
                let summary: SharedString = role.summary.clone().into();
                let id: SharedString = format!("create-agent-template-{}", role.role_id).into();
                let created = role.created;
                let role_id = role.role_id.clone();
                let mut card = template_card(id, title, summary, selected, created, if created { None } else { Some(Some(role_id)) }, cx);
                if created {
                    card = card.child(div().mt_1().child(badge(i18n::t(locale, "createAgent.template.createdBadge"), BadgeVariant::Outline)));
                }
                grid = grid.child(card);
            }
        }
    }

    div().flex().flex_col().child(section_label(i18n::t(locale, "createAgent.template.sectionTitle"))).child(grid)
}

// ── 基本資料 ────────────────────────────────────────────────────────────

fn field_row(label: SharedString, field: Entity<TextField>, extra_badge: Option<Div>) -> Div {
    let value = div()
        .flex_1()
        .flex()
        .items_center()
        .justify_end()
        .gap_2()
        .children(extra_badge)
        .child(div().w(px(220.)).child(field));
    kv_row(label, value)
}

pub(super) fn basics_section(locale: Locale, g: &CreateAgentState) -> Div {
    let show_customized_badge = g.selected_role_id.is_some() && g.display_name_touched;
    let customized_badge = show_customized_badge.then(|| badge(i18n::t(locale, "createAgent.basics.customizedBadge"), BadgeVariant::Warning));

    let rows = vec![
        field_row(i18n::t(locale, "createAgent.basics.id"), g.id_field.clone(), None),
        field_row(i18n::t(locale, "createAgent.basics.displayName"), g.display_name_field.clone(), customized_badge),
        field_row(i18n::t(locale, "createAgent.basics.trigger"), g.trigger_field.clone(), None),
    ];

    div().flex().flex_col().child(section_label(i18n::t(locale, "createAgent.basics.sectionTitle"))).child(boxed_group(rows))
}

// ── 組織位置 ────────────────────────────────────────────────────────────

fn agent_display_label(agents: &[AgentListItem], id: &str) -> SharedString {
    agents.iter().find(|a| a.id == id).map(|a| a.display_name.clone().into()).unwrap_or_else(|| id.to_string().into())
}

/// Click-to-cycle chip — this crate's established `mds_gpui`-has-no-
/// dropdown-menu-primitive substitute (`wiki_trust.rs::agent_picker_chip`'s
/// own doc comment documents the same gap and the same fix). Cycle order:
/// default (`None`) → each option in list order → back to default.
fn cycle_chip(id: SharedString, label: SharedString, options: Vec<String>, current: Option<String>, on_change: impl Fn(&mut Context<RootView>, Option<String>) + 'static, cx: &mut Context<RootView>) -> Stateful<Div> {
    let clickable = !options.is_empty();
    let mut chip = div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(label);
    if clickable {
        chip = chip.cursor_pointer().hover(|s| s.bg(theme::alpha(theme::MUTED, 0.4))).on_click(cx.listener(move |_this, _ev, _window, cx| {
            let next = match &current {
                None => Some(options[0].clone()),
                Some(cur) => match options.iter().position(|o| o == cur) {
                    Some(i) if i + 1 < options.len() => Some(options[i + 1].clone()),
                    _ => None,
                },
            };
            on_change(cx, next);
            cx.notify();
        }));
    }
    chip
}

// Takes every piece of state it needs as an already-cloned, cx-independent
// value (never a live `&CreateAgentState` borrow) — this function also needs
// `cx` itself (for `cycle_chip`'s `cx.listener`), and holding a borrow
// derived from `cx.global::<CreateAgentState>()` while also taking `cx: &mut
// Context<RootView>` in the same call would be a live shared+exclusive
// aliasing conflict the borrow checker rejects outright. Callers snapshot
// (clone) the fields from the global first — see `create_agent.rs::render`'s
// own call site.
#[allow(clippy::too_many_arguments)]
pub(super) fn org_section(
    locale: Locale,
    agents_list: &Loadable<Vec<AgentListItem>>,
    departments: &Loadable<Vec<DepartmentItem>>,
    reports_to: &Option<String>,
    department: &Option<String>,
    template_reports_to: Option<String>,
    cx: &mut Context<RootView>,
) -> Div {
    let reports_to_label: SharedString = match reports_to {
        Some(id) => match agents_list {
            Loadable::Ready(list) => agent_display_label(list, id),
            _ => id.clone().into(),
        },
        None => match &template_reports_to {
            Some(name) => i18n::t1(locale, "createAgent.org.reportsToTemplateDefault", "name", name),
            None => i18n::t(locale, "createAgent.org.reportsToNone"),
        },
    };
    let reports_to_options: Vec<String> = match agents_list {
        Loadable::Ready(list) => list.iter().map(|a| a.id.clone()).collect(),
        _ => Vec::new(),
    };
    let reports_to_chip = cycle_chip(
        "create-agent-reports-to-chip".into(),
        reports_to_label,
        reports_to_options,
        reports_to.clone(),
        |cx, next| {
            cx.global_mut::<CreateAgentState>().reports_to = next;
        },
        cx,
    );

    let department_label: SharedString = match department {
        Some(name) => name.clone().into(),
        None => i18n::t(locale, "createAgent.org.departmentNone"),
    };
    let department_options: Vec<String> = match departments {
        Loadable::Ready(list) => list.iter().map(|d| d.name.clone()).collect(),
        _ => Vec::new(),
    };
    let department_chip = cycle_chip(
        "create-agent-department-chip".into(),
        department_label,
        department_options,
        department.clone(),
        |cx, next| {
            cx.global_mut::<CreateAgentState>().department = next;
        },
        cx,
    );

    let rows = vec![
        kv_row_captioned(i18n::t(locale, "createAgent.org.reportsTo"), Some(i18n::t(locale, "createAgent.org.reportsToCaption")), reports_to_chip),
        kv_row(i18n::t(locale, "createAgent.org.department"), department_chip),
    ];

    div().flex().flex_col().child(section_label(i18n::t(locale, "createAgent.org.sectionTitle"))).child(boxed_group(rows))
}

// ── SOUL.md preview + 進階 disclosure (only when a template is selected —
// the blank path has no template body to preview: `agents.create` doesn't
// even accept a `soul_md`/`contract_toml`/`agent_toml` override, it accepts
// an unrelated optional `soul` one-liner and no TOML overrides at all, so
// showing empty preview boxes on the blank path would show content that
// doesn't exist rather than an honest absence). ───────────────────────────

pub(super) fn soul_and_advanced_section(locale: Locale, role_detail: &Loadable<TemplateRoleDetail>, advanced_open: bool, cx: &mut Context<RootView>) -> Div {
    let (soul_md, contract_toml, agent_toml) = match role_detail {
        Loadable::Ready(d) => (d.soul_md.clone(), d.contract_toml.clone(), d.agent_toml.clone()),
        _ => (String::new(), String::new(), String::new()),
    };

    let soul_block: Div = match role_detail {
        Loadable::Loading => skeleton(px(CONTENT_WIDTH - 28.), px(96.)),
        Loadable::Failed(msg) => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(i18n::t1(locale, "native.home.card.errorPrefix", "message", msg)),
        Loadable::Ready(_) => div()
            .text_size(px(11.5))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .line_height(px(19.))
            .child(SharedString::from(soul_md)),
    };

    let soul_section = div()
        .flex()
        .flex_col()
        .child(section_label(i18n::t(locale, "createAgent.soul.sectionTitle")))
        .child(div().rounded(px(theme::RADIUS_XL)).bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).p_3().child(soul_block));

    let disclosure_row = div()
        .id("create-agent-advanced-toggle")
        .mt_2()
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .px_3p5()
        .py_2()
        .cursor_pointer()
        .hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "createAgent.advanced.toggle")))
        .child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(if advanced_open { "▲" } else { "▼" }))
        .on_click(cx.listener(|_this, _ev, _window, cx| {
            toggle_advanced(cx);
            cx.notify();
        }));

    let mut wrap = soul_section.child(disclosure_row);

    if advanced_open {
        let toml_block = |title: SharedString, content: String| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(title))
                .child(
                    div()
                        .rounded(px(theme::RADIUS_LG))
                        .bg(theme::alpha(theme::SURFACE, 1.0))
                        .border_1()
                        .border_color(theme::surface_border())
                        .p_2p5()
                        .text_size(px(11.))
                        .line_height(px(18.))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(SharedString::from(content)),
                )
        };
        wrap = wrap.child(
            div()
                .mt_2()
                .flex()
                .flex_col()
                .gap_2()
                .child(toml_block(i18n::t(locale, "createAgent.advanced.contractLabel"), contract_toml))
                .child(toml_block(i18n::t(locale, "createAgent.advanced.agentTomlLabel"), agent_toml)),
        );
    }

    wrap
}
