// WP-S6b2-O (S6b 第二波, 2026-08-21) — "新增 Widget" (`commercial/design/
// duduclaw-s6-form-pages/WidgetComposer.dc.html`, B21 複合編輯器桶). A
// creation-only leaf of `screens::widgets` ("Widget 工坊"): the canvas's own
// title is literally "新增 Widget" (no edit-existing-widget mode drawn), so
// this page mirrors that scope exactly — no `?id=` edit path like
// `web/src/pages/WidgetComposerPage.tsx`'s `/widgets/:id/edit` route.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `widgets.custom.create {title, description, html, origin}` (dispatch
//   L6369, handler L26153, any authenticated user — `origin="html"` is
//   admin-gated, `origin="ai"` is not) and `widgets.custom.update {id,
//   title?, description?, html?}` (dispatch L6370, handler L26185) are the
//   two write paths `WidgetComposerPage.tsx`'s own `save()` calls — NEITHER
//   is fired here (儲存 renders `disabled: true`, this pass's own "決策類組
//   裝不真按" instruction, same convention `screens::widgets`'s own 新增/
//   匯入/新增HTML header buttons already establish). `widgets.custom.
//   generate {prompt, style, data_sources, prior_html?, feedback?}`
//   (dispatch L6373, handler L26270) is the guided-flow LLM call the 產生/
//   依回饋重新生成 buttons would fire — also not wired, same reasoning (a
//   real LLM call is exactly the kind of side-effecting decision-class
//   action this crate's "assembled, not wired" convention exists for).
//
//   The one RPC this page DOES call is `cost.agents {hours: 720}`
//   (dispatch L6312, handler L24399, **admin-gated** — `require_admin!()`,
//   same 720h window `screens::reports.rs`'s own 快取效率表 section already
//   uses) → `{available, agents: [{agent_id, avg_cache_efficiency,
//   total_requests, total_cost_millicents}]}`. This feeds the right-column
//   live preview: the canvas's own example widget ("各員工本週用量花費",
//   資料來源=用量花費 + 呈現風格=長條圖, both pre-selected in the mockup) is
//   drawn from REAL per-agent cost data instead of the canvas's illustrative
//   numbers — top 4 by `total_cost_millicents` descending, an honest
//   `NT$` amount matching `reports.rs`'s own `total_cost_millicents as f64 /
//   100_000.0` conversion. A non-admin caller sees the real `require_admin!`
//   rejection surface through the normal `Loadable::Failed` error card, not
//   a silently-empty panel.
//
// ── Deviations from the canvas (documented, not silent) ──────────────────
// 1. The preview is a FIXED illustrative real-data-shaped bar chart, not a
//    live re-render of whatever 資料來源/呈現風格 chips happen to be
//    selected — there is no `widgets.custom.generate` call wired this pass
//    (see above), so nothing could actually change what the right panel
//    shows. The chips themselves ARE genuinely interactive local UI state
//    (per this task's "設定欄位可真互動" instruction) — clicking toggles the
//    highlight exactly like the real composer would track it, it just has
//    nowhere to submit to yet.
// 2. No red "超過預算 80%" highlighting on any bar. `cost.agents` carries no
//    per-agent budget field at all (grep-verified) — inventing an over-
//    budget threshold would be exactly the kind of fabricated-signal the
//    project's memory/data conventions forbid (`screens::identity.rs`'s own
//    "no invented 18 位 count" precedent, `screens::world.rs`'s "no
//    fabricated speech-bubble text"). Every bar renders the same brand
//    color; only the width differs.
// 3. The canvas draws real-time `Bounds`-painted bars via `gpui::canvas` +
//    `paint_quad`, growing HORIZONTALLY (matching the canvas's own
//    progress-bar visual language) rather than the vertical bars
//    `screens::reports.rs::cost_bar_chart` draws — same underlying "spike
//    配方" (`gpui::canvas` + `window.paint_quad`, no `PathBuilder` needed
//    for solid rectangles), just re-oriented per-row.
// 4. No decorative header icon square (canvas draws a small rounded-square
//    icon left of the title) — this crate has no icon set yet beyond emoji/
//    letter glyphs (`mds_gpui::empty_state`'s own module doc comment already
//    documents this gap), so it is skipped rather than faked with a random
//    glyph.
// 5. "微調" (revise-with-feedback) renders as a fully static/disabled card —
//    same reasoning as #1: revising would itself fire `widgets.custom.
//    generate` again, so there is nothing for a live feedback field to
//    submit to. Its placeholder text is drawn straight from the canvas's
//    own example copy.
// 6. Title/description/freeform fields ARE real, live `TextField` entities
//    (typing has zero side effects — nothing downstream reads them yet) —
//    matching the precedent `screens::identity.rs`'s `resolve_input` field
//    sets for "an input with no write consequence is safe to make live".
//    They start EMPTY with the canvas's own example copy as placeholder
//    text, not pre-filled — a fresh composer has nothing typed into it yet,
//    matching `WidgetComposerPage.tsx`'s own `useState('')` initial state.
//    The freeform field is single-line (`text_field.rs`'s own documented
//    scope: no multiline support yet), unlike the canvas's taller textarea.

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::screens::catalog_common as cc;
use crate::screens::dashboard::Loadable;
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

