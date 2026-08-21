// WP-S6b3-P (S6b 第三波, 2026-08-22) — "本地模型市集" (`LocalModels.dc.
// html`, B2 已安裝/可下載雙態型錄卡). A "進階設定" drill-down leaf
// (`active_page == "localModels"`, no `nav.rs` entry — wired from
// `manage_advanced.rs`'s 本地模型市集 row by this same pass).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/
// handlers.rs` + `crates/duduclaw-gateway/src/local_models.rs`, never
// guessed) ─────────────────────────────────────────────────────────────
//   `localmodels.installed {}` (dispatch L6027, `crate::local_models::
//   installed` L97-112) → `{"models": [{"filename", "size_bytes"}]}` — a
//   `<home>/models/*.gguf` directory scan, sorted by filename.
//   `localmodels.search {intent}` (dispatch L6013, `crate::local_models::
//   search` L64-73) → `{"models": [MarketModel], "hardware": {...}}`.
//   `intent` defaults to `"chat"` server-side when omitted (L6014) — this
//   page never renders an intent picker, matching the canvas's single
//   catalog view (no search-intent chips drawn). `MarketModel`
//   (`crates/duduclaw-inference/src/model_registry/market.rs` L108-139):
//   `{repo, name, publisher, downloads, likes, gated, params_b?,
//   architecture?, moe, active_params_b?, context_length?,
//   has_chat_template, languages, recommended?: QuantOption, quants:
//   [QuantOption]}`; `QuantOption` L86-105: `{filename, quant, size_bytes,
//   shards, imatrix, fit, fit_offload?}`.
//
// ── Deliberate deviations from the canvas (documented, not silent) ────────
// 1. **已安裝 cards carry no description or "使用中" badge.** `installed()`
//    returns only `{filename, size_bytes}` — no quant/purpose description,
//    no "which model is currently loaded" field anywhere in this RPC family
//    (that's `inference.get()`'s `default_model`, a different section — see
//    point 3). Every installed card shows the real filename + size only,
//    same "no backing field → show what the RPC actually returns"
//    precedent `departments.rs`'s own header comment already establishes
//    this same pass.
// 2. **可下載 description is derived, not fabricated.** `MarketModel` has no
//    free-text description field either — this page builds one from real
//    sub-fields (`architecture` + `params_b`, e.g. "qwen3 · 8B 參數"),
//    same "derive from real fields, don't invent prose" pattern
//    `governance.rs::policy_detail` already establishes for its own
//    per-type summary strings.
// 3. **本地模型 tab (see `inference.rs`'s own header comment) is the one
//    place `inference.get()`'s `default_model` is cross-referenced** — NOT
//    duplicated here. This page's "使用中" badge would need exactly that
//    field; rather than adding a second RPC call to a catalog page, the
//    honest choice is simply not to claim a "currently active" fact this
//    page's own RPC family can't back (see point 1).
// 4. **下載 / 解除安裝 are decision-class — assembled, not wired**, same
//    "disabled, no click handler" precedent `users.rs`/`departments.rs`
//    already establish (`localmodels.install`/`.remove` are real
//    `require_manager!()` RPCs, dispatch L6033/6064, but writing is out of
//    this page's read-only scope this round).

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, SharedString, Stateful};
use serde_json::{json, Value};
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, tabs, BadgeVariant, ButtonVariant, TabItem};
use crate::rpc::CallError;
use crate::screens::manage_advanced_common::breadcrumb;
use crate::text_field::TextField;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand, WsConnState};
use crate::RootView;

pub use crate::screens::dashboard::Loadable;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct InstalledModel {
    pub filename: String,
    pub size_bytes: u64,
}

pub fn parse_installed(v: &Value) -> Vec<InstalledModel> {
    v.get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| InstalledModel {
            filename: m.get("filename").and_then(Value::as_str).unwrap_or("").to_string(),
            size_bytes: m.get("size_bytes").and_then(Value::as_u64).unwrap_or(0),
        })
        .collect()
}

