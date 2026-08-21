// WP-S6b1-K (S6b 第一波, 2026-08-21) — "Wiki 信任層級" (`WikiTrustPage.dc.
// html`, B18). A "進階設定" drill-down leaf (`active_page == "wikiTrust"`,
// no `nav.rs` entry of its own), reached as the second tab of the
// `GovernanceShell` this page shares with `governance.rs` (see that
// module's own header comment for the shell/tabs/breadcrumb/spawn_call
// sharing rationale — this file imports all four rather than duplicating
// them, same batch).
//
// Visual authority: `commercial/design/duduclaw-s6-biz-pages/WikiTrustPage.
// dc.html` — breadcrumb → GovernanceShell tabs (Wiki 信任 active) → header
// (title/subtitle + 員工選擇 + 信任上限) → 3 stat tiles (已封存/已鎖定/訊號
// 數) → boxed trust table (頁面/信任分數/引用數/誤/正/標記).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ──────────────────────────────────────────────────
//   `agents.list {}` (~L5396, `handle_agents_list_filtered`) — reused via
//   `crate::screens::agents_data::parse_agents_list` (the S4b "AI 員工"
//   page's own parser, reachable cross-file per this crate's established
//   "private `mod`, `pub` items reachable within `crate::screens`"
//   convention — see `screens/mod.rs`'s own header comment) — populates the
//   員工 picker. First agent in the list becomes the default selection,
//   mirroring `WikiTrustPage.tsx`'s own `list[0].name` fallback.
//   `wiki.trust_audit` (~L5869, handler `handle_wiki_trust_audit`
//   ~L14441, `require_admin!()`) — params `{agent_id, max_trust, limit}` →
//   `{"rows":[{"page_path","agent_id","trust","citation_count",
//   "error_signal_count","success_signal_count","do_not_inject","locked",
//   ...}], "available":bool, "note"?}`. `available:false` (trust store not
//   initialized) carries a `note` explaining why — rendered as a warning
//   banner, same as `WikiTrustPage.tsx`'s own `available === false && note`
//   branch.
//   `wiki.trust_history`/`wiki.trust_override` (~L5873/5877) exist and are
//   real, but the approved canvas draws no per-row detail dialog for this
//   page (unlike `web/src/pages/WikiTrustPage.tsx`'s own click-to-inspect
//   dialog) — per this task's "版面禁抄 web，視覺=拍板畫布" priority rule,
//   this page stops at the flat table the canvas actually draws; wiring a
//   dialog neither canvas artboard shows would be scope creep, not fidelity.
//
// ── Deviations from the canvas (documented, not silent) ──────────────────
// 1. Employee/threshold pickers — the canvas draws both as collapsed
//    dropdown-style chips (a value + a chevron, implying an overlay menu).
//    `mds_gpui` ships no dropdown-menu primitive yet (confirmed: only
//    badge/button/card/dialog/empty_state/skeleton/table/tabs/toast exist
//    — see `mds_gpui/mod.rs`), and building one is out of this WP's scope.
//    Both render instead as real, already-established filter-chip controls
//    (same idiom `governance.rs`'s own policy-type chips and `inbox_rows::
//    filter_chip` use): the employee picker is a single click-to-cycle chip
//    (advances to the next agent in `agents.list`'s order); the threshold
//    picker is a 4-option chip row (≤0.30/≤0.50/≤0.70/全部), matching the
//    real `<Select>` options `WikiTrustPage.tsx` itself offers. Both are
//    genuinely wired — clicking either re-fetches `wiki.trust_audit` for
//    real, unlike the canvas's static "客服 · Alice" / "上限 0.90" mockup
//    values.
// 2. Trust-score bar color — the canvas color-codes each row's bar
//    (green/amber/red) by its trust value; this page reproduces that
//    exact 3-tier scheme (≥0.6 success / ≥0.3 warning / else destructive)
//    rather than `WikiTrustPage.tsx`'s own flat single-hue bar — visual
//    fidelity to the approved canvas wins per this task's priority rule.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::screens::agents_data::{self, AgentListItem};
use crate::screens::dashboard::{error_row, Loadable};
use crate::screens::governance::{breadcrumb, shell_tabs, spawn_call};
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

