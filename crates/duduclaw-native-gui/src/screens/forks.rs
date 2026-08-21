// WP-S5b3-H (S5b 第三波, 2026-08-21) — Screen "分支決戰" (`nav.rs` id
// `forks` — not yet wired; see this task's own "nav.rs 不歸你動" boundary,
// this page's `shell.rs` arm is hung by this same pass per the "D 先掛好分支
// 就直接可達，未掛就自己掛" precedent `screens/shell.rs`'s WP-S5b2-E comment
// already establishes).
//
// Visual authority: `commercial/design/duduclaw-s5-viz-pages/Forks.dc.html`
// (B13) — left "分支任務" list + right side-by-side branch cards (steering/
// output/spend/test/選為勝者). Functional reference: `web/src/pages/
// ForkPage.tsx`, RFC-26 Live Run Forking (layout NOT copied — see
// `forks_data.rs`'s header comment for the exact RPC shapes and the left-
// list title/branch-count deviation the real `fork.list` row shape forces).
//
// Per this task's own brief ("決策類組裝不真按"): the "選為勝者" affordance
// renders (`mds_gpui::button` with `disabled: true`) but is never wired to
// `fork.resolve` — an irreversible decision (it ends the fork and discards
// every other branch) stays exactly as decorative as every other page in
// this wave keeps its own non-actionable decision buttons.
//
// ── PathBuilder connector (spike_t7.rs primitive-1 recipe) ───────────────
// A thin canvas strip between the list column and the detail column draws
// one straight stroked line + two endpoint dot markers — the same
// `PathBuilder::stroke` / `move_to` / `line_to` / rounded-quad-as-circle
// technique `spike_t7.rs::render_edges_section` already proved, just a
// single edge instead of ~50. Deliberately the SIMPLEST of the spike's
// three connector shapes (straight, not the elbow/bezier variants) — a
// static "these two panels are linked" bridge, not a live pointer tracking
// the selected row's real on-screen position (that needs `ScrollHandle`-
// style bounds measurement this page doesn't otherwise need).

use std::collections::HashMap;

use gpui::{div, prelude::*, px, Context, Div, Global, ScrollHandle, SharedString, Stateful};
use serde_json::json;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::rpc::CallError;
use crate::screens::forks_data::{parse_fork_detail, parse_fork_list, short_id, ForkBranch, ForkDetail, ForkSummary};
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── State ──────────────────────────────────────────────────────────────

pub struct ForksState {
    requested_list: bool,
    pub forks: Loadable<Vec<ForkSummary>>,
    pub selected: Option<String>,
    pub detail_cache: HashMap<String, Loadable<ForkDetail>>,
    pub list_scroll: ScrollHandle,
    pub page_scroll: ScrollHandle,
}

impl ForksState {
    fn new() -> Self {
        Self {
            requested_list: false,
            forks: Loadable::Loading,
            selected: None,
            detail_cache: HashMap::new(),
            list_scroll: ScrollHandle::new(),
            page_scroll: ScrollHandle::new(),
        }
    }
}

impl Global for ForksState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<ForksState>() {
        cx.set_global(ForksState::new());
    }
}

fn maybe_fetch_list(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.global::<ForksState>().requested_list {
        return;
    }
    cx.global_mut::<ForksState>().requested_list = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "fork.list", json!({ "limit": 50 }), |cx, result| {
        cx.global_mut::<ForksState>().forks = result.map(|v| parse_fork_list(&v)).into();
    });
}

fn maybe_fetch_detail(fork_id: &str, state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<ForksState>().detail_cache.contains_key(fork_id) {
        return;
    }
    cx.global_mut::<ForksState>().detail_cache.insert(fork_id.to_string(), Loadable::Loading);
    let tx = state.session_tx.clone();
    let id = fork_id.to_string();
    let id_for_apply = id.clone();
    spawn_call(cx, tx, "fork.inspect", json!({ "fork_id": id }), move |cx, result| {
        cx.global_mut::<ForksState>().detail_cache.insert(id_for_apply, result.map(|v| parse_fork_detail(&v)).into());
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

// ── Left list ──────────────────────────────────────────────────────────

fn fork_row(locale: Locale, f: &ForkSummary, active: bool, is_last: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let fork_id = f.fork_id.clone();
    let mut row = div()
        .id(SharedString::from(format!("forks-row-{}", f.fork_id)))
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2p5()
        .cursor_pointer()
        .when(active, |s| s.bg(theme::alpha(theme::BRAND, 0.10)))
        .when(!active, |s| s.hover(|h| h.bg(theme::alpha(theme::MUTED, 0.3))))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().font_family("SF Mono").text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(short_id(&f.fork_id))))
                .child(if f.resolved {
                    badge(i18n::t(locale, "forks.state.resolved"), BadgeVariant::Secondary)
                } else {
                    badge(i18n::t(locale, "forks.state.open"), BadgeVariant::Info)
                }),
        )
        .child(div().text_size(px(12.5)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(f.agent_id.clone())))
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(format!("{} · ${:.4}", f.merge_mode, f.aggregate_spent_usd))),
        );

    if !is_last {
        row = row.border_b_1().border_color(theme::border());
    }
    row.on_click(cx.listener(move |_this, _ev, _window, cx| {
        cx.global_mut::<ForksState>().selected = Some(fork_id.clone());
        cx.notify();
    }))
}