#[derive(Clone)]
pub struct MarketRow {
    pub repo: String,
    pub name: String,
    pub architecture: Option<String>,
    pub params_b: Option<f64>,
    pub gated: bool,
    /// The recommended quant's size, falling back to the first enumerable
    /// quant when no `recommended` was selected — `0` when neither exists
    /// (an empty `quants` array is possible for a repo the sweep couldn't
    /// fully detail-fetch).
    pub size_bytes: u64,
}

pub fn parse_search_results(v: &Value) -> Vec<MarketRow> {
    v.get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let size_bytes = m
                .get("recommended")
                .and_then(|q| q.get("size_bytes"))
                .and_then(Value::as_u64)
                .or_else(|| m.get("quants").and_then(Value::as_array).and_then(|arr| arr.first()).and_then(|q| q.get("size_bytes")).and_then(Value::as_u64))
                .unwrap_or(0);
            MarketRow {
                repo: m.get("repo").and_then(Value::as_str).unwrap_or("").to_string(),
                name: m.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                architecture: m.get("architecture").and_then(Value::as_str).map(str::to_string),
                params_b: m.get("params_b").and_then(Value::as_f64),
                gated: m.get("gated").and_then(Value::as_bool).unwrap_or(false),
                size_bytes,
            }
        })
        .collect()
}

// ── State ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Installed,
    Downloadable,
}

pub struct LocalModelsState {
    requested: bool,
    pub tab: Tab,
    pub installed: Loadable<Vec<InstalledModel>>,
    pub downloadable: Loadable<Vec<MarketRow>>,
    pub search: Entity<TextField>,
}

impl LocalModelsState {
    fn new(cx: &mut gpui::App) -> Self {
        Self {
            requested: false,
            tab: Tab::Installed,
            installed: Loadable::Loading,
            downloadable: Loadable::Loading,
            search: TextField::new(cx, i18n::t(Locale::ZhTw, "localModels.search.placeholder"), false, ""),
        }
    }
}

impl Global for LocalModelsState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<LocalModelsState>() {
        let state = LocalModelsState::new(cx);
        cx.set_global(state);
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.active_page != "localModels" || cx.global::<LocalModelsState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<LocalModelsState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "localmodels.installed", json!({}), |cx, result| {
        cx.global_mut::<LocalModelsState>().installed = result.map(|v| parse_installed(&v)).into();
    });
    spawn_call(cx, tx, "localmodels.search", json!({}), |cx, result| {
        cx.global_mut::<LocalModelsState>().downloadable = result.map(|v| parse_search_results(&v)).into();
    });
}

fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<Value, String>) + 'static,
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
        CallError::Rejected(v) => v
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| v.as_str().map(str::to_string))
            .unwrap_or_else(|| v.to_string()),
    }
}

// ── Display helpers ────────────────────────────────────────────────────

pub fn format_gb(bytes: u64) -> String {
    if bytes == 0 {
        return "—".to_string();
    }
    format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// "qwen3moe · 8B 參數" style — built from real sub-fields only, never a
/// fabricated free-text description; see module header §2.
pub fn market_description(locale: Locale, row: &MarketRow) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(arch) = &row.architecture {
        if !arch.is_empty() {
            parts.push(arch.clone());
        }
    }
    if let Some(b) = row.params_b {
        parts.push(i18n::t1(locale, "localModels.paramsLabel", "n", &format!("{b:.0}")).to_string());
    }
    if parts.is_empty() {
        i18n::t(locale, "localModels.noDescription").to_string()
    } else {
        parts.join(" · ")
    }
}

fn matches_query(name: &str, repo: &str, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let q = query.trim().to_lowercase();
    name.to_lowercase().contains(&q) || repo.to_lowercase().contains(&q)
}

// ── Cards ──────────────────────────────────────────────────────────────

fn catalog_card(title: SharedString, description: SharedString, top_badge: (SharedString, BadgeVariant), action: Stateful<Div>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p(px(13.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title))
        .child(div().flex_1().text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(description))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(crate::mds_gpui::badge(top_badge.0, top_badge.1))
                .child(action),
        )
}