const THRESHOLDS: [f32; 4] = [0.3, 0.5, 0.7, 1.0];

#[derive(Debug, Clone, PartialEq)]
pub struct WikiTrustRow {
    pub page_path: String,
    pub trust: f32,
    pub citation_count: i64,
    pub error_signal_count: i64,
    pub success_signal_count: i64,
    pub do_not_inject: bool,
    pub locked: bool,
}

pub fn parse_wiki_trust_audit(v: &Value) -> (Vec<WikiTrustRow>, bool, Option<String>) {
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some(WikiTrustRow {
                        page_path: r.get("page_path")?.as_str()?.to_string(),
                        trust: r.get("trust").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                        citation_count: r.get("citation_count").and_then(Value::as_i64).unwrap_or(0),
                        error_signal_count: r.get("error_signal_count").and_then(Value::as_i64).unwrap_or(0),
                        success_signal_count: r.get("success_signal_count").and_then(Value::as_i64).unwrap_or(0),
                        do_not_inject: r.get("do_not_inject").and_then(Value::as_bool).unwrap_or(false),
                        locked: r.get("locked").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let available = v.get("available").and_then(Value::as_bool).unwrap_or(true);
    let note = v.get("note").and_then(Value::as_str).map(str::to_string);
    (rows, available, note)
}

struct TrustSummary {
    archived: usize,
    locked: usize,
    total_err: i64,
    total_ok: i64,
}

fn summarize(rows: &[WikiTrustRow]) -> TrustSummary {
    TrustSummary {
        archived: rows.iter().filter(|r| r.do_not_inject).count(),
        locked: rows.iter().filter(|r| r.locked).count(),
        total_err: rows.iter().map(|r| r.error_signal_count).sum(),
        total_ok: rows.iter().map(|r| r.success_signal_count).sum(),
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct WikiTrustState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub selected_agent: Option<String>,
    pub max_trust: f32,
    pub rows: Loadable<Vec<WikiTrustRow>>,
    pub available: bool,
    pub note: Option<String>,
    /// `(agent_id, max_trust as a fixed-precision string)` the current
    /// `rows`/`available`/`note` were fetched for — compared every render
    /// against the live selection to decide whether a fresh `wiki.
    /// trust_audit` round trip is needed. Generalizes the "requested"
    /// boolean guard every other page's `maybe_fetch` uses into a
    /// key-comparison guard, since this page's fetch depends on two
    /// user-changeable filters, not a fixed one-shot load.
    fetched_for: Option<(String, String)>,
}

impl WikiTrustState {
    fn new() -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            selected_agent: None,
            max_trust: 0.5,
            rows: Loadable::Loading,
            available: true,
            note: None,
            fetched_for: None,
        }
    }
}

impl Global for WikiTrustState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<WikiTrustState>() {
        cx.set_global(WikiTrustState::new());
    }
}

fn maybe_fetch_agents(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<WikiTrustState>().requested_agents {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<WikiTrustState>().requested_agents = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
        match result {
            Ok(v) => {
                let list = agents_data::parse_agents_list(&v);
                if cx.global::<WikiTrustState>().selected_agent.is_none() {
                    if let Some(first) = list.first() {
                        cx.global_mut::<WikiTrustState>().selected_agent = Some(first.id.clone());
                    }
                }
                cx.global_mut::<WikiTrustState>().agents = Loadable::Ready(list);
            }
            Err(e) => cx.global_mut::<WikiTrustState>().agents = Loadable::Failed(e),
        }
    });
}

fn maybe_fetch_trust(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let (agent, max_trust, fetched_for) = {
        let st = cx.global::<WikiTrustState>();
        (st.selected_agent.clone(), st.max_trust, st.fetched_for.clone())
    };
    let Some(agent) = agent else { return };
    let key = (agent.clone(), format!("{max_trust:.2}"));
    if fetched_for.as_ref() == Some(&key) {
        return;
    }
    cx.global_mut::<WikiTrustState>().fetched_for = Some(key);
    cx.global_mut::<WikiTrustState>().rows = Loadable::Loading;
    let tx = state.session_tx.clone();
    spawn_call(
        cx,
        tx,
        "wiki.trust_audit",
        json!({ "agent_id": agent, "max_trust": max_trust, "limit": 200 }),
        |cx, result| match result {
            Ok(v) => {
                let (rows, available, note) = parse_wiki_trust_audit(&v);
                let st = cx.global_mut::<WikiTrustState>();
                st.rows = Loadable::Ready(rows);
                st.available = available;
                st.note = note;
            }
            Err(e) => cx.global_mut::<WikiTrustState>().rows = Loadable::Failed(e),
        },
    );
}