// ── Connector canvas (see this module's header comment) ─────────────────

fn connector_bridge() -> Div {
    const W: f32 = 28.0;
    const H: f32 = 40.0;
    div().flex_shrink_0().w(px(W)).h(px(H)).mt(px(30.)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                let mid_y = bounds.origin.y + px(H / 2.0);
                let start = gpui::point(bounds.origin.x, mid_y);
                let end = gpui::point(bounds.origin.x + px(W), mid_y);
                let mut builder = gpui::PathBuilder::stroke(px(1.5));
                builder.move_to(start);
                builder.line_to(end);
                if let Ok(path) = builder.build() {
                    window.paint_path(path, theme::alpha(theme::BRAND, 0.6));
                }
                for center in [start, end] {
                    let r = px(2.5);
                    let dot = gpui::Bounds::new(gpui::point(center.x - r, center.y - r), gpui::size(r * 2., r * 2.));
                    window.paint_quad(gpui::quad(dot, r, theme::alpha(theme::BRAND, 0.9), px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));
                }
            },
        )
        .size_full(),
    )
}

// ── Branch card ────────────────────────────────────────────────────────

fn branch_state_badge_variant(state: &str) -> BadgeVariant {
    match state {
        "finished" => BadgeVariant::Success,
        "running" => BadgeVariant::Info,
        "budget_killed" | "failed" => BadgeVariant::Destructive,
        _ => BadgeVariant::Secondary,
    }
}

fn branch_card(locale: Locale, b: &ForkBranch, fork_resolved: bool, is_winner: bool, cx: &mut Context<RootView>) -> Div {
    let mut card = div()
        .flex_1()
        .min_w(px(240.))
        .flex()
        .flex_col()
        .gap_2p5()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(if is_winner { theme::alpha(theme::BRAND, 0.6).into() } else { theme::surface_border() });

    card = card.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .font_family("SF Mono")
                    .text_size(px(11.))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .children(is_winner.then(|| div().text_color(theme::alpha(theme::BRAND, 1.0)).child("🏆")))
                    .child(SharedString::from(short_id(&b.branch_id))),
            )
            .child(badge(i18n::t(locale, &format!("forks.branchState.{}", b.state)), branch_state_badge_variant(&b.state))),
    );

    if is_winner {
        card = card.child(badge(i18n::t(locale, "forks.branch.adoptedWinner"), BadgeVariant::Success));
    }

    if let Some(steering) = b.steering.as_deref().filter(|s| !s.is_empty()) {
        card = card.child(div().text_size(px(12.)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(steering.to_string())));
    }

    let output_text: SharedString = if b.output.is_empty() { i18n::t(locale, "forks.branch.noOutput") } else { SharedString::from(b.output.clone()) };
    card = card.child(
        div()
            .h(px(64.))
            .overflow_hidden()
            .rounded(px(theme::RADIUS_MD))
            .bg(theme::alpha(theme::MUTED, 0.35))
            .p_2()
            .text_size(px(11.5))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(output_text),
    );

    let mut meta = div().flex().items_center().justify_between().text_size(px(11.)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(
        div().font_family("SF Mono").child(SharedString::from(format!("${:.4} / ${:.2}", b.spent_usd, b.budget_usd))),
    );
    if let Some(code) = b.test_exit_code {
        let ok = code == 0;
        meta = meta.child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .font_family("SF Mono")
                .text_color(theme::alpha(if ok { theme::SUCCESS } else { theme::DESTRUCTIVE }, 1.0))
                .child(if ok { "✓" } else { "✗" })
                .child(SharedString::from(format!("test {code}"))),
        );
    }
    card = card.child(meta);

    let ratio = if b.budget_usd > 0.0 { (b.spent_usd / b.budget_usd).clamp(0.0, 1.0) } else { 0.0 };
    card = card.child(
        div().h(px(5.)).rounded(px(2.5)).bg(theme::alpha(theme::MUTED, 0.5)).overflow_hidden().child(
            div().h_full().w(gpui::relative(ratio as f32)).bg(theme::alpha(theme::BRAND, 1.0)),
        ),
    );

    let footer: Div = if is_winner {
        div()
    } else if fork_resolved {
        div().child(button("forks-branch-ended", i18n::t(locale, "forks.branch.ended"), ButtonVariant::Secondary, true, None, |_ev, _window, _cx| {}))
    } else {
        div().child(button(
            SharedString::from(format!("forks-select-winner-{}", b.branch_id)),
            i18n::t(locale, "forks.selectWinner"),
            ButtonVariant::Primary,
            true, // decorative only — see this module's header comment.
            None,
            |_ev, _window, _cx| {},
        ))
    };
    let _ = cx;
    card.child(footer)
}