/// Mirrors `WidgetComposerPage.tsx`'s own `DATA_SOURCES` array exactly (same
/// order), each id resolving to a `widgetComposer.source.<id>` i18n key.
const DATA_SOURCES: [&str; 5] = ["agents", "tasks", "cost", "channels", "system"];
/// Mirrors `WidgetComposerPage.tsx`'s own `STYLES` array exactly.
const STYLES: [&str; 4] = ["stat", "list", "bars", "free"];

#[derive(Clone)]
struct PreviewCostRow {
    agent_id: String,
    cost_millicents: i64,
}

/// Same `available` gate + field names `screens::reports.rs::parse_cost_agents`
/// reads — a local copy (not a cross-module import) per this crate's
/// established "each page keeps its own ~15-line RPC parse" precedent
/// (`screens/reports.rs`'s own module doc comment calls this out for
/// `spawn_call`, the same reasoning applies to a page-specific slice of a
/// shared RPC's response). Sorted by cost descending, capped to the
/// canvas's own 4-row example count.
fn parse_preview_cost_agents(v: &Value) -> Vec<PreviewCostRow> {
    if !v.get("available").and_then(Value::as_bool).unwrap_or(false) {
        return Vec::new();
    }
    let mut rows: Vec<PreviewCostRow> = v
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|a| PreviewCostRow {
            agent_id: a.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string(),
            cost_millicents: a.get("total_cost_millicents").and_then(Value::as_i64).unwrap_or(0),
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.cost_millicents));
    rows.truncate(4);
    rows
}

// ── State ──────────────────────────────────────────────────────────────

pub struct WidgetComposerState {
    title: Entity<TextField>,
    description: Entity<TextField>,
    freeform: Entity<TextField>,
    sources: Vec<&'static str>,
    style: &'static str,
    cost_requested: bool,
    cost_agents: Loadable<Vec<PreviewCostRow>>,
}

impl WidgetComposerState {
    /// `locale` is the session's locale AT THE MOMENT this page is first
    /// opened (same "baked into the placeholder at construction time, not
    /// re-read on locale change" limitation `main.rs`'s own
    /// `ime_input::ImeTextInput::new(cx, i18n::t(initial_locale, …))` call
    /// already accepts for the chat composer — `TextField`/`ImeTextInput`
    /// neither expose a placeholder setter, so this is the same pre-existing
    /// gap, not a new one).
    fn new(cx: &mut gpui::App, locale: Locale) -> Self {
        Self {
            title: TextField::new(cx, i18n::t(locale, "widgetComposer.titlePlaceholder"), false, ""),
            description: TextField::new(cx, i18n::t(locale, "widgetComposer.descPlaceholder"), false, ""),
            freeform: TextField::new(cx, i18n::t(locale, "widgetComposer.freeformPlaceholder"), false, ""),
            // Pre-selected to match the canvas's own highlighted chips
            // (資料來源=用量花費 / 呈現風格=長條圖) — the exact pair the real
            // preview panel below renders data for.
            sources: vec!["cost"],
            style: "bars",
            cost_requested: false,
            cost_agents: Loadable::Loading,
        }
    }
}

impl Global for WidgetComposerState {}

fn ensure_state(locale: Locale, cx: &mut Context<RootView>) {
    if !cx.has_global::<WidgetComposerState>() {
        let state = WidgetComposerState::new(cx, locale);
        cx.set_global(state);
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<WidgetComposerState>().cost_requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<WidgetComposerState>().cost_requested = true;
    let tx = state.session_tx.clone();
    cc::spawn_call(cx, tx, "cost.agents", json!({"hours": 720}), |cx, result| {
        cx.global_mut::<WidgetComposerState>().cost_agents = result.map(|v| parse_preview_cost_agents(&v)).into();
    });
}

// ── Settings-card shell ────────────────────────────────────────────────

fn settings_card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_3p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
}

fn section_title(locale: Locale, key: &str) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
        .child(i18n::t(locale, key))
}

// ── Chips (real toggle state — deviation #1) ──────────────────────────

