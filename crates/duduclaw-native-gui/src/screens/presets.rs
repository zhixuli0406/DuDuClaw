// WP-S5b2-E (2026-08-21) — "職務組合" (`nav.rs` id `presets`, not yet wired
// into `nav.rs`/`shell.rs` by the parallel D package as of this pass — see
// this file's own module-wiring note in `screens/mod.rs` and the
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=presets` fallback `shell.rs` gets here,
// same "add my own branch, D reconciles later" precedent `channels.rs`'s
// module doc comment set in the first S5b wave).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Presets.dc.
// html` — a read-only page: a 3-column catalog card grid ("可用職務組合")
// above a 4-column AI-staff binding table ("AI 員工綁定狀態"). Mirrors
// `web/src/pages/PresetsPage.tsx` (WP-7I, P1 scope: "reference + layer +
// leave a trail", explicitly no switching UI — binding stays a CLI-only
// operation, `duduclaw preset bind`). This page issues no write of any kind.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ───────────────────────────────────────────
//   `presets.list {}` (manager-scoped, `handle_presets_list` ~L7355) →
//   `{"presets": [{"id","version","label","description"} | {"id","error"}]}`
//   — `version`/`label`/`description` are `PresetMeta`'s plain (possibly
//   empty) `String` fields (`crates/duduclaw-core/src/preset.rs:294-299`),
//   never `null`; an entry that failed to parse carries `error` instead and
//   omits the other three.
//   `presets.status {"agent_id"}` (viewer-scoped, per-agent, `handle_
//   presets_status` ~L7376) → `{"agent_id","resolution":{"state":"unbound"}
//   | {"state":"applied","preset_id","version","label","changed_fields"} |
//   {"state":"unresolved","preset_id","version","reason"}}`.
//   `agents.list {}` — reused via `agents_data::parse_agents_list` (already
//   a `pub fn` on a private-but-crate-visible sibling module, see that
//   file's own header comment) for the binding table's AI-staff roster,
//   exactly how `PresetsPage.tsx` uses `useAgentsStore`.
//
// ── Fetch fan-out (per-agent `presets.status`, mirrors `PresetsPage.tsx`'s
// own `Promise.all(agents.map(...))`) ────────────────────────────────────
// `agents.list` and `presets.list` fire independently and in parallel (each
// its own `Loadable`, same "every data source fetches independently" rule
// this WP's brief names). Once `agents` resolves `Ready`, ONE `presets.
// status` call fires per agent id not already in `status_requested_for` —
// a single agent's failed lookup only fails that agent's own table cell
// (`Loadable::Failed` in the per-agent map), never the whole table, mirroring
// `PresetsPage.tsx`'s own `.catch(() => [a.name, null])` per-agent isolation.
//
// ── Deviations from the canvas (documented, not silent) ──────────────────
// 1. The canvas's header actions row is an empty `<div>` (no buttons) — P1
//    genuinely has no write action, so this page renders no header actions
//    at all rather than inventing a placeholder button with nothing to do.
// 2. Breadcrumb root — see `catalog_common::breadcrumb`'s own doc comment:
//    static text, not a clickable "AI 員工" hub (no such single page id
//    exists to jump to).

use std::collections::HashMap;