fn installed_card(locale: Locale, m: &InstalledModel) -> Div {
    catalog_card(
        SharedString::from(m.filename.clone()),
        SharedString::from(format_gb(m.size_bytes)),
        (i18n::t(locale, "localModels.badge.installed"), BadgeVariant::Success),
        button(
            SharedString::from(format!("localmodels-remove-{}", m.filename)),
            i18n::t(locale, "localModels.action.remove"),
            ButtonVariant::Ghost,
            true, // decision-type action — assembled, not wired; see module header §4
            None,
            |_ev, _window, _cx| {},
        ),
    )
}

fn downloadable_card(locale: Locale, m: &MarketRow) -> Div {
    let mut description = market_description(locale, m);
    if m.size_bytes > 0 {
        description = format!("{description} · {}", format_gb(m.size_bytes));
    }
    catalog_card(
        SharedString::from(m.name.clone()),
        SharedString::from(description),
        (
            i18n::t(locale, if m.gated { "localModels.badge.gated" } else { "localModels.badge.huggingface" }),
            if m.gated { BadgeVariant::Warning } else { BadgeVariant::Secondary },
        ),
        button(
            SharedString::from(format!("localmodels-download-{}", m.repo)),
            i18n::t(locale, "localModels.action.download"),
            ButtonVariant::Primary,
            true, // decision-type action — assembled, not wired; see module header §4
            None,
            |_ev, _window, _cx| {},
        ),
    )
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);
    let locale = state.locale;

    let g = cx.global::<LocalModelsState>();
    let tab = g.tab;
    let installed = g.installed.clone();
    let downloadable = g.downloadable.clone();
    let search_entity = g.search.clone();
    let query = search_entity.read(cx).content.clone();

    let installed_count = match &installed {
        Loadable::Ready(v) => v.len().to_string(),
        _ => "…".to_string(),
    };

    let crumb = breadcrumb("localmodels-breadcrumb", locale, i18n::t(locale, "localModels.title"), cx);
    let header = div()
        .child(div().text_size(px(17.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "localModels.title")))
        .child(div().mt(px(2.)).text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "localModels.subtitle")));

    let tab_items = vec![
        TabItem::new(
            "installed",
            i18n::t1(locale, "localModels.tab.installed", "n", &installed_count),
            cx.listener(|_this, _ev, _window, cx| {
                cx.global_mut::<LocalModelsState>().tab = Tab::Installed;
                cx.notify();
            }),
        ),
        TabItem::new(
            "downloadable",
            i18n::t(locale, "localModels.tab.downloadable"),
            cx.listener(|_this, _ev, _window, cx| {
                cx.global_mut::<LocalModelsState>().tab = Tab::Downloadable;
                cx.notify();
            }),
        ),
    ];
    let tab_id = match tab {
        Tab::Installed => "installed",
        Tab::Downloadable => "downloadable",
    };
    let tab_row = tabs(tab_items, tab_id);

    let search_row = div().flex().items_center().gap_2().child(div().flex_1().max_w(px(320.)).child(search_entity));

    let body: Div = match tab {
        Tab::Installed => match &installed {
            Loadable::Loading => grid_skeleton(),
            Loadable::Failed(err) => error_block(err),
            Loadable::Ready(rows) => {
                let filtered: Vec<&InstalledModel> = rows.iter().filter(|m| matches_query(&m.filename, "", &query)).collect();
                if filtered.is_empty() {
                    div().child(empty_state("📦", i18n::t(locale, "localModels.empty.installed"), None, None::<Div>))
                } else {
                    let mut grid = div().grid().grid_cols(3).gap_3();
                    for m in filtered {
                        grid = grid.child(installed_card(locale, m));
                    }
                    grid
                }
            }
        },
        Tab::Downloadable => match &downloadable {
            Loadable::Loading => grid_skeleton(),
            Loadable::Failed(err) => error_block(err),
            Loadable::Ready(rows) => {
                let filtered: Vec<&MarketRow> = rows.iter().filter(|m| matches_query(&m.name, &m.repo, &query)).collect();
                if filtered.is_empty() {
                    div().child(empty_state("🌐", i18n::t(locale, "localModels.empty.downloadable"), None, None::<Div>))
                } else {
                    let mut grid = div().grid().grid_cols(3).gap_3();
                    for m in filtered {
                        grid = grid.child(downloadable_card(locale, m));
                    }
                    grid
                }
            }
        },
    };

    div()
        .id("localmodels-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .items_center()
        .child(div().w_full().max_w(px(980.)).p_6().flex().flex_col().gap_3p5().child(crumb).child(header).child(tab_row).child(search_row).child(body))
}