fn chip_base(id: SharedString, label: SharedString, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(26.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| {
            el.bg(theme::alpha(theme::BRAND, 0.12)).text_color(theme::alpha(theme::BRAND, 1.0)).border_1().border_color(theme::alpha(theme::BRAND, 0.4))
        })
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        })
        .child(label)
}

fn source_chip(locale: Locale, id: &'static str, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let label = i18n::t(locale, &format!("widgetComposer.source.{id}"));
    chip_base(format!("wc-source-{id}").into(), label, selected).on_click(cx.listener(move |_this, _ev, _window, cx| {
        let g = cx.global_mut::<WidgetComposerState>();
        if let Some(pos) = g.sources.iter().position(|s| *s == id) {
            g.sources.remove(pos);
        } else {
            g.sources.push(id);
        }
        cx.notify();
    }))
}

fn style_chip(locale: Locale, id: &'static str, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let label = i18n::t(locale, &format!("widgetComposer.style.{id}"));
    chip_base(format!("wc-style-{id}").into(), label, selected).on_click(cx.listener(move |_this, _ev, _window, cx| {
        cx.global_mut::<WidgetComposerState>().style = id;
        cx.notify();
    }))
}

// ── Live preview: real `cost.agents` data, horizontal canvas bars ────────
// (deviation #3 — same `gpui::canvas` + `window.paint_quad` recipe
// `screens/reports.rs::cost_bar_chart` proved, re-oriented horizontally.)

const PREVIEW_BAR_WIDTH: f32 = 380.0;
const PREVIEW_BAR_HEIGHT: f32 = 8.0;

fn preview_bar(cost_millicents: i64, max_cost: i64) -> Div {
    let pct = if max_cost > 0 { (cost_millicents as f32 / max_cost as f32).clamp(0.04, 1.0) } else { 0.04 };
    div().w(px(PREVIEW_BAR_WIDTH)).h(px(PREVIEW_BAR_HEIGHT)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                window.paint_quad(gpui::quad(
                    bounds,
                    px(PREVIEW_BAR_HEIGHT / 2.0),
                    theme::alpha(theme::MUTED, 0.6),
                    px(0.),
                    gpui::transparent_black(),
                    gpui::BorderStyle::default(),
                ));
                let fill_bounds = gpui::Bounds::new(bounds.origin, gpui::size(px(PREVIEW_BAR_WIDTH * pct), px(PREVIEW_BAR_HEIGHT)));
                window.paint_quad(gpui::quad(
                    fill_bounds,
                    px(PREVIEW_BAR_HEIGHT / 2.0),
                    theme::alpha(theme::CHART_1, 0.9),
                    px(0.),
                    gpui::transparent_black(),
                    gpui::BorderStyle::default(),
                ));
            },
        )
        .size_full(),
    )
}

fn preview_row(row: &PreviewCostRow, max_cost: i64) -> Div {
    let amount: SharedString = format!("NT${:.2}", row.cost_millicents as f64 / 100_000.0).into();
    let label: SharedString = if row.agent_id.is_empty() { "—".into() } else { row.agent_id.clone().into() };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).truncate().child(label))
                .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::SEMIBOLD).font_family("SF Mono").text_color(theme::alpha(theme::CHART_1, 1.0)).child(amount)),
        )
        .child(preview_bar(row.cost_millicents, max_cost))
}