use gpui::{div, prelude::*, px, Context, Div, Global, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::screens::agents_data::{parse_agents_list, AgentListItem};
use crate::screens::catalog_common as cc;
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Data model (pure — unit tested without a live `App`) ─────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetSummary {
    pub id: String,
    pub version: Option<String>,
    pub label: Option<String>,
    pub description: Option<String>,
    pub error: Option<String>,
}

/// Empty-string wire fields collapse to `None` — `PresetMeta`'s `version`/
/// `label`/`description` are plain (never-null) `String`s, so an unset
/// field arrives as `""`, not absent; treating `""` as "no value" matches
/// `PresetsPage.tsx`'s own `preset.version &&`/`preset.label &&` guards.
fn non_empty(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn parse_presets(v: &Value) -> Vec<PresetSummary> {
    v.get("presets")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let id = p.get("id").and_then(Value::as_str)?.to_string();
                    if id.is_empty() {
                        return None;
                    }
                    if let Some(error) = non_empty(p, "error") {
                        return Some(PresetSummary { id, version: None, label: None, description: None, error: Some(error) });
                    }
                    Some(PresetSummary {
                        id,
                        version: non_empty(p, "version"),
                        label: non_empty(p, "label"),
                        description: non_empty(p, "description"),
                        error: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetResolutionState {
    Unbound,
    Applied { preset_id: String, version: Option<String>, label: Option<String>, changed_fields: Vec<String> },
    Unresolved { preset_id: Option<String>, version: Option<String>, reason: Option<String> },
}

/// Parses the `presets.status` response's `resolution` object. Any
/// unrecognized/missing `state` (including the real `"unbound"` value)
/// degrades to `Unbound` — the safest, most neutral rendering, never a
/// panic on a wire shape this page doesn't recognize yet.
pub fn parse_resolution(v: &Value) -> PresetResolutionState {
    let res = v.get("resolution").cloned().unwrap_or(Value::Null);
    match res.get("state").and_then(Value::as_str) {
        Some("applied") => PresetResolutionState::Applied {
            preset_id: res.get("preset_id").and_then(Value::as_str).unwrap_or("").to_string(),
            version: non_empty(&res, "version"),
            label: non_empty(&res, "label"),
            changed_fields: res
                .get("changed_fields")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        },
        Some("unresolved") => PresetResolutionState::Unresolved {
            preset_id: non_empty(&res, "preset_id"),
            version: non_empty(&res, "version"),
            reason: non_empty(&res, "reason"),
        },
        _ => PresetResolutionState::Unbound,
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct PresetsState {
    presets_requested: bool,
    pub presets: Loadable<Vec<PresetSummary>>,
    agents_requested: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    status_requested_for: std::collections::HashSet<String>,
    pub statuses: HashMap<String, Loadable<PresetResolutionState>>,
}

impl Default for PresetsState {
    fn default() -> Self {
        Self {
            presets_requested: false,
            presets: Loadable::Loading,
            agents_requested: false,
            agents: Loadable::Loading,
            status_requested_for: std::collections::HashSet::new(),
            statuses: HashMap::new(),
        }
    }
}

impl Global for PresetsState {}

// ── Fetch orchestration ───────────────────────────────────────────────────

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if !cx.default_global::<PresetsState>().presets_requested {
        cx.default_global::<PresetsState>().presets_requested = true;
        let tx = state.session_tx.clone();
        cc::spawn_call(cx, tx, "presets.list", json!({}), |cx, result| {
            cx.default_global::<PresetsState>().presets = result.map(|v| parse_presets(&v)).into();
        });
    }
    if !cx.default_global::<PresetsState>().agents_requested {
        cx.default_global::<PresetsState>().agents_requested = true;
        let tx = state.session_tx.clone();
        cc::spawn_call(cx, tx, "agents.list", json!({}), |cx, result| {
            cx.default_global::<PresetsState>().agents = result.map(|v| parse_agents_list(&v)).into();
        });
    }
}

/// Fires `presets.status {"agent_id"}` once per agent id, the moment
/// `agents` resolves `Ready` — see module doc comment's "Fetch fan-out".
fn maybe_fetch_statuses(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let Loadable::Ready(agents) = cx.default_global::<PresetsState>().agents.clone() else { return };
    for agent in agents {
        if cx.default_global::<PresetsState>().status_requested_for.contains(&agent.id) {
            continue;
        }
        cx.default_global::<PresetsState>().status_requested_for.insert(agent.id.clone());
        cx.default_global::<PresetsState>().statuses.insert(agent.id.clone(), Loadable::Loading);
        let tx = state.session_tx.clone();
        let agent_id = agent.id.clone();
        cc::spawn_call(cx, tx, "presets.status", json!({"agent_id": agent_id}), move |cx, result| {
            let resolved: Loadable<PresetResolutionState> = result.map(|v| parse_resolution(&v)).into();
            cx.default_global::<PresetsState>().statuses.insert(agent_id, resolved);
        });
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    maybe_fetch_statuses(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("presets-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                "🔌",
                i18n::t(locale, "native.home.connError.title"),
                Some(i18n::t(locale, "native.home.connError.desc")),
                None::<Div>,
            ));
    }

    let g = cx.default_global::<PresetsState>();
    let presets = g.presets.clone();
    let agents = g.agents.clone();
    let statuses = g.statuses.clone();

    div()
        .id("presets-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .child(cc::breadcrumb(i18n::t(locale, "navArea.agents"), i18n::t(locale, "native.presets.title")))
        .child(cc::page_header(
            i18n::t(locale, "native.presets.title"),
            i18n::t(locale, "native.presets.subtitle"),
            None,
        ))
        .child(catalog_section(locale, cx, &presets))
        .child(bindings_section(locale, &agents, &statuses))
}

fn catalog_section(locale: Locale, cx: &mut Context<RootView>, presets: &Loadable<Vec<PresetSummary>>) -> Div {
    let body: Div = match presets {
        Loadable::Loading => grid_3col((0..3).map(|_| card_skeleton()).collect()),
        Loadable::Failed(msg) => div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(empty_state("⚠️", i18n::t1(locale, "native.presets.loadError", "message", msg), None, None::<Div>))
            .child(crate::mds_gpui::button(
                "presets-retry",
                i18n::t(locale, "native.presets.retry"),
                crate::mds_gpui::ButtonVariant::Secondary,
                false,
                None,
                cx.listener(|_this, _ev, _window, cx| {
                    cx.default_global::<PresetsState>().presets_requested = false;
                    cx.default_global::<PresetsState>().presets = Loadable::Loading;
                    cx.notify();
                }),
            )),
        Loadable::Ready(rows) if rows.is_empty() => {
            div().child(empty_state("📦", i18n::t(locale, "native.presets.empty.catalog"), Some(i18n::t(locale, "native.presets.empty.catalog.hint")), None::<Div>))
        }
        Loadable::Ready(rows) => grid_3col(rows.iter().map(|p| preset_card(locale, p)).collect()),
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(cc::category_group_header(i18n::t(locale, "native.presets.section.catalog")))
        .child(body)
}

fn grid_3col(cards: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap_3().children(cards.into_iter().map(|c| c.flex_1().min_w(px(240.))))
}

fn card_skeleton() -> Div {
    cc::catalog_card().child(skeleton(px(120.), px(14.))).child(skeleton(px(180.), px(12.))).child(skeleton(px(90.), px(10.)))
}

fn preset_card(locale: Locale, p: &PresetSummary) -> Div {
    let header = div()
        .flex()
        .items_start()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .font_family("SF Mono")
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(p.id.clone()),
        )
        .children(p.version.clone().map(|v| badge(i18n::t1(locale, "native.presets.catalog.version", "version", &v), BadgeVariant::Secondary)));

    let body = if let Some(err) = &p.error {
        div()
            .flex()
            .items_start()
            .gap_1p5()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::DESTRUCTIVE, 1.0))
            .child("⚠")
            .child(err.clone())
    } else {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(p.label.clone().map(|l| div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(l)))
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(p.description.clone().unwrap_or_else(|| i18n::t(locale, "native.presets.catalog.noDescription").to_string())),
            )
    };

    cc::catalog_card().child(header).child(body)
}

fn bindings_section(locale: Locale, agents: &Loadable<Vec<AgentListItem>>, statuses: &HashMap<String, Loadable<PresetResolutionState>>) -> Div {
    let body: Div = match agents {
        Loadable::Loading => div().flex().flex_col().gap_2().children((0..3).map(|_| binding_row_skeleton()).collect::<Vec<_>>()),
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "native.presets.loadError", "message", msg), None, None::<Div>),
        Loadable::Ready(rows) if rows.is_empty() => empty_state("🐾", i18n::t(locale, "native.presets.empty.bindings"), None, None::<Div>),
        Loadable::Ready(rows) => {
            let mut table = div()
                .w_full()
                .rounded(px(theme::RADIUS_XL))
                .overflow_hidden()
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .child(binding_header_row(locale));
            for (i, agent) in rows.iter().enumerate() {
                let status = statuses.get(&agent.id).cloned().unwrap_or(Loadable::Loading);
                table = table.child(binding_row(locale, agent, &status, i == rows.len() - 1));
            }
            table
        }
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(cc::category_group_header(i18n::t(locale, "native.presets.section.bindings")))
        .child(body)
}

// gpui has no CSS grid (see this file's tail comment) and this crate's own
// `.flex_grow(f32)` usage has no precedent elsewhere to mirror, so — like
// `channels.rs::channel_row` — proportional columns use `.flex_1()` for the
// two columns that should share remaining width and a fixed `.w(px(...))`
// for the two that shouldn't, rather than the canvas's literal `1.4fr 1fr
// 1.6fr 1.6fr` grid-template-columns ratio.
const COL_STATUS_W: f32 = 88.;
const COL_OVERRIDDEN_W: f32 = 170.;

fn binding_header_row(locale: Locale) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .px_4()
        .py_2()
        .bg(theme::alpha(theme::MUTED, 0.4))
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(div().flex_1().min_w_0().child(i18n::t(locale, "native.presets.col.agent")))
        .child(div().w(px(COL_STATUS_W)).flex_shrink_0().child(i18n::t(locale, "native.presets.col.status")))
        .child(div().flex_1().min_w_0().child(i18n::t(locale, "native.presets.col.preset")))
        .child(div().w(px(COL_OVERRIDDEN_W)).flex_shrink_0().child(i18n::t(locale, "native.presets.col.overridden")))
}

fn binding_row_skeleton() -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .child(skeleton(px(24.), px(24.)).rounded_full())
        .child(skeleton(px(120.), px(12.)))
}

fn binding_row(locale: Locale, agent: &AgentListItem, status: &Loadable<PresetResolutionState>, is_last: bool) -> Div {
    let name = if agent.display_name.is_empty() { agent.id.clone() } else { agent.display_name.clone() };

    let status_cell: Div = match status {
        Loadable::Loading => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("…"),
        Loadable::Failed(_) => div().child(badge(i18n::t(locale, "native.presets.badge.error"), BadgeVariant::Destructive)),
        Loadable::Ready(PresetResolutionState::Applied { .. }) => div().child(badge(i18n::t(locale, "native.presets.badge.applied"), BadgeVariant::Success)),
        Loadable::Ready(PresetResolutionState::Unresolved { .. }) => div().child(badge(i18n::t(locale, "native.presets.badge.unresolved"), BadgeVariant::Destructive)),
        Loadable::Ready(PresetResolutionState::Unbound) => div().child(badge(i18n::t(locale, "native.presets.badge.unbound"), BadgeVariant::Outline)),
    };

    let preset_cell: Div = match status {
        Loadable::Ready(PresetResolutionState::Applied { preset_id, version, label, .. }) => {
            let mono = match version {
                Some(v) => format!("{preset_id}@{v}"),
                None => preset_id.clone(),
            };
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_XS)).font_family("SF Mono").text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(mono))
                .children(label.clone().map(|l| div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(l)))
        }
        Loadable::Ready(PresetResolutionState::Unresolved { preset_id, reason, .. }) => div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .font_family("SF Mono")
                    .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                    .child(preset_id.clone().unwrap_or_else(|| "—".to_string())),
            )
            .children(reason.clone().map(|r| div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(i18n::t1(locale, "native.presets.reason", "reason", &r)))),
        _ => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("—"),
    };

    let overridden_cell: Div = match status {
        Loadable::Ready(PresetResolutionState::Applied { changed_fields, .. }) if !changed_fields.is_empty() => {
            let mut row = div().flex().flex_wrap().gap_1();
            for f in changed_fields {
                row = row.child(badge(f.clone(), BadgeVariant::Secondary));
            }
            row
        }
        _ => div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "native.presets.overridden.none")),
    };

    let row = div()
        .flex()
        .items_center()
        .gap_2p5()
        .px_4()
        .py_2p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(cc::initial_avatar(&name, px(22.)))
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).truncate().child(name)),
        )
        .child(div().w(px(COL_STATUS_W)).flex_shrink_0().child(status_cell))
        .child(div().flex_1().min_w_0().child(preset_cell))
        .child(div().w(px(COL_OVERRIDDEN_W)).flex_shrink_0().child(overridden_cell));

    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_presets_reads_real_handler_shape() {
        let v = json!({ "presets": [
            { "id": "customer-service", "version": "2", "label": "客服基礎組合", "description": "標準客服語氣" },
            { "id": "retail-frontdesk", "error": "缺少必要欄位 department" },
        ]});
        let rows = parse_presets(&v);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "customer-service");
        assert_eq!(rows[0].version.as_deref(), Some("2"));
        assert!(rows[0].error.is_none());
        assert_eq!(rows[1].id, "retail-frontdesk");
        assert_eq!(rows[1].error.as_deref(), Some("缺少必要欄位 department"));
        assert!(rows[1].label.is_none());
    }

    #[test]
    fn parse_presets_treats_empty_string_fields_as_absent() {
        let v = json!({ "presets": [{ "id": "onboarding-basic", "version": "", "label": "", "description": "" }] });
        let rows = parse_presets(&v);
        assert_eq!(rows[0].version, None);
        assert_eq!(rows[0].label, None);
        assert_eq!(rows[0].description, None);
    }

    #[test]
    fn parse_presets_missing_array_is_empty_not_a_panic() {
        assert_eq!(parse_presets(&json!({})).len(), 0);
        assert_eq!(parse_presets(&json!(null)).len(), 0);
    }

    #[test]
    fn parse_presets_skips_entries_missing_id() {
        let v = json!({ "presets": [{ "version": "1" }] });
        assert_eq!(parse_presets(&v).len(), 0);
    }

    #[test]
    fn parse_resolution_reads_applied_state() {
        let v = json!({ "agent_id": "sam", "resolution": {
            "state": "applied", "preset_id": "customer-service", "version": "2",
            "label": "客服基礎組合", "changed_fields": ["model", "temperature"],
        }});
        match parse_resolution(&v) {
            PresetResolutionState::Applied { preset_id, version, label, changed_fields } => {
                assert_eq!(preset_id, "customer-service");
                assert_eq!(version.as_deref(), Some("2"));
                assert_eq!(label.as_deref(), Some("客服基礎組合"));
                assert_eq!(changed_fields, vec!["model".to_string(), "temperature".to_string()]);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn parse_resolution_reads_unresolved_state() {
        let v = json!({ "resolution": { "state": "unresolved", "preset_id": "retail-frontdesk", "reason": "缺少 department" }});
        match parse_resolution(&v) {
            PresetResolutionState::Unresolved { preset_id, reason, .. } => {
                assert_eq!(preset_id.as_deref(), Some("retail-frontdesk"));
                assert_eq!(reason.as_deref(), Some("缺少 department"));
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn parse_resolution_unbound_and_unknown_states_degrade_to_unbound() {
        assert_eq!(parse_resolution(&json!({ "resolution": { "state": "unbound" }})), PresetResolutionState::Unbound);
        assert_eq!(parse_resolution(&json!({ "resolution": { "state": "some_future_state" }})), PresetResolutionState::Unbound);
        assert_eq!(parse_resolution(&json!({})), PresetResolutionState::Unbound);
    }
}
