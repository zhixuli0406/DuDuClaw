// WP-S5b3-H (S5b 第三波, 2026-08-21) — Screen "組織架構" (`nav.rs` id `org` —
// not yet wired; see this task's own "nav.rs 不歸你動" boundary, this page's
// `shell.rs` arm is hung by this same pass per the "D 先掛好分支就直接可達，
// 未掛就自己掛" precedent `screens/shell.rs`'s WP-S5b2-E comment already
// establishes).
//
// Visual authority: `commercial/design/duduclaw-s5-viz-pages/OrgIndented.dc.
// html` (方案 A 縮排階層清單 — the user's own 2026-08-21 拍板; B/C
// alternatives, `OrgTree.dc.html`/`OrgFocus.dc.html`, are explicitly NOT
// built). Functional reference: `web/src/pages/OrgChartPage.tsx` (layout
// NOT copied — that page renders an interactive canvas org-chart via
// `OrgChart`; this page is a plain indented list per the approved canvas).
//
// ── Honest deviations from the canvas ─────────────────────────────────────
// 1. The mockup's per-row grey caption ("行政總管"/"財務助理"/…) reads like a
//    decorative job title, but `AgentInfo` has no such field — real data
//    only has `department`/`role`/`trigger`. This page shows `department`
//    there instead (omitted entirely when empty), never a fabricated title.
// 2. Status dot colors follow this crate's own established `agents.rs`
//    convention (active/unrecognized→BRAND, paused→WARNING,
//    terminated→DESTRUCTIVE), not the mockup's literal green/gray — same
//    "theme tokens, not hex-matched to a static mockup" rule every other
//    S5b page in this crate already follows.
// 3. Avatar circles are a solid BRAND-tinted initial, not the mockup's
//    per-row distinct hues (mock uses a slightly darker grey for the
//    paused 小點 row and blue for everyone else — arbitrary mockup flavor,
//    not status-encoding, so not reproduced) — matches this crate's
//    existing avatar convention (`screens::runs`/`screens::agents_list`).

use std::collections::HashSet;

use gpui::{div, prelude::*, px, Context, Div, Global, ScrollHandle, SharedString, Stateful};
use serde_json::json;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::rpc::CallError;
use crate::screens::org_data::{flatten_tree, parse_org_agents, OrgAgentRow, OrgNode};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

const INDENT_PER_DEPTH: f32 = 26.0;

// ── State ──────────────────────────────────────────────────────────────

pub struct OrgState {
    requested: bool,
    pub agents: Loadable<Vec<OrgAgentRow>>,
    pub collapsed: HashSet<String>,
    pub page_scroll: ScrollHandle,
}

impl OrgState {
    fn new() -> Self {
        Self { requested: false, agents: Loadable::Loading, collapsed: HashSet::new(), page_scroll: ScrollHandle::new() }
    }
}

impl Global for OrgState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<OrgState>() {
        cx.set_global(OrgState::new());
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.global::<OrgState>().requested {
        return;
    }
    cx.global_mut::<OrgState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
        cx.global_mut::<OrgState>().agents = result.map(|v| parse_org_agents(&v)).into();
    });
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: serde_json::Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<serde_json::Value, String>) + 'static,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, method, params);
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            apply(cx, outcome);
            cx.notify();
        });
    })
    .detach();
}

fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Labels (local copies of `screens::agents`'s role/status mapping — that
// module's own functions are `pub(super)`, i.e. private to `screens::
// agents`'s child modules; this crate's established precedent for a second
// page needing the same small mapping is a local copy, not widened
// visibility — see `screens/agents.rs`'s own `avatar_glyph` doc comment for
// the identical precedent this follows) ──────────────────────────────────

fn role_label(locale: Locale, role: &str) -> SharedString {
    let key = match role {
        "main" => "native.agents.role.main",
        "specialist" => "native.agents.role.specialist",
        "worker" => "native.agents.role.worker",
        "developer" => "native.agents.role.developer",
        "qa" => "native.agents.role.qa",
        "planner" => "native.agents.role.planner",
        "teamleader" => "native.agents.role.teamLeader",
        "productmanager" => "native.agents.role.productManager",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

fn role_badge_variant(role: &str) -> BadgeVariant {
    match role {
        "main" => BadgeVariant::Info,
        "worker" => BadgeVariant::Secondary,
        _ => BadgeVariant::Default, // specialist + any other role
    }
}

fn status_label(locale: Locale, status: &str) -> SharedString {
    let key = match status {
        "active" => "native.agents.status.active",
        "paused" => "native.agents.status.paused",
        "terminated" => "native.agents.status.terminated",
        other => return other.to_string().into(),
    };
    i18n::t(locale, key)
}

fn status_dot_color(status: &str) -> u32 {
    match status {
        "paused" => theme::WARNING,
        "terminated" => theme::DESTRUCTIVE,
        _ => theme::BRAND,
    }
}

fn avatar_glyph(display_name: &str, icon: Option<&str>) -> String {
    if let Some(icon) = icon {
        return icon.to_string();
    }
    display_name.chars().next().map(|c| c.to_string()).unwrap_or_else(|| "?".to_string())
}

// ── Row ────────────────────────────────────────────────────────────────

fn org_row(locale: Locale, node: &OrgNode, is_collapsed: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let a = &node.agent;
    let indent = 12.0 + node.depth as f32 * INDENT_PER_DEPTH;

    let disclosure: Div = if node.has_children {
        div()
            .flex_shrink_0()
            .w(px(12.))
            .text_size(px(10.))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(if is_collapsed { "▸" } else { "▾" })
    } else {
        div().flex_shrink_0().w(px(12.))
    };

    let avatar = div()
        .flex_shrink_0()
        .w(px(28.))
        .h(px(28.))
        .rounded(px(9.))
        .bg(theme::alpha(theme::BRAND, 0.85))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
        .child(SharedString::from(avatar_glyph(&a.display_name, a.icon.as_deref())));

    let mut row = div()
        .id(SharedString::from(format!("org-row-{}", a.id)))
        .flex()
        .items_center()
        .gap_2()
        .pl(px(indent))
        .pr_3()
        .py_2()
        .rounded(px(theme::RADIUS_MD))
        .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.3)))
        .child(disclosure)
        .child(avatar)
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(a.display_name.clone())))
        .child(badge(role_label(locale, &a.role), role_badge_variant(&a.role)));

    if !a.department.is_empty() {
        row = row.child(div().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(a.department.clone())));
    }

    row = row.child(
        div()
            .ml_auto()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(px(10.5))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(div().w(px(6.)).h(px(6.)).rounded(px(3.)).bg(theme::alpha(status_dot_color(&a.status), 1.0)))
            .child(status_label(locale, &a.status)),
    );

    if node.has_children {
        let id = a.id.clone();
        row = row.cursor_pointer().on_click(cx.listener(move |_this, _ev, _window, cx| {
            let g = cx.global_mut::<OrgState>();
            if !g.collapsed.remove(&id) {
                g.collapsed.insert(id.clone());
            }
            cx.notify();
        }));
    }

    row
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let agents = cx.global::<OrgState>().agents.clone();
    let collapsed = cx.global::<OrgState>().collapsed.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "org.title")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "org.desc")));

    let body: Div = match &agents {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(700.), px(44.))).child(skeleton(px(700.), px(44.))).child(skeleton(px(700.), px(44.))),
        Loadable::Failed(err) => div().p_4().child(badge(SharedString::from(err.clone()), BadgeVariant::Destructive)),
        Loadable::Ready(rows) if rows.is_empty() => div().child(empty_state("🧑‍🤝‍🧑", i18n::t(locale, "org.empty"), Some(i18n::t(locale, "org.empty.hint")), None::<Div>)),
        Loadable::Ready(rows) => {
            let nodes = flatten_tree(rows, &collapsed);
            let mut card = div().rounded(px(theme::RADIUS_XL)).p_2().bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).flex().flex_col().gap_0p5();
            for node in &nodes {
                let is_collapsed = collapsed.contains(&node.agent.id);
                card = card.child(org_row(locale, node, is_collapsed, cx));
            }
            card
        }
    };

    div()
        .id("org-page")
        .size_full()
        .track_scroll(&cx.global::<OrgState>().page_scroll)
        .overflow_y_scroll()
        .child(div().max_w(px(900.)).mx_auto().flex().flex_col().gap_3().child(header).child(body))
}