// ── Right detail panel ─────────────────────────────────────────────────

fn detail_panel(locale: Locale, detail: &Loadable<ForkDetail>, cx: &mut Context<RootView>) -> Div {
    match detail {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(700.), px(60.))).child(skeleton(px(700.), px(200.))),
        Loadable::Failed(err) => div().p_4().child(badge(SharedString::from(err.clone()), BadgeVariant::Destructive)),
        Loadable::Ready(d) => {
            let header = div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3p5()
                .rounded(px(theme::RADIUS_XL))
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(if d.prompt.is_empty() { i18n::t(locale, "forks.detail.noPrompt") } else { SharedString::from(d.prompt.clone()) }))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .child(badge(SharedString::from(d.merge_mode.clone()), BadgeVariant::Secondary))
                        .child(if d.resolved { badge(i18n::t(locale, "forks.state.resolved"), BadgeVariant::Secondary) } else { badge(i18n::t(locale, "forks.state.open"), BadgeVariant::Info) })
                        .children(d.winner.as_deref().map(|w| badge(i18n::t1(locale, "forks.winner", "id", &short_id(w)), BadgeVariant::Default))),
                );

            let mut cards = div().flex().flex_wrap().gap_3();
            for b in &d.branches {
                let is_winner = d.winner.as_deref() == Some(b.branch_id.as_str());
                cards = cards.child(branch_card(locale, b, d.resolved, is_winner, cx));
            }
            if d.branches.is_empty() {
                cards = cards.child(empty_state("🔀", i18n::t(locale, "forks.branch.noOutput"), None, None::<Div>));
            }

            div().flex().flex_col().gap_3().child(header).child(cards)
        }
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_list(state, cx);

    let locale = state.locale;
    let forks = cx.global::<ForksState>().forks.clone();
    let selected = cx.global::<ForksState>().selected.clone();

    if let Some(id) = &selected {
        maybe_fetch_detail(id, state, cx);
    }

    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "forks.title")))
        .children(match &forks {
            Loadable::Ready(v) => Some(div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(v.len().to_string())),
            _ => None,
        });
    let subtitle = div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "forks.desc"));

    let list_col: Div = match &forks {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(230.), px(64.))).child(skeleton(px(230.), px(64.))),
        Loadable::Failed(err) => div().child(badge(SharedString::from(err.clone()), BadgeVariant::Destructive)),
        Loadable::Ready(rows) if rows.is_empty() => div().child(empty_state("🔀", i18n::t(locale, "forks.empty.title"), Some(i18n::t(locale, "forks.empty.hint")), None::<Div>)),
        Loadable::Ready(rows) => {
            let n = rows.len();
            let mut card = div().rounded(px(theme::RADIUS_XL)).overflow_hidden().bg(theme::alpha(theme::SURFACE, 1.0)).border_1().border_color(theme::surface_border()).flex().flex_col();
            for (i, f) in rows.iter().enumerate() {
                let active = selected.as_deref() == Some(f.fork_id.as_str());
                card = card.child(fork_row(locale, f, active, i + 1 == n, cx));
            }
            card
        }
    };

    let has_selection_with_branches = selected.is_some()
        && matches!(
            selected.as_deref().and_then(|id| cx.global::<ForksState>().detail_cache.get(id)),
            Some(Loadable::Ready(d)) if !d.branches.is_empty()
        );

    let detail_col: Div = match &selected {
        None => div().child(empty_state("🔀", i18n::t(locale, "forks.detail.empty"), None, None::<Div>)),
        Some(id) => {
            let detail = cx.global::<ForksState>().detail_cache.get(id).cloned().unwrap_or(Loadable::Loading);
            detail_panel(locale, &detail, cx)
        }
    };

    let list_col_scroll = div()
        .id("forks-list-scroll")
        .max_h(px(560.))
        .overflow_y_scroll()
        .track_scroll(&cx.global::<ForksState>().list_scroll)
        .child(list_col);
    let mut columns = div().flex().gap_3().items_start().child(div().w(px(250.)).flex_shrink_0().flex().flex_col().gap_2().child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "forks.list.heading"))).child(list_col_scroll));
    if has_selection_with_branches {
        columns = columns.child(connector_bridge());
    }
    columns = columns.child(div().flex_1().min_w(px(320.)).child(detail_col));

    div()
        .id("forks-page")
        .size_full()
        .track_scroll(&cx.global::<ForksState>().page_scroll)
        .overflow_y_scroll()
        .child(div().max_w(px(1200.)).mx_auto().flex().flex_col().gap_3().child(header).child(subtitle).child(columns))
}