fn grid_skeleton() -> Div {
    let mut grid = div().grid().grid_cols(3).gap_3();
    for _ in 0..3 {
        grid = grid.child(skeleton(px(280.), px(110.)));
    }
    grid
}

fn error_block(err: &str) -> Div {
    div().p_4().child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_installed_reads_filename_and_size() {
        let v = json!({ "models": [ {"filename": "qwen2.5-7b.Q4_K_M.gguf", "size_bytes": 4_400_000_000_u64} ] });
        let rows = parse_installed(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].filename, "qwen2.5-7b.Q4_K_M.gguf");
        assert_eq!(rows[0].size_bytes, 4_400_000_000);
    }

    #[test]
    fn parse_installed_missing_array_is_empty_not_panicking() {
        assert!(parse_installed(&json!({})).is_empty());
    }

    #[test]
    fn parse_search_results_prefers_recommended_size_over_first_quant() {
        let v = json!({ "models": [ {
            "repo": "unsloth/Qwen3-8B-GGUF", "name": "Qwen3-8B", "architecture": "qwen3", "params_b": 8.0, "gated": false,
            "recommended": { "size_bytes": 5_000_000_000_u64 },
            "quants": [ { "size_bytes": 9_000_000_000_u64 } ],
        }]});
        let rows = parse_search_results(&v);
        assert_eq!(rows[0].size_bytes, 5_000_000_000);
    }

    #[test]
    fn parse_search_results_falls_back_to_first_quant_when_no_recommended() {
        let v = json!({ "models": [ {
            "repo": "r", "name": "n", "gated": true,
            "quants": [ { "size_bytes": 2_000_000_000_u64 } ],
        }]});
        let rows = parse_search_results(&v);
        assert_eq!(rows[0].size_bytes, 2_000_000_000);
        assert!(rows[0].gated);
    }

    #[test]
    fn format_gb_scales_and_handles_zero() {
        assert_eq!(format_gb(0), "—");
        assert_eq!(format_gb(4_400_000_000), "4.1 GB");
    }

    #[test]
    fn market_description_derives_from_real_fields_only() {
        let row = MarketRow { repo: "r".into(), name: "n".into(), architecture: Some("qwen3moe".into()), params_b: Some(8.0), gated: false, size_bytes: 0 };
        assert_eq!(market_description(Locale::ZhTw, &row), format!("qwen3moe · {}", i18n::t1(Locale::ZhTw, "localModels.paramsLabel", "n", "8")));
    }

    #[test]
    fn market_description_falls_back_when_no_real_fields() {
        let row = MarketRow { repo: "r".into(), name: "n".into(), architecture: None, params_b: None, gated: false, size_bytes: 0 };
        assert_eq!(market_description(Locale::ZhTw, &row), i18n::t(Locale::ZhTw, "localModels.noDescription").to_string());
    }

    #[test]
    fn matches_query_is_case_insensitive_over_name_and_repo() {
        assert!(matches_query("Qwen3-8B", "unsloth/Qwen3-8B-GGUF", ""));
        assert!(matches_query("Qwen3-8B", "unsloth/Qwen3-8B-GGUF", "qwen3"));
        assert!(matches_query("Qwen3-8B", "unsloth/Qwen3-8B-GGUF", "unsloth"));
        assert!(!matches_query("Qwen3-8B", "unsloth/Qwen3-8B-GGUF", "llama"));
    }
}