fn preview_card(locale: Locale, cost_agents: &Loadable<Vec<PreviewCostRow>>) -> Div {
    let body: Div = match cost_agents {
        Loadable::Loading => div().flex().flex_col().gap_2().child(skeleton(px(PREVIEW_BAR_WIDTH), px(16.))).child(skeleton(px(PREVIEW_BAR_WIDTH), px(16.))).child(skeleton(px(PREVIEW_BAR_WIDTH), px(16.))),
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "widgetComposer.previewError", "message", msg), None, None::<Div>),
        Loadable::Ready(rows) if rows.is_empty() => empty_state("📊", i18n::t(locale, "widgetComposer.previewEmpty"), None, None::<Div>),
        Loadable::Ready(rows) => {
            let max_cost = rows.iter().map(|r| r.cost_millicents).max().unwrap_or(0).max(1);
            let mut col = div().flex().flex_col().gap_3();
            for r in rows {
                col = col.child(preview_row(r, max_cost));
            }
            col
        }
    };
    let card = settings_card()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "widgetComposer.previewCardTitle")))
        .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "widgetComposer.previewCardSubtitle")))
        .child(body);
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "widgetComposer.previewBadge")))
        .child(card)
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    ensure_state(locale, cx);
    maybe_fetch(state, cx);

    if state.ws_state != WsConnState::Authenticated {
        return div().id("widget-composer-page").size_full().flex().items_center().justify_center().child(empty_state(
            "🔌",
            i18n::t(locale, "native.home.connError.title"),
            Some(i18n::t(locale, "native.home.connError.desc")),
            None::<Div>,
        ));
    }

    let title_entity = cx.global::<WidgetComposerState>().title.clone();
    let desc_entity = cx.global::<WidgetComposerState>().description.clone();
    let freeform_entity = cx.global::<WidgetComposerState>().freeform.clone();
    let sources = cx.global::<WidgetComposerState>().sources.clone();
    let style = cx.global::<WidgetComposerState>().style;
    let cost_agents = cx.global::<WidgetComposerState>().cost_agents.clone();

    let actions = div().child(button("widget-composer-save", i18n::t(locale, "widgetComposer.save"), ButtonVariant::Primary, true, None, |_ev, _window, _app| {}));

    let basic_card = settings_card()
        .child(section_title(locale, "widgetComposer.basicInfo"))
        .child(div().flex().flex_col().gap_2().child(title_entity).child(desc_entity));

    let source_row = div().flex().flex_wrap().gap_1p5().children(DATA_SOURCES.iter().map(|id| source_chip(locale, id, sources.contains(id), cx)));
    let style_row = div().flex().flex_wrap().gap_1p5().children(STYLES.iter().map(|id| style_chip(locale, id, style == *id, cx)));
    let source_style_card = settings_card()
        .child(section_title(locale, "widgetComposer.dataSourceLabel"))
        .child(source_row)
        .child(section_title(locale, "widgetComposer.styleLabel"))
        .child(style_row);

    let freeform_card = settings_card().child(section_title(locale, "widgetComposer.freeformLabel")).child(freeform_entity);

    // Deviation #5: 微調 is a static, disabled card — revising would itself
    // fire `widgets.custom.generate`, out of this pass's scope.
    let tweak_card = settings_card()
        .child(section_title(locale, "widgetComposer.tweakLabel"))
        .child(
            div()
                .w_full()
                .min_h(px(40.))
                .px_2p5()
                .py_2()
                .rounded(px(theme::RADIUS_LG))
                .border_1()
                .border_color(theme::surface_border())
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.7))
                .child(i18n::t(locale, "widgetComposer.tweakPlaceholder")),
        )
        .child(
            div().flex().justify_end().child(button(
                "widget-composer-revise",
                i18n::t(locale, "widgetComposer.revise"),
                ButtonVariant::Secondary,
                true,
                None,
                |_ev, _window, _app| {},
            )),
        );

    let left = div().flex_1().min_w(px(380.)).flex().flex_col().gap_3p5().child(basic_card).child(source_style_card).child(freeform_card).child(tweak_card);
    let right = div().flex_1().min_w(px(380.)).child(preview_card(locale, &cost_agents));

    div()
        .id("widget-composer-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_6()
        .child(cc::breadcrumb(i18n::t(locale, "native.widgets.title"), i18n::t(locale, "widgetComposer.title")))
        .child(cc::page_header(i18n::t(locale, "widgetComposer.title"), i18n::t(locale, "widgetComposer.subtitle"), Some(actions)))
        .child(div().flex().gap_4().items_start().child(left).child(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_preview_cost_agents_reads_and_sorts_by_cost_descending() {
        let v = json!({ "available": true, "agents": [
            { "agent_id": "a", "total_cost_millicents": 100 },
            { "agent_id": "b", "total_cost_millicents": 500 },
            { "agent_id": "c", "total_cost_millicents": 300 },
        ]});
        let rows = parse_preview_cost_agents(&v);
        assert_eq!(rows.iter().map(|r| r.agent_id.as_str()).collect::<Vec<_>>(), vec!["b", "c", "a"]);
    }

    #[test]
    fn parse_preview_cost_agents_unavailable_is_empty_not_a_panic() {
        assert!(parse_preview_cost_agents(&json!({ "available": false, "agents": [{"agent_id": "x", "total_cost_millicents": 1}] })).is_empty());
    }

    #[test]
    fn parse_preview_cost_agents_caps_at_four_rows() {
        let agents: Vec<Value> = (0..10).map(|i| json!({ "agent_id": format!("a{i}"), "total_cost_millicents": i })).collect();
        let v = json!({ "available": true, "agents": agents });
        assert_eq!(parse_preview_cost_agents(&v).len(), 4);
    }

    #[test]
    fn parse_preview_cost_agents_missing_array_is_empty() {
        assert!(parse_preview_cost_agents(&json!({ "available": true })).is_empty());
    }
}