// ── Header controls (see module doc comment deviation #1) ────────────────

fn agent_display_label(agents: &[AgentListItem], id: &str) -> SharedString {
    match agents.iter().find(|a| a.id == id) {
        Some(a) if !a.department.is_empty() => format!("{} · {}", a.department, a.display_name).into(),
        Some(a) => a.display_name.clone().into(),
        None => id.to_string().into(),
    }
}

fn agent_picker_chip(locale: Locale, agents: &Loadable<Vec<AgentListItem>>, selected: &Option<String>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let label = match (agents, selected) {
        (Loadable::Ready(list), Some(id)) => agent_display_label(list, id),
        _ => i18n::t(locale, "wikiTrust.agent"),
    };
    let list_snapshot: Vec<String> = match agents {
        Loadable::Ready(list) => list.iter().map(|a| a.id.clone()).collect(),
        _ => Vec::new(),
    };
    let current = selected.clone();
    div()
        .id("wikitrust-agent-chip")
        .h(px(30.))
        .px_3()
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(px(theme::RADIUS_LG))
        .cursor_pointer()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
        .hover(|s| s.bg(theme::alpha(theme::MUTED, 0.4)))
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            if list_snapshot.is_empty() {
                return;
            }
            let next = match &current {
                Some(cur) => {
                    let idx = list_snapshot.iter().position(|id| id == cur).unwrap_or(0);
                    list_snapshot[(idx + 1) % list_snapshot.len()].clone()
                }
                None => list_snapshot[0].clone(),
            };
            cx.global_mut::<WikiTrustState>().selected_agent = Some(next);
            cx.notify();
        }))
}

fn threshold_chip(locale: Locale, value: f32, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let label: SharedString = if value >= 1.0 {
        i18n::t(locale, "wikiTrust.maxTrust.all")
    } else {
        format!("≤ {value:.2}").into()
    };
    let id: SharedString = format!("wikitrust-threshold-{value:.2}").into();
    div()
        .id(id)
        .h(px(26.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        })
        .child(label)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<WikiTrustState>().max_trust = value;
            cx.notify();
        }))
}

// ── Stat tiles ─────────────────────────────────────────────────────────

fn stat_tile(label: SharedString, value: String, color: u32) -> Div {
    div()
        .flex_1()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .px_4()
        .py_3()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(
            div()
                .mt_1()
                .text_size(px(19.))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(color, 1.0))
                .child(value),
        )
}

// ── Trust table ────────────────────────────────────────────────────────

fn trust_bar_color(trust: f32) -> u32 {
    if trust >= 0.6 {
        theme::SUCCESS
    } else if trust >= 0.3 {
        theme::WARNING
    } else {
        theme::DESTRUCTIVE
    }
}

fn table_header(locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(theme::alpha(theme::MUTED, 0.35))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(div().flex_1().min_w_0().child(i18n::t(locale, "wikiTrust.col.page")))
        .child(div().w(px(100.)).flex_shrink_0().child(i18n::t(locale, "wikiTrust.col.trust")))
        .child(div().w(px(50.)).flex_shrink_0().child(i18n::t(locale, "wikiTrust.col.cite")))
        .child(div().w(px(36.)).flex_shrink_0().child(i18n::t(locale, "wikiTrust.col.err")))
        .child(div().w(px(36.)).flex_shrink_0().child(i18n::t(locale, "wikiTrust.col.ok")))
        .child(div().w(px(80.)).flex_shrink_0().child(i18n::t(locale, "wikiTrust.col.flags")))
}

fn trust_row(locale: Locale, r: &WikiTrustRow, is_last: bool) -> Div {
    let pct = (r.trust.clamp(0.0, 1.0) * 100.0) as f64;
    let bar_color = trust_bar_color(r.trust);
    let bar = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(52.))
                .h(px(5.))
                .rounded(px(5.))
                .bg(theme::alpha(theme::MUTED, 0.5))
                .overflow_hidden()
                .child(div().h_full().w(gpui::relative((pct / 100.0) as f32)).bg(theme::alpha(bar_color, 1.0))),
        )
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(format!("{:.3}", r.trust)),
        );

    let mut flags = div().flex().items_center().gap_1();
    if r.do_not_inject {
        flags = flags.child(badge(i18n::t(locale, "wikiTrust.flag.archived"), BadgeVariant::Destructive));
    }
    if r.locked {
        flags = flags.child(badge(i18n::t(locale, "wikiTrust.flag.locked"), BadgeVariant::Success));
    }

    let mut row = div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .text_size(px(theme::TEXT_SM))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_family("SF Mono")
                .text_size(px(12.))
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(SharedString::from(r.page_path.clone())),
        )
        .child(div().w(px(100.)).flex_shrink_0().child(bar))
        .child(
            div()
                .w(px(50.))
                .flex_shrink_0()
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(r.citation_count.to_string()),
        )
        .child(
            div()
                .w(px(36.))
                .flex_shrink_0()
                .text_color(if r.error_signal_count > 0 { theme::alpha(theme::DESTRUCTIVE, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 0.5) })
                .child(r.error_signal_count.to_string()),
        )
        .child(
            div()
                .w(px(36.))
                .flex_shrink_0()
                .text_color(if r.success_signal_count > 0 { theme::alpha(theme::SUCCESS, 1.0) } else { theme::alpha(theme::MUTED_FOREGROUND, 0.5) })
                .child(r.success_signal_count.to_string()),
        )
        .child(div().w(px(80.)).flex_shrink_0().child(flags));
    if !is_last {
        row = row.border_b_1().border_color(theme::border());
    }
    row
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_agents(state, cx);
    maybe_fetch_trust(state, cx);

    let locale = state.locale;
    let st = cx.global::<WikiTrustState>();
    let agents = st.agents.clone();
    let selected_agent = st.selected_agent.clone();
    let max_trust = st.max_trust;
    let rows = st.rows.clone();
    let available = st.available;
    let note = st.note.clone();

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, "wikiTrust.title")),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, "nav.wikiTrust.desc")),
                ),
        )
        .child(agent_picker_chip(locale, &agents, &selected_agent, cx));

    let mut threshold_row = div().flex().items_center().gap_1p5();
    for t in THRESHOLDS {
        threshold_row = threshold_row.child(threshold_chip(locale, t, (t - max_trust).abs() < 0.001, cx));
    }

    let body: Div = match &rows {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(760.), px(44.))).child(skeleton(px(760.), px(44.))),
        Loadable::Failed(err) => div().child(error_row(locale, err)),
        Loadable::Ready(list) if list.is_empty() => div().child(empty_state("🛡️", i18n::t(locale, "wikiTrust.empty"), None, None::<Div>)),
        Loadable::Ready(list) => {
            let n = list.len();
            let mut group = div()
                .w_full()
                .flex()
                .flex_col()
                .rounded(px(theme::RADIUS_XL))
                .overflow_hidden()
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .child(table_header(locale));
            for (i, r) in list.iter().enumerate() {
                group = group.child(trust_row(locale, r, i + 1 == n));
            }
            group
        }
    };

    let summary = match &rows {
        Loadable::Ready(list) => summarize(list),
        _ => TrustSummary { archived: 0, locked: 0, total_err: 0, total_ok: 0 },
    };
    let tiles = div()
        .flex()
        .gap_2p5()
        .child(stat_tile(i18n::t(locale, "wikiTrust.archived"), summary.archived.to_string(), theme::DESTRUCTIVE))
        .child(stat_tile(i18n::t(locale, "wikiTrust.locked"), summary.locked.to_string(), theme::SUCCESS))
        .child(stat_tile(i18n::t(locale, "wikiTrust.signals"), format!("{} / {}", summary.total_err, summary.total_ok), theme::FOREGROUND));

    let mut content = div()
        .max_w(px(880.))
        .mx_auto()
        .flex()
        .flex_col()
        .gap_3p5()
        .p_2()
        .child(breadcrumb("wikitrust-breadcrumb", locale, cx))
        .child(shell_tabs(locale, "wikiTrust", cx))
        .child(header)
        .child(threshold_row)
        .child(tiles);

    if !available {
        if let Some(n) = &note {
            content = content.child(
                div()
                    .rounded(px(theme::RADIUS_LG))
                    .bg(theme::alpha(theme::WARNING, 0.10))
                    .border_1()
                    .border_color(theme::alpha(theme::WARNING, 0.3))
                    .px_3p5()
                    .py_2()
                    .text_size(px(11.5))
                    .text_color(theme::alpha(theme::WARNING, 1.0))
                    .child(SharedString::from(n.clone())),
            );
        }
    }

    content = content.child(body);

    div().id("wikitrust-page").size_full().overflow_y_scroll().child(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wiki_trust_audit_reads_real_handler_shape() {
        let v = json!({
            "rows": [
                { "page_path": "faq/退換貨政策.md", "agent_id": "cs-bot", "trust": 0.91, "citation_count": 34,
                  "error_signal_count": 0, "success_signal_count": 41, "do_not_inject": false, "locked": false },
                { "page_path": "draft/未驗證促銷方案.md", "agent_id": "cs-bot", "trust": 0.08, "citation_count": 2,
                  "error_signal_count": 6, "success_signal_count": 1, "do_not_inject": true, "locked": false },
            ],
            "available": true,
        });
        let (rows, available, note) = parse_wiki_trust_audit(&v);
        assert_eq!(rows.len(), 2);
        assert!((rows[0].trust - 0.91).abs() < 1e-6);
        assert!(!rows[0].do_not_inject);
        assert!(rows[1].do_not_inject);
        assert!(available);
        assert!(note.is_none());
    }

    #[test]
    fn parse_wiki_trust_audit_unavailable_carries_note() {
        let v = json!({ "rows": [], "available": false, "note": "Trust store not initialized" });
        let (rows, available, note) = parse_wiki_trust_audit(&v);
        assert!(rows.is_empty());
        assert!(!available);
        assert_eq!(note.as_deref(), Some("Trust store not initialized"));
    }

    #[test]
    fn parse_wiki_trust_audit_missing_fields_is_empty_not_panicking() {
        assert!(parse_wiki_trust_audit(&json!({})).0.is_empty());
        assert!(parse_wiki_trust_audit(&json!(null)).0.is_empty());
    }

    #[test]
    fn summarize_counts_archived_locked_and_signal_totals() {
        let rows = vec![
            WikiTrustRow { page_path: "a".into(), trust: 0.9, citation_count: 1, error_signal_count: 0, success_signal_count: 5, do_not_inject: false, locked: true },
            WikiTrustRow { page_path: "b".into(), trust: 0.1, citation_count: 1, error_signal_count: 3, success_signal_count: 0, do_not_inject: true, locked: false },
        ];
        let s = summarize(&rows);
        assert_eq!(s.archived, 1);
        assert_eq!(s.locked, 1);
        assert_eq!(s.total_err, 3);
        assert_eq!(s.total_ok, 5);
    }

    #[test]
    fn trust_bar_color_thresholds() {
        assert_eq!(trust_bar_color(0.91), theme::SUCCESS);
        assert_eq!(trust_bar_color(0.34), theme::WARNING);
        assert_eq!(trust_bar_color(0.08), theme::DESTRUCTIVE);
    }

    #[test]
    fn agent_display_label_prefers_department_dot_display_name() {
        let agents = vec![AgentListItem {
            id: "cs-bot".into(),
            display_name: "Alice".into(),
            role: "worker".into(),
            department: "客服".into(),
            status: "active".into(),
            icon: None,
        }];
        assert_eq!(agent_display_label(&agents, "cs-bot").to_string(), "客服 · Alice");
        assert_eq!(agent_display_label(&agents, "missing").to_string(), "missing");
    }
}
